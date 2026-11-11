//! WP07: incoming webhooks – HMAC verification (generic and GitHub) and
//! the evaluation of GitHub events against a trigger's filters. Pure logic;
//! the HTTP side is `api::hooks`.
//!
//! Payload fields used here are documented in GitHub's OpenAPI description
//! (`github/rest-api-description`, `components/schemas/webhook-*`) and the
//! webhook docs (`github/docs`, validating deliveries, delivery headers);
//! the exceptions are marked "unverified".
use hmac::{Hmac, Mac};
use queued_api::triggers::{branch_matches, GithubEvent, GithubFilter};
use serde_json::Value;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// `sha256=<hex>` of HMAC-SHA256(secret, parts…).
pub fn signature(secret: &[u8], parts: &[&[u8]]) -> String {
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC takes any key length");
    for p in parts {
        m.update(p);
    }
    let out = m.finalize().into_bytes();
    format!(
        "sha256={}",
        out.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Constant-time check of a `sha256=<hex>` header over `parts`.
pub fn verify(secret: &[u8], parts: &[&[u8]], header: &str) -> bool {
    let Some(hex) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Some(given) = unhex(hex) else {
        return false;
    };
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC takes any key length");
    for p in parts {
        m.update(p);
    }
    // `verify_slice` compares in constant time (subtle).
    m.verify_slice(&given).is_ok()
}

/// What a generic webhook signs: `"{timestamp}.{delivery}.{body}"`. The
/// delivery ID is signed, so a captured request cannot be replayed under a
/// new ID within the time window.
pub fn generic_parts<'a>(timestamp: &'a str, delivery: &'a str, body: &'a [u8]) -> [&'a [u8]; 5] {
    [timestamp.as_bytes(), b".", delivery.as_bytes(), b".", body]
}

/// Delivery IDs: 1–100 characters `[A-Za-z0-9._:-]`.
pub fn valid_delivery(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// A generic body for the agent: pretty JSON, text, or a note for binaries.
pub fn generic_text(body: &[u8]) -> String {
    if body.is_empty() {
        return "(leer)".into();
    }
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        return serde_json::to_string_pretty(&v).unwrap_or_default();
    }
    match std::str::from_utf8(body) {
        Ok(t) => t.to_string(),
        Err(_) => format!("(binäre Daten, {} Bytes – nicht angezeigt)", body.len()),
    }
}

/// The JSON of a GitHub delivery (`application/json` or form-encoded
/// `payload=…`, both documented).
pub fn github_payload(content_type: &str, body: &[u8]) -> Option<Value> {
    if content_type
        .to_ascii_lowercase()
        .starts_with("application/x-www-form-urlencoded")
    {
        let p = url::form_urlencoded::parse(body)
            .find(|(k, _)| k == "payload")
            .map(|(_, v)| v.into_owned())?;
        return serde_json::from_str(&p).ok();
    }
    serde_json::from_slice(body).ok()
}

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(*k))?.as_str()
}

fn n(v: &Value, path: &[&str]) -> Option<u64> {
    path.iter().try_fold(v, |v, k| v.get(*k))?.as_u64()
}

fn b(v: &Value, path: &[&str]) -> bool {
    path.iter()
        .try_fold(v, |v, k| v.get(*k))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn sha(v: Option<&str>) -> Option<&str> {
    v.filter(|h| (h.len() == 40 || h.len() == 64) && h.bytes().all(|c| c.is_ascii_hexdigit()))
}

fn labels(v: &Value, path: &[&str]) -> Vec<String> {
    path.iter()
        .try_fold(v, |v, k| v.get(*k))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Committer of every commit Queue makes (runner `HARDEN`): its pushes must
/// not trigger new tasks (loop guard).
pub const QUEUE_COMMITTER: &str = "queue@localhost";
/// Conclusions of a failed check (enum of `check_run.conclusion`).
pub const FAILED_CONCLUSIONS: [&str; 3] = ["failure", "timed_out", "startup_failure"];

/// A matching event.
#[derive(Debug, Clone, PartialEq)]
pub struct Matched {
    /// Trusted title suffix (fixed words + numbers only).
    pub title_suffix: String,
    /// One task per key and trigger (`None` = no extra dedupe).
    pub dedupe_key: Option<String>,
    pub intro: String,
    pub entries: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Ping,
    Ignore(String),
    Match(Matched),
}

/// Sender is a bot: `sender.type == "Bot"` (value unverified, the schema
/// only says string) or a login ending in `[bot]` (GitHub's documented
/// app logins, e.g. `copilot-swe-agent[bot]`).
fn is_bot(p: &Value) -> bool {
    s(p, &["sender", "type"]) == Some("Bot")
        || s(p, &["sender", "login"]).is_some_and(|l| l.ends_with("[bot]"))
}

fn label_ok(filter: &GithubFilter, have: &[String]) -> bool {
    filter.labels.is_empty()
        || have
            .iter()
            .any(|h| filter.labels.iter().any(|l| l.eq_ignore_ascii_case(h)))
}

fn branch_ok(filter: &GithubFilter, branch: &str) -> bool {
    filter.branches.is_empty() || filter.branches.iter().any(|p| branch_matches(p, branch))
}

fn ignore(why: impl Into<String>) -> Verdict {
    Verdict::Ignore(why.into())
}

fn first_line(t: &str) -> &str {
    t.lines().next().unwrap_or("")
}

/// `event` / `event.action` for the history.
pub fn event_label(event: &str, p: &Value) -> String {
    match s(p, &["action"]) {
        Some(a) if a.len() <= 40 && a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') => {
            format!("{event}.{a}")
        }
        _ => event.chars().take(60).collect(),
    }
}

/// Evaluates a GitHub delivery against the filters. `repo` = expected
/// `owner/repo` (filter or project), `None` = not checked.
pub fn github_verdict(event: &str, p: &Value, f: &GithubFilter, repo: Option<&str>) -> Verdict {
    if event == "ping" {
        return Verdict::Ping;
    }
    if let Some(want) = repo {
        match s(p, &["repository", "full_name"]) {
            Some(have) if have.eq_ignore_ascii_case(want) => {}
            Some(have) => return ignore(format!("Anderes Repository ({have}, erwartet {want})")),
            None => return ignore("Ereignis ohne Repository"),
        }
    }
    let want = |e: GithubEvent| f.events.contains(&e);
    let repo_name = s(p, &["repository", "full_name"])
        .unwrap_or("?")
        .to_string();
    let action = s(p, &["action"]).unwrap_or("");
    match event {
        "push" => {
            if !want(GithubEvent::Push) {
                return ignore("Push ist für diesen Auslöser nicht ausgewählt");
            }
            if !f.include_bots && is_bot(p) {
                return ignore("Von einem Bot ausgelöst");
            }
            let Some(branch) = s(p, &["ref"]).and_then(|r| r.strip_prefix("refs/heads/")) else {
                return ignore("Kein Branch (z. B. ein Tag)");
            };
            if b(p, &["deleted"]) {
                return ignore("Branch wurde gelöscht");
            }
            if branch.starts_with("queue/") {
                return ignore("Arbeits-Branch von Queue");
            }
            if !branch_ok(f, branch) {
                return ignore(format!("Branch „{branch}“ passt nicht zum Filter"));
            }
            if s(p, &["head_commit", "committer", "email"]) == Some(QUEUE_COMMITTER) {
                return ignore("Der neueste Commit stammt von Queue selbst");
            }
            let mut commits = String::new();
            if let Some(list) = p.get("commits").and_then(Value::as_array) {
                for c in list.iter().take(20) {
                    commits.push_str(&format!(
                        "- {} {} ({})\n",
                        s(c, &["id"])
                            .unwrap_or("")
                            .chars()
                            .take(12)
                            .collect::<String>(),
                        first_line(s(c, &["message"]).unwrap_or("")),
                        s(c, &["author", "name"]).unwrap_or("?")
                    ));
                }
                if list.len() > 20 {
                    commits.push_str(&format!("- … {} weitere\n", list.len() - 20));
                }
            }
            Verdict::Match(Matched {
                title_suffix: "GitHub-Push".into(),
                dedupe_key: None,
                intro: format!("A push to a branch of the GitHub repository {repo_name} started this task."),
                entries: vec![
                    (
                        "Push".into(),
                        format!(
                            "Repository: {repo_name}\nBranch: {branch}\nVorher: {}\nNachher: {}\nVon: {}\nVergleich: {}",
                            s(p, &["before"]).unwrap_or("?"),
                            s(p, &["after"]).unwrap_or("?"),
                            s(p, &["pusher", "name"]).unwrap_or("?"),
                            s(p, &["compare"]).unwrap_or("")
                        ),
                    ),
                    ("Commits".into(), commits),
                ],
            })
        }
        "issues" => {
            if !want(GithubEvent::Issues) {
                return ignore("Issues sind für diesen Auslöser nicht ausgewählt");
            }
            if !matches!(action, "opened" | "reopened" | "labeled") {
                return ignore(format!("Aktion „{action}“ wird nicht ausgewertet"));
            }
            if !f.include_bots && is_bot(p) {
                return ignore("Von einem Bot ausgelöst");
            }
            let have = labels(p, &["issue", "labels"]);
            if action == "labeled" {
                let added = s(p, &["label", "name"]).unwrap_or("");
                if f.labels.is_empty() {
                    return ignore("Label hinzugefügt – zählt nur mit Label-Filter");
                }
                if !label_ok(f, &[added.to_string()]) {
                    return ignore("Hinzugefügtes Label passt nicht zum Filter");
                }
            } else if !label_ok(f, &have) {
                return ignore("Issue hat keines der Labels aus dem Filter");
            }
            let Some(num) = n(p, &["issue", "number"]) else {
                return ignore("Issue ohne Nummer");
            };
            Verdict::Match(Matched {
                title_suffix: format!("GitHub-Issue #{num}"),
                dedupe_key: (action != "reopened").then(|| format!("issue:{num}")),
                intro: format!("Issue #{num} of the GitHub repository {repo_name} started this task ({action})."),
                entries: vec![(
                    format!("Issue #{num}"),
                    format!(
                        "Titel: {}\nVon: {}\nLabels: {}\nAdresse: {}\n\n{}",
                        s(p, &["issue", "title"]).unwrap_or(""),
                        s(p, &["issue", "user", "login"]).unwrap_or("?"),
                        have.join(", "),
                        s(p, &["issue", "html_url"]).unwrap_or(""),
                        s(p, &["issue", "body"]).unwrap_or("")
                    ),
                )],
            })
        }
        "pull_request" => {
            if !want(GithubEvent::PullRequest) {
                return ignore("Pull Requests sind für diesen Auslöser nicht ausgewählt");
            }
            if !matches!(
                action,
                "opened" | "reopened" | "synchronize" | "ready_for_review" | "labeled"
            ) {
                return ignore(format!("Aktion „{action}“ wird nicht ausgewertet"));
            }
            if !f.include_bots && is_bot(p) {
                return ignore("Von einem Bot ausgelöst");
            }
            let head = s(p, &["pull_request", "head", "ref"]).unwrap_or("");
            let base = s(p, &["pull_request", "base", "ref"]).unwrap_or("");
            if head.starts_with("queue/") {
                return ignore("Pull Request von Queue selbst (dafür ist der Auto-Fix zuständig)");
            }
            if b(p, &["pull_request", "draft"]) && !f.include_drafts {
                return ignore("Entwurf");
            }
            if !branch_ok(f, base) {
                return ignore(format!("Ziel-Branch „{base}“ passt nicht zum Filter"));
            }
            let have = labels(p, &["pull_request", "labels"]);
            if action == "labeled" {
                if f.labels.is_empty() {
                    return ignore("Label hinzugefügt – zählt nur mit Label-Filter");
                }
                if !label_ok(f, &[s(p, &["label", "name"]).unwrap_or("").to_string()]) {
                    return ignore("Hinzugefügtes Label passt nicht zum Filter");
                }
            } else if !label_ok(f, &have) {
                return ignore("Pull Request hat keines der Labels aus dem Filter");
            }
            let Some(num) = n(p, &["number"]).or_else(|| n(p, &["pull_request", "number"])) else {
                return ignore("Pull Request ohne Nummer");
            };
            let head_sha = sha(s(p, &["pull_request", "head", "sha"]));
            Verdict::Match(Matched {
                title_suffix: format!("GitHub-PR #{num}"),
                dedupe_key: head_sha.map(|h| format!("pr:{num}:{h}")),
                intro: format!("Pull request #{num} of the GitHub repository {repo_name} started this task ({action})."),
                entries: vec![(
                    format!("Pull Request #{num}"),
                    format!(
                        "Titel: {}\nVon: {}\nZiel: {base} ← {head}\nCommit: {}\nLabels: {}\nAdresse: {}\n\n{}",
                        s(p, &["pull_request", "title"]).unwrap_or(""),
                        s(p, &["pull_request", "user", "login"]).unwrap_or("?"),
                        head_sha.unwrap_or("?"),
                        have.join(", "),
                        s(p, &["pull_request", "html_url"]).unwrap_or(""),
                        s(p, &["pull_request", "body"]).unwrap_or("")
                    ),
                )],
            })
        }
        "check_run" => {
            if !want(GithubEvent::CheckFailure) {
                return ignore("Fehlgeschlagene Checks sind für diesen Auslöser nicht ausgewählt");
            }
            if action != "completed" {
                return ignore(format!("Aktion „{action}“ wird nicht ausgewertet"));
            }
            let conclusion = s(p, &["check_run", "conclusion"]).unwrap_or("");
            if !FAILED_CONCLUSIONS.contains(&conclusion) {
                return ignore(format!("Check nicht fehlgeschlagen ({conclusion})"));
            }
            // CI runs as a bot (or the ghost user): no bot filter here.
            let branch = s(p, &["check_run", "check_suite", "head_branch"]);
            if branch.is_some_and(|b| b.starts_with("queue/")) {
                return ignore("Branch von Queue (dafür ist der Auto-Fix zuständig)");
            }
            match branch {
                Some(b) if branch_ok(f, b) => {}
                None if f.branches.is_empty() => {}
                Some(b) => return ignore(format!("Branch „{b}“ passt nicht zum Filter")),
                None => return ignore("Check ohne Branch (z. B. aus einem Fork)"),
            }
            let head_sha = sha(s(p, &["check_run", "head_sha"]));
            let prs: Vec<String> = p
                .pointer("/check_run/pull_requests")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.get("number").and_then(Value::as_u64))
                        .map(|x| format!("#{x}"))
                        .collect()
                })
                .unwrap_or_default();
            Verdict::Match(Matched {
                title_suffix: "GitHub-Check fehlgeschlagen".into(),
                dedupe_key: head_sha.map(|h| format!("check:{h}")),
                intro: format!("A failed check in the GitHub repository {repo_name} started this task."),
                entries: vec![(
                    "Fehlgeschlagener Check".into(),
                    format!(
                        "Check: {}\nErgebnis: {conclusion}\nBranch: {}\nCommit: {}\nPull Requests: {}\nAdresse: {}\nTitel der Ausgabe: {}\n\n{}",
                        s(p, &["check_run", "name"]).unwrap_or("?"),
                        branch.unwrap_or("?"),
                        head_sha.unwrap_or("?"),
                        if prs.is_empty() { "–".into() } else { prs.join(", ") },
                        s(p, &["check_run", "html_url"]).or_else(|| s(p, &["check_run", "details_url"])).unwrap_or(""),
                        s(p, &["check_run", "output", "title"]).unwrap_or(""),
                        s(p, &["check_run", "output", "summary"]).unwrap_or("")
                    ),
                )],
            })
        }
        other => ignore(format!(
            "Ereignis „{}“ wird nicht ausgewertet",
            other.chars().take(40).collect::<String>()
        )),
    }
}

/// Pull request numbers and branches an event is about (for WP05's
/// `pr::poke`: "poll now", nothing from the payload is trusted).
pub fn pr_refs(event: &str, p: &Value) -> (Vec<i64>, Vec<String>) {
    let num = |v: Option<u64>| v.and_then(|x| i64::try_from(x).ok());
    let mut nums: Vec<i64> = vec![];
    let mut branches: Vec<String> = vec![];
    let list = |path: &str| -> Vec<i64> {
        p.pointer(path)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| num(x.get("number").and_then(Value::as_u64)))
                    .collect()
            })
            .unwrap_or_default()
    };
    match event {
        "pull_request" => {
            nums.extend(num(n(p, &["number"])));
            branches.extend(s(p, &["pull_request", "head", "ref"]).map(String::from));
        }
        "pull_request_review" | "pull_request_review_comment" => {
            nums.extend(num(n(p, &["pull_request", "number"])));
        }
        "issue_comment" if p.pointer("/issue/pull_request").is_some() => {
            nums.extend(num(n(p, &["issue", "number"])));
        }
        "check_run" => {
            nums.extend(list("/check_run/pull_requests"));
            branches.extend(s(p, &["check_run", "check_suite", "head_branch"]).map(String::from));
        }
        "check_suite" => {
            nums.extend(list("/check_suite/pull_requests"));
            branches.extend(s(p, &["check_suite", "head_branch"]).map(String::from));
        }
        "push" => {
            branches.extend(
                s(p, &["ref"])
                    .and_then(|r| r.strip_prefix("refs/heads/"))
                    .map(String::from),
            );
        }
        _ => {}
    }
    branches.retain(|b| b.starts_with("queue/") && b.len() <= 64);
    nums.truncate(20);
    (nums, branches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn github_documented_test_vector() {
        // docs.github.com "Validating webhook deliveries" → "Testing the
        // webhook payload validation".
        let secret = b"It's a Secret to Everybody";
        let body = b"Hello, World!";
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert_eq!(signature(secret, &[body]), sig);
        assert!(verify(secret, &[body], sig));
        assert!(!verify(secret, &[b"Hello, World?"], sig));
        assert!(!verify(b"other", &[body], sig));
        assert!(!verify(secret, &[body], &sig.replace("sha256=", "sha1=")));
        assert!(!verify(secret, &[body], "sha256=zz"));
        assert!(!verify(secret, &[body], &sig[..sig.len() - 2]), "truncated");
        assert!(!verify(secret, &[body], ""));
    }

    #[test]
    fn generic_signature_covers_timestamp_delivery_and_body() {
        let k = b"k";
        let sig = signature(k, &generic_parts("1700000000", "d-1", b"{}"));
        assert!(verify(k, &generic_parts("1700000000", "d-1", b"{}"), &sig));
        assert!(!verify(k, &generic_parts("1700000001", "d-1", b"{}"), &sig));
        assert!(!verify(k, &generic_parts("1700000000", "d-2", b"{}"), &sig));
        assert!(!verify(
            k,
            &generic_parts("1700000000", "d-1", b"{ }"),
            &sig
        ));
        assert!(valid_delivery("72d3162e-cc78-11e3-81ab-4c9367dc0958"));
        assert!(!valid_delivery("a b") && !valid_delivery("") && !valid_delivery(&"x".repeat(101)));
    }

    #[test]
    fn payload_formats() {
        let j = br#"{"payload":{"a":1}}"#;
        assert!(generic_text(j).contains("\"a\": 1"));
        assert_eq!(generic_text(b"plain"), "plain");
        assert!(generic_text(&[0xff, 0xfe]).contains("2 Bytes"));
        let form = b"payload=%7B%22zen%22%3A%22x%22%7D";
        assert_eq!(
            github_payload("application/x-www-form-urlencoded", form).unwrap()["zen"],
            "x"
        );
        assert_eq!(
            github_payload("application/json", br#"{"zen":"y"}"#).unwrap()["zen"],
            "y"
        );
        assert!(github_payload("application/json", b"nope").is_none());
    }

    fn filter(events: &[GithubEvent]) -> GithubFilter {
        GithubFilter {
            events: events.to_vec(),
            ..Default::default()
        }
    }

    fn push(branch: &str, committer: &str) -> Value {
        json!({
            "ref": format!("refs/heads/{branch}"), "before": "a".repeat(40), "after": "b".repeat(40),
            "deleted": false, "compare": "https://github.com/acme/app/compare/x",
            "repository": {"full_name": "acme/app"}, "pusher": {"name": "dev"},
            "sender": {"login": "dev", "type": "User"},
            "head_commit": {"id": "b".repeat(40), "message": "fix", "committer": {"email": committer}},
            "commits": [{"id": "b".repeat(40), "message": "fix: x\n\nbody", "author": {"name": "Dev"}}]
        })
    }

    fn ignored(v: Verdict) -> String {
        match v {
            Verdict::Ignore(r) => r,
            other => panic!("expected ignore, got {other:?}"),
        }
    }

    #[test]
    fn push_filters_and_loop_guard() {
        let mut f = filter(&[GithubEvent::Push]);
        f.branches = vec!["main".into(), "release/*".into()];
        let ok = github_verdict("push", &push("main", "dev@x"), &f, Some("ACME/app"));
        let Verdict::Match(m) = ok else {
            panic!("{ok:?}")
        };
        assert_eq!(m.title_suffix, "GitHub-Push");
        assert!(m.entries[1].1.contains("fix: x (Dev)"), "{:?}", m.entries);
        assert!(!m.entries[1].1.contains("body"), "first line only");
        assert!(matches!(
            github_verdict("push", &push("release/2", "d@x"), &f, None),
            Verdict::Match(_)
        ));
        assert!(ignored(github_verdict("push", &push("dev", "d@x"), &f, None)).contains("Filter"));
        assert!(ignored(github_verdict(
            "push",
            &push("main", QUEUE_COMMITTER),
            &f,
            None
        ))
        .contains("Queue"));
        assert!(ignored(github_verdict(
            "push",
            &push("main", "d@x"),
            &f,
            Some("acme/other")
        ))
        .contains("Anderes Repository"));
        let mut del = push("main", "d@x");
        del["deleted"] = true.into();
        assert!(ignored(github_verdict("push", &del, &f, None)).contains("gelöscht"));
        let mut tag = push("main", "d@x");
        tag["ref"] = "refs/tags/v1".into();
        assert!(ignored(github_verdict("push", &tag, &f, None)).contains("Tag"));
        let mut bot = push("main", "d@x");
        bot["sender"] = json!({"login": "dependabot[bot]", "type": "Bot"});
        assert!(ignored(github_verdict("push", &bot, &f, None)).contains("Bot"));
        f.include_bots = true;
        assert!(matches!(
            github_verdict("push", &bot, &f, None),
            Verdict::Match(_)
        ));
        f.branches.clear();
        assert!(
            ignored(github_verdict("push", &push("queue/abc", "d@x"), &f, None)).contains("Queue")
        );
        assert!(ignored(github_verdict(
            "push",
            &push("main", "d@x"),
            &filter(&[GithubEvent::Issues]),
            None
        ))
        .contains("nicht ausgewählt"));
        assert_eq!(
            github_verdict("ping", &json!({"zen": "z"}), &f, Some("x/y")),
            Verdict::Ping
        );
    }

    #[test]
    fn issues_and_pull_requests_with_labels_and_drafts() {
        let mut f = filter(&[GithubEvent::Issues, GithubEvent::PullRequest]);
        let issue = |action: &str, labels: &[&str], added: Option<&str>| {
            let mut v = json!({
                "action": action, "repository": {"full_name": "acme/app"}, "sender": {"login": "u", "type": "User"},
                "issue": {"number": 42, "title": "Bug", "body": "steps", "html_url": "https://github.com/acme/app/issues/42",
                          "user": {"login": "u"}, "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>()}
            });
            if let Some(a) = added {
                v["label"] = json!({"name": a});
            }
            v
        };
        let Verdict::Match(m) = github_verdict("issues", &issue("opened", &[], None), &f, None)
        else {
            panic!()
        };
        assert_eq!(m.title_suffix, "GitHub-Issue #42");
        assert_eq!(m.dedupe_key.as_deref(), Some("issue:42"));
        assert!(ignored(github_verdict(
            "issues",
            &issue("closed", &[], None),
            &f,
            None
        ))
        .contains("closed"));
        assert!(ignored(github_verdict(
            "issues",
            &issue("labeled", &["x"], Some("x")),
            &f,
            None
        ))
        .contains("Label-Filter"));
        f.labels = vec!["Queue".into()];
        assert!(ignored(github_verdict(
            "issues",
            &issue("opened", &["bug"], None),
            &f,
            None
        ))
        .contains("Labels"));
        assert!(matches!(
            github_verdict("issues", &issue("opened", &["queue"], None), &f, None),
            Verdict::Match(_)
        ));
        assert!(matches!(
            github_verdict(
                "issues",
                &issue("labeled", &["queue"], Some("queue")),
                &f,
                None
            ),
            Verdict::Match(_)
        ));
        assert!(ignored(github_verdict(
            "issues",
            &issue("labeled", &["queue", "x"], Some("x")),
            &f,
            None
        ))
        .contains("Hinzugefügtes"));
        let Verdict::Match(m) =
            github_verdict("issues", &issue("reopened", &["queue"], None), &f, None)
        else {
            panic!()
        };
        assert_eq!(m.dedupe_key, None, "reopening may start again");
        f.labels.clear();
        let pr = |action: &str, draft: bool, head: &str, base: &str| {
            json!({
                "action": action, "number": 7, "repository": {"full_name": "acme/app"}, "sender": {"login": "u", "type": "User"},
                "pull_request": {"number": 7, "title": "Feature", "body": "b", "draft": draft, "html_url": "u",
                                 "user": {"login": "u"}, "labels": [],
                                 "head": {"ref": head, "sha": "c".repeat(40)}, "base": {"ref": base}}
            })
        };
        let Verdict::Match(m) = github_verdict(
            "pull_request",
            &pr("synchronize", false, "feat", "main"),
            &f,
            None,
        ) else {
            panic!()
        };
        assert_eq!(m.dedupe_key, Some(format!("pr:7:{}", "c".repeat(40))));
        assert_eq!(m.title_suffix, "GitHub-PR #7");
        assert!(ignored(github_verdict(
            "pull_request",
            &pr("opened", true, "feat", "main"),
            &f,
            None
        ))
        .contains("Entwurf"));
        assert!(matches!(
            github_verdict(
                "pull_request",
                &pr("ready_for_review", false, "feat", "main"),
                &f,
                None
            ),
            Verdict::Match(_)
        ));
        assert!(ignored(github_verdict(
            "pull_request",
            &pr("opened", false, "queue/abc", "main"),
            &f,
            None
        ))
        .contains("Auto-Fix"));
        f.branches = vec!["main".into()];
        assert!(ignored(github_verdict(
            "pull_request",
            &pr("opened", false, "feat", "dev"),
            &f,
            None
        ))
        .contains("Ziel-Branch"));
        assert!(ignored(github_verdict(
            "pull_request",
            &pr("closed", false, "feat", "main"),
            &f,
            None
        ))
        .contains("closed"));
    }

    #[test]
    fn check_failures_once_per_commit_and_not_on_queue_branches() {
        let mut f = filter(&[GithubEvent::CheckFailure]);
        let run = |conclusion: &str, branch: Option<&str>| {
            json!({
                "action": "completed", "repository": {"full_name": "acme/app"},
                "sender": {"login": "github-actions[bot]", "type": "Bot"},
                "check_run": {"name": "test", "conclusion": conclusion, "head_sha": "d".repeat(40), "html_url": "u",
                              "check_suite": {"head_branch": branch}, "pull_requests": [{"number": 3}],
                              "output": {"title": "1 failed", "summary": "assert x"}}
            })
        };
        let Verdict::Match(m) =
            github_verdict("check_run", &run("failure", Some("main")), &f, None)
        else {
            panic!()
        };
        assert_eq!(m.dedupe_key, Some(format!("check:{}", "d".repeat(40))));
        assert!(m.entries[0].1.contains("#3") && m.entries[0].1.contains("assert x"));
        assert!(
            matches!(
                github_verdict("check_run", &run("timed_out", None), &f, None),
                Verdict::Match(_)
            ),
            "bots do not matter for CI"
        );
        assert!(ignored(github_verdict(
            "check_run",
            &run("success", Some("main")),
            &f,
            None
        ))
        .contains("success"));
        assert!(ignored(github_verdict(
            "check_run",
            &run("failure", Some("queue/abc")),
            &f,
            None
        ))
        .contains("Auto-Fix"));
        f.branches = vec!["main".into()];
        assert!(
            ignored(github_verdict("check_run", &run("failure", None), &f, None)).contains("Fork")
        );
        assert!(ignored(github_verdict(
            "check_suite",
            &run("failure", None),
            &f,
            None
        ))
        .contains("check_suite"));
    }

    #[test]
    fn pr_refs_for_poking() {
        let (n, b) = pr_refs(
            "check_run",
            &json!({"check_run": {"pull_requests": [{"number": 5}], "check_suite": {"head_branch": "queue/abcdef123456"}}}),
        );
        assert_eq!((n, b), (vec![5], vec!["queue/abcdef123456".to_string()]));
        let (n, b) = pr_refs(
            "issue_comment",
            &json!({"issue": {"number": 9, "pull_request": {}}}),
        );
        assert_eq!((n, b.len()), (vec![9], 0));
        let (n, _) = pr_refs("issue_comment", &json!({"issue": {"number": 9}}));
        assert!(n.is_empty(), "comment on an issue, not a pull request");
        let (_, b) = pr_refs("push", &json!({"ref": "refs/heads/main"}));
        assert!(b.is_empty(), "only Queue's branches are tracked");
        assert_eq!(
            event_label("issues", &json!({"action": "opened"})),
            "issues.opened"
        );
        assert_eq!(event_label("push", &json!({})), "push");
        assert_eq!(event_label("x", &json!({"action": "a b"})), "x");
    }
}
