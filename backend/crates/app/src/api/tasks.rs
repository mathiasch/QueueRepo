use axum::{
    extract::{Path, State},
    Json,
};
use queued_api::{
    ApproveAllResult, CreateTask, DiffView, MoveTask, TaskDetail, UpdateTask, MOVE_BOTTOM,
    MOVE_DOWN, MOVE_TOP, MOVE_UP,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    limits::CheckLimits,
    projects::{require, runner_err, Role},
};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{status, Task, TaskEvent},
    queue::{self, NewTask, TaskLimits},
    state::AppState,
};

async fn own_task(st: &AppState, id: Uuid, user_id: Uuid) -> ApiResult<Task> {
    let t = queue::load_task(st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if t.user_id != user_id {
        return Err(AppError::not_found());
    }
    Ok(t)
}

pub async fn list(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Vec<Task>>> {
    Ok(Json(
        sqlx::query_as::<_, Task>(
            "(SELECT * FROM tasks WHERE user_id=$1 AND status IN ('queued','running','waiting_batch','waiting_input','pending_approval','held'))
             UNION ALL
             (SELECT * FROM tasks WHERE user_id=$1 AND status IN ('done','failed','blocked','cancelled') ORDER BY updated_at DESC LIMIT 150)
             ORDER BY position",
        )
        .bind(u.id)
        .fetch_all(&st.db)
        .await?,
    ))
}

pub async fn create(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(c): Json<CreateTask>,
) -> ApiResult<Json<Task>> {
    require(&st, c.project_id, u.id, Role::Editor).await?;
    if c.prompt.trim().is_empty() {
        return Err(AppError::bad_request("prompt required"));
    }
    let lim = c.limits.check()?;
    let settings = queue::load_settings(&st, u.id).await?;
    let project: crate::models::Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(c.project_id)
        .fetch_one(&st.db)
        .await?;
    let exec = crate::routing::resolve_execute(
        &st,
        u.id,
        &project,
        c.connector_id,
        c.provider.as_deref(),
        c.model.clone(),
        &settings.default_provider,
    )
    .await?
    .map_err(AppError::bad_request)?;
    if let Some(pid) = c.parent_id {
        let ok: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tasks WHERE id=$1 AND project_id=$2)")
                .bind(pid)
                .bind(c.project_id)
                .fetch_one(&st.db)
                .await?;
        if !ok {
            return Err(AppError::bad_request("parent task not in project"));
        }
    }
    let title = c.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| {
        c.prompt
            .trim()
            .lines()
            .next()
            .unwrap_or("Auftrag")
            .chars()
            .take(80)
            .collect()
    });
    let mut tx = st.db.begin().await?;
    let task = queue::insert_task_in(
        &mut tx,
        NewTask {
            user_id: u.id,
            project_id: c.project_id,
            parent_id: c.parent_id,
            kind: "task",
            title: title.trim().to_string(),
            prompt: c.prompt.trim().to_string(),
            context: c.context.trim().to_string(),
            provider: exec.provider,
            model: exec.model,
            status: status::QUEUED,
            attempt: 0,
            depth: 0,
            created_by_agent: false,
            front: c.front,
            limits: TaskLimits {
                max_runtime_secs: lim.max_runtime_secs.flatten(),
                max_cost_usd: lim.max_cost_usd.flatten(),
                max_tokens: lim.max_tokens.flatten(),
                max_turns: lim.max_turns.flatten(),
            },
            connector_id: exec.connector_id,
            escalation_level: 0,
            allow_test_changes: c.allow_test_changes,
            carry_from: None,
            pr_id: None,
            plan_mode: c.plan_mode.unwrap_or(project.plan_mode),
        },
    )
    .await?;
    // WP09: the composer's draft attachments belong to the task before
    // anyone (the scheduler) sees it.
    super::attachments::attach_drafts(&mut tx, &c.attachment_ids, task.id, c.project_id, u.id)
        .await?;
    tx.commit().await?;
    queue::announce(&st, &task);
    Ok(Json(task))
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TaskDetail>> {
    let t = queue::load_task(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if t.user_id != u.id {
        require(&st, t.project_id, u.id, Role::Viewer).await?;
    }
    let events: Vec<TaskEvent> = sqlx::query_as(
        "SELECT * FROM (SELECT * FROM task_events WHERE task_id=$1 ORDER BY id DESC LIMIT 1000) e ORDER BY id",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    let pull_request = super::pulls::of_task(&st, &t).await?;
    let intake = crate::intake::link_view(&st, t.id).await?;
    let attachments = super::attachments::views_for_task(&st, &t, u.id).await?;
    Ok(Json(TaskDetail {
        task: t,
        events,
        pull_request,
        intake,
        attachments,
    }))
}

pub async fn update(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(c): Json<UpdateTask>,
) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if ![
        status::QUEUED,
        status::PENDING_APPROVAL,
        status::WAITING_INPUT,
    ]
    .contains(&t.status.as_str())
    {
        return Err(AppError::conflict("only waiting tasks can be edited"));
    }
    let lim = c.limits.check()?;
    // Provider/connector/model change: resolved and checked like a new task.
    let access_change = c.provider.is_some() || c.connector_id.is_some();
    let exec = if access_change {
        let project: crate::models::Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
            .bind(t.project_id)
            .fetch_one(&st.db)
            .await?;
        // `connector_id: null` or only a provider: back to a legacy credential.
        let connector = c.connector_id.flatten();
        let model = match &c.model {
            Some(m) => Some(m.clone()),
            None => t.model.clone(),
        };
        let provider = if connector.is_some() {
            None
        } else {
            c.provider.clone().or(Some(t.provider.clone()))
        };
        Some(
            crate::routing::resolve_execute(
                &st,
                u.id,
                &project,
                connector,
                provider.as_deref(),
                model,
                &t.provider,
            )
            .await?
            .map_err(AppError::bad_request)?,
        )
    } else {
        None
    };
    sqlx::query(
        "UPDATE tasks SET title=COALESCE($2,title), prompt=COALESCE($3,prompt), context=COALESCE($4,context),
                provider=COALESCE($5,provider), model=CASE WHEN $6::text IS NULL THEN model ELSE NULLIF($6,'') END,
                max_runtime_secs = CASE WHEN $7 THEN $8 ELSE max_runtime_secs END,
                max_cost_usd = CASE WHEN $9 THEN $10 ELSE max_cost_usd END,
                max_tokens = CASE WHEN $11 THEN $12 ELSE max_tokens END,
                max_turns = CASE WHEN $13 THEN $14 ELSE max_turns END,
                connector_id = CASE WHEN $15 THEN $16 ELSE connector_id END,
                updated_at=now()
         WHERE id=$1",
    )
    .bind(id)
    .bind(c.title.filter(|s| !s.trim().is_empty()))
    .bind(c.prompt.filter(|s| !s.trim().is_empty()))
    .bind(c.context)
    .bind(exec.as_ref().map(|e| e.provider.clone()))
    .bind(match &exec {
        Some(e) => Some(e.model.clone().unwrap_or_default()),
        None => c.model,
    })
    .bind(lim.max_runtime_secs.is_some())
    .bind(lim.max_runtime_secs.flatten())
    .bind(lim.max_cost_usd.is_some())
    .bind(lim.max_cost_usd.flatten())
    .bind(lim.max_tokens.is_some())
    .bind(lim.max_tokens.flatten())
    .bind(lim.max_turns.is_some())
    .bind(lim.max_turns.flatten())
    .bind(exec.is_some())
    .bind(exec.as_ref().and_then(|e| e.connector_id))
    .execute(&st.db)
    .await?;
    queue::emit_task(&st, id).await;
    Ok(Json(json!({"ok": true})))
}

async fn set_status(st: &AppState, id: Uuid, from: &[&str], to: &str) -> ApiResult<()> {
    let from: Vec<String> = from.iter().map(|s| s.to_string()).collect();
    let n = sqlx::query(
        "UPDATE tasks SET status=$2, updated_at=now() WHERE id=$1 AND status = ANY($3)",
    )
    .bind(id)
    .bind(to)
    .bind(&from)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::conflict("task is not in a suitable state"));
    }
    queue::emit_task(st, id).await;
    st.wake.notify_one();
    Ok(())
}

pub async fn cancel(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if t.status == status::RUNNING {
        cancel_running(&st, id).await?;
        return Ok(Json(json!({"ok": true})));
    }
    if t.status == status::WAITING_BATCH {
        crate::batch::cancel_for_task(&st, id).await?;
        sqlx::query(
            "UPDATE tasks SET status='cancelled', failure_class='cancelled', error='abgebrochen', finished_at=now(), updated_at=now()
             WHERE id=$1 AND status='waiting_batch'",
        )
        .bind(id)
        .execute(&st.db)
        .await?;
        queue::emit_task(&st, id).await;
        return Ok(Json(json!({"ok": true})));
    }
    if t.status == status::HELD {
        // Held changes are discarded with the task (they never reach the branch).
        if t.gate_parsed().is_some_and(|g| g.has_diff) {
            st.runner
                .discard_held(super::gate::held_ws(&t), t.id)
                .await
                .map_err(runner_err)?;
        }
        let n = sqlx::query(
            "UPDATE tasks SET status='cancelled', failure_class='cancelled', error='abgebrochen',
                    gate=NULL, gate_pending=NULL, finished_at=now(), updated_at=now()
             WHERE id=$1 AND status='held'",
        )
        .bind(id)
        .execute(&st.db)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(AppError::conflict("task is not in a suitable state"));
        }
        queue::emit_task(&st, id).await;
        crate::runner::merge::cleanup_task(&st, t.project_id, id, true).await;
        return Ok(Json(json!({"ok": true})));
    }
    if t.status == status::MERGING {
        // Being merged right now: the merge job stops at its next step.
        if st.cancel_run(id).await {
            return Ok(Json(json!({"ok": true})));
        }
        let n = sqlx::query(
            "UPDATE tasks SET status='cancelled', failure_class='cancelled', error='abgebrochen',
                    gate_pending=NULL, finished_at=now(), updated_at=now()
             WHERE id=$1 AND status='merging' AND merge_state='queued'",
        )
        .bind(id)
        .execute(&st.db)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(AppError::conflict("task is not in a suitable state"));
        }
        queue::emit_task(&st, id).await;
        crate::runner::merge::cleanup_task(&st, t.project_id, id, true).await;
        return Ok(Json(json!({"ok": true})));
    }
    if let Err(e) = set_status(
        &st,
        id,
        &[
            status::QUEUED,
            status::PENDING_APPROVAL,
            status::WAITING_INPUT,
        ],
        status::CANCELLED,
    )
    .await
    {
        // The scheduler claimed it in the same moment: cancel the run.
        let now: Option<String> = sqlx::query_scalar("SELECT status FROM tasks WHERE id=$1")
            .bind(id)
            .fetch_optional(&st.db)
            .await?;
        if now.as_deref() == Some(status::RUNNING) {
            cancel_running(&st, id).await?;
            return Ok(Json(json!({"ok": true})));
        }
        return Err(e);
    }
    // A cancelled task's working tree and branch are gone (it may be
    // retried; it then starts afresh).
    crate::runner::merge::cleanup_task(&st, t.project_id, id, true).await;
    Ok(Json(json!({"ok": true})))
}

/// Cancels the live run of a running task. Claimed but its token not
/// registered yet (microseconds), or the previous run of a retried task
/// still cleaning up: wait for the live token instead of losing the cancel.
async fn cancel_running(st: &AppState, id: Uuid) -> ApiResult<()> {
    for _ in 0..30 {
        if st.cancel_run(id).await {
            return Ok(());
        }
        let still: Option<String> = sqlx::query_scalar("SELECT status FROM tasks WHERE id=$1")
            .bind(id)
            .fetch_optional(&st.db)
            .await?;
        if still.as_deref() != Some(status::RUNNING) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(())
}

pub async fn retry(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if ![status::FAILED, status::BLOCKED, status::CANCELLED].contains(&t.status.as_str()) {
        return Err(AppError::conflict(
            "only failed/blocked/cancelled tasks can be retried",
        ));
    }
    sqlx::query(
        "UPDATE tasks SET status='queued', attempt=0, error=NULL, finished_at=NULL, updated_at=now(),
                transient_retries=0, not_before=NULL, failure_class=NULL, usage=NULL,
                position=nextval('task_position_seq') WHERE id=$1",
    )
    .bind(id)
    .execute(&st.db)
    .await?;
    queue::emit_task(&st, id).await;
    st.wake.notify_one();
    Ok(Json(json!({"ok": true})))
}

pub async fn approve(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    own_task(&st, id, u.id).await?;
    set_status(&st, id, &[status::PENDING_APPROVAL], status::QUEUED).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn approve_all(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<ApproveAllResult>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE tasks SET status='queued', updated_at=now() WHERE user_id=$1 AND status='pending_approval' RETURNING id",
    )
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    for id in &ids {
        queue::emit_task(&st, *id).await;
    }
    st.wake.notify_one();
    Ok(Json(ApproveAllResult {
        approved: ids.len(),
    }))
}

pub async fn move_task(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(m): Json<MoveTask>,
) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if t.status != status::QUEUED {
        return Err(AppError::conflict("only queued tasks can be moved"));
    }
    let mut tx = st.db.begin().await?;
    match m.direction.as_str() {
        MOVE_TOP | MOVE_BOTTOM => {
            let sql = if m.direction == MOVE_TOP {
                "UPDATE tasks SET position = (SELECT COALESCE(MIN(position),0) - 1 FROM tasks WHERE user_id=$2 AND status='queued') WHERE id=$1"
            } else {
                "UPDATE tasks SET position = (SELECT COALESCE(MAX(position),0) + 1 FROM tasks WHERE user_id=$2 AND status='queued') WHERE id=$1"
            };
            sqlx::query(sql)
                .bind(id)
                .bind(u.id)
                .execute(&mut *tx)
                .await?;
        }
        MOVE_UP | MOVE_DOWN => {
            let sql = if m.direction == MOVE_UP {
                "SELECT id, position FROM tasks WHERE user_id=$1 AND status='queued' AND position < $2 ORDER BY position DESC LIMIT 1"
            } else {
                "SELECT id, position FROM tasks WHERE user_id=$1 AND status='queued' AND position > $2 ORDER BY position ASC LIMIT 1"
            };
            if let Some((nid, npos)) = sqlx::query_as::<_, (Uuid, i64)>(sql)
                .bind(u.id)
                .bind(t.position)
                .fetch_optional(&mut *tx)
                .await?
            {
                sqlx::query("UPDATE tasks SET position=$2 WHERE id=$1")
                    .bind(nid)
                    .bind(t.position)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE tasks SET position=$2 WHERE id=$1")
                    .bind(id)
                    .bind(npos)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                queue::emit_task(&st, nid).await;
                queue::emit_task(&st, id).await;
                return Ok(Json(json!({"ok": true})));
            }
        }
        _ => return Err(AppError::bad_request("direction: up|down|top|bottom")),
    }
    tx.commit().await?;
    queue::emit_task(&st, id).await;
    Ok(Json(json!({"ok": true})))
}

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
    // Against the merge base (WP04): everything the task brings into the
    // target branch, before and after the merge.
    let (Some(a), Some(b)) = (t.diff_base(), t.snapshot_after.as_deref()) else {
        return Ok(Json(DiffView {
            diff: String::new(),
            changed: None,
        }));
    };
    let d = match st.runner.diff(t.project_id, a, b, 2_000_000).await {
        Ok(d) => d,
        // The branch of a long-finished task may be gone.
        Err(e) if crate::runner_client::runner_status(&e) == Some(422) => {
            return Ok(Json(DiffView {
                diff: String::new(),
                changed: None,
            }))
        }
        Err(e) => return Err(runner_err(e)),
    };
    Ok(Json(DiffView {
        // Approved secrets are in the repository, but never in API answers.
        diff: queued_common::secrets::redact(&d.diff),
        changed: Some(d.changed),
    }))
}

pub async fn revert(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = queue::load_task(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    require(&st, t.project_id, u.id, Role::Editor).await?;
    let (Some(a), Some(b)) = (t.diff_base(), t.snapshot_after.as_deref()) else {
        return Err(AppError::bad_request("task has no commits"));
    };
    if !t.on_target() {
        return Err(AppError::conflict(
            "Rückgängig geht erst, wenn der Auftrag in den Hauptstand übernommen ist",
        ));
    }
    if t.branch.is_none() {
        // Tasks from before WP04 committed into the shared working tree.
        let running: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE project_id=$1 AND status='running' AND branch IS NULL)",
        )
        .bind(t.project_id)
        .fetch_one(&st.db)
        .await?;
        if running {
            return Err(AppError::conflict("a task is running in this project"));
        }
    }
    let sha = st.runner.revert(t.project_id, a, b).await.map_err(|e| {
        tracing::info!("revert of {id} failed: {e:#}");
        match crate::runner_client::runner_status(&e) {
            Some(409) => AppError::conflict(
                "Rückgängig nicht möglich: spätere Aufträge haben dieselben Stellen geändert",
            ),
            _ => runner_err(e),
        }
    })?;
    // WP09: a reverted task is a negative example for OnEmpty proposals.
    sqlx::query("UPDATE tasks SET reverted_at=now() WHERE id=$1")
        .bind(t.id)
        .execute(&st.db)
        .await?;
    queue::event(
        &st,
        &t,
        "status",
        &format!(
            "Änderungen per git revert zurückgenommen ({})",
            &sha[..8.min(sha.len())]
        ),
    )
    .await;
    Ok(Json(json!({"ok": true, "sha": sha})))
}
