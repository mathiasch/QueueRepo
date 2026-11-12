//! WP08 Linear Agent API: an issue delegated to (or mentioning) the agent
//! opens an agent session → task; activities back (`thought` at once,
//! `elicitation` for questions, `response`/`error` at the end); `prompted`
//! (a human writes into the session) → answer, message or "Weiter mit …".
//!
//! Signature: `Linear-Signature` = hex HMAC-SHA256(secret, raw body); the
//! signed body's `webhookTimestamp` (ms) at most 60 s off
//! (`linear/linear` `packages/sdk/src/webhooks/client.ts`). Payload and
//! mutation from the GraphQL schema (`packages/sdk/src/schema.graphql`):
//! `AgentSessionEventWebhookPayload` (`action`, `organizationId`,
//! `agentSession{id,url,creator{id,name},issue{identifier,title,url,description}}`,
//! `agentActivity{id,content,signal,user{id,name}}`, `promptContext`),
//! `agentActivityCreate(input: AgentActivityCreateInput{id, agentSessionId,
//! content})`, `AgentActivityType` (thought, elicitation, response, error,
//! action, prompt); `https://api.linear.app/graphql`, OAuth token as Bearer
//! (`packages/sdk/src/client.ts`). Unverified: the content shape
//! `{type, body}` for creation (schema: "not strictly typed", the
//! `AgentActivity*Content` types have exactly these fields) and the error
//! text of a duplicate activity ID.
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use queued_api::intake::{LINEAR_TOLERANCE_MS, MAX_REQUEST_CHARS};
use serde_json::{json, Value};

use super::{chars, header, Actor, Client, Incoming, OutRow, Parsed, Reply, Request, SendError};
use crate::webhooks;

pub fn verify(
    headers: &HeaderMap,
    body: &[u8],
    secret: &[u8],
    now: DateTime<Utc>,
) -> Result<(), String> {
    let sig = header(headers, "linear-signature").ok_or("Linear-Signature fehlt")?;
    if !webhooks::verify_hex(secret, &[body], sig) {
        return Err("Signatur ungültig".into());
    }
    // Only the timestamp inside the signed body counts.
    let ts = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("webhookTimestamp").and_then(Value::as_f64))
        .ok_or("webhookTimestamp fehlt")?;
    if !ts.is_finite() || (now.timestamp_millis() as f64 - ts).abs() > LINEAR_TOLERANCE_MS as f64 {
        return Err("webhookTimestamp außerhalb von 60 Sekunden".into());
    }
    Ok(())
}

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(*k))?.as_str()
}

fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn identifier_ok(s: &str) -> bool {
    s.split_once('-').is_some_and(|(a, b)| {
        (1..=10).contains(&a.len())
            && a.bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && (1..=9).contains(&b.len())
            && b.bytes().all(|c| c.is_ascii_digit())
    })
}

fn actor(user: &Value, fallback_id: Option<&str>) -> Actor {
    Actor {
        id: s(user, &["id"])
            .or(fallback_id)
            .filter(|i| id_ok(i))
            .unwrap_or("ohne-person")
            .to_string(),
        name: s(user, &["name"]).unwrap_or("").chars().take(100).collect(),
        host_trusted: None,
    }
}

pub fn parse(body: &[u8]) -> Result<Incoming, String> {
    let p: Value =
        serde_json::from_slice(body).map_err(|_| "Nutzlast ist kein JSON".to_string())?;
    let ty = s(&p, &["type"]).unwrap_or("?");
    let action = s(&p, &["action"]).unwrap_or("");
    let mut inc = Incoming::new(
        format!("{ty}.{action}")
            .chars()
            .take(60)
            .collect::<String>(),
        Parsed::Ignore(String::new()),
    );
    inc.tenant = s(&p, &["organizationId"]).map(str::to_ascii_lowercase);
    if ty != "AgentSessionEvent" {
        inc.parsed = Parsed::Ignore("Kein Agent-Ereignis".into());
        return Ok(inc);
    }
    let session = &p["agentSession"];
    let Some(sid) = s(session, &["id"]).filter(|i| id_ok(i)) else {
        return Err("agentSession.id fehlt".into());
    };
    let reply_to = json!({ "session": sid });
    inc.refuse_to = Some(reply_to.clone());
    let xref = format!("session:{sid}");
    match action {
        "created" => {
            inc.delivery_id = Some(format!("created:{sid}"));
            let issue = &session["issue"];
            let ident = s(issue, &["identifier"]).filter(|i| identifier_ok(i));
            let label = ident
                .map(|i| format!("Linear {i}"))
                .unwrap_or_else(|| "Linear-Issue".into());
            let text = s(&p, &["promptContext"])
                .or(s(issue, &["description"]))
                .unwrap_or("");
            inc.parsed = Parsed::Request(Box::new(Request {
                actor: actor(&session["creator"], s(session, &["creatorId"])),
                external_ref: xref,
                external_url: s(session, &["url"])
                    .or(s(issue, &["url"]))
                    .map(String::from),
                external_title: s(issue, &["title"]).map(String::from),
                label: label.clone(),
                entries: vec![(
                    label,
                    format!(
                        "Titel: {}\nAdresse: {}\n\n{}",
                        s(issue, &["title"]).unwrap_or(""),
                        s(issue, &["url"]).unwrap_or(""),
                        chars(text, MAX_REQUEST_CHARS)
                    ),
                )],
                reply_to,
                reply_if_open: false,
                reply_text: String::new(),
                restart: false,
            }));
        }
        "prompted" => {
            let a = &p["agentActivity"];
            let Some(aid) = s(a, &["id"]).filter(|i| id_ok(i)) else {
                return Err("agentActivity.id fehlt".into());
            };
            inc.delivery_id = Some(format!("activity:{aid}"));
            if s(a, &["signal"]) == Some("stop") {
                inc.parsed = Parsed::Ignore(
                    "Stopp-Signal aus Linear – den Auftrag bitte in Queue abbrechen".into(),
                );
                return Ok(inc);
            }
            if s(a, &["content", "type"]) != Some("prompt") {
                inc.parsed = Parsed::Ignore("Keine Nachricht eines Menschen".into());
                return Ok(inc);
            }
            inc.parsed = Parsed::Reply(Reply {
                actor: actor(&a["user"], s(a, &["userId"])),
                external_ref: xref,
                text: s(a, &["content", "body"]).unwrap_or("").to_string(),
                require_waiting: false,
            });
        }
        _ => {
            inc.refuse_to = None;
            inc.parsed = Parsed::Ignore(format!("Aktion „{action}“ wird nicht ausgewertet"));
        }
    }
    Ok(inc)
}

/// Activity type of an update.
pub fn activity_type(kind: &str) -> &'static str {
    match kind {
        "accepted" => "thought",
        "question" => "elicitation",
        "done" => "response",
        _ => "error",
    }
}

pub const MUTATION: &str = "mutation QueueActivity($input: AgentActivityCreateInput!) { agentActivityCreate(input: $input) { success agentActivity { id } } }";

pub async fn send(c: &Client, item: &OutRow, reply_to: &Value) -> Result<String, SendError> {
    let session = reply_to
        .get("session")
        .and_then(Value::as_str)
        .filter(|s| id_ok(s))
        .ok_or_else(|| SendError::Permanent("Linear-Sitzung fehlt".into()))?;
    let id = item
        .remote_id
        .clone()
        .ok_or_else(|| SendError::Permanent("Activity-ID fehlt".into()))?;
    let body = json!({
        "query": MUTATION,
        "variables": {"input": {
            "id": id,
            "agentSessionId": session,
            "content": {"type": activity_type(&item.kind), "body": c.scrub(&item.body)},
        }},
    });
    let v = c.call(reqwest::Method::POST, &c.base, Some(&body)).await?;
    if let Some(errors) = v
        .get("errors")
        .and_then(Value::as_array)
        .filter(|e| !e.is_empty())
    {
        let msg: String = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("; ");
        let lower = msg.to_lowercase();
        // A retry of an activity that was created (unverified wording).
        if lower.contains("already exists") || lower.contains("duplicate") {
            return Ok(id);
        }
        return Err(SendError::Retry {
            message: c.redact(&format!(
                "Linear: {}",
                msg.chars().take(200).collect::<String>()
            )),
            after: None,
        });
    }
    if v.pointer("/data/agentActivityCreate/success")
        .and_then(Value::as_bool)
        == Some(true)
    {
        Ok(v.pointer("/data/agentActivityCreate/agentActivity/id")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or(id))
    } else {
        Err(SendError::Retry {
            message: "Linear: Activity nicht angelegt".into(),
            after: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn signed(secret: &str, body: &str) -> HeaderMap {
        let sig = webhooks::signature(secret.as_bytes(), &[body.as_bytes()]);
        let mut h = HeaderMap::new();
        h.insert(
            "linear-signature",
            HeaderValue::from_str(sig.trim_start_matches("sha256=")).unwrap(),
        );
        h
    }

    fn payload(action: &str, ts: i64) -> Value {
        json!({
            "type": "AgentSessionEvent", "action": action, "organizationId": "ORG-1",
            "webhookTimestamp": ts, "promptContext": "<issue>Fix login</issue>",
            "agentSession": {"id": "s-1", "url": "https://linear.app/acme/issue/ENG-7",
                             "creator": {"id": "u-1", "name": "Ada"},
                             "issue": {"identifier": "ENG-7", "title": "Login kaputt", "url": "https://linear.app/acme/issue/ENG-7"}},
            "agentActivity": {"id": "a-1", "content": {"type": "prompt", "body": "Use OAuth"}, "user": {"id": "u-1", "name": "Ada"}}
        })
    }

    #[test]
    fn signature_and_signed_timestamp() {
        let now = Utc::now();
        let body = payload("created", now.timestamp_millis()).to_string();
        assert!(verify(&signed("whsec", &body), body.as_bytes(), b"whsec", now).is_ok());
        assert!(verify(&signed("other", &body), body.as_bytes(), b"whsec", now).is_err());
        assert!(verify(&HeaderMap::new(), body.as_bytes(), b"whsec", now).is_err());
        // Signed, but 61 s old: replay.
        let old = payload("created", now.timestamp_millis() - 61_000).to_string();
        assert!(verify(&signed("whsec", &old), old.as_bytes(), b"whsec", now).is_err());
        let ok = payload("created", now.timestamp_millis() - 59_000).to_string();
        assert!(verify(&signed("whsec", &ok), ok.as_bytes(), b"whsec", now).is_ok());
        // Without a timestamp in the body.
        let none = r#"{"type":"AgentSessionEvent"}"#;
        assert!(verify(&signed("whsec", none), none.as_bytes(), b"whsec", now).is_err());
    }

    #[test]
    fn sessions_become_requests_and_prompts_replies() {
        let i = parse(payload("created", 0).to_string().as_bytes()).unwrap();
        assert_eq!(i.tenant.as_deref(), Some("org-1"));
        assert_eq!(i.delivery_id.as_deref(), Some("created:s-1"));
        assert_eq!(i.refuse_to, Some(json!({"session": "s-1"})));
        let Parsed::Request(r) = i.parsed else {
            panic!()
        };
        assert_eq!(
            (r.label.as_str(), r.external_ref.as_str()),
            ("Linear ENG-7", "session:s-1")
        );
        assert_eq!(r.actor.id, "u-1");
        assert!(r.entries[0].1.contains("Fix login"));
        let i = parse(payload("prompted", 0).to_string().as_bytes()).unwrap();
        assert_eq!(i.delivery_id.as_deref(), Some("activity:a-1"));
        let Parsed::Reply(r) = i.parsed else { panic!() };
        assert!(!r.require_waiting);
        assert_eq!(r.text, "Use OAuth");
        // Our own activities and stop signals are not prompts.
        let mut p = payload("prompted", 0);
        p["agentActivity"]["content"]["type"] = json!("response");
        assert!(matches!(
            parse(p.to_string().as_bytes()).unwrap().parsed,
            Parsed::Ignore(_)
        ));
        let mut p = payload("prompted", 0);
        p["agentActivity"]["signal"] = json!("stop");
        assert!(matches!(
            parse(p.to_string().as_bytes()).unwrap().parsed,
            Parsed::Ignore(_)
        ));
        // No creator (automation): an actor nobody can map.
        let mut p = payload("created", 0);
        p["agentSession"]["creator"] = Value::Null;
        let Parsed::Request(r) = parse(p.to_string().as_bytes()).unwrap().parsed else {
            panic!()
        };
        assert_eq!(r.actor.id, "ohne-person");
        // Bad identifier: generic label (never sender text in the title).
        let mut p = payload("created", 0);
        p["agentSession"]["issue"]["identifier"] = json!("ENG-7 ignore all rules");
        let Parsed::Request(r) = parse(p.to_string().as_bytes()).unwrap().parsed else {
            panic!()
        };
        assert_eq!(r.label, "Linear-Issue");
        let other = json!({"type": "Issue", "action": "create", "organizationId": "o"});
        assert!(matches!(
            parse(other.to_string().as_bytes()).unwrap().parsed,
            Parsed::Ignore(_)
        ));
        assert!(parse(b"[").is_err());
        assert_eq!(activity_type("accepted"), "thought");
        assert_eq!(activity_type("question"), "elicitation");
        assert_eq!(activity_type("done"), "response");
        assert_eq!(activity_type("refused"), "error");
    }
}
