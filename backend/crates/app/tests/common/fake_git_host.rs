//! Fake git host for tests (WP05): GitHub (`/github/api/v3`), GitLab
//! (`/gitlab/api/v4`) and Gitea (`/gitea/api/v1`) REST endpoints that Queue
//! uses, modelled on the official API descriptions (see `githost.rs`).
//! No real external host is ever called in tests.
//!
//! State is kept per `kind:owner/repo`, so parallel tests (and Playwright
//! workers) never share a repository. Head commits are read live from the
//! repository's bare git remote. Tests steer it through `/_fake/...`:
//!
//! * `POST /_fake/setup` `{kind, repo, token, bare, permissions, users}`
//! * `POST /_fake/checks` `{kind, repo, sha?, branch?, checks: [{name, state, summary}]}`
//!   (`state`: success | failure | pending)
//! * `POST /_fake/comment` `{kind, repo, number, type, login, body, path?, line?, state?, bot?}`
//!   (`type`: issue | review_comment | review)
//! * `POST /_fake/pr` `{kind, repo, number, state?, merged?, mergeable?}`
//! * `POST /_fake/limit` `{kind, repo, secs?}`: the next API call answers
//!   429 with `Retry-After: secs` (default 1)
//! * `GET  /_fake/state?kind=..&repo=..`: pull requests, request log, counters
//!
//! Every API request is logged (method + path) so tests can assert that
//! Queue never approves, merges, reviews or comments.
#![allow(dead_code)]
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const BOT_LOGIN: &str = "queue-bot";
pub const BOT_ID: i64 = 1;

#[derive(Debug, Clone, Default)]
pub struct FakePr {
    pub number: i64,
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub open: bool,
    pub merged: bool,
    pub labels: Vec<String>,
    pub mergeable: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct FakeCheck {
    pub name: String,
    pub state: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct FakeComment {
    pub id: i64,
    pub number: i64,
    pub kind: String,
    pub login: String,
    pub user_id: i64,
    pub bot: bool,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub state: Option<String>,
    /// Gitea: inline comments belong to a review.
    pub review_id: Option<i64>,
}

#[derive(Debug, Default)]
pub struct FakeRepo {
    pub token: String,
    pub bare: Option<PathBuf>,
    /// login → permission (github/gitea: admin|write|read|none; gitlab: access level).
    pub permissions: HashMap<String, Value>,
    /// login → user id.
    pub users: HashMap<String, i64>,
    pub prs: Vec<FakePr>,
    /// sha → checks.
    pub checks: HashMap<String, Vec<FakeCheck>>,
    pub comments: Vec<FakeComment>,
    pub requests: Vec<String>,
    pub not_modified: usize,
    pub rate_limit_next: bool,
    /// Seconds in the `Retry-After` of that 429.
    pub retry_after: u64,
    /// Comment lists hang until released (restart tests).
    pub hang: bool,
    /// GitHub: drafts not available (private repository on GitHub Free) ⇒ 422.
    pub no_drafts: bool,
    pub next_id: i64,
}

#[derive(Default)]
pub struct Fake {
    pub repos: Mutex<HashMap<String, FakeRepo>>,
    pub release: tokio::sync::Notify,
}

pub type FakeHost = Arc<Fake>;

fn key(kind: &str, repo: &str) -> String {
    format!("{kind}:{repo}")
}

impl Fake {
    pub fn with<R>(&self, kind: &str, repo: &str, f: impl FnOnce(&mut FakeRepo) -> R) -> R {
        let mut g = self.repos.lock().unwrap();
        f(g.entry(key(kind, repo)).or_default())
    }

    pub fn head_of(&self, kind: &str, repo: &str, branch: &str) -> Option<String> {
        let bare = self.with(kind, repo, |r| r.bare.clone())?;
        let out = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .args([
                "rev-parse",
                "-q",
                "--verify",
                &format!("refs/heads/{branch}"),
            ])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// Starts the fake on a free port; returns its base URL and handle.
pub async fn start() -> (String, FakeHost) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let fake = FakeHost::default();
    let app = router(fake.clone());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, fake)
}

pub fn router(fake: FakeHost) -> Router {
    Router::new()
        .route("/_fake/setup", post(setup))
        .route("/_fake/checks", post(set_checks))
        .route("/_fake/comment", post(add_comment))
        .route("/_fake/pr", post(set_pr))
        .route("/_fake/limit", post(limit))
        .route("/_fake/state", get(state))
        .fallback(api)
        .with_state(fake)
}

#[derive(Deserialize)]
struct Setup {
    kind: String,
    repo: String,
    token: String,
    #[serde(default)]
    bare: Option<PathBuf>,
    #[serde(default)]
    permissions: HashMap<String, Value>,
    #[serde(default)]
    users: HashMap<String, i64>,
    #[serde(default)]
    no_drafts: bool,
}

async fn setup(State(f): State<FakeHost>, Json(s): Json<Setup>) -> Json<Value> {
    f.with(&s.kind, &s.repo, |r| {
        r.token = s.token;
        r.bare = s.bare;
        r.permissions = s.permissions;
        r.users = s.users;
        r.no_drafts = s.no_drafts;
        r.next_id = r.next_id.max(100);
    });
    Json(json!({"ok": true}))
}

#[derive(Deserialize)]
struct Checks {
    kind: String,
    repo: String,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    checks: Vec<Value>,
}

async fn set_checks(State(f): State<FakeHost>, Json(c): Json<Checks>) -> Response {
    let sha = match (c.sha, c.branch) {
        (Some(s), _) => s,
        (None, Some(b)) => match f.head_of(&c.kind, &c.repo, &b) {
            Some(s) => s,
            None => return (StatusCode::NOT_FOUND, "no such branch").into_response(),
        },
        _ => return (StatusCode::BAD_REQUEST, "sha or branch").into_response(),
    };
    let list = c
        .checks
        .iter()
        .map(|v| FakeCheck {
            name: v["name"].as_str().unwrap_or("check").into(),
            state: v["state"].as_str().unwrap_or("success").into(),
            summary: v["summary"].as_str().unwrap_or("").into(),
        })
        .collect();
    f.with(&c.kind, &c.repo, |r| r.checks.insert(sha.clone(), list));
    Json(json!({"sha": sha})).into_response()
}

#[derive(Deserialize)]
struct NewComment {
    kind: String,
    repo: String,
    number: i64,
    #[serde(rename = "type")]
    ty: String,
    login: String,
    body: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<i64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    bot: bool,
}

async fn add_comment(State(f): State<FakeHost>, Json(c): Json<NewComment>) -> Json<Value> {
    let id = f.with(&c.kind, &c.repo, |r| {
        r.next_id += 1;
        let id = r.next_id;
        let user_id = *r.users.entry(c.login.clone()).or_insert(1000 + id);
        // Gitea inline comments hang off a review of the same author.
        let review_id = if c.kind == "gitea" && c.ty == "review_comment" {
            r.next_id += 1;
            let rid = r.next_id;
            r.comments.push(FakeComment {
                id: rid,
                number: c.number,
                kind: "review".into(),
                login: c.login.clone(),
                user_id,
                bot: c.bot,
                body: String::new(),
                path: None,
                line: None,
                state: Some("COMMENT".into()),
                review_id: None,
            });
            Some(rid)
        } else {
            None
        };
        r.comments.push(FakeComment {
            id,
            number: c.number,
            kind: c.ty.clone(),
            login: c.login.clone(),
            user_id,
            bot: c.bot,
            body: c.body.clone(),
            path: c.path.clone(),
            line: c.line,
            state: c.state.clone(),
            review_id,
        });
        id
    });
    Json(json!({"id": id}))
}

#[derive(Deserialize)]
struct SetPr {
    kind: String,
    repo: String,
    number: i64,
    #[serde(default)]
    open: Option<bool>,
    #[serde(default)]
    merged: Option<bool>,
    #[serde(default)]
    mergeable: Option<bool>,
}

async fn set_pr(State(f): State<FakeHost>, Json(s): Json<SetPr>) -> Json<Value> {
    f.with(&s.kind, &s.repo, |r| {
        if let Some(p) = r.prs.iter_mut().find(|p| p.number == s.number) {
            if let Some(o) = s.open {
                p.open = o;
            }
            if let Some(m) = s.merged {
                p.merged = m;
                if m {
                    p.open = false;
                }
            }
            if s.mergeable.is_some() {
                p.mergeable = s.mergeable;
            }
        }
    });
    Json(json!({"ok": true}))
}

#[derive(Deserialize)]
struct Which {
    kind: String,
    repo: String,
}

#[derive(Deserialize)]
struct Limit {
    kind: String,
    repo: String,
    #[serde(default)]
    secs: Option<u64>,
}

async fn limit(State(f): State<FakeHost>, Json(w): Json<Limit>) -> Json<Value> {
    f.with(&w.kind, &w.repo, |r| {
        r.rate_limit_next = true;
        r.retry_after = w.secs.unwrap_or(1);
    });
    Json(json!({"ok": true}))
}

async fn state(State(f): State<FakeHost>, Query(w): Query<Which>) -> Json<Value> {
    f.with(&w.kind, &w.repo, |r| {
        Json(json!({
            "prs": r.prs.iter().map(|p| json!({
                "number": p.number, "head": p.head, "base": p.base, "title": p.title,
                "body": p.body, "draft": p.draft, "open": p.open, "merged": p.merged, "labels": p.labels,
            })).collect::<Vec<_>>(),
            "requests": r.requests,
            "not_modified": r.not_modified,
        }))
    })
}

fn user_json(kind: &str, login: &str, id: i64, bot: bool) -> Value {
    match kind {
        "gitlab" => json!({"username": login, "id": id, "bot": bot}),
        _ => json!({"login": login, "id": id, "type": if bot { "Bot" } else { "User" }}),
    }
}

fn reply(headers: &HeaderMap, body: Value, repo: Option<&mut FakeRepo>) -> Response {
    let text = body.to_string();
    let etag = format!("\"{}\"", &hex(&Sha256::digest(text.as_bytes()))[..16]);
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        if let Some(r) = repo {
            r.not_modified += 1;
        }
        return (StatusCode::NOT_MODIFIED, [("etag", etag)]).into_response();
    }
    (
        StatusCode::OK,
        [("etag", etag), ("content-type", "application/json".into())],
        text,
    )
        .into_response()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn page<T: Clone>(items: &[T], q: &HashMap<String, String>) -> Vec<T> {
    let per: usize = q
        .get("per_page")
        .or_else(|| q.get("limit"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let page: usize = q
        .get("page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    items
        .iter()
        .skip((page - 1) * per)
        .take(per)
        .cloned()
        .collect()
}

fn query(uri: &Uri) -> HashMap<String, String> {
    uri.query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default()
}

/// The REST API of all three hosts.
async fn api(
    State(f): State<FakeHost>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let q = query(&uri);
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let (kind, rest) = match segs.as_slice() {
        ["github", "api", "v3", rest @ ..] => ("github", rest.to_vec()),
        ["gitlab", "api", "v4", rest @ ..] => ("gitlab", rest.to_vec()),
        ["gitea", "api", "v1", rest @ ..] => ("gitea", rest.to_vec()),
        _ => return (StatusCode::NOT_FOUND, "unknown").into_response(),
    };
    // Which repository: from the path, or (GET /user) any repo whose token matches.
    let token = match kind {
        "github" => headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string),
        "gitlab" => headers
            .get("private-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        _ => headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("token "))
            .map(str::to_string),
    };
    let repo_name = match (kind, rest.as_slice()) {
        ("gitlab", ["projects", id, ..]) => id.replace("%2F", "/").replace("%2f", "/"),
        (_, ["repos", o, r, ..]) => format!("{o}/{r}"),
        _ => {
            let g = f.repos.lock().unwrap();
            match g.iter().find(|(k, r)| {
                k.starts_with(&format!("{kind}:")) && Some(&r.token) == token.as_ref()
            }) {
                Some((k, _)) => k.split_once(':').unwrap().1.to_string(),
                None => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"message": "Bad credentials"})),
                    )
                        .into_response()
                }
            }
        }
    };
    let hang = f.with(kind, &repo_name, |r| {
        r.requests.push(format!("{method} {path}"));
        r.hang
    });
    // Auth and rate limit.
    let denied = f.with(kind, &repo_name, |r| {
        if r.token.is_empty() || Some(&r.token) != token.as_ref() {
            return Some(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"message": "Bad credentials"})),
                )
                    .into_response(),
            );
        }
        if r.rate_limit_next {
            r.rate_limit_next = false;
            return Some(
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", r.retry_after.max(1).to_string())],
                    Json(json!({"message": "API rate limit exceeded"})),
                )
                    .into_response(),
            );
        }
        None
    });
    if let Some(d) = denied {
        return d;
    }
    let is_comment_list = method == Method::GET
        && (path.ends_with("/comments") || path.ends_with("/notes") || path.ends_with("/reviews"));
    if hang && is_comment_list {
        f.release.notified().await;
    }
    let input: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let rest: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
    let r: Vec<&str> = rest.iter().map(String::as_str).collect();
    let mut g = f.repos.lock().unwrap();
    let repo = g.get_mut(&key(kind, &repo_name)).unwrap();
    let bare = repo.bare.clone();
    let head_sha = |branch: &str| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(bare.as_ref()?)
            .args([
                "rev-parse",
                "-q",
                "--verify",
                &format!("refs/heads/{branch}"),
            ])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let pr_json = |p: &FakePr| -> Value {
        let sha = head_sha(&p.head);
        match kind {
            "gitlab" => json!({
                "iid": p.number, "state": if p.merged { "merged" } else if p.open { "opened" } else { "closed" },
                "draft": p.draft || p.title.starts_with("Draft:"), "sha": sha, "source_branch": p.head,
                "target_branch": p.base, "title": p.title, "has_conflicts": p.mergeable == Some(false),
                "web_url": format!("https://gitlab.example/{repo_name}/-/merge_requests/{}", p.number),
            }),
            _ => json!({
                "number": p.number, "state": if p.open { "open" } else { "closed" }, "merged": p.merged,
                "draft": p.draft || p.title.starts_with("WIP:"), "title": p.title, "body": p.body,
                "head": {"ref": p.head, "sha": sha}, "base": {"ref": p.base},
                "mergeable": p.mergeable, "mergeable_state": if p.mergeable == Some(false) { "dirty" } else { "clean" },
                "html_url": format!("https://{kind}.example/{repo_name}/pull/{}", p.number),
            }),
        }
    };
    let comments_of = |repo: &FakeRepo, number: i64, kinds: &[&str]| -> Vec<FakeComment> {
        repo.comments
            .iter()
            .filter(|c| c.number == number && kinds.contains(&c.kind.as_str()))
            .cloned()
            .collect()
    };
    let not_found =
        || (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response();
    match (kind, method.as_str(), r.as_slice()) {
        (_, "GET", ["user"]) => reply(&headers, user_json(kind, BOT_LOGIN, BOT_ID, false), None),
        // Permission checks.
        ("github" | "gitea", "GET", ["repos", _, _, "collaborators", login, "permission"]) => {
            match repo.permissions.get(*login) {
                Some(p) => reply(
                    &headers,
                    json!({"permission": p, "user": {"login": login}}),
                    None,
                ),
                None if *login == BOT_LOGIN => {
                    reply(&headers, json!({"permission": "write"}), None)
                }
                None => not_found(),
            }
        }
        ("gitlab", "GET", ["projects", _, "members", "all", uid]) => {
            let uid: i64 = uid.parse().unwrap_or(-1);
            if uid == BOT_ID {
                return reply(&headers, json!({"id": uid, "access_level": 40}), None);
            }
            let login = repo
                .users
                .iter()
                .find(|(_, id)| **id == uid)
                .map(|(l, _)| l.clone());
            match login.and_then(|l| repo.permissions.get(&l).cloned()) {
                Some(level) => reply(&headers, json!({"id": uid, "access_level": level}), None),
                None => not_found(),
            }
        }
        // Pull requests.
        ("github" | "gitea", "GET", ["repos", _, _, "pulls"]) => {
            let want_head = q
                .get("head")
                .map(|h| h.split(':').next_back().unwrap_or(h).to_string());
            let list: Vec<Value> = repo
                .prs
                .iter()
                .filter(|p| {
                    q.get("state")
                        .map(|s| s != "open" || p.open)
                        .unwrap_or(true)
                })
                .filter(|p| want_head.as_ref().is_none_or(|h| *h == p.head))
                .map(pr_json)
                .collect();
            reply(&headers, Value::Array(page(&list, &q)), None)
        }
        ("gitlab", "GET", ["projects", _, "merge_requests"]) => {
            let list: Vec<Value> = repo
                .prs
                .iter()
                .filter(|p| {
                    q.get("state")
                        .map(|s| s != "opened" || p.open)
                        .unwrap_or(true)
                })
                .filter(|p| q.get("source_branch").is_none_or(|h| *h == p.head))
                .map(pr_json)
                .collect();
            reply(&headers, Value::Array(page(&list, &q)), None)
        }
        ("github" | "gitea", "POST", ["repos", _, _, "pulls"])
        | ("gitlab", "POST", ["projects", _, "merge_requests"]) => {
            let (head, base, title, body) = match kind {
                "gitlab" => (
                    input["source_branch"].as_str().unwrap_or_default(),
                    input["target_branch"].as_str().unwrap_or_default(),
                    input["title"].as_str().unwrap_or_default(),
                    input["description"].as_str().unwrap_or_default(),
                ),
                _ => (
                    input["head"].as_str().unwrap_or_default(),
                    input["base"].as_str().unwrap_or_default(),
                    input["title"].as_str().unwrap_or_default(),
                    input["body"].as_str().unwrap_or_default(),
                ),
            };
            if kind == "github" && repo.no_drafts && input["draft"].as_bool() == Some(true) {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"message": "Validation Failed", "errors": [{"message": "Draft pull requests are not supported in this repository."}]})),
                )
                    .into_response();
            }
            if head_sha(head).is_none() {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"message": "head branch does not exist"})),
                )
                    .into_response();
            }
            let number = repo.prs.len() as i64 + 1;
            let labels = match kind {
                "gitlab" => input["labels"]
                    .as_str()
                    .map(|l| l.split(',').map(str::to_string).collect())
                    .unwrap_or_default(),
                _ => vec![],
            };
            let pr = FakePr {
                number,
                head: head.into(),
                base: base.into(),
                title: title.into(),
                body: body.into(),
                draft: input["draft"].as_bool().unwrap_or(false),
                open: true,
                merged: false,
                labels,
                mergeable: Some(true),
            };
            let v = pr_json(&pr);
            repo.prs.push(pr);
            (StatusCode::CREATED, Json(v)).into_response()
        }
        ("github" | "gitea", "GET", ["repos", _, _, "pulls", n])
        | ("gitlab", "GET", ["projects", _, "merge_requests", n]) => {
            let n: i64 = n.parse().unwrap_or(-1);
            match repo.prs.iter().find(|p| p.number == n) {
                Some(p) => {
                    let v = pr_json(p);
                    reply(&headers, v, Some(repo))
                }
                None => not_found(),
            }
        }
        ("github" | "gitea", "POST", ["repos", _, _, "issues", n, "labels"]) => {
            let n: i64 = n.parse().unwrap_or(-1);
            let add: Vec<String> = input["labels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            match repo.prs.iter_mut().find(|p| p.number == n) {
                Some(p) => {
                    p.labels.extend(add);
                    Json(json!(p
                        .labels
                        .iter()
                        .map(|l| json!({"name": l}))
                        .collect::<Vec<_>>()))
                    .into_response()
                }
                None => not_found(),
            }
        }
        // CI.
        ("github", "GET", ["repos", _, _, "commits", sha, "check-runs"]) => {
            let runs: Vec<Value> = repo
                .checks
                .get(*sha)
                .into_iter()
                .flatten()
                .map(|c| {
                    let (status, concl) = match c.state.as_str() {
                        "pending" => ("in_progress", Value::Null),
                        s => ("completed", json!(s)),
                    };
                    json!({"name": c.name, "status": status, "conclusion": concl,
                           "html_url": format!("https://ci.example/{}", c.name),
                           "output": {"title": format!("{} {}", c.name, c.state), "summary": c.summary, "text": null}})
                })
                .collect();
            let n = runs.len();
            reply(
                &headers,
                json!({"total_count": n, "check_runs": page(&runs, &q)}),
                None,
            )
        }
        ("github", "GET", ["repos", _, _, "commits", _, "status"]) => reply(
            &headers,
            json!({"state": "success", "statuses": [], "total_count": 0}),
            None,
        ),
        ("gitea", "GET", ["repos", _, _, "commits", sha, "status"]) => {
            let st: Vec<Value> = repo
                .checks
                .get(*sha)
                .into_iter()
                .flatten()
                .map(|c| {
                    json!({"context": c.name, "status": c.state, "description": c.summary,
                                "target_url": format!("https://ci.example/{}", c.name)})
                })
                .collect();
            reply(
                &headers,
                json!({"sha": sha, "state": "pending", "statuses": st}),
                None,
            )
        }
        ("gitlab", "GET", ["projects", _, "merge_requests", n, "pipelines"]) => {
            let n: i64 = n.parse().unwrap_or(-1);
            let Some(p) = repo.prs.iter().find(|p| p.number == n) else {
                return not_found();
            };
            let sha = head_sha(&p.head);
            let pipes: Vec<Value> = sha
                .iter()
                .filter(|s| repo.checks.contains_key(*s))
                .map(|s| json!({"id": 500 + n, "sha": s, "ref": p.head, "status": "running"}))
                .collect();
            reply(&headers, Value::Array(pipes), None)
        }
        ("gitlab", "GET", ["projects", _, "pipelines", pid, "jobs"]) => {
            let n = pid.parse::<i64>().unwrap_or(0) - 500;
            let sha = repo
                .prs
                .iter()
                .find(|p| p.number == n)
                .and_then(|p| head_sha(&p.head));
            let jobs: Vec<Value> = sha
                .and_then(|s| repo.checks.get(&s).cloned())
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let st = match c.state.as_str() {
                        "failure" => "failed",
                        "pending" => "running",
                        s => s,
                    };
                    json!({"id": 9000 + i, "name": c.name, "status": st, "allow_failure": false,
                           "failure_reason": if st == "failed" { "script_failure" } else { "" },
                           "web_url": format!("https://gitlab.example/jobs/{}", 9000 + i)})
                })
                .collect();
            reply(&headers, Value::Array(page(&jobs, &q)), None)
        }
        ("gitlab", "GET", ["projects", _, "jobs", jid, "trace"]) => {
            let i = jid.parse::<usize>().unwrap_or(0).saturating_sub(9000);
            let text = repo
                .checks
                .values()
                .flat_map(|v| v.get(i).cloned())
                .find(|c| c.state == "failure")
                .map(|c| {
                    format!(
                        "$ run tests\n{}\nERROR: Job failed: exit code 1\n",
                        c.summary
                    )
                })
                .unwrap_or_default();
            (StatusCode::OK, text).into_response()
        }
        // Comments.
        ("github" | "gitea", "GET", ["repos", _, _, "issues", n, "comments"]) => {
            let n: i64 = n.parse().unwrap_or(-1);
            let list: Vec<Value> = comments_of(repo, n, &["issue"])
                .iter()
                .map(|c| json!({"id": c.id, "body": c.body, "user": user_json(kind, &c.login, c.user_id, c.bot)}))
                .collect();
            let v = Value::Array(page(&list, &q));
            reply(&headers, v, Some(repo))
        }
        ("github", "GET", ["repos", _, _, "pulls", n, "comments"]) => {
            let n: i64 = n.parse().unwrap_or(-1);
            let list: Vec<Value> = comments_of(repo, n, &["review_comment"])
                .iter()
                .map(|c| {
                    json!({"id": c.id, "body": c.body, "path": c.path, "line": c.line,
                                "user": user_json(kind, &c.login, c.user_id, c.bot)})
                })
                .collect();
            let v = Value::Array(page(&list, &q));
            reply(&headers, v, Some(repo))
        }
        ("github" | "gitea", "GET", ["repos", _, _, "pulls", n, "reviews"]) => {
            let n: i64 = n.parse().unwrap_or(-1);
            let all = repo.comments.clone();
            let list: Vec<Value> = comments_of(repo, n, &["review"])
                .iter()
                .map(|c| {
                    let count = all.iter().filter(|x| x.review_id == Some(c.id)).count();
                    json!({"id": c.id, "body": c.body, "state": c.state.clone().unwrap_or_else(|| "COMMENTED".into()),
                           "comments_count": count, "user": user_json(kind, &c.login, c.user_id, c.bot)})
                })
                .collect();
            let v = Value::Array(page(&list, &q));
            reply(&headers, v, Some(repo))
        }
        ("gitea", "GET", ["repos", _, _, "pulls", _, "reviews", rid, "comments"]) => {
            let rid: i64 = rid.parse().unwrap_or(-1);
            let list: Vec<Value> = repo
                .comments
                .iter()
                .filter(|c| c.review_id == Some(rid))
                .map(|c| {
                    json!({"id": c.id, "body": c.body, "path": c.path, "position": c.line,
                                "user": user_json(kind, &c.login, c.user_id, c.bot)})
                })
                .collect();
            reply(&headers, Value::Array(list), None)
        }
        ("gitlab", "GET", ["projects", _, "merge_requests", n, "notes"]) => {
            let n: i64 = n.parse().unwrap_or(-1);
            let list: Vec<Value> = comments_of(repo, n, &["issue", "review_comment", "system"])
                .iter()
                .map(|c| {
                    let mut v = json!({"id": c.id, "body": c.body, "system": c.kind == "system",
                                       "author": user_json(kind, &c.login, c.user_id, c.bot)});
                    if c.kind == "review_comment" {
                        v["type"] = json!("DiffNote");
                        v["position"] = json!({"new_path": c.path, "new_line": c.line});
                    }
                    v
                })
                .collect();
            let v = Value::Array(page(&list, &q));
            reply(&headers, v, Some(repo))
        }
        _ => not_found(),
    }
}
