//! WP11: wire types and texts of MFA, sessions, the admin console, the audit
//! log and the runner fleet. Shared by the app (serialization, validation)
//! and the web UI (labels, the same validation before sending).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Minimum password length (registration, password change).
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 256;
/// Number of recovery codes per set.
pub const RECOVERY_CODES: usize = 10;
/// Characters of a recovery code (without the dash).
pub const RECOVERY_CODE_LEN: usize = 10;
/// Base32 without look-alikes (no 0/O, 1/I/L) for recovery codes.
pub const RECOVERY_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
/// Passkeys per account.
pub const MAX_PASSKEYS: i64 = 20;
pub const MAX_PASSKEY_NAME: usize = 60;
/// Invitation validity (days).
pub const INVITE_DAYS: [(i32, &str); 3] = [(1, "1 Tag"), (7, "7 Tage (Standard)"), (30, "30 Tage")];
pub const MAX_INVITE_DAYS: i32 = 30;
pub const MAX_RUNNER_NAME: usize = 60;
/// Entries per page of the audit log.
pub const AUDIT_PAGE: i64 = 50;
/// Minimum retention of the audit log (enforced by the database trigger).
pub const AUDIT_MIN_RETENTION_DAYS: i64 = 365;

pub fn validate_password(pw: &str) -> Result<(), String> {
    let n = pw.chars().count();
    if n < MIN_PASSWORD_LEN {
        return Err(format!(
            "Das Passwort braucht mindestens {MIN_PASSWORD_LEN} Zeichen."
        ));
    }
    if pw.len() > MAX_PASSWORD_LEN {
        return Err(format!(
            "Das Passwort darf höchstens {MAX_PASSWORD_LEN} Zeichen haben."
        ));
    }
    Ok(())
}

/// Canonical form of a typed recovery code: upper case, without spaces and
/// dashes; `None` if it cannot be one.
pub fn normalize_recovery_code(input: &str) -> Option<String> {
    let s: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (s.len() == RECOVERY_CODE_LEN && s.bytes().all(|b| RECOVERY_ALPHABET.contains(&b))).then_some(s)
}

/// `XXXXX-XXXXX` for display.
pub fn format_recovery_code(code: &str) -> String {
    if code.len() == RECOVERY_CODE_LEN {
        format!("{}-{}", &code[..5], &code[5..])
    } else {
        code.to_string()
    }
}

/// A TOTP code as typed: six digits, spaces allowed.
pub fn normalize_totp(input: &str) -> Option<String> {
    let s: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    (s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit())).then_some(s)
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

/// Second factors a person can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MfaMethod {
    Totp,
    Webauthn,
    Recovery,
}

/// Answer of password/OIDC login: either a session (`ok`) or the second step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginOutcome {
    pub ok: bool,
    #[serde(default)]
    pub mfa_required: bool,
    #[serde(default)]
    pub methods: Vec<MfaMethod>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfaCode {
    pub code: String,
}

/// WebAuthn options for `navigator.credentials.create/get` (JSON of
/// webauthn-rs: binary fields as base64url strings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebauthnChallenge {
    pub options: Value,
}

/// The browser's answer (base64url fields), plus a name for new passkeys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebauthnResponse {
    pub credential: Value,
    #[serde(default)]
    pub name: String,
}

// ---------------------------------------------------------------------------
// Account security
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PasskeyInfo {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityOverview {
    pub totp_enabled: bool,
    pub passkeys: Vec<PasskeyInfo>,
    pub recovery_left: i64,
    /// The account has a password (OIDC-only accounts have none).
    pub password_set: bool,
    /// The instance policy requires a second factor for this person.
    pub mfa_required: bool,
}

impl SecurityOverview {
    pub fn mfa_enabled(&self) -> bool {
        self.totp_enabled || !self.passkeys.is_empty()
    }
}

/// Re-authentication for sensitive changes: the password or a current
/// second factor (TOTP code, recovery code).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StepUp {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpSetup {
    /// Base32 seed for manual entry.
    pub secret: String,
    /// `otpauth://totp/…` for the QR code.
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpConfirm {
    pub code: String,
}

/// Shown exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryCodes {
    pub codes: Vec<String>,
}

/// Result of enabling a factor: new recovery codes if there were none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorAdded {
    #[serde(default)]
    pub recovery_codes: Option<Vec<String>>,
    /// Other sessions signed out.
    pub revoked_sessions: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePassword {
    #[serde(default)]
    pub step_up: StepUp,
    pub new_password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartPasskey {
    #[serde(default)]
    pub step_up: StepUp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub user_agent: String,
    pub auth_method: String,
    pub current: bool,
}

/// Short device description from a user agent ("Firefox auf Linux").
pub fn describe_user_agent(ua: &str) -> String {
    if ua.trim().is_empty() {
        return "Unbekanntes Gerät".into();
    }
    let l = ua.to_lowercase();
    let browser = if l.contains("edg/") {
        "Edge"
    } else if l.contains("firefox/") {
        "Firefox"
    } else if l.contains("chrome/") || l.contains("crios/") {
        "Chrome"
    } else if l.contains("safari/") {
        "Safari"
    } else if l.contains("queuectl") {
        "queuectl"
    } else if l.contains("curl/") {
        "curl"
    } else {
        "Browser"
    };
    let os = if l.contains("iphone") {
        "iPhone"
    } else if l.contains("ipad") {
        "iPad"
    } else if l.contains("android") {
        "Android"
    } else if l.contains("mac os") || l.contains("macintosh") {
        "macOS"
    } else if l.contains("windows") {
        "Windows"
    } else if l.contains("linux") {
        "Linux"
    } else {
        ""
    };
    if os.is_empty() {
        browser.into()
    } else {
        format!("{browser} auf {os}")
    }
}

/// Readable sign-in method (`password+totp` → "Passwort + Authenticator-App").
pub fn describe_auth_method(m: &str) -> String {
    m.split('+')
        .map(|p| match p {
            "password" => "Passwort".to_string(),
            "totp" => "Authenticator-App".to_string(),
            "webauthn" => "Passkey".to_string(),
            "recovery" => "Wiederherstellungscode".to_string(),
            "invite" => "Einladung".to_string(),
            "register" => "Registrierung".to_string(),
            o if o.starts_with("oidc:") => format!("Anmeldung über {}", &o[5..]),
            o => o.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

// ---------------------------------------------------------------------------
// Admin console
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    Open,
    Invite,
    Closed,
}

impl RegistrationMode {
    pub const ALL: [RegistrationMode; 3] = [Self::Open, Self::Invite, Self::Closed];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Invite => "invite",
            Self::Closed => "closed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Offen",
            Self::Invite => "Nur mit Einladung",
            Self::Closed => "Geschlossen",
        }
    }
    pub fn help(self) -> &'static str {
        match self {
            Self::Open => "Jede Person mit dem Link kann sich ein Konto anlegen.",
            Self::Invite => "Neue Konten nur über einen Einladungslink, den ein Admin erstellt.",
            Self::Closed => "Niemand kann sich neu registrieren. Bestehende Konten bleiben.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MfaPolicy {
    Off,
    Admins,
    All,
}

impl MfaPolicy {
    pub const ALL: [MfaPolicy; 3] = [Self::Off, Self::Admins, Self::All];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Admins => "admins",
            Self::All => "all",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Freiwillig",
            Self::Admins => "Pflicht für Admins",
            Self::All => "Pflicht für alle",
        }
    }
    pub fn applies(self, is_admin: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Admins => is_admin,
            Self::All => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceSettings {
    pub registration_mode: RegistrationMode,
    /// Not chosen in the console yet: follows `ALLOW_REGISTRATION`.
    pub registration_from_env: bool,
    pub mfa_policy: MfaPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateInstanceSettings {
    #[serde(default)]
    pub registration_mode: Option<RegistrationMode>,
    #[serde(default)]
    pub mfa_policy: Option<MfaPolicy>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    #[serde(default)]
    pub max_cost_30d_usd: Option<f64>,
    #[serde(default)]
    pub max_parallel: Option<i32>,
    #[serde(default)]
    pub max_projects: Option<i32>,
}

impl Quota {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(c) = self.max_cost_30d_usd {
            if !c.is_finite() || !(0.0..=1_000_000.0).contains(&c) {
                return Err("Kostengrenze: 0 bis 1 000 000 USD.".into());
            }
        }
        if let Some(p) = self.max_parallel {
            if !(1..=64).contains(&p) {
                return Err("Gleichzeitige Aufträge: 1 bis 64.".into());
            }
        }
        if let Some(p) = self.max_projects {
            if !(0..=10_000).contains(&p) {
                return Err("Projekte: 0 bis 10 000.".into());
            }
        }
        Ok(())
    }
    pub fn is_unlimited(&self) -> bool {
        self.max_cost_30d_usd.is_none()
            && self.max_parallel.is_none()
            && self.max_projects.is_none()
    }
}

/// The quota of the signed-in person and how much of it is used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaStatus {
    pub quota: Quota,
    pub cost_30d_usd: f64,
    pub projects: i64,
    /// The cost quota is used up: no new tasks start.
    pub exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminUser {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
    pub disabled_at: Option<DateTime<Utc>>,
    pub disabled_reason: String,
    pub created_at: DateTime<Utc>,
    pub mfa_enabled: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub sessions: i64,
    pub active_tokens: i64,
    pub projects: i64,
    pub cost_30d_usd: f64,
    pub quota: Quota,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateUser {
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub is_admin: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InviteStatus {
    Open,
    Used,
    Expired,
    Revoked,
}

impl InviteStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Offen",
            Self::Used => "Angenommen",
            Self::Expired => "Abgelaufen",
            Self::Revoked => "Widerrufen",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invitation {
    pub id: Uuid,
    pub email: Option<String>,
    pub make_admin: bool,
    pub note: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub created_by: Option<String>,
    pub used_by: Option<String>,
    pub status: InviteStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewInvitation {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub make_admin: bool,
    #[serde(default)]
    pub note: String,
    pub days: i32,
}

impl NewInvitation {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_INVITE_DAYS).contains(&self.days) {
            return Err(format!("Gültigkeit: 1 bis {MAX_INVITE_DAYS} Tage."));
        }
        if let Some(e) = &self.email {
            let e = e.trim();
            if !e.is_empty() && (!e.contains('@') || e.len() > 320) {
                return Err("Bitte eine gültige E-Mail-Adresse angeben.".into());
            }
        }
        if self.note.chars().count() > 200 {
            return Err("Notiz: höchstens 200 Zeichen.".into());
        }
        Ok(())
    }
}

/// The link is shown exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedInvitation {
    pub invitation: Invitation,
    pub link: String,
}

/// Public preview of an invitation link (registration form).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvitePreview {
    pub valid: bool,
    pub email: Option<String>,
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: i64,
    pub at: DateTime<Utc>,
    pub actor_id: Option<Uuid>,
    pub actor_label: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub project_id: Option<Uuid>,
    pub details: Value,
    /// Hex SHA-256 of the entry (hash chain).
    pub hash: String,
    pub prev_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    /// `before` for the next (older) page.
    pub next_before: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditVerify {
    pub checked: i64,
    pub intact: bool,
    pub first_broken: Option<i64>,
}

/// Filter groups of the audit view (action prefixes).
pub const AUDIT_GROUPS: [(&str, &str); 9] = [
    ("", "Alle Ereignisse"),
    ("auth.", "Anmeldung"),
    ("account.", "Konto & Zwei-Faktor"),
    ("credential.", "Zugangsdaten & Geheimnisse"),
    ("project.", "Projekte & Mitglieder"),
    ("git.", "Pushes"),
    ("approval.", "Freigaben"),
    ("token.", "API-Tokens"),
    ("admin.", "Verwaltung"),
];

/// German description of an audit action.
pub fn audit_label(action: &str) -> &'static str {
    match action {
        "auth.login" => "Angemeldet",
        "auth.login_failed" => "Anmeldung fehlgeschlagen",
        "auth.locked" => "Anmeldung vorübergehend gesperrt (zu viele Fehlversuche)",
        "auth.mfa_failed" => "Zweiter Faktor falsch",
        "auth.logout" => "Abgemeldet",
        "auth.register" => "Konto angelegt",
        "auth.recovery_used" => "Wiederherstellungscode verwendet",
        "account.password_changed" => "Passwort geändert",
        "account.totp_enabled" => "Authenticator-App eingerichtet",
        "account.totp_disabled" => "Authenticator-App entfernt",
        "account.passkey_added" => "Passkey hinzugefügt",
        "account.passkey_removed" => "Passkey entfernt",
        "account.recovery_regenerated" => "Neue Wiederherstellungscodes erzeugt",
        "account.session_revoked" => "Gerät abgemeldet",
        "account.sessions_revoked" => "Alle anderen Geräte abgemeldet",
        "credential.saved" => "Zugangsdaten gespeichert",
        "credential.deleted" => "Zugangsdaten gelöscht",
        "credential.connector_created" => "Anbieter-Verbindung angelegt",
        "credential.connector_updated" => "Anbieter-Verbindung geändert",
        "credential.connector_deleted" => "Anbieter-Verbindung gelöscht",
        "credential.secret_saved" => "Projekt-Geheimnis gespeichert",
        "credential.secret_deleted" => "Projekt-Geheimnis gelöscht",
        "credential.environment_changed" => "Projekt-Umgebung geändert",
        "project.created" => "Projekt angelegt",
        "project.updated" => "Projekt geändert",
        "project.deleted" => "Projekt gelöscht",
        "project.member_added" => "Mitglied hinzugefügt",
        "project.member_removed" => "Mitglied entfernt",
        "project.moved" => "Projekt auf anderen Runner umgezogen",
        "git.push" => "Gepusht",
        "git.push_failed" => "Push fehlgeschlagen",
        "approval.gate_approved" => "Geschützte Änderung freigegeben",
        "approval.gate_rejected" => "Geschützte Änderung abgelehnt",
        "approval.plan_approved" => "Plan freigegeben",
        "approval.plan_rejected" => "Plan verworfen",
        "approval.task_approved" => "Auftrag freigegeben",
        "approval.knowledge_approved" => "Wissens-Vorschlag übernommen",
        "approval.knowledge_rejected" => "Wissens-Vorschlag abgelehnt",
        "token.created" => "API-Token erstellt",
        "token.revoked" => "API-Token widerrufen",
        "token.deleted" => "API-Token gelöscht",
        "admin.user_disabled" => "Person gesperrt",
        "admin.user_enabled" => "Person entsperrt",
        "admin.role_changed" => "Rolle geändert",
        "admin.quota_changed" => "Kontingent geändert",
        "admin.invite_created" => "Einladung erstellt",
        "admin.invite_revoked" => "Einladung widerrufen",
        "admin.settings_changed" => "Instanz-Einstellungen geändert",
        "admin.prices_changed" => "Preistabelle geändert",
        "admin.runner_created" => "Runner angelegt",
        "admin.runner_updated" => "Runner geändert",
        "admin.runner_token_rotated" => "Runner-Token erneuert",
        "admin.runner_revoked" => "Runner widerrufen",
        "admin.runner_deleted" => "Runner gelöscht",
        "admin.runner_offline" => "Runner nicht mehr erreichbar",
        "admin.runner_online" => "Runner wieder erreichbar",
        "admin.backup_created" => "Backup erstellt",
        "admin.restore_completed" => "Backup wiederhergestellt",
        "admin.audit_exported" => "Protokoll exportiert",
        _ => "Ereignis",
    }
}

// ---------------------------------------------------------------------------
// Runner fleet
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerStatus {
    Active,
    Draining,
    Revoked,
}

impl RunnerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Revoked => "revoked",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Active, Self::Draining, Self::Revoked]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerView {
    pub id: Uuid,
    pub name: String,
    pub url: Option<String>,
    pub builtin: bool,
    pub status: RunnerStatus,
    pub capacity: Option<i32>,
    pub online: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub offline_since: Option<DateTime<Utc>>,
    pub engine: String,
    pub egress_enforced: bool,
    pub version: String,
    pub active_runs: i32,
    pub projects: i64,
    pub running_tasks: i64,
}

impl RunnerView {
    /// Plain-language state for the overview.
    pub fn state_label(&self) -> &'static str {
        match (self.status, self.online) {
            (RunnerStatus::Revoked, _) => "Widerrufen",
            (_, false) => "Nicht erreichbar",
            (RunnerStatus::Draining, true) if self.running_tasks > 0 => "Leert sich",
            (RunnerStatus::Draining, true) => "Pausiert (leer)",
            (RunnerStatus::Active, true) => "Bereit",
        }
    }
    pub fn state_tone(&self) -> &'static str {
        match (self.status, self.online) {
            (RunnerStatus::Revoked, _) => "neutral",
            (_, false) => "danger",
            (RunnerStatus::Draining, _) => "warning",
            (RunnerStatus::Active, true) => "success",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewRunner {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub capacity: Option<i32>,
}

pub fn validate_runner_name(name: &str) -> Result<(), String> {
    let n = name.trim().chars().count();
    if n == 0 || n > MAX_RUNNER_NAME {
        return Err(format!("Name: 1 bis {MAX_RUNNER_NAME} Zeichen."));
    }
    Ok(())
}

pub fn validate_runner_url(url: &str) -> Result<(), String> {
    let u = url.trim();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or("Die Adresse beginnt mit http:// oder https://.")?;
    if rest.is_empty() || rest.contains(char::is_whitespace) || u.len() > 500 {
        return Err("Bitte eine gültige Adresse angeben, z. B. http://10.0.0.5:8090.".into());
    }
    if rest.contains('?') || rest.contains('#') || rest.contains('@') {
        return Err("Die Adresse darf keine Zugangsdaten, Parameter oder Anker enthalten.".into());
    }
    Ok(())
}

pub fn validate_capacity(c: Option<i32>) -> Result<(), String> {
    match c {
        Some(c) if !(1..=1000).contains(&c) => Err("Kapazität: 1 bis 1000 Läufe.".into()),
        _ => Ok(()),
    }
}

impl NewRunner {
    pub fn validate(&self) -> Result<(), String> {
        validate_runner_name(&self.name)?;
        validate_runner_url(&self.url)?;
        validate_capacity(self.capacity)
    }
}

/// The token is shown exactly once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedRunner {
    pub runner: RunnerView,
    pub token: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateRunner {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "crate::double_option")]
    pub capacity: Option<Option<i32>>,
    /// `active` or `draining` (revoking has its own endpoint).
    #[serde(default)]
    pub status: Option<RunnerStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveProject {
    pub runner_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveResult {
    pub workspaces: usize,
    pub bytes: u64,
}

/// Project on a runner (overview, move dialog).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerProject {
    pub id: Uuid,
    pub name: String,
    pub owner: String,
    pub runner_id: Uuid,
    pub busy: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_codes_normalize() {
        assert_eq!(
            normalize_recovery_code(" abcde-fghjk ").as_deref(),
            Some("ABCDEFGHJK")
        );
        assert_eq!(normalize_recovery_code("ABCDE-FGHJ"), None, "too short");
        assert_eq!(
            normalize_recovery_code("ABCDE-FGHJ0"),
            None,
            "0 is not in the alphabet"
        );
        assert_eq!(format_recovery_code("ABCDEFGHJK"), "ABCDE-FGHJK");
        assert_eq!(normalize_totp("123 456").as_deref(), Some("123456"));
        assert_eq!(normalize_totp("12345a"), None);
        assert_eq!(normalize_totp("1234567"), None);
    }

    #[test]
    fn validation_texts() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("longenough").is_ok());
        assert!(validate_runner_url("http://10.0.0.5:8090").is_ok());
        assert!(validate_runner_url("ftp://x").is_err());
        assert!(validate_runner_url("http://user:pw@host").is_err());
        assert!(validate_capacity(Some(0)).is_err());
        assert!(Quota {
            max_cost_30d_usd: Some(f64::NAN),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(NewInvitation {
            email: None,
            make_admin: false,
            note: String::new(),
            days: 31
        }
        .validate()
        .is_err());
    }

    #[test]
    fn device_and_method_texts() {
        assert_eq!(
            describe_user_agent("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"),
            "Safari auf iPhone"
        );
        assert_eq!(
            describe_user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36"),
            "Chrome auf Linux"
        );
        assert_eq!(describe_user_agent(""), "Unbekanntes Gerät");
        assert_eq!(
            describe_auth_method("password+totp"),
            "Passwort + Authenticator-App"
        );
        assert_eq!(
            describe_auth_method("oidc:Google+webauthn"),
            "Anmeldung über Google + Passkey"
        );
        assert!(MfaPolicy::Admins.applies(true) && !MfaPolicy::Admins.applies(false));
    }
}
