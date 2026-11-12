//! WP09 API: project rules (managed AGENTS.md) and the project state.
//!
//! Rights: everybody in the project reads. The owner saves approved
//! versions, decides proposals and exports to the repository; editors
//! propose and import (an import is a proposal). The project state can be
//! edited by editors and owners (humans may restructure it).
use axum::{
    extract::{Path, State},
    Json,
};
use queued_api::memory::{
    validate_knowledge, KnowledgeDecision, KnowledgeExportView, KnowledgeImport, KnowledgeVersion,
    KnowledgeView, PutState, SaveKnowledge, StateRevision, StateView, IMPORTABLE_FILES,
};
use uuid::Uuid;

use super::projects::{require, role, runner_err, Role};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    knowledge::{self, Row, SELECT},
    models::Project,
    project_state,
    state::AppState,
};

async fn member(st: &AppState, project: Uuid, user: Uuid) -> ApiResult<Role> {
    role(st, project, user)
        .await?
        .ok_or_else(AppError::not_found)
}

async fn view(st: &AppState, project: Uuid, r: Role) -> ApiResult<KnowledgeView> {
    let versions: Vec<KnowledgeVersion> = sqlx::query_as::<_, Row>(&format!(
        "{SELECT} WHERE k.project_id=$1 AND k.status='approved' ORDER BY k.version DESC LIMIT 30"
    ))
    .bind(project)
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect();
    let proposals: Vec<KnowledgeVersion> = sqlx::query_as::<_, Row>(&format!(
        "{SELECT} WHERE k.project_id=$1 AND k.status='proposed' ORDER BY k.created_at, k.id"
    ))
    .bind(project)
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect();
    let rejected: Vec<KnowledgeVersion> = sqlx::query_as::<_, Row>(&format!(
        "{SELECT} WHERE k.project_id=$1 AND k.status='rejected' ORDER BY k.decided_at DESC NULLS LAST LIMIT 10"
    ))
    .bind(project)
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect();
    let trust: bool = sqlx::query_scalar("SELECT trust_repo_config FROM projects WHERE id=$1")
        .bind(project)
        .fetch_one(&st.db)
        .await?;
    Ok(KnowledgeView {
        current: versions.first().cloned(),
        versions,
        proposals,
        rejected,
        can_approve: r == Role::Owner,
        can_propose: r >= Role::Editor,
        trust_repo_config: trust,
    })
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<KnowledgeView>> {
    let r = member(&st, id, u.id).await?;
    Ok(Json(view(&st, id, r).await?))
}

async fn current_version(
    conn: &mut sqlx::PgConnection,
    project: Uuid,
) -> sqlx::Result<Option<i32>> {
    sqlx::query_scalar(
        "SELECT max(version) FROM knowledge_versions WHERE project_id=$1 AND status='approved'",
    )
    .bind(project)
    .fetch_one(conn)
    .await
}

/// Owner: new approved version. Editor: proposal.
pub async fn save(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SaveKnowledge>,
) -> ApiResult<Json<KnowledgeView>> {
    let r = member(&st, id, u.id).await?;
    if r < Role::Editor {
        return Err(AppError::forbidden());
    }
    validate_knowledge(&b.content, &b.note).map_err(AppError::bad_request)?;
    let content = b.content.trim_end().to_string();
    let mut tx = st.db.begin().await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let cur = current_version(&mut tx, id).await?;
    if r == Role::Owner {
        if b.base_version != cur {
            return Err(AppError::conflict(
                "Die Regeln wurden inzwischen geändert. Bitte neu laden und die Änderung erneut vornehmen.",
            ));
        }
        sqlx::query(
            "INSERT INTO knowledge_versions (id, project_id, version, status, content, source, author_id, base_version, note, decided_by, decided_at)
             VALUES ($1, $2, $3, 'approved', $4, 'human', $5, $6, $7, $5, now())",
        )
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(cur.unwrap_or(0) + 1)
        .bind(&content)
        .bind(u.id)
        .bind(cur)
        .bind(b.note.trim())
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO knowledge_versions (id, project_id, status, content, source, author_id, base_version, note)
             VALUES ($1, $2, 'proposed', $3, 'human', $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(&content)
        .bind(u.id)
        .bind(b.base_version.or(cur))
        .bind(b.note.trim())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(view(&st, id, r).await?))
}

pub async fn approve(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, kid)): Path<(Uuid, Uuid)>,
    Json(d): Json<KnowledgeDecision>,
) -> ApiResult<Json<KnowledgeView>> {
    require(&st, id, u.id, Role::Owner).await?;
    decide(&st, id, kid, u.id, true, &d.note).await?;
    Ok(Json(view(&st, id, Role::Owner).await?))
}

pub async fn reject(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, kid)): Path<(Uuid, Uuid)>,
    Json(d): Json<KnowledgeDecision>,
) -> ApiResult<Json<KnowledgeView>> {
    require(&st, id, u.id, Role::Owner).await?;
    decide(&st, id, kid, u.id, false, &d.note).await?;
    Ok(Json(view(&st, id, Role::Owner).await?))
}

async fn decide(
    st: &AppState,
    project: Uuid,
    kid: Uuid,
    user: Uuid,
    approve: bool,
    note: &str,
) -> ApiResult<()> {
    validate_knowledge("", note).map_err(AppError::bad_request)?;
    let mut tx = st.db.begin().await?;
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(project)
        .execute(&mut *tx)
        .await?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM knowledge_versions WHERE id=$1 AND project_id=$2")
            .bind(kid)
            .bind(project)
            .fetch_optional(&mut *tx)
            .await?;
    match status.as_deref() {
        None => return Err(AppError::not_found()),
        Some("proposed") => {}
        Some(_) => {
            return Err(AppError::conflict(
                "Über diesen Vorschlag wurde schon entschieden.",
            ))
        }
    }
    let cur = current_version(&mut tx, project).await?;
    let note = note.trim();
    sqlx::query(
        "UPDATE knowledge_versions SET status=$3, version=$4, decided_by=$5, decided_at=now(),
                note = CASE WHEN $6 = '' THEN note WHEN note = '' THEN $6 ELSE note || E'\\n' || $6 END
          WHERE id=$1 AND project_id=$2",
    )
    .bind(kid)
    .bind(project)
    .bind(if approve { "approved" } else { "rejected" })
    .bind(approve.then(|| cur.unwrap_or(0) + 1))
    .bind(user)
    .bind(note.chars().take(500).collect::<String>())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A repository instruction file becomes a proposal (never trusted as is).
pub async fn import(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(b): Json<KnowledgeImport>,
) -> ApiResult<Json<KnowledgeView>> {
    let r = member(&st, id, u.id).await?;
    if r < Role::Editor {
        return Err(AppError::forbidden());
    }
    if !IMPORTABLE_FILES.contains(&b.file.as_str()) {
        return Err(AppError::bad_request(
            "Nur AGENTS.md, CLAUDE.md oder GEMINI.md.",
        ));
    }
    let f = st.runner.file(id, &b.file).await.map_err(
        |e| match crate::runner_client::runner_status(&e) {
            Some(_) => AppError::bad_request(format!(
                "{} gibt es im Repository nicht (oder sie ist zu groß).",
                b.file
            )),
            None => runner_err(e),
        },
    )?;
    let mut content = f.content;
    if content.starts_with(queued_common::MANAGED_MARKER) {
        // A file Queue wrote itself: drop the marker line (and a bridge
        // has nothing to import).
        content = content
            .split_once('\n')
            .map(|x| x.1)
            .unwrap_or("")
            .trim_start()
            .to_string();
        if content.trim() == "@AGENTS.md" {
            return Err(AppError::bad_request(format!(
                "{} ist nur eine Brücke zu AGENTS.md – bitte AGENTS.md übernehmen.",
                b.file
            )));
        }
    }
    let content = queued_common::secrets::redact(content.trim());
    if content.is_empty() {
        return Err(AppError::bad_request(format!("{} ist leer.", b.file)));
    }
    validate_knowledge(&content, "").map_err(AppError::bad_request)?;
    let mut tx = st.db.begin().await?;
    let cur = current_version(&mut tx, id).await?;
    sqlx::query(
        "INSERT INTO knowledge_versions (id, project_id, status, content, source, author_id, base_version, note)
         VALUES ($1, $2, 'proposed', $3, 'repo', $4, $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(&content)
    .bind(u.id)
    .bind(cur)
    .bind(format!("Aus {} im Repository", b.file))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(view(&st, id, r).await?))
}

/// Writes the approved rules into the repository (owner).
pub async fn export(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<KnowledgeExportView>> {
    require(&st, id, u.id, Role::Owner).await?;
    let Some((version, content)) = knowledge::approved(&st, id).await? else {
        return Err(AppError::bad_request(
            "Es gibt noch keine freigegebene Fassung.",
        ));
    };
    let project: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(id)
        .fetch_one(&st.db)
        .await?;
    let (name, email): (String, String) =
        sqlx::query_as("SELECT name, email FROM users WHERE id=$1")
            .bind(u.id)
            .fetch_one(&st.db)
            .await?;
    st.runner
        .init(
            id,
            &queued_common::InitWorkspace {
                remote: crate::runner::project_remote(&st, &project),
                branch: Some(project.git_branch.clone()),
            },
        )
        .await
        .map_err(runner_err)?;
    // Leftovers of the runner-only main tree first (WIP or quarantine), so
    // the export commit holds exactly the managed files. No pull.
    st.runner
        .begin(id, None, &project.secret_allowlist)
        .await
        .map_err(runner_err)?;
    let out = st
        .runner
        .knowledge_export(
            id,
            &queued_common::KnowledgeExport {
                content,
                version,
                author_name: if name.trim().is_empty() {
                    email.clone()
                } else {
                    name
                },
                author_email: email,
                allow: project.secret_allowlist.clone(),
            },
        )
        .await
        .map_err(runner_err)?;
    if !out.blocked.is_empty() {
        return Err(AppError::conflict(
            "Die Regeln enthalten etwas, das wie ein Geheimnis aussieht – nichts wurde geschrieben. Bitte das Geheimnis aus den Regeln entfernen.",
        ));
    }
    // Trusted repository configuration: the CLIs read these files, so they
    // are watched agent configuration. The owner just approved exactly
    // them; other pending changes keep needing their own approval.
    let mut baseline_updated = false;
    if project.trust_repo_config && !out.written.is_empty() {
        let res = st
            .runner
            .baseline_check(
                id,
                &queued_common::gate::BaselineCheck {
                    name: "agent-config".into(),
                    paths: queued_common::gate::AGENT_CONFIG_PATHS
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    task: None,
                },
            )
            .await
            .map_err(runner_err)?;
        if !res.changed.is_empty() && res.changed.iter().all(|p| out.written.contains(p)) {
            st.runner
                .baseline_approve(id, "agent-config", &res.head)
                .await
                .map_err(runner_err)?;
            baseline_updated = true;
        }
    }
    Ok(Json(KnowledgeExportView {
        sha: out.sha,
        version,
        written: out.written,
        skipped: out.skipped,
        baseline_updated,
    }))
}

// ---------------------------------------------------------------------------
// Project state
// ---------------------------------------------------------------------------

async fn state_view(st: &AppState, project: Uuid, r: Role) -> ApiResult<StateView> {
    let (rev, state) = project_state::current(st, project).await?;
    let history: Vec<StateRevision> = sqlx::query_as::<
        _,
        (
            i32,
            String,
            Option<Uuid>,
            Option<String>,
            Option<String>,
            String,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT r.rev, r.source, r.task_id, t.title, u.email, r.note, r.created_at
           FROM project_state_revisions r
           LEFT JOIN tasks t ON t.id = r.task_id
           LEFT JOIN users u ON u.id = r.author_id
          WHERE r.project_id=$1 ORDER BY r.rev DESC LIMIT 20",
    )
    .bind(project)
    .fetch_all(&st.db)
    .await?
    .into_iter()
    .map(
        |(rev, source, task_id, task_title, author, note, created_at)| StateRevision {
            rev,
            source,
            task_id,
            task_title,
            author,
            note,
            created_at,
        },
    )
    .collect();
    Ok(StateView {
        rev,
        state,
        history,
        can_edit: r >= Role::Editor,
    })
}

pub async fn get_state(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StateView>> {
    let r = member(&st, id, u.id).await?;
    Ok(Json(state_view(&st, id, r).await?))
}

pub async fn put_state(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PutState>,
) -> ApiResult<Json<StateView>> {
    let r = member(&st, id, u.id).await?;
    if r < Role::Editor {
        return Err(AppError::forbidden());
    }
    // Same schema as for agents (humans may delete and rename).
    let text = serde_json::to_string(&b.state).unwrap_or_default();
    let mut s = queued_common::state::parse(&text).map_err(AppError::bad_request)?;
    for f in &mut s.features {
        if let Some(bad) = f
            .files
            .iter()
            .find(|p| !queued_common::state::valid_file_ref(p))
        {
            return Err(AppError::bad_request(format!(
                "„{}“: ungültiger Datei-Verweis „{bad}“ (relativer Pfad im Repository, nicht in .git/ oder .queue/)",
                f.id
            )));
        }
        if f.passes && f.status != queued_common::state::FeatureStatus::Done {
            f.passes = false;
        }
    }
    if b.note.chars().count() > 2000 {
        return Err(AppError::bad_request("Die Notiz ist zu lang."));
    }
    let who = project_state::Author {
        source: "human",
        task_id: None,
        user_id: Some(u.id),
        note: b.note.trim().to_string(),
    };
    if project_state::store(&st, id, &s, Some(b.base_rev), who)
        .await?
        .is_none()
    {
        return Err(AppError::conflict(
            "Der Projektzustand wurde inzwischen geändert. Bitte neu laden.",
        ));
    }
    Ok(Json(state_view(&st, id, r).await?))
}
