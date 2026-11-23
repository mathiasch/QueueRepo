//! Connectors, data residency, per-step routing, CLI status and evals (WP13).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{Money, Usage};

/// Hard data-location rule of a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Residency {
    /// No restriction.
    #[default]
    Any,
    /// EU or Switzerland only.
    Eu,
    /// Switzerland only.
    Ch,
}

impl Residency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Eu => "eu",
            Self::Ch => "ch",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Any, Self::Eu, Self::Ch]
            .into_iter()
            .find(|r| r.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "Keine Einschränkung",
            Self::Eu => "Nur EU/Schweiz",
            Self::Ch => "Nur Schweiz",
        }
    }

    /// Does a connector at `loc` satisfy this rule?
    pub fn allows(self, loc: DataLocation) -> bool {
        match self {
            Self::Any => true,
            Self::Eu => matches!(
                loc,
                DataLocation::Eu | DataLocation::Ch | DataLocation::Local
            ),
            Self::Ch => matches!(loc, DataLocation::Ch | DataLocation::Local),
        }
    }
}

/// Where prompts and code are processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataLocation {
    Us,
    /// Global routing of the provider (location not guaranteed).
    Global,
    Eu,
    Ch,
    /// Test provider on this machine.
    Local,
    /// Not known (self-hosted endpoint without declaration).
    Unknown,
}

impl DataLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Us => "us",
            Self::Global => "global",
            Self::Eu => "eu",
            Self::Ch => "ch",
            Self::Local => "local",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Us,
            Self::Global,
            Self::Eu,
            Self::Ch,
            Self::Local,
            Self::Unknown,
        ]
        .into_iter()
        .find(|l| l.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Us => "USA",
            Self::Global => "Weltweit",
            Self::Eu => "EU",
            Self::Ch => "Schweiz",
            Self::Local => "Lokal",
            Self::Unknown => "Unbekannt",
        }
    }
}

/// One configuration field of a connector type (non-secret).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    /// text | url | select | bool
    pub input: String,
    #[serde(default)]
    pub options: Vec<FieldOption>,
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
}

/// Static description of a connector type (`GET /api/meta`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorTypeInfo {
    pub kind: String,
    pub label: String,
    pub description: String,
    /// Group in the UI: claude | openai | google | eu_ch | open
    pub group: String,
    /// CLI that runs the agent loop (claude | codex | gemini | opencode | mock),
    /// `None` when the user picks it (LiteLLM).
    #[serde(default)]
    pub harness: Option<String>,
    /// Label of the secret input (`None` = no secret).
    #[serde(default)]
    pub secret_label: Option<String>,
    #[serde(default)]
    pub secret_help: String,
    /// Secret is a JSON document (validated on save).
    #[serde(default)]
    pub secret_json: bool,
    #[serde(default)]
    pub secret_optional: bool,
    pub fields: Vec<ConnectorField>,
    /// Data location without configuration (the config can change it).
    pub location: DataLocation,
    /// Single-shot steps (plan/review/analysis) possible: synchronous / batch.
    pub single_shot: bool,
    pub batch: bool,
    /// OpenAI Flex tier available.
    pub flex: bool,
    /// Example model ids for the input field.
    #[serde(default)]
    pub models: Vec<String>,
    /// Facts that could not be verified (endpoints, formats).
    #[serde(default)]
    pub unverified: Vec<String>,
    /// Hint shown to the user (e.g. terms of service of subscriptions).
    #[serde(default)]
    pub note: String,
}

/// `GET /api/connectors` – the secret is write-only and never returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorView {
    pub id: Uuid,
    pub user_id: Uuid,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    pub kind: String,
    pub label: String,
    pub config: Value,
    pub has_secret: bool,
    pub harness: String,
    pub location: DataLocation,
    /// The location was declared by the user (self-hosted, gateway).
    pub location_declared: bool,
    pub single_shot: bool,
    pub batch: bool,
    /// The caller may edit/delete it (owner).
    pub can_manage: bool,
    #[serde(default)]
    pub paused_reason: Option<String>,
    #[serde(default)]
    pub limited_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /api/connectors`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NewConnector {
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub secret: Option<String>,
}

/// `PATCH /api/connectors/{id}` – absent fields stay unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UpdateConnector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    /// New secret; absent keeps the stored one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// `POST /api/connectors/{id}/check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorCheck {
    pub ok: bool,
    pub message: String,
}

/// How a pipeline step runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepMode {
    /// Step disabled (plan only).
    Off,
    /// Agentic run through the CLI (review/analysis as today).
    #[default]
    Harness,
    /// One direct API call, answer awaited.
    Sync,
    /// Provider batch API (−50 %, up to 24 h), polled.
    Batch,
}

impl StepMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Harness => "harness",
            Self::Sync => "sync",
            Self::Batch => "batch",
        }
    }
}

/// Model and access of one step. `connector_id` wins over `provider`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Target {
    #[serde(default)]
    pub connector_id: Option<Uuid>,
    /// Legacy provider (credential in Einstellungen → Zugänge).
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StepRoute {
    #[serde(default)]
    pub mode: StepMode,
    /// `None` = same access/model as the agent loop.
    #[serde(default)]
    pub target: Option<Target>,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            preset: None,
            plan: StepRoute::off(),
            execute: ExecuteRoute::default(),
            review: StepRoute::default(),
            analyze: StepRoute::default(),
            rules: vec![],
        }
    }
}

impl StepRoute {
    pub fn off() -> Self {
        Self {
            mode: StepMode::Off,
            target: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExecuteRoute {
    #[serde(default)]
    pub target: Option<Target>,
    /// Models for the next fix attempts (same access), e.g. Sonnet, Opus.
    #[serde(default)]
    pub escalation: Vec<String>,
    /// OpenAI Flex service tier for the agent loop (−50 %, slower, 429 possible).
    #[serde(default)]
    pub flex: bool,
}

/// `projects.routing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    /// Preset this routing came from (`None`/"custom" = hand-made).
    #[serde(default)]
    pub preset: Option<String>,
    /// Plan step: off | sync | batch (no agentic planning).
    #[serde(default = "StepRoute::off")]
    pub plan: StepRoute,
    #[serde(default)]
    pub execute: ExecuteRoute,
    #[serde(default)]
    pub review: StepRoute,
    #[serde(default)]
    pub analyze: StepRoute,
    /// WP12: rules for the agent loop by task features (first match wins),
    /// applied when the task names no access itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RoutingRule>,
}

/// Most rules per project.
pub const MAX_RULES: usize = 20;

/// WP12: one routing rule – when the task matches `when`, its agent loop
/// runs on `target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RoutingRule {
    /// Shown in the explanation at the task ("Regel „Kleine Aufträge“").
    pub name: String,
    #[serde(default)]
    pub when: RuleWhen,
    pub target: Target,
}

/// Conditions of a [`RoutingRule`]; all given conditions must hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RuleWhen {
    /// Task kinds (`task`, `fix`); empty = new tasks (`task`) only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Prompt + context at most this many characters ("small task").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<u32>,
    /// Prompt + context at least this many characters ("large task").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_chars: Option<u32>,
    /// Any of these words in title or prompt (case-insensitive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// With (`true`) or without (`false`) attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_attachments: Option<bool>,
}

/// Features of a task the routing decides on (deterministic, no model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskFacts {
    /// `task` | `fix` | `onempty` | `attempt` | `continue`.
    pub kind: String,
    /// Characters of prompt + context.
    pub chars: u32,
    #[serde(default)]
    pub attachments: u32,
}

/// Where a [`RoutingDecision`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// Chosen at the task (composer, API, template).
    Explicit,
    /// Best-of-N: the access/model of this attempt.
    Attempt,
    /// Fix task: next model of the escalation chain.
    Escalation,
    /// A routing rule matched.
    Rule,
    /// The project's agent-loop route (preset or hand-made).
    Route,
    /// Fix task without escalation: same access/model as the failed task.
    Inherited,
    /// No routing: the user's default provider.
    Default,
}

/// WP12: the routing decision of a task's agent loop, stored at the task
/// and shown in the UI. WP15 (model chooser) produces the same structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingDecision {
    /// Pipeline step (`execute`).
    pub step: String,
    pub source: DecisionSource,
    pub target: Target,
    /// Name of the matched rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// Position of the matched rule (0-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_index: Option<u32>,
    /// Escalation level after this decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_level: Option<i32>,
    /// German explanation for the UI.
    pub reason: String,
    pub facts: TaskFacts,
}

/// `PUT /api/projects/{id}/routing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingUpdate {
    /// `None` = back to the legacy behaviour.
    #[serde(default)]
    pub routing: Option<Routing>,
    pub data_residency: Residency,
}

/// `GET /api/projects/{id}/presets`: presets resolved against the caller's connectors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub available: bool,
    /// Why not available (missing access), German.
    #[serde(default)]
    pub missing: Option<String>,
    #[serde(default)]
    pub routing: Option<Routing>,
    /// Residency the preset requires (EU preset).
    pub residency: Residency,
    /// Estimated cost per attempt of an M task (report profile).
    #[serde(default)]
    pub estimate: Vec<Money>,
}

/// Maintenance state of a pinned CLI (static research result).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliNote {
    /// active | frozen
    pub state: String,
    pub note: String,
    pub checked: String,
}

/// `GET /api/cli-status`: what the runner found in the agent sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliStatus {
    pub cli: String,
    #[serde(default)]
    pub pinned: Option<String>,
    /// `--version` output (first line), `None` = not installed / failed.
    #[serde(default)]
    pub installed: Option<String>,
    /// ok | missing | version_mismatch | replaced | unexpected
    pub state: String,
    pub note: String,
}

/// One case of the eval set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalCaseInfo {
    pub id: String,
    pub title: String,
    pub category: String,
    /// S | M | L
    pub size: String,
}

/// `POST /api/evals`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StartEval {
    #[serde(default)]
    pub connector_id: Option<Uuid>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Subset of case ids (empty = all).
    #[serde(default)]
    pub cases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalResultView {
    pub case_id: String,
    #[serde(default)]
    pub task_id: Option<Uuid>,
    /// Task status (queued … done/failed).
    pub status: String,
    pub success: bool,
    pub usage: Usage,
    pub cost: Vec<Money>,
    #[serde(default)]
    pub duration_secs: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalSummary {
    pub cases: usize,
    pub finished: usize,
    pub succeeded: usize,
    pub success_rate: Option<f64>,
    pub cost: Vec<Money>,
    /// Cost per successful case (`None` without success).
    pub cost_per_success: Vec<Money>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalRunView {
    pub id: Uuid,
    pub target: StartEval,
    /// Readable target ("Claude Sonnet 5.5 über Anthropic API").
    pub target_label: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    pub results: Vec<EvalResultView>,
    pub summary: EvalSummary,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residency_rules() {
        use DataLocation::*;
        assert!(Residency::Any.allows(Us));
        assert!(Residency::Eu.allows(Eu) && Residency::Eu.allows(Ch));
        assert!(!Residency::Eu.allows(Us) && !Residency::Eu.allows(Global));
        assert!(!Residency::Eu.allows(Unknown));
        assert!(Residency::Ch.allows(Ch) && !Residency::Ch.allows(Eu));
        assert!(Residency::Ch.allows(Local) && Residency::Eu.allows(Local));
        for r in [Residency::Any, Residency::Eu, Residency::Ch] {
            assert_eq!(Residency::parse(r.as_str()), Some(r));
        }
        for l in [Us, Global, Eu, Ch, Local, Unknown] {
            assert_eq!(DataLocation::parse(l.as_str()), Some(l));
        }
    }

    #[test]
    fn routing_defaults_from_empty_json() {
        let r: Routing = serde_json::from_str("{}").unwrap();
        assert_eq!(r.plan.mode, StepMode::Off);
        assert_eq!(r.review.mode, StepMode::Harness);
        assert_eq!(r, Routing::default());
        assert!(r.execute.escalation.is_empty());
        let full = Routing {
            preset: Some("balanced".into()),
            plan: StepRoute {
                mode: StepMode::Batch,
                target: Some(Target {
                    connector_id: Some(Uuid::nil()),
                    provider: None,
                    model: Some("claude-opus-5-5".into()),
                }),
            },
            ..Default::default()
        };
        let back: Routing = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
        assert_eq!(back, full);
    }
}
