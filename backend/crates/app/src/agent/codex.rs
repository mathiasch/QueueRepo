//! Codex CLI (pinned 0.167.0): `codex exec --json … --output-schema FILE -`.
//!
//! Verified with real 0.167.0 recordings (no network / proxy refused): retries
//! appear as `error` events "Reconnecting... n/5 (…)", and transport notices
//! as `item.completed` items of type `error` ("Falling back from WebSockets…")
//! – those are not task failures. Codex has no turn or cost limit.
use std::sync::OnceLock;

use anyhow::{bail, Result};
use regex::Regex;
use serde_json::Value;

use super::{
    classify::ErrorCategory, error_event, n, s, trunc, AgentEvent, ConnCfg, Credential, Invocation,
    ParseState, RetryInfo, RunKind,
};
use crate::models::{Usage, RESULT_SCHEMA, REVIEW_SCHEMA};

pub(super) fn invocation(
    inv: &mut Invocation,
    model: Option<&str>,
    cred: &Credential,
    kind: RunKind,
) -> Result<()> {
    let (file, schema) = match kind {
        RunKind::Agent => ("result.schema.json", RESULT_SCHEMA),
        RunKind::Review => ("review.schema.json", REVIEW_SCHEMA),
    };
    inv.program = "codex".into();
    inv.args = [
        "exec",
        "--json",
        "--skip-git-repo-check",
        "--dangerously-bypass-approvals-and-sandbox",
        "--output-schema",
    ]
    .map(String::from)
    .to_vec();
    inv.args.push(format!(".queue/{file}"));
    inv.queue_files.push((file.into(), schema.into()));
    if let Some(m) = model {
        inv.args.extend(["--model".into(), m.into()]);
    }
    inv.args.push("-".into()); // prompt from stdin
    let kind = match &cred.conn {
        Some(c) => match c.kind.as_str() {
            "codex_subscription" => "auth_json",
            "openai_api" => "api_key",
            _ => return connector_provider(inv, c, &cred.secret),
        },
        None => cred.kind.as_str(),
    };
    match kind {
        "auth_json" => {
            inv.home_files
                .push((".codex/auth.json".into(), cred.secret.clone()));
            inv.writeback = Some(".codex/auth.json".into());
        }
        "api_key" => {
            inv.env.push(("OPENAI_API_KEY".into(), cred.secret.clone()));
            inv.env.push(("CODEX_API_KEY".into(), cred.secret.clone()));
            if let Some(c) = &cred.conn {
                if let Some(b) = c.get("base_url") {
                    inv.env.push(("OPENAI_BASE_URL".into(), b.into()));
                }
                if c.flex {
                    inv.home_files.push((
                        ".codex/config.toml".into(),
                        "service_tier = \"flex\"\n".into(),
                    ));
                }
            }
        }
        k => bail!("unsupported codex credential kind {k}"),
    }
    Ok(())
}

fn toml_str(s: &str) -> String {
    serde_json::to_string(s).expect("string serialises")
}

/// Custom Responses-API provider in the **user** `config.toml` (a project
/// config may not define providers). Keys `model_provider`,
/// `model_providers.*.{base_url,env_key,env_http_headers,wire_api}` exist in
/// the 0.167.0 binary; behaviour against Azure/LiteLLM is not verified.
fn connector_provider(inv: &mut Invocation, c: &ConnCfg, secret: &str) -> Result<()> {
    const KEY: &str = "QUEUE_LLM_API_KEY";
    let (name, base, auth) = match c.kind.as_str() {
        "azure_openai" => (
            "Azure OpenAI",
            c.get("base_url").map(String::from),
            format!("env_http_headers = {{ \"api-key\" = \"{KEY}\" }}"),
        ),
        "litellm" => (
            "LiteLLM",
            c.get("base_url")
                .map(|b| format!("{}/v1", b.trim_end_matches('/'))),
            format!("env_key = \"{KEY}\""),
        ),
        k => bail!("Zugang {k} kann nicht mit Codex betrieben werden"),
    };
    let Some(base) = base else {
        bail!("Basis-URL fehlt");
    };
    let mut cfg = format!(
        "model_provider = \"queue\"\n{}\n[model_providers.queue]\nname = {}\nbase_url = {}\nwire_api = \"responses\"\n{auth}\n",
        if c.flex { "service_tier = \"flex\"\n" } else { "" },
        toml_str(name),
        toml_str(&base),
    );
    cfg = cfg.replace("\n\n[", "\n[");
    inv.home_files.push((".codex/config.toml".into(), cfg));
    inv.env.push((KEY.into(), secret.to_string()));
    Ok(())
}

/// "Reconnecting... 2/5 (…)", "stream disconnected - retrying sampling request (1/5 …)",
/// "Reconnecting... waiting for network (…)".
fn retry_hint(msg: &str) -> Option<RetryInfo> {
    static N_OF: OnceLock<Regex> = OnceLock::new();
    let l = msg.to_lowercase();
    if !(l.starts_with("reconnecting") || l.contains("retrying")) {
        return None;
    }
    let re = N_OF.get_or_init(|| Regex::new(r"(\d+)/(\d+)").expect("static regex"));
    let (attempt, max) = re
        .captures(msg)
        .map(|c| (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0)))
        .unwrap_or((0, 0));
    Some(RetryInfo {
        category: ErrorCategory::Network,
        attempt,
        max,
        delay_ms: 0,
        status: None,
    })
}

pub(super) fn parse(v: &Value, st: &mut ParseState) -> Vec<AgentEvent> {
    let mut out = vec![];
    let typ = s(v, "type");
    match typ.as_str() {
        "item.started" | "item.completed" => {
            let item = &v["item"];
            let started = typ == "item.started";
            match item["type"].as_str() {
                Some("agent_message") if !started => {
                    st.last_text = s(item, "text");
                    out.push(AgentEvent::Text(trunc(&st.last_text, 2000)));
                }
                Some("command_execution") if started => out.push(AgentEvent::Tool(format!(
                    "$ {}",
                    trunc(&s(item, "command"), 200)
                ))),
                Some("file_change") if !started => {
                    let files: Vec<String> = item["changes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| s(c, "path"))
                        .collect();
                    out.push(AgentEvent::Tool(format!("edit {}", files.join(", "))));
                }
                Some("web_search") if !started => {
                    out.push(AgentEvent::Tool(format!("search: {}", s(item, "query"))))
                }
                // Non-fatal notices (documented as such; see recordings).
                Some("error") => out.push(AgentEvent::Text(format!(
                    "⚠ {}",
                    trunc(&s(item, "message"), 2000)
                ))),
                _ => {}
            }
        }
        "turn.completed" => {
            let u = &v["usage"];
            // Codex reports cached tokens as part of input_tokens.
            let cached = n(u, "cached_input_tokens");
            st.total.add(&Usage {
                input_tokens: (n(u, "input_tokens") - cached).max(0),
                output_tokens: n(u, "output_tokens"),
                cache_read_tokens: cached,
                ..Default::default()
            });
            out.push(AgentEvent::UsageTotal(st.total.clone()));
            let last = std::mem::take(&mut st.last_text);
            // With --output-schema the final message is the JSON contract.
            if let Ok(j @ Value::Object(_)) = serde_json::from_str::<Value>(last.trim()) {
                out.push(AgentEvent::Structured(j));
            }
            out.push(AgentEvent::Final(last));
        }
        "turn.failed" => out.push(error_event(&s(&v["error"], "message"))),
        "error" => {
            let msg = s(v, "message");
            out.push(match retry_hint(&msg) {
                Some(r) => AgentEvent::Retry(r),
                None => error_event(&msg),
            });
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{invocation as inv, parse_line, RunLimits};
    use super::*;

    #[test]
    fn invocation_uses_output_schema_file() {
        let c = Credential {
            kind: "api_key".into(),
            secret: "k".into(),
            conn: None,
        };
        let i = inv("codex", None, &c, &RunLimits::default(), RunKind::Agent).unwrap();
        assert_eq!(
            i.args,
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--dangerously-bypass-approvals-and-sandbox",
                "--output-schema",
                ".queue/result.schema.json",
                "-"
            ]
        );
        assert_eq!(
            i.queue_files,
            vec![("result.schema.json".to_string(), RESULT_SCHEMA.to_string())]
        );
        let r = inv(
            "codex",
            Some("gpt-x"),
            &c,
            &RunLimits::default(),
            RunKind::Review,
        )
        .unwrap();
        assert!(r.args.contains(&".queue/review.schema.json".to_string()));
        assert_eq!(r.queue_files[0].0, "review.schema.json");
        assert_eq!(r.args.last().unwrap(), "-");
        for (name, _) in i.queue_files.iter().chain(&r.queue_files) {
            assert!(queued_common::valid_queue_file_name(name));
        }
    }

    #[test]
    fn parses_codex_stream_with_structured_final_message() {
        let mut st = ParseState::default();
        parse_line(
            "codex",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"{\"status\":\"done\",\"summary\":\"s\"}"}}"#,
            &mut st,
        );
        let ev = parse_line(
            "codex",
            r#"{"type":"turn.completed","usage":{"input_tokens":7,"cached_input_tokens":3,"output_tokens":2}}"#,
            &mut st,
        );
        assert!(
            matches!(&ev[0], AgentEvent::UsageTotal(u) if u.input_tokens == 4 && u.cache_read_tokens == 3)
        );
        assert!(matches!(&ev[1], AgentEvent::Structured(v) if v["status"] == "done"));
        assert!(matches!(&ev[2], AgentEvent::Final(t) if t.contains("done")));
    }

    #[test]
    fn plain_final_message_is_not_structured() {
        let mut st = ParseState::default();
        parse_line(
            "codex",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
            &mut st,
        );
        let ev = parse_line(
            "codex",
            r#"{"type":"turn.completed","usage":{"input_tokens":7,"cached_input_tokens":3,"output_tokens":2}}"#,
            &mut st,
        );
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[1], AgentEvent::Final(t) if t == "done"));
    }

    #[test]
    fn codex_reconnect_is_retry() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "codex",
            r#"{"type":"error","message":"Reconnecting... 2/5 (stream disconnected before completion: failed to lookup address information: Name does not resolve)"}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Retry(r) if r.attempt == 2 && r.max == 5));
        let ev = parse_line(
            "codex",
            r#"{"type":"error","message":"Reconnecting... waiting for network (Connection failed: error sending request)"}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Retry(r) if r.attempt == 0));
        let ev = parse_line(
            "codex",
            r#"{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Falling back from WebSockets to HTTPS transport."}}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Text(t) if t.starts_with('⚠')));
    }

    #[test]
    fn turn_failed_and_usage_limit() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "codex",
            r#"{"type":"turn.failed","error":{"message":"You’ve hit your usage limit. Try again in 2 hours."}}"#,
            &mut st,
        );
        assert!(matches!(
            &ev[0],
            AgentEvent::Failure {
                category: ErrorCategory::RateLimit(Some(_)),
                ..
            }
        ));
        let ev = parse_line(
            "codex",
            r#"{"type":"turn.failed","error":{"message":"tool crashed"}}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Error(_)));
    }
}
