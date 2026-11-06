# WP01 – Strukturierte CLI-Integration & Fehlerklassifikation: Testbericht

Status: Test (Tester-Agent), 2026-11-06. Basis: Commit `2ff1e57` („implementation, pre-test“) auf `mvp`. Meine Änderungen sind nicht committet.
Grundlage: `WP01-plan.md` (34 AK), `WP01-impl.md`, Code-Review des Diffs, eigene Tests.

## 1. Ergebnis in Kürze

- **4 Produktfehler gefunden und behoben**, jeder mit Regressionstest, der ohne den Fix nachweislich rot ist (B1–B3 durch Ausführen gegen den alten Code geprüft, B4 ist eine reine Funktion).
- **20 neue Tests**: 11 Integrationstests (`tests/edge_cases.rs`), 6 Parser-Robustheitstests (`tests/parser_robustness.rs`), 1 Runner-Unit-Test, 2 Playwright-Systemtests.
- Alle Gates zweimal grün: Backend **180/180** (vorher 162), Vitest **11/11**, Playwright **25/25** (vorher 23).
- Offene Punkte in Abschnitt 5. Die wichtigsten: AK27 ist nur als reine Funktion getestet, AK08 hat für Gemini kein Retry-Fixture, Codex/Gemini liefen nie gegen den echten Provider.

## 2. Gefundene Fehler

### B1 – Diff/Revert eines fortgesetzten Tasks enthielten fremde Commits (schwer)
- **Ursache:** Abweichung 11 der Umsetzung behält `snapshot_before` nach einer Unterbrechung (transient/infra/stalled/rate_limit/provider_auth). Solange Task A im Backoff wartet, überspringt `claim_next` ihn (`not_before`) und startet **Task B desselben Projekts**. A setzt danach auf Bs Commit auf, sein Diff `snapshot_before..snapshot_after` enthält Bs Commit, und „Rückgängig“ auf A hätte B mit zurückgenommen. Dasselbe passiert mit Commits, die `begin` per `git pull` holt, und mit WIP-Commits. Für die Fortsetzung gab es keinen einzigen Test.
- **Fix** (`runner.rs`): Bei Re-Queue, Rate-Limit-Pause und Auth-Pause wird der Teil-Commit als `snapshot_after` gespeichert (`Committer::interrupted_commit`). `continues_interrupted(task, head)` setzt den Lauf nur dann fort, wenn HEAD nach `begin` noch genau dieser Teil-Commit ist. Sonst beginnt der Diff neu bei HEAD, wie vor WP01.
- **Regressionstests:** `edge_cases::interrupted_task_diff_never_includes_other_tasks` war vor dem Fix rot (Bs `Queue-Task:` im Log von A). `interrupted_task_diff_covers_all_its_runs` belegt, dass die gewollte Fortsetzung weiter funktioniert (beide Commits des Tasks liegen im Diff).

### B2 – Verwaister Runner-Lauf blockierte jede Wiederholung (mittel)
- **Ursache:** Bricht der Exec-Stream ab (Netz, Proxy), merkt der Runner das erst beim nächsten Ausgabe-Frame (`body_tx.send` schlägt fehl). Ein stummer Agent, etwa Claude während eines langen Bash-Tools, läuft daher weiter. Jede Wiederholung mit gleichem Lauf-Namen (`q-<id>-agent`) bekommt 409, wird als `infra` gezählt und endet nach `TRANSIENT_MAX_RETRIES` als `blocked`. Davor committet `begin` halbfertige Dateien des noch laufenden Agenten als WIP. Nach einem App-Neustart (`worker::recover`) passiert dasselbe.
- **Fix:** `runner::exec` sendet nach jedem Transport- oder Startfehler ein Best-Effort-`cancel` für den Lauf-Namen. `worker::recover` stoppt vor dem Re-Queue die Läufe `agent/review/verify` aller betroffenen Tasks: parallel, mit 10 s Gesamt-Timeout. `runner::run_name` ist dafür `pub`. Neues Mock-Keyword `MOCK_QUIET` (6 s ohne Ausgabe), im README dokumentiert.
- **Regressionstests:** `edge_cases::silent_run_after_stream_drop_is_stopped_and_retried` (FlakyProxy, ohne Fix `blocked`, mit Fix `done` nach genau 2 Exec-Aufrufen). `edge_cases::recover_cancels_orphaned_runner_runs` (ohne Fix lief der verwaiste Lauf nach 10 s noch).

### B3 – Nutzer-Abbruch während Verify erzeugte einen Fix-Task (mittel, bestand schon vor WP01, fällt aber in den WP01-Umfang „Abbruch“)
- **Ursache:** Der abgebrochene Verify-Lauf wurde als „Verifikation fehlgeschlagen“ gewertet. Folge: `failure_class=task`, Status `failed` und ein Fix-Task. Den Review-Pfad hatte die Umsetzung korrigiert, den Verify-Pfad nicht.
- **Fix** (`run_inner`): Nach `run_verify` wird `cancel.is_cancelled()` geprüft. Ist es gesetzt, endet der Task mit `cancelled`/`cancelled`, ohne Fix-Task.
- **Regressionstests:** `edge_cases::cancel_during_verify_is_cancelled_without_fix` (vor dem Fix rot: `failed`, Klasse `task`) und Playwright-System „cancel during verify across processes ends cancelled without a fix task“.

### B4 – Unterlauf in der Stillstandserkennung (gering, Race)
- **Ursache:** `started.elapsed() - last_output` in `exec.rs`. Ein Reader-Task kann zwischen den beiden Lesezugriffen einen neueren Zeitstempel speichern. Debug-Build: Panic (Exec-Task stirbt, App sieht Stream-Abbruch). Release-Build: Wrap-around, also ein riesiger Idle-Wert und **falscher Stillstand**.
- **Fix:** `idle_for(now, last) = now.saturating_sub(last)`.
- **Regressionstest:** `exec::tests::idle_time_never_underflows`.

Außerdem habe ich einen Race in meinem eigenen neuen Playwright-Test behoben, keinen Produktfehler: Die Pause wird vor dem Re-Queue gesetzt, deshalb wird der Status jetzt gepollt.

## 3. Akzeptanzkriterien → Tests → Ergebnis

U = Unit, C = Contract, I = Integration (sqlx::test + Runner in-process), S/E/M = Playwright system/desktop/iPhone. „neu“ = vom Tester ergänzt.

| AK | Test(s) | Ergebnis / Bewertung |
|---|---|---|
| 01 | U `claude::tests::invocation_flags_with_and_without_limits`, `agent::tests::no_secret_in_argv_for_any_provider_and_limit` | grün; prüft Flag-Fenster und Negativfall |
| 02 | U `codex::tests::invocation_uses_output_schema_file`, Runner `writes_queue_files_and_rejects_bad_names` | grün |
| 03 | U `gemini::tests::settings_json_contains_turn_limit_and_auth` (parst JSON) | grün |
| 04 | U `agent::tests::dockerfile_pins_match_cli_versions`, C `fixture_dirs_exist_for_pinned_versions` | grün; Image nicht gebaut (s. 5) |
| 05 | U `models::tests::schemas_are_strict_and_match_structs` | grün, rekursiv |
| 06 | U `runner::tests::contract_precedence`, I `structured_output_wins_and_invalid_falls_back` | grün |
| 07 | U `runner::tests::missing_contract_is_task_failure_for_schema_providers` | grün; nur U, da Claude/Codex nicht ausführbar |
| 08 | C 24 Tests in `cli_contract.rs`; **neu** `parser_robustness::*` (abgeschnittene Zeilen jeder Fixture an jeder Zeichengrenze, Müll zwischen Zeilen, falsche Feldtypen, Stream-Abbruch vor `result`) | grün; **Lücke:** kein Gemini-Retry-Fixture (AK verlangt „Retry-Hinweis“ je Version) |
| 09 | C `claude_usage_dedupes_message_ids`, `claude_model_usage_includes_subagent` | grün |
| 10 | U `classify::tests::*`, I `task_failures_keep_fix_loop_and_class` (MOCK_STDERR429); **neu** `parser_robustness::agent_text_and_tool_output_never_classify`, `rate_limit_event_status_only` | grün; der reale `overageStatus:"rejected"`-Fall ist abgedeckt |
| 11 | U `classify::tests::category_mapping_table` | grün |
| 12 | I `transient_provider_error_requeues_without_fix_attempt` | grün |
| 13 | I `not_before_delays_claim`; **neu** `cancel_during_backoff_is_final` | grün |
| 14 | I `transient_retries_exhaust_to_blocked` | grün |
| 15 | I `runner_stream_interruption_is_infra_and_retried`; **neu** `silent_run_after_stream_drop_is_stopped_and_retried` (B2) | grün nach Fix |
| 16 | I `recover_counts_retry_and_dedupes_note`; **neu** `recover_cancels_orphaned_runner_runs` (B2) | grün nach Fix |
| 17 | I `auth_failure_pauses_provider_until_resumed`; **neu** `resume_only_lifts_own_pause` (fremder Resume wirkungslos, anonym 401, unbekannter Provider 400), `auth_pause_is_per_user`; S **neu** „resume only lifts the caller's own provider pause“ | grün |
| 18 | I `unknown_credential_kind_blocks_as_config`, U `model_not_found_is_config` | grün |
| 19 | alle 19 Tests in `tests/api.rs` unverändert, I `task_failures_keep_fix_loop_and_class` | grün |
| 20 | I `review_rate_limit_pauses_review_provider` | grün |
| 21 | U `limits::tests::effective_precedence`, I `fix_tasks_inherit_task_limits` | grün |
| 22 | I `runtime_limit_blocks_without_fix` | grün |
| 23 | U Runner `detects_stall_and_sends_sigint_first`, `output_resets_the_stall_timer`, **neu** `idle_time_never_underflows` (B4); I `stall_requeues_then_blocks` | grün nach Fix |
| 24 | I `token_limit_stops_run_live`; **neu** `guard_stop_logs_usage_once_and_retry_resets_task_budget` (genau eine `usage_log`-Zeile pro Lauf, Task-Usage = geloggt, Retry startet mit frischem Budget), `exhausted_budget_blocks_before_the_next_run` (kumuliertes Limit blockt ohne Lauf) | grün |
| 25 | I `cost_limit_stops_run_live`, C `claude_max_budget_recorded` | grün |
| 26 | I `turn_limit_is_passed_to_cli`, C `claude_max_turns_recorded`, `gemini_turn_limit` | grün |
| 27 | U `runner::tests::post_hoc_token_overrun_warns_only` | grün, aber **schwach**: Es wird nur die reine Funktion geprüft. Event `guard` und Notification `limit_exceeded` hat kein Integrationstest ausgelöst, weil Codex/Gemini nicht ausführbar sind und der Mock `live_usage` hat. |
| 28 | U Runner `sigint_then_grace_lets_process_flush`, `sigkill_after_grace_when_int_ignored`, `stop_commands_for_engines` | grün; Docker nur manuell von der Umsetzung geprüft (von mir nicht wiederholt), Podman unverifiziert |
| 29 | I `cancel_sends_sigint_and_records_usage`, S „SIGINT cancel flushes usage across processes“ | grün |
| 30 | I `limit_fields_validation_reset_retry_and_meta`, U `api::limits::tests` | grün |
| 31 | E 3 Tests in `queue.spec.ts` | grün; der Zwischenzustand „wartet bis“/„Provider gestört“ in der Queue-Zeile wird nicht geprüft, nur das Retry-Event (laut AK-Wortlaut erlaubt) |
| 32 | M „project settings with limits …“ (iphone + landscape) | grün |
| 33 | S 2 Tests der Umsetzung + **neu** „cancel during verify across processes …“ (B3) | grün |
| 34 | Abschnitt 4 | grün |
| – | **neu** `migration_0003_on_existing_rows`: 0001/0002 anwenden, Altdaten (User, Projekt, blockierter Task, `provider_limits`) einfügen, dann 0003 | grün; Defaults korrekt (0 Retries, `NULL` Klasse/Limits/`paused_reason`) |

## 4. Testläufe (alle in dieser Sitzung)

| Befehl | Lauf 1 | Lauf 2 |
|---|---|---|
| `cargo fmt --all -- --check` | Exit 0 | – |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit 0 | – |
| `DATABASE_URL=… cargo test --workspace` | Exit 0, **180 passed**: app lib 70, api 19, cli_contract 24, edge_cases 11 (12,7 s), failures 18 (16,6 s), parser_robustness 6, common 6, runner 26 | Exit 0, 180 passed (edge_cases 12,8 s, failures 17,0 s) |
| `cd frontend && npm run build && npm test` | tsc + vite ok, Vitest **11/11** | – |
| `cd e2e && npx playwright test` | 25/25 (33,6 s) | 25/25 (33,6 s) |

Zum Vergleich der Stand vor meinen Änderungen: 162/162 Backend-Tests, ein Lauf.
Playwright vollständig erzählt: Der erste Lauf mit den neuen Tests war 25/25. Im zweiten scheiterte mein neuer Resume-Test an einem Race im Test selbst (Status vor dem Re-Queue gelesen). Nach der Korrektur bestand er 3× isoliert, danach liefen die beiden oben genannten vollen Läufe grün.

## 5. Verbleibende Lücken und Beobachtungen (ehrlich)

- **Nicht live geprüft:** Codex und Gemini liefen nie gegen den echten Provider, und die meisten Fehler-Fixtures sind konstruiert (`PROVENANCE.md`). Für Gemini gibt es kein Retry-Fixture (AK08 dort nur teilweise erfüllt). Das Agent-Image ist nicht gebaut. Docker-SIGINT habe ich nicht selbst wiederholt, Podman ist unverifiziert.
- **AK27** ist nur als reine Funktion getestet (s. Tabelle).
- **Widerspruch im Plan, nicht behoben:** Codex, Gemini und Claude (bei untererfassten Deltas) melden eine Token-Überschreitung erst nach dem Lauf, mit der Meldung „Ergebnis bleibt bestehen“ (5.5.3). Mit aktivem Review blockiert `run_review` den Task danach trotzdem als `limit_tokens`/`limit_cost` („kein Review-Lauf“, 5.5.4). Für den Nutzer sind die beiden Meldungen widersprüchlich. Das ist eine Produktentscheidung für den Reviewer.
- **Folge von B1:** Nach einem App-Neustart, oder wenn der Teil-Commit scheitert (Runner weg), wird nicht mehr fortgesetzt. Der WIP-Commit des abgebrochenen Laufs liegt dann außerhalb des Task-Diffs, wie vor WP01. Das ist bewusst konservativ: lieber zu wenig im Diff als fremde Commits.
- **Reihenfolge im Projekt:** Ein Task im Backoff oder in einer Provider-Pause wird von späteren Tasks desselben Projekts überholt. Nach B1 ist das für Diff und Revert unschädlich, inhaltliche Abhängigkeiten zwischen Tasks sichert es aber nicht ab. Für Rate-Limits pro Provider galt das schon vorher.
- **Rest von B2:** Zwischen `cancel` und dem tatsächlichen Ende des verwaisten Laufs liegt bis zu `RUNNER_KILL_GRACE_SECS`. Ein Retry in diesem Fenster bekommt 409, was ein weiterer Retry abfängt, und `begin` kann Zwischenstände als WIP committen. Mit dem Default-Backoff (30 s) gegenüber der Grace (15 s) ist das praktisch selten.
- **Ungeprüfte Stream-Werte:** Negative oder extrem große Token-/Kostenwerte aus dem CLI-Stream werden nicht begrenzt. Negative Werte senken Task- und Fensterverbrauch. `i64`-Überlauf in `Usage::add` panict im Debug-Build, und der Task bliebe dann `running` bis zum Neustart. Der Stream ist ohnehin agentkontrolliert (der Agent könnte auch einen Contract fälschen), daher nur notiert.
- **Textfallback:** `\b401\b` matcht im Fehlerkanal (Codex/Gemini) auch ohne HTTP-Kontext und kann so eine Auth-Pause auslösen, wie im Plan vorgesehen. Agent-Text, Tool-Ausgabe und stderr sind nachweislich ausgenommen.
- **Zeitbasierte Tests** (Backoff-Überholen ≈ 3 s, Stall ≈ 10 s, Laufzeit < 8 s) waren hier zweimal stabil. Auf langsamen CI-Runnern können sie knapp werden.
