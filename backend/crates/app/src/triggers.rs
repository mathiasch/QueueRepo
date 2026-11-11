//! WP07: triggers – firing (one transaction per firing: row lock, dedupe,
//! rights, rate limit, "a task is still open", task + history) and the
//! scheduler for schedules and one-off triggers.
//!
//! Exactly-once for schedules: the slot is recorded (UNIQUE per trigger),
//! the task inserted and `next_run_at` advanced in the same transaction as
//! the `FOR UPDATE SKIP LOCKED` row lock. A crash before the commit leaves
//! nothing behind (the slot is fired after the restart), a crash after it
//! leaves the slot done. Several app instances get disjoint rows.
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use queued_api::triggers::{
    Cron, GithubFilter, RunOutcome, RunSource, TriggerKind, MISFIRE_GRACE_MINS,
};
use serde_json::Value;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::{
    api::projects::{role, Role},
    models::{status, Project, Task},
    queue::{self, NewTask, TaskLimits},
    state::AppState,
};

/// Database row of a trigger.
#[derive(Debug, Clone, FromRow)]
pub struct TriggerRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub user_id: Uuid,
    pub template_id: Uuid,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub cron: Option<String>,
    pub timezone: Option<String>,
    pub run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub catch_up: bool,
    pub secret_enc: Option<Vec<u8>>,
    pub github: Option<Value>,
    pub max_per_hour: i32,
    pub skip_if_active: bool,
    pub disabled_reason: Option<String>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TriggerRow {
    pub fn kind(&self) -> TriggerKind {
        TriggerKind::parse(&self.kind).unwrap_or(TriggerKind::Api)
    }

    pub fn github_filter(&self) -> Option<GithubFilter> {
        self.github
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

/// Database row of a template.
#[derive(Debug, Clone, FromRow)]
pub struct TemplateRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub created_by: Option<Uuid>,
    pub name: String,
    pub title: String,
    pub prompt: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub plan_mode: Option<bool>,
    pub max_runtime_secs: Option<i32>,
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<i64>,
    pub max_turns: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Statuses of a task that is not finished yet ("a task is still open").
const OPEN: [&str; 7] = [
    status::QUEUED,
    status::RUNNING,
    status::WAITING_BATCH,
    status::WAITING_INPUT,
    status::PENDING_APPROVAL,
    status::HELD,
    status::MERGING,
];

/// Everything one firing knows besides the trigger.
#[derive(Debug, Clone, Default)]
pub struct Firing {
    pub source: Option<RunSource>,
    pub actor: Option<Uuid>,
    pub token: Option<Uuid>,
    /// GitHub `event` / `event.action`.
    pub event: Option<String>,
    // Dedupe keys (unique per trigger).
    pub scheduled_for: Option<DateTime<Utc>>,
    pub delivery_id: Option<String>,
    pub body_sha256: Option<String>,
    pub dedupe_key: Option<String>,
    /// Delivery ID for the history only.
    pub delivery_ref: Option<String>,
    /// Trusted suffix of the task title (never sender data).
    pub title_suffix: String,
    /// Untrusted data for the agent: intro + entries (header, text).
    pub data: Option<(String, Vec<(String, String)>)>,
    /// Note in the history of a created task (e.g. "nachgeholt").
    pub note: Option<String>,
}

impl Firing {
    fn source(&self) -> RunSource {
        self.source.unwrap_or(RunSource::Manual)
    }
}

/// What happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Fired {
    Done {
        run: Uuid,
        outcome: RunOutcome,
        reason: Option<String>,
        task: Option<Uuid>,
    },
    /// Seen before (same slot, delivery ID, body or event key): nothing recorded.
    Duplicate,
}

impl Fired {
    pub fn outcome(&self) -> Option<RunOutcome> {
        match self {
            Fired::Done { outcome, .. } => Some(*outcome),
            Fired::Duplicate => None,
        }
    }
}

/// Which dedupe keys a run keeps: final decisions keep all of them, a
/// refusal or failure keeps only the schedule slot (the slot is consumed;
/// a webhook sender may retry).
fn keeps_keys(o: RunOutcome) -> bool {
    matches!(
        o,
        RunOutcome::Created | RunOutcome::Ignored | RunOutcome::Skipped
    )
}

/// Records one run. `None` = a run with one of the dedupe keys exists.
async fn record(
    conn: &mut PgConnection,
    t: &TriggerRow,
    f: &Firing,
    outcome: RunOutcome,
    reason: Option<&str>,
    task: Option<Uuid>,
) -> sqlx::Result<Option<Uuid>> {
    let keep = keeps_keys(outcome);
    let reason = reason.map(|r| r.chars().take(500).collect::<String>());
    sqlx::query_scalar(
        "INSERT INTO trigger_runs (id, trigger_id, source, outcome, reason, task_id, actor_id, token_id, event,
                                   scheduled_for, delivery_id, body_sha256, dedupe_key, delivery_ref)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(t.id)
    .bind(f.source().as_str())
    .bind(outcome.as_str())
    .bind(reason)
    .bind(task)
    .bind(f.actor)
    .bind(f.token)
    .bind(f.event.as_deref().map(|e| e.chars().take(100).collect::<String>()))
    .bind(f.scheduled_for)
    .bind(f.delivery_id.as_deref().filter(|_| keep))
    .bind(f.body_sha256.as_deref().filter(|_| keep))
    .bind(f.dedupe_key.as_deref().filter(|_| keep))
    .bind(
        f.delivery_ref
            .as_deref()
            .or(f.delivery_id.as_deref())
            .map(|d| d.chars().take(100).collect::<String>()),
    )
    .fetch_optional(conn)
    .await
}

async fn seen(conn: &mut PgConnection, t: &TriggerRow, f: &Firing) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM trigger_runs WHERE trigger_id=$1 AND (
             scheduled_for = $2 OR delivery_id = $3 OR body_sha256 = $4 OR dedupe_key = $5))",
    )
    .bind(t.id)
    .bind(f.scheduled_for)
    .bind(&f.delivery_id)
    .bind(&f.body_sha256)
    .bind(&f.dedupe_key)
    .fetch_one(conn)
    .await
}

async fn done(
    conn: &mut PgConnection,
    t: &TriggerRow,
    f: &Firing,
    outcome: RunOutcome,
    reason: Option<String>,
    task: Option<Uuid>,
) -> anyhow::Result<Fired> {
    match record(conn, t, f, outcome, reason.as_deref(), task).await? {
        Some(run) => Ok(Fired::Done {
            run,
            outcome,
            reason,
            task,
        }),
        None => Ok(Fired::Duplicate),
    }
}

/// Records a run without a task (ping, filtered event) unless one of its
/// dedupe keys was seen; inside the caller's transaction with the row lock.
pub async fn record_outcome(
    conn: &mut PgConnection,
    t: &TriggerRow,
    f: &Firing,
    outcome: RunOutcome,
    reason: &str,
) -> anyhow::Result<Fired> {
    if seen(conn, t, f).await? {
        return Ok(Fired::Duplicate);
    }
    done(conn, t, f, outcome, Some(reason.to_string()), None).await
}

/// Fires `t` inside `conn`'s transaction. The caller holds the trigger row
/// lock (`FOR UPDATE`), commits and then calls [`after_commit`].
pub async fn fire_locked(
    st: &AppState,
    conn: &mut PgConnection,
    t: &TriggerRow,
    f: &Firing,
) -> anyhow::Result<(Fired, Option<Task>)> {
    if seen(conn, t, f).await? {
        return Ok((Fired::Duplicate, None));
    }
    // The owner pays: still at least editor of the project?
    if !matches!(role(st, t.project_id, t.user_id).await.map_err(|e| anyhow::anyhow!(e.1))?, Some(r) if r >= Role::Editor)
    {
        let why = "Die Person, der der Auslöser gehört, darf in diesem Projekt keine Aufträge mehr anlegen. Auslöser ausgeschaltet.";
        sqlx::query(
            "UPDATE triggers SET enabled=FALSE, disabled_reason=$2, updated_at=now() WHERE id=$1",
        )
        .bind(t.id)
        .bind(why)
        .execute(&mut *conn)
        .await?;
        return Ok((
            done(conn, t, f, RunOutcome::Rejected, Some(why.into()), None).await?,
            None,
        ));
    }
    let created_last_hour: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM trigger_runs WHERE trigger_id=$1 AND outcome='created' AND created_at > now() - interval '1 hour'",
    )
    .bind(t.id)
    .fetch_one(&mut *conn)
    .await?;
    if created_last_hour >= i64::from(t.max_per_hour) {
        let why = format!(
            "Limit erreicht: höchstens {} Aufträge pro Stunde",
            t.max_per_hour
        );
        return Ok((
            done(conn, t, f, RunOutcome::Rejected, Some(why), None).await?,
            None,
        ));
    }
    if t.skip_if_active {
        let open: Vec<String> = OPEN.iter().map(|s| s.to_string()).collect();
        let busy: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE trigger_id=$1 AND status = ANY($2) LIMIT 1",
        )
        .bind(t.id)
        .bind(&open)
        .fetch_optional(&mut *conn)
        .await?;
        if busy.is_some() {
            return Ok((
                done(
                    conn,
                    t,
                    f,
                    RunOutcome::Skipped,
                    Some("Ein Auftrag dieses Auslösers ist noch nicht fertig".into()),
                    None,
                )
                .await?,
                None,
            ));
        }
    }
    match new_task(st, conn, t, f).await? {
        Ok(task) => {
            sqlx::query(
                "UPDATE triggers SET last_fired_at=now(), disabled_reason=NULL WHERE id=$1",
            )
            .bind(t.id)
            .execute(&mut *conn)
            .await?;
            let fired = done(
                conn,
                t,
                f,
                RunOutcome::Created,
                f.note.clone(),
                Some(task.id),
            )
            .await?;
            Ok((fired, Some(task)))
        }
        Err(why) => Ok((
            done(conn, t, f, RunOutcome::Failed, Some(why), None).await?,
            None,
        )),
    }
}

/// The task of a firing (`Err` = German reason, e.g. no access).
async fn new_task(
    st: &AppState,
    conn: &mut PgConnection,
    t: &TriggerRow,
    f: &Firing,
) -> anyhow::Result<Result<Task, String>> {
    let tpl: TemplateRow = sqlx::query_as("SELECT * FROM task_templates WHERE id=$1")
        .bind(t.template_id)
        .fetch_one(&mut *conn)
        .await
        .context("template of trigger")?;
    let project: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(t.project_id)
        .fetch_one(&mut *conn)
        .await?;
    let settings = queue::load_settings(st, t.user_id).await?;
    let exec = match crate::routing::resolve_execute(
        st,
        t.user_id,
        &project,
        None,
        tpl.provider.as_deref(),
        tpl.model.clone(),
        &settings.default_provider,
    )
    .await?
    {
        Ok(e) => e,
        Err(why) => return Ok(Err(format!("Kein Auftrag möglich: {why}"))),
    };
    let context = match &f.data {
        Some((intro, entries)) => {
            crate::prompt::event_section(intro, &crate::prompt::nonce(), entries)
        }
        None => String::new(),
    };
    let title: String = format!("{} ({})", tpl.title.trim(), f.title_suffix)
        .chars()
        .take(200)
        .collect();
    let mut task = queue::insert_task_in(
        conn,
        NewTask {
            user_id: t.user_id,
            project_id: t.project_id,
            parent_id: None,
            kind: "task",
            title,
            prompt: tpl.prompt.clone(),
            context,
            provider: exec.provider,
            model: exec.model,
            status: status::QUEUED,
            attempt: 0,
            depth: 0,
            created_by_agent: false,
            front: false,
            limits: TaskLimits {
                max_runtime_secs: tpl.max_runtime_secs,
                max_cost_usd: tpl.max_cost_usd,
                max_tokens: tpl.max_tokens,
                max_turns: tpl.max_turns,
            },
            connector_id: exec.connector_id,
            escalation_level: 0,
            allow_test_changes: false,
            carry_from: None,
            pr_id: None,
            plan_mode: tpl.plan_mode.unwrap_or(project.plan_mode),
        },
    )
    .await?;
    sqlx::query("UPDATE tasks SET trigger_id=$2 WHERE id=$1")
        .bind(task.id)
        .bind(t.id)
        .execute(&mut *conn)
        .await?;
    task.trigger_id = Some(t.id);
    Ok(Ok(task))
}

/// After the commit: tell UI and scheduler; tell the owner once when a
/// trigger stops working.
pub async fn after_commit(st: &AppState, t: &TriggerRow, fired: &Fired, task: Option<&Task>) {
    if let Some(task) = task {
        queue::announce(st, task);
    }
    if let Fired::Done {
        outcome: outcome @ (RunOutcome::Failed | RunOutcome::Rejected),
        reason,
        run,
        ..
    } = fired
    {
        // Only the first of a series (a webhook may keep failing).
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT outcome FROM trigger_runs WHERE trigger_id=$1 AND id<>$2 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(t.id)
        .bind(run)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
        let rate = reason.as_deref().is_some_and(|r| r.starts_with("Limit"));
        if previous.as_deref() != Some(outcome.as_str()) && !rate {
            crate::notify::notify(
                st,
                t.user_id,
                crate::notify::Note {
                    kind: "trigger",
                    title: format!("Auslöser „{}“: kein Auftrag gestartet", t.name),
                    body: reason.clone().unwrap_or_default(),
                    task_id: None,
                    push: true,
                },
            )
            .await;
        }
    }
}

/// Locks the trigger row and fires it (API, UI, webhooks).
pub async fn fire(st: &AppState, id: Uuid, f: Firing) -> anyhow::Result<Option<Fired>> {
    let mut tx = st.db.begin().await?;
    let Some(t) = sqlx::query_as::<_, TriggerRow>("SELECT * FROM triggers WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
    else {
        return Ok(None);
    };
    if !t.enabled {
        return Ok(None);
    }
    let (fired, task) = fire_locked(st, &mut tx, &t, &f).await?;
    tx.commit().await?;
    after_commit(st, &t, &fired, task.as_ref()).await;
    Ok(Some(fired))
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

pub fn parse_tz(s: &str) -> Option<Tz> {
    s.parse::<Tz>().ok()
}

/// The schedule of a row (cron + zone), `Err` = German reason.
pub fn schedule_of(t: &TriggerRow) -> Result<(Cron, Tz), String> {
    let cron = Cron::parse(t.cron.as_deref().unwrap_or_default())?;
    let tz = t
        .timezone
        .as_deref()
        .and_then(parse_tz)
        .ok_or_else(|| "Unbekannte Zeitzone".to_string())?;
    Ok((cron, tz))
}

/// Next slot after `after`, at least one hour after `last` (a schedule
/// never fires twice within an hour, not even around a DST change).
pub fn next_slot(
    cron: &Cron,
    tz: &Tz,
    after: DateTime<Utc>,
    last: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let floor = last.map(|l| l + chrono::Duration::minutes(59) + chrono::Duration::seconds(59));
    let from = match floor {
        Some(f) if f > after => f,
        _ => after,
    };
    cron.next_after(tz, from)
}

/// What to do with a due slot.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    /// The latest slot that is due (the one that runs or is skipped).
    pub slot: DateTime<Utc>,
    /// Earlier due slots that were missed entirely.
    pub missed: u32,
    /// More than [`MISFIRE_GRACE_MINS`] late.
    pub late: bool,
    /// The next slot after this one.
    pub next: Option<DateTime<Utc>>,
}

/// Collapses all slots from `first` to `now` into one decision.
pub fn due(cron: &Cron, tz: &Tz, first: DateTime<Utc>, now: DateTime<Utc>) -> Due {
    let mut slot = first;
    let mut missed = 0u32;
    // Bounded: an hourly schedule after a year of downtime is 8760 steps.
    while missed < 100_000 {
        match next_slot(cron, tz, slot, Some(slot)) {
            Some(n) if n <= now => {
                slot = n;
                missed += 1;
            }
            _ => break,
        }
    }
    Due {
        slot,
        missed,
        late: now - slot > chrono::Duration::minutes(MISFIRE_GRACE_MINS),
        next: next_slot(cron, tz, now, Some(slot)),
    }
}

fn local(t: DateTime<Utc>, tz: Option<&Tz>) -> String {
    match tz {
        Some(tz) => t.with_timezone(tz).format("%d.%m.%Y %H:%M").to_string(),
        None => t.format("%d.%m.%Y %H:%M UTC").to_string(),
    }
}

/// Handles one due schedule/one-off row inside the caller's transaction.
async fn process_due(
    st: &AppState,
    conn: &mut PgConnection,
    t: &TriggerRow,
    now: DateTime<Utc>,
) -> anyhow::Result<(Fired, Option<Task>)> {
    let first = t.next_run_at.unwrap_or(now);
    let (d, tz) = match t.kind() {
        TriggerKind::Schedule => match schedule_of(t) {
            Ok((cron, tz)) => (due(&cron, &tz, first, now), Some(tz)),
            Err(why) => {
                sqlx::query("UPDATE triggers SET enabled=FALSE, next_run_at=NULL, disabled_reason=$2, updated_at=now() WHERE id=$1")
                    .bind(t.id)
                    .bind(format!("Zeitplan ungültig: {why}"))
                    .execute(&mut *conn)
                    .await?;
                return Ok((Fired::Duplicate, None));
            }
        },
        _ => (
            Due {
                slot: first,
                missed: 0,
                late: now - first > chrono::Duration::minutes(MISFIRE_GRACE_MINS),
                next: None,
            },
            t.timezone.as_deref().and_then(parse_tz),
        ),
    };
    sqlx::query("UPDATE triggers SET next_run_at=$2, updated_at=now() WHERE id=$1")
        .bind(t.id)
        .bind(d.next)
        .execute(&mut *conn)
        .await?;
    let missed_note = (d.missed > 0).then(|| {
        format!(
            "{} {} Termin(e) verpasst (Queue war nicht erreichbar)",
            d.missed,
            if d.missed == 1 {
                "früherer"
            } else {
                "frühere"
            }
        )
    });
    let mut f = Firing {
        source: Some(RunSource::Schedule),
        scheduled_for: Some(d.slot),
        title_suffix: format!(
            "{}, {}",
            if t.kind() == TriggerKind::Once {
                "Einmalig"
            } else {
                "Zeitplan"
            },
            local(d.slot, tz.as_ref())
        ),
        note: missed_note.clone(),
        ..Default::default()
    };
    if d.late {
        if !t.catch_up {
            let why = format!(
                "Verpasst: geplant für {}, Queue war nicht erreichbar{}",
                local(d.slot, tz.as_ref()),
                missed_note.map(|m| format!("; {m}")).unwrap_or_default()
            );
            let fired = done(conn, t, &f, RunOutcome::Skipped, Some(why), None).await?;
            return Ok((fired, None));
        }
        f.source = Some(RunSource::CatchUp);
        f.note = Some(format!(
            "Nachgeholt: geplant für {}{}",
            local(d.slot, tz.as_ref()),
            missed_note.map(|m| format!("; {m}")).unwrap_or_default()
        ));
    }
    fire_locked(st, conn, t, &f).await
}

/// Fires every due schedule/one-off trigger (one transaction each).
/// Returns how many slots were handled.
pub async fn tick(st: &AppState) -> anyhow::Result<usize> {
    let mut n = 0;
    // Bounded per tick; the rest follows at once in the next tick.
    while n < 100 {
        let mut tx = st.db.begin().await?;
        let row: Option<TriggerRow> = sqlx::query_as(
            "SELECT * FROM triggers WHERE enabled AND next_run_at IS NOT NULL AND next_run_at <= now()
               AND kind IN ('schedule','once')
             ORDER BY next_run_at LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(t) = row else {
            break;
        };
        let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut *tx)
            .await?;
        let (fired, task) = process_due(st, &mut tx, &t, now).await?;
        tx.commit().await?;
        after_commit(st, &t, &fired, task.as_ref()).await;
        n += 1;
    }
    Ok(n)
}

/// History older than 90 days and beyond 500 entries per trigger.
pub async fn prune(st: &AppState) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM trigger_runs WHERE created_at < now() - interval '90 days'")
        .execute(&st.db)
        .await?;
    sqlx::query(
        "DELETE FROM trigger_runs r USING (
             SELECT id, row_number() OVER (PARTITION BY trigger_id ORDER BY created_at DESC) AS n FROM trigger_runs
         ) o WHERE r.id = o.id AND o.n > 500",
    )
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Background loop (own task; never blocks the task scheduler).
pub async fn run(st: AppState) {
    let every = Duration::from_secs(st.cfg.trigger_tick_secs.max(1));
    let mut last_prune = std::time::Instant::now() - Duration::from_secs(7200);
    loop {
        if let Err(e) = tick(&st).await {
            tracing::warn!("trigger scheduler: {e:#}");
        }
        if last_prune.elapsed() > Duration::from_secs(3600) {
            last_prune = std::time::Instant::now();
            if let Err(e) = prune(&st).await {
                tracing::warn!("trigger history prune: {e:#}");
            }
        }
        tokio::select! {
            _ = st.trigger_wake.notified() => {}
            _ = tokio::time::sleep(every) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn dst_forward_and_back_in_berlin() {
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        // 2026-03-29: 02:00 → 03:00. 02:30 does not exist ⇒ 03:30 CEST.
        let c = Cron::parse("30 2 * * *").unwrap();
        let n = c.next_after(&tz, utc("2026-03-28T12:00:00Z")).unwrap();
        assert_eq!(n, utc("2026-03-29T01:30:00Z"), "03:30 CEST");
        assert_eq!(
            c.next_after(&tz, n).unwrap(),
            utc("2026-03-30T00:30:00Z"),
            "next day 02:30 CEST"
        );
        // 2026-10-25: 03:00 → 02:00. 02:30 exists twice ⇒ only the first.
        let n = c.next_after(&tz, utc("2026-10-24T12:00:00Z")).unwrap();
        assert_eq!(n, utc("2026-10-25T00:30:00Z"), "02:30 CEST");
        assert_eq!(
            c.next_after(&tz, n).unwrap(),
            utc("2026-10-26T01:30:00Z"),
            "not again at 02:30 CET"
        );
        // Hourly across the switch back: never two runs within an hour.
        let c = Cron::parse("0 * * * *").unwrap();
        let mut t = utc("2026-10-24T23:30:00Z");
        let mut last: Option<DateTime<Utc>> = None;
        for _ in 0..6 {
            let n = next_slot(&c, &tz, t, last).unwrap();
            if let Some(l) = last {
                assert!(n - l >= chrono::Duration::minutes(59), "{l} → {n}");
            }
            last = Some(n);
            t = n;
        }
    }

    #[test]
    fn lord_howe_half_hour_shift_keeps_an_hour_between_runs() {
        // UTC+10:30 / +11; on 2026-10-04 02:00 → 02:30.
        let tz: Tz = "Australia/Lord_Howe".parse().unwrap();
        let c = Cron::parse("45 1,2 * * *").unwrap();
        let first = c.next_after(&tz, utc("2026-10-03T12:00:00Z")).unwrap();
        let second = next_slot(&c, &tz, first, Some(first)).unwrap();
        assert!(
            second - first >= chrono::Duration::minutes(59),
            "{first} → {second}"
        );
    }

    #[test]
    fn missed_slots_collapse_into_one_decision() {
        let tz: Tz = "UTC".parse().unwrap();
        let c = Cron::parse("0 * * * *").unwrap();
        // On time (2 minutes late): no miss.
        let d = due(
            &c,
            &tz,
            utc("2026-11-11T10:00:00Z"),
            utc("2026-11-11T10:02:00Z"),
        );
        assert_eq!(d.slot, utc("2026-11-11T10:00:00Z"));
        assert_eq!((d.missed, d.late), (0, false));
        assert_eq!(d.next, Some(utc("2026-11-11T11:00:00Z")));
        // Down from 09:30 to 13:20: slots 10, 11, 12, 13 ⇒ 13:00 counts, 3 missed, not late.
        let d = due(
            &c,
            &tz,
            utc("2026-11-11T10:00:00Z"),
            utc("2026-11-11T13:05:00Z"),
        );
        assert_eq!(d.slot, utc("2026-11-11T13:00:00Z"));
        assert_eq!((d.missed, d.late), (3, false));
        // 13:20: the 13:00 slot is late.
        let d = due(
            &c,
            &tz,
            utc("2026-11-11T10:00:00Z"),
            utc("2026-11-11T13:20:00Z"),
        );
        assert_eq!((d.missed, d.late), (3, true));
        assert_eq!(d.next, Some(utc("2026-11-11T14:00:00Z")));
        // A year of downtime is bounded and correct.
        let d = due(
            &c,
            &tz,
            utc("2025-11-11T10:00:00Z"),
            utc("2026-11-11T10:30:00Z"),
        );
        assert_eq!(d.slot, utc("2026-11-11T10:00:00Z"));
        assert_eq!(d.missed, 365 * 24);
        assert!(d.late);
    }
}
