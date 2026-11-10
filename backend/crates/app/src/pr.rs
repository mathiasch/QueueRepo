//! Pull requests and their feedback loop (WP05).
//!
//! After a task branch was pushed in output mode `pr` (merge queue, see
//! `runner_publish.rs`), a row in `pull_requests` with state `creating`
//! exists. Every host API call happens here, in one background poller that
//! never blocks the scheduler:
//!
//! * **create** the pull/merge request (idempotent: an open one for the
//!   branch is reused), set labels;
//! * **poll** state, CI of the head commit and comments: ETag/If-None-Match
//!   where it is safe, `Retry-After`/rate-limit headers, exponential backoff
//!   on errors, a lease per row so two pollers (or a restarted app) never
//!   work on the same row at once;
//! * **feedback → follow-up**: failed CI of the head commit (dedupe per
//!   commit) and comments (dedupe per comment) of people with write access –
//!   checked through the host API, never taken from the comment – become one
//!   follow-up task on the same branch; at most one unfinished follow-up per
//!   pull request, at most `max_followups` per pull request (loop guard).
//!   Recording the feedback, counting the round and inserting the task
//!   happen in one transaction: a crash never loses or duplicates feedback.
//!
//! Webhooks (WP07) plug in through [`poke`]: they only make the next poll
//! happen now; their payload is never trusted.
use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use queued_api::pr::{
    ci_state, Check, CiState, FeedbackOutcome, FeedbackSource, FeedbackView, GitHost, PrState,
    PullRequestView,
};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    crypto,
    githost::{Comment, HostClient, HostError},
    models::{status, Project, Task},
    notify::{notify, Note},
    prompt::{self, PrFeedbackItem},
    queue::{self, NewTask, TaskLimits},
    state::AppState,
};

/// Lease of one poll (a crashed poller's rows come back after it).
const LEASE_SECS: i64 = 300;
/// Hard limit of one poll (all its host calls).
const POLL_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest wait between polls after errors.
const MAX_BACKOFF_SECS: i64 = 3600;
/// Rows polled per tick.
const BATCH: i64 = 20;

#[derive(Debug, Clone, FromRow)]
pub struct PrRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Uuid,
    pub user_id: Uuid,
    pub host: String,
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub number: Option<i64>,
    pub url: Option<String>,
    pub state: String,
    pub draft: bool,
    pub head_sha: Option<String>,
    pub ci_state: String,
    pub checks: Value,
    pub conflict: bool,
    pub auto_fix: bool,
    pub followups: i32,
    pub max_followups: i32,
    pub bot_login: Option<String>,
    pub bot_id: Option<i64>,
    pub etags: Value,
    pub failures: i32,
    pub rate_until: Option<DateTime<Utc>>,
    pub next_poll_at: DateTime<Utc>,
    pub lease_until: Option<DateTime<Utc>>,
    pub last_polled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PrRow {
    pub fn state(&self) -> PrState {
        PrState::parse(&self.state).unwrap_or(PrState::Closed)
    }

    fn host(&self) -> GitHost {
        GitHost::parse(&self.host).unwrap_or(GitHost::Github)
    }

    /// "Pull Request #4" (or "Merge Request" on GitLab).
    pub fn label(&self) -> String {
        match self.number {
            Some(n) => format!("{} #{n}", self.host().request_name()),
            None => self.host().request_name().to_string(),
        }
    }

    pub fn view(&self, task_title: String, active_task: Option<Uuid>) -> PullRequestView {
        PullRequestView {
            id: self.id,
            project_id: self.project_id,
            task_id: self.task_id,
            task_title,
            host: self.host(),
            number: self.number,
            url: self.url.clone(),
            state: self.state(),
            draft: self.draft,
            head_branch: self.head_branch.clone(),
            base_branch: self.base_branch.clone(),
            head_sha: self.head_sha.clone(),
            ci_state: CiState::parse(&self.ci_state).unwrap_or_default(),
            checks: serde_json::from_value(self.checks.clone()).unwrap_or_default(),
            conflict: self.conflict,
            auto_fix: self.auto_fix,
            followups: self.followups,
            max_followups: self.max_followups,
            active_task,
            last_polled_at: self.last_polled_at,
            next_poll_at: self.state().is_open().then_some(self.next_poll_at),
            last_error: self.last_error.clone(),
            created_at: self.created_at,
        }
    }
}

/// Webhook interface (WP07) and "check now": the pull request is polled at
/// once. Nothing from a webhook payload is trusted – the poll reads the
/// host API.
pub async fn poke(st: &AppState, pr: Uuid) -> sqlx::Result<bool> {
    let n = sqlx::query(
        "UPDATE pull_requests SET next_poll_at=GREATEST(now(), COALESCE(rate_until, now())), updated_at=now()
         WHERE id=$1 AND state IN ('creating','open')",
    )
    .bind(pr)
    .execute(&st.db)
    .await?
    .rows_affected();
    st.pr_wake.notify_one();
    Ok(n > 0)
}

/// Start-up: leases of a previous process are void, due rows are polled.
pub async fn recover(st: &AppState) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE pull_requests SET lease_until=NULL WHERE lease_until IS NOT NULL AND state IN ('creating','open')",
    )
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Background poller (own task; the scheduler is never blocked by it).
pub async fn run(st: AppState) {
    let every = Duration::from_secs(st.cfg.pr_poll_secs.max(1));
    loop {
        if let Err(e) = tick(&st).await {
            tracing::warn!("pull request poller: {e:#}");
        }
        tokio::select! {
            _ = st.pr_wake.notified() => {}
            _ = tokio::time::sleep(every) => {}
        }
    }
}

/// Polls all due pull requests (each in its own task with a time limit).
/// Returns how many were picked.
pub async fn tick(st: &AppState) -> anyhow::Result<usize> {
    let rows = claim(st).await?;
    let n = rows.len();
    let mut handles = vec![];
    for row in rows {
        let st = st.clone();
        handles.push(tokio::spawn(async move {
            let id = row.id;
            match tokio::time::timeout(POLL_TIMEOUT, poll(&st, row)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("pull request {id}: {e:#}"),
                Err(_) => {
                    let _ = failed(
                        &st,
                        id,
                        "Zeitüberschreitung beim Abfragen des Git-Servers",
                        None,
                    )
                    .await;
                }
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    Ok(n)
}

/// Takes due rows under a lease (`FOR UPDATE SKIP LOCKED`: concurrent
/// pollers get disjoint rows).
pub async fn claim(st: &AppState) -> sqlx::Result<Vec<PrRow>> {
    sqlx::query_as(
        "UPDATE pull_requests SET lease_until = now() + make_interval(secs => $1)
         WHERE id IN (SELECT id FROM pull_requests
                      WHERE state IN ('creating','open') AND next_poll_at <= now()
                        AND (lease_until IS NULL OR lease_until < now())
                      ORDER BY next_poll_at LIMIT $2 FOR UPDATE SKIP LOCKED)
         RETURNING *",
    )
    .bind(LEASE_SECS as f64)
    .bind(BATCH)
    .fetch_all(&st.db)
    .await
}

fn token_of(st: &AppState, p: &Project) -> Option<String> {
    p.git_token_enc
        .as_deref()
        .and_then(|e| crypto::decrypt(&st.cfg.master_key, e).ok())
        .and_then(|b| String::from_utf8(b).ok())
}

/// Host client of a project (`Err` = readable configuration problem).
pub fn client(st: &AppState, p: &Project) -> Result<HostClient, String> {
    HostClient::new(p, token_of(st, p), st.cfg.git_host_guard())
}

fn backoff(failures: i32, poll_secs: u64) -> chrono::Duration {
    let base = poll_secs.max(5) as i64;
    let secs = base
        .saturating_mul(1i64 << failures.clamp(0, 12))
        .min(MAX_BACKOFF_SECS);
    chrono::Duration::seconds(secs)
}

/// A poll failed: backoff (or the host's rate-limit time), lease released.
async fn failed(
    st: &AppState,
    id: Uuid,
    msg: &str,
    until: Option<DateTime<Utc>>,
) -> sqlx::Result<()> {
    let row: Option<PrRow> = sqlx::query_as("SELECT * FROM pull_requests WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?;
    let Some(row) = row else { return Ok(()) };
    let rate = until.is_some();
    let failures = if rate { row.failures } else { row.failures + 1 };
    let next = until.unwrap_or_else(|| Utc::now() + backoff(failures, st.cfg.pr_poll_secs));
    sqlx::query(
        "UPDATE pull_requests SET failures=$2, next_poll_at=$3, lease_until=NULL, last_error=$4,
                rate_until=$5, last_polled_at=now(), updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .bind(failures)
    .bind(next)
    .bind(msg.chars().take(500).collect::<String>())
    .bind(until)
    .execute(&st.db)
    .await?;
    // Tell the owner once when the host keeps refusing.
    if !rate && failures == 3 {
        notify(
            st,
            row.user_id,
            Note {
                kind: "pr",
                title: format!("{}: Git-Server nicht erreichbar", row.label()),
                body: format!("{msg}\nQueue versucht es mit wachsendem Abstand weiter."),
                task_id: Some(row.task_id),
                push: true,
            },
        )
        .await;
    }
    Ok(())
}

async fn on_error(st: &AppState, id: Uuid, e: HostError) -> anyhow::Result<()> {
    let until = match &e {
        HostError::RateLimited(t) => Some(*t),
        _ => None,
    };
    failed(st, id, &e.to_string(), until).await?;
    Ok(())
}

async fn task_event(st: &AppState, task: Uuid, kind: &str, msg: &str) {
    if let Ok(Some(t)) = queue::load_task(st, task).await {
        queue::event(st, &t, kind, msg).await;
    }
}

/// One poll of one leased row.
pub async fn poll(st: &AppState, row: PrRow) -> anyhow::Result<()> {
    let project: Option<Project> = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(row.project_id)
        .fetch_optional(&st.db)
        .await?;
    let Some(project) = project else {
        return Ok(());
    };
    let host = match client(st, &project) {
        Ok(h) => h,
        Err(msg) => {
            failed(st, row.id, &msg, None).await?;
            return Ok(());
        }
    };
    let mut row = row;
    if row.bot_login.is_none() {
        match host.whoami().await {
            Ok((login, id)) => {
                sqlx::query("UPDATE pull_requests SET bot_login=$2, bot_id=$3 WHERE id=$1")
                    .bind(row.id)
                    .bind(&login)
                    .bind(id)
                    .execute(&st.db)
                    .await?;
                row.bot_login = Some(login);
                row.bot_id = id;
            }
            Err(e) => return on_error(st, row.id, e).await,
        }
    }
    if row.state() == PrState::Creating {
        return create(st, &project, &host, row).await;
    }
    let Some(number) = row.number else {
        return Ok(());
    };
    let mut etags = row.etags.as_object().cloned().unwrap_or_default();
    let pr_key = format!("pr:{number}");
    let info = match host
        .pr(number, etags.get(&pr_key).and_then(Value::as_str))
        .await
    {
        Ok(Some((info, tag))) => {
            match tag {
                Some(t) => etags.insert(pr_key, json!(t)),
                None => etags.remove(&pr_key),
            };
            Some(info)
        }
        Ok(None) => None,
        Err(e) => return on_error(st, row.id, e).await,
    };
    let state = info.as_ref().map(|i| i.state).unwrap_or(row.state());
    if !state.is_open() {
        return closed(st, &row, state).await;
    }
    let head_sha = info
        .as_ref()
        .and_then(|i| i.head_sha.clone())
        .or(row.head_sha.clone());
    let draft = info.as_ref().map(|i| i.draft).unwrap_or(row.draft);
    let conflict = info.as_ref().map(|i| i.conflict).unwrap_or(row.conflict);
    let url = info
        .as_ref()
        .and_then(|i| i.url.clone())
        .or(row.url.clone());

    let seen: Vec<(String, String)> =
        sqlx::query_as("SELECT source, external_id FROM pr_feedback WHERE pr_id=$1")
            .bind(row.id)
            .fetch_all(&st.db)
            .await?;
    // CI of the head commit. A commit whose CI is final and handled (green,
    // or red and recorded) is not asked again: saves the host's quota
    // (a re-run on the same commit shows up with the next commit).
    let handled = match (&head_sha, CiState::parse(&row.ci_state)) {
        (Some(sha), Some(CiState::Success)) => row.head_sha.as_ref() == Some(sha),
        (Some(sha), Some(CiState::Failure)) => {
            row.head_sha.as_ref() == Some(sha) && seen.iter().any(|(s, id)| s == "ci" && id == sha)
        }
        _ => false,
    };
    let (checks, details) = match &head_sha {
        Some(_) if handled => (
            serde_json::from_value(row.checks.clone()).unwrap_or_default(),
            vec![],
        ),
        Some(sha) => match host.ci(number, sha).await {
            Ok(rep) => (rep.checks, rep.details),
            Err(e) => return on_error(st, row.id, e).await,
        },
        None => (vec![], vec![]),
    };
    let ci = ci_state(&checks);

    // Comments (only what is new decides anything).
    let known_reviews: Vec<String> = seen
        .iter()
        .filter(|(s, _)| s == "review")
        .map(|(_, id)| id.clone())
        .collect();
    let (comments, new_etags) = match host.comments(number, &etags, &known_reviews).await {
        Ok(c) => c,
        Err(e) => return on_error(st, row.id, e).await,
    };
    let mut decided: Vec<(Comment, FeedbackOutcome)> = vec![];
    let mut writers: HashMap<String, bool> = HashMap::new();
    for c in comments {
        if c.system
            || seen
                .iter()
                .any(|(s, id)| s == c.source.as_str() && *id == c.id)
        {
            continue;
        }
        let own = row.bot_login.as_deref() == Some(c.author.as_str())
            || (c.author_id.is_some() && c.author_id == row.bot_id);
        let outcome = if c.bot || own {
            FeedbackOutcome::IgnoredBot
        } else if !c.triggers() {
            FeedbackOutcome::IgnoredUntriggered
        } else {
            let ok = match writers.get(&c.author) {
                Some(ok) => *ok,
                None => match host.can_write(&c.author, c.author_id).await {
                    Ok(ok) => {
                        writers.insert(c.author.clone(), ok);
                        ok
                    }
                    // Unknown permission: nothing recorded, retried later.
                    Err(e) => return on_error(st, row.id, e).await,
                },
            };
            if ok {
                FeedbackOutcome::Task
            } else {
                FeedbackOutcome::IgnoredPermission
            }
        };
        decided.push((c, outcome));
    }
    for (url, tag) in new_etags {
        match tag {
            Some(t) => etags.insert(url, json!(t)),
            None => etags.remove(&url),
        };
    }
    let ci_item = match (&head_sha, ci) {
        (Some(sha), CiState::Failure) if !seen.iter().any(|(s, id)| s == "ci" && id == sha) => {
            Some((sha.clone(), details))
        }
        _ => None,
    };
    apply(
        st,
        &project,
        &row,
        Polled {
            draft,
            conflict,
            url,
            head_sha,
            ci,
            checks,
            etags: Value::Object(etags),
            decided,
            ci_item,
            pause: host.quota_pause(),
        },
    )
    .await
}

/// Result of the host calls of one poll.
struct Polled {
    draft: bool,
    conflict: bool,
    url: Option<String>,
    head_sha: Option<String>,
    ci: CiState,
    checks: Vec<Check>,
    etags: Value,
    decided: Vec<(Comment, FeedbackOutcome)>,
    /// Failed CI of a head commit not handled yet: (sha, untrusted details).
    ci_item: Option<(String, Vec<crate::githost::CiDetail>)>,
    /// Quota used up until then (rate-limit headers of a successful answer).
    pause: Option<DateTime<Utc>>,
}

/// Statuses after which a follow-up does not run any more.
const FINISHED: [&str; 4] = [
    status::DONE,
    status::FAILED,
    status::BLOCKED,
    status::CANCELLED,
];

/// Writes the poll result; feedback that asks for work becomes one
/// follow-up task – all in one transaction with the pull request row locked.
async fn apply(st: &AppState, project: &Project, row: &PrRow, p: Polled) -> anyhow::Result<()> {
    let mut tx = st.db.begin().await?;
    // Serializes with "extra round"/"auto-fix" changes and other pollers.
    let cur: PrRow = sqlx::query_as("SELECT * FROM pull_requests WHERE id=$1 FOR UPDATE")
        .bind(row.id)
        .fetch_one(&mut *tx)
        .await?;
    // Another poller (lease expired mid-poll) may have handled items of
    // this poll meanwhile: decide only on what is still unrecorded.
    let recorded: Vec<(String, String)> =
        sqlx::query_as("SELECT source, external_id FROM pr_feedback WHERE pr_id=$1")
            .bind(row.id)
            .fetch_all(&mut *tx)
            .await?;
    let mut p = p;
    p.decided.retain(|(c, _)| {
        !recorded
            .iter()
            .any(|(s, id)| s == c.source.as_str() && *id == c.id)
    });
    if p.ci_item
        .as_ref()
        .is_some_and(|(sha, _)| recorded.iter().any(|(s, id)| s == "ci" && id == sha))
    {
        p.ci_item = None;
    }
    let active: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE pr_id=$1 AND NOT (status = ANY($2)) LIMIT 1",
    )
    .bind(row.id)
    .bind(FINISHED.map(String::from).to_vec())
    .fetch_optional(&mut *tx)
    .await?;
    let wants_work =
        p.ci_item.is_some() || p.decided.iter().any(|(_, o)| *o == FeedbackOutcome::Task);
    // While a follow-up is unfinished, new work waits (not recorded, so it
    // is picked up afterwards); ignored items are recorded right away.
    let outcome_for_work = if active.is_some() {
        None
    } else if !cur.auto_fix {
        Some(FeedbackOutcome::IgnoredDisabled)
    } else if cur.followups >= cur.max_followups {
        Some(FeedbackOutcome::IgnoredLimit)
    } else {
        Some(FeedbackOutcome::Task)
    };
    let mut etags = p.etags.clone();
    if outcome_for_work.is_none() && p.decided.iter().any(|(_, o)| *o == FeedbackOutcome::Task) {
        // Deferred comments must be listed again next time: no 304 for
        // the comment lists until they are recorded.
        if let Some(m) = etags.as_object_mut() {
            m.retain(|k, _| k.starts_with("pr:"));
        }
    }
    let mut new_task: Option<Task> = None;
    if wants_work && outcome_for_work == Some(FeedbackOutcome::Task) {
        let origin = queue::load_task(st, row.task_id).await?;
        if let Some(origin) = origin {
            let mut items = vec![];
            if let Some((sha, details)) = &p.ci_item {
                if details.is_empty() {
                    items.push(PrFeedbackItem {
                        header: format!(
                            "CI of commit {} failed (no details reported by the host)",
                            &sha[..8.min(sha.len())]
                        ),
                        text: String::new(),
                    });
                }
                for d in details {
                    items.push(PrFeedbackItem {
                        header: format!(
                            "CI check \"{}\" failed on commit {}",
                            d.name.replace(['\n', '\r'], " "),
                            &sha[..8.min(sha.len())]
                        ),
                        text: d.text.clone(),
                    });
                }
            }
            for (c, o) in &p.decided {
                if *o != FeedbackOutcome::Task {
                    continue;
                }
                let place = match (&c.path, c.line) {
                    (Some(path), Some(l)) => format!(" on {path}:{l}"),
                    (Some(path), None) => format!(" on {path}"),
                    _ => String::new(),
                };
                let what = match c.source {
                    FeedbackSource::ReviewComment => "Inline review comment",
                    FeedbackSource::Review => "Review",
                    _ => "Comment",
                };
                items.push(PrFeedbackItem {
                    header: format!("{what} by @{} (write access verified){place}", c.author),
                    text: c.body.clone(),
                });
            }
            let ci_only = p.decided.iter().all(|(_, o)| *o != FeedbackOutcome::Task);
            let title = format!(
                "{}: {}",
                row.label(),
                if ci_only {
                    "CI-Fehler beheben"
                } else if p.ci_item.is_some() {
                    "CI-Fehler und Review umsetzen"
                } else {
                    "Review-Kommentare umsetzen"
                }
            );
            let nonce = crypto::random_token();
            let prompt = prompt::pr_followup(
                &row.label(),
                &row.head_branch,
                &origin.title,
                &origin.prompt,
                &items,
                &nonce[..16.min(nonce.len())],
            );
            let task = queue::insert_task_in(
                &mut tx,
                NewTask {
                    user_id: row.user_id,
                    project_id: row.project_id,
                    parent_id: Some(origin.id),
                    kind: "pr_fix",
                    title,
                    prompt,
                    context: String::new(),
                    provider: origin.provider.clone(),
                    model: origin.model.clone(),
                    status: status::QUEUED,
                    attempt: 0,
                    depth: origin.depth,
                    // From third-party input: no test exemption ever.
                    created_by_agent: true,
                    front: false,
                    limits: TaskLimits::default(),
                    connector_id: origin.connector_id,
                    escalation_level: 0,
                    allow_test_changes: false,
                    carry_from: None,
                    pr_id: Some(row.id),
                },
            )
            .await?;
            sqlx::query("UPDATE pull_requests SET followups=followups+1 WHERE id=$1")
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            new_task = Some(task);
        }
    }
    let task_id = new_task.as_ref().map(|t| t.id);
    let record = |source: &str, ext: &str, author: Option<&str>, outcome: FeedbackOutcome| {
        sqlx::query(
            "INSERT INTO pr_feedback (pr_id, source, external_id, author, outcome, task_id)
             VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (pr_id, source, external_id) DO NOTHING",
        )
        .bind(row.id)
        .bind(source.to_string())
        .bind(ext.to_string())
        .bind(author.map(str::to_string))
        .bind(outcome.as_str())
        .bind(if outcome == FeedbackOutcome::Task {
            task_id
        } else {
            None
        })
    };
    for (c, o) in &p.decided {
        let o = match o {
            FeedbackOutcome::Task => match outcome_for_work {
                // Unfinished follow-up: left for later.
                None => continue,
                Some(FeedbackOutcome::Task) if task_id.is_none() => continue,
                Some(w) => w,
            },
            other => *other,
        };
        record(c.source.as_str(), &c.id, Some(&c.author), o)
            .execute(&mut *tx)
            .await?;
    }
    if let Some((sha, _)) = &p.ci_item {
        if let Some(w) = outcome_for_work {
            if w != FeedbackOutcome::Task || task_id.is_some() {
                record("ci", sha, None, w).execute(&mut *tx).await?;
            }
        }
    }
    sqlx::query(
        "UPDATE pull_requests SET draft=$2, conflict=$3, url=COALESCE($4, url), head_sha=$5, ci_state=$6,
                checks=$7, etags=$8, failures=0, last_error=NULL, last_polled_at=now(), rate_until=$10,
                next_poll_at=GREATEST(now() + make_interval(secs => $9), COALESCE($10, now())),
                lease_until=NULL, updated_at=now()
         WHERE id=$1",
    )
    .bind(row.id)
    .bind(p.draft)
    .bind(p.conflict)
    .bind(&p.url)
    .bind(&p.head_sha)
    .bind(p.ci.as_str())
    .bind(serde_json::to_value(&p.checks).unwrap_or_default())
    .bind(&etags)
    .bind(st.cfg.pr_poll_secs.max(1) as f64)
    .bind(p.pause)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // After the commit: tell people.
    if let Some(t) = &new_task {
        queue::announce(st, t);
        task_event(
            st,
            row.task_id,
            "pr",
            &format!("Folge-Auftrag aus {} gestartet: {}", row.label(), t.title),
        )
        .await;
        notify(
            st,
            row.user_id,
            Note {
                kind: "pr",
                title: format!("{}: Folge-Auftrag", row.label()),
                body: t.title.clone(),
                task_id: Some(t.id),
                push: false,
            },
        )
        .await;
    } else if wants_work && outcome_for_work == Some(FeedbackOutcome::IgnoredLimit) {
        task_event(
            st,
            row.task_id,
            "pr",
            &format!(
                "{}: Runden-Grenze ({}) erreicht – neue Rückmeldungen werden nicht mehr automatisch bearbeitet",
                row.label(),
                cur.max_followups
            ),
        )
        .await;
        notify(
            st,
            row.user_id,
            Note {
                kind: "pr",
                title: format!("{}: Runden-Grenze erreicht", row.label()),
                body: format!(
                    "Neue Rückmeldungen zu „{}“ warten auf dich. „Eine weitere Runde erlauben“ startet einen weiteren Folge-Auftrag.",
                    row.title
                ),
                task_id: Some(row.task_id),
                push: true,
            },
        )
        .await;
    }
    let ignored = p
        .decided
        .iter()
        .filter(|(_, o)| *o == FeedbackOutcome::IgnoredPermission)
        .count();
    if ignored > 0 {
        task_event(
            st,
            row.task_id,
            "pr",
            &format!(
                "{}: {ignored} Kommentar(e) ignoriert – Verfasser ohne Schreibrecht",
                row.label()
            ),
        )
        .await;
    }
    if p.conflict && !row.conflict {
        task_event(
            st,
            row.task_id,
            "pr",
            &format!(
                "{} hat einen Konflikt mit {} – bitte auf dem Git-Server lösen",
                row.label(),
                row.base_branch
            ),
        )
        .await;
        notify(
            st,
            row.user_id,
            Note {
                kind: "pr",
                title: format!("{}: Konflikt", row.label()),
                body: format!("Der Branch passt nicht mehr zu {}.", row.base_branch),
                task_id: Some(row.task_id),
                push: true,
            },
        )
        .await;
    }
    let _ = project;
    emit_tasks(st, row).await;
    Ok(())
}

/// The UI shows pull requests in the drawer of their tasks: refresh them.
async fn emit_tasks(st: &AppState, row: &PrRow) {
    queue::emit_task(st, row.task_id).await;
}

/// Creates the pull request on the host (or adopts the open one).
async fn create(
    st: &AppState,
    project: &Project,
    host: &HostClient,
    row: PrRow,
) -> anyhow::Result<()> {
    let existing = match host.find_open(&row.head_branch, &row.base_branch).await {
        Ok(e) => e,
        Err(e) => return on_error(st, row.id, e).await,
    };
    let info = match existing {
        Some(i) => i,
        None => {
            let body = body_of(st, &row).await?;
            match host
                .create(
                    &row.head_branch,
                    &row.base_branch,
                    &row.title,
                    &body,
                    project.pr_draft,
                    &project.pr_labels,
                )
                .await
            {
                Ok(i) => {
                    if let Err(e) = host.add_labels(i.number, &project.pr_labels).await {
                        // Labels are optional: the request exists.
                        task_event(
                            st,
                            row.task_id,
                            "pr",
                            &format!("Labels konnten nicht gesetzt werden: {e}"),
                        )
                        .await;
                    }
                    i
                }
                Err(e) => return on_error(st, row.id, e).await,
            }
        }
    };
    sqlx::query(
        "UPDATE pull_requests SET state='open', number=$2, url=$3, draft=$4, head_sha=COALESCE($5, head_sha),
                failures=0, last_error=NULL, last_polled_at=now(), rate_until=$7,
                next_poll_at=GREATEST(now() + make_interval(secs => $6), COALESCE($7, now())),
                lease_until=NULL, updated_at=now()
         WHERE id=$1",
    )
    .bind(row.id)
    .bind(info.number)
    .bind(&info.url)
    .bind(info.draft)
    .bind(&info.head_sha)
    .bind(st.cfg.pr_poll_secs.max(1) as f64)
    .bind(host.quota_pause())
    .execute(&st.db)
    .await?;
    let label = format!("{} #{}", host.kind.request_name(), info.number);
    task_event(
        st,
        row.task_id,
        "pr",
        &format!(
            "{label} angelegt{}",
            info.url
                .as_deref()
                .map(|u| format!(": {u}"))
                .unwrap_or_default()
        ),
    )
    .await;
    notify(
        st,
        row.user_id,
        Note {
            kind: "pr",
            title: format!("{label} angelegt: {}", row.title),
            body: info.url.clone().unwrap_or_default(),
            task_id: Some(row.task_id),
            push: false,
        },
    )
    .await;
    emit_tasks(st, &row).await;
    Ok(())
}

/// Description of the request: the task (human input), the agent's summary
/// (redacted) and the guardrails.
async fn body_of(st: &AppState, row: &PrRow) -> anyhow::Result<String> {
    let t = queue::load_task(st, row.task_id).await?;
    let (prompt, summary) = t
        .map(|t| (t.prompt, t.summary.unwrap_or_default()))
        .unwrap_or_default();
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    Ok(queued_common::secrets::redact(&format!(
        "## Auftrag\n\n{}\n\n## Zusammenfassung\n\n{}\n\n---\nErstellt von Queue (Auftrag `{}`), geprüft durch Verify/Review vor dem Hochladen. \
         Queue genehmigt und merged nie selbst. Rote CI und Review-Kommentare von Personen mit Schreibrecht \
         (allgemeine Kommentare mit `/queue`) starten höchstens {} Folge-Aufträge auf diesem Branch.",
        cut(prompt.trim(), 3000),
        cut(summary.trim(), 3000),
        row.task_id,
        row.max_followups
    )))
}

/// Merged or closed on the host: stop polling, tidy the local branches.
async fn closed(st: &AppState, row: &PrRow, state: PrState) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE pull_requests SET state=$2, lease_until=NULL, last_polled_at=now(), last_error=NULL,
                failures=0, updated_at=now() WHERE id=$1",
    )
    .bind(row.id)
    .bind(state.as_str())
    .execute(&st.db)
    .await?;
    let what = if state == PrState::Merged {
        "auf dem Git-Server gemergt"
    } else {
        "geschlossen"
    };
    task_event(
        st,
        row.task_id,
        "pr",
        &format!("{} wurde {what}", row.label()),
    )
    .await;
    notify(
        st,
        row.user_id,
        Note {
            kind: "pr",
            title: format!("{} {what}", row.label()),
            body: row.title.clone(),
            task_id: Some(row.task_id),
            push: false,
        },
    )
    .await;
    // Local task branches of the request are not needed any more.
    let tasks: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE (id=$1 OR pr_id=$2) AND branch IS NOT NULL AND NOT workspace_cleaned
           AND status = ANY($3)",
    )
    .bind(row.task_id)
    .bind(row.id)
    .bind(FINISHED.map(String::from).to_vec())
    .fetch_all(&st.db)
    .await?;
    for t in tasks {
        crate::runner::merge::cleanup_task(st, row.project_id, t, true).await;
    }
    emit_tasks(st, row).await;
    Ok(())
}

/// After a follow-up was pushed: new head, poll soon (fresh CI).
pub async fn pushed(st: &AppState, pr: Uuid, sha: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE pull_requests SET head_sha=$2, ci_state='pending', checks='[]', next_poll_at=now(), updated_at=now()
         WHERE id=$1",
    )
    .bind(pr)
    .bind(sha)
    .execute(&st.db)
    .await?;
    st.pr_wake.notify_one();
    Ok(())
}

/// Row for a freshly pushed task branch in mode `pr` (idempotent).
pub async fn open_for(
    st: &AppState,
    project: &Project,
    task: &Task,
    branch: &str,
    sha: &str,
) -> sqlx::Result<Option<Uuid>> {
    let Some(host) = project.host() else {
        return Ok(None);
    };
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO pull_requests (id, project_id, task_id, user_id, host, head_branch, base_branch, title,
                                    head_sha, auto_fix, max_followups, draft)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
         ON CONFLICT (project_id, head_branch) DO NOTHING RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(project.id)
    .bind(task.id)
    .bind(task.user_id)
    .bind(host.as_str())
    .bind(branch)
    .bind(&project.git_branch)
    .bind(task.title.chars().take(200).collect::<String>())
    .bind(sha)
    .bind(project.pr_auto_fix)
    .bind(project.pr_max_followups)
    .bind(project.pr_draft)
    .fetch_optional(&st.db)
    .await?;
    st.pr_wake.notify_one();
    Ok(id)
}

/// Unfinished follow-up of a pull request.
pub async fn active_task(st: &AppState, pr: Uuid) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT id FROM tasks WHERE pr_id=$1 AND NOT (status = ANY($2)) LIMIT 1")
        .bind(pr)
        .bind(FINISHED.map(String::from).to_vec())
        .fetch_optional(&st.db)
        .await
}

pub async fn load(st: &AppState, pr: Uuid) -> sqlx::Result<Option<PrRow>> {
    sqlx::query_as("SELECT * FROM pull_requests WHERE id=$1")
        .bind(pr)
        .fetch_optional(&st.db)
        .await
}

pub async fn view_of(st: &AppState, row: &PrRow) -> sqlx::Result<PullRequestView> {
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM tasks WHERE id=$1")
        .bind(row.task_id)
        .fetch_optional(&st.db)
        .await?;
    let active = active_task(st, row.id).await?;
    Ok(row.view(title.unwrap_or_else(|| row.title.clone()), active))
}

pub async fn feedback_of(st: &AppState, pr: Uuid) -> sqlx::Result<Vec<FeedbackView>> {
    let rows: Vec<(String, Option<String>, String, Option<Uuid>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT source, author, outcome, task_id, created_at FROM pr_feedback WHERE pr_id=$1
         ORDER BY created_at DESC, id DESC LIMIT 100",
    )
    .bind(pr)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(s, a, o, t, c)| {
            Some(FeedbackView {
                source: FeedbackSource::parse(&s)?,
                author: a,
                outcome: FeedbackOutcome::parse(&o)?,
                task_id: t,
                created_at: c,
            })
        })
        .collect())
}

/// "Allow one more round": the limit of this request goes up by one and
/// feedback that was ignored because of the limit is looked at again.
pub async fn extra_round(st: &AppState, pr: Uuid) -> sqlx::Result<()> {
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "UPDATE pull_requests SET max_followups = LEAST(GREATEST(max_followups, followups) + 1, 20),
                next_poll_at=now(), etags='{}', updated_at=now() WHERE id=$1",
    )
    .bind(pr)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM pr_feedback WHERE pr_id=$1 AND outcome='ignored_limit'")
        .bind(pr)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    st.pr_wake.notify_one();
    Ok(())
}

/// Auto-fix on/off for one request; switching it on looks again at what
/// was ignored while it was off.
pub async fn set_auto_fix(st: &AppState, pr: Uuid, on: bool) -> sqlx::Result<()> {
    let mut tx = st.db.begin().await?;
    sqlx::query("UPDATE pull_requests SET auto_fix=$2, next_poll_at=now(), etags=CASE WHEN $2 THEN '{}'::jsonb ELSE etags END, updated_at=now() WHERE id=$1")
        .bind(pr)
        .bind(on)
        .execute(&mut *tx)
        .await?;
    if on {
        sqlx::query("DELETE FROM pr_feedback WHERE pr_id=$1 AND outcome='ignored_disabled'")
            .bind(pr)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    st.pr_wake.notify_one();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff(0, 60), chrono::Duration::seconds(60));
        assert_eq!(backoff(1, 60), chrono::Duration::seconds(120));
        assert_eq!(backoff(3, 60), chrono::Duration::seconds(480));
        assert_eq!(backoff(30, 60), chrono::Duration::seconds(MAX_BACKOFF_SECS));
        assert_eq!(backoff(0, 1), chrono::Duration::seconds(5), "at least 5 s");
    }
}
