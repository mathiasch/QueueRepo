//! WP08 Slack: app mention or slash command → task, updates as thread
//! replies, answers in the thread.
//!
//! Signature: `X-Slack-Signature: v0=<hex>` = HMAC-SHA256(signing secret,
//! `v0:{X-Slack-Request-Timestamp}:{body}`), timestamp at most 5 minutes
//! off (`slackapi/bolt-js` `src/receivers/verify-request.ts`,
//! `slackapi/python-slack-sdk` `slack_sdk/signature/__init__.py`; the test
//! vector below is the one of the SDK tests, which is the example of the
//! Slack docs). Payload fields from `slackapi/java-slack-sdk`
//! (`EventsApiPayload`: `team_id`, `event_id`, `authorizations`;
//! `AppMentionEvent`/`MessageEvent`: `user`, `text`, `ts`, `thread_ts`,
//! `channel`, `bot_id`, `subtype`; `SlashCommandPayload`: `team_id`,
//! `user_id`, `user_name`, `channel_id`, `command`, `text`, `trigger_id`;
//! `UrlVerificationPayload`: `challenge`; `chat.postMessage` with
//! `thread_ts` and `metadata`; `conversations.replies` with
//! `include_all_metadata`).
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use queued_api::intake::{MAX_REQUEST_CHARS, SLACK_TOLERANCE_SECS};
use serde_json::{json, Value};

use super::{chars, header, Actor, Client, Incoming, OutRow, Parsed, Reply, Request, SendError};
use crate::{state::AppState, webhooks};

pub fn verify(
    headers: &HeaderMap,
    body: &[u8],
    secret: &[u8],
    now: DateTime<Utc>,
) -> Result<(), String> {
    let (Some(ts), Some(sig)) = (
        header(headers, "x-slack-request-timestamp"),
        header(headers, "x-slack-signature"),
    ) else {
        return Err("X-Slack-Request-Timestamp oder X-Slack-Signature fehlt".into());
    };
    let Some(hex) = sig.strip_prefix("v0=") else {
        return Err("Unbekannte Signaturversion".into());
    };
    if !webhooks::verify_hex(secret, &[b"v0:", ts.as_bytes(), b":", body], hex) {
        return Err("Signatur ungültig".into());
    }
    let at: i64 = ts.parse().map_err(|_| "Zeitstempel ungültig".to_string())?;
    if (now.timestamp() - at).abs() > SLACK_TOLERANCE_SECS {
        return Err("Zeitstempel außerhalb von ±5 Minuten".into());
    }
    Ok(())
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn slack_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn slack_ts(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 30
        && s.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty()
                && !b.is_empty()
                && (a.bytes().chain(b.bytes())).all(|c| c.is_ascii_digit())
        })
}

/// Removes `<@U…>` mentions (Slack's encoding of a user mention).
fn without_mentions(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("<@") {
        out.push_str(&rest[..i]);
        match rest[i..].find('>') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                rest = &rest[i..];
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

pub fn parse(headers: &HeaderMap, body: &[u8]) -> Result<Incoming, String> {
    let ct = header(headers, "content-type").unwrap_or("");
    if ct
        .to_ascii_lowercase()
        .starts_with("application/x-www-form-urlencoded")
    {
        return slash(body);
    }
    let p: Value =
        serde_json::from_slice(body).map_err(|_| "Nutzlast ist kein JSON".to_string())?;
    match s(&p, "type") {
        Some("url_verification") => {
            let c = s(&p, "challenge")
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>();
            Ok(Incoming::new(
                "url_verification",
                Parsed::Challenge(json!({ "challenge": c })),
            ))
        }
        Some("event_callback") => event(&p),
        other => Ok(Incoming::new(
            other.unwrap_or("?").chars().take(60).collect::<String>(),
            Parsed::Ignore("Nicht ausgewertet".into()),
        )),
    }
}

fn slash(body: &[u8]) -> Result<Incoming, String> {
    let f: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(body).into_owned().collect();
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    let (team, user, channel, trigger) = (
        get("team_id"),
        get("user_id"),
        get("channel_id"),
        get("trigger_id"),
    );
    if !slack_id(team)
        || !slack_id(user)
        || !slack_id(channel)
        || trigger.is_empty()
        || trigger.len() > 150
    {
        return Err("Slash-Befehl unvollständig".into());
    }
    let command: String = get("command").chars().take(40).collect();
    let mut inc = Incoming::new(format!("slash {command}"), Parsed::Ignore(String::new()));
    inc.slash = true;
    inc.tenant = Some(team.to_string());
    inc.delivery_id = Some(format!(
        "slash:{}",
        trigger.chars().take(150).collect::<String>()
    ));
    let text = get("text").trim();
    if text.is_empty() {
        inc.parsed = Parsed::Ignore(format!(
            "Bitte schreib, was zu tun ist, z. B. „{command} Füge eine Suche hinzu“"
        ));
        return Ok(inc);
    }
    let name: String = get("user_name").chars().take(100).collect();
    inc.parsed = Parsed::Request(Box::new(Request {
        actor: Actor {
            id: user.to_string(),
            name: name.clone(),
            host_trusted: None,
        },
        external_ref: format!("slash:{trigger}"),
        external_url: None,
        external_title: Some(chars(text, 120)),
        label: "Slack-Auftrag".into(),
        entries: vec![(
            format!("Slack-Befehl {command} von {name}"),
            chars(text, MAX_REQUEST_CHARS),
        )],
        reply_to: json!({"channel": channel, "requester": user}),
        reply_if_open: false,
        reply_text: String::new(),
        restart: false,
    }));
    Ok(inc)
}

fn event(p: &Value) -> Result<Incoming, String> {
    let ev = p.get("event").cloned().unwrap_or(Value::Null);
    let etype = s(&ev, "type").unwrap_or("?");
    let mut inc = Incoming::new(
        etype.chars().take(60).collect::<String>(),
        Parsed::Ignore(String::new()),
    );
    inc.tenant = s(p, "team_id").map(String::from);
    let ignore = |why: &str| Parsed::Ignore(why.to_string());
    let (Some(channel), Some(ts)) = (s(&ev, "channel"), s(&ev, "ts")) else {
        inc.parsed = ignore("Ereignis ohne Kanal");
        return Ok(inc);
    };
    if !slack_id(channel) || !slack_ts(ts) {
        return Err("Ungültiger Kanal oder Zeitstempel".into());
    }
    // One message can arrive as `app_mention` and as `message`: the key is
    // the message, not the event.
    inc.delivery_id = Some(format!("msg:{channel}:{ts}"));
    if ev.get("bot_id").is_some() || ev.get("subtype").is_some() {
        if etype == "message" {
            inc.delivery_id = None;
        }
        inc.parsed = ignore("Bot-Nachricht oder Systemnachricht");
        return Ok(inc);
    }
    let Some(user) = s(&ev, "user").filter(|u| slack_id(u)) else {
        inc.parsed = ignore("Nachricht ohne Person");
        return Ok(inc);
    };
    let actor = Actor {
        id: user.to_string(),
        name: String::new(),
        host_trusted: None,
    };
    let text = s(&ev, "text").unwrap_or("");
    let thread = s(&ev, "thread_ts").filter(|t| slack_ts(t));
    match etype {
        "app_mention" => {
            let clean = without_mentions(text);
            let root = thread.unwrap_or(ts);
            if clean.is_empty() {
                inc.parsed = ignore("Erwähnung ohne Text");
                return Ok(inc);
            }
            inc.parsed = Parsed::Request(Box::new(Request {
                actor,
                external_ref: format!("{channel}:{root}"),
                external_url: None,
                external_title: Some(chars(&clean, 120)),
                label: "Slack-Auftrag".into(),
                entries: vec![("Slack-Nachricht".into(), chars(&clean, MAX_REQUEST_CHARS))],
                reply_to: json!({"channel": channel, "thread_ts": root}),
                reply_if_open: true,
                reply_text: clean,
                restart: false,
            }));
        }
        "message" => {
            // An ignored `message` must not use up the key of its message:
            // the same message may still arrive as `app_mention`.
            let Some(root) = thread.filter(|t| *t != ts) else {
                inc.delivery_id = None;
                inc.parsed = ignore("Keine Antwort in einem Thread");
                return Ok(inc);
            };
            let bot = p
                .pointer("/authorizations/0/user_id")
                .and_then(Value::as_str)
                .filter(|b| slack_id(b));
            if bot.is_some_and(|b| text.contains(&format!("<@{b}>"))) {
                inc.delivery_id = None;
                inc.parsed = ignore("Erwähnung – kommt als app_mention");
                return Ok(inc);
            }
            inc.parsed = Parsed::Reply(Reply {
                actor,
                external_ref: format!("{channel}:{root}"),
                text: without_mentions(text),
                require_waiting: true,
            });
        }
        _ => inc.parsed = ignore("Ereignis wird nicht ausgewertet"),
    }
    Ok(inc)
}

/// Slack text escaping (`&`, `<`, `>`): no `<!channel>` or links from
/// agent output.
pub fn escape(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn target(reply_to: &Value) -> Result<(String, Option<String>), SendError> {
    let ch = reply_to
        .get("channel")
        .and_then(Value::as_str)
        .filter(|c| slack_id(c))
        .ok_or_else(|| SendError::Permanent("Slack-Kanal fehlt".into()))?;
    let th = reply_to
        .get("thread_ts")
        .and_then(Value::as_str)
        .filter(|t| slack_ts(t))
        .map(String::from);
    Ok((ch.to_string(), th))
}

fn slack_error(v: &Value) -> Option<SendError> {
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let e = v
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unbekannt");
    let e: String = e.chars().take(80).collect();
    Some(match e.as_str() {
        "channel_not_found" | "is_archived" | "not_in_channel" | "msg_too_long"
        | "invalid_arguments" | "no_text" => SendError::Permanent(format!("Slack: {e}")),
        _ => SendError::Retry {
            message: format!("Slack: {e}"),
            after: None,
        },
    })
}

pub async fn send(c: &Client, item: &OutRow, reply_to: &Value) -> Result<String, SendError> {
    let (channel, thread) = target(reply_to)?;
    let mut text = escape(&c.scrub(&item.body));
    if thread.is_none() {
        // Root message of a slash command: who asked (Slack renders `<@U…>`).
        if let Some(u) = reply_to
            .get("requester")
            .and_then(Value::as_str)
            .filter(|u| slack_id(u))
        {
            text = format!("Auftrag von <@{u}>: {text}");
        }
    }
    let mut body = json!({
        "channel": channel,
        "text": text,
        "metadata": {"event_type": "queue_update", "event_payload": {"ref": item.id.to_string()}},
    });
    if let Some(t) = thread {
        body["thread_ts"] = json!(t);
    }
    let v = c
        .call(
            reqwest::Method::POST,
            &format!("{}/chat.postMessage", c.base),
            Some(&body),
        )
        .await?;
    if let Some(e) = slack_error(&v) {
        return Err(e);
    }
    v.get("ts")
        .and_then(Value::as_str)
        .filter(|t| slack_ts(t))
        .map(String::from)
        .ok_or_else(|| SendError::Retry {
            message: "Slack: Antwort ohne ts".into(),
            after: None,
        })
}

/// A reply of an earlier attempt (metadata `ref`) in the thread. A root
/// message without a thread cannot be looked up (documented residual risk).
pub async fn find(
    c: &Client,
    item: &OutRow,
    reply_to: &Value,
) -> Result<Option<String>, SendError> {
    let (channel, Some(thread)) = target(reply_to)? else {
        return Ok(None);
    };
    let url = format!(
        "{}/conversations.replies?channel={channel}&ts={thread}&include_all_metadata=true&limit=200",
        c.base
    );
    let v = c.call(reqwest::Method::GET, &url, None).await?;
    if let Some(e) = slack_error(&v) {
        return Err(e);
    }
    let id = item.id.to_string();
    Ok(v.get("messages").and_then(Value::as_array).and_then(|a| {
        a.iter()
            .find(|m| {
                m.pointer("/metadata/event_payload/ref")
                    .and_then(Value::as_str)
                    == Some(id.as_str())
            })
            .and_then(|m| s(m, "ts"))
            .map(String::from)
    }))
}

/// The root message of a slash command starts the thread: later updates
/// and answers belong to it.
pub async fn after_sent(
    st: &AppState,
    item: &OutRow,
    reply_to: &Value,
    ts: &str,
) -> sqlx::Result<()> {
    let (Some(link), Ok((channel, None))) = (item.link_id, target(reply_to)) else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE intake_links SET reply_to = reply_to || jsonb_build_object('thread_ts', $2::text),
                external_ref = $3 WHERE id=$1 AND NOT (reply_to ? 'thread_ts')",
    )
    .bind(link)
    .bind(ts)
    .bind(format!("{channel}:{ts}"))
    .execute(&st.db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const SECRET: &str = "8f742231b10e8888abcd99yyyzzz85a5";
    const BODY: &str = "token=xyzz0WbapA4vBCDEFasx0q6G&team_id=T1DC2JH3J&team_domain=testteamnow&channel_id=G8PSS9T3V&channel_name=foobar&user_id=U2CERLKJA&user_name=roadrunner&command=%2Fwebhook-collect&text=&response_url=https%3A%2F%2Fhooks.slack.com%2Fcommands%2FT1DC2JH3J%2F397700885554%2F96rGlfmibIGlgcZRskXaIFfN&trigger_id=398738663015.47445629121.803a0bc887a14d10d2c447fce8b6703c";
    const TS: &str = "1531420618";
    const SIG: &str = "v0=a2114d57b48eac39b9ad189dd8316235a7b4a8d21a10bd27519666489c69b503";

    fn hdr(ts: &str, sig: &str, ct: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "x-slack-request-timestamp",
            HeaderValue::from_str(ts).unwrap(),
        );
        h.insert("x-slack-signature", HeaderValue::from_str(sig).unwrap());
        h.insert("content-type", HeaderValue::from_str(ct).unwrap());
        h
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn documented_test_vector_and_time_window() {
        let h = hdr(TS, SIG, "application/x-www-form-urlencoded");
        assert!(verify(&h, BODY.as_bytes(), SECRET.as_bytes(), at(1531420618)).is_ok());
        assert!(verify(&h, BODY.as_bytes(), SECRET.as_bytes(), at(1531420618 + 300)).is_ok());
        assert!(verify(&h, BODY.as_bytes(), SECRET.as_bytes(), at(1531420618 + 301)).is_err());
        assert!(verify(&h, BODY.as_bytes(), SECRET.as_bytes(), at(1531420618 - 301)).is_err());
        let changed = format!("{BODY}------");
        assert!(verify(&h, changed.as_bytes(), SECRET.as_bytes(), at(1531420618)).is_err());
        assert!(verify(&h, BODY.as_bytes(), b"other", at(1531420618)).is_err());
        let v1 = hdr(TS, &SIG.replace("v0=", "v1="), "x");
        assert!(verify(&v1, BODY.as_bytes(), SECRET.as_bytes(), at(1531420618)).is_err());
        // The signed timestamp: another header value breaks the signature.
        let other = hdr("1531420619", SIG, "x");
        assert!(verify(&other, BODY.as_bytes(), SECRET.as_bytes(), at(1531420618)).is_err());
        // The empty command text of the documented example asks for text.
        let i = parse(&h, BODY.as_bytes()).unwrap();
        assert!(i.slash);
        assert_eq!(i.tenant.as_deref(), Some("T1DC2JH3J"));
        assert!(matches!(i.parsed, Parsed::Ignore(_)));
    }

    #[test]
    fn slash_command_makes_a_request() {
        let body = BODY.replace("text=&", "text=Add+a+search+%3C%3C%3Cx&");
        let i = parse(
            &hdr(TS, SIG, "application/x-www-form-urlencoded"),
            body.as_bytes(),
        )
        .unwrap();
        assert_eq!(
            i.delivery_id.as_deref(),
            Some("slash:398738663015.47445629121.803a0bc887a14d10d2c447fce8b6703c")
        );
        let Parsed::Request(r) = i.parsed else {
            panic!()
        };
        assert_eq!(r.actor.id, "U2CERLKJA");
        assert_eq!(
            r.reply_to,
            json!({"channel": "G8PSS9T3V", "requester": "U2CERLKJA"})
        );
        assert!(r.entries[0].1.contains("Add a search <<<x"));
        assert!(!r.reply_if_open);
    }

    fn ev(event: Value) -> Incoming {
        let p = json!({"type": "event_callback", "team_id": "T1", "event_id": "Ev1",
                       "authorizations": [{"user_id": "UBOT"}], "event": event});
        parse(&hdr(TS, SIG, "application/json"), p.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn mentions_thread_replies_and_loop_guard() {
        let i = ev(
            json!({"type": "app_mention", "user": "U1", "text": "<@UBOT> fix the login", "ts": "1.2", "channel": "C1"}),
        );
        assert_eq!(i.delivery_id.as_deref(), Some("msg:C1:1.2"));
        assert_eq!(i.tenant.as_deref(), Some("T1"));
        let Parsed::Request(r) = i.parsed else {
            panic!()
        };
        assert_eq!(r.external_ref, "C1:1.2");
        assert_eq!(r.reply_text, "fix the login");
        assert!(r.reply_if_open);
        assert_eq!(r.reply_to, json!({"channel": "C1", "thread_ts": "1.2"}));
        // Mention inside a thread belongs to the thread.
        let Parsed::Request(r) = ev(json!({"type": "app_mention", "user": "U1", "text": "<@UBOT> also docs", "ts": "1.5", "thread_ts": "1.2", "channel": "C1"})).parsed else { panic!() };
        assert_eq!(r.external_ref, "C1:1.2");
        // Plain thread reply: only as an answer.
        let Parsed::Reply(r) = ev(json!({"type": "message", "user": "U1", "text": "use postgres", "ts": "1.6", "thread_ts": "1.2", "channel": "C1"})).parsed else { panic!() };
        assert!(r.require_waiting);
        assert_eq!(r.external_ref, "C1:1.2");
        // A top-level message is ignored without using up the message key.
        let top = ev(
            json!({"type": "message", "user": "U1", "text": "<@UBOT> fix", "ts": "1.2", "channel": "C1"}),
        );
        assert!(matches!(top.parsed, Parsed::Ignore(_)));
        assert_eq!(top.delivery_id, None);
        // Same message as app_mention: ignored here.
        assert!(matches!(ev(json!({"type": "message", "user": "U1", "text": "<@UBOT> x", "ts": "1.7", "thread_ts": "1.2", "channel": "C1"})).parsed, Parsed::Ignore(_)));
        // Our own replies (bot_id / subtype) and top-level messages: nothing.
        for e in [
            json!({"type": "message", "bot_id": "B1", "text": "Queue ist fertig", "ts": "1.8", "thread_ts": "1.2", "channel": "C1"}),
            json!({"type": "message", "subtype": "message_changed", "user": "U1", "ts": "1.9", "thread_ts": "1.2", "channel": "C1"}),
            json!({"type": "message", "user": "U1", "text": "hi", "ts": "2.0", "channel": "C1"}),
            json!({"type": "app_mention", "user": "U1", "text": "<@UBOT>", "ts": "2.1", "channel": "C1"}),
            json!({"type": "reaction_added", "user": "U1", "ts": "2.2", "channel": "C1"}),
        ] {
            assert!(matches!(ev(e).parsed, Parsed::Ignore(_)));
        }
        let c = parse(
            &hdr(TS, SIG, "application/json"),
            br#"{"type":"url_verification","challenge":"abc"}"#,
        )
        .unwrap();
        assert_eq!(c.parsed, Parsed::Challenge(json!({"challenge": "abc"})));
        assert!(parse(&hdr(TS, SIG, "application/json"), b"{").is_err());
    }

    #[test]
    fn escaping_and_helpers() {
        assert_eq!(
            escape("<!channel> & <https://x|y>"),
            "&lt;!channel&gt; &amp; &lt;https://x|y&gt;"
        );
        assert_eq!(without_mentions("<@U1> hi <@U2>!"), "hi !");
        assert!(slack_ts("1700000000.000100"));
        assert!(!slack_ts("1700000000"));
        assert!(!slack_ts("a.b"));
        assert!(target(&json!({"channel": "c1"})).is_err());
    }
}
