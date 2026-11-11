//! opencode (pinned 1.19.5): `opencode run --format json --dangerously-skip-permissions --model queue/<m>`.
//!
//! Verified against the JavaScript bundled in the 1.19.5 linux-x64 binary
//! (read as data, never executed – see docs/roadmap/WP13-plan.md §0):
//! * the prompt is read from stdin when it is not a TTY;
//! * `--format json` writes one object per line
//!   `{type, timestamp, sessionID, …}` with `type` ∈ `step_start`,
//!   `step_finish` (`part.tokens = {input, output, reasoning, cache:{read,write}}`,
//!   `part.cost`), `text` (`part.text`), `reasoning`, `tool_use`
//!   (`part.tool`, `part.state.{status,input,error}`) and `error`
//!   (`error.name`, `error.data.message`);
//! * config via `OPENCODE_CONFIG_CONTENT`, `{env:VAR}` placeholders,
//!   `OPENCODE_DISABLE_AUTOUPDATE`.
//!
//! * error types `ProviderAuthError{providerID,message}`,
//!   `APIError{message,statusCode,isRetryable,…}`, `MessageOutputLengthError`,
//!   `ContextOverflowError`, `MessageAbortedError` (NamedError → `{name, data}`).
//!
//! Not verified: a real run against any provider. opencode has no
//! result-schema flag and no turn/cost limit → contract via
//! `.queue/result.json`, limits via the queue's own guards.
use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::{
    classify::ErrorCategory, error_event, n, s, tool_summary, trunc, usd, AgentEvent, ConnCfg,
    Credential, Invocation, ParseState, RunKind, Session,
};
use crate::models::Usage;

/// Environment variable that carries the API key (referenced as `{env:…}`).
pub const KEY_ENV: &str = "QUEUE_LLM_API_KEY";

/// opencode provider config + model reference for a connector.
pub(super) fn provider_config(c: &ConnCfg, model: &str) -> Result<(Value, String)> {
    let key = format!("{{env:{KEY_ENV}}}");
    if c.kind == "gemini_api" {
        return Ok((
            json!({"google": {"options": {"apiKey": key}}}),
            format!("google/{model}"),
        ));
    }
    let Some((crate::connectors::Wire::OpenAiChat, base)) =
        crate::connectors::wire(&c.kind, &c.config)
    else {
        bail!("{} kann nicht mit opencode betrieben werden", c.kind);
    };
    Ok((
        json!({"queue": {
            "npm": "@ai-sdk/openai-compatible",
            "name": c.label,
            "options": {"baseURL": base, "apiKey": key},
            "models": {model: {"name": model}}
        }}),
        format!("queue/{model}"),
    ))
}

pub(super) fn invocation(
    inv: &mut Invocation,
    model: Option<&str>,
    cred: &Credential,
    kind: RunKind,
    session: &Session,
) -> Result<()> {
    let Some(c) = &cred.conn else {
        bail!("opencode braucht einen Zugang (Connector)");
    };
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        bail!("Für opencode muss ein Modell angegeben werden (z. B. mistral-large-4)");
    };
    let (provider, model_ref) = provider_config(c, model)?;
    let config = json!({
        "$schema": "https://opencode.ai/config.json",
        "autoupdate": false,
        "provider": provider,
        "model": model_ref,
    });
    inv.program = "opencode".into();
    inv.args = vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        "--dangerously-skip-permissions".into(),
        "--model".into(),
        model_ref,
    ];
    // WP06: built-in agent `plan` ("Plan mode. Disallows all edit tools") and
    // `--session <id>` ("session id to continue") – 1.19.5 binary, not executed.
    if kind == RunKind::Plan {
        inv.args.extend(["--agent".into(), "plan".into()]);
    }
    if let Session::Resume(id) = session {
        inv.args.extend(["--session".into(), id.clone()]);
    }
    inv.env
        .push(("OPENCODE_CONFIG_CONTENT".into(), config.to_string()));
    inv.env
        .push(("OPENCODE_DISABLE_AUTOUPDATE".into(), "1".into()));
    // Self-hosted endpoints may not need a key; the SDK still wants a value.
    let key = if cred.secret.trim().is_empty() {
        "none".to_string()
    } else {
        cred.secret.clone()
    };
    inv.env.push((KEY_ENV.into(), key));
    Ok(())
}

fn category(err: &Value) -> Option<ErrorCategory> {
    let name = s(err, "name");
    let status = err["data"]["statusCode"]
        .as_u64()
        .and_then(|x| u16::try_from(x).ok());
    match (name.as_str(), status) {
        ("ProviderAuthError", _) | (_, Some(401 | 403)) => Some(ErrorCategory::Auth),
        (_, Some(429)) => Some(ErrorCategory::RateLimit(None)),
        (_, Some(404)) => Some(ErrorCategory::ModelNotFound),
        (_, Some(s)) if s >= 500 => Some(ErrorCategory::ServerError),
        ("APIError", _) if err["data"]["isRetryable"].as_bool() == Some(true) => {
            Some(ErrorCategory::Overloaded)
        }
        ("MessageOutputLengthError", _) => Some(ErrorCategory::MaxOutputTokens),
        _ => None,
    }
}

pub(super) fn parse(v: &Value, st: &mut ParseState) -> Vec<AgentEvent> {
    let part = &v["part"];
    match s(v, "type").as_str() {
        "text" => {
            let t = s(part, "text");
            if t.trim().is_empty() {
                return vec![];
            }
            st.last_text = t.clone();
            vec![AgentEvent::Text(trunc(&t, 2000)), AgentEvent::Final(t)]
        }
        "tool_use" => {
            let state = &part["state"];
            let mut out = vec![AgentEvent::Tool(tool_summary(
                &s(part, "tool"),
                &state["input"],
            ))];
            if s(state, "status") == "error" {
                out.push(AgentEvent::Text(format!(
                    "⚠ Werkzeugfehler: {}",
                    trunc(&s(state, "error"), 1000)
                )));
            }
            out
        }
        "step_finish" => {
            let t = &part["tokens"];
            let u = Usage {
                input_tokens: n(t, "input"),
                output_tokens: n(t, "output").saturating_add(n(t, "reasoning")),
                cache_read_tokens: n(&t["cache"], "read"),
                cache_write_tokens: n(&t["cache"], "write"),
                cost_usd: usd(&part["cost"]),
            };
            vec![AgentEvent::UsageDelta(u)]
        }
        "error" => {
            let err = &v["error"];
            let msg = match err["data"]["message"].as_str() {
                Some(m) if !m.is_empty() => m.to_string(),
                _ => s(err, "name"),
            };
            let msg = if msg.is_empty() {
                "opencode meldete einen Fehler ohne Text".to_string()
            } else {
                msg
            };
            match category(err) {
                Some(category) => vec![AgentEvent::Failure {
                    category,
                    message: trunc(&msg, 4000),
                }],
                None => vec![error_event(&msg)],
            }
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::super::{invocation as inv, parse_line, RunKind, RunLimits};
    use super::*;

    fn cred(kind: &str, config: Value, secret: &str) -> Credential {
        Credential {
            kind: format!("connector:{kind}"),
            secret: secret.into(),
            conn: Some(ConnCfg {
                id: None,
                kind: kind.into(),
                label: "Mein Zugang".into(),
                config,
                flex: false,
            }),
        }
    }

    #[test]
    fn invocation_uses_config_content_and_env_key() {
        let c = cred("mistral", json!({"endpoint": "eu"}), "SECRET-1");
        let i = inv(
            "opencode",
            Some("mistral-large-4"),
            &c,
            &RunLimits::default(),
            RunKind::Agent,
        )
        .unwrap();
        assert_eq!(i.program, "opencode");
        assert_eq!(
            i.args,
            [
                "run",
                "--format",
                "json",
                "--dangerously-skip-permissions",
                "--model",
                "queue/mistral-large-4"
            ]
        );
        let cfg: Value = serde_json::from_str(
            &i.env
                .iter()
                .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
                .unwrap()
                .1,
        )
        .unwrap();
        assert_eq!(
            cfg["provider"]["queue"]["options"]["baseURL"],
            "https://api.eu.mistral.ai/v1"
        );
        assert_eq!(
            cfg["provider"]["queue"]["options"]["apiKey"],
            "{env:QUEUE_LLM_API_KEY}"
        );
        assert!(
            !cfg.to_string().contains("SECRET-1"),
            "key only in its env var"
        );
        assert!(i.env.contains(&(KEY_ENV.into(), "SECRET-1".into())));
        assert!(!i.args.iter().any(|a| a.contains("SECRET")));

        let g = cred("gemini_api", json!({}), "G");
        let i = inv(
            "opencode",
            Some("gemini-3.8-flash"),
            &g,
            &RunLimits::default(),
            RunKind::Review,
        )
        .unwrap();
        assert!(i.args.contains(&"google/gemini-3.8-flash".to_string()));

        let local = cred(
            "openai_compatible",
            json!({"base_url": "http://gpu:8000/v1"}),
            "",
        );
        let i = inv(
            "opencode",
            Some("qwen"),
            &local,
            &RunLimits::default(),
            RunKind::Agent,
        )
        .unwrap();
        assert!(i.env.contains(&(KEY_ENV.into(), "none".into())));

        assert!(
            inv("opencode", None, &c, &RunLimits::default(), RunKind::Agent).is_err(),
            "model required"
        );
        let no_conn = Credential {
            kind: "api_key".into(),
            secret: "x".into(),
            conn: None,
        };
        assert!(inv(
            "opencode",
            Some("m"),
            &no_conn,
            &RunLimits::default(),
            RunKind::Agent
        )
        .is_err());
        let bedrock = cred("bedrock", json!({"geo": "eu"}), "x");
        assert!(inv(
            "opencode",
            Some("m"),
            &bedrock,
            &RunLimits::default(),
            RunKind::Agent
        )
        .is_err());
    }

    #[test]
    fn parses_events() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "opencode",
            r#"{"type":"text","timestamp":1,"sessionID":"s","part":{"type":"text","text":"Fertig."}}"#,
            &mut st,
        );
        // WP06: the session id comes first, once per session.
        assert_eq!(
            ev,
            vec![
                AgentEvent::Session("s".into()),
                AgentEvent::Text("Fertig.".into()),
                AgentEvent::Final("Fertig.".into())
            ]
        );
        let ev = parse_line(
            "opencode",
            r#"{"type":"step_finish","part":{"type":"step-finish","tokens":{"input":1200,"output":80,"reasoning":20,"cache":{"read":5000,"write":300}},"cost":0.0123}}"#,
            &mut st,
        );
        assert_eq!(
            ev,
            vec![AgentEvent::UsageDelta(Usage {
                input_tokens: 1200,
                output_tokens: 100,
                cache_read_tokens: 5000,
                cache_write_tokens: 300,
                cost_usd: 0.0123
            })]
        );
        let ev = parse_line(
            "opencode",
            r#"{"type":"tool_use","part":{"tool":"bash","state":{"status":"error","input":{"command":"ls"},"error":"denied"}}}"#,
            &mut st,
        );
        assert_eq!(ev[0], AgentEvent::Tool("bash: ls".into()));
        assert!(matches!(&ev[1], AgentEvent::Text(t) if t.contains("denied")));
        let ev = parse_line(
            "opencode",
            r#"{"type":"error","error":{"name":"APIError","data":{"message":"Too many requests","statusCode":429}}}"#,
            &mut st,
        );
        assert!(matches!(
            &ev[0],
            AgentEvent::Failure {
                category: ErrorCategory::RateLimit(_),
                ..
            }
        ));
        let ev = parse_line(
            "opencode",
            r#"{"type":"error","error":{"name":"ProviderAuthError","data":{"message":"bad key"}}}"#,
            &mut st,
        );
        assert!(matches!(
            &ev[0],
            AgentEvent::Failure {
                category: ErrorCategory::Auth,
                ..
            }
        ));
        let ev = parse_line(
            "opencode",
            r#"{"type":"error","error":{"name":"UnknownError","data":{"message":"something broke"}}}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Error(t) if t == "something broke"));
        assert!(parse_line("opencode", r#"{"type":"step_start","part":{}}"#, &mut st).is_empty());
        assert!(parse_line(
            "opencode",
            r#"{"type":"text","part":{"text":"  "}}"#,
            &mut st
        )
        .is_empty());
    }
}
