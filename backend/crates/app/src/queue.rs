//! Task persistence helpers shared by API, runner and scheduler.
use chrono::{DateTime, Utc};
use queued_api::ServerEvent;
use serde_json::Value;
use uuid::Uuid;

use queued_common::secrets::redact;

use crate::{
    agent::Credential,
    crypto,
    models::{FailureClass, Settings, Task, TaskEvent, Usage},
    state::AppState,
};

/// Task-level limit overrides (NULL = project default / global).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskLimits {
    pub max_runtime_secs: Option<i32>,
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<i64>,
    pub max_turns: Option<i32>,
}

impl TaskLimits {
    pub fn of(t: &Task) -> Self {
        Self {
            max_runtime_secs: t.max_runtime_secs,
            max_cost_usd: t.max_cost_usd,
            max_tokens: t.max_tokens,
            max_turns: t.max_turns,
        }
    }
}

pub struct NewTask {
    pub user_id: Uuid,
    pub project_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub kind: &'static str,
    pub title: String,
    pub prompt: String,
    pub context: String,
    pub provider: String,
    pub model: Option<String>,
    pub status: &'static str,
    pub attempt: i32,
    pub depth: i32,
    pub created_by_agent: bool,
    /// Insert at the head of the user's queue.
    pub front: bool,
    pub limits: TaskLimits,
    /// Connector of the agent loop (`None` = legacy credential of `provider`).
    pub connector_id: Option<Uuid>,
    /// Position in the escalation chain of the project routing.
    pub escalation_level: i32,
    /// Test task (human-created only): protected paths/tests without approval.
    pub allow_test_changes: bool,
    /// WP04: task whose unmerged changes this one continues (fix tasks).
    pub carry_from: Option<Uuid>,
}

pub async fn insert_task(st: &AppState, t: NewTask) -> sqlx::Result<Task> {
    let task = sqlx::query_as::<_, Task>(
        "INSERT INTO tasks (id, user_id, project_id, parent_id, kind, title, prompt, context, provider, model, status,
                            attempt, depth, created_by_agent, position,
                            max_runtime_secs, max_cost_usd, max_tokens, max_turns, connector_id, escalation_level,
                            allow_test_changes, carry_from)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,
                 CASE WHEN $15 THEN (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id = $2 AND status = 'queued')
                      ELSE nextval('task_position_seq') END,
                 $16,$17,$18,$19,$20,$21,$22,$23)
         RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(t.user_id)
    .bind(t.project_id)
    .bind(t.parent_id)
    .bind(t.kind)
    .bind(t.title.chars().take(200).collect::<String>())
    .bind(t.prompt)
    .bind(t.context)
    .bind(t.provider)
    .bind(t.model)
    .bind(t.status)
    .bind(t.attempt)
    .bind(t.depth)
    .bind(t.created_by_agent)
    .bind(t.front)
    .bind(t.limits.max_runtime_secs)
    .bind(t.limits.max_cost_usd)
    .bind(t.limits.max_tokens)
    .bind(t.limits.max_turns)
    .bind(t.connector_id)
    .bind(t.escalation_level)
    .bind(t.allow_test_changes && !t.created_by_agent)
    .bind(t.carry_from)
    .fetch_one(&st.db)
    .await?;
    st.emit(
        task.user_id,
        ServerEvent::Task {
            task: Box::new(task.clone()),
        },
    );
    st.wake.notify_one();
    Ok(task)
}

pub async fn load_task(st: &AppState, id: Uuid) -> sqlx::Result<Option<Task>> {
    sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await
}

pub async fn emit_task(st: &AppState, id: Uuid) {
    if let Ok(Some(t)) = load_task(st, id).await {
        let user = t.user_id;
        st.emit(user, ServerEvent::Task { task: Box::new(t) });
    }
}

/// Persists a task event. Every content passes the secret scanner first:
/// tokens are stored and streamed only as `[geschwärzt: …]` (WP03).
pub async fn event(st: &AppState, task: &Task, kind: &str, content: &str) {
    let content: String = queued_common::secrets::redact(content)
        .chars()
        .take(8000)
        .collect();
    let res: sqlx::Result<(i64, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "INSERT INTO task_events (task_id, kind, content) VALUES ($1,$2,$3) RETURNING id, ts",
    )
    .bind(task.id)
    .bind(kind)
    .bind(&content)
    .fetch_one(&st.db)
    .await;
    if let Ok((id, ts)) = res {
        st.emit(
            task.user_id,
            ServerEvent::TaskEvent {
                task_id: task.id,
                event: TaskEvent {
                    id,
                    task_id: task.id,
                    ts,
                    kind: kind.to_string(),
                    content,
                },
            },
        );
    }
}

pub struct Finish<'a> {
    pub status: &'a str,
    pub summary: Option<String>,
    pub handoff: Option<String>,
    pub error: Option<String>,
    pub review: Option<Value>,
    pub questions: Option<Value>,
    pub usage: &'a Usage,
    /// `None` for a normal end (done / waiting for input).
    pub failure_class: Option<FailureClass>,
}

pub async fn finish(st: &AppState, id: Uuid, f: Finish<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE tasks SET status=$2, summary=COALESCE($3, summary), handoff=COALESCE($4, handoff), error=$5,
                review=COALESCE($6, review), questions=$7, usage=$8, failure_class=$9, not_before=NULL,
                gate=NULL, gate_pending=NULL, finished_at=now(), updated_at=now()
         WHERE id=$1",
    )
    .bind(id)
    .bind(f.status)
    .bind(f.summary.as_deref().map(redact))
    .bind(f.handoff.as_deref().map(redact))
    .bind(f.error.as_deref().map(redact))
    .bind(f.review)
    .bind(f.questions.map(|q| match q {
        Value::Array(a) => Value::Array(
            a.into_iter()
                .map(|v| match v {
                    Value::String(s) => Value::String(redact(&s)),
                    o => o,
                })
                .collect(),
        ),
        o => o,
    }))
    .bind(serde_json::to_value(f.usage).unwrap_or_default())
    .bind(f.failure_class.map(FailureClass::as_str))
    .execute(&st.db)
    .await?;
    emit_task(st, id).await;
    Ok(())
}

/// Puts a running task back into the queue without consuming a fix attempt.
/// `note` (for the next agent) is appended to the context only once.
pub struct Requeue<'a> {
    pub class: FailureClass,
    /// New value of `transient_retries`.
    pub transient_retries: i32,
    pub not_before: Option<DateTime<Utc>>,
    pub note: Option<&'a str>,
    pub error: Option<String>,
    pub usage: &'a Usage,
}

pub async fn requeue(st: &AppState, id: Uuid, r: Requeue<'_>) -> sqlx::Result<bool> {
    let n = sqlx::query(
        "UPDATE tasks SET status='queued', started_at=NULL, updated_at=now(), failure_class=$2,
                transient_retries=$3, not_before=$4, error=$5, usage=$6,
                context = CASE WHEN $7::text IS NULL OR position($7 in context) > 0 THEN context
                               ELSE context || $7 END
         WHERE id=$1 AND status='running'",
    )
    .bind(id)
    .bind(r.class.as_str())
    .bind(r.transient_retries)
    .bind(r.not_before)
    .bind(r.error)
    .bind(serde_json::to_value(r.usage).unwrap_or_default())
    .bind(r.note.map(|n| format!("\n\n{n}")))
    .execute(&st.db)
    .await?
    .rows_affected();
    emit_task(st, id).await;
    if let Some(t) = r.not_before {
        // Wake the scheduler when the backoff is over (it polls only every TICK_SECS).
        let wake = st.wake.clone();
        let d = (t - Utc::now()).to_std().unwrap_or_default();
        tokio::spawn(async move {
            tokio::time::sleep(d).await;
            wake.notify_one();
        });
    }
    st.wake.notify_one();
    Ok(n > 0)
}

pub async fn load_settings(st: &AppState, user_id: Uuid) -> sqlx::Result<Settings> {
    sqlx::query("INSERT INTO user_settings (user_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(user_id)
        .execute(&st.db)
        .await?;
    sqlx::query_as::<_, Settings>("SELECT * FROM user_settings WHERE user_id=$1")
        .bind(user_id)
        .fetch_one(&st.db)
        .await
}

pub async fn load_credential(
    st: &AppState,
    user_id: Uuid,
    provider: &str,
) -> anyhow::Result<Option<Credential>> {
    if provider == "mock" {
        return Ok(Some(Credential::legacy("none", "")));
    }
    let row: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT kind, secret_enc FROM provider_credentials WHERE user_id=$1 AND provider=$2",
    )
    .bind(user_id)
    .bind(provider)
    .fetch_optional(&st.db)
    .await?;
    match row {
        None => Ok(None),
        Some((kind, enc)) => {
            let secret = String::from_utf8(crypto::decrypt(&st.cfg.master_key, &enc)?)?;
            Ok(Some(Credential::legacy(kind, secret)))
        }
    }
}

pub async fn store_credential(
    st: &AppState,
    user_id: Uuid,
    provider: &str,
    kind: &str,
    secret: &str,
) -> anyhow::Result<()> {
    let enc = crypto::encrypt(&st.cfg.master_key, secret.as_bytes())?;
    sqlx::query(
        "INSERT INTO provider_credentials (user_id, provider, kind, secret_enc) VALUES ($1,$2,$3,$4)
         ON CONFLICT (user_id, provider) DO UPDATE SET kind=EXCLUDED.kind, secret_enc=EXCLUDED.secret_enc, updated_at=now()",
    )
    .bind(user_id)
    .bind(provider)
    .bind(kind)
    .bind(enc)
    .execute(&st.db)
    .await?;
    Ok(())
}
