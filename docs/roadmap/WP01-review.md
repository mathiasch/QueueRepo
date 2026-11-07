# WP01 – Strukturierte CLI-Integration & Fehlerklassifikation: Review

Status: Review (Reviewer-Agent), 2026-11-07. Basis: `git diff fe0e5f1..cdb0084` (Umsetzung + Testpass), vollständig gelesen, dazu Plan, Umsetzungs- und Testbericht.
Meine Änderungen liegen zum Teil im Snapshot-Commit `421075d` („review in progress“), den der Koordinator während des Reviews angelegt hat. Der Rest ist nicht committet.

## Urteil: **CHANGES-MADE**

Fünf blockierende Funde, alle behoben und jeweils mit Test belegt. Drei davon hat kein Test der Vorgänger erfasst: eine Symlink-Lücke im Runner, ein Panic im Reset-Parser und ungeprüfte Verbrauchswerte. Die offene Produktfrage aus dem Testbericht ist entschieden und umgesetzt. Die übrigen Funde sind nicht blockierend und unten dokumentiert.

## Blockierende Funde (alle behoben)

### R1 – Runner folgt Symlinks des Agenten in `.queue/` (Sicherheit, schwer)
- **Problem:** Das Arbeitsverzeichnis gehört dem Agenten. Er kann `.queue` oder `.queue/review.json` als Symlink auf eine Datei des Hosts anlegen.
  - **Schreiben (neu in WP01):** Vor dem Review-Lauf schreibt `write_queue_files` eine leere `review.json`, bei Codex außerdem `*.schema.json`. Das ging mit `tokio::fs::write` durch den Symlink hindurch. Damit konnte der Agent jede Datei, die der Runner-User schreiben darf, leeren oder überschreiben, etwa `runner.env`, Dateien anderer Workspaces oder Dotfiles. Das gilt auch mit Container-Engine, denn der Symlink wird auf dem Host aufgelöst.
  - **Lesen:** `Repo::queue_file` las `result.json`/`review.json` über Symlinks, also beliebige Host-Dateien, und reichte sie an die App weiter. Eine Größenbegrenzung gab es nicht.
- **Fix** (`runner/src/git.rs`): `write_queue_file`. Ist `.queue` ein Symlink oder kein Verzeichnis, wird es durch ein echtes Verzeichnis ersetzt. Ein vorhandener Eintrag wird entfernt, danach wird die Datei mit `create_new` (O_EXCL) angelegt, was keinem Symlink folgt, und geflusht. `queue_file` liest nur reguläre Dateien in einem echten `.queue`-Verzeichnis und höchstens 1 MiB, sonst `None`. Ein Fehler wäre falsch: Er würde als `infra` zu einer Re-Queue-Schleife führen.
- **Test:** `git::tests::queue_files_never_follow_symlinks` prüft Datei- und Verzeichnis-Symlink sowie eine übergroße Datei. Der Test hat außerdem einen Fehler in meinem ersten Fix gezeigt: Ohne `flush()` war die Datei sporadisch leer (1 von 3 Läufen rot). Danach 6/6 grün.

### R2 – Panic in `parse_reset`, Provider-Pause über Jahre (Korrektheit, schwer)
- **Problem:** Fehlertexte wie „try again in 999999999999999 days“ führten zu `Duration::days()` außerhalb des Wertebereichs bzw. zu einem `DateTime`-Überlauf, also zu einem Panic im Consumer von `agent_run`. Der Task blieb dann bis zum Neustart der App `running`. Auch plausible, aber weit entfernte Resets (Text, `resetsAt`, `|epoch`) pausierten den Provider unbegrenzt.
- **Fix** (`agent/classify.rs`, `runner.rs`): Die Rechnung mit den Teilwerten ist saturierend. Neu sind `MAX_RESET` (8 Tage) und `cap_reset`, die sowohl in `parse_reset` als auch in `pause_rate_limited` gelten, also für jede Reset-Quelle.
- **Test:** `classify::tests::absurd_resets_never_panic_and_are_capped`.

### R3 – Unplausible Verbrauchswerte aus dem CLI-Stream (Korrektheit/Missbrauch, mittel)
- **Problem:** Negative Token- und Kostenwerte wurden ungeprüft übernommen. Sie senkten den Task-Verbrauch, die Budgets und die Limits, mit ihnen ließ sich also jedes Limit umgehen. Extremwerte liefen in `Usage::add` bzw. in der Claude-Delta-Subtraktion über: Panic im Debug-Build, Wrap-around im Release-Build.
- **Fix:** `agent::n` liefert nur nicht-negative Ganzzahlen bis `MAX_REPORTED_TOKENS` (10^10, entspricht der API-Obergrenze für `max_tokens`). Andere Typen und Brüche zählen 0. `agent::usd` begrenzt Kosten auf 0 bis 10 000 USD. `Usage::add`, `budget::counted` und `EffectiveLimits::remaining` rechnen saturierend. `modelUsage` summiert über `Usage::add`.
- **Test:** `agent::tests::implausible_usage_values_are_clamped` deckt alle vier Parser ab, dazu Codex-Summen über 2000 maximale Turns und den saturierenden `add`.

### R4 – Offene Produktfrage: nachträgliche Limit-Überschreitung (Codex/Gemini) → **blockieren**
**Entscheidung:** Ein Auftrag, dessen Kosten- oder Token-Limit erst laut der finalen Verbrauchsmeldung überschritten ist, wird `blocked` (`limit_tokens`/`limit_cost`), genau wie beim Live-Abbruch. Das gilt auch mit gültigem `done`-Contract. Die Änderungen bleiben als Teil-Commit `(unterbrochen: limit_*)` im Diff des Auftrags erhalten. „Wiederholen“ startet mit frischem Budget.

**Begründung:**
1. **Eine Semantik für alle Provider.** Bisher wurde ein Claude- oder Mock-Auftrag über dem Limit blockiert, ein Codex-Auftrag über demselben Limit dagegen `done`. Ein Limit, dessen Wirkung vom Provider abhängt, ist keins.
2. **Das Limit soll weitere automatische Ausgaben stoppen.** Das ausgegebene Geld ist ohnehin weg. Mit „Ergebnis behalten“ hätte ein `failed`-Contract aber einen Fix-Task ausgelöst, ein `done`-Contract automatische Folgeaufträge und ein `needs_input` eine Fortsetzung. Das ist genau die Kette weiterer Ausgaben, die ein Limit verhindern soll.
3. **Keine Gate-Umgehung.** Bei aktivem Review hätte „behalten“ heißen müssen: Review ausführen, also das Limit noch weiter überschreiten, oder Review überspringen, also ungeprüften Code als `done` übernehmen. Der bisherige Code tat keins von beidem sauber: Er meldete „Ergebnis bleibt bestehen“ und blockierte danach doch („kein Review-Lauf“). Das ist der Widerspruch aus dem Testbericht.
4. **Es geht keine Arbeit verloren.** Commit, Diff und Revert stehen im Drawer zur Verfügung.

**Umsetzung:** In `early()` greift nach dem Live-Guard die Prüfung `l.exhausted(used)`, mit `used` = Task-Verbrauch inklusive dieses Laufs. Das gilt für Agent- und Review-Lauf. Die Meldung nennt Limit, Verbrauch und „erst nach dem Lauf gemeldet; die Änderungen sind committet“. Entfernt wurden: `warn_post_hoc_overrun`, `post_hoc_overrun`, der Notification-Kind `limit_exceeded` (die UI hat ihn nie ausgewertet) und die nun unerreichbare Limit-Vorprüfung in `run_review`. README und der UI-Hinweis in den Projekt-Limits sind angepasst. Das weicht bewusst von Plan 5.5.3 und AK27 ab.

**Tests:**
- Unit `runner::tests::post_hoc_overrun_blocks_even_with_a_contract` für codex, gemini und claude, für Kosten, für den kumulierten Verbrauch über mehrere Läufe und mit Nutzerabbruch, der Vorrang behält.
- Integration `failures::post_hoc_token_overrun_blocks_without_review_or_fix` mit dem neuen Mock-Keyword `MOCK_USAGE_TOTAL`. Die finale Meldung liegt weit über den Live-Deltas. Geprüft wird: Review aktiv, aber kein Review-Lauf, keine Contract-Auswertung, kein Fix, genau eine `usage_log`-Zeile (4000/500), Teil-Commit gleich `snapshot_after`, und ohne Limit endet derselbe Auftrag `done`.
- Damit ist auch die Schwäche aus dem Testbericht behoben, dass AK27 nur als reine Funktion getestet war.

### R5 – `run_inner` überlang und dupliziert (Wartbarkeit)
- **Problem:** Rund 455 Zeilen. Das Beenden als `cancelled` war viermal kopiert, das Zusammensetzen von Fehlertext plus stderr-Tail dreimal.
- **Fix:** `run_inner` hat jetzt 226 Zeilen. Ausgelagert sind `prepare_workspace`, `build_prompt`, `record_run_usage` (auch vom Review genutzt), `push`, `finish_cancelled` (an 4 Stellen genutzt), `finish_done`, `finish_failed` und `Continue::into_result`. Das Verhalten ist unverändert, alle 184 Backend-Tests sind grün.
- **Zusätzlich:** Der Pfad „keine Zugangsdaten“ nutzt jetzt `block_running`, also nur bei `status='running'`, wie alle anderen Block-Pfade, statt eines eigenen bedingungslosen `finish`. Fehlermeldung und Klasse `config` bleiben gleich, sodass ein späteres Credential-PUT den Task weiter erneut einreiht.

## Nicht blockierende Funde (dokumentiert, nicht geändert)

| # | Fund | Bewertung |
|---|---|---|
| N1 | Provider-, Infra- oder Stillstandsfehler im **Review-Lauf**: Der Review wird übersprungen und der Task wird `done`, ohne geprüft zu sein. | Lücke im Gate. Im Plan als Nicht-Ziel geführt, im README vermerkt. Gehört in WP03 (Phasen-Retry statt Überspringen). |
| N2 | Ein Panic irgendwo in `run_task` lässt den Task samt `st.running`-Eintrag bis zum Neustart `running`, weil der Join-Handle nicht überwacht wird. | Bestand vor WP01. R2 und R3 entfernen die bekannten Panic-Quellen aus dem Stream, die Ursache selbst bleibt. |
| N3 | `PATCH /api/tasks/{id}` prüft den Status und führt dann ein `UPDATE` ohne Statusbedingung aus (TOCTOU). | Muster bestand schon. Limits auf einem gerade gestarteten Task wirken erst beim nächsten Lauf. |
| N4 | Der Gemini-Parser puffert Text-Chunks ohne Obergrenze bis zu `tool_use`/`result`. | Speicherwachstum bei sehr langer Ausgabe, die der Agent steuert. Leicht zu begrenzen. |
| N5 | `agent_run` hat noch 181 Zeilen (Consumer-Closure mit Guard-Meldung), `run_review` 122. | Lesbar, aber Kandidaten für die nächste Aufräumrunde. |
| N6 | UI: Minuten werden zu Sekunden gerundet. Werte unter 1 s liefern 400 mit einer Meldung in Sekunden, gebrochene Turns/Tokens einen englischen 422-serde-Fehler. Task-Limits sind nur per API setzbar (Abweichung 17 der Umsetzung). | UX-Kanten, beim WP00-Port beheben. |
| N7 | Der Textfallback `\b401\b` greift im Fehlerkanal auch ohne HTTP-Kontext und kann so eine Auth-Pause auslösen. | So im Plan vorgesehen. Betrifft nur den Fehlerkanal, ist nachweislich nicht über stderr oder Agent-Text auslösbar. |
| N8 | Die Labels der Fehlerklassen sind doppelt gepflegt (Rust `class_label`, TS `FAILURE_LABEL`). | In WP00 über `queued-common` teilen. |
| N9 | Codex und Gemini wurden nie live getestet, die meisten Fixtures sind konstruiert. Podman-SIGINT ist unverifiziert, das Agent-Image wurde nie gebaut. | Ehrlich in den Berichten und in `PROVENANCE.md` ausgewiesen. Docker-SIGINT habe ich nicht erneut geprüft. |
| N10 | „Pause aufheben“ hebt auch ein erkanntes Abo-Limit (`limited_until`) auf. | Gewollt (Nutzerentscheidung). Der Folge-Lauf pausiert erneut, falls das Limit noch gilt. |
| N11 | Die Obergrenze `RUNNER_KILL_GRACE_SECS` ≤ 60 ist an die feste Zahl `+180` im HTTP-Timeout der App gekoppelt. | Dokumentiert und erzwungen, aber eine implizite Kopplung über die Prozessgrenze. |

Geprüft und in Ordnung: Tenant-Isolation (`resume` und `limits` nutzen die User-ID, Test `resume_only_lifts_own_pause`), keine Secrets in argv/Events (Test über alle Provider und Credential-Typen), die Architektur (Datei- und Prozessarbeit ausschließlich im Runner, die App setzt nur `queue_files` im Protokoll), Migration 0003 additiv mit CHECKs passend zu `FailureClass::ALL` (Test), Notiz-Dedupe sowie Backoff/`not_before` inklusive Weck-Timer.

## Geänderte Dateien (Review)

`backend/crates/runner/src/{git.rs,exec.rs}` (R1), `backend/crates/app/src/agent/{classify.rs,mod.rs,claude.rs,mock.rs}` (R2, R3, `MOCK_USAGE_TOTAL`), `backend/crates/app/src/{models.rs,budget.rs,limits.rs}` (R3), `backend/crates/app/src/runner.rs` (R2-Cap, R4, R5), `backend/crates/app/tests/failures.rs` (R4), `README.md`, `frontend/src/Projects.tsx` (Hinweistext zu R4).

## Testergebnisse (2026-11-07, alle nach den Änderungen in dieser Sitzung)

| Befehl | Ergebnis |
|---|---|
| Baseline vor den Änderungen: `cargo test --workspace` | 180/180 grün |
| `cargo fmt --all -- --check` | Exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit 0, keine Warnungen |
| `DATABASE_URL=postgres://queued:queued@localhost/queued cargo test --workspace` | **184/184 grün**: app lib 72, api 19, cli_contract 24, edge_cases 11, failures 19, parser_robustness 6, common 6, runner 27 (neu: 4 Tests, 1 ersetzt) |
| `cargo test -p queued-runner --lib` ×6 nach dem Flush-Fix | 6/6 grün |
| `cd frontend && npm run build && npm test` | tsc und vite ok, Vitest **11/11** |
| `cd e2e && npx playwright test` | **25/25 grün** (38,1 s) |
