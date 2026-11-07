# Arbeitspakete: Wettbewerbsfunktionen

Quelle: `reports/Funktionsumfang agentischer Coding Queues.md` (Gap-Analyse + Roadmap).
**Ausgeklammert (kommt später):** Lizenz-/ToS-Thema und das Credential-Modell (wie Abo-Tokens gespeichert/übergeben werden). Bestehende Credential-Mechanik bleibt unverändert.

Jedes Paket durchläuft vier Agenten mit frischem Kontext: **Plan → Umsetzung → Test → Review**.
Artefakte pro Paket in `docs/roadmap/`: `WPxx-plan.md`, `WPxx-impl.md` (Umsetzungsnotizen), `WPxx-test.md` (Testbericht), `WPxx-review.md` (Review-Urteil).

## Gemeinsame Regeln für alle Agenten

- Haltung: unfreundlicher, extrem kritischer Senior-Entwickler. Keine Schönfärberei, keine Abkürzungen, keine TODOs/Stubs/"später", keine toten Feature-Flags, keine Behauptung ohne Beleg (Testlauf, Befehlsausgabe). Annahmen anderer Agenten werden geprüft, nicht übernommen.
- Repo: `/home/user/QueueRepo`, Branch `mvp`. **Nicht committen, nicht pushen** – das macht der Koordinator.
- Architektur nicht aufweichen: `queued` (App) hat keine Host-Rechte; Datei-/Prozess-/Git-Arbeit gehört in `queued-runner`; Protokoll in `queued-common`. Neue DB-Änderungen nur als neue Migration (`backend/crates/app/migrations/00NN_*.sql`), bestehende Migrationen nie ändern.
- Qualität: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` sauber; Frontend `npm run build` (tsc) sauber.
- Tests laufen lassen (Postgres: `service postgresql start`, `DATABASE_URL=postgres://queued:queued@localhost/queued`):
  - `cd backend && DATABASE_URL=… cargo test --workspace`
  - `cd frontend && npm run build && npm test`
  - `cd e2e && npx playwright test` (startet den kompletten Stack selbst; Chromium unter `/opt/pw-browsers`)
- Externe Dienste (GitHub/GitLab/Linear/Slack/Jira-APIs, echte Agent-CLIs) sind hier nicht live erreichbar: gegen lokale Fake-Server im Test (axum/wiremock-artig, eigene Implementierung im Test) testen und das im Bericht klar sagen. Der Mock-Provider (`backend/crates/app/src/agent.rs`, `MOCK_*`-Keywords) darf für neue Szenarien erweitert werden.
- Docker-Daemon ist ggf. startbar (`dockerd &`), Podman fehlt; Sandbox-Engine in Tests ist `none`.
- Frontend: bis WP00 abgeschlossen ist React (`frontend/`), danach Rust/Leptos – UI-Arbeit immer im aktuellen Frontend.
- UI ist deutsch; mobile Safe-Area-Regeln (CSS-Variablen `--sat/--sab/...`, Tab-Bar unten) einhalten.

## WP00 – GUI auf Rust umstellen (Reihenfolge: direkt nach WP01, vor allen anderen Paketen; danach WP13, dann WP02 ff.)
Nutzerwunsch: das komplette Frontend in Rust statt React/TypeScript.
- Framework: **Leptos (CSR, WASM, Build mit Trunk)** – bleibt Web-App/PWA (iPhone, Web Push, Safe Areas), E2E-Tests bleiben gültig. Abweichung (z. B. Dioxus) nur mit harter Begründung im Plan.
- Volle Funktionsparität mit dem bisherigen React-UI (Login/Registrierung/OIDC-Buttons, Queue mit allen Sektionen, Composer inkl. Optionen, Task-Drawer mit Live-Log/Diff/Revert/Edit, Inbox, Projekte inkl. Git/Dateien/Mitglieder/Einstellungen, Einstellungen inkl. Budgets/Credentials/Push/Limits, Toasts, SSE-Live-Updates, Limit-Chips, alles was WP01 ergänzt hat). Safe-Area-Variablen, Tab-Bar unten mobil und 16px-Inputs auf Touch bleiben Pflicht.
- **Redesign (Nutzerwunsch): benutzerfreundlich und optisch ansprechend für normale Menschen, nicht nur Entwickler; modernes Design erwünscht.** Konkret:
  - Klare visuelle Hierarchie, ein konsistentes Design-System (Tokens für Farben/Abstände/Radien/Schatten/Typografie, Light/Dark), moderne, ruhige Optik (z. B. weiche Karten, gute Typografie, dezente Animationen mit `prefers-reduced-motion`), Icons (inline SVG, kein CDN).
  - Verständliche Sprache statt Jargon (z. B. Status als "Läuft", "Wartet auf dich", "Fertig" mit Erklärung; Fachbegriffe wie Handoff, Verify, Provider nur mit kurzer Hilfe/Tooltip), hilfreiche Leerzustände, Onboarding beim ersten Start (Projekt anlegen → Provider verbinden → ersten Auftrag schreiben), Fortschrittsanzeige für laufende Aufträge statt roher Logs (Log aufklappbar).
  - Einfache Bedienung: wichtigste Aktion immer prominent ("Neuer Auftrag"), selten genutzte Optionen hinter "Erweitert", Bestätigungen für destruktive Aktionen, Rückgängig wo möglich, Tastaturkürzel für Power-User, gut erreichbare Touch-Ziele (≥44pt).
  - Barrierefreiheit: WCAG 2.2 AA (Kontrast, Fokus sichtbar, Labels/ARIA, Screenreader-taugliche Live-Region für Statuswechsel).
  - Beleg: Screenshots Desktop/iPhone (hell/dunkel) im Bericht; E2E-Tests dürfen an neues Markup angepasst werden, ihre fachlichen Prüfungen (Abläufe, Safe Areas, Touch-Größen, kein horizontales Scrollen) bleiben mindestens gleich streng und kommen um Accessibility-Checks (z. B. axe-core) ergänzt.
- API-Typen nach Möglichkeit mit dem Backend teilen (z. B. DTOs in `queued-common`), keine Typ-Duplikate per Hand, wo das sauber geht.
- PWA: Manifest, Icons, Service Worker (`sw.js` bleibt JS – Browser-Vorgabe), Push-Subscription.
- Vitest-Tests durch Rust-Tests ersetzen (reine Logik als normale `cargo test`, DOM-nahe Logik ggf. `wasm-bindgen-test`); **alle Playwright-System- und E2E-Tests (inkl. iPhone hoch/quer) müssen ohne inhaltliche Abschwächung grün sein** – Selektoren nur anpassen, wo das Markup sich zwingend ändert.
- React/TS-Frontend vollständig entfernen; Build in `e2e/scripts/stack.sh`, `deploy/install.sh`, `Dockerfile`, CI (`.github/workflows/ci.yml`) und README auf Trunk umstellen. `trunk` (0.22.1) und Target `wasm32-unknown-unknown` sind hier installiert; `wasm-bindgen-cli` ggf. per `cargo install` in exakt der zur Crate passenden Version (Trunk-Download von GitHub kann blockiert sein).
- Nach WP00 gilt für alle Pakete: UI-Arbeit erfolgt im Rust-Frontend.

## WP13 – Provider-Anbindungen & Kostenoptimierung (Reihenfolge: direkt nach WP00, vor WP02)
Quelle: `reports/Kostenoptimale Modellanbieter für Queue.md`. Nutzer: keine zeitkritischen Aufgaben, Kosten minimieren, User soll Anbieter/Modell wählen können. Lizenz-/ToS-Thema bleibt ausgeklammert.
- **Eigene Kostenrechnung:** versionierte Preistabelle (Input, Output, Cache-Write 5 min/1 h, Cache-Read, Batch/Flex-Rabatt, Long-Context-Aufschläge wie Haiku >100K und OpenAI >272K, Regionsaufschläge, gültig-ab-Datum für angekündigte Preisänderungen) im Backend; Budgets/Analytics rechnen aus Token-Zählern, nicht aus CLI-Dollarwerten. Preistabelle per Admin/Config überschreibbar.
- **Connectoren (wählbar pro User/Projekt/Task):**
  - Claude Code mit API-Key, mit Bedrock-EU und Vertex-EU (Region wählbar) und mit eigener Base-URL (Gateway).
  - Codex CLI mit API-Key inkl. **OpenAI Flex** (Service-Tier) für den Agent-Loop; Custom Provider (Responses API).
  - **opencode** als Multi-Provider-Harness (Mistral inkl. EU-Endpoint, OpenAI-kompatible Endpoints, Infomaniak/Self-Hosted vLLM) mit Headless-/JSON-Events.
  - Gemini: Status der Gemini CLI prüfen (Ablösung durch Antigravity laut Sekundärquellen); Connector nur weiterführen, wenn lauffähig, sonst sauber als nicht verfügbar markieren.
- **Gateway:** optional selbst gehostetes LiteLLM als OpenAI-/Anthropic-kompatibler Endpunkt mit Budget-Key pro Task; Container erreichen dann nur das Gateway (Anknüpfung an WP02-Egress).
- **Modell-Routing pro Pipeline-Schritt** (vorgezogen aus WP12): Schritte Plan, Agent-Loop, Review, OnEmptyQueue-Analyse je mit eigenem Provider/Modell; Presets ("Günstig": Haiku-first mit Eskalation Haiku→Sonnet→Opus; "Ausgewogen": Opus plant/Sonnet führt aus; "EU": Bedrock/Vertex-EU bzw. Mistral EU; "Maximal günstig": OpenAI Terra Flex); Eskalation bei Fehlschlag. Review nie mit dem billigsten Modell.
- **Batch-Connector** für Einzelschritte (Plan, OnEmptyQueue-Analyse, optional Review) über Anthropic Message Batches bzw. OpenAI Batch – asynchron mit Polling, Scheduler kann darauf warten (keine Zeitkritik); Cache-Hygiene (stabile Prompt-Präfixe, 1-h-Cache für Batch).
- **UI:** Anbieter-/Modellwahl mit geschätzten Kosten pro Task (aus Preistabelle + historischen Token-Profilen), Preset-Auswahl, Hinweis auf Datenstandort (EU/CH/US).
- Externe APIs sind hier nicht live erreichbar: gegen lokale Fake-Server testen; Fixtures für Batch-/Flex-/opencode-Events mit Herkunftsangabe.
- **Vollständigkeit (Nutzerwunsch):** alle im Kostenbericht beschriebenen Anbindungen bauen, nicht nur eine Auswahl: Anthropic API direkt, Bedrock (EU-Profile), Vertex (EU-Multi-Region), OpenAI API inkl. Flex und Batch, Azure OpenAI/Foundry (EU Data Zone / Switzerland North), Mistral (inkl. EU-Endpoint), Infomaniak (CH) und beliebige OpenAI-kompatible Endpoints (selbst gehostetes vLLM/Ollama), OpenRouter, LiteLLM-Gateway, opencode-Harness; dazu ein kleines Eval-Set (repräsentative Tasks gegen den Mock + optional echte Provider), um Erfolgsquoten pro Modell/Route zu messen und die Routing-Presets zu kalibrieren.

## WP01 – Strukturierte CLI-Integration & Fehlerklassifikation
Wettbewerb nutzt strukturierte Signale statt Text-Parsing.
- Claude: `--json-schema` für den Result-Contract (statt nur `.queue/result.json`, Datei als Fallback), `--max-budget-usd`/Turn-Limits pro Task, `system/api_retry`-Events (rate_limit/overloaded/billing_error) auswerten, `--permission-prompts none`-Äquivalent, sauberer Abbruch (SIGINT vor SIGKILL). Codex/Gemini: entsprechende Optionen soweit vorhanden, sonst sauber dokumentierter Fallback.
- Fehlerklassen: Infra-/Provider-/Rate-Limit-Fehler werden re-queued **ohne** Fix-Versuch zu verbrauchen; nur echte Task-Fehler erzeugen Fix-Tasks. Retry mit Backoff für transiente Fehler.
- Per-Task-Limits: max. Laufzeit, max. Kosten/Tokens (Projekt- und Task-Ebene), Stuck-Erkennung (keine Ausgabe für N Minuten).
- CLI-Versionen pinnen (Agent-Image) und Contract-Tests für die Parser mit realistischen Fixtures.

## WP02 – Environments, Setup-Phase & Egress-Kontrolle
Codex/Claude/Copilot: Default-Deny-Netz mit Allowlist, Setup-Skript mit Cache, Secrets nur in der Setup-Phase.
- Pro Projekt "Environment": Setup-Skript, Env-Vars (nicht geheim), Secrets (verschlüsselt, nur Setup-Phase), Netzwerk-Policy (`off` | `allowlist` | `open`), Domain-Allowlist mit Presets (Paket-Registries), optional nur GET/HEAD/OPTIONS.
- Zwei Phasen pro Task im Runner: Setup (mit Netz gemäß Setup-Policy, mit Secrets) → Agent-Phase (Secrets entfernt, Netz gemäß Agent-Policy).
- Egress-Durchsetzung im Runner: pro Lauf ein Allowlist-HTTP(S)-CONNECT-Proxy; Container ohne direkten Netzzugang (z. B. `--network none` + Unix-Socket-Bridge in den Container, oder internes Netz nur zum Proxy). Link-local/Metadata-IPs und private Netze blockiert. Proxy-Log als Task-Events.
- Dependency-/Setup-Cache (z. B. Cache-Volume pro Projekt, Invalidierung bei geändertem Setup-Skript/Lockfiles).
- UI: Environment-Editor im Projekt.

## WP03 – Gate- und Supply-Chain-Härtung
- Secret-Scanning von Diffs und Event-Logs vor Commit/Push (eigene Regeln für gängige Token-Formate + Entropie); Treffer blockieren Commit/Push und erzeugen Rückfrage.
- Untrusted-Repo-Konfiguration: Projekt-Hooks/MCP-Server aus dem Repo (`.claude/settings.json`, `.mcp.json`, Codex/Gemini-Äquivalente) standardmäßig nicht ausführen (z. B. `--bare`/Settings-Override), pro Projekt explizit freischaltbar.
- Anti-Reward-Hacking: geschützte Pfade (Tests/Verify-Skripte read-only bzw. Änderungen an geschützten Pfaden erzwingen Freigabe), Erkennung gelöschter/übersprungener Tests (`skip`, `xfail`, `#[ignore]`, `.only`), Verify läuft auf Basis-Version der geschützten Dateien; Review-Agent bekommt Rubrik und bevorzugt anderen Provider.
- Auto-Push nie direkt auf den Default-Branch ohne Freigabe.

## WP04 – Branch/Worktree pro Task, Parallelität im Repo, Merge-Queue
Standard bei Konkurrenz: Branch pro Task, mehrere Agenten parallel pro Repo.
- Runner: Git-Worktree pro Task (`queue/<task-kurz-id>`), Agent arbeitet isoliert.
- Parallelität pro Projekt konfigurierbar (Default 1), Scheduler berücksichtigt das.
- Serielle Merge-Queue pro Projekt: Rebase auf Ziel-Branch → Verify erneut → Fast-Forward-Merge; Konflikt → Fix-Task mit Konfliktkontext; Branch-Aufräumen.
- Diff/Revert/Log weiter korrekt (Diff gegen Merge-Base).

## WP05 – PR-Workflow & Feedback-Loop
Copilot/Claude/Codex/Jules: Output als PR, Iteration über PR-Kommentare und CI.
- Git-Host-Adapter: GitHub, GitLab, Gitea/Forgejo (REST): PR/MR erstellen (Draft optional), Titel/Body aus Task, Labels.
- Auto-Fix: CI-Status (Checks/Pipelines) und Review-Kommentare pollen (Webhooks zusätzlich, falls in WP07 vorhanden – sonst Polling) → Folge-Task auf demselben Branch; nur Kommentare von Nutzern mit Schreibrechten; Agent approved/merged nie selbst.
- Projekt-Einstellung: Output-Modus `commit` | `branch` | `pr`.
- UI: PR-Link, Status, Checks.

## WP06 – Plan-Modus, Steering & Inline-Kommentare
- Plan-Modus pro Task/Projekt: Agent erstellt nur Plan (keine Änderungen) → Nutzer genehmigt/ändert/verwirft (optional Auto-Approve nach Timeout) → Umsetzung mit Plan als Kontext.
- Steering: Folge-Nachrichten an laufende Tasks (in Queue gestellt, vor Zustellung zurückziehbar), zugestellt via Session-Resume (`--resume`/`--continue`) bzw. nach Lauf als Fortsetzung derselben Session; auch für erledigte Tasks ("Weiter mit…").
- Inline-Kommentare im Diff (Zeile/Datei) → gesammelt als Follow-up an den Agenten.

## WP07 – Trigger, API & CLI
- Zeitpläne pro Projekt (Cron, min. stündlich; einmalige Ausführung zu Zeitpunkt), Task-Vorlagen.
- Persönliche API-Tokens (gehasht, scopes), REST-API für Tasks; `POST /api/triggers/{id}/fire` mit Token.
- Generische eingehende Webhooks (HMAC-signiert) → Task aus Vorlage mit Payload-Kontext; GitHub-Events (push/issue/PR/check failure) mit Filtern.
- Kleines CLI (`queuectl`, Rust-Binary im Workspace): add/list/status/watch/cancel.

## WP08 – Intake-Integrationen
- GitHub: Issues mit Label/Mention → Task, Status-Kommentare zurück.
- Linear: Agent-API-Muster (Issue delegieren → Task, Activity-Updates zurück).
- Slack: App-Mention/Slash-Command → Task, Thread-Updates, Rückfragen im Thread beantworten.
- Jira: Issue mit Label → Task, Kommentar zurück.
- Gemeinsame Abstraktion "Intake-Quelle" mit Verknüpfung Task ↔ externes Objekt; Signaturprüfung aller Webhooks.

## WP09 – Memory, Wissen, Dedupe & Anhänge
- Projekt-Regeln/Wissen: verwaltete `AGENTS.md` (+ CLAUDE.md/GEMINI.md-Brücke), UI-Editor, Agent darf Vorschläge machen (Freigabe).
- Strukturierter Projektzustand statt Freitext-Handoff: JSON-Feature-/Aufgabenliste mit Status, wird jedem Agenten mitgegeben und vom Runner gegen Repo-Zustand validiert.
- OnEmptyQueue: Dedupe gegen offene/erledigte Tasks (Ähnlichkeit) + Critic-Schritt vor Vorschlag.
- Anhänge an Tasks (Bilder/Screenshots/Dateien, Upload im Composer, mobil per Kamera), im Workspace unter `.queue/attachments/` bereitgestellt.

## WP10 – Observability, Analytics & Benachrichtigungen
- `/healthz`, `/readyz` (App: DB + Runner erreichbar), Prometheus `/metrics` (App + Runner), OpenTelemetry-Traces (OTLP, optional per Env) nach GenAI-Konventionen (`invoke_agent`, Tools).
- Analytics-Dashboard: Erfolgsquote first-pass, Kosten/Tokens pro Task/Projekt/Provider, Cache-Hit-Ratio, Dauer bis erledigt, Pausenzeiten durch Limits; Session-Insights pro Task.
- Benachrichtigungen: pro Ereignistyp und Kanal konfigurierbar, Ruhezeiten, tägliche Zusammenfassung.

## WP11 – Enterprise & Betrieb
- MFA: TOTP + WebAuthn/Passkeys, Recovery-Codes; Session-Verwaltung (aktive Sessions, Widerruf).
- Append-only Audit-Log (Auth, Credentials, Projekt-/Mitglieder-Änderungen, Pushes, Freigaben) mit UI für Admins.
- Admin-Konsole: Nutzer (sperren, Rollen), Einladungen, Registrierungsmodus, Quotas pro Nutzer.
- Backups/Restore: `pg_dump` + Workspace-Bundles (git bundle), Kommando + systemd-Timer, Restore-Test.
- Runner-Flotte: mehrere Runner-Hosts mit Registrierung, Lease/Heartbeat, Kapazität, Drain; Scheduler verteilt Projekte auf Runner.

## WP12 – Best-of-N, Preview & Modell-Routing
- Best-of-N: ein Task als N parallele Versuche (gleicher oder verschiedene Provider), Vergleichsansicht der Diffs/Ergebnisse, Auswahl → Merge; optional automatische Auswahl per Review-Agent.
- Preview: Dev-Server im Sandbox-Container starten (Projekt-Einstellung), Screenshots per Headless-Browser nach UI-Änderungen, im Task anzeigen.
- Modell-Routing: Regeln pro Projekt (z. B. Plan/Review mit anderem Modell als Umsetzung, günstiges Modell für kleine Tasks).

## WP15 – Intelligenter Modell-Chooser (Reihenfolge: nach WP12, vor WP14)
Nutzerwunsch: ein "richtig schlauer" Chooser, der jede Aufgabe dem passenden Modell gibt. Baut auf WP13 (Connectoren, Preistabelle, Routing-Presets, Eval-Set) und WP10 (Analytics) auf.
- **Task-Analyse:** Merkmale aus Prompt, Anhängen und Repo (Aufgabentyp wie Bugfix/Feature/Refactor/Doku/Tests/Recherche/UI, geschätzte Grösse, Komplexität, Risiko z. B. Security/Migration/Infra, benötigte Fähigkeiten wie Vision, Long-Context, Web-Recherche, Sprache/Framework, betroffene Dateien/Zeilen). Heuristik + günstige Klassifikation (billiges Modell oder lokale Embeddings), Kosten des Choosers selbst werden gezählt und bleiben vernachlässigbar.
- **Entscheidung:** Kandidaten = verfügbare Connectoren/Modelle des Users, gefiltert nach harten Constraints (Budget/Limits/Pausen, Datenstandort EU/CH, Nutzer- und Projektregeln, Fähigkeiten, Kontextlänge). Bewertung nach erwarteten Kosten pro *akzeptiertem* Task = Kosten pro Versuch / Erfolgswahrscheinlichkeit, inkl. Eskalationskette; Qualität-vs-Kosten-Regler pro Projekt.
- **Lernen aus Ergebnissen:** Erfolgswahrscheinlichkeiten pro (Aufgabenkategorie × Modell) aus Eval-Set und echten Läufen (first-pass Verify, Review-Ergebnis, Fix-Tasks, Kosten, Dauer) – kontextueller Bandit (z. B. Thompson Sampling mit Priors aus dem Eval-Set) mit kontrollierter Exploration, Zerfall alter Daten, Mindeststichprobe.
- **Schritt-spezifisch:** eigene Wahl für Plan, Agent-Loop, Review (nie schwächer als der Ausführer, bevorzugt anderer Provider gegen Self-Preference-Bias) und OnEmptyQueue-Analyse; Batch/Flex automatisch, wenn kein Zeitdruck.
- **Transparenz & Kontrolle:** Begründung pro Task in der UI ("Gewählt: X, weil …, erwartete Kosten …, Alternativen …"), manuelles Override, Feedback des Users fliesst ins Lernen ein; Admin-Sicht auf Chooser-Statistiken.
- **Absicherung:** Offline-Evaluation gegen historische Läufe (Replay), Vergleich mit statischen Presets (muss Kosten pro akzeptiertem Task nachweislich senken oder Qualität halten), Fallback auf Preset bei fehlenden Daten, deterministische Tests der Entscheidungslogik.

## WP14 – Kommerzielles Angebot: Zugang gegen Bezahlung (Reihenfolge: ganz am Schluss, nach WP15)
Nutzerwunsch: das Produkt für Kunden zugänglich machen, die dafür bezahlen.
- Hinweis: Das ausgeklammerte Lizenz-/ToS-Thema wird hier zwingend relevant (Weiterverkauf/Vermittlung von Modellnutzung). Das Abrechnungsmodell so bauen, dass es konfigurierbar ist (z. B. Plattformgebühr mit eigenem API-Key des Kunden vs. nutzungsbasierte Abrechnung über eigene Provider-Verträge); die Entscheidung trifft der Betreiber vor dem Go-live.
- Pläne/Tarife (z. B. Free/Trial, Pro, Team) mit Limits (Tasks, Projekte, parallele Agenten, Token-Kontingent), Feature-Gates, Upgrade/Downgrade, Testphase.
- Zahlungsanbieter: Stripe (Checkout, Customer Portal, Subscriptions, Usage-based Metering, Webhooks mit Signaturprüfung, Idempotenz) als primärer Adapter; Schnittstelle so, dass weitere Anbieter (z. B. Payrexx/TWINT für CH) ergänzbar sind.
- Nutzungsmessung: Abrechnung aus der eigenen Kostenrechnung (WP13) mit Marge/Preisliste pro Plan, Kontingent-Warnungen, harte Limits, Überziehung optional.
- Rechnungen/Steuern: CH-MWST (8.1 %) und EU-USt (Reverse Charge, OSS-Hinweise) über Stripe Tax o. ä., Rechnungsadresse/UID, Rechnungsarchiv.
- Mahnwesen (fehlgeschlagene Zahlungen → Gnadenfrist → Sperre), Kündigung, Datenexport und Löschung (DSG/GDPR).
- Kunden-Onboarding: Landingpage mit Preisseite, Registrierung → Plan wählen → bezahlen → Onboarding; Admin-Sicht auf Kunden, Umsatz, MRR, Churn.
- Rechtstexte als Vorlagen mit klarer Markierung "vom Betreiber zu prüfen": AGB, Datenschutzerklärung, AVV/DPA, Impressum.
- Tests gegen Stripe-Testmodus-Fixtures/Fake-Server (Webhooks signiert), E2E des Kaufflows.

