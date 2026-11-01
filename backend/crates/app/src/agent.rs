//! Provider adapters: build the CLI invocation and parse its JSON stream.
//!
//! Subscriptions are used through the official CLIs only:
//!   claude  – Claude Code  (`claude setup-token` → CLAUDE_CODE_OAUTH_TOKEN, or ANTHROPIC_API_KEY)
//!   codex   – Codex CLI    (~/.codex/auth.json from `codex login`, or OPENAI_API_KEY)
//!   gemini  – Gemini CLI   (~/.gemini/oauth_creds.json from Google login, or GEMINI_API_KEY)
use anyhow::{bail, Result};
use chrono::{DateTime, Duration, TimeZone, Utc};
use regex::Regex;
use serde_json::Value;

use crate::models::Usage;

pub struct Credential {
    pub kind: String,
    pub secret: String,
}

pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Files written into the run's $HOME (path relative to HOME, content).
    pub home_files: Vec<(String, String)>,
    /// Home-relative file to read back after the run (refreshed OAuth tokens).
    pub writeback: Option<String>,
}

#[derive(Debug)]
pub enum AgentEvent {
    Text(String),
    Tool(String),
    Usage(Usage),
    Final(String),
    Error(String),
    RateLimited(Option<DateTime<Utc>>),
}

pub fn credential_kinds(provider: &str) -> &'static [&'static str] {
    match provider {
        "claude" => &["oauth_token", "api_key"],
        "codex" => &["auth_json", "api_key"],
        "gemini" => &["oauth_creds", "api_key"],
        "mock" => &["none"],
        _ => &[],
    }
}

pub fn invocation(provider: &str, model: Option<&str>, cred: &Credential) -> Result<Invocation> {
    let mut inv = Invocation {
        program: String::new(),
        args: vec![],
        env: vec![],
        home_files: vec![],
        writeback: None,
    };
    match provider {
        "claude" => {
            inv.program = "claude".into();
            inv.args = [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--dangerously-skip-permissions",
            ]
            .map(String::from)
            .to_vec();
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
        }
        "codex" => {
            inv.program = "codex".into();
            inv.args = [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
            .map(String::from)
            .to_vec();
            if let Some(m) = model {
                inv.args.extend(["--model".into(), m.into()]);
            }
            inv.args.push("-".into()); // prompt from stdin
            match cred.kind.as_str() {
                "auth_json" => {
                    inv.home_files
                        .push((".codex/auth.json".into(), cred.secret.clone()));
                    inv.writeback = Some(".codex/auth.json".into());
                }
                "api_key" => {
                    inv.env.push(("OPENAI_API_KEY".into(), cred.secret.clone()));
                    inv.env.push(("CODEX_API_KEY".into(), cred.secret.clone()));
                }
                k => bail!("unsupported codex credential kind {k}"),
            }
        }
        "gemini" => {
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
            match cred.kind.as_str() {
                "oauth_creds" => {
                    inv.home_files
                        .push((".gemini/oauth_creds.json".into(), cred.secret.clone()));
                    inv.home_files.push((
                        ".gemini/settings.json".into(),
                        r#"{"selectedAuthType":"oauth-personal","security":{"auth":{"selectedType":"oauth-personal"}}}"#.into(),
                    ));
                    inv.writeback = Some(".gemini/oauth_creds.json".into());
                }
                "api_key" => inv.env.push(("GEMINI_API_KEY".into(), cred.secret.clone())),
                k => bail!("unsupported gemini credential kind {k}"),
            }
        }
        "mock" => {
            inv.program = "sh".into();
            inv.args = vec!["-c".into(), MOCK_SCRIPT.into()];
        }
        p => bail!("unknown provider {p}"),
    }
    Ok(inv)
}

/// Deterministic fake agent for development/tests (ENABLE_MOCK_PROVIDER=true).
/// Prompt keywords: MOCK_FAIL, MOCK_ASK, MOCK_FOLLOWUPS, MOCK_LIMIT, MOCK_SLOW.
const MOCK_SCRIPT: &str = r#"P=$(cat); mkdir -p .queue
echo '{"type":"text","text":"mock agent: reading task"}'
echo '{"type":"tool","text":"edit MOCK_LOG.md"}'
echo "run $(date -u +%FT%TZ)" >> MOCK_LOG.md
echo '{"approved":true,"issues":[]}' > .queue/review.json
echo '{"type":"usage","input_tokens":1200,"output_tokens":150,"cache_read_tokens":800}'
case "$P" in
  *"strict code reviewer"*) echo '{"type":"final","text":"review ok"}'; exit 0;;
esac
case "$P" in
  *MOCK_SLOW*) echo '{"type":"text","text":"working slowly"}'; sleep 30;;
esac
case "$P" in
  *MOCK_LIMIT*) echo '{"type":"error","text":"Claude AI usage limit reached|'$(( $(date +%s) + 120 ))'"}'; exit 1;;
  *MOCK_FAIL*) echo '{"status":"failed","summary":"mock failure","handoff":"tried X, failed"}' > .queue/result.json;;
  *MOCK_ASK*) case "$P" in *"Answers from user"*) echo '{"status":"done","summary":"answered","handoff":"used answer"}' > .queue/result.json;;
              *) echo '{"status":"needs_input","summary":"need decision","questions":["Postgres or SQLite?"]}' > .queue/result.json;; esac;;
  *MOCK_FOLLOWUPS*) echo '{"status":"done","summary":"planned","handoff":"plan ready","follow_up_tasks":[{"title":"A","prompt":"do A"},{"title":"B","prompt":"do B"},{"title":"C","prompt":"do C"}]}' > .queue/result.json;;
  *) echo '{"status":"done","summary":"mock done","handoff":"mock handoff"}' > .queue/result.json;;
esac
echo '{"type":"final","text":"finished"}'"#;

/// Per-run parser state (some CLIs stream deltas).
#[derive(Default)]
pub struct ParseState {
    buf: String,
    last_text: String,
}

fn trunc(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn tool_summary(name: &str, input: &Value) -> String {
    let detail = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
    .map(String::from)
    .unwrap_or_else(|| input.to_string());
    format!("{name}: {}", trunc(&detail, 200))
}

pub fn parse_line(provider: &str, line: &str, st: &mut ParseState) -> Vec<AgentEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return if line.trim().is_empty() {
            vec![]
        } else {
            vec![AgentEvent::Text(trunc(line, 2000))]
        };
    };
    let s = |v: &Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let n = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let typ = s(&v, "type");
    let mut out = vec![];
    match provider {
        "claude" => match typ.as_str() {
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
            }
            "result" => {
                let u = &v["usage"];
                out.push(AgentEvent::Usage(Usage {
                    input_tokens: n(u, "input_tokens"),
                    output_tokens: n(u, "output_tokens"),
                    cache_read_tokens: n(u, "cache_read_input_tokens"),
                    cache_write_tokens: n(u, "cache_creation_input_tokens"),
                    cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
                }));
                let text = s(&v, "result");
                if v["is_error"].as_bool().unwrap_or(false) || s(&v, "subtype").starts_with("error")
                {
                    out.push(classify_error(&text));
                } else {
                    out.push(AgentEvent::Final(text));
                }
            }
            "rate_limit_event" => {
                let blob = v.to_string().to_lowercase();
                if blob.contains("reject") || blob.contains("exceeded") {
                    out.push(AgentEvent::RateLimited(find_reset_in_json(&v)));
                }
            }
            _ => {}
        },
        "codex" => match typ.as_str() {
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
                    Some("error") => out.push(AgentEvent::Error(s(item, "message"))),
                    _ => {}
                }
            }
            "turn.completed" => {
                let u = &v["usage"];
                // Codex reports cached tokens as part of input_tokens.
                let cached = n(u, "cached_input_tokens");
                out.push(AgentEvent::Usage(Usage {
                    input_tokens: (n(u, "input_tokens") - cached).max(0),
                    output_tokens: n(u, "output_tokens"),
                    cache_read_tokens: cached,
                    ..Default::default()
                }));
                out.push(AgentEvent::Final(std::mem::take(&mut st.last_text)));
            }
            "turn.failed" => out.push(classify_error(&s(&v["error"], "message"))),
            "error" => out.push(classify_error(&s(&v, "message"))),
            _ => {}
        },
        "gemini" => match typ.as_str() {
            "message" if s(&v, "role") == "assistant" => st.buf.push_str(&s(&v, "content")),
            "tool_use" => {
                flush(st, &mut out);
                out.push(AgentEvent::Tool(tool_summary(
                    &s(&v, "tool_name"),
                    &v["parameters"],
                )));
            }
            "result" => {
                flush(st, &mut out);
                let u = &v["stats"];
                let cached = n(u, "cached");
                out.push(AgentEvent::Usage(Usage {
                    input_tokens: (n(u, "input_tokens") - cached).max(0),
                    output_tokens: n(u, "output_tokens"),
                    cache_read_tokens: cached,
                    ..Default::default()
                }));
                if s(&v, "status") == "error" {
                    out.push(classify_error(&s(&v["error"], "message")));
                } else {
                    out.push(AgentEvent::Final(std::mem::take(&mut st.last_text)));
                }
            }
            "error" => out.push(classify_error(&s(&v, "message"))),
            _ => {}
        },
        "mock" => match typ.as_str() {
            "text" => out.push(AgentEvent::Text(s(&v, "text"))),
            "tool" => out.push(AgentEvent::Tool(s(&v, "text"))),
            "final" => out.push(AgentEvent::Final(s(&v, "text"))),
            "error" => out.push(classify_error(&s(&v, "text"))),
            "usage" => out.push(AgentEvent::Usage(Usage {
                input_tokens: n(&v, "input_tokens"),
                output_tokens: n(&v, "output_tokens"),
                cache_read_tokens: n(&v, "cache_read_tokens"),
                ..Default::default()
            })),
            _ => {}
        },
        _ => {}
    }
    out
}

fn flush(st: &mut ParseState, out: &mut Vec<AgentEvent>) {
    if !st.buf.trim().is_empty() {
        st.last_text = std::mem::take(&mut st.buf);
        out.push(AgentEvent::Text(trunc(&st.last_text, 2000)));
    }
}

pub fn is_rate_limit_text(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "usage limit",
        "rate limit",
        "rate_limit",
        "limit reached",
        "quota",
        "resource_exhausted",
        "too many requests",
        "hit your limit",
        "429",
    ]
    .iter()
    .any(|k| t.contains(k))
}

/// Errors from the CLI: either a (subscription) rate limit or a real failure.
pub fn classify_error(text: &str) -> AgentEvent {
    if is_rate_limit_text(text) {
        AgentEvent::RateLimited(parse_reset(text))
    } else {
        AgentEvent::Error(trunc(text, 4000))
    }
}

/// Extracts a reset timestamp from typical limit messages.
pub fn parse_reset(text: &str) -> Option<DateTime<Utc>> {
    // "…limit reached|1730000000"
    if let Some(c) = Regex::new(r"\|(\d{10})\b").unwrap().captures(text) {
        return Utc.timestamp_opt(c[1].parse().ok()?, 0).single();
    }
    // "try again in 2 hours 5 minutes" / "in 3 days"
    let re = Regex::new(r"(?i)(?:try again|resets?|retry)\s+in\s+((?:\d+\s*(?:days?|d|hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s)[\s,and]*)+)").unwrap();
    if let Some(c) = re.captures(text) {
        let mut d = Duration::zero();
        for p in Regex::new(r"(?i)(\d+)\s*([a-z]+)")
            .unwrap()
            .captures_iter(&c[1])
        {
            let v: i64 = p[1].parse().unwrap_or(0);
            d += match p[2].chars().next().unwrap_or('m') {
                'd' => Duration::days(v),
                'h' => Duration::hours(v),
                's' => Duration::seconds(v),
                _ => Duration::minutes(v),
            };
        }
        if d > Duration::zero() {
            return Some(Utc::now() + d);
        }
    }
    None
}

fn find_reset_in_json(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if k.to_lowercase().contains("reset") {
                    if let Some(n) = val
                        .as_i64()
                        .or_else(|| val.as_str().and_then(|s| s.parse().ok()))
                    {
                        let secs = if n > 10_000_000_000 { n / 1000 } else { n };
                        if let Some(t) = Utc.timestamp_opt(secs, 0).single() {
                            return Some(t);
                        }
                    }
                    if let Some(t) = val
                        .as_str()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    {
                        return Some(t.with_timezone(&Utc));
                    }
                }
                if let Some(t) = find_reset_in_json(val) {
                    return Some(t);
                }
            }
            None
        }
        Value::Array(a) => a.iter().find_map(find_reset_in_json),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_reset() {
        let t = parse_reset("Claude AI usage limit reached|1900000000").unwrap();
        assert_eq!(t.timestamp(), 1_900_000_000);
    }

    #[test]
    fn parses_relative_reset() {
        let t =
            parse_reset("You've hit your usage limit. Try again in 2 hours 30 minutes.").unwrap();
        let d = t - Utc::now();
        assert!(d > Duration::minutes(149) && d <= Duration::minutes(150));
    }

    #[test]
    fn classifies_errors() {
        assert!(matches!(
            classify_error("Quota exceeded for model"),
            AgentEvent::RateLimited(_)
        ));
        assert!(matches!(
            classify_error("compilation failed"),
            AgentEvent::Error(_)
        ));
    }

    #[test]
    fn parses_claude_result() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "claude",
            r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","total_cost_usd":0.12,"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}"#,
            &mut st,
        );
        assert!(
            matches!(&ev[0], AgentEvent::Usage(u) if u.cache_read_tokens == 100 && u.cost_usd > 0.1)
        );
        assert!(matches!(&ev[1], AgentEvent::Final(t) if t == "ok"));
    }

    #[test]
    fn parses_codex_stream() {
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
        assert!(matches!(&ev[1], AgentEvent::Final(t) if t == "done"));
        assert!(
            matches!(&ev[0], AgentEvent::Usage(u) if u.input_tokens == 4 && u.cache_read_tokens == 3)
        );
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
            matches!(&ev[1], AgentEvent::Usage(u) if u.input_tokens == 30 && u.output_tokens == 7)
        );
        assert!(matches!(&ev[2], AgentEvent::Final(t) if t == "Hello"));
    }

    #[test]
    fn claude_error_result_with_limit_is_rate_limited() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "claude",
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached|1900000000","usage":{}}"#,
            &mut st,
        );
        assert!(
            matches!(&ev[1], AgentEvent::RateLimited(Some(t)) if t.timestamp() == 1_900_000_000)
        );
    }

    #[test]
    fn claude_rate_limit_event_with_reset() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "claude",
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1900000000}}"#,
            &mut st,
        );
        assert!(
            matches!(&ev[0], AgentEvent::RateLimited(Some(t)) if t.timestamp() == 1_900_000_000)
        );
        let ok = parse_line(
            "claude",
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1900000000}}"#,
            &mut st,
        );
        assert!(ok.is_empty());
    }

    #[test]
    fn claude_tool_use_is_summarized() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "claude",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}},{"type":"text","text":"ok"}]}}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::Tool(t) if t == "Bash: cargo test"));
        assert!(matches!(&ev[1], AgentEvent::Text(t) if t == "ok"));
    }

    #[test]
    fn non_json_lines_become_text_and_long_lines_are_truncated() {
        let mut st = ParseState::default();
        let long = "x".repeat(5000);
        let ev = parse_line("claude", &long, &mut st);
        assert!(matches!(&ev[0], AgentEvent::Text(t) if t.chars().count() <= 2001));
    }

    #[test]
    fn invocations_use_env_or_home_files_for_credentials() {
        let c = Credential {
            kind: "oauth_token".into(),
            secret: "tok".into(),
        };
        let i = invocation("claude", Some("opus"), &c).unwrap();
        assert!(i
            .env
            .contains(&("CLAUDE_CODE_OAUTH_TOKEN".into(), "tok".into())));
        assert!(i
            .args
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "opus"));
        assert!(!i.args.iter().any(|a| a.contains("tok")));
        let c = Credential {
            kind: "auth_json".into(),
            secret: "{}".into(),
        };
        let i = invocation("codex", None, &c).unwrap();
        assert_eq!(i.home_files[0].0, ".codex/auth.json");
        assert_eq!(i.writeback.as_deref(), Some(".codex/auth.json"));
        assert!(invocation(
            "claude",
            None,
            &Credential {
                kind: "bogus".into(),
                secret: "".into()
            }
        )
        .is_err());
        assert!(invocation("nope", None, &c).is_err());
    }
}
