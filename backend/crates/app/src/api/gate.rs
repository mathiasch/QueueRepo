//! WP03 endpoints: decisions on held tasks (`/api/tasks/{id}/gate/*`) and the
//! protection settings of a project (`/api/projects/{id}/protection`).
//!
//! A held task is decided by the person who created it (like answering a
//! question) or by the project owner. Protection settings are owner-only:
//! they are a security boundary like the environment (WP02).
use axum::{
    extract::{Path, State},
    Json,
};
use queued_api::{
    gate::{
        default_protected, validate_protected_paths, GateAction, GateDecision, GateResume,
        GateStage, ProtectionView, UpdateProtection,
    },
    DiffView,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{load, require, role, runner_err, Role};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{status, Task},
    queue,
    state::AppState,
};

/// The held task, if the caller may decide it.
async fn decidable(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<Task> {
    let t = queue::load_task(st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if t.user_id != user {
        match role(st, t.project_id, user).await? {
            Some(Role::Owner) => {}
            Some(_) => return Err(AppError::forbidden()),
            None => return Err(AppError::not_found()),
        }
    }
    Ok(t)
}

fn held_gate(t: &Task) -> ApiResult<queued_api::gate::TaskGate> {
    if t.status != status::HELD {
        return Err(AppError::conflict(
            "Der Auftrag wartet nicht auf eine Freigabe",
        ));
    }
    t.gate_parsed()
        .ok_or_else(|| AppError::conflict("Der Auftrag wartet nicht auf eine Freigabe"))
}

async fn user_label(st: &AppState, user: Uuid) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(NULLIF(name, ''), email) FROM users WHERE id=$1",
    )
    .bind(user)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}

/// `POST /api/tasks/{id}/gate/approve`.
pub async fn approve(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(d): Json<GateDecision>,
) -> ApiResult<Json<Value>> {
    let t = decidable(&st, id, u.id).await?;
    let mut g = held_gate(&t)?;
    if g.stage == GateStage::Plan {
        // WP06: the plan decisions (with their own rights check).
        let (t, _) = super::steer::steerable(&st, id, u.id).await?;
        return super::steer::approve_plan(&st, &t, u.id, None).await;
    }
    let who = user_label(&st, u.id).await;
    if d.action == GateAction::RetryReview && g.stage != GateStage::Review {
        return Err(AppError::bad_request(
            "„Review wiederholen“ gibt es nur beim Review",
        ));
    }
    match g.stage {
        GateStage::Setup | GateStage::Config => {
            // Trust exactly the commit the person saw, then start afresh.
            let head = g
                .head
                .clone()
                .ok_or_else(|| AppError::conflict("Stand der Freigabe unbekannt"))?;
            let name = if g.stage == GateStage::Setup {
                "setup"
            } else {
                "agent-config"
            };
            st.runner
                .baseline_approve(t.project_id, name, &head)
                .await
                .map_err(runner_err)?;
            let n = sqlx::query(
                "UPDATE tasks SET status='queued', gate=NULL, gate_pending=NULL, updated_at=now(),
                        position = (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id=$2 AND status='queued')
                 WHERE id=$1 AND status='held'",
            )
            .bind(id)
            .bind(t.user_id)
            .execute(&st.db)
            .await?
            .rows_affected();
            if n == 0 {
                return Err(AppError::conflict(
                    "Der Auftrag wartet nicht auf eine Freigabe",
                ));
            }
        }
        stage => {
            g.resume = Some(match (stage, d.action) {
                (GateStage::Changes, _) => GateResume::Commit,
                (GateStage::Review, GateAction::RetryReview) => GateResume::Review,
                (GateStage::Review, _) => GateResume::SkipReview,
                (GateStage::Push, _) => GateResume::Push,
                (GateStage::Setup | GateStage::Config | GateStage::Plan, _) => {
                    unreachable!("handled above")
                }
            });
            // Approved secrets: their fingerprints (never the values) are
            // allowed in this project from now on (commit, push, leftovers).
            let fps: Vec<String> = g
                .findings
                .iter()
                .filter(|f| f.kind == queued_api::gate::FindingKind::Secret)
                .filter_map(|f| f.fingerprint.clone())
                .collect();
            let mut tx = st.db.begin().await?;
            if !fps.is_empty() {
                sqlx::query(
                    "UPDATE projects SET secret_allowlist = ARRAY(SELECT DISTINCT unnest(secret_allowlist || $2::text[])) WHERE id=$1",
                )
                .bind(t.project_id)
                .bind(&fps)
                .execute(&mut *tx)
                .await?;
            }
            // Approved findings go to the reviewer as context.
            let approved = serde_json::to_value(&g.findings).unwrap_or_default();
            let n = sqlx::query(
                "UPDATE tasks SET status='queued', gate=$2,
                        gate_pending = CASE WHEN gate_pending IS NULL THEN NULL
                                            ELSE jsonb_set(gate_pending, '{approved}', COALESCE(gate_pending->'approved', '[]'::jsonb) || $3) END,
                        transient_retries = 0, not_before = NULL, updated_at=now(),
                        position = (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id=$4 AND status='queued')
                 WHERE id=$1 AND status='held'",
            )
            .bind(id)
            .bind(serde_json::to_value(&g).unwrap_or_default())
            .bind(approved)
            .bind(t.user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if n == 0 {
                return Err(AppError::conflict(
                    "Der Auftrag wartet nicht auf eine Freigabe",
                ));
            }
            tx.commit().await?;
        }
    }
    let what = match (g.stage, d.action) {
        (GateStage::Review, GateAction::RetryReview) => "Review wird wiederholt".to_string(),
        (s, _) => format!("{} – freigegeben", s.title()),
    };
    queue::event(&st, &t, "gate", &format!("{what} (von {who})")).await;
    queue::emit_task(&st, id).await;
    st.wake.notify_one();
    Ok(Json(json!({"ok": true})))
}

/// `POST /api/tasks/{id}/gate/reject`.
pub async fn reject(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = decidable(&st, id, u.id).await?;
    let mut g = held_gate(&t)?;
    if g.stage == GateStage::Plan {
        let (t, _) = super::steer::steerable(&st, id, u.id).await?;
        return super::steer::reject_plan(&st, &t, u.id).await;
    }
    let who = user_label(&st, u.id).await;
    if g.stage == GateStage::Push {
        // "Nicht hochladen": the task finishes without a push.
        g.resume = Some(GateResume::NoPush);
        let n = sqlx::query(
            "UPDATE tasks SET status='queued', gate=$2, updated_at=now(),
                    position = (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id=$3 AND status='queued')
             WHERE id=$1 AND status='held'",
        )
        .bind(id)
        .bind(serde_json::to_value(&g).unwrap_or_default())
        .bind(t.user_id)
        .execute(&st.db)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(AppError::conflict(
                "Der Auftrag wartet nicht auf eine Freigabe",
            ));
        }
        queue::event(&st, &t, "gate", &format!("Nicht hochladen (von {who})")).await;
        queue::emit_task(&st, id).await;
        st.wake.notify_one();
        return Ok(Json(json!({"ok": true})));
    }
    if g.stage == GateStage::Changes && g.has_diff {
        st.runner
            .discard_held(held_ws(&t), t.id)
            .await
            .map_err(runner_err)?;
    }
    let msg = match g.stage {
        GateStage::Changes => "Freigabe abgelehnt: Die Änderungen wurden verworfen.",
        GateStage::Review if t.branch.is_some() => "Freigabe abgelehnt: ohne Review nicht übernommen. Die Änderungen bleiben auf dem Branch des Auftrags und kommen nicht in den Hauptstand.",
        GateStage::Review => "Freigabe abgelehnt: ohne Review nicht übernommen. Die Änderungen sind gespeichert und lassen sich mit „Rückgängig“ entfernen.",
        _ => "Freigabe abgelehnt: Auftrag angehalten.",
    };
    let n = sqlx::query(
        "UPDATE tasks SET status='blocked', failure_class='gate', error=$2, gate=NULL, gate_pending=NULL,
                finished_at=now(), updated_at=now()
         WHERE id=$1 AND status='held'",
    )
    .bind(id)
    .bind(msg)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::conflict(
            "Der Auftrag wartet nicht auf eine Freigabe",
        ));
    }
    queue::event(&st, &t, "gate", &format!("{msg} (von {who})")).await;
    queue::emit_task(&st, id).await;
    // The working tree goes; the branch stays for a while (diff, retry).
    crate::runner::merge::cleanup_task(&st, t.project_id, t.id, false).await;
    st.wake.notify_one();
    Ok(Json(json!({"ok": true})))
}

/// Where a task's held changes are: its own working tree (WP04), or the
/// shared workspace for holds from before WP04 (moved on approval).
pub fn held_ws(t: &Task) -> String {
    match &t.branch {
        Some(_) => crate::runner_client::task_ws(t.id),
        None => t.project_id.to_string(),
    }
}

/// `GET /api/tasks/{id}/gate/diff`: the held changes, secrets redacted.
pub async fn diff(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DiffView>> {
    let t = queue::load_task(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if t.user_id != u.id {
        require(&st, t.project_id, u.id, Role::Viewer).await?;
    }
    let g = held_gate(&t)?;
    if !g.has_diff {
        return Ok(Json(DiffView {
            diff: String::new(),
            changed: None,
        }));
    }
    let d = st
        .runner
        .held_diff(held_ws(&t), t.id)
        .await
        .map_err(runner_err)?;
    Ok(Json(DiffView {
        diff: queued_common::secrets::redact(&d.diff),
        changed: Some(d.changed),
    }))
}

async fn view(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<ProtectionView> {
    let p = load(st, id).await?;
    let owner = role(st, id, user).await? == Some(Role::Owner);
    Ok(ProtectionView {
        trust_repo_config: p.trust_repo_config,
        protected_default: p.protected_paths.is_none(),
        protected_paths: p.protected(),
        test_guard: p.test_guard,
        setup_guard: p.setup_guard,
        approved_secrets: p.secret_allowlist.len(),
        can_edit: owner,
    })
}

/// `GET /api/projects/{id}/protection`.
pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ProtectionView>> {
    require(&st, id, u.id, Role::Viewer).await?;
    Ok(Json(view(&st, id, u.id).await?))
}

/// `PUT /api/projects/{id}/protection` (owner only).
pub async fn put(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(up): Json<UpdateProtection>,
) -> ApiResult<Json<ProtectionView>> {
    require(&st, id, u.id, Role::Owner).await?;
    let paths = match &up.protected_paths {
        Some(Some(v)) => {
            let v: Vec<String> = v
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            validate_protected_paths(&v).map_err(AppError::bad_request)?;
            // Saving exactly the default list keeps following future defaults.
            Some(if v == default_protected() {
                None
            } else {
                Some(v)
            })
        }
        Some(None) => Some(None),
        None => None,
    };
    let mut tx = st.db.begin().await?;
    if let Some(v) = up.trust_repo_config {
        sqlx::query("UPDATE projects SET trust_repo_config=$2 WHERE id=$1")
            .bind(id)
            .bind(v)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(p) = paths {
        sqlx::query("UPDATE projects SET protected_paths=$2 WHERE id=$1")
            .bind(id)
            .bind(p)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(v) = up.test_guard {
        sqlx::query("UPDATE projects SET test_guard=$2 WHERE id=$1")
            .bind(id)
            .bind(v)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(v) = up.setup_guard {
        sqlx::query("UPDATE projects SET setup_guard=$2 WHERE id=$1")
            .bind(id)
            .bind(v)
            .execute(&mut *tx)
            .await?;
    }
    if up.clear_approved_secrets {
        sqlx::query("UPDATE projects SET secret_allowlist='{}' WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(view(&st, id, u.id).await?))
}
