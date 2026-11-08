# CLI stream fixtures – provenance

Directory names carry the CLI version they belong to; they must match
`agent::CLI_VERSIONS` and the pins in `docker/agent.Dockerfile`
(checked by `tests/cli_contract.rs::fixture_dirs_exist_for_pinned_versions`).

**recorded** = real output of the pinned CLI, captured on 2026-11-06 in the
development sandbox with `env -i` (empty `HOME`, no credentials passed from the
environment). Only temp paths were rewritten (`/workspace`, `/home/agent`,
socket path); everything else is byte-for-byte the CLI output.

**constructed** = written by hand. Event and field names are taken from the
schema embedded in the pinned binary/bundle (Claude Code: Zod schemas in
`/opt/claude-code/bin/claude` 2.1.319; Codex: strings in the 0.167.0 musl
binary; Gemini: `bundle/*.js` of 0.67.0) and from the shapes of the recorded
files. They are *not* live output – real streams may contain extra fields
(the parsers ignore unknown fields/types).

## claude-2.1.319

| File | Origin | Notes |
|---|---|---|
| `success_structured.jsonl` | recorded | `-p --output-format stream-json --verbose --permission-mode bypassPermissions --permission-prompts none --json-schema '{…"a"…}' --max-turns 3 --max-budget-usd 0.5 "say hi"`. Shows `structured_output` in the stream-json `result`, `StructuredOutput` tool call, `assistant.message.{id,usage}`, `modelUsage`, and two `rate_limit_event`s with `status:"allowed"` but `overageStatus:"rejected"`. |
| `max_turns.jsonl` | recorded | `--max-turns 1 --json-schema …` → `result.subtype:"error_max_turns"`, `errors[]`, no `result` field. |
| `max_budget.jsonl` | recorded | `--max-budget-usd 0.0001` → `result.subtype:"error_max_budget_usd"`. |
| `sigint_cancel.jsonl` | recorded | SIGINT to the CLI process while a foreground Bash tool (`python3 … sleep 30`) ran: CLI exited after ~2 s with exit code 0 and still emitted `result` (`error_during_execution`, `terminal_reason:"aborted_tools"`) incl. `modelUsage`. |
| `retry_then_success.jsonl` | constructed | `system/api_retry` (fields `attempt,max_retries,retry_delay_ms,error_status,error`) ×2, then success; duplicate assistant line with the same `message.id`. |
| `overloaded_terminal.jsonl` | constructed | 10× `api_retry` `overloaded`/529, assistant `error:"overloaded"`, error result with `api_error_status:529`. |
| `server_error_unknown.jsonl` | constructed | `api_retry` with `error:"unknown"`, `error_status:null` (no HTTP response), then `error_during_execution`. |
| `rate_limit_rejected.jsonl` | constructed | `rate_limit_event.rate_limit_info.status:"rejected"` with `resetsAt`, assistant `error:"rate_limit"`, error result. |
| `rate_limit_allowed_warning.jsonl` | constructed | `status:"allowed_warning"` → must not pause anything. |
| `auth_failed.jsonl` | constructed | assistant `error:"authentication_failed"` ("Invalid API key · Please run /login"). |
| `model_not_found.jsonl` | constructed | assistant `error:"model_not_found"`, `api_error_status:404`. |
| `model_usage_subagent.jsonl` | constructed | `modelUsage` with a second (subagent) model; `usage` covers only the main model. |

## codex-0.167.0

| File | Origin | Notes |
|---|---|---|
| `reconnect_network.jsonl` | recorded | `codex exec --json --skip-git-repo-check -` without network (DNS fails); stopped by `timeout 60`. `error` events "Reconnecting... n/5 (…)" and "Reconnecting... waiting for network (…)", plus an `item.completed` of type `error` that is only a transport notice. (A second recording through the sandbox proxy was refused with HTTP 403 for api.openai.com, so no real 401/usage-limit stream could be captured.) |
| `success_structured.jsonl` | constructed | `command_execution`, `file_change`, final `agent_message` carrying the JSON contract (`--output-schema`), `turn.completed.usage` with `cached_input_tokens`. |
| `usage_limit.jsonl` | constructed | Text "You’ve hit your usage limit. Upgrade to Plus …" (string present in the 0.167.0 binary, typographic apostrophe) in `error` + `turn.failed`. |
| `stream_failed_terminal.jsonl` | constructed | last reconnect attempts, then `turn.failed` "stream disconnected before completion: …" (string in binary). |
| `auth_401.jsonl` | constructed | `turn.failed` "unexpected status 401 Unauthorized: …". |

## gemini-0.67.0

| File | Origin | Notes |
|---|---|---|
| `api_key_invalid.jsonl` | recorded | `gemini --output-format stream-json -p …` with `GEMINI_API_KEY=invalid-dummy-key` through the sandbox proxy: `result.status:"error"`, `error.type:"unknown"`, message "API key not valid … API_KEY_INVALID" (HTTP 400); exit code 144. |
| `success_tools.jsonl` | constructed | `init`, delta `message`s, `tool_use`/`tool_result`, `result.stats` with `cached`. |
| `warning_then_success.jsonl` | constructed | `error` with `severity:"warning"` (emitted e.g. for loop detection in the bundle). |
| `turn_limit.jsonl` | constructed | `result.error.type:"FatalTurnLimitedError"` (exit code 53 in the bundle) from `model.maxSessionTurns`. |
| `quota_429.jsonl` | constructed | quota text ("exhausted your capacity", `RESOURCE_EXHAUSTED`). Gemini 0.67.0 has no structured retry/limit event in stream-json; classification is a text fallback on the error channel. |
| `network_transient.jsonl` | constructed | `error` severity `error` + `result.error` "fetch failed: read ECONNRESET". |

## opencode-1.19.5

All **constructed**: opencode could not be executed in this environment
(the auto-mode policy forbids running downloaded binaries). Event and field
names come from the JavaScript bundled in `opencode-linux-x64@1.19.5`
(`npm pack`, binary grepped as data on 2026-11-08): the `run` command writes
`JSON.stringify({type, timestamp, sessionID, ...})` for `tool_use`
(completed/error tool parts), `step_start`, `step_finish` (part with
`tokens:{input,output,reasoning,cache:{read,write}}` and `cost`), `text`
(finished text parts), `reasoning` and `error` (`{error}` of `session.error`,
NamedError shape `{name, data}` with `ProviderAuthError{providerID,message}`,
`APIError{message,statusCode,isRetryable,…}`, `ContextOverflowError{message}`).
Ids/timestamps are invented.

| File | Origin | Notes |
|---|---|---|
| `success_tools.jsonl` | constructed | two steps with `read`/`bash` tools, final text, `step_finish` usage incl. reasoning and cache read/write. |
| `rate_limit_429.jsonl` | constructed | `APIError` with `statusCode:429` → rate limit. |
| `auth_error.jsonl` | constructed | `ProviderAuthError` → provider_auth (pause). |
| `server_error_503.jsonl` | constructed | `APIError` 503 → transient. |
| `retryable_without_status.jsonl` | constructed | `APIError` without status, `isRetryable:true` → transient. |
| `context_overflow.jsonl` | constructed | `ContextOverflowError` → task failure (no provider cause). |
| `tool_error_then_text.jsonl` | constructed | tool part with `status:"error"` is a notice, not a failure. |
