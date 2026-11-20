//! WP10: notification events, channels, quiet hours and the daily summary
//! (shared by the app and the web frontend).
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a notification is about – the unit users switch on and off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    NeedsInput,
    Approval,
    Blocked,
    TaskDone,
    Provider,
    PrAttention,
    PrUpdate,
    Automation,
    Intake,
    Followups,
    Knowledge,
    Digest,
}

/// Where a notification goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Postfach in the app (and the live update).
    Inapp,
    /// Web Push to installed apps/browsers.
    Push,
    /// ntfy topic (`ntfy_url` in the settings).
    Ntfy,
    /// Signed HTTP POST to the user's webhook.
    Webhook,
}

impl Channel {
    pub const ALL: [Channel; 4] = [Self::Inapp, Self::Push, Self::Ntfy, Self::Webhook];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inapp => "inapp",
            Self::Push => "push",
            Self::Ntfy => "ntfy",
            Self::Webhook => "webhook",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inapp => "Postfach",
            Self::Push => "Push",
            Self::Ntfy => "ntfy",
            Self::Webhook => "Webhook",
        }
    }

    /// Delivered outside the app (outbox, quiet hours apply except webhooks).
    pub fn external(self) -> bool {
        self != Self::Inapp
    }
}

impl Event {
    pub const ALL: [Event; 12] = [
        Self::NeedsInput,
        Self::Approval,
        Self::Blocked,
        Self::TaskDone,
        Self::Provider,
        Self::PrAttention,
        Self::PrUpdate,
        Self::Automation,
        Self::Intake,
        Self::Followups,
        Self::Knowledge,
        Self::Digest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsInput => "needs_input",
            Self::Approval => "approval",
            Self::Blocked => "blocked",
            Self::TaskDone => "task_done",
            Self::Provider => "provider",
            Self::PrAttention => "pr_attention",
            Self::PrUpdate => "pr_update",
            Self::Automation => "automation",
            Self::Intake => "intake",
            Self::Followups => "followups",
            Self::Knowledge => "knowledge",
            Self::Digest => "digest",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "Rückfrage des Agenten",
            Self::Approval => "Freigabe nötig",
            Self::Blocked => "Auftrag blockiert",
            Self::TaskDone => "Auftrag erledigt",
            Self::Provider => "Anbieter pausiert oder Budget erreicht",
            Self::PrAttention => "Pull Request braucht dich",
            Self::PrUpdate => "Neuigkeiten zum Pull Request",
            Self::Automation => "Automatisierung hat nicht gestartet",
            Self::Intake => "Anbindungen (GitHub, Slack, …)",
            Self::Followups => "Neue Folgeaufträge",
            Self::Knowledge => "Vorschlag für Projekt-Regeln",
            Self::Digest => "Tägliche Zusammenfassung",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::NeedsInput => "Der Agent kommt ohne deine Antwort nicht weiter.",
            Self::Approval => "Ein Plan, eine Änderung an geschützten Dateien oder neue Aufträge warten auf dein OK.",
            Self::Blocked => "Ein Auftrag ist gestoppt und braucht deine Hilfe.",
            Self::TaskDone => "Ein Auftrag ist fertig und geprüft.",
            Self::Provider => "Ein Anbieter-Limit, ein Anmeldeproblem oder dein Budget hält die Warteschlange an.",
            Self::PrAttention => "Konflikt, Runden-Grenze oder Git-Server nicht erreichbar.",
            Self::PrUpdate => "Pull Request angelegt, aktualisiert oder zusammengeführt.",
            Self::Automation => "Ein Zeitplan oder Webhook konnte keinen Auftrag starten.",
            Self::Intake => "Zuordnungen bestätigen, Rückmeldungen nicht zugestellt.",
            Self::Followups => "Agenten haben weitere Aufträge vorgeschlagen.",
            Self::Knowledge => "Ein Agent schlägt eine Änderung an den Projekt-Regeln vor.",
            Self::Digest => "Einmal am Tag: was erledigt wurde, was es gekostet hat und was auf dich wartet.",
        }
    }

    /// Needs the user now – may break through quiet hours.
    pub fn critical(self) -> bool {
        matches!(self, Self::NeedsInput | Self::Approval | Self::Blocked)
    }

    /// Default of a channel (= the behaviour before WP10: push and ntfy
    /// exactly where a notification needed attention; webhooks are opt-in).
    pub fn default_on(self, c: Channel) -> bool {
        match c {
            Channel::Inapp => true,
            Channel::Webhook => false,
            Channel::Push | Channel::Ntfy => matches!(
                self,
                Self::NeedsInput
                    | Self::Approval
                    | Self::Blocked
                    | Self::Provider
                    | Self::PrAttention
                    | Self::Automation
                    | Self::Digest
            ),
        }
    }

    /// Event of an internal notification kind; `push` is the call site's
    /// "needs attention" flag (splits kinds used for both). `None`: the
    /// push test, which ignores all settings.
    pub fn classify(kind: &str, push: bool) -> Option<Self> {
        Some(match kind {
            "test" => return None,
            "question" => Self::NeedsInput,
            "approval" | "held" | "gate" => Self::Approval,
            "blocked" => Self::Blocked,
            "done" => Self::TaskDone,
            "provider_paused" | "budget" | "limit" => Self::Provider,
            "pr" | "pr_fix" | "push_failed" if push => Self::PrAttention,
            "pr" | "pr_fix" | "push_failed" => Self::PrUpdate,
            "trigger" => Self::Automation,
            "intake" => Self::Intake,
            "followups" | "onempty" => Self::Followups,
            "knowledge" => Self::Knowledge,
            "digest" => Self::Digest,
            _ if push => Self::Blocked,
            _ => Self::TaskDone,
        })
    }
}

/// Minutes after midnight as `HH:MM`.
pub fn fmt_minute(m: i32) -> String {
    let m = m.clamp(0, 1439);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// `HH:MM` → minutes after midnight.
pub fn parse_minute(s: &str) -> Option<i32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

/// Is `minute` (local, after midnight) inside the quiet window? `start ==
/// end` = no quiet time; `start > end` spans midnight (22:00–07:00).
pub fn in_quiet(start: i32, end: i32, minute: i32) -> bool {
    if start == end {
        false
    } else if start < end {
        (start..end).contains(&minute)
    } else {
        minute >= start || minute < end
    }
}

/// Effective matrix `event → channel → on`.
pub type Matrix = BTreeMap<Event, BTreeMap<Channel, bool>>;

pub fn default_matrix() -> Matrix {
    Event::ALL
        .into_iter()
        .map(|e| {
            (
                e,
                Channel::ALL
                    .into_iter()
                    .map(|c| (c, e.default_on(c)))
                    .collect(),
            )
        })
        .collect()
}

/// `GET /api/notifications/settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// IANA zone of quiet hours and the daily summary.
    pub timezone: String,
    pub quiet_enabled: bool,
    /// Minutes after local midnight.
    pub quiet_start: i32,
    pub quiet_end: i32,
    /// Approval, question and blocked break through quiet hours.
    pub quiet_critical: bool,
    pub digest_enabled: bool,
    pub digest_minute: i32,
    pub webhook_url: Option<String>,
    /// A webhook secret exists (the secret itself is shown once only).
    pub webhook_secret_set: bool,
    pub matrix: Matrix,
    /// ntfy configured in the general settings.
    #[serde(default)]
    pub ntfy_configured: bool,
    /// Web Push subscriptions of this account.
    #[serde(default)]
    pub push_devices: i64,
}

/// `PUT /api/notifications/settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationSettingsUpdate {
    pub timezone: String,
    pub quiet_enabled: bool,
    pub quiet_start: i32,
    pub quiet_end: i32,
    pub quiet_critical: bool,
    pub digest_enabled: bool,
    pub digest_minute: i32,
    /// `None`/empty = no webhook.
    pub webhook_url: Option<String>,
    pub matrix: Matrix,
}

impl From<&NotificationSettings> for NotificationSettingsUpdate {
    fn from(s: &NotificationSettings) -> Self {
        Self {
            timezone: s.timezone.clone(),
            quiet_enabled: s.quiet_enabled,
            quiet_start: s.quiet_start,
            quiet_end: s.quiet_end,
            quiet_critical: s.quiet_critical,
            digest_enabled: s.digest_enabled,
            digest_minute: s.digest_minute,
            webhook_url: s.webhook_url.clone(),
            matrix: s.matrix.clone(),
        }
    }
}

/// `POST /api/notifications/webhook/secret` (shown once).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookSecret {
    pub secret: String,
}

pub const MAX_WEBHOOK_URL: usize = 500;

/// Checks the numbers and the URL form (the time zone is checked by the
/// app, which has the zone database).
pub fn validate(u: &NotificationSettingsUpdate) -> Result<(), String> {
    for (v, what) in [
        (u.quiet_start, "Beginn der Ruhezeit"),
        (u.quiet_end, "Ende der Ruhezeit"),
        (u.digest_minute, "Uhrzeit der Zusammenfassung"),
    ] {
        if !(0..1440).contains(&v) {
            return Err(format!("{what}: ungültige Uhrzeit"));
        }
    }
    if u.quiet_enabled && u.quiet_start == u.quiet_end {
        return Err("Ruhezeit: Beginn und Ende dürfen nicht gleich sein".into());
    }
    if let Some(w) = u
        .webhook_url
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty())
    {
        if w.len() > MAX_WEBHOOK_URL {
            return Err("Webhook-Adresse ist zu lang".into());
        }
        if !(w.starts_with("https://") || w.starts_with("http://")) {
            return Err("Webhook-Adresse muss mit https:// beginnen".into());
        }
    }
    Ok(())
}

pub const QUIET_HELP: &str = "In der Ruhezeit kommen keine Push- und ntfy-Nachrichten. Alles landet trotzdem im Postfach und in der täglichen Zusammenfassung. Webhooks sind Maschinen und werden weiter zugestellt.";
pub const DIGEST_HELP: &str = "Einmal am Tag zur gewählten Uhrzeit: erledigte und gestoppte Aufträge, Kosten, was auf dich wartet und was in der Ruhezeit zurückgehalten wurde. Ist nichts passiert, kommt keine Nachricht.";
pub const WEBHOOK_HELP: &str = "Die Queue schickt für die angehakten Ereignisse ein JSON per POST an diese Adresse. Prüfe die Signatur im Kopf X-Queue-Signature (t=<Zeit>,v1=<HMAC-SHA256 von \"<t>.<Inhalt>\" mit dem Secret>).";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_window_spans_midnight() {
        assert!(in_quiet(22 * 60, 7 * 60, 23 * 60));
        assert!(in_quiet(22 * 60, 7 * 60, 0));
        assert!(in_quiet(22 * 60, 7 * 60, 6 * 60 + 59));
        assert!(!in_quiet(22 * 60, 7 * 60, 7 * 60));
        assert!(!in_quiet(22 * 60, 7 * 60, 12 * 60));
        assert!(in_quiet(13 * 60, 14 * 60, 13 * 60 + 30));
        assert!(!in_quiet(13 * 60, 14 * 60, 14 * 60));
        assert!(!in_quiet(600, 600, 600), "equal start/end = no quiet time");
    }

    #[test]
    fn minutes_round_trip() {
        assert_eq!(fmt_minute(7 * 60 + 5), "07:05");
        assert_eq!(parse_minute("07:05"), Some(425));
        assert_eq!(parse_minute("24:00"), None);
        assert_eq!(parse_minute("7"), None);
    }

    #[test]
    fn classification_keeps_the_old_push_behaviour() {
        for (kind, push, ev) in [
            ("question", true, Event::NeedsInput),
            ("held", true, Event::Approval),
            ("gate", true, Event::Approval),
            ("approval", true, Event::Approval),
            ("blocked", true, Event::Blocked),
            ("done", false, Event::TaskDone),
            ("limit", true, Event::Provider),
            ("budget", true, Event::Provider),
            ("provider_paused", true, Event::Provider),
            ("pr", true, Event::PrAttention),
            ("pr", false, Event::PrUpdate),
            ("push_failed", false, Event::PrUpdate),
            ("trigger", true, Event::Automation),
            ("intake", false, Event::Intake),
            ("followups", false, Event::Followups),
            ("knowledge", false, Event::Knowledge),
        ] {
            let e = Event::classify(kind, push).unwrap();
            assert_eq!(e, ev, "{kind}");
            // Default external delivery = the call site's old push flag.
            assert_eq!(e.default_on(Channel::Push), push, "{kind}");
            assert_eq!(e.default_on(Channel::Ntfy), push, "{kind}");
            assert!(e.default_on(Channel::Inapp));
            assert!(!e.default_on(Channel::Webhook));
        }
        assert_eq!(Event::classify("test", true), None);
        for e in Event::ALL {
            assert_eq!(Event::parse(e.as_str()), Some(e));
        }
        assert!(Event::Blocked.critical() && !Event::TaskDone.critical());
    }

    #[test]
    fn validation() {
        let mut u = NotificationSettingsUpdate {
            timezone: "Europe/Berlin".into(),
            quiet_enabled: true,
            quiet_start: 1320,
            quiet_end: 420,
            quiet_critical: true,
            digest_enabled: true,
            digest_minute: 480,
            webhook_url: None,
            matrix: default_matrix(),
        };
        assert!(validate(&u).is_ok());
        u.quiet_end = 1320;
        assert!(validate(&u).is_err());
        u.quiet_end = 420;
        u.digest_minute = 1440;
        assert!(validate(&u).is_err());
        u.digest_minute = 0;
        u.webhook_url = Some("ftp://x".into());
        assert!(validate(&u).is_err());
        u.webhook_url = Some("https://hooks.example.com/q".into());
        assert!(validate(&u).is_ok());
        // Matrix keys survive JSON (enum keys as strings).
        let j = serde_json::to_string(&u).unwrap();
        assert!(j.contains("\"needs_input\":{\"inapp\":true"));
        assert_eq!(
            serde_json::from_str::<NotificationSettingsUpdate>(&j).unwrap(),
            u
        );
    }
}
