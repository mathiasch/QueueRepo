//! WP08: intake integrations – GitHub issues, Linear agent sessions, Slack
//! and Jira. Kinds, texts, validation and wire types shared by the app and
//! the web UI.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Most characters of the request text that goes into a task (issue body,
/// Slack message, Jira description, Linear prompt context).
pub const MAX_REQUEST_CHARS: usize = 8_000;
/// Most characters of the title shown with a link (untrusted, display only).
pub const MAX_TITLE_CHARS: usize = 300;
/// Most integrations per project.
pub const MAX_INTEGRATIONS: i64 = 20;
/// Most identities per integration.
pub const MAX_IDENTITIES: i64 = 500;
/// Body limit of `POST /api/intake/{id}`.
pub const MAX_BODY: usize = 1024 * 1024;
/// Slack: allowed difference of `X-Slack-Request-Timestamp` (seconds).
pub const SLACK_TOLERANCE_SECS: i64 = 300;
/// Linear: allowed difference of `webhookTimestamp` (milliseconds).
pub const LINEAR_TOLERANCE_MS: i64 = 60_000;
/// Default mention word (GitHub/Jira text, Slack slash command text).
pub const DEFAULT_MENTION: &str = "@queue";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeKind {
    Github,
    Linear,
    Slack,
    Jira,
}

impl IntakeKind {
    pub const ALL: [IntakeKind; 4] = [Self::Github, Self::Linear, Self::Slack, Self::Jira];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Linear => "linear",
            Self::Slack => "slack",
            Self::Jira => "jira",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub Issues",
            Self::Linear => "Linear",
            Self::Slack => "Slack",
            Self::Jira => "Jira",
        }
    }

    /// Short name of the source ("über Slack").
    pub fn short(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Linear => "Linear",
            Self::Slack => "Slack",
            Self::Jira => "Jira",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Github => "Ein Issue mit einem bestimmten Label oder einer Erwähnung wird zum Auftrag. Queue schreibt den Stand als Kommentar ins Issue, Rückfragen beantwortest du dort.",
            Self::Linear => "Du delegierst ein Linear-Issue an Queue (oder erwähnst es). Queue meldet Fortschritt und Ergebnis direkt in Linear, Rückfragen beantwortest du dort.",
            Self::Slack => "Erwähne Queue in einem Kanal oder nutze einen Slash-Befehl. Queue antwortet im Thread, Rückfragen beantwortest du im selben Thread.",
            Self::Jira => "Ein Jira-Issue mit einem bestimmten Label wird zum Auftrag. Queue schreibt den Stand als Kommentar, Rückfragen beantwortest du per Kommentar.",
        }
    }

    /// What the tenant field means.
    pub fn tenant_label(self) -> &'static str {
        match self {
            Self::Github => "Repository",
            Self::Linear => "Linear-Organisations-ID",
            Self::Slack => "Slack-Workspace-ID",
            Self::Jira => "Jira-Adresse",
        }
    }

    pub fn tenant_help(self) -> &'static str {
        match self {
            Self::Github => "Form „besitzer/name“, z. B. „acme/shop“. Nur Ereignisse aus genau diesem Repository werden angenommen.",
            Self::Linear => "Steht in Linear unter Einstellungen → Arbeitsbereich (eine lange ID mit Bindestrichen). Nur Ereignisse dieser Organisation werden angenommen.",
            Self::Slack => "Beginnt mit „T“, z. B. „T01ABCDEF“. Du findest sie in der Adresse von Slack im Browser (app.slack.com/client/T…). Nur dieser Workspace wird angenommen.",
            Self::Jira => "Die Adresse deiner Jira-Cloud-Site, z. B. „https://acme.atlassian.net“.",
        }
    }

    pub fn tenant_placeholder(self) -> &'static str {
        match self {
            Self::Github => "acme/shop",
            Self::Linear => "8f1c2d3e-…",
            Self::Slack => "T01ABCDEF",
            Self::Jira => "https://acme.atlassian.net",
        }
    }

    /// Queue creates the signing secret (GitHub, Jira); Slack and Linear
    /// create it themselves and it is copied into Queue.
    pub fn queue_makes_secret(self) -> bool {
        matches!(self, Self::Github | Self::Jira)
    }

    pub fn secret_label(self) -> &'static str {
        match self {
            Self::Github | Self::Jira => "Webhook-Secret",
            Self::Linear => "Webhook-Signing-Secret von Linear",
            Self::Slack => "Signing Secret der Slack-App",
        }
    }

    pub fn token_label(self) -> &'static str {
        match self {
            Self::Github => "GitHub-Token für Kommentare",
            Self::Linear => "Linear-Zugriffstoken der App (actor=app)",
            Self::Slack => "Bot-Token (xoxb-…)",
            Self::Jira => "Jira-API-Token",
        }
    }

    /// Minimal rights of the token (UI and README say the same).
    pub fn token_help(self) -> &'static str {
        match self {
            Self::Github => "Fine-grained Token nur für dieses Repository mit „Issues: Read and write“ (Metadata: Read kommt automatisch dazu). Ohne Token gibt es keine Kommentare zurück.",
            Self::Linear => "OAuth-Token der Linear-App mit actor=app und den Scopes „app:assignable“ und „app:mentionable“ (plus „read“, „write“). Ohne Token kann Queue in Linear nicht antworten – Linear zeigt die Sitzung dann als hängend.",
            Self::Slack => "Bot-Token der Slack-App mit den Scopes app_mentions:read, chat:write, commands, channels:history (und groups:history für private Kanäle).",
            Self::Jira => "API-Token eines eigenen Jira-Kontos für Queue (id.atlassian.com → Sicherheit → API-Token) mit Rechten „Durchsuchen“ und „Kommentieren“ im Jira-Projekt.",
        }
    }

    /// Whether the kind needs `account` (Jira: e-mail of the token account).
    pub fn needs_account(self) -> bool {
        matches!(self, Self::Jira)
    }

    /// What the mapping field "external ID" is.
    pub fn person_id_label(self) -> &'static str {
        match self {
            Self::Github => "GitHub-Nutzer-ID (Zahl)",
            Self::Linear => "Linear-Nutzer-ID",
            Self::Slack => "Slack-Mitglieds-ID (beginnt mit U oder W)",
            Self::Jira => "Atlassian-Konto-ID",
        }
    }

    pub fn person_id_help(self) -> &'static str {
        match self {
            Self::Github => "Steht unter https://api.github.com/users/NAME im Feld „id“. Die Zahl bleibt gleich, auch wenn jemand den Namen ändert.",
            Self::Linear => "In Linear: Profil öffnen → „…“ → „Copy user ID“.",
            Self::Slack => "In Slack: Profil öffnen → „⋮“ → „Mitglieds-ID kopieren“.",
            Self::Jira => "In Jira: Profil öffnen, die ID steht in der Adresse (…/people/ID).",
        }
    }
}

/// Settings of an integration (JSON column `config`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IntakeConfig {
    /// GitHub/Jira: issues with this label become tasks.
    pub label: Option<String>,
    /// GitHub/Jira: text that turns an issue or comment into a request
    /// (e.g. `@queue`). `None` = only the label.
    pub mention: Option<String>,
    /// People without an identity act as the integration's owner (their
    /// access). Default off. GitHub additionally requires the author to be
    /// owner, member or collaborator of the repository.
    pub allow_unmapped: bool,
    /// Extra instructions of the project owner for every task (trusted).
    pub instructions: Option<String>,
}

pub const MAX_INSTRUCTIONS_CHARS: usize = 2_000;

fn plain(s: &str) -> bool {
    !s.chars().any(|c| c.is_control())
}

/// Normalised tenant of a kind, `Err` = German reason.
pub fn validate_tenant(kind: IntakeKind, raw: &str) -> Result<String, String> {
    let t = raw.trim();
    match kind {
        IntakeKind::Github => {
            let t = t
                .trim_start_matches("https://github.com/")
                .trim_end_matches('/')
                .trim_end_matches(".git");
            let ok_part = |p: &str| {
                !p.is_empty()
                    && p.len() <= 100
                    && !p.starts_with('.')
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            };
            match t.split_once('/') {
                Some((o, r)) if ok_part(o) && ok_part(r) => Ok(t.to_ascii_lowercase()),
                _ => Err("Repository bitte als „besitzer/name“ angeben".into()),
            }
        }
        IntakeKind::Slack => {
            let ok = (3..=32).contains(&t.len())
                && matches!(t.as_bytes()[0], b'T' | b'E')
                && t.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
            if ok {
                Ok(t.to_string())
            } else {
                Err("Die Workspace-ID beginnt mit „T“ (oder „E“ bei Enterprise Grid) und enthält nur Großbuchstaben und Ziffern".into())
            }
        }
        IntakeKind::Linear => match Uuid::parse_str(t) {
            Ok(u) => Ok(u.hyphenated().to_string()),
            Err(_) => {
                Err("Die Organisations-ID ist eine lange ID mit Bindestrichen (UUID)".into())
            }
        },
        IntakeKind::Jira => {
            let t = t.trim_end_matches('/');
            let Some(host) = t.strip_prefix("https://") else {
                return Err("Die Jira-Adresse beginnt mit https://".into());
            };
            let ok = !host.is_empty()
                && host.len() <= 190
                && !host.starts_with('.')
                && !host.contains("..")
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b':'));
            if ok {
                Ok(format!("https://{}", host.to_ascii_lowercase()))
            } else {
                Err("Bitte nur die Adresse der Site angeben, z. B. https://acme.atlassian.net".into())
            }
        }
    }
}

/// Jira Cloud sites (the app allows other hosts only for tests/LAN).
pub fn is_atlassian_site(tenant: &str) -> bool {
    tenant
        .strip_prefix("https://")
        .is_some_and(|h| h.ends_with(".atlassian.net") && !h.contains(':'))
}

/// Label: 1–50 characters, no control characters.
pub fn validate_label(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > 50 || !plain(s) {
        return Err("Das Label hat 1–50 Zeichen".into());
    }
    if s.contains(' ') {
        return Err("Jira-Labels haben keine Leerzeichen – bitte ohne Leerzeichen".into());
    }
    Ok(s.to_string())
}

/// Mention word: 2–40 characters without spaces (e.g. `@queue`, `/queue`).
pub fn validate_mention(s: &str) -> Result<String, String> {
    let s = s.trim();
    let n = s.chars().count();
    if !(2..=40).contains(&n) || !plain(s) || s.contains(char::is_whitespace) {
        return Err("Die Erwähnung hat 2–40 Zeichen ohne Leerzeichen, z. B. „@queue“".into());
    }
    Ok(s.to_string())
}

/// Validates and normalises a config for `kind`.
pub fn validate_config(kind: IntakeKind, c: &IntakeConfig) -> Result<IntakeConfig, String> {
    let label = c
        .label
        .as_deref()
        .filter(|l| !l.trim().is_empty())
        .map(validate_label)
        .transpose()?;
    let mention = c
        .mention
        .as_deref()
        .filter(|l| !l.trim().is_empty())
        .map(validate_mention)
        .transpose()?;
    let instructions = c
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.chars().count() > MAX_INSTRUCTIONS_CHARS {
                Err(format!(
                    "Die Hinweise haben höchstens {MAX_INSTRUCTIONS_CHARS} Zeichen"
                ))
            } else {
                Ok(s.to_string())
            }
        })
        .transpose()?;
    if matches!(kind, IntakeKind::Github | IntakeKind::Jira) && label.is_none() && mention.is_none()
    {
        return Err("Bitte ein Label oder eine Erwähnung angeben – sonst würde jedes Issue zum Auftrag".into());
    }
    if kind == IntakeKind::Jira && mention.is_some() && label.is_none() {
        return Err("Für Jira bitte ein Label angeben (die Erwähnung gilt nur für Kommentare)".into());
    }
    Ok(IntakeConfig {
        // Slack and Linear have their own trigger (mention, delegation).
        label: label.filter(|_| matches!(kind, IntakeKind::Github | IntakeKind::Jira)),
        mention: mention.filter(|_| kind == IntakeKind::Github),
        allow_unmapped: c.allow_unmapped,
        instructions,
    })
}

/// External person IDs: 1–200 characters `[A-Za-z0-9:._@-]`.
pub fn valid_external_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'.' | b'_' | b'@' | b'-'))
}

/// Validates a person ID for `kind` (`Err` = German reason).
pub fn validate_external_id(kind: IntakeKind, s: &str) -> Result<String, String> {
    let s = s.trim();
    let ok = valid_external_id(s)
        && match kind {
            IntakeKind::Github => s.bytes().all(|b| b.is_ascii_digit()) && s.len() <= 20,
            IntakeKind::Slack => {
                matches!(s.as_bytes()[0], b'U' | b'W')
                    && s.bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            }
            IntakeKind::Linear => Uuid::parse_str(s).is_ok(),
            IntakeKind::Jira => true,
        };
    if ok {
        Ok(match kind {
            IntakeKind::Linear => Uuid::parse_str(s)
                .map(|u| u.hyphenated().to_string())
                .unwrap_or_default(),
            _ => s.to_string(),
        })
    } else {
        Err(format!(
            "Ungültige {}: {}",
            kind.person_id_label(),
            kind.person_id_help()
        ))
    }
}

pub fn validate_name(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > 80 || !plain(s) {
        return Err("Der Name hat 1–80 Zeichen".into());
    }
    Ok(s.to_string())
}

/// What happened to a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeOutcome {
    Created,
    Answered,
    Steered,
    Continued,
    Ignored,
    Rejected,
    Failed,
}

impl IntakeOutcome {
    pub const ALL: [IntakeOutcome; 7] = [
        Self::Created,
        Self::Answered,
        Self::Steered,
        Self::Continued,
        Self::Ignored,
        Self::Rejected,
        Self::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Answered => "answered",
            Self::Steered => "steered",
            Self::Continued => "continued",
            Self::Ignored => "ignored",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Created => "Auftrag angelegt",
            Self::Answered => "Rückfrage beantwortet",
            Self::Steered => "Nachricht an den Auftrag",
            Self::Continued => "Weiter mit …",
            Self::Ignored => "Ignoriert",
            Self::Rejected => "Abgewiesen",
            Self::Failed => "Fehlgeschlagen",
        }
    }

    pub fn tone(self) -> &'static str {
        match self {
            Self::Created | Self::Answered | Self::Steered | Self::Continued => "success",
            Self::Ignored => "info",
            Self::Rejected => "warning",
            Self::Failed => "danger",
        }
    }

    /// Whether the delivery's dedupe keys are kept (a refusal or failure
    /// keeps none: the sender may retry).
    pub fn keeps_keys(self) -> bool {
        !matches!(self, Self::Rejected | Self::Failed)
    }
}

/// Phase of a linked task as reported outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Accepted,
    Question,
    Done,
    Failed,
    Cancelled,
    /// A request that was refused (Linear only: the session would hang).
    Refused,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Question => "question",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Refused => "refused",
        }
    }

    /// Phase of a task status (`None` = nothing new to report).
    pub fn of_status(status: &str) -> Option<Self> {
        match status {
            "waiting_input" => Some(Self::Question),
            "done" => Some(Self::Done),
            "failed" | "blocked" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Accepted => "Angenommen",
            Self::Question => "Rückfrage",
            Self::Done => "Fertig",
            Self::Failed => "Fehlgeschlagen",
            Self::Cancelled => "Abgebrochen",
            Self::Refused => "Abgewiesen",
        }
    }
}

/// State of an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    Active,
    /// The person has to confirm (their access is used).
    WaitingUser,
    /// The project owner has to approve.
    WaitingOwner,
}

impl IdentityState {
    pub fn of(owner_ok: bool, user_ok: bool) -> Self {
        match (owner_ok, user_ok) {
            (true, true) => Self::Active,
            (true, false) => Self::WaitingUser,
            _ => Self::WaitingOwner,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "Aktiv",
            Self::WaitingUser => "Wartet auf Bestätigung der Person",
            Self::WaitingOwner => "Wartet auf Freigabe der Projekt-Eigentümerin",
        }
    }

    pub fn tone(self) -> &'static str {
        match self {
            Self::Active => "success",
            _ => "attention",
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntakeEventView {
    pub id: Uuid,
    pub outcome: IntakeOutcome,
    pub reason: Option<String>,
    pub event: Option<String>,
    pub external_user: Option<String>,
    pub task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: IntakeKind,
    pub name: String,
    pub tenant: String,
    pub enabled: bool,
    pub owner_id: Uuid,
    pub owner_name: String,
    pub config: IntakeConfig,
    pub max_per_hour: i32,
    /// A token for status updates is stored (never the token itself).
    pub has_token: bool,
    /// Jira: e-mail of the token account.
    pub account: Option<String>,
    /// Address for the webhook (Slack: also the slash command).
    pub hook_url: String,
    pub disabled_reason: Option<String>,
    pub active_people: i64,
    pub waiting_people: i64,
    /// Updates that could not be delivered (after all retries).
    pub failed_updates: i64,
    pub last_event: Option<IntakeEventView>,
    /// The caller may change it (project owner).
    pub can_manage: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST /api/projects/{id}/integrations`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewIntegration {
    pub kind: IntakeKind,
    pub name: String,
    pub tenant: String,
    /// Slack/Linear: the provider's signing secret (required). GitHub/Jira:
    /// ignored, Queue creates one.
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub config: IntakeConfig,
    #[serde(default)]
    pub max_per_hour: Option<i32>,
}

/// `PATCH /api/integrations/{id}` (absent = unchanged).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpdateIntegration {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `Some("")` removes the token.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    /// Slack/Linear: new signing secret.
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub config: Option<IntakeConfig>,
    #[serde(default)]
    pub max_per_hour: Option<i32>,
}

/// Answer of creating an integration or renewing its secret: the secret
/// Queue created is shown exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedIntegration {
    pub integration: IntegrationView,
    pub secret: Option<String>,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityView {
    pub id: Uuid,
    pub external_id: String,
    pub external_name: String,
    pub user_id: Uuid,
    pub user_name: String,
    pub state: IdentityState,
    /// The caller can give the missing approval/confirmation.
    pub can_confirm: bool,
    pub can_remove: bool,
    pub created_at: DateTime<Utc>,
}

/// `GET /api/integrations/{id}/identities`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identities {
    pub identities: Vec<IdentityView>,
    /// Members the caller may map (owner: every editor; others: themselves).
    pub mappable: Vec<crate::ProjectMemberRef>,
}

/// `POST /api/integrations/{id}/identities`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewIdentity {
    pub external_id: String,
    #[serde(default)]
    pub external_name: String,
    pub user_id: Uuid,
}

/// The external object a task came from (task detail).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntakeLinkView {
    pub kind: IntakeKind,
    pub integration_name: String,
    /// Trusted label, e.g. "GitHub-Issue #12".
    pub label: String,
    /// Untrusted title of the external object (display only).
    pub title: Option<String>,
    /// Only http(s).
    pub url: Option<String>,
    /// State of the last update sent outside.
    pub last_update: Option<String>,
}

/// Only http(s) links (shown as links in the UI).
pub fn safe_url(u: &str) -> Option<String> {
    let u = u.trim();
    ((u.starts_with("https://") || u.starts_with("http://"))
        && u.len() <= 500
        && !u.chars().any(|c| c.is_control() || c.is_whitespace()))
    .then(|| u.to_string())
}

/// Setup steps of a kind (UI after connecting, README).
pub fn setup_steps(kind: IntakeKind, hook_url: &str, cfg: &IntakeConfig) -> Vec<String> {
    let label = cfg.label.as_deref().unwrap_or("queue");
    match kind {
        IntakeKind::Github => vec![
            "Im Repository auf GitHub: Settings → Webhooks → Add webhook.".into(),
            format!("Payload URL: {hook_url}"),
            "Content type: application/json; Secret: das oben angezeigte Secret.".into(),
            "„Let me select individual events“: Issues und Issue comments anhaken, sonst nichts.".into(),
            match (&cfg.label, &cfg.mention) {
                (Some(l), Some(m)) => format!("Ein Issue wird zum Auftrag, wenn es das Label „{l}“ bekommt oder „{m}“ im Text bzw. in einem Kommentar steht."),
                (Some(l), None) => format!("Ein Issue wird zum Auftrag, wenn es das Label „{l}“ bekommt."),
                (None, Some(m)) => format!("Ein Issue wird zum Auftrag, wenn „{m}“ im Text oder in einem Kommentar steht."),
                (None, None) => String::new(),
            },
            "Für Kommentare zurück: ein Fine-grained Token nur für dieses Repository mit „Issues: Read and write“ eintragen.".into(),
            "Ordne unten die GitHub-Konten den Personen im Projekt zu – sonst nimmt Queue nichts an.".into(),
        ],
        IntakeKind::Linear => vec![
            "In Linear: Settings → API → OAuth Applications → neue App anlegen.".into(),
            format!("Webhook aktivieren, URL: {hook_url}; Ereignis „Agent session events“ anhaken."),
            "Das Webhook-Signing-Secret der App hier eintragen.".into(),
            "App mit actor=app und den Scopes app:assignable, app:mentionable, read, write im Workspace installieren und das Zugriffstoken hier eintragen.".into(),
            "Danach ein Issue an Queue delegieren oder Queue in einem Kommentar erwähnen.".into(),
            "Ordne unten die Linear-Konten den Personen im Projekt zu – sonst lehnt Queue ab.".into(),
        ],
        IntakeKind::Slack => vec![
            "Auf api.slack.com/apps eine App anlegen (From scratch) und im Workspace installieren.".into(),
            format!("Event Subscriptions einschalten, Request URL: {hook_url}; Bot-Events app_mention und message.channels (für Antworten im Thread) abonnieren."),
            format!("Optional: Slash Commands → z. B. „/queue“ mit derselben Request URL: {hook_url}"),
            "OAuth & Permissions: Bot-Scopes app_mentions:read, chat:write, commands, channels:history (groups:history für private Kanäle).".into(),
            "Signing Secret (Basic Information) und Bot-Token (xoxb-…) hier eintragen; die App in den Kanal einladen.".into(),
            "Ordne unten die Slack-Konten den Personen im Projekt zu – sonst nimmt Queue nichts an.".into(),
        ],
        IntakeKind::Jira => vec![
            "In Jira: Einstellungen → System → WebHooks → Webhook erstellen (Jira-Administration nötig).".into(),
            format!("URL: {hook_url}; Secret: das oben angezeigte Secret."),
            "Ereignisse: Vorgang erstellt, Vorgang aktualisiert, Kommentar erstellt; per JQL am besten auf das Projekt einschränken.".into(),
            format!("Ein Vorgang wird zum Auftrag, wenn er das Label „{label}“ hat."),
            "Für Kommentare zurück: E-Mail und API-Token eines Kontos mit „Kommentieren“-Recht eintragen.".into(),
            "Ordne unten die Atlassian-Konten den Personen im Projekt zu – sonst nimmt Queue nichts an.".into(),
        ],
    }
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_roundtrip_and_have_texts() {
        for k in IntakeKind::ALL {
            assert_eq!(IntakeKind::parse(k.as_str()), Some(k));
            assert_eq!(
                serde_json::to_string(&k).unwrap(),
                format!("\"{}\"", k.as_str())
            );
            for t in [
                k.label(),
                k.help(),
                k.tenant_label(),
                k.tenant_help(),
                k.token_label(),
                k.token_help(),
                k.person_id_help(),
            ] {
                assert!(!t.is_empty());
            }
        }
        assert_eq!(IntakeKind::parse("gitlab"), None);
        for o in IntakeOutcome::ALL {
            assert_eq!(IntakeOutcome::parse(o.as_str()), Some(o));
        }
        assert!(IntakeOutcome::Created.keeps_keys());
        assert!(IntakeOutcome::Ignored.keeps_keys());
        assert!(!IntakeOutcome::Rejected.keeps_keys());
        assert!(!IntakeOutcome::Failed.keeps_keys());
    }

    #[test]
    fn tenants_are_validated_and_normalised() {
        use IntakeKind::*;
        assert_eq!(validate_tenant(Github, " Acme/Shop ").unwrap(), "acme/shop");
        assert_eq!(
            validate_tenant(Github, "https://github.com/acme/shop.git").unwrap(),
            "acme/shop"
        );
        for bad in ["acme", "acme/", "/shop", "a/b/c", "acme/sh op", "../x"] {
            assert!(validate_tenant(Github, bad).is_err(), "{bad}");
        }
        assert_eq!(validate_tenant(Slack, "T01ABCDEF").unwrap(), "T01ABCDEF");
        assert_eq!(validate_tenant(Slack, "E0123456").unwrap(), "E0123456");
        for bad in ["t01abc", "U01ABCDEF", "T", "T01-ABC"] {
            assert!(validate_tenant(Slack, bad).is_err(), "{bad}");
        }
        let id = "8F1C2D3E-1111-4222-8333-444455556666";
        assert_eq!(
            validate_tenant(Linear, id).unwrap(),
            id.to_ascii_lowercase()
        );
        assert!(validate_tenant(Linear, "acme").is_err());
        assert_eq!(
            validate_tenant(Jira, "https://Acme.atlassian.net/").unwrap(),
            "https://acme.atlassian.net"
        );
        for bad in [
            "http://acme.atlassian.net",
            "https://acme.atlassian.net/jira",
            "https://user@acme.atlassian.net",
            "https://",
            "https://a..b",
        ] {
            assert!(validate_tenant(Jira, bad).is_err(), "{bad}");
        }
        assert!(is_atlassian_site("https://acme.atlassian.net"));
        assert!(!is_atlassian_site("https://acme.atlassian.net:8443"));
        assert!(!is_atlassian_site("https://evil.example"));
        assert!(!is_atlassian_site("https://atlassian.net.evil.example"));
    }

    #[test]
    fn config_needs_a_trigger_for_issue_trackers() {
        use IntakeKind::*;
        let none = IntakeConfig::default();
        assert!(validate_config(Github, &none).is_err());
        assert!(validate_config(Jira, &none).is_err());
        assert_eq!(validate_config(Slack, &none).unwrap(), none);
        let c = IntakeConfig {
            label: Some(" queue ".into()),
            mention: Some("@queue".into()),
            allow_unmapped: true,
            instructions: Some("  Bitte Tests schreiben  ".into()),
        };
        let v = validate_config(Github, &c).unwrap();
        assert_eq!(v.label.as_deref(), Some("queue"));
        assert_eq!(v.mention.as_deref(), Some("@queue"));
        assert_eq!(v.instructions.as_deref(), Some("Bitte Tests schreiben"));
        // Slack/Linear keep neither label nor mention.
        let v = validate_config(Linear, &c).unwrap();
        assert_eq!((v.label, v.mention), (None, None));
        assert!(validate_config(
            Jira,
            &IntakeConfig {
                mention: Some("@queue".into()),
                ..Default::default()
            }
        )
        .is_err());
        assert!(validate_label("two words").is_err());
        assert!(validate_label(&"x".repeat(51)).is_err());
        assert!(validate_mention("@").is_err());
        assert!(validate_mention("@ queue").is_err());
        assert!(validate_config(
            Github,
            &IntakeConfig {
                label: Some("q".into()),
                instructions: Some("x".repeat(MAX_INSTRUCTIONS_CHARS + 1)),
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn person_ids_per_kind() {
        use IntakeKind::*;
        assert_eq!(validate_external_id(Github, " 583231 ").unwrap(), "583231");
        assert!(validate_external_id(Github, "octocat").is_err());
        assert!(validate_external_id(Slack, "U2CERLKJA").is_ok());
        assert!(validate_external_id(Slack, "W012A3CDE").is_ok());
        assert!(validate_external_id(Slack, "T2CERLKJA").is_err());
        assert!(validate_external_id(Linear, "not-a-uuid").is_err());
        assert!(validate_external_id(Jira, "557058:f58131cb-b67d-43c7-b30d-6b58d40bd077").is_ok());
        assert!(validate_external_id(Jira, "a b").is_err());
        assert!(validate_external_id(Jira, "").is_err());
    }

    #[test]
    fn phases_and_states() {
        assert_eq!(Phase::of_status("waiting_input"), Some(Phase::Question));
        assert_eq!(Phase::of_status("blocked"), Some(Phase::Failed));
        assert_eq!(Phase::of_status("running"), None);
        assert_eq!(Phase::of_status("queued"), None);
        assert_eq!(IdentityState::of(true, true), IdentityState::Active);
        assert_eq!(IdentityState::of(true, false), IdentityState::WaitingUser);
        assert_eq!(IdentityState::of(false, true), IdentityState::WaitingOwner);
        assert_eq!(safe_url("javascript:alert(1)"), None);
        assert_eq!(safe_url("https://x.example/a b"), None);
        assert_eq!(
            safe_url("https://github.com/a/b/issues/1").as_deref(),
            Some("https://github.com/a/b/issues/1")
        );
    }

    #[test]
    fn setup_steps_name_the_address_and_minimal_rights() {
        let cfg = IntakeConfig {
            label: Some("queue".into()),
            ..Default::default()
        };
        for k in IntakeKind::ALL {
            let steps = setup_steps(k, "https://q.example/api/intake/x", &cfg);
            assert!(steps.iter().any(|s| s.contains("https://q.example/api/intake/x")));
            assert!(steps.iter().all(|s| !s.is_empty()));
        }
    }
}
