//! Provider adapters: build the CLI invocation and parse its JSON stream.
//!
//! Subscriptions are used through the official CLIs only:
//!   claude  – Claude Code  (`claude setup-token` → CLAUDE_CODE_OAUTH_TOKEN, or ANTHROPIC_API_KEY)
//!   codex   – Codex CLI    (~/.codex/auth.json from `codex login`, or OPENAI_API_KEY)
//!   gemini  – Gemini CLI   (~/.gemini/oauth_creds.json from Google login, or GEMINI_API_KEY)
//!   opencode – opencode    (only through connectors: Mistral, Gemini API, Infomaniak,
//!              OpenAI-compatible endpoints, OpenRouter, LiteLLM)
//!
//! Connectors (WP13) add Bedrock/Vertex/gateway modes to Claude Code and
//! Azure/Flex/gateway modes to Codex, see [`ConnCfg`].
//!
//! The parsers rely on the structured signals of the CLI versions pinned in
//! `docker/agent.Dockerfile` ([`CLI_VERSIONS`]); contract tests in
//! `tests/cli_contract.rs` replay fixtures of exactly these versions.
pub mod classify;
mod claude;
mod codex;
mod gemini;
mod mock;
pub mod opencode;

use anyhow::{bail, Result};
use serde_json::Value;

pub use classify::{classify_text, parse_reset, ErrorCategory};

use crate::models::Usage;

/// CLI versions installed in the agent image (must match `docker/agent.Dockerfile`).
pub const CLI_VERSIONS: [(&str, &str); 4] = [
    ("claude", "2.1.319"),
    ("codex", "0.167.0"),
    ("gemini", "0.67.0"),
    ("opencode", "1.19.5"),
];

/// Maintenance state of the pinned CLIs (research of 2026-11-08, see
/// docs/roadmap/WP13-plan.md §0). The live check is `GET /api/cli-status`.
pub fn cli_notes() -> std::collections::BTreeMap<String, queued_api::CliNote> {
    let note = |state: &str, note: &str| queued_api::CliNote {
        state: state.into(),
        note: note.into(),
        checked: "2026-11-08".into(),
    };
    [
        ("claude", note("active", "Gepinnt auf 2.1.319.")),
        ("codex", note("active", "Gepinnt auf 0.167.0 (= aktuelle npm-Version am 8.11.2026).")),
        ("gemini", note(
            "frozen",
            "Eingefroren auf 0.67.0: keine Weiterentwicklung im Queue. Das npm-Paket wird weiter veröffentlicht (0.67.0 am 4.11.2026, 0.68.0-preview, nicht als veraltet markiert). Sekundärquellen melden eine Ablösung durch das Closed-Source-CLI Antigravity und ein Ende des Abo-/OAuth-Pfads am 18.6.2026 – nicht bestätigt. Für Gemini per API-Schlüssel den Zugang „Gemini API (über opencode)“ nutzen.",
        )),
        ("opencode", note("active", "Gepinnt auf 1.19.5 (MIT), Mehranbieter-Agent für Mistral, Gemini-API, Infomaniak, eigene Server, OpenRouter und LiteLLM.")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// Interprets the runner's CLI probe against the pins (Gemini: frozen;
/// Antigravity `agy` present = the reported successor is installed, unsupported).
pub fn interpret_cli_status(found: &[queued_common::CliProbe]) -> Vec<queued_api::CliStatus> {
    found
        .iter()
        .map(|p| {
            let pinned = CLI_VERSIONS
                .iter()
                .find(|(c, _)| *c == p.cli)
                .map(|(_, v)| v.to_string());
            let (state, note) = match (&p.version, &pinned) {
                (None, Some(_)) => ("missing", format!("{} ist im Agenten-Image nicht installiert oder startet nicht.", p.cli)),
                (Some(v), Some(pin)) if !v.contains(pin.as_str()) => (
                    "version_mismatch",
                    format!("Gefunden: {v}, erwartet {pin} – die Auswertung ist auf {pin} abgestimmt."),
                ),
                (Some(_), Some(_)) if p.cli == "gemini" => (
                    "frozen",
                    "Läuft, ist aber eingefroren: keine Weiterentwicklung im Queue (siehe Hinweis).".to_string(),
                ),
                (Some(_), Some(_)) => ("ok", "Installiert in der gepinnten Version.".to_string()),
                (Some(v), None) if p.cli == "agy" => (
                    "replaced",
                    format!("Antigravity ({v}) ist installiert – der Queue unterstützt es nicht (Closed Source, Format unbekannt)."),
                ),
                (None, None) => ("missing", "Nicht installiert.".to_string()),
                (Some(v), None) => ("unexpected", format!("Gefunden: {v}")),
            };
            queued_api::CliStatus {
                cli: p.cli.clone(),
                pinned,
                installed: p.version.clone(),
                state: state.into(),
                note,
            }
        })
        .collect()
}

pub struct Credential {
    pub kind: String,
    pub secret: String,
    /// Set when the credential comes from a connector (WP13).
    pub conn: Option<ConnCfg>,
}

impl Credential {
    pub fn legacy(kind: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            secret: secret.into(),
            conn: None,
        }
    }
}

/// Non-secret connector settings handed to the adapters.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnCfg {
    /// Connector id (refreshed OAuth files are written back to it).
    pub id: Option<uuid::Uuid>,
    pub kind: String,
    pub label: String,
    pub config: Value,
    /// OpenAI Flex service tier for this run.
    pub flex: bool,
}

impl ConnCfg {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.config
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
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
    /// Variables pointing into the run's $HOME (`(name, home-relative path)`).
    pub home_env: Vec<(String, String)>,
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
pub use queued_api::ProviderCapabilities as Capabilities;

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
        // Usage per step (`step_finish`) → live token/cost guard of the queue.
        "opencode" => Capabilities {
            structured_output: false,
            max_turns: false,
            max_cost: false,
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

/// Capabilities of a run: Claude Code's own cost limit (`--max-budget-usd`)
/// estimates with Anthropic prices, which is wrong behind a gateway with other
/// models – there the queue's live cost guard (catalog prices) applies alone.
pub fn capabilities_for(provider: &str, conn: Option<&ConnCfg>) -> Capabilities {
    let mut c = capabilities(provider);
    if provider == "claude"
        && conn.is_some_and(|c| matches!(c.kind.as_str(), "anthropic_gateway" | "litellm"))
    {
        c.max_cost = false;
    }
    c
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
        // Only through connectors.
        "opencode" => &[],
        _ => &[],
    }
}

/// `host[:port]` of a URL as an allowlist rule (port only if non-default).
fn url_host(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    let host = u.host_str()?.to_string();
    Some(match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

/// Hosts the CLI of a run must reach itself: the model API and, for
/// subscriptions, the token refresh endpoints. They are allowed in every
/// agent network mode (otherwise the agent could not work) and never
/// inspected. Prior knowledge of the CLIs and providers, **not verified
/// live** – a missing host shows up as a refused connection in the log.
pub fn egress_hosts(provider: &str, cred: &Credential) -> Vec<String> {
    let v = |hs: &[&str]| hs.iter().map(|h| h.to_string()).collect::<Vec<_>>();
    if let Some(c) = &cred.conn {
        let base = crate::connectors::wire(&c.kind, &c.config).and_then(|(_, b)| url_host(&b));
        let mut hosts = match c.kind.as_str() {
            "claude_subscription" => v(&["api.anthropic.com"]),
            "codex_subscription" => v(&["chatgpt.com", "auth.openai.com", "api.openai.com"]),
            "gemini_subscription" => v(&[
                "cloudcode-pa.googleapis.com",
                "oauth2.googleapis.com",
                "generativelanguage.googleapis.com",
            ]),
            "gemini_api" => v(&["generativelanguage.googleapis.com"]),
            "bedrock" => vec![format!(
                "bedrock-runtime.{}.amazonaws.com",
                c.get("region").unwrap_or("eu-central-1")
            )],
            "vertex" => {
                let r = c.get("region").unwrap_or("eu");
                let mut h = v(&["oauth2.googleapis.com", "aiplatform.googleapis.com"]);
                match r {
                    "global" => {}
                    "eu" | "us" => h.push(format!("aiplatform.{r}.rep.googleapis.com")),
                    region => h.push(format!("{region}-aiplatform.googleapis.com")),
                }
                h
            }
            _ => vec![],
        };
        hosts.extend(base);
        hosts.dedup();
        return hosts;
    }
    match (provider, cred.kind.as_str()) {
        ("claude", _) => v(&["api.anthropic.com"]),
        ("codex", "auth_json") => v(&["chatgpt.com", "auth.openai.com", "api.openai.com"]),
        ("codex", _) => v(&["api.openai.com"]),
        ("gemini", "oauth_creds") => v(&[
            "cloudcode-pa.googleapis.com",
            "oauth2.googleapis.com",
            "generativelanguage.googleapis.com",
        ]),
        ("gemini", _) => v(&["generativelanguage.googleapis.com"]),
        _ => vec![],
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
        home_env: vec![],
    };
    match provider {
        "claude" => claude::invocation(&mut inv, model, cred, limits, kind)?,
        "codex" => codex::invocation(&mut inv, model, cred, kind)?,
        "gemini" => gemini::invocation(&mut inv, model, cred, limits)?,
        "opencode" => opencode::invocation(&mut inv, model, cred)?,
        "mock" => mock::invocation(&mut inv, limits),
        p => bail!("unknown provider {p}"),
    }
    Ok(inv)
}

/// Gemini system settings (highest precedence, above the workspace settings
/// a trusted folder would load): no hooks, no tool discovery/call commands,
/// no MCP server command, no `.env` from the project directory. From the
/// 0.67.0 bundle: `mergeSettings` (system merged last), `loadCliConfig`
/// (`enableHooks: settings.hooksConfig.enabled`, `toolDiscoveryCommand`,
/// `mcpServerCommand`), `findEnvFile` (`advanced.ignoreLocalEnv`).
pub const GEMINI_SYSTEM_SETTINGS: &str = r#"{"hooksConfig":{"enabled":false},"tools":{"discoveryCommand":"","callCommand":""},"mcp":{"serverCommand":""},"advanced":{"ignoreLocalEnv":true}}"#;
pub const GEMINI_SYSTEM_SETTINGS_FILE: &str = ".gemini/queue-system-settings.json";

/// WP03: keep the CLI from running configuration out of the repository
/// (hooks, MCP servers, settings with `env`/helpers, plugins) unless the
/// project owner trusts it. Codex needs nothing: it loads the project
/// `.codex/config.toml` only for trusted projects, and Queue never marks a
/// workspace as trusted (strings of the 0.167.0 binary; not executed).
pub fn restrict_repo_config(inv: &mut Invocation, provider: &str) {
    match provider {
        // `--setting-sources user`: no `.claude/settings{,.local}.json` of the
        // project; `--strict-mcp-config` without `--mcp-config`: no `.mcp.json`
        // (both in `--help` of the installed 2.1.319).
        "claude" => inv
            .args
            .extend(["--setting-sources", "user", "--strict-mcp-config"].map(String::from)),
        "gemini" => {
            inv.home_files.push((
                GEMINI_SYSTEM_SETTINGS_FILE.into(),
                GEMINI_SYSTEM_SETTINGS.into(),
            ));
            inv.home_env.push((
                "GEMINI_CLI_SYSTEM_SETTINGS_PATH".into(),
                GEMINI_SYSTEM_SETTINGS_FILE.into(),
            ));
            // MCP servers are allowed only under this unguessable name, so
            // none of the workspace's servers start (`isBlockedBySettings`).
            inv.args.extend([
                "--allowed-mcp-server-names".into(),
                format!("queue-none-{}", uuid::Uuid::new_v4().simple()),
            ]);
        }
        // Disables project `opencode.json(c)`, `.opencode/` (plugins, agents,
        // commands) and project AGENTS.md (`ConfigPaths`, 1.19.5).
        "opencode" => inv
            .env
            .push(("OPENCODE_DISABLE_PROJECT_CONFIG".into(), "1".into())),
        _ => {}
    }
}

/// Repository files a CLI loads even when Queue restricts its configuration
/// (watched by the `agent-config` baseline): Gemini must trust the workspace
/// to run tools headless and then always reads `.gemini/.env` (0.67.0
/// `findEnvFile`), which may set variables Queue did not set.
pub fn always_loaded_config(provider: &str) -> &'static [&'static str] {
    match provider {
        "gemini" => &[".gemini/.env"],
        _ => &[],
    }
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

/// Upper bound for one token count reported by a CLI (the stream is not
/// trusted: negative or absurd values would corrupt budgets and limits, and
/// overflow in sums).
pub const MAX_REPORTED_TOKENS: i64 = 10_000_000_000;
/// Upper bound for one cost value (USD) reported by a CLI.
pub const MAX_REPORTED_COST_USD: f64 = 10_000.0;

/// Non-negative count from the stream, capped at [`MAX_REPORTED_TOKENS`];
/// missing, fractional, negative or non-numeric values count as 0.
pub(crate) fn n(v: &Value, k: &str) -> i64 {
    match v.get(k) {
        Some(x) if x.is_u64() => x
            .as_u64()
            .map_or(0, |u| u.min(MAX_REPORTED_TOKENS as u64) as i64),
        _ => 0,
    }
}

/// Retry counters for display (`n` is already non-negative).
pub(crate) fn count(x: i64) -> u32 {
    u32::try_from(x).unwrap_or(u32::MAX)
}

/// Cost in USD from the stream, clamped to `0..=MAX_REPORTED_COST_USD`.
pub(crate) fn usd(v: &Value) -> f64 {
    v.as_f64()
        .filter(|c| c.is_finite() && *c > 0.0)
        .map_or(0.0, |c| c.min(MAX_REPORTED_COST_USD))
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
        "opencode" => opencode::parse(&v, st),
        "mock" => mock::parse(&v),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_config_is_restricted_per_cli() {
        let none = RunLimits::default();
        let inv = |p: &str, kind: &str| {
            let mut i = invocation(
                p,
                None,
                &Credential::legacy(kind, "k"),
                &none,
                RunKind::Agent,
            )
            .unwrap();
            let before = i.args.clone();
            restrict_repo_config(&mut i, p);
            (before, i)
        };
        let (before, c) = inv("claude", "api_key");
        assert_eq!(c.args.len(), before.len() + 3);
        let joined = c.args.join(" ");
        assert!(
            joined.contains("--setting-sources user --strict-mcp-config"),
            "{joined}"
        );
        assert!(!c.args.contains(&"--mcp-config".to_string()));
        assert!(
            !c.args.contains(&"--bare".to_string()),
            "--bare would ignore OAuth tokens"
        );

        let (_, g) = inv("gemini", "api_key");
        let settings = g
            .home_files
            .iter()
            .find(|(p, _)| p == GEMINI_SYSTEM_SETTINGS_FILE)
            .map(|(_, c)| serde_json::from_str::<Value>(c).unwrap())
            .unwrap();
        assert_eq!(settings["hooksConfig"]["enabled"], false);
        assert_eq!(settings["advanced"]["ignoreLocalEnv"], true);
        assert_eq!(settings["tools"]["discoveryCommand"], "");
        assert_eq!(
            g.home_env,
            vec![(
                "GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(),
                GEMINI_SYSTEM_SETTINGS_FILE.to_string()
            )]
        );
        let i = g
            .args
            .iter()
            .position(|a| a == "--allowed-mcp-server-names")
            .unwrap();
        assert!(g.args[i + 1].starts_with("queue-none-") && g.args[i + 1].len() > 30);
        // Unguessable per run.
        let (_, g2) = inv("gemini", "api_key");
        let j = g2
            .args
            .iter()
            .position(|a| a == "--allowed-mcp-server-names")
            .unwrap();
        assert_ne!(g.args[i + 1], g2.args[j + 1]);
        // Trust stays on (headless tools need it); the user settings are untouched.
        assert!(g
            .env
            .contains(&("GEMINI_CLI_TRUST_WORKSPACE".into(), "true".into())));

        let mut o = invocation(
            "opencode",
            Some("mistral-large-4"),
            &Credential {
                kind: "connector:mistral".into(),
                secret: "k".into(),
                conn: Some(ConnCfg {
                    id: None,
                    kind: "mistral".into(),
                    label: "m".into(),
                    config: serde_json::json!({}),
                    flex: false,
                }),
            },
            &none,
            RunKind::Agent,
        )
        .unwrap();
        restrict_repo_config(&mut o, "opencode");
        assert!(o
            .env
            .contains(&("OPENCODE_DISABLE_PROJECT_CONFIG".into(), "1".into())));

        let (before, x) = inv("codex", "api_key");
        assert_eq!(x.args, before, "codex never trusts the workspace by itself");
        assert_eq!(always_loaded_config("gemini"), &[".gemini/.env"]);
        assert!(always_loaded_config("claude").is_empty());
    }

    #[test]
    fn egress_hosts_per_access() {
        let legacy = |p: &str, k: &str| egress_hosts(p, &Credential::legacy(k, "x"));
        assert_eq!(legacy("claude", "oauth_token"), vec!["api.anthropic.com"]);
        assert_eq!(legacy("codex", "api_key"), vec!["api.openai.com"]);
        assert!(legacy("codex", "auth_json").contains(&"chatgpt.com".to_string()));
        assert!(legacy("gemini", "oauth_creds").contains(&"oauth2.googleapis.com".to_string()));
        assert!(legacy("mock", "none").is_empty());
        let conn = |kind: &str, config: Value| Credential {
            kind: format!("connector:{kind}"),
            secret: "x".into(),
            conn: Some(ConnCfg {
                id: None,
                kind: kind.into(),
                label: "l".into(),
                config,
                flex: false,
            }),
        };
        assert_eq!(
            egress_hosts(
                "claude",
                &conn(
                    "anthropic_gateway",
                    serde_json::json!({"base_url": "https://gw.example.ch:8443/x"})
                )
            ),
            vec!["gw.example.ch:8443"]
        );
        assert_eq!(
            egress_hosts(
                "claude",
                &conn("bedrock", serde_json::json!({"region": "eu-west-3"}))
            ),
            vec!["bedrock-runtime.eu-west-3.amazonaws.com"]
        );
        assert_eq!(
            egress_hosts("opencode", &conn("mistral", serde_json::json!({}))),
            vec!["api.eu.mistral.ai"]
        );
        assert_eq!(
            egress_hosts(
                "opencode",
                &conn(
                    "openai_compatible",
                    serde_json::json!({"base_url": "http://gpu:8000/v1"})
                )
            ),
            vec!["gpu:8000"]
        );
        assert!(egress_hosts(
            "claude",
            &conn("vertex", serde_json::json!({"region": "europe-west4"}))
        )
        .contains(&"europe-west4-aiplatform.googleapis.com".to_string()));
        for h in egress_hosts("codex", &conn("codex_subscription", serde_json::json!({}))) {
            assert!(queued_common::net::HostRule::parse(&h).is_ok(), "{h}");
        }
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
        let none = RunLimits::default();
        let c = Credential {
            kind: "oauth_token".into(),
            secret: "tok".into(),
            conn: None,
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
        let c = Credential::legacy("auth_json", "{}");
        let i = invocation("codex", None, &c, &none, RunKind::Agent).unwrap();
        assert_eq!(i.home_files[0].0, ".codex/auth.json");
        assert_eq!(i.writeback.as_deref(), Some(".codex/auth.json"));
        assert!(invocation(
            "claude",
            None,
            &Credential {
                kind: "bogus".into(),
                secret: "".into(),
                conn: None,
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
                        conn: None,
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

    fn usages(ev: &[AgentEvent]) -> Vec<Usage> {
        ev.iter()
            .filter_map(|e| match e {
                AgentEvent::UsageDelta(u) | AgentEvent::UsageTotal(u) => Some(u.clone()),
                _ => None,
            })
            .collect()
    }

    fn plausible(u: &Usage) -> bool {
        [
            u.input_tokens,
            u.output_tokens,
            u.cache_read_tokens,
            u.cache_write_tokens,
        ]
        .iter()
        .all(|t| (0..=MAX_REPORTED_TOKENS).contains(t))
            && (0.0..=MAX_REPORTED_COST_USD).contains(&u.cost_usd)
    }

    /// The CLI stream is untrusted: negative, absurd or mistyped usage must
    /// neither panic (overflow) nor lower/explode budgets and limits.
    #[test]
    fn implausible_usage_values_are_clamped() {
        let huge = "99999999999999999999";
        let cases: [(&str, String); 7] = [
            ("claude", format!(r#"{{"type":"assistant","message":{{"id":"m1","usage":{{"input_tokens":-9223372036854775808,"output_tokens":{huge},"cache_creation_input_tokens":9223372036854775807}}}}}}"#)),
            ("claude", r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":9223372036854775807,"output_tokens":-5}}}"#.into()),
            ("claude", format!(r#"{{"type":"result","subtype":"success","result":"x","total_cost_usd":-3.5,"modelUsage":{{"a":{{"inputTokens":-100,"outputTokens":9223372036854775807,"costUSD":1e300}},"b":{{"inputTokens":9223372036854775807,"cacheCreationInputTokens":{huge}}}}}}}"#)),
            ("codex", r#"{"type":"turn.completed","usage":{"input_tokens":-50,"cached_input_tokens":9223372036854775807,"output_tokens":-1}}"#.into()),
            ("gemini", r#"{"type":"result","status":"success","stats":{"input_tokens":"lots","cached":-7,"output_tokens":1.5e30}}"#.into()),
            ("mock", r#"{"type":"usage","input_tokens":-1200,"output_tokens":9223372036854775807,"cost_usd":-1}"#.into()),
            ("mock", r#"{"type":"usage","input_tokens":12.7,"cost_usd":"0.5"}"#.into()),
        ];
        for (p, line) in &cases {
            let mut st = ParseState::default();
            let us = usages(&parse_line(p, line, &mut st));
            assert!(!us.is_empty(), "{p}: {line}");
            for u in &us {
                assert!(plausible(u), "{p}: {u:?} from {line}");
            }
        }
        // Codex sums turns: many maximal turns saturate instead of overflowing.
        let mut st = ParseState::default();
        let turn = r#"{"type":"turn.completed","usage":{"input_tokens":9223372036854775807,"output_tokens":9223372036854775807}}"#;
        let mut last = Usage::default();
        for _ in 0..2000 {
            last = usages(&parse_line("codex", turn, &mut st)).pop().unwrap();
        }
        assert!(last.output_tokens > 0 && last.input_tokens >= 0, "{last:?}");
        let mut acc = Usage {
            input_tokens: i64::MAX - 1,
            ..Default::default()
        };
        acc.add(&Usage {
            input_tokens: 10,
            ..Default::default()
        });
        assert_eq!(acc.input_tokens, i64::MAX);
        assert_eq!(crate::budget::counted(&acc), i64::MAX);
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
        for (arg, (provider, version)) in [
            "CLAUDE_CODE_VERSION",
            "CODEX_VERSION",
            "GEMINI_CLI_VERSION",
            "OPENCODE_VERSION",
        ]
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

    fn conn(kind: &str, config: serde_json::Value, secret: &str, flex: bool) -> Credential {
        Credential {
            kind: format!("connector:{kind}"),
            secret: secret.into(),
            conn: Some(ConnCfg {
                id: None,
                kind: kind.into(),
                label: "Zugang".into(),
                config,
                flex,
            }),
        }
    }

    fn env<'a>(i: &'a Invocation, k: &str) -> Option<&'a str> {
        i.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    /// Every connector type: the secret never appears in argv or the program,
    /// and only travels through env/home files.
    #[test]
    fn no_secret_in_argv_for_any_connector() {
        use serde_json::json;
        let secret = "SECRET-CONN-987";
        let lim = RunLimits {
            turns: Some(5),
            cost_usd: Some(2.0),
        };
        for t in crate::connectors::types(true) {
            let mut cfg = serde_json::Map::new();
            for f in &t.fields {
                let v = match (f.key.as_str(), f.input.as_str()) {
                    (_, "select") => f.default.clone().unwrap(),
                    ("base_url", _) => "https://gw.example.ch".into(),
                    ("project_id", _) => "proj".into(),
                    ("region", _) => "eu-central-1".into(),
                    _ => continue,
                };
                cfg.insert(f.key.clone(), json!(v));
            }
            let cfg = serde_json::Value::Object(cfg);
            let harness = crate::connectors::harness(&t.kind, &cfg);
            let s = if t.secret_json {
                format!("{{\"k\":\"{secret}\"}}")
            } else {
                secret.to_string()
            };
            for kind in [RunKind::Agent, RunKind::Review] {
                let c = conn(&t.kind, cfg.clone(), &s, t.flex);
                let i = invocation(&harness, Some("claude-sonnet-5-5"), &c, &lim, kind)
                    .unwrap_or_else(|e| panic!("{}: {e}", t.kind));
                assert!(
                    !i.args.iter().any(|a| a.contains(secret)),
                    "{}: {:?}",
                    t.kind,
                    i.args
                );
                assert!(!i.program.contains(secret));
                if t.kind != "mock" && t.secret_label.is_some() {
                    let carried = i.env.iter().any(|(_, v)| v.contains(secret))
                        || i.home_files.iter().any(|(_, v)| v.contains(secret));
                    assert!(carried, "{}: secret not handed over", t.kind);
                }
            }
        }
    }

    #[test]
    fn claude_connector_modes() {
        use serde_json::json;
        let none = RunLimits::default();
        // Bedrock EU: geo profile, region, bearer token, background model in the same geo.
        let b = conn(
            "bedrock",
            json!({"geo":"eu","region":"eu-central-1"}),
            "tok",
            false,
        );
        let i = invocation(
            "claude",
            Some("claude-sonnet-5-5"),
            &b,
            &none,
            RunKind::Agent,
        )
        .unwrap();
        assert_eq!(env(&i, "CLAUDE_CODE_USE_BEDROCK"), Some("1"));
        assert_eq!(env(&i, "AWS_REGION"), Some("eu-central-1"));
        assert_eq!(env(&i, "AWS_BEARER_TOKEN_BEDROCK"), Some("tok"));
        assert_eq!(
            env(&i, "ANTHROPIC_SMALL_FAST_MODEL"),
            Some("eu.anthropic.claude-sonnet-5-5")
        );
        assert!(i
            .args
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "eu.anthropic.claude-sonnet-5-5"));
        assert!(env(&i, "ANTHROPIC_API_KEY").is_none());
        // Access keys as JSON.
        let k = conn(
            "bedrock",
            json!({"geo":"eu","region":"eu-west-1"}),
            r#"{"access_key_id":"AK","secret_access_key":"SK","session_token":"ST"}"#,
            false,
        );
        let i = invocation(
            "claude",
            Some("eu.anthropic.claude-opus-5-5"),
            &k,
            &none,
            RunKind::Agent,
        )
        .unwrap();
        assert_eq!(
            (
                env(&i, "AWS_ACCESS_KEY_ID"),
                env(&i, "AWS_SECRET_ACCESS_KEY"),
                env(&i, "AWS_SESSION_TOKEN")
            ),
            (Some("AK"), Some("SK"), Some("ST"))
        );
        // A US profile on an EU connector, or no model, is refused.
        assert!(invocation(
            "claude",
            Some("us.anthropic.claude-opus-5-5"),
            &b,
            &none,
            RunKind::Agent
        )
        .is_err());
        assert!(invocation("claude", None, &b, &none, RunKind::Agent).is_err());
        let bad = conn(
            "bedrock",
            json!({"geo":"eu"}),
            r#"{"access_key_id":"AK"}"#,
            false,
        );
        assert!(invocation(
            "claude",
            Some("claude-sonnet-5-5"),
            &bad,
            &none,
            RunKind::Agent
        )
        .is_err());
        // Vertex: ADC file in HOME, region and project.
        let v = conn(
            "vertex",
            json!({"project_id":"p1","region":"eu"}),
            r#"{"type":"service_account"}"#,
            false,
        );
        let i = invocation(
            "claude",
            Some("claude-sonnet-5-5"),
            &v,
            &none,
            RunKind::Agent,
        )
        .unwrap();
        assert_eq!(env(&i, "CLAUDE_CODE_USE_VERTEX"), Some("1"));
        assert_eq!(env(&i, "CLOUD_ML_REGION"), Some("eu"));
        assert_eq!(env(&i, "ANTHROPIC_VERTEX_PROJECT_ID"), Some("p1"));
        assert_eq!(
            i.home_files[0].0,
            ".config/gcloud/application_default_credentials.json"
        );
        // Gateway: base URL + auth token, no cost flag (Claude would price other models wrongly).
        let g = conn(
            "anthropic_gateway",
            json!({"base_url":"https://gw.example.ch","location":"ch"}),
            "gw",
            false,
        );
        let lim = RunLimits {
            turns: None,
            cost_usd: Some(1.0),
        };
        let caps = capabilities_for("claude", g.conn.as_ref());
        assert!(!caps.max_cost && caps.live_usage);
        let i = invocation(
            "claude",
            Some("mistral-large-4"),
            &g,
            &RunLimits {
                cost_usd: lim.cost_usd.filter(|_| caps.max_cost),
                ..lim
            },
            RunKind::Agent,
        )
        .unwrap();
        assert_eq!(env(&i, "ANTHROPIC_BASE_URL"), Some("https://gw.example.ch"));
        assert_eq!(env(&i, "ANTHROPIC_AUTH_TOKEN"), Some("gw"));
        assert!(!i.args.contains(&"--max-budget-usd".to_string()));
        // Anthropic API with a proxy base URL.
        let a = conn(
            "anthropic_api",
            json!({"base_url":"https://proxy.example.ch"}),
            "sk",
            false,
        );
        let i = invocation("claude", None, &a, &none, RunKind::Agent).unwrap();
        assert_eq!(
            (env(&i, "ANTHROPIC_API_KEY"), env(&i, "ANTHROPIC_BASE_URL")),
            (Some("sk"), Some("https://proxy.example.ch"))
        );
        // Wrong harness for a connector.
        let m = conn("mistral", json!({"endpoint":"eu"}), "k", false);
        assert!(invocation("claude", Some("x"), &m, &none, RunKind::Agent).is_err());
        assert!(invocation("codex", Some("x"), &m, &none, RunKind::Agent).is_err());
        assert!(invocation("gemini", Some("x"), &m, &none, RunKind::Agent).is_err());
    }

    #[test]
    fn codex_connector_modes() {
        use serde_json::json;
        let none = RunLimits::default();
        let o = conn("openai_api", json!({}), "sk", true);
        let i = invocation("codex", Some("gpt-6.1-terra"), &o, &none, RunKind::Agent).unwrap();
        assert_eq!(env(&i, "OPENAI_API_KEY"), Some("sk"));
        assert_eq!(
            i.home_files,
            vec![(
                ".codex/config.toml".to_string(),
                "service_tier = \"flex\"\n".to_string()
            )]
        );
        let o = conn("openai_api", json!({}), "sk", false);
        assert!(invocation("codex", None, &o, &none, RunKind::Agent)
            .unwrap()
            .home_files
            .is_empty());
        let a = conn(
            "azure_openai",
            json!({"base_url":"https://r.openai.azure.com/openai/v1","deployment":"switzerland_north"}),
            "az",
            false,
        );
        let i = invocation("codex", Some("gpt-6.1-sol"), &a, &none, RunKind::Agent).unwrap();
        let toml = &i
            .home_files
            .iter()
            .find(|(p, _)| p == ".codex/config.toml")
            .unwrap()
            .1;
        assert!(
            toml.starts_with("model_provider = \"queue\"\n[model_providers.queue]\n"),
            "{toml}"
        );
        assert!(toml.contains("base_url = \"https://r.openai.azure.com/openai/v1\""));
        assert!(toml.contains("wire_api = \"responses\""));
        assert!(toml.contains("env_http_headers = { \"api-key\" = \"QUEUE_LLM_API_KEY\" }"));
        assert!(!toml.contains("az\""), "no secret in the config file");
        assert_eq!(env(&i, "QUEUE_LLM_API_KEY"), Some("az"));
        let l = conn(
            "litellm",
            json!({"base_url":"https://llm.example.ch/","harness":"codex"}),
            "vk",
            true,
        );
        let i = invocation("codex", Some("gpt-6.1-terra"), &l, &none, RunKind::Agent).unwrap();
        let toml = &i.home_files[0].1;
        assert!(
            toml.contains("base_url = \"https://llm.example.ch/v1\"")
                && toml.contains("env_key = \"QUEUE_LLM_API_KEY\"")
        );
        assert!(toml.contains("service_tier = \"flex\""));
        let sub = conn("codex_subscription", json!({}), "{}", false);
        let i = invocation("codex", None, &sub, &none, RunKind::Agent).unwrap();
        assert_eq!(i.writeback.as_deref(), Some(".codex/auth.json"));
        let gs = conn("gemini_subscription", json!({}), "{}", false);
        let i = invocation("gemini", None, &gs, &none, RunKind::Agent).unwrap();
        assert_eq!(i.writeback.as_deref(), Some(".gemini/oauth_creds.json"));
    }

    #[test]
    fn cli_status_interpretation() {
        use queued_common::CliProbe;
        let p = |cli: &str, v: Option<&str>| CliProbe {
            cli: cli.into(),
            version: v.map(String::from),
        };
        let st = interpret_cli_status(&[
            p("claude", Some("2.1.319 (Claude Code)")),
            p("codex", Some("codex-cli 0.150.0")),
            p("gemini", Some("0.67.0")),
            p("opencode", None),
            p("agy", Some("1.0.0")),
        ]);
        let state: Vec<&str> = st.iter().map(|s| s.state.as_str()).collect();
        assert_eq!(
            state,
            ["ok", "version_mismatch", "frozen", "missing", "replaced"]
        );
        assert_eq!(st[0].pinned.as_deref(), Some("2.1.319"));
        assert!(st[4].pinned.is_none());
    }
}
