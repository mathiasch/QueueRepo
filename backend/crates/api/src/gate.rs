//! Gate and supply-chain protection (WP03): approvals of held tasks and the
//! protection settings of a project. Shared by backend and frontend, including
//! the German texts that explain findings to non-developers.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use queued_common::gate::{FindingKind, GateFinding, AGENT_CONFIG_PATHS, DEFAULT_PROTECTED};

/// Where in the pipeline a task waits for a human decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStage {
    /// After the agent run, before the commit.
    Changes,
    /// Before a setup run with secrets: setup files changed.
    Setup,
    /// Before an agent run with trusted repository configuration: it changed.
    Config,
    /// The review could not run.
    Review,
    /// Auto-push to the default branch (or secrets in pushed commits).
    Push,
    /// WP06 plan mode: the agent's plan waits for approval (nothing changed yet).
    Plan,
}

impl GateStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Setup => "setup",
            Self::Config => "config",
            Self::Review => "review",
            Self::Push => "push",
            Self::Plan => "plan",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Changes => "Änderungen brauchen deine Freigabe",
            Self::Setup => "Vorbereitung braucht deine Freigabe",
            Self::Config => "Agenten-Einstellungen im Repository geändert",
            Self::Review => "Prüfung durch den Review-Agenten war nicht möglich",
            Self::Push => "Hochladen (Push) braucht deine Freigabe",
            Self::Plan => "Der Plan wartet auf deine Freigabe",
        }
    }

    /// What happens on approval / rejection, in plain words.
    pub fn help(self) -> &'static str {
        match self {
            Self::Changes => "Der Agent ist fertig, aber seine Änderungen sind noch nicht gespeichert. Prüfe die Hinweise und die Änderungen. „Freigeben“ speichert sie und setzt den Auftrag fort (Prüfung, Review, ggf. Hochladen). „Verwerfen“ löscht die Änderungen.",
            Self::Setup => "Dateien, die das Vorbereitungs-Skript ausführt, wurden seit der letzten Freigabe geändert. Das Skript läuft mit deinen Geheimnissen. „Freigeben“ vertraut dem aktuellen Stand und startet den Auftrag neu.",
            Self::Config => "Du hast erlaubt, dass Agenten Einstellungen aus dem Repository laden. Diese Einstellungen wurden seit der letzten Freigabe geändert und könnten Programme starten. „Freigeben“ vertraut dem aktuellen Stand und startet den Auftrag neu.",
            Self::Review => "Die Änderungen sind gespeichert, wurden aber noch nicht vom Review-Agenten geprüft. Du kannst die Prüfung wiederholen oder die Änderungen ohne Prüfung übernehmen.",
            Self::Push => "Der Auftrag ist fertig. Queue lädt nie ohne Freigabe auf den Standard-Branch hoch. „Hochladen“ pusht genau den Stand dieses Auftrags (und frühere, noch nicht hochgeladene Commits).",
            Self::Plan => "Der Agent hat sich das Projekt angesehen und beschreibt, wie er die Aufgabe lösen will. Geändert hat er noch nichts. „Plan genehmigen“ lässt ihn den Plan umsetzen. Mit „Ändern“ kannst du den Plan selbst anpassen oder einen neuen anfordern, „Verwerfen“ beendet den Auftrag.",
        }
    }

    pub fn approve_label(self) -> &'static str {
        match self {
            Self::Changes => "Freigeben",
            Self::Setup | Self::Config => "Freigeben und neu starten",
            Self::Review => "Ohne Prüfung übernehmen",
            Self::Push => "Hochladen",
            Self::Plan => "Plan genehmigen",
        }
    }

    pub fn reject_label(self) -> &'static str {
        match self {
            Self::Changes => "Änderungen verwerfen",
            Self::Setup | Self::Config | Self::Review => "Auftrag anhalten",
            Self::Push => "Nicht hochladen",
            Self::Plan => "Plan verwerfen",
        }
    }
}

/// What a resumed task continues with (set by an approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateResume {
    /// Apply the held changes, commit, continue with verify/review/push.
    Commit,
    /// Run only the review again (also the automatic phase retry).
    Review,
    /// Take over without review (push gate still applies).
    SkipReview,
    /// Push the task's commit (approved), then finish.
    Push,
    /// Finish without pushing.
    NoPush,
}

/// `tasks.gate`: the current hold of a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskGate {
    pub stage: GateStage,
    #[serde(default)]
    pub findings: Vec<GateFinding>,
    pub held_at: DateTime<Utc>,
    /// HEAD the hold refers to (setup/config: the commit a human approves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The held changes can be shown as a diff (`GET /api/tasks/{id}/gate/diff`).
    #[serde(default)]
    pub has_diff: bool,
    /// Set by an approval; the worker continues there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<GateResume>,
}

/// `POST /api/tasks/{id}/gate/approve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateAction {
    /// Default approval of the stage.
    Approve,
    /// Review stage: try the review again.
    RetryReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateDecision {
    pub action: GateAction,
}

/// Short title of a finding.
pub fn finding_title(k: FindingKind) -> &'static str {
    match k {
        FindingKind::Secret => "Mögliches Geheimnis in den Änderungen",
        FindingKind::SecretInLog => "Mögliches Geheimnis in der Ausgabe des Agenten",
        FindingKind::ProtectedChanged => "Geschützte Datei geändert",
        FindingKind::TestSkipped => "Test wird übersprungen",
        FindingKind::TestDeleted => "Testdatei gelöscht",
        FindingKind::AssertionsRemoved => "Prüfungen aus Tests entfernt",
        FindingKind::VerifierConfig => "Test-Einstellungen geändert",
        FindingKind::TooLarge => "Änderung zu groß zum Prüfen",
        FindingKind::SetupFiles => "Vorbereitungs-Datei geändert",
        FindingKind::AgentConfig => "Agenten-Einstellung geändert",
        FindingKind::ReviewUnavailable => "Review nicht möglich",
        FindingKind::PushProtected => "Ziel ist der Standard-Branch",
        FindingKind::Conflict => "Änderungen passen nicht mehr",
    }
}

/// One-sentence explanation of why the finding matters.
pub fn finding_help(k: FindingKind) -> &'static str {
    match k {
        FindingKind::Secret => "Sieht aus wie ein Passwort, Token oder Schlüssel. Gespeichert und hochgeladen könnte es jeder mit Zugriff auf das Repository lesen. Der Wert wird hier nie angezeigt.",
        FindingKind::SecretInLog => "Der Agent hat etwas ausgegeben, das wie ein Zugangsschlüssel aussieht. Im Protokoll ist es geschwärzt. Prüfe, ob der Agent Geheimnisse gelesen oder weitergegeben hat.",
        FindingKind::ProtectedChanged => "Tests und CI-Dateien entscheiden, ob ein Auftrag als erledigt gilt. Ändert der Agent sie, kann er die Prüfung aushebeln.",
        FindingKind::TestSkipped => "Ein übersprungener Test prüft nichts mehr. So kann ein Fehler unbemerkt bleiben.",
        FindingKind::TestDeleted => "Ein gelöschter Test prüft nichts mehr.",
        FindingKind::AssertionsRemoved => "Es wurden mehr Prüfungen entfernt als hinzugefügt. Der Test wird dadurch schwächer.",
        FindingKind::VerifierConfig => "Diese Datei steuert, wie Tests laufen. Eine Änderung kann Tests abschalten.",
        FindingKind::TooLarge => "Die Änderung ist so groß, dass Queue sie nicht vollständig auf Geheimnisse prüfen kann.",
        FindingKind::SetupFiles => "Das Vorbereitungs-Skript führt diese Datei aus und sieht dabei deine Geheimnisse.",
        FindingKind::AgentConfig => "Agenten laden diese Einstellungen aus dem Repository. Sie können Programme starten oder Zugänge umleiten.",
        FindingKind::ReviewUnavailable => "Ohne Review hat niemand außer dem Agenten selbst die Änderungen angesehen.",
        FindingKind::PushProtected => "Der Standard-Branch ist der Hauptstand des Projekts. Dorthin lädt Queue nur mit deiner Freigabe hoch.",
        FindingKind::Conflict => "Andere Aufträge haben dieselben Stellen inzwischen geändert.",
    }
}

/// `GET /api/projects/{id}/protection`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtectionView {
    /// Agents may load hooks/MCP/settings from the repository.
    pub trust_repo_config: bool,
    /// Effective protected paths.
    pub protected_paths: Vec<String>,
    /// `protected_paths` is the built-in default list.
    pub protected_default: bool,
    /// Detect skipped/deleted tests and weakened verifier config.
    pub test_guard: bool,
    /// Approval before a setup with secrets runs changed setup files.
    pub setup_guard: bool,
    /// Number of approved secret findings (fingerprints only).
    pub approved_secrets: usize,
    /// Only the owner may change protection settings.
    pub can_edit: bool,
}

/// `PUT /api/projects/{id}/protection` – absent fields stay unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateProtection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_repo_config: Option<bool>,
    /// `Some(None)` = back to the default list.
    #[serde(
        default,
        deserialize_with = "crate::double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub protected_paths: Option<Option<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_guard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_guard: Option<bool>,
    /// Forget all approved secret findings.
    #[serde(default)]
    pub clear_approved_secrets: bool,
}

pub const MAX_PROTECTED_PATHS: usize = 100;

/// Validation shared by backend and frontend; `Err` is a German message.
pub fn validate_protected_paths(paths: &[String]) -> Result<(), String> {
    if paths.len() > MAX_PROTECTED_PATHS {
        return Err(format!("Höchstens {MAX_PROTECTED_PATHS} Muster"));
    }
    for p in paths {
        if let Some(e) = queued_common::glob::invalid(p) {
            return Err(format!("„{p}“: {e}"));
        }
    }
    Ok(())
}

/// Default protected paths as owned strings.
pub fn default_protected() -> Vec<String> {
    DEFAULT_PROTECTED.iter().map(|s| s.to_string()).collect()
}

/// How Queue keeps each CLI from loading configuration out of the
/// repository, and how well that is verified (shown in the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RepoConfigSupport {
    pub cli: &'static str,
    pub off: &'static str,
    pub evidence: &'static str,
}

pub const REPO_CONFIG_SUPPORT: [RepoConfigSupport; 4] = [
    RepoConfigSupport {
        cli: "Claude Code",
        off: "Nur eigene Einstellungen (--setting-sources user), keine MCP-Server aus .mcp.json (--strict-mcp-config).",
        evidence: "Optionen im installierten Programm 2.1.319 vorhanden; Wirkung nicht mit echtem Modell geprüft.",
    },
    RepoConfigSupport {
        cli: "Gemini CLI",
        off: "System-Einstellungen mit höchster Priorität: keine Hooks, keine MCP-Server, keine Tool-Befehle, keine .env aus dem Projekt. .gemini/.env lädt Gemini trotzdem (nur nicht gesetzte Variablen).",
        evidence: "Aus dem Quelltext von 0.67.0 abgeleitet; nicht ausgeführt.",
    },
    RepoConfigSupport {
        cli: "opencode",
        off: "Keine opencode.json, kein .opencode/ (Plugins) und keine AGENTS.md aus dem Projekt (OPENCODE_DISABLE_PROJECT_CONFIG).",
        evidence: "Aus dem Programm 1.19.5 abgeleitet; nicht ausgeführt.",
    },
    RepoConfigSupport {
        cli: "Codex",
        off: "Codex lädt .codex/config.toml nur in vertrauten Ordnern; Queue vertraut nie. Die Freischaltung wirkt bei Codex nicht.",
        evidence: "Aus Texten im Programm 0.167.0 abgeleitet; nicht ausgeführt.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_and_stage_has_texts() {
        for k in FindingKind::ALL {
            assert!(!finding_title(k).is_empty() && finding_help(k).len() > 20);
        }
        for s in [
            GateStage::Changes,
            GateStage::Setup,
            GateStage::Config,
            GateStage::Review,
            GateStage::Push,
        ] {
            assert!(!s.title().is_empty() && !s.help().is_empty());
            assert!(!s.approve_label().is_empty() && !s.reject_label().is_empty());
            let j = serde_json::to_string(&s).unwrap();
            assert_eq!(j, format!("\"{}\"", s.as_str()));
        }
    }

    #[test]
    fn gate_roundtrip_and_old_rows() {
        let g = TaskGate {
            stage: GateStage::Changes,
            findings: vec![GateFinding {
                kind: FindingKind::Secret,
                path: "a".into(),
                line: Some(3),
                detail: "GitHub-Token ghp_… (40 Zeichen)".into(),
                fingerprint: Some("ab".into()),
            }],
            held_at: Utc::now(),
            head: None,
            has_diff: true,
            resume: Some(GateResume::Commit),
        };
        let v = serde_json::to_value(&g).unwrap();
        assert_eq!(v["resume"], "commit");
        assert_eq!(serde_json::from_value::<TaskGate>(v).unwrap(), g);
        let min: TaskGate =
            serde_json::from_str(r#"{"stage":"push","held_at":"2026-11-08T10:00:00Z"}"#).unwrap();
        assert!(min.findings.is_empty() && min.resume.is_none() && !min.has_diff);
    }

    #[test]
    fn protected_path_validation() {
        assert!(validate_protected_paths(&default_protected()).is_ok());
        assert!(validate_protected_paths(&["../x".into()]).is_err());
        assert!(validate_protected_paths(&vec!["a".to_string(); 101]).is_err());
        let u: UpdateProtection = serde_json::from_str(r#"{"protected_paths":null}"#).unwrap();
        assert_eq!(u.protected_paths, Some(None));
        let u: UpdateProtection = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(u.protected_paths, None);
    }
}
