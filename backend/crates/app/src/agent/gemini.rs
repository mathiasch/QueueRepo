//! Gemini CLI (pinned 0.67.0): `--output-format stream-json --yolo -p …`.
//!
//! * `GEMINI_CLI_TRUST_WORKSPACE=true`: in an untrusted folder 0.67.0 resets
//!   the approval mode from yolo to default (bundle `checkPathTrust`); every
//!   run gets a fresh HOME without `trustedFolders.json`.
//! * Turn limit only via `settings.json` `model.maxSessionTurns` →
//!   `result.error.type = "FatalTurnLimitedError"`, exit code 53.
//! * No result-schema flag, no cost reporting → contract only via the file.
//! * `error` events carry `severity` (`warning` = notice, not a failure).
//! * Real 0.67.0 recording (dummy API key): `result.status:"error"`,
//!   `error.type:"unknown"`, message "API key not valid … API_KEY_INVALID".
use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::{
    error_event, n, s, tool_summary, trunc, AgentEvent, Credential, Invocation, LimitKind,
    ParseState, RunLimits,
};
use crate::models::Usage;

/// Exit code of `FatalTurnLimitedError` in 0.67.0.
pub const TURN_LIMIT_EXIT: i32 = 53;

pub(super) fn invocation(
    inv: &mut Invocation,
    model: Option<&str>,
    cred: &Credential,
    limits: &RunLimits,
) -> Result<()> {
    inv.program = "gemini".into();
    inv.args = [
        "--output-format",
        "stream-json",
        "--yolo",
        "-p",
        "Follow the instructions given on stdin.",
    ]
    .map(String::from)
    .to_vec();
    if let Some(m) = model {
        inv.args.extend(["--model".into(), m.into()]);
    }
    inv.env
        .push(("GEMINI_CLI_TRUST_WORKSPACE".into(), "true".into()));
    let mut settings = json!({});
    match cred.kind.as_str() {
        "oauth_creds" => {
            inv.home_files
                .push((".gemini/oauth_creds.json".into(), cred.secret.clone()));
            settings["selectedAuthType"] = json!("oauth-personal");
            settings["security"] = json!({"auth": {"selectedType": "oauth-personal"}});
            inv.writeback = Some(".gemini/oauth_creds.json".into());
        }
        "api_key" => inv.env.push(("GEMINI_API_KEY".into(), cred.secret.clone())),
        k => bail!("unsupported gemini credential kind {k}"),
    }
    if let Some(t) = limits.turns {
        settings["model"] = json!({"maxSessionTurns": t});
    }
    inv.home_files
        .push((".gemini/settings.json".into(), settings.to_string()));
    Ok(())
}

fn flush(st: &mut ParseState, out: &mut Vec<AgentEvent>) {
    if !st.buf.trim().is_empty() {
        st.last_text = std::mem::take(&mut st.buf);
        out.push(AgentEvent::Text(trunc(&st.last_text, 2000)));
    }
}

pub(super) fn parse(v: &Value, st: &mut ParseState) -> Vec<AgentEvent> {
    let mut out = vec![];
    match s(v, "type").as_str() {
        "message" if s(v, "role") == "assistant" => st.buf.push_str(&s(v, "content")),
        "tool_use" => {
            flush(st, &mut out);
            out.push(AgentEvent::Tool(tool_summary(
                &s(v, "tool_name"),
                &v["parameters"],
            )));
        }
        "error" => {
            let msg = s(v, "message");
            if s(v, "severity") == "warning" {
                out.push(AgentEvent::Text(format!("⚠ {}", trunc(&msg, 2000))));
            } else {
                out.push(error_event(&msg));
            }
        }
        "result" => {
            flush(st, &mut out);
            let u = &v["stats"];
            let cached = n(u, "cached");
            out.push(AgentEvent::UsageTotal(Usage {
                input_tokens: (n(u, "input_tokens") - cached).max(0),
                output_tokens: n(u, "output_tokens"),
                cache_read_tokens: cached,
                ..Default::default()
            }));
            if s(v, "status") == "error" {
                match s(&v["error"], "type").as_str() {
                    "FatalTurnLimitedError" => out.push(AgentEvent::LimitHit(LimitKind::Turns)),
                    // Our own SIGINT; the exit frame carries cancel/timeout/stall.
                    "FatalCancellationError" => {}
                    _ => out.push(error_event(&s(&v["error"], "message"))),
                }
            } else {
                out.push(AgentEvent::Final(std::mem::take(&mut st.last_text)));
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{classify::ErrorCategory, invocation as inv, parse_line, RunKind};
    use super::*;

    fn settings(i: &Invocation) -> Value {
        let (_, c) = i
            .home_files
            .iter()
            .find(|(p, _)| p == ".gemini/settings.json")
            .expect("settings.json is always written");
        serde_json::from_str(c).expect("valid JSON")
    }

    #[test]
    fn settings_json_contains_turn_limit_and_auth() {
        let oauth = Credential {
            kind: "oauth_creds".into(),
            secret: "{}".into(),
        };
        let lim = RunLimits {
            turns: Some(9),
            cost_usd: Some(1.0),
        };
        let i = inv("gemini", None, &oauth, &lim, RunKind::Agent).unwrap();
        let s = settings(&i);
        assert_eq!(s["model"]["maxSessionTurns"], 9);
        assert_eq!(s["selectedAuthType"], "oauth-personal");
        assert_eq!(s["security"]["auth"]["selectedType"], "oauth-personal");
        assert!(i
            .env
            .contains(&("GEMINI_CLI_TRUST_WORKSPACE".into(), "true".into())));
        assert_eq!(i.writeback.as_deref(), Some(".gemini/oauth_creds.json"));
        assert!(i.queue_files.is_empty(), "gemini has no schema flag");

        let key = Credential {
            kind: "api_key".into(),
            secret: "k".into(),
        };
        let i = inv("gemini", None, &key, &RunLimits::default(), RunKind::Agent).unwrap();
        assert_eq!(settings(&i), json!({}));
        assert!(i
            .env
            .contains(&("GEMINI_CLI_TRUST_WORKSPACE".into(), "true".into())));
    }

    #[test]
    fn parses_gemini_stream_and_buffers_deltas() {
        let mut st = ParseState::default();
        assert!(parse_line(
            "gemini",
            r#"{"type":"message","role":"assistant","content":"Hel","delta":true}"#,
            &mut st
        )
        .is_empty());
        parse_line(
            "gemini",
            r#"{"type":"message","role":"assistant","content":"lo","delta":true}"#,
            &mut st,
        );
        let ev = parse_line(
            "gemini",
            r#"{"type":"result","status":"success","stats":{"input_tokens":50,"output_tokens":7,"cached":20}}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Text(t) if t == "Hello"));
        assert!(
            matches!(&ev[1], AgentEvent::UsageTotal(u) if u.input_tokens == 30 && u.output_tokens == 7)
        );
        assert!(matches!(&ev[2], AgentEvent::Final(t) if t == "Hello"));
    }

    #[test]
    fn gemini_warning_is_not_error_and_turn_limit_is_limit() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "gemini",
            r#"{"type":"error","severity":"warning","message":"Loop detected, stopping"}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Text(t) if t.starts_with('⚠')));
        let ev = parse_line(
            "gemini",
            r#"{"type":"result","status":"error","error":{"type":"FatalTurnLimitedError","message":"Reached max session turns"},"stats":{}}"#,
            &mut st,
        );
        assert_eq!(ev[1], AgentEvent::LimitHit(LimitKind::Turns));
        let ev = parse_line(
            "gemini",
            r#"{"type":"result","status":"error","error":{"type":"FatalCancellationError","message":"Operation cancelled."},"stats":{}}"#,
            &mut st,
        );
        assert_eq!(ev.len(), 1);
        let ev = parse_line(
            "gemini",
            r#"{"type":"error","severity":"error","message":"[API Error: got status: 429 Too Many Requests. RESOURCE_EXHAUSTED]"}"#,
            &mut st,
        );
        assert!(matches!(
            &ev[0],
            AgentEvent::Failure {
                category: ErrorCategory::RateLimit(_),
                ..
            }
        ));
    }
}
