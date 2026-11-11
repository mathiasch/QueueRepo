//! WP06 steering: messages to tasks, "Weiter mit …" tasks and inline diff
//! comments (persistence and delivery bookkeeping; the runs live in
//! `runner_steer.rs`, the endpoints in `api/steer.rs`).
//!
//! Every state change of a message is one conditional UPDATE, so
//! withdrawing and delivering can never both win: a message is delivered
//! exactly when `UPDATE … SET state='delivered' WHERE state='queued'`
//! returned it, and withdrawn exactly when the same condition held for the
//! withdrawal.
use chrono::{DateTime, Utc};
use queued_api::steer::{Delivery, MessageSource};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    models::{status, Task},
    prompt::{CommentItem, MessageItem},
    queue::{self, NewTask, TaskLimits},
    state::AppState,
};

#[derive(Debug, Clone, FromRow)]
pub struct MsgRow {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    pub source: String,
    pub state: String,
    pub urgent: bool,
    pub delivery: Option<String>,
    pub continued_as: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct CommentRow {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Uuid,
    pub path: String,
    pub side: String,
    pub line_start: i32,
    pub line_end: i32,
    pub body: String,
    pub anchor: serde_json::Value,
    pub diff_rev: String,
    pub state: String,
    pub message_id: Option<Uuid>,
    pub sent_task: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

impl CommentRow {
    pub fn anchor_lines(&self) -> Vec<String> {
        serde_json::from_value(self.anchor.clone()).unwrap_or_default()
    }
}

/// Statuses in which a task still takes messages (it will run again).
pub const ACTIVE: [&str; 7] = [
    status::PENDING_APPROVAL,
    status::QUEUED,
    status::RUNNING,
    status::WAITING_BATCH,
    status::WAITING_INPUT,
    status::HELD,
    status::MERGING,
];

/// Statuses after which "Weiter mit …" is possible.
pub const FINAL: [&str; 4] = [
    status::DONE,
    status::FAILED,
    status::BLOCKED,
    status::CANCELLED,
];

pub async fn insert(
    conn: &mut sqlx::PgConnection,
    task: Uuid,
    author: Uuid,
    body: &str,
    source: MessageSource,
    urgent: bool,
) -> sqlx::Result<MsgRow> {
    sqlx::query_as(
        "INSERT INTO task_messages (id, task_id, author_id, body, source, urgent) VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(task)
    .bind(author)
    .bind(body)
    .bind(source.as_str())
    .bind(urgent)
    .fetch_one(conn)
    .await
}

/// Delivers every waiting message of `task` (atomic, see module docs).
pub async fn claim(st: &AppState, task: Uuid, how: Delivery) -> sqlx::Result<Vec<MsgRow>> {
    let mut v: Vec<MsgRow> = sqlx::query_as(
        "UPDATE task_messages SET state='delivered', delivered_at=now(), delivery=$2
         WHERE task_id=$1 AND state='queued' RETURNING *",
    )
    .bind(task)
    .bind(how.as_str())
    .fetch_all(&st.db)
    .await?;
    v.sort_by_key(|m| m.created_at);
    Ok(v)
}

/// All messages delivered to `task` so far, oldest first.
pub async fn delivered(st: &AppState, task: Uuid) -> sqlx::Result<Vec<MsgRow>> {
    sqlx::query_as(
        "SELECT * FROM task_messages WHERE task_id=$1 AND state='delivered' ORDER BY created_at",
    )
    .bind(task)
    .fetch_all(&st.db)
    .await
}

pub async fn has_queued(st: &AppState, task: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM task_messages WHERE task_id=$1 AND state='queued')",
    )
    .bind(task)
    .fetch_one(&st.db)
    .await
}

pub async fn list(st: &AppState, task: Uuid) -> sqlx::Result<Vec<MsgRow>> {
    sqlx::query_as("SELECT * FROM task_messages WHERE task_id=$1 ORDER BY created_at")
        .bind(task)
        .fetch_all(&st.db)
        .await
}

pub async fn user_name(st: &AppState, user: Option<Uuid>) -> String {
    match user {
        Some(u) => sqlx::query_scalar::<_, String>(
            "SELECT COALESCE(NULLIF(name, ''), email) FROM users WHERE id=$1",
        )
        .bind(u)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "?".into()),
        None => "(gelöscht)".into(),
    }
}

/// Prompt items of messages, with the comments bundled in them. `files` is
/// the current diff of the commented task (for the stale/moved mark).
pub async fn items(st: &AppState, msgs: &[MsgRow]) -> sqlx::Result<Vec<MessageItem>> {
    let mut out = vec![];
    for m in msgs {
        let mut comments = vec![];
        if m.source == MessageSource::Comments.as_str() {
            let rows: Vec<CommentRow> = sqlx::query_as(
                "SELECT * FROM diff_comments WHERE message_id=$1 ORDER BY path, line_start, created_at",
            )
            .bind(m.id)
            .fetch_all(&st.db)
            .await?;
            let files = current_diff_files(st, &rows).await;
            for c in rows {
                let side =
                    queued_api::steer::Side::parse(&c.side).unwrap_or(queued_api::steer::Side::New);
                let anchor = c.anchor_lines();
                let now = match files.as_ref() {
                    Some(f) => {
                        match queued_api::diff::locate(f, &c.path, side, &anchor, c.line_start) {
                            queued_api::steer::CommentLocation::Current => {
                                Some((c.line_start, c.line_end))
                            }
                            queued_api::steer::CommentLocation::Moved {
                                line_start,
                                line_end,
                            } => Some((line_start, line_end)),
                            queued_api::steer::CommentLocation::Stale => None,
                        }
                    }
                    None => Some((c.line_start, c.line_end)),
                };
                comments.push(CommentItem {
                    path: c.path.clone(),
                    side_new: side == queued_api::steer::Side::New,
                    line_start: c.line_start,
                    line_end: c.line_end,
                    anchor,
                    body: c.body.clone(),
                    now,
                });
            }
        }
        out.push(MessageItem {
            at: m.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            author: user_name(st, m.author_id).await,
            body: m.body.clone(),
            comments,
        });
    }
    Ok(out)
}

/// Parsed current diff of the task the comments belong to (`None` = the
/// diff is not available; the comments are then taken as they are).
async fn current_diff_files(
    st: &AppState,
    rows: &[CommentRow],
) -> Option<Vec<queued_api::diff::File>> {
    let task = rows.first()?.task_id;
    let t = queue::load_task(st, task).await.ok()??;
    let text = task_diff(st, &t).await.ok()?;
    // No diff any more (e.g. the branch of a long-finished task is gone):
    // the comments count as they were, never as outdated.
    (!text.is_empty()).then(|| queued_api::diff::parse(&text))
}

/// The task's diff as the API shows it (secrets redacted), `""` if none.
pub async fn task_diff(st: &AppState, t: &Task) -> anyhow::Result<String> {
    let (Some(a), Some(b)) = (t.diff_base(), t.snapshot_after.as_deref()) else {
        return Ok(String::new());
    };
    match st.runner.diff(t.project_id, a, b, 2_000_000).await {
        Ok(d) => Ok(queued_common::secrets::redact(&d.diff)),
        Err(e) if crate::runner_client::runner_status(&e) == Some(422) => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Logs a message's lifecycle in the task log (the drawer refreshes on it).
pub async fn log(st: &AppState, task: &Task, text: &str) {
    queue::event(st, task, "steer", text).await;
}

/// "Weiter mit …": a new task that continues `origin`. Waiting messages of
/// the origin and the given comments go with it. Runs in `tx`; the caller
/// announces the task after the commit.
#[allow(clippy::too_many_arguments)]
pub async fn continuation(
    tx: &mut sqlx::PgConnection,
    origin: &Task,
    user: Uuid,
    text: &str,
    plan_mode: bool,
    comments: &[Uuid],
    comment_note: Option<&str>,
) -> sqlx::Result<(Task, Option<Uuid>)> {
    let title: String = format!("Weiter: {}", origin.title)
        .chars()
        .take(200)
        .collect();
    // The agent continues the origin's changes when they are not on the
    // target yet (like a fix task, WP04), on the same pull request (WP05).
    let carry = (!origin.on_target() && origin.branch.is_some()).then_some(origin.id);
    let mut task = queue::insert_task_in(
        tx,
        NewTask {
            user_id: user,
            project_id: origin.project_id,
            parent_id: Some(origin.id),
            kind: "continue",
            title,
            prompt: text.to_string(),
            context: String::new(),
            provider: origin.provider.clone(),
            model: origin.model.clone(),
            status: status::QUEUED,
            attempt: 0,
            depth: origin.depth,
            created_by_agent: false,
            front: true,
            limits: TaskLimits::of(origin),
            connector_id: origin.connector_id,
            escalation_level: origin.escalation_level,
            allow_test_changes: false,
            carry_from: carry,
            pr_id: origin.pr_id,
            plan_mode,
        },
    )
    .await?;
    // The origin's CLI session only for its own author (a transcript never
    // goes to another user), with the same agent.
    let session_from =
        (origin.user_id == user && origin.agent_session.is_some()).then_some(origin.id);
    task = sqlx::query_as("UPDATE tasks SET session_from=$2 WHERE id=$1 RETURNING *")
        .bind(task.id)
        .bind(session_from)
        .fetch_one(&mut *tx)
        .await?;
    // Messages still waiting on the origin move to the new task.
    let moved: Vec<MsgRow> = sqlx::query_as(
        "UPDATE task_messages SET state='delivered', delivered_at=now(), delivery='continuation', continued_as=$2
         WHERE task_id=$1 AND state='queued' RETURNING *",
    )
    .bind(origin.id)
    .bind(task.id)
    .fetch_all(&mut *tx)
    .await?;
    let mut moved = moved;
    moved.sort_by_key(|m| m.created_at);
    for m in &moved {
        let copy = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO task_messages (id, task_id, author_id, body, source, created_at) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(copy)
        .bind(task.id)
        .bind(m.author_id)
        .bind(&m.body)
        .bind(&m.source)
        .bind(m.created_at)
        .execute(&mut *tx)
        .await?;
        // Comments bundled in a moved message go with it.
        sqlx::query("UPDATE diff_comments SET message_id=$2, sent_task=$3 WHERE message_id=$1")
            .bind(m.id)
            .bind(copy)
            .bind(task.id)
            .execute(&mut *tx)
            .await?;
    }
    let mut msg_id = None;
    if !comments.is_empty() {
        let body = comment_note
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} Kommentar(e) zu den Änderungen", comments.len()));
        let m = insert(tx, task.id, user, &body, MessageSource::Comments, false).await?;
        sqlx::query(
            "UPDATE diff_comments SET state='sent', sent_at=now(), message_id=$2, sent_task=$3, updated_at=now()
             WHERE id = ANY($1) AND state='draft'",
        )
        .bind(comments)
        .bind(m.id)
        .bind(task.id)
        .execute(&mut *tx)
        .await?;
        msg_id = Some(m.id);
    }
    Ok((task, msg_id))
}

/// After a task reached an end state: messages that never reached its agent.
/// `done` ⇒ a "Weiter mit …" task gets them (they came after the agent's
/// work phase, e.g. during verify/review/merge); `cancelled` ⇒ dropped;
/// `failed`/`blocked` ⇒ they keep waiting (delivered on retry or with
/// "Weiter mit …"; never an automatic paid run after a limit stop).
pub async fn settle(st: &AppState, task_id: Uuid) -> anyhow::Result<()> {
    let Some(t) = queue::load_task(st, task_id).await? else {
        return Ok(());
    };
    match t.status.as_str() {
        status::CANCELLED => {
            let n = sqlx::query(
                "UPDATE task_messages SET state='dropped' WHERE task_id=$1 AND state='queued'",
            )
            .bind(t.id)
            .execute(&st.db)
            .await?
            .rows_affected();
            if n > 0 {
                log(
                    st,
                    &t,
                    &format!("{n} Nachricht(en) nicht zugestellt: Auftrag abgebrochen"),
                )
                .await;
            }
        }
        status::DONE => {
            if !has_queued(st, t.id).await? {
                return Ok(());
            }
            let mut tx = st.db.begin().await?;
            // Lock the task row: two settles must not both continue it.
            let still: Option<String> =
                sqlx::query_scalar("SELECT status FROM tasks WHERE id=$1 FOR UPDATE")
                    .bind(t.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let queued: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM task_messages WHERE task_id=$1 AND state='queued')",
            )
            .bind(t.id)
            .fetch_one(&mut *tx)
            .await?;
            if still.as_deref() != Some(status::DONE) || !queued {
                return Ok(());
            }
            let (next, _) = continuation(
                &mut tx,
                &t,
                t.user_id,
                "Setze die Arbeit des vorherigen Auftrags mit den Nachrichten des Nutzers fort.",
                false,
                &[],
                None,
            )
            .await?;
            tx.commit().await?;
            queue::announce(st, &next);
            log(
                st,
                &t,
                &format!(
                    "Nachricht(en) kamen nach der Arbeitsphase – werden als Fortsetzung „{}“ zugestellt",
                    next.title
                ),
            )
            .await;
        }
        _ => {}
    }
    Ok(())
}

/// Safety net in the scheduler tick: tasks that ended while messages waited.
pub async fn sweep(st: &AppState) -> anyhow::Result<()> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT m.task_id FROM task_messages m JOIN tasks t ON t.id = m.task_id
         WHERE m.state='queued' AND t.status IN ('done','cancelled') LIMIT 50",
    )
    .fetch_all(&st.db)
    .await?;
    for id in ids {
        if let Err(e) = settle(st, id).await {
            tracing::warn!("task {id}: messages not settled: {e:#}");
        }
    }
    Ok(())
}
