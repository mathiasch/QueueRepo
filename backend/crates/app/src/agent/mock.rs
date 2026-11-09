//! Deterministic fake agent for development/tests (ENABLE_MOCK_PROVIDER=true).
//!
//! Prompt keywords: MOCK_FAIL, MOCK_ASK, MOCK_FOLLOWUPS, MOCK_LIMIT, MOCK_SLOW,
//! MOCK_TRANSIENT (fails until the re-queue note is in the prompt),
//! MOCK_TRANSIENT_ALWAYS, MOCK_AUTH, MOCK_STRUCTURED, MOCK_STRUCTURED_BAD,
//! MOCK_TOKENS (usage lines with pauses), MOCK_TURNS (fails if a turn limit is
//! passed), MOCK_TRAP (waits for SIGINT and flushes usage), MOCK_STDERR429,
//! MOCK_REVIEW_LIMIT (the review run hits a rate limit), MOCK_QUIET (6 s
//! without any output after the first usage line, then success),
//! MOCK_USAGE_TOTAL (final usage report far above the live deltas, like
//! codex/gemini that report usage only at the end), MOCK_SOLUTION_B64:<b64>
//! (runs the decoded shell script in the workspace – eval set, see `eval.rs`),
//! MOCK_ENV (writes the names of its environment variables to MOCK_ENV.txt),
//! MOCK_NET:<url> (fetches the URL through the run's egress proxy, WP02),
//! WP03: `MOCK_SECRET ` (keyword followed by a space) or MOCK_SECRET_FILE (writes a freshly generated fake GitHub token into
//! `config/credentials.py`), MOCK_SECRET_LOG (prints such a token),
//! MOCK_SELF_COMMIT (commits a fake token itself), MOCK_EDIT_TEST (changes
//! the existing `tests/test_core.py`), MOCK_SKIP_TEST (adds a skipped test),
//! MOCK_REVIEW_DOWN (the review run always fails transiently).
use serde_json::Value;

use super::{
    classify::ErrorCategory, error_event, n, s, AgentEvent, Invocation, LimitKind, RetryInfo,
    RunLimits,
};
use crate::models::Usage;

const MOCK_SCRIPT: &str = r#"P=$(cat); mkdir -p .queue
echo '{"type":"text","text":"mock agent: reading task"}'
echo '{"type":"tool","text":"edit MOCK_LOG.md"}'
echo "run $(date -u +%FT%TZ)" >> MOCK_LOG.md
echo '{"type":"usage","input_tokens":1200,"output_tokens":150,"cache_read_tokens":800}'
case "$P" in
  *"strict code reviewer"*)
    case "$P" in
      *MOCK_REVIEW_LIMIT*) echo '{"type":"error","category":"rate_limit","text":"usage limit reached|'$(( $(date +%s) + 120 ))'"}'; exit 1;;
      *MOCK_REVIEW_DOWN*) echo '{"type":"error","category":"overloaded","text":"API Error: 529 overloaded"}'; exit 1;;
    esac
    echo '{"approved":true,"issues":[]}' > .queue/review.json
    echo '{"type":"final","text":"review ok"}'; exit 0;;
esac
case "$P" in
  *MOCK_SLOW*) echo '{"type":"text","text":"working slowly"}'; sleep 30;;
  *MOCK_QUIET*) sleep 6;;
  *MOCK_USAGE_TOTAL*) echo '{"type":"usage_total","input_tokens":4000,"output_tokens":500}';;
  *MOCK_TRAP*)
    trap 'echo "{\"type\":\"usage\",\"input_tokens\":1200,\"output_tokens\":150}"; echo "{\"type\":\"text\",\"text\":\"interrupted cleanly\"}"; echo "{\"type\":\"final\",\"text\":\"interrupted cleanly\"}"; exit 130' INT
    echo '{"type":"text","text":"waiting for SIGINT"}'; sleep 30 & wait $!;;
  *MOCK_TOKENS*)
    echo '{"type":"usage","input_tokens":1200,"output_tokens":150,"cost_usd":0.006}'; sleep 2
    echo '{"type":"usage","input_tokens":1200,"output_tokens":150,"cost_usd":0.006}'; sleep 2;;
esac
case "$P" in
  *MOCK_TRANSIENT_ALWAYS*) echo '{"type":"retry","category":"overloaded","attempt":1,"max":3,"delay_ms":10,"status":529}'
    echo '{"type":"error","category":"overloaded","text":"API Error: 529 overloaded"}'; exit 1;;
  *MOCK_TRANSIENT*) case "$P" in *"transient provider error"*) ;;
    *) echo '{"type":"retry","category":"overloaded","attempt":1,"max":3,"delay_ms":10,"status":529}'
       echo '{"type":"error","category":"overloaded","text":"API Error: 529 overloaded"}'; exit 1;; esac;;
  *MOCK_AUTH*) echo '{"type":"error","category":"authentication_failed","text":"Invalid API key"}'; exit 1;;
  *MOCK_TURNS*) if [ -n "$QUEUE_MAX_TURNS" ]; then echo '{"type":"limit","kind":"turns"}'; exit 1; fi;;
esac
case "$P" in
  *MOCK_ENV*) env | cut -d= -f1 | sort > MOCK_ENV.txt;;
esac
fake_token() { printf 'gh%s_%s' p "$(head -c 400 /dev/urandom | tr -dc 'A-Za-z0-9' | head -c 36)"; }
case "$P" in
  *MOCK_SECRET_LOG*) echo '{"type":"text","text":"found key '"$(fake_token)"' in env"}';;
esac
case "$P" in
  *MOCK_SECRET_FILE*|*"MOCK_SECRET "*) mkdir -p config; echo "GITHUB_TOKEN = \"$(fake_token)\"" > config/credentials.py;;
esac
case "$P" in
  *MOCK_SELF_COMMIT*) echo "token: $(fake_token)" > leaked.txt; git add leaked.txt; git -c user.name=a -c user.email=a@x commit -q -m "agent commit";;
esac
case "$P" in
  *MOCK_EDIT_TEST*) mkdir -p tests; echo 'WEAKENED = True' >> tests/test_core.py;;
  *MOCK_SKIP_TEST*) mkdir -p tests; printf 'import pytest\n\n@pytest.mark.skip\ndef test_later():\n    assert False\n' > tests/test_skip.py;;
esac
case "$P" in
  *MOCK_NET:*) U=$(printf '%s\n' "$P" | sed -n 's/.*MOCK_NET:\([^ ]*\).*/\1/p' | head -n 1)
    C=$(curl -s -o /dev/null -w '%{http_code}/%{http_connect}' "$U" 2>/dev/null || true)
    echo '{"type":"text","text":"net '"$C"'"}';;
esac
case "$P" in
  *MOCK_SOLUTION_B64:*) printf '%s\n' "$P" | sed -n 's/.*MOCK_SOLUTION_B64:\([A-Za-z0-9+/=]*\).*/\1/p' | head -n 1 | base64 -d | sh
    echo '{"type":"tool","text":"eval: applied reference solution"}';;
esac
case "$P" in
  *MOCK_LIMIT*) echo '{"type":"error","text":"Claude AI usage limit reached|'$(( $(date +%s) + 120 ))'"}'; exit 1;;
  *MOCK_STDERR429*) echo '{"status":"failed","summary":"mock failure","handoff":"tried X, failed"}' > .queue/result.json
    echo "npm ERR! HTTP 429 quota exceeded" >&2; exit 1;;
  *MOCK_FAIL*) echo '{"status":"failed","summary":"mock failure","handoff":"tried X, failed"}' > .queue/result.json;;
  *MOCK_ASK*) case "$P" in *"Answers from user"*) echo '{"status":"done","summary":"answered","handoff":"used answer"}' > .queue/result.json;;
              *) echo '{"status":"needs_input","summary":"need decision","questions":["Postgres or SQLite?"]}' > .queue/result.json;; esac;;
  *MOCK_FOLLOWUPS*) echo '{"status":"done","summary":"planned","handoff":"plan ready","follow_up_tasks":[{"title":"A","prompt":"do A"},{"title":"B","prompt":"do B"},{"title":"C","prompt":"do C"}]}' > .queue/result.json;;
  *MOCK_STRUCTURED_BAD*) echo '{"status":"done","summary":"file ok","handoff":"from file"}' > .queue/result.json
    echo '{"type":"structured","value":{"status":42}}';;
  *MOCK_STRUCTURED*) echo '{"status":"failed","summary":"file must lose","handoff":"x"}' > .queue/result.json
    echo '{"type":"structured","value":{"status":"done","summary":"structured ok","handoff":"h","questions":[],"follow_up_tasks":[]}}';;
  *) echo '{"status":"done","summary":"mock done","handoff":"mock handoff"}' > .queue/result.json;;
esac
echo '{"type":"final","text":"finished"}'"#;

pub(super) fn invocation(inv: &mut Invocation, limits: &RunLimits) {
    inv.program = "sh".into();
    inv.args = vec!["-c".into(), MOCK_SCRIPT.into()];
    // Analogue of the CLI flags, so integration tests can see what was passed.
    if let Some(t) = limits.turns {
        inv.env.push(("QUEUE_MAX_TURNS".into(), t.to_string()));
    }
    if let Some(c) = limits.cost_usd {
        inv.env
            .push(("QUEUE_MAX_COST_USD".into(), format!("{c:.4}")));
    }
}

pub(super) fn parse(v: &Value) -> Vec<AgentEvent> {
    let status = v["status"].as_u64().and_then(|x| u16::try_from(x).ok());
    match s(v, "type").as_str() {
        "text" => vec![AgentEvent::Text(s(v, "text"))],
        "tool" => vec![AgentEvent::Tool(s(v, "text"))],
        "final" => vec![AgentEvent::Final(s(v, "text"))],
        "error" => match v["category"].as_str() {
            Some(c) => {
                let category = match ErrorCategory::from_claude(c, status) {
                    // The mock encodes resets like the old Claude text: "…|epoch".
                    ErrorCategory::RateLimit(None) => {
                        ErrorCategory::RateLimit(super::parse_reset(&s(v, "text")))
                    }
                    other => other,
                };
                vec![AgentEvent::Failure {
                    category,
                    message: s(v, "text"),
                }]
            }
            None => vec![error_event(&s(v, "text"))],
        },
        "retry" => vec![AgentEvent::Retry(RetryInfo {
            category: ErrorCategory::from_claude(&s(v, "category"), status),
            attempt: super::count(n(v, "attempt")),
            max: super::count(n(v, "max")),
            delay_ms: n(v, "delay_ms") as u64,
            status,
        })],
        "structured" => vec![AgentEvent::Structured(v["value"].clone())],
        "limit" => match s(v, "kind").as_str() {
            "turns" => vec![AgentEvent::LimitHit(LimitKind::Turns)],
            "cost" => vec![AgentEvent::LimitHit(LimitKind::Cost)],
            _ => vec![],
        },
        "usage" | "usage_total" => {
            let u = Usage {
                input_tokens: n(v, "input_tokens"),
                output_tokens: n(v, "output_tokens"),
                cache_read_tokens: n(v, "cache_read_tokens"),
                cost_usd: super::usd(&v["cost_usd"]),
                ..Default::default()
            };
            vec![if s(v, "type") == "usage" {
                AgentEvent::UsageDelta(u)
            } else {
                AgentEvent::UsageTotal(u)
            }]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::super::{invocation as inv, parse_line, Credential, ParseState, RunKind};
    use super::*;

    #[test]
    fn limits_are_passed_as_env() {
        let c = Credential {
            kind: "none".into(),
            secret: String::new(),
            conn: None,
        };
        let i = inv(
            "mock",
            None,
            &c,
            &RunLimits {
                turns: Some(3),
                cost_usd: Some(0.5),
            },
            RunKind::Agent,
        )
        .unwrap();
        assert!(i.env.contains(&("QUEUE_MAX_TURNS".into(), "3".into())));
        assert!(i
            .env
            .contains(&("QUEUE_MAX_COST_USD".into(), "0.5000".into())));
        let i = inv("mock", None, &c, &RunLimits::default(), RunKind::Agent).unwrap();
        assert!(i.env.is_empty());
    }

    #[test]
    fn mock_events() {
        let mut st = ParseState::default();
        let ev = parse_line(
            "mock",
            r#"{"type":"error","category":"overloaded","text":"API Error: 529 overloaded","status":529}"#,
            &mut st,
        );
        assert!(matches!(
            &ev[0],
            AgentEvent::Failure {
                category: ErrorCategory::Overloaded,
                ..
            }
        ));
        let ev = parse_line(
            "mock",
            r#"{"type":"error","text":"Claude AI usage limit reached|1900000000"}"#,
            &mut st,
        );
        assert!(matches!(
            &ev[0],
            AgentEvent::Failure { category: ErrorCategory::RateLimit(Some(t)), .. } if t.timestamp() == 1_900_000_000
        ));
        let ev = parse_line(
            "mock",
            r#"{"type":"error","category":"rate_limit","text":"usage limit reached|1900000000"}"#,
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
            "mock",
            r#"{"type":"usage","input_tokens":1,"output_tokens":2,"cost_usd":0.25}"#,
            &mut st,
        );
        assert!(matches!(&ev[0], AgentEvent::UsageDelta(u) if u.cost_usd == 0.25));
        assert_eq!(
            parse_line("mock", r#"{"type":"limit","kind":"turns"}"#, &mut st),
            vec![AgentEvent::LimitHit(LimitKind::Turns)]
        );
    }
}
