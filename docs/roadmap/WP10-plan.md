# WP10 – Observability, Analytics & Benachrichtigungen: Plan

Stand: 2026-11-17, Branch `mvp` (Basis `2b936f7`, WP09). Ein Agent plant, baut, testet und reviewt dieses Paket. Grundlage: `work-packages.md` (Gemeinsame Regeln + WP10), Report P1.10/P1.13, Berichte WP00–WP09/WP13.

## 1. Ausgangslage (am Code geprüft, nicht aus den Berichten übernommen)

| Thema | Befund | Folge für WP10 |
|---|---|---|
| Health | Runner hat `GET /healthz` (ohne Auth). Die App hat **keinen** Health-Endpunkt; `/healthz` liefert heute die SPA (`index.html`, 200) – ein Load-Balancer hielte eine App ohne DB für gesund. | `/healthz` + `/readyz` in der App, ausdrücklich vor dem SPA-Fallback. |
| Metriken | Keine. `tracing`-Logs nur auf stdout. | Eigenes kleines Prometheus-Modul in `queued-common` (App + Runner). |
| Traces | Keine. | OTLP-Export, optional. |
| Verbrauch | `usage_log` (pro Schritt, Tokens inkl. Cache, Kosten in Geld + Währung, `price_id`, Projekt, Connector) und `tasks.cost` (WP13). **`budget::prune` löscht `usage_log` nach 35 Tagen** – 90-Tage-Auswertungen wären leer. Kein Index auf `usage_log.task_id`. | Aufbewahrung konfigurierbar (Standard 400 Tage, min. 35); Index auf `task_id`. |
| Token-Semantik | Alle Parser normalisieren: `input_tokens` **ohne** Cache-Reads (Codex/Gemini ziehen `cached` ab, Claude/opencode melden getrennt). | Cache-Quote = `cache_read / (input + cache_read + cache_write)`; OTel `gen_ai.usage.input_tokens` = Summe aller drei (Spec: „SHOULD include cached tokens“). |
| Pausen | `provider_limits` hält nur den **aktuellen** Zustand (`limited_until`, `paused_reason`, `budget_notified_until`); Historie fehlt. Connector-Pausen laufen als `connector:<uuid>`. | Neue Tabelle `provider_pauses` (Beginn/Ende/Grund), geschrieben in `budget::set_limited/set_paused/clear_pause/take_budget_notification/set_budget`. |
| Läufe | Ein Auftrag hat beliebig viele Läufe (Re-Queue, Rückfrage, Steering, Review, Plan). Es gibt keine Lauf-Tabelle; `task_events` ist Freitext. | Neue Tabelle `task_runs` (eine Zeile pro Agentenlauf: Art, Anbieter, Modell, Start/Ende, Ergebnis, Tokens, Werkzeugzähler). Grundlage für Session-Insights, Metriken und Spans. |
| Werkzeuge | `AgentEvent::Tool(String)` ist eine Zusammenfassung **mit Inhalt** (`"Bash: cargo test"`, `"$ rm -rf …"`). | Variante wird `Tool { name, summary }`; nur der bereinigte `name` darf in Spans/Insights. |
| Benachrichtigungen | `notify::notify` schreibt in-app + SSE, bei `push=true` sofort ntfy + Web Push **inline** (kein Retry, kein Schutz vor Neustart). Keine Einstellungen pro Ereignis, keine Ruhezeiten, kein Digest. **ntfy-URL und Push-Endpunkt sind nutzergesteuert und werden ohne SSRF-Schutz aufgerufen** (WP13 hat `llm::check_target` + `GuardedResolver` nur für LLM-/Git-Ziele eingeführt). | Outbox + Einstellungsmatrix + Ruhezeiten + Digest; SSRF-Schutz für alle Benachrichtigungsziele (`ALLOW_PRIVATE_NOTIFY_TARGETS`). |
| Ausgehende Webhooks (WP07-N11) | Gibt es nicht. Report P1.6 nennt „started/finished“-Webhooks. | Passen als **Kanal** in die Ereignis×Kanal-Matrix: ein signierter Webhook pro Nutzer (HMAC-SHA256, Zeitstempel), über Outbox mit Retry, SSRF-geschützt. Keine eigene Ereignisliste daneben. |
| WP09-N11 | `GET /api/tasks` lässt `merging` aus (Bestand WP04). Für Status/Analytics relevant: ein API-Nutzer (queuectl, Dashboards) sieht Aufträge in der Übernahme gar nicht. Die UI (Queue-Seite) filtert selbst. | Fix hier: `merging` in die aktive Menge. UI-Darstellung prüfen (keine Doppelanzeige). |
| Auth | `AuthUser` = Cookie oder API-Token mit Routen-Tabelle `tokens::ROUTES` (nicht gelistet = 403 für Tokens). | Neue Analytics-/Benachrichtigungs-Routen: **nicht** in `ROUTES` (nur Cookie). |

## 2. Architektur

### 2.1 Health
- `GET /healthz` (App): 200 `ok`, keine Abhängigkeit, keine DB.
- `GET /readyz` (App): DB `SELECT 1` und Runner `GET /v1/info` **mit** Runner-Token (prüft Erreichbarkeit *und* passendes Token), je 2 s Timeout, parallel. 200 `{"status":"ready","checks":{"database":"ok","runner":"ok"}}`, sonst 503 mit `fail` je Prüfung. Keine Fehlertexte, keine Adressen; Details nur im Log (Warnung). Ergebnis 1 s gecacht + Single-Flight, damit ein unauthentifizierter Flood weder DB noch Runner hämmert.
- Runner: bestehendes `/healthz` bleibt.

### 2.2 Prometheus `/metrics`
- **Modul `queued_common::metrics`** (feature-frei, keine neue Dependency): `CounterVec`, `GaugeVec`, `HistogramVec` mit festen Labelnamen, Text-Exposition 0.0.4 (`# HELP`/`# TYPE`, Escaping von `\`, `"`, Zeilenumbruch, `_bucket{le}`/`_sum`/`_count`, `+Inf`). **Kardinalitätsdeckel** pro Familie (Standard 500 Labelkombinationen; darüber landet alles in `…="_other"`) – Schutz gegen Fehler in Aufrufern.
- **App** (`METRICS_TOKEN` und/oder `METRICS_BIND`):
  - Nur `METRICS_TOKEN` (≥ 24 Zeichen): `/metrics` auf dem Haupt-Listener, nur mit `Authorization: Bearer`, Vergleich in konstanter Zeit.
  - `METRICS_BIND` (z. B. `127.0.0.1:9464`): eigener Listener nur mit `/metrics`; Haupt-Listener antwortet 404. Nicht-Loopback-Adresse **ohne** Token → Start verweigert.
  - Nichts gesetzt → `/metrics` 404 (nicht SPA). Kein Default-Token, nie öffentlich ohne Absicht.
- **Runner** (`RUNNER_METRICS_TOKEN` und/oder `RUNNER_METRICS_BIND`): gleiche Regeln. Das Metrik-Token **muss sich vom `RUNNER_TOKEN` unterscheiden** (Start verweigert) – Prometheus bekommt nie das Token, das `exec` erlaubt. `/metrics` liegt außerhalb von `/v1` und erlaubt nichts anderes.
- Metriken (Labels nur aus festen Mengen):
  - App: `queued_build_info{version}`, `queued_http_requests_total{method,route,status}` (Route = Muster wie `/api/tasks/{id}`, Status = Klasse `2xx`…), `queued_http_request_duration_seconds` (Histogramm), `queued_tasks{status}` (aktive Status, aus DB, 10 s gecacht), `queued_agent_runs_total{provider,kind,outcome}`, `queued_agent_run_duration_seconds{kind}`, `queued_tokens_total{provider,model,type}` (`model` = Katalog-`price_id` oder `unknown`, nie Freitext), `queued_cost_total{provider,currency}`, `queued_provider_pauses{reason}` (offene Pausen, DB), `queued_notifications_total{channel,result}`, `queued_notification_outbox_pending` (DB), `queued_digests_total`, `queued_database_up`, `queued_runner_up`, `queued_otel_spans_dropped_total`, `queued_otel_export_errors_total`.
  - Runner: `queued_runner_build_info`, `queued_runner_http_requests_total{method,route,status}`, `queued_runner_exec_active`, `queued_runner_exec_total{kind,outcome}` (`kind` = Suffix des Laufnamens: agent/review/verify/setup/…, **nie** die Task-ID), `queued_runner_exec_duration_seconds{kind}`, `queued_runner_egress_requests_total{decision}`.

### 2.3 OpenTelemetry (Traces, OTLP)
- **Eigener, schlanker OTLP/HTTP-JSON-Exporter** (`app/src/otel.rs`) statt `opentelemetry`+`opentelemetry_sdk`+`opentelemetry-otlp`+`tracing-opentelemetry`:
  - keine neuen Laufzeit-Dependencies (reqwest/serde_json/tokio vorhanden), keine Versionskopplung von vier Crates;
  - **nur explizit gebaute GenAI-Spans** werden exportiert. Eine `tracing`-Brücke würde alle Spans exportieren, auch `TraceLayer`-Spans mit URIs (OIDC-Callback mit `code=`, `?task=`) → Inhaltsleck.
  - Format-Prüfung im Test gegen die **offiziellen Proto-Typen** (`opentelemetry-proto` 0.33 als *dev-dependency*, Feature `with-serde`, keine tonic-Transportschicht; Probe: deserialisiert OTLP-JSON mit Hex-IDs und String-Int64 korrekt).
- Aktiv nur, wenn `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` oder `OTEL_EXPORTER_OTLP_ENDPOINT` gesetzt ist und weder `OTEL_SDK_DISABLED=true` noch `OTEL_TRACES_EXPORTER=none`. Sonst: `Tracer = None`, Span-Aufrufe sind ein `Option`-Check – keine Allokation, kein Task, kein Socket.
- Unterstützt: `OTEL_EXPORTER_OTLP_(TRACES_)HEADERS`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_EXPORTER_OTLP_(TRACES_)TIMEOUT`. Protokoll nur `http/json`; ein anderer Wert in `OTEL_EXPORTER_OTLP_(TRACES_)PROTOCOL` → Start verweigert (laut statt falsches Format).
- Batching: begrenzte Queue (2048 Spans, Überlauf wird gezählt und verworfen), Export alle 5 s oder bei 512 Spans, 10 s Timeout, kein Retry-Sturm; Flush beim geordneten Herunterfahren.
- **Semantik** (geprüft gegen `open-telemetry/semantic-conventions-genai`, Commit `e6658f1` vom 2026-11-06; die GenAI-Konventionen sind aus dem Hauptrepo ausgezogen, Status **Development**):
  - Wurzel pro Lauf eines Auftrags: `invoke_workflow queue_task` (INTERNAL, `gen_ai.operation.name=invoke_workflow`) – die Queue bestimmt den Ablauf.
  - Pro Agentenlauf (Agent, Review, Plan): `invoke_agent {gen_ai.agent.name}` (CLIENT – der Agent läuft außerhalb des Prozesses), Attribute `gen_ai.operation.name=invoke_agent`, `gen_ai.provider.name` (Mapping auf Well-known-Werte: `anthropic`, `openai`, `gcp.gemini`, `aws.bedrock`, `gcp.vertex_ai`, `azure.ai.openai`, `mistral`, sonst Connector-Typ), `gen_ai.agent.name` (`claude-code`, `codex`, `gemini-cli`, `opencode`, `mock`), `gen_ai.request.model` (nur wenn gesetzt, Katalog-normalisiert), `gen_ai.usage.input_tokens` (inkl. Cache), `gen_ai.usage.output_tokens`, `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_write.input_tokens` (nur > 0), `error.type` (Fehlerklasse, niedrige Kardinalität), `queue.run.kind`.
  - Pro Werkzeugaufruf: `execute_tool {gen_ai.tool.name}` (INTERNAL), `gen_ai.tool.name` (bereinigt, ≤ 64 Zeichen `[A-Za-z0-9_.:-]`, sonst `other`), `gen_ai.tool.type=function`. Dauer = bis zum nächsten Ereignis des Agenten (Näherung, die CLIs melden kein Werkzeug-Ende einheitlich – dokumentiert).
  - Direkte Einzelschritte (`llm.rs`): `chat {model}` (CLIENT).
  - **Niemals**: Prompt, Titel, Kontext, Code, Diffs, Werkzeug-Argumente/-Ergebnisse, Fehlertexte, Pfade, URLs, Secrets. `OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT` (Spec-Name) wird **nicht** unterstützt; ein anderer Wert als `NO_CONTENT` erzeugt eine Warnung im Log und bleibt wirkungslos. Begründung: Inhalte enthalten Kundencode und ggf. Secrets; der Auftrag erlaubt höchstens ein Betreiber-Opt-in, nötig ist es nicht.
  - Task-/Projekt-IDs als Span-Attribute (`queue.task.id`, `queue.project.id`) sind zulässig (Traces sind kein Metrik-Label), kein Inhalt.
- Kontext: Tokio-`task_local` für den aktuellen Span (kein Durchreichen durch 20 Funktionen).

### 2.4 Analytics (`GET /api/analytics`, `GET /api/tasks/{id}/insights`)
- Zeitfenster `days ∈ {7, 30, 90}` (sonst 400); Aggregation vollständig in SQL.
- **Mandantentrennung**:
  - `scope=mine` (Standard): eigene Aufträge/Verbrauch (`user_id = ich`), nur in Projekten, in denen ich **aktuell** Mitglied bin (Projektnamen anderer Projekte werden nie gezeigt).
  - `scope=project&project_id=…`: alle Aufträge eines Projekts – nur mit Rolle ≥ Betrachter (wie `GET /api/projects/{id}/tasks`, das Betrachtern heute schon jeden Auftrag samt Kosten zeigt; strengere Regeln wären inkonsistent). Nicht-Mitglied → 404.
  - Pausen sind nutzerbezogen und erscheinen nur bei `scope=mine`.
  - Session-Insights: gleiche Regel wie `GET /api/tasks/{id}` (Eigentümer oder Projektmitglied).
- Kennzahlen: First-Pass-Quote (Ursprungsaufträge `kind<>'fix'`, die ohne Reparaturauftrag `done` wurden ÷ alle beendeten `done|failed|blocked`), Endgültig erfolgreich (inkl. Fix-Kette, rekursives CTE), Blockiert-Quote, Abbrüche; Kosten je Währung und Tokens pro Projekt / Anbieter+Modell / Auftrag (Top 10) / Tag (Zeitreihe); Kosten pro erledigtem Auftrag; Cache-Quote pro Anbieter; Dauer bis erledigt (Median/P90 von `finished_at-created_at`) und reine Laufzeit (Summe `task_runs`); Pausenminuten pro Anbieter und Grund (Vereinigung überlappender Intervalle mit `range_agg`, PG ≥ 14 – hier 16.15).
- Indizes (neue Migration): `tasks (user_id, finished_at)`, `tasks (project_id, finished_at)` (partiell `finished_at IS NOT NULL`), `usage_log (task_id)`, `usage_log (user_id, ts)`, `task_runs (task_id)`, `task_runs (user_id, started_at)`, `provider_pauses (user_id, started_at)`.
- Performance-Test mit realistischen Mengen (Integrationstest): ~60 000 Aufträge, ~250 000 `usage_log`-Zeilen, 50 Nutzer; Antwortzeit-Grenze + `EXPLAIN` ohne Seq-Scan auf `usage_log`/`tasks` für den 30-Tage-Pfad.

### 2.5 Benachrichtigungen
- **Ereignisse** (`queued_api::notify::Event`, aus `kind` + bisherigem `push`-Flag abgeleitet, Aufrufstellen bleiben): `task_done`, `needs_input` (kritisch), `approval` (kritisch: Freigabe, Plan, Gate), `blocked` (kritisch), `provider` (Pausen, Budget, Limits), `pr_attention`, `pr_update`, `automation` (Auslöser, Intake), `followups` (Folgeaufträge, Leerlauf), `knowledge`, `digest`. `test` umgeht Einstellungen und Ruhezeiten.
- **Kanäle**: `inapp`, `push` (Web Push), `ntfy`, `webhook`. Matrix in `notification_prefs` (nur Abweichungen vom Standard gespeichert). Standard = bisheriges Verhalten (extern genau dort, wo bisher `push=true`; Webhook standardmäßig aus).
- **Ruhezeiten**: Start/Ende (Minuten 0–1439, über Mitternacht erlaubt), IANA-Zeitzone (`chrono-tz`, schon Dependency), an/aus. In der Ruhezeit: in-app ja, Push/ntfy nein (Notiz `quiet_suppressed`), Webhook unberührt (Maschinen schlafen nicht). Kritische Ereignisse brechen durch, wenn „Wichtiges trotzdem melden“ an ist (Standard an). Zeitumstellung: Berechnung mit lokaler Uhrzeit des Zeitpunkts.
- **Tägliche Zusammenfassung** zur gewählten lokalen Uhrzeit: erledigt/fehlgeschlagen/blockiert, Kosten je Währung, wartet auf dich (Freigaben/Rückfragen/Blockiert), in der Ruhezeit zurückgehaltene Meldungen, Pausenminuten. Leerer Zeitraum → nichts senden (Zeile wird trotzdem verbucht).
  - **Genau einmal pro Nutzer und lokalem Datum**: `notification_digests (user_id, local_date)` PRIMARY KEY; Anlage von Digest-Zeile, in-app-Notiz und Outbox-Einträgen in **einer** Transaktion (`INSERT … ON CONFLICT DO NOTHING RETURNING`). Zwei Instanzen/Neustart → höchstens eine gewinnt.
  - Nachholen nach Ausfall: bis 12 h nach der Uhrzeit am selben lokalen Tag, sonst entfällt der Tag.
- **Outbox** `notification_outbox` für alle externen Kanäle: Lease (`FOR UPDATE SKIP LOCKED`, 60 s), 5 Versuche mit Backoff, danach `failed`. Neustart/mehrere Instanzen: keine Doppelzustellung außer im Fenster „extern gesendet, aber Markierung nicht mehr geschrieben“ (dokumentiert). Schleife `notify::run` (Tick `NOTIFY_TICK_SECS`, Standard 15 s; 0 = Tests treiben selbst) + Wecker.
- **Webhook-Kanal**: HTTPS-URL (HTTP nur bei `ALLOW_PRIVATE_NOTIFY_TARGETS`), Secret `qwh_…` (bestehendes Format/Scanner aus WP07) einmal angezeigt, verschlüsselt gespeichert; Header `X-Queue-Event`, `X-Queue-Delivery`, `X-Queue-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`.
- **SSRF**: ntfy, Web Push und Webhook nur über `llm::client(guard)` + `check_target`; Link-Local immer gesperrt, Loopback/privat nur mit `ALLOW_PRIVATE_NOTIFY_TARGETS=true` (Verhaltensänderung für LAN-ntfy, im README).

### 2.6 UI (Leptos, WP00-Designsystem)
- Neuer Bereich **„Auswertung“** (`/analytics`, fünfter Tab; Tab-Leiste mobil prüfen): Zeitraum (7/30/90 Tage), Bereich (Meine Aufträge / Projekt), Kennzahlkacheln (Erfolg beim ersten Versuch, Kosten, Cache-Quote, Dauer bis erledigt, Pausen), Balken als reines SVG/CSS ohne JS-Bibliothek, **jedes Diagramm mit Tabelle** (`<details>` „Als Tabelle anzeigen“ bzw. visuell versteckte Tabelle + `role="img"` mit `aria-label`-Zusammenfassung), Erklärungen in Alltagssprache, Leerzustand.
- Drawer: Block **„Auswertung dieses Auftrags“** (Läufe, Zeiten, Tokens, Cache-Quote, häufigste Werkzeuge, Hinweise in Klartext).
- Einstellungen → Benachrichtigungen: Matrix Ereignis × Kanal (Checkboxen mit Beschriftung, mobil als Liste je Ereignis), Ruhezeiten (Zeitfelder, Zeitzone mit Browser-Vorschlag), „Wichtiges trotzdem melden“, Zusammenfassung (an/aus, Uhrzeit), Webhook (URL, Secret erneuern, Test). 16-px-Inputs, 44-pt-Ziele, Safe-Areas, axe.

## 3. Datenmodell (Migration `0013_observability_notifications.sql`, 0001–0012 unverändert)

```sql
CREATE TABLE task_runs (id BIGSERIAL PK, task_id UUID FK CASCADE, user_id UUID FK CASCADE, project_id UUID FK CASCADE,
  kind TEXT CHECK (agent|review|plan), provider TEXT, model TEXT, started_at, ended_at, outcome TEXT,
  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens BIGINT, tool_calls INT, tools JSONB);
CREATE TABLE provider_pauses (id BIGSERIAL PK, user_id FK, provider TEXT, reason CHECK (rate_limit|auth|budget), started_at, ends_at NULL);
CREATE TABLE notification_settings (user_id PK FK, timezone TEXT, quiet_enabled, quiet_start SMALLINT, quiet_end SMALLINT,
  quiet_critical BOOL, digest_enabled BOOL, digest_minute SMALLINT, webhook_url TEXT, webhook_secret_enc BYTEA, updated_at);
CREATE TABLE notification_prefs (user_id, event, channel, enabled, PK(user_id,event,channel));
CREATE TABLE notification_digests (user_id, local_date DATE, period_start, period_end, notification_id, created_at, PK(user_id, local_date));
CREATE TABLE notification_outbox (id BIGSERIAL, user_id, notification_id NULL, channel CHECK(push|ntfy|webhook), event, payload JSONB,
  status CHECK(pending|sent|failed), attempts, next_at, lease_until, last_error, created_at, sent_at);
ALTER TABLE notifications ADD event TEXT, ADD quiet_suppressed BOOLEAN DEFAULT FALSE;
+ Indizes aus 2.4
```

## 4. Tests (Pflicht laut Auftrag fett)

| Ebene | Inhalt |
|---|---|
| Unit | Metrik-Format (Escaping, Histogramm, Deckel), Ruhezeit-Fenster (über Mitternacht, Zeitumstellung, Zeitzonen), Digest-Fälligkeit/Nachholfenster, Ereignis-Ableitung, Werkzeugnamen-Bereinigung, Provider-Mapping, OTLP-JSON gegen offizielle Proto-Typen, Webhook-Signatur |
| Integration (App+Runner in-process, Postgres) | **`/readyz` bei DB- und Runner-Ausfall** (DB-Pool geschlossen, Runner-Port tot, falsches Token), **`/metrics`-Schutz und Format** (ohne/falsches/richtiges Token, 404 ohne Konfiguration, Inhalt ohne IDs/Titel), **keine Inhalte in Spans** (Fake-OTLP-Collector, Auftrag mit Geheimnis-Marker im Prompt/Titel/Werkzeugargument → kein Vorkommen im Export), **Mandantentrennung der Analytics** (zwei Nutzer, geteiltes Projekt, Betrachter, Nicht-Mitglied, entfernter Nutzer), **Ruhezeiten und Zusammenfassung ohne Doppelversand über Neustart** (zwei AppStates auf derselben DB, parallele Ticks, Neustart zwischen Anlage und Zustellung, Fake-ntfy zählt), Outbox-Retry, Webhook mit Fake-Empfänger (Signatur), SSRF-Sperre, Pausen-Historie, Performance mit großen Datenmengen, `GET /api/tasks` mit `merging` |
| Runner | `/metrics` mit eigenem Token, `RUNNER_TOKEN` gilt dort nicht und umgekehrt, keine Task-IDs in Labels |
| System (getrennte Prozesse, Playwright `system`) | `/healthz`, `/readyz`, `/metrics` von App und Runner mit Token; Analytics-API zweier Nutzer |
| E2E Desktop/iPhone | Auswertung (Kacheln, Diagramm + Tabelle, Drawer-Insights), Benachrichtigungs-Einstellungen speichern/neu laden, axe, Safe-Areas, kein horizontales Scrollen, 16-px-Inputs |

Keine echten externen Calls: Fake-OTLP-Collector, Fake-ntfy, Fake-Webhook-Empfänger, Web Push nur gegen Fake-Endpunkt (oder gar nicht). Keine LLM-Live-Calls (Kosten 0 USD).

## 5. Risiken

| Risiko | Gegenmaßnahme |
|---|---|
| Eigener OTLP-Exporter weicht vom Standard ab | Prüfung gegen offizielle Proto-Typen; nur `http/json`; Collector-Kompatibilität ist dennoch **nicht live verifiziert** (kein echter Collector hier). |
| GenAI-Konventionen sind „Development“ und gerade umgezogen | Commit/Datum im README; Attributnamen an einer Stelle. |
| Doppelte Zusammenfassung bei mehreren Instanzen | PK + Transaktion; Test mit zwei AppStates. |
| Kardinalitätsexplosion | Feste Labelmengen, `price_id` statt Modellname, Deckel im Registry. |
| Analytics langsam bei Wachstum | Indizes, Zeitfenster, SQL-Aggregation, Performance-Test. |
| Längere `usage_log`-Aufbewahrung vergrößert die Tabelle | Budget-Abfragen nutzen `(user_id, provider, ts)` mit Zeitfilter; Aufbewahrung konfigurierbar. |
| Verhaltensänderung ntfy im LAN | README + Env-Schalter. |
| 5. Tab auf dem iPhone zu eng | Mobile-Test prüft 44 pt und Sichtbarkeit aller Tabs. |
