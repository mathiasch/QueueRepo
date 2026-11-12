//! WP08: intake integrations – the common "intake source" abstraction.
//!
//! Inbound (`api::intake`): every kind verifies its signature with the
//! HMAC primitives of WP07 (`webhooks::verify`), parses its delivery into an
//! [`Incoming`] (new request, reply, ignore, challenge) and hands it to
//! [`accept`], which decides everything in **one** transaction under the
//! integration's row lock: tenant, dedupe, external → internal person,
//! rights, rate limit, "a task for this object is still open", then task +
//! link + outbox row (or a WP06 message / "Weiter mit …").
//!
//! Outbound: status updates only through `intake_outbox`, delivered by
//! [`run`] (own Tokio task, never the scheduler): lease per row, retry with
//! backoff, `Retry-After`, in order per link, idempotent per phase (UNIQUE
//! key) and – for a lost answer – per remote object (marker / chosen ID
//! looked up before a retry). Calls only through the SSRF-guarded client,
//! no redirects, the token only in a header and never in a text.
pub mod github;
pub mod jira;
pub mod linear;
pub mod slack;

use std::time::Duration;

use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use queued_api::{
    intake::{IntakeConfig, IntakeKind, IntakeOutcome, Phase},
    steer::{MessageSource, MAX_MESSAGE_CHARS},
};
use serde_json::{json, Value};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::{
    api::projects::{role, Role},
    crypto,
    error::AppError,
    llm,
    models::{status, Project, Task},
    queue::{self, NewTask, TaskLimits},
    state::AppState,
    steer::{ACTIVE, FINAL},
};

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromRow)]
pub struct IntegrationRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_id: Uuid,
    pub kind: String,
    pub name: String,
    pub tenant: String,
    pub enabled: bool,
    pub secret_enc: Vec<u8>,
    pub token_enc: Option<Vec<u8>>,
    pub account: Option<String>,
    pub config: Value,
    pub max_per_hour: i32,
    pub disabled_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl IntegrationRow {
    pub fn kind(&self) -> IntakeKind {
        IntakeKind::parse(&self.kind).unwrap_or(IntakeKind::Github)
    }

    pub fn config(&self) -> IntakeConfig {
        serde_json::from_value(self.config.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct LinkRow {
    pub id: Uuid,
    pub integration_id: Uuid,
    pub task_id: Uuid,
    pub external_ref: String,
    pub external_url: Option<String>,
    pub external_title: Option<String>,
    pub reply_to: Value,
    pub reported: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OutRow {
    pub id: Uuid,
    pub integration_id: Uuid,
    pub link_id: Option<Uuid>,
    pub key: String,
    pub kind: String,
    pub body: String,
    pub reply_to: Value,
    pub remote_id: Option<String>,
    pub state: String,
    pub attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub lease_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Inbound: what a source makes of a delivery
// ---------------------------------------------------------------------------

/// The external person behind a delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    /// Stable ID (GitHub numeric user ID, Slack user ID, Linear user ID,
    /// Jira account ID).
    pub id: String,
    /// Display name (untrusted, history only).
    pub name: String,
    /// The host says this person may write (GitHub `author_association`
    /// OWNER/MEMBER/COLLABORATOR or a label change, which needs triage
    /// rights). Only used for `allow_unmapped`; `None` = no such signal.
    pub host_trusted: Option<bool>,
}

/// A request for a new task.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub actor: Actor,
    /// Key of the external object (links are looked up by it).
    pub external_ref: String,
    pub external_url: Option<String>,
    /// Untrusted title (display only).
    pub external_title: Option<String>,
    /// Trusted label for the task title, e.g. "GitHub-Issue #12".
    pub label: String,
    /// Untrusted data for the agent (header, text).
    pub entries: Vec<(String, String)>,
    /// Where updates go.
    pub reply_to: Value,
    /// An open task for the same object makes this a message to it (Slack
    /// mention in a thread, mention in a comment) instead of "läuft schon".
    pub reply_if_open: bool,
    /// The text for that message.
    pub reply_text: String,
    /// A new task even if the object already had one (GitHub `reopened`).
    /// Otherwise an object gets one task (GitHub sends `opened` and
    /// `labeled` for one new issue; a label added again is no new order).
    pub restart: bool,
}

/// A reply to a linked task (answer, steering, "Weiter mit …").
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub actor: Actor,
    pub external_ref: String,
    pub text: String,
    /// Only while the task waits for an answer (plain comments and thread
    /// messages: human chatter must not reach the agent otherwise).
    pub require_waiting: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// Answer the request with this JSON (Slack URL verification).
    Challenge(Value),
    Ignore(String),
    Request(Box<Request>),
    Reply(Reply),
}

/// A verified, parsed delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    /// Event name for the history (`issues.labeled`, `app_mention`, …).
    pub event: String,
    /// Tenant named in the delivery (`None` = none, e.g. a ping).
    pub tenant: Option<String>,
    /// Dedupe keys (unique per integration).
    pub delivery_id: Option<String>,
    pub body_sha256: Option<String>,
    pub parsed: Parsed,
    /// Slack slash command: the HTTP answer is a message for the user.
    pub slash: bool,
    /// Linear: the session a refusal has to answer (it would hang).
    pub refuse_to: Option<Value>,
}

impl Incoming {
    pub fn new(event: impl Into<String>, parsed: Parsed) -> Self {
        Self {
            event: event.into().chars().take(100).collect(),
            tenant: None,
            delivery_id: None,
            body_sha256: None,
            parsed,
            slash: false,
            refuse_to: None,
        }
    }
}

/// Verifies the signature of a delivery (`Err` = reason, 401).
pub fn verify(
    kind: IntakeKind,
    headers: &HeaderMap,
    body: &[u8],
    secret: &[u8],
    now: DateTime<Utc>,
) -> Result<(), String> {
    match kind {
        IntakeKind::Github => github::verify(headers, body, secret),
        IntakeKind::Slack => slack::verify(headers, body, secret, now),
        IntakeKind::Linear => linear::verify(headers, body, secret, now),
        IntakeKind::Jira => jira::verify(headers, body, secret),
    }
}

/// Parses a verified delivery (`Err` = reason, 400).
pub fn parse(
    kind: IntakeKind,
    headers: &HeaderMap,
    body: &[u8],
    cfg: &IntakeConfig,
    tenant: &str,
) -> Result<Incoming, String> {
    let mut inc = match kind {
        IntakeKind::Github => github::parse(headers, body, cfg)?,
        IntakeKind::Slack => slack::parse(headers, body)?,
        IntakeKind::Linear => linear::parse(body)?,
        IntakeKind::Jira => jira::parse(headers, body, cfg, tenant)?,
    };
    if inc.body_sha256.is_none() && matches!(kind, IntakeKind::Github | IntakeKind::Jira) {
        // GitHub and Jira sign no timestamp: the body hash is a second
        // replay guard next to the delivery ID.
        inc.body_sha256 = Some(crypto::sha256_hex_bytes(body));
    }
    Ok(inc)
}

pub fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

/// First `max` characters (untrusted text limit).
pub fn chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n[… gekürzt]");
    out
}

// ---------------------------------------------------------------------------
// Accept: one transaction per delivery
// ---------------------------------------------------------------------------

/// What happened (for the HTTP answer and the history).
#[derive(Debug, Clone, PartialEq)]
pub enum Accepted {
    Duplicate,
    Done {
        outcome: IntakeOutcome,
        reason: Option<String>,
        task: Option<Uuid>,
    },
}

/// Work after the commit (announce tasks, log messages, wake loops).
#[derive(Default)]
struct After {
    tasks: Vec<Task>,
    message: Option<(Task, Uuid, crate::api::steer::Queued)>,
    outbox: bool,
}

/// Who acts for an external person.
enum Who {
    /// Mapped (identity active).
    User(Uuid),
    /// Not mapped, the integration allows it: the integration's owner.
    Owner(Uuid),
}

impl Who {
    fn id(&self) -> Uuid {
        match self {
            Self::User(u) | Self::Owner(u) => *u,
        }
    }
}

async fn who(
    conn: &mut PgConnection,
    row: &IntegrationRow,
    cfg: &IntakeConfig,
    actor: &Actor,
) -> anyhow::Result<Result<Who, String>> {
    let mapped: Option<(Uuid, bool, bool)> = sqlx::query_as(
        "SELECT user_id, owner_ok, user_ok FROM intake_identities WHERE integration_id=$1 AND external_id=$2",
    )
    .bind(row.id)
    .bind(&actor.id)
    .fetch_optional(&mut *conn)
    .await?;
    let name = row.kind().short();
    match mapped {
        Some((u, true, true)) => Ok(Ok(Who::User(u))),
        Some(_) => Ok(Err(format!(
            "Die Zuordnung dieses {name}-Kontos ist noch nicht bestätigt"
        ))),
        None if cfg.allow_unmapped => {
            if actor.host_trusted == Some(false) {
                return Ok(Err(format!(
                    "{name}-Konto ist nicht zugeordnet und hat keine Schreibrechte im Repository"
                )));
            }
            Ok(Ok(Who::Owner(row.owner_id)))
        }
        None => Ok(Err(format!(
            "{name}-Konto ist keiner Person im Projekt zugeordnet"
        ))),
    }
}

async fn seen(conn: &mut PgConnection, id: Uuid, inc: &Incoming) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM intake_events WHERE integration_id=$1 AND (delivery_id=$2 OR body_sha256=$3))",
    )
    .bind(id)
    .bind(&inc.delivery_id)
    .bind(&inc.body_sha256)
    .fetch_one(conn)
    .await
}

async fn record(
    conn: &mut PgConnection,
    row: &IntegrationRow,
    inc: &Incoming,
    actor: Option<&Actor>,
    outcome: IntakeOutcome,
    reason: Option<&str>,
    task: Option<Uuid>,
) -> sqlx::Result<()> {
    let keep = outcome.keeps_keys();
    sqlx::query(
        "INSERT INTO intake_events (id, integration_id, outcome, reason, event, external_user, task_id, delivery_id, body_sha256)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(row.id)
    .bind(outcome.as_str())
    .bind(reason.map(|r| r.chars().take(500).collect::<String>()))
    .bind(&inc.event)
    .bind(actor.map(|a| {
        let n = if a.name.is_empty() {
            a.id.clone()
        } else {
            format!("{} ({})", a.name, a.id)
        };
        n.chars().take(200).collect::<String>()
    }))
    .bind(task)
    .bind(inc.delivery_id.as_deref().filter(|_| keep))
    .bind(inc.body_sha256.as_deref().filter(|_| keep))
    .execute(conn)
    .await?;
    Ok(())
}

/// Records a delivery that is refused before the transaction (other
/// tenant): no dedupe keys, no content.
pub async fn record_refused(st: &AppState, row: &IntegrationRow, inc: &Incoming, why: &str) {
    if let Ok(mut c) = st.db.acquire().await {
        let _ = record(
            &mut c,
            row,
            inc,
            None,
            IntakeOutcome::Rejected,
            Some(why),
            None,
        )
        .await;
    }
}

/// Processes a verified delivery of an enabled integration.
pub async fn accept(st: &AppState, id: Uuid, inc: Incoming) -> anyhow::Result<Option<Accepted>> {
    let mut tx = st.db.begin().await?;
    let Some(row) =
        sqlx::query_as::<_, IntegrationRow>("SELECT * FROM integrations WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
    else {
        return Ok(None);
    };
    if !row.enabled {
        return Ok(None);
    }
    if seen(&mut tx, row.id, &inc).await? {
        return Ok(Some(Accepted::Duplicate));
    }
    let mut after = After::default();
    let (outcome, reason, task, actor) = match &inc.parsed {
        Parsed::Challenge(_) => (IntakeOutcome::Ignored, Some("Prüfung".into()), None, None),
        Parsed::Ignore(why) => (IntakeOutcome::Ignored, Some(why.clone()), None, None),
        Parsed::Request(r) => {
            let (o, why, t) = request(st, &mut tx, &row, r, &mut after).await?;
            (o, why, t, Some(r.actor.clone()))
        }
        Parsed::Reply(r) => {
            let (o, why, t) = reply(st, &mut tx, &row, r, &mut after).await?;
            (o, why, t, Some(r.actor.clone()))
        }
    };
    // Linear: a refused session would hang – say why (once per session).
    if matches!(outcome, IntakeOutcome::Rejected | IntakeOutcome::Failed) {
        if let Some(to) = &inc.refuse_to {
            let body = texts::refused(reason.as_deref().unwrap_or("Abgewiesen"));
            let key = format!(
                "refused:{}",
                to.get("session").and_then(Value::as_str).unwrap_or("?")
            );
            enqueue(&mut tx, &row, None, &key, Phase::Refused, &body, to).await?;
            after.outbox = true;
        }
    }
    record(
        &mut tx,
        &row,
        &inc,
        actor.as_ref(),
        outcome,
        reason.as_deref(),
        task,
    )
    .await?;
    tx.commit().await?;
    for t in &after.tasks {
        queue::announce(st, t);
        queue::event(
            st,
            t,
            "intake",
            &format!("Angelegt über {} „{}“", row.kind().short(), row.name),
        )
        .await;
    }
    if let Some((t, user, q)) = &after.message {
        crate::api::steer::announce_message(st, t, *user, q).await;
    }
    if after.outbox {
        st.intake_wake.notify_one();
    }
    Ok(Some(Accepted::Done {
        outcome,
        reason,
        task,
    }))
}

type Decision = (IntakeOutcome, Option<String>, Option<Uuid>);

fn rejected(why: impl Into<String>) -> Decision {
    (IntakeOutcome::Rejected, Some(why.into()), None)
}

/// The newest link of an external object and its task.
async fn latest_link(
    conn: &mut PgConnection,
    integration: Uuid,
    external_ref: &str,
) -> sqlx::Result<Option<(LinkRow, Task)>> {
    let link: Option<LinkRow> = sqlx::query_as(
        "SELECT * FROM intake_links WHERE integration_id=$1 AND external_ref=$2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(integration)
    .bind(external_ref)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(link) = link else { return Ok(None) };
    let task: Option<Task> = sqlx::query_as("SELECT * FROM tasks WHERE id=$1")
        .bind(link.task_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(task.map(|t| (link, t)))
}

async fn rate_limited(conn: &mut PgConnection, row: &IntegrationRow) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM intake_events WHERE integration_id=$1 AND outcome IN ('created','continued')
           AND created_at > now() - interval '1 hour'",
    )
    .bind(row.id)
    .fetch_one(conn)
    .await?;
    Ok(n >= i64::from(row.max_per_hour))
}

/// Fixed task prompt of an intake task (the request is in the context).
pub fn task_prompt(kind: IntakeKind, label: &str, requester: &str, cfg: &IntakeConfig) -> String {
    let mut p = format!(
        "Work on the request from {} ({label}) quoted in the context below. {requester}, a member of this project, asked for it there. Implement the requested change in this repository; if something is unclear, ask (questions in the result file) instead of guessing.",
        kind.short()
    );
    if let Some(i) = &cfg.instructions {
        p.push_str(&format!(
            "\n\nNotes from the project owner for every task from {}:\n{i}",
            kind.short()
        ));
    }
    p
}

async fn request(
    st: &AppState,
    conn: &mut PgConnection,
    row: &IntegrationRow,
    r: &Request,
    after: &mut After,
) -> anyhow::Result<Decision> {
    let cfg = row.config();
    let user = match who(conn, row, &cfg, &r.actor).await? {
        Ok(w) => w,
        Err(why) => return Ok(rejected(why)),
    };
    if !matches!(role(st, row.project_id, user.id()).await.map_err(|e| anyhow::anyhow!(e.1))?, Some(x) if x >= Role::Editor)
    {
        return Ok(rejected(match user {
            Who::User(_) => "Die zugeordnete Person darf in diesem Projekt keine Aufträge anlegen",
            Who::Owner(_) => {
                "Die Eigentümerin der Anbindung darf in diesem Projekt keine Aufträge mehr anlegen"
            }
        }));
    }
    if let Some((_, t)) = latest_link(conn, row.id, &r.external_ref).await? {
        // A mention where a task already exists (Slack thread, issue
        // comment) is a message to it – or "Weiter mit …" once it is done.
        if r.reply_if_open && !r.reply_text.trim().is_empty() {
            let rep = Reply {
                actor: r.actor.clone(),
                external_ref: r.external_ref.clone(),
                text: r.reply_text.clone(),
                require_waiting: false,
            };
            return reply(st, conn, row, &rep, after).await;
        }
        if ACTIVE.contains(&t.status.as_str()) {
            return Ok((
                IntakeOutcome::Ignored,
                Some("Zu diesem Objekt läuft schon ein Auftrag".into()),
                Some(t.id),
            ));
        }
        if !r.restart {
            return Ok((
                IntakeOutcome::Ignored,
                Some("Zu diesem Objekt gab es schon einen Auftrag".into()),
                Some(t.id),
            ));
        }
    }
    if rate_limited(conn, row).await? {
        return Ok(rejected(format!(
            "Limit erreicht: höchstens {} Aufträge pro Stunde",
            row.max_per_hour
        )));
    }
    let project: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(row.project_id)
        .fetch_one(&mut *conn)
        .await?;
    let settings = queue::load_settings(st, user.id()).await?;
    let exec = match crate::routing::resolve_execute(
        st,
        user.id(),
        &project,
        None,
        None,
        None,
        &settings.default_provider,
    )
    .await?
    {
        Ok(e) => e,
        Err(why) => {
            return Ok((
                IntakeOutcome::Failed,
                Some(format!("Kein Auftrag möglich: {why}")),
                None,
            ))
        }
    };
    let requester = crate::steer::user_name(st, Some(user.id())).await;
    let kind = row.kind();
    let intro = format!(
        "This task came in through the {} integration \"{}\" ({}).",
        kind.short(),
        row.name.replace(['\n', '\r'], " "),
        r.label
    );
    let context = crate::prompt::intake_section(&intro, &crate::prompt::nonce(), &r.entries);
    let title: String = match kind {
        IntakeKind::Slack => format!("{} von {requester}", r.label),
        _ => r.label.clone(),
    }
    .chars()
    .take(200)
    .collect();
    let task = queue::insert_task_in(
        conn,
        NewTask {
            user_id: user.id(),
            project_id: row.project_id,
            parent_id: None,
            kind: "task",
            title,
            prompt: task_prompt(kind, &r.label, &requester, &cfg),
            context,
            provider: exec.provider,
            model: exec.model,
            status: status::QUEUED,
            attempt: 0,
            depth: 0,
            created_by_agent: false,
            front: false,
            limits: TaskLimits::default(),
            connector_id: exec.connector_id,
            escalation_level: 0,
            allow_test_changes: false,
            carry_from: None,
            pr_id: None,
            plan_mode: project.plan_mode,
        },
    )
    .await?;
    link(st, conn, row, &task, r, after).await?;
    after.tasks.push(task.clone());
    Ok((IntakeOutcome::Created, None, Some(task.id)))
}

/// Link + "accepted" update of a new task.
async fn link(
    st: &AppState,
    conn: &mut PgConnection,
    row: &IntegrationRow,
    task: &Task,
    r: &Request,
    after: &mut After,
) -> anyhow::Result<()> {
    let link: LinkRow = sqlx::query_as(
        "INSERT INTO intake_links (id, integration_id, task_id, external_ref, external_url, external_title, reply_to)
         VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(row.id)
    .bind(task.id)
    .bind(r.external_ref.chars().take(300).collect::<String>())
    .bind(
        r.external_url
            .as_deref()
            .and_then(queued_api::intake::safe_url),
    )
    .bind(
        r.external_title
            .as_deref()
            .map(|t| t.chars().take(queued_api::intake::MAX_TITLE_CHARS).collect::<String>()),
    )
    .bind(&r.reply_to)
    .fetch_one(&mut *conn)
    .await?;
    let body = texts::accepted(task, &st.cfg.public_url);
    enqueue(
        conn,
        row,
        Some(link.id),
        &format!("accepted:{}", task.id),
        Phase::Accepted,
        &body,
        &r.reply_to,
    )
    .await?;
    after.outbox = true;
    Ok(())
}

/// Fixed prompt of a "Weiter mit …" task from an integration: the text is a
/// message (own block), never the prompt.
const CONTINUE_PROMPT: &str = "Continue the previous task with the follow-up request in the messages below (it came in through an integration).";

async fn reply(
    st: &AppState,
    conn: &mut PgConnection,
    row: &IntegrationRow,
    r: &Reply,
    after: &mut After,
) -> anyhow::Result<Decision> {
    let Some((link_row, task)) = latest_link(conn, row.id, &r.external_ref).await? else {
        return Ok((
            IntakeOutcome::Ignored,
            Some("Kein Auftrag zu diesem Objekt".into()),
            None,
        ));
    };
    if r.require_waiting && task.status != status::WAITING_INPUT {
        return Ok((
            IntakeOutcome::Ignored,
            Some("Kein Auftrag wartet hier auf eine Antwort".into()),
            Some(task.id),
        ));
    }
    let cfg = row.config();
    let user = match who(conn, row, &cfg, &r.actor).await? {
        Ok(w) => w.id(),
        Err(why) => return Ok(rejected(why)),
    };
    // Steering rights as in WP06: the creator (still ≥ editor) or the owner.
    let r_role = role(st, task.project_id, user)
        .await
        .map_err(|e| anyhow::anyhow!(e.1))?;
    let may = match r_role {
        Some(Role::Owner) => true,
        Some(Role::Editor) => task.user_id == user,
        _ => false,
    };
    let text = r.text.trim();
    if text.is_empty() {
        return Ok((
            IntakeOutcome::Ignored,
            Some("Leere Nachricht".into()),
            Some(task.id),
        ));
    }
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Ok(rejected(format!(
            "Nachricht zu lang (höchstens {MAX_MESSAGE_CHARS} Zeichen)"
        )));
    }
    let via = row.kind().as_str();
    if ACTIVE.contains(&task.status.as_str()) {
        if !may {
            return Ok(rejected(
                "Die zugeordnete Person darf diesen Auftrag nicht steuern",
            ));
        }
        match crate::api::steer::queue_message(
            conn,
            &task,
            user,
            text,
            MessageSource::Message,
            false,
            &[],
            Some(via),
        )
        .await
        {
            Ok(q) => {
                let outcome = if q.answered {
                    IntakeOutcome::Answered
                } else {
                    IntakeOutcome::Steered
                };
                after.message = Some((task.clone(), user, q));
                Ok((outcome, None, Some(task.id)))
            }
            Err(AppError(code, msg)) if code.is_client_error() => Ok(rejected(msg)),
            Err(AppError(_, msg)) => Err(anyhow::anyhow!(msg)),
        }
    } else if FINAL.contains(&task.status.as_str()) {
        if !matches!(r_role, Some(x) if x >= Role::Editor) {
            return Ok(rejected(
                "Die zugeordnete Person darf in diesem Projekt keine Aufträge anlegen",
            ));
        }
        if rate_limited(conn, row).await? {
            return Ok(rejected(format!(
                "Limit erreicht: höchstens {} Aufträge pro Stunde",
                row.max_per_hour
            )));
        }
        let project: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
            .bind(task.project_id)
            .fetch_one(&mut *conn)
            .await?;
        // Same agent and access as the origin, checked for this person.
        let settings = queue::load_settings(st, user).await?;
        let exec = crate::routing::resolve_execute(
            st,
            user,
            &project,
            task.connector_id,
            task.connector_id
                .is_none()
                .then_some(task.provider.as_str()),
            task.model.clone(),
            &settings.default_provider,
        )
        .await?;
        match exec {
            Ok(e) if e.provider == task.provider && e.connector_id == task.connector_id => {}
            Ok(_) => {
                return Ok((
                    IntakeOutcome::Failed,
                    Some(
                        "Der Agent des Auftrags ist für die zugeordnete Person nicht verfügbar"
                            .into(),
                    ),
                    None,
                ))
            }
            Err(why) => {
                return Ok((
                    IntakeOutcome::Failed,
                    Some(format!("Kein Auftrag möglich: {why}")),
                    None,
                ))
            }
        }
        let (next, _) = crate::steer::continuation(
            conn,
            &task,
            user,
            CONTINUE_PROMPT,
            project.plan_mode,
            &[],
            None,
        )
        .await?;
        crate::steer::insert(
            conn,
            next.id,
            user,
            text,
            MessageSource::Message,
            false,
            Some(via),
        )
        .await?;
        let req = Request {
            actor: r.actor.clone(),
            external_ref: link_row.external_ref.clone(),
            external_url: link_row.external_url.clone(),
            external_title: link_row.external_title.clone(),
            label: String::new(),
            entries: vec![],
            reply_to: link_row.reply_to.clone(),
            reply_if_open: false,
            reply_text: String::new(),
            restart: false,
        };
        link(st, conn, row, &next, &req, after).await?;
        after.tasks.push(next.clone());
        Ok((IntakeOutcome::Continued, None, Some(next.id)))
    } else {
        Ok((
            IntakeOutcome::Ignored,
            Some("Der Auftrag nimmt gerade keine Nachrichten an".into()),
            Some(task.id),
        ))
    }
}

// ---------------------------------------------------------------------------
// Outbox
// ---------------------------------------------------------------------------

async fn enqueue(
    conn: &mut PgConnection,
    row: &IntegrationRow,
    link: Option<Uuid>,
    key: &str,
    phase: Phase,
    body: &str,
    reply_to: &Value,
) -> sqlx::Result<bool> {
    // Without a token the owner chose "no updates": nothing to queue (and
    // nothing to fail and notify about).
    if row.token_enc.is_none() {
        return Ok(false);
    }
    // Linear: the activity ID is chosen up front, so a retry after a lost
    // answer can never create a second activity.
    let remote = (row.kind() == IntakeKind::Linear).then(|| Uuid::new_v4().to_string());
    let n = sqlx::query(
        "INSERT INTO intake_outbox (id, integration_id, link_id, key, kind, body, reply_to, remote_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (integration_id, key) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(row.id)
    .bind(link)
    .bind(key.chars().take(200).collect::<String>())
    .bind(phase.as_str())
    .bind(texts::bounded(body))
    .bind(reply_to)
    .bind(remote)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Fingerprint of what the outside has seen of a task.
const FINGERPRINT: &str =
    "t.status || ':' || COALESCE(md5(t.questions::text), '') || ':' || COALESCE(extract(epoch FROM t.finished_at)::text, '')";

/// Puts new phases of linked tasks into the outbox. Returns how many rows
/// were added.
pub async fn sync(st: &AppState) -> anyhow::Result<usize> {
    let mut added = 0;
    let mut tx = st.db.begin().await?;
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(&format!(
        "SELECT l.id, l.task_id, {FINGERPRINT} FROM intake_links l JOIN tasks t ON t.id = l.task_id
          WHERE l.reported IS DISTINCT FROM ({FINGERPRINT})
          ORDER BY l.created_at LIMIT 200 FOR UPDATE OF l SKIP LOCKED"
    ))
    .fetch_all(&mut *tx)
    .await?;
    for (link_id, task_id, fp) in rows {
        let link: LinkRow = sqlx::query_as("SELECT * FROM intake_links WHERE id=$1")
            .bind(link_id)
            .fetch_one(&mut *tx)
            .await?;
        let row: IntegrationRow = sqlx::query_as("SELECT * FROM integrations WHERE id=$1")
            .bind(link.integration_id)
            .fetch_one(&mut *tx)
            .await?;
        let task: Task = sqlx::query_as("SELECT * FROM tasks WHERE id=$1")
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await?;
        if let Some(phase) = Phase::of_status(&task.status) {
            let key = match phase {
                Phase::Question => format!(
                    "question:{}:{}:{}",
                    task.id,
                    // The same question asked again (after an unclear
                    // answer) is a new episode.
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM intake_outbox WHERE link_id=$1 AND kind='question'",
                    )
                    .bind(link.id)
                    .fetch_one(&mut *tx)
                    .await?,
                    crypto::sha256_hex(
                        &task
                            .questions
                            .as_ref()
                            .map(Value::to_string)
                            .unwrap_or_default()
                    )
                    .get(..16)
                    .unwrap_or_default()
                ),
                _ => format!(
                    "{}:{}:{}",
                    phase.as_str(),
                    task.id,
                    task.finished_at.map(|f| f.timestamp()).unwrap_or_default()
                ),
            };
            let body = texts::status(row.kind(), phase, &task, &st.cfg.public_url);
            if enqueue(
                &mut tx,
                &row,
                Some(link.id),
                &key,
                phase,
                &body,
                &link.reply_to,
            )
            .await?
            {
                added += 1;
            }
        }
        sqlx::query("UPDATE intake_links SET reported=$2 WHERE id=$1")
            .bind(link_id)
            .bind(fp.chars().take(200).collect::<String>())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(added)
}

/// Retry after `attempts` failed attempts: 5 s · 2^(n−1), at most 1 h.
pub fn backoff(attempts: i32) -> Duration {
    let n = attempts.clamp(1, 20) as u32 - 1;
    Duration::from_secs((5u64.saturating_mul(1u64 << n.min(12))).min(3600))
}

/// Attempts until an update counts as failed.
pub const MAX_ATTEMPTS: i32 = 8;
/// How long a claimed row is reserved for one deliverer.
const LEASE_SECS: i64 = 60;

/// Why sending failed.
#[derive(Debug, Clone, PartialEq)]
pub enum SendError {
    /// Try again later (network, 5xx, rate limit, auth that may be fixed).
    Retry {
        message: String,
        after: Option<Duration>,
    },
    /// Never works (bad address, deleted object, 4xx).
    Permanent(String),
}

impl SendError {
    fn message(&self) -> &str {
        match self {
            Self::Retry { message, .. } | Self::Permanent(message) => message,
        }
    }
}

/// HTTP client of one integration (SSRF guard, no redirects, timeouts).
#[derive(Clone)]
pub struct Client {
    pub kind: IntakeKind,
    pub base: String,
    token: String,
    account: Option<String>,
    http: reqwest::Client,
    guard: llm::Guard,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("kind", &self.kind)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(st: &AppState, row: &IntegrationRow) -> Result<Self, String> {
        let token = match &row.token_enc {
            Some(enc) => String::from_utf8(
                crypto::decrypt(&st.cfg.master_key, enc).map_err(|e| e.to_string())?,
            )
            .map_err(|_| "Token ist kein Text".to_string())?,
            None => return Err("Kein Token eingetragen – Rückmeldungen sind aus".into()),
        };
        let kind = row.kind();
        let base = match kind {
            IntakeKind::Github => st.cfg.intake_github_api.clone(),
            IntakeKind::Slack => st.cfg.intake_slack_api.clone(),
            IntakeKind::Linear => st.cfg.intake_linear_api.clone(),
            IntakeKind::Jira => st
                .cfg
                .intake_jira_api
                .clone()
                .unwrap_or_else(|| row.tenant.clone()),
        };
        let guard = st.cfg.git_host_guard();
        let http = llm::guarded_builder(&guard)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("queued/0.2 (+intake)")
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("HTTP-Client: {e}"))?;
        Ok(Self {
            kind,
            base,
            token: token.trim().to_string(),
            account: row.account.clone(),
            http,
            guard,
        })
    }

    /// Error text without the token (and anything the scanner knows).
    pub fn redact(&self, text: &str) -> String {
        let t = if self.token.is_empty() {
            text.to_string()
        } else {
            text.replace(&self.token, "***")
        };
        queued_common::secrets::redact(&t)
    }

    /// The token never leaves in a text (belt and braces: the texts are
    /// built from fixed words and redacted agent output).
    pub fn scrub(&self, text: &str) -> String {
        self.redact(text)
    }

    /// One JSON request. 2xx = the parsed body (`Null` when empty).
    pub async fn call(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Value, SendError> {
        llm::check_target(url, &self.guard)
            .await
            .map_err(|e| SendError::Permanent(self.redact(&e.message)))?;
        let mut rb = self.http.request(method, url);
        rb = match self.kind {
            IntakeKind::Github => rb
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            IntakeKind::Slack | IntakeKind::Linear => rb.bearer_auth(&self.token),
            IntakeKind::Jira => rb
                .basic_auth(
                    self.account.as_deref().unwrap_or_default(),
                    Some(&self.token),
                )
                .header("Accept", "application/json"),
        };
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let resp = rb.send().await.map_err(|e| SendError::Retry {
            message: self.redact(&format!("{} nicht erreichbar: {e}", self.kind.short())),
            after: None,
        })?;
        let code = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|s| Duration::from_secs(s.min(3600)));
        let text = resp.text().await.unwrap_or_default();
        let short = |t: &str| self.redact(&t.chars().take(300).collect::<String>());
        match code {
            200..=299 => Ok(serde_json::from_str(&text).unwrap_or(Value::Null)),
            429 => Err(SendError::Retry {
                message: format!("{}: Rate-Limit (429)", self.kind.short()),
                after: retry_after.or(Some(Duration::from_secs(60))),
            }),
            401 | 403 => Err(SendError::Retry {
                message: format!(
                    "{}: Zugang abgelehnt ({code}) – Token und Rechte prüfen: {}",
                    self.kind.short(),
                    short(&text)
                ),
                after: None,
            }),
            500..=599 | 408 => Err(SendError::Retry {
                message: format!("{}: Serverfehler {code}", self.kind.short()),
                after: retry_after,
            }),
            300..=399 => Err(SendError::Permanent(format!(
                "{}: Weiterleitung ({code}) – Adresse prüfen",
                self.kind.short()
            ))),
            _ => Err(SendError::Permanent(format!(
                "{}: abgelehnt ({code}): {}",
                self.kind.short(),
                short(&text)
            ))),
        }
    }
}

/// Short reference of an outbox row (Jira text marker).
pub fn short_ref(id: Uuid) -> String {
    format!("q-{}", &id.simple().to_string()[..12])
}

async fn send_one(client: &Client, item: &OutRow, reply_to: &Value) -> Result<String, SendError> {
    // A previous attempt may have reached the service and lost the answer.
    if item.attempts > 1 {
        let found = match client.kind {
            IntakeKind::Github => github::find(client, item, reply_to).await?,
            IntakeKind::Slack => slack::find(client, item, reply_to).await?,
            IntakeKind::Jira => jira::find(client, item, reply_to).await?,
            // The activity ID was chosen up front: sending again is safe.
            IntakeKind::Linear => None,
        };
        if let Some(id) = found {
            return Ok(id);
        }
    }
    match client.kind {
        IntakeKind::Github => github::send(client, item, reply_to).await,
        IntakeKind::Slack => slack::send(client, item, reply_to).await,
        IntakeKind::Linear => linear::send(client, item, reply_to).await,
        IntakeKind::Jira => jira::send(client, item, reply_to).await,
    }
}

/// Delivers due outbox rows (claimed with a lease, in order per link).
/// Returns how many were handled.
pub async fn deliver(st: &AppState) -> anyhow::Result<usize> {
    // One row per claim: the lease (60 s) always outlives the work on it
    // (a lookup and a send, 15 s timeout each), so a second instance never
    // takes over a row that is still being sent.
    let mut claimed: Vec<OutRow> = vec![];
    for _ in 0..20 {
        let row: Option<OutRow> = sqlx::query_as(
            "UPDATE intake_outbox SET lease_until = now() + make_interval(secs => $1), attempts = attempts + 1
              WHERE id = (
                SELECT o.id FROM intake_outbox o
                 WHERE o.state='pending' AND o.next_attempt_at <= now()
                   AND (o.lease_until IS NULL OR o.lease_until < now())
                   AND NOT EXISTS (SELECT 1 FROM intake_outbox p WHERE p.link_id = o.link_id
                                     AND p.state='pending' AND p.created_at < o.created_at)
                 ORDER BY o.next_attempt_at LIMIT 1 FOR UPDATE SKIP LOCKED)
              RETURNING *",
        )
        .bind(LEASE_SECS as f64)
        .fetch_optional(&st.db)
        .await?;
        let Some(item) = row else { break };
        deliver_one(st, item.clone()).await?;
        claimed.push(item);
    }
    Ok(claimed.len())
}

async fn deliver_one(st: &AppState, item: OutRow) -> anyhow::Result<()> {
    let row: Option<IntegrationRow> = sqlx::query_as("SELECT * FROM integrations WHERE id=$1")
        .bind(item.integration_id)
        .fetch_optional(&st.db)
        .await?;
    let Some(row) = row else { return Ok(()) };
    // Slack: the thread may have been created by an earlier row.
    let reply_to = match item.link_id {
        Some(l) => sqlx::query_scalar::<_, Value>("SELECT reply_to FROM intake_links WHERE id=$1")
            .bind(l)
            .fetch_optional(&st.db)
            .await?
            .unwrap_or_else(|| item.reply_to.clone()),
        None => item.reply_to.clone(),
    };
    let result = match Client::new(st, &row) {
        Ok(c) => send_one(&c, &item, &reply_to).await,
        Err(why) => Err(SendError::Permanent(why)),
    };
    match result {
        Ok(remote) => {
            sqlx::query(
                "UPDATE intake_outbox SET state='sent', sent_at=now(), remote_id=$2, lease_until=NULL, last_error=NULL WHERE id=$1",
            )
            .bind(item.id)
            .bind(remote.chars().take(200).collect::<String>())
            .execute(&st.db)
            .await?;
            if row.kind() == IntakeKind::Slack {
                slack::after_sent(st, &item, &reply_to, &remote).await?;
            }
        }
        Err(e) => {
            let last = e.message().chars().take(500).collect::<String>();
            let give_up = matches!(e, SendError::Permanent(_)) || item.attempts >= MAX_ATTEMPTS;
            if give_up {
                sqlx::query(
                    "UPDATE intake_outbox SET state='failed', lease_until=NULL, last_error=$2 WHERE id=$1",
                )
                .bind(item.id)
                .bind(&last)
                .execute(&st.db)
                .await?;
                tracing::warn!("intake update {} failed: {last}", item.id);
                crate::notify::notify(
                    st,
                    row.owner_id,
                    crate::notify::Note {
                        kind: "intake",
                        title: format!("Anbindung „{}“: Rückmeldung nicht zugestellt", row.name),
                        body: last,
                        task_id: None,
                        push: false,
                    },
                )
                .await;
            } else {
                let wait = match &e {
                    SendError::Retry { after: Some(a), .. } => (*a).max(backoff(item.attempts)),
                    _ => backoff(item.attempts),
                };
                sqlx::query(
                    "UPDATE intake_outbox SET lease_until=NULL, last_error=$2,
                            next_attempt_at = now() + make_interval(secs => $3) WHERE id=$1",
                )
                .bind(item.id)
                .bind(&last)
                .bind(wait.as_secs_f64())
                .execute(&st.db)
                .await?;
            }
        }
    }
    Ok(())
}

/// One round: new phases into the outbox, then deliver what is due.
pub async fn tick(st: &AppState) -> anyhow::Result<usize> {
    sync(st).await?;
    let mut n = 0;
    // Bounded per tick.
    for _ in 0..10 {
        let d = deliver(st).await?;
        n += d;
        if d == 0 {
            break;
        }
    }
    Ok(n)
}

/// History older than 90 days / beyond 500 per integration, sent updates
/// older than 30 days.
pub async fn prune(st: &AppState) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM intake_events WHERE created_at < now() - interval '90 days'")
        .execute(&st.db)
        .await?;
    sqlx::query(
        "DELETE FROM intake_events e USING (
             SELECT id, row_number() OVER (PARTITION BY integration_id ORDER BY created_at DESC) AS n FROM intake_events
         ) o WHERE e.id = o.id AND o.n > 500",
    )
    .execute(&st.db)
    .await?;
    sqlx::query(
        "DELETE FROM intake_outbox WHERE state <> 'pending' AND created_at < now() - interval '30 days'",
    )
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Background loop (own task; never blocks the task scheduler).
pub async fn run(st: AppState) {
    let every = Duration::from_secs(st.cfg.intake_tick_secs.max(1));
    let mut last_prune = std::time::Instant::now() - Duration::from_secs(7200);
    loop {
        if let Err(e) = tick(&st).await {
            tracing::warn!("intake outbox: {e:#}");
        }
        if last_prune.elapsed() > Duration::from_secs(3600) {
            last_prune = std::time::Instant::now();
            if let Err(e) = prune(&st).await {
                tracing::warn!("intake prune: {e:#}");
            }
        }
        tokio::select! {
            _ = st.intake_wake.notified() => {}
            _ = tokio::time::sleep(every) => {}
        }
    }
}

/// The link of a task for its detail view.
pub async fn link_view(
    st: &AppState,
    task: Uuid,
) -> sqlx::Result<Option<queued_api::intake::IntakeLinkView>> {
    let row: Option<(String, String, String, Option<String>, Option<String>, Uuid, bool)> =
        sqlx::query_as(
            "SELECT i.kind, i.name, l.external_ref, l.external_title, l.external_url, l.id, i.token_enc IS NOT NULL
               FROM intake_links l JOIN integrations i ON i.id = l.integration_id WHERE l.task_id=$1",
        )
        .bind(task)
        .fetch_optional(&st.db)
        .await?;
    let Some((kind, name, xref, title, url, link, updates)) = row else {
        return Ok(None);
    };
    let kind = IntakeKind::parse(&kind).unwrap_or(IntakeKind::Github);
    let last: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT kind, state, last_error FROM intake_outbox WHERE link_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(link)
    .fetch_optional(&st.db)
    .await?;
    let last_update = last.map(|(k, s, e)| {
        let phase = match k.as_str() {
            "accepted" => Phase::Accepted,
            "question" => Phase::Question,
            "done" => Phase::Done,
            "failed" => Phase::Failed,
            "cancelled" => Phase::Cancelled,
            _ => Phase::Refused,
        };
        match s.as_str() {
            "sent" => format!("„{}“ gemeldet", phase.label()),
            "failed" => format!(
                "„{}“ nicht zugestellt{}",
                phase.label(),
                e.map(|e| format!(": {e}")).unwrap_or_default()
            ),
            _ => format!("„{}“ wird gemeldet", phase.label()),
        }
    });
    Ok(Some(queued_api::intake::IntakeLinkView {
        kind,
        integration_name: name,
        label: texts::link_label(kind, &xref),
        title,
        url: url.as_deref().and_then(queued_api::intake::safe_url),
        last_update,
        updates,
    }))
}

// ---------------------------------------------------------------------------
// Texts sent outside: fixed German words + redacted agent output, never a
// diff, a token or the task's context.
// ---------------------------------------------------------------------------

pub mod texts {
    use super::*;

    pub const MAX_BODY: usize = 6000;
    pub const MAX_SUMMARY: usize = 1500;

    pub fn bounded(s: &str) -> String {
        let s = queued_common::secrets::redact(s);
        let s = if s.trim().is_empty() {
            "(leer)".to_string()
        } else {
            s
        };
        s.chars().take(MAX_BODY).collect()
    }

    fn link(task: &Task, public_url: &str) -> String {
        format!("{}/?task={}", public_url.trim_end_matches('/'), task.id)
    }

    fn agent_text(s: &str, max: usize) -> String {
        let r = queued_common::secrets::redact(s.trim());
        chars(&r, max)
    }

    /// Readable label of a link key.
    pub fn link_label(kind: IntakeKind, xref: &str) -> String {
        match kind {
            IntakeKind::Github => format!(
                "GitHub-Issue #{}",
                xref.strip_prefix("issue:").unwrap_or(xref)
            ),
            IntakeKind::Jira => format!("Jira {xref}"),
            IntakeKind::Linear => "Linear-Sitzung".to_string(),
            IntakeKind::Slack => "Slack-Thread".to_string(),
        }
    }

    pub fn accepted(task: &Task, public_url: &str) -> String {
        bounded(&format!(
            "Queue hat den Auftrag angenommen und eingereiht. Den Fortschritt siehst du hier: {}",
            link(task, public_url)
        ))
    }

    pub fn refused(why: &str) -> String {
        bounded(&format!(
            "Queue hat diese Anfrage nicht angenommen: {why}. Die Projekt-Eigentümerin kann das in Queue unter „Anbindungen“ einrichten."
        ))
    }

    /// Text of a phase. `kind` decides how to ask for an answer.
    pub fn status(kind: IntakeKind, phase: Phase, task: &Task, public_url: &str) -> String {
        let url = link(task, public_url);
        let reply_how = match kind {
            IntakeKind::Github | IntakeKind::Jira => "mit einem Kommentar in diesem Issue",
            IntakeKind::Slack => "hier im Thread",
            IntakeKind::Linear => "hier in Linear",
        };
        let text = match phase {
            Phase::Accepted => accepted(task, public_url),
            Phase::Refused => refused("abgewiesen"),
            Phase::Question => {
                let qs: Vec<String> = task
                    .questions
                    .clone()
                    .and_then(|q| serde_json::from_value(q).ok())
                    .unwrap_or_default();
                let mut t = String::from("Queue hat eine Rückfrage:\n");
                for q in qs.iter().take(10) {
                    t.push_str(&format!("- {}\n", agent_text(q, 500)));
                }
                t.push_str(&format!(
                    "\nBitte antworte {reply_how} (höchstens {MAX_MESSAGE_CHARS} Zeichen). Auftrag: {url}"
                ));
                t
            }
            Phase::Done => {
                let summary = task
                    .summary
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| agent_text(s, MAX_SUMMARY))
                    .unwrap_or_else(|| "(keine Zusammenfassung)".into());
                format!(
                    "Queue ist fertig.\n\nZusammenfassung des Agenten:\n{summary}\n\nÄnderungen und Diff (nur in Queue): {url}"
                )
            }
            Phase::Failed => {
                let why = task
                    .error
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| agent_text(s, 300))
                    .unwrap_or_else(|| "ohne Angabe".into());
                format!("Queue konnte den Auftrag nicht abschließen ({why}). Details: {url}")
            }
            Phase::Cancelled => format!("Der Auftrag wurde in Queue abgebrochen. {url}"),
        };
        bounded(&text)
    }
}

/// Answer of a Slack slash command / for the history.
pub fn outcome_text(a: &Accepted) -> String {
    match a {
        Accepted::Duplicate => "Schon verarbeitet.".into(),
        Accepted::Done {
            outcome, reason, ..
        } => match reason {
            Some(r) => format!("{}: {r}", outcome.label()),
            None => outcome.label().to_string(),
        },
    }
}

/// Hook address of an integration.
pub fn hook_url(public_url: &str, id: Uuid) -> String {
    format!("{}/api/intake/{id}", public_url.trim_end_matches('/'))
}

/// `json!` helper for the HTTP answers.
pub fn answer(a: &Accepted) -> Value {
    match a {
        Accepted::Duplicate => json!({"outcome": "duplicate"}),
        Accepted::Done {
            outcome,
            reason,
            task,
        } => json!({"outcome": outcome.as_str(), "reason": reason, "task_id": task}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff(1), Duration::from_secs(5));
        assert_eq!(backoff(2), Duration::from_secs(10));
        assert_eq!(backoff(4), Duration::from_secs(40));
        assert_eq!(backoff(20), Duration::from_secs(3600));
        assert_eq!(backoff(0), Duration::from_secs(5));
    }

    #[test]
    fn chars_limits_untrusted_text() {
        assert_eq!(chars("abc", 5), "abc");
        let c = chars(&"ä".repeat(10), 4);
        assert!(c.starts_with("ääää\n[… gekürzt]"));
    }
}
