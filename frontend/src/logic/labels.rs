//! Plain-language labels and explanations for statuses, failure classes,
//! providers, credentials and log entries.
use queued_api::{FailureClass, TaskStatus};

/// Visual tone of a status / tag (maps to CSS classes `tone-*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Run,
    Success,
    Warning,
    Danger,
}

impl Tone {
    pub fn class(self) -> &'static str {
        match self {
            Tone::Neutral => "tone-neutral",
            Tone::Info => "tone-info",
            Tone::Run => "tone-run",
            Tone::Success => "tone-success",
            Tone::Warning => "tone-warning",
            Tone::Danger => "tone-danger",
        }
    }
}

pub fn status_label(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::PendingApproval => "Freigabe nötig",
        TaskStatus::Queued => "Wartet",
        TaskStatus::Running => "Läuft",
        TaskStatus::WaitingInput => "Wartet auf dich",
        TaskStatus::Done => "Fertig",
        TaskStatus::Failed => "Fehlgeschlagen",
        TaskStatus::Blocked => "Blockiert",
        TaskStatus::Cancelled => "Abgebrochen",
    }
}

/// One sentence for people who do not know the internals.
pub fn status_help(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::PendingApproval => {
            "Ein Agent schlägt diesen Auftrag vor. Er startet erst, wenn du ihn freigibst."
        }
        TaskStatus::Queued => "Steht in der Warteschlange und startet, sobald ein Agent frei ist.",
        TaskStatus::Running => "Ein Agent arbeitet gerade daran.",
        TaskStatus::WaitingInput => "Der Agent hat eine Rückfrage und wartet auf deine Antwort.",
        TaskStatus::Done => "Erledigt, geprüft und im Projekt gespeichert.",
        TaskStatus::Failed => {
            "Das Ergebnis war nicht in Ordnung. Ein Reparatur-Auftrag versucht es erneut."
        }
        TaskStatus::Blocked => {
            "Gestoppt: Automatische Versuche sind aufgebraucht oder ein Limit wurde erreicht."
        }
        TaskStatus::Cancelled => "Wurde abgebrochen oder verworfen.",
    }
}

pub fn status_tone(s: TaskStatus) -> Tone {
    match s {
        TaskStatus::PendingApproval | TaskStatus::WaitingInput => Tone::Warning,
        TaskStatus::Queued => Tone::Info,
        TaskStatus::Running => Tone::Run,
        TaskStatus::Done => Tone::Success,
        TaskStatus::Failed | TaskStatus::Blocked => Tone::Danger,
        TaskStatus::Cancelled => Tone::Neutral,
    }
}

pub fn failure_label(c: FailureClass) -> &'static str {
    match c {
        FailureClass::Task => "Task-Fehler",
        FailureClass::RateLimit => "Abo-Limit",
        FailureClass::ProviderTransient => "Provider gestört",
        FailureClass::Infra => "Infrastruktur",
        FailureClass::Stalled => "Stillstand",
        FailureClass::ProviderAuth => "Anmeldung/Abrechnung",
        FailureClass::Config => "Konfiguration",
        FailureClass::LimitRuntime => "Limit: Laufzeit",
        FailureClass::LimitCost => "Limit: Kosten",
        FailureClass::LimitTokens => "Limit: Tokens",
        FailureClass::LimitTurns => "Limit: Turns",
        FailureClass::Cancelled => "Abgebrochen",
    }
}

/// What happened and what the user can do.
pub fn failure_help(c: FailureClass) -> &'static str {
    match c {
        FailureClass::Task => "Das Ergebnis des Agenten war fehlerhaft.",
        FailureClass::RateLimit => {
            "Das Kontingent deines Abos ist erschöpft. Der Auftrag startet automatisch nach dem Reset."
        }
        FailureClass::ProviderTransient => {
            "Der Anbieter war kurz nicht erreichbar oder überlastet. Es wird automatisch erneut versucht."
        }
        FailureClass::Infra => {
            "Ein technischer Fehler in der Ausführung. Es wird automatisch erneut versucht."
        }
        FailureClass::Stalled => {
            "Der Agent hat zu lange nichts ausgegeben und wurde gestoppt. Es wird erneut versucht."
        }
        FailureClass::ProviderAuth => {
            "Die Anmeldung beim Anbieter ist fehlgeschlagen. Prüfe deinen Zugang unter Einstellungen."
        }
        FailureClass::Config => {
            "Die Einstellungen passen nicht (z. B. unbekanntes Modell). Ändere sie und versuche es erneut."
        }
        FailureClass::LimitRuntime => "Die maximale Laufzeit wurde erreicht.",
        FailureClass::LimitCost => "Das Kostenlimit wurde erreicht.",
        FailureClass::LimitTokens => "Das Token-Limit wurde erreicht.",
        FailureClass::LimitTurns => "Die maximale Zahl an Arbeitsschritten (Turns) wurde erreicht.",
        FailureClass::Cancelled => "Der Lauf wurde abgebrochen.",
    }
}

pub fn provider_label(p: &str) -> String {
    match p {
        "claude" => "Claude".into(),
        "codex" => "Codex (GPT)".into(),
        "gemini" => "Gemini".into(),
        "mock" => "Mock".into(),
        other => other.into(),
    }
}

pub fn task_kind_label(kind: &str) -> &'static str {
    match kind {
        "fix" => "Reparatur",
        "onempty" => "Automatisch",
        _ => "Auftrag",
    }
}

pub fn credential_kind_label(k: &str) -> String {
    match k {
        "oauth_token" => "Abo-Token".into(),
        "auth_json" => "Abo-Login (auth.json)".into(),
        "oauth_creds" => "Abo-Login (oauth_creds.json)".into(),
        "api_key" => "API-Key".into(),
        other => other.into(),
    }
}

pub fn credential_help(provider: &str, kind: &str) -> &'static str {
    match (provider, kind) {
        ("claude", "oauth_token") => "Nutzt dein Claude Pro/Max-Abo. Lokal im Terminal `claude setup-token` ausführen und den angezeigten Token (sk-ant-oat…) hier einfügen.",
        ("claude", "api_key") => "Anthropic API-Key (sk-ant-api…), Abrechnung pro Token.",
        ("codex", "auth_json") => "Nutzt dein ChatGPT Plus/Pro-Abo. Lokal `codex login` ausführen, dann den Inhalt von ~/.codex/auth.json hier einfügen. Erneuerte Tokens werden automatisch zurückgespeichert.",
        ("codex", "api_key") => "OpenAI API-Key (sk-…), Abrechnung pro Token.",
        ("gemini", "oauth_creds") => "Nutzt dein Google-Konto/Abo. Lokal `gemini` starten, mit Google anmelden, dann den Inhalt von ~/.gemini/oauth_creds.json hier einfügen.",
        ("gemini", "api_key") => "Gemini API-Key aus Google AI Studio.",
        _ => "",
    }
}

/// JSON file contents need a multi-line field.
pub fn credential_is_json(kind: &str) -> bool {
    kind.ends_with("_json") || kind == "oauth_creds"
}

/// Short label for a log entry kind.
pub fn event_kind_label(kind: &str) -> &'static str {
    match kind {
        "status" => "Status",
        "text" => "Agent",
        "tool" => "Werkzeug",
        "stderr" => "Ausgabe",
        "error" => "Fehler",
        "verify" => "Prüfung",
        "review" => "Review",
        "limit" => "Limit",
        "retry" => "Wiederholung",
        "guard" => "Wächter",
        "contract" => "Ergebnis",
        _ => "Info",
    }
}

/// Notification kinds that mean something went wrong.
pub fn notification_is_error(kind: &str) -> bool {
    matches!(kind, "blocked" | "provider_paused" | "push_failed")
}

/// OnEmptyQueue actions: id, label, description.
pub const ONEMPTY_ACTIONS: [(&str, &str, &str); 3] = [
    (
        "review",
        "Code prüfen",
        "Code auf Bugs, Sicherheitslücken und fehlende Tests prüfen",
    ),
    (
        "research",
        "Recherche",
        "Im Web nach neuen Versionen, Tools und Best Practices suchen",
    ),
    (
        "improve",
        "Verbesserungen",
        "Bisherige Arbeit analysieren und nächste Schritte vorschlagen",
    ),
];

/// Glossary for help tips.
pub mod help {
    pub const PROVIDER: &str = "Der Anbieter ist die KI, die deine Aufträge bearbeitet (z. B. Claude, Codex oder Gemini). Du verbindest ihn einmal unter Einstellungen → Zugänge.";
    pub const MODEL: &str = "Optional ein bestimmtes Modell des Anbieters, z. B. ein günstigeres. Leer lassen für den Standard.";
    pub const HANDOFF: &str = "Übergabe: Jeder Auftrag hinterlässt eine kurze Notiz für den nächsten. Wählst du hier einen erledigten Auftrag, setzt der neue genau dort an.";
    pub const CONTEXT: &str = "Zusätzliche Infos für den Agenten, z. B. Anforderungen, Links oder Hinweise. Wird dem Auftrag mitgegeben.";
    pub const FRONT: &str = "Der Auftrag wird vor alle anderen wartenden Aufträge gestellt.";
    pub const VERIFY: &str = "Ein Befehl, der nach jedem Auftrag prüft, ob alles funktioniert (z. B. die Tests). Schlägt er fehl, wird automatisch ein Reparatur-Auftrag angelegt.";
    pub const REVIEW: &str = "Ein zweiter Agent sieht sich die Änderungen mit frischem Blick an und lehnt sie bei Problemen ab.";
    pub const ONEMPTY: &str = "Ist die Warteschlange leer, kann die Queue selbstständig sinnvolle Arbeit vorschlagen – nur bei freiem Budget und wenn nichts auf dich wartet.";
    pub const TURNS: &str = "Ein Turn ist ein Arbeitsschritt des Agenten (eine Antwort inklusive Werkzeugaufrufen). Das Limit begrenzt, wie viele Schritte ein Lauf machen darf.";
    pub const TOKENS: &str = "Tokens sind die Abrechnungseinheit der KI-Anbieter (grob: ein Token ≈ ¾ Wort). Gezählt werden Eingabe, Ausgabe und Cache-Schreibvorgänge.";
    pub const LIMITS: &str = "Schutz vor ausufernden Läufen. Leer = Standard. Laufzeit und Stillstand gelten je Lauf, Kosten und Tokens für den ganzen Auftrag.";
    pub const BUDGET: &str = "Rollierende Fenster pro Anbieter. Ist ein Budget erreicht, startet die Queue für diesen Anbieter nichts Neues, bis genug Verbrauch aus dem Fenster gefallen ist.";
    pub const AUTO_APPROVE: &str = "Schlägt ein Agent mehr neue Aufträge vor als hier eingestellt, musst du sie erst freigeben.";
    pub const FIX: &str = "Wie oft die Queue nach einem fehlgeschlagenen Auftrag automatisch einen Reparatur-Auftrag startet, bevor sie stoppt und dich benachrichtigt.";
    pub const PARALLEL: &str = "Wie viele Agenten gleichzeitig arbeiten dürfen (projektübergreifend). Pro Projekt läuft immer nur einer.";
    pub const NTFY: &str = "ntfy ist ein kostenloser Push-Dienst. Mit einer geheimen Topic-URL bekommst du Benachrichtigungen aufs Handy, auch ohne installierte App.";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_has_label_help_and_tone() {
        for s in TaskStatus::ALL {
            assert!(!status_label(s).is_empty());
            assert!(status_help(s).ends_with('.'), "{s:?}");
        }
        assert_eq!(status_label(TaskStatus::WaitingInput), "Wartet auf dich");
        assert_eq!(status_tone(TaskStatus::Done), Tone::Success);
        assert_eq!(status_tone(TaskStatus::Blocked).class(), "tone-danger");
    }

    #[test]
    fn every_failure_class_has_label_and_help() {
        for c in FailureClass::ALL {
            assert!(!failure_label(c).is_empty());
            assert!(failure_help(c).ends_with('.'), "{c:?}");
        }
        assert_eq!(failure_label(FailureClass::Stalled), "Stillstand");
        assert_eq!(failure_label(FailureClass::LimitTokens), "Limit: Tokens");
    }

    #[test]
    fn providers_and_credentials() {
        assert_eq!(provider_label("gemini"), "Gemini");
        assert_eq!(provider_label("codex"), "Codex (GPT)");
        assert_eq!(provider_label("x"), "x");
        assert_eq!(credential_kind_label("api_key"), "API-Key");
        assert!(credential_help("claude", "oauth_token").contains("setup-token"));
        assert_eq!(credential_help("x", "y"), "");
        assert!(credential_is_json("auth_json") && credential_is_json("oauth_creds"));
        assert!(!credential_is_json("api_key"));
    }

    #[test]
    fn event_kinds_and_notifications() {
        assert_eq!(event_kind_label("verify"), "Prüfung");
        assert_eq!(event_kind_label("whatever"), "Info");
        assert!(notification_is_error("blocked"));
        assert!(!notification_is_error("question"));
        assert_eq!(task_kind_label("fix"), "Reparatur");
        let ids: Vec<&str> = ONEMPTY_ACTIONS.iter().map(|a| a.0).collect();
        assert_eq!(ids, queued_api::ONEMPTY_ACTIONS.to_vec());
    }
}
