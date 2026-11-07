//! Keyboard shortcuts for power users (ignored while typing in a field).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NewTask,
    GoQueue,
    GoInbox,
    GoProjects,
    GoSettings,
    Help,
}

/// Shown in the help dialog.
pub const LIST: [(&str, &str); 8] = [
    ("n", "Neuer Auftrag"),
    ("g a", "Zu den Aufträgen"),
    ("g p", "Zum Postfach"),
    ("g o", "Zu den Projekten"),
    ("g e", "Zu den Einstellungen"),
    ("?", "Diese Hilfe"),
    ("Esc", "Details oder Dialog schließen"),
    (
        "Strg/⌘ + Enter",
        "Auftrag einreihen, Antwort senden, Änderungen speichern",
    ),
];

/// `g` must be followed by the second key within this time.
const CHORD_MS: f64 = 1500.0;

#[derive(Debug, Default)]
pub struct Keys {
    pending_g: Option<f64>,
}

impl Keys {
    /// Feeds one key (`KeyboardEvent.key`, modifiers already filtered out).
    pub fn press(&mut self, key: &str, now_ms: f64) -> Option<Action> {
        if let Some(t) = self.pending_g.take() {
            if now_ms - t <= CHORD_MS {
                return match key {
                    "a" => Some(Action::GoQueue),
                    "p" => Some(Action::GoInbox),
                    "o" => Some(Action::GoProjects),
                    "e" => Some(Action::GoSettings),
                    _ => None,
                };
            }
        }
        match key {
            "g" => {
                self.pending_g = Some(now_ms);
                None
            }
            "n" => Some(Action::NewTask),
            "?" => Some(Action::Help),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_keys_and_chords() {
        let mut k = Keys::default();
        assert_eq!(k.press("n", 0.0), Some(Action::NewTask));
        assert_eq!(k.press("?", 0.0), Some(Action::Help));
        assert_eq!(k.press("g", 0.0), None);
        assert_eq!(k.press("p", 500.0), Some(Action::GoInbox));
        assert_eq!(k.press("g", 1000.0), None);
        assert_eq!(k.press("o", 1100.0), Some(Action::GoProjects));
        assert_eq!(k.press("g", 2000.0), None);
        assert_eq!(k.press("e", 2100.0), Some(Action::GoSettings));
        assert_eq!(k.press("g", 3000.0), None);
        assert_eq!(k.press("a", 3200.0), Some(Action::GoQueue));
    }

    #[test]
    fn chord_expires_and_unknown_keys_are_ignored() {
        let mut k = Keys::default();
        assert_eq!(k.press("g", 0.0), None);
        // Too late: "e" is not a single-key shortcut.
        assert_eq!(k.press("e", 2000.0), None);
        assert_eq!(k.press("x", 2100.0), None);
        assert_eq!(k.press("g", 3000.0), None);
        assert_eq!(
            k.press("x", 3100.0),
            None,
            "unknown second key cancels the chord"
        );
        assert_eq!(k.press("p", 3200.0), None, "p alone does nothing");
        assert_eq!(LIST.len(), 8);
    }
}
