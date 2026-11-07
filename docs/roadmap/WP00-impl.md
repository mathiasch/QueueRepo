# WP00 – Umsetzung: Rust/Leptos-Frontend und Redesign

Grundlage: `WP00-plan.md`. Hier steht, was gebaut wurde, wo es liegt und wo ich vom Plan abgewichen bin (mit Begründung). Testergebnisse: `WP00-test.md`, Urteil: `WP00-review.md`.

## 1. Ergebnis in Zahlen

| | |
|---|---|
| React/TS entfernt | `frontend/src/*.tsx`, `api.ts`, `api.test.ts`, `styles.css`, `package*.json`, `tsconfig*`, `vite.config.ts`, `node_modules`, alter `dist` |
| Neues Frontend `queued-web` | 7 426 Zeilen Rust in `frontend/src/**`, 666 Zeilen CSS (`frontend/style/app.css`, lange Zeilen), `index.html`, `Trunk.toml`, `Cargo.toml/.lock` |
| Geteilte DTO-Crate `queued-api` | `backend/crates/api/src/lib.rs` (inkl. 6 Tests) |
| Backend-Diff | 15 Dateien, +325/−452 Zeilen (Typen verschoben, `json!`-Antworten typisiert, Gzip für statische Dateien, JSON-404 unter `/api`) |
| Release-Bundle | WASM 1,51 MB (gzip 0,60 MB, wird jetzt komprimiert ausgeliefert), JS-Glue 63 KB, CSS 44 KB |

## 2. Architektur

```
backend/crates/api   (queued-api)  serde-DTOs, Feature "sqlx" → FromRow  ◄── backend/crates/app (queued)
        ▲                                                                 
        └─────────── Pfad-Abhängigkeit ─── frontend/ (queued-web, eigener Cargo-Workspace)
                                             src/logic/*   reine Logik, nativ getestet
                                             src/api.rs    gloo-net-Client, deutsche Fehlermeldungen
                                             src/store.rs  Signals, SSE (web_sys::EventSource), Toasts, Live-Region
                                             src/ui/*      Design-System (Icons, Field, HelpTip, Modal, Meter …)
                                             src/pages/*   Login, Shell, Aufträge, Composer, Onboarding, Details, Postfach, Projekte, Projekt, Einstellungen
```

- **Leptos 0.8.23 CSR + leptos_router 0.8**, Build `trunk build --release` (wasm-opt `-Oz`, Version `version_131` in `Trunk.toml` gepinnt; `wasm-bindgen = "=0.2.130"` exakt gepinnt, passend dazu `cargo install wasm-bindgen-cli --version 0.2.130`). Ausgabe weiter nach `frontend/dist`, also ohne Änderung an `FRONTEND_DIR`.
- **Routen:** `/`, `/inbox`, `/projects`, `/projects/:id?tab=…`, `/settings`; Details-Ansicht über `?task=<id>` (kompatibel zu den Push-/ntfy-Links `PUBLIC_URL/?task=…`). Öffnen legt einen History-Eintrag an, Schließen geht zurück (iPhone-Zurück-Geste schließt die Details), Deep-Links schließen per `replace`.
- **Store:** Ein `Copy`-Struct aus `RwSignal`s im Context. SSE-Nachrichten werden als `queued_api::ServerEvent` deserialisiert. Log-Zeilen gehen direkt in die Event-Liste eines offenen Detail-Fensters („Sink“), nicht über einen Effekt. Leptos-Effekte laufen asynchron und hätten bei schnellen Folgen Zeilen verloren. Duplikate aus Fetch/SSE-Überlappung werden per Event-ID verworfen (`store::push_event`, getestet). React hatte diese Doppelungen.
- **Fortschritt:** `logic::progress` leitet aus den Event-Arten die Schritte Vorbereiten → Umsetzen → Prüfen → Abschließen ab. Für Aufträge, die beim Laden schon laufen, holt der Store einmal die Events.

## 3. Geteilte Typen (`queued-api`)

Verschoben bzw. neu typisiert, das JSON bleibt identisch. Geprüft wird das durch die unveränderten Backend-Integrationstests, die System-Tests und Rundreise-Tests in der Crate:

- Verschoben aus `models.rs`/`budget.rs`/`agent`: `User`, `Task`, `TaskEvent`, `Usage`, `ReviewResult`, `FailureClass`, `Settings`, `Budget`, `Window`, `ProviderStatus`, `ProviderCapabilities`. Das Backend re-exportiert sie (`pub use`), der restliche Backend-Code ist unverändert.
- Bisher `json!`, jetzt Structs: `Meta`, `OidcProvider`, `ProjectSummary`, `ProjectView`, `CreatedProject`, `Member`, `Notification`, `NotificationPush`, `CredentialInfo`, `FileView`, `ImportResult`, `Commit`, `PushResult`, `PushKey`, `TaskDetail`, `DiffView`, `ApproveAllResult`, `ServerEvent` (SSE).
- Request-Bodies: `LoginRequest`, `RegisterRequest`, `CreateTask`, `UpdateTask`, `TaskLimitFields`, `NewProject`, `UpdateProject`, `AddMember`, `PutCredential`, `Answer`, `MoveTask` (+`MOVE_*`), `SettingsUpdate`, `double_option` (PATCH: Feld fehlt vs. `null`).

## 4. Abweichungen vom Plan bzw. vom alten Verhalten (begründet)

1. **`Settings` getrennt in `Settings` (Antwort) und `SettingsUpdate` (PUT).** Das alte Struct hatte `skip_deserializing` auf `user_id`/`last_onempty_at`. Als geteilter Typ hätte das Frontend `last_onempty_at` nie gesehen. Die PUT-Form bleibt gleich, unbekannte Felder werden wie bisher ignoriert.
2. **Projekt-Zeile vs. DTO:** Das Backend behält die DB-Zeile `models::Project` (mit `git_token_enc`). Die API liefert `ProjectView` über `Project::view(role)`. Der verschlüsselte Token kann so strukturell nicht im Frontend-Typ landen. `PATCH /projects/{id}` und `POST /projects` liefern jetzt zusätzlich `role`/`has_git_token`. Das ist additiv, kein Test und kein Client bricht.
3. **SSE-`task_event.event` enthält jetzt `task_id`** (additiv), damit derselbe `TaskEvent`-Typ für Fetch und Live gilt.
4. **Backend: Gzip nur für statische Dateien** (`tower-http` Feature `compression-gzip`, Layer nur auf dem SPA-Fallback). Das WASM ist 1,5 MB groß, gzip 0,6 MB. API und SSE bleiben unkomprimiert (System-Test prüft `content-encoding` und `text/event-stream`). Das berührt keine Architekturgrenze.
5. **Backend: `/api/*` ohne Route liefert JSON-404 statt `index.html`** (vorher: SPA-Fallback auch unter `/api`, also 200 mit HTML). Gefunden durch den neuen System-Test. Ein Tippfehler im Client würde sonst als „Unerwartete Antwort“ statt 404 enden.
6. **Fehlertexte des Backends werden im Client übersetzt** (`logic::errors`, z. B. „invalid credentials“ → „E-Mail oder Passwort ist falsch.“). Das Backend bleibt unverändert, unbekannte Texte werden durchgereicht. Der E2E-Login-Test prüft jetzt den deutschen Text.
7. **Verwerfen eines wartenden Auftrags ohne Dialog, dafür mit „Wiederherstellen“.** Das nutzt `retry`, der Auftrag landet am **Ende** der Warteschlange (das Backend kennt keine Positionswiederherstellung). Das ist so benannt und im Test geprüft. Laufend abbrechen, Rückgängig (revert), Projekt löschen, Zugang entfernen und Mitglied entfernen fragen per Dialog nach.
8. **Kürzel `g o` für Projekte** statt `g p` (Plan), weil `g p` Postfach ist. Der Plan nennt das bereits so.
9. **Hell/Dunkel über `light-dark()`** (Chromium ≥ 123, Safari ≥ 17.5). Ältere iOS-Versionen bekommen keine Farben. Das ist im Plan als Risiko akzeptiert.
10. **Review-Konflikt beim Rückgängig-Test:** Der Mock-Reviewer hängt an dieselbe Datei an, `revert` liefert dann 409 (bekanntes Backend-Verhalten, Integrationstest `revert_uses_git_and_is_atomic_on_conflict`). Der E2E-Test schaltet das Review für sein Projekt ab, statt den Fall zu verschweigen. Die 409-Meldung erscheint im UI verständlich als Toast.
11. **Icons:** Lucide-Pfade (ISC-Lizenz, Hinweis im Kopf von `ui/icons.rs`) als Inline-SVG über `inner_html` eines `<span aria-hidden>`. Kein CDN, keine Webfonts.
12. **App-Icon neu** (Verlaufs-Logo im Design-System). Die PNGs wurden aus `icon.svg` per Chromium gerendert (vollflächig für `maskable`).

## 5. WP01-Umfang im neuen UI

Fehlerklassen-Tag (`data-testid=failure-tag`), Backoff-Hinweis „wartet bis … (Wiederholung n)“, Block „Abbruchgrund“ (`data-testid=abort-reason`, mit Erklärung je Klasse), Task-Limits im Detail, Projekt-Limits mit Hinweis auf nicht erzwungene Limits je `provider_capabilities`, Pause-Tag + „Pause aufheben“, CLI-Version je Anbieter. **WP01-Fund N6 behoben:** Composer („Erweitert“) und „Bearbeiten“ setzen Task-Limits. Ungültige Eingaben (gebrochene Tokens/Turns, < 1 s, Kosten ≤ 0, Bereichsgrenzen des Backends) werden vor dem Request mit deutscher Meldung am Feld abgelehnt (`logic::limits`, `aria-invalid`).

## 6. Funktionsabgleich (Plan 1.1 → neues UI)

| React | Leptos |
|---|---|
| Login/Registrierung/OIDC/`login_error` | `pages/login.rs`, mit echten Labels |
| Tabs + Badges + Puls + Limit-Chips + Toasts + Drawer | `pages/shell.rs`: Seitenleiste (Desktop/Querformat) bzw. Tab-Leiste unten (hochkant), Chips verlinken zu den Budgets, Toasts mit „Öffnen“/Aktion/Schließen |
| Queue-Sektionen, Tags, Meta, Aktionen, Reorder, Freigaben, Inline-Antwort, Verlauf-Filter | `pages/queue.rs`. „Läuft“ als Fortschrittskarte, Freigaben und Rückfragen unter „Wartet auf dich“ |
| Composer inkl. Optionen | `pages/composer.rs` + Limits |
| Task-Drawer | `pages/drawer.rs`: Fortschritt, Rückfragen mit Antwortfeld, Abbruchgrund, Limits, Fehler, Ergebnis, Review, Übergabe, Auftrag, Kontext, Zusammenhang, Verbrauch, Diff mit Datei-Übersicht, Protokoll (aufklappbar), Bearbeiten inkl. Limits, Rückgängig mit Dialog |
| Inbox | `pages/inbox.rs` |
| Projekte, Detail mit Dateien/Git/Aufträge/Einstellungen/Mitglieder, ZIP | `pages/projects.rs`, `pages/project.rs` (Tabs mit Pfeiltasten, Tab im URL) |
| Einstellungen inkl. Budgets/Zugänge/Push/Konto | `pages/settings.rs` + Darstellung (Theme) + Kürzel-Hilfe |
| Vitest (11 Tests) | `cargo test` in `frontend` (35 Tests) |

Neu: Onboarding (`pages/onboarding.rs`), Tastaturkürzel (`logic/shortcuts.rs`, Hilfedialog `?`), Live-Region für Statuswechsel, Theme-Wahl, deutsche Fehlermeldungen, Bestätigungsdialoge mit Fokusfalle und Fokus-Rückgabe (`ui/dialog.rs`).

## 7. Build/Deploy

- `e2e/scripts/stack.sh`: `trunk build --release` statt `npm run build`.
- `Dockerfile`: Stufe `web` auf `rust:1-bookworm` mit `wasm32-unknown-unknown`, `cargo install --locked trunk@0.22.1 wasm-bindgen-cli@0.2.130`, kopiert `backend/` mit (geteilte Crate). Ein Docker-Build lief hier **nicht** (kein Daemon gestartet), siehe Testbericht.
- `deploy/install.sh`: Node/npm entfernt, Rust-Target + Trunk + wasm-bindgen-cli als Service-User, `frontend/target` und `dist` vom Kopieren ausgenommen, `/opt/queued/web` wird vor dem Kopieren geleert (alte Hash-Dateien). `bash -n` geprüft, nicht real ausgeführt (bräuchte Root auf einem frischen Host).
- CI: Frontend-Job mit fmt, clippy (wasm32 und nativ), `cargo test`, `trunk build --release`. Der E2E-Job installiert Trunk/wasm-bindgen statt npm im Frontend.
- README: Komponenten, Tests, Entwicklung (`trunk serve` mit `/api`-Proxy auf :8080).
- `.gitignore`/`.dockerignore`: `frontend/target` statt `frontend/node_modules`.
