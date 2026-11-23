//! WP12 Best-of-N: one task as N parallel attempts, a comparison view and
//! the selection (by a human or a review agent).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{preview::ScreenshotView, Money, ReviewResult, Target};

/// Most attempts of one task.
pub const MAX_ATTEMPTS: i32 = 8;

/// Who picked the winner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedBy {
    User,
    Auto,
}

/// The winner of a group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BonChoice {
    pub attempt_id: Uuid,
    pub attempt_no: i32,
    pub by: SelectedBy,
    /// Review agent's reason, or a fixed text for human/only-candidate picks.
    pub reason: String,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
}

/// `tasks.bon` of a group (parent task).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Bon {
    /// Access/model of every attempt (exactly `attempts` entries).
    pub targets: Vec<Target>,
    /// A review agent picks the winner when all attempts are settled.
    #[serde(default)]
    pub auto_select: bool,
    /// Access/model of the review agent (resolved at creation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<Target>,
    /// Why this reviewer (shown in the UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector_reason: Option<String>,
    /// Start of the current round (attempts created at or after it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanned_out_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<BonChoice>,
    /// The group budget was exhausted: every attempt was stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted: Option<String>,
    /// The automatic selection failed (the user picks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select_error: Option<String>,
    /// The user was told that the attempts wait for a selection.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ready_notified: bool,
}

/// Verify result of an attempt as far as the queue knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyState {
    /// The project has no verify command.
    None,
    Passed,
    Failed,
    /// The attempt ended (or runs) before the verify command.
    NotReached,
}

/// One attempt in the comparison (`GET /api/tasks/{id}/attempts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptView {
    pub id: Uuid,
    pub attempt_no: i32,
    pub status: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Label of the connector (or the legacy provider).
    pub access: String,
    pub files_changed: u32,
    pub additions: u32,
    pub deletions: u32,
    pub verify: VerifyState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewResult>,
    /// Money per currency (catalog prices).
    pub cost: Vec<Money>,
    /// Counted tokens (input + output + cache writes).
    pub tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub screenshots: Vec<ScreenshotView>,
    pub selected: bool,
}

/// `GET /api/tasks/{id}/attempts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub group: Uuid,
    pub attempts: Vec<AttemptView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<BonChoice>,
    pub auto_select: bool,
    /// Label of the review agent of the automatic selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aborted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select_error: Option<String>,
    /// The caller may pick a candidate now.
    pub can_select: bool,
    /// Sum over all attempts and the selection.
    pub total: Vec<Money>,
}

/// `POST /api/tasks/{id}/select`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectAttempt {
    pub attempt_id: Uuid,
}

/// Answer of the review agent (parsed leniently from the model text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectorAnswer {
    /// Number of the winning candidate (as listed in the request).
    pub winner: i32,
    #[serde(default)]
    pub reason: String,
}

/// German label of an attempt status in the comparison.
pub fn attempt_status_label(status: &str, selected: bool) -> &'static str {
    match status {
        "candidate" => "Erfolgreich – wählbar",
        "merging" | "done" if selected => "Gewählt",
        "cancelled" => "Verworfen",
        "failed" => "Fehlgeschlagen",
        "blocked" => "Gestoppt",
        "held" => "Wartet auf Freigabe",
        "waiting_input" => "Rückfrage",
        "queued" => "Wartet",
        "running" | "waiting_batch" => "Läuft",
        "merging" => "Wird übernommen",
        "done" => "Übernommen",
        _ => "Unbekannt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bon_roundtrip_and_old_rows() {
        let b = Bon {
            targets: vec![Target {
                connector_id: None,
                provider: Some("mock".into()),
                model: Some("m".into()),
            }],
            auto_select: true,
            ..Default::default()
        };
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(serde_json::from_value::<Bon>(v).unwrap(), b);
        let empty: Bon = serde_json::from_value(serde_json::json!({"targets": []})).unwrap();
        assert!(!empty.auto_select && empty.choice.is_none());
    }

    #[test]
    fn status_labels_are_german_and_selection_aware() {
        assert_eq!(attempt_status_label("candidate", false), "Erfolgreich – wählbar");
        assert_eq!(attempt_status_label("merging", true), "Gewählt");
        assert_eq!(attempt_status_label("merging", false), "Wird übernommen");
        assert_eq!(attempt_status_label("cancelled", false), "Verworfen");
        assert_eq!(attempt_status_label("nope", false), "Unbekannt");
    }
}
