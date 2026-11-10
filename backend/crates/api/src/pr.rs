//! Output mode, git hosts and pull requests (WP05): shared types, texts
//! and validation (server and UI use the same wording and rules).
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a finished, checked task delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// One commit per task on the target branch (merge queue, WP04).
    #[default]
    Commit,
    /// The task branch is pushed to the remote; merged on the host.
    Branch,
    /// Like `Branch`, plus a pull/merge request with feedback loop.
    Pr,
}

impl OutputMode {
    pub const ALL: [OutputMode; 3] = [Self::Commit, Self::Branch, Self::Pr];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Branch => "branch",
            Self::Pr => "pr",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Commit => "Commit im Hauptstand",
            Self::Branch => "Branch hochladen",
            Self::Pr => "Pull Request",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Commit => "Queue übernimmt jeden geprüften Auftrag als einen Commit in den Hauptstand. Hochgeladen wird nur mit „Automatisch hochladen“, auf den Standard-Branch nur nach deiner Freigabe.",
            Self::Branch => "Queue lädt den geprüften Branch des Auftrags (queue/…) auf den Git-Server. Ihr übernehmt ihn dort selbst. Der Hauptstand in Queue bleibt unverändert.",
            Self::Pr => "Wie „Branch hochladen“, zusätzlich legt Queue einen Pull Request an. Schlägt die CI fehl oder schreibt jemand mit Schreibrecht einen Review-Kommentar, startet Queue einen Folge-Auftrag auf demselben Branch. Queue genehmigt und merged nie selbst.",
        }
    }

    /// Delivers through the remote (needs a remote URL and a token).
    pub fn pushes_branch(self) -> bool {
        matches!(self, Self::Branch | Self::Pr)
    }
}

/// Supported git hosts (REST APIs). Forgejo uses the Gitea API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHost {
    Github,
    Gitlab,
    Gitea,
}

impl GitHost {
    pub const ALL: [GitHost; 3] = [Self::Github, Self::Gitlab, Self::Gitea];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Gitea => "gitea",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Gitlab => "GitLab",
            Self::Gitea => "Gitea / Forgejo",
        }
    }

    /// "Pull Request" or "Merge Request".
    pub fn request_name(self) -> &'static str {
        match self {
            Self::Gitlab => "Merge Request",
            _ => "Pull Request",
        }
    }

    /// Minimal token rights (shown next to the token field and in the README).
    pub fn token_help(self) -> &'static str {
        match self {
            Self::Github => "Klassisches Token mit repo, oder Fine-grained Token nur für dieses Repository: Contents und Pull requests (Lesen und Schreiben), Commit statuses, Checks und Metadata (Lesen).",
            Self::Gitlab => "Projekt- oder persönliches Token mit den Bereichen api und write_repository, Rolle mindestens Developer.",
            Self::Gitea => "Token mit write:repository, write:issue und read:user.",
        }
    }
}

/// The git host of well-known public instances (`None` = must be chosen).
pub fn detect_host(remote_url: &str) -> Option<GitHost> {
    let host = remote_parts(remote_url)?.host;
    match host.as_str() {
        "github.com" => Some(GitHost::Github),
        "gitlab.com" => Some(GitHost::Gitlab),
        "codeberg.org" | "gitea.com" => Some(GitHost::Gitea),
        _ => None,
    }
}

/// Host, port and repository path of a remote URL (https, ssh or scp form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteParts {
    pub host: String,
    pub port: Option<u16>,
    /// `owner/repo` (GitLab: `group/sub/project`), without `.git`.
    pub path: String,
    pub https: bool,
}

pub fn remote_parts(url: &str) -> Option<RemoteParts> {
    let url = url.trim();
    let (rest, https, scp) = if let Some(r) = url.strip_prefix("https://") {
        (r, true, false)
    } else if let Some(r) = url.strip_prefix("http://") {
        (r, false, false)
    } else if let Some(r) = url.strip_prefix("ssh://") {
        (r, false, false)
    } else if url.contains("://") {
        return None;
    } else {
        (url, false, true)
    };
    let (authority, path) = if scp {
        // git@host:owner/repo.git
        let (a, p) = rest.split_once(':')?;
        (a, p)
    } else {
        rest.split_once('/')?
    };
    let authority = authority.rsplit('@').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !scp => (h, Some(p.parse::<u16>().ok()?)),
        _ => (authority, None),
    };
    let path = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_matches('/');
    if host.is_empty() || !valid_repo_path(path) {
        return None;
    }
    Some(RemoteParts {
        host: host.to_ascii_lowercase(),
        port,
        path: path.to_string(),
        https,
    })
}

/// Default REST base of a host for a remote URL (`None` = cannot derive,
/// e.g. a local remote; then the API address must be set).
pub fn default_api_base(host: GitHost, remote_url: &str) -> Option<String> {
    let p = remote_parts(remote_url)?;
    if host == GitHost::Github && p.host == "github.com" {
        return Some("https://api.github.com".into());
    }
    let port = match (p.https, p.port) {
        (true, Some(port)) if port != 443 => format!(":{port}"),
        _ => String::new(),
    };
    let suffix = match host {
        // GitHub Enterprise Server.
        GitHost::Github => "/api/v3",
        GitHost::Gitlab => "/api/v4",
        GitHost::Gitea => "/api/v1",
    };
    Some(format!("https://{}{port}{suffix}", p.host))
}

/// `owner/repo` (or `group/sub/project`): letters, digits, `-_.`, 2–5 parts.
pub fn valid_repo_path(p: &str) -> bool {
    let parts: Vec<&str> = p.split('/').collect();
    (2..=5).contains(&parts.len())
        && p.len() <= 200
        && parts.iter().all(|s| {
            !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

/// Highest number of feedback rounds per pull request.
pub const MAX_FOLLOWUPS: i32 = 10;
pub const DEFAULT_FOLLOWUPS: i32 = 3;
/// General comments need this word to start a follow-up (inline review
/// comments and "changes requested" reviews always count).
pub const TRIGGER_WORD: &str = "/queue";

pub fn validate_max_followups(n: i32) -> Result<(), String> {
    if (0..=MAX_FOLLOWUPS).contains(&n) {
        Ok(())
    } else {
        Err(format!(
            "Folge-Runden pro Pull Request: bitte eine Zahl von 0 bis {MAX_FOLLOWUPS}"
        ))
    }
}

/// Labels: at most 10, each 1–50 characters, no commas or control characters.
pub fn validate_labels(labels: &[String]) -> Result<(), String> {
    if labels.len() > 10 {
        return Err("Höchstens 10 Labels".into());
    }
    for l in labels {
        let t = l.trim();
        if t.is_empty()
            || t.chars().count() > 50
            || t.contains(',')
            || t.chars().any(char::is_control)
        {
            return Err(format!("Ungültiges Label „{t}“ (1–50 Zeichen, ohne Komma)"));
        }
    }
    Ok(())
}

/// API address set by the owner: http(s) URL without credentials, query
/// or fragment.
pub fn validate_api_url(u: &str) -> Result<(), String> {
    let err = || "API-Adresse: bitte eine https://-Adresse ohne Benutzer und Parameter".to_string();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or_else(err)?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || u.contains(['?', '#', ' '])
        || u.len() > 300
        || u.chars().any(char::is_control)
    {
        return Err(err());
    }
    Ok(())
}

/// Delivery settings of a project (`GET /api/projects/{id}/delivery`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryView {
    pub output_mode: OutputMode,
    /// Chosen host (`None` = detected from the remote URL).
    pub git_host: Option<GitHost>,
    pub detected_host: Option<GitHost>,
    /// Override of the REST base (self-hosted under a path, tests).
    pub git_api_url: Option<String>,
    /// Override of `owner/repo`.
    pub git_repo_path: Option<String>,
    /// What Queue will use (`None` = incomplete).
    pub effective_host: Option<GitHost>,
    pub effective_api_url: Option<String>,
    pub effective_repo: Option<String>,
    pub has_remote: bool,
    pub has_token: bool,
    pub pr_draft: bool,
    pub pr_labels: Vec<String>,
    pub pr_auto_fix: bool,
    pub pr_max_followups: i32,
    /// What is missing for the chosen mode (empty = ready).
    pub problems: Vec<String>,
}

/// `PUT /api/projects/{id}/delivery` (owner only). Absent = unchanged;
/// empty strings clear the overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateDelivery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_mode: Option<OutputMode>,
    /// `""` = detect from the remote URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_api_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_auto_fix: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_max_followups: Option<i32>,
}

/// Result of `POST /api/projects/{id}/delivery/check` (never echoes the token).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostCheck {
    pub ok: bool,
    /// Account of the token.
    pub login: Option<String>,
    /// The token may push and open pull requests.
    pub can_write: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    /// Branch uploaded, the request is being created.
    Creating,
    Open,
    Merged,
    Closed,
}

impl PrState {
    pub const ALL: [PrState; 4] = [Self::Creating, Self::Open, Self::Merged, Self::Closed];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self, draft: bool) -> &'static str {
        match self {
            Self::Creating => "Wird angelegt",
            Self::Open if draft => "Entwurf",
            Self::Open => "Offen",
            Self::Merged => "Gemergt",
            Self::Closed => "Geschlossen",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Creating | Self::Open)
    }
}

/// Aggregate of the checks of the pull request's current commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiState {
    /// No checks reported (yet).
    #[default]
    None,
    Pending,
    Success,
    Failure,
}

impl CiState {
    pub const ALL: [CiState; 4] = [Self::None, Self::Pending, Self::Success, Self::Failure];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Keine Checks",
            Self::Pending => "Checks laufen",
            Self::Success => "Checks grün",
            Self::Failure => "Checks rot",
        }
    }
}

/// State of one check (CI job, status context).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pending,
    Success,
    Failure,
    /// Skipped, cancelled, neutral: neither red nor green.
    Neutral,
}

impl CheckState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "läuft",
            Self::Success => "grün",
            Self::Failure => "rot",
            Self::Neutral => "übersprungen",
        }
    }
}

/// One check as stored and shown (names come from the host and are shown
/// as text only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    #[serde(default)]
    pub url: Option<String>,
}

/// Aggregate rule: any pending ⇒ pending (wait for the whole run), else any
/// failure ⇒ failure, else any success ⇒ success, else none.
pub fn ci_state(checks: &[Check]) -> CiState {
    if checks.is_empty() {
        CiState::None
    } else if checks.iter().any(|c| c.state == CheckState::Pending) {
        CiState::Pending
    } else if checks.iter().any(|c| c.state == CheckState::Failure) {
        CiState::Failure
    } else if checks.iter().any(|c| c.state == CheckState::Success) {
        CiState::Success
    } else {
        CiState::None
    }
}

/// `GET /api/projects/{id}/pulls`, `GET /api/pulls/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestView {
    pub id: Uuid,
    pub project_id: Uuid,
    /// Task that opened it.
    pub task_id: Uuid,
    pub task_title: String,
    pub host: GitHost,
    pub number: Option<i64>,
    /// Link to the host (only http(s) links are stored).
    pub url: Option<String>,
    pub state: PrState,
    pub draft: bool,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: Option<String>,
    pub ci_state: CiState,
    pub checks: Vec<Check>,
    /// The host reports a merge conflict with the target branch.
    pub conflict: bool,
    pub auto_fix: bool,
    pub followups: i32,
    pub max_followups: i32,
    /// Follow-up task that is not finished yet.
    pub active_task: Option<Uuid>,
    pub last_polled_at: Option<DateTime<Utc>>,
    pub next_poll_at: Option<DateTime<Utc>>,
    /// Last problem talking to the host (readable, token removed).
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl PullRequestView {
    /// "#12" or the host's word while it is being created.
    pub fn title(&self) -> String {
        match self.number {
            Some(n) => format!("{} #{n}", self.host.request_name()),
            None => self.host.request_name().to_string(),
        }
    }

    pub fn rounds_left(&self) -> i32 {
        (self.max_followups - self.followups).max(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSource {
    /// Failed CI run of one commit.
    Ci,
    /// General comment on the pull request.
    Comment,
    /// Review (body of "changes requested").
    Review,
    /// Inline comment on the code.
    ReviewComment,
}

impl FeedbackSource {
    pub const ALL: [FeedbackSource; 4] =
        [Self::Ci, Self::Comment, Self::Review, Self::ReviewComment];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ci => "ci",
            Self::Comment => "comment",
            Self::Review => "review",
            Self::ReviewComment => "review_comment",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ci => "CI fehlgeschlagen",
            Self::Comment => "Kommentar",
            Self::Review => "Review",
            Self::ReviewComment => "Kommentar am Code",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackOutcome {
    /// Became (part of) a follow-up task.
    Task,
    /// Author has no write access to the repository (checked on the host).
    IgnoredPermission,
    /// Written by a bot or by Queue's own account.
    IgnoredBot,
    /// General comment without the trigger word.
    IgnoredUntriggered,
    /// Round limit of the pull request reached.
    IgnoredLimit,
    /// Auto-fix switched off for this pull request.
    IgnoredDisabled,
}

impl FeedbackOutcome {
    pub const ALL: [FeedbackOutcome; 6] = [
        Self::Task,
        Self::IgnoredPermission,
        Self::IgnoredBot,
        Self::IgnoredUntriggered,
        Self::IgnoredLimit,
        Self::IgnoredDisabled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::IgnoredPermission => "ignored_permission",
            Self::IgnoredBot => "ignored_bot",
            Self::IgnoredUntriggered => "ignored_untriggered",
            Self::IgnoredLimit => "ignored_limit",
            Self::IgnoredDisabled => "ignored_disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Task => "Folge-Auftrag",
            Self::IgnoredPermission => "Ignoriert: kein Schreibrecht",
            Self::IgnoredBot => "Ignoriert: Bot oder Queue selbst",
            Self::IgnoredUntriggered => "Ignoriert: ohne /queue",
            Self::IgnoredLimit => "Ignoriert: Runden-Grenze erreicht",
            Self::IgnoredDisabled => "Ignoriert: Auto-Fix aus",
        }
    }
}

/// One piece of feedback Queue looked at (never its text: that stays on
/// the host and only goes, marked as data, into a follow-up prompt).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackView {
    pub source: FeedbackSource,
    pub author: Option<String>,
    pub outcome: FeedbackOutcome,
    pub task_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullRequestDetail {
    #[serde(flatten)]
    pub pr: PullRequestView,
    pub feedback: Vec<FeedbackView>,
}

/// `POST /api/pulls/{id}/auto-fix`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetAutoFix {
    pub enabled: bool,
}

pub const AUTO_FIX_HELP: &str = "Bei roter CI oder einem Review-Kommentar von jemandem mit Schreibrecht startet Queue einen Folge-Auftrag auf demselben Branch. Allgemeine Kommentare zählen nur mit /queue. Kommentare ohne Schreibrecht werden ignoriert. Queue genehmigt und merged nie.";

pub const ROUNDS_HELP: &str = "Schutz vor Endlosschleifen: Nach so vielen Folge-Aufträgen hört Queue auf und wartet auf dich. „Eine weitere Runde erlauben“ hebt die Grenze für diesen Pull Request um eins an.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_roundtrip_and_have_texts() {
        for m in OutputMode::ALL {
            assert_eq!(OutputMode::parse(m.as_str()), Some(m));
            assert!(m.help().len() > 60);
        }
        assert_eq!(OutputMode::default(), OutputMode::Commit);
        assert!(!OutputMode::Commit.pushes_branch() && OutputMode::Pr.pushes_branch());
        for h in GitHost::ALL {
            assert_eq!(GitHost::parse(h.as_str()), Some(h));
            assert!(h.token_help().len() > 20);
        }
        for s in PrState::ALL {
            assert_eq!(PrState::parse(s.as_str()), Some(s));
        }
        for s in CiState::ALL {
            assert_eq!(CiState::parse(s.as_str()), Some(s));
        }
        for s in FeedbackSource::ALL {
            assert_eq!(FeedbackSource::parse(s.as_str()), Some(s));
        }
        for s in FeedbackOutcome::ALL {
            assert_eq!(FeedbackOutcome::parse(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
        }
        assert_eq!(PrState::Open.label(true), "Entwurf");
        assert!(PrState::Creating.is_open() && !PrState::Merged.is_open());
    }

    #[test]
    fn remote_urls_and_api_bases() {
        let p = remote_parts("https://github.com/acme/app.git").unwrap();
        assert_eq!(
            (p.host.as_str(), p.path.as_str(), p.https),
            ("github.com", "acme/app", true)
        );
        let p = remote_parts("git@gitlab.com:group/sub/proj.git").unwrap();
        assert_eq!(
            (p.host.as_str(), p.path.as_str()),
            ("gitlab.com", "group/sub/proj")
        );
        let p = remote_parts("ssh://git@git.example.org:2222/o/r").unwrap();
        assert_eq!(
            (p.host.as_str(), p.port, p.path.as_str()),
            ("git.example.org", Some(2222), "o/r")
        );
        assert_eq!(remote_parts("/srv/git/x.git"), None);
        assert_eq!(remote_parts("file:///srv/x.git"), None);
        assert_eq!(remote_parts("https://h/../etc"), None);
        assert_eq!(detect_host("https://github.com/a/b"), Some(GitHost::Github));
        assert_eq!(detect_host("git@gitlab.com:a/b.git"), Some(GitHost::Gitlab));
        assert_eq!(
            detect_host("https://codeberg.org/a/b"),
            Some(GitHost::Gitea)
        );
        assert_eq!(detect_host("https://git.example.org/a/b"), None);
        assert_eq!(
            default_api_base(GitHost::Github, "https://github.com/a/b").as_deref(),
            Some("https://api.github.com")
        );
        assert_eq!(
            default_api_base(GitHost::Github, "https://ghe.corp/a/b").as_deref(),
            Some("https://ghe.corp/api/v3")
        );
        assert_eq!(
            default_api_base(GitHost::Gitlab, "git@gitlab.com:a/b.git").as_deref(),
            Some("https://gitlab.com/api/v4")
        );
        assert_eq!(
            default_api_base(GitHost::Gitea, "https://git.home:3000/a/b").as_deref(),
            Some("https://git.home:3000/api/v1")
        );
        // ssh port is not the web port.
        assert_eq!(
            default_api_base(GitHost::Gitea, "ssh://git@git.home:2222/a/b").as_deref(),
            Some("https://git.home/api/v1")
        );
    }

    #[test]
    fn validation() {
        assert!(validate_max_followups(0).is_ok() && validate_max_followups(MAX_FOLLOWUPS).is_ok());
        assert!(validate_max_followups(-1).is_err() && validate_max_followups(11).is_err());
        assert!(validate_labels(&["queue".into(), "ai generated".into()]).is_ok());
        assert!(validate_labels(&["a,b".into()]).is_err());
        assert!(validate_labels(&["".into()]).is_err());
        assert!(validate_labels(&vec!["x".to_string(); 11]).is_err());
        assert!(validate_api_url("https://git.example.org/api/v4").is_ok());
        assert!(validate_api_url("http://127.0.0.1:9/api/v3").is_ok());
        for bad in [
            "ftp://x",
            "https://u:p@x/api",
            "https://x/api?a=1",
            "https://",
            "javascript:x",
        ] {
            assert!(validate_api_url(bad).is_err(), "{bad}");
        }
        assert!(valid_repo_path("a/b") && valid_repo_path("g/s/p"));
        assert!(!valid_repo_path("a") && !valid_repo_path("a/../b") && !valid_repo_path("a/b c"));
    }

    #[test]
    fn ci_aggregate() {
        let c = |s| Check {
            name: "x".into(),
            state: s,
            url: None,
        };
        assert_eq!(ci_state(&[]), CiState::None);
        assert_eq!(
            ci_state(&[c(CheckState::Failure), c(CheckState::Pending)]),
            CiState::Pending
        );
        assert_eq!(
            ci_state(&[c(CheckState::Failure), c(CheckState::Success)]),
            CiState::Failure
        );
        assert_eq!(
            ci_state(&[c(CheckState::Success), c(CheckState::Neutral)]),
            CiState::Success
        );
        assert_eq!(ci_state(&[c(CheckState::Neutral)]), CiState::None);
    }
}
