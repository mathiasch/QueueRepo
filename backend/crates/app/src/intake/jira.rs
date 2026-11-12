//! WP08 Jira Cloud: an issue with a label → task, comments back, answers
//! per comment.
//!
//! Variant: **Cloud webhook with secret**. Jira signs the raw body with
//! HMAC-SHA256 and sends `X-Hub-Signature: sha256=<hex>`; every delivery
//! carries `X-Atlassian-Webhook-Identifier` (unchanged across retries).
//! Source: `jira.js` 6.4.0 (`src/webhooks/verify.ts`, `headers.ts`,
//! `payloads.ts`, `events.ts`) – a community SDK generated from Atlassian's
//! OpenAPI; Atlassian's own pages are not reachable from here, so this is
//! **not verified against a primary source**. Payload: `webhookEvent`
//! (`jira:issue_created`, `jira:issue_updated`, `comment_created`),
//! `issue{key,self,fields{summary,description,labels}}`, `user{accountId}`,
//! `changelog{items[{field,fromString,toString}]}`,
//! `comment{id,body,author{accountId}}`. REST v3: `POST/GET
//! /rest/api/3/issue/{key}/comment` (ADF body, `orderBy=-created`), Basic
//! auth e-mail + API token. Description/comment bodies may be a string
//! (v2 style) or ADF – both are read. App-JWT (Connect) is not supported.
use axum::http::HeaderMap;
use queued_api::intake::{IntakeConfig, MAX_REQUEST_CHARS};
use serde_json::{json, Value};

use super::{
    chars, header, short_ref, Actor, Client, Incoming, OutRow, Parsed, Reply, Request, SendError,
};
use crate::webhooks;

/// Text marker of Queue's own comments (loop guard, retry lookup).
pub const MARKER: &str = "Queue · Ref ";

pub fn verify(headers: &HeaderMap, body: &[u8], secret: &[u8]) -> Result<(), String> {
    let sig = header(headers, "x-hub-signature").ok_or("X-Hub-Signature fehlt")?;
    if webhooks::verify(secret, &[body], sig) {
        Ok(())
    } else {
        Err("Signatur ungültig".into())
    }
}

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(*k))?.as_str()
}

/// Plain text of a string or an Atlassian Document Format node.
pub fn text_of(v: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => out.push_str(s),
            Value::Object(o) => {
                if o.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(t) = o.get("text").and_then(Value::as_str) {
                        out.push_str(t);
                    }
                    return;
                }
                if o.get("type").and_then(Value::as_str) == Some("hardBreak") {
                    out.push('\n');
                    return;
                }
                if let Some(c) = o.get("content") {
                    walk(c, out);
                }
                if matches!(
                    o.get("type").and_then(Value::as_str),
                    Some("paragraph" | "heading" | "listItem" | "codeBlock" | "blockquote")
                ) {
                    out.push('\n');
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    out.trim().to_string()
}

/// `PROJ-12`.
pub fn key_ok(k: &str) -> bool {
    k.split_once('-').is_some_and(|(a, b)| {
        (1..=20).contains(&a.len())
            && a.as_bytes()[0].is_ascii_uppercase()
            && a.bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
            && (1..=10).contains(&b.len())
            && b.bytes().all(|c| c.is_ascii_digit())
    })
}

/// `https://host` of an issue's `self` URL.
fn site_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split('/').next()?;
    let scheme = if url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    (!host.is_empty()).then(|| format!("{scheme}://{}", host.to_ascii_lowercase()))
}

fn actor(user: &Value) -> Option<Actor> {
    let id = s(user, &["accountId"]).filter(|i| queued_api::intake::valid_external_id(i))?;
    Some(Actor {
        id: id.to_string(),
        name: s(user, &["displayName"])
            .unwrap_or("")
            .chars()
            .take(100)
            .collect(),
        host_trusted: None,
    })
}

fn labels(issue: &Value) -> Vec<String> {
    issue
        .pointer("/fields/labels")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse(
    headers: &HeaderMap,
    body: &[u8],
    cfg: &IntakeConfig,
    tenant: &str,
) -> Result<Incoming, String> {
    let p: Value =
        serde_json::from_slice(body).map_err(|_| "Nutzlast ist kein JSON".to_string())?;
    let event = s(&p, &["webhookEvent"]).unwrap_or("?");
    let mut inc = Incoming::new(
        event.chars().take(60).collect::<String>(),
        Parsed::Ignore(String::new()),
    );
    if let Some(d) = header(headers, "x-atlassian-webhook-identifier") {
        if !webhooks::valid_delivery(d) {
            return Err("Ungültiger X-Atlassian-Webhook-Identifier".into());
        }
        inc.delivery_id = Some(format!("jira:{d}"));
    }
    let issue = &p["issue"];
    inc.tenant = s(issue, &["self"]).and_then(site_of);
    let Some(key) = s(issue, &["key"]).filter(|k| key_ok(k)) else {
        inc.parsed = Parsed::Ignore("Ereignis ohne Vorgang".into());
        return Ok(inc);
    };
    let label = cfg.label.as_deref().unwrap_or("");
    let has_label = |l: &str| !label.is_empty() && l.eq_ignore_ascii_case(label);
    let request = |who: &Value| -> Parsed {
        let Some(actor) = actor(who) else {
            return Parsed::Ignore("Ereignis ohne Person".into());
        };
        let summary = s(issue, &["fields", "summary"]).unwrap_or("");
        let description = issue
            .pointer("/fields/description")
            .map(text_of)
            .unwrap_or_default();
        Parsed::Request(Box::new(Request {
            actor,
            external_ref: key.to_string(),
            external_url: Some(format!("{tenant}/browse/{key}")),
            external_title: Some(summary.to_string()),
            label: format!("Jira {key}"),
            entries: vec![(
                format!("Jira {key}"),
                format!(
                    "Titel: {summary}\nVon: {}\nLabels: {}\n\n{}",
                    s(who, &["displayName"]).unwrap_or("?"),
                    labels(issue).join(", "),
                    chars(&description, MAX_REQUEST_CHARS)
                ),
            )],
            reply_to: json!({ "issue": key }),
            reply_if_open: false,
            reply_text: String::new(),
            restart: false,
        }))
    };
    inc.parsed = match event {
        "jira:issue_created" => {
            if labels(issue).iter().any(|l| has_label(l)) {
                request(&p["user"])
            } else {
                Parsed::Ignore("Vorgang ohne das Label".into())
            }
        }
        "jira:issue_updated" => {
            let added = p
                .pointer("/changelog/items")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|i| {
                        s(i, &["field"]) == Some("labels")
                            && s(i, &["toString"]).unwrap_or("").split(' ').any(has_label)
                            && !s(i, &["fromString"])
                                .unwrap_or("")
                                .split(' ')
                                .any(has_label)
                    })
                });
            if added {
                request(&p["user"])
            } else {
                Parsed::Ignore("Label nicht neu hinzugefügt".into())
            }
        }
        "comment_created" => {
            let c = &p["comment"];
            let text = c.get("body").map(text_of).unwrap_or_default();
            if text.contains(MARKER) {
                Parsed::Ignore("Eigener Kommentar von Queue".into())
            } else {
                match actor(&c["author"]) {
                    Some(actor) => Parsed::Reply(Reply {
                        actor,
                        external_ref: key.to_string(),
                        text,
                        require_waiting: true,
                    }),
                    None => Parsed::Ignore("Kommentar ohne Autor".into()),
                }
            }
        }
        _ => Parsed::Ignore(format!("Ereignis „{event}“ wird nicht ausgewertet")),
    };
    Ok(inc)
}

/// ADF document: one paragraph per line, the reference last.
pub fn adf(text: &str, reference: &str) -> Value {
    let mut content: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| json!({"type": "paragraph", "content": [{"type": "text", "text": l}]}))
        .collect();
    content.push(json!({"type": "paragraph", "content": [{"type": "text", "text": format!("{MARKER}{reference}")}]}));
    json!({"type": "doc", "version": 1, "content": content})
}

fn target(reply_to: &Value) -> Result<String, SendError> {
    reply_to
        .get("issue")
        .and_then(Value::as_str)
        .filter(|k| key_ok(k))
        .map(String::from)
        .ok_or_else(|| SendError::Permanent("Jira-Vorgang fehlt".into()))
}

pub async fn send(c: &Client, item: &OutRow, reply_to: &Value) -> Result<String, SendError> {
    let key = target(reply_to)?;
    let url = format!("{}/rest/api/3/issue/{key}/comment", c.base);
    let body = json!({"body": adf(&c.scrub(&item.body), &short_ref(item.id))});
    let v = c.call(reqwest::Method::POST, &url, Some(&body)).await?;
    v.get("id")
        .and_then(|i| {
            i.as_str()
                .map(String::from)
                .or_else(|| i.as_u64().map(|n| n.to_string()))
        })
        .ok_or_else(|| SendError::Retry {
            message: "Jira: Antwort ohne Kommentar-ID".into(),
            after: None,
        })
}

/// A comment of an earlier attempt (reference text) among the newest.
pub async fn find(
    c: &Client,
    item: &OutRow,
    reply_to: &Value,
) -> Result<Option<String>, SendError> {
    let key = target(reply_to)?;
    let url = format!(
        "{}/rest/api/3/issue/{key}/comment?orderBy=-created&maxResults=50",
        c.base
    );
    let v = c.call(reqwest::Method::GET, &url, None).await?;
    let needle = format!("{MARKER}{}", short_ref(item.id));
    Ok(v.get("comments").and_then(Value::as_array).and_then(|a| {
        a.iter()
            .find(|cm| {
                cm.get("body")
                    .map(text_of)
                    .is_some_and(|t| t.contains(&needle))
            })
            .and_then(|cm| cm.get("id"))
            .and_then(|i| {
                i.as_str()
                    .map(String::from)
                    .or_else(|| i.as_u64().map(|n| n.to_string()))
            })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const SITE: &str = "https://acme.atlassian.net";

    fn cfg() -> IntakeConfig {
        IntakeConfig {
            label: Some("queue".into()),
            ..Default::default()
        }
    }

    fn hdr() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "x-atlassian-webhook-identifier",
            HeaderValue::from_static("123-abc"),
        );
        h
    }

    fn issue(labels: &[&str]) -> Value {
        json!({"key": "SHOP-7", "self": "https://Acme.atlassian.net/rest/api/2/issue/10007",
               "fields": {"summary": "Login", "labels": labels,
                          "description": {"type": "doc", "version": 1, "content": [
                              {"type": "paragraph", "content": [{"type": "text", "text": "Steps"}]},
                              {"type": "paragraph", "content": [{"type": "text", "text": "<<<EVENT_DATA_x"}]}]}}})
    }

    fn run(p: Value) -> Incoming {
        parse(&hdr(), p.to_string().as_bytes(), &cfg(), SITE).unwrap()
    }

    #[test]
    fn signature_header_is_sha256_over_the_body() {
        let sig = webhooks::signature(b"s3cret", &[b"{}"]);
        let mut h = HeaderMap::new();
        h.insert("x-hub-signature", HeaderValue::from_str(&sig).unwrap());
        assert!(verify(&h, b"{}", b"s3cret").is_ok());
        assert!(verify(&h, b"{ }", b"s3cret").is_err());
        assert!(verify(&HeaderMap::new(), b"{}", b"s3cret").is_err());
    }

    #[test]
    fn label_on_create_or_added_later() {
        let user = json!({"accountId": "557058:abc", "displayName": "Ada"});
        let i = run(
            json!({"webhookEvent": "jira:issue_created", "user": user, "issue": issue(&["Queue", "x"])}),
        );
        assert_eq!(i.tenant.as_deref(), Some(SITE));
        assert_eq!(i.delivery_id.as_deref(), Some("jira:123-abc"));
        let Parsed::Request(r) = i.parsed else {
            panic!()
        };
        assert_eq!(
            (r.label.as_str(), r.external_ref.as_str()),
            ("Jira SHOP-7", "SHOP-7")
        );
        assert_eq!(
            r.external_url.as_deref(),
            Some("https://acme.atlassian.net/browse/SHOP-7")
        );
        assert!(r.entries[0].1.contains("Steps\n<<<EVENT_DATA_x"));
        assert!(matches!(
            run(
                json!({"webhookEvent": "jira:issue_created", "user": user, "issue": issue(&["x"])})
            )
            .parsed,
            Parsed::Ignore(_)
        ));
        let upd = |from: &str, to: &str| {
            json!({"webhookEvent": "jira:issue_updated", "user": user, "issue": issue(&["queue"]),
                                    "changelog": {"items": [{"field": "labels", "fromString": from, "toString": to}]}})
        };
        assert!(matches!(
            run(upd("bug", "bug queue")).parsed,
            Parsed::Request(_)
        ));
        assert!(
            matches!(run(upd("queue", "queue bug")).parsed, Parsed::Ignore(_)),
            "not newly added"
        );
        assert!(matches!(
            run(upd("bug", "bugqueue")).parsed,
            Parsed::Ignore(_)
        ));
    }

    #[test]
    fn comments_answer_and_own_comments_are_ignored() {
        let c = |body: Value| {
            json!({"webhookEvent": "comment_created", "issue": issue(&[]),
                                     "comment": {"id": "1", "body": body, "author": {"accountId": "557058:abc"}}})
        };
        let Parsed::Reply(r) = run(c(json!("Ja, mit OAuth"))).parsed else {
            panic!()
        };
        assert!(r.require_waiting);
        assert_eq!(
            (r.external_ref.as_str(), r.text.as_str()),
            ("SHOP-7", "Ja, mit OAuth")
        );
        let own = adf("Fertig", "q-123");
        assert!(matches!(run(c(own)).parsed, Parsed::Ignore(_)));
        assert!(matches!(
            run(json!({"webhookEvent": "jira:issue_deleted", "issue": issue(&[])})).parsed,
            Parsed::Ignore(_)
        ));
        let mut bad = hdr();
        bad.insert(
            "x-atlassian-webhook-identifier",
            HeaderValue::from_static("a b"),
        );
        assert!(parse(&bad, b"{}", &cfg(), SITE).is_err());
    }

    #[test]
    fn helpers() {
        assert!(key_ok("SHOP-7") && key_ok("A1_B-12"));
        assert!(!key_ok("shop-7") && !key_ok("SHOP-") && !key_ok("SHOP-7/../x"));
        assert_eq!(
            site_of("https://A.atlassian.net/rest/x").as_deref(),
            Some("https://a.atlassian.net")
        );
        assert_eq!(text_of(&json!("plain")), "plain");
        let d = adf("Zeile 1\n\nZeile 2", "q-1");
        assert_eq!(text_of(&d), "Zeile 1\nZeile 2\nQueue · Ref q-1");
    }
}
