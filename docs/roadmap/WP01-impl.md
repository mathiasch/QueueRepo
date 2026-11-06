# WP01 – Strukturierte CLI-Integration & Fehlerklassifikation: Umsetzung

Status: umgesetzt (Umsetzer-Agent), Branch `mvp`, nicht committet. Grundlage: `WP01-plan.md`.

## 1. Testläufe (2026-11-06, alle in dieser Sitzung ausgeführt)

| Befehl | Ergebnis |
|---|---|
| `cd backend && cargo fmt --all -- --check` | Exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit 0 |
| `DATABASE_URL=postgres://queued:queued@localhost/queued cargo test --workspace` | Exit 0 – **162 Tests**: app lib 70, `tests/api.rs` 19, `tests/cli_contract.rs` 24, `tests/failures.rs` 18, common 6, runner 25 (vorher 72: 31/19/–/–/3/19) |
| `cargo test -p queued --test failures --test api` zusätzlich 2× wiederholt | 3/3 Läufe grün (Flakiness-Check der zeitbasierten Szenarien) |
| `cd frontend && npm run build && npm test` | tsc + vite build ok; Vitest **11/11** (vorher 6) |
| `cd e2e && npx playwright test` | **23/23 passed** (system 7, desktop 10, iphone 3, iphone-landscape 3; vorher 17), zweimal ausgeführt, beide grün |

Bestehende Tests wurden nicht verändert oder abgeschwächt; alle 19 Tests in `tests/api.rs` und alle 17 alten Playwright-Tests laufen unverändert.

Manuell zusätzlich (nicht automatisiert):
- **Docker-Engine** (A8): `dockerd` gestartet, echter `queued-runner` mit `RUNNER_ENGINE=docker`, Image `busybox:1.36`, Grace 3 s: Trap-Skript → `flushed-after-int`, Exit 130, `killed:false`, 2,3 s. `trap '' INT` → `killed:true`, 5,3 s (= 2 s Timeout + 3 s Grace). Danach kein Container übrig. **Podman ist nicht installiert → unverifiziert.**
- **Live-Aufnahmen**: Claude Code 2.1.319 ist hier (anders als im Plan angenommen) live erreichbar. 5 kleine Läufe aufgezeichnet (ca. 0,20 USD laut `total_cost_usd`), daraus 4 Fixtures. Codex 0.167.0 / Gemini 0.67.0 per `npm install` in den Scratchpad: Codex ohne Netz bzw. via Proxy (api.openai.com → HTTP 403 vom Proxy), Gemini mit Dummy-Key (echte 400-Antwort). Agent-Image selbst wurde **nicht** gebaut (die drei `--version | grep`-Checks des Dockerfiles wurden gegen die lokal installierten gepinnten CLIs geprüft: `2.1.319 (Claude Code)`, `codex-cli 0.167.0`, `0.67.0`).

## 2. Umgesetzt je Akzeptanzkriterium

| AK | Umsetzung | Nachweis |
|---|---|---|
| 01 | `agent/claude.rs`: `-p --output-format stream-json --verbose --permission-mode bypassPermissions --permission-prompts none --json-schema <RESULT|REVIEW_SCHEMA>`, `--max-turns`/`--max-budget-usd` (Restbudget, 4 Nachkommastellen) nur bei Limits; `--dangerously-skip-permissions` entfernt | `claude::tests::invocation_flags_with_and_without_limits`, `agent::tests::no_secret_in_argv_for_any_provider_and_limit` |
| 02 | Codex `--output-schema .queue/result.schema.json` bzw. `review.schema.json`, Schema als `queue_files` | `codex::tests::invocation_uses_output_schema_file`, Runner `writes_queue_files_and_rejects_bad_names` |
| 03 | Gemini: `GEMINI_CLI_TRUST_WORKSPACE=true`, `settings.json` immer per `serde_json`, `model.maxSessionTurns`, Auth-Keys bei `oauth_creds` | `gemini::tests::settings_json_contains_turn_limit_and_auth` |
| 04 | `docker/agent.Dockerfile` pinnt 2.1.319/0.167.0/0.67.0, kein `latest`, Versions-Check im Build; `agent::CLI_VERSIONS` | `agent::tests::dockerfile_pins_match_cli_versions`, `cli_contract::fixture_dirs_exist_for_pinned_versions` |
| 05 | `models::RESULT_SCHEMA`/`REVIEW_SCHEMA` strict | `models::tests::schemas_are_strict_and_match_structs` (rekursiv) |
| 06 | `runner::load_contract`: structured > `.queue/result.json` > keiner, Event `contract` | `runner::tests::contract_precedence`, I: `structured_output_wins_and_invalid_falls_back` |
| 07 | claude/codex ohne Contract → `task` „Ergebnis-Contract fehlt", gemini/mock Exit-Code-Fallback | `runner::tests::missing_contract_is_task_failure_for_schema_providers` |
| 08 | 23 Fixtures (6 aufgezeichnet, 17 konstruiert) + `PROVENANCE.md` | `tests/cli_contract.rs` (24 Tests) |
| 09 | `modelUsage`-Summe vor `usage`; Delta-Dedupe pro `message.id` | `claude_usage_dedupes_message_ids`, `claude_model_usage_includes_subagents`, Fixtures `retry_then_success`, `model_usage_subagent`, `success_structured` (echte Zahlen) |
| 10 | stderr wird nicht mehr klassifiziert (S2 entfernt), Textfallback nur Fehlerkanal; Codex „Reconnecting… n/5" = Retry; Gemini warning = Hinweis | `classify::tests::text_fallback_categories`, `codex_reconnect_is_retry`, `gemini_warning_is_not_error_and_turn_limit_is_limit`, I: `task_failures_keep_fix_loop_and_class` (MOCK_STDERR429 → Fix, Provider nicht pausiert) |
| 11 | `classify::ErrorCategory::{from_claude,class}` | `classify::tests::category_mapping_table` (17 Fälle, alle 13 Enum-Werte) |
| 12 | Re-Queue mit Backoff, `transient_retries`, kein Fix | I: `transient_provider_error_requeues_without_fix_attempt` (not_before ≈ +3 s mit Basis 3 s) |
| 13 | `claim_next` respektiert `not_before`, setzt es beim Claim zurück | I: `not_before_delays_claim` |
| 14 | Erschöpfung → `blocked`, gleiche Klasse, Push | I: `transient_retries_exhaust_to_blocked` (3 Starts, kein Fix) |
| 15 | Exec-/Stream-Fehler nicht mehr per `?`: `infra`, Teil-Usage geloggt | I: `runner_stream_interruption_is_infra_and_retried` (FlakyProxy in `tests/common`) |
| 16 | `worker::recover` zählt Retry, `infra`, Notiz-Dedupe; Rate-Limit-Notiz-Dedupe in `queue::requeue` | I: `recover_counts_retry_and_dedupes_note` |
| 17 | Auth → `provider_limits.paused_reason`, Task `queued`, Notification `provider_paused`, `POST /api/limits/{p}/resume`, Credential-PUT hebt Auth-Pause auf | I: `auth_failure_pauses_provider_until_resumed` |
| 18 | `RunError::Config` → `blocked`/`config`; `model_not_found` → config | I: `unknown_credential_kind_blocks_as_config`; U: `model_not_found_is_config` |
| 19 | Fix-Loop unverändert, `failure_class='task'` | alle 19 `api.rs`-Tests unverändert grün; I: `task_failures_keep_fix_loop_and_class` |
| 20 | Rate-Limit im Review pausiert den Review-Provider | I: `review_rate_limit_pauses_review_provider` |
| 21 | `limits::effective` Task > Projekt > Global; Fix erbt, Follow-up nicht | U: `limits::tests::effective_precedence`; I: `fix_tasks_inherit_task_limits` |
| 22 | Laufzeitlimit → `limit_runtime`, kein Fix, Teil-Commit | I: `runtime_limit_blocks_without_fix` (< 8 s, Commit „(unterbrochen: limit_runtime)") |
| 23 | Runner-Stall-Erkennung, `stalled`-Re-Queue ×2 → `blocked` | U (Runner): `detects_stall_and_sends_sigint_first`, `output_resets_the_stall_timer`; I: `stall_requeues_then_blocks` |
| 24 | Live-Token-Guard (`limits::Guard`, nur Provider mit `live_usage`) | U: `guard_trips_once_on_tokens_or_cost`; I: `token_limit_stops_run_live` (< 3,5 s, 2700 counted, dritte Usage-Zeile nie angekommen) |
| 25 | Live-Kosten-Guard; Claude `error_max_budget_usd` | I: `cost_limit_stops_run_live`; C: `claude_max_budget_recorded` (echte Aufnahme) |
| 26 | Turn-Limit an CLI; `error_max_turns`, Gemini `FatalTurnLimitedError`/Exit 53 | I: `turn_limit_is_passed_to_cli` (Task-, Projekt-Limit, ohne Limit done); C: `claude_max_turns_recorded`, `gemini_turn_limit`; U: `precedence_order` (Exit 53 nur Gemini) |
| 27 | Nachträgliche Prüfung: Event `guard` + Notification `limit_exceeded` (ohne Push), Ergebnis bleibt | U: `post_hoc_token_overrun_warns_only` |
| 28 | Runner: SIGINT (Prozessgruppe / `docker|podman kill --signal SIGINT`) → Grace → SIGKILL/`rm -f`, `killed`-Flag, Ausgabe nach SIGINT wird gestreamt | U: `sigint_then_grace_lets_process_flush`, `sigkill_after_grace_when_int_ignored`, `stop_commands_for_engines`; manuell Docker (s. o.) |
| 29 | Nutzer-Abbruch: Usage aus dem SIGINT-Trap im Task und in `usage_log` | I: `cancel_sends_sigint_and_records_usage` (2400 Input-Tokens) |
| 30 | API-Validierung (400, deutsch), `null` = Reset, Viewer 403, `retry` setzt zurück, `/api/meta` mit `provider_capabilities` + `cli_versions` | I: `limit_fields_validation_reset_retry_and_meta`; U: `api::limits::tests` |
| 31 | UI desktop | E: „project limits are saved, survive a reload and the token limit stops a run", „transient provider error is retried without a fix attempt", „provider pause after an auth failure is shown and can be lifted" |
| 32 | UI mobil | M: „project settings with limits: 16px inputs, no horizontal scrolling, clear of safe areas" (iphone + landscape) |
| 33 | System über Prozessgrenze | S: „transient provider error is retried across processes without a fix task", „SIGINT cancel flushes usage across processes" |
| 34 | Gates | Abschnitt 1 |

Weitere geänderte Dateien: Migration `0003_cli_contract_limits.sql` (neu, 0001/0002 unverändert), `queued-common` (`idle_timeout_secs`, `queue_files`, `Exit.{stalled,killed}`, `valid_queue_file_name`), `deploy/*.env.example`, `e2e/scripts/stack.sh`, README (Ablauf, Fehlerklassen, Provider-Tabelle, Mock-Keywords, Grenzen).

## 3. Abweichungen vom Plan (mit Begründung)

1. **Plan-Annahme „keine Live-Tests möglich" war falsch.** Claude ist erreichbar. Damit live belegt: A1 (`structured_output` im stream-json-`result`), A3 (`message.id`/`usage` in `assistant`), `error_max_turns`/`error_max_budget_usd` (Felder `errors[]`, kein `result`), SIGINT-Verhalten (Exit nach ≈2 s, **Exit-Code 0**, `result` mit `error_during_execution`/`terminal_reason:"aborted_tools"` und vollem `modelUsage`).
2. **Bestehender Bug gefunden:** Das echte `rate_limit_event` enthält `"overageStatus":"rejected"` auch bei `status:"allowed"`. Der alte Parser (`blob.contains("reject")`) hätte jeden normalen Abo-Lauf als Rate-Limit gewertet und den Provider pausiert. Jetzt zählt nur `rate_limit_info.status == "rejected"` (Test mit dem realen Event).
3. **13. Claude-Kategorie** `verification_required` (im 2.1.319-Enum, fehlt im Plan) → `provider_auth`.
4. **Codex `item.*` vom Typ `error`** ist laut echter Aufnahme ein Transport-Hinweis („Falling back from WebSockets…"), keine Task-Fehlermeldung → als `⚠`-Text geloggt, nicht als `Error` (Plan: „bleibt Error").
5. **Gemini-Auth-Muster ergänzt** (`api key not valid`, `api_key_invalid`): die echte 0.67.0-Antwort hätte mit den Plan-Mustern (`invalid api key`, `401` …) nicht gematcht und wäre als Task-Fehler mit Fix-Schleife geendet.
6. **Kein Pauschal-`Unknown(None)` → transient** bei einem Claude-Fehlerresultat ohne jedes Provider-Signal. Kategorie nur aus `assistant.error`, offenem `api_retry`, `api_error_status` oder Textfallback; sonst `Error` → `task` (Fix, begrenzt durch `max_fix_attempts`). Begründung: sonst würde jeder echte Absturz (`error_during_execution`) Re-Queues statt Diagnose erzeugen. Ein `api_retry` gilt nur, solange danach keine erfolgreiche Antwort kam.
7. **Textfallback auch für die Fehlertexte von Claude-Fehlerresultaten und Mock-`error` ohne `category`** (beides Fehlerkanal, nicht Agent-Ausgabe): Der Legacy-Text „…usage limit reached|epoch" existiert im 2.1.319-Binary noch; der bestehende Test `claude_error_result_with_limit_is_rate_limited` und `MOCK_LIMIT` bleiben damit unverändert gültig.
8. **Sicherheitslücke `review.json` geschlossen:** `.queue/` wird zwischen Agent- und Review-Lauf nicht geleert. Eine vom *Agenten* geschriebene `review.json` (`approved:true`) wäre als Review gewertet worden, wenn der Reviewer keine schreibt. Vor jedem Review-Lauf leert der Runner jetzt `review.json` (über `queue_files`). Der Mock schreibt `review.json` nur noch im Review-Zweig (sonst wäre `MOCK_REVIEW_LIMIT` nicht testbar).
9. **Review:** Rate-Limit/Auth pausieren den Review-Provider auch dann, wenn trotzdem ein Review-Ergebnis vorliegt (gleiche Regel wie Agent-Lauf). Nutzer-Abbruch während des Reviews → Task `cancelled` (vorher: Review übersprungen → `done`). Stall/Infra/transient im Review → „Review übersprungen: <Klasse>".
10. **Verbrauch kumulativ pro Task-Zeile** über alle Läufe (Re-Queue, Antwort auf Rückfrage), sonst könnte eine Stall-/Transient-Schleife das Token-Limit beliebig oft neu verbrauchen. Vor jedem Lauf und vor dem Review wird geprüft, ob das Limit schon erschöpft ist. `POST /retry` setzt zusätzlich `usage=NULL` zurück (Limits beginnen neu).
11. **`snapshot_before` bleibt bei unterbrochenen Läufen erhalten** (Klassen transient/infra/stalled/rate_limit/provider_auth), damit Diff/Revert/Review den Teil-Commit des unterbrochenen Laufs mit umfassen. Vorher ging dessen Änderung aus dem Task-Diff verloren.
12. **`worker::recover` blockiert bei Erschöpfung** (Plan: nur hochzählen). Ohne das würde ein Task, der die App wiederholt abstürzen lässt, endlos neu starten.
13. **Weck-Timer nach Backoff:** Der Scheduler pollt in Produktion nur alle `TICK_SECS` (15 s). `queue::requeue` weckt ihn deshalb gezielt bei `not_before`.
14. **`RUNNER_KILL_GRACE_SECS` ≤ 60 wird erzwungen**, weil der HTTP-Timeout der App (`timeout_secs + 180`) genau diese Obergrenze annimmt.
15. **Codex-Usage:** `turn.completed` wird je Turn aufsummiert und als `UsageTotal` gemeldet (der Plan sah einen einzelnen Total vor; bei mehreren Turns hätte das unterzählt).
16. **Gültigkeit des Contracts:** Ein Contract zählt nur mit Status `done|failed|needs_input`, ein Review nur mit booleschem `approved`. Vorher wurde `{}` als „approved:false" gewertet. Folge für gemini/mock: Eine `result.json` ohne Status liefert keine Zusammenfassung mehr, es gilt der Exit-Code-Fallback.
17. **UI bewusst reduziert (WP00 portiert danach):** Projekt-Limits, Fehlerklassen-Tag und Backoff-Hinweis in der Queue, Drawer-Block „Abbruchgrund" + Task-Limits, Pause-Tag + „Pause aufheben" sowie CLI-Version in den Budget-Zeilen (statt bei den Zugängen). **Nicht umgesetzt:** Task-Limit-Felder im Composer. Task-Limits sind nur per API (`POST`/`PATCH /api/tasks`) setzbar.
18. Laufzeit-Blocks haben Commit-Suffix `(unterbrochen: <klasse>)` statt „Limit"; Stall wird zusätzlich als Event `guard` protokolliert.

## 4. Bekannte Grenzen (ehrlich)

- **Konstruierte Fixtures:** Claude-Retry/Overload/Rate-Limit/Auth/model_not_found und alle Codex-Fälle außer „Reconnecting" sowie alle Gemini-Fälle außer ungültigem API-Key sind aus Binary-/Bundle-Schemas gebaut, nicht live aufgezeichnet (`PROVENANCE.md` kennzeichnet jede Datei). Codex und Gemini wurden nie mit gültigen Zugangsdaten gegen den echten Provider getestet, auch kein erfolgreicher Lauf. A5 (Codex schreibt den Schema-Contract in die letzte `agent_message`) und A6/A7 (Gemini Trust/SIGINT) sind weiter unverifiziert.
- **Claude-Live-Guard ist unscharf:** In der echten Aufnahme summieren die `assistant.message.usage`-Deltas 29 Output-Tokens, das Endergebnis meldet 74. Der Token-Guard kann also spät greifen; die endgültige Überschreitung wird dann nur nachträglich gemeldet. Kosten erzwingt Claude selbst (`--max-budget-usd`, Schätzung der CLI; A2 – Wirkung bei Abo-OAuth – ungeprüft).
- **Kleine Turn-Limits + Schema:** Claude verbraucht für den structured output selbst Turns („say hi" = 3 Turns). `max_turns` 1–2 wird mit Schema fast immer `limit_turns`. Im README dokumentiert, nicht technisch abgefangen.
- **Podman-SIGINT** (A8) unverifiziert; Docker nur mit busybox, nicht mit dem echten Agent-Image und nicht mit Claude im Container. Das Agent-Image wurde nicht gebaut.
- **Kein Phasen-Retry:** Provider-Fehler im Review überspringen den Review (Task wird `done`), statt den Review zu wiederholen.
- **Stillstand-Default 20 min** kann bei sehr langen, stummen Tool-Aufrufen fehlalarmieren (A4: während eines Bash-Tools kommen nur `task_started`-Events); konfigurierbar, 0 = aus, `stalled` wird erst re-queued.
- **`GEMINI_CLI_TRUST_WORKSPACE=true`** lässt Repo-Konfiguration von Gemini wirken (Härtung WP03).
- **Zeitbasierte Tests** (AK22 < 8 s, AK24 < 3,5 s, Stall-Szenario ≈ 10 s) liefen hier dreimal stabil, können auf sehr langsamen CI-Runnern aber knapp werden.
- `run_inner` ist trotz Auslagerung der Unterbrechungs-Dispositionen (`interrupted`, `Committer`) noch ≈ 430 Zeilen lang (Verify/Review/Finish-Fluss unverändert übernommen).
- Die Recover-/Credential-Pfade setzen `failure_class` beim erneuten Einreihen „fehlender Zugangsdaten" zurück; ältere blockierte Tasks aus der Zeit vor der Migration haben `failure_class = NULL`.
