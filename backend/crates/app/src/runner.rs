//! Executes one task: fresh agent → result contract → verify → review →
//! git commit → done | waiting_input | failed (+ fix task) | blocked, or –
//! for provider/infra/limit problems – re-queued / provider paused / blocked
//! without consuming a fix attempt (see [`classify_run`]).
//! All file/process work happens in queued-runner.
use chrono::{DateTime, Utc};
use queued_common::{ExecFrame, ExecRequest, GitRemote};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    agent::{
        self, AgentEvent, Capabilities, Credential, ErrorCategory, LimitKind, ParseState,
        RetryInfo, RunKind, RunLimits,
    },
    budget, crypto,
    limits::{self, EffectiveLimits, Guard},
    models::{
        status, AgentResult, FailureClass, FollowUp, Project, ReviewResult, Settings, Task, Usage,
    },
    notify::{notify, Note},
    prompt::{self, Parent},
    queue::{self, event, finish, Finish, NewTask, Requeue, TaskLimits},
    state::AppState,
};

/// Agent-generated tasks deeper than this always need approval (loop guard).
const MAX_AUTO_DEPTH: i32 = 3;

/// Context notes for the next agent after an interrupted run (English, for the agent).
pub const NOTE_RATE_LIMIT: &str = "(Note: a previous run was interrupted by a usage limit; the repository may contain partial changes from it – see git log.)";
pub const NOTE_TRANSIENT: &str = "(Note: a previous run was interrupted by a transient provider error; the repository may contain partial changes from it – see git log.)";
pub const NOTE_INFRA: &str = "(Note: a previous run was interrupted by an infrastructure error; the repository may contain partial changes from it – see git log.)";

fn note_stalled(secs: u64) -> String {
    format!("(Note: a previous run was stopped because it produced no output for {secs} seconds (possibly waiting for interactive input – never run interactive commands); the repository may contain partial changes from it – see git log.)")
}

/// Errors that end a run before its outcome could be classified.
pub enum RunError {
    /// Broken configuration (e.g. unsupported credential type) → blocked.
    Config(String),
    /// Runner/DB/orchestration failure → re-queued with backoff.
    Infra(anyhow::Error),
}

impl From<anyhow::Error> for RunError {
    fn from(e: anyhow::Error) -> Self {
        RunError::Infra(e)
    }
}

impl From<sqlx::Error> for RunError {
    fn from(e: sqlx::Error) -> Self {
        RunError::Infra(e.into())
    }
}

pub async fn run_task(st: AppState, task: Task, cancel: CancellationToken) {
    let id = task.id;
    // Usage is cumulative over all runs of this task (limits are per task).
    let mut usage: Usage = task
        .usage
        .clone()
        .and_then(|u| serde_json::from_value(u).ok())
        .unwrap_or_default();
    match run_inner(&st, &task, &cancel, &mut usage).await {
        Ok(()) => {}
        Err(_) if cancel.is_cancelled() => {
            let _ = finish(
                &st,
                id,
                Finish {
                    status: status::CANCELLED,
                    summary: None,
                    handoff: None,
                    error: Some("abgebrochen".into()),
                    review: None,
                    questions: None,
                    usage: &usage,
                    failure_class: Some(FailureClass::Cancelled),
                },
            )
            .await;
        }
        Err(RunError::Config(msg)) => {
            tracing::warn!("task {id}: configuration error: {msg}");
            event(&st, &task, "error", &msg).await;
            let msg = format!("Konfigurationsfehler: {msg}");
            if block_running(&st, id, FailureClass::Config, &msg, &usage).await {
                notify_blocked(&st, &task, &msg).await;
            }
        }
        Err(RunError::Infra(e)) => {
            let msg = format!("{e:#}");
            tracing::error!("task {id}: infrastructure error: {msg}");
            event(&st, &task, "error", &format!("Infrastrukturfehler: {msg}")).await;
            transient_requeue(
                &st,
                &task,
                FailureClass::Infra,
                &format!("Infrastrukturfehler: {msg}"),
                Some(NOTE_INFRA),
                &usage,
            )
            .await;
        }
    }
    st.running.lock().await.remove(&id);
    st.wake.notify_one();
}

/// Sets a still-running task to `blocked` (no-op if it already ended).
async fn block_running(
    st: &AppState,
    id: Uuid,
    class: FailureClass,
    msg: &str,
    usage: &Usage,
) -> bool {
    let n = sqlx::query(
        "UPDATE tasks SET status='blocked', failure_class=$2, error=$3, usage=$4, not_before=NULL,
                finished_at=now(), updated_at=now()
         WHERE id=$1 AND status='running'",
    )
    .bind(id)
    .bind(class.as_str())
    .bind(cut(msg, 8000))
    .bind(serde_json::to_value(usage).unwrap_or_default())
    .execute(&st.db)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or_else(|e| {
        tracing::error!("task {id}: could not block: {e:#}");
        0
    });
    queue::emit_task(st, id).await;
    n > 0
}

async fn notify_blocked(st: &AppState, task: &Task, msg: &str) {
    notify(
        st,
        task.user_id,
        Note {
            kind: "blocked",
            title: format!("Blockiert: {}", task.title),
            body: cut(msg, 400),
            task_id: Some(task.id),
            push: true,
        },
    )
    .await;
}

fn class_label(c: FailureClass) -> &'static str {
    match c {
        FailureClass::Task => "Task-Fehler",
        FailureClass::RateLimit => "Abo-Limit",
        FailureClass::ProviderTransient => "Provider gestört",
        FailureClass::Infra => "Infrastruktur",
        FailureClass::Stalled => "Stillstand",
        FailureClass::ProviderAuth => "Anmeldung/Abrechnung",
        FailureClass::Config => "Konfiguration",
        FailureClass::LimitRuntime => "Limit: Laufzeit",
        FailureClass::LimitCost => "Limit: Kosten",
        FailureClass::LimitTokens => "Limit: Tokens",
        FailureClass::LimitTurns => "Limit: Turns",
        FailureClass::Cancelled => "abgebrochen",
    }
}

/// Re-queue with backoff for transient classes, or block once
/// `TRANSIENT_MAX_RETRIES` is exceeded. Never consumes a fix attempt.
async fn transient_requeue(
    st: &AppState,
    task: &Task,
    class: FailureClass,
    reason: &str,
    note: Option<&str>,
    usage: &Usage,
) {
    let n = task.transient_retries + 1;
    if n > st.cfg.transient_max_retries {
        let msg = format!(
            "Nach {} Wiederholungen aufgegeben ({}): {reason}",
            st.cfg.transient_max_retries,
            class_label(class)
        );
        let _ = sqlx::query("UPDATE tasks SET transient_retries=$2 WHERE id=$1")
            .bind(task.id)
            .bind(n)
            .execute(&st.db)
            .await;
        if block_running(st, task.id, class, &msg, usage).await {
            event(st, task, "error", &msg).await;
            notify_blocked(st, task, &msg).await;
        }
        return;
    }
    let delay = limits::backoff(n, st.cfg.backoff_base_secs, st.cfg.backoff_max_secs);
    let not_before = Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default();
    match queue::requeue(
        st,
        task.id,
        Requeue {
            class,
            transient_retries: n,
            not_before: Some(not_before),
            note,
            error: Some(cut(reason, 2000)),
            usage,
        },
    )
    .await
    {
        Ok(true) => {
            event(
                st,
                task,
                "status",
                &format!(
                    "Erneut eingereiht ({}) – neuer Versuch in {} s ({n}/{}), kein Fix-Versuch verbraucht",
                    class_label(class),
                    delay.as_secs(),
                    st.cfg.transient_max_retries
                ),
            )
            .await
        }
        Ok(false) => {}
        Err(e) => tracing::error!("task {}: re-queue failed: {e:#}", task.id),
    }
}

#[derive(Default, Debug)]
pub(crate) struct RunOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The runner stopped the process because of a cancel (user or guard).
    pub cancelled: bool,
    pub stalled: bool,
    pub killed: bool,
    pub usage_delta: Usage,
    pub usage_total: Option<Usage>,
    pub final_text: String,
    pub error: Option<String>,
    pub failure: Option<(ErrorCategory, String)>,
    pub cli_limit: Option<LimitKind>,
    pub guard_limit: Option<LimitKind>,
    pub structured: Option<Value>,
    /// Runner unreachable / stream interrupted / process could not start.
    pub infra_error: Option<String>,
    pub stderr_tail: String,
}

impl RunOutcome {
    pub fn usage(&self) -> Usage {
        self.usage_total
            .clone()
            .unwrap_or_else(|| self.usage_delta.clone())
    }

    fn set_failure(&mut self, category: ErrorCategory, message: String) {
        let keep = match (&self.failure, &category) {
            (None, _) => false,
            // A reset time beats none; otherwise the first terminal error wins.
            (Some((ErrorCategory::RateLimit(None), _)), ErrorCategory::RateLimit(Some(_))) => false,
            (Some((ErrorCategory::Unknown(_), _)), c) => matches!(c, ErrorCategory::Unknown(_)),
            _ => true,
        };
        if !keep {
            self.failure = Some((category, message));
        }
    }
}

/// What happens to the task after the agent run.
#[derive(Debug, PartialEq)]
pub(crate) enum Disposition {
    Cancelled,
    /// Valid result contract: its status decides (done / failed / needs_input).
    Contract(AgentResult),
    /// No contract, provider without enforced schema: exit code decides.
    ExitFallback {
        success: bool,
    },
    /// Real task failure → fix task.
    TaskFailure(String),
    /// Transient: re-queue with backoff.
    Requeue {
        class: FailureClass,
        reason: String,
    },
    /// Subscription limit: pause provider, re-queue.
    RateLimited(Option<DateTime<Utc>>),
    /// Auth/billing: pause provider until credentials change, re-queue.
    AuthPaused(String),
    Blocked {
        class: FailureClass,
        reason: String,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) enum Pause {
    RateLimit(Option<DateTime<Utc>>),
    Auth(String),
}

fn limit_class(k: LimitKind) -> FailureClass {
    match k {
        LimitKind::Runtime => FailureClass::LimitRuntime,
        LimitKind::Cost => FailureClass::LimitCost,
        LimitKind::Tokens => FailureClass::LimitTokens,
        LimitKind::Turns => FailureClass::LimitTurns,
    }
}

fn limit_reason(k: LimitKind, l: &EffectiveLimits, by_cli: bool) -> String {
    let what = match k {
        LimitKind::Runtime => format!("Laufzeit {} s", l.runtime_secs),
        LimitKind::Cost => match l.cost_usd {
            Some(c) => format!("Kosten ${c:.4}"),
            None => "Kosten".into(),
        },
        LimitKind::Tokens => match l.tokens {
            Some(t) => format!("Tokens {t}"),
            None => "Tokens".into(),
        },
        LimitKind::Turns => match l.turns {
            Some(t) => format!("Turns {t}"),
            None => "Turns".into(),
        },
    };
    format!(
        "Limit überschritten: {what}{}",
        if by_cli {
            " (von der CLI gemeldet)"
        } else {
            ""
        }
    )
}

/// Decisions 1–5 of [`classify_run`] that do not need the result contract.
fn early(
    o: &RunOutcome,
    user_cancel: bool,
    provider: &str,
    l: &EffectiveLimits,
) -> Option<Disposition> {
    if user_cancel {
        return Some(Disposition::Cancelled);
    }
    if let Some(k) = o.guard_limit {
        return Some(Disposition::Blocked {
            class: limit_class(k),
            reason: limit_reason(k, l, false),
        });
    }
    if o.timed_out {
        return Some(Disposition::Blocked {
            class: FailureClass::LimitRuntime,
            reason: limit_reason(LimitKind::Runtime, l, false),
        });
    }
    if o.stalled {
        return Some(Disposition::Requeue {
            class: FailureClass::Stalled,
            reason: format!(
                "Keine Ausgabe seit {} s – Lauf gestoppt",
                l.stall_secs.unwrap_or(0)
            ),
        });
    }
    let gemini_turns = provider == "gemini"
        && o.exit_code == Some(agent::gemini_turn_limit_exit())
        && o.structured.is_none();
    if let Some(k) = o.cli_limit.or(gemini_turns.then_some(LimitKind::Turns)) {
        return Some(Disposition::Blocked {
            class: limit_class(k),
            reason: limit_reason(k, l, true),
        });
    }
    if let Some(e) = &o.infra_error {
        return Some(Disposition::Requeue {
            class: FailureClass::Infra,
            reason: e.clone(),
        });
    }
    None
}

/// Pure decision over a finished agent run (order is normative, see
/// docs/roadmap/WP01-plan.md §4). Returns the disposition and a provider
/// pause to apply even when a valid contract decides the task.
pub(crate) fn classify_run(
    o: &RunOutcome,
    user_cancel: bool,
    provider: &str,
    caps: Capabilities,
    contract: Option<AgentResult>,
    l: &EffectiveLimits,
) -> (Disposition, Option<Pause>) {
    if let Some(d) = early(o, user_cancel, provider, l) {
        return (d, None);
    }
    let pause = match &o.failure {
        Some((c, msg)) => match c.class() {
            FailureClass::RateLimit => Some(Pause::RateLimit(match c {
                ErrorCategory::RateLimit(t) => *t,
                _ => None,
            })),
            FailureClass::ProviderAuth => Some(Pause::Auth(msg.clone())),
            _ => None,
        },
        None => None,
    };
    if let Some(r) = contract {
        return (Disposition::Contract(r), pause);
    }
    if let Some((c, msg)) = &o.failure {
        let d = match c.class() {
            FailureClass::RateLimit => Disposition::RateLimited(match c {
                ErrorCategory::RateLimit(t) => *t,
                _ => None,
            }),
            FailureClass::ProviderAuth => Disposition::AuthPaused(msg.clone()),
            FailureClass::ProviderTransient => Disposition::Requeue {
                class: FailureClass::ProviderTransient,
                reason: format!("Provider gestört ({}): {}", c.label(), cut(msg, 500)),
            },
            FailureClass::Config => Disposition::Blocked {
                class: FailureClass::Config,
                reason: format!("Konfigurationsfehler ({}): {}", c.label(), cut(msg, 500)),
            },
            _ => Disposition::TaskFailure(format!("Agent error ({}): {msg}", c.label())),
        };
        return (d, None);
    }
    if caps.structured_output {
        let mut f = String::from(
            "Ergebnis-Contract fehlt: weder structured output noch .queue/result.json mit gültigem Status.",
        );
        if let Some(e) = &o.error {
            f.push_str(&format!("\n\nAgent error: {e}"));
        }
        return (Disposition::TaskFailure(f), None);
    }
    (
        Disposition::ExitFallback {
            success: o.success && o.error.is_none(),
        },
        None,
    )
}

/// Result contract: structured output (last) > `.queue/result.json` > none.
/// Returns the contract and a protocol line for the task log.
pub(crate) fn load_contract(
    structured: Option<&Value>,
    file: Option<&str>,
) -> (Option<AgentResult>, String) {
    let mut notes = vec![];
    if let Some(v) = structured {
        match serde_json::from_value::<AgentResult>(v.clone()) {
            Ok(r) if r.has_valid_status() => {
                return (Some(r), "Ergebnis via structured output".into())
            }
            Ok(_) => notes.push("structured output ohne gültigen Status".to_string()),
            Err(e) => notes.push(format!("structured output ungültig: {e}")),
        }
    }
    if let Some(f) = file {
        match serde_json::from_str::<AgentResult>(f) {
            Ok(r) if r.has_valid_status() => {
                notes.push("Ergebnis via .queue/result.json (Fallback)".into());
                return (Some(r), notes.join(" – "));
            }
            Ok(_) => notes.push(".queue/result.json ohne gültigen Status".into()),
            Err(e) => notes.push(format!(".queue/result.json ungültig: {e}")),
        }
    }
    notes.push("kein Ergebnis-Contract".into());
    (None, notes.join(" – "))
}

/// Review contract: structured output > `.queue/review.json`; `approved` must be present.
pub(crate) fn load_review(structured: Option<&Value>, file: Option<&str>) -> Option<ReviewResult> {
    let parse = |v: Value| {
        v.get("approved")
            .is_some_and(Value::is_boolean)
            .then(|| serde_json::from_value::<ReviewResult>(v).ok())
            .flatten()
    };
    structured.cloned().and_then(parse).or_else(|| {
        file.and_then(|f| serde_json::from_str::<Value>(f).ok())
            .and_then(parse)
    })
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn tail(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

pub fn project_remote(st: &AppState, p: &Project) -> Option<GitRemote> {
    let url = p.git_remote_url.clone().filter(|u| !u.trim().is_empty())?;
    let token = p
        .git_token_enc
        .as_deref()
        .and_then(|e| crypto::decrypt(&st.cfg.master_key, e).ok())
        .and_then(|b| String::from_utf8(b).ok());
    Some(GitRemote {
        url,
        branch: p.git_branch.clone(),
        token,
    })
}

/// Runner-side name of a task's run (`agent`, `review`, `verify`).
pub fn run_name(task: Uuid, label: &str) -> String {
    format!("q-{}-{label}", &task.simple().to_string()[..12])
}

/// Classes after which the next run continues the same work (keeps the
/// task's first `snapshot_before`, so diff/revert cover all runs) – but only
/// if HEAD is still the interrupted run's partial commit (`snapshot_after`).
/// Otherwise another task of the project ran in between (it may overtake a
/// task in backoff), the remote was pulled or leftovers were committed as WIP,
/// and keeping the old base would make diff/revert swallow foreign commits.
fn continues_interrupted(task: &Task, head: &str) -> bool {
    task.snapshot_before.is_some()
        && task.snapshot_after.as_deref() == Some(head)
        && task
            .failure_class
            .as_deref()
            .and_then(FailureClass::parse)
            .is_some_and(|c| {
                c.is_transient()
                    || matches!(c, FailureClass::RateLimit | FailureClass::ProviderAuth)
            })
}

async fn run_inner(
    st: &AppState,
    task: &Task,
    cancel: &CancellationToken,
    usage: &mut Usage,
) -> Result<(), RunError> {
    let project: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(task.project_id)
        .fetch_one(&st.db)
        .await?;
    let settings = queue::load_settings(st, task.user_id).await?;
    let (author_name, author_email): (String, String) =
        sqlx::query_as("SELECT name, email FROM users WHERE id=$1")
            .bind(task.user_id)
            .fetch_one(&st.db)
            .await?;
    let lim = limits::effective(task, &project, &st.cfg);
    event(
        st,
        task,
        "status",
        &format!(
            "Start #{} via {}{}",
            task.attempt,
            task.provider,
            if task.transient_retries > 0 {
                format!(" (Wiederholung {})", task.transient_retries)
            } else {
                String::new()
            }
        ),
    )
    .await;

    let Some(cred) = queue::load_credential(st, task.user_id, &task.provider).await? else {
        let msg = format!(
            "Keine Zugangsdaten für '{}' hinterlegt (Einstellungen → Provider).",
            task.provider
        );
        finish(
            st,
            task.id,
            Finish {
                status: status::BLOCKED,
                summary: None,
                handoff: None,
                error: Some(msg.clone()),
                review: None,
                questions: None,
                usage,
                failure_class: Some(FailureClass::Config),
            },
        )
        .await?;
        notify_blocked(st, task, &msg).await;
        return Ok(());
    };
    if let Some(k) = lim.exhausted(usage) {
        let msg = limit_reason(k, &lim, false);
        event(st, task, "guard", &msg).await;
        if block_running(st, task.id, limit_class(k), &msg, usage).await {
            notify_blocked(st, task, &msg).await;
        }
        return Ok(());
    }

    let remote = project_remote(st, &project);
    st.runner
        .init(
            project.id,
            &queued_common::InitWorkspace {
                remote: remote.clone(),
                branch: Some(project.git_branch.clone()),
            },
        )
        .await?;
    let begin = st.runner.begin(project.id, remote.clone()).await?;
    for w in &begin.warnings {
        event(st, task, "status", &format!("git: {w}")).await;
    }
    let before = if continues_interrupted(task, &begin.head) {
        task.snapshot_before.clone().unwrap_or(begin.head)
    } else {
        begin.head
    };
    sqlx::query("UPDATE tasks SET snapshot_before=$2 WHERE id=$1")
        .bind(task.id)
        .bind(&before)
        .execute(&st.db)
        .await?;

    let prompt = if task.kind == "onempty" {
        let history: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT title, summary FROM tasks WHERE project_id=$1 AND status='done' AND kind<>'onempty' ORDER BY finished_at DESC LIMIT 8",
        )
        .bind(project.id)
        .fetch_all(&st.db)
        .await?;
        prompt::onempty(&project, &settings.onempty_actions, &history)
    } else {
        let parent = match task.parent_id {
            Some(pid) => sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                "SELECT title, handoff, summary FROM tasks WHERE id=$1",
            )
            .bind(pid)
            .fetch_optional(&st.db)
            .await?
            .map(|(title, handoff, summary)| Parent {
                title,
                handoff,
                summary,
            }),
            None => None,
        };
        prompt::task(task, &project, parent.as_ref())
    };

    let caps = agent::capabilities(&task.provider);
    let out = agent_run(
        st,
        task,
        RunSpec {
            provider: &task.provider,
            model: task.model.as_deref(),
            cred: &cred,
            prompt,
            kind: RunKind::Agent,
            label: "agent",
            limits: &lim,
            used_before: usage,
        },
        cancel,
    )
    .await?;
    let run_usage = out.usage();
    usage.add(&run_usage);
    budget::record_usage(
        &st.db,
        task.user_id,
        &task.provider,
        Some(task.id),
        &run_usage,
    )
    .await?;
    check_budget(st, task.user_id, &task.provider).await;
    warn_post_hoc_overrun(st, task, &out, &lim, usage).await;

    let commits = Committer {
        st,
        task,
        project_id: project.id,
        author_name: &author_name,
        author_email: &author_email,
    };

    let user_cancel = cancel.is_cancelled();
    let contract = if early(&out, user_cancel, &task.provider, &lim).is_none() {
        let file = st.runner.queue_file(project.id, "result.json").await?;
        let (c, note) = load_contract(out.structured.as_ref(), file.as_deref());
        event(st, task, "contract", &note).await;
        c
    } else {
        None
    };
    let (disposition, pause) =
        classify_run(&out, user_cancel, &task.provider, caps, contract, &lim);
    match pause {
        Some(Pause::RateLimit(reset)) => {
            pause_rate_limited(st, task, &task.provider, reset).await;
        }
        Some(Pause::Auth(msg)) => pause_auth(st, task, &task.provider, &msg).await,
        None => {}
    }

    let Some(next) = interrupted(&commits, disposition, &lim, usage).await? else {
        return Ok(());
    };
    let (result, failure): (AgentResult, Option<String>) = match next {
        Continue::Contract(r) => {
            let failure = (r.status.as_deref() == Some("failed")).then(|| {
                let mut f = r
                    .summary
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| cut(&out.final_text, 1500));
                if let Some(e) = &out.error {
                    f.push_str(&format!("\n\nAgent error: {e}"));
                }
                if !out.stderr_tail.trim().is_empty() {
                    f.push_str(&format!("\n\nstderr (tail):\n{}", out.stderr_tail));
                }
                f
            });
            (r, failure)
        }
        Continue::ExitFallback { success } => {
            let failure = (!success).then(|| {
                let mut f = cut(&out.final_text, 1500);
                if let Some(e) = &out.error {
                    f.push_str(&format!("\n\nAgent error: {e}"));
                }
                if !out.stderr_tail.trim().is_empty() {
                    f.push_str(&format!("\n\nstderr (tail):\n{}", out.stderr_tail));
                }
                f
            });
            (AgentResult::default(), failure)
        }
        Continue::TaskFailure(f) => {
            let mut f = f;
            if !out.stderr_tail.trim().is_empty() {
                f.push_str(&format!("\n\nstderr (tail):\n{}", out.stderr_tail));
            }
            (AgentResult::default(), Some(f))
        }
    };

    let summary = result
        .summary
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| cut(&out.final_text, 1500));
    let handoff = result.handoff.clone().map(|h| cut(&h, prompt::MAX_HANDOFF));

    let after = commits.commit("").await?;
    set_snapshot_after(st, task.id, &after).await?;

    if failure.is_none()
        && result.status.as_deref() == Some("needs_input")
        && !result.questions.is_empty()
    {
        finish(
            st,
            task.id,
            Finish {
                status: status::WAITING_INPUT,
                summary: Some(summary),
                handoff,
                error: None,
                review: None,
                questions: Some(json!(result.questions)),
                usage,
                failure_class: None,
            },
        )
        .await?;
        notify(
            st,
            task.user_id,
            Note {
                kind: "question",
                title: format!("Rückfrage: {}", task.title),
                body: result.questions.join("\n"),
                task_id: Some(task.id),
                push: true,
            },
        )
        .await;
        return Ok(());
    }

    let mut failure = failure;
    let mut review_json = None;
    if failure.is_none() && task.kind != "onempty" {
        if let Some(cmd) = project
            .verify_command
            .as_deref()
            .filter(|c| !c.trim().is_empty())
        {
            event(st, task, "verify", &format!("$ {cmd}")).await;
            let verdict = run_verify(st, task, cmd, cancel).await?;
            if cancel.is_cancelled() {
                // A user cancel stops the verify command – not a task failure.
                finish(
                    st,
                    task.id,
                    Finish {
                        status: status::CANCELLED,
                        summary: Some(summary),
                        handoff,
                        error: Some("abgebrochen".into()),
                        review: None,
                        questions: None,
                        usage,
                        failure_class: Some(FailureClass::Cancelled),
                    },
                )
                .await?;
                return Ok(());
            }
            if let Err(msg) = verdict {
                failure = Some(format!("Verifikation `{cmd}` fehlgeschlagen:\n{msg}"));
            }
        }
        let changed = before != after;
        if failure.is_none() && project.review_enabled && changed {
            match run_review(
                st, task, &project, &before, &after, &summary, cancel, usage, &lim,
            )
            .await?
            {
                ReviewOutcome::Reviewed(r) => {
                    if !r.approved {
                        failure = Some(format!("Review abgelehnt:\n- {}", r.issues.join("\n- ")));
                    }
                    review_json = Some(json!(r));
                }
                ReviewOutcome::Skipped(why) => {
                    event(
                        st,
                        task,
                        "review",
                        &if why.is_empty() {
                            "Review übersprungen".to_string()
                        } else {
                            format!("Review übersprungen: {why}")
                        },
                    )
                    .await
                }
                ReviewOutcome::Cancelled => {
                    finish(
                        st,
                        task.id,
                        Finish {
                            status: status::CANCELLED,
                            summary: Some(summary),
                            handoff,
                            error: Some("abgebrochen".into()),
                            review: None,
                            questions: None,
                            usage,
                            failure_class: Some(FailureClass::Cancelled),
                        },
                    )
                    .await?;
                    return Ok(());
                }
                ReviewOutcome::Blocked(class, reason) => {
                    event(st, task, "guard", &reason).await;
                    if block_running(st, task.id, class, &reason, usage).await {
                        notify_blocked(st, task, &reason).await;
                    }
                    return Ok(());
                }
            }
        }
    }

    match failure {
        None => {
            if project.auto_push && before != after {
                if let Some(r) = remote {
                    let p = st.runner.push(project.id, r).await?;
                    event(
                        st,
                        task,
                        if p.ok { "status" } else { "error" },
                        &format!("git push: {}", if p.ok { "ok" } else { &p.output }),
                    )
                    .await;
                    if !p.ok {
                        notify(
                            st,
                            task.user_id,
                            Note {
                                kind: "push_failed",
                                title: format!("Push fehlgeschlagen: {}", project.name),
                                body: cut(&p.output, 400),
                                task_id: Some(task.id),
                                push: false,
                            },
                        )
                        .await;
                    }
                }
            }
            finish(
                st,
                task.id,
                Finish {
                    status: status::DONE,
                    summary: Some(summary.clone()),
                    handoff,
                    error: None,
                    review: review_json,
                    questions: None,
                    usage,
                    failure_class: None,
                },
            )
            .await?;
            event(st, task, "status", "Erledigt").await;
            let created = create_followups(st, task, &settings, result.follow_up_tasks).await?;
            if created == 0 && task.kind != "onempty" {
                notify(
                    st,
                    task.user_id,
                    Note {
                        kind: "done",
                        title: format!("Erledigt: {}", task.title),
                        body: cut(&summary, 300),
                        task_id: Some(task.id),
                        push: false,
                    },
                )
                .await;
            }
        }
        Some(f) => {
            event(st, task, "error", &f).await;
            let can_fix = task.kind != "onempty" && task.attempt < settings.max_fix_attempts;
            let st_final = if can_fix {
                status::FAILED
            } else {
                status::BLOCKED
            };
            finish(
                st,
                task.id,
                Finish {
                    status: st_final,
                    summary: Some(summary),
                    handoff,
                    error: Some(cut(&f, 8000)),
                    review: review_json,
                    questions: None,
                    usage,
                    failure_class: Some(FailureClass::Task),
                },
            )
            .await?;
            if can_fix {
                create_fix(st, task, &f).await?;
            } else {
                notify(
                    st,
                    task.user_id,
                    Note {
                        kind: "blocked",
                        title: format!("Blockiert: {}", task.title),
                        body: format!(
                            "Nach {} Fix-Versuchen nicht gelöst:\n{}",
                            task.attempt,
                            cut(&f, 400)
                        ),
                        task_id: Some(task.id),
                        push: true,
                    },
                )
                .await;
            }
        }
    }
    Ok(())
}

/// Best-effort git commits of one task run.
struct Committer<'a> {
    st: &'a AppState,
    task: &'a Task,
    project_id: Uuid,
    author_name: &'a str,
    author_email: &'a str,
}

impl Committer<'_> {
    fn message(&self, suffix: &str) -> String {
        format!(
            "{}{suffix}\n\nQueue-Task: {}\nProvider: {}",
            self.task.title, self.task.id, self.task.provider
        )
    }

    async fn commit(&self, suffix: &str) -> anyhow::Result<String> {
        self.st
            .runner
            .commit(
                self.project_id,
                &self.message(suffix),
                self.author_name,
                self.author_email,
            )
            .await
    }

    /// Partial commit of a run that is re-queued; remembered as
    /// `snapshot_after`, so the next run can tell whether it continues
    /// directly on top of it (see [`continues_interrupted`]).
    async fn interrupted_commit(&self, suffix: String) {
        if let Some(sha) = self.partial(suffix).await {
            if let Err(e) = set_snapshot_after(self.st, self.task.id, &sha).await {
                tracing::warn!("task {}: snapshot_after not stored: {e:#}", self.task.id);
            }
        }
    }

    /// A partial commit must not turn an interruption into a crash; the next
    /// run's `begin` commits leftovers as WIP anyway.
    async fn partial(&self, suffix: String) -> Option<String> {
        match self.commit(&suffix).await {
            Ok(sha) => Some(sha),
            Err(err) => {
                tracing::warn!("task {}: partial commit failed: {err:#}", self.task.id);
                None
            }
        }
    }
}

/// Handles every disposition that ends the run without evaluating the
/// result (cancel, limits, re-queue, provider pause). Returns what the
/// normal result flow has to continue with, if anything.
async fn interrupted(
    c: &Committer<'_>,
    disposition: Disposition,
    lim: &EffectiveLimits,
    usage: &Usage,
) -> Result<Option<Continue>, RunError> {
    let (st, task) = (c.st, c.task);
    match disposition {
        Disposition::Cancelled => {
            c.partial(" (abgebrochen)".into()).await;
            finish(
                st,
                task.id,
                Finish {
                    status: status::CANCELLED,
                    summary: None,
                    handoff: None,
                    error: Some("abgebrochen".into()),
                    review: None,
                    questions: None,
                    usage,
                    failure_class: Some(FailureClass::Cancelled),
                },
            )
            .await?;
            Ok(None)
        }
        Disposition::Blocked { class, reason } => {
            event(st, task, "guard", &reason).await;
            if let Some(after) = c
                .partial(format!(" (unterbrochen: {})", class.as_str()))
                .await
            {
                set_snapshot_after(st, task.id, &after).await?;
            }
            if block_running(st, task.id, class, &reason, usage).await {
                notify_blocked(st, task, &reason).await;
            }
            Ok(None)
        }
        Disposition::Requeue { class, reason } => {
            match class {
                FailureClass::Infra => {
                    event(st, task, "error", &format!("Infrastrukturfehler: {reason}")).await
                }
                FailureClass::Stalled => event(st, task, "guard", &reason).await,
                // The provider failure itself was already logged.
                _ => {}
            }
            c.interrupted_commit(format!(" (unterbrochen: {})", class.as_str()))
                .await;
            let note = match class {
                FailureClass::Stalled => Some(note_stalled(lim.stall_secs.unwrap_or(0))),
                FailureClass::ProviderTransient => Some(NOTE_TRANSIENT.to_string()),
                _ => Some(NOTE_INFRA.to_string()),
            };
            transient_requeue(st, task, class, &reason, note.as_deref(), usage).await;
            Ok(None)
        }
        Disposition::RateLimited(reset) => {
            let until = pause_rate_limited(st, task, &task.provider, reset).await;
            c.interrupted_commit(" (unterbrochen: Limit)".into()).await;
            queue::requeue(
                st,
                task.id,
                Requeue {
                    class: FailureClass::RateLimit,
                    transient_retries: task.transient_retries,
                    not_before: None,
                    note: Some(NOTE_RATE_LIMIT),
                    error: None,
                    usage,
                },
            )
            .await?;
            event(
                st,
                task,
                "limit",
                &format!(
                    "Limit erreicht – {} pausiert bis {}",
                    task.provider,
                    until.to_rfc3339()
                ),
            )
            .await;
            notify(
                st,
                task.user_id,
                Note {
                    kind: "limit",
                    title: format!("{}-Limit erreicht", task.provider),
                    body: format!(
                        "Queue für {} pausiert bis {} UTC, danach geht es automatisch weiter.",
                        task.provider,
                        until.format("%d.%m. %H:%M")
                    ),
                    task_id: Some(task.id),
                    push: true,
                },
            )
            .await;
            Ok(None)
        }
        Disposition::AuthPaused(msg) => {
            pause_auth(st, task, &task.provider, &msg).await;
            c.interrupted_commit(" (unterbrochen: Anmeldung)".into())
                .await;
            queue::requeue(
                st,
                task.id,
                Requeue {
                    class: FailureClass::ProviderAuth,
                    transient_retries: task.transient_retries,
                    not_before: None,
                    note: None,
                    error: Some(cut(&msg, 2000)),
                    usage,
                },
            )
            .await?;
            Ok(None)
        }
        Disposition::Contract(r) => Ok(Some(Continue::Contract(r))),
        Disposition::ExitFallback { success } => Ok(Some(Continue::ExitFallback { success })),
        Disposition::TaskFailure(f) => Ok(Some(Continue::TaskFailure(f))),
    }
}

/// Dispositions after which the result flow (commit, verify, review) continues.
enum Continue {
    Contract(AgentResult),
    ExitFallback { success: bool },
    TaskFailure(String),
}

async fn set_snapshot_after(st: &AppState, id: Uuid, sha: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE tasks SET snapshot_after=$2 WHERE id=$1")
        .bind(id)
        .bind(sha)
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Pauses `provider` until the reported reset (or the fallback estimate).
async fn pause_rate_limited(
    st: &AppState,
    task: &Task,
    provider: &str,
    reset: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    let until = match reset {
        Some(t) if t > Utc::now() => t,
        _ => {
            budget::fallback_reset(&st.db, task.user_id, provider, st.cfg.limit_fallback_min).await
        }
    };
    if let Err(e) = budget::set_limited(&st.db, task.user_id, provider, until).await {
        tracing::error!("set_limited failed: {e:#}");
    }
    until
}

/// Pauses `provider` until credentials are updated or the user resumes it.
async fn pause_auth(st: &AppState, task: &Task, provider: &str, msg: &str) {
    let reason = cut(msg, 300);
    if let Err(e) = budget::set_paused(&st.db, task.user_id, provider, &reason).await {
        tracing::error!("set_paused failed: {e:#}");
        return;
    }
    event(
        st,
        task,
        "limit",
        &format!("{provider} pausiert (Anmeldung/Abrechnung): {reason}"),
    )
    .await;
    notify(
        st,
        task.user_id,
        Note {
            kind: "provider_paused",
            title: format!("{provider} pausiert: Anmeldung/Abrechnung"),
            body: format!(
                "{reason}\nZugangsdaten aktualisieren oder die Pause in den Einstellungen aufheben."
            ),
            task_id: Some(task.id),
            push: true,
        },
    )
    .await;
}

/// Providers without live usage report tokens only at the end: the run is
/// not stopped, but an overrun is reported.
async fn warn_post_hoc_overrun(
    st: &AppState,
    task: &Task,
    out: &RunOutcome,
    lim: &EffectiveLimits,
    usage: &Usage,
) {
    if out.guard_limit.is_some() || out.cli_limit.is_some() {
        return;
    }
    if let Some(msg) = post_hoc_overrun(lim, usage) {
        event(st, task, "guard", &msg).await;
        notify(
            st,
            task.user_id,
            Note {
                kind: "limit_exceeded",
                title: format!("Limit überschritten: {}", task.title),
                body: msg,
                task_id: Some(task.id),
                push: false,
            },
        )
        .await;
    }
}

pub(crate) fn post_hoc_overrun(lim: &EffectiveLimits, usage: &Usage) -> Option<String> {
    let (tokens, cost) = lim.remaining(usage);
    if tokens.is_some_and(|t| t < 0) {
        Some(format!(
            "Token-Limit überschritten ({} / {}) – vom Provider erst nach dem Lauf gemeldet, Ergebnis bleibt bestehen",
            budget::counted(usage),
            lim.tokens.unwrap_or_default()
        ))
    } else if cost.is_some_and(|c| c < 0.0) {
        Some(format!(
            "Kosten-Limit überschritten (${:.4} / ${:.4}) – erst nach dem Lauf gemeldet, Ergebnis bleibt bestehen",
            usage.cost_usd,
            lim.cost_usd.unwrap_or_default()
        ))
    } else {
        None
    }
}

async fn check_budget(st: &AppState, user: Uuid, provider: &str) {
    if let Ok(Some((reason, until))) =
        budget::take_budget_notification(&st.db, user, provider).await
    {
        notify(
            st,
            user,
            Note {
                kind: "budget",
                title: format!("{provider}: Token-Budget erschöpft"),
                body: format!(
                    "{reason}. Neue Aufträge für {provider} warten bis ca. {} UTC.",
                    until.format("%d.%m. %H:%M")
                ),
                task_id: None,
                push: true,
            },
        )
        .await;
    }
}

/// Runs a process through the runner; cancellation is forwarded.
async fn exec(
    st: &AppState,
    req: ExecRequest,
    cancel: &CancellationToken,
    tx: mpsc::Sender<ExecFrame>,
) -> anyhow::Result<ExecFrame> {
    let watcher = {
        let (c, client, name) = (cancel.clone(), st.runner.clone(), req.name.clone());
        tokio::spawn(async move {
            c.cancelled().await;
            let _ = client.cancel(&name).await;
        })
    };
    let res = st.runner.exec(&req, tx).await;
    watcher.abort();
    if res.is_err() {
        // The stream broke (network drop, proxy) or the start was refused
        // (409: an orphaned run with this name is still active). The runner
        // only notices a vanished client on its next output line, so a
        // silent agent would keep running and block every retry of the task.
        if let Err(e) = st.runner.cancel(&req.name).await {
            tracing::debug!("cancel of {} after a stream error failed: {e:#}", req.name);
        }
    }
    res
}

fn retry_text(r: &RetryInfo) -> String {
    let mut s = format!("Provider: {}", r.category.label());
    if r.max > 0 {
        s.push_str(&format!(" – Versuch {}/{}", r.attempt, r.max));
    } else if r.attempt > 0 {
        s.push_str(&format!(" – Versuch {}", r.attempt));
    }
    if r.delay_ms > 0 {
        s.push_str(&format!(" in {:.0} s", (r.delay_ms as f64 / 1000.0).ceil()));
    }
    if let Some(code) = r.status {
        s.push_str(&format!(" (HTTP {code})"));
    }
    s
}

struct RunSpec<'a> {
    provider: &'a str,
    model: Option<&'a str>,
    cred: &'a Credential,
    prompt: String,
    kind: RunKind,
    label: &'a str,
    limits: &'a EffectiveLimits,
    /// Task usage before this run (limits are cumulative per task).
    used_before: &'a Usage,
}

async fn agent_run(
    st: &AppState,
    task: &Task,
    spec: RunSpec<'_>,
    cancel: &CancellationToken,
) -> Result<RunOutcome, RunError> {
    let caps = agent::capabilities(spec.provider);
    let (tokens_left, cost_left) = spec.limits.remaining(spec.used_before);
    let run_limits = RunLimits {
        turns: spec.limits.turns.filter(|_| caps.max_turns),
        cost_usd: cost_left.filter(|_| caps.max_cost),
    };
    let inv = agent::invocation(spec.provider, spec.model, spec.cred, &run_limits, spec.kind)
        .map_err(|e| RunError::Config(format!("{e:#}")))?;
    let mut unsupported = vec![];
    if spec.limits.turns.is_some() && !caps.max_turns {
        unsupported.push("Turn-Limit");
    }
    if spec.limits.cost_usd.is_some() && !caps.max_cost {
        unsupported.push("Kostenlimit");
    }
    if !unsupported.is_empty() {
        event(
            st,
            task,
            "guard",
            &format!(
                "{} unterstützt kein {} – es gelten Laufzeit, Stillstand und (nachträglich geprüfte) Tokens.",
                spec.provider,
                unsupported.join("/")
            ),
        )
        .await;
    }
    let req = ExecRequest {
        name: run_name(task.id, spec.label),
        workspace: task.project_id.to_string(),
        program: inv.program.clone(),
        args: inv.args.clone(),
        env: inv.env.clone(),
        home_files: inv.home_files.clone(),
        read_back: inv.writeback.iter().cloned().collect(),
        stdin: Some(spec.prompt),
        timeout_secs: spec.limits.runtime_secs,
        idle_timeout_secs: spec.limits.stall_secs,
        queue_files: {
            let mut q = inv.queue_files.clone();
            if spec.kind == RunKind::Review {
                // Only the reviewer may produce review.json – never a file the
                // agent left behind in .queue/ during its own run.
                q.push(("review.json".into(), String::new()));
            }
            q
        },
    };
    let run_cancel = cancel.child_token();
    let mut guard = caps.live_usage.then(|| Guard::new(tokens_left, cost_left));

    let (tx, mut rx) = mpsc::channel::<ExecFrame>(512);
    let provider = spec.provider;
    let consumer = async {
        let mut ps = ParseState::default();
        let mut o = RunOutcome::default();
        let mut stderr_events = 0;
        while let Some(frame) = rx.recv().await {
            match frame {
                ExecFrame::Out { line } => {
                    for ev in agent::parse_line(provider, &line, &mut ps) {
                        match ev {
                            AgentEvent::Text(t) if !t.trim().is_empty() => {
                                event(st, task, "text", &t).await
                            }
                            AgentEvent::Text(_) => {}
                            AgentEvent::Tool(t) => event(st, task, "tool", &t).await,
                            AgentEvent::UsageDelta(u) => {
                                o.usage_delta.add(&u);
                                if let Some(k) = guard.as_mut().and_then(|g| g.on_delta(&u)) {
                                    o.guard_limit = Some(k);
                                    let g = guard.as_ref().expect("guard");
                                    event(
                                        st,
                                        task,
                                        "guard",
                                        &match k {
                                            LimitKind::Cost => format!(
                                                "Kosten-Limit erreicht (${:.4} / ${:.4}) – Lauf wird beendet",
                                                g.seen.cost_usd,
                                                cost_left.unwrap_or_default()
                                            ),
                                            _ => format!(
                                                "Token-Limit erreicht ({} / {}) – Lauf wird beendet",
                                                budget::counted(&g.seen),
                                                tokens_left.unwrap_or_default()
                                            ),
                                        },
                                    )
                                    .await;
                                    run_cancel.cancel();
                                }
                            }
                            AgentEvent::UsageTotal(u) => o.usage_total = Some(u),
                            AgentEvent::Final(t) => o.final_text = t,
                            AgentEvent::Structured(v) => o.structured = Some(v),
                            AgentEvent::Retry(r) => event(st, task, "retry", &retry_text(&r)).await,
                            AgentEvent::Failure { category, message } => {
                                event(
                                    st,
                                    task,
                                    "error",
                                    &format!("{} [{}]", message, category.label()),
                                )
                                .await;
                                o.set_failure(category, message);
                            }
                            AgentEvent::LimitHit(k) => o.cli_limit = Some(k),
                            AgentEvent::Error(e) => {
                                event(st, task, "error", &e).await;
                                o.error = Some(e);
                            }
                        }
                    }
                }
                ExecFrame::Err { line } => {
                    o.stderr_tail.push_str(&line);
                    o.stderr_tail.push('\n');
                    if o.stderr_tail.len() > 8000 {
                        o.stderr_tail = tail(&o.stderr_tail, 4000);
                    }
                    if stderr_events < 100 && !line.trim().is_empty() {
                        stderr_events += 1;
                        event(st, task, "stderr", &line).await;
                    }
                }
                _ => {}
            }
        }
        o
    };
    let (last, mut o) = tokio::join!(exec(st, req, &run_cancel, tx), consumer);
    match last {
        Ok(ExecFrame::Exit {
            success,
            code,
            timed_out,
            cancelled,
            stalled,
            killed,
            files,
        }) => {
            o.success = success;
            o.exit_code = code;
            o.timed_out = timed_out;
            o.cancelled = cancelled;
            o.stalled = stalled;
            o.killed = killed;
            if killed {
                event(
                    st,
                    task,
                    "status",
                    "Prozess reagierte nicht auf SIGINT und wurde hart beendet (SIGKILL)",
                )
                .await;
            }
            // Persist refreshed OAuth credentials (codex/gemini rotate tokens).
            if let Some((_, new)) = files.into_iter().next() {
                if new.trim() != spec.cred.secret.trim()
                    && serde_json::from_str::<Value>(&new).is_ok()
                {
                    queue::store_credential(st, task.user_id, provider, &spec.cred.kind, &new)
                        .await?;
                }
            }
        }
        Ok(ExecFrame::Fail { error }) => o.infra_error = Some(format!("runner: {error}")),
        Ok(_) => o.infra_error = Some("runner: unexpected final frame".into()),
        Err(e) => o.infra_error = Some(format!("{e:#}")),
    }
    o.stderr_tail = tail(&o.stderr_tail, 3000);
    Ok(o)
}

/// Ok(Ok(())) = passed, Ok(Err(output tail)) = failed.
async fn run_verify(
    st: &AppState,
    task: &Task,
    cmd: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<std::result::Result<(), String>> {
    let req = ExecRequest {
        name: run_name(task.id, "verify"),
        workspace: task.project_id.to_string(),
        program: "sh".into(),
        args: vec!["-c".into(), cmd.into()],
        env: vec![],
        home_files: vec![],
        read_back: vec![],
        stdin: None,
        timeout_secs: st.cfg.verify_timeout_min * 60,
        idle_timeout_secs: None,
        queue_files: vec![],
    };
    let (tx, mut rx) = mpsc::channel::<ExecFrame>(512);
    let collect = async {
        let mut buf = String::new();
        while let Some(f) = rx.recv().await {
            if let ExecFrame::Out { line } | ExecFrame::Err { line } = f {
                buf.push_str(&line);
                buf.push('\n');
                if buf.len() > 16000 {
                    buf = tail(&buf, 8000);
                }
            }
        }
        buf
    };
    let (last, output) = tokio::join!(exec(st, req, cancel, tx), collect);
    let output = tail(&output, 4000);
    let (ok, timed_out, code) = match last? {
        ExecFrame::Exit {
            success,
            timed_out,
            code,
            ..
        } => (success, timed_out, code),
        ExecFrame::Fail { error } => {
            tracing::warn!("verify failed to start: {error}");
            (false, false, Some(-1))
        }
        _ => (false, false, None),
    };
    event(st, task, "verify", &format!("exit={code:?}\n{output}")).await;
    if ok {
        Ok(Ok(()))
    } else if timed_out {
        Ok(Err(format!(
            "Timeout nach {} Minuten\n{output}",
            st.cfg.verify_timeout_min
        )))
    } else {
        Ok(Err(output))
    }
}

enum ReviewOutcome {
    Reviewed(ReviewResult),
    /// Review not possible (reason for the log); the task continues.
    Skipped(String),
    Cancelled,
    Blocked(FailureClass, String),
}

#[allow(clippy::too_many_arguments)]
async fn run_review(
    st: &AppState,
    task: &Task,
    project: &Project,
    before: &str,
    after: &str,
    summary: &str,
    cancel: &CancellationToken,
    usage: &mut Usage,
    lim: &EffectiveLimits,
) -> Result<ReviewOutcome, RunError> {
    let provider = project
        .review_provider
        .clone()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| task.provider.clone());
    let model = if project.review_provider.is_some() {
        project.review_model.clone()
    } else {
        project.review_model.clone().or(task.model.clone())
    };
    let Some(cred) = queue::load_credential(st, task.user_id, &provider).await? else {
        return Ok(ReviewOutcome::Skipped(String::new()));
    };
    if budget::is_blocked(&st.db, task.user_id, &provider).await? {
        return Ok(ReviewOutcome::Skipped(String::new()));
    }
    if let Some(k) = lim.exhausted(usage) {
        return Ok(ReviewOutcome::Blocked(
            limit_class(k),
            format!("{} – kein Review-Lauf", limit_reason(k, lim, false)),
        ));
    }
    event(st, task, "review", &format!("Review via {provider}")).await;
    let diff = st
        .runner
        .diff(project.id, before, after, prompt::MAX_DIFF)
        .await?
        .diff;
    let out = match agent_run(
        st,
        task,
        RunSpec {
            provider: &provider,
            model: model.as_deref(),
            cred: &cred,
            prompt: prompt::review(task, summary, &diff),
            kind: RunKind::Review,
            label: "review",
            limits: lim,
            used_before: usage,
        },
        cancel,
    )
    .await
    {
        Ok(o) => o,
        Err(RunError::Config(msg)) => {
            return Ok(ReviewOutcome::Skipped(format!(
                "Konfigurationsfehler: {msg}"
            )))
        }
        Err(e) => return Err(e),
    };
    let run_usage = out.usage();
    usage.add(&run_usage);
    budget::record_usage(&st.db, task.user_id, &provider, Some(task.id), &run_usage).await?;
    check_budget(st, task.user_id, &provider).await;
    if cancel.is_cancelled() {
        return Ok(ReviewOutcome::Cancelled);
    }
    let caps = agent::capabilities(&provider);
    if let Some(d) = early(&out, false, &provider, lim) {
        return Ok(match d {
            Disposition::Blocked { class, reason } => {
                ReviewOutcome::Blocked(class, format!("{reason} (Review)"))
            }
            Disposition::Requeue { class, .. } => ReviewOutcome::Skipped(class.as_str().into()),
            _ => ReviewOutcome::Skipped(String::new()),
        });
    }
    let file = st.runner.queue_file(project.id, "review.json").await?;
    let review = load_review(out.structured.as_ref(), file.as_deref());
    // Limits/auth problems of the review provider pause it for the next tasks,
    // whether or not a review result exists (same rule as for agent runs).
    if let Some((c, msg)) = &out.failure {
        match c.class() {
            FailureClass::RateLimit => {
                let reset = match c {
                    ErrorCategory::RateLimit(t) => *t,
                    _ => None,
                };
                let until = pause_rate_limited(st, task, &provider, reset).await;
                event(
                    st,
                    task,
                    "limit",
                    &format!(
                        "Limit erreicht – {provider} pausiert bis {}",
                        until.to_rfc3339()
                    ),
                )
                .await;
            }
            FailureClass::ProviderAuth => pause_auth(st, task, &provider, msg).await,
            _ => {}
        }
    }
    if review.is_none() {
        if let Some((c, _)) = &out.failure {
            return Ok(ReviewOutcome::Skipped(c.class().as_str().into()));
        }
        if caps.structured_output {
            return Ok(ReviewOutcome::Skipped("kein Review-Ergebnis".into()));
        }
        return Ok(ReviewOutcome::Skipped(String::new()));
    }
    let r = review.expect("checked");
    event(
        st,
        task,
        "review",
        &if r.approved {
            "Review: OK".to_string()
        } else {
            format!("Review: abgelehnt\n- {}", r.issues.join("\n- "))
        },
    )
    .await;
    Ok(ReviewOutcome::Reviewed(r))
}

async fn create_followups(
    st: &AppState,
    parent: &Task,
    settings: &Settings,
    items: Vec<FollowUp>,
) -> anyhow::Result<usize> {
    let items: Vec<FollowUp> = items
        .into_iter()
        .filter(|f| !f.prompt.trim().is_empty())
        .take(10)
        .collect();
    if items.is_empty() {
        return Ok(0);
    }
    let depth = parent.depth + 1;
    let needs_approval = items.len() as i32 > settings.max_auto_tasks || depth > MAX_AUTO_DEPTH;
    let provider = if parent.kind == "onempty" {
        settings.default_provider.clone()
    } else {
        parent.provider.clone()
    };
    for f in &items {
        let title = if f.title.trim().is_empty() {
            f.prompt
                .lines()
                .next()
                .unwrap_or("Folgeauftrag")
                .to_string()
        } else {
            f.title.clone()
        };
        queue::insert_task(
            st,
            NewTask {
                user_id: parent.user_id,
                project_id: parent.project_id,
                parent_id: Some(parent.id),
                kind: "task",
                title,
                prompt: f.prompt.clone(),
                context: String::new(),
                provider: provider.clone(),
                model: if parent.kind == "onempty" {
                    None
                } else {
                    parent.model.clone()
                },
                status: if needs_approval {
                    status::PENDING_APPROVAL
                } else {
                    status::QUEUED
                },
                attempt: 0,
                depth,
                created_by_agent: true,
                front: false,
                // Follow-ups get the project defaults, not the parent's overrides.
                limits: TaskLimits::default(),
            },
        )
        .await?;
    }
    let n = items.len();
    let list: String = items
        .iter()
        .map(|f| {
            format!(
                "• {}",
                if f.title.is_empty() {
                    &f.prompt
                } else {
                    &f.title
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if needs_approval {
        notify(
            st,
            parent.user_id,
            Note {
                kind: "approval",
                title: format!("{n} neue Aufträge – Freigabe nötig"),
                body: list,
                task_id: Some(parent.id),
                push: true,
            },
        )
        .await;
    } else {
        notify(
            st,
            parent.user_id,
            Note {
                kind: "followups",
                title: format!("{n} Folgeaufträge eingeplant"),
                body: list,
                task_id: Some(parent.id),
                push: false,
            },
        )
        .await;
    }
    Ok(n)
}

async fn create_fix(st: &AppState, failed: &Task, failure: &str) -> anyhow::Result<()> {
    let mut root = failed.clone();
    while root.kind == "fix" {
        match root.parent_id {
            Some(pid) => match queue::load_task(st, pid).await? {
                Some(p) => root = p,
                None => break,
            },
            None => break,
        }
    }
    let fix = queue::insert_task(
        st,
        NewTask {
            user_id: failed.user_id,
            project_id: failed.project_id,
            parent_id: Some(failed.id),
            kind: "fix",
            title: format!("Fix #{}: {}", failed.attempt + 1, root.title),
            prompt: prompt::fix_prompt(&root.title, &root.prompt, failure),
            context: root.context.clone(),
            provider: failed.provider.clone(),
            model: failed.model.clone(),
            status: status::QUEUED,
            attempt: failed.attempt + 1,
            depth: failed.depth,
            created_by_agent: true,
            front: true,
            // A fix inherits the task-level limits of the failed task.
            limits: TaskLimits::of(failed),
        },
    )
    .await?;
    event(
        st,
        failed,
        "status",
        &format!("Fix-Auftrag erstellt: {}", fix.title),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim() -> EffectiveLimits {
        EffectiveLimits {
            runtime_secs: 120,
            stall_secs: Some(30),
            cost_usd: Some(1.0),
            tokens: Some(1000),
            turns: Some(5),
        }
    }

    fn done() -> AgentResult {
        AgentResult {
            status: Some("done".into()),
            summary: Some("s".into()),
            ..Default::default()
        }
    }

    fn classify(
        o: &RunOutcome,
        provider: &str,
        c: Option<AgentResult>,
    ) -> (Disposition, Option<Pause>) {
        classify_run(o, false, provider, agent::capabilities(provider), c, &lim())
    }

    #[test]
    fn precedence_order() {
        // 1. user cancel beats everything.
        let o = RunOutcome {
            guard_limit: Some(LimitKind::Tokens),
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(
            classify_run(
                &o,
                true,
                "claude",
                agent::capabilities("claude"),
                Some(done()),
                &lim()
            )
            .0,
            Disposition::Cancelled
        );
        // 2. guard beats timeout and cancel flag (the guard cancelled the run).
        let o = RunOutcome {
            guard_limit: Some(LimitKind::Tokens),
            cancelled: true,
            timed_out: true,
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "mock", Some(done())).0,
            Disposition::Blocked {
                class: FailureClass::LimitTokens,
                ..
            }
        ));
        // 3. timeout → limit_runtime, stall → re-queue.
        let o = RunOutcome {
            timed_out: true,
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "mock", Some(done())).0,
            Disposition::Blocked {
                class: FailureClass::LimitRuntime,
                ..
            }
        ));
        let o = RunOutcome {
            stalled: true,
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "mock", None).0,
            Disposition::Requeue {
                class: FailureClass::Stalled,
                ..
            }
        ));
        // 4. CLI-reported limits.
        let o = RunOutcome {
            cli_limit: Some(LimitKind::Turns),
            infra_error: Some("x".into()),
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "claude", None).0,
            Disposition::Blocked {
                class: FailureClass::LimitTurns,
                ..
            }
        ));
        let o = RunOutcome {
            cli_limit: Some(LimitKind::Cost),
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "claude", None).0,
            Disposition::Blocked {
                class: FailureClass::LimitCost,
                ..
            }
        ));
        let o = RunOutcome {
            exit_code: Some(53),
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "gemini", None).0,
            Disposition::Blocked {
                class: FailureClass::LimitTurns,
                ..
            }
        ));
        assert!(
            !matches!(classify(&o, "mock", None).0, Disposition::Blocked { .. }),
            "exit 53 means turn limit only for gemini"
        );
        // 5. infra.
        let o = RunOutcome {
            infra_error: Some("runner stream interrupted".into()),
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "mock", Some(done())).0,
            Disposition::Requeue {
                class: FailureClass::Infra,
                ..
            }
        ));
    }

    #[test]
    fn contract_beats_provider_errors_but_still_pauses() {
        let o = RunOutcome {
            failure: Some((ErrorCategory::RateLimit(None), "limit".into())),
            ..Default::default()
        };
        let (d, p) = classify(&o, "claude", Some(done()));
        assert_eq!(d, Disposition::Contract(done()));
        assert_eq!(p, Some(Pause::RateLimit(None)));
        let o = RunOutcome {
            failure: Some((ErrorCategory::Auth, "bad key".into())),
            ..Default::default()
        };
        assert_eq!(
            classify(&o, "claude", Some(done())).1,
            Some(Pause::Auth("bad key".into()))
        );
        let o = RunOutcome {
            failure: Some((ErrorCategory::Overloaded, "busy".into())),
            ..Default::default()
        };
        assert_eq!(
            classify(&o, "claude", Some(done())),
            (Disposition::Contract(done()), None)
        );
    }

    #[test]
    fn provider_failures_without_contract() {
        let f = |c: ErrorCategory| RunOutcome {
            failure: Some((c, "m".into())),
            ..Default::default()
        };
        assert_eq!(
            classify(&f(ErrorCategory::RateLimit(None)), "codex", None).0,
            Disposition::RateLimited(None)
        );
        assert_eq!(
            classify(&f(ErrorCategory::Billing), "codex", None).0,
            Disposition::AuthPaused("m".into())
        );
        assert!(matches!(
            classify(&f(ErrorCategory::Network), "gemini", None).0,
            Disposition::Requeue {
                class: FailureClass::ProviderTransient,
                ..
            }
        ));
        assert!(matches!(
            classify(&f(ErrorCategory::Unknown(None)), "claude", None).0,
            Disposition::Requeue {
                class: FailureClass::ProviderTransient,
                ..
            }
        ));
        assert!(matches!(
            classify(&f(ErrorCategory::ModelNotFound), "claude", None).0,
            Disposition::Blocked {
                class: FailureClass::Config,
                ..
            }
        ));
        assert!(matches!(
            classify(&f(ErrorCategory::MaxOutputTokens), "claude", None).0,
            Disposition::TaskFailure(_)
        ));
        assert!(matches!(
            classify(&f(ErrorCategory::Unknown(Some(400))), "claude", None).0,
            Disposition::TaskFailure(_)
        ));
    }

    #[test]
    fn model_not_found_is_config() {
        let o = RunOutcome {
            failure: Some((
                ErrorCategory::from_claude("model_not_found", Some(404)),
                "no such model".into(),
            )),
            ..Default::default()
        };
        assert!(matches!(
            classify(&o, "claude", None).0,
            Disposition::Blocked {
                class: FailureClass::Config,
                ..
            }
        ));
    }

    #[test]
    fn missing_contract_is_task_failure_for_schema_providers() {
        let o = RunOutcome {
            success: true,
            ..Default::default()
        };
        for p in ["claude", "codex"] {
            assert!(
                matches!(classify(&o, p, None).0, Disposition::TaskFailure(ref m) if m.contains("Ergebnis-Contract fehlt")),
                "{p}"
            );
        }
        for p in ["gemini", "mock"] {
            assert_eq!(
                classify(&o, p, None).0,
                Disposition::ExitFallback { success: true },
                "{p}"
            );
        }
        let failed = RunOutcome {
            success: true,
            error: Some("tool error".into()),
            ..Default::default()
        };
        assert_eq!(
            classify(&failed, "gemini", None).0,
            Disposition::ExitFallback { success: false }
        );
    }

    #[test]
    fn contract_precedence() {
        let good = serde_json::json!({"status":"done","summary":"structured","handoff":"","questions":[],"follow_up_tasks":[]});
        let file = r#"{"status":"failed","summary":"file"}"#;
        let (c, note) = load_contract(Some(&good), Some(file));
        assert_eq!(c.unwrap().summary.as_deref(), Some("structured"));
        assert_eq!(note, "Ergebnis via structured output");
        let bad = serde_json::json!({"status": 42});
        let (c, note) = load_contract(Some(&bad), Some(file));
        assert_eq!(c.unwrap().summary.as_deref(), Some("file"));
        assert!(
            note.contains("structured output ungültig") && note.contains("Fallback"),
            "{note}"
        );
        let (c, note) = load_contract(None, Some("{}"));
        assert!(c.is_none());
        assert!(note.contains("ohne gültigen Status") && note.ends_with("kein Ergebnis-Contract"));
        let (c, note) = load_contract(None, None);
        assert!(c.is_none());
        assert_eq!(note, "kein Ergebnis-Contract");
        assert!(load_contract(
            Some(&serde_json::json!({"status":"maybe"})),
            Some("not json")
        )
        .0
        .is_none());
    }

    #[test]
    fn review_contract_requires_approved() {
        let s = serde_json::json!({"approved": false, "issues": ["bug"]});
        assert!(
            !load_review(Some(&s), Some(r#"{"approved":true}"#))
                .unwrap()
                .approved
        );
        assert!(
            load_review(None, Some(r#"{"approved":true,"issues":[]}"#))
                .unwrap()
                .approved
        );
        assert!(load_review(Some(&serde_json::json!({"x":1})), Some("{}")).is_none());
        assert!(load_review(None, None).is_none());
    }

    #[test]
    fn post_hoc_token_overrun_warns_only() {
        // codex/gemini: no live usage → the run completes; the classifier
        // still decides by contract, the overrun is only reported.
        let o = RunOutcome {
            success: true,
            usage_total: Some(Usage {
                input_tokens: 5000,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            classify(&o, "codex", Some(done())).0,
            Disposition::Contract(done())
        );
        let msg = post_hoc_overrun(&lim(), &o.usage()).unwrap();
        assert!(
            msg.contains("5000 / 1000") && msg.contains("Ergebnis bleibt"),
            "{msg}"
        );
        assert!(post_hoc_overrun(&lim(), &Usage::default()).is_none());
        let over_cost = Usage {
            cost_usd: 2.0,
            ..Default::default()
        };
        assert!(post_hoc_overrun(&lim(), &over_cost)
            .unwrap()
            .contains("Kosten"));
    }

    #[test]
    fn failure_merge_keeps_reset_and_first_error() {
        let mut o = RunOutcome::default();
        o.set_failure(ErrorCategory::RateLimit(Some(Utc::now())), "a".into());
        o.set_failure(ErrorCategory::RateLimit(None), "b".into());
        assert!(matches!(
            o.failure,
            Some((ErrorCategory::RateLimit(Some(_)), _))
        ));
        let mut o = RunOutcome::default();
        o.set_failure(ErrorCategory::RateLimit(None), "a".into());
        o.set_failure(ErrorCategory::RateLimit(Some(Utc::now())), "b".into());
        assert!(matches!(
            o.failure,
            Some((ErrorCategory::RateLimit(Some(_)), _))
        ));
        let mut o = RunOutcome::default();
        o.set_failure(ErrorCategory::Unknown(None), "a".into());
        o.set_failure(ErrorCategory::Auth, "b".into());
        assert_eq!(o.failure.as_ref().unwrap().0, ErrorCategory::Auth);
        o.set_failure(ErrorCategory::Overloaded, "c".into());
        assert_eq!(o.failure.as_ref().unwrap().0, ErrorCategory::Auth);
    }

    #[test]
    fn usage_total_replaces_deltas() {
        let mut o = RunOutcome {
            usage_delta: Usage {
                input_tokens: 10,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(o.usage().input_tokens, 10);
        o.usage_total = Some(Usage {
            input_tokens: 99,
            ..Default::default()
        });
        assert_eq!(o.usage().input_tokens, 99);
    }

    #[test]
    fn retry_text_format() {
        assert_eq!(
            retry_text(&RetryInfo {
                category: ErrorCategory::Overloaded,
                attempt: 2,
                max: 10,
                delay_ms: 3500,
                status: Some(529)
            }),
            "Provider: overloaded – Versuch 2/10 in 4 s (HTTP 529)"
        );
        assert_eq!(
            retry_text(&RetryInfo {
                category: ErrorCategory::Network,
                attempt: 0,
                max: 0,
                delay_ms: 0,
                status: None
            }),
            "Provider: network"
        );
    }
}
