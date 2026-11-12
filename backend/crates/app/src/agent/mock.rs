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
//! WP04: MOCK_FILE:<name> (the run line goes to `<name>` instead of
//! `MOCK_LOG.md`; every run line is unique, so two tasks writing the same
//! file conflict in the merge queue and different files do not),
//! MOCK_LONG (like MOCK_SLOW, 300 s – long UI walkthroughs),
//! MOCK_RESOLVE (removes git conflict markers, keeping both sides – a fix
//! task inherits the keyword from the original prompt).
//! WP06: sessions in `$HOME/.mock-sessions/<id>` (`QUEUE_SESSION_ID`,
//! `QUEUE_RESUME`; an unknown id fails like Claude's "No conversation
//! found"); a resumed run only logs and finishes (MOCK_STEER_SLOW: slowly).
//! Plan mode (`QUEUE_PLAN_MODE=1`) writes nothing and answers a plan with
//! MOCKPLAN-STEP-n lines (MOCK_PLAN_WRITE: tries to write a file anyway,
//! MOCK_PLAN_ASK: structured plan with a question, MOCK_PLAN_EMPTY: no plan,
//! MOCK_PLAN_SLOW: 30 s). Tokens MOCKMSG*/MOCKPLAN*/MOCKNOTE*/MOCKCOMMENT*
//! in a prompt are appended to `SEEN.md` (`SEEN-<file>` with MOCK_FILE, so
//! parallel tasks do not conflict) – what reached the agent.
//! MOCK_REWRITE / MOCK_PREPEND: once answered (MOCK_ASK), the run line
//! replaces the file / goes on top (changes the diff under inline comments).
//! WP09: MOCKRULE* tokens (from the project rules) are recorded in `SEEN.md`
//! too; MOCK_SEE_ATTACHMENTS (sha256 of `.queue/attachments/*` into
//! `SEEN-attachments.txt`), MOCK_COMMIT_ATTACHMENT (un-ignores `.queue/`,
//! empties the exclude file and commits `.queue/` itself),
//! MOCK_STATE_B64:<b64> / MOCK_KNOWLEDGE_B64:<b64> (write the decoded text
//! to `.queue/state.json` / `.queue/knowledge.md`), MOCK_RESULT_B64:<b64>
//! (the decoded JSON is the result contract), MOCK_ONEMPTY_B64:<b64> (the
//! same, only in OnEmpty maintenance runs – e.g. in the project goal).
use serde_json::Value;

use super::{
    classify::ErrorCategory, error_event, n, s, AgentEvent, Invocation, LimitKind, RetryInfo,
    RunKind, RunLimits, Session,
};
use crate::models::Usage;

const MOCK_SCRIPT: &str = r#"P=$(cat)
if [ -n "$QUEUE_RESUME" ]; then
  if [ ! -f "$HOME/.mock-sessions/$QUEUE_RESUME" ]; then
    echo "No conversation found with session ID: $QUEUE_RESUME" >&2
    echo '{"type":"error","text":"No conversation found with session ID: '"$QUEUE_RESUME"'"}'; exit 1
  fi
  SID=$QUEUE_RESUME
else
  SID=${QUEUE_SESSION_ID:-mock-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')}
fi
mkdir -p "$HOME/.mock-sessions"; N=0
[ -f "$HOME/.mock-sessions/$SID" ] && N=$(wc -l < "$HOME/.mock-sessions/$SID" | tr -d ' ')
printf '%s\n' "$P" | head -c 200 | tr '\n' ' ' >> "$HOME/.mock-sessions/$SID"; echo >> "$HOME/.mock-sessions/$SID"
echo '{"type":"session","id":"'"$SID"'"}'
if [ "$QUEUE_PLAN_MODE" = 1 ]; then
  echo '{"type":"text","text":"mock agent: planning (read-only)"}'
  [ -e PLAN_SIDE_EFFECT.txt ] && echo '{"type":"text","text":"mock agent: saw a leftover"}'
  echo '{"type":"usage","input_tokens":900,"output_tokens":120}'
  case "$P" in *MOCK_PLAN_WRITE*) echo sneaky > PLAN_SIDE_EFFECT.txt 2>/dev/null; echo '{"type":"tool","text":"edit PLAN_SIDE_EFFECT.txt"}';; esac
  case "$P" in *MOCK_PLAN_SLOW*) sleep 30;; esac
  case "$P" in *MOCK_PLAN_EMPTY*) echo '{"type":"final","text":""}'; exit 0;; esac
  R=""; case "$P" in *"Revision requested"*) R=" (überarbeitet)";; esac
  case "$P" in
    *MOCK_PLAN_ASK*) printf '%s\n' '{"type":"structured","value":{"plan":"Plan'"$R"':\n1. MOCKPLAN-STEP-1 MOCK_LOG.md ergänzen","questions":["Postgres oder SQLite?"]}}';;
    *) printf '%s\n' '{"type":"final","text":"Plan'"$R"':\n1. MOCKPLAN-STEP-1 MOCK_LOG.md ergänzen\n2. MOCKPLAN-STEP-2 prüfen"}';;
  esac
  exit 0
fi
mkdir -p .queue
L=MOCK_LOG.md
case "$P" in *MOCK_FILE:*) L=$(printf '%s\n' "$P" | sed -n 's/.*MOCK_FILE:\([A-Za-z0-9_.-]*\).*/\1/p' | head -n 1);; esac
S=SEEN.md; [ "$L" != MOCK_LOG.md ] && S="SEEN-$L"
T=$(printf '%s\n' "$P" | grep -o 'MOCK\(MSG\|PLAN\|NOTE\|COMMENT\|RULE\)[A-Za-z0-9_-]*' | sort -u | tr '\n' ' ')
[ -n "$T" ] && echo "run ${QUEUE_RESUME:+resume }$T" >> "$S"
if [ -n "$QUEUE_RESUME" ]; then
  echo '{"type":"text","text":"mock agent: resumed session '"$SID"' after '"$N"' earlier prompts"}'
  echo '{"type":"usage","input_tokens":300,"output_tokens":40,"cache_read_tokens":1200}'
  echo "resumed $(date -u +%FT%TZ) $(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')" >> "$L"
  case "$P" in *MOCK_STEER_SLOW*) echo '{"type":"text","text":"working slowly"}'; sleep 30;; esac
  echo '{"status":"done","summary":"mock resumed","handoff":"resumed handoff"}' > .queue/result.json
  echo '{"type":"final","text":"resumed"}'; exit 0
fi
echo '{"type":"text","text":"mock agent: reading task"}'
echo '{"type":"tool","text":"edit '"$L"'"}'
R="run $(date -u +%FT%TZ) $(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
case "$P" in
  *MOCK_REWRITE*"Answers from user"*) echo "$R" > "$L";;
  *MOCK_PREPEND*"Answers from user"*) { echo "$R"; cat "$L"; } > "$L.new"; mv "$L.new" "$L";;
  *) echo "$R" >> "$L";;
esac
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
  *MOCK_LONG*) echo '{"type":"text","text":"working slowly"}'; sleep 300;;
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
  *MOCK_RESOLVE*) for f in $(grep -rl --exclude-dir=.git --exclude-dir=.queue '^<<<<<<< ' . 2>/dev/null); do
      sed -i '/^<<<<<<< /d;/^=======$/d;/^>>>>>>> /d' "$f"; echo '{"type":"tool","text":"resolved '"$f"'"}'; done;;
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
b64arg() { printf '%s\n' "$P" | sed -n "s/.*$1:\([A-Za-z0-9+/=]*\).*/\1/p" | head -n 1 | base64 -d; }
case "$P" in
  *MOCK_SEE_ATTACHMENTS*) if [ -d .queue/attachments ]; then (cd .queue/attachments && sha256sum * 2>/dev/null) > SEEN-attachments.txt; else echo none > SEEN-attachments.txt; fi;;
esac
case "$P" in
  *MOCK_COMMIT_ATTACHMENT*) printf '!.queue/\n!.queue/**\n' >> .gitignore; : > .git/info/exclude
    echo "work with attachment" >> "$L"; git add -f -A .queue "$L" .gitignore
    git -c user.name=a -c user.email=a@x commit -q -m "agent commits its attachments"
    echo '{"type":"tool","text":"git commit (with .queue/)"}';;
esac
case "$P" in *MOCK_STATE_B64:*) b64arg MOCK_STATE_B64 > .queue/state.json;; esac
case "$P" in *MOCK_KNOWLEDGE_B64:*) b64arg MOCK_KNOWLEDGE_B64 > .queue/knowledge.md;; esac
case "$P" in
  *"Maintenance run (the task queue is empty)"*MOCK_ONEMPTY_B64:*) b64arg MOCK_ONEMPTY_B64 > .queue/result.json; echo '{"type":"final","text":"finished"}'; exit 0;;
  *MOCK_RESULT_B64:*) b64arg MOCK_RESULT_B64 > .queue/result.json; echo '{"type":"final","text":"finished"}'; exit 0;;
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

pub(super) fn invocation(
    inv: &mut Invocation,
    limits: &RunLimits,
    kind: RunKind,
    session: &Session,
) {
    inv.program = "sh".into();
    inv.args = vec!["-c".into(), MOCK_SCRIPT.into()];
    if kind == RunKind::Plan {
        inv.env.push(("QUEUE_PLAN_MODE".into(), "1".into()));
    }
    match session {
        Session::Start(id) => inv.env.push(("QUEUE_SESSION_ID".into(), id.clone())),
        Session::Resume(id) => inv.env.push(("QUEUE_RESUME".into(), id.clone())),
        Session::None => {}
    }
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
