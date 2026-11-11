//! WP07: personal API tokens – format (prefix + checksum, recognisable by
//! secret scanners), scopes, the table of routes a token may call (default
//! deny) and the wire types. Shared by the app (enforcement), the web UI
//! (labels) and `queuectl` (offline format check).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Prefix of personal API tokens (`qk_…`).
pub const TOKEN_PREFIX: &str = "qk_";
/// Prefix of webhook secrets (`qwh_…`).
pub const SECRET_PREFIX: &str = "qwh_";
/// Random part: 43 base62 characters ≈ 256 bit.
pub const BODY_LEN: usize = 43;
/// Base62 CRC32 of the random part.
pub const CHECK_LEN: usize = 6;
/// Characters of the visible prefix in lists (`qk_` + 6).
pub const SHOWN_LEN: usize = 6;
/// Tokens per user (active and revoked rows count).
pub const MAX_TOKENS: i64 = 50;
/// Longest validity of a token (days).
pub const MAX_EXPIRY_DAYS: i32 = 366;
/// Choices in the UI (days).
pub const EXPIRY_CHOICES: [(i32, &str); 4] = [
    (7, "7 Tage"),
    (30, "30 Tage"),
    (90, "90 Tage (Standard)"),
    (365, "1 Jahr"),
];
pub const MAX_NAME_CHARS: usize = 80;

pub const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// CRC-32 (IEEE, reflected, poly 0xEDB88320) – the checksum of tokens.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Six base62 characters of the CRC32 of `body` (62^6 > 2^32).
pub fn checksum(body: &str) -> String {
    let mut n = u64::from(crc32(body.as_bytes()));
    let mut out = [b'0'; CHECK_LEN];
    for slot in out.iter_mut().rev() {
        *slot = BASE62[(n % 62) as usize];
        n /= 62;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `prefix` + `body` + checksum (`body` = [`BODY_LEN`] base62 characters).
pub fn compose(prefix: &str, body: &str) -> String {
    format!("{prefix}{body}{}", checksum(body))
}

/// Right prefix, length, alphabet and checksum. Says nothing about whether
/// the token exists or is valid.
pub fn well_formed(s: &str, prefix: &str) -> bool {
    let Some(rest) = s.strip_prefix(prefix) else {
        return false;
    };
    if rest.len() != BODY_LEN + CHECK_LEN || !rest.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    let (body, check) = rest.split_at(BODY_LEN);
    checksum(body) == check
}

/// Visible part of a token in lists (`qk_AbC123`).
pub fn shown_prefix(token: &str) -> String {
    token.chars().take(TOKEN_PREFIX.len() + SHOWN_LEN).collect()
}

/// What a token may do. Every route a token may call needs exactly one of
/// these (see [`ROUTES`]); everything else is never reachable with a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    #[serde(rename = "projects:read")]
    ProjectsRead,
    #[serde(rename = "tasks:read")]
    TasksRead,
    #[serde(rename = "tasks:write")]
    TasksWrite,
    #[serde(rename = "tasks:steer")]
    TasksSteer,
    #[serde(rename = "triggers:read")]
    TriggersRead,
    #[serde(rename = "triggers:write")]
    TriggersWrite,
    #[serde(rename = "triggers:fire")]
    TriggersFire,
}

impl Scope {
    pub const ALL: [Scope; 7] = [
        Self::ProjectsRead,
        Self::TasksRead,
        Self::TasksWrite,
        Self::TasksSteer,
        Self::TriggersRead,
        Self::TriggersWrite,
        Self::TriggersFire,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectsRead => "projects:read",
            Self::TasksRead => "tasks:read",
            Self::TasksWrite => "tasks:write",
            Self::TasksSteer => "tasks:steer",
            Self::TriggersRead => "triggers:read",
            Self::TriggersWrite => "triggers:write",
            Self::TriggersFire => "triggers:fire",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ProjectsRead => "Projekte lesen",
            Self::TasksRead => "Aufträge lesen",
            Self::TasksWrite => "Aufträge anlegen und abbrechen",
            Self::TasksSteer => "Aufträge steuern",
            Self::TriggersRead => "Automatisierungen lesen",
            Self::TriggersWrite => "Automatisierungen ändern",
            Self::TriggersFire => "Auslöser starten",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::ProjectsRead => "Projektliste, Projektdaten und Vorlagen.",
            Self::TasksRead => "Aufträge, Protokolle, Änderungen und Nachrichten ansehen; Live-Updates.",
            Self::TasksWrite => "Neue Aufträge anlegen, wartende ändern, abbrechen und wiederholen. Kostet Guthaben bzw. Kontingent.",
            Self::TasksSteer => "Nachrichten an den Agenten, „Weiter mit …“, Fragen beantworten, Pläne genehmigen, ändern oder verwerfen.",
            Self::TriggersRead => "Vorlagen, Zeitpläne, Webhooks und ihren Verlauf ansehen.",
            Self::TriggersWrite => "Vorlagen und Auslöser anlegen, ändern und löschen (inkl. neuer Webhook-Secrets).",
            Self::TriggersFire => "Eigene Auslöser per API starten (POST /api/triggers/{id}/fire).",
        }
    }
}

/// Routes a token may call: `(method, matched route, scope)`; scope `None`
/// = any valid token. A route that is not listed answers 403 to every
/// token – account, settings, credentials, token management, members,
/// secrets and gate decisions are deliberately missing.
pub const ROUTES: &[(&str, &str, Option<Scope>)] = &[
    ("GET", "/api/whoami", None),
    ("GET", "/api/projects", Some(Scope::ProjectsRead)),
    ("GET", "/api/projects/{id}", Some(Scope::ProjectsRead)),
    (
        "GET",
        "/api/projects/{id}/templates",
        Some(Scope::ProjectsRead),
    ),
    ("GET", "/api/tasks", Some(Scope::TasksRead)),
    ("GET", "/api/tasks/{id}", Some(Scope::TasksRead)),
    ("GET", "/api/projects/{id}/tasks", Some(Scope::TasksRead)),
    ("GET", "/api/tasks/{id}/diff", Some(Scope::TasksRead)),
    ("GET", "/api/tasks/{id}/messages", Some(Scope::TasksRead)),
    ("GET", "/api/events", Some(Scope::TasksRead)),
    ("POST", "/api/tasks", Some(Scope::TasksWrite)),
    ("PATCH", "/api/tasks/{id}", Some(Scope::TasksWrite)),
    ("POST", "/api/tasks/{id}/cancel", Some(Scope::TasksWrite)),
    ("POST", "/api/tasks/{id}/retry", Some(Scope::TasksWrite)),
    ("POST", "/api/tasks/{id}/messages", Some(Scope::TasksSteer)),
    (
        "DELETE",
        "/api/tasks/{id}/messages/{mid}",
        Some(Scope::TasksSteer),
    ),
    ("POST", "/api/tasks/{id}/continue", Some(Scope::TasksSteer)),
    ("POST", "/api/tasks/{id}/answer", Some(Scope::TasksSteer)),
    (
        "POST",
        "/api/tasks/{id}/plan/approve",
        Some(Scope::TasksSteer),
    ),
    (
        "POST",
        "/api/tasks/{id}/plan/revise",
        Some(Scope::TasksSteer),
    ),
    (
        "POST",
        "/api/tasks/{id}/plan/reject",
        Some(Scope::TasksSteer),
    ),
    (
        "GET",
        "/api/projects/{id}/triggers",
        Some(Scope::TriggersRead),
    ),
    ("GET", "/api/triggers/{id}", Some(Scope::TriggersRead)),
    ("GET", "/api/triggers/{id}/runs", Some(Scope::TriggersRead)),
    ("GET", "/api/timezones", Some(Scope::TriggersRead)),
    ("POST", "/api/triggers/preview", Some(Scope::TriggersRead)),
    (
        "POST",
        "/api/projects/{id}/templates",
        Some(Scope::TriggersWrite),
    ),
    ("PATCH", "/api/templates/{id}", Some(Scope::TriggersWrite)),
    ("DELETE", "/api/templates/{id}", Some(Scope::TriggersWrite)),
    (
        "POST",
        "/api/projects/{id}/triggers",
        Some(Scope::TriggersWrite),
    ),
    ("PATCH", "/api/triggers/{id}", Some(Scope::TriggersWrite)),
    ("DELETE", "/api/triggers/{id}", Some(Scope::TriggersWrite)),
    (
        "POST",
        "/api/triggers/{id}/secret",
        Some(Scope::TriggersWrite),
    ),
    ("POST", "/api/triggers/{id}/fire", Some(Scope::TriggersFire)),
];

/// What a token needs for `method route`: `None` = never allowed,
/// `Some(None)` = any valid token, `Some(Some(s))` = scope `s`.
pub fn required(method: &str, route: &str) -> Option<Option<Scope>> {
    ROUTES
        .iter()
        .find(|(m, r, _)| m.eq_ignore_ascii_case(method) && *r == route)
        .map(|(_, _, s)| *s)
}

/// One token as listed (never the secret itself).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiTokenView {
    pub id: Uuid,
    pub name: String,
    /// `qk_` + the first characters, to recognise the token.
    pub prefix: String,
    pub scopes: Vec<Scope>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiTokenView {
    pub fn active_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

/// `POST /api/tokens`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewApiToken {
    pub name: String,
    pub scopes: Vec<Scope>,
    pub expires_in_days: i32,
}

/// Answer of `POST /api/tokens`: the token is shown exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedApiToken {
    pub token: String,
    pub info: ApiTokenView,
}

/// `GET /api/whoami` (any valid token).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhoAmI {
    pub user_id: Uuid,
    pub name: String,
    pub email: String,
    pub token: ApiTokenView,
}

/// Checks a new token request; returns the trimmed name and sorted scopes.
pub fn validate_new(t: &NewApiToken) -> Result<(String, Vec<Scope>), String> {
    let name = t.name.trim();
    if name.is_empty() {
        return Err("Name darf nicht leer sein".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "Name ist zu lang (höchstens {MAX_NAME_CHARS} Zeichen)"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("Name enthält ungültige Zeichen".into());
    }
    let mut scopes = t.scopes.clone();
    scopes.sort();
    scopes.dedup();
    if scopes.is_empty() {
        return Err("Mindestens eine Berechtigung wählen".into());
    }
    if !(1..=MAX_EXPIRY_DAYS).contains(&t.expires_in_days) {
        return Err(format!(
            "Gültigkeit: 1 bis {MAX_EXPIRY_DAYS} Tage (Tokens ohne Ablaufdatum gibt es nicht)"
        ));
    }
    Ok((name.to_string(), scopes))
}

pub const TOKENS_HELP: &str = "Mit einem API-Token können Skripte, CI-Systeme und das Kommandozeilen-Werkzeug queuectl in deinem Namen arbeiten – nur mit den Berechtigungen, die du hier auswählst, und nur bis zum Ablaufdatum. Konto, Zugänge, Einstellungen und andere Tokens lassen sich mit einem Token nie ändern.";
pub const SHOW_ONCE: &str = "Kopiere das Token jetzt. Queue speichert nur einen Fingerabdruck davon und kann es dir später nicht mehr zeigen.";

#[cfg(test)]
mod tests {
    use super::*;

    fn body(seed: u8) -> String {
        (0..BODY_LEN)
            .map(|i| BASE62[(usize::from(seed) * 7 + i * 13) % 62] as char)
            .collect()
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // The CRC-32/ISO-HDLC check value of "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn composed_tokens_are_well_formed_and_typos_are_caught() {
        let t = compose(TOKEN_PREFIX, &body(1));
        assert_eq!(t.len(), TOKEN_PREFIX.len() + BODY_LEN + CHECK_LEN);
        assert!(well_formed(&t, TOKEN_PREFIX));
        assert!(!well_formed(&t, SECRET_PREFIX), "other prefix");
        // One changed character anywhere breaks the checksum.
        for i in TOKEN_PREFIX.len()..t.len() {
            let mut b = t.clone().into_bytes();
            b[i] = if b[i] == b'a' { b'b' } else { b'a' };
            let s = String::from_utf8(b).unwrap();
            assert!(!well_formed(&s, TOKEN_PREFIX), "typo at {i}: {s}");
        }
        assert!(!well_formed(&t[..t.len() - 1], TOKEN_PREFIX), "too short");
        assert!(!well_formed(&format!("{t}0"), TOKEN_PREFIX), "too long");
        assert!(!well_formed(&t.replace('_', "-"), TOKEN_PREFIX));
        let s = compose(SECRET_PREFIX, &body(2));
        assert!(well_formed(&s, SECRET_PREFIX));
        assert_eq!(shown_prefix(&t), t[..9]);
    }

    #[test]
    fn scopes_roundtrip_and_parse() {
        for s in Scope::ALL {
            assert_eq!(Scope::parse(s.as_str()), Some(s));
            let j = serde_json::to_string(&s).unwrap();
            assert_eq!(j, format!("\"{}\"", s.as_str()));
            assert_eq!(serde_json::from_str::<Scope>(&j).unwrap(), s);
            assert!(!s.label().is_empty() && !s.help().is_empty());
        }
        assert_eq!(Scope::parse("admin"), None);
        assert!(serde_json::from_str::<Scope>("\"admin\"").is_err());
    }

    #[test]
    fn route_table_is_unique_and_default_deny() {
        let mut keys: Vec<_> = ROUTES.iter().map(|(m, r, _)| (*m, *r)).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), ROUTES.len(), "duplicate route");
        assert_eq!(
            required("POST", "/api/tasks"),
            Some(Some(Scope::TasksWrite))
        );
        assert_eq!(required("get", "/api/whoami"), Some(None));
        for never in [
            ("GET", "/api/tokens"),
            ("POST", "/api/tokens"),
            ("POST", "/api/tokens/{id}/revoke"),
            ("GET", "/api/settings"),
            ("PUT", "/api/settings"),
            ("GET", "/api/auth/me"),
            ("PUT", "/api/credentials/{provider}"),
            ("POST", "/api/connectors"),
            ("POST", "/api/projects/{id}/members"),
            ("PUT", "/api/projects/{id}/secrets/{name}"),
            ("POST", "/api/tasks/{id}/gate/approve"),
            ("DELETE", "/api/projects/{id}"),
            ("POST", "/api/hooks/{id}"),
        ] {
            assert_eq!(required(never.0, never.1), None, "{never:?}");
        }
        // Every scope is used by at least one route.
        for s in Scope::ALL {
            assert!(ROUTES.iter().any(|(_, _, x)| *x == Some(s)), "{s:?}");
        }
    }

    #[test]
    fn new_token_validation() {
        let ok = NewApiToken {
            name: "  CI  ".into(),
            scopes: vec![Scope::TasksWrite, Scope::TasksRead, Scope::TasksRead],
            expires_in_days: 90,
        };
        let (n, s) = validate_new(&ok).unwrap();
        assert_eq!(n, "CI");
        assert_eq!(s, vec![Scope::TasksRead, Scope::TasksWrite]);
        for bad in [
            NewApiToken {
                name: " ".into(),
                ..ok.clone()
            },
            NewApiToken {
                scopes: vec![],
                ..ok.clone()
            },
            NewApiToken {
                expires_in_days: 0,
                ..ok.clone()
            },
            NewApiToken {
                expires_in_days: 367,
                ..ok.clone()
            },
            NewApiToken {
                name: "x".repeat(81),
                ..ok.clone()
            },
            NewApiToken {
                name: "a\nb".into(),
                ..ok.clone()
            },
        ] {
            assert!(validate_new(&bad).is_err(), "{bad:?}");
        }
    }
}
