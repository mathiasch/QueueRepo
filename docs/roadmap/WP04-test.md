# WP04 – Testbericht

Umgebung: Linux-Container, rustc/cargo 1.97.0, Git 2.43, Postgres 16 lokal, Trunk 0.22.1, Node 22, Playwright mit Chromium (`/opt/pw-browsers`), axe-core. Runner-Engine in allen automatisierten Tests: `none`. **Keine externen Calls:** Agenten sind der Mock-Provider (neu: `MOCK_FILE:<name>`, `MOCK_RESOLVE`), Git-Remotes lokale Bare-Repositories. `CARGO_INCREMENTAL=0` wegen Plattenplatz.

## 1. Abnahmeläufe (finaler Stand, zweimal hintereinander)

Skript (Scratchpad `accept.sh`): Backend fmt → clippy → `cargo test --workspace` → Frontend fmt → clippy wasm32 → clippy nativ → `cargo test` → `trunk build --release` → `npx playwright test` (alle Projekte; `stack.sh` baut die Binaries selbst).

| Schritt | Lauf A | Lauf B |
|---|---|---|
| Backend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Backend `cargo clippy --workspace --all-targets -- -D warnings` | rc=0 | rc=0 |
| Backend `cargo test --workspace` | **368 passed, 0 failed** (137 s) | **368 passed, 0 failed** (103 s) |
| Frontend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Frontend `cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo clippy --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo test` | **45 passed** | **45 passed** |
| Frontend `trunk build --release` | rc=0 | rc=0 |
| Playwright (system 19, desktop 29, iphone 9, iphone-landscape 9; inkl. axe) | **66 passed** (4,0 min) | **66 passed** (3,9 min) |

Vergleich mit WP03: Backend 345 → 368 (+23), Frontend 43 → 45, Playwright 59 → 66.
Backend je Testziel: `queued` lib 115, `api.rs` 19, **`branches.rs` 9 (neu)**, `cli_contract.rs` 26, `edge_cases.rs` 11, `environments.rs` 8, `failures.rs` 19, `gate.rs` 9, `parser_robustness.rs` 6, `providers.rs` 16, `queued-api` 20, `queued-common` 34, `queued-runner` 76.

## 2. Neue Tests

### Unit
| Ort | Tests | Was |
|---|---|---|
| `runner/src/tasks/tests.rs` | 8 | **Isolation:** eigenes Repo mit echtem `.git`, kein `commondir`/Alternates/`gitdir`, nur Ziel-Historie (Quarantäne-, Held- und Fremd-Objekte per `cat-file -e` abwesend), `.queue/` ignoriert, Fortsetzung mit WIP, ungültige Namen/Branches. **Agent ⇸ Hauptrepo/andere Aufträge:** Agent-Commit, gefälschte Baseline, Hooks (post-commit, pre-push, post-checkout, reference-transaction, post-merge), `core.fsmonitor`, `uploadpack.packObjectsHook`, `core.alternateRefsCommand` – nichts läuft auf dem Host (Marker-Datei), Config vor dem Upload-Pack neu geschrieben, Baseline aus dem Hauptrepo, anderer Auftrag sieht nichts, Ziel unverändert. **Sync** genau des gegateten SHA (nicht des später verschobenen Branch-Kopfs), Baum mit `.git`-Eintrag per fsck abgelehnt. **Merge-Queue:** Squash ohne Bewegung (Baum = Branch), 3-Wege bei bewegtem Ziel, Checkout im Arbeitsbaum, veraltetes Ziel abgelehnt, lineare Historie, Autor, Diff/Revert gegen Merge-Base, zweite Übernahme ⇒ `empty`. **Konflikt:** Konfliktdateien, nichts übernommen, Fix-Arbeitsbaum mit Carry + Markern, Marker blockieren, aufgelöst ⇒ übernommen, leerer Carry. **Nachprüfung:** Secret ohne Scanner committet ⇒ neuer Fund (maskiert), Allowlist; eigene Änderung an geschützter Datei ⇒ kein neuer Fund. **Alt-Hold** wandert in den Arbeitsbaum und lässt sich freigeben. **Beschädigtes `.git`** (Symlink aufs Hauptrepo) ⇒ Neuaufbau vom Branch |
| `runner/src/lib.rs` | 1 | Task-Endpunkte: Projekt-ID als Task bzw. Task-ID als Haupt ⇒ 400 (fremder Hauptarbeitsbereich bleibt), Anlegen/Löschen über HTTP |
| `runner/src/exec.rs` | 1 | Setups desselben Cache-Scopes teilen den Projekt-Cache und laufen nacheinander (Gegenprobe ohne Sperre: Test schlägt fehl „setups overlapped“); Task-Scope fällt auf den eigenen Workspace zurück |
| `runner/src/git.rs` | 1 | Dateien-API folgt keinen Symlinks nach außen/in `.git`, ZIP-Import schreibt nicht durch Symlinks |
| `common/src/lib.rs` | 1 | Workspace-IDs und Branch-Namen (gültig/ungültig), `CacheSpec` ohne Scope |
| `api/src/branch.rs` | 2 | Merge-Zustände (Roundtrip, Texte), Parallelitätsgrenzen |
| `api/src/lib.rs` | erweitert | alte Task-Zeilen parsen, `diff_base`, `on_target`, `has_changes` gegen Merge-Base, Status `merging` |
| `frontend/src/logic/branch.rs` | 2 | Platz-Texte, Töne, Branch-Zeilen, Revert nur übernommen |
| `frontend/src/logic/labels.rs` | erweitert | Status „Wird übernommen“, Event „Übernahme“ |

### Integration (`backend/crates/app/tests/branches.rs`, App + echter Runner + Postgres)
1. Drei Aufträge, Projekt-Parallelität 2: zwei laufen gleichzeitig, der dritte wartet; eigene Arbeitsbäume auf `queue/<12 hex>`; alle übernommen, ein Commit je Auftrag, keine Merge-Commits, Arbeitsbäume und Branches gelöscht; Agent-`pwd` = eigener Arbeitsbaum.
2. Standard 1: der zweite Auftrag startet nie, solange der erste läuft oder übernommen wird; Merge-Base = Commit des ersten; kein Konflikt, kein Re-Verify.
3. Merge-Konflikt (gleiche Datei): Verlierer `failed`/`conflict`, Fix mit `carry_from`, Konfliktkontext im Prompt, Marker aufgelöst, beide Seiten im Hauptstand, Branches/Arbeitsbäume weg.
4. Verify nach Aktualisierung: jeder Auftrag allein grün, zusammen rot ⇒ Verlierer `merge_state=failed`, Verify-Event „nach Aktualisierung“, nichts übernommen, Diff nur eigene Änderung, Revert 409.
5. Diff/Revert gegen die Merge-Base; Revert des ersten lässt den zweiten unberührt.
6. Parallelität: Standard 1, Validierung (0/9/−1 ⇒ 400 mit Text), Bearbeiter 403, Rest weiter editierbar; Merge-Queue-API (Reihenfolge, Position, Nutzer, Branch, Fremde 404), Abbrechen eines wartenden Eintrags.
7. Abbrechen eines laufenden Auftrags entfernt Arbeitsbaum und Branch; Retry startet frisch; Projekt löschen entfernt auch Arbeitsbäume wartender Aufträge.
8. **Nebenläufigkeit:** 24 gleichzeitige `claim_next` über drei Projekte (Parallelität 1/2/3, eins mit wartender Übernahme), drei Runden: nie Überbuchung, nie doppelt, erste Runde genau 5; ein frei gewordener Slot ⇒ genau ein Claim im richtigen Projekt.
9. **Nebenläufigkeit Merge:** 16 gleichzeitige `claim_merge` über zwei Projekte ⇒ genau eine Übernahme pro Projekt, jeweils die älteste; während einer läuft keine zweite.

Angepasst (WP04-Verhalten, nicht abgeschwächt): `edge_cases::interrupted_task_diff_covers_all_its_runs` (ein Squash-Commit, Diff enthält beide Läufe), `interrupted_task_diff_never_includes_other_tasks` (getrennte Dateien; Revert jetzt verpflichtend 200 statt „200 oder 409“), `failures::runtime_limit_blocks_without_fix` und `post_hoc_token_overrun_…` (Teil-Commit auf dem Branch, **nicht** im Verlauf des Hauptstands), `gate::secret_in_diff_holds_…` (Held-Ref nur im Arbeitsbaum, im Hauptrepo nachweislich nicht).

### System/E2E (Playwright)
- `system/branches.spec.ts` (3): echte Binaries; parallele Aufträge + serielle Übernahme + leere Merge-Queue danach; Konflikt ⇒ Fix mit Carry ⇒ keine Marker, Revert nur für übernommen; Verify nach Aktualisierung, Diff gegen Merge-Base, Revert.
- `ui/branches.spec.ts` (2, Desktop): Eigentümer stellt „Gleichzeitige Aufträge“ ein (Reload, Hilfe), Bearbeiter sieht es gesperrt mit Hinweis und speichert den Rest; Queue-Sektion „Wird übernommen“, Drawer-Block mit Branch und Zustand, kein „Rückgängig“ vor der Übernahme, „Übernommen“ + „Rückgängig“ danach, Konflikt verständlich erklärt, Merge-Queue-Karte im Verlauf; axe auf allen vier Ansichten.
- `ui/branches-mobile.spec.ts` (iPhone hoch/quer): Auswahl ≥ 44 pt, innerhalb der Safe Areas, 16-px-Schrift, kein horizontales Scrollen, Merge-Queue-Karte, Drawer-Block; axe.
- Tour (a11y Desktop/iPhone, hell/dunkel) deckt den neuen Drawer-Block (erledigter Auftrag), die Einstellungen mit „Gleichzeitig arbeiten“ und den Verlauf mit der Merge-Queue-Karte automatisch mit ab.

## 3. Während der Tests gefundene Fehler

| Fund | Ursache | Fix |
|---|---|---|
| Alt-Test „diff never includes other tasks“: Auftrag A landete im Konflikt | Mock schreibt jede Laufzeile in `MOCK_LOG.md`; B überholte A in der Backoff-Phase | Test auf getrennte Dateien (`MOCK_FILE`), Verhalten ist gewollt (Konflikt ⇒ Fix) |
| Standard 1 erzeugte Konflikte | `merging` zählte nicht zur Projekt-Parallelität | zählt jetzt (B4) |
| Fix ohne Carry | `carry_from` nach dem Insert gesetzt (Scheduler schneller) | im Insert (B5) |
| Branch des Konflikt-Auftrags blieb | Fix zählte sich selbst als „setzt noch fort“ | B6 |
| sporadisch 409 beim Aufräumen | Runner gab Workspace erst nach Stream-Ende frei | B7 |
| Abbruch während der Vorbereitung wirkungslos (30 s) | Cancel vor Registrierung des Laufs | B8 |
| `transient_retries` nach Übernahme 0 | Merge setzte den Zähler zurück | entfernt (Zähler bleibt Auftragsgeschichte) |
| E2E „editors see it read-only“: strict-mode | `getByLabel("Projektziel")` traf auch den Hilfe-Button | Selektor `getByRole("textbox")` |

## 4. Nicht automatisiert geprüft

- Rootless Podman/Docker mit den Arbeitsbaum-Mounts (alle Tests Engine `none`; Container-Argumente nur per Unit-Test). Mit Engine `none` gibt es keine Isolation – die Isolationsbelege sind die Runner-Tests (Repository-Ebene) und die Mount-Liste.
- Große Repositories (Plattenplatz/Zeit pro Arbeitsbaum), Branch-Namenskollision (12 Hex), Verhalten echter Agent-CLIs bei Konfliktmarkern.
