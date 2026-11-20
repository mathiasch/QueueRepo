# WP10 – Observability, Analytics & Benachrichtigungen: Testbericht

Stand: 2026-11-19, Branch `mvp` + WP10-Arbeitsstand. Alle Zahlen aus Läufen in dieser Sitzung. Keine echten externen Dienste: OTLP-Collector, ntfy, Webhook-Empfänger und Push-Dienst sind lokale Fake-Server im Testprozess; keine Aufrufe an LLMs oder bezahlte APIs (**Kosten: 0 USD**). Die OTel-GenAI-Konventionen wurden aus dem offiziellen Repository `open-telemetry/semantic-conventions-genai` (Commit `e6658f1`, 6. 11. 2026) gelesen; `opentelemetry.io` war aus der Sitzung nicht erreichbar.

## 1. Teststrategie

| Ebene | Was | Wo |
|---|---|---|
| Unit | Metrik-Registry: Text-Format (HELP/TYPE, Escaping von `\`, `"`, Zeilenumbruch, Histogramm kumulativ mit `+Inf`, `_sum/_count`), Zähler nie negativ, Kardinalitätsdeckel (550 Serien ⇒ 500 + `_other`), Exposition-Validator (lehnt 6 kaputte Varianten ab), Zugriffsregeln (kein Token/kein Listener ⇒ aus; Token-Länge; Nicht-Loopback ohne Token verweigert; Metrik-Token = Runner-Token verweigert; `Debug` ohne Token) | `common/src/metrics.rs` (6) |
| Unit | Laufart aus dem Laufnamen (nie Task-ID) | `runner/src/metrics.rs` (1) |
| Unit | OTel-Konfiguration (aus ohne Endpunkt, `OTEL_SDK_DISABLED`, `OTEL_TRACES_EXPORTER=none`; Endpunkt-Ableitung `/v1/traces`; Header/Resource mit Prozent-Kodierung; Protokoll ≠ `http/json`, `zipkin`, kaputte Header, Timeout mit Einheit ⇒ Fehler; `Debug` ohne Header-Werte), Span ohne Tracer baut nicht einmal den Namen, Werkzeugnamen-Bereinigung, Provider-/Agent-/Modell-Mapping, Prozent-Dekodierung inkl. Mehrbyte | `app/src/otel.rs` (5) |
| Unit | Ruhezeiten in Zeitzonen (Berlin Winter/Sommer, New York, Durchbruch kritischer Ereignisse, Digest nie still), Webhook-Signatur gegen Python-HMAC-Referenz, Backoff | `app/src/notify.rs` (3) |
| Unit | Fälligkeit der Zusammenfassung (vor/nach Uhrzeit, 12-h-Nachholfenster, lokales statt UTC-Datum, Sommerzeit, Fenster endet um Mitternacht), Text und Leerfall | `app/src/digest.rs` (2) |
| Unit | Ereignis-Ableitung = altes Push-Verhalten je Aufrufstelle, Ruhezeit-Fenster über Mitternacht, Uhrzeit-Parser, Validierung, Matrix-JSON | `api/src/notify.rs` (4) |
| Unit | Quoten, Cache-Quote, Dauer-/Prozentformat, Klartext-Hinweise der Insights | `api/src/analytics.rs` (2) |
| Unit (Frontend) | Balkenskalierung (sichtbares Minimum), Textalternativen der Diagramme | `frontend/src/logic/analytics.rs` (2) |
| Runner (echter HTTP-Server) | `/metrics` nur mit eigenem Token, `RUNNER_TOKEN` öffnet es nicht, Metrik-Token öffnet `/v1` nicht, Lauf `q-…-agent` ⇒ `kind="agent"`, keine Task-/Workspace-ID, Routen-Muster der verschachtelten API, Format-Validator; ohne Konfiguration 404 | `runner/src/lib.rs` (2) |
| Integration (App + Runner in-process + Postgres) | siehe Abschnitt 3 | `app/tests/observability.rs` (9), `analytics.rs` (4), `notifications.rs` (6) |
| System (getrennte Prozesse, Playwright `system`) | `/healthz`, `/readyz`, `/metrics` App/Runner mit eigenen Tokens (Format geprüft, keine IDs/Titel), Runner-Metrik-Token öffnet `/v1/info` nicht; Analytics zweier Personen getrennt, Insights 404 für Fremde, Benachrichtigungs-Einstellungen Round-Trip | `e2e/tests/system/observability.spec.ts` (2) |
| E2E Desktop | Auswertung: KPI-Texte, Diagramm mit `aria-label`-Zusammenfassung, Tabelle (32 Zeilen = 31 Tage + Kopf), Zeitraum/Bereich, Leerzustand, Sprung in den Auftrag, Insights im Drawer; Benachrichtigungen: Matrix, Ruhezeit, Zeitzone, Zusammenfassung speichern, nach Reload erhalten, verständlicher Fehler bei gleicher Start-/Endzeit; axe | `e2e/tests/ui/analytics.spec.ts` (2) |
| E2E iPhone (hoch + quer) | Tab „Auswertung“ ≥ 44 pt in der Safe-Area, 90 Tage ohne horizontales Scrollen, Karten innerhalb der Safe-Area, Select/Inputs ≥ 16 px, Checkbox-Zeilen ≥ 44 pt, Speichern über dem Home-Indikator, axe | `e2e/tests/ui/analytics-mobile.spec.ts` (1 × 2) |
| Accessibility-Tour | Auswertung (mit aufgeklappter Tabelle) hell/dunkel, Drawer mit Insights, Einstellungen mit Benachrichtigungskarte | `e2e/tests/tour.ts` (erweitert) |

## 2. Gegenproben (Tests können scheitern)

| Mutation | Erwartet rot | Ergebnis |
|---|---|---|
| Ruhezeit-Prüfung in `notify::create` deaktiviert (`quiet = false && …`) | `quiet_hours_hold_back_push_but_not_critical_or_webhooks` | **rot** (2 statt 1 ntfy-Nachricht), zurückgesetzt |
| Werkzeug-Zusammenfassung (enthält `MOCK_LOG.md`) als Span-Attribut | `spans_follow_genai_conventions_and_never_carry_content` | **rot** („MOCK_LOG.md leaked into a span“), zurückgesetzt |
| Digest-Konflikt ignoriert (zweite Instanz schreibt trotzdem Notiz + Outbox) | `daily_summary_is_sent_once_across_instances_and_restarts` | **rot** (2 statt 1 Zusammenfassung bei ntfy), zurückgesetzt |
| Wurzelspan ohne Fehler bei gescheitertem Auftrag (ursprünglicher Code) | `failed_runs_mark_spans_without_messages` | war **rot**, Produktfehler behoben (Review B4) |

Weitere Rot-Läufe während der Entwicklung: Span-Test wartete auf `done`, der Auftrag mit `MOCK_SECRET_LOG` wird aber vom Gate gehalten (`held`) – Testfehler; Lasttest-Grenze `> 300` bei genau 300 Aufträgen – Testfehler; erster vollständiger Lauf scheiterte an `cargo fmt --check` (nach dem Formatieren nachträglich eingefügter Push-Test).

## 3. Integrationstests (Auszug)

| Test | Prüft |
|---|---|
| `healthz_and_readyz_report_failures_without_internals` | bereit ⇒ 200 mit `ok/ok`; Runner-Port tot ⇒ 503 `runner: fail` in < 5 s, Body ohne Adresse/Port/Fehlertext; falsches Runner-Token ⇒ 503; DB-Pool geschlossen ⇒ 503 `database: fail`; `/healthz` bleibt 200 |
| `readyz_is_cached_against_floods` | 40 parallele Proben, alle 200, Gauge `queued_runner_up 1` |
| `metrics_are_off_by_default_and_never_the_spa` | 404 ohne Konfiguration, kein HTML |
| `metrics_need_their_token_and_carry_no_ids_or_content` | 401 ohne/falsch/Session/API-Token; Metrik-Token öffnet `/api/tasks` nicht; nach echtem Lauf: Lauf-, Token-, Dauer-, Aufgaben-, HTTP-Metriken vorhanden; Task-/Projekt-/Nutzer-ID, E-Mail, Titel, Projektname, Prompt fehlen; Format-Validator |
| `metrics_on_their_own_listener` | `METRICS_BIND`: Haupt-Listener 404, eigener Listener 200 ohne Token (Loopback) |
| `spans_follow_genai_conventions_and_never_carry_content` | Fake-Collector (prüft `application/json` und Auth-Header); jede Anfrage deserialisiert in `ExportTraceServiceRequest`; Baum `invoke_workflow` (INTERNAL) → `invoke_agent mock` (CLIENT, gleiche Trace-ID, Parent) → `execute_tool edit`; `gen_ai.usage.input_tokens = 2000` (1200 + 800 Cache), Cache 800, Output 150; nur `gen_ai.*`/`queue.*`/`error.type`; keine Marker aus Prompt, Titel, Projekt, Werkzeug-Argument, Agent-Ausgabe |
| `failed_runs_mark_spans_without_messages` | Agentenlauf OK, Wurzelspan ERROR ohne Nachricht mit `error.type=failed|blocked`; Contract-Text und Prompt-Marker fehlen |
| `tracing_is_off_without_configuration` | kein Tracer |
| `task_list_includes_merging` | WP09-N11 |
| `analytics_are_separated_per_tenant_and_role` | bekannte Daten: 5 beendet, First-Pass 1/3, am Ende 2/3, 1 Fix, Kosten 1,75 $ + 0,40 CHF, Cache 3000; fremde Projekte/Aufträge nie in `mine`; Bearbeiter/Betrachter sehen Projekt, Nicht-Mitglieder 404; nach Entfernen verschwinden eigene Daten im fremden Projekt; 7/30 Tage; 400 für `days=5`, fehlende Projekt-ID, unbekannte Zone; 401 ohne Login, 403 mit API-Token; Insights-Zugriff wie Auftrag |
| `insights_of_a_real_run` | echter Mock-Lauf: `task_runs`-Zeile, Cache 800, Werkzeug „edit“, Hinweise, Agentenzeit im Dashboard |
| `pause_history_merges_overlaps` | Rate-Limit verlängert statt doppelt, Aufheben beendet, zwei überlappende Pausen ⇒ 90 min, Budgetpause außerhalb des Fensters zählt nicht, offene Auth-Pause nur einmal |
| `analytics_stay_fast_with_realistic_volumes` | 50 Personen, **60 000 Aufträge, ≈ 195 000 Verbrauchszeilen** (über 120 Tage), ANALYZE; Messung (Debug-Build): mine 90 d **35–38 ms**, mine 30 d **23 ms**, Projekt 90 d **31 ms** (Grenze 3 s); `EXPLAIN` ohne Seq Scan auf `usage_log`/`tasks` |
| `settings_api_validates_and_stores_only_deviations` | Standardmatrix, 5 ungültige Eingaben ⇒ 400 (auch Metadaten-IP), nur 2 Abweichungen gespeichert, API-Token 403 |
| `quiet_hours_hold_back_push_but_not_critical_or_webhooks` | Fake-ntfy + Fake-Webhook: in der Ruhezeit nur das kritische Ereignis per ntfy, Webhook trotzdem, Signatur `t=…,v1=…` korrekt, Postfach vollständig mit `quiet_suppressed`; ohne Durchbruch nichts; Matrix pro Ereignis wirkt; Postfach abschaltbar |
| `outbox_retries_transient_failures_and_blocks_internal_targets` | 503 ⇒ Retry mit Backoff, dann gesendet, Metriken `retry`/`sent`; Produktionsschalter: ntfy auf 127.0.0.1 ⇒ `failed`, 0 Anfragen; Webhook `https://127.0.0.1` und `http://…` ⇒ 400 |
| `daily_summary_is_sent_once_across_instances_and_restarts` | zwei AppStates auf derselben DB, 3 Runden parallel ⇒ 1 Zusammenfassung (Text mit Zahlen); Lease einer „abgestürzten“ Instanz respektiert, danach parallele Ticks ⇒ **genau 1 ntfy-Nachricht**; neue Instanz („Neustart“) am selben Tag ⇒ 0; Tag nach Ablauf des Nachholfensters ⇒ übersprungen; leerer Tag ⇒ Zeile ohne Notiz, nichts gesendet; Metrik `queued_digests_total` = 1 |
| `end_to_end_flow_and_webhook_test` | Webhook-Test erst mit Secret, Push-Test ignoriert Ruhezeit, Zustellstatus-API |
| `web_push_per_device_and_gone_devices` | RFC-8291-Schlüssel, je Gerät eine verschlüsselte Anfrage (`aes128gcm`, VAPID), 410 ⇒ Gerät entfernt, Zeile `failed`, das andere `sent` |

## 4. Ergebnisse (Abnahme: komplette Suite zweimal hintereinander auf dem Endstand)

Skript `wp10-accept.sh` (Scratchpad, identisch mit dem Abnahmeskript aus WP09): Backend `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`; E2E-Binaries; Frontend `cargo fmt --check`, `cargo clippy --target wasm32-unknown-unknown --all-targets -D warnings`, `cargo clippy --all-targets -D warnings`, `cargo test`, `trunk build --release`; danach `frontend/target` gelöscht; Playwright alle Projekte (system, desktop, iphone, iphone-landscape inkl. axe-Tour). Stoppt beim ersten Fehler. Geprüfter Stand = Inhalt von `07f6108` (Unterschied zum getesteten Snapshot nur `WP10-test.md`).

| Schritt | Lauf 2 (19.11., 21:48–22:12 UTC) | Lauf 3 (19.11., 22:12–22:36 UTC) |
|---|---|---|
| Backend fmt / clippy `-D warnings` | grün / grün | grün / grün |
| Backend Tests | **579 bestanden, 0 fehlgeschlagen** | **579 bestanden, 0 fehlgeschlagen** |
| Frontend fmt / clippy wasm32 / clippy nativ | grün / grün / grün | grün / grün / grün |
| Frontend Tests | **62 bestanden** | **62 bestanden** |
| `trunk build --release` | ok | ok |
| Playwright (system, desktop, iphone, iphone-landscape, inkl. axe-Tour) | **109 bestanden** (7,6 min) | **109 bestanden** (7,6 min) |

Vergleich WP09-Abnahme: Backend 535 → 579 (+44: common 6, runner 3, app lib 10, api 6, Integration 19), Frontend 60 → 62, Playwright 103 → 109 (system 2, desktop 2, iphone 1, iphone-landscape 1) plus Tour um „Auswertung“ (hell/dunkel × 3 Geräte) und Insights im Drawer erweitert. Bestehende Tests wurden nur angepasst, nicht abgeschwächt: `AgentEvent::Tool { name, summary }` statt `Tool(String)` (gleiche Prüfungen, zusätzlich Werkzeugname), iPhone-Tab-Leiste 4 → 5 Tabs (gleiche 44-pt-/Safe-Area-Prüfung).

Lauf 1 (davor) scheiterte an `cargo fmt --check` des danach eingefügten Push-Tests; Backend-Tests dort 579/0. Ein früherer Teil-Lauf brach mit „No space left on device“ ab (`frontend/target` + alte Test-Binaries); danach wurden veraltete Artefakte gelöscht (siehe Auftrag: volle Platte ⇒ falsche Fehler).

## 5. Nicht getestet / nicht verifizierbar

- Echter OTLP-Collector (Jaeger, Tempo, OTel Collector), echtes Prometheus, echte Push-Dienste (FCM/Mozilla/Apple), echte ntfy-Instanz.
- Verhalten unter echter Zeitumstellung im laufenden Betrieb (nur Unit-Tests mit festen Zeitpunkten in Sommer- und Winterzeit).
- Mehrere App-Instanzen als getrennte Prozesse (Integration nutzt zwei `AppState` auf derselben Datenbank im selben Prozess – gleiche SQL-Pfade).
- Leistung im Release-Build und mit mehr als 60 000 Aufträgen.
