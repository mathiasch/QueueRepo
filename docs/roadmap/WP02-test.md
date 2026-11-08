# WP02 – Testbericht

Umgebung: Linux-Container, rustc/cargo 1.97.0, Postgres 16 lokal, Trunk 0.22.1, Node 22, Playwright mit Chromium (`/opt/pw-browsers`), axe-core. Runner-Engine in allen automatisierten Tests: `none`. **Keine externen Calls:** Abgelehnte Ziele werden vor jeder DNS-Abfrage abgelehnt (Testnamen enden auf `.invalid`, und das Ergebnis `not_allowed` statt `unresolvable` belegt das). Erlaubte Ziele sind lokale Fake-Server auf `127.0.0.1` (HTTP; HTTPS mit eigener Test-CA), freigegeben über die Betreiberliste `RUNNER_EGRESS_PRIVATE_HOSTS` bzw. deren Test-Pendant.

## 1. Abnahmeläufe (finaler Stand, zweimal hintereinander)

Skript: Backend fmt → clippy → `cargo test --workspace` → Frontend fmt → clippy wasm32 → clippy nativ → `cargo test` → `trunk build --release` → `npx playwright test` (alle Projekte; `stack.sh` baut Backend-Binaries und Frontend selbst).

| Schritt | Lauf A | Lauf B |
|---|---|---|
| Backend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Backend `cargo clippy --workspace --all-targets -- -D warnings` | rc=0, 0 Warnungen | rc=0, 0 Warnungen |
| Backend `cargo test --workspace` | **296 passed, 0 failed** (80 s) | **296 passed, 0 failed** (72 s) |
| Frontend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Frontend `cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo clippy --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo test` | **42 passed** | **42 passed** |
| Frontend `trunk build --release` | rc=0 | rc=0 |
| Playwright (system, desktop, iphone, iphone-landscape; inkl. axe) | **52 passed** (2,4 min) | **52 passed** (2,3 min) |

Vor WP02 (WP13-Testbericht): Backend 247, Frontend 39, Playwright 46 → **+49 / +3 / +6**. Bestehende Tests wurden nicht abgeschwächt. Angepasst wurden nur `progress.rs` (der Neustart eines Laufs beginnt jetzt bei „Vorbereiten“, weil das Setup dazugehört), eine `ExecRequest`-Konstruktion in `edge_cases.rs` und die Exec-Tests des Runners (neue Pflichtfelder).

Backend 296 = app lib 111 (+5) · api 19 · cli_contract 26 · edge_cases 11 · **environments 8 (neu)** · failures 19 · parser_robustness 6 · providers 16 · queued-api 15 (+5) · queued-common 12 (+6) · runner 53 (+25).
Playwright 52 = system 14 (+2) · desktop 24 (+2) · iphone 7 (+1) · iphone-landscape 7 (+1). Dazu prüft die bestehende a11y-Tour jetzt auch den Reiter „Umgebung“ (hell/dunkel, Desktop und iPhone).

## 2. Neue Tests nach Ebene

**Unit – `queued-common::net` (6):** Hostregeln (Normalisierung, `*.`, Port, IP/IPv6, Apex nicht in `*.`, Suffix-Tricks wie `registry.npmjs.org.evil.com`), Ablehnung von Müll (15 Fälle), gesperrte Bereiche (immer / nur ohne LAN-Freigabe / öffentlich), CIDR inkl. IPv4-mapped, Default-Policy = kein Netz.

**Unit – Runner (25):**
- Proxy (`egress::tests`, 14): Entscheidungslogik je Modus; Adressfilter in Schichten; `/proc`-Parser; Nur-Lesen-Regeln; CONNECT-Tunnel zum erlaubten Upstream; **Ablehnung vor DNS**; erlaubtes `localhost` und Metadaten-IP trotzdem gesperrt; `open` nur öffentlich; `off` nur Modell-API, Müll ⇒ 400; absolute-form HTTP wird auf Origin-Form umgeschrieben, `Proxy-Authorization` entfernt; Nur-Lesen HTTP: POST ⇒ 403, POST per Pipelining auf derselben Verbindung erreicht den Upstream nie; **Nur-Lesen HTTPS**: TLS-Upstream mit eigener CA, Client vertraut nur der Lauf-CA, GET geht durch, POST ⇒ 403 im Tunnel, Upstream sieht ihn nie; nicht vertrauenswürdiges Upstream-Zertifikat ⇒ 502; Unix-Socket (Rechte 0600) mit der **echten Node-Brücke**; Wrapper/Umgebungsvariablen.
- Exec (4): Container-Argumente (`--network none` immer, ro-Mount, Cache-Mount, Wrapper-Position, kein Secret in argv, `cap-drop`/`no-new-privileges`); Engine `none` hinter dem Proxy mit Egress-Frames und curl `connect=403`; Redaktion und Leck-Meldung samt Bereinigung; Cache-Treffer und Invalidierung über eine Lock-Datei.
- Setup (6): Cache-Schlüssel (Material, Lock-Datei, Symlink nicht gefolgt, Pfad-Traversal), Anlegen/Verwerfen/Aufräumen, Leck-Suche ohne Symlink-Folgen, Bereinigung (untracked / tracked / `.git/config`) gegen echtes git, Redaktion, Cache-Variablen.
- HTTP (1): `/v1/info` (Token nötig), Cache löschen (idempotent, auch beim Workspace-Löschen, Traversal ⇒ 400).

**Unit – App (5):** `egress_hosts` je Zugangsart; Policies je Phase und Defaults; Cache-Material; **SSRF:** Resolver lehnt Loopback zur Verbindungszeit ab, Redirects zu Metadaten-IP/Runner/anderen Schemata/zu lange Ketten werden nicht verfolgt.

**Unit – `queued-api::env` (5) und Frontend `logic::env` (3):** Validierung und Normalisierung (gemeinsamer Code), reservierte Namen, **Anbieter-Variablen gesperrt** (B1), Secret-Namen/-Werte, Domain-Textfeld, Presets gültig; Formular → Request, Fehlersammlung, Texte für alle Modi und Statuswerte. Dazu kommen Event-Labels und Fortschrittsphasen.

**Integration – `tests/environments.rs` (8, App + Runner + Postgres):** Rollen (Owner schreibt; Editor/Viewer lesen; Fremde 404), Validierung (400/422), Secret-Werte nie in Antworten, verschlüsselt in der DB, `/api/meta` ohne Schutzstatus; Setup vor dem Agenten, Redaktion `token: ***`, Agent-Env ohne Secret, mit Variable und Proxy, Verify mit Variable und ohne Secret; Setup-Fehler ⇒ `blocked/config`, `sh -e`, kein Agent, kein Fix; Secret in Datei ⇒ blockiert und bereinigt; Cache MISS → HIT → MISS nach Lock-Datei-Änderung → MISS nach „Cache leeren“; Egress-Events aller Phasen (Setup erlaubt/abgelehnt, Agent abgelehnt, Fake-Upstream genau 1 Treffer); Projekt ohne Umgebung = Default-Deny ohne Setup; Abbruch während des Setups ⇒ `cancelled`, kein Agent.

**System (Playwright, getrennte Prozesse, echte Binaries):** `/v1/info` braucht das Token; Schutzstatus nur für Mitglieder; Setup mit Secret und Variable über die Prozessgrenze, Egress-Events, Fake-Upstream genau 1 Treffer, Secret nirgends im Task-JSON, `MOCK_ENV.txt` ohne Secret-Namen. Dabei läuft auch das Parsen von `RUNNER_EGRESS_PRIVATE_HOSTS` im echten Binary.

**E2E Desktop:** Banner „nicht durchgesetzt“, Defaults, Skript/Variable/Netz/Preset/Domain bearbeiten, ungültige Domain wird erklärt und nicht gespeichert, Secret zu kurz ⇒ Hinweis, Secret speichern, Reload zeigt alles außer dem Wert (Seiteninhalt geprüft), Auftrag mit Setup ⇒ Drawer zeigt „Setup startet“ und die Ablehnung; Bearbeiter sieht alles schreibgeschützt und darf den Cache leeren.

**E2E iPhone hoch/quer:** kein horizontales Scrollen, alle Eingaben ≥ 16 px, alle Bedienelemente innerhalb der Safe-Areas, Speichern-Button ≥ 44 pt oberhalb von Home-Indikator und Tab-Leiste, axe.

## 3. Während der Arbeit gefundene Fehler (eigener Code)

| Fehler | Gefunden durch |
|---|---|
| Host-Adressliste übersteuerte LAN-Freigaben (B3) | Code-Review vor dem ersten Lauf |
| Test-Helfer verwarf gepufferte Bytes; Fake-Upstream ignorierte `Connection: close` (Test hing) | erster Runner-Testlauf (Timeout) |
| `fmt`-Umbruch ließ eine Codeersetzung ins Leere laufen; die Sperre der Anbieter-Variablen war kurz wirkungslos | der neue Test schlug an |
| Klick-Ziel der Segment-Radios im Test falsch (Overlay-Input) | Playwright |
| Hilfe-Button im Label, `fieldset disabled`, Überschriftenstufen (B7) | Screenshot-Review |

## 4. Manuelle Gegenprobe mit echter Container-Engine (Docker 29.9.2)

Kein großer Build: `node:22-alpine` gezogen, echtes `queued-runner`-Binary mit `RUNNER_ENGINE=docker`, `RUNNER_EGRESS_PRIVATE_HOSTS=127.0.0.1`, lokalem HTTP-Upstream auf dem Host. Danach Image entfernt und Daemon beendet.

| Prüfung | Ergebnis |
|---|---|
| `GET /v1/info` | `{"engine":"docker","egress_enforced":true}` |
| Netzwerk-Interfaces im Container | nur `lo` |
| direkte Verbindung `wget -Y off http://1.1.1.1/` | Exit 1 |
| DNS `nslookup example.com` | Exit 1 |
| `GET http://127.0.0.1:18777/ok.txt` über `nc 127.0.0.1 3128` (Brücke → Socket → Proxy → Host) | `upstream-ok`, Frame `verdict: allowlist` |
| `CONNECT evil.invalid:443` | `HTTP/1.1 403 Forbidden`, Frame `not_allowed` |
| Node `http` über den Proxy | `200 upstream-ok` |
| Policy `off` | kein `/queue-egress`, keine Proxy-Variable |
| Cache-Mount + Secret | `/cache/npm` beschreibbar, Ausgabe `token=***`, Cache-Info im Exit-Frame |
| Aufräumen | Egress-Verzeichnis leer, keine Container übrig |
| `RUNNER_DATA_DIR` mit 85 Bytes | Runner verweigert den Start mit klarer Meldung |

**Nicht geprüft:** rootless Podman, SELinux-Relabeling (`:Z`), das echte Agent-Image (nicht gebaut), echte Agent-CLIs hinter dem Proxy, „Nur lesen“ mit echten Registries.

## 5. Aufräumen

`backend/target/debug/incremental` und veraltete Test-Binaries wurden gelöscht (Platte zeitweise bei 6,8 GB frei), nach den Läufen auch `frontend/target`. Das Docker-Image ist entfernt.
