use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{require, Role};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{status, Task, TaskEvent},
    queue::{self, NewTask},
    state::AppState,
    workspace::Workspace,
};

async fn own_task(st: &AppState, id: Uuid, user_id: Uuid) -> ApiResult<Task> {
    let t = queue::load_task(st, id).await?.ok_or_else(AppError::not_found)?;
    if t.user_id != user_id {
        return Err(AppError::not_found());
    }
    Ok(t)
}

pub async fn list(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Vec<Task>>> {
    Ok(Json(
        sqlx::query_as::<_, Task>(
            "(SELECT * FROM tasks WHERE user_id=$1 AND status IN ('queued','running','waiting_input','pending_approval'))
             UNION ALL
             (SELECT * FROM tasks WHERE user_id=$1 AND status IN ('done','failed','blocked','cancelled') ORDER BY updated_at DESC LIMIT 150)
             ORDER BY position",
        )
        .bind(u.id)
        .fetch_all(&st.db)
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct CreateTask {
    project_id: Uuid,
    title: Option<String>,
    prompt: String,
    #[serde(default)]
    context: String,
    provider: Option<String>,
    model: Option<String>,
    parent_id: Option<Uuid>,
    #[serde(default)]
    front: bool,
}

pub async fn create(State(st): State<AppState>, AuthUser(u): AuthUser, Json(c): Json<CreateTask>) -> ApiResult<Json<Task>> {
    require(&st, c.project_id, u.id, Role::Editor).await?;
    if c.prompt.trim().is_empty() {
        return Err(AppError::bad_request("prompt required"));
    }
    let settings = queue::load_settings(&st, u.id).await?;
    let provider = c.provider.filter(|p| !p.is_empty()).unwrap_or(settings.default_provider);
    if !st.cfg.providers().contains(&provider.as_str()) {
        return Err(AppError::bad_request("unknown provider"));
    }
    if let Some(pid) = c.parent_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tasks WHERE id=$1 AND project_id=$2)")
            .bind(pid)
            .bind(c.project_id)
            .fetch_one(&st.db)
            .await?;
        if !ok {
            return Err(AppError::bad_request("parent task not in project"));
        }
    }
    let title = c
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| c.prompt.trim().lines().next().unwrap_or("Auftrag").chars().take(80).collect());
    let task = queue::insert_task(&st, NewTask {
        user_id: u.id,
        project_id: c.project_id,
        parent_id: c.parent_id,
        kind: "task",
        title: title.trim().to_string(),
        prompt: c.prompt.trim().to_string(),
        context: c.context.trim().to_string(),
        provider,
        model: c.model.filter(|m| !m.trim().is_empty()),
        status: status::QUEUED,
        attempt: 0,
        depth: 0,
        created_by_agent: false,
        front: c.front,
    })
    .await?;
    Ok(Json(task))
}

pub async fn get(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let t = queue::load_task(&st, id).await?.ok_or_else(AppError::not_found)?;
    if t.user_id != u.id {
        require(&st, t.project_id, u.id, Role::Viewer).await?;
    }
    let events: Vec<TaskEvent> = sqlx::query_as(
        "SELECT * FROM (SELECT * FROM task_events WHERE task_id=$1 ORDER BY id DESC LIMIT 1000) e ORDER BY id",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!({"task": t, "events": events})))
}

#[derive(Deserialize)]
pub struct UpdateTask {
    title: Option<String>,
    prompt: Option<String>,
    context: Option<String>,
    provider: Option<String>,
    model: Option<String>,
}

pub async fn update(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>, Json(c): Json<UpdateTask>) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if ![status::QUEUED, status::PENDING_APPROVAL, status::WAITING_INPUT].contains(&t.status.as_str()) {
        return Err(AppError::conflict("only waiting tasks can be edited"));
    }
    if let Some(p) = &c.provider {
        if !st.cfg.providers().contains(&p.as_str()) {
            return Err(AppError::bad_request("unknown provider"));
        }
    }
    sqlx::query(
        "UPDATE tasks SET title=COALESCE($2,title), prompt=COALESCE($3,prompt), context=COALESCE($4,context),
                provider=COALESCE($5,provider), model=CASE WHEN $6::text IS NULL THEN model ELSE NULLIF($6,'') END, updated_at=now()
         WHERE id=$1",
    )
    .bind(id)
    .bind(c.title.filter(|s| !s.trim().is_empty()))
    .bind(c.prompt.filter(|s| !s.trim().is_empty()))
    .bind(c.context)
    .bind(c.provider)
    .bind(c.model)
    .execute(&st.db)
    .await?;
    queue::emit_task(&st, id).await;
    Ok(Json(json!({"ok": true})))
}

async fn set_status(st: &AppState, id: Uuid, from: &[&str], to: &str) -> ApiResult<()> {
    let from: Vec<String> = from.iter().map(|s| s.to_string()).collect();
    let n = sqlx::query("UPDATE tasks SET status=$2, updated_at=now() WHERE id=$1 AND status = ANY($3)")
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

pub async fn cancel(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if t.status == status::RUNNING {
        if let Some(tok) = st.running.lock().await.get(&id) {
            tok.cancel();
        }
        return Ok(Json(json!({"ok": true})));
    }
    set_status(&st, id, &[status::QUEUED, status::PENDING_APPROVAL, status::WAITING_INPUT], status::CANCELLED).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn retry(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if ![status::FAILED, status::BLOCKED, status::CANCELLED].contains(&t.status.as_str()) {
        return Err(AppError::conflict("only failed/blocked/cancelled tasks can be retried"));
    }
    sqlx::query(
        "UPDATE tasks SET status='queued', attempt=0, error=NULL, finished_at=NULL, updated_at=now(),
                position=nextval('task_position_seq') WHERE id=$1",
    )
    .bind(id)
    .execute(&st.db)
    .await?;
    queue::emit_task(&st, id).await;
    st.wake.notify_one();
    Ok(Json(json!({"ok": true})))
}

pub async fn approve(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    own_task(&st, id, u.id).await?;
    set_status(&st, id, &[status::PENDING_APPROVAL], status::QUEUED).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn approve_all(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Value>> {
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
    Ok(Json(json!({"approved": ids.len()})))
}

#[derive(Deserialize)]
pub struct Answer {
    answer: String,
}

pub async fn answer(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>, Json(a): Json<Answer>) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if t.status != status::WAITING_INPUT {
        return Err(AppError::conflict("task is not waiting for input"));
    }
    let questions: Vec<String> = t.questions.and_then(|q| serde_json::from_value(q).ok()).unwrap_or_default();
    let mut ctx = String::from("\n\n## Answers from user\n");
    for q in &questions {
        ctx.push_str(&format!("Q: {q}\n"));
    }
    ctx.push_str(&format!("A: {}\n", a.answer.trim()));
    sqlx::query(
        "UPDATE tasks SET status='queued', questions=NULL, context = context || $2, updated_at=now(),
                position = (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id=$3 AND status='queued')
         WHERE id=$1",
    )
    .bind(id)
    .bind(ctx)
    .bind(u.id)
    .execute(&st.db)
    .await?;
    queue::emit_task(&st, id).await;
    st.wake.notify_one();
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct Move {
    direction: String,
}

pub async fn move_task(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>, Json(m): Json<Move>) -> ApiResult<Json<Value>> {
    let t = own_task(&st, id, u.id).await?;
    if t.status != status::QUEUED {
        return Err(AppError::conflict("only queued tasks can be moved"));
    }
    let mut tx = st.db.begin().await?;
    match m.direction.as_str() {
        "top" | "bottom" => {
            let sql = if m.direction == "top" {
                "UPDATE tasks SET position = (SELECT COALESCE(MIN(position),0) - 1 FROM tasks WHERE user_id=$2 AND status='queued') WHERE id=$1"
            } else {
                "UPDATE tasks SET position = (SELECT COALESCE(MAX(position),0) + 1 FROM tasks WHERE user_id=$2 AND status='queued') WHERE id=$1"
            };
            sqlx::query(sql).bind(id).bind(u.id).execute(&mut *tx).await?;
        }
        "up" | "down" => {
            let sql = if m.direction == "up" {
                "SELECT id, position FROM tasks WHERE user_id=$1 AND status='queued' AND position < $2 ORDER BY position DESC LIMIT 1"
            } else {
                "SELECT id, position FROM tasks WHERE user_id=$1 AND status='queued' AND position > $2 ORDER BY position ASC LIMIT 1"
            };
            if let Some((nid, npos)) = sqlx::query_as::<_, (Uuid, i64)>(sql).bind(u.id).bind(t.position).fetch_optional(&mut *tx).await? {
                sqlx::query("UPDATE tasks SET position=$2 WHERE id=$1").bind(nid).bind(t.position).execute(&mut *tx).await?;
                sqlx::query("UPDATE tasks SET position=$2 WHERE id=$1").bind(id).bind(npos).execute(&mut *tx).await?;
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

pub async fn diff(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let t = queue::load_task(&st, id).await?.ok_or_else(AppError::not_found)?;
    if t.user_id != u.id {
        require(&st, t.project_id, u.id, Role::Viewer).await?;
    }
    let (Some(a), Some(b)) = (&t.snapshot_before, &t.snapshot_after) else {
        return Ok(Json(json!({"diff": ""})));
    };
    let ws = Workspace::new(&st.cfg, t.project_id);
    Ok(Json(json!({"diff": ws.diff(a, b, 2_000_000).await?})))
}

pub async fn revert(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let t = queue::load_task(&st, id).await?.ok_or_else(AppError::not_found)?;
    require(&st, t.project_id, u.id, Role::Editor).await?;
    let (Some(a), Some(b)) = (&t.snapshot_before, &t.snapshot_after) else {
        return Err(AppError::bad_request("task has no snapshot"));
    };
    let running: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tasks WHERE project_id=$1 AND status='running')")
        .bind(t.project_id)
        .fetch_one(&st.db)
        .await?;
    if running {
        return Err(AppError::conflict("a task is running in this project"));
    }
    let ws = Workspace::new(&st.cfg, t.project_id);
    let sha = ws.revert(a, b, &t.title).await.map_err(|e| {
        tracing::info!("revert of {id} failed: {e:#}");
        AppError::conflict("Rückgängig nicht möglich: spätere Aufträge haben dieselben Stellen geändert")
    })?;
    queue::event(&st, &t, "status", "Änderungen zurückgesetzt").await;
    Ok(Json(json!({"ok": true, "snapshot": sha})))
}
