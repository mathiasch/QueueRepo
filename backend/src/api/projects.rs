use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::header,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{Project, Task},
    state::AppState,
    workspace::Workspace,
};

#[derive(PartialEq, PartialOrd, Clone, Copy)]
pub enum Role {
    Viewer,
    Editor,
    Owner,
}

pub async fn role(st: &AppState, project_id: Uuid, user_id: Uuid) -> ApiResult<Option<Role>> {
    let r: Option<String> = sqlx::query_scalar("SELECT role FROM project_members WHERE project_id=$1 AND user_id=$2")
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

pub async fn list(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT p.id, p.name, p.description, m.role, ou.name FROM projects p
         JOIN project_members m ON m.project_id=p.id AND m.user_id=$1
         JOIN users ou ON ou.id = p.owner_id
         ORDER BY p.name",
    )
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(id, name, description, role, owner)| json!({"id": id, "name": name, "description": description, "role": role, "owner_name": owner}))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
pub struct NewProject {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    goal: String,
}

pub async fn create(State(st): State<AppState>, AuthUser(u): AuthUser, Json(p): Json<NewProject>) -> ApiResult<Json<Project>> {
    if p.name.trim().is_empty() {
        return Err(AppError::bad_request("name required"));
    }
    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await?;
    let project = sqlx::query_as::<_, Project>(
        "INSERT INTO projects (id, owner_id, name, description, goal) VALUES ($1,$2,$3,$4,$5) RETURNING *",
    )
    .bind(id)
    .bind(u.id)
    .bind(p.name.trim())
    .bind(p.description)
    .bind(p.goal)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO project_members (project_id, user_id, role) VALUES ($1,$2,'owner')")
        .bind(id)
        .bind(u.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Workspace::new(&st.cfg, id).ensure().await?;
    Ok(Json(project))
}

pub async fn get(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let role = role(&st, id, u.id).await?.ok_or_else(AppError::not_found)?;
    let p: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1").bind(id).fetch_one(&st.db).await?;
    let role = match role {
        Role::Owner => "owner",
        Role::Editor => "editor",
        Role::Viewer => "viewer",
    };
    let mut v = serde_json::to_value(p).map_err(anyhow::Error::from)?;
    v["role"] = json!(role);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct UpdateProject {
    name: Option<String>,
    description: Option<String>,
    goal: Option<String>,
    verify_command: Option<String>,
    review_enabled: Option<bool>,
    review_provider: Option<String>,
    review_model: Option<String>,
}

pub async fn update(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>, Json(p): Json<UpdateProject>) -> ApiResult<Json<Project>> {
    require(&st, id, u.id, Role::Editor).await?;
    let empty_to_null = |s: Option<String>| s.map(|v| v.trim().to_string());
    let project = sqlx::query_as::<_, Project>(
        "UPDATE projects SET
            name = COALESCE($2, name), description = COALESCE($3, description), goal = COALESCE($4, goal),
            verify_command = CASE WHEN $5::text IS NULL THEN verify_command ELSE NULLIF($5, '') END,
            review_enabled = COALESCE($6, review_enabled),
            review_provider = CASE WHEN $7::text IS NULL THEN review_provider ELSE NULLIF($7, '') END,
            review_model = CASE WHEN $8::text IS NULL THEN review_model ELSE NULLIF($8, '') END
         WHERE id=$1 RETURNING *",
    )
    .bind(id)
    .bind(p.name.filter(|n| !n.trim().is_empty()))
    .bind(p.description)
    .bind(p.goal)
    .bind(empty_to_null(p.verify_command))
    .bind(p.review_enabled)
    .bind(empty_to_null(p.review_provider))
    .bind(empty_to_null(p.review_model))
    .fetch_one(&st.db)
    .await?;
    Ok(Json(project))
}

pub async fn remove(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Owner).await?;
    let running: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tasks WHERE project_id=$1 AND status='running')")
        .bind(id)
        .fetch_one(&st.db)
        .await?;
    if running {
        return Err(AppError::conflict("a task is running in this project"));
    }
    sqlx::query("DELETE FROM projects WHERE id=$1").bind(id).execute(&st.db).await?;
    Workspace::new(&st.cfg, id).delete().await;
    Ok(Json(json!({"ok": true})))
}

pub async fn members(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT u.id, u.email, u.name, m.role FROM project_members m JOIN users u ON u.id=m.user_id WHERE m.project_id=$1 ORDER BY m.role, u.name",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(id, email, name, role)| json!({"user_id": id, "email": email, "name": name, "role": role}))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
pub struct AddMember {
    email: String,
    role: String,
}

pub async fn add_member(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>, Json(m): Json<AddMember>) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Owner).await?;
    if !["editor", "viewer"].contains(&m.role.as_str()) {
        return Err(AppError::bad_request("role must be editor or viewer"));
    }
    let uid: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email=$1")
        .bind(m.email.trim().to_lowercase())
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("no user with this email – they must sign in once first"))?;
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

pub async fn remove_member(State(st): State<AppState>, AuthUser(u): AuthUser, Path((id, user_id)): Path<(Uuid, Uuid)>) -> ApiResult<Json<Value>> {
    if user_id != u.id {
        require(&st, id, u.id, Role::Owner).await?;
    }
    sqlx::query("DELETE FROM project_members WHERE project_id=$1 AND user_id=$2 AND role <> 'owner'")
        .bind(id)
        .bind(user_id)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn files(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Vec<String>>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let ws = Workspace::new(&st.cfg, id);
    ws.ensure().await?;
    Ok(Json(ws.list_files().await?))
}

#[derive(Deserialize)]
pub struct FileQuery {
    path: String,
}

pub async fn file(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>, Query(q): Query<FileQuery>) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let ws = Workspace::new(&st.cfg, id);
    let content = ws.read_file(&q.path, 1024 * 1024).await.map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(Json(json!({"path": q.path, "content": content})))
}

pub async fn zip(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    require(&st, id, u.id, Role::Viewer).await?;
    let name: String = sqlx::query_scalar("SELECT name FROM projects WHERE id=$1").bind(id).fetch_one(&st.db).await?;
    let ws = Workspace::new(&st.cfg, id);
    ws.ensure().await?;
    let data = ws.zip().await?;
    let fname: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{fname}.zip\"")),
        ],
        data,
    ))
}

pub async fn import(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>, body: Bytes) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Editor).await?;
    let running: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tasks WHERE project_id=$1 AND status='running')")
        .bind(id)
        .fetch_one(&st.db)
        .await?;
    if running {
        return Err(AppError::conflict("a task is running in this project"));
    }
    let ws = Workspace::new(&st.cfg, id);
    ws.ensure().await?;
    let n = ws.import_zip(body.to_vec()).await.map_err(|e| AppError::bad_request(format!("import failed: {e}")))?;
    Ok(Json(json!({"files": n})))
}

pub async fn tasks(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Vec<Task>>> {
    require(&st, id, u.id, Role::Viewer).await?;
    Ok(Json(
        sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE project_id=$1 ORDER BY created_at DESC LIMIT 200")
            .bind(id)
            .fetch_all(&st.db)
            .await?,
    ))
}
