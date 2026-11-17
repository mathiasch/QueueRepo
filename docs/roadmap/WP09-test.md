# WP09 – Memory, Wissen, Dedupe & Anhänge: Testbericht

Stand: 2026-11-12, Branch `mvp` (+ WP09-Arbeitsstand). Alle Zahlen aus Läufen in dieser Sitzung; keine externen Dienste, keine Live-Aufrufe an LLMs oder bezahlte APIs (Kosten: 0 USD). Der Modell-Critic läuft nur gegen den lokalen Fake-Server `tests/common/fake_llm.rs`. Das Verhalten der echten CLIs (Codex/Gemini/Claude/opencode) wurde nur aus Programm-Strings bzw. Bundle-Quelltext abgeleitet (`npm pack` der gepinnten Pakete als Daten, nichts ausgeführt).

## 1. Teststrategie

| Ebene | Was | Wo |
|---|---|---|
| Unit (rein) | Zustand: Parser/Schema (kein JSON, Version, unbekannte Felder, doppelte/ungültige IDs, Größe, Anzahl), Datei-Verweise (Traversal, `.git`, `.queue`, absolut, Backslash), Reparaturregeln (Löschen, Umbenennen, `done → todo`, `done`/`passes` ohne Erfolg, `passes ⇒ done`, fehlende Dateien, Log nur anhängen, ≤ 5 neue, Zeitstempel von Queue), Merge (parallel geändert/gelöscht/angelegt), Zusammenfassung begrenzt | `common/src/state.rs` (7) |
| Unit (rein) | Anweisungsdateien in der Agent-Konfig-Beobachtung (jede Tiefe, `docs/agents.md` nicht) | `common/src/gate.rs` `watched_instruction_files` |
| Unit | Sniffing ignoriert Namen, MIME-Fälschung (SVG/HTML/Skript in Text- oder Bildnamen, ELF/PE, NUL, leer, HEIC, PNG-/JPEG-Kopf mit HTML), EXIF/GPS (JPEG mit APP1/GPS-IFD und Orientation 6 ⇒ kein APP1, kein „GPS-SECRET“, 16×8 ⇒ 8×16), PNG-`tEXt` weg, Größen- und Pixel-Limits (20 000² Dekompressionsbombe), Namensbereinigung (Pfade, RTL-Override, Umlaute, Länge), Auswahl (10 Dateien, 25 MiB, eindeutige Namen) und Prompt-Liste nur mit Queue-Namen | `app/src/attachments.rs` (6) |
| Unit | Ähnlichkeit (Jaccard, Symmetrie, Bereich), Stemming/Füllwörter, Verdikte für Duplikat/ähnlich/revertiert/neu/doppelt im Lauf/zu knapp/sensibel, Determinismus, Schwellen | `app/src/dedupe.rs` (3) |
| Unit | Regel-Abschnitt byte-stabil, Zustandsabschnitt als Datenblock (Grenze nicht schließbar), Critic-Prompt als Datenblock, revertierte Aufträge in beiden OnEmpty-Prompts, Codex `-c project_doc_max_bytes=0` vor `exec`, Gemini-`context.fileName` pro Lauf verschieden | `knowledge.rs`, `project_state.rs`, `prompt.rs`, `agent/mod.rs` |
| Unit | Validierung/Labels/Dateigröße der geteilten Typen | `api/src/memory.rs` |
| Runner (echtes Dateisystem + Git) | Bereitstellung mit Symlink-Fallen (`.queue` → Host-Ordner, `attachments` → Host-Ordner, Symlink in der Liste), Namen/Duplikate/Anzahl/Größe, Fehlschlag lässt alte Dateien stehen; `.queue/` nie gestaget (`!.queue/` + geleerte Exclude-Datei, nur-`.queue`-Änderung ⇒ kein Commit); Agenten-Commits mit `.queue/` zusammengefasst, ohne `.queue/` nicht; Zustand gegen echten Baum (fehlend, Symlink nach außen, Traversal, `.git`; abgelehnt: Junk, Version, Größe; Symlink als `state.json` nie gelesen); Export (Marker, Brücken, Handgeschriebenes/Symlink unangetastet, Symlink-AGENTS.md ersetzt, Secret blockiert und Rückbau); Übernahme lehnt `.queue` im Baum ab | `runner/src/wp09_tests.rs` (5), `tasks/tests.rs` (1) |
| Integration (App + Runner in-process + Postgres) | siehe Abschnitt 3 | `app/tests/memory.rs` (7) |
| System (getrennte Prozesse) | Fälschung, EXIF/GPS, Header, Bereitstellung und nie im Commit, Regel erst nach Freigabe im Prompt, ungültiger Zustand abgelehnt, Dedupe verhindert Duplikat | `e2e/tests/system/memory.spec.ts` (4) |
| E2E Desktop | Anhang im Composer (Dateiauswahl), bleibt nach Reload, Fälschung mit verständlicher Meldung, im Drawer; Regeln speichern, Agenten-Vorschlag mit Diff, Freigabe im UI, CLI-Tabelle; Zustand anlegen/Status/Speichern/Verlauf; Dedupe-Begründung im Drawer mit Sprung zum ähnlichen Auftrag; axe auf jeder Ansicht | `e2e/tests/ui/memory.spec.ts` (3) |
| E2E iPhone (hoch + quer) | „Foto aufnehmen“ per **Tippen** öffnet den Kamera-Eingang (`capture="environment"`, `accept="image/*"`, einzeln), Foto mit GPS hochgeladen, Vorschau, Server-Kopie ohne EXIF/GPS, 44-pt-Ziele innerhalb der Safe-Areas, kein horizontales Scrollen, Inputs ≥ 16 px auf „Wissen“ und „Zustand“, Speichern-Knopf über dem Home-Indikator, axe | `e2e/tests/ui/memory-mobile.spec.ts` (1 × 2 Projekte) |
| Accessibility-Tour | „Wissen“ (mit aufgeklappter CLI-Tabelle) und „Zustand“ hell/dunkel, Desktop und iPhone, Screenshots | `e2e/tests/tour.ts` (erweitert) |

Testdaten: `e2e/fixtures/iphone-gps.jpg` (64×48, EXIF Orientation 6, Make/Model, GPS 47°22' N/8°32' E, Marker „GPS-SECRET-E2E“; mit Pillow erzeugt), `e2e/fixtures/screenshot.png`.

## 2. Gegenprobe (Tests können scheitern)

- Runner: `stage_all` ohne `rm --cached .queue` ⇒ `queue_dir_is_never_staged` **rot** (Mutant erkannt), danach zurückgesetzt und grün.
- `invalid_agent_state_is_rejected_or_repaired` war zuerst rot, weil der Titel des Auftrags (aus der ersten Prompt-Zeile, auf 80 Zeichen gekürzt) das Base64-Schlüsselwort abschnitt – Testfehler, mit expliziten Titeln behoben.
- Integration/E2E zum Leerlauf waren im Volllauf rot (Bericht wird nach „erledigt“ geschrieben) – Tests warten jetzt auf den Bericht.
- iPhone-Spec war rot (Speichern-Knopf unter der Tab-Bar) – Produktfehler, mit `scroll-padding-bottom` behoben (Review B6).
- Bestehender WP07-Test `automation.spec.ts` war im ersten Volllauf rot (Race, Review B7), 4× isoliert wiederholt grün, danach robust gemacht.
- Abnahmelauf 2 war rot: System-Spec „OnEmpty dedupe“ fand den eingeplanten Auftrag nicht in `GET /api/tasks` (Status `merging` fehlt dort, Review B9). Testfehler; Integration und System holen die Aufträge jetzt per ID. Abnahmelauf 3 war rot durch einen racy WP06-Test (Review B10, Testfehler, behoben). Danach zwei neue, vollständige Abnahmeläufe (5 und 6) auf dem Endstand.

## 3. Integrationstests `tests/memory.rs`

| Test | Prüft |
|---|---|
| `upload_limits_mime_forgery_exif_rights_and_headers` | EXIF/GPS weg + Ausrichtung, `Content-Type: text/html` des Clients ignoriert, Pfad im Namen entfernt, Header (`attachment`, `nosniff`, CSP `sandbox`, `no-store`), sha256; 7 Fälschungen ⇒ 422; Datei > 10 MiB ⇒ 422, Body > Limit ⇒ 413; Quota (3 MiB im Test) ⇒ 413 mit Meldung, Quota-Endpunkt; 11. Entwurf ⇒ 400; Entwurfsliste nur eigene, Betrachter 403; Betrachter lädt nicht hoch (403); Entwürfe privat (404 für andere), Fremde 404; fremder Entwurf nicht anhängbar (400); angehängt: Betrachter liest, darf nicht löschen; laufender Auftrag ⇒ 409 für Hochladen/Löschen; Eigentümerin löscht; Löschen von Auftrag und Projekt entfernt alle Anhänge |
| `attachments_reach_the_agent_and_never_a_commit` | 2 Anhänge bereitgestellt (Ereignis), Prüfsummen im Arbeitsbaum = gespeicherte, Agent committet `.queue/` mit `!.queue/` und leerer Exclude-Datei ⇒ zusammengefasst, `git log --all --name-only` des Hauptrepos ohne `.queue` |
| `agent_rule_proposal_needs_owner_approval` | Version 1 im Prompt; Agenten-Vorschlag (Secret geschwärzt) nicht im Prompt bis zur Freigabe; Bearbeiter: Vorschlag ja, Freigabe 403; Freigabe ⇒ Version 2 im nächsten Prompt; doppelte Entscheidung 409; Ablehnung mit Notiz; abgelehnter Text nicht erneut; veraltete Basis 409; Nicht-Mitglied 404; zu lang 400 |
| `rules_export_import_and_trusted_repo_config` | Export ohne Fassung 400; mit vertrauter Repo-Konfiguration: Export schreibt AGENTS.md + Brücken, Baseline nachgezogen, nächster Lauf **nicht** gehalten; Agent ändert AGENTS.md ⇒ gehalten (`agent_config`); Import entfernt Marker, Brücke 400, unbekannte/fehlende Datei 400 |
| `invalid_agent_state_is_rejected_or_repaired` | Mensch: Schema, Konflikt 409; Agent: Junk abgelehnt (Stand bleibt), Löschen/Umbenennen/fehlende Datei/Traversal repariert, `done` übernommen, Log gestempelt, Verlauf mit Auftrag; gescheiterter Auftrag ändert nichts |
| `onempty_dedupe_drops_duplicates_and_explains` | Schwellen-Validierung 400; Duplikat verworfen (mit ähnlichstem Auftrag), Variante eines revertierten Auftrags verworfen, neuer eingeplant, CI-Vorschlag zur Freigabe; nur 2 Aufträge erzeugt, mit Begründung; Ereignis mit Zahlen |
| `onempty_model_critic_is_stricter_and_costs_are_recorded` | Analyse per Fake-API, Modell verwirft den ersten Vorschlag, behält den zweiten; genau ein Critic-Aufruf; `usage_log.step = critic` mit Kosten > 0; Schritt in `tasks.cost` |

## 4. Ergebnisse (Abnahme: komplette Suite zweimal hintereinander)

Skript `accept.sh` (Scratchpad): Backend `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`; Frontend `cargo fmt --check`, `cargo clippy --target wasm32-unknown-unknown --all-targets -D warnings`, `cargo clippy --all-targets -D warnings`, `cargo test`, `trunk build --release`; danach `frontend/target` gelöscht; Playwright alle Projekte (system, desktop, iphone, iphone-landscape inkl. axe-Tour). Stoppt beim ersten Fehler.

| Schritt | Lauf 1 | Lauf 2 |
|---|---|---|
| Backend fmt / clippy | grün / grün | siehe unten |
| Backend Tests | **535 bestanden, 0 fehlgeschlagen**: app lib 171, api 49, common 44, runner 96, queuectl 6, Integration: api 19, branches 9, cli_contract 26, edge_cases 11, environments 8, failures 19, gate 9, intake 8, **memory 7**, parser_robustness 6, plan_steering 13, providers 16, pull_requests 9, tokens 4, triggers 5 | |
| Frontend fmt / clippy wasm / clippy nativ | grün / grün / grün | |
| Frontend Tests | **60 bestanden** (davon 4 neu in `logic::memory`) | |
| `trunk build --release` | ok | |
| Playwright | **103 bestanden** (7,8 min; neu: system 4, desktop 3, iphone 1, iphone-landscape 1) | |

Neue Tests insgesamt: common 8, runner 6, api 1, app lib 14 (+ Erweiterung `repo_config_is_restricted_per_cli`), Integration 7 (+ Kompatibilitätstests in `queued-api` um die neuen Felder ergänzt), Frontend 4, Playwright 9 Ausführungen (+ Tour um 2 Ansichten × 2 Farbschemata × 3 Geräte).

## 5. Nicht verifiziert

- Echte CLIs: dass Codex mit `project_doc_max_bytes=0` keine `AGENTS.md` lädt, dass Gemini mit umgeleitetem `context.fileName` keine `GEMINI.md` lädt, dass Claude mit `--setting-sources user` keine Projekt-`CLAUDE.md`/`AGENTS.md` lädt, dass `@AGENTS.md` in den Brücken importiert wird – nur aus Strings/Quelltext.
- Echtes iPhone/Safari (Kamera, HEIC→JPEG-Umwandlung): nur Chromium mit iPhone-15-Pro-Profil und simulierten Safe-Areas.
- Echte Modell-Antworten des Critic (nur Fake-Server); Kalibrierung der Ähnlichkeitsschwellen an echten Projektdaten.
- Container-Engines (Docker/Podman) mit `.queue/attachments/` im Mount: Tests laufen mit Engine `none`.
