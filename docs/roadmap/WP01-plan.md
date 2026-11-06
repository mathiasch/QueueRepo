# WP01 – Strukturierte CLI-Integration & Fehlerklassifikation: Plan

Status: Plan (Planer-Agent). Basis: Branch `mvp`, Stand 2026-11-06.
Grundlage: `docs/roadmap/work-packages.md` (gemeinsame Regeln + Abschnitt WP01), README, Code unter `backend/`, `frontend/`, `e2e/`, `docker/`.

Begriffe: **Run** = ein Prozessstart im Runner (Agent- oder Review-Lauf). **Task** = Eintrag in `tasks`. **Fehlerklasse** = neue Spalte `tasks.failure_class`.

---

## 1. Ziel

1. Agent-CLIs werden über ihre **strukturierten Signale** angebunden statt über Text-Heuristiken: Result-Contract per JSON-Schema (Claude `--json-schema`, Codex `--output-schema`), strukturierte Fehler-/Retry-Events (Claude `system/api_retry` + `assistant.error` + `result.subtype`, Codex `turn.failed`/`error`, Gemini `result.error.type`/`error.severity`), explizite Permission-Flags, native Turn-/Kosten-Limits.
2. **Fehlerklassen** trennen echte Task-Fehler (→ Fix-Task wie bisher) von Infra-/Provider-/Rate-Limit-/Konfigurations-/Limit-Fällen (→ Re-Queue mit Backoff bzw. Provider-Pause bzw. `blocked`), **ohne** Fix-Versuche (`attempt`) zu verbrauchen.
3. **Per-Task-Limits** (Projekt-Default, Task-Override): max. Laufzeit, max. Kosten (USD), max. Tokens, max. Turns, Stillstandserkennung (keine Ausgabe für N Sekunden).
4. **Sauberer Abbruch**: SIGINT an Prozessgruppe/Container, Grace-Periode, erst dann SIGKILL – Ausgabe nach SIGINT (Claude schreibt dann noch `result` mit Usage) wird weiter gestreamt und ausgewertet.
5. **CLI-Versionen gepinnt** im Agent-Image, **Contract-Tests** der Parser gegen Fixtures, die an genau diese Versionen gebunden sind.

## 2. Nicht-Ziele (explizit, mit Paketzuordnung)

- `--bare`/Abschalten von Repo-Hooks, `.mcp.json`, Projekt-Settings → **WP03**. Hinweis: `--bare` liest laut Doku *keine OAuth-Tokens* (nur `ANTHROPIC_API_KEY`/`apiKeyHelper`) und wäre mit dem bestehenden Credential-Typ `oauth_token` inkompatibel – daher hier ausdrücklich nicht.
- Session-Resume/Steering (`--resume`) → WP06. Deshalb kein `--no-session-persistence` (HOME wird nach jedem Run ohnehin gelöscht, `exec.rs:139`).
- Credential-Modell/Lizenzfragen → ausgeklammert; Credential-Mechanik bleibt unverändert (nur: Credential-Update hebt eine Auth-Pause auf, s. 5.6).
- Phasen-Wiederaufnahme (Verify/Review erneut ohne Agent-Run): Ein Provider-Fehler **im Review-Run** führt nicht zum Re-Queue des Tasks (s. 5.5.4), das bleibt bekannte Grenze.
- Netz-/Egress-Policy → WP02. Metriken/Dashboard → WP10.
- Live-Tests gegen echte Provider-Accounts: hier nicht möglich (keine Zugangsdaten, keine Erreichbarkeit). Alle Aussagen dazu sind als Annahme markiert (Kap. 10).

---

## 3. Ist-Analyse (geprüft, mit Belegstellen)

Recherche-Belege: `claude --help` (installiert: 2.1.319, `/opt/claude-code/bin/claude`), Strings im Claude-Binary (Zod-Schemas), `npm install @openai/codex@0.167.0 @google/gemini-cli@0.67.0` in den Scratchpad + `codex exec --help`, `gemini --help`, Bundle-Quelltext von Gemini 0.67.0, Doku `code.claude.com/docs/en/headless` und `/cli-reference` (abgerufen 2026-11-06).

| # | Schwäche | Stelle | Folge |
|---|---|---|---|
| S1 | Rate-Limit-Erkennung per Substring (`"429"`, `"quota"`, `"rate limit"` …) | `agent.rs:359-374`, `classify_error` `agent.rs:377-383` | False Positives: Projekttext/Testausgabe mit „429“/„quota“ pausiert den Provider. |
| S2 | stderr-Tail wird nach denselben Keywords durchsucht, sobald Exit ≠ 0 | `runner.rs:656-662` | Ein fehlschlagender `npm`/`curl`-Aufruf im Agenten mit „429“ auf stderr pausiert den Provider bis Reset/Fallback (60 min). |
| S3 | Transiente Provider-Fehler (overloaded/5xx/Netz) werden zu `AgentEvent::Error` → `failure` → Fix-Task, `attempt` +1 | `agent.rs:246-251,302-303,331`; `runner.rs:378-387,480-501` | Fix-Budget (Default 2) wird von Infra-Problemen verbrannt, Task endet `blocked`, obwohl er nie fehlerhaft war. |
| S4 | Codex-`error`-Events (z. B. „Reconnecting… 1/5“, „stream disconnected – retrying …“; String im Codex-Binary 0.167.0 belegt) und Gemini-`error` mit `severity:"warning"` (Bundle 0.67.0 belegt) werden als harte Fehler gewertet | `agent.rs:303`, `agent.rs:331` | Erfolgreiche Runs bekommen `o.error` gesetzt; ohne result.json → `failed`. |
| S5 | Runner-/Infra-Fehler (Runner nicht erreichbar, Stream abgebrochen, `ExecFrame::Fail`) → `run_task`-Fehlerpfad setzt `failed` ohne Fix, ohne Retry, ohne Backoff; bis dahin gesehene Usage geht verloren (nicht in `usage_log`) | `runner.rs:27-49`, `runner.rs:632,652`, `runner_client.rs:224-237` | Kurzer Runner-Neustart killt jeden laufenden Task endgültig; Budget-Unterzählung. |
| S6 | Result-Contract nur per Datei; ungültiges JSON wird nur geloggt, dann `Default` → Status aus Exit-Code | `runner.rs:311-333` | Agent, der den Contract nicht schreibt und Exit 0 liefert, gilt als `done`. |
| S7 | Laufzeit nur global `TASK_TIMEOUT_MIN` (Minuten-Granularität, nicht testbar unter 60 s); Timeout → Fix-Task mit derselben Grenze | `config.rs:67`, `runner.rs:376-377,587` | Endlosschleife aus Timeout-Fixes bis `blocked`; keine Projekt-/Task-Steuerung. |
| S8 | Keine Kosten-/Token-/Turn-Limits pro Task; Budgets nur pro Provider-Fenster und erst **nach** dem Run geprüft | `budget.rs` (`evaluate`, `take_budget_notification`), `runner.rs:206-214` | Ein einzelner Task kann das gesamte 5h-Fenster verbrauchen. |
| S9 | Keine Stillstandserkennung (Agent hängt an interaktivem Prompt o. Ä. bis zum Timeout) | `exec.rs:221-230` | Bis 60 min verbrannte Slot-Zeit. |
| S10 | Abbruch/Timeout = sofort SIGKILL (Prozessgruppe bzw. `podman/docker rm -f`) | `exec.rs:231-242`, `exec.rs:140-159` | Claude kann den Turn nicht beenden, `result`-Event (einzige Usage-Quelle für Claude, `agent.rs:236-244`) kommt nie → abgebrochene/zeitüberschrittene Runs zählen **0 Tokens** im Budget. Doku: SIGTERM → Exit 143 ohne Result; SIGINT beendet den Turn sauber. |
| S11 | Claude-Aufruf ohne explizite Permission-Mode/`--permission-prompts none`, ohne `--json-schema`, ohne Limits | `agent.rs:58-82` | Default-Mode von `-p` ist seit 2.1.302 „Manual“; `--dangerously-skip-permissions` ist zwar äquivalent zu `bypassPermissions`, aber `--permission-prompts none` (entfernt `AskUserQuestion`, sagt dem Modell „nicht erneut versuchen“) fehlt. |
| S12 | Claude-Usage nur aus `result.usage` (laut Doku zählt `usage` Subagenten zu wenig; `modelUsage` enthält sie) | `agent.rs:237-244` | Budget-Unterzählung bei Subagenten. |
| S13 | **Gemini 0.67.0: YOLO wird in nicht vertrauenswürdigem Ordner auf `default` zurückgesetzt** (`checkPathTrust` → `isTrusted ?? false`; Log „Approval mode overridden to "default" because the current folder is not trusted.“). Der Run-HOME ist frisch, es gibt keine `trustedFolders.json` | Gemini-Bundle `gemini-*.js`; `agent.rs:110-137` | Gemini darf im Headless-Modus keine Schreib-/Shell-Tools ausführen → Integration funktional kaputt (Annahme A6 zur genauen Wirkung). |
| S14 | Agent-Image installiert `@latest` aller CLIs | `docker/agent.Dockerfile:11-17` | Stilles Format-Drift; Parser-Tests prüfen keine konkrete Version. |
| S15 | Review-Run: Rate-Limit/Abbruch → Review wird stillschweigend übersprungen, **Provider wird nicht pausiert**, Task wird `done` | `runner.rs:772-774`, `runner.rs:414` | Nächste Tasks laufen sofort wieder ins Limit. |
| S16 | Kontext-Notiz bei Re-Queue wird bei jedem Limit erneut angehängt | `runner.rs:271-279`, `worker.rs:21-23` | Wachsender, redundanter Kontext (Prompt-Bloat). |
| S17 | Scheduler kennt kein „nicht vor“ | `worker.rs:89-106` | Kein Backoff möglich. |
| S18 | `RunnerClient::exec` HTTP-Timeout = `timeout_secs + 120` | `runner_client.rs:214` | Mit Grace-Periode muss das mitwachsen. |
| S19 | Integrations-Config `task_timeout_min: 1` → Laufzeit-Szenarien nicht in Sekunden testbar | `config.rs:96` | Limits müssen in Sekunden gespeichert werden. |

Verifizierte CLI-Fakten (Grundlage des Designs):

- **Claude Code 2.1.319**: `--json-schema <schema>` (print-only; Ergebnis im Feld `structured_output` des `result`-Events; fehlt, wenn kein gültiges Ergebnis), `--max-budget-usd <amount>` (→ `result.subtype = "error_max_budget_usd"`), `--max-turns <n>` (versteckt in `--help`, in `/cli-reference` dokumentiert; → `error_max_turns`), `--permission-mode bypassPermissions`, `--permission-prompts none` (ab 2.1.259). Result-Subtypes: `success`, `error_during_execution`, `error_max_turns`, `error_max_budget_usd`, `error_max_structured_output_retries`. `system/api_retry`: `attempt`, `max_retries`, `retry_delay_ms`, `error_status` (int|null), `error` ∈ {`authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `rate_limit`, `overloaded`, `invalid_request`, `model_not_found`, `server_error`, `max_output_tokens`, `cloud_credential_error`, `unknown`}. Dieselbe Enum steht im `error`-Feld von `assistant`-Nachrichten (Binary-Schema). `rate_limit_event.rate_limit_info`: `status` ∈ {allowed, allowed_warning, rejected}, `resetsAt`, `rateLimitType`. `modelUsage{<model>: {inputTokens, outputTokens, cacheReadInputTokens, cacheCreationInputTokens, costUSD}}`. SIGINT beendet den Turn, SIGTERM → Exit 143 ohne Result.
- **Codex CLI 0.167.0** (`codex exec --help`): `--json`, `--output-schema <FILE>` (Schema für die finale Antwort), `-o/--output-last-message <FILE>`, `--dangerously-bypass-approvals-and-sandbox`, `--skip-git-repo-check`, `--ephemeral`, `-c key=value`. **Kein** Turn-/Kosten-Limit. JSONL-Events: `thread.started`, `turn.started`, `turn.completed` (usage inkl. `cached_input_tokens`), `turn.failed`, `item.started|updated|completed`, `error`. Limit-Text „You’ve hit your usage limit.“ (typografischer Apostroph!).
- **Gemini CLI 0.67.0**: `-o stream-json`, `-p`, `--yolo`/`--approval-mode`, `--skip-trust`; Env `GEMINI_CLI_TRUST_WORKSPACE=true` erzwingt Vertrauen. Stream-Events: `init`, `message`, `tool_use`, `tool_result`, `error` (mit `severity` warning|error), `result` (`status`, `error{type,message}`, `stats`). Turn-Limit nur über `settings.json` `model.maxSessionTurns` → `FatalTurnLimitedError`, Exit-Code 53. **Kein** Schema-Flag, **keine** Kostenangabe. SIGINT-Handler vorhanden (`handleCancellationError` → `result` mit `status:"error"`).

---

## 4. Fehlerklassen und Disposition (normativ)

Neue Spalte `tasks.failure_class` (NULL = kein Fehler). Werte und Behandlung:

| `failure_class` | Auslöser | Task-Status danach | `attempt` | Fix-Task | Provider | Notification |
|---|---|---|---|---|---|---|
| `task` | Agent meldet `failed`; Verify fehlgeschlagen; Review abgelehnt; Exit ≠ 0 ohne Provider-Signal; fehlender Contract bei Provider mit Schema-Fähigkeit; `max_output_tokens`; `error_max_structured_output_retries` ohne Datei-Fallback | `failed` (+Fix) bzw. `blocked` nach `max_fix_attempts` – **unverändert** | +1 im Fix | ja (wie bisher) | – | wie bisher |
| `rate_limit` | `rate_limit_event.status=rejected`; `api_retry`/`assistant.error`=`rate_limit` als terminaler Fehler; Codex/Gemini Limit-Text (nur Textfallback, s. 5.3.5) | `queued` (bisheriges Verhalten) | unverändert | nein | `limited_until` = Reset bzw. Fallback (bestehend) | `limit` (bestehend) |
| `provider_transient` | terminal `overloaded`, `server_error`, `unknown` mit `error_status` ≥ 500 oder `null` (kein HTTP), Codex `turn.failed` mit Stream-/Netzfehler, Gemini `result.error` mit Retryable-/Netzfehler | `queued`, `not_before = now + backoff(n)` | unverändert | nein | – | nur Event; bei Erschöpfung `blocked` + Push |
| `infra` | Runner nicht erreichbar, NDJSON-Stream abgebrochen, `ExecFrame::Fail` (Programm startet nicht), DB-/HTTP-Fehler im Orchestrierungspfad, App-Neustart (`worker::recover`) | wie `provider_transient` | unverändert | nein | – | wie `provider_transient` |
| `stalled` | Runner meldet `stalled` (keine stdout/stderr-Zeile für `stall_timeout_secs`) | wie `provider_transient` | unverändert | nein | – | wie `provider_transient` |
| `provider_auth` | `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `cloud_credential_error` | `queued` (ohne Backoff-Zähler) | unverändert | nein | **pausiert** mit `provider_limits.paused_reason`, bis Credential-Update oder `POST /api/limits/{p}/resume` | `provider_paused` (Push) |
| `config` | `model_not_found`, `invalid_request`, unbekannter Credential-Typ (`agent::invocation` → Err) | `blocked` | unverändert | nein | – | `blocked` (Push) |
| `limit_runtime` | Runner `timed_out` (effektive Laufzeitgrenze) | `blocked` | unverändert | nein | – | `blocked` (Push) |
| `limit_cost` | App-Guard (Live-Kosten ≥ Rest-Budget) oder Claude `error_max_budget_usd` | `blocked` | unverändert | nein | – | `blocked` (Push) |
| `limit_tokens` | App-Guard (Live-Tokens ≥ Rest-Budget) | `blocked` | unverändert | nein | – | `blocked` (Push) |
| `limit_turns` | Claude `error_max_turns`, Gemini `FatalTurnLimitedError`/Exit 53, Mock `limit/turns` | `blocked` | unverändert | nein | – | `blocked` (Push) |
| `cancelled` | Nutzer-Abbruch | `cancelled` (bestehend) | – | nein | – | – |

Re-Queue-Zähler: `tasks.transient_retries` gilt gemeinsam für `provider_transient`, `infra`, `stalled`. Ist nach Inkrement `transient_retries > TRANSIENT_MAX_RETRIES` → `blocked` (Klasse bleibt erhalten, `error` enthält „Nach N Wiederholungen aufgegeben: …“).
Backoff: `backoff(n) = min(TRANSIENT_BACKOFF_BASE_SECS · 2^(n−1), TRANSIENT_BACKOFF_MAX_SECS)`, n = Zählerstand nach Inkrement (1-basiert), ohne Jitter (deterministisch testbar). Defaults: Basis 30 s, Max 900 s, Max-Retries 5.

**Vorrang bei der Auswertung eines Agent-Runs** (reine Funktion `classify_run`, Reihenfolge verbindlich):
1. Nutzer-Abbruch (`cancel` des Task-Tokens) → `cancelled`.
2. Guard-Limit der App (`limit_tokens`/`limit_cost`) → entsprechende Klasse.
3. Runner `timed_out` → `limit_runtime`; Runner `stalled` → `stalled`.
4. CLI-gemeldetes Limit (`error_max_turns`, `error_max_budget_usd`, Gemini 53) → `limit_turns`/`limit_cost`.
5. Infra-Fehler (Stream/Runner/Fail) → `infra`.
6. **Gültiger Contract dieses Runs vorhanden** (structured output oder `result.json`) → Contract-Status gilt (`done`/`failed`/`needs_input`). Ein gleichzeitig erkanntes `rate_limit`/`provider_auth` setzt trotzdem die Provider-Pause (für Folgetasks), ändert aber das Task-Ergebnis nicht.
7. Terminaler Provider-Fehler ohne Contract → `rate_limit` / `provider_auth` / `provider_transient` / `config` gemäß Kategorie-Mapping (5.3.4).
8. Kein Contract: Provider mit Schema-Fähigkeit (claude, codex) → `task` („Ergebnis-Contract fehlt“); sonst (gemini, mock) bisheriger Exit-Code-Fallback (`success` → done, sonst `task`).

Verify-/Review-Fehler bleiben `task` (Fix-Loop unverändert).

---

## 5. Design

### 5.1 Datenmodell – neue Migration `backend/crates/app/migrations/0003_cli_contract_limits.sql`

```sql
-- Per-task limits: project defaults + task overrides (NULL = use next level)
ALTER TABLE projects
    ADD COLUMN max_runtime_secs   INT    CHECK (max_runtime_secs   BETWEEN 1 AND 86400),
    ADD COLUMN stall_timeout_secs INT    CHECK (stall_timeout_secs BETWEEN 0 AND 86400), -- 0 = off
    ADD COLUMN max_cost_usd       DOUBLE PRECISION CHECK (max_cost_usd > 0),
    ADD COLUMN max_tokens         BIGINT CHECK (max_tokens > 0),
    ADD COLUMN max_turns          INT    CHECK (max_turns BETWEEN 1 AND 1000);

ALTER TABLE tasks
    ADD COLUMN max_runtime_secs  INT    CHECK (max_runtime_secs BETWEEN 1 AND 86400),
    ADD COLUMN max_cost_usd      DOUBLE PRECISION CHECK (max_cost_usd > 0),
    ADD COLUMN max_tokens        BIGINT CHECK (max_tokens > 0),
    ADD COLUMN max_turns         INT    CHECK (max_turns BETWEEN 1 AND 1000),
    ADD COLUMN failure_class     TEXT   CHECK (failure_class IN ('task','rate_limit','provider_transient','infra','stalled',
                                     'provider_auth','config','limit_runtime','limit_cost','limit_tokens','limit_turns','cancelled')),
    ADD COLUMN transient_retries INT    NOT NULL DEFAULT 0,
    ADD COLUMN not_before        TIMESTAMPTZ;

CREATE INDEX tasks_claim_idx ON tasks (user_id, position) WHERE status = 'queued';

-- Provider paused by an auth/billing failure until credentials change or the user resumes
ALTER TABLE provider_limits ADD COLUMN paused_reason TEXT;
```

Keine Änderung an 0001/0002. Stall-Timeout bewusst nur auf Projektebene (Task-Override bringt keinen Mehrwert, kostet UI).

Effektive Limits (`limits::effective(task, project, cfg)`):
- `runtime_secs = task.max_runtime_secs ?? project.max_runtime_secs ?? cfg.task_timeout_min·60`
- `stall_secs = project.stall_timeout_secs ?? cfg.stall_timeout_secs` (0 → aus → `None` im ExecRequest)
- `cost_usd`, `tokens`, `turns = task ?? project ?? None` (unbegrenzt)

Geltungsbereich: Laufzeit und Stall **pro Run** (Agent-Run und Review-Run je einzeln; Verify behält `VERIFY_TIMEOUT_MIN`). Kosten und Tokens **kumulativ pro Task** über Agent- + Review-Run (Rest = Limit − bisher verbraucht; ist der Rest vor dem Review ≤ 0 → `limit_cost`/`limit_tokens`, kein Review-Run). Turns **pro Run**.
Vererbung: `create_fix` kopiert die Task-Level-Limits des fehlgeschlagenen Tasks; Follow-ups und OnEmpty-Tasks erhalten keine Task-Level-Limits (→ Projekt-Defaults).

### 5.2 Protokoll `queued-common` (`backend/crates/common/src/lib.rs`)

- `ExecRequest` += `#[serde(default)] idle_timeout_secs: Option<u64>` (Stall-Grenze), `#[serde(default)] queue_files: Vec<(String, String)>` (Dateien, die der Runner vor dem Start nach `<workspace>/.queue/<name>` schreibt; Name muss `valid_queue_file_name` erfüllen: `^[a-z0-9][a-z0-9._-]{0,63}$`, kein `/`, kein `..`).
- `ExecFrame::Exit` += `#[serde(default)] stalled: bool`, `#[serde(default)] killed: bool` (true = Prozess reagierte nicht innerhalb der Grace-Periode auf SIGINT und wurde per SIGKILL beendet).
- `pub fn valid_queue_file_name(&str) -> bool` + Tests; Roundtrip-Test um neue Felder erweitert; Test, dass ein alter Exit-Frame ohne die neuen Felder noch deserialisiert.

### 5.3 App: Provider-Adapter (`agent.rs` → Modul `agent/`)

`agent.rs` (620 Zeilen) wird aufgeteilt; öffentliche API bleibt über `crate::agent::*` erreichbar:

- `agent/mod.rs`: `Credential`, `Invocation` (+ `queue_files`), `RunLimits { turns: Option<u32>, cost_usd: Option<f64> }`, `invocation(provider, model, cred, &RunLimits, kind: RunKind{Agent,Review})`, `AgentEvent`, `ErrorCategory`, `ParseState`, `parse_line` (Dispatch), `trunc`, `tool_summary`, `credential_kinds`, `CLI_VERSIONS`, `capabilities(provider) -> Capabilities { structured_output, max_turns, max_cost, live_usage }`.
- `agent/claude.rs`, `agent/codex.rs`, `agent/gemini.rs`, `agent/mock.rs`: je `invocation_*` + `parse_*`.
- `agent/classify.rs`: `ErrorCategory` ↔ CLI-Strings, Text-Fallback (eingeschränkt), `parse_reset`, `find_reset_in_json` (aus `agent.rs` verschoben).

#### 5.3.1 Neue Event-Typen

```rust
pub enum AgentEvent {
    Text(String),
    Tool(String),
    UsageDelta(Usage),          // live, inkrementell (Summe ≈ Verbrauch bisher)
    UsageTotal(Usage),          // autoritativ für den Run, ersetzt Summe der Deltas
    Final(String),
    Structured(serde_json::Value), // Result-Contract aus dem Stream
    Retry { category: ErrorCategory, attempt: u32, max: u32, delay_ms: u64, status: Option<u16> }, // nur Hinweis
    Failure { category: ErrorCategory, message: String },  // terminaler Provider-/CLI-Fehler
    LimitHit(LimitKind),        // Turns | Cost (von der CLI gemeldet)
    Error(String),              // generischer Fehlertext ohne Kategorie (→ task)
}
pub enum ErrorCategory { RateLimit(Option<DateTime<Utc>>), Overloaded, ServerError, Network, Auth, Billing,
                         InvalidRequest, ModelNotFound, MaxOutputTokens, Unknown(Option<u16>) }
pub enum LimitKind { Runtime, Cost, Tokens, Turns }
```

`RateLimited` entfällt (geht in `Failure{RateLimit}` auf); Aufrufer in `runner.rs` werden angepasst.

#### 5.3.2 Aufrufe (Invocation)

Schema-Konstanten in `models.rs` (eine Quelle, s. 5.4):

- **claude** (Agent & Review):
  `claude -p --output-format stream-json --verbose --permission-mode bypassPermissions --permission-prompts none --json-schema <RESULT_SCHEMA|REVIEW_SCHEMA> [--max-turns N] [--max-budget-usd X] [--model M]`
  X = verbleibendes Kostenbudget des Tasks, formatiert mit 4 Nachkommastellen. `--dangerously-skip-permissions` entfällt (äquivalent zu `bypassPermissions`). Env unverändert (`CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY`, `DISABLE_AUTOUPDATER=1`).
- **codex**:
  `codex exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox --output-schema .queue/result.schema.json [--model M] -`
  `queue_files = [("result.schema.json", RESULT_SCHEMA)]` bzw. `("review.schema.json", REVIEW_SCHEMA)` mit Pfad `.queue/review.schema.json`. Turn-/Kosten-Limit: von Codex nicht unterstützt → keine Flags; die App emittiert beim Start ein Event `guard`: „codex unterstützt kein Turn-/Kostenlimit – es gelten Laufzeit, Stillstand und (nachträglich geprüfte) Tokens.“ (nur wenn ein solches Limit gesetzt ist).
- **gemini**:
  `gemini --output-format stream-json --yolo -p "Follow the instructions given on stdin." [--model M]`, Env `GEMINI_CLI_TRUST_WORKSPACE=true` (behebt S13). `home_files` enthält **immer** `.gemini/settings.json`, erzeugt per `serde_json` (nicht per String-Konkatenation): bei `oauth_creds` die bisherigen Auth-Keys, bei gesetztem Turn-Limit zusätzlich `{"model":{"maxSessionTurns":N}}`. Kein Schema → Contract nur via Datei.
- **mock**: wie bisher `sh -c MOCK_SCRIPT`; Limits als Env `QUEUE_MAX_TURNS`, `QUEUE_MAX_COST_USD` (Analogon zu argv-Flags, damit Integrationstests das Durchreichen prüfen). Keine Secrets in argv bei allen Providern (bestehender Test erweitert).

#### 5.3.3 Parser (pro Provider)

- **claude**:
  - `assistant`: Text/Tool wie bisher; `message.usage` → `UsageDelta` mit Dedupe über `message.id` (ParseState hält `HashMap<id, Usage>`; Delta = neu − zuletzt gesehen für diese id, komponentenweise, nie negativ). Feld `error` (Enum s. 3) → merkt sich `st.last_error_category`.
  - `system` + `subtype:"api_retry"` → `Retry{…}`; merkt Kategorie in `st.last_retry_category`.
  - `rate_limit_event`: `rate_limit_info.status == "rejected"` → `Failure{RateLimit(resetsAt)}` (strukturiert statt `blob.contains("reject")`); `allowed`/`allowed_warning` → nichts.
  - `result`: Usage aus `modelUsage` (Summe über Modelle, falls vorhanden) sonst `usage` → `UsageTotal` (inkl. `total_cost_usd`); `structured_output` vorhanden → `Structured`; `subtype`: `error_max_turns` → `LimitHit(Turns)`, `error_max_budget_usd` → `LimitHit(Cost)`, `error_max_structured_output_retries` → `Error("…")`; `is_error` sonst → `Failure{category}` mit Kategorie = `st.last_error_category` ∨ `st.last_retry_category` ∨ Textfallback(result) ∨ `Unknown(None)`; Erfolg → `Final(result)`.
- **codex**:
  - `item.completed/agent_message` → Text + `last_text`; `turn.completed` → `UsageTotal` + `Final(last_text)`; wenn `last_text` als JSON parsebar → zusätzlich `Structured`.
  - `error`-Event: Nachricht matcht `^(Reconnecting|stream disconnected)` bzw. enthält „retrying“ → `Retry{Network,…}` (Attempt/Max per Regex `(\d+)/(\d+)` aus der Nachricht, sonst 0); sonst Textklassifikation (5.3.5) → `Failure` oder `Error`.
  - `turn.failed` → Textklassifikation → `Failure`/`Error`.
  - `item.*` mit `type:"error"` → bleibt `Error` (Tool-Fehler sind Task-Ebene).
- **gemini**:
  - `error` mit `severity:"warning"` → `Text("⚠ …")` (Hinweis), `severity:"error"` → Textklassifikation.
  - `result`: `stats` → `UsageTotal`; `status:"error"`: `error.type == "FatalTurnLimitedError"` → `LimitHit(Turns)`; `error.type == "FatalCancellationError"` → nichts (Abbruch wird über Exit-Frame erkannt); sonst Textklassifikation; Erfolg → `Final`.
  - Exit-Code 53 ohne `result` → in `classify_run` als `limit_turns` (Gemini-spezifisch über `capabilities`).
- **mock** (erweitert, s. 5.8): `text`, `tool`, `final`, `usage` (→ `UsageDelta`, jetzt inkl. `cost_usd`), `error` (+ optional `category` → `Failure`), `retry` → `Retry`, `structured` → `Structured(value)`, `limit` (`kind:"turns"|"cost"`) → `LimitHit`.

#### 5.3.4 Kategorie → Fehlerklasse (in `classify.rs`, rein, unit-getestet)

| Kategorie (Claude-String) | Fehlerklasse |
|---|---|
| `rate_limit` | `rate_limit` |
| `overloaded`, `server_error` | `provider_transient` |
| `unknown` mit `error_status` ∈ {null, 408, 409, ≥500} | `provider_transient`; sonst `task` |
| Network (Codex/Gemini Text) | `provider_transient` |
| `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `billing_error`, `cloud_credential_error` | `provider_auth` |
| `model_not_found`, `invalid_request` | `config` |
| `max_output_tokens` | `task` |

#### 5.3.5 Textfallback (nur Codex/Gemini, eingeschränkt)

Ersetzt `is_rate_limit_text`. Gilt **nur** für Fehlermeldungen aus dem Fehlerkanal der CLI (`turn.failed`, `error`-Event mit severity error, `result.error.message`), nicht für Text/Tool-Ausgaben und **nicht mehr für stderr** (S2 entfällt komplett).
Muster (case-insensitive, Apostroph `'`/`’` beide):
- RateLimit: `usage limit`, `hit your limit`, `rate limit`, `resource_exhausted`, `too many requests`, `quota exceeded`, `exhausted your capacity` (Gemini `TerminalQuotaError`-Texte – im Contract-Fixture belegt), `\b429\b` nur zusammen mit `status|http|code`.
- Auth: `401`, `unauthorized`, `invalid api key`, `authentication`, `not logged in`, `refresh token`.
- Transient/Network: `overloaded`, `\b5\d\d\b` mit `status|http`, `stream disconnected`, `connection reset`, `timed out`, `ECONNRESET`, `ETIMEDOUT`, `EAI_AGAIN`, `service unavailable`.
- sonst `Error(text)` → Klasse `task`.
`parse_reset` bleibt (Epoch-`|1234567890`, „try again in …“).

### 5.4 Result-Contract

`models.rs`:
- `pub const RESULT_SCHEMA: &str` – OpenAI-„strict“-tauglich (Codex) und für Claude gültig: `type:object`, `additionalProperties:false`, **alle** Properties in `required`: `status` (enum done|failed|needs_input), `summary` (string), `handoff` (string), `questions` (array of string), `follow_up_tasks` (array of `{title:string, prompt:string}`, ebenfalls strict).
- `pub const REVIEW_SCHEMA: &str` – `{approved:boolean, issues:array<string>}`, strict.
- `AgentResult`/`ReviewResult` unverändert deserialisierbar (`#[serde(default)]`).

Auswertung in `runner.rs` (`load_contract`): Reihenfolge **`Structured`-Event (letztes) → `.queue/result.json` → keiner**. Ungültige Struktur (serde-Fehler) → nächste Quelle, Event `contract` mit Grund. Erfolgreiche Quelle wird als Event `contract` protokolliert: „Ergebnis via structured output“ | „Ergebnis via .queue/result.json (Fallback)“ | „kein Ergebnis-Contract“. Review analog mit `review.json`.
Prompt (`prompt.rs`): `RESULT_CONTRACT`-Text bleibt **bytegleich** (Datei bleibt Fallback und einziger Weg für Gemini; Prompt-Caching-Test bleibt grün). Kein provider-spezifischer Prompt-Text nötig: das Schema erzwingt die CLI.

### 5.5 App: Orchestrierung (`runner.rs`, `limits.rs`, `queue.rs`, `worker.rs`)

#### 5.5.1 Neues Modul `limits.rs` (rein, unit-getestet)
- `EffectiveLimits` + `effective(&Task, &Project, &Config)`.
- `backoff(n, base, max) -> Duration`.
- `Guard { tokens_left: Option<i64>, cost_left: Option<f64>, seen: Usage }` mit `fn on_delta(&mut self, &Usage) -> Option<LimitKind>` (prüft `counted(seen) ≥ tokens_left` bzw. `seen.cost_usd ≥ cost_left`).

#### 5.5.2 `agent_run`
- Baut `ExecRequest` mit `timeout_secs = runtime_secs`, `idle_timeout_secs = stall_secs`, `queue_files`.
- Eigenes `run_cancel = cancel.child_token()`; der Cancel-Watcher in `exec()` reagiert auf `run_cancel` (Nutzer-Abbruch kaskadiert automatisch).
- Consumer: `UsageDelta` → `o.usage_delta.add`, `guard.on_delta` → bei Treffer `o.limit_hit = Some(kind)`, Event `guard` („Token-Limit erreicht (x / y) – Lauf wird beendet“), `run_cancel.cancel()` (genau einmal). `UsageTotal` → `o.usage_total = Some`. `Retry` → Event `retry` („Provider: overloaded – Versuch 2/10 in 4 s (HTTP 529)“), keine Zustandsänderung. `Failure` → `o.failure = Some((cat,msg))` + Event `error`. `LimitHit` → `o.cli_limit`. `Structured` → `o.structured`.
- Run-Usage = `usage_total.unwrap_or(usage_delta)`.
- **Fehler aus `exec()` (Runner/Stream) werden nicht mehr per `?` propagiert**: `o.infra_error = Some(msg)`, gesammelte Usage bleibt erhalten. `ExecFrame::Fail` ebenso.
- Credentials-Writeback wie bisher.
- Rückgabe enthält `exit_code` (für Gemini 53), `stalled`, `killed`.

#### 5.5.3 `run_inner` / `run_task`
- `run_inner` liefert `Result<(), RunError>` mit `RunError::{Config(String), Infra(anyhow::Error)}`; `agent::invocation`-Fehler → `Config`; alles andere (`?` auf DB/Runner) → `Infra`.
- `run_task`-Fehlerpfad: `Config` → `blocked`/`config` + Push; `Infra` → `queue::requeue(…, Infra)` (s. u.) statt `failed`.
- Nach dem Agent-Run: `budget::record_usage` (wie bisher, jetzt auch bei Infra/Abbruch mit der Teil-Usage), dann `classify_run(&outcome, &contract, capabilities)` → `Disposition`:
  `Cancelled | Done(contract) | WaitingInput | TaskFailure(msg) | Requeue{class, note} | PauseAndRequeue{class, reason} | RateLimited(reset) | Blocked{class, msg}`.
- Teil-Commit vor Re-Queue/Blocked: `commit_msg(" (unterbrochen: <Klasse>)")`; schlägt der Commit fehl (Runner weg), wird das geloggt und ignoriert – `begin` des nächsten Runs committet Reste als WIP (bestehend, `git.rs:193-228`).
- `queue::requeue(st, task, class, note, backoff: bool)`: `UPDATE tasks SET status='queued', started_at=NULL, failure_class=$c, transient_retries = transient_retries + (backoff?1:0), not_before = CASE … , context = CASE WHEN position($note in context) > 0 THEN context ELSE context || $note END … WHERE id=$1 AND status='running'` (Notiz-Dedupe, behebt S16). Notizen (englisch, für den Agenten, je Klasse fix): rate_limit = bestehender Text „…interrupted by a usage limit…“ (Test prüft „usage limit“); transient = „(Note: a previous run was interrupted by a transient provider error; the repository may contain partial changes from it – see git log.)“; infra = „…by an infrastructure error…“; stalled = „…was stopped because it produced no output for N seconds (possibly waiting for interactive input – never run interactive commands)…“; provider_auth = keine Notiz.
- Erschöpfung (`transient_retries` nach Inkrement > Max) → `blocked` mit gleicher Klasse, Push `blocked`.
- `Blocked{limit_*}`: `finish(BLOCKED)` mit `error` „Limit überschritten: Laufzeit 120 s“ etc., **kein** `create_fix`.
- Bestehende Fix-Logik nur für `TaskFailure` (setzt `failure_class='task'`).
- `finish()` bekommt Parameter `failure_class: Option<&str>`; `done` setzt `failure_class=NULL`.
- Nachträgliche Token-Prüfung bei Providern ohne `live_usage` (codex, gemini): ist nach dem Run `counted(run_usage) > tokens_left`, wird ein Event `guard` und eine Notification `limit_exceeded` (ohne Push) erzeugt, das Task-Ergebnis bleibt bestehen (bewusste, dokumentierte Abweichung: Verbrauch kommt erst am Ende).

#### 5.5.4 Review-Run
- Gleiche Mechanik (Schema, Limits mit Rest-Budget, Guard).
- Ist Rest-Budget (Kosten/Tokens) ≤ 0 vor dem Review → Task `blocked`/`limit_*`.
- Review-Run mit `rate_limit` → `budget::set_limited` für den **Review-Provider** (behebt S15), `provider_auth` → Pause mit Grund; in beiden Fällen sowie bei `provider_transient`/`infra`/`stalled`: Event `review` „Review übersprungen: <Klasse>“, Task läuft weiter wie bisher bei „Review übersprungen“ (Nicht-Ziel: Phasen-Retry). `limit_*` im Review → Task `blocked`.

#### 5.5.5 Scheduler (`worker.rs`)
- `claim_next`: zusätzlich `AND (t.not_before IS NULL OR t.not_before <= now())`; bei Claim `not_before=NULL` setzen.
- `recover`: `transient_retries = transient_retries + 1, failure_class='infra', not_before = now()` + deduplizierte Notiz (bestehender Text bleibt).
- `maybe_onempty` „busy“-Prüfung zählt `queued` mit `not_before` weiter als busy (unverändert korrekt).

#### 5.5.6 Provider-Pause (`budget.rs`)
- `ProviderStatus.paused_reason: Option<String>`; `blocked = limited_until.is_some() || paused_reason.is_some() || budget exhausted`.
- `set_paused(db, user, provider, reason)`, `clear_pause(db, user, provider)` (setzt `paused_reason=NULL, limited_until=NULL`).

### 5.6 API

- `GET/PATCH /api/projects/{id}`: neue Felder `max_runtime_secs`, `stall_timeout_secs`, `max_cost_usd`, `max_tokens`, `max_turns`. Semantik wie `verify_command`: Feld fehlt → unverändert; `null` → zurück auf Default (NULL). Validierung in Rust **vor** SQL (400 mit deutscher Meldung, statt DB-CHECK-500): Laufzeit 1–86400, Stall 0–86400, Kosten > 0 und ≤ 10 000, Tokens 1–10^10, Turns 1–1000. Editor-Recht (bestehend).
  Umsetzungshinweis: Für „Feld fehlt vs. null“ `Option<Option<T>>` mit `#[serde(default, deserialize_with = …)]` (kleiner Helper `double_option` in `api/mod.rs`), da `COALESCE` hier nicht reicht.
- `POST /api/tasks`: optionale `max_runtime_secs`, `max_cost_usd`, `max_tokens`, `max_turns` (gleiche Validierung). `PATCH /api/tasks/{id}` (nur wartende Tasks, bestehende Regel): dieselben Felder.
- `POST /api/tasks/{id}/retry`: setzt zusätzlich `transient_retries=0, not_before=NULL, failure_class=NULL`.
- Task-JSON enthält automatisch die neuen Spalten (`SELECT *`).
- `GET /api/limits`: `paused_reason` je Provider. Neu `POST /api/limits/{provider}/resume` → `clear_pause`, `wake`; 400 bei unbekanntem Provider.
- `PUT /api/credentials/{provider}` (bestehend, `api/credentials.rs:35`): ruft nach erfolgreichem Speichern `clear_pause` auf, falls `paused_reason` gesetzt ist.
- `GET /api/meta`: `provider_capabilities: {claude:{structured_output,max_turns,max_cost,live_usage}, …}` und `cli_versions: {claude:"2.1.319", codex:"0.167.0", gemini:"0.67.0"}` aus `agent::CLI_VERSIONS`.
- Neue Notification-Kinds: `provider_paused`, `limit_exceeded`.

### 5.7 Runner (`queued-runner`)

- `RunnerConfig.kill_grace_secs: u64` (Env `RUNNER_KILL_GRACE_SECS`, Default 15; `for_tests` = 2).
- `exec::run_inner`:
  - schreibt `queue_files` nach `<workspace>/.queue/` (Verzeichnis anlegen; ungültiger Name → `Fail`).
  - Ausgabe-Zeitstempel: beide Reader-Tasks aktualisieren `last_output` (`Arc<Mutex<tokio::time::Instant>>`); zusätzlicher `select!`-Zweig: Intervall 250 ms, `stalled = true` wenn `idle_timeout_secs` gesetzt und `now − last_output ≥ idle`.
  - Stop-Sequenz (Timeout, Cancel, Stall) in `async fn stop(…)`: 1) SIGINT – `Engine::None`: `kill -INT -- -<pgid>`; Podman/Docker: `<bin> kill --signal SIGINT <name>`; 2) bis `kill_grace_secs` weiter auf `child.wait()` warten, Reader laufen weiter (Frames nach SIGINT werden gestreamt); 3) danach SIGKILL – `Engine::None`: `kill -KILL -- -<pgid>` + `child.kill()`; Container: `<bin> rm -f <name>` (bisheriger Cleanup in `run`, jetzt auch bei `stalled`), `killed = true`.
  - Pure Helper `stop_command(engine, name, pid, Signal) -> (String, Vec<String>)` für Unit-Tests der Argumente.
- `main.rs`: Env lesen; `deploy/runner.env.example` dokumentiert `RUNNER_KILL_GRACE_SECS`.
- App: `RunnerClient::exec` HTTP-Timeout = `timeout_secs + idle-unabhängig + kill_grace_max(60) + 120` → konkret `timeout_secs + 180`.

### 5.8 Mock-Provider (`agent/mock.rs`, Skript erweitert)

Bestehendes Verhalten bytegenau erhalten (Default-Pfad: gleiche Usage 1200/150/800 – Test „1350 counted tokens per run“). Neue Schlüsselwörter (vor den bestehenden `case`-Zweigen, Review-Zweig bleibt zuerst):

| Keyword | Verhalten |
|---|---|
| `MOCK_TRANSIENT` | enthält der Prompt `transient provider error` → normaler Erfolgsweg; sonst `{"type":"retry","category":"overloaded","attempt":1,"max":3,"delay_ms":10,"status":529}`, dann `{"type":"error","category":"overloaded","text":"API Error: 529 overloaded"}`, `exit 1` |
| `MOCK_TRANSIENT_ALWAYS` | immer wie oben (für Erschöpfung) |
| `MOCK_AUTH` | `{"type":"error","category":"authentication_failed","text":"Invalid API key"}`, `exit 1` |
| `MOCK_STRUCTURED` | schreibt `result.json` mit `status:"failed"`, emittiert aber `{"type":"structured","value":{"status":"done","summary":"structured ok","handoff":"h","questions":[],"follow_up_tasks":[]}}` (beweist Vorrang) |
| `MOCK_STRUCTURED_BAD` | `{"type":"structured","value":{"status":42}}` + gültige `result.json` „file ok“ (beweist Fallback) |
| `MOCK_TOKENS` | zwei Usage-Zeilen (je 1200/150, `cost_usd:0.006`) mit `sleep 2` dazwischen und danach, dann normales Ende |
| `MOCK_TURNS` | ist `$QUEUE_MAX_TURNS` gesetzt → `{"type":"limit","kind":"turns"}`, `exit 1`; sonst Erfolg |
| `MOCK_TRAP` | `trap 'echo {usage 1200/150}; echo {"type":"final","text":"interrupted cleanly"}; exit 130' INT`; `sleep 30 & wait $!` (steht nach der Start-Usage-Zeile → Gesamt 2400 Input-Tokens) |
| `MOCK_STDERR429` | wie `MOCK_FAIL`, zusätzlich `echo "npm ERR! HTTP 429 quota exceeded" >&2; exit 1` (Negativtest S2) |
| `MOCK_REVIEW_LIMIT` | nur im Review-Zweig (`*"strict code reviewer"*` und Auftrag enthält das Keyword): `{"type":"error","category":"rate_limit","text":"usage limit reached\|<epoch+120>"}`, `exit 1` |

`MOCK_SLOW` (bestehend) dient für Stall- und Laufzeittests.

### 5.9 Agent-Image & Versionen

- `docker/agent.Dockerfile`: `ARG CLAUDE_CODE_VERSION=2.1.319`, `ARG CODEX_VERSION=0.167.0`, `ARG GEMINI_CLI_VERSION=0.67.0` (die lokal mit `--help`/Bundle geprüften Versionen). Nach dem Install: `RUN claude --version | grep -F "$CLAUDE_CODE_VERSION" && codex --version | grep -F "$CODEX_VERSION" && gemini --version | grep -Fx "$GEMINI_CLI_VERSION"` (Build bricht bei Drift ab). Kein `latest`.
- `agent::CLI_VERSIONS: [(&str,&str);3]` – Unit-Test liest `docker/agent.Dockerfile` per `include_str!("../../../../docker/agent.Dockerfile")` und prüft Gleichheit der ARG-Defaults sowie Abwesenheit von `latest`.

### 5.10 UI (deutsch, mobile Safe-Area-Regeln)

- `api.ts`: Typen `Task` (+ `max_*`, `failure_class`, `transient_retries`, `not_before`), `Project` (+ Limits), `Limit` (+ `paused_reason`), `Meta` (+ `provider_capabilities`, `cli_versions`). Helper `FAILURE_LABEL: Record<FailureClass,string>` (z. B. `provider_transient` → „Provider gestört“, `infra` → „Infrastruktur“, `stalled` → „Stillstand“, `provider_auth` → „Anmeldung/Abrechnung“, `config` → „Konfiguration“, `limit_runtime` → „Limit: Laufzeit“, `limit_cost` → „Limit: Kosten“, `limit_tokens` → „Limit: Tokens“, `limit_turns` → „Limit: Turns“, `task` → „Task-Fehler“, `rate_limit` → „Abo-Limit“), `failureTag(t)`, `retryHint(t)` („neuer Versuch ab 14:05 (2/5)“), `secsToMin`/`minToSecs`, `limitChip` berücksichtigt `paused_reason`.
- `Projects.tsx` → `ProjectSettings`: neuer Abschnitt `<h4>Limits pro Auftrag</h4>` mit Feldern „Max. Laufzeit (Min.)“, „Stillstand nach (Min., 0 = aus)“, „Max. Kosten (USD)“, „Max. Tokens“, „Max. Turns“, leer = Standard; unter Kosten/Turns ein Hinweis aus `meta.provider_capabilities` („wird von Codex/Gemini nicht erzwungen“). `inputMode` numeric/decimal; gespeichert in Sekunden.
- `Queue.tsx` → Composer-Optionen: dieselben vier Task-Overrides (ohne Stall). `TaskRow`: Tag mit `FAILURE_LABEL` für `failure_class` ≠ null und ≠ `task`; für `queued` mit `not_before > now` Meta-Text „wartet bis HH:MM“.
- `TaskDrawer.tsx`: Block „Abbruchgrund“ (Klasse + `transient_retries` + `not_before`) und gesetzte Limits; Event-Kinds `retry`, `guard`, `contract` erhalten CSS-Klassen (`styles.css`: `.ev-retry`, `.ev-guard`, `.ev-contract`).
- `SettingsView.tsx` → Budget-Zeile: Tag `pausiert: <paused_reason>` (`.tag.err`) + Button „Pause aufheben“ (`POST /limits/{p}/resume`); Anzeige `cli_versions` bei den Provider-Zugängen.
- Mobile: neue Inputs nutzen bestehende Input-Styles (≥ 16 px), Zeilen `row wrap` – kein horizontales Scrollen.

### 5.11 Konfiguration (`config.rs`, `deploy/queued.env.example`)

Neu: `STALL_TIMEOUT_MIN` (Default 20; 0 = aus), `TRANSIENT_MAX_RETRIES` (5), `TRANSIENT_BACKOFF_BASE_SECS` (30), `TRANSIENT_BACKOFF_MAX_SECS` (900). `TASK_TIMEOUT_MIN` bleibt Default-Laufzeit. `Config::for_tests`: `stall_timeout_secs: 0` (aus, damit `MOCK_SLOW`-Cancel-Tests unverändert), `transient_max_retries: 2`, `backoff_base_secs: 1`, `backoff_max_secs: 2`. Intern speichert `Config` Sekunden (`stall_timeout_secs`).
Default 20 min Stall ist bewusst groß: Claude streamt während eines langen Bash-Tool-Aufrufs (z. B. `cargo build`) **nichts** (Annahme A4).

---

## 6. Datei-für-Datei-Änderungsliste

| Datei | Änderung |
|---|---|
| `backend/crates/app/migrations/0003_cli_contract_limits.sql` | **neu**, s. 5.1 |
| `backend/crates/common/src/lib.rs` | `ExecRequest.idle_timeout_secs`, `.queue_files`; `ExecFrame::Exit.stalled`, `.killed`; `valid_queue_file_name`; Tests |
| `backend/crates/runner/src/lib.rs` | `RunnerConfig.kill_grace_secs` (+ `for_tests`); Test `exec_streams_ndjson_over_http` um neue Felder ergänzt |
| `backend/crates/runner/src/exec.rs` | queue_files schreiben, Stall-Erkennung, SIGINT→Grace→SIGKILL, `stop_command`, Cleanup auch bei Stall; Tests (5 neu, s. 8) |
| `backend/crates/runner/src/main.rs` | `RUNNER_KILL_GRACE_SECS` |
| `backend/crates/app/src/agent.rs` | **entfernt**, ersetzt durch `agent/{mod,claude,codex,gemini,mock,classify}.rs` (s. 5.3); bestehende Tests wandern mit und bleiben inhaltlich erhalten (angepasst an neue Event-Namen) |
| `backend/crates/app/src/limits.rs` | **neu**: `EffectiveLimits`, `effective`, `backoff`, `Guard` + Unit-Tests |
| `backend/crates/app/src/lib.rs` | `pub mod limits;` |
| `backend/crates/app/src/models.rs` | `Task`/`Project` neue Felder; `FailureClass` (mit `as_str`, `from_str`); `RESULT_SCHEMA`, `REVIEW_SCHEMA`; Tests |
| `backend/crates/app/src/runner.rs` | `RunOutcome` erweitert, `agent_run` ohne `?`-Propagation von Exec-Fehlern, `classify_run` (rein, Unit-Tests), `Disposition`-Abarbeitung, Review-Fixes (S15), `RunError`, Limits/Guard, Contract-Laden, Fix-Vererbung der Limits |
| `backend/crates/app/src/queue.rs` | `NewTask` + Limits; `insert_task` bindet sie; `requeue`; `finish(…, failure_class)` |
| `backend/crates/app/src/worker.rs` | `claim_next` mit `not_before`; `recover` zählt Retry |
| `backend/crates/app/src/budget.rs` | `paused_reason`, `set_paused`, `clear_pause`; `blocked` |
| `backend/crates/app/src/config.rs` | neue Env-Variablen, `for_tests` |
| `backend/crates/app/src/runner_client.rs` | Exec-Timeout `+180` |
| `backend/crates/app/src/prompt.rs` | nur Test-Fixtures (`Task`/`Project`-Literale) um neue Felder ergänzen; Prompt-Text unverändert |
| `backend/crates/app/src/api/projects.rs` | Limits in `UpdateProject` (+ Validierung), `create` unverändert (Default NULL) |
| `backend/crates/app/src/api/tasks.rs` | Limits in `CreateTask`/`UpdateTask`; `retry` setzt Zähler zurück |
| `backend/crates/app/src/api/mod.rs` | `meta` erweitert; Route `POST /limits/{provider}/resume`; `double_option`-Helper |
| `backend/crates/app/src/api/credentials.rs` | `put` → `clear_pause` |
| `backend/crates/app/tests/common/mod.rs` | `start_with(pool, impl FnOnce(&mut Config))`; `FlakyProxy` (axum-Reverse-Proxy vor dem Runner, bricht den ersten `/v1/exec`-Stream nach dem ersten Frame ab, Zähler abfragbar) |
| `backend/crates/app/tests/failures.rs` | **neu**: Integrationstests s. 8 |
| `backend/crates/app/tests/cli_contract.rs` | **neu**: Contract-Tests gegen Fixtures (keine DB nötig) |
| `backend/crates/app/tests/fixtures/cli/claude-2.1.319/*.jsonl`, `codex-0.167.0/*.jsonl`, `gemini-0.67.0/*.jsonl`, `fixtures/cli/PROVENANCE.md` | **neu**: Fixtures + Herkunft je Datei („aufgezeichnet mit … am …“ bzw. „konstruiert aus Schema in Binary/Bundle Version X, Feldnamen belegt durch …“) |
| `backend/crates/app/tests/api.rs` | bestehende Tests unverändert lassen; nur dort anpassen, wo Strukturfelder/Erwartungen sich zwingend ändern (z. B. `failure_class` zusätzlich prüfen ist erlaubt, Erwartungen abschwächen nicht) |
| `docker/agent.Dockerfile` | Versions-Pins + Versions-Check |
| `deploy/queued.env.example`, `deploy/runner.env.example` | neue Variablen dokumentiert |
| `frontend/src/api.ts`, `api.test.ts`, `Projects.tsx`, `Queue.tsx`, `TaskDrawer.tsx`, `SettingsView.tsx`, `styles.css` | s. 5.10 |
| `e2e/scripts/stack.sh` | `TRANSIENT_BACKOFF_BASE_SECS=1 TRANSIENT_BACKOFF_MAX_SECS=2 TRANSIENT_MAX_RETRIES=2` (App), `RUNNER_KILL_GRACE_SECS=3` (Runner) |
| `e2e/tests/system/api.spec.ts`, `e2e/tests/ui/queue.spec.ts`, `e2e/tests/ui/mobile.spec.ts` | neue Tests s. 8 |
| `README.md` | Mock-Keywords, Ablauf-Diagramm (Fehlerklassen), Tabelle Provider-Fähigkeiten, „Bekannte Grenzen“ aktualisiert (Codex/Gemini-Einschränkungen, Review-Phasen-Retry) |

---

## 7. Akzeptanzkriterien (nummeriert, prüfbar)

**CLI-Aufrufe & Versionen**
- **AK01** Claude-Invocation enthält exakt `-p`, `--output-format stream-json`, `--verbose`, `--permission-mode bypassPermissions`, `--permission-prompts none`, `--json-schema <RESULT_SCHEMA>` (Review: `REVIEW_SCHEMA`), **nicht** `--dangerously-skip-permissions`; bei Limits `--max-turns N` und `--max-budget-usd X` (X = Restbudget); ohne Limits keine dieser beiden Flags. Kein Secret in argv.
- **AK02** Codex-Invocation enthält `exec --json --skip-git-repo-check --dangerously-bypass-approvals-and-sandbox --output-schema .queue/result.schema.json -`, `queue_files` enthält das Schema; Review-Variante nutzt `review.schema.json`.
- **AK03** Gemini-Invocation setzt `GEMINI_CLI_TRUST_WORKSPACE=true`, schreibt immer gültiges JSON nach `.gemini/settings.json`; mit Turn-Limit enthält es `model.maxSessionTurns = N`, bei `oauth_creds` zusätzlich die bisherigen Auth-Keys.
- **AK04** `docker/agent.Dockerfile` pinnt `2.1.319` / `0.167.0` / `0.67.0`, enthält kein `latest`, prüft die Versionen im Build; die Pins stimmen mit `agent::CLI_VERSIONS` überein; für jede Version existiert ein Fixture-Verzeichnis.

**Result-Contract**
- **AK05** `RESULT_SCHEMA`/`REVIEW_SCHEMA` sind gültiges JSON, `additionalProperties:false` und alle Properties `required` (rekursiv), ein Beispielobjekt nach Schema deserialisiert in `AgentResult`/`ReviewResult`.
- **AK06** Vorrang: structured output > `.queue/result.json` > kein Contract; ungültiger structured output fällt auf die Datei zurück; Quelle steht als Event `contract` im Verlauf.
- **AK07** Fehlt bei `claude`/`codex` jeder Contract, endet der Run als `task`-Fehler „Ergebnis-Contract fehlt“ (Fix-Task); bei `gemini`/`mock` bleibt der Exit-Code-Fallback.

**Parser / Contract-Tests**
- **AK08** Für jede gepinnte CLI-Version existieren Fixtures mindestens für: Erfolg mit Tool-Nutzung, Usage, Retry-Hinweis, terminaler transienter Fehler, Rate-Limit, Auth-Fehler; zusätzlich Claude: structured output, `error_max_turns`, `error_max_budget_usd`, `rate_limit_event` rejected/allowed_warning, `modelUsage` mit Subagent; Codex: structured final message, „Reconnecting…“-Error, „You’ve hit your usage limit.“; Gemini: `error` severity warning, `FatalTurnLimitedError`. Jede Fixture hat eine erwartete Event-/Klassenfolge, die `cli_contract.rs` prüft. `PROVENANCE.md` nennt für jede Datei Herkunft (aufgezeichnet vs. aus Schema konstruiert).
- **AK09** Claude-Usage: `modelUsage`-Summe hat Vorrang vor `usage`; `assistant.message.usage` wird pro `message.id` dedupliziert als Delta gemeldet (doppelte Zeilen derselben id zählen einmal).
- **AK10** Textfallback: stderr wird nicht mehr klassifiziert (Exit 1 mit „HTTP 429“/„quota“ nur auf stderr → `task`, kein Provider-Pause); Text/Tool-Ausgaben mit „rate limit“ lösen nichts aus; Codex-„Reconnecting… 2/5“ ist `Retry`, kein Fehler; Gemini-`error` mit severity warning ist kein Fehler.
- **AK11** Kategorie-Mapping gemäß Tabelle 5.3.4 vollständig unit-getestet (jede der 12 Claude-Kategorien).

**Fehlerklassen / Re-Queue**
- **AK12** Transienter Provider-Fehler (`MOCK_TRANSIENT`): Task wird `queued` mit `failure_class=provider_transient`, `transient_retries=1`, `attempt=0`, `not_before` ≈ jetzt + 1 s; kein Fix-Task entsteht; zweiter Lauf → `done`, `failure_class=NULL`; Event `retry` im Verlauf.
- **AK13** Scheduler startet keinen Task vor `not_before` (Integration: `not_before` per SQL in die Zukunft setzen → bleibt ≥ 2 s `queued`, danach läuft er).
- **AK14** Erschöpfung (`MOCK_TRANSIENT_ALWAYS`, Max 2): nach dem 3. Lauf `blocked`, `failure_class=provider_transient`, Notification `blocked`, kein Fix-Task, `attempt=0`.
- **AK15** Infra-Fehler (FlakyProxy bricht den ersten Exec-Stream ab): `failure_class=infra`, re-queued, kein Fix, `attempt=0`; zweiter Lauf `done`; der bis zum Abbruch gesehene Verbrauch steht in `usage_log`.
- **AK16** `worker::recover` erhöht `transient_retries`, setzt `failure_class=infra`; die Kontext-Notiz steht nach zweimaligem Recover genau einmal im Kontext. Gleiches für die Rate-Limit-Notiz.
- **AK17** Auth-Fehler (`MOCK_AUTH`): Provider `blocked` mit `paused_reason`, Task `queued` mit `failure_class=provider_auth`, `attempt=0`, Notification `provider_paused`; nichts läuft, bis `POST /api/limits/mock/resume` (bzw. Credential-PUT für einen echten Provider) die Pause aufhebt; danach läuft der (per PATCH entschärfte) Task `done`.
- **AK18** Konfigurationsfehler (unbekannter Credential-Typ in DB bzw. `model_not_found`): `blocked`, `failure_class=config`, kein Fix.
- **AK19** Echte Task-Fehler unverändert: `failing_verification_creates_fix_tasks_until_blocked`, Review-Ablehnung und `MOCK_FAIL` erzeugen Fix-Tasks wie bisher; `failure_class=task`. Alle bestehenden Tests in `tests/api.rs` bleiben grün, ohne abgeschwächte Assertions.
- **AK20** Review-Run mit Rate-Limit pausiert den Review-Provider (`limited_until` gesetzt) und protokolliert „Review übersprungen: rate_limit“.

**Per-Task-Limits**
- **AK21** Effektive Limits: Task > Projekt > Global (Unit-Tests für alle Kombinationen); Fix-Tasks erben Task-Level-Limits, Follow-ups nicht.
- **AK22** Laufzeit: Task `MOCK_SLOW` mit `max_runtime_secs=2` → `blocked`, `failure_class=limit_runtime`, kein Fix, Ende < 2 + Grace(2) + 4 s; Teil-Commit vorhanden.
- **AK23** Stall: Projekt `stall_timeout_secs=2`, `MOCK_SLOW` → zweimal `stalled`-Re-Queue, dann `blocked` mit `failure_class=stalled`; Exit-Frame meldet `stalled=true`.
- **AK24** Tokens live: Task `MOCK_TOKENS` mit `max_tokens=2000` → Lauf wird nach der zweiten Usage-Zeile beendet (kein Event „finished“), `blocked`, `failure_class=limit_tokens`, `usage` des Tasks ≥ 2700 counted, Event `guard`.
- **AK25** Kosten live: `MOCK_TOKENS` mit `max_cost_usd=0.01` → `limit_cost`; Claude-Fixture `error_max_budget_usd` → `limit_cost`.
- **AK26** Turns: `MOCK_TURNS` mit `max_turns=1` → `limit_turns`; ohne Limit → `done` (beweist Durchreichen); Claude-`error_max_turns`- und Gemini-`FatalTurnLimitedError`/Exit-53-Fixtures → `limit_turns`.
- **AK27** Nachträgliche Token-Prüfung (codex/gemini): Unit-Test von `classify_run` – Überschreitung erzeugt Warn-Event/Notification `limit_exceeded`, Ergebnis bleibt `done`.

**Abbruch**
- **AK28** Runner sendet bei Cancel/Timeout/Stall zuerst SIGINT (Prozessgruppe bzw. `podman|docker kill --signal SIGINT <name>`), wartet `kill_grace_secs`, dann SIGKILL bzw. `rm -f`; nach SIGINT erzeugte Ausgabe wird gestreamt; `killed` korrekt gesetzt (Unit: Trap-Skript → `killed=false` + Trap-Ausgabe; `trap '' INT` → `killed=true`, Dauer ≥ Grace).
- **AK29** Nutzer-Abbruch eines `MOCK_TRAP`-Tasks: Status `cancelled` < 10 s, Task-`usage.input_tokens = 2400` (Trap-Usage 1200 + Start-Usage 1200) und Verlauf enthält „interrupted cleanly“ – die Usage steht in `usage_log`.

**API / UI / System**
- **AK30** API-Validierung: Projekt-/Task-Limits außerhalb der Bereiche → 400; `null` setzt zurück; Viewer → 403; `retry` setzt `transient_retries`, `not_before`, `failure_class` zurück; `/api/meta` liefert `provider_capabilities` und `cli_versions`.
- **AK31** UI desktop (E2E): Limits im Projekt speichern und nach Reload wieder sehen; `MOCK_TOKENS`-Auftrag mit Projekt-Token-Limit zeigt Tag „Limit: Tokens“ und im Drawer den Block „Abbruchgrund“; `MOCK_TRANSIENT` zeigt zwischenzeitlich „Provider gestört“/„wartet bis“ (oder Retry-Event) und endet `done` ohne „Fix #1“; `MOCK_AUTH` zeigt in Einstellungen „pausiert: …“ und „Pause aufheben“ hebt sie auf.
- **AK32** UI mobil (iphone + landscape): Projekt-Einstellungen mit neuem Limit-Abschnitt – Inputs ≥ 16 px Schrift, kein horizontales Scrollen, nichts unter Safe-Areas.
- **AK33** System (getrennte Prozesse): transienter Fehler wird über die Prozessgrenze ohne Fix re-queued und erledigt; `MOCK_TRAP`-Abbruch über die Prozessgrenze liefert Usage > 0.
- **AK34** Qualität: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (mit Postgres), `npm run build && npm test`, `npx playwright test` (alle Projekte) grün; Befehlsausgaben im Testbericht.

Gesamt: **34 Akzeptanzkriterien**.

---

## 8. Testplan je Kriterium

Ebenen: **U** = Rust-Unit (`--lib`), **C** = Contract-Test `tests/cli_contract.rs` (ohne DB), **I** = Integration (`tests/failures.rs`/`api.rs`, Postgres, Runner in-process), **S** = Playwright `system`, **E** = Playwright `desktop`, **M** = Playwright `iphone`/`iphone-landscape`, **F** = Frontend-Vitest.

| AK | Ebene | Test (Name / Vorgehen) |
|---|---|---|
| 01 | U | `agent::claude::tests::invocation_flags_with_and_without_limits` (Flag-Fenster prüfen, Negativtest auf `--dangerously-skip-permissions`, Secret nicht in argv) |
| 02 | U | `agent::codex::tests::invocation_uses_output_schema_file` |
| 03 | U | `agent::gemini::tests::settings_json_contains_turn_limit_and_auth` (Parse mit serde, nicht String-Vergleich) |
| 04 | U | `agent::tests::dockerfile_pins_match_cli_versions`; C: `fixture_dirs_exist_for_pinned_versions` |
| 05 | U | `models::tests::schemas_are_strict_and_match_structs` (rekursive Prüfung `required == keys(properties)`, `additionalProperties==false`) |
| 06 | U+I | U: `runner::tests::contract_precedence`; I: `structured_output_wins_over_file` (`MOCK_STRUCTURED` → summary „structured ok“, Event „structured output“), `invalid_structured_falls_back_to_file` (`MOCK_STRUCTURED_BAD` → „file ok“) |
| 07 | U | `runner::tests::missing_contract_is_task_failure_for_schema_providers` (classify_run mit Capabilities claude vs. gemini) |
| 08 | C | je Fixture ein Test `claude_<szenario>`, `codex_<szenario>`, `gemini_<szenario>`; Helper `run_fixture(provider, path) -> (Vec<AgentEvent>, Option<FailureClass>)` |
| 09 | U+C | `claude_usage_dedupes_message_ids`, `claude_model_usage_includes_subagents` |
| 10 | U+I | U: `text_fallback_ignores_stderr_and_tool_output`, `codex_reconnect_is_retry`, `gemini_warning_is_not_error`; I: `stderr_429_does_not_pause_provider` (Mock-Variante: verify-unabhängiger Mock-Lauf `MOCK_FAIL` + `echo "HTTP 429 quota" >&2` – dafür Mock-Zeile `*MOCK_STDERR429*` ergänzen; erwartet Fix-Task, Provider nicht `blocked`) |
| 11 | U | `classify::tests::category_mapping_table` (Tabelle 5.3.4 vollständig) |
| 12 | I | `transient_provider_error_requeues_without_fix_attempt` |
| 13 | I | `not_before_delays_claim` |
| 14 | I | `transient_retries_exhaust_to_blocked` |
| 15 | I | `runner_stream_interruption_is_infra_and_retried` (FlakyProxy via `start_with`) |
| 16 | I | `recover_counts_retry_and_dedupes_note` (Task per SQL auf `running`, `worker::recover` zweimal aufrufen) |
| 17 | I | `auth_failure_pauses_provider_until_resumed` |
| 18 | I+U | I: `unknown_credential_kind_blocks_as_config` (Credential-Row mit `kind='bogus'` für `claude` per SQL einfügen; Provider `claude` ist in `providers()`; Task blockt sofort, ohne CLI-Start); U: `model_not_found_is_config` |
| 19 | I | bestehende Tests `tests/api.rs` + Zusatz-Assertion `failure_class=="task"` in einem neuen Test `task_failures_keep_fix_loop_and_class` |
| 20 | I | `review_rate_limit_pauses_review_provider` (Projekt `review_provider=mock`, Mock-Review-Zweig um `MOCK_REVIEW_LIMIT` im Auftragstext erweitern – Review-Prompt enthält den Auftrag) |
| 21 | U+I | U: `limits::tests::effective_precedence`; I: `fix_tasks_inherit_task_limits` |
| 22 | I | `runtime_limit_blocks_without_fix` (Zeitmessung) |
| 23 | U+I | U (Runner): `exec::tests::detects_stall`; I: `stall_requeues_then_blocks` |
| 24 | I | `token_limit_stops_run_live` |
| 25 | I+C | I: `cost_limit_stops_run_live`; C: `claude_max_budget` |
| 26 | I+C | I: `turn_limit_is_passed_to_cli` (zwei Tasks: mit/ohne Limit); C: `claude_max_turns`, `gemini_turn_limit` |
| 27 | U | `runner::tests::post_hoc_token_overrun_warns_only` |
| 28 | U | `exec::tests::sigint_then_grace_lets_process_flush`, `exec::tests::sigkill_after_grace_when_int_ignored`, `exec::tests::stop_commands_for_engines` (Podman/Docker/None-Argumente) |
| 29 | I | `cancel_sends_sigint_and_records_usage` |
| 30 | I+F | I: `limit_fields_validation_and_reset`, `retry_resets_transient_state`, `meta_has_capabilities`; F: `failureTag`, `retryHint`, `limitChip` mit `paused_reason`, `secsToMin` |
| 31 | E | `queue.spec.ts`: „Projekt-Limits speichern und Token-Limit greift“, „transienter Fehler ohne Fix-Versuch“, „Provider-Pause anzeigen und aufheben“ |
| 32 | M | `mobile.spec.ts`: „Projekt-Einstellungen mit Limits: Inputs ≥ 16px, kein horizontales Scrollen“ |
| 33 | S | `api.spec.ts`: „transient error is retried across processes without fix task“, „SIGINT cancel flushes usage across processes“ |
| 34 | – | Befehlsausgaben aller Gates im `WP01-test.md` |

Hinweise für Umsetzung/Test:
- Integrationstests mit Zeitgrenzen über `start_with` konfigurieren (Backoff 1 s/Max 2 s, Max-Retries 2, Stall default aus); Runner `kill_grace_secs = 2` via `RunnerConfig::for_tests`.
- `wait_status` hat 30 s Deadline – alle neuen Szenarien müssen darunter bleiben (Stall-Szenario ≈ 3 × (2 s + ≤2 s) + 1 + 2 s).
- Fixtures: Wo möglich echte Aufnahmen. Durchführbar ohne Zugangsdaten: Auth-Fehler-Streams aller drei CLIs (`env -i HOME=$(mktemp -d) PATH=… claude -p --output-format stream-json --verbose "x"`, analog `codex exec --json`, `gemini -o stream-json -p x`), dabei sicherstellen, dass **keine** echten Credentials der Umgebung durchgereicht werden. Alle übrigen Fixtures aus den im Binary/Bundle der gepinnten Version belegten Schemas konstruieren und in `PROVENANCE.md` exakt so kennzeichnen. Keine Behauptung „live getestet“.

---

## 9. Risiken

| # | Risiko | Gegenmaßnahme |
|---|---|---|
| R1 | Claude-Verhalten nach SIGINT im Container (Weiterleitung durch `--init`/catatonit bzw. tini an den Kindprozess) ist hier nicht live verifiziert | Unit-Test der Kommandos; Grace + SIGKILL als harte Rückfallebene; falls `dockerd` startbar, manueller Docker-Test im Testbericht dokumentieren, sonst als unverifiziert ausweisen |
| R2 | Fixture-Drift: konstruierte Fixtures bilden reale Ausgaben unvollständig ab | Versionen gepinnt, Herkunft dokumentiert, Parser tolerant (unbekannte Typen ignorieren), Auth-Fehler-Streams real aufgezeichnet |
| R3 | `GEMINI_CLI_TRUST_WORKSPACE=true` lässt Projekt-`.gemini/settings.json`/MCP aus dem Repo wirken | Claude lädt Repo-Hooks unter `-p` bereits heute (Doku); Härtung explizit WP03 zugeordnet; im README „Bekannte Grenzen“ vermerkt |
| R4 | Live-Token-Guard für Claude basiert auf `assistant.message.usage` (Annahme A3); fehlen die Felder, greift nur die Gesamt-Usage am Ende | Fallback: Kosten nativ per `--max-budget-usd`; Token-Grenze dann nachträglich wie bei Codex/Gemini (Warnung); Contract-Fixture deckt den Fall ohne Usage ab |
| R5 | Stall-Fehlalarm bei langen Tool-Aufrufen ohne Streaming | Default 20 min, Projekt-Override, 0 = aus; `stalled` ist re-queue-fähig, nicht sofort `blocked` |
| R6 | Strict-Schema (alle Felder required) erhöht Ausgabelänge minimal / Modelle füllen leere Strings | Inhaltlich identisch zum bisherigen Contract; leere Strings werden wie bisher behandelt (`filter(!trim().is_empty())`) |
| R7 | Umfang: viele Dateien, Modulaufteilung `agent/` | Reine Funktionen (`classify_run`, `effective`, `Guard`, `backoff`, Parser) zuerst mit Unit-Tests; Integrationsszenarien danach |
| R8 | Kosten-Schätzungen der CLI (`total_cost_usd` clientseitig) weichen von Rechnung ab | In UI-Hinweis „Schätzung der CLI“ benennen |
| R9 | `--permission-prompts` benötigt ≥ 2.1.259; ältere Images würden mit „unknown option“ scheitern | Versions-Pin + Build-Check (AK04); ein solcher Start-Fehler wäre `ExecFrame`-Exit ≠ 0 mit stderr „unknown option“ → `task`-Fehler sichtbar |

## 10. Annahmen (zu prüfen bzw. als ungeprüft zu berichten)

- **A1** Claude schreibt `structured_output` auch im `stream-json`-`result`-Event (Doku zeigt es für `--output-format json`; Binary-Schema enthält `structured_output` als optionales Feld im Result). Fallback auf Datei existiert.
- **A2** `--max-budget-usd` greift auch bei Abo-OAuth (clientseitige Kostenschätzung existiert unabhängig vom Abrechnungsmodell).
- **A3** `assistant`-Events im `stream-json` enthalten `message.id` und `message.usage`.
- **A4** Während eines laufenden Bash-Tools emittiert Claude (ohne `--include-partial-messages`) keine Zeilen.
- **A5** Codex-Retry-Hinweise erscheinen als JSONL-`error`-Events mit „Reconnecting…“/„stream disconnected – retrying“; terminale Fehler als `turn.failed`. Codex schreibt mit `--output-schema` die finale Antwort als JSON-Text in das letzte `agent_message`-Item.
- **A6** Ohne Workspace-Trust führt Gemini im Headless-Modus keine Schreib-/Shell-Tools aus (Approval-Override im Bundle belegt, Tool-Ablehnung im Non-Interactive-Modus nicht live geprüft). `GEMINI_CLI_TRUST_WORKSPACE=true` behebt das (im Bundle belegt: `checkPathTrust`).
- **A7** Gemini reagiert auf SIGINT im Headless-Modus mit `result`-Event `FatalCancellationError` (Handler im Bundle vorhanden).
- **A8** `podman kill --signal SIGINT` / `docker kill --signal SIGINT` erreicht über `--init` den Agentenprozess.
