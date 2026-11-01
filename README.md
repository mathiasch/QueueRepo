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
| Frontend | `frontend/` | React/Vite-PWA, Safe-Area-fähig (Notch/Dynamic Island), iOS-Tab-Bar |
| Tests | `backend/**/tests`, `frontend/src/*.test.ts`, `e2e/` | siehe unten |

**Isolation in der Praxis verifiziert:** Ein Agent-Container läuft mit der UID des Runner-Users, `CapEff: 0`, `NoNewPrivs: 1`. Secrets kommen nur per Env in den Container, nie in argv. Der Runner verweigert den Start als Root und mit schwachem Token.

## Ablauf eines Auftrags

```
queued ─► running ─► git pull (falls Remote) ─► frischer Agent (Auftrag + Handoff/Kontext + Projektziel)
                       │ schreibt .queue/result.json (git-ignored)
                       ├─ needs_input ─► waiting_input ─(Antwort)─► queued
                       ├─ Abo-Limit   ─► Provider pausiert bis Reset, Auftrag zurück in die Queue
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

## Tests

| Ebene | Was | Ausführen |
|---|---|---|
| Unit (Rust) | Git-Ops inkl. Clone/Push/Pull, atomarer Revert, Path-Traversal, Exec (Streaming, Timeout, Cancel, Prozessgruppe), Container-Flags, Parser aller Provider, Limit-Erkennung, Budget-Logik, Prompts, Crypto, OIDC/PKCE/Apple-JWT | `cd backend && cargo test --workspace --lib` |
| Integration (Rust) | App + Runner in-process + echtes Postgres (frische DB pro Test): Auth, Tenant-Isolation, Rollen, kompletter Task-Lifecycle mit Git, Fix-Loop, Rückfragen, Freigaben, Abo-Limit, Budgets, Cancel, Revert, Credentials, OnEmptyQueue, Clone + Auto-Push, Parallelität, SSE-Isolation, ZIP | `DATABASE_URL=postgres://… cargo test --workspace` |
| Unit (Frontend) | Formatierung, Budget-Anzeige, VAPID-Decoding | `cd frontend && npm test` |
| System | echte Binaries als getrennte Prozesse: Runner-Auth, Session-Pflicht, Flow über die Prozessgrenze, Budget, Cancel | `cd e2e && npx playwright test --project=system` |
| E2E | Browser gegen echten Stack: Registrierung, Einreihen, Live-Status, Drawer/Diff/Git-Log, Rückfrage, Freigabe, Fix-Kette, Budget-UI, Reorder | `npx playwright test --project=desktop` |
| E2E Mobile | iPhone 15 Pro hoch/quer mit simulierter **Dynamic Island**/Notch/Home-Indikator: nichts verdeckt, Tab-Bar ≥ 44pt, Inputs ≥ 16px (kein iOS-Zoom), kein horizontales Scrollen | `npx playwright test --project=iphone --project=iphone-landscape` |

Der Mock-Provider (`ENABLE_MOCK_PROVIDER=true`) macht alle Flows deterministisch und kostet keine Tokens. Er wird über Schlüsselwörter im Prompt gesteuert: `MOCK_FAIL`, `MOCK_ASK`, `MOCK_FOLLOWUPS`, `MOCK_LIMIT`, `MOCK_SLOW`. CI (`.github/workflows/ci.yml`) führt fmt, clippy `-D warnings`, alle Rust-Tests, Frontend-Build und -Tests sowie Playwright aus.

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
| Gemini CLI | Google-Login → Inhalt von `~/.gemini/oauth_creds.json` | Gemini API-Key |

Die Zugänge werden mit AES-256-GCM verschlüsselt (`MASTER_KEY`). Rotierte OAuth-Tokens schreibt das System automatisch zurück. Git-Tokens werden ebenfalls verschlüsselt gespeichert und nur als HTTP-Header an Git übergeben, nie in Remote-URL oder Config.

### Entwicklung

```bash
cd backend
RUNNER_ALLOW_ROOT=1 RUNNER_ENGINE=none RUNNER_TOKEN=dev-token-dev-token-dev-token cargo run --bin queued-runner &
DATABASE_URL=postgres://… RUNNER_TOKEN=dev-token-dev-token-dev-token ENABLE_MOCK_PROVIDER=true cargo run --bin queued
cd frontend && npm install && npm run dev      # http://localhost:5173
```

## Bekannte Grenzen

- Innerhalb eines Projekts läuft immer nur ein Agent (gemeinsamer Workspace). Die Parallelität gilt projektübergreifend.
- Das verbleibende Abo-Kontingent liefern die CLIs nicht exakt. Darum gibt es die Kombination aus eigenen Budgets, Erkennung echter Limits und beobachteten Werten.
- Der Review-Agent ist per Prompt read-only, nicht technisch erzwungen. Seine Änderungen würden als WIP-Commit sichtbar.
- Die Agent-CLIs selbst wurden mangels Zugangsdaten nicht live getestet; abgesichert sind die Parser über Unit-Tests mit den dokumentierten Stream-Formaten.
