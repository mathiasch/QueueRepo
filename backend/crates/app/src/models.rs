use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Task {
    pub id: Uuid,
    pub user_id: Uuid,
    pub project_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub kind: String,
    pub title: String,
    pub prompt: String,
    pub context: String,
    pub provider: String,
    pub model: Option<String>,
    pub status: String,
    pub position: i64,
    pub attempt: i32,
    pub depth: i32,
    pub created_by_agent: bool,
    pub questions: Option<Value>,
    pub handoff: Option<String>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub review: Option<Value>,
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
}

pub mod status {
    pub const PENDING_APPROVAL: &str = "pending_approval";
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const WAITING_INPUT: &str = "waiting_input";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    pub const BLOCKED: &str = "blocked";
    pub const CANCELLED: &str = "cancelled";
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Project {
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
    #[serde(skip_serializing)]
    pub git_token_enc: Option<Vec<u8>>,
    pub auto_push: bool,
    pub max_runtime_secs: Option<i32>,
    pub stall_timeout_secs: Option<i32>,
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<i64>,
    pub max_turns: Option<i32>,
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
}

impl FailureClass {
    pub const ALL: [FailureClass; 12] = [
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

/// JSON schema of [`AgentResult`] for CLIs that enforce structured output
/// (Claude `--json-schema`, Codex `--output-schema`). Strict: every property
/// is required and no others are allowed (OpenAI structured-output rules).
pub const RESULT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["status","summary","handoff","questions","follow_up_tasks"],"properties":{"status":{"type":"string","enum":["done","failed","needs_input"]},"summary":{"type":"string","description":"max 5 lines, what changed"},"handoff":{"type":"string","description":"max 15 lines for the next agent: current state, decisions, open points, relevant files"},"questions":{"type":"array","items":{"type":"string"}},"follow_up_tasks":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["title","prompt"],"properties":{"title":{"type":"string"},"prompt":{"type":"string"}}}}}}"#;

/// JSON schema of [`ReviewResult`] (strict, see [`RESULT_SCHEMA`]).
pub const REVIEW_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["approved","issues"],"properties":{"approved":{"type":"boolean"},"issues":{"type":"array","items":{"type":"string"}}}}"#;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Settings {
    #[serde(skip_deserializing)]
    pub user_id: Uuid,
    pub parallelism: i32,
    pub default_provider: String,
    pub onempty_enabled: bool,
    pub onempty_actions: Vec<String>,
    pub onempty_cooldown_minutes: i32,
    pub max_auto_tasks: i32,
    pub max_fix_attempts: i32,
    pub ntfy_url: Option<String>,
    #[serde(skip_deserializing)]
    pub last_onempty_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TaskEvent {
    pub id: i64,
    pub task_id: Uuid,
    pub ts: DateTime<Utc>,
    pub kind: String,
    pub content: String,
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
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
        self.cost_usd += o.cost_usd;
    }
}

/// Contract between queue and agent: structured output of the CLI or
/// `.queue/result.json` written by the agent.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentResult {
    pub status: Option<String>,
    pub summary: Option<String>,
    pub handoff: Option<String>,
    pub questions: Vec<String>,
    pub follow_up_tasks: Vec<FollowUp>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct FollowUp {
    pub title: String,
    pub prompt: String,
}

/// Written by the reviewer agent to `.queue/review.json`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ReviewResult {
    pub approved: bool,
    pub issues: Vec<String>,
}

impl AgentResult {
    /// A contract counts only with a known status.
    pub fn has_valid_status(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("done" | "failed" | "needs_input")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strictness required by OpenAI structured outputs, checked recursively.
    fn assert_strict(v: &Value, path: &str) {
        if v["type"] == "object" {
            assert_eq!(
                v["additionalProperties"], false,
                "{path}: additionalProperties"
            );
            let props = v["properties"].as_object().expect("properties");
            let mut keys: Vec<&String> = props.keys().collect();
            let mut req: Vec<&str> = v["required"]
                .as_array()
                .expect("required")
                .iter()
                .map(|r| r.as_str().unwrap())
                .collect();
            keys.sort();
            req.sort();
            assert_eq!(keys, req, "{path}: every property must be required");
            for (k, p) in props {
                assert_strict(p, &format!("{path}.{k}"));
            }
        }
        if v["type"] == "array" {
            assert_strict(&v["items"], &format!("{path}[]"));
        }
    }

    #[test]
    fn schemas_are_strict_and_match_structs() {
        for s in [RESULT_SCHEMA, REVIEW_SCHEMA] {
            let v: Value = serde_json::from_str(s).expect("valid JSON");
            assert_strict(&v, "$");
        }
        let example = serde_json::json!({
            "status": "needs_input", "summary": "s", "handoff": "h",
            "questions": ["q?"], "follow_up_tasks": [{"title": "t", "prompt": "p"}]
        });
        let r: AgentResult = serde_json::from_value(example).unwrap();
        assert!(r.has_valid_status());
        assert_eq!(r.questions, vec!["q?"]);
        assert_eq!(r.follow_up_tasks[0].prompt, "p");
        let rv: ReviewResult =
            serde_json::from_value(serde_json::json!({"approved": false, "issues": ["x"]}))
                .unwrap();
        assert!(!rv.approved);
        let schema: Value = serde_json::from_str(RESULT_SCHEMA).unwrap();
        assert_eq!(
            schema["properties"]["status"]["enum"],
            serde_json::json!(["done", "failed", "needs_input"])
        );
    }

    #[test]
    fn failure_class_roundtrip_matches_db_check() {
        let sql = include_str!("../migrations/0003_cli_contract_limits.sql");
        for c in FailureClass::ALL {
            assert_eq!(FailureClass::parse(c.as_str()), Some(c));
            assert!(
                sql.contains(&format!("'{}'", c.as_str())),
                "{c:?} missing in CHECK"
            );
        }
        assert_eq!(FailureClass::parse("nope"), None);
        assert!(FailureClass::Stalled.is_transient());
        assert!(!FailureClass::Task.is_transient());
        assert!(!FailureClass::RateLimit.is_transient());
    }

    #[test]
    fn contract_status_must_be_known() {
        assert!(!AgentResult::default().has_valid_status());
        let r = AgentResult {
            status: Some("maybe".into()),
            ..Default::default()
        };
        assert!(!r.has_valid_status());
    }
}
