//! HTTP API types shared by `queued` (backend) and `queued-web` (Leptos/WASM
//! frontend). One definition per wire type: the backend serializes exactly
//! these structs, the frontend deserializes them.
//!
//! Only `serde`, `uuid` and `chrono` are required, so the crate compiles to
//! `wasm32-unknown-unknown`. The `sqlx` feature (backend only) derives
//! `FromRow` for the types that are read straight from the database.
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub mod branch;
mod costs;
pub mod diff;
pub mod env;
pub mod gate;
pub mod pr;
mod providers;
pub mod steer;
pub mod tokens;
pub mod triggers;
pub use costs::*;
pub use providers::*;

/// For PATCH semantics: field absent → `None` (unchanged), `null` →
/// `Some(None)` (back to the default), value → `Some(Some(v))`.
pub fn double_option<'de, T, D>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

// ---------------------------------------------------------------------------
// Users, auth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OidcProvider {
    pub id: String,
    pub name: String,
}

/// What a provider integration can enforce/report natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// The CLI enforces the result schema (missing contract = task failure).
    pub structured_output: bool,
    pub max_turns: bool,
    pub max_cost: bool,
    /// Token usage arrives while the run is in progress (live token guard).
    pub live_usage: bool,
}

/// `GET /api/meta` (public).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub providers: Vec<String>,
    pub credential_kinds: BTreeMap<String, Vec<String>>,
    pub oidc: Vec<OidcProvider>,
    pub allow_registration: bool,
    pub provider_capabilities: BTreeMap<String, ProviderCapabilities>,
    pub cli_versions: BTreeMap<String, String>,
    /// Connector types this server offers (WP13).
    #[serde(default)]
    pub connector_types: Vec<ConnectorTypeInfo>,
    /// Maintenance state of the pinned CLIs (e.g. Gemini CLI).
    #[serde(default)]
    pub cli_notes: BTreeMap<String, CliNote>,
    /// Version of the built-in price catalog.
    #[serde(default)]
    pub price_catalog_version: String,
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// `tasks.status` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    PendingApproval,
    Queued,
    Running,
    /// A single-shot step (plan, review, analysis) waits for a provider
    /// (batch API or synchronous call). Occupies no slot, locks no project.
    WaitingBatch,
    WaitingInput,
    /// A gate holds the task until a human decides (WP03, see `Task::gate`).
    Held,
    /// Finished and checked; waits in the project's merge queue or is being
    /// merged into the target branch (WP04, see `Task::merge_state`).
    /// Occupies no agent slot.
    Merging,
    Done,
    Failed,
    Blocked,
    Cancelled,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 11] = [
        Self::PendingApproval,
        Self::Queued,
        Self::Running,
        Self::WaitingBatch,
        Self::WaitingInput,
        Self::Held,
        Self::Merging,
        Self::Done,
        Self::Failed,
        Self::Blocked,
        Self::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingApproval => "pending_approval",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingBatch => "waiting_batch",
            Self::WaitingInput => "waiting_input",
            Self::Held => "held",
            Self::Merging => "merging",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Finished runs (history).
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Blocked | Self::Cancelled
        )
    }

    /// The user has to do something (answer or approve).
    pub fn needs_user(self) -> bool {
        matches!(
            self,
            Self::WaitingInput | Self::PendingApproval | Self::Held
        )
    }

    /// Title, prompt, context, provider and limits may still be changed.
    pub fn is_editable(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::PendingApproval | Self::WaitingInput
        )
    }

    pub fn can_cancel(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::PendingApproval
                | Self::Running
                | Self::WaitingBatch
                | Self::WaitingInput
                | Self::Held
                | Self::Merging
        )
    }

    pub fn can_retry(self) -> bool {
        matches!(self, Self::Failed | Self::Blocked | Self::Cancelled)
    }
}

/// Why a run did not finish normally. Only [`FailureClass::Task`] consumes a
/// fix attempt; everything else is re-queued, pauses the provider or blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// The agent's work is wrong (contract `failed`, verify/review failed …) → fix task.
    Task,
    /// Subscription/API rate limit → provider paused until reset, task re-queued.
    RateLimit,
    /// Overloaded / 5xx / network → re-queued with backoff.
    ProviderTransient,
    /// Runner unreachable, stream broken, orchestration error → re-queued with backoff.
    Infra,
    /// No output for the stall timeout → re-queued with backoff.
    Stalled,
    /// Authentication / billing → provider paused until credentials change.
    ProviderAuth,
    /// Unknown model, invalid request, broken credential → blocked.
    Config,
    LimitRuntime,
    LimitCost,
    LimitTokens,
    LimitTurns,
    Cancelled,
    /// A human rejected a gate (WP03): changes discarded or task stopped.
    Gate,
}

impl FailureClass {
    pub const ALL: [FailureClass; 13] = [
        Self::Task,
        Self::RateLimit,
        Self::ProviderTransient,
        Self::Infra,
        Self::Stalled,
        Self::ProviderAuth,
        Self::Config,
        Self::LimitRuntime,
        Self::LimitCost,
        Self::LimitTokens,
        Self::LimitTurns,
        Self::Cancelled,
        Self::Gate,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::RateLimit => "rate_limit",
            Self::ProviderTransient => "provider_transient",
            Self::Infra => "infra",
            Self::Stalled => "stalled",
            Self::ProviderAuth => "provider_auth",
            Self::Config => "config",
            Self::LimitRuntime => "limit_runtime",
            Self::LimitCost => "limit_cost",
            Self::LimitTokens => "limit_tokens",
            Self::LimitTurns => "limit_turns",
            Self::Cancelled => "cancelled",
            Self::Gate => "gate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Re-queued with backoff and counted in `transient_retries`.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::ProviderTransient | Self::Infra | Self::Stalled)
    }
}

/// Token usage of one or more agent runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd: f64,
}

impl Usage {
    /// Saturating: sums over many runs must never overflow (debug panic,
    /// release wrap-around into negative usage).
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens = self.input_tokens.saturating_add(o.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(o.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(o.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(o.cache_write_tokens);
        self.cost_usd += o.cost_usd;
    }
}

/// Verdict of the review agent as stored in `tasks.review`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ReviewResult {
    pub approved: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Task {
    pub id: Uuid,
    pub user_id: Uuid,
    pub project_id: Uuid,
    pub parent_id: Option<Uuid>,
    /// `task` | `fix` | `onempty`
    pub kind: String,
    pub title: String,
    pub prompt: String,
    pub context: String,
    pub provider: String,
    pub model: Option<String>,
    /// See [`TaskStatus`].
    pub status: String,
    pub position: i64,
    pub attempt: i32,
    pub depth: i32,
    pub created_by_agent: bool,
    /// JSON array of strings, see [`Task::questions_list`].
    pub questions: Option<Value>,
    pub handoff: Option<String>,
    pub summary: Option<String>,
    pub error: Option<String>,
    /// [`ReviewResult`] as JSON, see [`Task::review_result`].
    pub review: Option<Value>,
    /// [`Usage`] as JSON, see [`Task::usage_parsed`].
    pub usage: Option<Value>,
    pub snapshot_before: Option<String>,
    pub snapshot_after: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub max_runtime_secs: Option<i32>,
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<i64>,
    pub max_turns: Option<i32>,
    /// [`FailureClass`] of the last run that did not finish normally.
    pub failure_class: Option<String>,
    pub transient_retries: i32,
    pub not_before: Option<DateTime<Utc>>,
    /// Connector of the agent loop (`None` = legacy credential of `provider`).
    #[serde(default)]
    pub connector_id: Option<Uuid>,
    /// Position in the model escalation chain of the project routing.
    #[serde(default)]
    pub escalation_level: i32,
    /// Result of the plan step (single-shot call), handed to the agent.
    #[serde(default)]
    pub plan: Option<String>,
    /// [`TaskCost`] as JSON, see [`Task::cost_parsed`].
    #[serde(default)]
    pub cost: Option<Value>,
    /// [`gate::TaskGate`] as JSON while a gate holds the task (WP03).
    #[serde(default)]
    pub gate: Option<Value>,
    /// Created by a human as test task: changes to protected paths and
    /// tests need no approval (agent-created tasks never get this).
    #[serde(default)]
    pub allow_test_changes: bool,
    /// Own branch `queue/<id>` (WP04); `None` = task from before WP04.
    #[serde(default)]
    pub branch: Option<String>,
    /// Target-branch commit the branch is based on (diff base); after the
    /// merge the parent of the merged commit.
    #[serde(default)]
    pub merge_base: Option<String>,
    /// [`branch::MergeState`], see [`Task::merge_state_kind`].
    #[serde(default)]
    pub merge_state: Option<String>,
    /// Entered the merge queue (order of the queue).
    #[serde(default)]
    pub merge_queued_at: Option<DateTime<Utc>>,
    /// Fix task that continues the changes of this task.
    #[serde(default)]
    pub carry_from: Option<Uuid>,
    /// WP05: pull request this task works on (follow-ups and their fixes
    /// push to its branch).
    #[serde(default)]
    pub pr_id: Option<Uuid>,
    /// WP06: the agent first writes a plan that a human approves.
    #[serde(default)]
    pub plan_mode: bool,
    /// [`steer::PlanState`] (`None` = no plan mode or not planned yet).
    #[serde(default)]
    pub plan_state: Option<String>,
    /// Number of revisions a human asked for.
    #[serde(default)]
    pub plan_revision: i32,
    /// Note for the next plan run ("Überarbeiten lassen").
    #[serde(default)]
    pub plan_feedback: Option<String>,
    /// [`steer::PlanMeta`] as JSON (questions, attempted changes, source).
    #[serde(default)]
    pub plan_meta: Option<Value>,
    /// [`steer::AgentSession`] as JSON: the CLI session of the last run.
    #[serde(default)]
    pub agent_session: Option<Value>,
    /// "Weiter mit …": the task whose CLI session this one continues.
    #[serde(default)]
    pub session_from: Option<Uuid>,
    /// Steering rounds (resumes for user messages) so far.
    #[serde(default)]
    pub steer_rounds: i32,
    /// WP07: the trigger (schedule, webhook, GitHub, API) that created it.
    #[serde(default)]
    pub trigger_id: Option<Uuid>,
}

impl Task {
    pub fn plan_state_kind(&self) -> Option<steer::PlanState> {
        self.plan_state.as_deref().and_then(steer::PlanState::parse)
    }

    pub fn plan_meta_parsed(&self) -> Option<steer::PlanMeta> {
        self.plan_meta
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn agent_session_parsed(&self) -> Option<steer::AgentSession> {
        self.agent_session
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn cost_parsed(&self) -> Option<TaskCost> {
        self.cost
            .as_ref()
            .and_then(|c| serde_json::from_value(c.clone()).ok())
    }

    /// Unknown values (never written by the backend) count as `queued`.
    pub fn status_kind(&self) -> TaskStatus {
        TaskStatus::parse(&self.status).unwrap_or(TaskStatus::Queued)
    }

    pub fn merge_state_kind(&self) -> Option<branch::MergeState> {
        self.merge_state
            .as_deref()
            .and_then(branch::MergeState::parse)
    }

    /// Base of the task's diff and revert: the merge base for branch tasks,
    /// the first snapshot for tasks from before WP04.
    pub fn diff_base(&self) -> Option<&str> {
        self.merge_base
            .as_deref()
            .or(self.snapshot_before.as_deref())
    }

    /// Its changes are on the target branch (revert possible): merged
    /// branch tasks and finished tasks from before WP04.
    pub fn on_target(&self) -> bool {
        match &self.branch {
            Some(_) => self.merge_state_kind() == Some(branch::MergeState::Merged),
            None => true,
        }
    }

    pub fn failure(&self) -> Option<FailureClass> {
        self.failure_class.as_deref().and_then(FailureClass::parse)
    }

    pub fn questions_list(&self) -> Vec<String> {
        self.questions
            .as_ref()
            .and_then(|q| serde_json::from_value(q.clone()).ok())
            .unwrap_or_default()
    }

    pub fn gate_parsed(&self) -> Option<gate::TaskGate> {
        self.gate
            .as_ref()
            .and_then(|g| serde_json::from_value(g.clone()).ok())
    }

    pub fn review_result(&self) -> Option<ReviewResult> {
        self.review
            .as_ref()
            .and_then(|r| serde_json::from_value(r.clone()).ok())
    }

    pub fn usage_parsed(&self) -> Option<Usage> {
        self.usage
            .as_ref()
            .and_then(|u| serde_json::from_value(u.clone()).ok())
    }

    /// Diff base and last commit exist and differ: there is a diff (revert
    /// additionally needs [`Task::on_target`]).
    pub fn has_changes(&self) -> bool {
        matches!((self.diff_base(), &self.snapshot_after), (Some(a), Some(b)) if a != b)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct TaskEvent {
    pub id: i64,
    pub task_id: Uuid,
    pub ts: DateTime<Utc>,
    pub kind: String,
    pub content: String,
}

/// `GET /api/tasks/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub task: Task,
    pub events: Vec<TaskEvent>,
    /// Pull request opened for this task or worked on by it (WP05).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<pr::PullRequestView>,
}

/// `GET /api/tasks/{id}/diff`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffView {
    pub diff: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
}

/// Task-level limit fields (as in POST/PATCH /api/tasks). Outer `Option` =
/// field present, inner `None` = `null` (back to the project default).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskLimitFields {
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_runtime_secs: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_cost_usd: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tokens: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_turns: Option<Option<i64>>,
}

/// `POST /api/tasks`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTask {
    pub project_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Connector of the agent loop; its harness replaces `provider`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_id: Option<Uuid>,
    #[serde(default)]
    pub front: bool,
    /// Test task: may change protected paths/tests without approval.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_test_changes: bool,
    /// WP06: plan first (`None` = the project's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
    #[serde(flatten)]
    pub limits: TaskLimitFields,
}

/// `PATCH /api/tasks/{id}` – absent fields stay unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Absent = unchanged, `null` = back to the legacy credential of `provider`.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub connector_id: Option<Option<Uuid>>,
    #[serde(flatten)]
    pub limits: TaskLimitFields,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answer {
    pub answer: String,
}

/// `POST /api/tasks/{id}/move`; `direction` is one of the `MOVE_*` constants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveTask {
    pub direction: String,
}

pub const MOVE_TOP: &str = "top";
pub const MOVE_UP: &str = "up";
pub const MOVE_DOWN: &str = "down";
pub const MOVE_BOTTOM: &str = "bottom";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproveAllResult {
    pub approved: usize,
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectRole {
    Owner,
    Editor,
    Viewer,
}

impl ProjectRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Editor => "editor",
            Self::Viewer => "viewer",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Owner, Self::Editor, Self::Viewer]
            .into_iter()
            .find(|r| r.as_str() == s)
    }

    pub fn can_edit(self) -> bool {
        self != Self::Viewer
    }
}

/// `GET /api/projects` (one row per membership).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub role: ProjectRole,
    pub owner_name: String,
    pub git_remote_url: Option<String>,
    /// WP06: new tasks of the project plan first (composer default).
    #[serde(default)]
    pub plan_mode: bool,
}

/// `GET/PATCH /api/projects/{id}`: the project without its encrypted git
/// token, plus the caller's role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub description: String,
    pub goal: String,
    pub verify_command: Option<String>,
    pub review_enabled: bool,
    pub review_provider: Option<String>,
    pub review_model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub git_remote_url: Option<String>,
    pub git_branch: String,
    pub auto_push: bool,
    pub max_runtime_secs: Option<i32>,
    pub stall_timeout_secs: Option<i32>,
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<i64>,
    pub max_turns: Option<i32>,
    pub role: ProjectRole,
    pub has_git_token: bool,
    /// Per-step routing (`None` = legacy behaviour: task provider, project review provider).
    #[serde(default)]
    pub routing: Option<Routing>,
    /// Hard data-location rule, enforced by the server.
    #[serde(default)]
    pub data_residency: Residency,
    /// Tasks of this project that may run at the same time (WP04).
    #[serde(default = "branch::default_parallelism")]
    pub parallelism: i32,
    /// What a finished task delivers (WP05).
    #[serde(default)]
    pub output_mode: pr::OutputMode,
    /// WP06: new tasks plan first (default of the composer option).
    #[serde(default)]
    pub plan_mode: bool,
    /// Plans are approved automatically after this many minutes (`None` = never).
    #[serde(default)]
    pub plan_auto_approve_mins: Option<i32>,
}

/// Response of `POST /api/projects`: the new project and its first commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedProject {
    #[serde(flatten)]
    pub project: ProjectView,
    pub head: String,
}

/// `POST /api/projects`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewProject {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_token: Option<String>,
}

/// `PATCH /api/projects/{id}` – absent fields stay unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateProject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// New token; empty string removes it; absent keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_push: Option<bool>,
    /// Limits per task run: absent = unchanged, `null` = back to the default.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_runtime_secs: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub stall_timeout_secs: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_cost_usd: Option<Option<f64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tokens: Option<Option<i64>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_turns: Option<Option<i64>>,
    /// Tasks of the project at the same time (1–8, owner only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<i32>,
    /// WP06 plan mode default of new tasks (owner only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
    /// Automatic plan approval in minutes; `null` = off (owner only).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub plan_auto_approve_mins: Option<Option<i32>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
    pub role: ProjectRole,
}

/// `POST /api/projects/{id}/members`; `role` is `editor` or `viewer`
/// (validated by the backend with a readable error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddMember {
    pub email: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileView {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportResult {
    pub files: usize,
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub author: String,
    /// As reported by git (ISO 8601 with offset).
    pub date: String,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushResult {
    pub ok: bool,
    pub output: String,
}

// ---------------------------------------------------------------------------
// Settings, budgets, credentials
// ---------------------------------------------------------------------------

/// `GET /api/settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Settings {
    pub user_id: Uuid,
    pub parallelism: i32,
    pub default_provider: String,
    pub onempty_enabled: bool,
    pub onempty_actions: Vec<String>,
    pub onempty_cooldown_minutes: i32,
    pub max_auto_tasks: i32,
    pub max_fix_attempts: i32,
    pub ntfy_url: Option<String>,
    #[serde(default)]
    pub last_onempty_at: Option<DateTime<Utc>>,
}

/// `PUT /api/settings` (the server clamps the numbers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsUpdate {
    pub parallelism: i32,
    pub default_provider: String,
    pub onempty_enabled: bool,
    pub onempty_actions: Vec<String>,
    pub onempty_cooldown_minutes: i32,
    pub max_auto_tasks: i32,
    pub max_fix_attempts: i32,
    pub ntfy_url: Option<String>,
}

impl From<&Settings> for SettingsUpdate {
    fn from(s: &Settings) -> Self {
        Self {
            parallelism: s.parallelism,
            default_provider: s.default_provider.clone(),
            onempty_enabled: s.onempty_enabled,
            onempty_actions: s.onempty_actions.clone(),
            onempty_cooldown_minutes: s.onempty_cooldown_minutes,
            max_auto_tasks: s.max_auto_tasks,
            max_fix_attempts: s.max_fix_attempts,
            ntfy_url: s.ntfy_url.clone(),
        }
    }
}

/// OnEmptyQueue actions accepted by `PUT /api/settings`.
pub const ONEMPTY_ACTIONS: [&str; 3] = ["review", "research", "improve"];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Budget {
    pub limit_5h_tokens: Option<i64>,
    pub limit_7d_tokens: Option<i64>,
    pub limit_30d_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Window {
    pub used_5h: i64,
    pub used_7d: i64,
    pub cost_30d: f64,
    pub cache_read_5h: i64,
    pub runs_5h: i64,
    pub oldest_5h: Option<DateTime<Utc>>,
    pub oldest_7d: Option<DateTime<Utc>>,
    pub oldest_30d: Option<DateTime<Utc>>,
}

/// `GET /api/limits` (one per provider).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderStatus {
    pub provider: String,
    #[serde(flatten)]
    pub window: Window,
    #[serde(flatten)]
    pub budget: Budget,
    /// Subscription limit reported by the CLI.
    pub limited_until: Option<DateTime<Utc>>,
    /// Own budget exhausted until (approx., rolling window).
    pub budget_until: Option<DateTime<Utc>>,
    pub budget_reason: Option<String>,
    pub observed_limit_tokens: Option<i64>,
    pub observed_at: Option<DateTime<Utc>>,
    /// Paused by an authentication/billing failure until credentials are
    /// updated or the user resumes the provider.
    pub paused_reason: Option<String>,
    pub blocked: bool,
}

/// `GET /api/credentials` – secrets are write-only and never returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialInfo {
    pub provider: String,
    pub kind: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutCredential {
    pub kind: String,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushKey {
    pub public_key: String,
}

// ---------------------------------------------------------------------------
// Notifications, live events
// ---------------------------------------------------------------------------

/// `GET /api/notifications`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub task_id: Option<Uuid>,
    pub read_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// A new notification as pushed over SSE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationPush {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub task_id: Option<Uuid>,
    /// Also sent to phones (ntfy / Web Push): needs the user's attention.
    pub push: bool,
}

/// Messages on `GET /api/events` (SSE `data:` lines).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A task was created or changed.
    Task {
        task: Box<Task>,
    },
    /// A new log line of a task.
    TaskEvent {
        task_id: Uuid,
        event: TaskEvent,
    },
    Notification {
        notification: NotificationPush,
    },
    /// Events were dropped: refetch everything.
    Resync,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task_json() -> Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "user_id": "00000000-0000-0000-0000-000000000002",
            "project_id": "00000000-0000-0000-0000-000000000003",
            "parent_id": null, "kind": "task", "title": "t", "prompt": "p", "context": "",
            "provider": "mock", "model": null, "status": "waiting_input", "position": 3,
            "attempt": 0, "depth": 0, "created_by_agent": false,
            "questions": ["Postgres or SQLite?"], "handoff": null, "summary": null, "error": null,
            "review": {"approved": false, "issues": ["x"]},
            "usage": {"input_tokens": 1, "output_tokens": 2, "cache_read_tokens": 3, "cache_write_tokens": 4, "cost_usd": 0.5},
            "snapshot_before": "a", "snapshot_after": "b",
            "created_at": "2026-01-01T10:00:00Z", "started_at": null, "finished_at": null,
            "updated_at": "2026-01-01T10:00:00Z", "max_runtime_secs": 60, "max_cost_usd": null,
            "max_tokens": null, "max_turns": 3, "failure_class": "limit_tokens",
            "transient_retries": 0, "not_before": null,
            "connector_id": null, "escalation_level": 0, "plan": null, "cost": null,
            "gate": null, "allow_test_changes": false
        })
    }

    #[test]
    fn task_roundtrip_and_accessors() {
        let t: Task = serde_json::from_value(task_json()).unwrap();
        let mut full = task_json();
        for k in [
            "branch",
            "merge_base",
            "merge_state",
            "merge_queued_at",
            "carry_from",
            "pr_id",
            "plan_state",
            "plan_feedback",
            "plan_meta",
            "agent_session",
            "session_from",
            "trigger_id",
        ] {
            full[k] = Value::Null;
        }
        full["plan_mode"] = Value::Bool(false);
        full["plan_revision"] = 0.into();
        full["steer_rounds"] = 0.into();
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            full,
            "rows from before WP04/WP05/WP06/WP07 still parse"
        );
        assert_eq!(serde_json::from_value::<Task>(full).unwrap(), t);
        // Before WP04: diff from the first snapshot, always on the target.
        assert_eq!(t.diff_base(), Some("a"));
        assert!(t.on_target() && t.merge_state_kind().is_none());
        // Branch task: diff from the merge base; revertable once merged.
        let mut b = t.clone();
        b.branch = Some("queue/0123456789ab".into());
        b.merge_base = Some("m".into());
        b.merge_state = Some("queued".into());
        assert_eq!(b.diff_base(), Some("m"));
        assert!(!b.on_target() && b.has_changes());
        b.merge_state = Some("merged".into());
        assert!(b.on_target());
        assert_eq!(b.merge_state_kind(), Some(branch::MergeState::Merged));
        b.merge_base = Some("b".into());
        assert!(!b.has_changes());
        assert_eq!(t.status_kind(), TaskStatus::WaitingInput);
        assert_eq!(t.failure(), Some(FailureClass::LimitTokens));
        assert_eq!(t.questions_list(), vec!["Postgres or SQLite?"]);
        assert_eq!(
            t.review_result(),
            Some(ReviewResult {
                approved: false,
                issues: vec!["x".into()]
            })
        );
        assert_eq!(t.usage_parsed().unwrap().cache_write_tokens, 4);
        assert!(t.has_changes());
    }

    #[test]
    fn status_and_failure_names_roundtrip() {
        for s in TaskStatus::ALL {
            assert_eq!(TaskStatus::parse(s.as_str()), Some(s));
            assert_eq!(serde_json::to_value(s).unwrap(), json!(s.as_str()));
        }
        for c in FailureClass::ALL {
            assert_eq!(FailureClass::parse(c.as_str()), Some(c));
        }
        assert!(TaskStatus::Blocked.is_final() && !TaskStatus::Running.is_final());
        assert!(TaskStatus::PendingApproval.needs_user());
        assert!(TaskStatus::WaitingInput.can_cancel() && !TaskStatus::Done.can_cancel());
        assert!(TaskStatus::Merging.can_cancel() && !TaskStatus::Merging.is_final());
        assert!(!TaskStatus::Merging.needs_user() && !TaskStatus::Merging.is_editable());
        assert!(TaskStatus::Cancelled.can_retry() && !TaskStatus::Queued.can_retry());
    }

    #[test]
    fn patch_bodies_distinguish_absent_and_null() {
        let empty = serde_json::to_value(UpdateProject::default()).unwrap();
        assert_eq!(empty, json!({}));
        let u = UpdateProject {
            max_tokens: Some(None),
            max_turns: Some(Some(5)),
            name: Some("n".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v, json!({"name": "n", "max_tokens": null, "max_turns": 5}));
        let back: UpdateProject = serde_json::from_value(v).unwrap();
        assert_eq!(back.max_tokens, Some(None));
        assert_eq!(back.max_turns, Some(Some(5)));
        assert_eq!(back.max_cost_usd, None);

        let c = CreateTask {
            project_id: Uuid::nil(),
            title: None,
            prompt: "p".into(),
            context: String::new(),
            provider: None,
            model: None,
            parent_id: None,
            connector_id: None,
            front: true,
            allow_test_changes: false,
            plan_mode: None,
            limits: TaskLimitFields {
                max_runtime_secs: Some(Some(90)),
                ..Default::default()
            },
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(
            v,
            json!({"project_id": Uuid::nil(), "prompt": "p", "context": "", "front": true, "max_runtime_secs": 90})
        );
        let back: CreateTask = serde_json::from_value(v).unwrap();
        assert_eq!(back.limits.max_runtime_secs, Some(Some(90)));
        let t: UpdateTask = serde_json::from_value(json!({"max_cost_usd": null})).unwrap();
        assert_eq!(t.limits.max_cost_usd, Some(None));
        assert_eq!(t.limits.max_turns, None);
    }

    #[test]
    fn server_events_have_the_documented_shape() {
        let t: Task = serde_json::from_value(task_json()).unwrap();
        let v = serde_json::to_value(ServerEvent::Task { task: Box::new(t) }).unwrap();
        assert_eq!(v["type"], "task");
        assert_eq!(v["task"]["title"], "t");
        assert_eq!(
            serde_json::to_value(ServerEvent::Resync).unwrap(),
            json!({"type": "resync"})
        );
        let raw = json!({"type": "task_event", "task_id": Uuid::nil(), "event": {
            "id": 7, "task_id": Uuid::nil(), "ts": "2026-01-01T10:00:00Z", "kind": "tool", "content": "ls"
        }});
        let e: ServerEvent = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(&e).unwrap(), raw);
        let n = json!({"type": "notification", "notification": {
            "id": Uuid::nil(), "kind": "blocked", "title": "Blockiert", "body": "", "task_id": null, "push": true
        }});
        assert!(matches!(
            serde_json::from_value::<ServerEvent>(n).unwrap(),
            ServerEvent::Notification { .. }
        ));
    }

    #[test]
    fn provider_status_is_flat() {
        let s = ProviderStatus {
            provider: "claude".into(),
            window: Window {
                used_5h: 5,
                ..Default::default()
            },
            budget: Budget {
                limit_5h_tokens: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["used_5h"], 5);
        assert_eq!(v["limit_5h_tokens"], 10);
        assert!(v.get("window").is_none());
        assert_eq!(serde_json::from_value::<ProviderStatus>(v).unwrap(), s);
    }

    #[test]
    fn settings_update_ignores_server_fields() {
        let v = json!({"user_id": Uuid::nil(), "parallelism": 2, "default_provider": "mock",
            "onempty_enabled": false, "onempty_actions": ["review"], "onempty_cooldown_minutes": 300,
            "max_auto_tasks": 2, "max_fix_attempts": 2, "ntfy_url": null, "last_onempty_at": null});
        let s: Settings = serde_json::from_value(v.clone()).unwrap();
        let u: SettingsUpdate = serde_json::from_value(v).unwrap();
        assert_eq!(SettingsUpdate::from(&s), u);
        assert_eq!(
            ProjectRole::parse("viewer").map(ProjectRole::can_edit),
            Some(false)
        );
    }
}
