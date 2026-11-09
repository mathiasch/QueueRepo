# WP03 – Testbericht

Umgebung: Linux-Container, rustc/cargo 1.97.0, Postgres 16 lokal, Trunk 0.22.1, Node 22, Playwright mit Chromium (`/opt/pw-browsers`), axe-core. Runner-Engine in allen automatisierten Tests: `none`. **Keine externen Calls:** Git-Remotes sind lokale Bare-Repositories (`file://`, im E2E-Stack per `ALLOW_LOCAL_GIT_REMOTES=true` nur dort erlaubt), Agenten sind der Mock-Provider. **Keine echten Secrets:** Testwerte werden zur Laufzeit erzeugt (`queued_common::secrets::testdata` setzt Präfix und Füllzeichen aus Teilstrings zusammen; der Mock erzeugt Fake-GitHub-Tokens aus `/dev/urandom`). Kein Test prüft gegen einen Klartext-Wert im Quelltext.

## 1. Abnahmeläufe (finaler Stand, zweimal hintereinander)

Skript `accept.sh`: Backend fmt → clippy → `cargo test --workspace` → Frontend fmt → clippy wasm32 → clippy nativ → `cargo test` → `trunk build --release` → `npx playwright test` (alle Projekte; `stack.sh` baut die Binaries selbst). `CARGO_INCREMENTAL=0` wegen Plattenplatz.

| Schritt | Lauf A | Lauf B |
|---|---|---|
| Backend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Backend `cargo clippy --workspace --all-targets -- -D warnings` | rc=0 | rc=0 |
| Backend `cargo test --workspace` | **345 passed, 0 failed** (78 s) | **345 passed, 0 failed** (80 s) |
| Frontend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Frontend `cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo clippy --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo test` | **43 passed** | **43 passed** |
| Frontend `trunk build --release` | rc=0 | rc=0 |
| Playwright (system, desktop, iphone, iphone-landscape; inkl. axe) | **59 passed** (2,7 min) | **59 passed** (2,8 min) |

Vergleich mit WP02: Backend 296 → 345 (+49), Frontend 42 → 43, Playwright 52 → 59.

Backend je Testziel (Lauf A): `queued` lib 115, `api.rs` 19, `cli_contract.rs` 26, `edge_cases.rs` 11, `environments.rs` 8, `failures.rs` 19, **`gate.rs` 9 (neu)**, `parser_robustness.rs` 6, `providers.rs` 16, `queued-api` 18, `queued-common` 33, `queued-runner` 65.

## 2. Neue Tests

### Unit (Logik)
| Ort | Tests | Was |
|---|---|---|
| `common/src/secrets.rs` | 9 | alle Regeln kompilieren/eindeutig; feste Formate; privater Schlüssel samt Körper im Fingerabdruck; Entropie-Regel mit 10 Falsch-Positiv-Kandidaten (Platzhalter, `${…}`, `process.env`, niedrige Entropie, `author`, `count_tokens`); AWS-`EXAMPLE` und Füllzeichen ignoriert; Passwort in URL (Entwicklungs-Standard `postgres://queued:queued@…` nicht); Redaktion und Maskierung zeigen nie den Wert; Überlappung; HMAC stabil/keyed; UTF-8 |
| `common/src/gate.rs` | 8 | Diff-Parser (Status, Pfade, Zeilen, binär); Secrets nur in hinzugefügten Zeilen, Allowlist; geschützte Pfade (nur bestehende Dateien); 12 Skip/Only-Marker inkl. Ausnahmen (Markdown, Ruby-`skip` nur in `.rb`); gelöschte Tests, entfernte Assertions, Verifier-Konfiguration; `package.json`-Skripte; beobachtete Agent-Konfiguration (auch neue Dateien) |
| `common/src/glob.rs` | 4 | Basisname in jeder Tiefe, `dir/`, `**`, `?`, Backtracking, Validierung |
| `api/src/gate.rs` | 3 | Texte für alle Fundarten/Stufen, `TaskGate`-Roundtrip und alte Zeilen, Pfad-Validierung, `null` vs. fehlend |
| `runner/src/git.rs` | 11 | **Agenten-`.git` führt nichts auf dem Host aus** (fsmonitor, Hooks inkl. `post-commit`, clean/smudge-Filter, textconv, `diff.external`, gpg, `insteadOf`, `include`, Alternates; Marker-Datei bleibt aus, Config bereinigt, Filter nicht angewendet); symlinkte `.git`/`.git/objects` abgelehnt; Secret blockiert Commit, Fingerabdruck erlaubt, `binary`-Attribut versteckt nichts; `begin` stellt Reste mit Secret in Quarantäne; `inspect` (Secret, geschützt, Skip, Assertions, `package.json`) ohne den echten Index zu berühren; hold/release (3-Wege auf späteren Commit)/Konflikt atomar/discard prunt (Blob weg), verschobener Prune bei laufendem Prozess; Freigabe mit unerlaubtem Secret hält weiter; Baselines (TOFU, Änderung, Freigabe, ungültiger Name); Push: leeres Remote → `main` geschützt, Default-Branch per `ls-remote`, Feature-Branch exakter SHA, Secret in später entferntem Commit, Allowlist, umgebogenes `origin` irrelevant, Options-Injection; `ls-remote`/Config-Hilfsfunktionen; zu große Änderung nur mit `allow_large` |
| `runner/src/exec.rs` | 1 | `home_env` zeigt ins Lauf-HOME (none) bzw. `/home/agent` (Container), Traversal/ungültige Namen abgelehnt |
| `app/src/agent/mod.rs` | 1 | Repo-Konfiguration je CLI: Claude-Flags (ohne `--bare`), Gemini-System-Settings + `home_env` + zufälliger MCP-Name pro Lauf, opencode-Variable, Codex unverändert |
| `app/src/runner_gate.rs` | 2 | Setup-Pfade (Manifeste, Cache-Dateien, Pfade aus dem Skript, keine URLs/absoluten Pfade/Variablen); `Pending`-Roundtrip |
| `app/src/prompt.rs` | 1 | Rubrik, freigegebene Funde im Review-Prompt, Mock-Erkennungssatz unverändert |
| `frontend/src/logic/protection.rs` | 1 | Pfade parsen/validieren, Standardliste erkennen |
| `frontend/src/logic/labels.rs` | erweitert | Status `held`, Klasse `gate`, Event „Schutz“, Benachrichtigung „gate“ ist kein Fehler |

### Integration (App + echter Runner + Postgres, `tests/gate.rs`, 9)
1. Secret im Diff → `held`, Fund mit Pfad/Zeile/Maske, nichts auf Branch oder Arbeitsbaum, echter Wert (aus dem Held-Ref gelesen) nirgends in Task-JSON, Gate-Diff, Diff, Benachrichtigungen; Fremder 404; Freigabe → `done`, Fingerabdruck (32 hex, kein `ghp_`) im Projekt.
2. Verwerfen → `blocked/gate`, Held-Ref weg, doppelte Entscheidung 409; Abbrechen eines gehaltenen Auftrags verwirft ebenfalls.
3. Token in der Agenten-Ausgabe → geschwärztes Event, rohe `task_events` ohne Scanner-Treffer, `held` (`secret_in_log`).
4. Geschützter Test geändert → `held` **vor** Verify; nach Freigabe läuft Verify; Skip-Marker → `held`; Test-Auftrag ohne Freigabe; Eigentümer schaltet Test-Schutz ab.
5. Selbst-Commit des Agenten → `held`, Branch zurückgesetzt, nach Verwerfen auch in `log --all` weg.
6. Review dauerhaft gestört → Phasen-Retries (nur Review), dann `held`; „Review wiederholen“; „Ohne Prüfung übernehmen“ → `done` ohne Review.
7. Setup-Datei nach erster Freigabe geändert → `held` (Stufe `setup`) ohne Setup-Start; Freigabe → Setup läuft; danach keine Rückfrage; Secret-Wert nirgends sichtbar.
8. Vertraute Repo-Konfiguration: Änderung durch Agent → `held` (`changes`, `agent_config`), danach keine zweite Rückfrage; Änderung am Gate vorbei (während nicht vertraut) → `held` (`config`) vor dem nächsten Lauf.
9. Schutz-API: Standardwerte, Rollen (Bearbeiter/Betrachter lesen, 403 beim Schreiben, Fremde 404), Validierung, Standardliste, Freigaben vergessen, `held` in der Auftragsliste, `retry_review` nur in Stufe `review`.

Angepasst (strenger): `api.rs::clones_remote_and_auto_pushes` (Push auf `main` erst nach Freigabe, vorher nichts im Remote), `failures.rs::review_rate_limit_pauses_review_provider` (kein `done`, Phasen-Retry wartet auf den pausierten Provider).

### System/E2E (Playwright)
- `system/gate.spec.ts` (2): echte Binaries; Auto-Push auf `main` eines Bare-Remotes nur nach Freigabe („Nicht hochladen“ pusht nichts); Secret + Log-Token gehalten, End-to-End geschwärzt (Task, Gate-Diff, Benachrichtigungen), Verwerfen hinterlässt keine Datei.
- `ui/gate.spec.ts` (3, Desktop): Freigabe-Bereich mit Fund, Maske, Hilfe, geschwärztem Diff; Bestätigungsdialog beim Freigeben eines Geheimnisses (Abbrechen hält); Verwerfen mit Bestätigung → Abbruchgrund „Freigabe abgelehnt“; Review-Ausfall mit „Review wiederholen“/„Ohne Prüfung übernehmen“; Composer „Darf Tests ändern“; Schutz-Reiter (Validierungsfehler, Bestätigung beim Freischalten, Speichern, Reload, Bearbeiter nur lesend).
- `ui/gate-mobile.spec.ts` (iPhone hoch/quer): Buttons ≥ 44 pt und innerhalb der Safe Areas, kein horizontales Scrollen, 16-px-Eingaben, Speichern über der Tab-Leiste, axe.
- `tour.ts` (a11y Desktop/iPhone, hell/dunkel): zusätzlich gehaltener Auftrag im Drawer mit geschwärzter Diff-Ansicht und Reiter „Schutz“ mit aufgeklappter CLI-Liste – axe ohne Verstöße. Screenshots: `docs/roadmap/WP03-screenshots/`.

## 3. Während der Tests gefundene Fehler

| Fund | Ursache | Fix |
|---|---|---|
| E2E-Lauf 1: 10 Fehlschläge inkl. „Target crashed“ | Platte voll (Build-Artefakte + heruntergeladene CLI-Pakete) | aufgeräumt, `CARGO_INCREMENTAL=0` |
| Reiter „Schutz“ leer | `Memo::track()` im Render-Closure abonnierte nicht | Muster des Umgebungs-Reiters |
| System-Test 400 | `file://`-Remotes sind nur mit `ALLOW_LOCAL_GIT_REMOTES` erlaubt | im E2E-Stack gesetzt |
| `MOCK_SECRET MOCK_SECRET_LOG` erzeugte nur einen Fund | erster Treffer im `case` gewinnt | getrennte `case`-Blöcke, `MOCK_SECRET_FILE` |
| Agent-Konfiguration im Vertrauensmodus erst beim nächsten Lauf | Watch-Liste fehlte am Gate (Ersetzung im Runner nicht angewendet) | `watched_findings` im `inspect`, Test |
| Freigabe-Diff via MOCK_SOLUTION_B64 | Titel enthielt abgeschnittenes base64 | eigener Titel im Test-Helfer |

## 4. Nicht automatisiert geprüft

- Echte CLIs: Claude-Flags nur im installierten Binary belegt (nicht mit Modell ausgeführt); Gemini/opencode/Codex nicht ausgeführt (Bundle-Quelltext bzw. Binary-Strings).
- „Anderer Review-Anbieter“: kein Integrationstest (der Mock ist einziger Anbieter); Code-Review.
- Rootless Podman/Docker: Git-Härtung ist engine-unabhängig (läuft im Runner auf dem Host); `home_env`-Containerpfad nur per Unit-Test.
- Batch-Review-Ausfall (`complete_review` → `held`): Code-Review, kein eigener Test (Batch-Poller-Tests aus WP13 bleiben grün).
