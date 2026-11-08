use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::header,
    response::IntoResponse,
    Json,
};
use queued_api::{
    AddMember, Commit, CreatedProject, FileView, ImportResult, Member, NewProject, ProjectRole,
    ProjectSummary, ProjectView, PushResult, UpdateProject,
};
use queued_common::{GitRemote, InitWorkspace};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::limits;
use crate::{
    auth::AuthUser,
    crypto,
    error::{ApiResult, AppError},
    models::{Project, Task},
    runner::project_remote,
    runner_client::runner_status,
    state::AppState,
};

#[derive(PartialEq, PartialOrd, Clone, Copy)]
pub enum Role {
    Viewer,
    Editor,
    Owner,
}

impl From<Role> for ProjectRole {
    fn from(r: Role) -> Self {
        match r {
            Role::Owner => ProjectRole::Owner,
            Role::Editor => ProjectRole::Editor,
            Role::Viewer => ProjectRole::Viewer,
        }
    }
}

pub async fn role(st: &AppState, project_id: Uuid, user_id: Uuid) -> ApiResult<Option<Role>> {
    let r: Option<String> =
        sqlx::query_scalar("SELECT role FROM project_members WHERE project_id=$1 AND user_id=$2")
            .bind(project_id)
            .bind(user_id)
            .fetch_optional(&st.db)
            .await?;
    Ok(r.map(|r| match r.as_str() {
        "owner" => Role::Owner,
        "editor" => Role::Editor,
        _ => Role::Viewer,
    }))
}

pub async fn require(st: &AppState, project_id: Uuid, user_id: Uuid, min: Role) -> ApiResult<()> {
    match role(st, project_id, user_id).await? {
        Some(r) if r >= min => Ok(()),
        Some(_) => Err(AppError::forbidden()),
        None => Err(AppError::not_found()),
    }
}

/// Runner errors → API errors (4xx from the runner are user errors).
pub fn runner_err(e: anyhow::Error) -> AppError {
    match runner_status(&e) {
        Some(409) => AppError::conflict(format!("{e}")),
        Some(s) if (400..500).contains(&s) => AppError::bad_request(format!("{e}")),
        _ => e.into(),
    }
}

async fn ensure_idle(st: &AppState, id: Uuid) -> ApiResult<()> {
    let running: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tasks WHERE project_id=$1 AND status='running')",
    )
    .bind(id)
    .fetch_one(&st.db)
    .await?;
    if running {
        return Err(AppError::conflict("a task is running in this project"));
    }
    Ok(())
}

pub(crate) async fn load(st: &AppState, id: Uuid) -> ApiResult<Project> {
    Ok(sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(id)
        .fetch_one(&st.db)
        .await?)
}

fn valid_remote(st: &AppState, url: &str) -> bool {
    url.starts_with("https://")
        || url.starts_with("ssh://")
        || url.starts_with("git@")
        || url.starts_with("http://")
        || (st.cfg.allow_local_remotes && (url.starts_with("file://") || url.starts_with('/')))
}

pub async fn list(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<ProjectSummary>>> {
    let rows: Vec<(Uuid, String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT p.id, p.name, p.description, m.role, ou.name, p.git_remote_url FROM projects p
         JOIN project_members m ON m.project_id=p.id AND m.user_id=$1
         JOIN users ou ON ou.id = p.owner_id
         WHERE NOT p.is_eval
         ORDER BY p.name",
    )
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(id, name, description, role, owner_name, git_remote_url)| ProjectSummary {
                    id,
                    name,
                    description,
                    role: ProjectRole::parse(&role).unwrap_or(ProjectRole::Viewer),
                    owner_name,
                    git_remote_url,
                },
            )
            .collect(),
    ))
}

/// Every project is a git repository: fresh (`git init`) or cloned from a remote.
pub async fn create(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(p): Json<NewProject>,
) -> ApiResult<Json<CreatedProject>> {
    if p.name.trim().is_empty() {
        return Err(AppError::bad_request("name required"));
    }
    let remote_url = p
        .git_remote_url
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(u) = &remote_url {
        if !valid_remote(&st, u) {
            return Err(AppError::bad_request(
                "git remote must be an https:// or ssh URL",
            ));
        }
    }
    let branch = p
        .git_branch
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "main".into());
    if !branch
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
        || branch.starts_with('-')
    {
        return Err(AppError::bad_request("invalid branch name"));
    }
    let token = p
        .git_token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let token_enc = match &token {
        Some(t) => Some(crypto::encrypt(&st.cfg.master_key, t.as_bytes())?),
        None => None,
    };

    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await?;
    let project = sqlx::query_as::<_, Project>(
        "INSERT INTO projects (id, owner_id, name, description, goal, git_remote_url, git_branch, git_token_enc)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
    )
    .bind(id)
    .bind(u.id)
    .bind(p.name.trim())
    .bind(p.description)
    .bind(p.goal)
    .bind(&remote_url)
    .bind(&branch)
    .bind(token_enc)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO project_members (project_id, user_id, role) VALUES ($1,$2,'owner')")
        .bind(id)
        .bind(u.id)
        .execute(&mut *tx)
        .await?;

    // Create/clone the repository before committing the DB row.
    let remote = remote_url.map(|url| GitRemote {
        url,
        branch: branch.clone(),
        token,
    });
    let head = st
        .runner
        .init(
            id,
            &InitWorkspace {
                remote,
                branch: Some(branch),
            },
        )
        .await
        .map_err(|e| {
            tracing::info!("init of project {id} failed: {e:#}");
            AppError::bad_request(format!("Repository konnte nicht angelegt werden: {e}"))
        })?;
    tx.commit().await?;
    Ok(Json(CreatedProject {
        project: project.view(ProjectRole::Owner),
        head,
    }))
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ProjectView>> {
    let role = role(&st, id, u.id).await?.ok_or_else(AppError::not_found)?;
    Ok(Json(load(&st, id).await?.view(role.into())))
}

pub async fn update(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(p): Json<UpdateProject>,
) -> ApiResult<Json<ProjectView>> {
    require(&st, id, u.id, Role::Editor).await?;
    limits::int(
        "Max. Laufzeit (Sekunden)",
        p.max_runtime_secs.flatten(),
        limits::RUNTIME,
    )?;
    limits::int(
        "Stillstand (Sekunden)",
        p.stall_timeout_secs.flatten(),
        limits::STALL,
    )?;
    limits::int("Max. Tokens", p.max_tokens.flatten(), limits::TOKENS)?;
    limits::int("Max. Turns", p.max_turns.flatten(), limits::TURNS)?;
    limits::cost(p.max_cost_usd.flatten())?;
    let narrow = |v: Option<Option<i64>>| v.flatten().map(|x| x as i32);
    let trim = |s: Option<String>| s.map(|v| v.trim().to_string());
    if let Some(url) = p
        .git_remote_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        if !valid_remote(&st, url) {
            return Err(AppError::bad_request(
                "git remote must be an https:// or ssh URL",
            ));
        }
    }
    if let Some(rp) = p
        .review_provider
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        // Legacy review providers need their own credential (opencode: connectors only).
        if !st.cfg.providers().contains(&rp) || crate::agent::credential_kinds(rp).is_empty() {
            return Err(AppError::bad_request("unknown provider"));
        }
    }
    let (token_set, token_enc) = match p.git_token.as_deref().map(str::trim) {
        None => (false, None),
        Some("") => (true, None),
        Some(t) => (
            true,
            Some(crypto::encrypt(&st.cfg.master_key, t.as_bytes())?),
        ),
    };
    let project = sqlx::query_as::<_, Project>(
        "UPDATE projects SET
            name = COALESCE($2, name), description = COALESCE($3, description), goal = COALESCE($4, goal),
            verify_command = CASE WHEN $5::text IS NULL THEN verify_command ELSE NULLIF($5, '') END,
            review_enabled = COALESCE($6, review_enabled),
            review_provider = CASE WHEN $7::text IS NULL THEN review_provider ELSE NULLIF($7, '') END,
            review_model = CASE WHEN $8::text IS NULL THEN review_model ELSE NULLIF($8, '') END,
            git_remote_url = CASE WHEN $9::text IS NULL THEN git_remote_url ELSE NULLIF($9, '') END,
            git_branch = COALESCE(NULLIF($10, ''), git_branch),
            git_token_enc = CASE WHEN $11 THEN $12 ELSE git_token_enc END,
            auto_push = COALESCE($13, auto_push),
            max_runtime_secs = CASE WHEN $14 THEN $15 ELSE max_runtime_secs END,
            stall_timeout_secs = CASE WHEN $16 THEN $17 ELSE stall_timeout_secs END,
            max_cost_usd = CASE WHEN $18 THEN $19 ELSE max_cost_usd END,
            max_tokens = CASE WHEN $20 THEN $21 ELSE max_tokens END,
            max_turns = CASE WHEN $22 THEN $23 ELSE max_turns END
         WHERE id=$1 RETURNING *",
    )
    .bind(id)
    .bind(p.name.filter(|n| !n.trim().is_empty()))
    .bind(p.description)
    .bind(p.goal)
    .bind(trim(p.verify_command))
    .bind(p.review_enabled)
    .bind(trim(p.review_provider))
    .bind(trim(p.review_model))
    .bind(trim(p.git_remote_url))
    .bind(trim(p.git_branch))
    .bind(token_set)
    .bind(token_enc)
    .bind(p.auto_push)
    .bind(p.max_runtime_secs.is_some())
    .bind(narrow(p.max_runtime_secs))
    .bind(p.stall_timeout_secs.is_some())
    .bind(narrow(p.stall_timeout_secs))
    .bind(p.max_cost_usd.is_some())
    .bind(p.max_cost_usd.flatten())
    .bind(p.max_tokens.is_some())
    .bind(p.max_tokens.flatten())
    .bind(p.max_turns.is_some())
    .bind(narrow(p.max_turns))
    .fetch_one(&st.db)
    .await?;
    st.wake.notify_one();
    let role = role(&st, id, u.id).await?.ok_or_else(AppError::not_found)?;
    Ok(Json(project.view(role.into())))
}

pub async fn remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Owner).await?;
    ensure_idle(&st, id).await?;
    sqlx::query("DELETE FROM projects WHERE id=$1")
        .bind(id)
        .execute(&st.db)
        .await?;
    if let Err(e) = st.runner.delete(id).await {
        tracing::warn!("workspace {id} not deleted: {e:#}");
    }
    Ok(Json(json!({"ok": true})))
}

pub async fn members(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<Member>>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT u.id, u.email, u.name, m.role FROM project_members m JOIN users u ON u.id=m.user_id WHERE m.project_id=$1 ORDER BY m.role, u.name",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(user_id, email, name, role)| Member {
                user_id,
                email,
                name,
                role: ProjectRole::parse(&role).unwrap_or(ProjectRole::Viewer),
            })
            .collect(),
    ))
}

pub async fn add_member(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(m): Json<AddMember>,
) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Owner).await?;
    if !["editor", "viewer"].contains(&m.role.as_str()) {
        return Err(AppError::bad_request("role must be editor or viewer"));
    }
    let uid: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email=$1")
        .bind(m.email.trim().to_lowercase())
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| {
            AppError::bad_request("no user with this email – they must sign in once first")
        })?;
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role) VALUES ($1,$2,$3)
         ON CONFLICT (project_id, user_id) DO UPDATE SET role=EXCLUDED.role WHERE project_members.role <> 'owner'",
    )
    .bind(id)
    .bind(uid)
    .bind(&m.role)
    .execute(&st.db)
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn remove_member(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    if user_id != u.id {
        require(&st, id, u.id, Role::Owner).await?;
    }
    sqlx::query(
        "DELETE FROM project_members WHERE project_id=$1 AND user_id=$2 AND role <> 'owner'",
    )
    .bind(id)
    .bind(user_id)
    .execute(&st.db)
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn files(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<String>>> {
    require(&st, id, u.id, Role::Viewer).await?;
    Ok(Json(st.runner.files(id).await.map_err(runner_err)?))
}

#[derive(Deserialize)]
pub struct FileQuery {
    path: String,
}

pub async fn file(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<FileQuery>,
) -> ApiResult<Json<FileView>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let f = st.runner.file(id, &q.path).await.map_err(runner_err)?;
    Ok(Json(FileView {
        path: f.path,
        content: f.content,
    }))
}

pub async fn zip(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require(&st, id, u.id, Role::Viewer).await?;
    let name = load(&st, id).await?.name;
    let data = st.runner.zip(id).await.map_err(runner_err)?;
    let fname: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{fname}.zip\""),
            ),
        ],
        data,
    ))
}

pub async fn import(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> ApiResult<Json<ImportResult>> {
    require(&st, id, u.id, Role::Editor).await?;
    ensure_idle(&st, id).await?;
    let r = st
        .runner
        .import(id, body.to_vec(), &u.name, &u.email)
        .await
        .map_err(runner_err)?;
    Ok(Json(ImportResult {
        files: r.files,
        sha: r.sha,
    }))
}

pub async fn commits(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<Commit>>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let log = st.runner.log(id, 100).await.map_err(runner_err)?;
    Ok(Json(
        log.into_iter()
            .map(|c| Commit {
                sha: c.sha,
                author: c.author,
                date: c.date,
                subject: c.subject,
            })
            .collect(),
    ))
}

pub async fn push(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PushResult>> {
    require(&st, id, u.id, Role::Editor).await?;
    let p = load(&st, id).await?;
    let remote =
        project_remote(&st, &p).ok_or_else(|| AppError::bad_request("no git remote configured"))?;
    // A person clicked "Push": that approves the target branch. The secret
    // scan of the pushed commits still applies (WP03).
    let req = queued_common::PushRequest {
        remote,
        sha: None,
        allow_protected: true,
        allow: p.secret_allowlist.clone(),
        allow_secrets: false,
    };
    let r = st.runner.push(id, &req).await.map_err(runner_err)?;
    if !r.held.is_empty() {
        return Err(AppError::conflict(format!(
            "Push blockiert – mögliches Geheimnis in noch nicht hochgeladenen Commits: {}. Bitte entfernen (neuer Commit reicht nicht, der alte Commit enthält es weiterhin) oder außerhalb von Queue pushen.",
            r.held
                .iter()
                .map(|f| format!("{} ({})", f.detail, f.path))
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }
    if !r.ok {
        return Err(AppError::bad_request(format!("push failed: {}", r.output)));
    }
    Ok(Json(PushResult {
        ok: true,
        output: r.output,
    }))
}

pub async fn tasks(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<Task>>> {
    require(&st, id, u.id, Role::Viewer).await?;
    Ok(Json(
        sqlx::query_as::<_, Task>(
            "SELECT * FROM tasks WHERE project_id=$1 ORDER BY created_at DESC LIMIT 200",
        )
        .bind(id)
        .fetch_all(&st.db)
        .await?,
    ))
}
