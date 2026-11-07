# WP00 – Review

Prüfer-Haltung: als hätte jemand anderes das geschrieben. Geprüft habe ich den Code (`frontend/**`, `backend/crates/api`, den Backend-Diff, `e2e/**`, Build/Deploy), die Screenshots aller Ansichten in beiden Themes auf Desktop und iPhone und die Testläufe selbst.

## Urteil

**Abnahme mit Auflagen (nicht blockierend).** Das React/TS-Frontend ist vollständig durch Rust/Leptos ersetzt und funktional mindestens gleichwertig. Alle Tests sind auf dem finalen Stand zweimal grün (Zahlen unten): fmt, clippy, 190 Backend-, 35 Frontend- und 37 Playwright-Tests, Letztere inkl. axe-core hell/dunkel, Desktop/iPhone hoch/quer. Gegenüber dem React-UI ist das Ergebnis deutlich zugänglicher und verständlicher. Bei der Erstlade-Größe ist es schlechter (siehe N1). Blockierende Funde sind keine offen.

## Akzeptanzkriterien (Plan)

| AK | Status | Beleg |
|---|---|---|
| 01 kein TS/React/npm im Frontend | erfüllt | `frontend/` enthält nur Rust, Trunk, CSS, HTML, PWA-Dateien; `sw.js` ist die einzige JS-Datei |
| 02 Trunk-Build, Auslieferung über `FRONTEND_DIR` | erfüllt | `stack.sh` baut `trunk build --release` in jedem Playwright-Lauf; System-Test prüft Bundle, Gzip, Deep-Links |
| 03 geteilte DTOs, kein Backend-Verhalten geändert | erfüllt, mit 2 begründeten additiven Änderungen | `queued-api`; alle 184 alten Backend-Tests unverändert grün; JSON-404 unter `/api` und Gzip für statische Dateien (Impl. §4.4/4.5) |
| 04 Funktionsparität | erfüllt | Abgleich Impl. §6; E2E deckt alle alten Abläufe ab |
| 05 WP01-Umfang | erfüllt | Fehlerklassen-Tag, Backoff, Abbruchgrund, Limits, Pause, CLI-Version – E2E |
| 06 Task-Limits + Validierung (N6) | erfüllt | Composer- und Edit-Test, `logic::limits`-Tests |
| 07 Design-System | erfüllt | `style/app.css` Tokens, `light-dark()`, Theme-Wahl (E2E), Inline-SVG, `prefers-reduced-motion` |
| 08 Sprache/Hilfen/Onboarding/Leerzustände | erfüllt | `logic::labels` (Tests: jede Status-/Fehlerklasse erklärt), Onboarding-E2E |
| 09 Fortschritt statt Log | erfüllt | `logic::progress` + Schritte in Karte/Details; Protokoll zugeklappt |
| 10 Bestätigungen/Rückgängig | erfüllt | E2E: Abbrechen, Revert, Projekt löschen, Mitglied entfernen, Verwerfen→Wiederherstellen; Zugang entfernen nur Code-Review |
| 11 Tastaturkürzel | erfüllt | E2E + Unit |
| 12 WCAG 2.2 AA (axe 0 Verstöße), Live-Region | erfüllt im Umfang von axe | a11y-Specs; Live-Region-E2E. Grenze: axe findet nicht alles (s. N4) |
| 13 Mobil | erfüllt, strenger als vorher | mobile.spec |
| 14 PWA/Push | teilweise belegt | Manifest/SW/Icons per System-Test; Push-Abo nicht end-to-end testbar (N5) |
| 15 Tests zweimal grün | erfüllt | unten |
| 16 Build/Deploy umgestellt | erfüllt; Docker-Build hier nur mit Sandbox-CA geprüft, `install.sh` nur `bash -n` | §Docker |
| 17 Screenshots | erfüllt | `WP00-test.md` §4 |
| 18 fmt/clippy | erfüllt | unten |

## Funde

### Blockierend – im Review gefunden und behoben

| # | Fund | Fix |
|---|---|---|
| B1 | **Budget-Eingaben gingen verloren:** Die Budget-Zeilen waren nach dem kompletten `ProviderStatus` gekeyt und wurden bei jedem Limit-Refresh (60 s, nach jedem Lauf, nach jedem Speichern) neu gebaut. Ungespeicherte Eingaben verschwanden. | Keyed nach Anbieter, Anzeige reaktiv über `Memo`, Eingaben stabil. E2E: Eingabe in der Claude-Zeile überlebt das Speichern der Mock-Zeile |
| B2 | **Allgemeine Einstellungen verloren Eingaben** bei jedem `reload()` (SSE-Reconnect). | Formular wird nur beim ersten Eintreffen der Einstellungen gerendert (`Memo<bool>`) |
| B3 | **Details-Fenster baute sich bei jeder Task-Änderung komplett neu:** Das Protokoll klappte zu, der Fokus ging verloren. | Kopf/Aktionen/Details einzeln reaktiv; Protokoll und Diff bleiben stehen |
| B4 | **Panics möglich** (`expect` beim Projektwechsel in `project.rs`). Mit `panic = "abort"` wäre die ganze App tot. | Alle `expect` außerhalb von Tests entfernt (Rückgabe leerer Ansicht) |
| B5 | **Skip-Link ragte auf dem iPhone unter der Dynamic Island hervor** (Screenshot), und der Router machte ihn wirkungslos. | `clip-path` bis zum Fokus, eigener Klick-Handler; E2E (Desktop + mobil) |
| B6 | Escape nach dem Speichern im Details-Fenster wirkungslos (Fokus auf `<body>`). | Fenster-Listener + Fokus-Rückgabe; E2E |
| B7 | `/api/<unbekannt>` → 200 mit `index.html` (vorbestehend, durch neuen System-Test aufgefallen). | JSON-404 |

### Nicht blockierend – behoben

- Projektname fehlte in der „Läuft“-Karte (nicht reaktiv). Behoben, im Screenshot geprüft.
- Mobile Composer-Leiste brach unschön um, „Strg+Enter“ auf Touch sinnlos, der „+“-Button verlor seine Primärfarbe (`.icon-only` überschrieb `.primary`). Behoben.
- Hilfe-„?“ in `<summary>` (axe `nested-interactive`), 22-px-Ziele (axe `target-size`). Behoben.
- Felder mit Hilfe-Button standen 4 px tiefer als ohne (Screenshot Projekt-Limits). Behoben über `min-height` der Label-Zeile.
- „Zusammenhang“ (Reparatur-/Folgeaufträge) war nicht live. Jetzt `Memo`.
- Doppelte Live-Regionen bei Toasts reduziert (Container `aria-live`, nur Fehler `role=alert`).
- Full-Page-Screenshots zeigten die Seitenleiste mitten auf der Seite (Sticky + Scroll). Der Screenshot-Helper scrollt jetzt vorher nach oben.

### Nicht blockierend – offen (bewusst, mit Begründung)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **WASM-Bundle 1,52 MB (gzip 0,60 MB)**, React lag geschätzt bei ~0,15 MB gzip. Erstladen über Mobilfunk merklich langsamer. Leptos-CSR hat kein Code-Splitting. | Gzip ist jetzt aktiv. Weitere Hebel (Brotli vorkomprimiert, `view!`-Typen mit `.into_any()` an Grenzen reduzieren) wären ein eigenes Paket. Kein Funktionsfehler. |
| N2 | `light-dark()` braucht Safari ≥ 17.5. Ältere iPhones sehen keine Farben. | Im Plan akzeptiert. Ein Fallback würde die Token-Definition verdoppeln. |
| N3 | „Wiederherstellen“ nach Verwerfen reiht am **Ende** ein (Backend `retry`). | Im UI so angekündigt („wieder am Ende der Warteschlange“). Eine echte Positions-Wiederherstellung bräuchte eine Backend-API. |
| N4 | axe deckt nur automatisch prüfbare WCAG-Kriterien ab. Ein Screenreader-Durchgang (VoiceOver/NVDA) fand nicht statt. | Live-Region, Fokusfallen, Tab-Muster und Labels sind per E2E geprüft. Ein manueller Durchgang wäre der nächste Schritt. |
| N5 | Web Push, OIDC und echtes iOS-Safari nicht end-to-end getestet (keine Dienste/Geräte). | Vorher ebenso ungetestet, ehrlich in `WP00-test.md` §5. |
| N6 | Ohne gespeicherte Auswahl ist das Composer-Projekt das alphabetisch erste beschreibbare, nicht das zuletzt benutzte. | Wie bei React. Die Auswahl wird gemerkt. |
| N7 | Einstellungen zeigen Budget-Zeilen für alle Anbieter, auch ungenutzte. Die Seite ist lang. | Parität zu React. Eine Unternavigation wäre ein Komfortthema. |
| N8 | Release-Build des Frontends ~1 m 45 s (LTO, `opt-level=z`), CI-Job installiert Trunk/wasm-bindgen per `cargo install` (~2 min ohne Cache). | Akzeptabel. Mit `rust-cache` wird es nur beim ersten Lauf teuer. |
| N9 | Revert schlägt mit 409 fehl, wenn der Review-Agent dieselbe Datei verändert hat (Mock-Reviewer tut das). | Bekanntes Backend-Verhalten seit WP01 („Review-Agent ist per Prompt read-only, nicht technisch erzwungen“). UI zeigt eine verständliche Meldung. |

## Docker

`docker build --target web` scheiterte hier zunächst an der TLS-Interception des Sandbox-Proxys: `rustup` innerhalb des Images vertraut der Proxy-CA nicht (`invalid peer certificate: UnknownIssuer`). Das ist ein Umgebungsproblem, kein Fehler des Dockerfiles. Gegenprobe mit einer Kopie des Dockerfiles, die zusätzlich nur die Sandbox-CA ins Image legt (`--build-context cacert=…`, sonst identisch): In der Stufe `web` liefen `rustup target add`, `cargo install --locked trunk@0.22.1 wasm-bindgen-cli@0.2.130` (487 s) und `trunk build --release` (110 s, „success“) durch. Danach brach das Exportieren des Images mit `failed to extract layer` ab, weil die Platte voll lief (99 %, 527 MB frei). Die Stufen `api` und das finale Image habe ich deshalb hier **nicht** gebaut. Sie sind gegenüber vorher unverändert bis auf `COPY --from=web /src/frontend/dist`. Das ausgelieferte `Dockerfile` enthält keine Sandbox-Spezifika. Danach habe ich die Docker-Daten wieder gelöscht.

## Finale Testergebnisse (zweimal, finaler Stand)

| Schritt | Lauf 1 | Lauf 2 |
|---|---|---|
| Backend `cargo fmt --all -- --check` | ok | ok |
| Backend `cargo clippy --workspace --all-targets -- -D warnings` | 0 Warnungen | 0 Warnungen |
| Backend `cargo test --workspace` (Postgres) | 190 passed / 0 failed | 190 passed / 0 failed |
| Frontend `cargo fmt --all -- --check` | ok | ok |
| Frontend `cargo clippy` wasm32 + nativ, `-D warnings` | 0 / 0 | 0 / 0 |
| Frontend `cargo test` | 35 passed | 35 passed |
| `cd e2e && npx playwright test` (system 10, desktop 17, iphone 5, iphone-landscape 5) | 37 passed | 37 passed |

Screenshots: `docs/roadmap/WP00-screenshots/` (32 Dateien, Index in `WP00-test.md` §4).
