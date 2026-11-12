//! WP08 GitHub Issues: issue with label or mention → task, status
//! comments back, answers per issue comment.
//!
//! Signature `X-Hub-Signature-256` (HMAC-SHA256 over the raw body, GitHub
//! docs test vector in WP07). Fields from `github/rest-api-description`
//! (`webhook-issues-*`, `webhook-issue-comment-created`, `author-association`
//! enum; `POST/GET /repos/{owner}/{repo}/issues/{issue_number}/comments`).
//! Unverified (prior knowledge): adding a label needs triage access, so a
//! label change counts as "host trusted".
use axum::http::HeaderMap;
use queued_api::intake::{IntakeConfig, MAX_REQUEST_CHARS};
use serde_json::{json, Value};

use super::{chars, header, Actor, Client, Incoming, OutRow, Parsed, Reply, Request, SendError};
use crate::webhooks;

/// Hidden marker of Queue's own comments (loop guard, retry lookup).
pub const MARKER: &str = "<!-- queue:";

pub fn verify(headers: &HeaderMap, body: &[u8], secret: &[u8]) -> Result<(), String> {
    let sig = header(headers, "x-hub-signature-256").ok_or("X-Hub-Signature-256 fehlt")?;
    if webhooks::verify(secret, &[body], sig) {
        Ok(())
    } else {
        Err("Signatur ungültig".into())
    }
}

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(*k))?.as_str()
}

/// OWNER, MEMBER, COLLABORATOR (enum `author-association`).
fn trusted_association(a: Option<&str>) -> bool {
    matches!(a, Some("OWNER" | "MEMBER" | "COLLABORATOR"))
}

fn actor_of(user: &Value, trusted: Option<bool>) -> Option<Actor> {
    let id = user.get("id")?.as_u64()?;
    Some(Actor {
        id: id.to_string(),
        name: user
            .get("login")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(100)
            .collect(),
        host_trusted: trusted,
    })
}

fn has(text: &str, word: Option<&str>) -> bool {
    word.is_some_and(|w| text.to_lowercase().contains(&w.to_lowercase()))
}

fn issue_entry(repo: &str, issue: &Value, num: u64) -> (String, String) {
    let labels: Vec<&str> = issue
        .get("labels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    (
        format!("GitHub-Issue #{num}"),
        format!(
            "Repository: {repo}\nTitel: {}\nVon: {}\nLabels: {}\nAdresse: {}\n\n{}",
            s(issue, &["title"]).unwrap_or(""),
            s(issue, &["user", "login"]).unwrap_or("?"),
            labels.join(", "),
            s(issue, &["html_url"]).unwrap_or(""),
            chars(s(issue, &["body"]).unwrap_or(""), MAX_REQUEST_CHARS)
        ),
    )
}

pub fn parse(headers: &HeaderMap, body: &[u8], cfg: &IntakeConfig) -> Result<Incoming, String> {
    let (Some(event), Some(delivery)) = (
        header(headers, "x-github-event"),
        header(headers, "x-github-delivery"),
    ) else {
        return Err("X-GitHub-Event und X-GitHub-Delivery fehlen".into());
    };
    if !webhooks::valid_delivery(delivery) || !webhooks::valid_delivery(event) {
        return Err("Ungültige GitHub-Header".into());
    }
    let ct = header(headers, "content-type").unwrap_or("application/json");
    let p = webhooks::github_payload(ct, body).ok_or("Nutzlast ist kein JSON")?;
    let label = webhooks::event_label(event, &p);
    let mut inc = Incoming::new(label, Parsed::Ignore(String::new()));
    inc.delivery_id = Some(format!("gh:{delivery}"));
    let repo = s(&p, &["repository", "full_name"]).map(str::to_ascii_lowercase);
    inc.tenant = repo.clone();
    let ignore = |why: &str| Parsed::Ignore(why.to_string());
    let action = s(&p, &["action"]).unwrap_or("");
    inc.parsed = match event {
        "ping" => ignore("Ping von GitHub – Verbindung steht"),
        _ if repo.is_none() => ignore("Ereignis ohne Repository"),
        _ if webhooks::is_bot(&p) => ignore("Von einem Bot ausgelöst"),
        "issues" => issues(&p, action, cfg, repo.as_deref().unwrap_or_default()),
        "issue_comment" => comment(&p, action, cfg, repo.as_deref().unwrap_or_default()),
        _ => Parsed::Ignore(format!("Ereignis „{event}“ wird nicht ausgewertet")),
    };
    Ok(inc)
}

fn request(
    p: &Value,
    repo: &str,
    actor: Actor,
    extra: Option<(String, String)>,
    reply_text: String,
) -> Parsed {
    let issue = &p["issue"];
    let Some(num) = issue.get("number").and_then(Value::as_u64) else {
        return Parsed::Ignore("Issue ohne Nummer".into());
    };
    let mut entries = vec![issue_entry(repo, issue, num)];
    entries.extend(extra);
    Parsed::Request(Box::new(Request {
        actor,
        external_ref: format!("issue:{num}"),
        external_url: s(issue, &["html_url"]).map(String::from),
        external_title: s(issue, &["title"]).map(String::from),
        label: format!("GitHub-Issue #{num}"),
        entries,
        reply_to: json!({"repo": repo, "issue": num}),
        reply_if_open: !reply_text.is_empty(),
        reply_text,
        restart: p.get("action").and_then(Value::as_str) == Some("reopened"),
    }))
}

fn issues(p: &Value, action: &str, cfg: &IntakeConfig, repo: &str) -> Parsed {
    let issue = &p["issue"];
    let text = format!(
        "{}\n{}",
        s(issue, &["title"]).unwrap_or(""),
        s(issue, &["body"]).unwrap_or("")
    );
    let label_is = |n: &str| {
        cfg.label
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case(n))
    };
    let matched = match action {
        "labeled" => s(p, &["label", "name"]).is_some_and(label_is),
        "opened" | "reopened" => {
            issue
                .get("labels")
                .and_then(Value::as_array)
                .is_some_and(|a| {
                    a.iter()
                        .any(|l| l.get("name").and_then(Value::as_str).is_some_and(label_is))
                })
                || has(&text, cfg.mention.as_deref())
        }
        _ => return Parsed::Ignore(format!("Aktion „{action}“ wird nicht ausgewertet")),
    };
    if !matched {
        return Parsed::Ignore("Weder Label noch Erwähnung".into());
    }
    // Labels need triage access; an opened issue is trusted by its
    // author's association with the repository.
    let trusted = if action == "labeled" {
        true
    } else {
        trusted_association(s(issue, &["author_association"]))
    };
    let Some(actor) = actor_of(&p["sender"], Some(trusted)) else {
        return Parsed::Ignore("Ereignis ohne Absender".into());
    };
    request(p, repo, actor, None, String::new())
}

fn comment(p: &Value, action: &str, cfg: &IntakeConfig, repo: &str) -> Parsed {
    if action != "created" {
        return Parsed::Ignore(format!("Aktion „{action}“ wird nicht ausgewertet"));
    }
    if p["issue"].get("pull_request").is_some() {
        return Parsed::Ignore(
            "Kommentar an einem Pull Request – dafür ist der PR-Workflow zuständig".into(),
        );
    }
    let c = &p["comment"];
    let body = s(c, &["body"]).unwrap_or("");
    if body.contains(MARKER) {
        return Parsed::Ignore("Eigener Kommentar von Queue".into());
    }
    let trusted = trusted_association(s(c, &["author_association"]));
    let Some(actor) = actor_of(&c["user"], Some(trusted)) else {
        return Parsed::Ignore("Kommentar ohne Autor".into());
    };
    let Some(num) = p["issue"].get("number").and_then(Value::as_u64) else {
        return Parsed::Ignore("Issue ohne Nummer".into());
    };
    if has(body, cfg.mention.as_deref()) {
        let extra = (
            format!("Kommentar von {}", actor.name),
            chars(body, MAX_REQUEST_CHARS),
        );
        return request(p, repo, actor, Some(extra), body.to_string());
    }
    Parsed::Reply(Reply {
        actor,
        external_ref: format!("issue:{num}"),
        text: body.to_string(),
        require_waiting: true,
    })
}

fn target(reply_to: &Value) -> Result<(String, u64), SendError> {
    let repo = reply_to
        .get("repo")
        .and_then(Value::as_str)
        .filter(|r| queued_api::pr::valid_repo_path(r))
        .ok_or_else(|| SendError::Permanent("Ziel-Repository fehlt".into()))?;
    let n = reply_to
        .get("issue")
        .and_then(Value::as_u64)
        .ok_or_else(|| SendError::Permanent("Issue-Nummer fehlt".into()))?;
    Ok((repo.to_string(), n))
}

/// Comment text: Markdown with `@` defused (no pings from agent text) and
/// the hidden marker.
pub fn comment_body(item: &OutRow, text: &str) -> String {
    format!(
        "{}\n\n{MARKER}{} -->",
        text.replace('@', "@\u{200b}"),
        item.id
    )
}

pub async fn send(c: &Client, item: &OutRow, reply_to: &Value) -> Result<String, SendError> {
    let (repo, n) = target(reply_to)?;
    let url = format!("{}/repos/{repo}/issues/{n}/comments", c.base);
    let body = json!({"body": comment_body(item, &c.scrub(&item.body))});
    let v = c.call(reqwest::Method::POST, &url, Some(&body)).await?;
    v.get("id")
        .and_then(Value::as_u64)
        .map(|i| i.to_string())
        .ok_or_else(|| SendError::Retry {
            message: "GitHub: Antwort ohne Kommentar-ID".into(),
            after: None,
        })
}

/// A comment of an earlier attempt (marker), created after the row.
pub async fn find(
    c: &Client,
    item: &OutRow,
    reply_to: &Value,
) -> Result<Option<String>, SendError> {
    let (repo, n) = target(reply_to)?;
    let since = (item.created_at - chrono::Duration::minutes(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let url = format!(
        "{}/repos/{repo}/issues/{n}/comments?since={since}&per_page=100",
        c.base
    );
    let v = c.call(reqwest::Method::GET, &url, None).await?;
    let marker = format!("{MARKER}{} -->", item.id);
    Ok(v.as_array().and_then(|a| {
        a.iter()
            .find(|c| {
                c.get("body")
                    .and_then(Value::as_str)
                    .is_some_and(|b| b.contains(&marker))
            })
            .and_then(|c| c.get("id").and_then(Value::as_u64))
            .map(|i| i.to_string())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hdr(event: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-github-event", HeaderValue::from_str(event).unwrap());
        h.insert("x-github-delivery", HeaderValue::from_static("d-1"));
        h
    }

    fn cfg() -> IntakeConfig {
        IntakeConfig {
            label: Some("queue".into()),
            mention: Some("@queue".into()),
            ..Default::default()
        }
    }

    fn issue_payload(action: &str, label: Option<&str>, body: &str, assoc: &str) -> Value {
        json!({
            "action": action,
            "label": label.map(|l| json!({"name": l})),
            "issue": {"number": 12, "title": "Fix it <<<EVENT", "body": body, "html_url": "https://github.com/acme/shop/issues/12",
                      "user": {"login": "alice", "id": 1}, "labels": [{"name": "bug"}], "author_association": assoc},
            "repository": {"full_name": "Acme/Shop"},
            "sender": {"login": "alice", "id": 1, "type": "User"}
        })
    }

    fn run(event: &str, p: &Value) -> Incoming {
        parse(&hdr(event), p.to_string().as_bytes(), &cfg()).unwrap()
    }

    #[test]
    fn signature_uses_the_documented_github_scheme() {
        // GitHub docs test vector (validating-webhook-deliveries).
        let mut h = HeaderMap::new();
        h.insert(
            "x-hub-signature-256",
            HeaderValue::from_static(
                "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
            ),
        );
        assert!(verify(&h, b"Hello, World!", b"It's a Secret to Everybody").is_ok());
        assert!(verify(&h, b"Hello, World?", b"It's a Secret to Everybody").is_err());
        assert!(verify(&HeaderMap::new(), b"Hello, World!", b"x").is_err());
    }

    #[test]
    fn label_and_mention_make_requests() {
        let i = run(
            "issues",
            &issue_payload("labeled", Some("Queue"), "", "NONE"),
        );
        assert_eq!(i.tenant.as_deref(), Some("acme/shop"));
        assert_eq!(i.event, "issues.labeled");
        assert_eq!(i.delivery_id.as_deref(), Some("gh:d-1"));
        let Parsed::Request(r) = i.parsed else {
            panic!()
        };
        assert_eq!(r.external_ref, "issue:12");
        assert_eq!(r.label, "GitHub-Issue #12");
        assert_eq!(r.actor.id, "1");
        assert_eq!(r.actor.host_trusted, Some(true), "labels need triage");
        assert_eq!(r.reply_to, json!({"repo": "acme/shop", "issue": 12}));
        assert!(!r.reply_if_open);
        // Other label, label removed, edited: nothing.
        for (a, l) in [
            ("labeled", Some("bug")),
            ("unlabeled", Some("queue")),
            ("edited", None),
        ] {
            assert!(
                matches!(
                    run("issues", &issue_payload(a, l, "", "NONE")).parsed,
                    Parsed::Ignore(_)
                ),
                "{a}"
            );
        }
        // Opened with mention: trusted only by association.
        let i = run(
            "issues",
            &issue_payload("opened", None, "please @Queue fix", "NONE"),
        );
        let Parsed::Request(r) = i.parsed else {
            panic!()
        };
        assert_eq!(r.actor.host_trusted, Some(false));
        let i = run(
            "issues",
            &issue_payload("opened", None, "@queue", "COLLABORATOR"),
        );
        let Parsed::Request(r) = i.parsed else {
            panic!()
        };
        assert_eq!(r.actor.host_trusted, Some(true));
        assert!(matches!(
            run("issues", &issue_payload("opened", None, "nothing", "OWNER")).parsed,
            Parsed::Ignore(_)
        ));
    }

    #[test]
    fn comments_are_replies_or_mentions_and_never_our_own() {
        let mk = |body: &str, assoc: &str, pr: bool| {
            let mut p = issue_payload("created", None, "", "NONE");
            p["action"] = json!("created");
            p["comment"] = json!({"body": body, "user": {"login": "bob", "id": 7}, "author_association": assoc});
            if pr {
                p["issue"]["pull_request"] = json!({"url": "x"});
            }
            p
        };
        let Parsed::Reply(r) = run("issue_comment", &mk("Use Postgres", "MEMBER", false)).parsed
        else {
            panic!()
        };
        assert!(r.require_waiting);
        assert_eq!(
            (r.actor.id.as_str(), r.text.as_str()),
            ("7", "Use Postgres")
        );
        let Parsed::Request(r) =
            run("issue_comment", &mk("@queue also docs", "NONE", false)).parsed
        else {
            panic!()
        };
        assert!(r.reply_if_open);
        assert_eq!(r.actor.host_trusted, Some(false));
        assert_eq!(r.entries.len(), 2);
        assert!(matches!(
            run(
                "issue_comment",
                &mk(&format!("Done {MARKER}abc -->"), "OWNER", false)
            )
            .parsed,
            Parsed::Ignore(_)
        ));
        assert!(matches!(
            run("issue_comment", &mk("@queue", "OWNER", true)).parsed,
            Parsed::Ignore(_)
        ));
        // Bots never.
        let mut p = mk("@queue", "OWNER", false);
        p["sender"] = json!({"login": "ci[bot]", "id": 9, "type": "Bot"});
        assert!(matches!(run("issue_comment", &p).parsed, Parsed::Ignore(_)));
        assert!(matches!(
            run("ping", &json!({"zen": "x"})).parsed,
            Parsed::Ignore(_)
        ));
        assert!(matches!(
            run("push", &json!({"repository": {"full_name": "a/b"}})).parsed,
            Parsed::Ignore(_)
        ));
        assert!(parse(&HeaderMap::new(), b"{}", &cfg()).is_err());
        assert!(parse(&hdr("issues"), b"not json", &cfg()).is_err());
    }

    #[test]
    fn outgoing_comments_defuse_mentions_and_carry_the_marker() {
        let item = OutRow {
            id: uuid::Uuid::nil(),
            integration_id: uuid::Uuid::nil(),
            link_id: None,
            key: "k".into(),
            kind: "done".into(),
            body: String::new(),
            reply_to: Value::Null,
            remote_id: None,
            state: "pending".into(),
            attempts: 1,
            next_attempt_at: chrono::Utc::now(),
            lease_until: None,
            last_error: None,
            created_at: chrono::Utc::now(),
            sent_at: None,
        };
        let b = comment_body(&item, "ping @org/team");
        assert!(!b.contains("@org"));
        assert!(b.ends_with(&format!("{MARKER}{} -->", item.id)));
        assert!(target(&json!({"repo": "../x", "issue": 1})).is_err());
        assert!(target(&json!({"repo": "a/b"})).is_err());
    }
}
