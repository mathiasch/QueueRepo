# WP13 – Provider-Anbindungen & Kostenoptimierung: Umsetzung

Grundlage: `WP13-plan.md`. Branch `mvp`, nicht committet. Testergebnisse: `WP13-test.md`, Urteil: `WP13-review.md`.

## 1. Was gebaut wurde

| Baustein | Dateien | Kurz |
|---|---|---|
| Migration | `backend/crates/app/migrations/0004_providers_costs.sql` | `connectors`, `batch_jobs`, `price_overrides`, `eval_runs/results`; Spalten an `tasks` (`connector_id`, `escalation_level`, `plan`, `cost`), `projects` (`routing`, `data_residency`, `is_eval`), `usage_log` (`project_id`, `connector_id`, `model`, `step`, `cost_source`, `currency`, `cost_amount`, `price_id`). 0001–0003 unverändert |
| Geteilte Typen | `backend/crates/api/src/costs.rs`, `providers.rs`, Ergänzungen in `lib.rs` | Preiskatalog, Kosten, Schätzung, Connectoren, Routing, Presets, CLI-Status, Eval; `TaskStatus::WaitingBatch` |
| Preiskatalog | `src/pricing/catalog.json`, `src/pricing/mod.rs` | 26 Preiszeilen, 7 Regionsregeln, 17 Quellen; Matching (Bedrock-/Vertex-/OpenRouter-IDs), Formel inkl. Cache 5m/1h, Batch/Flex, Long-Context pro Request, Region, Gültigkeitsdatum; `Meter` misst live; Admin-Overrides versioniert |
| Connectoren | `src/connectors.rs`, `src/api/providers.rs` | 15 Typen (+ Test-Typ), Konfig-Validierung, Datenstandort, Regionsregel, Wire/Batch-Fähigkeit, Zugriff (Eigentümer bzw. Projekt-Bearbeiter), CRUD, Prüfen, Pause aufheben |
| Agent-Adapter | `src/agent/{mod,claude,codex,gemini,opencode}.rs` | Claude: Abo, API (+Proxy-URL), Bedrock (Geo-Profil erzwungen), Vertex (ADC-Datei), Gateway/LiteLLM; Codex: Abo, API (+Flex per User-`config.toml`), Azure/LiteLLM als Responses-Provider; Gemini: Abo-Connector; **opencode** neu (Invocation + Parser) |
| Routing | `src/routing.rs` | 5 Presets, Auflösung gegen vorhandene Zugänge, Validierung (Residency, Review-Stufe, Batch/Direkt-Fähigkeit, Modellpflicht), `resolve_execute` für neue Aufträge, Eskalation |
| Kostenschätzung | `src/estimate.rs` | Report-Profile S/M/L (Annahme) bzw. eigene Historie (Median/P10/P90 ab 3 Aufträgen) |
| Einzelschritte | `src/llm.rs`, `src/batch.rs` | Direkter Aufruf (Anthropic Messages, OpenAI-kompatibel) und Batch (Anthropic, OpenAI, Mistral), Poller, Retry, Ablauf 25 h, Abbruch, Neustart, Waisen-Sweep, SSRF-Schutz |
| Gateway | `src/gateway.rs` | LiteLLM-Budget-Schlüssel pro Lauf |
| Runner-Probe | `crates/runner/src/{exec,lib}.rs`, `crates/common` | `GET /v1/cli-status`: `--version` einer festen CLI-Liste in der Sandbox |
| Eval | `evals/cases.json`, `src/eval.rs`, `src/api/evals.rs` | 8 Aufgaben, versteckte Projekte, keine Fix-Tasks, Mock-Stärken |
| Integration | `src/runner.rs`, `src/worker.rs`, `src/queue.rs`, `src/api/{tasks,projects,mod}.rs` | Zugriffs-/Residency-Prüfung je Lauf, Kosten pro Schritt, Pausen pro Connector, Plan/Review/Analyse-Aufschub, Eskalation, Scheduler connector-aware |
| Agent-Image | `docker/agent.Dockerfile` | `opencode-ai@1.19.5` gepinnt + Versionscheck (Image **nicht gebaut**) |
| Frontend | `frontend/src/pages/{providers,composer,drawer,queue,project,settings}.rs`, `logic/costs.rs`, `store.rs`, `style/app.css` | Zugänge, Kosten, Preistabelle (+Admin), CLI-Status, Modell-Test, Composer mit Zugang/Modell/Schätzung, Drawer mit Plan und Kosten je Schritt, Bereich „Beim Anbieter“, Projekt-Modellwahl/Datenstandort/Kosten |
| Doku | `README.md`, `deploy/queued.env.example` | Preis-/Connector-Übersicht, Presets, Batch, Residency, Eval, neue Env-Variablen |

## 2. Wichtige Entscheidungen (mit Begründung)

1. **Legacy-Zugänge unverändert, Connectoren zusätzlich.** `provider_credentials` und Aufträge ohne `connector_id` verhalten sich wie vorher (alle Alt-Tests unverändert grün). Grund: Regel „bestehende Credential-Mechanik bleibt unverändert“.
2. **`tasks.provider` = Harness.** Budgets bleiben pro Harness (Usage wird unter `claude|codex|opencode|…` geloggt, zusätzlich `connector_id`). Pausen (Rate-Limit, Auth) eines Connectors laufen unter `connector:<uuid>` in `provider_limits`; der Scheduler blockiert Legacy-Aufträge über Provider-Pausen/Budgets, Connector-Aufträge über Connector-Pausen und Harness-Budget.
3. **Kosten aus Tokens × Katalog**, CLI-Dollar nur als Fallback (`cost_source='cli'`), sonst `unknown` (0 $, sichtbar markiert). Bei Claude wird Long-Context pro `message.id`-Delta (= Request) bewertet, bei opencode pro `step_finish`; Rest (Totals, Codex) zur Basisstufe mit Hinweis „Untergrenze“. Der Live-Kosten-Guard bekommt Katalogkosten, wirkt also auch für opencode und Gateways.
4. **CHF bleibt CHF.** Infomaniak-Preise werden in CHF gerechnet und ausgewiesen; `usage_log.cost_usd` (Budget) bekommt dafür 0, weil kein belegter Wechselkurs existiert.
5. **Einzelschritte in der App**, nicht im Runner: kein Prozess, keine Datei, Secrets verlassen die App nicht. Kontext (Dateiliste, Diff) kommt wie bisher über die Runner-API.
6. **`waiting_batch`** belegt keinen Slot und sperrt das Projekt nicht (Test mit Parallelität 1 belegt das). Jobs persistent, Lease 15 min, `recover()` setzt Leases beim Start zurück.
7. **Bedrock/Vertex nur als Agent.** Direkte Einzelschritte bräuchten SigV4 bzw. Google-OAuth-Eigenbau gegen nicht prüfbare Endpunkte. Im UI als „Planung/Review direkt: –“ sichtbar; Presets fallen auf agentisches Review zurück.
8. **Bedrock erzwingt das Geo-Profil**: Modell `claude-sonnet-5-5` → `eu.anthropic.claude-sonnet-5-5`; ein `us.`-Profil an einem EU-Zugang und ein fehlendes Modell werden abgelehnt; der Hintergrundmodell-Name (`ANTHROPIC_SMALL_FAST_MODEL`, `ANTHROPIC_DEFAULT_HAIKU_MODEL`) wird auf dasselbe Profil gesetzt, damit keine Nebenanfrage den Datenraum verlässt.
9. **Codex-Provider nur in der User-`config.toml`** (Home-Datei), Schlüssel per `env_key`/`env_http_headers` – nie in der Datei.
10. **opencode-Konfiguration per `OPENCODE_CONFIG_CONTENT`**, Schlüssel als `{env:QUEUE_LLM_API_KEY}`; alle Provider außer Gemini über `@ai-sdk/openai-compatible` mit der Basis-URL des Connectors (eine Implementierung statt fünf).

## 3. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| Eval-Ergebnisse in `eval_results` mit Kosten/Tokens | `eval_results` speichert nur Run/Fall/Task/Projekt; Kennzahlen werden beim Lesen aus den Tasks berechnet | keine doppelte Wahrheit; Kosten stehen ohnehin in `tasks.cost` |
| Gemini-Status nur statisch | zusätzlich **Runner-Probe** `GET /v1/cli-status` (auch `agy`) | „Status-Check“ soll den tatsächlichen Stand der Sandbox zeigen; Befund der npm-Prüfung als statischer Hinweis in `/api/meta` |
| – | **SSRF-Schutz** (`llm::check_target`, `ALLOW_PRIVATE_LLM_ENDPOINTS`) | im Selbst-Review als blockierend gefunden: die App ruft jetzt nutzergesteuerte URLs auf |
| – | **Waisen-Sweep** für `waiting_batch` | im Selbst-Review gefunden: ein Abbruch zwischen Job-Ende und Task-Fortsetzung hätte den Auftrag ewig hängen lassen |
| Projekt-Routing frei editierbar | UI: Presets + „Zurück zum Standard“; freie Schritt-Konfiguration nur per API (`PUT /api/projects/{id}/routing`) | Bedienbarkeit für normale Menschen; die API validiert jede Kombination identisch |
| Mock nutzt keine Schlüssel | Test-Connector-Typ `mock` (nur mit `ENABLE_MOCK_PROVIDER`) mit optionalem Schlüssel und Fake-Server-URL | Batch/Direkt/Redaktion/Auth-Pausen lassen sich nur so ohne echte APIs Ende-zu-Ende testen |
| Kostenlimit-Hinweise „Schätzung der CLI“ | Texte angepasst: Live-Guard mit Katalogpreisen, wo Live-Usage existiert | alte Aussage stimmte nicht mehr |

## 4. Belegte Fakten vs. Annahmen (Code-Ebene)

- **Im Binary/Bundle gefunden** (als Daten gegrept, nichts ausgeführt): Claude-Code-Env-Variablen für Bedrock/Vertex/Gateway, Modell-IDs; opencode-`run`-Optionen, Event-Typen und -Felder, Fehlernamen, `OPENCODE_CONFIG_CONTENT`, `{env:}`; Codex-Konfigschlüssel `service_tier`, `model_providers`, `env_key`, `env_http_headers`, `OPENAI_BASE_URL`, Enum-Wert `flex`.
- **Aus Vorwissen, nur gegen Fake-Server getestet:** Anthropic Message Batches, OpenAI Files/Batch, Mistral Batch-Jobs, Azure-Pfad `/openai/v1` + `api-key`, Infomaniak-Basis-URL (deshalb Pflichtfeld), LiteLLM `/key/generate`/`/key/delete`, Vertex-ADC-Ablage, `/v1/models` für die Verbindungsprüfung.
- **Annahmen im Katalog** sind als `assumption` bzw. in `notes` markiert (Aliase `opus/sonnet/haiku` = 5.5, Flash-Cache-Read 0.1×, `*-latest` = aktuelles Modell).
