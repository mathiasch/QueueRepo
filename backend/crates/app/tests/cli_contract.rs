//! Contract tests: the stream parsers against fixtures of exactly the CLI
//! versions pinned in docker/agent.Dockerfile (`agent::CLI_VERSIONS`).
//! Provenance of every fixture (recorded vs. constructed): fixtures/cli/PROVENANCE.md.
//! No database needed.
use std::path::PathBuf;

use queued::{
    agent::{parse_line, AgentEvent, ErrorCategory, LimitKind, ParseState, CLI_VERSIONS},
    models::{FailureClass, Usage},
};

fn dir(provider: &str) -> PathBuf {
    let v = CLI_VERSIONS.iter().find(|(p, _)| *p == provider).unwrap().1;
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/cli/{provider}-{v}"))
}

/// Replays a fixture; returns the events and the failure class they imply
/// (CLI limit > categorised failure > plain error).
fn run_fixture(provider: &str, name: &str) -> (Vec<AgentEvent>, Option<FailureClass>) {
    let path = dir(provider).join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let mut st = ParseState::default();
    let evs: Vec<AgentEvent> = text
        .lines()
        .flat_map(|l| parse_line(provider, l, &mut st))
        .collect();
    let class = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::LimitHit(LimitKind::Turns) => Some(FailureClass::LimitTurns),
            AgentEvent::LimitHit(LimitKind::Cost) => Some(FailureClass::LimitCost),
            _ => None,
        })
        .or_else(|| {
            evs.iter().find_map(|e| match e {
                AgentEvent::Failure { category, .. } => Some(category.class()),
                _ => None,
            })
        })
        .or_else(|| {
            evs.iter()
                .any(|e| matches!(e, AgentEvent::Error(_)))
                .then_some(FailureClass::Task)
        });
    (evs, class)
}

fn total(evs: &[AgentEvent]) -> Option<Usage> {
    evs.iter().rev().find_map(|e| match e {
        AgentEvent::UsageTotal(u) => Some(u.clone()),
        _ => None,
    })
}

fn deltas(evs: &[AgentEvent]) -> Usage {
    let mut u = Usage::default();
    for e in evs {
        if let AgentEvent::UsageDelta(d) = e {
            u.add(d);
        }
    }
    u
}

fn count(evs: &[AgentEvent], f: impl Fn(&AgentEvent) -> bool) -> usize {
    evs.iter().filter(|e| f(e)).count()
}

#[test]
fn fixture_dirs_exist_for_pinned_versions() {
    for (p, _) in CLI_VERSIONS {
        let d = dir(p);
        assert!(d.is_dir(), "missing fixtures for pinned {p}: {d:?}");
        assert!(std::fs::read_dir(&d).unwrap().count() >= 5, "{d:?}");
    }
    let prov = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli/PROVENANCE.md"),
    )
    .unwrap();
    for (p, _) in CLI_VERSIONS {
        for f in std::fs::read_dir(dir(p)).unwrap() {
            let name = f.unwrap().file_name().into_string().unwrap();
            assert!(prov.contains(&name), "{name} has no provenance entry");
        }
    }
}

// ---- Claude 2.1.319 -------------------------------------------------------

#[test]
fn claude_success_structured_recorded() {
    let (evs, class) = run_fixture("claude", "success_structured");
    assert_eq!(class, None, "{evs:?}");
    let s = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::Structured(v) => Some(v.clone()),
            _ => None,
        })
        .expect("structured_output in stream-json result");
    assert_eq!(s["a"], "Hi! How can I help you today?");
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Tool(t) if t.starts_with("StructuredOutput"))));
    let t = total(&evs).unwrap();
    assert_eq!(
        (
            t.input_tokens,
            t.output_tokens,
            t.cache_read_tokens,
            t.cache_write_tokens
        ),
        (6, 74, 13554, 20722),
        "modelUsage"
    );
    assert!((t.cost_usd - 0.0849954).abs() < 1e-9);
    // Live deltas: two assistant messages with distinct ids.
    let d = deltas(&evs);
    assert_eq!((d.input_tokens, d.cache_write_tokens), (6, 20722));
    assert!(matches!(evs.last().unwrap(), AgentEvent::Final(_)));
    // Two real rate_limit_events with status "allowed" (overage "rejected") → nothing.
    assert_eq!(count(&evs, |e| matches!(e, AgentEvent::Failure { .. })), 0);
}

#[test]
fn claude_max_turns_recorded() {
    let (evs, class) = run_fixture("claude", "max_turns");
    assert_eq!(class, Some(FailureClass::LimitTurns), "{evs:?}");
    assert!(total(&evs).unwrap().cost_usd > 0.0);
}

#[test]
fn claude_max_budget_recorded() {
    let (evs, class) = run_fixture("claude", "max_budget");
    assert_eq!(class, Some(FailureClass::LimitCost), "{evs:?}");
}

#[test]
fn claude_sigint_flushes_result_with_usage_recorded() {
    let (evs, class) = run_fixture("claude", "sigint_cancel");
    // No provider category: the run outcome comes from the exit frame (cancel).
    assert_eq!(class, Some(FailureClass::Task));
    assert_eq!(count(&evs, |e| matches!(e, AgentEvent::Failure { .. })), 0);
    let t = total(&evs).unwrap();
    assert_eq!((t.output_tokens, t.cache_write_tokens), (152, 5620));
    assert!(t.cost_usd > 0.02);
}

#[test]
fn claude_retry_then_success() {
    let (evs, class) = run_fixture("claude", "retry_then_success");
    assert_eq!(class, None, "{evs:?}");
    let retries: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Retry(r) => Some((r.attempt, r.max, r.status, r.category.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        retries,
        vec![
            (1, 10, Some(529), ErrorCategory::Overloaded),
            (2, 10, Some(529), ErrorCategory::Overloaded)
        ]
    );
    // Duplicate assistant line with the same message id counts once.
    let d = deltas(&evs);
    assert_eq!(
        (d.input_tokens, d.output_tokens, d.cache_write_tokens),
        (5, 52, 500)
    );
}

#[test]
fn claude_overloaded_terminal() {
    let (evs, class) = run_fixture("claude", "overloaded_terminal");
    assert_eq!(class, Some(FailureClass::ProviderTransient));
    assert_eq!(count(&evs, |e| matches!(e, AgentEvent::Retry(_))), 10);
}

#[test]
fn claude_connection_error_after_retry_is_transient() {
    let (_, class) = run_fixture("claude", "server_error_unknown");
    assert_eq!(class, Some(FailureClass::ProviderTransient));
}

#[test]
fn claude_rate_limit_rejected() {
    let (evs, class) = run_fixture("claude", "rate_limit_rejected");
    assert_eq!(class, Some(FailureClass::RateLimit));
    assert!(evs.iter().any(|e| matches!(
        e,
        AgentEvent::Failure { category: ErrorCategory::RateLimit(Some(t)), .. } if t.timestamp() == 1_900_000_000
    )));
}

#[test]
fn claude_rate_limit_allowed_warning_is_no_failure() {
    let (evs, class) = run_fixture("claude", "rate_limit_allowed_warning");
    assert_eq!(class, None, "{evs:?}");
}

#[test]
fn claude_auth_failed() {
    let (_, class) = run_fixture("claude", "auth_failed");
    assert_eq!(class, Some(FailureClass::ProviderAuth));
}

#[test]
fn claude_model_not_found() {
    let (_, class) = run_fixture("claude", "model_not_found");
    assert_eq!(class, Some(FailureClass::Config));
}

#[test]
fn claude_model_usage_includes_subagent() {
    let (evs, _) = run_fixture("claude", "model_usage_subagent");
    let t = total(&evs).unwrap();
    assert_eq!(
        (
            t.input_tokens,
            t.output_tokens,
            t.cache_read_tokens,
            t.cache_write_tokens
        ),
        (4015, 928, 21000, 4000),
        "sum over models incl. the subagent's model, not `usage`"
    );
    assert!((t.cost_usd - 0.31).abs() < 1e-9);
}

// ---- Codex 0.167.0 --------------------------------------------------------

#[test]
fn codex_reconnect_recorded_is_retry_not_failure() {
    let (evs, class) = run_fixture("codex", "reconnect_network");
    assert_eq!(class, None, "{evs:?}");
    assert_eq!(count(&evs, |e| matches!(e, AgentEvent::Retry(_))), 8);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Retry(r) if r.attempt == 5 && r.max == 5)));
    // Transport notice item of type "error" is informational.
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Text(t) if t.contains("Falling back from WebSockets"))));
}

#[test]
fn codex_success_structured_final_message() {
    let (evs, class) = run_fixture("codex", "success_structured");
    assert_eq!(class, None);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Structured(v) if v["status"] == "done")));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Tool(t) if t.contains("cargo test"))));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Tool(t) if t == "edit src/lib.rs")));
    let t = total(&evs).unwrap();
    assert_eq!(
        (t.input_tokens, t.cache_read_tokens, t.output_tokens),
        (4000, 20000, 900)
    );
}

#[test]
fn codex_usage_limit() {
    let (evs, class) = run_fixture("codex", "usage_limit");
    assert_eq!(class, Some(FailureClass::RateLimit));
    assert!(evs.iter().any(|e| matches!(
        e,
        AgentEvent::Failure {
            category: ErrorCategory::RateLimit(Some(_)),
            ..
        }
    )));
}

#[test]
fn codex_stream_failed_is_transient() {
    let (evs, class) = run_fixture("codex", "stream_failed_terminal");
    assert_eq!(class, Some(FailureClass::ProviderTransient));
    assert_eq!(count(&evs, |e| matches!(e, AgentEvent::Retry(_))), 2);
}

#[test]
fn codex_auth_401() {
    let (_, class) = run_fixture("codex", "auth_401");
    assert_eq!(class, Some(FailureClass::ProviderAuth));
}

// ---- Gemini 0.67.0 --------------------------------------------------------

#[test]
fn gemini_api_key_invalid_recorded() {
    let (evs, class) = run_fixture("gemini", "api_key_invalid");
    assert_eq!(class, Some(FailureClass::ProviderAuth), "{evs:?}");
}

#[test]
fn gemini_success_with_tools() {
    let (evs, class) = run_fixture("gemini", "success_tools");
    assert_eq!(class, None);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Tool(t) if t == "run_shell_command: npm test")));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Final(t) if t == "Tests pass.")));
    let t = total(&evs).unwrap();
    assert_eq!(
        (t.input_tokens, t.cache_read_tokens, t.output_tokens),
        (1000, 4000, 300)
    );
}

#[test]
fn gemini_warning_is_not_error() {
    let (evs, class) = run_fixture("gemini", "warning_then_success");
    assert_eq!(class, None, "{evs:?}");
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Text(t) if t.starts_with('⚠'))));
}

#[test]
fn gemini_turn_limit() {
    let (_, class) = run_fixture("gemini", "turn_limit");
    assert_eq!(class, Some(FailureClass::LimitTurns));
}

#[test]
fn gemini_quota_is_rate_limit() {
    let (_, class) = run_fixture("gemini", "quota_429");
    assert_eq!(class, Some(FailureClass::RateLimit));
}

#[test]
fn gemini_network_error_is_transient() {
    let (_, class) = run_fixture("gemini", "network_transient");
    assert_eq!(class, Some(FailureClass::ProviderTransient));
}

// --- opencode 1.19.5 (constructed from the bundled source, see PROVENANCE.md) ---

#[test]
fn opencode_success_with_tools_and_usage() {
    let (evs, class) = run_fixture("opencode", "success_tools");
    assert_eq!(class, None);
    assert_eq!(count(&evs, |e| matches!(e, AgentEvent::Tool(_))), 2);
    assert!(evs.contains(&AgentEvent::Final("Tests laufen, Fix ist drin.".into())));
    let u = deltas(&evs);
    assert_eq!(
        (
            u.input_tokens,
            u.output_tokens,
            u.cache_read_tokens,
            u.cache_write_tokens
        ),
        (6000, 210, 5300, 200),
        "reasoning counts as output"
    );
    assert!((u.cost_usd - 0.0034).abs() < 1e-9);
    assert!(total(&evs).is_none(), "opencode reports per step only");
}

#[test]
fn opencode_provider_failures_classify() {
    assert_eq!(
        run_fixture("opencode", "rate_limit_429").1,
        Some(FailureClass::RateLimit)
    );
    assert_eq!(
        run_fixture("opencode", "auth_error").1,
        Some(FailureClass::ProviderAuth)
    );
    assert_eq!(
        run_fixture("opencode", "server_error_503").1,
        Some(FailureClass::ProviderTransient)
    );
    assert_eq!(
        run_fixture("opencode", "retryable_without_status").1,
        Some(FailureClass::ProviderTransient)
    );
    let (evs, class) = run_fixture("opencode", "context_overflow");
    assert_eq!(class, Some(FailureClass::Task));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Error(t) if t.contains("too long"))));
    let (evs, class) = run_fixture("opencode", "tool_error_then_text");
    assert_eq!(class, None, "a failed tool call is not a run failure");
    assert!(!evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Failure { .. } | AgentEvent::Error(_))));
}
