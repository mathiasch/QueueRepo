# WP13 – Provider-Anbindungen & Kostenoptimierung: Plan

Stand: 2026-11-08, Branch `mvp` auf `06dd9c2` (WP00 abgeschlossen). Ein Agent plant, setzt um, testet und reviewt.
Quellen: `docs/roadmap/work-packages.md` (Regeln, WP13), `reports/Kostenoptimale Modellanbieter für Queue.md` (im Folgenden „Report“), `research_notes/Kostenoptimale Modellanbieter für Queue/*` („Notes“).

## 0. Was in dieser Sitzung selbst geprüft wurde (Belege)

| Behauptung | Prüfung | Ergebnis |
|---|---|---|
| Claude Code kennt Bedrock/Vertex/Gateway-Variablen | `grep -a` im installierten Binary 2.1.319 (`/opt/claude-code/bin/claude`) | `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `ANTHROPIC_VERTEX_PROJECT_ID`, `CLOUD_ML_REGION`, `AWS_BEARER_TOKEN_BEDROCK`, `AWS_REGION`, `GOOGLE_APPLICATION_CREDENTIALS`, `ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN`, `ephemeral_5m/1h_input_tokens` vorhanden. Semantik der Variablen: Vorwissen, nicht live geprüft |
| Modell-IDs Claude 5.5 | dito | `claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`, `claude-fable-5-1`; Bedrock-Profile `us.anthropic.claude-opus-5-5` (EU-Präfix `eu.` laut Report/Bedrock-Model-Card, Suchauszug); Host `aiplatform.eu.rep.googleapis.com` im Binary |
| opencode headless + JSON-Events | npm-Paket `opencode-linux-x64@1.19.5` geladen, **nicht ausgeführt** (Auto-Mode verbietet das), gebündelten JS-Quelltext gegrept | `opencode run [message..] --format json --model provider/model`; Prompt wird von stdin gelesen, wenn kein TTY; Events `{type,timestamp,sessionID,…}` mit `type ∈ {step_start, step_finish, text, reasoning, tool_use, error}`; `step_finish.part.tokens = {input,output,reasoning,cache:{read,write}}`, `part.cost`; Freigaben automatisch mit `--dangerously-skip-permissions`/`--auto`; Konfiguration per `OPENCODE_CONFIG_CONTENT`, `{env:VAR}`-Platzhalter, `OPENCODE_DISABLE_AUTOUPDATE`; AI-SDK-Provider `@ai-sdk/openai-compatible`, `@ai-sdk/mistral`, `@ai-sdk/google` gebündelt |
| Codex Flex / Custom Provider | `@openai/codex@0.167.0-linux-x64` als Daten gegrept | Konfig-Schlüssel `service_tier`, `model_providers`, `wire_api`, `env_key`, `http_headers`; Enum-Strings `fast/flex/default/priority` nebeneinander. Modell-IDs `gpt-6-astra`, `gpt-6.1-sol`, `gpt-6.1-terra`, `gpt-6-luna`. **Wirkung von `service_tier="flex"` nicht live geprüft** |
| Gemini CLI „abgelöst“ | `npm view @google/gemini-cli` | **Nicht deprecated**, `latest = 0.67.0` vom 2026-11-04, `preview = 0.68.0-preview.0`, Nightlies bis 2026-11-04. Die Sekundärquellen (Ablösung durch Antigravity, OAuth-Free-Pfad seit 18.6.2026 tot) sind damit für das npm-Paket **widerlegt bzw. unbestätigt**; der OAuth-Abo-Pfad bleibt unverifiziert |
| Aktuelle Versionen | `npm view` | codex 0.167.0 (= Pin), gemini 0.67.0 (= Pin), opencode-ai 1.19.5 (neu zu pinnen) |

Alles andere an Endpunkten/Wire-Formaten (Message Batches, OpenAI Batch, Mistral Batch, Azure-v1-Pfad, Infomaniak-Basis-URL, LiteLLM-Admin-API) stammt aus Vorwissen und wird **im Code und in der Doku als „nicht verifiziert“ markiert**. Getestet wird ausschließlich gegen lokale Fake-Server.

## 1. Ziele (aus WP13 + Auftrag) und Abgrenzung

1. **Preis-/Modell-Katalog** versioniert (`version`, `effective_from/until`, Quelle, Evidenzstufe), Input/Output/Cache-Write 5 min/1 h/Cache-Read, Batch/Flex-Faktor, Long-Context-Stufen (Haiku 5.5 >100K, OpenAI >272K), Regionsaufschläge (Bedrock/Vertex EU +10 %, Mistral EU +10 %, Azure EU Data Zone +20 %, Switzerland North +30 %), Währung (Infomaniak CHF). Admin-Overrides als neue Version in der DB (Historie bleibt).
2. **Kosten in Geld** pro Lauf/Schritt/Task/Projekt aus Token-Zählern × Katalog; CLI-Dollarwerte nur als Fallback mit Kennzeichnung (`cost_source`). Live-Kostenlimit nutzt Katalogpreise (wirkt damit auch für Codex/opencode/Gateway, sofern Live-Usage vorhanden).
3. **Kostenschätzung** pro Task und Schritt vor dem Absenden (historisches Token-Profil des Nutzers, sonst S/M/L-Profile des Reports, als Annahme gekennzeichnet).
4. **Connectoren** pro User oder Projekt, Secret AES-GCM-verschlüsselt (bestehendes `crypto.rs`), write-only. Typen siehe §3.
5. **Single-Shot-Schritte** Plan / Review / Analyse (OnEmpty) als direkter API-Aufruf, synchron oder per **Batch** (Anthropic Message Batches, OpenAI Batch, Mistral Batch) mit Polling – blockiert weder Scheduler noch Projekt. Agent-Loop bleibt synchron (CLI) mit Prompt-Caching.
6. **Routing pro Schritt** + Presets „Günstig“, „Ausgewogen“, „Beste Qualität“, „Nur EU/CH“, „Maximal günstig“, „Eigene“. Eskalation Haiku→Sonnet→Opus über Fix-Versuche. Review nie auf der billigsten Stufe (serverseitig erzwungen).
7. **Datenstandort-Regel pro Projekt** (`any|eu|ch`), serverseitig erzwungen bei Anlegen/Ändern/Claim/Lauf/Review/Plan/Analyse/Folge-/Fix-Tasks.
8. **Gemini-Status-Check**: Runner-Probe `GET /v1/cli-status` (`<cli> --version` für eine feste Liste im Sandbox-Kontext) + statischer Befund (§0) in `/api/meta`.
9. **Eval-Set**: 8 kleine Coding-Aufgaben mit Setup- und Verify-Skript, Mock-Modus deterministisch (simulierte Modellstärken), Ergebnisse pro Modell/Route in der DB (Grundlage WP15).
10. **UI (Leptos)**: Zugänge verwalten, Preset/Datenstandort im Projekt, Schätzung im Composer, Kosten in Details/Projekt/Einstellungen, Preistabelle (Admin editierbar), Eval starten/ansehen.

**Nicht in WP13:** Egress-Allowlist (WP02), Lizenz/ToS (nur Hinweistexte), Task-Analyse/Bandit (WP15), Best-of-N (WP12). Bedrock/Vertex werden nur über Claude Code (Agent-Loop, agentisches Review) angebunden, **nicht** für direkte Single-Shot-/Batch-Calls (SigV4/Google-OAuth wären ungeprüfte Eigenbauten) – im UI als „Batch nicht verfügbar“ sichtbar.

## 2. Architektur

```
queued-api  (geteilte DTOs)  ConnectorTypeInfo, ConnectorView, Routing, StepRoute, PriceEntry, Estimate, TaskCost, CostSummary, EvalRun …
   ▲
queued (App, keine Host-Rechte)
   pricing/      catalog-2026-11-07.json (include_str), Matching, Kostenformel, Overrides aus DB
   connectors.rs Registry der Typen (Harness, Wire, Datenstandort, Felder, Verifikationsstatus) + CRUD + Secret-Handling
   agent/opencode.rs  dritter CLI-Adapter (Invocation + Parser für --format json)
   agent/* (claude/codex) erweitert um Connector-Konfiguration (Bedrock/Vertex/Gateway, Azure/Flex)
   routing.rs    Presets, Auflösung, Validierung (Residency, Review-Stufe, Batch-Fähigkeit)
   llm.rs        Single-Shot-Client (anthropic messages, openai-chat-kompatibel)
   batch.rs      Batch-Jobs: Submit/Poll/Ergebnis (anthropic, openai, mistral), Poller-Loop, Fortsetzung des Tasks
   gateway.rs    LiteLLM: virtueller Key pro Task mit max_budget, danach löschen
   eval.rs       Eval-Set laden, Läufe anlegen, Ergebnisse aggregieren
   runner.rs     Lauf nutzt Connector, Kosten pro Schritt, Plan-/Review-/Analyse-Schritt-Dispatch
queued-runner   + GET /v1/cli-status (feste CLI-Liste, `--version`, Timeout)
queued-web      Zugänge, Routing, Schätzung, Kosten, Preise, Eval
```

Entscheidungen und Begründung:
- **Legacy bleibt unverändert:** `provider_credentials` und Tasks ohne `connector_id` verhalten sich exakt wie vorher (Regel „bestehende Credential-Mechanik bleibt unverändert“, alle Alt-Tests). Connectoren sind ein zusätzlicher, expliziter Weg.
- **`tasks.provider` = Harness** (`claude|codex|gemini|opencode|mock`), `tasks.connector_id` optional. Pausen/Rate-Limits eines Connectors laufen unter dem Schlüssel `connector:<uuid>` in `provider_limits` (ein kaputter Mistral-Key pausiert nicht alle opencode-Zugänge). Budgets bleiben pro Harness (Usage wird unter dem Harness geloggt, zusätzlich `connector_id`).
- **Direkte API-Calls aus der App** (Batch/Sync/LiteLLM-Admin): Secrets verlassen die App nicht, keine Prozess-/Dateiarbeit → Architekturgrenze bleibt. Kontext (Dateiliste, Diff) holt die App wie bisher über die Runner-API.
- **Batch-Task-Zustand `waiting_batch`:** belegt keinen Slot und sperrt das Projekt nicht; Poller (`BATCH_POLL_SECS`) ist eine eigene Tokio-Schleife; Jobs sind persistent (Neustart-fest). Sync-Single-Shots laufen ebenfalls über die Job-Tabelle (Modus `sync`), damit Neustart/Abbruch einheitlich sind.
- **Kostenformel pro Anfrage:** Claude liefert Usage pro `message.id` (= API-Request) → Long-Context-Stufe pro Request. Für den Rest (Totals ohne Deltas, Codex, Subagents) Basisstufe + Hinweis „Long-Context-Aufschlag nicht prüfbar“. Cache-Writes: Claude-Stream unterscheidet 5m/1h nicht im aggregierten Feld → 5m angenommen (Annahme, dokumentiert); Batch-Requests setzen `ttl: 1h` und werden mit 1h-Preis gerechnet.

## 3. Connector-Typen

| Typ | Harness | Wire (Single-Shot/Batch) | Datenstandort | Secret | Konfiguration | Verifikation |
|---|---|---|---|---|---|---|
| `claude_subscription` | claude | – | US | OAuth-Token (`claude setup-token`) | – | wie WP01 |
| `anthropic_api` | claude | anthropic / Message Batches | US (Workspace-Daten nur US, Report) | API-Key | – | Batch-Wire: Vorwissen |
| `bedrock` | claude | – | EU (Geo-Profil `eu.`) / wählbar | AWS-Bearer-Token **oder** Access-Key-JSON | Region (Default `eu-central-1`) | Env-Namen im Binary, Semantik Vorwissen |
| `vertex` | claude | – | EU (`CLOUD_ML_REGION=eu`) / wählbar | Service-Account-JSON | GCP-Projekt-ID, Region | dito |
| `anthropic_gateway` | claude | anthropic (sync) | vom Nutzer deklariert | Token | Base-URL | – |
| `codex_subscription` | codex | – | US | `auth.json` | – | wie WP01 |
| `openai_api` | codex | openai-chat / OpenAI Batch | US | API-Key | Flex an/aus | Flex-Wirkung unverifiziert |
| `azure_openai` | codex | openai-chat (`api-key`-Header) | EU Data Zone / Switzerland North / Global | API-Key | Ressourcen-URL, Bereitstellungsart | Pfad `/openai/v1` Vorwissen |
| `gemini_subscription` | gemini | – | US | `oauth_creds.json` | – | OAuth-Pfad laut Sekundärquellen gestört (§0) |
| `gemini_api` | opencode (`@ai-sdk/google`) | – | Global | API-Key | – | – |
| `mistral` | opencode | openai-chat / Mistral Batch | EU (EU-Endpoint, +10 %) oder Global | API-Key | Endpoint eu/global | Batch-Wire Vorwissen |
| `infomaniak` | opencode | openai-chat | CH | API-Token | Base-URL (Produkt-ID) | Base-URL-Schema unverifiziert |
| `openai_compatible` | opencode | openai-chat | vom Nutzer deklariert (Self-Hosting) | optional | Base-URL | – |
| `openrouter` | opencode | openai-chat | Global oder EU (`eu.openrouter.ai`, Business) | API-Key | Region | Account-Daten ggf. US (Report) |
| `litellm` | claude **oder** codex **oder** opencode | anthropic oder openai-chat | vom Nutzer deklariert | Master-/Virtual-Key | Base-URL, Harness, „Budget-Key pro Task“ | Admin-API Vorwissen |
| `mock` (nur `ENABLE_MOCK_PROVIDER`) | mock | Fake-Server-URL konfigurierbar (Tests) | lokal | – | – | Test |

Datenstandort-Ordnung: `ch` ⊂ `eu` (CH erfüllt EU-Policy; EU-Data-Zone von Azure zählt als EU, nicht CH). „Vom Nutzer deklariert“ wird im UI als „selbst angegeben“ markiert; `local` (Mock) erfüllt jede Policy.

## 4. Datenmodell (Migration `0004_providers_costs.sql`)

```sql
connectors(id, user_id → users CASCADE, project_id → projects CASCADE NULL, kind, label, config JSONB,
           secret_enc BYTEA NULL, created_at, updated_at)
tasks      + connector_id → connectors SET NULL, + escalation_level INT DEFAULT 0, + plan TEXT, + cost JSONB
             (Status-Wert neu: 'waiting_batch'; kein CHECK vorhanden)
projects   + routing JSONB, + data_residency TEXT DEFAULT 'any' CHECK (any|eu|ch), + is_eval BOOL DEFAULT false
usage_log  + project_id, connector_id, model, step CHECK(execute|plan|review|analyze), cost_source CHECK(catalog|cli|unknown),
             currency DEFAULT 'USD', cost_amount, price_id
batch_jobs(id, task_id CASCADE, user_id, step, mode CHECK(sync|batch), api, connector_id SET NULL, model, status CHECK(...),
           external_id, request JSONB (ohne Secret), result TEXT, usage JSONB, error, attempts, next_poll_at, submitted_at, finished_at)
price_overrides(version SERIAL, entries JSONB, note, created_by, created_at)   -- jede Admin-Änderung = neue Version
eval_runs(id, user_id, target JSONB, status, created_at, finished_at)
eval_results(run_id, case_id, task_id, project_id, success, cost JSONB, tokens BIGINT, duration_secs, model, connector_kind)
```

Löschen eines Connectors: 409, solange nicht-finale Tasks ihn nutzen; finale Tasks behalten ihre Kosten (FK SET NULL).

## 5. Kostenformel

Für eine Usage `u` (input, cache_write, cache_read, output) und Preisposten `p` am Datum `d` (gültig `effective_from ≤ d < effective_until`):
`cost = (in·p.in + cw·p.cw(ttl) + cr·p.cr + out·p.out)/1e6 × tier × region × lc`
- `tier`: 1 / `batch_factor` / `flex_factor`; `region`: Multiplikator aus Katalog je Connector-Region; `lc`: Long-Context-Stufe (`whole_request`: alle Raten des Requests; Haiku: eigene Preiszeile >100K; OpenAI >272K: Input/Cache ×2, Output ×1.5).
- Fehlende Cache-Preise: Cache-Read = Input×`cache_read_mult` nur, wenn der Katalog es angibt; sonst Input-Preis (konservativ, als `assumed` markiert).
- Unbekanntes Modell → `cost_source='cli'` (falls die CLI Dollar meldet) sonst `unknown` (Kosten 0, UI: „Kosten unbekannt – Preis in der Preistabelle ergänzen“).

## 6. Abläufe

**Anlegen:** Composer wählt „Zugang & Modell“ (Default: Projekt-Routing → Nutzer-Default). Server löst Execute-Ziel auf, prüft Residency/Zugriff, speichert `provider`(Harness)+`connector_id`+`model`.

**Lauf:** `run_inner` lädt Connector (Secret entschlüsselt nur im Speicher), prüft Residency erneut, ggf. LiteLLM-Task-Key, baut Invocation, rechnet Kosten pro Delta (Guard) und am Ende pro Schritt, schreibt `usage_log` (+Kosten) und `tasks.cost`.

**Plan-Schritt (sync/batch):** Beim Claim eines Tasks ohne `plan` und mit aktivem Plan-Schritt: Job anlegen, Task → `waiting_batch`; Poller holt Ergebnis → `tasks.plan`, Task zurück auf `queued` (Position bleibt), Prompt enthält den Plan. Fehler/Ablauf (24 h) → ohne Plan weiter, Ereignis + Hinweis.

**Review-Schritt (sync/batch):** nach Agent-Lauf + Verify: Diff (Runner, gekürzt) → Job → Task `waiting_batch` (Zusammenfassung/Übergabe/Snapshots gespeichert, Folgeaufgaben im Job-Payload) → Poller wertet `ReviewResult` aus → `done` (+Auto-Push, Folgeaufgaben) bzw. `failed` (+Fix mit Eskalation). Fehler → „Review übersprungen“ wie bisher.

**Analyse (OnEmpty):** Task `onempty` mit Analyse-Schritt sync/batch → Job statt Agent-Lauf; Ergebnis als Contract-JSON → Folgeaufgaben (Freigabe nötig wie bisher).

**Abbruch:** `waiting_batch` → Job `cancelled` (Batch-API-Cancel best effort), Task `cancelled`.

## 7. Akzeptanzkriterien (je mit Testart)

| AK | Kriterium | Test |
|---|---|---|
| 1 | Katalog lädt, jede Position hat Quelle+Evidenz, Preise = Report/Notes (Stichproben Opus/Sonnet/Haiku/GPT/Gemini-Sprung/Mistral/Infomaniak) | U |
| 2 | Kostenformel inkl. Cache, Batch/Flex, Haiku >100K, OpenAI >272K, Region, Gültigkeitsdatum | U |
| 3 | Admin-Override = neue Version, wirkt sofort, Nicht-Admin 403, Validierung | I |
| 4 | Lauf schreibt Kosten pro Schritt in `usage_log`/`tasks.cost` aus Katalog (nicht CLI-Dollar) | I |
| 5 | Live-Kostenlimit greift mit Katalogpreisen | I |
| 6 | Connector-CRUD: Secret nie in Antworten/Events/argv, verschlüsselt in DB, Projekt-Connector nur für Mitglieder | I + U |
| 7 | Invocation je Typ (Env/Home-Files korrekt, kein Secret in argv) | U |
| 8 | opencode-Parser gegen Fixtures (Herkunft dokumentiert) | Contract |
| 9 | Residency: EU-Projekt lehnt US-Connector ab (Anlegen, Ändern, Routing, Lauf nach Connector-Änderung) | I |
| 10 | Presets: Auflösung gegen vorhandene Zugänge, fehlender Zugang → verständlicher Fehler; Review-Billigstufe → 400 | U + I |
| 11 | Batch Plan/Review/Analyse gegen Fake-Anthropic-/OpenAI-/Mistral-Server, Scheduler arbeitet parallel weiter, Neustart-fest, Abbruch | I |
| 12 | Eskalation: Fix-Task nutzt nächstes Modell der Kette | I |
| 13 | Schätzung: Report-Profil ohne Historie, Median mit Historie, alle Schritte | U + I |
| 14 | LiteLLM-Task-Key: erzeugt mit `max_budget`, im Lauf verwendet, danach gelöscht (Fake-Server) | I |
| 15 | CLI-Status-Probe (Runner) | U (Runner) + I |
| 16 | Eval-Set im Mock-Modus reproduzierbar (2 Läufe identisch), Ergebnisse pro Modell | I |
| 17 | UI: Zugang anlegen, Preset wählen, Schätzung sichtbar, Kosten im Detail, EU-Regel sichtbar | E2E desktop + mobil, axe |
| 18 | Gates: fmt/clippy/test Backend + Frontend (wasm32+nativ), Playwright alle Projekte, 2× grün | Bericht |

## 8. Risiken

- **Ungeprüfte Wire-Formate** (Batch-APIs, Azure, Infomaniak, LiteLLM-Admin): Fake-Server testen nur unsere Annahme. Gegenmaßnahme: jede Annahme an einer Stelle (Konstanten + Kommentar „nicht verifiziert“), Doku-Liste in README.
- **opencode lädt Repo-`opencode.json`** (wie Gemini-Trust): Repo kann Konfiguration beeinflussen → Härtung WP03, dokumentiert.
- **Projekt-Connectoren**: Bearbeiter nutzen den Schlüssel des Eigentümers, der Agent sieht ihn im Container (wie heute beim eigenen Key) → UI-Hinweis, nur Eigentümer verwaltet.
- **Kosten bei Gateway-Fremdmodellen** hängen am Modellnamen, den der Nutzer einträgt; unbekannt → „unbekannt“, nie geraten.
- **Long-Context nur bei Claude pro Request** exakt; sonst Untergrenze.
- **Umfang**: sehr groß. Priorität bei Zeitnot: Katalog/Kosten → Connectoren/Residency → Routing/Batch → UI → Eval. Kein Punkt wird stillschweigend weggelassen; Abweichungen stehen in `WP13-impl.md`.
- **Plattenplatz**: `backend/target` 13 GB, 15 GB frei → `frontend/target` nach Tests leeren, Downloads im Scratchpad löschen.
