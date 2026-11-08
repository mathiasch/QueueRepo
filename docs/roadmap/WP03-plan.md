# WP03 – Gate- und Supply-Chain-Härtung: Plan

Stand: 2026-11-08, Branch `mvp` (Basis `ae37b03`, WP02 abgeschlossen). Ein Agent für Plan, Umsetzung, Test und Review.
Verbindlich: `work-packages.md` (Gemeinsame Regeln, WP03), Report P0.5/P0.6/P0.8/P0.9 und Abschnitt „Verify und Review sind nachweislich gamebar“, offene Funde WP01-N1, WP02-N1/N14, WP13-N10.

## 1. Ausgangslage (am Code geprüft)

- Ablauf eines Auftrags (`runner.rs::run_inner`): `init`/`begin` (Pull, WIP-Commit von Resten) → Setup (WP02) → Agent → `commit` → Verify → Review → optional `push` → `done`. Unterbrochene Läufe machen Teil-Commits („(unterbrochen: …)“).
- **Kein Secret-Scanning.** Weder Diffs noch Event-Logs werden geprüft. Was der Agent schreibt, wird committet und bei `auto_push` gepusht. Event-Inhalte werden nur für Provider-Schlüssel (WP13) und Setup-Secrets (WP02) geschwärzt.
- **Repo-Konfiguration wird ausgeführt:**
  - Claude Code 2.1.319 läuft mit `-p` ohne `--bare`: `.claude/settings.json` (Hooks, `env`, `apiKeyHelper`, Plugins) und `.mcp.json` des Repos wirken. `env.ANTHROPIC_BASE_URL` aus dem Repo würde den API-Schlüssel an einen fremden Host schicken.
  - Gemini CLI 0.67.0 läuft mit `GEMINI_CLI_TRUST_WORKSPACE=true` (ohne Vertrauen kein `--yolo`): Workspace-Settings mit Hooks, MCP-Servern, Tool-Discovery-Befehlen und `.env` wirken.
  - opencode 1.19.5 lädt `opencode.json(c)` und `.opencode/` (Plugins = Code) aus dem Repo (WP13-N10).
  - Codex 0.167.0 lädt `.codex/config.toml` laut Binary nur für vertraute Projekte; Queue markiert keins als vertraut.
- **Fund dieses Pakets (schwer): Der Runner führt Git auf dem Host in einem vom Agenten kontrollierten Repository aus.** `.git/config` und `.git/hooks` gehören dem Agenten. `core.fsmonitor`, `filter.*.clean` (über `.gitattributes`), Hooks (`post-commit` läuft trotz `--no-verify`, `post-merge` beim Pull), `diff.external`, `gpg.program` + `commit.gpgSign`, `credential.helper`, `url.*.insteadOf`, `http.*`-Proxy und eine umgebogene `remote.origin.url` führen Code **außerhalb der Sandbox als Runner-User** aus bzw. leiten das Git-Token um. Das hebelt WP02 (Container ohne Netz) komplett aus.
- **Reward-Hacking:** Verify läuft auf dem Stand, den der Agent gerade geschrieben hat. Tests, Verify-Skripte und CI-Konfiguration darf er ändern, Tests überspringen oder löschen. Der Review sieht nur Aufgabe, Zusammenfassung und Diff, ohne Rubrik, und nutzt standardmäßig denselben Anbieter.
- **WP01-N1:** Kann der Review nicht laufen (Provider-/Infra-Fehler, fehlender Zugang, pausiert, kein Ergebnis), wird er übersprungen und der Auftrag ist `done`, ungeprüft. Gleiches im Batch-Review (`complete_review`).
- **Auto-Push** geht direkt auf `git_branch`, auch wenn das der Default-Branch des Remotes ist.
- **WP02-N1:** Das Setup (mit Secrets) führt Repository-Code aus, den frühere Agentenläufe verändert haben können.

## 2. Ziele / Nicht-Ziele

**Ziele:**
1. Secret-Scanner (eigene Regeln + Entropie) in `queued-common`, genutzt vom Runner (Diffs vor Commit und Push, Leftover-WIP) und von der App (Events, Ergebnisse, Fehlertexte, Diff-Anzeige). Treffer werden **nie im Klartext** gespeichert oder ausgeliefert, nur maskiert mit Fingerabdruck (HMAC).
2. Freigabe-Mechanik („Gate“): neuer Status `held` („Wartet auf Freigabe“) mit strukturierten Funden, verständlicher Erklärung, Diff-Vorschau (maskiert) und Aktionen Freigeben / Verwerfen bzw. stufenspezifisch.
3. Repo-Konfiguration standardmäßig aus, pro Projekt durch die Eigentümerin freischaltbar; wenn freigeschaltet, erzwingen Änderungen an dieser Konfiguration eine Freigabe vor dem nächsten Lauf.
4. Git im Runner härten (Kommandozeilen-Overrides + Bereinigung von `.git/config` per Allowlist, keine Hooks, kein fsmonitor, Pull/Push nur gegen die URL der App, `.git` muss ein echtes Verzeichnis sein).
5. Anti-Reward-Hacking: geschützte Pfade (Änderung/Löschung bestehender Dateien → Freigabe), Erkennung neuer Skip-/Only-Marker und gelöschter Tests/Assertions, Verify nur auf Basis- oder freigegebenen Fassungen geschützter Dateien, Review mit Rubrik, Gate-Funden und standardmäßig anderem Anbieter.
6. Auto-Push nie direkt auf den Default-Branch ohne Freigabe (nicht abschaltbar).
7. WP01-N1: Review-Ausfall → automatischer Phasen-Retry (nur Review) mit Backoff, danach Freigabe statt `done`.
8. WP02-N1: Setup mit Secrets nur, wenn Setup-relevante Dateien seit dem letzten freigegebenen Stand unverändert sind, sonst Freigabe.
9. UI: Freigabe-Dialog im Drawer, Inbox/Queue-Sektion, Reiter „Schutz“ im Projekt, Option „Darf Tests ändern“ im Composer; WP00-Designsystem, axe, Safe-Areas.

**Nicht-Ziele:** externe Scanner (gitleaks/trufflehog) – nicht installiert, keine Downloads im Betrieb; eigener Regelsatz. Branch pro Auftrag/PR (WP04/WP05). Credential-Modell, Lizenz/ToS. Prüfung von Binärdateien auf Secrets.

## 3. Architektur

### 3.1 Scanner (`queued-common::secrets`)
- Regeln mit festem Präfix/Format: AWS Access Key ID (`AKIA|ASIA…`) und Secret im Zuweisungskontext, GitHub (`ghp_/gho_/ghu_/ghs_/ghr_`, `github_pat_`), GitLab (`glpat-`), Slack (`xox[abposr]-`, Webhook-URL), Stripe (`sk_live_/rk_live_`), Google API (`AIza…`), OpenAI (`sk-proj-…`, `sk-…T3BlbkFJ…`), Anthropic (`sk-ant-…`), npm (`npm_…`), PyPI (`pypi-AgEIcHlwaS5vcmc…`), Private-Key-Blöcke, JWT, URL mit Passwort (`scheme://user:pass@`), generische Zuweisung (`password|secret|token|api_key|… = "…"`) mit Shannon-Entropie ≥ Schwelle und Mindestlänge, Platzhalter-Filter (`example`, `xxx`, `${…}`, `<…>`, `changeme` …).
- `scan(text) -> Vec<Finding{rule, start, end}>`, `redact(text) -> String` (Treffer → `[geschwärzt: <Regel>]`), `Finding::masked()` zeigt nur das feste Präfix + Länge, `fingerprint(key, rule, value)` = HMAC-SHA256 gekürzt.
- Testwerte werden in Tests zur Laufzeit zusammengesetzt (kein echtes Secret, kein vollständiges Token im Quelltext).

### 3.2 Diff-Analyse (`queued-common::gate`)
Reine Funktionen auf einem geparsten Diff (`git diff -U0`-Format + `--name-status`): geschützte Pfade (Glob), Skip-/Only-Marker in hinzugefügten Zeilen (Rust `#[ignore]`, Python `@pytest.mark.skip/xfail`, `unittest.skip`, JS/TS `.skip(`/`.only(`/`xit(`/`xdescribe(`/`test.todo`, Go `t.Skip`, Java/Kotlin `@Disabled/@Ignore`, Ruby `skip`/`pending`, PHP `markTestSkipped`), gelöschte Testdateien, netto entfernte Assertions in Testdateien, neue Verifier-Konfiguration (`conftest.py`, `pytest.ini`, `jest.config.*` …), geänderte `scripts` in `package.json`. Ergebnis: `Vec<GateFinding>` (Art, Pfad, Zeile, maskierte Vorschau).

### 3.3 Runner
- `POST /v1/workspaces/{id}/inspect` `{base, protected, test_guard, secret_key_id?}` → `GateReport` (Funde, Datei-/Bytezahlen, `truncated`). Scant `HEAD..Arbeitsbaum` auf Secrets (Commits sind bereits gescannt) und `base..Arbeitsbaum` auf geschützte Pfade/Tests. Über der Grenze (16 MiB Diff) → Fund `too_large` (fail-closed).
- `POST …/hold {task}` → Arbeitsbaum als Commit-Objekt unter `refs/queue/held/<task>` (Elternteil HEAD), Arbeitsbaum auf HEAD zurück. `POST …/release {task, allow, message, author}` → Änderungen per 3-Wege-Merge auf aktuelles HEAD anwenden, committen (Scanner mit Allowlist), Ref löschen. `DELETE …/held/{task}` → verwerfen + `git reflog expire`/`prune` der unerreichbaren Objekte. `GET …/held/{task}/diff` → Diff des gehaltenen Stands.
- `commit`: Scanner auf `git diff --cached`; Treffer außerhalb der Allowlist → kein Commit, Index zurücksetzen, `CommitOutcome::Blocked{findings}`. `begin`: Reste mit Treffern werden nicht als WIP committet, sondern unter `refs/queue/quarantine/<zeit>` abgelegt (Warnung mit maskierten Funden).
- `push {sha?, allow_protected, allow_secrets}`: Default-Branch per `git ls-remote --symref <url> HEAD` (Fallback `main`/`master`); Ziel = Default-Branch ohne `allow_protected` → `needs_approval`. Scanner über `remote..sha`. Nur Fast-Forward, nie Force.
- Baselines `refs/queue/baseline/<name>`: `POST …/baseline/check {name, paths}` → geänderte Dateien seit Baseline (fehlt sie: auf HEAD setzen = Trust on first use), `POST …/baseline/approve {name}` → auf HEAD.
- **Git-Härtung:** jeder Git-Aufruf mit `-c core.hooksPath=/dev/null -c core.fsmonitor=false -c credential.helper= -c protocol.ext.allow=never -c protocol.file.allow=user -c commit.gpgSign=false -c tag.gpgSign=false -c core.sshCommand=false -c diff.external=` sowie `--no-ext-diff --no-textconv` bei Diffs; vor jeder Workspace-Operation `sanitize()`: `.git` muss echtes Verzeichnis sein, `.git/config` wird auf eine Allowlist (`core.repositoryformatversion|filemode|bare|logallrefupdates|ignorecase|precomposeunicode`, `extensions.*`, `branch.*.remote|merge`, `remote.origin.fetch`) neu geschrieben, `objects/info/alternates` entfernt. Pull und Push nur gegen die URL aus der App, nie gegen das vom Agenten änderbare `origin`.
- `ExecRequest.home_env`: Variablen, deren Wert ein Pfad im Wegwerf-HOME ist (für Geminis System-Settings).

### 3.4 Repo-Konfiguration der CLIs (Projekt-Schalter `trust_repo_config`, Standard aus)
| CLI | Aus (Standard) | Beleg |
|---|---|---|
| Claude Code 2.1.319 | `--setting-sources user --strict-mcp-config` | `--help` + Strings im installierten Binary |
| Gemini 0.67.0 | Workspace-Vertrauen bleibt nötig (sonst kein `--yolo`), aber System-Settings (`GEMINI_CLI_SYSTEM_SETTINGS_PATH`, höchste Priorität): `hooksConfig.enabled=false`, `tools.discoveryCommand/callCommand=""`, `mcp.serverCommand=""`, `advanced.ignoreLocalEnv=true`; `--allowed-mcp-server-names queue-none-<zufall>` | Bundle-Quelltext (`mergeSettings`, `loadCliConfig`, `isBlockedBySettings`, `findEnvFile`) |
| opencode 1.19.5 | `OPENCODE_DISABLE_PROJECT_CONFIG=1` | Strings im Binary (`ConfigPaths`, `fc()`) |
| Codex 0.167.0 | nichts nötig: Projekt-`.codex/config.toml` nur bei vertrauten Projekten; Queue vertraut nie → Schalter wirkt bei Codex nicht | Strings im Binary (nicht ausgeführt) |
Freigeschaltet: Flags entfallen; vor jedem Agentenlauf Baseline-Check `agent-config` über `.claude/**`, `.mcp.json`, `.gemini/**`, `opencode.json*`, `.opencode/**`, `.codex/**` → geändert ⇒ Freigabe.

### 3.5 Freigabe-Stufen (`tasks.gate`, Status `held`)
| Stufe | Auslöser | Freigeben | Verwerfen |
|---|---|---|---|
| `changes` | nach Agentenlauf, vor Commit: Secret im Diff oder im Event-Log des Laufs, geschützte Datei geändert/gelöscht, Test-Schwächung, Diff zu groß | Änderungen anwenden + committen (Fingerabdrücke erlaubt), weiter mit Verify/Review/Push | gehaltene Änderungen löschen, Auftrag `blocked`/`gate` |
| `setup` | vor Setup mit Secrets: Setup-Dateien seit Baseline geändert | Baseline = HEAD, Auftrag neu einreihen | `blocked`/`gate` |
| `config` | vor Agentenlauf bei freigeschalteter Repo-Konfiguration: Konfiguration seit Baseline geändert | Baseline = HEAD, neu einreihen | `blocked`/`gate` |
| `review` | Review nach Phasen-Retries nicht möglich | „Review wiederholen“ (nur Review) oder „Ohne Review übernehmen“ | `blocked`/`gate` (Commit bleibt, „Rückgängig“ möglich) |
| `push` | Auto-Push auf Default-Branch oder Secret im Push-Bereich | genau `snapshot_after` pushen, dann `done` + Folgeaufträge | `done` ohne Push |
Fortsetzung über den Worker: Freigabe setzt `gate.resume` und `queued`; `run_inner` springt an die Stufe. Freigeben/Verwerfen dürfen Auftragsersteller und Projekt-Eigentümer (Rolle ≥ Bearbeiter).

### 3.6 Review
Rubrik im Prompt (Aufgabe erfüllt? Tests nicht geschwächt/gelöscht/übersprungen? keine hart kodierten Ergebnisse/Sonderfälle für Tests? keine Secrets? kein Umgehen von Prüfungen? Umfang?), freigegebene Gate-Funde als Kontext. Ohne explizite Review-Wahl: anderer verfügbarer Anbieter (Legacy-Zugänge, nicht pausiert, Datenstandort erlaubt) vor demselben.

## 4. Datenmodell (Migration `0006_gate.sql`)
```sql
ALTER TABLE projects ADD COLUMN trust_repo_config BOOLEAN NOT NULL DEFAULT FALSE,
  ADD COLUMN protected_paths TEXT[],            -- NULL = Standardliste
  ADD COLUMN test_guard BOOLEAN NOT NULL DEFAULT TRUE,
  ADD COLUMN setup_guard BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE tasks ADD COLUMN gate JSONB, ADD COLUMN allow_test_changes BOOLEAN NOT NULL DEFAULT FALSE;
-- failure_class CHECK um 'gate' erweitern
```

## 5. Risiken / Grenzen
1. Regelbasierter Scanner: unbekannte Formate, Base64/verschlüsselte Secrets, Binärdateien werden nicht erkannt; Entropie-Regel kann Fehlalarme liefern (→ Freigabe).
2. Teil-Commits unterbrochener Läufe werden auf Secrets gescannt, aber geschützte Pfade/Tests erst am Ende des Auftrags (Gate prüft `snapshot_before..Arbeitsbaum`).
3. Setup-Dateiliste ist eine Heuristik (Lock-/Manifest-Dateien, im Setup-Skript genannte Pfade, eigene Pfade).
4. CLI-Flags nicht live mit Modell geprüft (keine Schlüssel, keine externen Calls); Gemini liest `.gemini/.env` in vertrauten Ordnern immer (setzt nur nicht gesetzte Variablen).
5. Gehaltene/quarantänierte Secrets liegen als Git-Objekte im Runner-Workspace (nie gepusht); Verwerfen entfernt sie per `prune`.
