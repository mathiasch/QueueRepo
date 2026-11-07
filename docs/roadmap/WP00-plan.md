# WP00 – Plan: GUI auf Rust (Leptos) umstellen und neu gestalten

Grundlage: `docs/roadmap/work-packages.md` (gemeinsame Regeln + Abschnitt WP00), `WP01-*.md` (UI-Ergänzungen, offener Fund N6), React-Code unter `frontend/src/` (2 443 Zeilen, vollständig gelesen), Backend-Routen `backend/crates/app/src/api/*.rs`, E2E-Tests `e2e/tests/**`.

Ein Spike vorab (Scratchpad, nicht im Repo) hat die Toolchain belegt: Leptos 0.8.23 (`csr`) baut für `wasm32-unknown-unknown` und nativ (für `cargo test`), `wasm-bindgen` löst auf **0.2.130** auf, `cargo install wasm-bindgen-cli --version 0.2.130 --locked` funktioniert, `trunk build --release` lädt `wasm-opt version_131` von GitHub (hier erreichbar).

## 1. Ist-Inventar React-UI

### 1.1 Screens und Funktionen

| Screen | Funktionen (React) |
|---|---|
| Login (`App.tsx/Login`) | E-Mail/Passwort-Login, Registrierung (Name, min. 8 Zeichen) nur wenn `meta.allow_registration`, OIDC-Buttons je `meta.oidc[]` → `/api/auth/oidc/{id}/start`, Fehler aus `?login_error=` und API-Fehlertext |
| Shell (`App.tsx/Shell`) | Tabs Queue/Inbox/Projekte/Einstellungen (Tab in `localStorage`), Badges (laufend+wartend; Rückfragen+Freigaben+ungelesen, warn-Farbe bei Aufmerksamkeit), Puls bei laufenden Aufträgen, Limit-Chips je Provider (nur wenn Runs/Block/Budget) mit Tooltip (5h/7d/30d, Cache, Budget-Grund), Toasts (max. 4, 7 s, Klick öffnet Task), Drawer per `?task=` |
| Queue (`Queue.tsx`) | Leerzustand ohne Projekt („Lege zuerst ein Projekt an“), Composer (Prompt mit ⌘/Ctrl+Enter, Entwurf in `localStorage`, Projektwahl nur Owner/Editor, „als Nächstes“, Optionen: Kontext, Fortsetzung von (Handoff, letzte 20 erledigten Tasks des Projekts), Provider, Modell), Sektionen „Läuft“, „Braucht dich“ (Rückfragen mit Inline-Antwort, Freigaben, „Alle N freigeben“ ab 2), „Warteschlange (N)“ (⤒ ↑ ↓, Verwerfen), „Verlauf“ (Filter Alle/Erledigt/Probleme, max. 100), Zeilen-Tags Fix/Auto/Agent/Fehlerklasse (WP01), Meta: Projekt, Provider/Modell, Laufzeit, Endzeit, Backoff-Hinweis „wartet bis … (Wiederholung n)“ (WP01), letzte Live-Zeile, Zusammenfassung; Aktionen Freigeben, Abbrechen/Verwerfen, Erneut |
| Task-Drawer (`TaskDrawer.tsx`) | Kopf mit Status/Provider/Art/Versuch/Dauer, Aktionen Bearbeiten (Titel/Prompt/Kontext), Freigeben, Abbrechen/Verwerfen, Erneut versuchen, Änderungen anzeigen/ausblenden (Diff), Rückgängig (confirm, `revert`), Blöcke: Abbruchgrund (WP01, `data-testid=abort-reason`, „Kein Fix-Versuch verbraucht.“), Limits des Auftrags (WP01), Fehler, Rückfragen, Zusammenfassung, Review ✓/✗ mit Punkten, Handoff (einklappbar), Auftrag, Kontext, Kette (Eltern/Kinder), Verbrauch; Live-Log mit Event-Arten (`tool`, `error`, `stderr`, `limit`, `retry`, `guard`, `contract`, `review`, `verify`, `status`, `text`), Auto-Scroll, Escape schließt |
| Inbox (`Inbox.tsx`) | „Wartet auf dich (N)“, Benachrichtigungen (gelesen/ungelesen, Klick markiert gelesen und öffnet Task, „Alle gelesen“) |
| Projekte (`Projects.tsx`) | Neues Projekt (Name, Ziel, optional Klonen: Remote, Branch, Token), Karten (Name, Rolle, Beschreibung/Owner, Remote). Detail: ZIP-Download, Tabs Dateien (Filter, max. 500, ZIP-Import, Datei-Viewer), Git (Remote/Branch, „Jetzt pushen“, Commit-Liste), Aufträge, Einstellungen (Name, Beschreibung, Ziel, Verify-Befehl, Review an/aus + Provider + Modell, **Limits pro Auftrag** (WP01: Laufzeit/Stillstand in Min., Kosten USD, Tokens, Turns, Hinweis auf nicht erzwungene Limits je `provider_capabilities`), Git Remote/Branch/Token (gesetzt-Tag, leeren entfernt), Auto-Push, Speichern, Löschen (Owner, confirm)), Mitglieder (Liste, Entfernen durch Owner bzw. sich selbst, Hinzufügen per E-Mail mit Rolle Editor/Viewer) |
| Einstellungen (`SettingsView.tsx`) | Ausführung (Parallelität, Standard-Provider, Fix-Versuche, Auto-Freigabe bis), OnEmptyQueue (an/aus, Aktionen review/research/improve, Cooldown, zuletzt), Benachrichtigungen (ntfy-URL, Web Push aktivieren/erneuern, Test, Status), Token-Budgets je Provider (Tags Pause/Abo-Limit/Budget erschöpft, „Pause aufheben“ (WP01), CLI-Version (WP01), drei Balken, drei Budget-Felder, Speichern, beobachtetes Limit → 90 %), Provider-Zugänge (Art wählen, Hilfetext je Art, Secret einfügen, Ändern, Entfernen mit confirm), Konto (Name, E-Mail, Abmelden) |

### 1.2 API-Aufrufe des UI

`GET /meta`, `GET /auth/me`, `POST /auth/login|register|logout`, `GET /tasks`, `POST /tasks`, `GET|PATCH /tasks/{id}`, `POST /tasks/{id}/cancel|retry|approve|answer|move|revert`, `GET /tasks/{id}/diff`, `POST /tasks/approve-all`, `GET /notifications`, `POST /notifications/read-all`, `POST /notifications/{id}/read`, `GET /settings`, `PUT /settings`, `GET /limits`, `POST /limits/{p}/resume`, `PUT /budgets/{p}`, `GET /credentials`, `PUT|DELETE /credentials/{p}`, `GET /push/key`, `POST /push/subscribe`, `POST /push/test`, `GET /events` (SSE: `task`, `task_event`, `notification`, `resync`), `GET|POST /projects`, `GET|PATCH|DELETE /projects/{id}`, `GET|POST /projects/{id}/members`, `DELETE /projects/{id}/members/{uid}`, `GET /projects/{id}/files`, `GET /projects/{id}/file?path=`, `GET /projects/{id}/zip` (Link), `POST /projects/{id}/import` (Octet-Stream), `GET /projects/{id}/tasks`, `GET /projects/{id}/commits`, `POST /projects/{id}/push`.

### 1.3 Vitest-Logik (wird nach Rust portiert)

`fmtTokens`, `duration`, `budgetUsage`, `limitChip`, `failureTag`, `FAILURE_LABEL`, `retryHint`, `secsToMin`, `minToSecs`, `numOrNull`, `taskLimits`, `providerLabel`, `b64ToBytes` – 11 Tests.

## 2. Architektur

### 2.1 Crates

| Crate | Pfad | Inhalt |
|---|---|---|
| `queued-api` (neu) | `backend/crates/api` | **Geteilte HTTP-DTOs** App ↔ Browser: `User`, `Task`, `TaskEvent`, `Usage`, `Settings`, `SettingsUpdate`, `Budget`, `Window`, `ProviderStatus`, `ProjectSummary`, `ProjectView`, `Member`, `Commit`, `CredentialInfo`, `Notification`, `Meta`, `ProviderCapabilities`, `ServerEvent` (SSE), Request-Bodies (`CreateTask`, `UpdateTask`, `TaskLimitFields`, `NewProject`, `UpdateProject`, `AddMember`, `PutCredential`, `Answer`, `MoveTask`, `LoginRequest`, `RegisterRequest`) und Accessoren (`Task::questions_list()`, `review_parsed()`, `usage_parsed()`). Abhängigkeiten nur `serde`, `serde_json`, `uuid`, `chrono`; Feature `sqlx` leitet `FromRow` ab (nur im Backend aktiv). Damit kompiliert die Crate für WASM. |
| `queued` | `backend/crates/app` | nutzt `queued-api` mit Feature `sqlx`; `models.rs`/`budget.rs` re-exportieren die verschobenen Typen, die `json!`-Antworten für Projekte, Mitglieder, Benachrichtigungen, Meta, Zugänge und SSE werden typisiert. **JSON-Form bleibt byte-kompatibel** (Feldnamen, `null`-Semantik, PATCH-„absent vs. null“). Keine DB-Änderung. |
| `queued-common` | unverändert | bleibt reines Runner-Protokoll (Scope nicht aufweichen). |
| `queued-web` (neu, ersetzt React) | `frontend/` | eigener Cargo-Workspace (Ziel `wasm32-unknown-unknown`, Logik-Tests nativ), Build mit Trunk nach `frontend/dist` (Pfad bleibt, `FRONTEND_DIR` unverändert). |

Warum `frontend/` nicht im Backend-Workspace: `cargo clippy --workspace` und `cargo test --workspace` des Backends sollen nicht Leptos bauen; Trunk braucht ohnehin ein WASM-Target. Die geteilte Crate wird per Pfad eingebunden.

### 2.2 Frontend-Module

```
frontend/
  Cargo.toml  Cargo.lock  Trunk.toml  index.html
  public/     sw.js (JS, Browser-Vorgabe)  manifest.webmanifest  icon.svg  icon-192.png  icon-512.png
  style/app.css          Design-Tokens + Komponenten (eine Datei, von Trunk gehasht)
  src/main.rs            mount
  src/lib.rs             run(): Panic-Hook, Theme, Service Worker, App
  src/logic/*.rs         reine Logik, nativ getestet: format, labels, budget, limits, progress, diff, shortcuts, b64
  src/api.rs             HTTP-Client (gloo-net), ApiError mit deutscher Meldung
  src/store.rs           globaler Zustand (Signals), SSE, Toasts, Live-Region, Fortschritt
  src/ui/*.rs            Design-System-Komponenten: icons, button, field, dialog, toast, status, empty, help, progress
  src/pages/*.rs         login, shell, queue, composer, drawer, inbox, projects, project, settings, onboarding
```

### 2.3 Router, State, SSE, API

- **Router** (`leptos_router` 0.8): `/` Aufträge, `/inbox`, `/projects`, `/projects/:id` (Tab per `?tab=files|git|tasks|settings|members`), `/settings`. Der Drawer hängt an `?task=<id>` (kompatibel zu Push-/ntfy-Links `PUBLIC_URL/?task=…`). Öffnen legt einen History-Eintrag an, Schließen geht zurück, wenn der Drawer im Programm geöffnet wurde (sonst `replace`) – Zurück-Taste auf dem iPhone schließt den Drawer. Backend-SPA-Fallback (`ServeFile index.html`) existiert bereits.
- **Store** (`provide_context`): `me`, `meta`, `tasks`, `projects`, `settings`, `limits`, `notifications`, `credentials`, `last_event` (je Task), `progress` (Phase je laufendem Task), `toasts`, `announce` (Live-Region-Text), Drawer-Senken (`HashMap<Uuid, RwSignal<Vec<TaskEvent>>>`, SSE schreibt direkt hinein; Duplikate per Event-ID verworfen – React konnte Events zwischen Fetch und Abo doppelt zeigen).
- **SSE**: `web_sys::EventSource` auf `/api/events`, Nachrichten als `queued_api::ServerEvent` (Backend serialisiert denselben Typ). Fehler → schließen, nach 3 s neu laden und neu verbinden (wie bisher), `resync` → neu laden, Limits alle 60 s und bei Statuswechseln.
- **API-Client**: `gloo-net` `fetch` mit `same-origin`-Cookies; Fehler `{"error": …}` → `ApiError{status, message}`.
- **Geteilte DTOs**: Frontend importiert ausschließlich `queued_api`-Typen; kein handgeschriebenes Typ-Duplikat. Zeiten sind `chrono::DateTime<Utc>` (Feature `wasmbind` nur im Frontend für `Local`/`now`).

## 3. Design-System

- **Tokens** (`:root`, CSS-Custom-Properties): Farben semantisch (`--bg`, `--surface`, `--surface-2`, `--border`, `--text`, `--text-muted`, `--primary`, `--on-primary`, `--primary-soft`, `--success/-soft`, `--warning/-soft`, `--danger/-soft`, `--run/-soft`, `--focus`), Abstände 4er-Skala (`--sp-1…--sp-8`), Radien (`--r-sm 8px`, `--r-md 12px`, `--r-lg 16px`, `--r-full`), Schatten (`--shadow-sm/md/lg`), Typo (Systemschrift, `--fs-xs 12…--fs-2xl 28`), Bewegung (`--dur-fast 120ms`, `--dur 200ms`, `--ease`). Safe Areas bleiben `--sat/--sar/--sab/--sal` (Tests injizieren sie).
- **Hell/Dunkel**: Farben über `light-dark()` mit `color-scheme`; Auswahl „System/Hell/Dunkel“ in Einstellungen → `data-theme` auf `<html>` (localStorage). `theme-color`-Meta für beide Schemata.
- **Komponenten**: Button (primary/secondary/ghost/danger, `sm`, Icon-Button mit Pflicht-`aria-label`), Card, StatusPill (Farbe + Icon + Text, nie Farbe allein), Tag, Field (Label, Hilfe, Fehler, `aria-describedby`), Switch (`role=switch`), Segmented (Radiogruppe), Tabs (`role=tablist`), Dialog (Bestätigung, Fokusfalle, Escape, Fokus-Rückgabe), Drawer (`role=dialog`, `aria-modal`), Toast (Region `role=status`/`alert`, Aktion „Rückgängig“), EmptyState (Icon, Text, Aktion), HelpTip (Toggletip-Button „?“ mit Erklärung, per Tastatur/Touch bedienbar), ProgressSteps, Balken (`role=meter`-artig mit `aria-valuenow`).
- **Icons**: Inline-SVG (Stroke 2, 24er Raster, Lucide-Pfade, ISC-Lizenz im Kopf von `icons.rs`), kein CDN, keine Webfonts.
- **Bewegung**: Drawer/Dialog/Toast mit kurzen Übergängen, Puls bei „Läuft“; `prefers-reduced-motion` schaltet alles ab.

## 4. UX-Konzept

- **Sprache**: Status „Wartet“, „Läuft“, „Wartet auf dich“, „Freigabe nötig“, „Fertig“, „Fehlgeschlagen“, „Blockiert“, „Abgebrochen“ – jeweils mit Ein-Satz-Erklärung (Tooltip/Drawer). Fachbegriffe (Handoff, Verify, Review, Provider, OnEmptyQueue, Turns, Token) nur mit HelpTip. Navigation: „Aufträge“, „Postfach“, „Projekte“, „Einstellungen“.
- **Onboarding**: Karte „Los geht's“ mit drei Schritten und Fortschritt (Projekt anlegen – direkt in der Karte; Anbieter verbinden – Link zu Zugängen, erledigt bei vorhandenem Zugang oder Standard-Anbieter `mock`; ersten Auftrag schreiben). Sichtbar, bis alle Schritte erledigt sind oder der Nutzer sie ausblendet.
- **Prominente Hauptaktion**: Composer „Neuer Auftrag“ oben in Aufträge; Desktop-Seitenleiste mit Button „Neuer Auftrag“ (springt zum Composer), mobil „+“ im Kopf. Seltene Optionen unter „Erweitert“ (Kontext, Fortsetzung von, Anbieter, Modell, **Limits dieses Auftrags** – schließt WP01-N6).
- **Fortschritt statt Log**: laufende Aufträge zeigen Schritte „Vorbereiten → Umsetzen → Prüfen → Abschließen“ (aus Event-Arten abgeleitet: `Start #` → Umsetzen, `verify`/`review` → Prüfen, Commit/Push/„Erledigt“ → Abschließen), Laufzeit und letzte Aktivität; Protokoll im Drawer aufklappbar.
- **Leerzustände** mit Erklärung und Aktion (keine Projekte, leere Warteschlange, leerer Verlauf, leeres Postfach, leerer Workspace, keine Commits, keine Mitglieder).
- **Sicherheit/Rückgängig**: Bestätigungsdialog für Abbrechen eines laufenden Auftrags, Änderungen zurücksetzen (revert), Projekt löschen, Zugang entfernen, Mitglied entfernen. Verwerfen eines wartenden Auftrags ohne Dialog, dafür Toast „Wiederherstellen“ (`retry`, reiht am Ende wieder ein – im UI so benannt).
- **Tastaturkürzel**: `n` Neuer Auftrag, `g a/p/o/e` Aufträge/Postfach/Projekte/Einstellungen (o = prOjekte vermeidet Kollision mit Postfach), `?` Kürzel-Hilfe, `Esc` schließt Drawer/Dialog, `⌘/Ctrl+Enter` sendet Composer/Antwort/Bearbeiten. Kürzel greifen nicht in Eingabefeldern.
- **Touch**: alle Bedienelemente ≥ 44×44 px bei `pointer: coarse`, Inputs 16 px, Tab-Leiste unten (hochkant), Seitenleiste im Querformat.
- **WP01-N6** (Review-Fund): Limit-Eingaben werden clientseitig geprüft (ganze positive Zahlen für Tokens/Turns, Minuten ≥ 1 s, Kosten > 0) mit deutscher Meldung am Feld, bevor der Server 400/422 liefern kann.

## 5. Accessibility (WCAG 2.2 AA)

Landmarks (`header`, `nav aria-label`, `main`), Skip-Link, eine `h1` je Seite, sichtbarer Fokus (`:focus-visible`, 2 px Ring, Kontrast ≥ 3:1), Kontraste Text ≥ 4.5:1 / UI ≥ 3:1 in beiden Themes, alle Eingaben mit `<label>`, Fehler mit `aria-invalid` + `aria-describedby`, Status nie nur Farbe, Live-Region (`aria-live=polite`) meldet Statuswechsel („Auftrag „X“: Fertig“), Toasts als `role=status` (Fehler `role=alert`), Dialoge mit Fokusfalle und Fokus-Rückgabe, Zielgrößen ≥ 24 px (2.5.8) bzw. 44 px auf Touch, `prefers-reduced-motion`, `lang="de"`.

## 6. Teststrategie

| Ebene | Was | Werkzeug |
|---|---|---|
| Rust-Unit Frontend | alle portierten Vitest-Fälle + Fortschrittsphasen, Diff-Klassifikation, Limit-Validierung (N6), Kürzel-Automat, Zeitformat mit fester Zeitzone, Status-/Hilfetexte vollständig | `cd frontend && cargo test` |
| Rust-Unit `queued-api` | JSON-Rundreise aller DTOs, PATCH-„absent vs. null“, `ServerEvent`-Form identisch zur bisherigen | `cargo test -p queued-api` |
| Backend | unverändert alle Unit- + Integrationstests grün (JSON-Kompatibilität) | `cargo test --workspace` |
| System | unverändert | Playwright `system` |
| E2E desktop | alle bisherigen Abläufe, Selektoren auf neues Markup (Rollen/Labels/`data-status` statt CSS-Klassen), zusätzlich Bestätigungsdialoge, Onboarding, Composer-Limits, Kürzel | Playwright `desktop` |
| E2E mobil | iPhone hoch/quer: Safe Areas (Login, Kopf, Tabs, Composer, Drawer, Toasts, Projekt-Limits), Tabs ≥ 44 px, **alle sichtbaren Buttons ≥ 44 px**, Inputs ≥ 16 px, kein horizontales Scrollen | Playwright `iphone`, `iphone-landscape` |
| Accessibility | axe-core (`@axe-core/playwright`, Tags `wcag2a/aa`, `wcag21a/aa`, `wcag22aa`) auf Login, Aufträge (mit Daten), Drawer, Postfach, Projekt-Tabs, Einstellungen – hell **und** dunkel, desktop und iPhone: 0 Verstöße | Playwright |
| Screenshots | Desktop + iPhone, hell + dunkel, nach `docs/roadmap/WP00-screenshots/` (gesteuert über `WP00_SCREENSHOTS_DIR`) | Playwright |

## 7. Build/Deploy

`e2e/scripts/stack.sh`: `trunk build --release` statt `npm run build`. `Dockerfile`: Stufe `rust` mit `wasm32-unknown-unknown`, `cargo install --locked trunk@0.22.1 wasm-bindgen-cli@0.2.130`, `trunk build --release`. `deploy/install.sh`: Node/npm entfällt, Rust-Target + Trunk + wasm-bindgen-cli. CI: Frontend-Job `cargo fmt --check`, `cargo clippy` (wasm + nativ, `-D warnings`), `cargo test`, `trunk build --release`; E2E-Job installiert Trunk/wasm-bindgen. README angepasst. React/Vite/Vitest/`node_modules`/`package*.json`/`tsconfig*`/`vite.config.ts`/`src/*.tsx` werden gelöscht.

## 8. Akzeptanzkriterien

- **AK01** `frontend/` enthält kein TypeScript/React/Vite/npm-Projekt mehr; `git ls-files frontend` zeigt nur Rust-, Trunk-, CSS-, HTML-, PWA-Dateien (`sw.js` einzige JS-Datei).
- **AK02** `cd frontend && trunk build --release` erzeugt `dist/index.html` mit gehashtem WASM/JS/CSS, `sw.js`, Manifest, Icons; das Backend liefert es unverändert über `FRONTEND_DIR` aus.
- **AK03** Frontend nutzt für alle API-Daten Typen aus `queued-api`; das Backend serialisiert dieselben Typen (inkl. SSE). Keine DB-Migration, kein Backend-Verhalten geändert.
- **AK04** Funktionsparität: jede Zeile aus 1.1 ist im neuen UI vorhanden (Abgleich in `WP00-impl.md`).
- **AK05** WP01-Umfang: Fehlerklassen-Tag, Backoff-Hinweis, Abbruchgrund, Task-Limits im Drawer, Projekt-Limits mit Hinweis, Pause-Tag + „Pause aufheben“, CLI-Version.
- **AK06** Composer-„Erweitert“ setzt Task-Limits; Drawer-„Bearbeiten“ ändert sie; ungültige Limit-Eingaben werden vor dem Request mit deutscher Meldung am Feld abgelehnt (N6).
- **AK07** Design-System: Tokens für Farbe/Abstand/Radius/Schatten/Typo/Bewegung, Hell/Dunkel + manuelle Wahl, Inline-SVG-Icons, `prefers-reduced-motion`.
- **AK08** Verständliche Sprache: Status mit Erklärung, HelpTips für Handoff/Verify/Review/Provider/OnEmptyQueue/Turns/Tokens, Onboarding mit drei Schritten, Leerzustände mit Aktion.
- **AK09** Fortschrittsanzeige für laufende Aufträge (Schritte + Laufzeit + letzte Aktivität), Protokoll aufklappbar.
- **AK10** Bestätigung für alle destruktiven Aktionen (laufend abbrechen, revert, Projekt löschen, Zugang entfernen, Mitglied entfernen); Verwerfen mit „Wiederherstellen“.
- **AK11** Tastaturkürzel wie in 4. beschrieben, mit Hilfedialog.
- **AK12** Accessibility: axe-core 0 Verstöße (WCAG 2.0/2.1/2.2 A+AA) auf allen Hauptansichten, hell und dunkel, desktop und iPhone; Live-Region meldet Statuswechsel.
- **AK13** Mobil: Safe Areas, Tab-Leiste unten ≥ 44 px, alle sichtbaren Buttons ≥ 44 px, Inputs ≥ 16 px, kein horizontales Scrollen – iPhone hoch und quer.
- **AK14** PWA: Manifest, Icons, `sw.js` registriert, Web-Push-Abo über Rust (`PushManager.subscribe`), Push-Klick öffnet `?task=`.
- **AK15** Tests: Frontend-`cargo test` (≥ alle 11 portierten Vitest-Fälle + neue Logik), `queued-api`-Tests, Backend fmt/clippy/test, alle Playwright-Projekte grün – zweimal hintereinander.
- **AK16** Build/Deploy umgestellt: `stack.sh`, `Dockerfile`, `deploy/install.sh`, CI, README.
- **AK17** Screenshots Desktop + iPhone, hell + dunkel unter `docs/roadmap/WP00-screenshots/`, im Testbericht verlinkt.
- **AK18** Qualität: `cargo fmt --all` (Backend + Frontend), clippy `-D warnings` Backend und Frontend (wasm32 + nativ).

## 9. Risiken

| Risiko | Gegenmaßnahme |
|---|---|
| Backend-JSON ändert sich beim Typisieren unbemerkt | Rundreise-Tests in `queued-api`, alle Integrationstests, System-Tests |
| `light-dark()` erst ab iOS 17.5 | akzeptiert (PWA-Zielgeräte 2026); dokumentiert |
| WASM-Größe/Startzeit | Release-Profil `opt-level="z"`, LTO, `wasm-opt -Oz` via Trunk |
| Trunk-Download von wasm-bindgen/wasm-opt blockiert | wasm-bindgen-cli lokal installiert (Version gepinnt), wasm-opt-Version in `Trunk.toml` gepinnt; Fehlschlag wird dokumentiert |
| Leptos-Effekte laufen asynchron → verpasste SSE-Events | Events werden in Senken geschrieben, nicht über Effekte |
