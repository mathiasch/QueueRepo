//! Branch per task, parallelism per project and the merge queue (WP04):
//! shared types and texts (server and UI use the same wording).
use serde::{Deserialize, Serialize};

/// Highest parallelism per project. More parallel tasks mean more merge
/// conflicts and load; practitioners stop at about three.
pub const MAX_PARALLELISM: i32 = 8;

pub fn default_parallelism() -> i32 {
    1
}

/// Readable error for an invalid parallelism.
pub fn validate_parallelism(n: i32) -> Result<(), String> {
    if (1..=MAX_PARALLELISM).contains(&n) {
        Ok(())
    } else {
        Err(format!(
            "Gleichzeitige Aufträge: bitte eine Zahl von 1 bis {MAX_PARALLELISM}"
        ))
    }
}

pub const PARALLELISM_HELP: &str = "Jeder Auftrag arbeitet auf einem eigenen Branch in einem eigenen Arbeitsverzeichnis. Fertige Aufträge werden der Reihe nach in den Hauptstand übernommen; ändern zwei Aufträge dieselbe Stelle, erstellt Queue einen Korrektur-Auftrag. Mehr gleichzeitige Aufträge sind schneller, erzeugen aber mehr Konflikte und Last – 1 bis 3 ist meist sinnvoll.";

/// Where a task stands in the project's merge queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// Finished and checked, waits for its turn.
    Queued,
    /// Being brought up to date, re-checked and merged right now.
    Merging,
    /// On the target branch.
    Merged,
    /// Changed the same places as a task merged before it.
    Conflict,
    /// Checks failed after bringing it up to date.
    Failed,
    /// WP05 (output `branch`/`pr`): the task branch is on the remote; the
    /// merge into the target branch happens on the git host, by people.
    Published,
}

impl MergeState {
    pub const ALL: [MergeState; 6] = [
        Self::Queued,
        Self::Merging,
        Self::Merged,
        Self::Conflict,
        Self::Failed,
        Self::Published,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Merging => "merging",
            Self::Merged => "merged",
            Self::Conflict => "conflict",
            Self::Failed => "failed",
            Self::Published => "published",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Wartet auf Übernahme",
            Self::Merging => "Wird übernommen",
            Self::Merged => "Übernommen",
            Self::Conflict => "Konflikt",
            Self::Failed => "Prüfung nach Aktualisierung fehlgeschlagen",
            Self::Published => "Hochgeladen",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Queued => "Der Auftrag ist fertig und geprüft. Er wartet, bis die Aufträge vor ihm in den Hauptstand übernommen sind.",
            Self::Merging => "Queue bringt den Branch auf den neuesten Stand, prüft ihn erneut und übernimmt ihn dann in den Hauptstand.",
            Self::Merged => "Die Änderungen sind als ein Commit im Hauptstand. „Rückgängig“ nimmt sie wieder heraus.",
            Self::Conflict => "Ein anderer Auftrag hat inzwischen dieselben Stellen geändert. Queue hat einen Korrektur-Auftrag erstellt, der beides zusammenführt.",
            Self::Failed => "Nach dem Aktualisieren auf den neuesten Stand schlug die Prüfung fehl. Queue hat einen Korrektur-Auftrag erstellt.",
            Self::Published => "Der geprüfte Branch liegt auf dem Git-Server. Übernommen wird er dort – von Menschen, nie von Queue. „Rückgängig“ gibt es hier deshalb nicht.",
        }
    }
}

/// One entry of a project's merge queue (`GET /api/projects/{id}/merge-queue`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeQueueEntry {
    pub task_id: uuid::Uuid,
    pub title: String,
    /// Who created the task (name or e-mail).
    pub user: String,
    pub branch: String,
    pub state: MergeState,
    pub queued_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 1 = being merged or next.
    pub position: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_roundtrip_and_have_texts() {
        for m in MergeState::ALL {
            assert_eq!(MergeState::parse(m.as_str()), Some(m));
            assert_eq!(
                serde_json::to_string(&m).unwrap(),
                format!("\"{}\"", m.as_str())
            );
            assert!(!m.label().is_empty() && m.help().len() > 40);
        }
        assert_eq!(MergeState::parse("x"), None);
    }

    #[test]
    fn parallelism_bounds() {
        assert!(validate_parallelism(1).is_ok() && validate_parallelism(MAX_PARALLELISM).is_ok());
        for bad in [0, -1, MAX_PARALLELISM + 1] {
            assert!(validate_parallelism(bad).unwrap_err().contains("1 bis"));
        }
        assert_eq!(default_parallelism(), 1);
    }
}
