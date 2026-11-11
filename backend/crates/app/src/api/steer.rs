//! WP06 endpoints: plan decisions, messages to tasks ("Nachricht an den
//! Agenten", withdraw), "Weiter mit …" and inline diff comments.
//!
//! Rights as in the existing APIs: steering a task (plan decisions,
//! messages) is for its creator – while still at least editor in the
//! project – and for the project owner (like gate decisions, WP03); a
//! "Weiter mit …" task is a new task of the caller (editor, like creating
//! one); reading is for every member (viewer); strangers get 404.
use axum::{
    extract::{Path, State},
    Json,
};
use queued_api::{
    diff,
    steer::{
        validate_range, validate_text, CommentLocation, CommentView, ContinueTask, Delivery,
        MessageSource, MessageState, NewComment, PlanApprove, PlanRevise, SendComments,
        SendMessage, SentFollowUp, Side, TaskComments, TaskMessageView, TaskMessages,
        UpdateComment, MAX_COMMENTS, MAX_COMMENT_CHARS, MAX_MESSAGE_CHARS, MAX_NOTE_CHARS,
        MAX_PLAN_CHARS, MAX_QUEUED_MESSAGES, MAX_STEER_ROUNDS,
    },
    Task,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{require, role, Role};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::status,
    plan::{self, Decided},
    queue,
    state::AppState,
    steer::{self, CommentRow, MsgRow, ACTIVE, FINAL},
};

async fn load(st: &AppState, id: Uuid) -> ApiResult<Task> {
    queue::load_task(st, id)
        .await?
        .ok_or_else(AppError::not_found)
}

/// The task, if the caller may steer it (creator with role ≥ editor, or owner).
pub async fn steerable(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<(Task, Role)> {
    let t = load(st, id).await?;
    match role(st, t.project_id, user).await? {
        Some(Role::Owner) => Ok((t, Role::Owner)),
        Some(Role::Editor) if t.user_id == user => Ok((t, Role::Editor)),
        Some(_) => Err(AppError::forbidden()),
        None => Err(AppError::not_found()),
    }
}

/// The task, if the caller may read it (any member; the creator only while
/// still a member).
async fn viewable(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<(Task, Role)> {
    let t = load(st, id).await?;
    match role(st, t.project_id, user).await? {
        Some(r) => Ok((t, r)),
        None => Err(AppError::not_found()),
    }
}

fn may_steer(t: &Task, user: Uuid, r: Role) -> bool {
    r == Role::Owner || (r == Role::Editor && t.user_id == user)
}

async fn name(st: &AppState, user: Uuid) -> String {
    steer::user_name(st, Some(user)).await
}

fn bad(e: String) -> AppError {
    AppError::bad_request(e)
}

// ---------------------------------------------------------------------------
// Plan decisions
// ---------------------------------------------------------------------------

fn plan_held(t: &Task) -> ApiResult<()> {
    let held = t.status == status::HELD
        && t.gate_parsed()
            .is_some_and(|g| g.stage == queued_api::gate::GateStage::Plan);
    if held {
        Ok(())
    } else {
        Err(AppError::conflict(
            "Der Auftrag wartet nicht auf eine Plan-Freigabe",
        ))
    }
}

fn decided(d: Decided) -> ApiResult<Json<Value>> {
    match d {
        Decided::Ok => Ok(Json(json!({"ok": true}))),
        Decided::NotHeld => Err(AppError::conflict(
            "Der Auftrag wartet nicht (mehr) auf eine Plan-Freigabe",
        )),
    }
}

pub async fn approve_plan(
    st: &AppState,
    t: &Task,
    user: Uuid,
    edited: Option<String>,
) -> ApiResult<Json<Value>> {
    plan_held(t)?;
    let edited = match edited {
        Some(p) => {
            let p = validate_text("Der Plan", &p, MAX_PLAN_CHARS).map_err(bad)?;
            // Unchanged text is no edit.
            (Some(p.as_str()) != t.plan.as_deref().map(str::trim)).then_some(p)
        }
        None => None,
    };
    let who = name(st, user).await;
    decided(plan::approve(st, t, &who, edited, false).await?)
}

pub async fn reject_plan(st: &AppState, t: &Task, user: Uuid) -> ApiResult<Json<Value>> {
    plan_held(t)?;
    let who = name(st, user).await;
    decided(plan::reject(st, t, &who).await?)
}

/// `POST /api/tasks/{id}/plan/approve`.
pub async fn plan_approve(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(a): Json<PlanApprove>,
) -> ApiResult<Json<Value>> {
    let (t, _) = steerable(&st, id, u.id).await?;
    approve_plan(&st, &t, u.id, a.plan).await
}

/// `POST /api/tasks/{id}/plan/revise`.
pub async fn plan_revise(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(r): Json<PlanRevise>,
) -> ApiResult<Json<Value>> {
    let (t, _) = steerable(&st, id, u.id).await?;
    plan_held(&t)?;
    let note = validate_text("Die Anmerkung", &r.note, MAX_NOTE_CHARS).map_err(bad)?;
    let who = name(&st, u.id).await;
    match plan::revise(&st, &t, &who, &note).await? {
        Ok(d) => decided(d),
        Err(e) => Err(AppError::conflict(e)),
    }
}

/// `POST /api/tasks/{id}/plan/reject`.
pub async fn plan_reject(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (t, _) = steerable(&st, id, u.id).await?;
    reject_plan(&st, &t, u.id).await
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

async fn view_of(st: &AppState, m: &MsgRow, user: Uuid, owner: bool) -> TaskMessageView {
    let state = MessageState::parse(&m.state).unwrap_or(MessageState::Queued);
    TaskMessageView {
        id: m.id,
        task_id: m.task_id,
        author_id: m.author_id,
        author: steer::user_name(st, m.author_id).await,
        body: m.body.clone(),
        source: if m.source == "comments" {
            MessageSource::Comments
        } else {
            MessageSource::Message
        },
        state,
        urgent: m.urgent,
        delivery: m.delivery.as_deref().and_then(Delivery::parse),
        continued_as: m.continued_as,
        created_at: m.created_at,
        delivered_at: m.delivered_at,
        can_withdraw: state == MessageState::Queued && (owner || m.author_id == Some(user)),
    }
}

/// `GET /api/tasks/{id}/messages`.
pub async fn messages(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TaskMessages>> {
    let (t, r) = viewable(&st, id, u.id).await?;
    let mut out = vec![];
    for m in steer::list(&st, id).await? {
        out.push(view_of(&st, &m, u.id, r == Role::Owner).await);
    }
    Ok(Json(TaskMessages {
        messages: out,
        can_send: may_steer(&t, u.id, r) && ACTIVE.contains(&t.status.as_str()),
        can_continue: r >= Role::Editor && FINAL.contains(&t.status.as_str()),
        running: t.status == status::RUNNING,
    }))
}

/// `POST /api/tasks/{id}/messages`.
pub async fn send(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(m): Json<SendMessage>,
) -> ApiResult<Json<TaskMessageView>> {
    let (t, r) = steerable(&st, id, u.id).await?;
    let body = validate_text("Die Nachricht", &m.body, MAX_MESSAGE_CHARS).map_err(bad)?;
    let msg = post_message(&st, &t, u.id, &body, MessageSource::Message, m.now, &[]).await?;
    Ok(Json(view_of(&st, &msg, u.id, r == Role::Owner).await))
}

/// Queues a message for an active task (shared by messages and comments).
async fn post_message(
    st: &AppState,
    t: &Task,
    user: Uuid,
    body: &str,
    source: MessageSource,
    now: bool,
    comments: &[Uuid],
) -> ApiResult<MsgRow> {
    if !ACTIVE.contains(&t.status.as_str()) {
        return Err(AppError::conflict(
            "Der Auftrag ist beendet – nutze „Weiter mit …“",
        ));
    }
    let mut tx = st.db.begin().await?;
    // Serialises with the scheduler's claim (the row lock).
    let current: Option<String> =
        sqlx::query_scalar("SELECT status FROM tasks WHERE id=$1 FOR UPDATE")
            .bind(t.id)
            .fetch_optional(&mut *tx)
            .await?;
    let current = current.ok_or_else(AppError::not_found)?;
    if !ACTIVE.contains(&current.as_str()) {
        return Err(AppError::conflict(
            "Der Auftrag ist gerade fertig geworden – nutze „Weiter mit …“",
        ));
    }
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM task_messages WHERE task_id=$1 AND state='queued'",
    )
    .bind(t.id)
    .fetch_one(&mut *tx)
    .await?;
    if queued >= MAX_QUEUED_MESSAGES {
        return Err(AppError::conflict(format!(
            "Höchstens {MAX_QUEUED_MESSAGES} wartende Nachrichten pro Auftrag"
        )));
    }
    let urgent = now && current == status::RUNNING && t.steer_rounds < MAX_STEER_ROUNDS;
    let msg = steer::insert(&mut tx, t.id, user, body, source, urgent).await?;
    // Bundled comments in the same transaction: the agent never sees the
    // message without them.
    if !comments.is_empty() {
        sqlx::query(
            "UPDATE diff_comments SET state='sent', sent_at=now(), message_id=$2, sent_task=$3, updated_at=now()
             WHERE id = ANY($1) AND state='draft'",
        )
        .bind(comments)
        .bind(msg.id)
        .bind(t.id)
        .execute(&mut *tx)
        .await?;
    }
    // A task waiting for an answer: the message is the answer.
    let answered = current == status::WAITING_INPUT;
    if answered {
        let questions: Vec<String> = t
            .questions
            .clone()
            .and_then(|q| serde_json::from_value(q).ok())
            .unwrap_or_default();
        let mut ctx = String::from("\n\n## Answers from user\n");
        for q in &questions {
            ctx.push_str(&format!("Q: {}\n", q.chars().take(500).collect::<String>()));
        }
        ctx.push_str("A: see the messages from the user below\n");
        sqlx::query(
            "UPDATE tasks SET status='queued', questions=NULL, context = context || $2, updated_at=now(),
                    position = (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id=$3 AND status='queued')
             WHERE id=$1 AND status='waiting_input'",
        )
        .bind(t.id)
        .bind(ctx)
        .bind(t.user_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let who = name(st, user).await;
    let what = match source {
        MessageSource::Comments => "Kommentare",
        MessageSource::Message => "Nachricht",
    };
    let mut text = format!("{what} von {who} – wartet auf Zustellung");
    if answered {
        text = format!("{what} von {who} beantwortet die Rückfrage – der Auftrag geht weiter");
    } else if now {
        if urgent && st.steer_now(t.id) {
            text = format!("{what} von {who} – der Agent wird unterbrochen und bekommt sie sofort");
        } else {
            text.push_str(" (der Agent arbeitet gerade nicht an einem Schritt, der sich unterbrechen lässt – sie wird beim nächsten Halt zugestellt)");
        }
    }
    steer::log(st, t, &text).await;
    queue::emit_task(st, t.id).await;
    st.wake.notify_one();
    Ok(msg)
}

/// `DELETE /api/tasks/{id}/messages/{mid}`: withdraw before delivery.
pub async fn withdraw(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, mid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    let (t, r) = viewable(&st, id, u.id).await?;
    let m: MsgRow = sqlx::query_as("SELECT * FROM task_messages WHERE id=$1 AND task_id=$2")
        .bind(mid)
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::not_found)?;
    let author = m.author_id == Some(u.id) && r >= Role::Editor;
    if !author && r != Role::Owner {
        return Err(AppError::forbidden());
    }
    let n = sqlx::query(
        "UPDATE task_messages SET state='withdrawn', withdrawn_at=now() WHERE id=$1 AND task_id=$2 AND state='queued'",
    )
    .bind(mid)
    .bind(id)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::conflict(
            "Die Nachricht ist schon beim Agenten angekommen und lässt sich nicht mehr zurückziehen",
        ));
    }
    // Comments bundled in it become drafts again.
    sqlx::query(
        "UPDATE diff_comments SET state='draft', message_id=NULL, sent_task=NULL, sent_at=NULL, updated_at=now() WHERE message_id=$1",
    )
    .bind(mid)
    .execute(&st.db)
    .await?;
    let who = name(&st, u.id).await;
    steer::log(&st, &t, &format!("Nachricht zurückgezogen (von {who})")).await;
    queue::emit_task(&st, id).await;
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// "Weiter mit …"
// ---------------------------------------------------------------------------

async fn start_continuation(
    st: &AppState,
    t: &Task,
    user: Uuid,
    text: &str,
    plan_mode: Option<bool>,
    comments: &[Uuid],
    note: Option<&str>,
) -> ApiResult<(Task, Option<Uuid>)> {
    if !FINAL.contains(&t.status.as_str()) {
        return Err(AppError::conflict(
            "Der Auftrag läuft noch – schick ihm stattdessen eine Nachricht",
        ));
    }
    let project: crate::models::Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(t.project_id)
        .fetch_one(&st.db)
        .await?;
    // Same agent and access as the origin, checked for the caller like a
    // new task (access rights, data residency).
    let settings = queue::load_settings(st, user).await?;
    let exec = crate::routing::resolve_execute(
        st,
        user,
        &project,
        t.connector_id,
        t.connector_id.is_none().then_some(t.provider.as_str()),
        t.model.clone(),
        &settings.default_provider,
    )
    .await?
    .map_err(AppError::bad_request)?;
    if exec.provider != t.provider || exec.connector_id != t.connector_id {
        return Err(AppError::bad_request(
            "Der Agent des Auftrags ist für dich nicht verfügbar",
        ));
    }
    let mut tx = st.db.begin().await?;
    let (next, msg) = steer::continuation(
        &mut tx,
        t,
        user,
        text,
        plan_mode.unwrap_or(project.plan_mode),
        comments,
        note,
    )
    .await?;
    tx.commit().await?;
    queue::announce(st, &next);
    let who = name(st, user).await;
    steer::log(
        st,
        t,
        &format!("Fortgesetzt als „{}“ (von {who})", next.title),
    )
    .await;
    queue::event(st, &next, "steer", &format!("Setzt „{}“ fort", t.title)).await;
    Ok((next, msg))
}

/// `POST /api/tasks/{id}/continue`.
pub async fn continue_task(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(c): Json<ContinueTask>,
) -> ApiResult<Json<Task>> {
    let t = load(&st, id).await?;
    require(&st, t.project_id, u.id, Role::Editor).await?;
    let text = validate_text("Die Nachricht", &c.message, MAX_MESSAGE_CHARS).map_err(bad)?;
    let (next, _) = start_continuation(&st, &t, u.id, &text, c.plan_mode, &[], None).await?;
    Ok(Json(next))
}

// ---------------------------------------------------------------------------
// Inline comments
// ---------------------------------------------------------------------------

fn locate(files: Option<&[diff::File]>, c: &CommentRow) -> CommentLocation {
    let side = Side::parse(&c.side).unwrap_or(Side::New);
    match files {
        Some(f) => diff::locate(f, &c.path, side, &c.anchor_lines(), c.line_start),
        // The diff is gone (e.g. long-finished task): as it was.
        None => CommentLocation::Current,
    }
}

async fn comment_view(
    st: &AppState,
    c: &CommentRow,
    user: Uuid,
    files: Option<&[diff::File]>,
) -> CommentView {
    CommentView {
        id: c.id,
        task_id: c.task_id,
        author_id: c.author_id,
        author: steer::user_name(st, Some(c.author_id)).await,
        path: c.path.clone(),
        side: Side::parse(&c.side).unwrap_or(Side::New),
        line_start: c.line_start,
        line_end: c.line_end,
        body: c.body.clone(),
        anchor: c.anchor_lines(),
        sent: c.state == "sent",
        location: locate(files, c),
        created_at: c.created_at,
        sent_task: c.sent_task,
        mine: c.author_id == user,
    }
}

async fn diff_files(st: &AppState, t: &Task) -> Option<Vec<diff::File>> {
    let text = steer::task_diff(st, t).await.ok()?;
    (!text.is_empty()).then(|| diff::parse(&text))
}

/// `GET /api/tasks/{id}/comments`: own drafts and every sent comment.
pub async fn comments(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TaskComments>> {
    let (t, r) = viewable(&st, id, u.id).await?;
    let rows: Vec<CommentRow> = sqlx::query_as(
        "SELECT * FROM diff_comments WHERE task_id=$1 AND (state='sent' OR author_id=$2)
         ORDER BY path, line_start, created_at",
    )
    .bind(id)
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    let files = if rows.is_empty() {
        None
    } else {
        diff_files(&st, &t).await
    };
    let mut out = vec![];
    for c in &rows {
        out.push(comment_view(&st, c, u.id, files.as_deref()).await);
    }
    Ok(Json(TaskComments {
        comments: out,
        can_comment: r >= Role::Editor && t.snapshot_after.is_some(),
    }))
}

/// `POST /api/tasks/{id}/comments` (a draft on the task's current diff).
pub async fn add_comment(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(c): Json<NewComment>,
) -> ApiResult<Json<CommentView>> {
    let (t, r) = viewable(&st, id, u.id).await?;
    if r < Role::Editor {
        return Err(AppError::forbidden());
    }
    let body = validate_text("Der Kommentar", &c.body, MAX_COMMENT_CHARS).map_err(bad)?;
    validate_range(c.line_start, c.line_end).map_err(bad)?;
    if c.path.is_empty() || c.path.chars().count() > 1000 {
        return Err(bad("Ungültiger Dateipfad".into()));
    }
    let Some(rev) = t.snapshot_after.clone() else {
        return Err(AppError::conflict("Der Auftrag hat noch keine Änderungen"));
    };
    let drafts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM diff_comments WHERE task_id=$1 AND author_id=$2 AND state='draft'",
    )
    .bind(id)
    .bind(u.id)
    .fetch_one(&st.db)
    .await?;
    if drafts >= MAX_COMMENTS {
        return Err(AppError::conflict(format!(
            "Höchstens {MAX_COMMENTS} gesammelte Kommentare – sende sie zuerst"
        )));
    }
    let files = diff_files(&st, &t).await.ok_or_else(|| {
        AppError::conflict("Die Änderungen des Auftrags sind nicht mehr verfügbar")
    })?;
    let f = diff::file(&files, &c.path)
        .ok_or_else(|| bad("Diese Datei ist nicht in den Änderungen".into()))?;
    let lines = diff::lines_of(f, c.side, c.line_start, c.line_end).ok_or_else(|| {
        bad("Diese Zeilen sind nicht in den Änderungen (oder nicht in einem Abschnitt)".into())
    })?;
    let anchor: Vec<String> = lines
        .into_iter()
        .map(|l| l.chars().take(2000).collect())
        .collect();
    let row: CommentRow = sqlx::query_as(
        "INSERT INTO diff_comments (id, task_id, author_id, path, side, line_start, line_end, body, anchor, diff_rev)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(u.id)
    .bind(&c.path)
    .bind(c.side.as_str())
    .bind(c.line_start)
    .bind(c.line_end)
    .bind(&body)
    .bind(serde_json::to_value(&anchor).unwrap_or_default())
    .bind(&rev)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(comment_view(&st, &row, u.id, Some(&files)).await))
}

async fn own_draft(
    st: &AppState,
    id: Uuid,
    cid: Uuid,
    user: Uuid,
) -> ApiResult<(Task, CommentRow)> {
    let (t, r) = viewable(st, id, user).await?;
    let c: CommentRow = sqlx::query_as("SELECT * FROM diff_comments WHERE id=$1 AND task_id=$2")
        .bind(cid)
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::not_found)?;
    if c.author_id != user || r < Role::Editor {
        return Err(AppError::forbidden());
    }
    if c.state != "draft" {
        return Err(AppError::conflict("Der Kommentar ist schon gesendet"));
    }
    Ok((t, c))
}

/// `PATCH /api/tasks/{id}/comments/{cid}`.
pub async fn update_comment(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, cid)): Path<(Uuid, Uuid)>,
    Json(c): Json<UpdateComment>,
) -> ApiResult<Json<Value>> {
    own_draft(&st, id, cid, u.id).await?;
    let body = validate_text("Der Kommentar", &c.body, MAX_COMMENT_CHARS).map_err(bad)?;
    sqlx::query("UPDATE diff_comments SET body=$2, updated_at=now() WHERE id=$1 AND state='draft'")
        .bind(cid)
        .bind(body)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `DELETE /api/tasks/{id}/comments/{cid}`.
pub async fn delete_comment(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, cid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Value>> {
    own_draft(&st, id, cid, u.id).await?;
    sqlx::query("DELETE FROM diff_comments WHERE id=$1 AND state='draft'")
        .bind(cid)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `POST /api/tasks/{id}/comments/send`: all own drafts as ONE follow-up –
/// a message for an active task, a "Weiter mit …" task for a finished one.
pub async fn send_comments(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(s): Json<SendComments>,
) -> ApiResult<Json<SentFollowUp>> {
    let (t, r) = viewable(&st, id, u.id).await?;
    if r < Role::Editor {
        return Err(AppError::forbidden());
    }
    let note = match s.message.as_deref().filter(|m| !m.trim().is_empty()) {
        Some(m) => Some(validate_text("Die Nachricht", m, MAX_MESSAGE_CHARS).map_err(bad)?),
        None => None,
    };
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM diff_comments WHERE task_id=$1 AND author_id=$2 AND state='draft' ORDER BY created_at",
    )
    .bind(id)
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    if ids.is_empty() {
        return Err(bad("Keine gesammelten Kommentare".into()));
    }
    let n = ids.len();
    let body = note
        .clone()
        .unwrap_or_else(|| format!("{n} Kommentar(e) zu den Änderungen"));
    if FINAL.contains(&t.status.as_str()) {
        let text = note.unwrap_or_else(|| {
            format!(
                "Setze die {n} Kommentar(e) zu den Änderungen von „{}“ um.",
                t.title
            )
        });
        let (next, msg) = start_continuation(
            &st,
            &t,
            u.id,
            &text,
            None,
            &ids,
            Some(&format!("{n} Kommentar(e) zu den Änderungen")),
        )
        .await?;
        return Ok(Json(SentFollowUp {
            delivered_as: "continuation".into(),
            task_id: next.id,
            message_id: msg,
            comments: n,
        }));
    }
    if !may_steer(&t, u.id, r) {
        return Err(AppError::forbidden());
    }
    let msg = post_message(&st, &t, u.id, &body, MessageSource::Comments, false, &ids).await?;
    Ok(Json(SentFollowUp {
        delivered_as: "message".into(),
        task_id: t.id,
        message_id: Some(msg.id),
        comments: n,
    }))
}
