# Queue – agentische Coding-Queue

Aufträge per Web-UI (Desktop oder iPhone als PWA) einreihen. Pro Auftrag arbeitet ein **frischer Agent** (Claude Code, Codex/GPT oder Gemini CLI) ihn in einer **rootless Sandbox** ab. Danach wird verifiziert, reviewt und **in Git committet**, und die Queue geht zum nächsten Auftrag. Ist sie leer, laufen **OnEmptyQueue**-Aktionen (Review, Web-Recherche, Verbesserungen), die die Queue wieder befüllen können.

- **Multi-User/Multi-Tenant:** Jeder User hat eine Queue und sein eigenes Abo. Mehrere Projekte pro User sind möglich, Projekte lassen sich teilen (Owner/Editor/Viewer).
- **Login:** lokale Accounts (Argon2) oder OIDC/OAuth2 (Google, Apple, GitHub, Keycloak, Authentik, Entra …).
- **Jedes Projekt ist ein Git-Repo:** neu angelegt oder von einem Remote geklont. Jeder Auftrag arbeitet auf einem **eigenen Branch** (`queue/<id>`) in einem eigenen Arbeitsverzeichnis; fertige, geprüfte Aufträge kommen über eine **Übernahme-Warteschlange** (Merge-Queue) als ein Commit in den Hauptstand (Autor = User). Rückgängig per `git revert`, optional Auto-Push.
- **Parallel im selben Projekt:** pro Projekt einstellbar, wie viele Aufträge gleichzeitig laufen (Standard 1). Konflikte zwischen parallelen Aufträgen löst ein automatischer Korrektur-Auftrag.
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
                                                                                                ├─ podman run --rm --cap-drop ALL --network none
                                                                                                │    --security-opt no-new-privileges
                                                                                                │    queued-agent (Claude/Codex/Gemini CLI)
                                                                                                │      └─ 127.0.0.1:3128 ─(Unix-Socket, ro gemountet)─┐
                                                                                                └─ Egress-Proxy pro Lauf (Allowlist, Protokoll) ◄──────┘ ─► Internet
```

| Komponente | Pfad | Verantwortung |
|---|---|---|
| `queued` | `backend/crates/app` | API, Auth (Cookie + API-Tokens), Scheduler (`FOR UPDATE SKIP LOCKED`), Zeitpläne und Webhooks, Budgets, Prompts, Fix-Loop, OnEmptyQueue |
| `queued-runner` | `backend/crates/runner` | Git (init/clone/commit/diff/revert/push, Arbeitsverzeichnis pro Auftrag, Merge-Queue-Schritte), Sandbox-Exec mit NDJSON-Streaming, Cancel |
| `queued-common` | `backend/crates/common` | Wire-Protokoll zwischen beiden |
| `queued-api` | `backend/crates/api` | HTTP-/SSE-Typen, geteilt von Backend und Frontend (kompiliert auch nach WASM); auch Token-Format, Berechtigungen und Cron-Dialekt (WP07) |
| `queuectl` | `backend/crates/queuectl` | Kommandozeilen-Client mit API-Token (WP07, siehe „Automatisierung“) |
| Frontend `queued-web` | `frontend/` | Rust/Leptos (CSR, WebAssembly, Build mit Trunk), PWA mit Web Push, Safe-Area-fähig (Notch/Dynamic Island), Tab-Leiste unten auf dem iPhone, hell/dunkel, WCAG 2.2 AA |
| Tests | `backend/**/tests`, `frontend/src/**` (`cargo test`), `e2e/` | siehe unten |

**Isolation in der Praxis verifiziert:** Ein Agent-Container läuft mit der UID des Runner-Users, `CapEff: 0`, `NoNewPrivs: 1`. Secrets kommen nur per Env in den Container, nie in argv. Der Runner verweigert den Start als Root und mit schwachem Token. Seit WP02 hat jeder Container **kein eigenes Netz** (`--network none`): Der einzige Weg nach außen ist der Egress-Proxy des Runners (siehe „Umgebung, Setup und Netzwerk“). Mit Docker 29 hier geprüft: im Container nur `lo`, direkte Verbindungen und DNS schlagen fehl, erlaubte Ziele gehen über den Proxy, abgelehnte erscheinen im Protokoll.

## Ablauf eines Auftrags

```
queued ─► running ─► [Plan-Modus: Plan-Lauf nur lesend in Wegwerf-Kopie ─► held (Plan) ─ genehmigen/ändern ─► queued | verwerfen ─► cancelled]
                   ─► git pull (falls Remote) ─► [Setup-Dateien geändert? ─► held] ─► Setup-Skript (falls vorhanden; mit Geheimnissen, Setup-Netz, Cache)
                       │  Fehler / Zeitlimit / Geheimnis in einer Datei ─► blocked (Konfiguration), kein Agent, kein Fix
                       ▼
                     frischer Agent (Auftrag + Handoff/Kontext + Projektziel; ohne Geheimnisse, Agent-Netz)
                       │ Nachrichten des Nutzers? ─► gleiche Sitzung fortsetzen (Resume), dann erst weiter
                       │ Ergebnis: structured output (Claude --json-schema, Codex --output-schema)
                       │           sonst .queue/result.json (git-ignored, Fallback, einziger Weg für Gemini)
                       ├─ needs_input ─► waiting_input ─(Antwort)─► queued
                       ├─ Abo-Limit           ─► Provider pausiert bis Reset, Auftrag zurück in die Queue
                       ├─ Anmeldung/Abrechnung ─► Provider pausiert bis neue Zugangsdaten / „Pause aufheben"
                       ├─ Provider gestört / Infrastruktur / Stillstand ─► zurück in die Queue mit Backoff
                       │                         (kein Fix-Versuch; nach TRANSIENT_MAX_RETRIES ─► blocked)
                       ├─ Limit (Laufzeit/Kosten/Tokens/Turns) bzw. Konfigurationsfehler ─► blocked, kein Fix
                       ▼
                  Schutzprüfung (WP03): Geheimnisse im Diff/Protokoll, geschützte Dateien, geschwächte Tests
                       ├─ Fund ─► held („Freigabe nötig“), Änderungen außerhalb des Branches geparkt
                       │          ├─ Freigeben ─► weiter mit Commit   └─ Verwerfen ─► blocked (Freigabe abgelehnt)
                       ▼
                  git commit (auf queue/<id>)  ─►  verify_command (z. B. `cargo test`)  ─►  Review-Agent (Auftrag + Diff + Rubrik)
                       ├─ Review nicht möglich ─► nur das Review wird wiederholt, danach held (nie ungeprüft done)
                       ├─ ok   ─► merging („Wird übernommen“): Übernahme-Warteschlange des Projekts, einer nach dem anderen
                       │            ├─ Branch auf den aktuellen Hauptstand gebracht (ein Commit, 3-Wege) ─► Nachprüfung
                       │            ├─ Hauptstand hat sich bewegt ─► verify_command erneut
                       │            ├─ Konflikt / Prüfung danach fehlgeschlagen ─► failed ─► Korrektur-Auftrag mit den Änderungen
                       │            └─ Fast-Forward des Hauptstands ─► (auto push; Standard-Branch nur nach Freigabe) ─► done ─► Folgeaufträge
                       └─ fail ─► failed ─► Fix-Auftrag (Root-Cause-Analyse, setzt die Änderungen fort) vorne in der Queue
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

## Umgebung, Setup und Netzwerk (Projekt → Umgebung)

Jedes Projekt hat eine Umgebung. Ändern darf sie nur die Eigentümerin bzw. der Eigentümer; alle Mitglieder sehen sie.

| Teil | Wirkung |
|---|---|
| **Setup-Skript** | Läuft vor jedem Auftrag als eigener Container im Projektordner (`sh -e`, bricht beim ersten Fehler ab, Zeitlimit 10–3600 s). Typisch: `npm ci`, `pip install -r requirements.txt`. Fehlschlag ⇒ Auftrag `blocked` (Klasse Konfiguration), der Agent startet nicht, es entsteht kein Fix-Auftrag. |
| **Umgebungsvariablen** | Nicht geheim. Gelten für Setup, Agent, Prüfbefehl und Review. Namen, die die Queue selbst setzt (Proxy, Zertifikate, `HOME`, `PATH`, `QUEUE_*`, `LD_*` …), sind gesperrt, ebenso alles, was einen KI-Zugang oder eine Agent-CLI umlenkt (`ANTHROPIC_*`, `OPENAI_*`, `CODEX_*`, `CLAUDE_*`, `GEMINI_*`, `GOOGLE_*`, `AWS_*`, `XDG_*` …): Aufträge laufen mit den Zugängen der Person, die sie anlegt – sonst könnte die Eigentümerin den Schlüssel eines Bearbeiters an einen eigenen Server umleiten. |
| **Geheimnisse** | AES-256-GCM verschlüsselt (`MASTER_KEY`), nie wieder angezeigt, **nur im Setup** als Umgebungsvariable. Ausgaben des Setups werden geschwärzt (`***`). Nach dem Setup durchsucht der Runner Workspace und Cache nach den Werten; ein Fund blockiert den Auftrag, und die Datei wird bereinigt (nicht versioniert: gelöscht; versioniert: auf den Repo-Stand zurückgesetzt; unter `.git/`: Zeile entfernt). Mindestlänge 8 Zeichen. |
| **Netz im Setup** | `Kein Netz` · `Erlaubte Adressen` (Vorgabe: npm, PyPI, crates.io, Go, GitHub) · `Offen`. |
| **Netz für den Agenten** | `Nur Modell-API` (**Vorgabe**) · `Modell-API + erlaubte Adressen` · `Offen`. Die Schnittstelle des KI-Modells (z. B. `api.anthropic.com`, die Basis-URL eines Zugangs, Bedrock-/Vertex-Region) ist immer erreichbar. Der Prüfbefehl bekommt dieselbe Regel ohne Modell-API. |
| **Nur lesen** | Pro Phase: nur GET/HEAD/OPTIONS ohne Body zu den freigegebenen Adressen. Für HTTPS bricht der Proxy TLS mit einer **pro Lauf erzeugten CA** auf (`NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE` u. a. zeigen darauf) und prüft upstream gegen die Mozilla-Wurzeln. Gilt nie für die Modell-API. |
| **Cache** | Pro Projekt und Schlüssel unter `RUNNER_DATA_DIR/caches/`, im Container `/cache` (`npm_config_cache`, `PIP_CACHE_DIR`, `CARGO_HOME`, `GOMODCACHE` …). Schlüssel = Setup-Skript + Variablen + Setup-Netz + Image + Inhalt der Lock-Dateien (`package-lock.json`, `Cargo.lock`, `uv.lock`, `go.sum` … plus eigene Pfade). **Nur das Setup** sieht den Cache; der Agent kann ihn nicht vergiften. „Cache leeren“ dürfen auch Bearbeiter. |

**Durchsetzung (ohne Root):** Container laufen mit `--network none`. Der Runner startet pro Lauf einen Proxy auf einem Unix-Socket und mountet dessen Verzeichnis read-only nach `/queue-egress`. Eine kleine Node-Brücke im Container (vom Runner geschrieben, nicht im Image) bietet ihn als `http://127.0.0.1:3128` an; `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY` zeigen darauf. Der Proxy prüft den **Hostnamen vor jeder DNS-Abfrage**, löst dann selbst auf und verbindet nur zu geprüften Adressen. Link-Local/Metadaten (`169.254.169.254`), Loopback, private Netze, CGNAT, Multicast und die **eigenen Adressen des Hosts** sind immer gesperrt. Jede Ablehnung wird als Ereignis „Netzwerk“ im Auftrag protokolliert, erlaubte Ziele als Zusammenfassung pro Phase.

**Engine `none` (Entwicklung) setzt nichts durch.** Dort läuft der Proxy auf `127.0.0.1` und nur Programme, die die Proxy-Variablen beachten, halten sich an die Regeln. UI (Banner im Reiter „Umgebung“), `/api/meta` (`sandbox.egress_enforced=false`) und jedes Protokoll („nicht durchgesetzt“) sagen das ausdrücklich.

Runner-Variablen dazu (`/etc/queued/runner.env`):

| Variable | Bedeutung |
|---|---|
| `RUNNER_EGRESS_PRIVATE_HOSTS` | Kommagetrennte Hosts, die auf interne Adressen zeigen dürfen (LAN-Registry, eigener Modell-Server wie `gpu.lan` oder `10.0.0.5`). Ohne Port = alle Ports. Link-Local/Metadaten bleiben gesperrt. **Nötig für opencode-/Codex-Zugänge mit Basis-URL im LAN.** |
| `RUNNER_EGRESS_DENY` | Zusätzlich gesperrte Netze (CIDR), gelten auch für die Hosts oben. |
| `RUNNER_EGRESS_EXTRA_CA` | PEM-Datei mit weiteren Wurzelzertifikaten für „Nur lesen“ (z. B. interne CA). |

**Upgrade-Hinweis WP02:** `RUNNER_NETWORK` gibt es nicht mehr; ist die Variable gesetzt, startet der Runner nicht (bitte entfernen). Agenten haben ohne weitere Einstellung **kein Internet mehr außer ihrer Modell-API** – wer bisher im Agentenlauf Pakete nachinstalliert hat, verschiebt das ins Setup-Skript oder gibt die Paketquellen im Agent-Netz frei. `RUNNER_DATA_DIR` darf höchstens 80 Bytes lang sein (Pfadgrenze für Unix-Sockets).

## Branch pro Auftrag, Parallelität und Übernahme (Projekt → Einstellungen / Verlauf)

- **Eigenes Arbeitsverzeichnis pro Auftrag.** Der Runner legt für jeden Auftrag ein eigenes Git-Repository (`<RUNNER_DATA_DIR>/workspaces/t<auftrag>`) auf dem Branch `queue/<12 Zeichen der Auftrags-ID>` an. Nur dieses Verzeichnis wird in die Container des Auftrags gemountet (Setup, Agent, Verify, Review). Das Projekt-Repository (`workspaces/<projekt>`, Hauptstand) sieht kein Agent mehr.
- **Bewusst kein `git worktree`:** Ein verknüpfter Worktree teilt Objekte, Refs, Hooks und Config mit dem Haupt-Repository. Ein Agent hätte damit entweder kein funktionierendes Git (Haupt-`.git` nicht gemountet) oder könnte Branches anderer Aufträge, zurückgehaltene Änderungen und Freigabe-Stände umschreiben. Das Arbeitsverzeichnis wird stattdessen per `git fetch` befüllt (nur die Historie des Hauptstands, keine Hardlinks) und hat keinen Rückweg ins Projekt-Repository. Kosten: Die Historie liegt pro laufendem Auftrag einmal auf der Platte; Arbeitsverzeichnisse beendeter Aufträge löscht Queue sofort.
- **Gleichzeitige Aufträge** (Projekt → Einstellungen → „Gleichzeitig arbeiten“, nur Eigentümer, 1–8, Standard 1): zählt laufende Aufträge und solche in der Übernahme-Warteschlange. Mit 1 startet ein Auftrag erst, wenn sein Vorgänger übernommen ist – wie vor WP04, ohne Konflikte. Die Parallelität pro Nutzer (Einstellungen) und Anbieter-Pausen gelten weiter. Setups mit Abhängigkeits-Cache laufen pro Projekt nacheinander (gemeinsamer Cache).
- **Übernahme-Warteschlange:** Fertige, geprüfte Aufträge (Status „Wird übernommen“) werden pro Projekt einer nach dem anderen übernommen: Die Änderungen des Branches werden als **ein Commit** per 3-Wege-Merge auf den aktuellen Hauptstand gesetzt (`git merge-tree`, ohne Arbeitsbaum und ohne Hooks), die Schutzregeln prüfen das Ergebnis erneut (fail-closed), der Prüfbefehl läuft erneut, wenn sich der Hauptstand inzwischen geändert hat, dann folgt ein Fast-Forward. Erst danach kommt der (geschützte) Auto-Push. Die Warteschlange steht unter Projekt → Verlauf.
- **Konflikte:** Haben inzwischen übernommene Aufträge dieselben Stellen geändert, wird der Auftrag „Fehlgeschlagen (Konflikt)“ und Queue legt einen Korrektur-Auftrag an. Dessen Arbeitsverzeichnis startet auf dem aktuellen Hauptstand mit den Änderungen des fehlgeschlagenen Auftrags; in den betroffenen Dateien stehen Konfliktmarker, die der Agent auflöst. Bleiben Marker übrig, wird nicht übernommen. Gleiches (ohne Marker) gilt, wenn der Prüfbefehl nach dem Aktualisieren scheitert und für jeden anderen Fix-Auftrag: Fix-Aufträge setzen die nicht übernommenen Änderungen fort.
- **Diff und Rückgängig:** Der Diff zeigt immer alles, was der Auftrag in den Hauptstand bringt (gegen die Merge-Base), vor und nach der Übernahme. „Rückgängig“ gibt es nur für übernommene Aufträge (Revert des einen Commits im Hauptstand) und geht jetzt auch, während andere Aufträge laufen.
- **Aufräumen:** Arbeitsverzeichnisse beendeter Aufträge werden sofort gelöscht; Branches übernommener, abgebrochener und erledigter Aufträge ebenfalls, Branches fehlgeschlagener/blockierter Aufträge nach 7 Tagen (solange kein Korrektur-Auftrag sie noch braucht). „Abbrechen“ verwirft Branch und Arbeitsverzeichnis.
- **Sicherheit:** Zurückgehaltene Änderungen (WP03) und Teil-Commits unterbrochener Läufe liegen nur noch im Arbeitsverzeichnis bzw. auf dem Branch des Auftrags, nie auf dem Hauptstand und nie für andere Aufträge lesbar. Die Freigabe-Stände für Setup-Dateien und Agent-Konfiguration liegen nur im Projekt-Repository und lassen sich von Agenten nicht mehr fälschen. Objekte aus einem Arbeitsverzeichnis übernimmt der Runner nur per exakter Commit-ID mit `transfer.fsckObjects`, nachdem dessen `.git/config` neu geschrieben wurde. Bei Engine `none` (Entwicklung) läuft der Agent weiterhin ungeschützt auf dem Host.

**Upgrade-Hinweis WP04:** Fehlgeschlagene Aufträge landen nicht mehr im Hauptstand (vorher blieb ihr Commit dort und der Fix arbeitete obendrauf); der Fix-Auftrag übernimmt die Änderungen. Teil-Commits unterbrochener Läufe erscheinen erst mit der Übernahme im Verlauf. Pro Auftrag gibt es weiterhin genau einen Commit im Hauptstand, Zwischen-Commits des Branches werden dabei zusammengefasst. Vor dem Upgrade zurückgehaltene Änderungen werden bei ihrer Freigabe in das Arbeitsverzeichnis des Auftrags verschoben.

## Ergebnis als Branch oder Pull Request, Feedback-Loop (Projekt → Ergebnis)

Pro Projekt legt die Eigentümerin bzw. der Eigentümer fest, was ein fertiger, geprüfter Auftrag abliefert:

| Modus | Was passiert | Wer pusht wann |
|---|---|---|
| **Commit im Hauptstand** (Standard, wie bisher) | Übernahme-Warteschlange: ein Commit pro Auftrag im Hauptstand von Queue | nur mit „Automatisch hochladen“, nach der Übernahme; auf den Standard-Branch nur nach Freigabe |
| **Branch hochladen** | Statt der lokalen Übernahme lädt die Warteschlange den Branch `queue/<id>` des Auftrags hoch | Queue (Runner, vom Projekt-Repository aus), genau der Commit nach Schutzprüfung, Prüfbefehl und Review; Geheimnis-Prüfung der neuen Commits; nie Force, nie direkt auf den Standard-Branch |
| **Pull Request** | wie „Branch hochladen“, danach legt Queue einen Pull Request (GitLab: Merge Request) an und beobachtet ihn | wie oben; Folge-Aufträge pushen per Fast-Forward auf denselben Branch |

**Feedback-Loop (nur „Pull Request“):** Queue fragt alle `PR_POLL_SECS` (Standard 60 s) Zustand, Checks und Kommentare ab.
- **Rote CI** des aktuellen Commits (GitHub Check-Runs und Commit-Status, GitLab-Pipeline des Merge Requests mit dem Ende des Job-Logs, Gitea/Forgejo-Commit-Status) startet **einen** Folge-Auftrag – pro Commit höchstens einmal.
- **Kommentare** zählen nur von Personen **mit Schreibrecht**; das fragt Queue beim Git-Server ab (GitHub/Gitea: Berechtigung des Mitarbeitenden, GitLab: Rolle ≥ Developer), nie aus dem Kommentar. Kommentare am Code und „Änderungen anfordern“ zählen immer, allgemeine Kommentare nur mit **`/queue`**. Bots, Queues eigenes Konto und Systemnotizen zählen nie. Jeder Kommentar wird genau einmal betrachtet; das Ergebnis (auch „ignoriert: kein Schreibrecht“) steht im Auftrag unter „Pull Request → Rückmeldungen“.
- Der **Folge-Auftrag** arbeitet auf demselben Branch: Sein Arbeitsverzeichnis startet auf dem aktuellen Stand des Pull-Request-Branches beim Git-Server (auch mit Commits, die Menschen dort gepusht haben), er durchläuft Schutzprüfung, Prüfbefehl und Review und wird dann per Fast-Forward hochgeladen. Hat inzwischen jemand auf den Branch gepusht, überschreibt Queue nichts, sondern setzt die Änderungen in einem Korrektur-Auftrag auf dem neuen Stand fort.
- **Prompt-Injection:** Kommentar-, Review- und CI-Texte gelten als nicht vertrauenswürdig. Sie stehen im Prompt in einem Datenblock mit zufälliger Grenze, davor die Regel, dass es Daten und keine Anweisungen sind; Geheimnisse werden geschwärzt, die Länge ist begrenzt. Ihr Text erscheint nicht in der Oberfläche.
- **Schleifenschutz:** höchstens ein laufender Folge-Auftrag pro Pull Request und höchstens „Folge-Runden pro Pull Request“ (Standard 3, 0–10) Folge-Aufträge. Danach werden Rückmeldungen als „Runden-Grenze erreicht“ vermerkt und du bekommst eine Benachrichtigung; „Eine weitere Runde erlauben“ im Auftrag hebt die Grenze um eins an. Auto-Fix lässt sich pro Pull Request ausschalten.
- **Queue genehmigt und merged nie.** Der Code enthält keinen Aufruf dafür; die Tests prüfen, dass die Fake-Server nie eine solche Anfrage sehen. Gemergt oder geschlossen wird auf dem Git-Server; Queue hört dann auf zu fragen und räumt die lokalen Branches auf. Konflikte mit dem Ziel-Branch zeigt Queue an und meldet sie, löst sie aber nicht selbst.
- **Robust:** Abfragen laufen in einem eigenen Hintergrund-Task (blockiert die Warteschlange nie), mit Lease pro Pull Request (übersteht Neustarts und mehrere App-Instanzen), wachsender Pause bei Fehlern (bis 1 h), `Retry-After`/Rate-Limit-Headern (auch „Jetzt prüfen“ wartet sie ab) und `If-None-Match` (unveränderte Antworten kommen als 304 ohne Inhalt zurück). Für Webhooks (WP07) gibt es eine Schnittstelle, die nur eine sofortige Abfrage auslöst – Inhalte kommen immer von der API.

### Einrichtung pro Git-Server

1. Projekt → Einstellungen → Git: Repository-Adresse, Branch (Ziel des Pull Requests) und Zugangstoken eintragen (nur Eigentümer). Das Token wird verschlüsselt gespeichert (`MASTER_KEY`), nie angezeigt, nie geloggt und an Git nur über die Prozess-Umgebung übergeben (nicht über die Kommandozeile). Wechselt die Adresse auf einen anderen Server, löscht Queue das gespeicherte Token.
2. Projekt → Ergebnis: Modus wählen. Bei github.com, gitlab.com und codeberg.org erkennt Queue den Dienst; sonst Git-Server wählen. API-Adresse und Repository-Pfad leitet Queue aus der Repository-Adresse ab; nur bei Installationen unter einem Unterpfad eintragen.
3. „Verbindung prüfen“ zeigt, als wer das Token angemeldet ist und ob es schreiben darf.

| Git-Server | API (abgeleitet) | Minimale Token-Rechte |
|---|---|---|
| **GitHub** / GitHub Enterprise | `https://api.github.com` bzw. `https://<host>/api/v3` | Klassisches Token: **`repo`** (laut API-Beschreibung nötig für Checks privater Repositories). Fine-grained Token, nur dieses Repository: Contents (Lesen und Schreiben), Pull requests (Lesen und Schreiben), Commit statuses (Lesen), Checks (Lesen), Metadata (Lesen) – diese Rechte-Namen sind aus Vorwissen, nicht aus der Dokumentation belegt. Entwurfs-Pull-Requests gibt es bei privaten Repositories erst ab GitHub Team; sonst legt Queue einen normalen Pull Request an |
| **GitLab** (gitlab.com oder eigene Instanz) | `https://<host>/api/v4` | Projekt-Access-Token (Rolle **Developer**) oder persönliches Token mit **`api`** (umfasst `write_repository`; `read_api` reicht nicht, weil Merge Requests angelegt werden) |
| **Gitea / Forgejo** (z. B. Codeberg) | `https://<host>/api/v1` | Zugangstoken mit **`write:repository`**, **`write:issue`** (Labels) und **`read:user`** |

Eigene Server im internen Netz: `ALLOW_PRIVATE_GIT_HOSTS=true` (erlaubt dann auch eine `http://`-API-Adresse). Ohne diese Freigabe ruft die App nur öffentliche Adressen auf; Link-Local/Metadaten-Adressen und der Runner sind immer gesperrt, Weiterleitungen folgt der Client nie (ein Token in einem eigenen Header wie GitLabs `PRIVATE-TOKEN` würde sonst mitwandern).

**Upgrade-Hinweis WP05:** Repository-Adresse, Branch, Token und „Automatisch hochladen“ darf nur noch die Eigentümerin bzw. der Eigentümer ändern (vorher konnten Bearbeiter die Adresse umstellen, und das gespeicherte Token ging dorthin mit). Bestehende Projekte bleiben im Modus „Commit im Hauptstand“.

## Schutz vor Lecks und ausgetricksten Prüfungen (Projekt → Schutz)

Queue prüft, bevor etwas gespeichert, geprüft oder hochgeladen wird. Findet es etwas, hält es den Auftrag an (Status **„Freigabe nötig“**, Push-Benachrichtigung, Eintrag unter „Wartet auf dich“). Im Auftrag stehen die Funde mit Datei, Zeile und einer Erklärung; die zurückgehaltenen Änderungen lassen sich ansehen. Freigeben dürfen die Person, die den Auftrag angelegt hat, und die Eigentümerin bzw. der Eigentümer des Projekts.

| Prüfung | Wann | Abschaltbar |
|---|---|---|
| **Geheimnisse** (Tokens von GitHub, GitLab, AWS, Anthropic, OpenAI, Google, Slack, Stripe, npm, PyPI, JWT, private Schlüssel, Passwörter in URLs und Zuweisungen mit hoher Entropie) | Vor jedem Commit (auch Teil- und WIP-Commits), vor jedem Push, in der Ausgabe des Agenten | nein |
| **Push auf den Standard-Branch** des Remotes (bei unbekanntem Standard: `main`, `master`) | Auto-Push | nein; „Hochladen“ pusht genau den Stand des Auftrags, „Nicht hochladen“ schließt ohne Push ab |
| **Review-Ausfall** | Review aktiv, aber nicht möglich | nein; transiente Fehler werden zuerst nur für das Review wiederholt |
| **Geschützte Dateien** (Standard: CI-Dateien, `tests/`, `test/`, `__tests__/`, `*.test.*`, `*.spec.*`, `*_test.go`, `test_*.py`, `conftest.py` …) | Bestehende Datei geändert, umbenannt oder gelöscht; neue Dateien sind erlaubt | Liste bearbeitbar |
| **Geschwächte Tests** (neue `skip`/`xfail`/`#[ignore]`/`.only`/`@Disabled` …, gelöschte Testdateien, entfernte Prüfungen, geänderte Test-Einstellungen wie `jest.config.*` oder `package.json`-Skripte) | wie oben | ja |
| **Setup-Dateien** (Lock-/Manifest-Dateien, im Setup-Skript genannte Pfade, Cache-Dateien) | Vor einem Setup **mit Geheimnissen**, wenn sie sich seit der letzten Freigabe geändert haben (erster Lauf vertraut dem aktuellen Stand) | ja |
| **Einstellungen aus dem Repository** | Standard: Agenten laden sie nicht. Erlaubt die Eigentümerin sie, braucht jede spätere Änderung daran (`.claude/`, `.mcp.json`, `.gemini/`, `opencode.json`, `.opencode/`, `.codex/`) vor dem nächsten Lauf eine Freigabe | ja (Standard aus) |

- Werte von Geheimnissen erscheinen **nie** im Klartext: Protokoll, Ergebnis, Benachrichtigungen (auch ntfy/Web Push), Diff-Ansicht und API zeigen `[geschwärzt: …]` bzw. „GitHub-Token ghp_… (40 Zeichen)“. Eine Freigabe merkt sich nur einen Fingerabdruck (HMAC mit dem `RUNNER_TOKEN`) im Projekt; wird das Token gewechselt, fragt Queue erneut. „Freigaben vergessen“ im Reiter „Schutz“ setzt das zurück.
- Gehaltene Änderungen liegen im Runner unter `refs/queue/held/<auftrag>` im eigenen Arbeitsverzeichnis des Auftrags (nie auf dem Branch, nie gepusht, für andere Aufträge unsichtbar), auch Commits, die der Agent selbst gemacht hat. „Verwerfen“ und „Abbrechen“ löschen sie samt Git-Objekten. Reste unterbrochener Läufe mit Geheimnissen werden nicht als WIP committet, sondern unter `refs/queue/quarantine/…` abgelegt.
- Verify läuft erst nach der Schutzprüfung: geschützte Dateien sieht der Prüfbefehl nur im Ausgangsstand oder in einer freigegebenen Fassung.
- **Test-Auftrag:** Im Composer unter „Erweitert“ → „Darf Tests ändern“. Gilt nur für diesen, von einem Menschen angelegten Auftrag (nie für Fix- oder Folgeaufträge).
- **Review:** Ohne eigene Review-Wahl prüft ein anderer Anbieter als der umsetzende, sofern ein Zugang dafür hinterlegt ist. Der Review-Agent bekommt eine Rubrik (Aufgabe wirklich erfüllt? Tests geschwächt? hart kodierte Ergebnisse? Sicherheit? Umfang?) und die freigegebenen Funde.
- **Git im Runner:** Hooks, `core.fsmonitor`, Filter, Diff-/Merge-Treiber, Signatur-Programme, Credential-Helper, `url.*.insteadOf` und `http.*` aus der (vom Agenten beschreibbaren) `.git/config` werden nie ausgeführt: Der Runner überschreibt sie per Kommandozeile und schreibt `.git/config` vor jeder Operation aus einer Allowlist neu. Pull und Push gehen immer an die URL aus den Projekteinstellungen.

So wird Repository-Konfiguration je Agent abgeschaltet (Standard):

| CLI | Mittel | Beleg |
|---|---|---|
| Claude Code 2.1.319 | `--setting-sources user --strict-mcp-config` (keine Projekt-Settings mit Hooks/`env`/Helfern, keine `.mcp.json`) | Optionen im installierten Programm; nicht mit echtem Modell geprüft |
| Gemini CLI 0.67.0 | System-Einstellungen mit höchster Priorität (keine Hooks, keine Tool-/MCP-Befehle, keine Projekt-`.env`) und `--allowed-mcp-server-names` mit Zufallsnamen. Gemini braucht das Workspace-Vertrauen weiter (sonst kein `--yolo`) und liest `.gemini/.env` trotzdem – Änderungen daran brauchen deshalb immer eine Freigabe | Quelltext des Bundles; nicht ausgeführt |
| opencode 1.19.5 | `OPENCODE_DISABLE_PROJECT_CONFIG=1` (keine `opencode.json`, kein `.opencode/` mit Plugins, keine Projekt-`AGENTS.md`) | Programm-Strings; nicht ausgeführt |
| Codex 0.167.0 | nichts nötig: lädt `.codex/config.toml` nur in vertrauten Ordnern, Queue vertraut nie (die Freischaltung wirkt bei Codex nicht) | Programm-Strings; nicht ausgeführt |

**Upgrade-Hinweis WP03:** Projekte mit Auto-Push auf `main`/`master` (bzw. den Standard-Branch des Remotes) pushen nicht mehr automatisch, sondern fragen pro Auftrag nach. Aufträge mit Review fallen bei einem Review-Ausfall nicht mehr auf „Fertig“ zurück. Änderungen an bestehenden Tests brauchen eine Freigabe (Liste im Reiter „Schutz“ anpassbar, oder Test-Auftrag).

## Plan-Modus, Nachrichten an den Agenten und Kommentare im Diff

**Erst planen, dann umsetzen.** Im Projekt (Einstellungen → „Erst planen, dann umsetzen“, nur Eigentümer) oder pro Auftrag (Composer → Erweitert → „Erst einen Plan zeigen“). Der Agent schaut sich das Projekt nur an und schreibt einen Plan; der Auftrag wartet danach unter „Wartet auf dich“ (Status „Freigabe nötig“). Im Auftrag stehen der Plan, offene Fragen und Warnungen:

- **Plan genehmigen** – der Agent setzt genau diesen Plan um (er steht als freigegebener Plan in seinem Auftrag).
- **Ändern** → „Selbst bearbeiten“ (der geänderte Text wird umgesetzt) oder „Neuen Plan anfordern“ (mit deiner Anmerkung, wieder nur lesend; höchstens 10 Überarbeitungen).
- **Verwerfen** – der Auftrag endet, nichts wurde geändert („Erneut versuchen“ plant neu).
- **Automatisch genehmigen** (Projekt, Standard: nie): nach 15 min bis 24 h, nie bei offenen Fragen oder wenn der Agent beim Planen etwas ändern wollte.

Dass der Plan-Lauf nichts ändert, erzwingt Queue technisch, nicht nur per Prompt: (1) der Plan-Schalter der CLI, (2) der Runner hängt das Arbeitsverzeichnis im Container schreibgeschützt ein und vergleicht auf jeder Engine den ganzen Baum (inkl. `.git` und ignorierter Dateien, mit ctime) vor und nach dem Lauf, (3) der Plan-Lauf arbeitet in einer Wegwerf-Kopie mit demselben Ausgangsstand, die danach immer gelöscht wird – Branch, Arbeitsverzeichnis und Hauptstand des Auftrags werden nie berührt. Versuchte Änderungen erscheinen als Warnung beim Plan. Ist für das Projekt eine Planung per API/Batch eingestellt (Modellwahl), wartet deren Ergebnis ebenso auf die Freigabe.

| CLI | Plan-Schalter | Fortsetzen der Sitzung | Beleg |
|---|---|---|---|
| Claude Code | `--permission-mode plan` (+ Plan-Schema) | `--session-id` / `--resume <id>` | live mit dem installierten 2.1.322 geprüft (gepinnt ist 2.1.319); Aufnahmen in `tests/fixtures/cli/claude-2.1.322/` |
| Codex | – (Codex' eigene Sandbox ist im Container generell aus; Schutz durch Queue) | `codex exec … resume <id> -` | Programm-Strings 0.167.0, **nicht ausgeführt** |
| Gemini CLI | `--approval-mode plan` | `--session-id` / `--resume <id>` | Quelltext 0.67.0, **nicht ausgeführt** |
| opencode | `--agent plan` | `--session <id>` | Programm-Strings 1.19.5, **nicht ausgeführt** |

**Nachricht an den Agenten.** Im Auftrag (solange er nicht fertig ist) unter „Nachricht an den Agenten“. Die Nachricht wartet bis zur nächsten Pause des Agenten und lässt sich bis dahin **zurückziehen**. Zugestellt wird sie

- beim Start des Agenten (Auftrag wartet noch) – jeder neue Lauf sieht alle bisher zugestellten Nachrichten;
- am Ende seines aktuellen Laufs **in dieselbe Sitzung** (CLI-Resume, Verlauf und Cache bleiben), danach erst Prüfung, Commit, Verify und Review; „**Sofort zustellen**“ unterbricht den Agenten dafür (SIGINT) und setzt fort; ohne Resume-Fähigkeit oder bei unbekannter Sitzung als neuer Lauf im selben Arbeitsverzeichnis mit Übergabe;
- als **Fortsetzungs-Auftrag**, wenn der Agent schon fertig war (Nachricht während Verify/Review/Übernahme). Bei Abbruch verfällt sie; bei fehlgeschlagenen/blockierten Aufträgen wartet sie auf „Erneut versuchen“ oder „Weiter mit …“.
Höchstens 10 Zustell-Runden und 20 wartende Nachrichten pro Auftrag, 4 000 Zeichen pro Nachricht; die Limits des Auftrags gelten über alle Runden. Ein Auftrag mit Rückfrage nimmt eine Nachricht als Antwort.

**Weiter mit …** Bei fertigen Aufträgen startet „Weiter mit diesem Auftrag“ einen neuen Auftrag, der genau dort weitermacht: gleicher Agent, Übergabe des Vorgängers, dessen Änderungen (falls noch nicht im Hauptstand) bzw. derselbe Pull Request – und, wenn du den Auftrag selbst angelegt hattest, **dieselbe Sitzung** des Agenten. Fremde Sitzungsverläufe gehen nie an andere Nutzer.

**Kommentare im Diff.** „Änderungen anzeigen“ → „Kommentieren“: Zeile antippen (bzw. klicken), eine zweite Zeile im selben Abschnitt macht daraus einen Bereich; „Kommentar schreiben“. Kommentare werden als Entwurf gesammelt (nur du siehst sie) und mit „N Kommentare senden“ als **eine** Nachricht verschickt – an den laufenden bzw. wartenden Auftrag, oder als „Weiter mit …“, wenn er fertig ist. Ändert sich der Diff danach, steht der Kommentar als „verschoben“ an der neuen Stelle oder als „veraltet“ (mit den ursprünglichen Zeilen) in einer eigenen Liste; veraltete Kommentare gehen mit diesem Hinweis an den Agenten.

**Sitzungen** speichert der Runner pro Auftrag unter `<RUNNER_DATA_DIR>/sessions/<projekt>/<auftrag>` (nur die Sitzungsdateien der CLI, nie Zugangs- oder Konfigurationsdateien, keine Symlinks, höchstens 64 MiB); sie werden mit dem Projekt und nach `RUNNER_SESSION_TTL_DAYS` (Standard 7) Tagen ohne Nutzung gelöscht.

**Rechte:** Plan entscheiden und Nachrichten senden dürfen die Person, die den Auftrag angelegt hat (solange sie mindestens Bearbeiter ist), und die Projekt-Eigentümerin bzw. der -Eigentümer; Betrachter lesen mit. „Weiter mit …“ und Kommentare: alle Bearbeiter (der neue Auftrag gehört dann dir und läuft mit deinen Zugängen). Nachrichten, Kommentare, bearbeitete Pläne und Anmerkungen kommen in eigenen, mit Zufallsgrenze markierten Blöcken in den Prompt (Längen begrenzt, Geheimnisse geschwärzt).

## Automatisierung: Vorlagen, Zeitpläne, Webhooks, API und `queuectl`

**Vorlagen** (Projekt → Automatisierung → Vorlagen, ab Bearbeiter): ein gespeicherter Auftrag mit Titel, Beschreibung, „Erst planen“ und Kostenobergrenze. Auslöser starten Aufträge aus Vorlagen; im Auftragsformular (Erweitert → „Aus Vorlage“) und mit `queuectl add --template` kannst du sie auch von Hand nutzen.

**Auslöser** (Projekt → Automatisierung → Auslöser). Ein Auslöser gehört der Person, die ihn anlegt; seine Aufträge laufen **immer mit ihrem Zugang** zum Anbieter, nie mit dem eines anderen Mitglieds. Nur sie ändert ihn, startet ihn von Hand oder erzeugt ein neues Secret; ausschalten und löschen darf zusätzlich die Projekt-Eigentümerin. Verliert sie die Bearbeitungsrechte im Projekt, schaltet sich der Auslöser beim nächsten Versuch ab (Verlauf + Benachrichtigung).

| Art | Startet | Hinweise |
|---|---|---|
| Zeitplan | nach Cron (`Minute Stunde Tag Monat Wochentag`, z. B. `15 7 * * 1-5`) in einer Zeitzone (`Europe/Zurich`) | Minute muss eine feste Zahl sein ⇒ **höchstens stündlich**; `@daily` usw. gehen. Zeitumstellung: eine Ortszeit, die es nicht gibt, läuft eine Stunde später; eine doppelte nur einmal; zwischen zwei Läufen liegt immer mindestens eine Stunde |
| Einmalig | zu einem Zeitpunkt | danach erledigt |
| Webhook | signierter `POST` an `PUBLIC_URL/api/hooks/<id>` | siehe unten |
| GitHub | GitHub-Webhook mit Filtern | siehe unten |
| Nur per API | `POST /api/triggers/<id>/fire` mit Token | z. B. aus CI |

Für alle gilt: „Nicht starten, solange der letzte Auftrag läuft“ (Standard an), höchstens N Aufträge pro Stunde (Standard 10, 1–100; darüber `429` und Eintrag „abgelehnt“), „Jetzt auslösen“ von Hand. Der **Verlauf** zeigt jede Auslösung mit Ergebnis (erstellt, ignoriert, übersprungen, abgelehnt, fehlgeschlagen), Grund und Link zum Auftrag (90 Tage, höchstens 500 Einträge pro Auslöser). Was ein Absender mitschickt, bekommt der Agent **nur als markierten Datenblock** (`<<<EVENT_DATA_<zufall>` … mit der Regel davor, dass es keine Anweisung ist; Grenzen im Inhalt entschärft, Geheimnisse geschwärzt, höchstens 16 000 Bytes) – nie im Titel oder in der Aufgabe.

**Zuverlässigkeit der Zeitpläne.** Ein fälliger Termin wird in **einer** Datenbank-Transaktion erledigt: Zeile sperren (`FOR UPDATE SKIP LOCKED`), Termin im Verlauf eintragen (eindeutig pro Auslöser und Termin), Auftrag anlegen, nächsten Termin setzen. Stürzt Queue vorher ab, passiert nichts und der Termin läuft nach dem Neustart genau einmal; danach nie ein zweites Mal – auch nicht mit mehreren App-Instanzen. War Queue zum Termin nicht erreichbar (mehr als 10 min zu spät), gilt er als verpasst: Mit „Verpasste Läufe einmal nachholen“ (Standard an) läuft **genau ein** Auftrag, egal wie viele Termine verpasst wurden (Verlauf: „Nachgeholt …; N frühere Termine verpasst“), sonst steht „Verpasst“ im Verlauf. Ausgeschaltete Auslöser holen nach dem Einschalten nichts nach. Takt: `TRIGGER_TICK_SECS` (Standard 15).

### Webhook einrichten (eigene Systeme)

Nach dem Anlegen zeigt Queue **einmal** Adresse und Secret (`qwh_…`; bei Verlust „Neues Secret“). Der Absender schickt:

```
POST /api/hooks/<id>
X-Queue-Timestamp: <Unix-Sekunden>        # höchstens ±300 s Abweichung
X-Queue-Delivery:  <eindeutige ID>        # 1–100 Zeichen A–Z a–z 0–9 . _ : -
X-Queue-Signature: sha256=<hex HMAC-SHA256(secret, "<timestamp>.<delivery>.<body>")>
```

```bash
TS=$(date +%s); ID=$(uuidgen); BODY='{"service":"api","status":"failed"}'
SIG=$(printf '%s.%s.%s' "$TS" "$ID" "$BODY" | openssl dgst -sha256 -hmac "$SECRET" | sed 's/^.* //')
curl -X POST "$URL" -H 'Content-Type: application/json' -H "X-Queue-Timestamp: $TS" \
  -H "X-Queue-Delivery: $ID" -H "X-Queue-Signature: sha256=$SIG" --data "$BODY"
```

Antworten: `201` Auftrag angelegt, `200` Duplikat/ignoriert/übersprungen, `401` Signatur oder Zeitstempel falsch (nichts gespeichert), `404` unbekannt, `409` ausgeschaltet oder kein Auftrag möglich, `413` größer als 1 MiB, `429` Limit (`Retry-After`). Die Signatur wird in konstanter Zeit verglichen. Jede Delivery-ID zählt einmal (sie ist mitsigniert, ein abgefangener Aufruf lässt sich also auch mit neuer ID nicht wiederholen); ein abgelehnter Aufruf (Limit, Fehler) darf mit derselben ID wiederholt werden.

### GitHub einrichten

1. Projekt → Automatisierung → Neuer Auslöser → **GitHub**: Ereignisse wählen, optional Branches (`main`, `release/*`; `*` ohne `/`, `**` mit), Labels, Repository (leer = das des Projekts aus „Ergebnis“ bzw. der Remote-Adresse).
2. Queue zeigt einmal Adresse und Secret. Auf GitHub im Repository: **Settings → Webhooks → Add webhook**, *Payload URL* = Adresse, *Content type* `application/json` (Form-Kodierung geht auch), *Secret* = Secret, „Let me select individual events“: die passenden Ereignisse (unten). GitHub schickt sofort einen *Ping*, der im Verlauf erscheint.

| Ereignis in Queue | GitHub-Ereignis | löst aus bei | Schleifenschutz |
|---|---|---|---|
| Push | `push` | neue Commits auf passendem Branch | nicht bei gelöschtem Branch, Tags, Branches `queue/…`, wenn der neueste Commit von Queue committet wurde (`queue@localhost`), nicht von Bots (abschaltbar) |
| Issues | `issues` | `opened`, `reopened`, `labeled` (nur mit Label-Filter und passendem Label) | ein Auftrag pro Issue (außer „wieder geöffnet“), nicht von Bots |
| Pull Requests | `pull_request` | `opened`, `reopened`, `synchronize`, `ready_for_review`, `labeled`; Branch-Filter = Ziel-Branch | ein Auftrag pro PR-Commit, keine Entwürfe (abschaltbar), nie für Queues eigene PRs |
| Fehlgeschlagene Checks | `check_run` | `completed` mit `failure`, `timed_out`, `startup_failure` | ein Auftrag pro Commit; nie auf `queue/…` (dafür ist der Auto-Fix aus WP05 da) |

Signatur: `X-Hub-Signature-256` (HMAC-SHA256 über den Body, konstanter Vergleich, geprüft mit dem Testvektor aus GitHubs Doku). GitHub signiert keinen Zeitstempel; Wiederholungen erkennt Queue an `X-GitHub-Delivery` **und** am SHA-256 des Bodys (90 Tage gespeichert). Zusätzlich gilt für **Queues eigene Pull Requests** (WP05): Jedes gültig signierte Ereignis dazu (`pull_request`, `pull_request_review`, `pull_request_review_comment`, `issue_comment` am PR, `check_run`, `check_suite`, `push` auf `queue/…`) lässt den Pull Request **sofort** abfragen – der Zustand kommt weiterhin nur aus der GitHub-API, der Inhalt des Webhooks wird dafür nie verwendet. Dafür die genannten Ereignisse zusätzlich abonnieren.

Belegt sind Header und Felder aus GitHubs offizieller OpenAPI-Beschreibung (`github/rest-api-description`, Schemas `webhook-*`) und der Webhook-Doku (`github/docs`); nicht belegt ist der Wert `"Bot"` von `sender.type` (Queue erkennt Bots zusätzlich an `[bot]` im Login). Getestet ist gegen nachgebaute Zustellungen, nicht gegen GitHub selbst.

### API-Tokens und REST-API

Einstellungen → **API-Zugang**: Token mit Namen, Berechtigungen und Ablauf (7 Tage bis 1 Jahr, ohne Ablauf gibt es nicht) anlegen. Das Token (`qk_…`, 256 Bit Zufall + Prüfsumme, vom Secret-Scanner erkannt) wird **einmal** angezeigt; gespeichert wird nur sein SHA-256. Die Liste zeigt Präfix, Rechte, Ablauf und „zuletzt benutzt“; Widerrufen wirkt sofort (auch auf offene Live-Verbindungen spätestens nach 10 min).

Aufruf mit `Authorization: Bearer qk_…`. Mit Token gilt **nur** das Token (ein Cookie wird ignoriert), und nur auf diesen Endpunkten – alles andere (Konto, Einstellungen, Zugänge, **Token-Verwaltung**, Mitglieder, Geheimnisse, Ergebnis/Git-Zugang, Schutz-Freigaben, Benachrichtigungen) antwortet `403`. Rollen und Projekte gelten wie im Browser.

| Berechtigung | Endpunkte |
|---|---|
| (jedes Token) | `GET /api/whoami` |
| `projects:read` | `GET /api/projects`, `GET /api/projects/{id}`, `GET /api/projects/{id}/templates` |
| `tasks:read` | `GET /api/tasks`, `GET /api/tasks/{id}`, `GET /api/projects/{id}/tasks`, `GET /api/tasks/{id}/diff`, `GET /api/tasks/{id}/messages`, `GET /api/events` (SSE) |
| `tasks:write` | `POST /api/tasks`, `PATCH /api/tasks/{id}`, `POST /api/tasks/{id}/cancel`, `POST /api/tasks/{id}/retry` |
| `tasks:steer` | `POST /api/tasks/{id}/messages`, `DELETE /api/tasks/{id}/messages/{mid}`, `POST /api/tasks/{id}/continue`, `POST /api/tasks/{id}/answer`, `POST /api/tasks/{id}/plan/{approve,revise,reject}` |
| `triggers:read` | `GET /api/projects/{id}/triggers`, `GET /api/triggers/{id}`, `GET /api/triggers/{id}/runs`, `GET /api/timezones`, `POST /api/triggers/preview` |
| `triggers:write` | `POST /api/projects/{id}/templates`, `PATCH/DELETE /api/templates/{id}`, `POST /api/projects/{id}/triggers`, `PATCH/DELETE /api/triggers/{id}`, `POST /api/triggers/{id}/secret` |
| `triggers:fire` | `POST /api/triggers/{id}/fire` (Body optional `{"text": "…"}`; der Text geht als Datenblock an den Agenten) |

Fehler: `401` mit `invalid token`, `token expired` oder `token revoked`; `403` mit `token lacks scope …` bzw. `this endpoint cannot be used with an API token`. Beispiel:

```bash
curl -H "Authorization: Bearer $QUEUE_TOKEN" -H 'Content-Type: application/json' \
  -d '{"project_id":"<uuid>","prompt":"Tests für den Login ergänzen"}' https://queue.example/api/tasks
curl -X POST -H "Authorization: Bearer $QUEUE_TOKEN" -H 'Content-Type: application/json' \
  -d '{"text":"Build 1234 ist rot"}' https://queue.example/api/triggers/<id>/fire
```

### `queuectl`

Kleines Kommandozeilen-Werkzeug im Backend-Workspace (`cd backend && cargo build --release --bin queuectl` ⇒ `target/release/queuectl`).

```bash
mkdir -p ~/.config/queuectl && umask 077
cat > ~/.config/queuectl/config <<'EOF2'
url = https://queue.example
token = qk_…
project = Vereins-Website        # Standardprojekt (optional)
EOF2
chmod 600 ~/.config/queuectl/config
queuectl whoami
```

Das Token steht **nie auf der Kommandozeile** (es gibt keine Option dafür): aus `QUEUE_TOKEN` oder aus der Datei (`$QUEUECTL_CONFIG`, sonst `$XDG_CONFIG_HOME/queuectl/config` bzw. `~/.config/queuectl/config`). Eine Datei mit Token muss dir gehören und darf für niemand anderen lesbar sein (`0600`), sonst bricht `queuectl` ab. `QUEUE_URL`/`QUEUE_PROJECT` überschreiben die Datei. `http://` nur für `localhost` (sonst ginge das Token im Klartext übers Netz; Ausnahme `insecure = true`), Weiterleitungen werden nie verfolgt, Format und Prüfsumme des Tokens prüft `queuectl` vor dem ersten Aufruf.

| Befehl | Was |
|---|---|
| `queuectl add [-p PROJEKT] [-t TITEL] [--template NAME] [--plan] [--max-cost USD] [-w] TEXT…` | Auftrag anlegen (Text auch über stdin mit `-`); `-w` verfolgt ihn danach |
| `queuectl list [-p PROJEKT] [-s STATUS]` | Aufträge |
| `queuectl status ID` | Details (ID oder eindeutiger Anfang, z. B. `3f2a9c1d`) |
| `queuectl watch ID [--poll] [--timeout S]` | live verfolgen (SSE, verbindet neu; `--poll` fragt ab) bis zum Ende |
| `queuectl cancel ID` | abbrechen |
| `queuectl steer ID [--now] TEXT…` | Nachricht an den Agenten (zurückziehbar bis zur Zustellung) |
| `queuectl continue ID [--plan] TEXT…` | „Weiter mit …“ |
| `queuectl plan ID approve` / `revise TEXT…` / `reject` | Plan entscheiden |
| `queuectl fire AUSLÖSER-ID [TEXT…]` | eigenen Auslöser starten |
| `queuectl projects`, `queuectl whoami` | Projekte, Token prüfen |

`--json` gibt die Rohdaten aus. Exit-Codes: `0` ok, `1` Auftrag nicht erfolgreich (fehlgeschlagen, blockiert, abgebrochen) bzw. Server lehnt ab, `2` Bedienung/Konfiguration, `3` Token fehlt, ungültig, abgelaufen, widerrufen oder ohne Berechtigung. Beispiel für CI: `queuectl add -w --template "Abhängigkeiten" "Nur Patch-Versionen" || exit 1`.

## Anbindungen: GitHub Issues, Linear, Slack, Jira (Projekt → Anbindungen)

Aufträge entstehen dort, wo die Arbeit beschrieben wird; Queue meldet den Stand dorthin zurück und nimmt Antworten auf Rückfragen entgegen. Verbinden kann nur die Projekt-Eigentümerin; jede Anbindung hat eine eigene Adresse `https://…/api/intake/<id>` (Slack: Events **und** Slash-Befehl an dieselbe Adresse).

| Werkzeug | Was wird zum Auftrag? | Rückmeldung | Antwort auf Rückfragen |
|---|---|---|---|
| GitHub Issues | Issue bekommt das Label (oder wird damit geöffnet) bzw. enthält die Erwähnung (z. B. `@queue`) – im Text oder in einem neuen Kommentar | Kommentare im Issue (angenommen, Rückfrage, fertig/fehlgeschlagen/abgebrochen) | Kommentar im Issue, während der Auftrag wartet; Kommentar mit Erwähnung = Nachricht bzw. „Weiter mit …“ |
| Linear (Agent-API) | Issue an die Queue-App delegieren oder sie erwähnen (`AgentSessionEvent created`) | Agent-Activities: `thought` sofort, `elicitation` bei Rückfragen, `response`/`error` am Ende | Nachricht in der Agent-Sitzung (`prompted`); nach dem Ende = „Weiter mit …“ |
| Slack | Erwähnung der App (`app_mention`) oder Slash-Befehl | Antworten im Thread (beim Slash-Befehl beginnt Queue den Thread) | Antwort im Thread, während der Auftrag wartet; Erwähnung im Thread = Nachricht bzw. „Weiter mit …“ |
| Jira Cloud | Vorgang wird mit dem Label erstellt oder bekommt es | Kommentare am Vorgang | Kommentar am Vorgang, während der Auftrag wartet |

**Wer darf?** Standard ist restriktiv: Nur externe Konten, die einer Person im Projekt **zugeordnet** sind, legen Aufträge an oder antworten. Eine Zuordnung gilt erst, wenn **beide** zugestimmt haben – die Projekt-Eigentümerin (wer im Projekt handeln darf) und die Person selbst (deren Zugang und Abo werden verbraucht). Der Auftrag läuft als diese Person; sie braucht mindestens die Rolle „Bearbeiten“, für Antworten gelten die Regeln von „Nachricht an den Agenten“ (eigener Auftrag oder Eigentümerin). Optional („Erweitert“) dürfen auch nicht zugeordnete Personen Aufträge anlegen – dann **als Eigentümerin der Anbindung, auf deren Kosten**; bei GitHub zusätzlich nur mit `author_association` OWNER/MEMBER/COLLABORATOR. Ein Workspace/eine Organisation/ein Repository/eine Jira-Site gehört zu **genau einem** Queue-Projekt; Ereignisse aus einem anderen Arbeitsbereich (eine Slack- oder Linear-App kann in mehreren Workspaces installiert sein) werden nach der Signaturprüfung mit 403 abgewiesen.

**Sicherheit:** Jede Zustellung wird vor allem anderen geprüft (falsch ⇒ 401, nichts gespeichert), Wiederholungen werden in der Datenbank erkannt (Duplikat ⇒ 200). Issue-Texte, Slack-Nachrichten, Linear-Kontext und Jira-Beschreibungen gehen nur als gekennzeichneter `EVENT_DATA`-Block (Zufallsgrenze, entschärft, Secrets geschwärzt, ≤ 8 000 Zeichen Text) an den Agenten, nie in Titel oder Prompt; Antworten sind Nachrichten (≤ 4 000 Zeichen, eigener Block, im Auftrag als „über Slack“ usw. markiert). Höchstens N Aufträge pro Stunde je Anbindung (Standard 10). Rückmeldungen enthalten feste Texte, die Zusammenfassung des Agenten (Secret-Scanner, gekürzt) und den Link zum Auftrag, **nie einen Diff** und nie ein Token; sie laufen in einer eigenen Warteschlange (Wiederholung mit Backoff bis 1 h, `Retry-After`, nach 8 Versuchen „nicht zugestellt“ + Benachrichtigung), jede Phase genau einmal – auch wenn eine Antwort des Dienstes verloren geht (GitHub: unsichtbarer Marker, Jira: „Queue · Ref …“, Slack: Metadaten, Linear: vorab gewählte Activity-ID). Aufrufe nur über den SSRF-geschützten Client ohne Weiterleitungen; Jira nur `https://…atlassian.net`. Secrets und Tokens liegen AES-256-GCM-verschlüsselt in der Datenbank und erscheinen in keiner API-Antwort. Ohne Token gibt es keine Rückmeldungen (und keine Fehlermeldungen darüber).

| Werkzeug | Einrichtung | Minimale Rechte des Tokens |
|---|---|---|
| GitHub | Repository → Settings → Webhooks → Add webhook: Payload URL = Adresse, `application/json`, Secret = das einmal angezeigte `qwh_…`, Ereignisse „Issues“ und „Issue comments“ | Fine-grained Token nur für dieses Repository: „Issues: Read and write“ (Metadata: Read automatisch) |
| Linear | Settings → API → OAuth-App anlegen, Webhook mit „Agent session events“ auf die Adresse, Signing-Secret in Queue eintragen; App mit `actor=app` installieren | Scopes `app:assignable`, `app:mentionable`, `read`, `write` (Zugriffstoken der App) |
| Slack | api.slack.com/apps → App anlegen, Event Subscriptions (Request URL = Adresse; Bot-Events `app_mention`, `message.channels`), optional Slash-Befehl mit derselben URL; Signing Secret und Bot-Token eintragen, App in den Kanal einladen | Bot-Scopes `app_mentions:read`, `chat:write`, `commands`, `channels:history` (+ `groups:history` für private Kanäle) |
| Jira Cloud | Einstellungen → System → WebHooks: URL = Adresse, Secret = `qwh_…`, Ereignisse „Vorgang erstellt/aktualisiert“, „Kommentar erstellt“ (per JQL aufs Projekt einschränken) | API-Token eines eigenen Kontos mit „Durchsuchen“ und „Kommentieren“ im Jira-Projekt, dazu dessen E-Mail-Adresse |

Belege: Slack `X-Slack-Signature` v0 mit Zeitfenster ±5 min (Testvektor der Slack-SDKs = Doku-Beispiel), Linear `Linear-Signature` + signierter `webhookTimestamp` (60 s) und GraphQL-Schema aus `linear/linear`, GitHub aus OpenAPI/Doku (WP07-Testvektor). **Jira** ist nur über eine Sekundärquelle belegt (`jira.js` 6.4.0: `X-Hub-Signature: sha256=…`, `X-Atlassian-Webhook-Identifier`), die Atlassian-Doku war nicht abrufbar; App-JWT (Connect) wird nicht unterstützt. Getestet ist ausschließlich gegen nachgebaute Zustellungen und Fake-Server (`tests/common/fake_intake.rs`, im E2E-Stack `cargo run --example fake_intake`), nie gegen die echten Dienste. Für Tests lassen sich die API-Adressen mit `INTAKE_{GITHUB,SLACK,LINEAR,JIRA}_API_URL` umbiegen (auch dann nur über den SSRF-Client); `INTAKE_TICK_SECS` (Standard 5) steuert die Rückmelde-Schleife.

## Tests

| Ebene | Was | Ausführen |
|---|---|---|
| Unit (Rust) | Git-Ops inkl. Clone/Push/Pull, atomarer Revert, Path-Traversal, Exec (Streaming, Timeout, Cancel, Prozessgruppe), Container-Flags, Parser aller Provider, Limit-Erkennung, Budget-Logik, Prompts, Crypto, OIDC/PKCE/Apple-JWT | `cd backend && cargo test --workspace --lib` |
| Integration (Rust) | App + Runner in-process + echtes Postgres (frische DB pro Test): Auth, Tenant-Isolation, Rollen, kompletter Task-Lifecycle mit Git, Fix-Loop, Rückfragen, Freigaben, Abo-Limit, Budgets, Cancel, Revert, Credentials, OnEmptyQueue, Clone + Auto-Push, Parallelität, SSE-Isolation, ZIP | `DATABASE_URL=postgres://… cargo test --workspace` |
| Integration WP02 | Umgebung (Rollen, Validierung, Geheimnisse nie in Antworten/Events, verschlüsselt), Setup vor dem Agenten, Geheimnis nur im Setup, Setup-Fehler ⇒ blocked ohne Agent/Fix, Geheimnis in Datei ⇒ blocked + bereinigt, Cache-Treffer/Invalidierung/Leeren, Netz-Ereignisse aller Phasen (erlaubt gegen lokalen Fake-Server, abgelehnt vor DNS) | `cargo test -p queued --test environments` |
| Unit WP02 | Egress-Proxy (CONNECT, absolute-form HTTP, Allowlist vor DNS, interne/Metadaten-/Host-Adressen, `open`/`off`, Nur-lesen für HTTP und HTTPS mit Lauf-CA gegen lokalen TLS-Server, Pipelining, Unix-Socket + echte Node-Brücke), Container-Argumente, Cache-Schlüssel, Leck-Suche/Bereinigung, SSRF-Resolver und Redirect-Policy der App | `cargo test -p queued-runner -p queued-common -p queued-api` und `cargo test -p queued --lib llm::` |
| Integration WP04 | Parallele Aufträge im selben Projekt (eigene Arbeitsverzeichnisse, serielle Übernahme, lineare Historie, Aufräumen), Standard-Parallelität 1 inkl. Übernahme, Merge-Konflikt ⇒ Korrektur-Auftrag mit übernommenen Änderungen und Markern, Verify nach Aktualisierung, Diff/Revert gegen die Merge-Base, Parallelität nur für Eigentümer, Merge-Queue-API, Abbrechen/Löschen räumt auf, nebenläufige Claims (Agent und Merge) ohne Überbuchung | `cargo test -p queued --test branches` |
| Integration WP05 | Pull Requests gegen Fake-Server für GitHub, GitLab und Gitea (`tests/common/fake_git_host.rs`) mit echtem Git-Remote: Anlegen (Entwurf, Labels), rote CI ⇒ Folge-Auftrag auf demselben Branch (Fast-Forward, Dedupe pro CI-Lauf), Kommentare ohne Schreibrecht/Bots/ohne `/queue` ignoriert, Runden-Grenze und „weitere Runde“, Neustart mitten im Abfragen (genau einmal), fremder Push auf den PR-Branch wird nie überschrieben, Modus „Branch“, Rollen/Mandanten, Rate-Limit/`Retry-After`, falsches Token, nie Approve/Merge, Token nirgends sichtbar | `cargo test -p queued --test pull_requests` |
| Integration WP13 | Zugänge (Verschlüsselung, Zugriff), Kosten je Schritt aus der Preistabelle, Live-Kostenlimit, Datenstandort, Vorlagen, Planung/Review/Analyse per Batch (Anthropic, OpenAI, Mistral) und direkt gegen Fake-Server, Eskalation, Abbruch/Neustart, Schätzung, Kostenübersicht, Admin-Preise, LiteLLM-Schlüssel, CLI-Status, Eval-Set | `cargo test -p queued --test providers` |
| Integration WP06 | Plan-Lauf ändert nichts (Wegwerf-Kopie, Änderungsversuch gemeldet, Reste eines alten Plan-Laufs unsichtbar), Genehmigen ⇒ Umsetzung mit Plan im Prompt, bearbeiteter Plan, Überarbeiten, Verwerfen, automatische Genehmigung (nie bei Fragen), Planung per API zur Freigabe, Nachrichten (Zurückziehen, Zustellung beim Start, in die laufende Sitzung, „Sofort zustellen“, nach Abschluss als Fortsetzung, Abbruch verwirft), „Weiter mit …“ (Sitzung nur für den eigenen Auftrag, Fallback ohne Sitzung), Kommentare (Validierung, eine Nachricht, verschoben/veraltet), Rechte, Längen | `cargo test -p queued --test plan_steering` |
| Integration WP07 | Jede Token-Route mit und ohne ihre Berechtigung (403), Konto/Einstellungen/Token-Verwaltung nie per Token, abgelaufen/widerrufen/Tippfehler (401), Token schlägt Cookie, Mandanten und Rollen; Auslösen per API (Datenblock, „läuft schon“, Limit + `Retry-After`, Rechte); Webhook (falsche Signatur, Zeitfenster, Replay, neue ID mit alter Signatur, 1 MiB, Secret-Wechsel); GitHub (Filter, Ping, Form-Kodierung, Dedupe per Delivery-ID und Body, ein Auftrag pro Commit, `poke` der WP05-PRs); Zeitplan genau einmal bei gesperrter Zeile, Absturz vor Commit, zwei Instanzen, Neustart und veralteter Instanz; verpasste Läufe mit/ohne Nachholen; einmalig; Eigentümer verliert Rechte | `cargo test -p queued --test tokens --test triggers` |
| Integration WP08 | je Anbindung (GitHub, Slack, Linear, Jira): falsche/fehlende Signatur (401, nichts gespeichert), Zeitfenster (Slack ±5 min, Linear 60 s), Replay ⇒ Duplikat, fremder Arbeitsbereich ⇒ 403, nicht zugeordnete Person abgewiesen (Linear bekommt eine `error`-Activity), Auftrag mit Verknüpfung und Datenblock, Rückmeldung genau einmal (auch nach verlorener Antwort, mit zwei parallelen Zustellern, gleiche Rückfrage erneut), Rückfrage über den externen Kanal beantwortet bis „erledigt“, „Weiter mit …“; Zuordnung mit zwei Zustimmungen, Rechte/Mandanten, Opt-in mit GitHub-Schreibrechten, Limit (429), Secrets verschlüsselt und nie in Antworten; `answer` über den Nachrichtenpfad | `cargo test -p queued --test intake` |
| Unit (Frontend) | Formatierung, Status-/Fehlertexte, Budget-Anzeige, Limit-Eingaben, Fortschrittsphasen, Diff, Tastaturkürzel, VAPID-Decoding, Geldbeträge/Schätzung | `cd frontend && cargo test` |
| System | echte Binaries als getrennte Prozesse: Runner-Auth, Session-Pflicht, Flow über die Prozessgrenze, Budget, Cancel | `cd e2e && npx playwright test --project=system` |
| System WP07 | echtes `queuectl` gegen den echten Stack (add über stdin, watch per SSE und Polling, list/status/steer/cancel, fehlende Berechtigung, 0644-Datei abgelehnt, Token aus der Umgebung, Tippfehler, `http://` zu fremdem Host, widerrufen), Webhook mit Node-HMAC, GitHub-Ping, `queuectl fire`, einmaliger Auslöser durch den Scheduler | `npx playwright test tests/system/triggers.spec.ts` |
| System WP08 | signierte Zustellungen (Node-`crypto`) je Anbindung gegen den echten Stack, Rückmeldungen an den Fake (`fake_intake`), Rückfrage und Antwort über den externen Kanal | `npx playwright test tests/system/intake.spec.ts` |
| E2E | Browser gegen echten Stack: Registrierung, Onboarding, Einreihen mit Limits, Live-Status, Details/Diff/Rückgängig/Bearbeiten, Rückfrage, Freigabe, Fix-Kette, Budget-UI, Reorder/Verwerfen/Wiederherstellen, Bestätigungsdialoge, Tastaturkürzel, Live-Region, Theme | `npx playwright test --project=desktop` |
| E2E Mobile | iPhone 15 Pro hoch/quer mit simulierter **Dynamic Island**/Notch/Home-Indikator: nichts verdeckt, Tab-Bar und alle Bedienelemente ≥ 44pt, Inputs ≥ 16px (kein iOS-Zoom), kein horizontales Scrollen | `npx playwright test --project=iphone --project=iphone-landscape` |
| Accessibility | axe-core (WCAG 2.0/2.1/2.2 A+AA) auf allen Hauptansichten, hell und dunkel, Desktop und iPhone; Screenshots nach `WP00_SCREENSHOTS_DIR` | `a11y.spec.ts`, `mobile-a11y.spec.ts` (Teil der Projekte oben) |

Der Mock-Provider (`ENABLE_MOCK_PROVIDER=true`) macht alle Flows deterministisch und kostet keine Tokens. Er wird über Schlüsselwörter im Prompt gesteuert: `MOCK_FAIL`, `MOCK_ASK`, `MOCK_FOLLOWUPS`, `MOCK_LIMIT`, `MOCK_SLOW`, `MOCK_LONG` (300 s), `MOCK_TRANSIENT` (scheitert transient, bis die Re-Queue-Notiz im Prompt steht), `MOCK_TRANSIENT_ALWAYS`, `MOCK_AUTH`, `MOCK_STRUCTURED`, `MOCK_STRUCTURED_BAD`, `MOCK_TOKENS`, `MOCK_TURNS`, `MOCK_TRAP` (wartet auf SIGINT und meldet dann Verbrauch), `MOCK_STDERR429`, `MOCK_REVIEW_LIMIT`, `MOCK_QUIET` (6 s ohne Ausgabe, dann Erfolg), `MOCK_USAGE_TOTAL` (Gesamtverbrauch erst am Ende, weit über den Live-Werten), `MOCK_SOLUTION_B64:<base64>` (führt das Skript im Workspace aus – Eval-Set), `MOCK_ENV` (schreibt die Namen seiner Umgebungsvariablen nach `MOCK_ENV.txt`), `MOCK_NET:<url>` (ruft die URL über den Egress-Proxy ab), `MOCK_FILE:<name>` (schreibt die Lauf-Zeile in `<name>` statt `MOCK_LOG.md`; gleiche Datei in parallelen Aufträgen ⇒ Konflikt), `MOCK_RESOLVE` (entfernt Konfliktmarker, beide Seiten bleiben); WP06: Plan-Modus ohne Schreibzugriff mit `MOCK_PLAN_WRITE` (versucht trotzdem zu schreiben), `MOCK_PLAN_ASK` (Plan mit Frage), `MOCK_PLAN_EMPTY`, `MOCK_PLAN_SLOW`; Sitzungen in `~/.mock-sessions` (unbekannte Sitzung scheitert wie bei Claude), `MOCK_STEER_SLOW`, `MOCK_REWRITE`/`MOCK_PREPEND` (nach einer Antwort ersetzt bzw. ergänzt die Datei oben). Was den Agenten erreicht hat (Tokens `MOCKMSG…`, `MOCKPLAN…`, `MOCKNOTE…`, `MOCKCOMMENT…`), schreibt der Mock nach `SEEN.md`. Für Pull Requests gibt es einen Fake-Server für GitHub, GitLab und Gitea (`tests/common/fake_git_host.rs`; im E2E-Stack als `cargo run --example fake_git_host`); echte Git-Server werden in Tests nie aufgerufen. Für Planung/Review/Batch gibt es in den Integrationstests einen lokalen Fake-Server für die Anthropic-, OpenAI-, Mistral- und LiteLLM-APIs (`tests/common/fake_llm.rs`); echte Anbieter werden in Tests nie aufgerufen. CI (`.github/workflows/ci.yml`) führt fmt, clippy `-D warnings` (Backend sowie Frontend für wasm32 und nativ), alle Rust-Tests, den Trunk-Build sowie Playwright aus.

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

- **Branches (WP04):** Jeder laufende Auftrag belegt eine vollständige Kopie der Projekt-Historie auf der Platte (bewusst ohne Hardlinks/Alternates, siehe „Branch pro Auftrag“). Mehr gleichzeitige Aufträge erzeugen mehr Konflikte (typisch an Registries, Routern, Lockfiles); jeder Konflikt kostet einen Korrektur-Auftrag. Erwartete Dateibereiche pro Auftrag (nur disjunkte parallel) gibt es nicht. Der Agent eines Korrektur-Auftrags muss Konfliktmarker selbst auflösen. Nach einer Freigabe (Schutz) und einem späteren erneuten Lauf desselben Auftrags (z. B. nach einer Rückfrage) prüft das Gate wieder ab dem ersten Ausgangsstand und kann dieselbe Freigabe erneut verlangen.
- **Pull Requests (WP05):** Die API-Formate von GitHub sind aus der offiziellen OpenAPI-Beschreibung, die von GitLab aus deren Dokumentationsquellen und die von Gitea aus dem Swagger-Template 1.22 abgeleitet; getestet ist nur gegen eigene Fake-Server, nicht gegen die echten Dienste. Nicht aus Dokumentation belegt: die Review-Zustände von Gitea (`REQUEST_CHANGES`), Entwürfe bei Gitea über das Titel-Präfix `WIP:`, die Berechtigungswerte `owner` bei Gitea und das Verhalten von Gitea bei Basic-Auth mit Token als Passwort. Konflikte zwischen Pull Request und Ziel-Branch löst Queue nicht (Anzeige + Benachrichtigung). Rechte werden bei jeder Abfrage geprüft; wer nach seinem Kommentar das Schreibrecht verliert, dessen bereits verarbeiteter Kommentar bleibt verarbeitet. Ohne GitHub-Webhook (WP07, siehe „GitHub einrichten“) beträgt die Reaktionszeit bis zu `PR_POLL_SECS`; GitLab und Gitea werden nur abgefragt.
- Das verbleibende Abo-Kontingent liefern die CLIs nicht exakt. Darum gibt es die Kombination aus eigenen Budgets, Erkennung echter Limits und beobachteten Werten.
- Der Review-Agent ist per Prompt read-only, nicht technisch erzwungen. Seine Änderungen bleiben im Arbeitsverzeichnis des Auftrags; übernommen wird genau der geprüfte Commit.
- Codex und Gemini kennen kein Kostenlimit, Codex kein Turn-Limit; ihr Token-Verbrauch kommt erst am Ende eines Laufs. Ein Lauf kann ein Kosten-/Token-Limit daher überschreiten, bevor es greift. Der Auftrag wird dann trotzdem `blocked` (wie beim Live-Abbruch): kein „erledigt“, kein Review, kein Fix, keine Folgeaufträge. Die Änderungen bleiben als Teil-Commit im Diff des Auftrags; „Wiederholen“ startet mit frischem Budget. Gleiches gilt, wenn Claudes Live-Zähler zu wenig meldet.
- Verbrauchswerte aus dem CLI-Stream werden begrenzt (negativ → 0, je Meldung höchstens 10^10 Tokens bzw. 10 000 USD); gemeldete Limit-Resets höchstens 8 Tage in der Zukunft.
- `GEMINI_CLI_TRUST_WORKSPACE=true` bleibt nötig (sonst führt Gemini headless keine Tools aus); Hooks, MCP- und Tool-Befehle sowie die Projekt-`.env` schaltet Queue per System-Einstellungen ab, `.gemini/.env` liest Gemini trotzdem (Freigabe bei Änderungen, siehe „Schutz“).
- **Secret-Scanner (WP03):** eigener Regelsatz, keine Garantie. Unbekannte Token-Formate, kodierte (base64) oder in Binärdateien versteckte Werte und kurze Passwörter mit niedriger Entropie werden nicht erkannt; die Entropie-Regel kann Fehlalarme liefern (dann freigeben). Diffs über 16 MiB werden nicht vollständig geprüft und brauchen eine Freigabe.
- Teil-Commits unterbrochener Läufe werden auf Geheimnisse geprüft, geschützte Dateien und Tests erst am Ende des Auftrags (über alle Läufe ab dem ersten Ausgangsstand). Seit WP04 liegen sie nur auf dem Branch des Auftrags und erreichen den Hauptstand erst nach dieser Prüfung.
- Die Liste der Setup-Dateien ist eine Heuristik: Skripte, die das Setup nur indirekt aufruft (z. B. aus einem `postinstall`), erkennt Queue nur, wenn ihr Pfad im Setup-Skript steht oder sie unter die Manifest-/Lock-Dateien fallen.
- Kleine Turn-Limits vertragen sich schlecht mit dem Ergebnis-Schema: Claude braucht für den structured output selbst zusätzliche Turns (beobachtet: „say hi" = 3 Turns).
- Live gegen echte Provider aufgezeichnet sind nur Teile der Fixtures (Claude: Erfolg, Turn-/Kostenlimit, SIGINT; Codex: Netzwerk-Retries; Gemini: ungültiger API-Key). Rate-Limit-, Überlastungs- und Codex-Auth-Streams sind aus den Schemas der gepinnten Versionen konstruiert.
- Kosten sind Listenpreise aus Recherche-Quellen: Nur die Anthropic-Preise wurden direkt auf der Anbieterseite gelesen, OpenAI/Gemini/Mistral stammen aus Suchauszügen. Long-Context-Aufschläge lassen sich nur prüfen, wo die CLI den Verbrauch pro Anfrage meldet (Claude, opencode); sonst ist der Betrag eine Untergrenze. Abo-Läufe werden zu API-Preisen bewertet.
- Die Batch-, Azure-, Infomaniak- und LiteLLM-Schnittstellen sowie Bedrock/Vertex/opencode im Echtbetrieb sind nur gegen lokale Fake-Server bzw. Strings im Binary geprüft, nicht gegen die echten Dienste.
- **Egress (WP02):** Der Proxy prüft Hostnamen, nicht Inhalte (außer „Nur lesen“). Daten können an jede *erlaubte* Domain abfließen (z. B. github.com), Domain-Fronting über CDNs ist nicht verhindert. Bei `Offen` löst der Runner beliebige Namen auf (DNS als Kanal). Welche Hosts die Agent-CLIs brauchen und dass sie `HTTPS_PROXY` beachten, ist Wissen über die CLIs, **nicht live geprüft**; ein fehlender Host zeigt sich als „Blockiert“ im Protokoll. Images ohne `node` haben im Container kein Netz (sicher, aber 10 s Wartezeit). Container müssen als Runner-UID laufen (Agent-Image: Standard), sonst ist der Socket nicht erreichbar.
- **Geheimnisse im Setup:** Das Setup führt Code aus dem Repository aus (z. B. `preinstall` beim `npm install`), und das Repository wird von früheren Agentenläufen verändert. Seit WP03 fragt Queue vor einem Setup mit Geheimnissen nach, wenn sich Setup-Dateien seit der letzten Freigabe geändert haben (Heuristik, siehe oben). Trotzdem: enge Setup-Allowlist und nur Tokens mit minimalen Rechten. Die Leck-Suche findet nur wörtliche Kopien (nicht base64 o. Ä.) und liest Dateien bis 8 MiB.
- Das Setup läuft bei jedem Auftrag (mit warmem Cache), es gibt keinen Container-Snapshot nach dem Setup.
- **Plan-Modus/Sitzungen (WP06):** Nur Claude Code ist für Plan-Modus und Fortsetzen live geprüft (2.1.322, nicht die gepinnte 2.1.319). Bei Codex, Gemini und opencode stammen Aufruf und Speicherort der Sitzung aus den Programmen bzw. dem Quelltext, ausgeführt wurde nichts; scheitert ein Fortsetzen ohne jeden Verbrauch, startet Queue einen neuen Lauf mit Übergabe, eine still neu begonnene Sitzung bemerkt Queue aber nicht (der Prompt enthält deshalb immer die Aufgabe). Mit Engine `none` (nur Entwicklung) kann ein Plan-Lauf außerhalb seiner Wegwerf-Kopie schreiben; Queue erkennt nur Änderungen in der Kopie. „Sofort zustellen“ unterbricht nur den Agenten selbst, nicht Setup, Verify oder Review. Nachrichten erreichen einen Lauf nicht mitten in einem Schritt, sondern an dessen Ende (oder per Unterbrechung).
- **Automatisierung (WP07):** GitHub signiert keinen Zeitstempel; nach 90 Tagen (Aufbewahrung des Verlaufs) könnte eine abgefangene GitHub-Zustellung erneut wirken. Ungültig signierte Anfragen werden nicht gespeichert und nicht gezählt (jede kostet einen Datenbankzugriff) – eine Drosselung von `/api/hooks/` gehört in den Reverse-Proxy bzw. Tunnel. Webhook-Bodys über 1 MiB lehnt Queue ab (GitHub erlaubt bis 25 MB; sehr große Pushes erscheinen dann als fehlgeschlagene Zustellung bei GitHub). Vorlagen lassen Anbieter/Modell im UI weg (Projekt-/Nutzervorgabe), per API sind sie setzbar. Ein Token gilt für alle Projekte seiner Person (Rollen greifen), es gibt keine projektgebundenen Tokens. `X-Hub-Signature-256` und alle ausgewerteten Felder sind aus GitHubs OpenAPI/Doku belegt, aber nur gegen nachgebaute Zustellungen getestet; der Wert `"Bot"` von `sender.type` ist nicht belegt. Zeitpläne laufen mit Tick-Auflösung (`TRIGGER_TICK_SECS`, Standard 15 s). Ausgehende Webhooks (Benachrichtigung an andere Systeme) gibt es nicht (WP10). Die Tests laufen als root: Die Eigentümer-Prüfung der `queuectl`-Datei ist mit einer fremden UID nicht automatisiert geprüft.
- **Anbindungen (WP08):** Nur gegen nachgebaute Zustellungen und Fake-Server getestet, nie gegen GitHub, Linear, Slack oder Jira selbst. Jira-Signatur und -Felder sind nur über die Sekundärquelle `jira.js` belegt; ob `issue.self` bei allen Jira-Webhooks die Site-Adresse enthält, ist nicht belegt (sonst „Fremder Arbeitsbereich“). Nicht belegt sind außerdem: der Fehlertext von Linear bei einer schon vorhandenen Activity-ID (Queue wertet „already exists“/„duplicate“ als Erfolg), dass bei GitHub nur Personen mit Triage-Rechten Labels setzen können (ein Label zählt beim Opt-in als Schreibrecht). GitHub und Jira signieren keinen Zeitstempel: Replays werden 90 Tage lang erkannt. Eine Slack-Wurzelnachricht (Slash-Befehl) kann bei verlorener Antwort doppelt erscheinen (eine Nachricht ohne Thread lässt sich nicht nachschlagen). Dass ein externes Konto wirklich der Person gehört, prüft Queue nicht – dafür steht die Freigabe der Eigentümerin. Ein Arbeitsbereich, den jemand zuerst mit einem anderen Projekt verbindet, ist für alle anderen belegt; lösen lässt sich das nur durch die Eigentümerin jenes Projekts (oder direkt in der Datenbank). Das Stopp-Signal aus Linear bricht den Auftrag nicht ab (Hinweis im Verlauf). Der WP07-GitHub-Auslöser und eine GitHub-Anbindung auf demselben Repository reagieren beide auf dasselbe Issue, wenn beide passen.
