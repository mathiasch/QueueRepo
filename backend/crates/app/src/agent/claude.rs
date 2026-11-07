//! Claude Code (pinned 2.1.319): `-p --output-format stream-json --verbose`.
//!
//! Structured signals used (all present in the 2.1.319 SDK schema and, where
//! marked, verified with real recordings in `tests/fixtures/cli`):
//! * `result.structured_output` with `--json-schema` (recorded)
//! * `result.subtype` `error_max_turns` / `error_max_budget_usd` (recorded)
//! * `result.modelUsage` (recorded), `assistant.message.{id,usage}` (recorded)
//! * `system/api_retry` {attempt,max_retries,retry_delay_ms,error_status,error}
//! * top-level `assistant.error` (same enum)
//! * `rate_limit_event.rate_limit_info.status` (recorded: `allowed`)
use anyhow::{bail, Result};
use serde_json::Value;

use super::{
    classify::{epoch_value, ErrorCategory},
    count, n, s, tool_summary, trunc, usd, AgentEvent, Credential, Invocation, LimitKind, ParseState,
    RetryInfo, RunKind, RunLimits,
};
use crate::models::{Usage, RESULT_SCHEMA, REVIEW_SCHEMA};

pub(super) fn invocation(
    inv: &mut Invocation,
    model: Option<&str>,
    cred: &Credential,
    limits: &RunLimits,
    kind: RunKind,
) -> Result<()> {
    inv.program = "claude".into();
    inv.args = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "bypassPermissions",
        "--permission-prompts",
        "none",
        "--json-schema",
        match kind {
            RunKind::Agent => RESULT_SCHEMA,
            RunKind::Review => REVIEW_SCHEMA,
        },
    ]
    .map(String::from)
    .to_vec();
    if let Some(t) = limits.turns {
        inv.args.extend(["--max-turns".into(), t.to_string()]);
    }
    if let Some(c) = limits.cost_usd {
        inv.args
            .extend(["--max-budget-usd".into(), format!("{:.4}", c.max(0.0001))]);
    }
    if let Some(m) = model {
        inv.args.extend(["--model".into(), m.into()]);
    }
    match cred.kind.as_str() {
        "oauth_token" => inv
            .env
            .push(("CLAUDE_CODE_OAUTH_TOKEN".into(), cred.secret.clone())),
        "api_key" => inv
            .env
            .push(("ANTHROPIC_API_KEY".into(), cred.secret.clone())),
        k => bail!("unsupported claude credential kind {k}"),
    }
    inv.env.push(("DISABLE_AUTOUPDATER".into(), "1".into()));
    Ok(())
}

fn usage_of(u: &Value) -> Usage {
    Usage {
        input_tokens: n(u, "input_tokens"),
        output_tokens: n(u, "output_tokens"),
        cache_read_tokens: n(u, "cache_read_input_tokens"),
        cache_write_tokens: n(u, "cache_creation_input_tokens"),
        cost_usd: 0.0,
    }
}

/// `modelUsage` includes subagents (`usage` does not); sum over models.
fn model_usage(m: &Value) -> Option<Usage> {
    let m = m.as_object().filter(|m| !m.is_empty())?;
    let mut u = Usage::default();
    for v in m.values() {
        u.add(&Usage {
            input_tokens: n(v, "inputTokens"),
            output_tokens: n(v, "outputTokens"),
            cache_read_tokens: n(v, "cacheReadInputTokens"),
            cache_write_tokens: n(v, "cacheCreationInputTokens"),
            cost_usd: usd(&v["costUSD"]),
        });
    }
    Some(u)
}

fn status_of(v: &Value) -> Option<u16> {
    v.as_u64().and_then(|x| u16::try_from(x).ok())
}

pub(super) fn parse(v: &Value, st: &mut ParseState) -> Vec<AgentEvent> {
    let mut out = vec![];
    match s(v, "type").as_str() {
        "assistant" => {
            for c in v["message"]["content"].as_array().into_iter().flatten() {
                match c["type"].as_str() {
                    Some("text") => out.push(AgentEvent::Text(trunc(&s(c, "text"), 2000))),
                    Some("tool_use") => {
                        out.push(AgentEvent::Tool(tool_summary(&s(c, "name"), &c["input"])))
                    }
                    _ => {}
                }
            }
            let id = s(&v["message"], "id");
            if !id.is_empty() && v["message"]["usage"].is_object() {
                let now = usage_of(&v["message"]["usage"]);
                let prev = st.seen_usage.get(&id).cloned().unwrap_or_default();
                let d = Usage {
                    input_tokens: (now.input_tokens - prev.input_tokens).max(0),
                    output_tokens: (now.output_tokens - prev.output_tokens).max(0),
                    cache_read_tokens: (now.cache_read_tokens - prev.cache_read_tokens).max(0),
                    cache_write_tokens: (now.cache_write_tokens - prev.cache_write_tokens).max(0),
                    cost_usd: 0.0,
                };
                let merged = Usage {
                    input_tokens: now.input_tokens.max(prev.input_tokens),
                    output_tokens: now.output_tokens.max(prev.output_tokens),
                    cache_read_tokens: now.cache_read_tokens.max(prev.cache_read_tokens),
                    cache_write_tokens: now.cache_write_tokens.max(prev.cache_write_tokens),
                    cost_usd: 0.0,
                };
                st.seen_usage.insert(id, merged);
                if d.input_tokens + d.output_tokens + d.cache_read_tokens + d.cache_write_tokens > 0
                {
                    out.push(AgentEvent::UsageDelta(d));
                }
            }
            match v["error"].as_str() {
                Some(e) => st.last_error = Some(ErrorCategory::from_claude(e, None)),
                // A successful answer: an earlier retry was not the cause of anything.
                None => {
                    st.last_error = None;
                    st.last_retry = None;
                }
            }
        }
        "system" if s(v, "subtype") == "api_retry" => {
            let status = status_of(&v["error_status"]);
            let category = ErrorCategory::from_claude(&s(v, "error"), status);
            st.last_retry = Some(category.clone());
            out.push(AgentEvent::Retry(RetryInfo {
                category,
                attempt: count(n(v, "attempt")),
                max: count(n(v, "max_retries")),
                delay_ms: n(v, "retry_delay_ms") as u64,
                status,
            }));
        }
        "rate_limit_event" => {
            let info = &v["rate_limit_info"];
            // Only the request status counts; `overageStatus:"rejected"` is
            // normal for subscriptions without overage (seen in recordings).
            if info["status"].as_str() == Some("rejected") {
                let kind = s(info, "rateLimitType");
                out.push(AgentEvent::Failure {
                    category: ErrorCategory::RateLimit(epoch_value(&info["resetsAt"])),
                    message: format!(
                        "Abo-Limit erreicht{}",
                        if kind.is_empty() {
                            String::new()
                        } else {
                            format!(" ({kind})")
                        }
                    ),
                });
            }
        }
        "result" => {
            let mut u = model_usage(&v["modelUsage"]).unwrap_or_else(|| usage_of(&v["usage"]));
            if v["total_cost_usd"].is_number() {
                u.cost_usd = usd(&v["total_cost_usd"]);
            }
            out.push(AgentEvent::UsageTotal(u));
            if !v["structured_output"].is_null() {
                out.push(AgentEvent::Structured(v["structured_output"].clone()));
            }
            let subtype = s(v, "subtype");
            let text = match v["result"].as_str() {
                Some(r) if !r.is_empty() => r.to_string(),
                _ => v["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            match subtype.as_str() {
                "error_max_turns" => out.push(AgentEvent::LimitHit(LimitKind::Turns)),
                "error_max_budget_usd" => out.push(AgentEvent::LimitHit(LimitKind::Cost)),
                "error_max_structured_output_retries" => out.push(AgentEvent::Error(format!(
                    "Claude lieferte kein gültiges strukturiertes Ergebnis (error_max_structured_output_retries). {}",
                    trunc(&text, 2000)
                ))),
                _ if v["is_error"].as_bool().unwrap_or(false) || subtype.starts_with("error") => {
                    let category = st
                        .last_error
                        .clone()
                        .or_else(|| st.last_retry.clone())
                        .or_else(|| {
                            status_of(&v["api_error_status"])
                                .map(|s| ErrorCategory::Unknown(Some(s)))
                        })
                        .or_else(|| super::classify_text(&text));
                    out.push(match category {
                        Some(category) => AgentEvent::Failure {
                            category,
                            message: trunc(&text, 4000),
                        },
                        None => AgentEvent::Error(trunc(
                            &format!("Claude-Lauf fehlgeschlagen ({subtype}): {text}"),
                            4000,
                        )),
                    });
                }
                _ => out.push(AgentEvent::Final(text)),
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{invocation as inv, parse_line};
    use super::*;

    fn p(st: &mut ParseState, line: &str) -> Vec<AgentEvent> {
        parse_line("claude", line, st)
    }

    #[test]
    fn invocation_flags_with_and_without_limits() {
        let c = Credential {
            kind: "api_key".into(),
            secret: "sk-secret".into(),
        };
        let i = inv("claude", None, &c, &RunLimits::default(), RunKind::Agent).unwrap();
        let has = |a: &[String], w: &[&str]| a.windows(w.len()).any(|x| x == w);
        for w in [
            &["-p"][..],
            &["--output-format", "stream-json"],
            &["--verbose"],
            &["--permission-mode", "bypassPermissions"],
            &["--permission-prompts", "none"],
            &["--json-schema", RESULT_SCHEMA],
        ] {
            assert!(has(&i.args, w), "missing {w:?}: {:?}", i.args);
        }
        assert!(!i.args.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(!i
            .args
            .iter()
            .any(|a| a == "--max-turns" || a == "--max-budget-usd"));
        assert!(!i.args.iter().any(|a| a.contains("sk-secret")));
        assert!(i
            .env
            .contains(&("ANTHROPIC_API_KEY".into(), "sk-secret".into())));

        let lim = RunLimits {
            turns: Some(12),
            cost_usd: Some(0.123456),
        };
        let i = inv("claude", Some("opus"), &c, &lim, RunKind::Review).unwrap();
        assert!(has(&i.args, &["--json-schema", REVIEW_SCHEMA]));
        assert!(has(&i.args, &["--max-turns", "12"]));
        assert!(has(&i.args, &["--max-budget-usd", "0.1235"]));
        assert!(has(&i.args, &["--model", "opus"]));
    }

    #[test]
    fn parses_claude_result() {
        let mut st = ParseState::default();
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.12,"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}"#,
        );
        assert!(
            matches!(&ev[0], AgentEvent::UsageTotal(u) if u.cache_read_tokens == 100 && u.cost_usd > 0.1)
        );
        assert!(matches!(&ev[1], AgentEvent::Final(t) if t == "ok"));
    }

    #[test]
    fn claude_error_result_with_limit_is_rate_limited() {
        let mut st = ParseState::default();
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached|1900000000","usage":{}}"#,
        );
        assert!(matches!(
            &ev[1],
            AgentEvent::Failure { category: ErrorCategory::RateLimit(Some(t)), .. } if t.timestamp() == 1_900_000_000
        ));
    }

    #[test]
    fn claude_rate_limit_event_with_reset() {
        let mut st = ParseState::default();
        let ev = p(
            &mut st,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1900000000}}"#,
        );
        assert!(matches!(
            &ev[0],
            AgentEvent::Failure { category: ErrorCategory::RateLimit(Some(t)), .. } if t.timestamp() == 1_900_000_000
        ));
        let ok = p(
            &mut st,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1900000000}}"#,
        );
        assert!(ok.is_empty());
        // Real 2.1.319 event: overage rejected, request allowed → nothing.
        let real = p(
            &mut st,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1794013800,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false}}"#,
        );
        assert!(real.is_empty(), "{real:?}");
    }

    #[test]
    fn claude_tool_use_is_summarized() {
        let mut st = ParseState::default();
        let ev = p(
            &mut st,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}},{"type":"text","text":"ok"}]}}"#,
        );
        assert!(matches!(&ev[0], AgentEvent::Tool(t) if t == "Bash: cargo test"));
        assert!(matches!(&ev[1], AgentEvent::Text(t) if t == "ok"));
    }

    #[test]
    fn claude_usage_dedupes_message_ids() {
        let mut st = ParseState::default();
        let line = |id: &str, out: i64| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"{id}","content":[],"usage":{{"input_tokens":10,"output_tokens":{out},"cache_read_input_tokens":0,"cache_creation_input_tokens":100}}}}}}"#
            )
        };
        let d1 = p(&mut st, &line("m1", 5));
        assert_eq!(
            d1,
            vec![AgentEvent::UsageDelta(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_write_tokens: 100,
                ..Default::default()
            })]
        );
        assert!(
            p(&mut st, &line("m1", 5)).is_empty(),
            "same id, same usage → no delta"
        );
        let d3 = p(&mut st, &line("m1", 9));
        assert_eq!(
            d3,
            vec![AgentEvent::UsageDelta(Usage {
                output_tokens: 4,
                ..Default::default()
            })]
        );
        assert_eq!(p(&mut st, &line("m2", 1)).len(), 1);
    }

    #[test]
    fn claude_model_usage_includes_subagents() {
        let mut st = ParseState::default();
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"x","total_cost_usd":0.5,"usage":{"input_tokens":1,"output_tokens":1},"modelUsage":{"a":{"inputTokens":10,"outputTokens":20,"cacheReadInputTokens":30,"cacheCreationInputTokens":40,"costUSD":0.2},"b":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4,"costUSD":0.3}}}"#,
        );
        assert_eq!(
            ev[0],
            AgentEvent::UsageTotal(Usage {
                input_tokens: 11,
                output_tokens: 22,
                cache_read_tokens: 33,
                cache_write_tokens: 44,
                cost_usd: 0.5,
            })
        );
    }

    #[test]
    fn api_retry_then_error_result_is_categorised_and_success_clears_retry() {
        let mut st = ParseState::default();
        let r = p(
            &mut st,
            r#"{"type":"system","subtype":"api_retry","attempt":2,"max_retries":10,"retry_delay_ms":4000,"error_status":529,"error":"overloaded"}"#,
        );
        assert_eq!(
            r,
            vec![AgentEvent::Retry(RetryInfo {
                category: ErrorCategory::Overloaded,
                attempt: 2,
                max: 10,
                delay_ms: 4000,
                status: Some(529)
            })]
        );
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["API Error: Overloaded"],"usage":{}}"#,
        );
        assert!(matches!(
            &ev[1],
            AgentEvent::Failure {
                category: ErrorCategory::Overloaded,
                ..
            }
        ));

        let mut st = ParseState::default();
        p(
            &mut st,
            r#"{"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":529,"error":"overloaded"}"#,
        );
        p(
            &mut st,
            r#"{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"hi"}]}}"#,
        );
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["[ede_diagnostic] result_type=user"],"usage":{}}"#,
        );
        assert!(matches!(&ev[1], AgentEvent::Error(_)), "{ev:?}");
    }

    #[test]
    fn assistant_error_field_wins() {
        let mut st = ParseState::default();
        p(
            &mut st,
            r#"{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"Invalid API key · Please run /login"}]},"error":"authentication_failed"}"#,
        );
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login","usage":{}}"#,
        );
        assert!(matches!(
            &ev[1],
            AgentEvent::Failure {
                category: ErrorCategory::Auth,
                ..
            }
        ));
    }

    #[test]
    fn limits_and_structured_output() {
        let mut st = ParseState::default();
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"errors":["Reached maximum number of turns (1)"],"usage":{}}"#,
        );
        assert_eq!(ev[1], AgentEvent::LimitHit(LimitKind::Turns));
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"errors":["Reached maximum budget ($0.0001)"],"usage":{}}"#,
        );
        assert_eq!(ev[1], AgentEvent::LimitHit(LimitKind::Cost));
        let ev = p(
            &mut st,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"{}","structured_output":{"status":"done"},"usage":{}}"#,
        );
        assert_eq!(
            ev[1],
            AgentEvent::Structured(serde_json::json!({"status":"done"}))
        );
        assert!(matches!(&ev[2], AgentEvent::Final(_)));
    }
}
