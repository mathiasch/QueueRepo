//! Progress of a running task, derived from its log entries.

/// Steps shown instead of the raw log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Prepare,
    Work,
    Check,
    Finish,
}

impl Phase {
    pub const ALL: [Phase; 4] = [Phase::Prepare, Phase::Work, Phase::Check, Phase::Finish];

    pub fn label(self) -> &'static str {
        match self {
            Phase::Prepare => "Vorbereiten",
            Phase::Work => "Umsetzen",
            Phase::Check => "Prüfen",
            Phase::Finish => "Abschließen",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Phase::Prepare => "Projekt wird aktualisiert und der Agent gestartet.",
            Phase::Work => "Der Agent liest Code, führt Befehle aus und ändert Dateien.",
            Phase::Check => "Tests laufen und ein zweiter Agent prüft die Änderungen.",
            Phase::Finish => "Änderungen werden gespeichert und ggf. hochgeladen.",
        }
    }

    pub fn index(self) -> usize {
        Phase::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// Next phase after one log entry. A new run (`Start #…`) starts over at
/// [`Phase::Work`]; otherwise the phase never goes backwards.
pub fn advance(current: Phase, kind: &str, content: &str) -> Phase {
    let next = match kind {
        "status" if content.starts_with("Start #") => return Phase::Work,
        "status"
            if content.starts_with("Erledigt")
                || content.starts_with("git push")
                || content.starts_with("Fix-Auftrag erstellt") =>
        {
            Phase::Finish
        }
        "verify" | "review" => Phase::Check,
        "text" | "tool" | "stderr" | "contract" => Phase::Work,
        _ => current,
    };
    next.max(current)
}

/// Phase after a sequence of `(kind, content)` log entries.
pub fn from_events<'a>(events: impl IntoIterator<Item = (&'a str, &'a str)>) -> Phase {
    events
        .into_iter()
        .fold(Phase::Prepare, |p, (k, c)| advance(p, k, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_follow_the_pipeline() {
        assert_eq!(from_events([]), Phase::Prepare);
        assert_eq!(
            from_events([("status", "git: pull failed")]),
            Phase::Prepare
        );
        assert_eq!(from_events([("status", "Start #0 via mock")]), Phase::Work);
        assert_eq!(
            from_events([
                ("status", "Start #0 via mock"),
                ("tool", "ls"),
                ("verify", "$ cargo test")
            ]),
            Phase::Check
        );
        assert_eq!(
            from_events([
                ("verify", "$ x"),
                ("review", "Review via mock"),
                ("status", "Erledigt")
            ]),
            Phase::Finish
        );
        // Never backwards within a run …
        assert_eq!(
            from_events([("verify", "$ x"), ("tool", "late tool line")]),
            Phase::Check
        );
        // … but a retry starts over.
        assert_eq!(
            from_events([
                ("verify", "$ x"),
                ("status", "Start #1 via mock (Wiederholung 1)")
            ]),
            Phase::Work
        );
    }

    #[test]
    fn labels_and_indices() {
        let labels: Vec<&str> = Phase::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(labels, ["Vorbereiten", "Umsetzen", "Prüfen", "Abschließen"]);
        assert_eq!(Phase::Check.index(), 2);
        assert!(Phase::ALL.iter().all(|p| p.help().ends_with('.')));
    }
}
