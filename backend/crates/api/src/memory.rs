//! WP09 wire types and German texts: project rules (managed AGENTS.md),
//! structured project state, attachments, OnEmpty dedupe/critic.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use queued_common::state::{
    valid_feature_id, valid_file_ref, Feature, FeatureStatus, ProgressEntry, ProjectState,
    MAX_TITLE,
};

// ---------------------------------------------------------------------------
// Project rules (knowledge)
// ---------------------------------------------------------------------------

/// Largest rules text (bytes).
pub const MAX_KNOWLEDGE_BYTES: usize = 32 * 1024;
/// Largest change note.
pub const MAX_KNOWLEDGE_NOTE: usize = 2000;
/// Open agent proposals per project (further ones are dropped).
pub const MAX_OPEN_AGENT_PROPOSALS: i64 = 5;

/// One version or proposal of the project rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeVersion {
    pub id: Uuid,
    /// Set for approved versions.
    pub version: Option<i32>,
    /// `proposed` | `approved` | `rejected`
    pub status: String,
    pub content: String,
    /// `human` | `agent` | `repo`
    pub source: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub task_id: Option<Uuid>,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub base_version: Option<i32>,
    #[serde(default)]
    pub note: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub decided_by: Option<String>,
}

/// `GET /api/projects/{id}/knowledge`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeView {
    /// Newest approved version (what the agents get).
    pub current: Option<KnowledgeVersion>,
    /// Approved versions, newest first (at most 30).
    pub versions: Vec<KnowledgeVersion>,
    /// Open proposals, oldest first.
    pub proposals: Vec<KnowledgeVersion>,
    /// Recently rejected proposals (at most 10).
    #[serde(default)]
    pub rejected: Vec<KnowledgeVersion>,
    /// Owner: saving creates an approved version; decides proposals.
    pub can_approve: bool,
    /// Editor or owner: may propose.
    pub can_propose: bool,
    /// The repository configuration of the CLIs is trusted (instruction
    /// files in the repository are read by the CLIs in addition).
    pub trust_repo_config: bool,
}

/// `PUT /api/projects/{id}/knowledge`: owner = new approved version,
/// editor = proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveKnowledge {
    pub content: String,
    #[serde(default)]
    pub note: String,
    /// Approved version the text was edited from (`None` = none yet);
    /// the owner's save is refused when the rules changed meanwhile.
    #[serde(default)]
    pub base_version: Option<i32>,
}

/// `POST /api/projects/{id}/knowledge/{kid}/approve|reject`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeDecision {
    #[serde(default)]
    pub note: String,
}

/// `POST /api/projects/{id}/knowledge/import`: which repository file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeImport {
    /// `AGENTS.md` | `CLAUDE.md` | `GEMINI.md`
    pub file: String,
}

pub const IMPORTABLE_FILES: [&str; 3] = ["AGENTS.md", "CLAUDE.md", "GEMINI.md"];

/// `POST /api/projects/{id}/knowledge/export`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeExportView {
    pub sha: String,
    pub version: i32,
    pub written: Vec<String>,
    pub skipped: Vec<String>,
    /// The agent-config baseline was moved to the export (trusted repo
    /// configuration only; other pending changes keep their approval).
    pub baseline_updated: bool,
}

/// Validation shared by backend and frontend; `Err` is German.
pub fn validate_knowledge(content: &str, note: &str) -> Result<(), String> {
    if content.len() > MAX_KNOWLEDGE_BYTES {
        return Err(format!(
            "Die Regeln sind zu lang ({} Bytes, höchstens {MAX_KNOWLEDGE_BYTES}).",
            content.len()
        ));
    }
    if content.contains('\0') {
        return Err("Die Regeln enthalten ungültige Zeichen.".into());
    }
    if note.chars().count() > MAX_KNOWLEDGE_NOTE {
        return Err(format!(
            "Die Notiz ist zu lang (höchstens {MAX_KNOWLEDGE_NOTE} Zeichen)."
        ));
    }
    Ok(())
}

pub fn knowledge_source_label(source: &str) -> &'static str {
    match source {
        "agent" => "Vorschlag eines Agenten",
        "repo" => "Aus dem Repository übernommen",
        _ => "Von einem Menschen",
    }
}

pub const KNOWLEDGE_HELP: &str = "Projekt-Regeln sind dauerhafte Hinweise für jeden Agenten in diesem Projekt – zum Beispiel wie getestet wird, welche Ordner tabu sind oder welcher Stil gilt. Nur die neueste freigegebene Fassung geht in die Aufträge. Agenten und Bearbeiter dürfen Änderungen nur vorschlagen; freigeben kann der Projekt-Eigentümer.";

/// What each CLI reads from the repository by itself, and what Queue does
/// about it (shown in the UI and documented in the README).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CliInstructionSupport {
    pub cli: &'static str,
    pub reads: &'static str,
    pub standard: &'static str,
    pub trusted: &'static str,
    pub evidence: &'static str,
}

pub const CLI_INSTRUCTIONS: [CliInstructionSupport; 4] = [
    CliInstructionSupport {
        cli: "Claude Code",
        reads: "CLAUDE.md, .claude/CLAUDE.md, .claude/rules/, CLAUDE.local.md, AGENTS.md",
        standard: "Liest nichts davon (nur eigene Einstellungen, --setting-sources user).",
        trusted: "Liest die Dateien; jede Änderung braucht eine Freigabe.",
        evidence: "Im Programm 2.1.322 nachgesehen (Dateien nur mit Projekt-Einstellungen); nicht mit echtem Modell geprüft.",
    },
    CliInstructionSupport {
        cli: "Codex",
        reads: "AGENTS.md, AGENTS.override.md",
        standard: "Queue schaltet das Lesen ab (project_doc_max_bytes=0).",
        trusted: "Liest die Dateien; jede Änderung braucht eine Freigabe.",
        evidence: "Einstellung im Programm 0.167.0 vorhanden; Wirkung nicht ausgeführt.",
    },
    CliInstructionSupport {
        cli: "Gemini CLI",
        reads: "GEMINI.md (auch in Unterordnern)",
        standard: "Queue lenkt den Dateinamen auf einen Namen um, den es nicht gibt (context.fileName).",
        trusted: "Liest GEMINI.md; jede Änderung braucht eine Freigabe.",
        evidence: "Aus dem Quelltext von 0.67.0 abgeleitet; nicht ausgeführt.",
    },
    CliInstructionSupport {
        cli: "opencode",
        reads: "AGENTS.md, CLAUDE.md, CONTEXT.md",
        standard: "Liest nichts davon (OPENCODE_DISABLE_PROJECT_CONFIG).",
        trusted: "Liest die Dateien; jede Änderung braucht eine Freigabe.",
        evidence: "Aus dem Programm 1.19.5 abgeleitet; nicht ausgeführt.",
    },
];

// ---------------------------------------------------------------------------
// Project state
// ---------------------------------------------------------------------------

/// One revision in the history list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRevision {
    pub rev: i32,
    /// `human` | `agent`
    pub source: String,
    #[serde(default)]
    pub task_id: Option<Uuid>,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub note: String,
    pub created_at: DateTime<Utc>,
}

/// `GET /api/projects/{id}/state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateView {
    /// 0 = no state yet.
    pub rev: i32,
    pub state: ProjectState,
    /// Newest first (at most 20).
    pub history: Vec<StateRevision>,
    pub can_edit: bool,
}

/// `PUT /api/projects/{id}/state` (humans may restructure).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PutState {
    pub state: ProjectState,
    /// Revision the edit started from (refused when it moved meanwhile).
    pub base_rev: i32,
    #[serde(default)]
    pub note: String,
}

pub fn feature_status_label(s: FeatureStatus) -> &'static str {
    match s {
        FeatureStatus::Todo => "Offen",
        FeatureStatus::InProgress => "In Arbeit",
        FeatureStatus::Done => "Erledigt",
        FeatureStatus::Blocked => "Blockiert",
    }
}

pub const STATE_HELP: &str = "Der Projektzustand ist eine Liste von Funktionen mit Status. Jeder Agent bekommt sie mit und darf sie fortschreiben; Queue prüft jede Änderung (verwiesene Dateien müssen existieren, „Erledigt“ nur nach erfolgreichem Lauf, löschen und umbenennen nur durch Menschen) und übernimmt sie erst, wenn der Auftrag erledigt ist.";

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

/// Largest single upload (bytes).
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
pub use queued_common::{MAX_ATTACHMENTS_PER_TASK, MAX_ATTACHMENT_BYTES_PER_TASK};
/// Default storage quota per user (MiB; `ATTACHMENT_QUOTA_MB`).
pub const DEFAULT_ATTACHMENT_QUOTA_MB: u64 = 200;
/// `accept` of the file input (the server sniffs anyway).
pub const ATTACHMENT_ACCEPT: &str = "image/png,image/jpeg,image/gif,image/webp,application/pdf,text/plain,text/markdown,text/csv,application/json,.txt,.md,.csv,.json,.log,.diff,.patch,.yaml,.yml,.toml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentView {
    pub id: Uuid,
    #[serde(default)]
    pub task_id: Option<Uuid>,
    /// Name in `.queue/attachments/`.
    pub name: String,
    pub display_name: String,
    pub mime: String,
    pub size: i64,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub can_delete: bool,
}

impl AttachmentView {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
}

/// Storage use of the caller (`GET /api/attachments/quota`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentQuota {
    pub used: i64,
    pub limit: i64,
}

pub fn attachment_kind_label(mime: &str) -> &'static str {
    match mime {
        "image/png" | "image/jpeg" => "Bild",
        "application/pdf" => "PDF",
        _ => "Text",
    }
}

/// Human file size in German ("1,2 MB").
pub fn file_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{} KB", fmt1(b / 1024.0))
    } else {
        format!("{} MB", fmt1(b / (1024.0 * 1024.0)))
    }
}

fn fmt1(x: f64) -> String {
    format!("{x:.1}").replace('.', ",")
}

pub const ATTACHMENT_HELP: &str = "Bilder, PDFs und Textdateien bis 10 MB, höchstens 10 pro Auftrag. Fotos werden neu gespeichert: Standort und Kameradaten (EXIF) werden dabei entfernt. Der Agent findet die Dateien im Ordner .queue/attachments/; sie landen nie in einem Commit.";

// ---------------------------------------------------------------------------
// OnEmpty dedupe / critic
// ---------------------------------------------------------------------------

/// Verdict for one OnEmpty proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Queued (or waiting for approval because of the count/depth rules).
    Accepted,
    /// Needs a human approval (similar to existing work, critic rule).
    Review,
    /// Not created (duplicate, critic rule).
    Dropped,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accepted => "Eingeplant",
            Self::Review => "Freigabe nötig",
            Self::Dropped => "Verworfen",
        }
    }
}

/// The most similar existing task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarTask {
    pub task_id: Uuid,
    pub title: String,
    pub status: String,
    /// 0..1
    pub score: f32,
    /// The task was reverted (negative example).
    #[serde(default)]
    pub reverted: bool,
}

/// Screening of one proposal (stored on the created task as `dedupe` and
/// in the OnEmpty task's report as one entry of `proposals`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screening {
    pub title: String,
    pub verdict: Verdict,
    /// German reasons, in order of the checks.
    pub reasons: Vec<String>,
    #[serde(default)]
    pub similar: Option<SimilarTask>,
    /// Task created for the proposal.
    #[serde(default)]
    pub task_id: Option<Uuid>,
    /// `rules` or `rules+model`.
    pub critic: String,
}

/// Validated dedupe settings (shared with the UI).
pub fn validate_dedupe(threshold: f32, review: f32, days: i32) -> Result<(), String> {
    if !(0.05..=1.0).contains(&threshold) || !(0.05..=1.0).contains(&review) {
        return Err("Schwellen liegen zwischen 5 % und 100 %.".into());
    }
    if review > threshold {
        return Err("Die Prüf-Schwelle darf nicht über der Duplikat-Schwelle liegen.".into());
    }
    if !(1..=3650).contains(&days) {
        return Err("Der Zeitraum liegt zwischen 1 und 3650 Tagen.".into());
    }
    Ok(())
}

pub const DEDUPE_HELP: &str = "Bevor die Leerlauf-Funktion Vorschläge einplant, vergleicht Queue sie mit offenen, erledigten, abgelehnten und rückgängig gemachten Aufträgen dieses Projekts. Sehr ähnliche Vorschläge werden verworfen, ähnliche brauchen eine Freigabe. Die Begründung steht am Auftrag.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_and_labels() {
        assert!(validate_knowledge("# Regeln", "").is_ok());
        assert!(validate_knowledge(&"x".repeat(MAX_KNOWLEDGE_BYTES + 1), "").is_err());
        assert!(validate_knowledge("a\0b", "").is_err());
        assert!(validate_dedupe(0.6, 0.35, 90).is_ok());
        assert!(validate_dedupe(0.3, 0.5, 90).is_err());
        assert!(validate_dedupe(1.5, 0.5, 90).is_err());
        assert!(validate_dedupe(0.6, 0.3, 0).is_err());
        assert_eq!(file_size(512), "512 B");
        assert_eq!(file_size(1536), "1,5 KB");
        assert_eq!(file_size(3 * 1024 * 1024), "3,0 MB");
        for s in FeatureStatus::ALL {
            assert!(!feature_status_label(s).is_empty());
        }
        let j = serde_json::to_string(&Verdict::Review).unwrap();
        assert_eq!(j, "\"review\"");
        assert_eq!(CLI_INSTRUCTIONS.len(), 4);
    }
}
