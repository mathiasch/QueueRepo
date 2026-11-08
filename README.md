# Queue – agentische Coding-Queue

Aufträge per Web-UI (Desktop oder iPhone als PWA) einreihen. Pro Auftrag arbeitet ein **frischer Agent** (Claude Code, Codex/GPT oder Gemini CLI) ihn in einer **rootless Sandbox** ab. Danach wird verifiziert, reviewt und **in Git committet**, und die Queue geht zum nächsten Auftrag. Ist sie leer, laufen **OnEmptyQueue**-Aktionen (Review, Web-Recherche, Verbesserungen), die die Queue wieder befüllen können.

- **Multi-User/Multi-Tenant:** Jeder User hat eine Queue und sein eigenes Abo. Mehrere Projekte pro User sind möglich, Projekte lassen sich teilen (Owner/Editor/Viewer).
- **Login:** lokale Accounts (Argon2) oder OIDC/OAuth2 (Google, Apple, GitHub, Keycloak, Authentik, Entra …).
- **Jedes Projekt ist ein Git-Repo:** neu angelegt oder von einem Remote geklont. Pro Auftrag gibt es einen Commit (Autor = User), Rückgängig per `git revert`, optional Auto-Push.
- **Token-Budgets pro Provider** (Claude, Codex/GPT, Gemini) in 5h- und 7-Tage-Fenstern plus 30-Tage-USD. Erkannte Abo-Limits pausieren den Provider bis zum Reset.
- **Kein Root zur Laufzeit:** Die App hat keinerlei Host-Rechte. Der separate `queued-runner` läuft als unprivilegierter User und startet Agenten in rootless Podman.
- **Notifications:** In-App (SSE), Web Push (PWA, auch iOS) und ntfy.

## Architektur

```
Browser / iPhone-PWA ──HTTPS──► Cloudflare Tunnel ──► queued  (User "queued", keine Host-Rechte, systemd-hardened)
                                                        │  Auth, Queues, Scheduler, Budgets, Notifications
                                                        ├── Postgres (rootless Podman-Container)
                                                        │
                                                        └─ HTTP + Bearer-Token (127.0.0.1) ─► queued-runner  (User "queued", kein Root)
                                                                                                ├─ Git-Workspaces  /var/lib/queued/runner
                                                                                                └─ podman run --rm --cap-drop ALL
                                                                                                     --security-opt no-new-privileges
                                                                                                     queued-agent (Claude/Codex/Gemini CLI)
```

| Komponente | Pfad | Verantwortung |
|---|---|---|
| `queued` | `backend/crates/app` | API, Auth, Scheduler (`FOR UPDATE SKIP LOCKED`), Budgets, Prompts, Fix-Loop, OnEmptyQueue |
| `queued-runner` | `backend/crates/runner` | Git (init/clone/commit/diff/revert/push), Sandbox-Exec mit NDJSON-Streaming, Cancel |
| `queued-common` | `backend/crates/common` | Wire-Protokoll zwischen beiden |
| `queued-api` | `backend/crates/api` | HTTP-/SSE-Typen, geteilt von Backend und Frontend (kompiliert auch nach WASM) |
| Frontend `queued-web` | `frontend/` | Rust/Leptos (CSR, WebAssembly, Build mit Trunk), PWA mit Web Push, Safe-Area-fähig (Notch/Dynamic Island), Tab-Leiste unten auf dem iPhone, hell/dunkel, WCAG 2.2 AA |
| Tests | `backend/**/tests`, `frontend/src/**` (`cargo test`), `e2e/` | siehe unten |

**Isolation in der Praxis verifiziert:** Ein Agent-Container läuft mit der UID des Runner-Users, `CapEff: 0`, `NoNewPrivs: 1`. Secrets kommen nur per Env in den Container, nie in argv. Der Runner verweigert den Start als Root und mit schwachem Token.

## Ablauf eines Auftrags

```
queued ─► running ─► git pull (falls Remote) ─► frischer Agent (Auftrag + Handoff/Kontext + Projektziel)
                       │ Ergebnis: structured output (Claude --json-schema, Codex --output-schema)
                       │           sonst .queue/result.json (git-ignored, Fallback, einziger Weg für Gemini)
                       ├─ needs_input ─► waiting_input ─(Antwort)─► queued
                       ├─ Abo-Limit           ─► Provider pausiert bis Reset, Auftrag zurück in die Queue
                       ├─ Anmeldung/Abrechnung ─► Provider pausiert bis neue Zugangsdaten / „Pause aufheben"
                       ├─ Provider gestört / Infrastruktur / Stillstand ─► zurück in die Queue mit Backoff
                       │                         (kein Fix-Versuch; nach TRANSIENT_MAX_RETRIES ─► blocked)
                       ├─ Limit (Laufzeit/Kosten/Tokens/Turns) bzw. Konfigurationsfehler ─► blocked, kein Fix
                       ▼
                  git commit  ─►  verify_command (z. B. `cargo test`)  ─►  Review-Agent (Auftrag + Diff)
                       ├─ ok   ─► (auto push) ─► done ─► Folgeaufträge (≤ 2 automatisch, > 2 → Freigabe + Push)
                       └─ fail ─► failed ─► Fix-Auftrag (Root-Cause-Analyse) vorne in der Queue
                                           nach 2 Fix-Versuchen ─► blocked + Push-Notification
Queue leer ─► OnEmptyQueue (Review / Recherche / Verbesserungen) ─► schlägt Aufträge vor
```

## Token-Effizienz & Budgets

| Maßnahme | Wirkung |
|---|---|
| Frischer Agent pro Auftrag, Kontext nur via **Handoff** (≤ 4 KB) oder explizitem Kontext | kein Kontext-Bloat |
| Statische Prompt-Teile identisch über alle Aufträge (getestet) | Provider-Prompt-Caching greift |
| Review sieht nur Auftrag + Diff (> 60 KB → `--stat`), optional günstigeres Modell | Review kostet einen Bruchteil |
| **Budgets** pro Provider: rollierende 5h/7d-Token- und 30d-USD-Limits | Queue startet nichts mehr, bis Verbrauch aus dem Fenster fällt |
| **Beobachtetes Limit:** Beim echten Abo-Limit wird der 5h-Verbrauch gespeichert, die UI schlägt 90 % davon als Budget vor | Kalibrierung ohne geratene Plan-Zahlen |
| OnEmptyQueue nur bei Leerlauf, ohne offene Rückfragen, mit freiem Budget und Cooldown | nutzt Restkontingent statt Budget zu verbrennen |
| Loop-Guards: Fix-Versuche, Freigabe ab > 2 Aufträgen bzw. Tiefe > 3 | keine Agent-Kaskaden |

Gezählt wird `input + output + cache_write`, Cache-Reads separat. Codex und Gemini melden Cache im Input, das wird herausgerechnet.

### Fehlerklassen und Limits pro Auftrag

Jeder abgebrochene Lauf bekommt eine Fehlerklasse (`tasks.failure_class`). Nur echte Task-Fehler (`task`: Ergebnis `failed`, Verify/Review fehlgeschlagen, fehlender Ergebnis-Contract bei Claude/Codex) erzeugen Fix-Aufträge. Erkannt wird über die strukturierten Signale der CLIs (Claude `system/api_retry`, `assistant.error`, `result.subtype`, `rate_limit_event.status`; Codex `turn.failed`/`error`; Gemini `result.error.type`, `error.severity`). Text-Heuristik nur noch auf dem Fehlerkanal der CLI, nie auf stderr oder Agent-Ausgaben.

Limits (Projekt-Standard unter Projekt → Einstellungen, pro Auftrag per API überschreibbar): max. Laufzeit und Stillstand (keine Ausgabe für N Sekunden) je Lauf, max. Kosten und Tokens kumulativ pro Auftrag, max. Turns je Lauf. Abbruch immer erst per SIGINT (Claude beendet den Turn und meldet noch den Verbrauch), nach `RUNNER_KILL_GRACE_SECS` SIGKILL.

| Provider (gepinnte CLI) | Ergebnis-Schema | Turn-Limit | Kostenlimit | Token-Verbrauch |
|---|---|---|---|---|
| Claude Code 2.1.319 | `--json-schema` | `--max-turns` | live mit Katalogpreisen, zusätzlich `--max-budget-usd` (nicht hinter Gateways) | live (`assistant.message.usage`), am Ende `modelUsage` |
| Codex CLI 0.167.0 | `--output-schema` | – | nach dem Lauf (Katalogpreise) | erst am Ende (`turn.completed`) → Überschreitung blockiert nach dem Lauf |
| Gemini CLI 0.67.0 (eingefroren) | – (nur Datei) | `model.maxSessionTurns` | nach dem Lauf | erst am Ende → Überschreitung blockiert nach dem Lauf |
| opencode 1.19.5 (nur über Zugänge) | – (nur Datei) | – | live mit Katalogpreisen | live je Schritt (`step_finish`) |

Die CLI-Versionen sind im Agent-Image gepinnt (`docker/agent.Dockerfile`, Build prüft die Versionen); die Parser haben Contract-Tests gegen Fixtures genau dieser Versionen (`backend/crates/app/tests/fixtures/cli/`, Herkunft in `PROVENANCE.md`).

## Anbieter, Kosten und Modellwahl

Die Queue rechnet Kosten selbst: **verbrauchte Tokens × versionierte Preistabelle** (Listenpreise, Stand 7.11.2026, jede Zeile mit Quelle und Belegstufe). Der Dollarwert einer CLI zählt nur noch als gekennzeichneter Ersatz, wenn für ein Modell kein Preis bekannt ist. Damit gelten Budgets, Kostenlimits und Auswertungen auch für Codex, opencode und Gateways, die selbst keine Kosten melden. Grundlage: `reports/Kostenoptimale Modellanbieter für Queue.md`.

### Preisübersicht (USD pro Million Tokens)

| Modell | Eingabe | Ausgabe | Cache lesen | Batch/Flex | Beleg |
|---|---|---|---|---|---|
| Claude Opus 5.5 | 4 | 20 | 0.20 (Schreiben 5 min 5, 1 h 8) | Batch −50 % | Anbieterseite gelesen |
| Claude Sonnet 5.5 | 2 | 10 | 0.10 (2.50 / 4) | Batch −50 % | Anbieterseite gelesen |
| Claude Haiku 5.5 (Prompt ≤ 100K) | 0.10 | 0.50 | 0.01 | Batch −50 % | Anbieterseite gelesen; > 100K ganzer Request 0.50/2.50 |
| GPT-6 Astra / 6.1 Sol / 6.1 Terra / 6 Luna | 10 / 2 / 1 / 0.10 | 50 / 10 / 5 / 0.50 | 1 / 0.10 / 0.05 / 0.01 | Batch und Flex −50 % | Suchauszug; > 272K Input ×2, Output ×1.5 |
| Gemini 4 Argon | 2 → **4 ab 1.12.2026** | 10 → 20 | 0.10 → 0.20 | Batch −50 % | Suchauszug |
| Gemini 3.8 Flash | 0.75 → **1.50 ab 1.1.2027** | 3.75 → 7.50 | 0.075 (Annahme) → 0.15 | Batch −50 % | Suchauszug |
| Mistral Large 4 / Small 4 | 0.70 / 0.15 | 2.10 / 0.60 | 0.07 / 0.015 | Batch −50 % | Suchauszug (Large 4 bis 28.10.2026: 1.36/4.18) |
| Infomaniak Gemma 4 31B / Apertus 70B | CHF 0.20 / 0.70 | CHF 0.40 / 2.50 | – | – | Anbieterseite (in den Notes ohne Markierung) |
| DeepSeek V4 Flash (China) | 0.14 | 0.28 | – | – | Sekundärquelle |

Aufschläge: Bedrock/Vertex EU +10 % (belegt; gleicher Basispreis wie die Anthropic-API **nicht verifiziert**), Mistral-EU-Endpunkt +10 %, Azure EU Data Zone +20 %, Switzerland North +30 %, OpenRouter-Gebühr +5.5 % (sekundär). Die vollständige Tabelle steht unter Einstellungen → Preistabelle; Admins können Preise überschreiben (jede Änderung ist eine neue Version, `GET/PUT /api/admin/prices`). CHF-Preise zählen nicht in USD-Budgets und -Limits (kein belegter Wechselkurs), sie werden getrennt ausgewiesen.

Mit den Token-Profilen des Reports (Annahme: mittlerer Auftrag = 40 Turns) kostet **ein Versuch** im Agenten: Opus 5.5 2.49 $, Sonnet 5.5 1.25 $, GPT-6.1 Terra 0.62 $ (Flex 0.31 $), Mistral Large 4 0.41 $, Haiku 5.5 0.08 $. Die Erfolgsquoten der günstigen Modelle sind unbekannt – dafür gibt es den Modell-Test (unten).

### Zugänge (Einstellungen → Weitere Zugänge)

Zusätzlich zu den bisherigen Abo-/API-Zugängen pro Anbieter lassen sich beliebig viele **Zugänge** anlegen, für alle eigenen Aufträge oder nur für ein Projekt (dann dürfen alle Bearbeiter des Projekts sie nutzen; der Agent sieht den Schlüssel im Container). Schlüssel werden mit AES-256-GCM verschlüsselt, nie zurückgegeben, nie in Kommandozeilen oder Logs geschrieben.

| Zugang | Agent | Datenstandort | Planung/Review direkt | Batch | Nicht verifiziert |
|---|---|---|---|---|---|
| Claude-Abo | Claude Code | USA | – | – | Nutzungsbedingungen |
| Anthropic API | Claude Code | USA | ja | Message Batches | Batch-Format |
| AWS Bedrock (EU-Geo-Profil) | Claude Code | EU (wählbar) | – | – | Variablen im Binary vorhanden, Verhalten nicht live geprüft |
| Google Vertex (EU-Multi-Region) | Claude Code | EU / Zürich (Verfügbarkeit offen) | – | – | dito; Schlüsselablage als ADC-Datei |
| Eigenes Anthropic-Gateway | Claude Code | selbst angegeben | ja | – | – |
| ChatGPT-Abo | Codex | USA | – | – | Nutzungsbedingungen |
| OpenAI API (+ Flex) | Codex | USA | ja | OpenAI Batch | Flex-Wirkung in Codex, Batch-Format |
| Azure OpenAI (EU Data Zone / Switzerland North) | Codex | EU / CH | ja | – | Pfad `/openai/v1`, Header `api-key` |
| Google-Abo | Gemini CLI | USA | – | – | Abo-Pfad laut Sekundärquellen gestört |
| Gemini API | opencode | weltweit | – | – | – |
| Mistral (EU-Endpunkt) | opencode | EU | ja | Mistral Batch | Modellabdeckung des EU-Endpunkts, Batch-Format |
| Infomaniak | opencode | Schweiz | ja | – | Basis-URL-Schema, Agent-Tauglichkeit der Modelle |
| Eigener Server (vLLM/Ollama, OpenAI-kompatibel) | opencode | selbst angegeben | ja | – | Preis nur per Admin-Eintrag |
| OpenRouter (global/EU) | opencode | weltweit / EU | ja | – | EU nur im Business-Plan, Kontodaten ggf. USA |
| LiteLLM-Gateway | Claude Code, Codex oder opencode | selbst angegeben | ja | – | Admin-API; **Budget-Schlüssel pro Auftrag** (virtueller Key mit `max_budget` = Rest des Kostenlimits, danach gelöscht) |

**Gemini CLI:** gepinnt auf 0.67.0 und eingefroren (keine Weiterentwicklung). Das npm-Paket wird weiter veröffentlicht (0.67.0 vom 4.11.2026, nicht als veraltet markiert); die Ablösung durch Antigravity und das Ende des Abo-Pfads stammen nur aus Sekundärquellen. Einstellungen → Agenten-Programme prüft live in der Sandbox, welche CLIs (inkl. `agy`) installiert sind (`GET /api/cli-status`). Für Gemini per API-Schlüssel ist der Zugang „Gemini API (über opencode)“ gedacht.

### Modellwahl pro Schritt und Datenstandort (Projekt → Einstellungen)

Jedes Projekt kann festlegen, wer **plant** (direkt oder Batch), **umsetzt** (Agent, mit Eskalationskette für Reparaturversuche, optional OpenAI Flex), **prüft** (Agent, direkt oder Batch) und bei leerer Warteschlange **analysiert**. Vorlagen werden gegen die vorhandenen Zugänge aufgelöst; fehlt einer, sagt die Oberfläche welcher. Geschätzte Kosten pro Versuch (mittlerer Auftrag, Report-Profil, eigene Rechnung):

| Vorlage | Ablauf | ca. pro Versuch |
|---|---|---|
| Günstig | Opus plant (Batch), Haiku setzt um, bei Fehlschlag Sonnet, dann Opus; Sonnet prüft | 0.31 $ (erster Versuch) |
| Ausgewogen | Opus plant (Batch), Sonnet setzt um und prüft | 1.49 $ |
| Beste Qualität | Opus für alles | 2.83 $ |
| Nur EU/CH | Mistral Large 4 über den EU-Endpunkt (plant per Batch, setzt um, prüft) – oder Sonnet über Bedrock/Vertex EU | 0.51 $ bzw. 1.48 $ |
| Maximal günstig | Sol plant (Batch) und prüft, Terra setzt im Flex-Tarif um | 0.43 $ |

Das Review bleibt in allen Vorlagen agentisch: Der Report empfiehlt das Batch-Review nur als Opt-in neben dem agentischen Review (weniger stabile Trefferquote, erst per A/B messen). Per API (`PUT /api/projects/{id}/routing`, Schritt `review` mit `mode: "batch"`) lässt es sich einschalten.

Serverseitig erzwungen: der **Datenstandort** des Projekts (`Keine Einschränkung` / `Nur EU/Schweiz` / `Nur Schweiz`) für jeden Schritt – beim Anlegen und Ändern eines Auftrags, beim Speichern der Modellwahl und nochmals vor jedem Lauf (ein Zugang, der inzwischen woanders steht, blockiert den Auftrag als Konfigurationsfehler); Lockern der Regel nur durch die Eigentümerin bzw. den Eigentümer. Das **Review** läuft nie auf der günstigsten Modellstufe (Haiku, Luna, Small, …).

**Batch:** Planung, Review und Analyse sind Einzelschritte. Im Batch-Modus wartet der Auftrag im Zustand „Beim Anbieter“ (bis zu 24 h, −50 %), belegt dabei keinen Agenten-Platz und sperrt das Projekt nicht; ein eigener Poller fragt den Anbieter ab (`BATCH_POLL_SECS`), Jobs überstehen Neustarts. Der stabile Teil des Prompts (Projektziel, Dateiliste – keine Auftrags-IDs) steht vorne und wird mit 1-h-Cache markiert. Der Agent selbst läuft weiterhin synchron mit Prompt-Caching. Die App ruft dafür Anbieter-APIs direkt auf; interne Adressen sind gesperrt (`ALLOW_PRIVATE_LLM_ENDPOINTS` für eigene Server im LAN).

**Kostenschätzung:** Der Composer zeigt vor dem Einreihen die geschätzten Kosten je Schritt (eigene Historie des Modells, sonst Report-Profil S/M/L als Annahme) und den Datenstandort. Die Details eines Auftrags zeigen die tatsächlichen Kosten je Schritt, Projekt und Einstellungen die Summen der letzten 30 Tage (`GET /api/costs`).

**Modell-Test (Eval-Set):** 8 kleine Programmieraufgaben (Fehler, Funktion, Doku, Tests, Umbau, Sicherheit; Größe S/M/L) mit Setup- und Prüfskript (`backend/crates/app/evals/cases.json`). Ein Lauf legt versteckte Projekte an, ohne Review und Reparaturen, und misst Erfolgsquote, Kosten und Kosten pro gelöster Aufgabe (`/api/evals`). Mit dem Test-Anbieter sind die Ergebnisse reproduzierbar (`mock-weak`/`mock-medium`/`mock-strong` lösen S / S+M / alle).

## Tests

| Ebene | Was | Ausführen |
|---|---|---|
| Unit (Rust) | Git-Ops inkl. Clone/Push/Pull, atomarer Revert, Path-Traversal, Exec (Streaming, Timeout, Cancel, Prozessgruppe), Container-Flags, Parser aller Provider, Limit-Erkennung, Budget-Logik, Prompts, Crypto, OIDC/PKCE/Apple-JWT | `cd backend && cargo test --workspace --lib` |
| Integration (Rust) | App + Runner in-process + echtes Postgres (frische DB pro Test): Auth, Tenant-Isolation, Rollen, kompletter Task-Lifecycle mit Git, Fix-Loop, Rückfragen, Freigaben, Abo-Limit, Budgets, Cancel, Revert, Credentials, OnEmptyQueue, Clone + Auto-Push, Parallelität, SSE-Isolation, ZIP | `DATABASE_URL=postgres://… cargo test --workspace` |
| Integration WP13 | Zugänge (Verschlüsselung, Zugriff), Kosten je Schritt aus der Preistabelle, Live-Kostenlimit, Datenstandort, Vorlagen, Planung/Review/Analyse per Batch (Anthropic, OpenAI, Mistral) und direkt gegen Fake-Server, Eskalation, Abbruch/Neustart, Schätzung, Kostenübersicht, Admin-Preise, LiteLLM-Schlüssel, CLI-Status, Eval-Set | `cargo test -p queued --test providers` |
| Unit (Frontend) | Formatierung, Status-/Fehlertexte, Budget-Anzeige, Limit-Eingaben, Fortschrittsphasen, Diff, Tastaturkürzel, VAPID-Decoding, Geldbeträge/Schätzung | `cd frontend && cargo test` |
| System | echte Binaries als getrennte Prozesse: Runner-Auth, Session-Pflicht, Flow über die Prozessgrenze, Budget, Cancel | `cd e2e && npx playwright test --project=system` |
| E2E | Browser gegen echten Stack: Registrierung, Onboarding, Einreihen mit Limits, Live-Status, Details/Diff/Rückgängig/Bearbeiten, Rückfrage, Freigabe, Fix-Kette, Budget-UI, Reorder/Verwerfen/Wiederherstellen, Bestätigungsdialoge, Tastaturkürzel, Live-Region, Theme | `npx playwright test --project=desktop` |
| E2E Mobile | iPhone 15 Pro hoch/quer mit simulierter **Dynamic Island**/Notch/Home-Indikator: nichts verdeckt, Tab-Bar und alle Bedienelemente ≥ 44pt, Inputs ≥ 16px (kein iOS-Zoom), kein horizontales Scrollen | `npx playwright test --project=iphone --project=iphone-landscape` |
| Accessibility | axe-core (WCAG 2.0/2.1/2.2 A+AA) auf allen Hauptansichten, hell und dunkel, Desktop und iPhone; Screenshots nach `WP00_SCREENSHOTS_DIR` | `a11y.spec.ts`, `mobile-a11y.spec.ts` (Teil der Projekte oben) |

Der Mock-Provider (`ENABLE_MOCK_PROVIDER=true`) macht alle Flows deterministisch und kostet keine Tokens. Er wird über Schlüsselwörter im Prompt gesteuert: `MOCK_FAIL`, `MOCK_ASK`, `MOCK_FOLLOWUPS`, `MOCK_LIMIT`, `MOCK_SLOW`, `MOCK_TRANSIENT` (scheitert transient, bis die Re-Queue-Notiz im Prompt steht), `MOCK_TRANSIENT_ALWAYS`, `MOCK_AUTH`, `MOCK_STRUCTURED`, `MOCK_STRUCTURED_BAD`, `MOCK_TOKENS`, `MOCK_TURNS`, `MOCK_TRAP` (wartet auf SIGINT und meldet dann Verbrauch), `MOCK_STDERR429`, `MOCK_REVIEW_LIMIT`, `MOCK_QUIET` (6 s ohne Ausgabe, dann Erfolg), `MOCK_USAGE_TOTAL` (Gesamtverbrauch erst am Ende, weit über den Live-Werten), `MOCK_SOLUTION_B64:<base64>` (führt das Skript im Workspace aus – Eval-Set). Für Planung/Review/Batch gibt es in den Integrationstests einen lokalen Fake-Server für die Anthropic-, OpenAI-, Mistral- und LiteLLM-APIs (`tests/common/fake_llm.rs`); echte Anbieter werden in Tests nie aufgerufen. CI (`.github/workflows/ci.yml`) führt fmt, clippy `-D warnings` (Backend sowie Frontend für wasm32 und nativ), alle Rust-Tests, den Trunk-Build sowie Playwright aus.

## Betrieb (Hetzner/Mac mini/Linux-Box, ohne Root zur Laufzeit)

```bash
sudo ./deploy/install.sh        # einmalig: Pakete, User "queued", subuid, linger, Build, Agent-Image, systemd-Units
sudoedit /etc/queued/queued.env /etc/queued/runner.env   # gleicher RUNNER_TOKEN (openssl rand -hex 32), MASTER_KEY
sudo -iu queued POSTGRES_PASSWORD=… podman-compose -f /opt/queued/deploy/compose.yml up -d   # + --profile tunnel
sudo systemctl enable --now queued-runner queued
```

Als IdP-Callback trägst du `${PUBLIC_URL}/api/auth/oidc/<id>/callback` ein. Bei Apple erfolgt der Callback als `form_post`, die Domain muss verifiziert sein.

### Provider-Zugänge (pro User, unter Einstellungen)

| Provider | Abo | API |
|---|---|---|
| Claude Code | `claude setup-token` → Token | `ANTHROPIC_API_KEY` |
| Codex CLI (GPT) | `codex login` → Inhalt von `~/.codex/auth.json` | OpenAI API-Key |
| Gemini CLI (eingefroren) | Google-Login → Inhalt von `~/.gemini/oauth_creds.json` | Gemini API-Key |

Weitere Zugänge (Bedrock, Vertex, Azure, Mistral, Infomaniak, eigene Server, OpenRouter, LiteLLM, Gemini über opencode) siehe „Anbieter, Kosten und Modellwahl“. Die Zugänge werden mit AES-256-GCM verschlüsselt (`MASTER_KEY`). Rotierte OAuth-Tokens schreibt das System automatisch zurück. Git-Tokens werden ebenfalls verschlüsselt gespeichert und nur als HTTP-Header an Git übergeben, nie in Remote-URL oder Config.

### Entwicklung

```bash
cd backend
RUNNER_ALLOW_ROOT=1 RUNNER_ENGINE=none RUNNER_TOKEN=dev-token-dev-token-dev-token cargo run --bin queued-runner &
DATABASE_URL=postgres://… RUNNER_TOKEN=dev-token-dev-token-dev-token ENABLE_MOCK_PROVIDER=true cargo run --bin queued
# Frontend: einmalig `rustup target add wasm32-unknown-unknown` und
# `cargo install --locked trunk@0.22.1 wasm-bindgen-cli@0.2.130` (Version = wasm-bindgen in frontend/Cargo.toml)
cd frontend && trunk serve                      # http://localhost:5173, /api wird an :8080 weitergereicht
```

## Bekannte Grenzen

- Innerhalb eines Projekts läuft immer nur ein Agent (gemeinsamer Workspace). Die Parallelität gilt projektübergreifend.
- Das verbleibende Abo-Kontingent liefern die CLIs nicht exakt. Darum gibt es die Kombination aus eigenen Budgets, Erkennung echter Limits und beobachteten Werten.
- Der Review-Agent ist per Prompt read-only, nicht technisch erzwungen. Seine Änderungen würden als WIP-Commit sichtbar.
- Codex und Gemini kennen kein Kostenlimit, Codex kein Turn-Limit; ihr Token-Verbrauch kommt erst am Ende eines Laufs. Ein Lauf kann ein Kosten-/Token-Limit daher überschreiten, bevor es greift. Der Auftrag wird dann trotzdem `blocked` (wie beim Live-Abbruch): kein „erledigt“, kein Review, kein Fix, keine Folgeaufträge. Die Änderungen bleiben als Teil-Commit im Diff des Auftrags; „Wiederholen“ startet mit frischem Budget. Gleiches gilt, wenn Claudes Live-Zähler zu wenig meldet.
- Verbrauchswerte aus dem CLI-Stream werden begrenzt (negativ → 0, je Meldung höchstens 10^10 Tokens bzw. 10 000 USD); gemeldete Limit-Resets höchstens 8 Tage in der Zukunft.
- Ein Provider-Fehler im Review-Lauf pausiert den Review-Provider, der Review wird aber übersprungen statt wiederholt (kein Phasen-Retry).
- `GEMINI_CLI_TRUST_WORKSPACE=true` (sonst führt Gemini headless keine Tools aus) lässt auch Gemini-Einstellungen aus dem Repo wirken; Härtung gegen nicht vertrauenswürdige Repo-Konfiguration folgt in WP03.
- Kleine Turn-Limits vertragen sich schlecht mit dem Ergebnis-Schema: Claude braucht für den structured output selbst zusätzliche Turns (beobachtet: „say hi" = 3 Turns).
- Live gegen echte Provider aufgezeichnet sind nur Teile der Fixtures (Claude: Erfolg, Turn-/Kostenlimit, SIGINT; Codex: Netzwerk-Retries; Gemini: ungültiger API-Key). Rate-Limit-, Überlastungs- und Codex-Auth-Streams sind aus den Schemas der gepinnten Versionen konstruiert.
- Kosten sind Listenpreise aus Recherche-Quellen: Nur die Anthropic-Preise wurden direkt auf der Anbieterseite gelesen, OpenAI/Gemini/Mistral stammen aus Suchauszügen. Long-Context-Aufschläge lassen sich nur prüfen, wo die CLI den Verbrauch pro Anfrage meldet (Claude, opencode); sonst ist der Betrag eine Untergrenze. Abo-Läufe werden zu API-Preisen bewertet.
- Die Batch-, Azure-, Infomaniak- und LiteLLM-Schnittstellen sowie Bedrock/Vertex/opencode im Echtbetrieb sind nur gegen lokale Fake-Server bzw. Strings im Binary geprüft, nicht gegen die echten Dienste.
- opencode liest auch eine `opencode.json` im Repository (wie Gemini den Workspace vertraut) – Härtung folgt mit WP03.

