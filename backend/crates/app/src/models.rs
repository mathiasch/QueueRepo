use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

// Wire types shared with the web frontend live in `queued-api`.
pub use queued_api::{
    FailureClass, ProjectRole, ProjectView, Residency, ReviewResult, Routing, Settings, Task,
    TaskEvent, Usage, User,
};

pub mod status {
    pub const PENDING_APPROVAL: &str = "pending_approval";
    pub const QUEUED: &str = "queued";
    pub const RUNNING: &str = "running";
    pub const WAITING_BATCH: &str = "waiting_batch";
    pub const WAITING_INPUT: &str = "waiting_input";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    pub const BLOCKED: &str = "blocked";
    pub const CANCELLED: &str = "cancelled";
}

/// Database row of a project. Internal: the API returns [`ProjectView`].
#[derive(Debug, Clone, FromRow)]
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
    pub git_token_enc: Option<Vec<u8>>,
    pub auto_push: bool,
    pub max_runtime_secs: Option<i32>,
    pub stall_timeout_secs: Option<i32>,
    pub max_cost_usd: Option<f64>,
    pub max_tokens: Option<i64>,
    pub max_turns: Option<i32>,
    /// [`Routing`] as JSON (`None` = legacy behaviour).
    pub routing: Option<serde_json::Value>,
    /// any | eu | ch (CHECK in the database).
    pub data_residency: String,
    /// Project created by an eval run (hidden from the project list).
    pub is_eval: bool,
}

impl Project {
    pub fn routing(&self) -> Option<Routing> {
        self.routing
            .as_ref()
            .and_then(|r| serde_json::from_value(r.clone()).ok())
    }

    pub fn residency(&self) -> Residency {
        Residency::parse(&self.data_residency).unwrap_or_default()
    }

    /// Public projection: the encrypted git token never leaves the backend.
    pub fn view(&self, role: ProjectRole) -> ProjectView {
        ProjectView {
            id: self.id,
            owner_id: self.owner_id,
            name: self.name.clone(),
            description: self.description.clone(),
            goal: self.goal.clone(),
            verify_command: self.verify_command.clone(),
            review_enabled: self.review_enabled,
            review_provider: self.review_provider.clone(),
            review_model: self.review_model.clone(),
            created_at: self.created_at,
            git_remote_url: self.git_remote_url.clone(),
            git_branch: self.git_branch.clone(),
            auto_push: self.auto_push,
            max_runtime_secs: self.max_runtime_secs,
            stall_timeout_secs: self.stall_timeout_secs,
            max_cost_usd: self.max_cost_usd,
            max_tokens: self.max_tokens,
            max_turns: self.max_turns,
            role,
            has_git_token: self.git_token_enc.is_some(),
            routing: self.routing(),
            data_residency: self.residency(),
        }
    }
}

/// JSON schema of [`AgentResult`] for CLIs that enforce structured output
/// (Claude `--json-schema`, Codex `--output-schema`). Strict: every property
/// is required and no others are allowed (OpenAI structured-output rules).
pub const RESULT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["status","summary","handoff","questions","follow_up_tasks"],"properties":{"status":{"type":"string","enum":["done","failed","needs_input"]},"summary":{"type":"string","description":"max 5 lines, what changed"},"handoff":{"type":"string","description":"max 15 lines for the next agent: current state, decisions, open points, relevant files"},"questions":{"type":"array","items":{"type":"string"}},"follow_up_tasks":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["title","prompt"],"properties":{"title":{"type":"string"},"prompt":{"type":"string"}}}}}}"#;

/// JSON schema of [`ReviewResult`] (strict, see [`RESULT_SCHEMA`]).
pub const REVIEW_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["approved","issues"],"properties":{"approved":{"type":"boolean"},"issues":{"type":"array","items":{"type":"string"}}}}"#;

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
    use serde_json::Value;

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
