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
        self, AgentEvent, Capabilities, ConnCfg, Credential, ErrorCategory, LimitKind, ParseState,
        RetryInfo, RunKind, RunLimits,
    },
    batch, budget, connectors, crypto,
    environment::{self, EgressLog, Env},
    gateway,
    limits::{self, EffectiveLimits, Guard},
    llm,
    models::{
        status, AgentResult, FailureClass, FollowUp, Project, ReviewResult, Settings, Task, Usage,
    },
    notify::{notify, Note},
    pricing::{self, Meter, PriceCtx, Tier},
    prompt::{self, Parent},
    queue::{self, event, finish, Finish, NewTask, Requeue, TaskLimits},
    state::AppState,
};
use queued_api::{add_money, StepCost, StepMode, Target, TaskCost};
use queued_common::{gate::GateFinding, secrets};

#[path = "runner_gate.rs"]
mod gate;

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
            if let Err(e) = finish_cancelled(&st, id, None, None, &usage).await {
                tracing::error!("task {id}: could not mark cancelled: {e:#}");
            }
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
                gate=NULL, gate_pending=NULL, finished_at=now(), updated_at=now()
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
        FailureClass::Gate => "Freigabe abgelehnt",
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
    /// Token formats seen in the agent's output (masked, WP03).
    pub log_findings: Vec<GateFinding>,
    /// Runner unreachable / stream interrupted / process could not start.
    pub infra_error: Option<String>,
    pub stderr_tail: String,
    /// Money cost of the run (catalog prices; CLI dollars only as fallback).
    pub step_cost: Option<StepCost>,
}

impl RunOutcome {
    pub fn usage(&self) -> Usage {
        self.usage_total
            .clone()
            .unwrap_or_else(|| self.usage_delta.clone())
    }

    /// Usage with the catalog cost (what budgets and limits count).
    pub fn priced_usage(&self) -> Usage {
        self.step_cost
            .as_ref()
            .map(|c| c.usage.clone())
            .unwrap_or_else(|| self.usage())
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

fn usage_text(k: LimitKind, used: &Usage) -> String {
    match k {
        LimitKind::Cost => format!("verbraucht ${:.4}", used.cost_usd),
        _ => format!("verbraucht {} Tokens", budget::counted(used)),
    }
}

/// Decisions 1–5 of [`classify_run`] that do not need the result contract.
///
/// `used` is the task's usage including this run. A cumulative limit that is
/// exceeded only according to the final usage report (codex/gemini report
/// usage at the end, Claude's live deltas can under-count) blocks exactly
/// like the live guard: a task over its limit never ends `done` and triggers
/// no further paid step (review, fix task, follow-ups). Its changes are kept
/// as a partial commit, visible in the task's diff.
fn early(
    o: &RunOutcome,
    user_cancel: bool,
    provider: &str,
    l: &EffectiveLimits,
    used: &Usage,
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
    if let Some(k) = l.exhausted(used) {
        return Some(Disposition::Blocked {
            class: limit_class(k),
            reason: format!(
                "{} – erst nach dem Lauf gemeldet ({}); die Änderungen sind committet (Diff im Auftrag)",
                limit_reason(k, l, false),
                usage_text(k, used)
            ),
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
    used: &Usage,
) -> (Disposition, Option<Pause>) {
    if let Some(d) = early(o, user_cancel, provider, l, used) {
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

    let routing = project.routing();
    let remote = project_remote(st, &project);
    let env = environment::load(&st.db, project.id).await?;
    let commits = Committer {
        st,
        task,
        project_id: project.id,
        author_name: &author_name,
        author_email: &author_email,
        allow: &project.secret_allowlist,
    };
    // Approved gate (WP03): continue where the task was held.
    if let Some(g) = task.gate_parsed().filter(|g| g.resume.is_some()) {
        let flow = gate::Flow {
            st,
            task,
            project: &project,
            settings: &settings,
            env: &env,
            lim: &lim,
            routing: routing.as_ref(),
            remote: remote.clone(),
            cancel,
            commits: &commits,
        };
        return gate::resume(&flow, g, usage).await;
    }
    // Flex applies to the agent loop of the routing's own access.
    let exec_flex = routing.as_ref().is_some_and(|r| {
        r.execute.flex
            && task.connector_id.is_some()
            && r.execute.target.as_ref().and_then(|t| t.connector_id) == task.connector_id
    });
    let acc = match access_for(
        st,
        task.user_id,
        &project,
        task.connector_id,
        &task.provider,
        exec_flex,
    )
    .await?
    {
        AccessResult::Ok(a) => a,
        AccessResult::Missing(msg) | AccessResult::Denied(msg) => {
            if block_running(st, task.id, FailureClass::Config, &msg, usage).await {
                notify_blocked(st, task, &msg).await;
            }
            return Ok(());
        }
    };
    if let Some(k) = lim.exhausted(usage) {
        let msg = limit_reason(k, &lim, false);
        event(st, task, "guard", &msg).await;
        if block_running(st, task.id, limit_class(k), &msg, usage).await {
            notify_blocked(st, task, &msg).await;
        }
        return Ok(());
    }

    // Single-shot steps before the agent loop (plan) or instead of it (analysis).
    if let Some(r) = &routing {
        let step = if task.kind == "task" && task.plan.is_none() {
            Some(("plan", &r.plan))
        } else if task.kind == "onempty" {
            Some(("analyze", &r.analyze))
        } else {
            None
        };
        if let Some((name, route)) =
            step.filter(|(_, s)| matches!(s.mode, StepMode::Sync | StepMode::Batch))
        {
            if defer_single(st, task, &project, &settings, r, name, route, usage).await? {
                return Ok(());
            }
        }
    }

    let before = prepare_workspace(st, task, &project, remote.clone()).await?;
    if env.has_setup() {
        // WP02-N1: the setup runs repository code with the secrets – only on
        // setup files a human approved.
        if project.setup_guard
            && environment::has_secrets(st, project.id).await?
            && gate::check_setup(st, task, &project, &env, usage).await?
        {
            return Ok(());
        }
        match run_setup(st, task, &project, &env, cancel).await? {
            SetupOutcome::Ready => {}
            SetupOutcome::Cancelled => {
                finish_cancelled(st, task.id, None, None, usage).await?;
                return Ok(());
            }
            SetupOutcome::Failed(msg) => {
                event(st, task, "guard", &msg).await;
                if block_running(st, task.id, FailureClass::Config, &msg, usage).await {
                    notify_blocked(st, task, &msg).await;
                }
                return Ok(());
            }
        }
    }
    if gate::check_agent_config(st, task, &project, &acc.provider, usage).await? {
        return Ok(());
    }
    let prompt = build_prompt(st, task, &project, &settings).await?;
    let meter = meter_for(
        st,
        &acc,
        task.model.as_deref(),
        if task.kind == "onempty" {
            "analyze"
        } else {
            "execute"
        },
    )
    .await;
    let out = agent_run(
        st,
        task,
        RunSpec {
            provider: &acc.provider,
            model: task.model.as_deref(),
            cred: &acc.cred,
            prompt,
            kind: RunKind::Agent,
            label: "agent",
            limits: &lim,
            used_before: usage,
            meter,
            env: &env,
            trust_repo_config: project.trust_repo_config,
        },
        cancel,
    )
    .await?;
    record_run_usage(st, task, &acc, &out, usage).await?;

    let user_cancel = cancel.is_cancelled();
    let contract = if early(&out, user_cancel, &acc.provider, &lim, usage).is_none() {
        let file = st.runner.queue_file(project.id, "result.json").await?;
        let (c, note) = load_contract(out.structured.as_ref(), file.as_deref());
        event(st, task, "contract", &note).await;
        c
    } else {
        None
    };
    let caps = agent::capabilities_for(&acc.provider, acc.cred.conn.as_ref());
    let (disposition, pause) = classify_run(
        &out,
        user_cancel,
        &acc.provider,
        caps,
        contract,
        &lim,
        usage,
    );
    match pause {
        Some(Pause::RateLimit(reset)) => {
            pause_rate_limited(st, task, &acc.key, reset).await;
        }
        Some(Pause::Auth(msg)) => pause_auth(st, task, &acc.key, &acc.label, &msg).await,
        None => {}
    }
    let Some(next) = interrupted(&commits, &acc, disposition, &lim, usage).await? else {
        return Ok(());
    };
    let (result, failure) = next.into_result(&out);
    let summary = result
        .summary
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| cut(&out.final_text, 1500));
    let handoff = result.handoff.clone().map(|h| cut(&h, prompt::MAX_HANDOFF));
    let pending = gate::Pending {
        summary,
        handoff,
        follow_ups: result.follow_up_tasks.clone(),
        failure,
        questions: if result.status.as_deref() == Some("needs_input") {
            result.questions.clone()
        } else {
            vec![]
        },
        before: before.clone(),
        after: None,
        approved: vec![],
        review: None,
    };
    let flow = gate::Flow {
        st,
        task,
        project: &project,
        settings: &settings,
        env: &env,
        lim: &lim,
        routing: routing.as_ref(),
        remote,
        cancel,
        commits: &commits,
    };
    // WP03: secrets, protected paths and weakened tests are checked before
    // anything is committed, verified, reviewed or pushed.
    if gate::check_changes(&flow, &pending, &out.log_findings, usage).await? {
        return Ok(());
    }
    let Some(after) = gate::commit(&flow, &pending, usage).await? else {
        return Ok(());
    };
    let pending = gate::Pending {
        after: Some(after),
        ..pending
    };
    gate::after_commit(&flow, pending, Some(&acc), gate::Step::Verify, usage).await
}

/// `init` + `begin` (pull, WIP commit of leftovers) and the task's diff base.
async fn prepare_workspace(
    st: &AppState,
    task: &Task,
    project: &Project,
    remote: Option<GitRemote>,
) -> Result<String, RunError> {
    st.runner
        .init(
            project.id,
            &queued_common::InitWorkspace {
                remote: remote.clone(),
                branch: Some(project.git_branch.clone()),
            },
        )
        .await?;
    let begin = st
        .runner
        .begin(project.id, remote, &project.secret_allowlist)
        .await?;
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
    Ok(before)
}

async fn build_prompt(
    st: &AppState,
    task: &Task,
    project: &Project,
    settings: &Settings,
) -> sqlx::Result<String> {
    if task.kind == "onempty" {
        let history: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT title, summary FROM tasks WHERE project_id=$1 AND status='done' AND kind<>'onempty' ORDER BY finished_at DESC LIMIT 8",
        )
        .bind(project.id)
        .fetch_all(&st.db)
        .await?;
        return Ok(prompt::onempty(
            project,
            &settings.onempty_actions,
            &history,
        ));
    }
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
    Ok(prompt::task(task, project, parent.as_ref()))
}

/// Adds a run's usage to the task total, the usage log, the task cost and the budget check.
async fn record_run_usage(
    st: &AppState,
    task: &Task,
    acc: &RunAccess,
    out: &RunOutcome,
    usage: &mut Usage,
) -> sqlx::Result<()> {
    let run_usage = out.priced_usage();
    usage.add(&run_usage);
    if let Some(sc) = &out.step_cost {
        record_cost(st, task, &acc.provider, acc.connector_id, sc).await?;
    }
    check_budget(st, task.user_id, &acc.provider).await;
    Ok(())
}

/// Logs one priced step and merges it into `tasks.cost`.
pub(crate) async fn record_cost(
    st: &AppState,
    task: &Task,
    provider: &str,
    connector: Option<Uuid>,
    sc: &StepCost,
) -> sqlx::Result<()> {
    budget::record_usage(
        &st.db,
        budget::UsageRow {
            user: task.user_id,
            provider,
            task: Some(task.id),
            project: Some(task.project_id),
            connector,
            cost: sc,
        },
    )
    .await?;
    let mut tx = st.db.begin().await?;
    let cur: Option<Value> = sqlx::query_scalar("SELECT cost FROM tasks WHERE id=$1 FOR UPDATE")
        .bind(task.id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
    let mut tc: TaskCost = cur
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    merge_cost(&mut tc, sc, &pricing::builtin().version);
    sqlx::query("UPDATE tasks SET cost=$2 WHERE id=$1")
        .bind(task.id)
        .bind(serde_json::to_value(&tc).unwrap_or_default())
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// Adds a step to the task cost (same step + model + access = one line).
pub(crate) fn merge_cost(tc: &mut TaskCost, sc: &StepCost, version: &str) {
    match tc.steps.iter_mut().find(|x| {
        x.step == sc.step
            && x.model == sc.model
            && x.connector == sc.connector
            && x.currency == sc.currency
    }) {
        Some(x) => {
            x.usage.add(&sc.usage);
            x.amount += sc.amount;
            x.runs += sc.runs;
            if x.source != sc.source {
                // Mixed sources: the weaker one is shown.
                x.source = if x.source == queued_api::CostSource::Unknown
                    || sc.source == queued_api::CostSource::Unknown
                {
                    queued_api::CostSource::Unknown
                } else {
                    queued_api::CostSource::Cli
                };
            }
            for n in &sc.notes {
                if !x.notes.contains(n) {
                    x.notes.push(n.clone());
                }
            }
        }
        None => tc.steps.push(sc.clone()),
    }
    tc.totals.clear();
    for x in &tc.steps {
        add_money(&mut tc.totals, &x.currency, x.amount);
    }
    tc.incomplete = tc.steps.iter().any(|x| {
        x.source == queued_api::CostSource::Unknown
            && x.usage.input_tokens + x.usage.output_tokens > 0
    });
    tc.price_version = version.to_string();
}

/// The access (credential) of one run.
pub(crate) struct RunAccess {
    /// CLI of the run; usage and budgets are logged under it.
    pub provider: String,
    pub cred: Credential,
    /// `provider_limits` key for pauses (provider, or `connector:<id>`).
    pub key: String,
    /// Readable name for messages.
    pub label: String,
    pub connector_id: Option<Uuid>,
    /// Regional surcharge rule of the price catalog.
    pub region: Option<&'static str>,
}

pub(crate) enum AccessResult {
    Ok(Box<RunAccess>),
    /// Legacy provider without stored credentials.
    Missing(String),
    /// Connector gone/not accessible, or the data-residency rule forbids it.
    Denied(String),
}

/// Resolves and checks (access rights, data residency) the access of a run.
pub(crate) async fn access_for(
    st: &AppState,
    user: Uuid,
    project: &Project,
    connector_id: Option<Uuid>,
    provider: &str,
    flex: bool,
) -> anyhow::Result<AccessResult> {
    let residency = project.residency();
    let Some(id) = connector_id else {
        let loc = connectors::legacy_location(provider);
        if !residency.allows(loc) {
            return Ok(AccessResult::Denied(connectors::residency_error(
                residency,
                loc,
                &format!("Der Standard-Zugang „{provider}“"),
            )));
        }
        return Ok(match queue::load_credential(st, user, provider).await? {
            None => AccessResult::Missing(format!(
                "Keine Zugangsdaten für '{provider}' hinterlegt (Einstellungen → Provider)."
            )),
            Some(cred) => AccessResult::Ok(Box::new(RunAccess {
                provider: provider.into(),
                cred,
                key: provider.into(),
                label: provider.into(),
                connector_id: None,
                region: None,
            })),
        });
    };
    let Some(c) = connectors::load_for(st, id, user, Some(project.id)).await? else {
        return Ok(AccessResult::Denied(
            "Der gewählte Zugang ist nicht mehr verfügbar (gelöscht oder kein Zugriff mehr)."
                .into(),
        ));
    };
    if !residency.allows(c.location) {
        return Ok(AccessResult::Denied(connectors::residency_error(
            residency,
            c.location,
            &format!("Der Zugang „{}“", c.label),
        )));
    }
    let flex = flex && connectors::type_info(&c.kind, true).is_some_and(|t| t.flex);
    Ok(AccessResult::Ok(Box::new(RunAccess {
        provider: c.harness.clone(),
        key: c.limit_key(),
        label: c.label.clone(),
        connector_id: Some(c.id),
        region: c.region_rule(),
        cred: Credential {
            kind: format!("connector:{}", c.kind),
            secret: c.secret.clone().unwrap_or_default(),
            conn: Some(ConnCfg {
                id: Some(c.id),
                kind: c.kind.clone(),
                label: c.label.clone(),
                config: c.config.clone(),
                flex,
            }),
        },
    })))
}

/// Paused (rate limit/auth) or out of budget.
async fn access_blocked(st: &AppState, user: Uuid, a: &RunAccess) -> sqlx::Result<bool> {
    let s = budget::status(&st.db, user, &a.provider).await?;
    if a.connector_id.is_none() {
        return Ok(s.blocked);
    }
    Ok(s.budget_until.is_some() || budget::status(&st.db, user, &a.key).await?.blocked)
}

/// Meter for a run of `a` with `model`.
async fn meter_for(st: &AppState, a: &RunAccess, model: Option<&str>, step: &'static str) -> Meter {
    let cat = pricing::current(st).await;
    let flex = a.cred.conn.as_ref().is_some_and(|c| c.flex);
    let region_mult = a
        .region
        .and_then(|r| pricing::region(&cat, r))
        .map_or(1.0, |r| r.multiplier);
    Meter::new(
        &cat,
        model,
        PriceCtx {
            tier: if flex { Tier::Flex } else { Tier::Standard },
            region_mult,
            long_ttl: false,
        },
        step,
        a.connector_id.map(|_| a.label.clone()),
    )
}

/// What the end of the result flow records on the task.
struct End<'a> {
    summary: String,
    handoff: Option<String>,
    review: Option<Value>,
    usage: &'a Usage,
}

async fn finish_cancelled(
    st: &AppState,
    id: Uuid,
    summary: Option<String>,
    handoff: Option<String>,
    usage: &Usage,
) -> sqlx::Result<()> {
    finish(
        st,
        id,
        Finish {
            status: status::CANCELLED,
            summary,
            handoff,
            error: Some("abgebrochen".into()),
            review: None,
            questions: None,
            usage,
            failure_class: Some(FailureClass::Cancelled),
        },
    )
    .await
}

async fn finish_done(
    st: &AppState,
    task: &Task,
    settings: &Settings,
    end: End<'_>,
    follow_ups: Vec<FollowUp>,
) -> Result<(), RunError> {
    finish(
        st,
        task.id,
        Finish {
            status: status::DONE,
            summary: Some(end.summary.clone()),
            handoff: end.handoff,
            error: None,
            review: end.review,
            questions: None,
            usage: end.usage,
            failure_class: None,
        },
    )
    .await?;
    event(st, task, "status", "Erledigt").await;
    let created = create_followups(st, task, settings, follow_ups).await?;
    if created == 0 && task.kind != "onempty" {
        notify(
            st,
            task.user_id,
            Note {
                kind: "done",
                title: format!("Erledigt: {}", task.title),
                body: cut(&end.summary, 300),
                task_id: Some(task.id),
                push: false,
            },
        )
        .await;
    }
    Ok(())
}

/// Real task failure: fix task while attempts are left, else blocked.
async fn finish_failed(
    st: &AppState,
    task: &Task,
    settings: &Settings,
    end: End<'_>,
    f: &str,
) -> Result<(), RunError> {
    event(st, task, "error", f).await;
    // Eval runs measure first-pass success: no fix tasks.
    let is_eval: bool = sqlx::query_scalar("SELECT is_eval FROM projects WHERE id=$1")
        .bind(task.project_id)
        .fetch_optional(&st.db)
        .await?
        .unwrap_or(false);
    let can_fix = task.kind != "onempty" && !is_eval && task.attempt < settings.max_fix_attempts;
    finish(
        st,
        task.id,
        Finish {
            status: if can_fix {
                status::FAILED
            } else {
                status::BLOCKED
            },
            summary: Some(end.summary),
            handoff: end.handoff,
            error: Some(cut(f, 8000)),
            review: end.review,
            questions: None,
            usage: end.usage,
            failure_class: Some(FailureClass::Task),
        },
    )
    .await?;
    if can_fix {
        create_fix(st, task, f).await?;
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
                    cut(f, 400)
                ),
                task_id: Some(task.id),
                push: true,
            },
        )
        .await;
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
    /// Approved secret fingerprints of the project.
    allow: &'a [String],
}

impl Committer<'_> {
    fn message(&self, suffix: &str) -> String {
        format!(
            "{}{suffix}\n\nQueue-Task: {}\nProvider: {}",
            self.task.title, self.task.id, self.task.provider
        )
    }

    /// Commit through the runner's secret scanner (WP03).
    async fn commit(&self, suffix: &str) -> anyhow::Result<queued_common::CommitOutcome> {
        self.st
            .runner
            .commit(
                self.project_id,
                &self.message(suffix),
                self.author_name,
                self.author_email,
                self.allow,
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
            Ok(o) if o.blocked.is_empty() => Some(o.sha),
            Ok(o) => {
                // The leftovers stay uncommitted; the next `begin` quarantines them.
                event(
                    self.st,
                    self.task,
                    "gate",
                    &format!(
                        "Teil-Commit nicht angelegt – mögliches Geheimnis: {}",
                        o.blocked
                            .iter()
                            .map(|f| format!("{} ({})", f.detail, f.path))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                )
                .await;
                None
            }
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
    acc: &RunAccess,
    disposition: Disposition,
    lim: &EffectiveLimits,
    usage: &Usage,
) -> Result<Option<Continue>, RunError> {
    let (st, task) = (c.st, c.task);
    match disposition {
        Disposition::Cancelled => {
            c.partial(" (abgebrochen)".into()).await;
            finish_cancelled(st, task.id, None, None, usage).await?;
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
            let until = pause_rate_limited(st, task, &acc.key, reset).await;
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
                    acc.label,
                    until.to_rfc3339()
                ),
            )
            .await;
            notify(
                st,
                task.user_id,
                Note {
                    kind: "limit",
                    title: format!("{}-Limit erreicht", acc.label),
                    body: format!(
                        "Queue für {} pausiert bis {} UTC, danach geht es automatisch weiter.",
                        acc.label,
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
            pause_auth(st, task, &acc.key, &acc.label, &msg).await;
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

impl Continue {
    /// The run's result and, for a real task failure, its description
    /// (with the agent error and the stderr tail for the fix task).
    fn into_result(self, out: &RunOutcome) -> (AgentResult, Option<String>) {
        let details = |mut f: String, with_error: bool| {
            if let Some(e) = out.error.as_ref().filter(|_| with_error) {
                f.push_str(&format!("\n\nAgent error: {e}"));
            }
            if !out.stderr_tail.trim().is_empty() {
                f.push_str(&format!("\n\nstderr (tail):\n{}", out.stderr_tail));
            }
            f
        };
        match self {
            Continue::Contract(r) => {
                let failure = (r.status.as_deref() == Some("failed")).then(|| {
                    let base = r
                        .summary
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| cut(&out.final_text, 1500));
                    details(base, true)
                });
                (r, failure)
            }
            Continue::ExitFallback { success } => (
                AgentResult::default(),
                (!success).then(|| details(cut(&out.final_text, 1500), true)),
            ),
            // The classifier already included the agent error.
            Continue::TaskFailure(f) => (AgentResult::default(), Some(details(f, false))),
        }
    }
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
    let now = Utc::now();
    let until = match reset {
        // A reset from the stream is capped: a bogus far-future value must
        // not pause the provider for years.
        Some(t) if t > now => agent::classify::cap_reset(t, now),
        _ => {
            budget::fallback_reset(&st.db, task.user_id, provider, st.cfg.limit_fallback_min).await
        }
    };
    if let Err(e) = budget::set_limited(&st.db, task.user_id, provider, until).await {
        tracing::error!("set_limited failed: {e:#}");
    }
    until
}

/// Pauses the access `key` (provider or connector) until credentials are
/// updated or the user resumes it. `provider` is the readable name.
async fn pause_auth(st: &AppState, task: &Task, key: &str, provider: &str, msg: &str) {
    let reason = cut(msg, 300);
    if let Err(e) = budget::set_paused(&st.db, task.user_id, key, &reason).await {
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
    /// Prices the usage while it streams in.
    meter: Meter,
    /// Project environment: variables and the agent network policy.
    env: &'a Env,
    /// The owner allows the CLI to load configuration from the repository.
    trust_repo_config: bool,
}

async fn agent_run(
    st: &AppState,
    task: &Task,
    spec: RunSpec<'_>,
    cancel: &CancellationToken,
) -> Result<RunOutcome, RunError> {
    let caps = agent::capabilities_for(spec.provider, spec.cred.conn.as_ref());
    let (tokens_left, cost_left) = spec.limits.remaining(spec.used_before);
    let run_limits = RunLimits {
        turns: spec.limits.turns.filter(|_| caps.max_turns),
        cost_usd: cost_left.filter(|_| caps.max_cost),
    };
    // LiteLLM: a virtual key per run, capped at the task's remaining cost limit.
    let gw = gateway::per_task(spec.cred);
    let run_cred;
    let cred: &Credential = match &gw {
        Some((base, master)) => {
            let key = gateway::issue(base, master, task.id, cost_left, &st.cfg.llm_guard())
                .await
                .map_err(RunError::Infra)?;
            event(
                st,
                task,
                "status",
                "Gateway: eigener Budget-Schlüssel für diesen Lauf",
            )
            .await;
            run_cred = gateway::with_key(spec.cred, key);
            &run_cred
        }
        None => spec.cred,
    };
    let mut inv = agent::invocation(spec.provider, spec.model, cred, &run_limits, spec.kind)
        .map_err(|e| RunError::Config(format!("{e:#}")))?;
    if !spec.trust_repo_config {
        agent::restrict_repo_config(&mut inv, spec.provider);
    }
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
    let egress = spec
        .env
        .agent_policy(agent::egress_hosts(spec.provider, cred));
    let phase = if spec.kind == RunKind::Review {
        "Review"
    } else {
        "Agent"
    };
    let enforced = environment::sandbox(st).await.egress_enforced;
    event(
        st,
        task,
        "egress",
        &format!(
            "Netz ({phase}): {}{}",
            environment::describe(&egress),
            if enforced {
                ""
            } else {
                " – nicht durchgesetzt"
            }
        ),
    )
    .await;
    // Project variables first: the invocation's own variables win.
    let mut run_env = spec.env.vars();
    run_env.extend(inv.env.iter().cloned());
    let req = ExecRequest {
        name: run_name(task.id, spec.label),
        workspace: task.project_id.to_string(),
        program: inv.program.clone(),
        args: inv.args.clone(),
        env: run_env,
        egress,
        cache: None,
        secret_env: vec![],
        home_env: inv.home_env.clone(),
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
    let mut meter = spec.meter;
    let consumer = async {
        let mut ps = ParseState::default();
        let mut o = RunOutcome::default();
        let mut stderr_events = 0;
        let mut elog = EgressLog::new(phase);
        let scan_log = spec.kind == RunKind::Agent;
        let key = st.cfg.master_key;
        let seen_log = |o: &mut RunOutcome, text: &str| {
            if !scan_log {
                return;
            }
            for h in secrets::scan_high_confidence(text) {
                let f = GateFinding {
                    kind: queued_common::FindingKind::SecretInLog,
                    path: String::new(),
                    line: None,
                    detail: h.masked(text),
                    fingerprint: Some(secrets::fingerprint(&key, h.rule, h.value(text))),
                };
                if o.log_findings.len() < 20 && !o.log_findings.contains(&f) {
                    o.log_findings.push(f);
                }
            }
        };
        while let Some(frame) = rx.recv().await {
            match frame {
                f @ ExecFrame::Egress { .. } => elog.on(st, task, &f).await,
                ExecFrame::Out { line } => {
                    seen_log(&mut o, &line);
                    for ev in agent::parse_line(provider, &line, &mut ps) {
                        match ev {
                            AgentEvent::Text(t) if !t.trim().is_empty() => {
                                event(st, task, "text", &t).await
                            }
                            AgentEvent::Text(_) => {}
                            AgentEvent::Tool(t) => event(st, task, "tool", &t).await,
                            AgentEvent::UsageDelta(mut u) => {
                                // Catalog price instead of the CLI's dollars (USD prices
                                // only; the cost limit is in USD).
                                let c = meter.on_delta(&u);
                                if meter.usd() {
                                    u.cost_usd = c;
                                }
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
                    seen_log(&mut o, &line);
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
        elog.finish(st, task, enforced).await;
        (o, meter)
    };
    let (last, (mut o, meter)) = tokio::join!(exec(st, req, &run_cancel, tx), consumer);
    if let Some((base, master)) = &gw {
        gateway::revoke(base, master, &cred.secret, &st.cfg.llm_guard()).await;
    }
    match last {
        Ok(ExecFrame::Exit {
            success,
            code,
            timed_out,
            cancelled,
            stalled,
            killed,
            files,
            ..
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
                    match spec.cred.conn.as_ref().and_then(|c| c.id) {
                        // Connector (e.g. ChatGPT subscription): back into its secret.
                        Some(id) => {
                            let enc = crypto::encrypt(&st.cfg.master_key, new.trim().as_bytes())?;
                            sqlx::query(
                                "UPDATE connectors SET secret_enc=$2, updated_at=now() WHERE id=$1",
                            )
                            .bind(id)
                            .bind(enc)
                            .execute(&st.db)
                            .await?;
                        }
                        None => {
                            queue::store_credential(
                                st,
                                task.user_id,
                                provider,
                                &spec.cred.kind,
                                &new,
                            )
                            .await?
                        }
                    }
                }
            }
        }
        Ok(ExecFrame::Fail { error }) => o.infra_error = Some(format!("runner: {error}")),
        Ok(_) => o.infra_error = Some("runner: unexpected final frame".into()),
        Err(e) => o.infra_error = Some(format!("{e:#}")),
    }
    o.stderr_tail = tail(&o.stderr_tail, 3000);
    o.step_cost = Some(meter.finish(&o.usage()));
    Ok(o)
}

enum SetupOutcome {
    Ready,
    Cancelled,
    /// Message for the user; the task is blocked (class `config`).
    Failed(String),
}

/// Most setup output lines written as events (the tail is in the error).
const MAX_SETUP_EVENTS: usize = 200;

/// Setup phase (WP02): the project's setup script in its own container,
/// with the secrets and the setup network policy, before the agent starts.
/// The agent phase never sees the secrets.
async fn run_setup(
    st: &AppState,
    task: &Task,
    project: &Project,
    env: &Env,
    cancel: &CancellationToken,
) -> Result<SetupOutcome, RunError> {
    let secrets = environment::secret_values(st, project.id)
        .await
        .map_err(RunError::Infra)?;
    let egress = env.setup_policy();
    let enforced = environment::sandbox(st).await.egress_enforced;
    let cache = env.cache_spec();
    event(
        st,
        task,
        "setup",
        &format!(
            "Setup startet – Netz: {}{}; Geheimnisse: {}; Cache: {}",
            environment::describe(&egress),
            if enforced {
                ""
            } else {
                " (nicht durchgesetzt)"
            },
            secrets.len(),
            if cache.is_some() { "an" } else { "aus" }
        ),
    )
    .await;
    let mut run_env = env.vars();
    run_env.extend(secrets.iter().cloned());
    let req = ExecRequest {
        name: run_name(task.id, "setup"),
        workspace: project.id.to_string(),
        program: "sh".into(),
        // -e: the setup fails at the first failing command.
        args: vec!["-e".into(), "-c".into(), env.setup_script.clone()],
        env: run_env,
        home_files: vec![],
        read_back: vec![],
        stdin: None,
        timeout_secs: env.setup_timeout_secs.max(1) as u64,
        idle_timeout_secs: None,
        queue_files: vec![],
        egress,
        cache,
        secret_env: secrets.iter().map(|(n, _)| n.clone()).collect(),
        home_env: vec![],
    };
    let values: Vec<String> = secrets.into_iter().map(|(_, v)| v).collect();
    let (tx, mut rx) = mpsc::channel::<ExecFrame>(512);
    let collect = async {
        let mut tail_buf = String::new();
        let mut lines = 0usize;
        let mut elog = EgressLog::new("Setup");
        while let Some(f) = rx.recv().await {
            match f {
                ExecFrame::Out { line } | ExecFrame::Err { line } => {
                    // The runner already redacts; this is the second line of defence.
                    let line = values
                        .iter()
                        .fold(line, |l, v| l.replace(v.as_str(), "***"));
                    tail_buf.push_str(&line);
                    tail_buf.push('\n');
                    if tail_buf.len() > 8000 {
                        tail_buf = tail(&tail_buf, 4000);
                    }
                    lines += 1;
                    if lines <= MAX_SETUP_EVENTS && !line.trim().is_empty() {
                        event(st, task, "setup", &line).await;
                    }
                }
                f @ ExecFrame::Egress { .. } => elog.on(st, task, &f).await,
                _ => {}
            }
        }
        elog.finish(st, task, enforced).await;
        tail_buf
    };
    let (last, output) = tokio::join!(exec(st, req, cancel, tx), collect);
    if cancel.is_cancelled() {
        return Ok(SetupOutcome::Cancelled);
    }
    let output = tail(&output, 2000);
    match last.map_err(RunError::Infra)? {
        ExecFrame::Exit {
            success,
            code,
            timed_out,
            cache,
            leaks,
            ..
        } => {
            if let Some(c) = cache {
                event(
                    st,
                    task,
                    "setup",
                    &format!(
                        "Cache {}: {}",
                        &c.key[..c.key.len().min(12)],
                        if c.hit {
                            "wiederverwendet"
                        } else {
                            "neu angelegt"
                        }
                    ),
                )
                .await;
            }
            if !leaks.is_empty() {
                return Ok(SetupOutcome::Failed(format!(
                    "Das Setup hat Geheimnisse in Dateien geschrieben – der Agent startet nicht, damit er sie nicht lesen kann. Betroffene Dateien wurden bereinigt:\n- {}\nBitte das Setup-Skript so ändern, dass es Geheimnisse nur aus Umgebungsvariablen liest (z. B. `npm config` mit `${{NPM_TOKEN}}` statt Token in `.npmrc`).",
                    leaks.join("\n- ")
                )));
            }
            if timed_out {
                return Ok(SetupOutcome::Failed(format!(
                    "Setup-Skript nach {} s abgebrochen (Zeitlimit der Umgebung).\n{output}",
                    env.setup_timeout_secs
                )));
            }
            if !success {
                return Ok(SetupOutcome::Failed(format!(
                    "Setup-Skript fehlgeschlagen (Exit {}). Der Agent wurde nicht gestartet.\n{output}",
                    code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
                )));
            }
            event(st, task, "setup", "Setup fertig").await;
            Ok(SetupOutcome::Ready)
        }
        ExecFrame::Fail { error } => {
            Err(RunError::Infra(anyhow::anyhow!("runner (setup): {error}")))
        }
        _ => Err(RunError::Infra(anyhow::anyhow!(
            "runner (setup): unexpected final frame"
        ))),
    }
}

/// Ok(Ok(())) = passed, Ok(Err(output tail)) = failed.
async fn run_verify(
    st: &AppState,
    task: &Task,
    cmd: &str,
    cancel: &CancellationToken,
    env: &Env,
) -> anyhow::Result<std::result::Result<(), String>> {
    let req = ExecRequest {
        name: run_name(task.id, "verify"),
        workspace: task.project_id.to_string(),
        program: "sh".into(),
        args: vec!["-c".into(), cmd.into()],
        env: env.vars(),
        home_files: vec![],
        read_back: vec![],
        stdin: None,
        timeout_secs: st.cfg.verify_timeout_min * 60,
        idle_timeout_secs: None,
        queue_files: vec![],
        egress: env.verify_policy(),
        cache: None,
        secret_env: vec![],
        home_env: vec![],
    };
    let enforced = environment::sandbox(st).await.egress_enforced;
    let (tx, mut rx) = mpsc::channel::<ExecFrame>(512);
    let collect = async {
        let mut buf = String::new();
        let mut elog = EgressLog::new("Prüfung");
        while let Some(f) = rx.recv().await {
            if matches!(f, ExecFrame::Egress { .. }) {
                elog.on(st, task, &f).await;
                continue;
            }
            if let ExecFrame::Out { line } | ExecFrame::Err { line } = f {
                buf.push_str(&line);
                buf.push('\n');
                if buf.len() > 16000 {
                    buf = tail(&buf, 8000);
                }
            }
        }
        elog.finish(st, task, enforced).await;
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
    /// Review not possible. `retry`: transient (provider/infra/pause), the
    /// review phase is retried; otherwise a human decides (WP03, WP01-N1).
    Unavailable {
        why: String,
        retry: bool,
    },
    Cancelled,
    Blocked(FailureClass, String),
}

struct ReviewCtx<'a> {
    before: &'a str,
    after: &'a str,
    summary: &'a str,
    /// Harness review route of the project routing (`None` = legacy fields).
    route: Option<&'a queued_api::StepRoute>,
    /// Access of the agent loop (default reviewer).
    exec: &'a RunAccess,
    env: &'a Env,
    /// Gate findings a human approved (context for the reviewer).
    approved: &'a [GateFinding],
}

/// Another legacy provider (not `exclude`) with stored credentials that the
/// data-residency rule allows and that is neither paused nor out of budget.
async fn other_reviewer(
    st: &AppState,
    user: Uuid,
    project: &Project,
    exclude: &str,
) -> anyhow::Result<Option<String>> {
    for p in ["claude", "codex", "gemini"] {
        if p == exclude || !st.cfg.providers().contains(&p) {
            continue;
        }
        if let AccessResult::Ok(a) = access_for(st, user, project, None, p, false).await? {
            if !access_blocked(st, user, &a).await? {
                return Ok(Some(p.to_string()));
            }
        }
    }
    Ok(None)
}

/// Reviewer target: routing route, legacy review provider, or the access
/// and model of the agent loop.
fn review_target(
    task: &Task,
    project: &Project,
    route: Option<&queued_api::StepRoute>,
) -> (Option<Uuid>, String, Option<String>) {
    match route {
        Some(r) => match &r.target {
            Some(t) => (
                t.connector_id,
                t.provider.clone().unwrap_or_else(|| task.provider.clone()),
                t.model
                    .clone()
                    .filter(|m| !m.is_empty())
                    .or(task.model.clone()),
            ),
            None => (task.connector_id, task.provider.clone(), task.model.clone()),
        },
        None => match project.review_provider.clone().filter(|p| !p.is_empty()) {
            Some(p) => (None, p, project.review_model.clone()),
            None => (
                task.connector_id,
                task.provider.clone(),
                project.review_model.clone().or(task.model.clone()),
            ),
        },
    }
}

async fn run_review(
    st: &AppState,
    task: &Task,
    project: &Project,
    rc: ReviewCtx<'_>,
    cancel: &CancellationToken,
    usage: &mut Usage,
    lim: &EffectiveLimits,
) -> Result<ReviewOutcome, RunError> {
    let (mut connector, mut provider, mut model) = review_target(task, project, rc.route);
    // Without an explicit reviewer: prefer another provider than the one
    // that implemented (self-preference of LLM judges, report P0.8).
    let explicit = rc.route.is_some_and(|r| r.target.is_some())
        || project
            .review_provider
            .as_deref()
            .is_some_and(|p| !p.is_empty());
    if !explicit {
        match other_reviewer(st, task.user_id, project, &rc.exec.provider).await? {
            Some(p) => {
                event(
                    st,
                    task,
                    "review",
                    &format!(
                        "Review durch einen anderen Anbieter ({p}) als die Umsetzung ({})",
                        rc.exec.provider
                    ),
                )
                .await;
                connector = None;
                provider = p;
                model = None;
            }
            None => {
                event(
                    st,
                    task,
                    "review",
                    "Review durch denselben Anbieter – kein anderer Zugang verfügbar",
                )
                .await
            }
        }
    }
    let same = connector == rc.exec.connector_id && provider == rc.exec.provider;
    let acc_owned;
    let acc: &RunAccess = if same && connector.is_some() {
        rc.exec
    } else {
        match access_for(st, task.user_id, project, connector, &provider, false).await? {
            AccessResult::Ok(a) => {
                acc_owned = a;
                &acc_owned
            }
            AccessResult::Missing(m) => {
                return Ok(ReviewOutcome::Unavailable {
                    why: m,
                    retry: false,
                })
            }
            // Residency or a vanished access must never be skipped silently.
            AccessResult::Denied(msg) => {
                return Ok(ReviewOutcome::Blocked(
                    FailureClass::Config,
                    format!("{msg} (Review)"),
                ))
            }
        }
    };
    if access_blocked(st, task.user_id, acc).await? {
        return Ok(ReviewOutcome::Unavailable {
            why: format!("{} ist pausiert oder das Budget ist erschöpft", acc.label),
            retry: true,
        });
    }
    event(st, task, "review", &format!("Review via {}", acc.label)).await;
    let diff = st
        .runner
        .diff(project.id, rc.before, rc.after, prompt::MAX_DIFF)
        .await?
        .diff;
    let meter = meter_for(st, acc, model.as_deref(), "review").await;
    let out = match agent_run(
        st,
        task,
        RunSpec {
            provider: &acc.provider,
            model: model.as_deref(),
            cred: &acc.cred,
            prompt: prompt::review(task, rc.summary, &secrets::redact(&diff), rc.approved),
            kind: RunKind::Review,
            label: "review",
            limits: lim,
            used_before: usage,
            meter,
            env: rc.env,
            trust_repo_config: project.trust_repo_config,
        },
        cancel,
    )
    .await
    {
        Ok(o) => o,
        Err(RunError::Config(msg)) => {
            return Ok(ReviewOutcome::Unavailable {
                why: format!("Konfigurationsfehler: {msg}"),
                retry: false,
            })
        }
        Err(e) => return Err(e),
    };
    record_run_usage(st, task, acc, &out, usage).await?;
    if cancel.is_cancelled() {
        return Ok(ReviewOutcome::Cancelled);
    }
    let caps = agent::capabilities_for(&acc.provider, acc.cred.conn.as_ref());
    if let Some(d) = early(&out, false, &acc.provider, lim, usage) {
        return Ok(match d {
            Disposition::Blocked { class, reason } => {
                ReviewOutcome::Blocked(class, format!("{reason} (Review)"))
            }
            Disposition::Requeue { class, reason } => ReviewOutcome::Unavailable {
                why: format!("{}: {}", class_label(class), cut(&reason, 300)),
                retry: true,
            },
            Disposition::RateLimited(_) | Disposition::AuthPaused(_) => {
                ReviewOutcome::Unavailable {
                    why: format!("{} ist pausiert (Limit/Anmeldung)", acc.label),
                    retry: true,
                }
            }
            _ => ReviewOutcome::Unavailable {
                why: "Review-Lauf abgebrochen".into(),
                retry: true,
            },
        });
    }
    let file = st.runner.queue_file(project.id, "review.json").await?;
    let review = load_review(out.structured.as_ref(), file.as_deref());
    // Limits/auth problems of the review access pause it for the next tasks,
    // whether or not a review result exists (same rule as for agent runs).
    if let Some((c, msg)) = &out.failure {
        match c.class() {
            FailureClass::RateLimit => {
                let reset = match c {
                    ErrorCategory::RateLimit(t) => *t,
                    _ => None,
                };
                let until = pause_rate_limited(st, task, &acc.key, reset).await;
                event(
                    st,
                    task,
                    "limit",
                    &format!(
                        "Limit erreicht – {} pausiert bis {}",
                        acc.label,
                        until.to_rfc3339()
                    ),
                )
                .await;
            }
            FailureClass::ProviderAuth => pause_auth(st, task, &acc.key, &acc.label, msg).await,
            _ => {}
        }
    }
    if review.is_none() {
        if let Some((c, msg)) = &out.failure {
            return Ok(ReviewOutcome::Unavailable {
                why: format!("{}: {}", class_label(c.class()), cut(msg, 300)),
                retry: c.class().is_transient()
                    || matches!(
                        c.class(),
                        FailureClass::RateLimit | FailureClass::ProviderAuth
                    ),
            });
        }
        let _ = caps;
        return Ok(ReviewOutcome::Unavailable {
            why: "der Review-Agent hat kein Ergebnis geliefert".into(),
            retry: true,
        });
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

// ---------------------------------------------------------------------------
// Single-shot steps (plan, review, analysis) as jobs – WP13
// ---------------------------------------------------------------------------

/// What the review job needs to finish the task later.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct PendingReview {
    pub summary: String,
    pub handoff: Option<String>,
    pub follow_ups: Vec<FollowUp>,
    pub before: String,
    pub after: String,
}

/// Connector of a single-shot step (target or the agent loop's connector).
async fn step_connector(
    st: &AppState,
    task: &Task,
    project: &Project,
    route: &queued_api::StepRoute,
    exec: Option<&Target>,
) -> anyhow::Result<Result<(connectors::Resolved, String), String>> {
    let target = route.target.as_ref().or(exec);
    let model = route
        .target
        .as_ref()
        .and_then(|t| t.model.clone())
        .or_else(|| exec.and_then(|t| t.model.clone()))
        .or(task.model.clone())
        .filter(|m| !m.trim().is_empty());
    let Some(cid) = target.and_then(|t| t.connector_id).or(task.connector_id) else {
        return Ok(Err("kein Zugang mit API-Schlüssel gewählt".into()));
    };
    let Some(c) = connectors::load_for(st, cid, task.user_id, Some(project.id)).await? else {
        return Ok(Err("Zugang nicht mehr verfügbar".into()));
    };
    let r = project.residency();
    if !r.allows(c.location) {
        return Ok(Err(connectors::residency_error(
            r,
            c.location,
            &format!("Der Zugang „{}“", c.label),
        )));
    }
    let Some(model) = model else {
        return Ok(Err("kein Modell angegeben".into()));
    };
    let blocked = budget::status(&st.db, task.user_id, &c.limit_key())
        .await?
        .blocked
        || budget::status(&st.db, task.user_id, &c.harness)
            .await?
            .budget_until
            .is_some();
    if blocked {
        return Ok(Err(format!(
            "{} ist pausiert oder das Budget ist erschöpft",
            c.label
        )));
    }
    Ok(Ok((c, model)))
}

/// Stable prefix of single-shot prompts (cacheable).
async fn stable_prefix(st: &AppState, project: &Project) -> String {
    let files = st.runner.files(project.id).await.unwrap_or_default();
    prompt::single_shot_system(project, &files)
}

/// Plan before / analysis instead of the agent loop. Returns whether the
/// task now waits for the job. On a configuration problem the plan is
/// skipped (and the analysis falls back to the agentic run).
#[allow(clippy::too_many_arguments)]
async fn defer_single(
    st: &AppState,
    task: &Task,
    project: &Project,
    settings: &Settings,
    routing: &queued_api::Routing,
    step: &'static str,
    route: &queued_api::StepRoute,
    usage: &Usage,
) -> Result<bool, RunError> {
    let (c, model) =
        match step_connector(st, task, project, route, routing.execute.target.as_ref()).await? {
            Ok(x) => x,
            Err(why) => {
                if why.contains("erlaubt nur") {
                    // Data residency is a hard rule, never a silent fallback.
                    return Err(RunError::Config(format!(
                        "{}: {why}",
                        batch::step_label(step)
                    )));
                }
                event(
                    st,
                    task,
                    "batch",
                    &format!("{} übersprungen: {why}", batch::step_label(step)),
                )
                .await;
                if step == "plan" {
                    sqlx::query("UPDATE tasks SET plan='' WHERE id=$1")
                        .bind(task.id)
                        .execute(&st.db)
                        .await?;
                }
                return Ok(false);
            }
        };
    let system = stable_prefix(st, project).await;
    let user = if step == "plan" {
        let parent = parent_of(st, task).await?;
        prompt::plan_request(task, parent.as_ref())
    } else {
        let history = onempty_history(st, project).await?;
        prompt::analyze_request(&settings.onempty_actions, &history)
    };
    let mode = route.mode;
    let _ = usage;
    batch::create(
        st,
        batch::JobSpec {
            task,
            step,
            mode,
            connector: &c,
            request: llm::Request {
                model,
                system,
                user,
                max_tokens: if step == "plan" { 8_000 } else { 12_000 },
                cache_long: mode == StepMode::Batch,
            },
            payload: json!({}),
        },
    )
    .await?;
    Ok(true)
}

/// Review as a single-shot job after a successful run and verify.
async fn defer_review(
    st: &AppState,
    task: &Task,
    project: &Project,
    routing: &queued_api::Routing,
    route: &queued_api::StepRoute,
    pending: &PendingReview,
    usage: &Usage,
) -> Result<bool, RunError> {
    let (c, model) =
        match step_connector(st, task, project, route, routing.execute.target.as_ref()).await? {
            Ok(x) => x,
            Err(why) => {
                if why.contains("erlaubt nur") {
                    return Err(RunError::Config(format!("Review: {why}")));
                }
                event(
                    st,
                    task,
                    "review",
                    &format!("Review per API nicht möglich ({why}) – agentisches Review"),
                )
                .await;
                return Ok(false);
            }
        };
    let diff = st
        .runner
        .diff(
            project.id,
            &pending.before,
            &pending.after,
            prompt::MAX_DIFF,
        )
        .await?
        .diff;
    let system = stable_prefix(st, project).await;
    // Keep what the run produced; the job finishes the task.
    sqlx::query("UPDATE tasks SET summary=$2, handoff=COALESCE($3, handoff), usage=$4 WHERE id=$1")
        .bind(task.id)
        .bind(&pending.summary)
        .bind(&pending.handoff)
        .bind(serde_json::to_value(usage).unwrap_or_default())
        .execute(&st.db)
        .await?;
    batch::create(
        st,
        batch::JobSpec {
            task,
            step: "review",
            mode: route.mode,
            connector: &c,
            request: llm::Request {
                model,
                system,
                user: prompt::review_request(task, &pending.summary, &diff),
                max_tokens: 4_000,
                cache_long: route.mode == StepMode::Batch,
            },
            payload: serde_json::to_value(pending).unwrap_or_default(),
        },
    )
    .await?;
    Ok(true)
}

async fn parent_of(st: &AppState, task: &Task) -> sqlx::Result<Option<Parent>> {
    Ok(match task.parent_id {
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
    })
}

async fn onempty_history(
    st: &AppState,
    project: &Project,
) -> sqlx::Result<Vec<(String, Option<String>)>> {
    sqlx::query_as(
        "SELECT title, summary FROM tasks WHERE project_id=$1 AND status='done' AND kind<>'onempty' ORDER BY finished_at DESC LIMIT 8",
    )
    .bind(project.id)
    .fetch_all(&st.db)
    .await
}

/// Continues a task after its review job (poller).
pub(crate) async fn complete_review(
    st: &AppState,
    task: &Task,
    payload: &Value,
    outcome: Result<ReviewResult, String>,
    usage: &Usage,
) -> anyhow::Result<()> {
    let p: PendingReview = serde_json::from_value(payload.clone()).unwrap_or_default();
    let project: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(task.project_id)
        .fetch_one(&st.db)
        .await?;
    let settings = queue::load_settings(st, task.user_id).await?;
    let mut pending = gate::Pending {
        summary: p.summary,
        handoff: p.handoff,
        follow_ups: p.follow_ups,
        failure: None,
        questions: vec![],
        before: p.before,
        after: Some(p.after),
        approved: vec![],
        review: None,
    };
    let res = match outcome {
        Ok(r) => {
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
            pending.failure =
                (!r.approved).then(|| format!("Review abgelehnt:\n- {}", r.issues.join("\n- ")));
            pending.review = Some(json!(r));
            gate::finish_flow(st, task, &project, &settings, pending, None, usage).await
        }
        // WP01-N1: a review that did not happen never ends as `done`.
        Err(why) => {
            gate::hold(
                st,
                task,
                queued_api::gate::GateStage::Review,
                vec![GateFinding {
                    kind: queued_common::FindingKind::ReviewUnavailable,
                    path: String::new(),
                    line: None,
                    detail: why,
                    fingerprint: None,
                }],
                &pending,
                None,
                false,
                usage,
            )
            .await
        }
    };
    res.map_err(|e| match e {
        RunError::Config(m) => anyhow::anyhow!(m),
        RunError::Infra(e) => e,
    })?;
    st.wake.notify_one();
    Ok(())
}

/// Continues an OnEmpty task after its analysis job (poller).
pub(crate) async fn complete_analyze(
    st: &AppState,
    task: &Task,
    outcome: Result<AgentResult, String>,
    usage: &Usage,
) -> anyhow::Result<()> {
    let settings = queue::load_settings(st, task.user_id).await?;
    let res = match outcome {
        Ok(r) if r.status.as_deref() == Some("needs_input") && !r.questions.is_empty() => finish(
            st,
            task.id,
            Finish {
                status: status::WAITING_INPUT,
                summary: r.summary.clone(),
                handoff: r.handoff.clone(),
                error: None,
                review: None,
                questions: Some(json!(r.questions)),
                usage,
                failure_class: None,
            },
        )
        .await
        .map_err(RunError::from),
        Ok(r) => {
            let end = End {
                summary: r
                    .summary
                    .clone()
                    .unwrap_or_else(|| "Analyse abgeschlossen".into()),
                handoff: r.handoff.clone(),
                review: None,
                usage,
            };
            finish_done(st, task, &settings, end, r.follow_up_tasks).await
        }
        Err(why) => {
            let end = End {
                summary: String::new(),
                handoff: None,
                review: None,
                usage,
            };
            finish_failed(
                st,
                task,
                &settings,
                end,
                &format!("Analyse fehlgeschlagen: {why}"),
            )
            .await
        }
    };
    res.map_err(|e| match e {
        RunError::Config(m) => anyhow::anyhow!(m),
        RunError::Infra(e) => e,
    })?;
    st.wake.notify_one();
    Ok(())
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
    // OnEmpty proposals run on the project's agent-loop access; other
    // follow-ups inherit the parent's access.
    let (provider, connector_id, model) = if parent.kind == "onempty" {
        let route =
            sqlx::query_scalar::<_, Option<Value>>("SELECT routing FROM projects WHERE id=$1")
                .bind(parent.project_id)
                .fetch_one(&st.db)
                .await?
                .and_then(|r| serde_json::from_value::<queued_api::Routing>(r).ok())
                .and_then(|r| r.execute.target);
        match route {
            Some(t) => (
                t.provider
                    .clone()
                    .unwrap_or_else(|| parent.provider.clone()),
                t.connector_id,
                t.model.clone(),
            ),
            None => (settings.default_provider.clone(), None, None),
        }
    } else {
        (
            parent.provider.clone(),
            parent.connector_id,
            parent.model.clone(),
        )
    };
    // A connector's harness wins over the stored provider.
    let provider = match connector_id {
        Some(c) => match connectors::load_row(st, c).await? {
            Some(row) => connectors::harness(&row.3, &row.5),
            None => provider,
        },
        None => provider,
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
                model: model.clone(),
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
                connector_id,
                escalation_level: 0,
                allow_test_changes: false,
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
    // Escalation along the routing chain (e.g. Haiku → Sonnet → Opus).
    let routing: Option<queued_api::Routing> =
        sqlx::query_scalar::<_, Option<Value>>("SELECT routing FROM projects WHERE id=$1")
            .bind(failed.project_id)
            .fetch_one(&st.db)
            .await?
            .and_then(|r| serde_json::from_value(r).ok());
    let (model, level) = match crate::routing::escalate(routing.as_ref(), failed) {
        Some((m, l)) => {
            event(
                st,
                failed,
                "status",
                &format!("Eskalation: der nächste Versuch nutzt {m}"),
            )
            .await;
            (Some(m), l)
        }
        None => (failed.model.clone(), failed.escalation_level),
    };
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
            model,
            status: status::QUEUED,
            attempt: failed.attempt + 1,
            depth: failed.depth,
            created_by_agent: true,
            front: true,
            // A fix inherits the task-level limits of the failed task.
            limits: TaskLimits::of(failed),
            connector_id: failed.connector_id,
            escalation_level: level,
            allow_test_changes: false,
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
        classify_run(
            o,
            false,
            provider,
            agent::capabilities(provider),
            c,
            &lim(),
            &o.usage(),
        )
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
                &lim(),
                &Usage::default()
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

    /// Reviewer decision on the open question of the test report: an overrun
    /// that only shows up in the final usage report (codex/gemini, or
    /// under-counted Claude deltas) blocks like the live guard, even with a
    /// valid `done` contract – no done, no review, no fix task, no follow-ups.
    #[test]
    fn post_hoc_overrun_blocks_even_with_a_contract() {
        let o = RunOutcome {
            success: true,
            usage_total: Some(Usage {
                input_tokens: 5000,
                ..Default::default()
            }),
            ..Default::default()
        };
        for p in ["codex", "gemini", "claude"] {
            match classify(&o, p, Some(done())).0 {
                Disposition::Blocked {
                    class: FailureClass::LimitTokens,
                    reason,
                } => assert!(
                    reason.contains("Tokens 1000")
                        && reason.contains("verbraucht 5000 Tokens")
                        && reason.contains("erst nach dem Lauf"),
                    "{reason}"
                ),
                d => panic!("{p}: {d:?}"),
            }
        }
        let over_cost = RunOutcome {
            usage_total: Some(Usage {
                cost_usd: 2.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(matches!(
            classify(&over_cost, "codex", Some(done())).0,
            Disposition::Blocked {
                class: FailureClass::LimitCost,
                ..
            }
        ));
        // Below the limit the contract decides; the task total counts, not the run.
        let small = RunOutcome {
            usage_total: Some(Usage {
                input_tokens: 999,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            classify(&small, "codex", Some(done())).0,
            Disposition::Contract(done())
        );
        let earlier = Usage {
            input_tokens: 600,
            ..Default::default()
        };
        let mut total = earlier.clone();
        total.add(&small.usage());
        assert!(matches!(
            classify_run(
                &small,
                false,
                "codex",
                agent::capabilities("codex"),
                Some(done()),
                &lim(),
                &total
            )
            .0,
            Disposition::Blocked {
                class: FailureClass::LimitTokens,
                ..
            }
        ));
        // A user cancel still wins.
        assert_eq!(
            classify_run(
                &o,
                true,
                "codex",
                agent::capabilities("codex"),
                Some(done()),
                &lim(),
                &o.usage()
            )
            .0,
            Disposition::Cancelled
        );
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
