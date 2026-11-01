# Queue – agentische Coding-Queue

Aufträge per Web-UI einreihen. Pro Auftrag arbeitet ein **frischer Agent** (Claude Code, Codex CLI oder Gemini CLI) ihn in einer Sandbox ab. Danach wird verifiziert und reviewt, und die Queue geht zum nächsten Auftrag. Ist sie leer, laufen **OnEmptyQueue**-Aktionen (Review, Recherche, Verbesserungen), die die Queue wieder befüllen können.

- **Multi-User/Multi-Tenant:** Jeder User hat seine eigene Queue und sein eigenes Abo. Projekte können geteilt werden (Owner/Editor/Viewer).
- **Login:** lokale Accounts (Argon2) oder OIDC/OAuth2 (Google, Apple, GitHub, beliebige OIDC-IdPs wie Keycloak, Authentik oder Entra).
- **Abo-Limits ausreizen:** Das 5h-Fenster wird pro Provider getrackt. Bei einem Limit pausiert die Queue für diesen Provider bis zum Reset und läuft danach automatisch weiter. OnEmptyQueue nutzt Kontingent, das sonst verfallen würde.
- **Kein Git für den User:** Workspaces liegen im Backend. Ein unsichtbares Snapshot-Repo liefert Diff, Rückgängig und ZIP-Export pro Auftrag.
- **Notifications:** In-App (SSE), Web Push (PWA, auch iOS) und ntfy.

## Ablauf eines Auftrags

```
queued ──► running ──► Agent (frischer Kontext: Auftrag + Handoff/Kontext + Projektziel)
                          │ schreibt .queue/result.json
                          ├─ needs_input ──► waiting_input ──(Antwort)──► queued
                          ├─ Usage-Limit ──► Provider pausiert bis Reset, Auftrag zurück in die Queue
                          ▼
                     Verifikation (verify_command, z. B. `npm test`)
                          ▼
                     Review-Agent (frischer Kontext, sieht nur Auftrag + Diff)
                          ├─ ok ──► done ──► Folgeaufträge (≤ 2 automatisch, > 2 → Freigabe + Push)
                          └─ fail ─► failed ──► Fix-Auftrag (Root-Cause-Analyse) vorne in der Queue
                                               nach 2 Fix-Versuchen ─► blocked + Push
Queue leer ──► OnEmptyQueue (Review / Recherche / Verbesserungen) ──► schlägt Aufträge vor
```

**Definition of Done** = Agent meldet `done`, `verify_command` liefert Exit 0 und der Review-Agent approved. Alle drei Bedingungen sind pro Projekt konfigurierbar.

## Token-Effizienz

| Maßnahme | Wirkung |
|---|---|
| Frischer Agent pro Auftrag | kein Kontext-Bloat über Aufträge hinweg |
| Kontext nur über **Handoff** (max. 15 Zeilen, hart auf 4 KB gekappt) oder explizit mitgegebenen Kontext | Der Nachfolger bekommt Entscheidungen und Stand, keinen Chatverlauf |
| Stabiler Prompt-Aufbau (feste Regeln/Ergebnis-Vertrag) | Provider-Prompt-Caching greift |
| Review sieht nur Auftrag + Diff (Diff > 60 KB → nur `--stat`), optional mit günstigerem Modell | Review kostet einen Bruchteil des Auftrags |
| OnEmptyQueue nur bei leerer Queue, ohne offene Rückfragen, mit freiem Limit und Cooldown (Default: 1× pro 5h-Fenster) | nutzt Restkontingent statt das Budget zu verbrennen |
| Loop-Guards: max. Fix-Versuche, Freigabe ab > 2 neuen Aufträgen, Freigabe ab Generationstiefe > 3 | keine endlosen Agent-Kaskaden |

## Architektur

```
Browser/PWA (React+Vite) ──HTTPS──► Cloudflare Tunnel ──► queued (Rust/axum)
                                                            ├── Postgres (sqlx, Migrationen embedded)
                                                            ├── Scheduler (pro User, Parallelität N, 1 Task/Projekt)
                                                            └── docker run --rm queued-agent  (pro Agent-/Verify-Run)
                                                                   ├── /workspace  ← data/workspaces/<projekt>
                                                                   └── $HOME       ← temporär, Credentials des Users
```

| Pfad | Inhalt |
|---|---|
| `backend/src/worker.rs` | Scheduler, Claim (`FOR UPDATE SKIP LOCKED`), OnEmptyQueue |
| `backend/src/runner.rs` | Ausführung, Verify, Review, Fix-Loop, Folgeaufträge |
| `backend/src/agent.rs` | Provider-Adapter (CLI-Aufruf + Stream-JSON-Parser, Limit-Erkennung) |
| `backend/src/prompt.rs` | Prompts + Ergebnis-Vertrag |
| `backend/src/workspace.rs` | Workspace und unsichtbares Snapshot-Repo (separates `--git-dir`) |
| `backend/src/sandbox.rs` | Docker- und Host-Ausführung, Streaming, Timeout, Cancel |
| `backend/src/oidc.rs`, `auth.rs` | Login (lokal, OIDC, Apple-Client-Secret-JWT, GitHub) |
| `frontend/src` | Web-UI (Queue, Inbox, Projekte, Einstellungen, Task-Drawer mit Live-Log und Diff) |

## Provider-Zugänge (pro User, unter Einstellungen)

Abos lassen sich nur über die offiziellen CLIs nutzen. Jeder User hinterlegt deshalb seinen eigenen Zugang:

| Provider | Abo | API |
|---|---|---|
| Claude Code | `claude setup-token` → Token einfügen | `ANTHROPIC_API_KEY` |
| Codex CLI | `codex login` → Inhalt von `~/.codex/auth.json` | OpenAI API-Key |
| Gemini CLI | Google-Login → Inhalt von `~/.gemini/oauth_creds.json` | Gemini API-Key |

Secrets werden mit AES-256-GCM (`MASTER_KEY`) verschlüsselt und nie wieder ausgeliefert. Erneuerte OAuth-Tokens (Codex, Gemini) speichert das System nach jedem Run automatisch zurück.

## Betrieb

### Produktion (z. B. Hetzner CAX21 + Cloudflare Tunnel)

```bash
cp .env.example .env          # POSTGRES_PASSWORD, PUBLIC_URL, MASTER_KEY (openssl rand -base64 32), IdPs
sudo mkdir -p /srv/queued
docker compose --profile build build agent-image   # Sandbox-Image mit den Agent-CLIs
docker compose up -d --build
docker compose --profile tunnel up -d              # optional: Cloudflare Tunnel (TUNNEL_TOKEN in .env)
```

Als IdP-Callback trägst du `${PUBLIC_URL}/api/auth/oidc/<id>/callback` ein. Bei Apple muss die Domain verifiziert sein, und die Callback-Route kommt als `form_post`.

### Entwicklung

```bash
# Postgres lokal, dann:
cd backend && DATABASE_URL=postgres://… SANDBOX=none ENABLE_MOCK_PROVIDER=true cargo run
cd frontend && npm install && npm run dev     # http://localhost:5173 (proxy → :8080)
```

`ENABLE_MOCK_PROVIDER=true` aktiviert den deterministischen Provider `mock`. Schlüsselwörter im Auftrag steuern sein Verhalten: `MOCK_FAIL`, `MOCK_ASK`, `MOCK_FOLLOWUPS`, `MOCK_LIMIT`. Damit lassen sich alle Flows ohne Tokens testen. `SANDBOX=none` führt Agenten ohne Isolation auf dem Host aus und ist nur für die Entwicklung gedacht.

## Sicherheit

- `app` braucht den Docker-Socket, um Sandboxes zu starten. Das ist root-äquivalent auf dem Host, deshalb nur auf einer dedizierten VM betreiben. Ausbaustufe: rootless Docker, Sysbox oder gVisor (`--runtime=runsc`).
- Agent-Container: `--memory`, `--cpus`, `--pids-limit`, `no-new-privileges`, nur Workspace und temporäres Home gemountet, Secrets nur per Env (nicht in argv). Internetzugriff ist für Recherche erlaubt, sonst per `DOCKER_NETWORK` einschränkbar.
- Sessions: HttpOnly, SameSite=Lax, Secure bei HTTPS. OIDC mit state, nonce, PKCE und Validierung von iss/aud/exp. Account-Linking erfolgt nur über verifizierte E-Mail.

## Bekannte Grenzen (MVP)

- Parallelität gilt projektübergreifend. Innerhalb eines Projekts läuft immer nur ein Agent, weil sich alle denselben Workspace teilen.
- Das verbleibende Abo-Kontingent ist über die CLIs nicht exakt abfragbar. Getrackt werden Tokens im 5h-Fenster, Limits werden aus den Fehlermeldungen erkannt.
- Der Review-Agent ist per Prompt read-only, nicht technisch erzwungen.
