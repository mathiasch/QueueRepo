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

## WP00 – GUI auf Rust umstellen (Reihenfolge: direkt nach WP01, vor allen anderen Paketen)
Nutzerwunsch: das komplette Frontend in Rust statt React/TypeScript.
- Framework: **Leptos (CSR, WASM, Build mit Trunk)** – bleibt Web-App/PWA (iPhone, Web Push, Safe Areas), E2E-Tests bleiben gültig. Abweichung (z. B. Dioxus) nur mit harter Begründung im Plan.
- Volle Funktionsparität mit dem bisherigen React-UI (Login/Registrierung/OIDC-Buttons, Queue mit allen Sektionen, Composer inkl. Optionen, Task-Drawer mit Live-Log/Diff/Revert/Edit, Inbox, Projekte inkl. Git/Dateien/Mitglieder/Einstellungen, Einstellungen inkl. Budgets/Credentials/Push, Toasts, SSE-Live-Updates, Limit-Chips) und gleiche UX/Design (bestehendes CSS weiterverwenden, Safe-Area-Variablen, Tab-Bar unten mobil, 16px-Inputs auf Touch).
- API-Typen nach Möglichkeit mit dem Backend teilen (z. B. DTOs in `queued-common`), keine Typ-Duplikate per Hand, wo das sauber geht.
- PWA: Manifest, Icons, Service Worker (`sw.js` bleibt JS – Browser-Vorgabe), Push-Subscription.
- Vitest-Tests durch Rust-Tests ersetzen (reine Logik als normale `cargo test`, DOM-nahe Logik ggf. `wasm-bindgen-test`); **alle Playwright-System- und E2E-Tests (inkl. iPhone hoch/quer) müssen ohne inhaltliche Abschwächung grün sein** – Selektoren nur anpassen, wo das Markup sich zwingend ändert.
- React/TS-Frontend vollständig entfernen; Build in `e2e/scripts/stack.sh`, `deploy/install.sh`, `Dockerfile`, CI (`.github/workflows/ci.yml`) und README auf Trunk umstellen. `trunk` (0.22.1) und Target `wasm32-unknown-unknown` sind hier installiert; `wasm-bindgen-cli` ggf. per `cargo install` in exakt der zur Crate passenden Version (Trunk-Download von GitHub kann blockiert sein).
- Nach WP00 gilt für alle Pakete: UI-Arbeit erfolgt im Rust-Frontend.

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
