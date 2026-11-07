# WP00 – Testbericht

Umgebung: Linux-Container, rustc 1.97.0, Trunk 0.22.1, wasm-bindgen-cli 0.2.130 (per `cargo install`), wasm-opt version_131 (von Trunk geladen), Node 22.23.4, Playwright 1.63.0 mit Chromium (`/opt/pw-browsers`), axe-core 4.14.0 über `@axe-core/playwright` 4.14.0, Postgres lokal (`service postgresql start`). Externe Dienste (Agent-CLIs, Web-Push-Dienste, IdPs) sind nicht erreichbar. Alle Agent-Läufe nutzen den Mock-Provider.

## 1. Volle Suite – zweimal hintereinander, auf dem finalen Stand

Beide Läufe wurden mit demselben Skript ausgeführt: fmt, clippy, Backend-Tests, Frontend-Tests, `npx playwright test` ohne `E2E_SKIP_BUILD`. `stack.sh` baut also Backend und `trunk build --release` selbst.

| Schritt | Befehl | Lauf 1 | Lauf 2 |
|---|---|---|---|
| Backend fmt | `cd backend && cargo fmt --all -- --check` | ok | ok |
| Backend clippy | `cargo clippy --workspace --all-targets -- -D warnings` | 0 Warnungen | 0 Warnungen |
| Backend Tests | `DATABASE_URL=postgres://queued:queued@localhost/queued cargo test --workspace` | **190 passed, 0 failed** | **190 passed, 0 failed** |
| Frontend fmt | `cd frontend && cargo fmt --all -- --check` | ok | ok |
| Frontend clippy (WASM) | `cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings` | 0 Warnungen | 0 Warnungen |
| Frontend clippy (nativ) | `cargo clippy --all-targets -- -D warnings` | 0 Warnungen | 0 Warnungen |
| Frontend Tests | `cargo test` | **35 passed** | **35 passed** |
| Playwright | `cd e2e && npx playwright test` | **37 passed** (2,9 min) | **37 passed** (2,9 min) |

Backend 190 = 184 vorher (WP01-Stand) + 6 neue in `queued-api`. Alle bisherigen Integrationstests (`api.rs`, `failures.rs`, `edge_cases.rs` …) sind nach dem Verschieben bzw. Typisieren der DTOs **ohne Änderung** grün. Das ist der Beleg, dass die JSON-Form kompatibel blieb.

Playwright 37 = system 10 + desktop 17 + iphone 5 + iphone-landscape 5. Vorher waren es 24 (9 + 9 + 3 + 3).

## 2. Was die Tests prüfen

### Rust (Frontend, ersetzt Vitest – alle 11 alten Fälle portiert)

`logic::format` (Tokens, Geld, Dauer, Uhrzeit mit fester Zeitzone, relative Zeiten), `logic::budget` (Auslastung, Chips inkl. „Anmeldung vor allem anderen“, Balken, 90 %-Vorschlag, Budget-Eingaben), `logic::limits` (Minuten↔Sekunden, Zahlen mit Komma, **N6-Validierung mit deutschen Meldungen je Feld**, Abbildung auf Create/PATCH-Bodies), `logic::task` (Fehlerklassen-Tag, Backoff-Hinweis, Task-Limits), `logic::labels` (jede Status- und Fehlerklasse hat Text + Erklärung, OnEmpty-Aktionen identisch zum Backend), `logic::progress` (Phasen, kein Rückschritt, Neustart bei „Start #“), `logic::diff`, `logic::shortcuts` (Chords, Timeout), `logic::b64` (VAPID), `logic::errors` (Übersetzung), `store::push_event` (Dedupe/Ordnung), `theme`, Avatar-Initialen.

### Rust (`queued-api`)

Task-Rundreise und Accessoren, Status-/Fehlerklassen-Namen, PATCH „fehlt vs. null“ für `UpdateProject`/`CreateTask`/`UpdateTask`, SSE-Form von `ServerEvent` (`{"type":"task"…}`, `{"type":"resync"}`), flaches `ProviderStatus`, `SettingsUpdate`.

### Playwright

| Projekt | Tests |
|---|---|
| system | alle 9 bisherigen + **neu:** Trunk-Bundle wird mit `content-encoding: gzip` und `application/wasm` ausgeliefert, PWA-Dateien vorhanden, SPA-Deep-Links (`/projects/…?tab=git`, `/inbox`, `/settings`) liefern die App, unbekannte `/api/*` geben 404 (vorher HTML), SSE bleibt `text/event-stream` ohne Kompression |
| desktop | alle 9 bisherigen Abläufe (Login/Registrierung/Leerzustand, Live-Lauf→Details/Diff/Git/Dateien, Rückfrage inline, Freigabe „Alle 3“, Fix-Kette→Blockiert+Benachrichtigung, Budget pausiert, Reorder+Abbrechen, Projekt-Limits+Token-Limit-Abbruch, Transient-Retry, Auth-Pause aufheben). **Neu:** Onboarding (Projekt inline anlegen, Fortschritt 1→2→fertig, „Zum Eingabefeld“ fokussiert), Composer-Limits inkl. Ablehnung „1,5“ ohne Request, Bearbeiten im Detail inkl. Limits, Rückgängig mit Bestätigung + Fokus-Rückgabe + Revert-Commit, Verwerfen + „Wiederherstellen“, Abbrechen-Dialog (Fokus auf „Abbrechen“, Abbruch des Dialogs lässt den Lauf weiterlaufen), Projekt löschen (Escape bricht ab), Mitglied hinzufügen/entfernen, Tastaturkürzel (`g o`, `g e`, `?`, `n`, kein Auslösen beim Tippen), Skip-Link (erster Tab-Stopp, fokussiert `main`), Live-Region („go: Fertig“), Theme dunkel/System inkl. Reload, Budget-Eingabe einer anderen Zeile überlebt den Refresh, deutsche Fehlermeldungen an Feldern |
| iphone / iphone-landscape | Safe Areas wie bisher (Login, Brand, 4 Tabs, Composer, Details-Kopf/Schließen, Body-Padding, Toasts), **verschärft:** Tabs ≥ 44 px auch im Querformat, **alle** sichtbaren Bedienelemente in Kopf, Inhalt, Details, Toasts, Projekt- und App-Einstellungen ≥ 44×44 (Hilfe-Buttons 32 px mit 44 px Trefferfläche), alle Eingaben der Projekt-Einstellungen ≥ 16 px (auch `select`), Details-Inhalt frei von beiden seitlichen Insets, Toasts nicht über der Tab-Leiste, „Speichern“ über Tab-Leiste/Home-Indikator erreichbar, Skip-Link unsichtbar, kein horizontales Scrollen auch auf den Einstellungen |
| Accessibility (desktop, iphone, iphone-landscape) | axe-core mit `wcag2a, wcag2aa, wcag21a, wcag21aa, wcag22aa`, **hell und dunkel**, mit echten Daten (laufend, Rückfrage, wartend, fertig): Login, Aufträge, Details mit Diff und Protokoll; auf Desktop und iPhone hochkant zusätzlich Postfach, Projekte, Projekt-Dateien/Verlauf/Einstellungen/Mitglieder, Einstellungen mit geöffnetem Zugang, Kürzel-Dialog → **0 Verstöße** |

Selektoren wurden umgestellt: von CSS-Klassen (`.item.done`, `.seg`, `.chip.err`) auf Rollen und Labels (`getByRole("link"/"tab"/"alertdialog")`, `getByLabel(…, {exact})`) bzw. `article[data-status]`. Die fachlichen Prüfungen sind dieselben oder strenger (siehe oben). Neu im Ablauf sind die Bestätigungsdialoge und das standardmäßig zugeklappte Protokoll. Die Tests klicken deshalb „Auftrag abbrechen“ bzw. „Protokoll anzeigen“, bevor sie dieselben Inhalte prüfen.

## 3. Während des Testens gefundene und behobene Fehler

| # | Fund | Ursache | Fix | Test |
|---|---|---|---|---|
| T1 | axe: `nested-interactive` im Details-Fenster | Hilfe-Button in `<summary>` | Hilfetext in den Inhalt verschoben | a11y |
| T2 | axe: `target-size` | Hilfe-Button 22 px | 24 px (Touch: 32 px + 44 px Trefferfläche) | a11y |
| T3 | Escape schloss Details nach „Speichern“ nicht | Fokus fiel auf `<body>`, Handler hing am Drawer | Fenster-Listener + Fokus zurück in den Dialog | edit-Test |
| T4 | Bearbeiten-Formular ohne Fokus | `data-autofocus` greift nur beim Öffnen | Effekt fokussiert das Titelfeld | edit-Test |
| T5 | Kleine Trefferflächen der Zeilentitel (mobil) | Messung am Button statt an der Overlay-Fläche; laufende Karte ohne Overlay | Karte bekommt Overlay, Test misst die Zeile | mobile |
| T6 | Toast im Querformat 0,7 px zu tief | Messung während der Einblend-Animation | Test wartet auf Animationen | mobile |
| T7 | `/api/<unbekannt>` lieferte 200 + HTML | SPA-Fallback galt auch unter `/api` (vorbestehend) | JSON-404-Fallback im API-Router | system |
| T8 | Revert-Test 409 | Mock-Reviewer schreibt in dieselbe Datei (bekanntes Backend-Verhalten) | Test schaltet Review für sein Projekt ab; 409 erscheint im UI als verständlicher Toast | revert |
| T9 | Skip-Link ragte im iPhone-Screenshot unter der Dynamic Island hervor | `translateY(-200%)` reicht bei 59 px Inset nicht | unsichtbar per `clip-path` bis zum Fokus | mobile |
| T10 | Skip-Link wirkungslos | leptos_router fängt `#main` als Navigation ab | eigener Klick-Handler fokussiert `main` | keyboard |

## 4. Screenshots

Erzeugt von `a11y.spec.ts`/`mobile-a11y.spec.ts` mit `WP00_SCREENSHOTS_DIR=…/docs/roadmap/WP00-screenshots` (iPhone mit simulierten Insets 59/34 bzw. 59/59/21). Chromium-Emulation, kein echtes Safari.

| Ansicht | Desktop hell | Desktop dunkel | iPhone hell | iPhone dunkel |
|---|---|---|---|---|
| Login | [d-l](WP00-screenshots/desktop-light-login.png) | [d-d](WP00-screenshots/desktop-dark-login.png) | [i-l](WP00-screenshots/iphone-light-login.png) | [i-d](WP00-screenshots/iphone-dark-login.png) |
| Aufträge | [d-l](WP00-screenshots/desktop-light-queue.png) | [d-d](WP00-screenshots/desktop-dark-queue.png) | [i-l](WP00-screenshots/iphone-light-queue.png) | [i-d](WP00-screenshots/iphone-dark-queue.png) |
| Details (Diff + Protokoll) | [d-l](WP00-screenshots/desktop-light-drawer.png) | [d-d](WP00-screenshots/desktop-dark-drawer.png) | [i-l](WP00-screenshots/iphone-light-drawer.png) | [i-d](WP00-screenshots/iphone-dark-drawer.png) |
| Postfach | [d-l](WP00-screenshots/desktop-light-inbox.png) | [d-d](WP00-screenshots/desktop-dark-inbox.png) | [i-l](WP00-screenshots/iphone-light-inbox.png) | [i-d](WP00-screenshots/iphone-dark-inbox.png) |
| Projekte | [d-l](WP00-screenshots/desktop-light-projects.png) | [d-d](WP00-screenshots/desktop-dark-projects.png) | [i-l](WP00-screenshots/iphone-light-projects.png) | [i-d](WP00-screenshots/iphone-dark-projects.png) |
| Projekt-Einstellungen | [d-l](WP00-screenshots/desktop-light-project-settings.png) | [d-d](WP00-screenshots/desktop-dark-project-settings.png) | [i-l](WP00-screenshots/iphone-light-project-settings.png) | [i-d](WP00-screenshots/iphone-dark-project-settings.png) |
| Einstellungen | [d-l](WP00-screenshots/desktop-light-settings.png) | [d-d](WP00-screenshots/desktop-dark-settings.png) | [i-l](WP00-screenshots/iphone-light-settings.png) | [i-d](WP00-screenshots/iphone-dark-settings.png) |
| iPhone quer | | | [Aufträge](WP00-screenshots/iphone-landscape-light-queue.png), [Details](WP00-screenshots/iphone-landscape-light-drawer.png) | [Aufträge](WP00-screenshots/iphone-landscape-dark-queue.png), [Details](WP00-screenshots/iphone-landscape-dark-drawer.png) |

## 5. Nicht oder nur teilweise geprüft (ehrlich)

- **Web Push end-to-end:** Abo-Code (`PushManager.subscribe`, VAPID-Dekodierung getestet) läuft gegen keinen echten Push-Dienst. Headless-Chromium meldet „im Browser blockiert“. Das war vorher genauso ungetestet.
- **OIDC-Buttons:** kein IdP konfiguriert, nur der Code-Pfad (Link auf `/api/auth/oidc/{id}/start`) existiert.
- **Echtes iOS/WebKit:** nur Chromium mit iPhone-Viewport/UA. `light-dark()` braucht Safari ≥ 17.5.
- **`trunk serve`** (Entwicklung mit Proxy) wurde nicht gestartet.
- **`deploy/install.sh`:** nur `bash -n`. Ein echter Lauf bräuchte Root auf einem frischen Host.
- **Docker:** Ergebnis des `docker build --target web` siehe Review (Abschnitt „Docker“).
- Verlust ungespeicherter Eingaben in den allgemeinen Einstellungen bei einem SSE-Reconnect ist behoben (Render nur beim ersten Laden), aber nicht per E2E provoziert (Reconnect ist im Test nicht deterministisch auslösbar).
