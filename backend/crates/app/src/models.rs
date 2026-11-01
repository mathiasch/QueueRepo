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
}

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

/// Contract between queue and agent: written by the agent to `.queue/result.json`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentResult {
    pub status: Option<String>,
    pub summary: Option<String>,
    pub handoff: Option<String>,
    pub questions: Vec<String>,
    pub follow_up_tasks: Vec<FollowUp>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
