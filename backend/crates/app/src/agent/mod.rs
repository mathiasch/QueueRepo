//! Provider adapters: build the CLI invocation and parse its JSON stream.
//!
//! Subscriptions are used through the official CLIs only:
//!   claude  – Claude Code  (`claude setup-token` → CLAUDE_CODE_OAUTH_TOKEN, or ANTHROPIC_API_KEY)
//!   codex   – Codex CLI    (~/.codex/auth.json from `codex login`, or OPENAI_API_KEY)
//!   gemini  – Gemini CLI   (~/.gemini/oauth_creds.json from Google login, or GEMINI_API_KEY)
//!
//! The parsers rely on the structured signals of the CLI versions pinned in
//! `docker/agent.Dockerfile` ([`CLI_VERSIONS`]); contract tests in
//! `tests/cli_contract.rs` replay fixtures of exactly these versions.
pub mod classify;
mod claude;
mod codex;
mod gemini;
mod mock;

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::Value;

pub use classify::{classify_text, parse_reset, ErrorCategory};

use crate::models::Usage;

/// CLI versions installed in the agent image (must match `docker/agent.Dockerfile`).
pub const CLI_VERSIONS: [(&str, &str); 3] = [
    ("claude", "2.1.319"),
    ("codex", "0.167.0"),
    ("gemini", "0.67.0"),
];

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
    /// Files placed in `<workspace>/.queue/` before the run (e.g. schemas).
    pub queue_files: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Agent,
    Review,
}

/// Limits handed to the CLI itself (where it supports them).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunLimits {
    pub turns: Option<u32>,
    /// Remaining cost budget of the task in USD.
    pub cost_usd: Option<f64>,
}

/// What a provider integration can enforce/report natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// The CLI enforces the result schema (missing contract = task failure).
    pub structured_output: bool,
    pub max_turns: bool,
    pub max_cost: bool,
    /// Token usage arrives while the run is in progress (live token guard).
    pub live_usage: bool,
}

pub fn capabilities(provider: &str) -> Capabilities {
    match provider {
        "claude" => Capabilities {
            structured_output: true,
            max_turns: true,
            max_cost: true,
            live_usage: true,
        },
        "codex" => Capabilities {
            structured_output: true,
            max_turns: false,
            max_cost: false,
            live_usage: false,
        },
        "gemini" => Capabilities {
            structured_output: false,
            max_turns: true,
            max_cost: false,
            live_usage: false,
        },
        "mock" => Capabilities {
            structured_output: false,
            max_turns: true,
            max_cost: true,
            live_usage: true,
        },
        _ => Capabilities {
            structured_output: false,
            max_turns: false,
            max_cost: false,
            live_usage: false,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Runtime,
    Cost,
    Tokens,
    Turns,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetryInfo {
    pub category: ErrorCategory,
    pub attempt: u32,
    pub max: u32,
    pub delay_ms: u64,
    pub status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    Text(String),
    Tool(String),
    /// Incremental usage while the run is in progress.
    UsageDelta(Usage),
    /// Authoritative usage of the whole run (replaces the sum of deltas).
    UsageTotal(Usage),
    Final(String),
    /// Result contract delivered through the CLI's structured output.
    Structured(Value),
    /// The CLI retries a provider request itself (informational only).
    Retry(RetryInfo),
    /// Terminal provider/CLI error with a recognised category.
    Failure {
        category: ErrorCategory,
        message: String,
    },
    /// The CLI stopped because a limit we passed to it was reached.
    LimitHit(LimitKind),
    /// Error without provider-level cause (→ task failure).
    Error(String),
}

/// Per-run parser state (some CLIs stream deltas or report errors in
/// several events).
#[derive(Default)]
pub struct ParseState {
    buf: String,
    last_text: String,
    /// Claude: usage last seen per assistant `message.id` (dedupe).
    seen_usage: std::collections::HashMap<String, Usage>,
    /// Claude: `assistant.error` of the most recent assistant message.
    last_error: Option<ErrorCategory>,
    /// Claude: category of a retry that has not been followed by a successful answer.
    last_retry: Option<ErrorCategory>,
    /// Codex: usage summed over `turn.completed` events.
    total: Usage,
}

/// Exit code with which Gemini reports its session turn limit.
pub fn gemini_turn_limit_exit() -> i32 {
    gemini::TURN_LIMIT_EXIT
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

pub fn invocation(
    provider: &str,
    model: Option<&str>,
    cred: &Credential,
    limits: &RunLimits,
    kind: RunKind,
) -> Result<Invocation> {
    let mut inv = Invocation {
        program: String::new(),
        args: vec![],
        env: vec![],
        home_files: vec![],
        writeback: None,
        queue_files: vec![],
    };
    match provider {
        "claude" => claude::invocation(&mut inv, model, cred, limits, kind)?,
        "codex" => codex::invocation(&mut inv, model, cred, kind)?,
        "gemini" => gemini::invocation(&mut inv, model, cred, limits)?,
        "mock" => mock::invocation(&mut inv, limits),
        p => bail!("unknown provider {p}"),
    }
    Ok(inv)
}

pub(crate) fn trunc(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

pub(crate) fn tool_summary(name: &str, input: &Value) -> String {
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

pub(crate) fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// Error message from a CLI's error channel → categorised failure or plain error.
pub(crate) fn error_event(text: &str) -> AgentEvent {
    match classify_text(text) {
        Some(category) => AgentEvent::Failure {
            category,
            message: trunc(text, 4000),
        },
        None => AgentEvent::Error(trunc(text, 4000)),
    }
}

pub fn parse_line(provider: &str, line: &str, st: &mut ParseState) -> Vec<AgentEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return if line.trim().is_empty() {
            vec![]
        } else {
            vec![AgentEvent::Text(trunc(line, 2000))]
        };
    };
    match provider {
        "claude" => claude::parse(&v, st),
        "codex" => codex::parse(&v, st),
        "gemini" => gemini::parse(&v, st),
        "mock" => mock::parse(&v),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_json_lines_become_text_and_long_lines_are_truncated() {
        let mut st = ParseState::default();
        let long = "x".repeat(5000);
        let ev = parse_line("claude", &long, &mut st);
        assert!(matches!(&ev[0], AgentEvent::Text(t) if t.chars().count() <= 2001));
    }

    #[test]
    fn invocations_use_env_or_home_files_for_credentials() {
        let none = RunLimits::default();
        let c = Credential {
            kind: "oauth_token".into(),
            secret: "tok".into(),
        };
        let i = invocation("claude", Some("opus"), &c, &none, RunKind::Agent).unwrap();
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
        let i = invocation("codex", None, &c, &none, RunKind::Agent).unwrap();
        assert_eq!(i.home_files[0].0, ".codex/auth.json");
        assert_eq!(i.writeback.as_deref(), Some(".codex/auth.json"));
        assert!(invocation(
            "claude",
            None,
            &Credential {
                kind: "bogus".into(),
                secret: "".into()
            },
            &none,
            RunKind::Agent
        )
        .is_err());
        assert!(invocation("nope", None, &c, &none, RunKind::Agent).is_err());
    }

    #[test]
    fn no_secret_in_argv_for_any_provider_and_limit() {
        let lim = RunLimits {
            turns: Some(7),
            cost_usd: Some(1.5),
        };
        for (p, kinds) in [
            ("claude", &["oauth_token", "api_key"][..]),
            ("codex", &["auth_json", "api_key"][..]),
            ("gemini", &["oauth_creds", "api_key"][..]),
        ] {
            for k in kinds {
                for kind in [RunKind::Agent, RunKind::Review] {
                    let c = Credential {
                        kind: k.to_string(),
                        secret: "SECRET-VALUE-123".into(),
                    };
                    let i = invocation(p, Some("m"), &c, &lim, kind).unwrap();
                    assert!(
                        !i.args.iter().any(|a| a.contains("SECRET-VALUE-123")),
                        "{p}/{k}: {:?}",
                        i.args
                    );
                    assert!(!i.program.contains("SECRET"));
                }
            }
        }
    }

    #[test]
    fn capabilities_per_provider() {
        assert!(capabilities("claude").structured_output && capabilities("claude").max_cost);
        assert!(capabilities("codex").structured_output && !capabilities("codex").max_turns);
        assert!(!capabilities("gemini").structured_output && capabilities("gemini").max_turns);
        assert!(!capabilities("gemini").live_usage && !capabilities("codex").live_usage);
        assert!(!capabilities("mock").structured_output && capabilities("mock").live_usage);
    }

    #[test]
    fn dockerfile_pins_match_cli_versions() {
        let df = include_str!("../../../../../docker/agent.Dockerfile");
        assert!(
            !df.contains("latest"),
            "agent image must not install latest"
        );
        for (arg, (provider, version)) in
            ["CLAUDE_CODE_VERSION", "CODEX_VERSION", "GEMINI_CLI_VERSION"]
                .iter()
                .zip(CLI_VERSIONS)
        {
            assert!(
                df.contains(&format!("ARG {arg}={version}\n")),
                "{provider}: Dockerfile pin != CLI_VERSIONS ({version})"
            );
        }
        assert!(
            df.contains("--version"),
            "the image build must verify the installed versions"
        );
    }
}
