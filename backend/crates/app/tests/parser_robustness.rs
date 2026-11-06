//! WP01 tester: parsers against broken input – truncated lines (stream cut
//! mid-line, runner restart), non-object JSON, wrong field types, garbage
//! between valid lines. No database needed.
use std::path::PathBuf;

use queued::{
    agent::{parse_line, AgentEvent, ParseState, CLI_VERSIONS},
    models::FailureClass,
};

fn fixtures() -> Vec<(&'static str, PathBuf)> {
    let mut v = vec![];
    for &(p, ver) in CLI_VERSIONS.iter() {
        let d =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/cli/{p}-{ver}"));
        for e in std::fs::read_dir(&d).unwrap() {
            let path = e.unwrap().path();
            if path.extension().is_some_and(|x| x == "jsonl") {
                v.push((p, path));
            }
        }
    }
    assert!(v.len() >= 20, "{v:?}");
    v
}

fn replay(provider: &str, lines: &[&str]) -> Vec<AgentEvent> {
    let mut st = ParseState::default();
    lines
        .iter()
        .flat_map(|l| parse_line(provider, l, &mut st))
        .collect()
}

/// Every structural signal (usage, contract, failure, limit) – what must
/// never come out of a broken line.
fn signals(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::Text(_) | AgentEvent::Tool(_) | AgentEvent::Final(_)
            )
        })
        .map(|e| {
            // Relative resets ("try again in 2 hours") depend on now().
            let ts = regex::Regex::new(r"\d{4}-\d\d-\d\dT[0-9:.]+Z").unwrap();
            ts.replace_all(&format!("{e:?}"), "<ts>").into_owned()
        })
        .collect()
}

fn class(evs: &[AgentEvent]) -> Option<FailureClass> {
    evs.iter().find_map(|e| match e {
        AgentEvent::Failure { category, .. } => Some(category.class()),
        _ => None,
    })
}

#[test]
fn truncated_lines_never_panic_and_never_yield_signals() {
    for (provider, path) in fixtures() {
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines().filter(|l| l.trim_start().starts_with('{')) {
            let cuts: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
            for &cut in cuts.iter().skip(1) {
                let part = &line[..cut];
                let evs = parse_line(provider, part, &mut ParseState::default());
                assert!(
                    signals(&evs).is_empty(),
                    "{path:?}: truncated line produced {:?} from {part}",
                    signals(&evs)
                );
                for e in &evs {
                    if let AgentEvent::Text(t) = e {
                        assert!(t.chars().count() <= 2001);
                    }
                }
            }
        }
    }
}

#[test]
fn garbage_between_lines_does_not_change_the_outcome() {
    let junk = [
        "",
        "   ",
        "not json at all",
        "{",
        "}",
        "null",
        "42",
        "\"just a string\"",
        "[1,2,3]",
        "{\"type\":5}",
        "{\"type\":null,\"message\":{}}",
        "\u{feff}{\"type\":\"bom\"}",
    ];
    for (provider, path) in fixtures() {
        let text = std::fs::read_to_string(&path).unwrap();
        let clean: Vec<&str> = text.lines().collect();
        let mut dirty = vec![];
        for (i, l) in clean.iter().enumerate() {
            dirty.push(junk[i % junk.len()]);
            dirty.push(*l);
        }
        dirty.extend(junk);
        let (a, b) = (replay(provider, &clean), replay(provider, &dirty));
        assert_eq!(signals(&a), signals(&b), "{path:?}");
        assert_eq!(class(&a), class(&b), "{path:?}");
    }
}

#[test]
fn wrong_field_types_never_panic() {
    let lines = [
        r#"{"type":"result","modelUsage":[],"usage":"x","total_cost_usd":"1"}"#,
        r#"{"type":"result","modelUsage":{"m":"str"},"is_error":"yes"}"#,
        r#"{"type":"result","subtype":7,"structured_output":null,"result":{}}"#,
        r#"{"type":"assistant","message":{"id":7,"usage":[],"content":"x"}}"#,
        r#"{"type":"assistant","message":{"id":"a","usage":{"input_tokens":"9"},"content":[{"type":"tool_use","input":null}]},"error":7}"#,
        r#"{"type":"system","subtype":"api_retry","attempt":-1,"max_retries":"5","retry_delay_ms":1e30,"error_status":99999999}"#,
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":"garbage"}}"#,
        r#"{"type":"rate_limit_event","rate_limit_info":"rejected"}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":99}}"#,
        r#"{"type":"turn.failed","error":"plain string"}"#,
        r#"{"type":"error","message":null}"#,
        r#"{"type":"error","message":"Reconnecting... 99999999999/5"}"#,
        r#"{"type":"item.completed","item":{"type":"file_change","changes":"x"}}"#,
        r#"{"type":"result","status":"error","error":"x","stats":[]}"#,
        r#"{"type":"tool_use","parameters":"x"}"#,
        r#"{"type":"usage","input_tokens":1.5,"cost_usd":"x"}"#,
        r#"{"type":"limit","kind":null}"#,
        r#"{"type":"retry","status":-4}"#,
    ];
    for p in ["claude", "codex", "gemini", "mock", "unknown"] {
        let mut st = ParseState::default();
        for l in lines {
            let _ = parse_line(p, l, &mut st);
        }
    }
    // Usage never goes negative from cached > input (codex).
    let evs = replay(
        "codex",
        &[
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":99,"output_tokens":2}}"#,
        ],
    );
    let AgentEvent::UsageTotal(u) = &evs[0] else {
        panic!("{evs:?}")
    };
    assert_eq!((u.input_tokens, u.cache_read_tokens), (0, 99));
}

/// Stream cut before Claude's final `result` line: the live deltas remain the
/// only usage (no fake total), no contract and no failure is invented.
#[test]
fn claude_stream_cut_before_result_keeps_deltas_only() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cli/claude-2.1.319/success_structured.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let last = lines
        .iter()
        .rposition(|l| l.contains("\"type\":\"result\""))
        .unwrap();
    let mut cut: Vec<&str> = lines[..last].to_vec();
    let half = &lines[last][..lines[last].len() / 2];
    cut.push(half);
    let evs = replay("claude", &cut);
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::UsageDelta(_))));
    assert!(!evs.iter().any(|e| matches!(
        e,
        AgentEvent::UsageTotal(_)
            | AgentEvent::Structured(_)
            | AgentEvent::Failure { .. }
            | AgentEvent::Error(_)
    )));
}

/// The real `rate_limit_event` (allowed, overage rejected) is never a limit;
/// only `status:"rejected"` is – also with unknown extra fields.
#[test]
fn rate_limit_event_status_only() {
    let ok = [
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","overageStatus":"rejected","isUsingOverage":false}}"#,
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","overageStatus":"rejected","utilization":0.95}}"#,
        r#"{"type":"rate_limit_event","rate_limit_info":{"overageStatus":"rejected"}}"#,
        r#"{"type":"rate_limit_event","overageStatus":"rejected","status":"rejected"}"#,
    ];
    for l in ok {
        assert!(signals(&replay("claude", &[l])).is_empty(), "{l}");
    }
    let rej = replay(
        "claude",
        &[
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1900000000,"rateLimitType":"five_hour"}}"#,
        ],
    );
    assert_eq!(class(&rej), Some(FailureClass::RateLimit));
}

/// Agent text / tool output mentioning limits, auth or 5xx never classifies
/// (false-positive rate limits pause the whole provider).
#[test]
fn agent_text_and_tool_output_never_classify() {
    let text = [
        r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"HTTP 429 Too Many Requests: rate limit exceeded, quota exceeded, usage limit reached|1900000000, 401 Unauthorized, invalid api key, 503 Service Unavailable, overloaded"}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"npm ERR! 429 rate limit / 401 unauthorized / overloaded"}]}}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Fixed the rate limit handling (HTTP 429) and the 401 path; server was overloaded","usage":{"input_tokens":1,"output_tokens":1}}"#,
    ];
    let evs = replay("claude", &text);
    assert!(class(&evs).is_none(), "{evs:?}");
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Error(_))));
    let codex = [
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"you've hit your usage limit? no: rate limit 429 handled"}}"#,
        r#"{"type":"item.completed","item":{"type":"command_execution","command":"curl","aggregated_output":"HTTP/1.1 429 Too Many Requests\nunauthorized","exit_code":22}}"#,
        r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":2}}"#,
    ];
    assert!(class(&replay("codex", &codex)).is_none());
    let gemini = [
        r#"{"type":"message","role":"assistant","content":"quota exceeded? rate limit 429, 401 unauthorized","delta":true}"#,
        r#"{"type":"tool_result","tool_id":"t1","status":"error","output":"Error: 429 Too Many Requests / API key not valid"}"#,
        r#"{"type":"result","status":"success","stats":{"input_tokens":5,"output_tokens":1}}"#,
    ];
    assert!(class(&replay("gemini", &gemini)).is_none());
}
