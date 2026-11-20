# WP10 – Observability, Analytics & Benachrichtigungen: Umsetzung

Grundlage: `WP10-plan.md`. Branch `mvp`, nicht committet (Sicherungen nur auf `wip-snapshot`). Testbericht: `WP10-test.md`, Urteil: `WP10-review.md`.

## 1. Was gebaut wurde

| Baustein | Dateien | Kurz |
|---|---|---|
| Metrik-Registry | `backend/crates/common/src/metrics.rs` (Feature `metrics`) | Counter/Gauge/Histogramm mit festen Labelnamen, Text-Exposition 0.0.4 (Escaping, `_bucket/_sum/_count`, `+Inf`), Deckel 500 Serien je Familie (`_other`), Zugriffsregeln `Access` (Token ≥ 24 Zeichen, eigener Listener, Nicht-Loopback nur mit Token, Token ≠ verbotenes Token), Validator für Tests |
| Runner-Metriken | `runner/src/metrics.rs`, `lib.rs`, `main.rs` | `/metrics` außerhalb von `/v1` nur mit `RUNNER_METRICS_TOKEN` oder auf `RUNNER_METRICS_BIND`; Middleware zählt Routen-Muster; Läufe nach Art (Suffix des Laufnamens, nie Task-ID), Ergebnis, Dauer, aktive Läufe, Egress-Entscheidungen. Start verweigert, wenn Metrik-Token = `RUNNER_TOKEN` |
| App-Metriken | `app/src/metrics.rs`, `api/mod.rs`, `main.rs`, `state.rs` | HTTP (Routen-Muster/Statusklasse), Agentenläufe, Tokens nach Katalog-ID, Kosten je Währung, aktive Aufträge/offene Pausen/Outbox aus der DB (10 s gecacht, 2 s Timeout), Benachrichtigungen, Digests, DB/Runner up, OTel-Verluste; `METRICS_TOKEN`/`METRICS_BIND`; ohne beides `404` statt SPA |
| Health | `app/src/health.rs` | `/healthz` ohne Abhängigkeit; `/readyz` DB + Runner (`/v1/info` mit Token) parallel, je 2 s, 1 s Cache + Single-Flight, nur `ok`/`fail` nach außen |
| OTLP-Exporter | `app/src/otel.rs`, `config.rs` | eigener OTLP/HTTP-JSON-Exporter (keine neue Laufzeit-Dependency), `OTEL_*`-Variablen, nur `http/json`, Batch/Queue/Flush, Task-lokaler Span-Kontext, GenAI-Mappings (`agent_name`, `provider_name`, `model_name`, `tool_name`) |
| Spans | `runner.rs` (`run_task`, `agent_run`, `finish_run_record`), `llm.rs` (`call_traced`), `batch.rs` | `invoke_workflow` → `invoke_agent` → `execute_tool`; `chat` für direkte Schritte |
| Werkzeug-Ereignis | `agent/{mod,claude,codex,gemini,opencode,mock}.rs`, `runner_plan.rs`, `runner_steer.rs` | `AgentEvent::Tool { name, summary }` statt `Tool(String)`: der Name ist ohne Inhalt verfügbar |
| Migration | `migrations/0013_observability_notifications.sql` | `task_runs`, `provider_pauses`, `notification_settings`, `notification_prefs`, `notification_digests`, `notification_outbox`, `notifications.event/quiet_suppressed`, 9 Indizes; 0001–0012 unverändert |
| Pausen-Historie | `budget.rs` | Schreiben in `set_limited` (verlängert statt doppelt), `set_paused` (eine offene je Anbieter), `clear_pause`, `take_budget_notification`, `set_budget` (beendet Budgetpause bei erhöhtem Budget); `prune` mit `USAGE_RETENTION_DAYS` |
| Analytics | `api/src/analytics.rs`, `app/src/api/analytics.rs` | `GET /api/analytics` (7/30/90 Tage, `mine`/`project`, Zeitzone der Tage), `GET /api/tasks/{id}/insights`; alles in SQL (rekursive Fix-Ketten, Perzentile, `range_agg` für Pausen) |
| Benachrichtigungen | `api/src/notify.rs`, `app/src/notify.rs`, `app/src/digest.rs`, `app/src/api/notifications.rs`, `push.rs`, `lib.rs` | Ereignisse × Kanäle, Ruhezeiten mit Zeitzone und Durchbruch, Outbox mit Lease/Backoff/5 Versuchen, Webhook-Kanal (HMAC, Secret einmal sichtbar), Tageszusammenfassung genau einmal, Einstellungen-API, Zustellstatus; SSRF-Schutz für ntfy/Web Push/Webhook |
| `GET /api/tasks` | `api/tasks.rs` | enthält jetzt `merging` (WP09-N11) |
| Frontend | `frontend/src/pages/{analytics,notify_settings,shell,drawer,settings}.rs`, `logic/analytics.rs`, `ui/icons.rs`, `style/app.css` | Tab „Auswertung“, KPI-Kacheln, Säulen-/Balkendiagramme mit Textalternative + Tabelle, Drawer-Block „Auswertung dieses Auftrags“, Einstellungskarte „Was wohin gemeldet wird“ |
| Betrieb/Doku | `deploy/queued.env.example`, `deploy/runner.env.example`, `README.md`, `e2e/scripts/stack.sh` | Monitoring-Einrichtung, Metrikliste, OTel, Benachrichtigungen |

## 2. Entscheidungen (mit Begründung)

1. **Kein OpenTelemetry-SDK.** Geprüft: `opentelemetry-proto` 0.33 allein zieht 52 Crates; SDK + OTLP + `tracing-opentelemetry` koppeln vier Versionen und exportieren über eine `tracing`-Brücke *jeden* Span – inklusive `TraceLayer`-Spans mit URIs (OIDC-Callback mit `code=`). Der eigene Exporter (≈ 500 Zeilen inkl. Tests) exportiert nur explizit gebaute Spans mit konstanten Attributschlüsseln. Formatbeleg: Die Tests deserialisieren jede Export-Anfrage in die offiziellen Proto-Typen (`opentelemetry-proto` nur als dev-dependency).
2. **Nur `http/json`.** Ein falsch gesetztes `OTEL_EXPORTER_OTLP_PROTOCOL` verhindert den Start, statt still im falschen Format zu senden.
3. **Kein Inhalts-Opt-in.** Die Spezifikation verlangt einen Schalter nur für Instrumentierungen, die Inhalte erfassen können. Queue kann es nicht; `OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT` erzeugt eine Warnung.
4. **`invoke_agent` als CLIENT**: der Agent läuft außerhalb des Prozesses (Runner/Container). Wurzel `invoke_workflow`, weil die Queue den Ablauf bestimmt (Spec: „when the application defines the control flow“).
5. **`gen_ai.usage.input_tokens` inkl. Cache** (Spec-Fußnote 54); intern bleiben Eingabe-Tokens ohne Cache (WP13-Semantik), die Cache-Quote rechnet `cache_read / (input + cache_read + cache_write)`.
6. **Metrik-Labels**: `model` ist die Katalog-`price_id` (beschränkte Menge, Admin-Overrides inklusive) statt des frei eingebbaren Modellnamens; Laufart = Suffix des Laufnamens.
7. **Metrik-Token des Runners ≠ `RUNNER_TOKEN`** (Start verweigert): Prometheus soll nie das Token halten, mit dem man Prozesse startet.
8. **Analytics-Mandantentrennung**: `mine` nur in Projekten mit *aktueller* Mitgliedschaft (Projektnamen fremder Projekte nie sichtbar), `project` für jede Rolle – Betrachter sehen dieselben Aufträge samt Kosten heute schon in `GET /api/projects/{id}/tasks`; eine strengere Regel wäre inkonsistent.
9. **Outbox statt Inline-Versand.** Vorher wurden ntfy/Web Push im Aufrufpfad gesendet (kein Retry, Verlust bei Neustart). Jetzt: Zeile pro Kanal (Web Push pro Gerät, damit ein Retry keine anderen Geräte doppelt trifft), Lease 300 s bei höchstens 10 Zeilen je Durchlauf (10 × 15 s Timeout < Lease).
10. **Digest genau einmal** über Primärschlüssel (Person, lokales Datum) und eine Transaktion für Digest-Zeile, Notiz und Outbox.
11. **Webhooks sind Kanal, nicht eigenes Feature** (WP07-N11): gleiche Ereignisse, gleiche Matrix, eine URL pro Person. Ruhezeiten gelten nicht für Webhooks.
12. **`usage_log` 400 Tage** statt 35 (sonst wären 90-Tage-Auswertungen leer); Budgetabfragen sind zeitgefiltert und indiziert.
13. **Diagramme ohne JS-Bibliothek** (CSS-Säulen, 0 KB zusätzlich im WASM-Bundle außer Code). Farben mit dem dataviz-Validator geprüft: das zuerst vorgesehene Grün/Rot-Paar fiel im Dunkelmodus durch (Helligkeitsband, CVD ΔE 7,9) → Blau/Orange (`--series-1/2`), beide Modi bestanden gegen die echten Flächen `#ffffff`/`#151922`.

## 3. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| Outbox Lease 60 s, 50 Zeilen | 300 s, 10 Zeilen | Selbst-Review: 50 × 15 s Timeout > 60 s ⇒ zweite Instanz hätte noch laufende Zeilen erneut beansprucht (Doppelversand) |
| Runner-Metrik `exec_total{kind,outcome}` | zusätzlich `outcome="aborted"` | Stream endet ohne Exit-Frame (Client weg) |
| Wurzelspan nur bei Fehler der Ausführung „ERROR“ | auch bei Endstatus `failed`/`blocked` | Selbst-Review: ein per Contract gescheiterter Auftrag hätte als erfolgreicher Trace gegolten |
| Digest-Text mit Punkt-Dezimalen | Komma wie die UI | Konsistenz |
| Tabs | fünfter Tab „Auswertung“ | auffindbar; Mobil-Test prüft 5 Tabs ≥ 44 pt |
| Webhook-Test-Zustellung über UI im E2E | nur Integrationstest | Fake-Empfänger läuft im Testprozess, nicht im Stack |

## 4. Belegt vs. angenommen

- **Belegt (Repo `open-telemetry/semantic-conventions-genai`, `e6658f1`):** Span-Namen, Arten, `gen_ai.*`-Attribute, Well-known-Werte von `gen_ai.provider.name`/`gen_ai.operation.name`, Content-Capture-Variable und Werte. Das Hauptrepo (`semantic-conventions`, `v1.45.0`) verweist für GenAI auf dieses Repo. `opentelemetry.io` war aus der Sitzung nicht erreichbar.
- **Belegt durch Tests:** OTLP-JSON gegen `opentelemetry-proto` 0.33 (`with-serde`).
- **Nicht verifiziert:** Annahme durch einen echten Collector (Jaeger/Tempo/OTel Collector), Prometheus-Scrape durch echtes Prometheus (nur eigener Format-Validator + Playwright-Prüfung), Web Push gegen echte Push-Dienste.
