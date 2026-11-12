//! WP06: plan mode, steering messages, "Weiter mit …" and inline diff
//! comments – wire types, limits and the German texts of the UI (shared by
//! backend and frontend).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Longest steering message / "Weiter mit …" text (characters).
pub const MAX_MESSAGE_CHARS: usize = 4_000;
/// Longest inline comment (characters).
pub const MAX_COMMENT_CHARS: usize = 2_000;
/// Longest plan (agent output or edited by a human, characters).
pub const MAX_PLAN_CHARS: usize = 20_000;
/// Longest revision note for a plan (characters).
pub const MAX_NOTE_CHARS: usize = 4_000;
/// Messages waiting for delivery per task.
pub const MAX_QUEUED_MESSAGES: i64 = 20;
/// Draft comments per user and task.
pub const MAX_COMMENTS: i64 = 100;
/// Lines one comment may span.
pub const MAX_COMMENT_LINES: i32 = 200;
/// Resumes for user messages per task (cost guard).
pub const MAX_STEER_ROUNDS: i32 = 10;
/// Revisions of a plan a human may ask for.
pub const MAX_PLAN_REVISIONS: i32 = 10;
/// Choices for the automatic plan approval (minutes), `None` = off.
pub const AUTO_APPROVE_CHOICES: [(Option<i32>, &str); 5] = [
    (None, "Nie (Standard)"),
    (Some(15), "nach 15 Minuten"),
    (Some(60), "nach 1 Stunde"),
    (Some(240), "nach 4 Stunden"),
    (Some(1440), "nach 24 Stunden"),
];

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Non-empty text of at most `max` characters (trimmed).
pub fn validate_text(what: &str, s: &str, max: usize) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(format!("{what} darf nicht leer sein"));
    }
    if chars(t) > max {
        return Err(format!(
            "{what} ist zu lang ({} Zeichen, höchstens {max})",
            chars(t)
        ));
    }
    if t.chars().any(|c| c == '\0') {
        return Err(format!("{what} enthält ungültige Zeichen"));
    }
    Ok(t.to_string())
}

/// Automatic approval: 5 minutes to 7 days.
pub fn validate_auto_approve(m: Option<i32>) -> Result<Option<i32>, String> {
    match m {
        None => Ok(None),
        Some(v) if (5..=10_080).contains(&v) => Ok(Some(v)),
        Some(_) => Err("Automatische Genehmigung: 5 Minuten bis 7 Tage".into()),
    }
}

pub fn auto_approve_label(m: Option<i32>) -> String {
    AUTO_APPROVE_CHOICES
        .iter()
        .find(|(v, _)| *v == m)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| match m {
            Some(v) => format!("nach {v} Minuten"),
            None => "Nie".into(),
        })
}

pub const PLAN_MODE_HELP: &str = "Der Agent schaut sich das Projekt zuerst nur an und schreibt einen Plan. Er ändert dabei nichts – Queue lässt ihn in einer schreibgeschützten Wegwerf-Kopie arbeiten und prüft danach jede Datei. Du liest den Plan, kannst ihn ändern oder verwerfen. Erst nach deiner Freigabe setzt der Agent ihn um.";
pub const AUTO_APPROVE_HELP: &str = "Wenn niemand reagiert, gilt der Plan nach dieser Zeit als genehmigt. Nie bei offenen Fragen im Plan oder wenn der Agent versucht hat, etwas zu ändern.";
pub const MESSAGE_HELP: &str = "Deine Nachricht wartet, bis der Agent eine Pause macht (Ende seines aktuellen Schritts), und wird dann in dieselbe Sitzung gegeben. Bis dahin kannst du sie zurückziehen. „Sofort zustellen“ unterbricht den Agenten kurz.";
pub const CONTINUE_HELP: &str = "Startet einen neuen Auftrag, der genau hier weitermacht: mit dem Stand dieses Auftrags und – wenn möglich – derselben Sitzung des Agenten.";
pub const COMMENTS_HELP: &str = "Tippe auf eine Zeile, um sie auszuwählen, und auf eine zweite für einen Bereich. Deine Kommentare werden gesammelt und zusammen als eine Nachricht an den Agenten geschickt.";

/// `tasks.plan_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    /// A plan run or plan step is in progress.
    Planning,
    /// The plan waits for a human (task `held`, gate stage `plan`).
    Ready,
    /// Approved: the task implements it.
    Approved,
    /// Discarded by a human (task cancelled).
    Rejected,
}

impl PlanState {
    pub const ALL: [PlanState; 4] = [Self::Planning, Self::Ready, Self::Approved, Self::Rejected];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Ready => "ready",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Planning => "Plan wird erstellt",
            Self::Ready => "Plan wartet auf dich",
            Self::Approved => "Plan genehmigt",
            Self::Rejected => "Plan verworfen",
        }
    }
}

/// Where a plan came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    /// Agentic plan run (CLI in plan mode, read-only throwaway working tree).
    #[default]
    Agent,
    /// Single-shot planning step (direct API call or batch, WP13).
    SingleShot,
}

/// `tasks.plan_meta`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanMeta {
    #[serde(default)]
    pub source: PlanSource,
    #[serde(default)]
    pub questions: Vec<String>,
    /// Paths the plan run changed anyway (all discarded).
    #[serde(default)]
    pub attempted_changes: Vec<String>,
    #[serde(default)]
    pub attempted_total: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// A human edited the text before approving.
    #[serde(default)]
    pub edited: bool,
    /// Approved by whom (name) / automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub auto_approved: bool,
    /// When the plan is approved automatically (if the project allows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve_at: Option<DateTime<Utc>>,
}

impl PlanMeta {
    /// Automatic approval never applies to plans with open questions or
    /// attempted changes.
    pub fn may_auto_approve(&self) -> bool {
        self.questions.is_empty() && self.attempted_total == 0
    }
}

/// `tasks.agent_session`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Harness (claude, codex, gemini, opencode, mock).
    pub provider: String,
    pub id: String,
}

/// What the CLIs offer for plan mode and resume, with the evidence level
/// (`docs/roadmap/WP06-plan.md` §0). Shown in the UI, used by the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliSessionSupport {
    pub provider: &'static str,
    /// The CLI's own read-only/plan switch.
    pub plan_flag: &'static str,
    /// Resume of the run's own session (steering, "Weiter mit …").
    pub resume: bool,
    /// How it is known to work.
    pub evidence: &'static str,
    pub verified: bool,
}

pub const CLI_SESSION_SUPPORT: [CliSessionSupport; 5] = [
    CliSessionSupport {
        provider: "claude",
        plan_flag: "--permission-mode plan",
        resume: true,
        evidence: "Live geprüft mit Claude Code 2.1.322: Plan-Modus ändert keine Datei, Fortsetzen mit --resume nach gelöschtem Home-Verzeichnis",
        verified: true,
    },
    CliSessionSupport {
        provider: "codex",
        plan_flag: "--sandbox read-only",
        resume: true,
        evidence: "Nur aus dem Programm 0.167.0 gelesen (codex exec resume <id>), nicht ausgeführt",
        verified: false,
    },
    CliSessionSupport {
        provider: "gemini",
        plan_flag: "--approval-mode plan",
        resume: true,
        evidence: "Nur aus dem Quelltext 0.67.0 gelesen (--session-id, --resume <id>), nicht ausgeführt",
        verified: false,
    },
    CliSessionSupport {
        provider: "opencode",
        plan_flag: "--agent plan",
        resume: true,
        evidence: "Nur aus dem Programm 1.19.5 gelesen (--session <id>), nicht ausgeführt",
        verified: false,
    },
    CliSessionSupport {
        provider: "mock",
        plan_flag: "QUEUE_PLAN_MODE=1",
        resume: true,
        evidence: "Test-Agent",
        verified: true,
    },
];

pub fn cli_session_support(provider: &str) -> Option<CliSessionSupport> {
    CLI_SESSION_SUPPORT
        .iter()
        .copied()
        .find(|c| c.provider == provider)
}

// ---------------------------------------------------------------------------
// Plan decisions
// ---------------------------------------------------------------------------

/// `POST /api/tasks/{id}/plan/approve` – optionally with an edited plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanApprove {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

/// `POST /api/tasks/{id}/plan/revise`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRevise {
    pub note: String,
}

// ---------------------------------------------------------------------------
// Steering messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    /// Waits for the next delivery point; can be withdrawn.
    Queued,
    Delivered,
    Withdrawn,
    /// The task was cancelled before delivery.
    Dropped,
}

impl MessageState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivered => "delivered",
            Self::Withdrawn => "withdrawn",
            Self::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Delivered,
            Self::Withdrawn,
            Self::Dropped,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Wartet auf Zustellung",
            Self::Delivered => "Zugestellt",
            Self::Withdrawn => "Zurückgezogen",
            Self::Dropped => "Nicht zugestellt (Auftrag abgebrochen)",
        }
    }
}

/// How a message reached the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// In the prompt of a fresh run (start, retry).
    Prompt,
    /// Into the running session (CLI resume).
    Resume,
    /// A new run in the same working tree with a handoff (no resume).
    Handoff,
    /// A "Weiter mit …" task (the task had finished).
    Continuation,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Resume => "resume",
            Self::Handoff => "handoff",
            Self::Continuation => "continuation",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Prompt,
            Self::Resume,
            Self::Handoff,
            Self::Continuation,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Prompt => "beim Start des Agenten",
            Self::Resume => "in die laufende Sitzung",
            Self::Handoff => "als neuer Lauf mit Übergabe",
            Self::Continuation => "als Fortsetzungs-Auftrag",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    Message,
    /// Bundled inline diff comments.
    Comments,
}

impl MessageSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Comments => "comments",
        }
    }
}

/// `GET /api/tasks/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskMessageView {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub author: String,
    pub body: String,
    pub source: MessageSource,
    pub state: MessageState,
    pub urgent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_as: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
    /// The caller may withdraw it now.
    pub can_withdraw: bool,
    /// Came in from an integration (WP08: Slack thread, issue comment, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<crate::intake::IntakeKind>,
}

/// `GET /api/tasks/{id}/messages`: the list and what the caller may do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskMessages {
    pub messages: Vec<TaskMessageView>,
    /// The caller may send messages to this task now.
    pub can_send: bool,
    /// The caller may start a "Weiter mit …" task.
    pub can_continue: bool,
    /// The agent runs right now ("Sofort zustellen" possible).
    pub running: bool,
}

/// `POST /api/tasks/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendMessage {
    pub body: String,
    /// Interrupt the running agent and deliver at once.
    #[serde(default)]
    pub now: bool,
}

/// `POST /api/tasks/{id}/continue` ("Weiter mit …").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinueTask {
    pub message: String,
    /// Plan first (`None` = the project's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
}

// ---------------------------------------------------------------------------
// Inline diff comments
// ---------------------------------------------------------------------------

/// Side of the diff a comment refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// New version (added and unchanged lines, new line numbers).
    New,
    /// Old version (removed lines, old line numbers).
    Old,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Old => "old",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "new" => Some(Self::New),
            "old" => Some(Self::Old),
            _ => None,
        }
    }
}

/// Where a comment is in the current diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommentLocation {
    /// Same lines at the same place.
    Current,
    /// The same lines, elsewhere in the file now.
    Moved { line_start: i32, line_end: i32 },
    /// The commented lines are no longer in the diff.
    Stale,
}

impl CommentLocation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Current => "aktuell",
            Self::Moved { .. } => "verschoben",
            Self::Stale => "veraltet",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Current => "Die Zeilen stehen unverändert an dieser Stelle.",
            Self::Moved { .. } => "Die Zeilen sind unverändert, stehen aber an einer anderen Stelle.",
            Self::Stale => "Die kommentierten Zeilen gibt es so nicht mehr. Der Kommentar wird mit den ursprünglichen Zeilen und einem Hinweis gesendet.",
        }
    }
}

/// `GET /api/tasks/{id}/comments`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentView {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Uuid,
    pub author: String,
    pub path: String,
    pub side: Side,
    pub line_start: i32,
    pub line_end: i32,
    pub body: String,
    /// The commented lines as they were.
    pub anchor: Vec<String>,
    /// `draft` (collected, only the author sees it) or `sent`.
    pub sent: bool,
    pub location: CommentLocation,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_task: Option<Uuid>,
    pub mine: bool,
}

/// `GET /api/tasks/{id}/comments`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskComments {
    pub comments: Vec<CommentView>,
    /// The caller may comment on this task's diff.
    pub can_comment: bool,
}

/// `POST /api/tasks/{id}/comments`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewComment {
    pub path: String,
    pub side: Side,
    pub line_start: i32,
    pub line_end: i32,
    pub body: String,
}

/// `PATCH /api/tasks/{id}/comments/{cid}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateComment {
    pub body: String,
}

/// `POST /api/tasks/{id}/comments/send`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SendComments {
    /// Optional words around the comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Result of sending comments or a "Weiter mit …".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SentFollowUp {
    /// `message` (into the task) or `continuation` (a new task).
    pub delivered_as: String,
    /// The task that gets it (the same one or the new one).
    pub task_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    /// Comments that were sent (stale ones included, marked).
    #[serde(default)]
    pub comments: usize,
}

/// Validates a new comment's range.
pub fn validate_range(start: i32, end: i32) -> Result<(), String> {
    if start < 1 || end < start {
        return Err("Ungültiger Zeilenbereich".into());
    }
    if end - start + 1 > MAX_COMMENT_LINES {
        return Err(format!(
            "Ein Kommentar umfasst höchstens {MAX_COMMENT_LINES} Zeilen"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_are_trimmed_bounded_and_never_empty() {
        assert_eq!(validate_text("Nachricht", "  hi  ", 10).unwrap(), "hi");
        assert!(validate_text("Nachricht", " \n ", 10)
            .unwrap_err()
            .contains("leer"));
        assert!(validate_text("Nachricht", &"ä".repeat(11), 10)
            .unwrap_err()
            .contains("zu lang"));
        // Characters, not bytes.
        assert!(validate_text("Nachricht", &"ä".repeat(10), 10).is_ok());
        assert!(validate_text("Nachricht", "a\0b", 10).is_err());
    }

    #[test]
    fn auto_approve_and_ranges_are_validated() {
        assert_eq!(validate_auto_approve(None), Ok(None));
        assert_eq!(validate_auto_approve(Some(15)), Ok(Some(15)));
        assert!(validate_auto_approve(Some(4)).is_err());
        assert!(validate_auto_approve(Some(10_081)).is_err());
        assert_eq!(auto_approve_label(Some(60)), "nach 1 Stunde");
        assert_eq!(auto_approve_label(Some(7)), "nach 7 Minuten");
        assert!(validate_range(1, 1).is_ok());
        assert!(validate_range(0, 1).is_err());
        assert!(validate_range(5, 4).is_err());
        assert!(validate_range(1, 200).is_ok());
        assert!(validate_range(1, 201).is_err());
    }

    #[test]
    fn states_round_trip_and_auto_approve_rules() {
        for s in PlanState::ALL {
            assert_eq!(PlanState::parse(s.as_str()), Some(s));
        }
        for m in [
            MessageState::Queued,
            MessageState::Delivered,
            MessageState::Withdrawn,
            MessageState::Dropped,
        ] {
            assert_eq!(MessageState::parse(m.as_str()), Some(m));
        }
        for d in [
            Delivery::Prompt,
            Delivery::Resume,
            Delivery::Handoff,
            Delivery::Continuation,
        ] {
            assert_eq!(Delivery::parse(d.as_str()), Some(d));
        }
        let mut m = PlanMeta::default();
        assert!(m.may_auto_approve());
        m.questions.push("Postgres?".into());
        assert!(!m.may_auto_approve());
        m.questions.clear();
        m.attempted_total = 1;
        assert!(!m.may_auto_approve());
        let j = serde_json::to_value(CommentLocation::Moved {
            line_start: 3,
            line_end: 4,
        })
        .unwrap();
        assert_eq!(
            j,
            serde_json::json!({"kind": "moved", "line_start": 3, "line_end": 4})
        );
        for p in ["claude", "codex", "gemini", "opencode", "mock"] {
            assert!(cli_session_support(p).is_some(), "{p}");
        }
        assert!(cli_session_support("claude").unwrap().verified);
        assert!(!cli_session_support("codex").unwrap().verified);
    }
}
