//! Task persistence helpers shared by API, runner and scheduler.
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    agent::Credential,
    crypto,
    models::{Settings, Task, Usage},
    state::AppState,
};

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
}

pub async fn insert_task(st: &AppState, t: NewTask) -> sqlx::Result<Task> {
    let task = sqlx::query_as::<_, Task>(
        "INSERT INTO tasks (id, user_id, project_id, parent_id, kind, title, prompt, context, provider, model, status,
                            attempt, depth, created_by_agent, position)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,
                 CASE WHEN $15 THEN (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id = $2 AND status = 'queued')
                      ELSE nextval('task_position_seq') END)
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
    .fetch_one(&st.db)
    .await?;
    st.emit(task.user_id, json!({"type": "task", "task": task}));
    st.wake.notify_one();
    Ok(task)
}

pub async fn load_task(st: &AppState, id: Uuid) -> sqlx::Result<Option<Task>> {
    sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id=$1").bind(id).fetch_optional(&st.db).await
}

pub async fn emit_task(st: &AppState, id: Uuid) {
    if let Ok(Some(t)) = load_task(st, id).await {
        st.emit(t.user_id, json!({"type": "task", "task": t}));
    }
}

pub async fn event(st: &AppState, task: &Task, kind: &str, content: &str) {
    let content: String = content.chars().take(8000).collect();
    let res: sqlx::Result<(i64, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("INSERT INTO task_events (task_id, kind, content) VALUES ($1,$2,$3) RETURNING id, ts")
            .bind(task.id)
            .bind(kind)
            .bind(&content)
            .fetch_one(&st.db)
            .await;
    if let Ok((id, ts)) = res {
        st.emit(
            task.user_id,
            json!({"type": "task_event", "task_id": task.id, "event": {"id": id, "ts": ts, "kind": kind, "content": content}}),
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
}

pub async fn finish(st: &AppState, id: Uuid, f: Finish<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE tasks SET status=$2, summary=COALESCE($3, summary), handoff=COALESCE($4, handoff), error=$5,
                review=COALESCE($6, review), questions=$7, usage=$8, finished_at=now(), updated_at=now()
         WHERE id=$1",
    )
    .bind(id)
    .bind(f.status)
    .bind(f.summary)
    .bind(f.handoff)
    .bind(f.error)
    .bind(f.review)
    .bind(f.questions)
    .bind(serde_json::to_value(f.usage).unwrap_or_default())
    .execute(&st.db)
    .await?;
    emit_task(st, id).await;
    Ok(())
}

pub async fn load_settings(st: &AppState, user_id: Uuid) -> sqlx::Result<Settings> {
    sqlx::query("INSERT INTO user_settings (user_id) VALUES ($1) ON CONFLICT DO NOTHING").bind(user_id).execute(&st.db).await?;
    sqlx::query_as::<_, Settings>("SELECT * FROM user_settings WHERE user_id=$1").bind(user_id).fetch_one(&st.db).await
}

pub async fn load_credential(st: &AppState, user_id: Uuid, provider: &str) -> anyhow::Result<Option<Credential>> {
    if provider == "mock" {
        return Ok(Some(Credential { kind: "none".into(), secret: String::new() }));
    }
    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT kind, secret_enc FROM provider_credentials WHERE user_id=$1 AND provider=$2")
            .bind(user_id)
            .bind(provider)
            .fetch_optional(&st.db)
            .await?;
    match row {
        None => Ok(None),
        Some((kind, enc)) => {
            let secret = String::from_utf8(crypto::decrypt(&st.cfg.master_key, &enc)?)?;
            Ok(Some(Credential { kind, secret }))
        }
    }
}

pub async fn store_credential(st: &AppState, user_id: Uuid, provider: &str, kind: &str, secret: &str) -> anyhow::Result<()> {
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
