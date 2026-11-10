//! Output modes and pull requests (WP05): texts and tones for the UI.
use queued_api::pr::{
    CheckState, CiState, FeedbackOutcome, GitHost, OutputMode, PrState, PullRequestView,
};

use super::labels::Tone;

pub fn ci_tone(c: CiState) -> Tone {
    match c {
        CiState::None => Tone::Neutral,
        CiState::Pending => Tone::Run,
        CiState::Success => Tone::Success,
        CiState::Failure => Tone::Danger,
    }
}

pub fn check_tone(c: CheckState) -> Tone {
    match c {
        CheckState::Pending => Tone::Run,
        CheckState::Success => Tone::Success,
        CheckState::Failure => Tone::Danger,
        CheckState::Neutral => Tone::Neutral,
    }
}

pub fn pr_tone(s: PrState) -> Tone {
    match s {
        PrState::Creating => Tone::Run,
        PrState::Open => Tone::Info,
        PrState::Merged => Tone::Success,
        PrState::Closed => Tone::Neutral,
    }
}

pub fn outcome_tone(o: FeedbackOutcome) -> Tone {
    match o {
        FeedbackOutcome::Task => Tone::Info,
        FeedbackOutcome::IgnoredLimit => Tone::Warning,
        _ => Tone::Neutral,
    }
}

/// "1 von 3 Folge-Runden genutzt" in plain words.
pub fn rounds_text(pr: &PullRequestView) -> String {
    match (pr.followups, pr.max_followups) {
        (_, 0) => "Automatische Folge-Aufträge sind für diesen Pull Request aus (Grenze 0).".into(),
        (n, m) if n >= m => {
            format!("Alle {m} Folge-Runden genutzt – neue Rückmeldungen warten auf dich.")
        }
        (0, m) => format!("Noch keine Folge-Runde genutzt (höchstens {m})."),
        (n, m) => format!("{n} von {m} Folge-Runden genutzt."),
    }
}

/// The owner may allow one more round when the limit is reached.
pub fn can_extend(pr: &PullRequestView) -> bool {
    pr.state.is_open() && pr.followups >= pr.max_followups
}

/// Short state line under the title.
pub fn state_line(pr: &PullRequestView) -> String {
    let mut s = format!("{} → {}", pr.head_branch, pr.base_branch);
    if pr.conflict {
        s.push_str(" · Konflikt mit dem Ziel-Branch – bitte auf dem Git-Server lösen");
    }
    s
}

/// Label of a host in the select ("Automatisch (erkannt: GitHub)").
pub fn auto_label(detected: Option<GitHost>) -> String {
    match detected {
        Some(h) => format!("Automatisch (erkannt: {})", h.label()),
        None => "Automatisch (nicht erkannt – bitte wählen)".into(),
    }
}

/// Who uploads when, per mode (shown under the mode choice).
pub fn who_pushes(m: OutputMode) -> &'static [&'static str] {
    match m {
        OutputMode::Commit => &[
            "Queue übernimmt jeden geprüften Auftrag als einen Commit in den Hauptstand (der Reihe nach).",
            "Hochgeladen wird nur mit „Automatisch hochladen“ – auf den Standard-Branch nur nach deiner Freigabe.",
        ],
        OutputMode::Branch => &[
            "Nach Schutzprüfung, Prüfbefehl und Review lädt Queue genau diesen Stand als Branch queue/… hoch.",
            "Geheimnisse werden vorher gesucht; der Standard-Branch wird nie direkt beschrieben.",
            "Übernommen wird auf dem Git-Server – von euch.",
        ],
        OutputMode::Pr => &[
            "Nach Schutzprüfung, Prüfbefehl und Review lädt Queue den Branch hoch und legt den Pull Request an.",
            "Rote CI oder ein Review-Kommentar von jemandem mit Schreibrecht startet einen Folge-Auftrag auf demselben Branch (allgemeine Kommentare nur mit /queue).",
            "Kommentare ohne Schreibrecht werden ignoriert, die Anzahl der Folge-Runden ist begrenzt.",
            "Queue genehmigt und merged nie selbst.",
        ],
    }
}

/// Labels as edited in one text field.
pub fn labels_text(l: &[String]) -> String {
    l.join(", ")
}

pub fn parse_labels(s: &str) -> Vec<String> {
    s.split([',', '\n'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pr(extra: serde_json::Value) -> PullRequestView {
        let mut v = json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "project_id": "00000000-0000-0000-0000-000000000002",
            "task_id": "00000000-0000-0000-0000-000000000003",
            "task_title": "t", "host": "github", "number": 4, "url": "https://github.com/a/b/pull/4",
            "state": "open", "draft": true, "head_branch": "queue/0123456789ab", "base_branch": "main",
            "head_sha": null, "ci_state": "failure", "checks": [], "conflict": false, "auto_fix": true,
            "followups": 1, "max_followups": 3, "active_task": null, "last_polled_at": null,
            "next_poll_at": null, "last_error": null, "created_at": "2026-01-01T10:00:00Z"
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn rounds_and_extension() {
        assert_eq!(rounds_text(&pr(json!({}))), "1 von 3 Folge-Runden genutzt.");
        assert!(rounds_text(&pr(json!({"followups": 0}))).contains("Noch keine"));
        let full = pr(json!({"followups": 3}));
        assert!(rounds_text(&full).contains("Alle 3") && can_extend(&full));
        assert!(!can_extend(&pr(json!({}))));
        assert!(!can_extend(&pr(json!({"followups": 3, "state": "merged"}))));
        assert!(rounds_text(&pr(json!({"max_followups": 0}))).contains("aus"));
        assert_eq!(pr(json!({})).title(), "Pull Request #4");
        assert_eq!(pr(json!({"host": "gitlab"})).title(), "Merge Request #4");
    }

    #[test]
    fn tones_texts_and_labels() {
        assert_eq!(ci_tone(CiState::Failure), Tone::Danger);
        assert_eq!(check_tone(CheckState::Pending), Tone::Run);
        assert_eq!(pr_tone(PrState::Merged), Tone::Success);
        assert_eq!(outcome_tone(FeedbackOutcome::IgnoredLimit), Tone::Warning);
        assert!(state_line(&pr(json!({"conflict": true}))).contains("Konflikt"));
        assert!(auto_label(Some(GitHost::Gitlab)).contains("GitLab"));
        assert!(auto_label(None).contains("bitte wählen"));
        assert!(who_pushes(OutputMode::Pr)
            .iter()
            .any(|l| l.contains("nie selbst")));
        assert_eq!(parse_labels(" queue, ki ,\n,x"), vec!["queue", "ki", "x"]);
        assert_eq!(labels_text(&["a".into(), "b".into()]), "a, b");
    }
}
