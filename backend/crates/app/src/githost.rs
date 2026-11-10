//! Git host adapters (WP05): GitHub, GitLab, Gitea/Forgejo over REST.
//!
//! One client per project and call site, built on the SSRF-guarded HTTP
//! client of the app (resolver checks every address at connect time; the
//! runner is never a target). Differences to the model-API client:
//! * **no redirects at all** – reqwest strips `Authorization` on
//!   cross-origin redirects, but not custom headers such as GitLab's
//!   `PRIVATE-TOKEN`; a host API that redirects is reported instead;
//! * the token only ever goes into a request header; every error text is
//!   redacted (token + secret scanner) before it leaves this module.
//!
//! There is deliberately **no** method that approves, merges, reviews or
//! comments: Queue never approves or merges (WP05 guardrail; integration
//! tests assert that the fake hosts never see such a request).
//!
//! API formats: GitHub from the official OpenAPI description
//! (`github/rest-api-description`), GitLab from its documentation sources
//! (`doc/api/*.md`), Gitea from its swagger template (v1.22). Fields marked
//! "unverified" are from prior knowledge only (see `WP05-impl.md`).
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use queued_api::pr::{Check, CheckState, FeedbackSource, GitHost, PrState, TRIGGER_WORD};
use queued_common::secrets;
use reqwest::{header::HeaderMap, Method};
use serde_json::{json, Value};

use crate::{llm, models::Project};

/// Items per page and pages read at most per list (bounded work per poll).
const PER_PAGE: usize = 50;
const MAX_PAGES: usize = 5;
/// Bytes of untrusted text kept per item (comment body, CI output, log tail).
pub const MAX_ITEM_TEXT: usize = 4000;

#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// Rate limit: do not call again before this time.
    RateLimited(DateTime<Utc>),
    /// 401/403 (token missing, expired or without rights).
    Auth(String),
    NotFound(String),
    /// Anything else (network, 5xx, unexpected answer).
    Other(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited(t) => write!(
                f,
                "Rate-Limit des Git-Servers – nächster Versuch ab {}",
                t.format("%H:%M UTC")
            ),
            Self::Auth(m) => write!(f, "Zugang abgelehnt: {m}"),
            Self::NotFound(m) => write!(f, "Nicht gefunden: {m}"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}

/// One response (2xx or 304).
#[derive(Debug, Clone)]
pub struct HostResp {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Value,
    /// Text body (GitLab job logs).
    pub text: String,
    /// The quota is used up: pause until then.
    pub pause_until: Option<DateTime<Utc>>,
}

impl HostResp {
    pub fn not_modified(&self) -> bool {
        self.status == 304
    }
}

/// Pull request as the host reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct PrInfo {
    pub number: i64,
    pub url: Option<String>,
    pub state: PrState,
    pub draft: bool,
    pub head_sha: Option<String>,
    pub head_ref: Option<String>,
    pub conflict: bool,
}

/// Untrusted details of a failed check (name + output/log tail).
#[derive(Debug, Clone, PartialEq)]
pub struct CiDetail {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CiReport {
    pub checks: Vec<Check>,
    pub details: Vec<CiDetail>,
}

/// One comment/review as read from the host. `body` is untrusted.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub source: FeedbackSource,
    pub id: String,
    pub author: String,
    pub author_id: Option<i64>,
    /// Marked as a bot by the host (GitHub `type: Bot`, `[bot]` suffix).
    pub bot: bool,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    /// Review state as reported (`CHANGES_REQUESTED`, `REQUEST_CHANGES`, …).
    pub review_state: Option<String>,
    /// System note (GitLab): never feedback.
    pub system: bool,
}

impl Comment {
    /// Whether this comment asks for work (before the permission check):
    /// inline comments and "changes requested" reviews always, general
    /// comments and other reviews only with the trigger word.
    pub fn triggers(&self) -> bool {
        if self.system {
            return false;
        }
        let has_word = self.body.to_lowercase().contains(TRIGGER_WORD);
        match self.source {
            FeedbackSource::ReviewComment => !self.body.trim().is_empty(),
            FeedbackSource::Review => {
                let changes = matches!(
                    self.review_state.as_deref(),
                    Some("CHANGES_REQUESTED" | "REQUEST_CHANGES")
                );
                (changes && !self.body.trim().is_empty()) || has_word
            }
            FeedbackSource::Comment => has_word,
            FeedbackSource::Ci => false,
        }
    }
}

/// Validated connection of a project to its git host.
#[derive(Clone)]
pub struct HostClient {
    pub kind: GitHost,
    api: String,
    pub repo: String,
    token: String,
    http: reqwest::Client,
    guard: llm::Guard,
    /// Latest "quota used up" time reported on a successful response.
    pause: std::sync::Arc<std::sync::Mutex<Option<DateTime<Utc>>>>,
}

impl std::fmt::Debug for HostClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostClient")
            .field("kind", &self.kind)
            .field("api", &self.api)
            .field("repo", &self.repo)
            .finish_non_exhaustive()
    }
}

/// Effective host settings of a project (`Err` = readable reason).
pub fn effective(
    project: &Project,
    allow_insecure: bool,
) -> Result<(GitHost, String, String), String> {
    let remote = project
        .git_remote_url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .ok_or("Kein Remote eingetragen")?;
    let kind = project
        .host()
        .ok_or("Git-Host nicht erkannt – bitte GitHub, GitLab oder Gitea wählen")?;
    let api = match project.git_api_url.as_deref().filter(|u| !u.is_empty()) {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => queued_api::pr::default_api_base(kind, remote)
            .ok_or("API-Adresse nicht ableitbar – bitte eintragen")?,
    };
    queued_api::pr::validate_api_url(&api)?;
    if api.starts_with("http://") && !allow_insecure {
        return Err("API-Adresse ohne https:// ist nur mit ALLOW_PRIVATE_GIT_HOSTS erlaubt".into());
    }
    let repo = match project.git_repo_path.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => p.to_string(),
        None => queued_api::pr::remote_parts(remote)
            .map(|p| p.path)
            .ok_or("Repository-Pfad nicht ableitbar – bitte eintragen")?,
    };
    if !queued_api::pr::valid_repo_path(&repo) {
        return Err(format!("Ungültiger Repository-Pfad „{repo}“"));
    }
    Ok((kind, api, repo))
}

/// Login names as they may appear in a URL path (fail-closed otherwise).
fn valid_login(l: &str) -> bool {
    !l.is_empty()
        && l.len() <= 100
        && l.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !l.starts_with('.')
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Only http(s) links are kept (shown as links in the UI).
fn web_url(u: Option<String>) -> Option<String> {
    u.filter(|u| (u.starts_with("https://") || u.starts_with("http://")) && u.len() <= 500)
}

/// Tail of a log (the error is usually at the end).
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

impl HostClient {
    /// Client for a project (`Err` = readable configuration problem).
    pub fn new(
        project: &Project,
        token: Option<String>,
        guard: llm::Guard,
    ) -> Result<Self, String> {
        let (kind, api, repo) = effective(project, guard.allow_private)?;
        let token = token
            .filter(|t| !t.trim().is_empty())
            .ok_or("Kein Zugangs-Token eingetragen")?;
        let http = llm::guarded_builder(&guard)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("queued/0.2 (+pull-requests)")
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("HTTP-Client: {e}"))?;
        Ok(Self {
            kind,
            api,
            repo,
            token: token.trim().to_string(),
            http,
            guard,
            pause: Default::default(),
        })
    }

    /// The host said its quota is used up until then (on a successful
    /// response): the next poll waits for it.
    pub fn quota_pause(&self) -> Option<DateTime<Utc>> {
        *self.pause.lock().unwrap()
    }

    fn redact(&self, text: &str) -> String {
        secrets::redact(&text.replace(&self.token, "***"))
    }

    /// `/repos/{owner}/{repo}` (GitHub, Gitea) or `/projects/{id}` (GitLab).
    fn repo_base(&self) -> String {
        match self.kind {
            GitHost::Gitlab => format!(
                "{}/projects/{}",
                self.api,
                url::form_urlencoded::byte_serialize(self.repo.as_bytes()).collect::<String>()
            ),
            _ => format!("{}/repos/{}", self.api, self.repo),
        }
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.kind {
            // docs.github.com: `Authorization: Bearer` for PATs/app tokens.
            GitHost::Github => rb
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            // GitLab docs: `PRIVATE-TOKEN` header for personal/project tokens.
            GitHost::Gitlab => rb.header("PRIVATE-TOKEN", &self.token),
            // Gitea swagger: `Authorization: token <t>`.
            GitHost::Gitea => rb.header("Authorization", format!("token {}", self.token)),
        }
    }

    /// One request. `etag` sends `If-None-Match` (a 304 is a success).
    pub async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
        etag: Option<&str>,
    ) -> Result<HostResp, HostError> {
        llm::check_target(url, &self.guard)
            .await
            .map_err(|e| HostError::Other(self.redact(&e.message)))?;
        let mut rb = self.auth(self.http.request(method, url));
        if let Some(e) = etag {
            rb = rb.header("If-None-Match", e);
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await.map_err(|e| {
            HostError::Other(self.redact(&format!("Git-Server nicht erreichbar: {e}")))
        })?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let text = resp.text().await.unwrap_or_default();
        let pause = rate_pause(&headers, Utc::now());
        if let Some(p) = pause {
            let mut cur = self.pause.lock().unwrap();
            *cur = Some(cur.map_or(p, |c| c.max(p)));
        }
        if status == 429 || (status == 403 && pause.is_some()) {
            return Err(HostError::RateLimited(
                pause.unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(60)),
            ));
        }
        let msg = || {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| s(&v, "message").or_else(|| s(&v, "error")))
                .unwrap_or_else(|| cut(&text, 200));
            self.redact(&format!("HTTP {status}: {}", cut(&detail, 300)))
        };
        match status {
            200..=299 | 304 => Ok(HostResp {
                status,
                etag: headers
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .filter(|v| v.len() <= 200)
                    .map(str::to_string),
                body: serde_json::from_str(&text).unwrap_or(Value::Null),
                text,
                pause_until: pause,
            }),
            300..=399 => Err(HostError::Other(self.redact(&format!(
                "Der Git-Server leitet weiter (HTTP {status}) – Queue folgt aus Sicherheitsgründen keinen Weiterleitungen. Bitte API-Adresse/Repository-Pfad prüfen (umbenannt oder verschoben?)."
            )))),
            401 | 403 => Err(HostError::Auth(msg())),
            404 => Err(HostError::NotFound(msg())),
            _ => Err(HostError::Other(msg())),
        }
    }

    async fn get(&self, url: &str) -> Result<HostResp, HostError> {
        self.send(Method::GET, url, None, None).await
    }

    /// All pages of a list (bounded). Returns the items and, when the list
    /// fits on one page, the ETag of that page (`None` = not cacheable).
    /// `etag` given and 304 ⇒ `Ok(None)` (unchanged).
    async fn list(
        &self,
        url: &str,
        etag: Option<&str>,
    ) -> Result<Option<(Vec<Value>, Option<String>)>, HostError> {
        let mut items = vec![];
        let mut first_etag = None;
        let sep = if url.contains('?') { '&' } else { '?' };
        let per = match self.kind {
            GitHost::Gitea => "limit",
            _ => "per_page",
        };
        for page in 1..=MAX_PAGES {
            let u = format!("{url}{sep}{per}={PER_PAGE}&page={page}");
            let r = self
                .send(Method::GET, &u, None, if page == 1 { etag } else { None })
                .await?;
            if r.not_modified() {
                return Ok(None);
            }
            if page == 1 {
                first_etag = r.etag.clone();
            }
            let got = match r.body {
                Value::Array(a) => a,
                // GitHub check runs: {"total_count", "check_runs": [...]}.
                Value::Object(ref o) => o
                    .get("check_runs")
                    .or_else(|| o.get("statuses"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                _ => vec![],
            };
            let n = got.len();
            items.extend(got);
            if n < PER_PAGE {
                // One page: the ETag of page 1 describes the whole list. A
                // longer list appends to later pages without changing page 1.
                let cacheable = page == 1;
                return Ok(Some((items, first_etag.filter(|_| cacheable))));
            }
        }
        Ok(Some((items, None)))
    }

    /// Account of the token: (login, id).
    pub async fn whoami(&self) -> Result<(String, Option<i64>), HostError> {
        let r = self.get(&format!("{}/user", self.api)).await?;
        let login = s(&r.body, "login")
            .or_else(|| s(&r.body, "username"))
            .ok_or_else(|| HostError::Other("Antwort ohne Benutzername".into()))?;
        Ok((login, r.body.get("id").and_then(Value::as_i64)))
    }

    /// Whether `login` may push to the repository – asked the host, never
    /// taken from the comment. Unknown users / 404 ⇒ `false`.
    pub async fn can_write(&self, login: &str, user_id: Option<i64>) -> Result<bool, HostError> {
        let url = match self.kind {
            GitHost::Gitlab => {
                let Some(id) = user_id else {
                    return Ok(false);
                };
                format!("{}/members/all/{id}", self.repo_base())
            }
            _ => {
                if !valid_login(login) {
                    return Ok(false);
                }
                format!("{}/collaborators/{login}/permission", self.repo_base())
            }
        };
        let r = match self.get(&url).await {
            Ok(r) => r,
            Err(HostError::NotFound(_)) => return Ok(false),
            // Gitea answers 403 for users without access to the repository.
            Err(HostError::Auth(_)) if self.kind == GitHost::Gitea => return Ok(false),
            Err(e) => return Err(e),
        };
        Ok(write_access(self.kind, &r.body))
    }

    /// Open pull request with this head branch (idempotent creation).
    pub async fn find_open(&self, head: &str, base: &str) -> Result<Option<PrInfo>, HostError> {
        let enc = |v: &str| url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>();
        let url = match self.kind {
            GitHost::Github => {
                let owner = self.repo.split('/').next().unwrap_or_default();
                format!(
                    "{}/pulls?state=open&head={}&base={}",
                    self.repo_base(),
                    enc(&format!("{owner}:{head}")),
                    enc(base)
                )
            }
            GitHost::Gitlab => format!(
                "{}/merge_requests?state=opened&source_branch={}&target_branch={}",
                self.repo_base(),
                enc(head),
                enc(base)
            ),
            GitHost::Gitea => format!("{}/pulls?state=open", self.repo_base()),
        };
        let Some((items, _)) = self.list(&url, None).await? else {
            return Ok(None);
        };
        Ok(items
            .iter()
            .filter_map(|v| parse_pr(self.kind, v))
            .find(|p| p.state == PrState::Open && p.head_ref.as_deref().is_none_or(|h| h == head)))
    }

    /// Creates the pull/merge request (draft: GitHub field, GitLab `Draft:`
    /// title prefix, Gitea `WIP:` title prefix – unverified for Gitea).
    pub async fn create(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
        labels: &[String],
    ) -> Result<PrInfo, HostError> {
        let (url, payload) = match self.kind {
            GitHost::Github => (
                format!("{}/pulls", self.repo_base()),
                json!({"title": title, "head": head, "base": base, "body": body, "draft": draft}),
            ),
            GitHost::Gitlab => {
                let t = if draft {
                    format!("Draft: {title}")
                } else {
                    title.to_string()
                };
                let mut p = json!({"source_branch": head, "target_branch": base, "title": t, "description": body});
                if !labels.is_empty() {
                    p["labels"] = json!(labels.join(","));
                }
                (format!("{}/merge_requests", self.repo_base()), p)
            }
            GitHost::Gitea => {
                let t = if draft {
                    format!("WIP: {title}")
                } else {
                    title.to_string()
                };
                (
                    format!("{}/pulls", self.repo_base()),
                    json!({"title": t, "head": head, "base": base, "body": body}),
                )
            }
        };
        let r = match self
            .send(Method::POST, &url, Some(payload.clone()), None)
            .await
        {
            // GitHub (pulls/create): "Draft pull requests are available in
            // public repositories with GitHub Free … and in public and private
            // repositories with GitHub Team and GitHub Enterprise Cloud" –
            // a private repository on Free answers 422: open a normal one.
            Err(HostError::Other(m))
                if self.kind == GitHost::Github && draft && m.starts_with("HTTP 422") =>
            {
                let mut p = payload;
                p["draft"] = json!(false);
                self.send(Method::POST, &url, Some(p), None).await?
            }
            r => r?,
        };
        parse_pr(self.kind, &r.body)
            .ok_or_else(|| HostError::Other("Antwort ohne Nummer des Pull Requests".into()))
    }

    /// Adds labels (GitHub, Gitea: issue labels by name; GitLab: at creation).
    pub async fn add_labels(&self, number: i64, labels: &[String]) -> Result<(), HostError> {
        if labels.is_empty() || self.kind == GitHost::Gitlab {
            return Ok(());
        }
        let url = format!("{}/issues/{number}/labels", self.repo_base());
        self.send(Method::POST, &url, Some(json!({"labels": labels})), None)
            .await?;
        Ok(())
    }

    fn pr_url(&self, number: i64) -> String {
        match self.kind {
            GitHost::Gitlab => format!("{}/merge_requests/{number}", self.repo_base()),
            _ => format!("{}/pulls/{number}", self.repo_base()),
        }
    }

    /// Current state (`Ok(None)` = unchanged since `etag`).
    pub async fn pr(
        &self,
        number: i64,
        etag: Option<&str>,
    ) -> Result<Option<(PrInfo, Option<String>)>, HostError> {
        let r = self
            .send(Method::GET, &self.pr_url(number), None, etag)
            .await?;
        if r.not_modified() {
            return Ok(None);
        }
        let p = parse_pr(self.kind, &r.body)
            .ok_or_else(|| HostError::Other("Unerwartete Antwort zum Pull Request".into()))?;
        Ok(Some((p, r.etag)))
    }

    /// Checks of a commit with untrusted failure details.
    pub async fn ci(&self, number: i64, sha: &str) -> Result<CiReport, HostError> {
        if !(7..=64).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(HostError::Other("ungültiger Commit".into()));
        }
        let mut rep = CiReport::default();
        match self.kind {
            GitHost::Github => {
                let runs = self
                    .list(
                        &format!("{}/commits/{sha}/check-runs", self.repo_base()),
                        None,
                    )
                    .await?
                    .map(|(i, _)| i)
                    .unwrap_or_default();
                for c in &runs {
                    let state = github_check_state(c);
                    let name = s(c, "name").unwrap_or_else(|| "Check".into());
                    if state == CheckState::Failure {
                        let o = c.get("output").cloned().unwrap_or(Value::Null);
                        let text = ["title", "summary", "text"]
                            .iter()
                            .filter_map(|k| s(&o, k))
                            .filter(|t| !t.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");
                        rep.details.push(CiDetail {
                            name: name.clone(),
                            text: cut(&text, MAX_ITEM_TEXT),
                        });
                    }
                    rep.checks.push(Check {
                        name,
                        state,
                        url: web_url(s(c, "html_url").or_else(|| s(c, "details_url"))),
                    });
                }
                // Combined status (classic commit statuses, external CI).
                let st = self
                    .get(&format!("{}/commits/{sha}/status", self.repo_base()))
                    .await?;
                statuses(&st.body, "state", &mut rep);
            }
            GitHost::Gitea => {
                let st = self
                    .get(&format!("{}/commits/{sha}/status", self.repo_base()))
                    .await?;
                // Gitea: field `status` per entry (swagger CommitStatus).
                statuses(&st.body, "status", &mut rep);
            }
            GitHost::Gitlab => {
                let pipes = self
                    .list(&format!("{}/pipelines", self.pr_url(number)), None)
                    .await?
                    .map(|(i, _)| i)
                    .unwrap_or_default();
                // Newest pipeline of the head commit.
                let Some(p) = pipes
                    .iter()
                    .filter(|p| s(p, "sha").as_deref() == Some(sha))
                    .max_by_key(|p| p.get("id").and_then(Value::as_i64).unwrap_or(0))
                else {
                    return Ok(rep);
                };
                let pid = p.get("id").and_then(Value::as_i64).unwrap_or(0);
                let jobs = self
                    .list(&format!("{}/pipelines/{pid}/jobs", self.repo_base()), None)
                    .await?
                    .map(|(i, _)| i)
                    .unwrap_or_default();
                let mut logs = 0;
                for j in &jobs {
                    let state = gitlab_state(
                        s(j, "status").as_deref().unwrap_or_default(),
                        j.get("allow_failure")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    );
                    let name = s(j, "name").unwrap_or_else(|| "Job".into());
                    if state == CheckState::Failure && logs < 3 {
                        logs += 1;
                        let jid = j.get("id").and_then(Value::as_i64).unwrap_or(0);
                        let log = self
                            .get(&format!("{}/jobs/{jid}/trace", self.repo_base()))
                            .await
                            .map(|r| tail(&r.text, MAX_ITEM_TEXT))
                            .unwrap_or_default();
                        let reason = s(j, "failure_reason").unwrap_or_default();
                        rep.details.push(CiDetail {
                            name: name.clone(),
                            text: format!("{reason}\n{log}").trim().to_string(),
                        });
                    }
                    rep.checks.push(Check {
                        name,
                        state,
                        url: web_url(s(j, "web_url")),
                    });
                }
                if jobs.is_empty() {
                    rep.checks.push(Check {
                        name: format!("Pipeline #{pid}"),
                        state: gitlab_state(s(p, "status").as_deref().unwrap_or_default(), false),
                        url: web_url(s(p, "web_url")),
                    });
                }
            }
        }
        Ok(rep)
    }

    /// New-or-old comments, reviews and inline comments of a pull request.
    /// `etags`/`known_reviews` avoid work: lists answered with 304 are
    /// skipped; review comments are only fetched for reviews not seen yet
    /// (Gitea). Returns the comments and the ETags to store.
    pub async fn comments(
        &self,
        number: i64,
        etags: &serde_json::Map<String, Value>,
        known_reviews: &[String],
    ) -> Result<(Vec<Comment>, Vec<(String, Option<String>)>), HostError> {
        let mut out = vec![];
        let mut new_etags = vec![];
        let base = self.repo_base();
        let lists: Vec<(String, FeedbackSource)> = match self.kind {
            GitHost::Github => vec![
                (
                    format!("{base}/issues/{number}/comments"),
                    FeedbackSource::Comment,
                ),
                (
                    format!("{base}/pulls/{number}/comments"),
                    FeedbackSource::ReviewComment,
                ),
                (
                    format!("{base}/pulls/{number}/reviews"),
                    FeedbackSource::Review,
                ),
            ],
            GitHost::Gitlab => vec![(
                format!("{base}/merge_requests/{number}/notes?sort=asc&order_by=created_at"),
                FeedbackSource::Comment,
            )],
            GitHost::Gitea => vec![
                (
                    format!("{base}/issues/{number}/comments"),
                    FeedbackSource::Comment,
                ),
                (
                    format!("{base}/pulls/{number}/reviews"),
                    FeedbackSource::Review,
                ),
            ],
        };
        for (url, source) in lists {
            let etag = etags.get(&url).and_then(Value::as_str);
            let Some((items, tag)) = self.list(&url, etag).await? else {
                continue;
            };
            new_etags.push((url.clone(), tag));
            for v in &items {
                let Some(c) = parse_comment(self.kind, source, v) else {
                    continue;
                };
                // Gitea inline comments hang off their review.
                // Known reviews are skipped only while the list's ETag
                // survives: every rescan (deferred work, one more round,
                // auto-fix switched on) clears the ETags and reads them all.
                if self.kind == GitHost::Gitea
                    && source == FeedbackSource::Review
                    && v.get("comments_count").and_then(Value::as_i64).unwrap_or(0) > 0
                    && !(etag.is_some() && known_reviews.contains(&c.id))
                {
                    let u = format!("{base}/pulls/{number}/reviews/{}/comments", c.id);
                    if let Ok(r) = self.get(&u).await {
                        for rc in r.body.as_array().into_iter().flatten() {
                            if let Some(mut x) =
                                parse_comment(self.kind, FeedbackSource::ReviewComment, rc)
                            {
                                // A review's comments share its author.
                                x.author = c.author.clone();
                                x.author_id = c.author_id;
                                out.push(x);
                            }
                        }
                    }
                }
                out.push(c);
            }
        }
        Ok((out, new_etags))
    }
}

/// Pause from rate-limit headers: `Retry-After` (seconds), or a used-up
/// quota with its reset (`X-RateLimit-*` GitHub/Gitea, `RateLimit-*` GitLab;
/// reset as Unix time).
pub fn rate_pause(h: &HeaderMap, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(str::trim);
    if let Some(secs) = get("retry-after").and_then(|v| v.parse::<i64>().ok()) {
        return Some(now + chrono::Duration::seconds(secs.clamp(1, 3600)));
    }
    for (rem, reset) in [
        ("x-ratelimit-remaining", "x-ratelimit-reset"),
        ("ratelimit-remaining", "ratelimit-reset"),
    ] {
        if get(rem) == Some("0") {
            let at = get(reset)
                .and_then(|v| v.parse::<i64>().ok())
                .and_then(|t| Utc.timestamp_opt(t, 0).single())
                .filter(|t| *t > now)
                .unwrap_or(now + chrono::Duration::seconds(60));
            // Never trust a reset more than an hour away.
            return Some(at.min(now + chrono::Duration::hours(1)));
        }
    }
    None
}

fn write_access(kind: GitHost, v: &Value) -> bool {
    match kind {
        // GitHub: legacy base role; `maintain` is reported as `write`.
        GitHost::Github => matches!(s(v, "permission").as_deref(), Some("admin" | "write")),
        // GitLab: 30 = Developer (may push), 40 Maintainer, 50 Owner.
        GitHost::Gitlab => v.get("access_level").and_then(Value::as_i64).unwrap_or(0) >= 30,
        // Gitea: none | read | write | admin | owner (values unverified beyond the swagger type).
        GitHost::Gitea => matches!(
            s(v, "permission").as_deref(),
            Some("write" | "admin" | "owner")
        ),
    }
}

fn parse_pr(kind: GitHost, v: &Value) -> Option<PrInfo> {
    match kind {
        GitHost::Github | GitHost::Gitea => {
            let number = v.get("number").and_then(Value::as_i64)?;
            let merged = v.get("merged").and_then(Value::as_bool).unwrap_or(false);
            let state = match (s(v, "state").as_deref(), merged) {
                (_, true) => PrState::Merged,
                (Some("open"), _) => PrState::Open,
                _ => PrState::Closed,
            };
            let head = v.get("head").cloned().unwrap_or(Value::Null);
            let conflict = match kind {
                // GitHub: mergeable false + mergeable_state "dirty" = conflicts.
                GitHost::Github => {
                    v.get("mergeable").and_then(Value::as_bool) == Some(false)
                        && s(v, "mergeable_state").as_deref() == Some("dirty")
                }
                _ => v.get("mergeable").and_then(Value::as_bool) == Some(false),
            };
            Some(PrInfo {
                number,
                url: web_url(s(v, "html_url")),
                state,
                draft: v.get("draft").and_then(Value::as_bool).unwrap_or(false),
                head_sha: s(&head, "sha"),
                head_ref: s(&head, "ref"),
                conflict: conflict && state == PrState::Open,
            })
        }
        GitHost::Gitlab => {
            let number = v.get("iid").and_then(Value::as_i64)?;
            let state = match s(v, "state").as_deref() {
                Some("opened") => PrState::Open,
                Some("merged") => PrState::Merged,
                _ => PrState::Closed,
            };
            Some(PrInfo {
                number,
                url: web_url(s(v, "web_url")),
                state,
                draft: v.get("draft").and_then(Value::as_bool).unwrap_or(false),
                head_sha: s(v, "sha"),
                head_ref: s(v, "source_branch"),
                conflict: v
                    .get("has_conflicts")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && state == PrState::Open,
            })
        }
    }
}

fn github_check_state(c: &Value) -> CheckState {
    if s(c, "status").as_deref() != Some("completed") {
        return CheckState::Pending;
    }
    match s(c, "conclusion").as_deref() {
        Some("success") => CheckState::Success,
        Some("failure" | "timed_out") => CheckState::Failure,
        // neutral, cancelled, skipped, stale, action_required (needs a
        // human on the host, not code changes).
        _ => CheckState::Neutral,
    }
}

fn gitlab_state(status: &str, allow_failure: bool) -> CheckState {
    match status {
        "success" => CheckState::Success,
        "failed" if allow_failure => CheckState::Neutral,
        "failed" => CheckState::Failure,
        "canceled" | "canceling" | "skipped" | "manual" => CheckState::Neutral,
        _ => CheckState::Pending,
    }
}

/// Commit statuses (GitHub combined status `state`, Gitea `status`).
fn statuses(body: &Value, field: &str, rep: &mut CiReport) {
    for st in body
        .get("statuses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let state = match s(st, field).as_deref() {
            Some("success") => CheckState::Success,
            Some("failure" | "error") => CheckState::Failure,
            Some("pending") => CheckState::Pending,
            _ => CheckState::Neutral,
        };
        let name = s(st, "context").unwrap_or_else(|| "Status".into());
        if state == CheckState::Failure {
            rep.details.push(CiDetail {
                name: name.clone(),
                text: cut(&s(st, "description").unwrap_or_default(), MAX_ITEM_TEXT),
            });
        }
        rep.checks.push(Check {
            name,
            state,
            url: web_url(s(st, "target_url")),
        });
    }
}

fn parse_comment(kind: GitHost, source: FeedbackSource, v: &Value) -> Option<Comment> {
    let id = v.get("id").and_then(Value::as_i64)?.to_string();
    let user = v
        .get("user")
        .or_else(|| v.get("author"))
        .cloned()
        .unwrap_or(Value::Null);
    let author = s(&user, "login").or_else(|| s(&user, "username"))?;
    let bot = s(&user, "type").as_deref() == Some("Bot")
        || user.get("bot").and_then(Value::as_bool) == Some(true)
        || author.ends_with("[bot]");
    let body = cut(&s(v, "body").unwrap_or_default(), MAX_ITEM_TEXT);
    let mut c = Comment {
        source,
        id,
        author,
        author_id: user.get("id").and_then(Value::as_i64),
        bot,
        body,
        path: s(v, "path"),
        line: v
            .get("line")
            .or_else(|| v.get("original_line"))
            .or_else(|| v.get("position"))
            .and_then(Value::as_i64),
        review_state: None,
        system: false,
    };
    match (kind, source) {
        (GitHost::Gitlab, _) => {
            c.system = v.get("system").and_then(Value::as_bool).unwrap_or(false);
            if s(v, "type").as_deref() == Some("DiffNote") {
                c.source = FeedbackSource::ReviewComment;
                let pos = v.get("position").cloned().unwrap_or(Value::Null);
                c.path = s(&pos, "new_path").or_else(|| s(&pos, "old_path"));
                c.line = pos
                    .get("new_line")
                    .or_else(|| pos.get("old_line"))
                    .and_then(Value::as_i64);
            }
        }
        (_, FeedbackSource::Review) => {
            let state = s(v, "state").unwrap_or_default();
            // Not submitted yet: invisible to others, not feedback (yet).
            if state == "PENDING" {
                return None;
            }
            c.review_state = Some(state);
            c.path = None;
            c.line = None;
        }
        _ => {}
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn rate_limit_headers() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut h = HeaderMap::new();
        assert_eq!(rate_pause(&h, now), None);
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("12"));
        assert_eq!(rate_pause(&h, now), None);
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        h.insert("x-ratelimit-reset", HeaderValue::from_static("1700000600"));
        assert_eq!(
            rate_pause(&h, now),
            Some(now + chrono::Duration::seconds(600))
        );
        // Far future reset is capped, past reset falls back to a minute.
        h.insert("x-ratelimit-reset", HeaderValue::from_static("1800000000"));
        assert_eq!(rate_pause(&h, now), Some(now + chrono::Duration::hours(1)));
        h.insert("x-ratelimit-reset", HeaderValue::from_static("1"));
        assert_eq!(
            rate_pause(&h, now),
            Some(now + chrono::Duration::seconds(60))
        );
        let mut g = HeaderMap::new();
        g.insert("ratelimit-remaining", HeaderValue::from_static("0"));
        g.insert("ratelimit-reset", HeaderValue::from_static("1700000030"));
        assert_eq!(
            rate_pause(&g, now),
            Some(now + chrono::Duration::seconds(30))
        );
        g.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(
            rate_pause(&g, now),
            Some(now + chrono::Duration::seconds(7))
        );
    }

    #[test]
    fn permissions_per_host() {
        assert!(write_access(
            GitHost::Github,
            &json!({"permission": "write"})
        ));
        assert!(write_access(
            GitHost::Github,
            &json!({"permission": "admin"})
        ));
        assert!(!write_access(
            GitHost::Github,
            &json!({"permission": "read", "role_name": "triage"})
        ));
        assert!(!write_access(
            GitHost::Github,
            &json!({"permission": "none"})
        ));
        assert!(write_access(GitHost::Gitlab, &json!({"access_level": 30})));
        assert!(!write_access(GitHost::Gitlab, &json!({"access_level": 20})));
        assert!(write_access(
            GitHost::Gitea,
            &json!({"permission": "owner"})
        ));
        assert!(!write_access(
            GitHost::Gitea,
            &json!({"permission": "read"})
        ));
        assert!(!write_access(GitHost::Gitea, &json!({})));
        assert!(valid_login("alice-1") && valid_login("a.b_c"));
        for bad in ["", "../x", "a/b", "x?y", "bot[bot]", ".hidden", "a b"] {
            assert!(!valid_login(bad), "{bad}");
        }
    }

    #[test]
    fn pull_requests_per_host() {
        let gh = parse_pr(
            GitHost::Github,
            &json!({"number": 7, "state": "closed", "merged": true, "draft": false,
                    "html_url": "https://github.com/a/b/pull/7", "head": {"sha": "abc1234", "ref": "queue/x"}}),
        )
        .unwrap();
        assert_eq!((gh.number, gh.state), (7, PrState::Merged));
        let gh = parse_pr(
            GitHost::Github,
            &json!({"number": 8, "state": "open", "mergeable": false, "mergeable_state": "dirty", "head": {}}),
        )
        .unwrap();
        assert!(gh.conflict && gh.state == PrState::Open);
        let gl = parse_pr(
            GitHost::Gitlab,
            &json!({"iid": 3, "state": "opened", "draft": true, "sha": "def5678", "has_conflicts": true,
                    "web_url": "javascript:alert(1)", "source_branch": "queue/y"}),
        )
        .unwrap();
        assert_eq!(
            (gl.number, gl.state, gl.draft, gl.conflict),
            (3, PrState::Open, true, true)
        );
        assert_eq!(gl.url, None, "only http(s) links");
        let gt = parse_pr(
            GitHost::Gitea,
            &json!({"number": 2, "state": "closed", "merged": false, "head": {"sha": "1111111"}}),
        )
        .unwrap();
        assert_eq!(gt.state, PrState::Closed);
    }

    #[test]
    fn checks_per_host() {
        let c = |status: &str, concl: Option<&str>| json!({"status": status, "conclusion": concl});
        assert_eq!(
            github_check_state(&c("in_progress", None)),
            CheckState::Pending
        );
        assert_eq!(
            github_check_state(&c("completed", Some("failure"))),
            CheckState::Failure
        );
        assert_eq!(
            github_check_state(&c("completed", Some("timed_out"))),
            CheckState::Failure
        );
        assert_eq!(
            github_check_state(&c("completed", Some("action_required"))),
            CheckState::Neutral
        );
        assert_eq!(
            github_check_state(&c("completed", Some("success"))),
            CheckState::Success
        );
        assert_eq!(gitlab_state("failed", false), CheckState::Failure);
        assert_eq!(gitlab_state("failed", true), CheckState::Neutral);
        assert_eq!(gitlab_state("running", false), CheckState::Pending);
        let mut rep = CiReport::default();
        statuses(
            &json!({"statuses": [{"context": "ci/lint", "status": "failure", "description": "2 errors"},
                                 {"context": "ci/test", "status": "success", "target_url": "https://ci/1"}]}),
            "status",
            &mut rep,
        );
        assert_eq!(rep.checks.len(), 2);
        assert_eq!(rep.details[0].text, "2 errors");
        assert_eq!(rep.checks[1].url.as_deref(), Some("https://ci/1"));
    }

    #[test]
    fn comments_and_triggers() {
        let inline = parse_comment(
            GitHost::Github,
            FeedbackSource::ReviewComment,
            &json!({"id": 1, "body": "use a constant", "path": "src/a.rs", "line": 4,
                    "user": {"login": "alice", "id": 9, "type": "User"}}),
        )
        .unwrap();
        assert!(inline.triggers() && !inline.bot);
        let general = parse_comment(
            GitHost::Github,
            FeedbackSource::Comment,
            &json!({"id": 2, "body": "nice!", "user": {"login": "bob", "type": "User"}}),
        )
        .unwrap();
        assert!(!general.triggers());
        let asked = Comment {
            body: "/QUEUE please rename".into(),
            ..general.clone()
        };
        assert!(asked.triggers());
        let bot = parse_comment(
            GitHost::Github,
            FeedbackSource::Comment,
            &json!({"id": 3, "body": "/queue", "user": {"login": "ci[bot]", "type": "Bot"}}),
        )
        .unwrap();
        assert!(bot.bot);
        let review = |state: &str, body: &str| {
            parse_comment(
                GitHost::Github,
                FeedbackSource::Review,
                &json!({"id": 4, "state": state, "body": body, "user": {"login": "carol"}}),
            )
        };
        assert!(review("CHANGES_REQUESTED", "split this")
            .unwrap()
            .triggers());
        assert!(!review("CHANGES_REQUESTED", "").unwrap().triggers());
        assert!(!review("APPROVED", "lgtm").unwrap().triggers());
        assert!(review("PENDING", "x").is_none());
        let gitea_review = parse_comment(
            GitHost::Gitea,
            FeedbackSource::Review,
            &json!({"id": 5, "state": "REQUEST_CHANGES", "body": "fix", "user": {"login": "dan"}}),
        )
        .unwrap();
        assert!(gitea_review.triggers());
        let note = parse_comment(
            GitHost::Gitlab,
            FeedbackSource::Comment,
            &json!({"id": 6, "body": "x", "type": "DiffNote", "system": false,
                    "position": {"new_path": "a.py", "new_line": 3}, "author": {"username": "eve", "id": 4}}),
        )
        .unwrap();
        assert_eq!(note.source, FeedbackSource::ReviewComment);
        assert_eq!(
            (note.path.as_deref(), note.line, note.author_id),
            (Some("a.py"), Some(3), Some(4))
        );
        let sys = parse_comment(
            GitHost::Gitlab,
            FeedbackSource::Comment,
            &json!({"id": 7, "body": "/queue added 1 commit", "system": true, "author": {"username": "eve"}}),
        )
        .unwrap();
        assert!(!sys.triggers());
        // Long bodies are cut (bounded prompt).
        let long = parse_comment(
            GitHost::Github,
            FeedbackSource::Comment,
            &json!({"id": 8, "body": "x".repeat(MAX_ITEM_TEXT * 2), "user": {"login": "f"}}),
        )
        .unwrap();
        assert!(long.body.len() <= MAX_ITEM_TEXT + 4);
        assert!(tail("abcdef", 3).ends_with("def"));
    }
}
