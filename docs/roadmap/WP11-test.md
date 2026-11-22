# WP11 – Enterprise & Betrieb: Testbericht

Stand: 2026-11-22, Branch `mvp` (WIP-Commit `10161aa` + Arbeitsstand). Alle Zahlen aus Läufen in dieser Sitzung. Keine echten externen Dienste: OpenID-Provider, Authenticator, Runner-Hosts und Netzstörungen sind lokale Fakes bzw. Prozesse im Testlauf; keine Aufrufe an LLMs oder bezahlte APIs (**Kosten: 0 USD**). webauthn-rs/webauthn-authenticator-rs 0.6.2 und qrcode 0.14.1 wurden von crates.io geladen (Version und Wartung im Plan geprüft).

## 1. Teststrategie

| Ebene | Was | Wo |
|---|---|---|
| Unit | TOTP: RFC-6238-Anhang-B-Vektoren (8 Stellen, 6 Zeitpunkte bis 2603) und RFC-4226-Anhang-D (10 Zähler), Fenster ±1 und neuester Schritt, Base32 (RFC 4648), `otpauth`-URI-Kodierung, Recovery-Code-Format, Pepper-Abhängigkeit des Hashs, RP aus `PUBLIC_URL` | `app/src/mfa.rs` (5) |
| Unit | Backup-Stream: Roundtrip für 0/1/64 KiB±1/3 Blöcke bei drei Fütterungsgrößen, zufälliger Nonce, **erkannt**: falscher Schlüssel, falscher Pfad (Dateitausch), geändertes Byte, Abschneiden nach vollem Block, abgeschnittenes Ende, nur Header, vertauschte Blöcke, angehängte Blöcke; Schlüsselformate, `PG*`-Variablen (prozentkodierte Zugangsdaten), Schlüsseldatei 0600 erzwungen und nie überschrieben, Aufbewahrung | `app/src/backup.rs` (5) |
| Unit | Wire-Typen: Recovery-/TOTP-Normalisierung, Validierungen, Geräte-/Anmeldeart-Texte, MFA-Richtlinie | `api/src/security.rs` (3) |
| Unit (Runner, echter HTTP-Server) | Bundle → Restore: alle Refs (inkl. `refs/queue/held/*`), symbolischer HEAD, Arbeitsbaum, `fsck`; nie über bestehende Workspaces; ungültiger HEAD-Header 400; kaputtes Bundle 422 ohne Reste; Temp-Dateien weg; Liste; ohne Token 401. Lease: Lauf bleibt bei Heartbeats über die Lease hinaus, endet ohne sie (Exit-Frame), zweiter Prozess im selben Workspace 409, anderer Workspace erlaubt | `runner/src/wp11_tests.rs` (2) |
| Unit (Frontend) | QR-Pfad (Finder-Muster, Ruhezone, zu lang ⇒ `None`), Base64url (RFC-4648-Vektoren, Rückweg über den bestehenden Decoder), Null-Bereinigung der WebAuthn-Optionen, Zahlen-/Geld-Eingaben, Audit-Details | `frontend/src/logic/security.rs` (4) |
| Integration (App + Runner in-process + Postgres) | siehe §3 | `app/tests/enterprise.rs` (12), `app/tests/fleet.rs` (3) |
| System (getrennte Prozesse, echte Binaries) | MFA über die API des Stacks (Replay, keine Session vor dem 2. Faktor, Token 403 auf Konto/Admin), Sperre ⇒ Token und Session sofort 401; `queued backup-key`/`backup`/`restore` mit eigenen Datenbanken und zwei `queued-runner`-Prozessen: ohne Schlüssel verweigert, 0700/0600, Format `QBK1`, Refs gleich, Migrationsstand gleich, zweiter Restore in die nun volle DB verweigert | `e2e/tests/system/enterprise.spec.ts` (2) |
| E2E Desktop | App mit QR-Code einrichten (falscher Code verständlich abgewiesen), Codes einmal angezeigt, zweiter Browser mit Wiederherstellungscode, Anmeldung mit nächstem TOTP-Code, Geräteliste, „alle anderen abmelden“ ⇒ zweiter Browser landet auf „abgemeldet“; Passkey mit Chromiums virtuellem CTAP2-Authenticator hinzufügen und damit anmelden; Verwaltung: sperren ⇒ Browser der Person abgemeldet, Login „gesperrt“, entsperren, Einladung (gebunden, Registrierung über den Link, „Angenommen“), Regeln, Protokoll (Filter, Unverändertheit), Runner (Standard „Bereit“, neuer Runner mit Token einmal, „Nicht erreichbar“, widerrufen); Nicht-Admins sehen keine Verwaltung (UI + 403); axe hell/dunkel | `e2e/tests/ui/security.spec.ts` (2), `admin.spec.ts` (2) |
| E2E iPhone hoch + quer | Sicherheitskarte und Einrichtungsdialog: Safe-Area, alle Knöpfe ≥ 44 pt, Eingaben ≥ 16 px, QR ≥ 160 px, kein horizontales Scrollen, axe; Verwaltung: alle fünf Reiter ebenso | `e2e/tests/ui/security-mobile.spec.ts` (2 × 2) |

E2E-Infrastruktur: `e2e/global-setup.ts` legt vor allen Tests das Admin-Konto an (der erste Account einer Instanz wird Admin; ohne das würde ein zufälliger paralleler Test Admin). Der Stack läuft mit `PUBLIC_URL=http://localhost:…`, weil WebAuthn keine IP-Adresse als RP-ID akzeptiert; alle anderen Browser-Tests bleiben auf `127.0.0.1`.

## 2. Gegenproben (Tests können scheitern)

Jede Mutation wurde mit einem Skript in den Quelltext eingebaut, der zuständige Test gebaut und ausgeführt, danach der Originalzustand zurückgeschrieben (`git status` geprüft).

| # | Mutation | Test | Ergebnis |
|---|---|---|---|
| M1 | TOTP-Replay-Schutz entfernt (`step <= last` und `WHERE last_step < $2`) | `totp_login_replay_and_lockout` | **rot** („replayed code accepted“) |
| M2 | Recovery-Code ohne `used_at IS NULL` | `recovery_codes_are_single_use` | **rot** |
| M3 | Audit-Trigger für UPDATE/DELETE aus der Migration entfernt | `audit_log_is_append_only_chained_and_secret_free` | **rot** (UPDATE gelingt) |
| M4 | Sperre widerruft Tokens nicht | `lock_revokes_sessions_and_tokens` | **rot** (Token nach Entsperren wieder gültig) |
| M5 | Prüfung „letzter Admin“ wirkungslos | `last_admin_cannot_lock_out` | **rot** |
| M6 | Session-Fixation: vorhandenes Cookie wird als Session-Token übernommen | `sessions_are_listed_revoked_and_never_fixed` | **rot** |
| M7 | Token ignoriert MFA-Pflicht | `mfa_cannot_be_bypassed` | **rot** |
| M8 | Runner akzeptiert jedes Token | `fleet_registration_isolation_drain_capacity_and_move` | **rot** (Token von Runner A öffnet Runner B) |
| M9 | Scheduler ignoriert Zustand/Kapazität der Runner | dito | **rot** |
| M10 | Kein Abbruch der Streams bei verlorenem Runner | `runner_failure_requeues_without_double_run` | **rot** (Auftrag hängt in „läuft“) |
| M11 | Workspace-Bundles unverschlüsselt | `backup_restore_roundtrip` | **rot** (Klartext-Marker gefunden) |
| M12 | OIDC-Callback erzeugt direkt eine Session (ohne 2. Faktor) | `oidc_login_still_needs_the_second_factor` | **rot** („OIDC must not skip the second factor“) |
| M13 | Fehlversuche unbekannter Adressen ins Audit-Log | `totp_login_replay_and_lockout` | **rot** |
| R1 | Runner: Workspace-Exklusivität deaktiviert | `wp11_tests::lease_keeps_runs_alive_only_while_heartbeats_arrive` | **rot** (200 statt 409) |
| R2 | Runner: Lease-Abbruch deaktiviert | dito | **rot** („the runner stops a run whose lease expired“) |

Echte Rot-Läufe während der Entwicklung (Produktfehler, im Review als B-Funde): `runner_failure_…` hing, weil Aufräum-Aufrufe an den toten Runner 300 s warteten (B2); E2E „Bestätigen“ nicht eindeutig (B7). Testfehler: falscher Feldname `value` statt `secret` für Zugangsdaten, `n >= 8` statt 7 Audit-Einträge, `runners`-Tabelle im Restore-Vergleich (der laufende Heartbeat ändert `last_seen_at` der Quelle; Vergleich ohne die Heartbeat-Spalten), Admin-Route mit Runner-Token ist 403 (Route vor Token-Prüfung abgewiesen) statt 401.

**Fund in der Testumgebung:** Zwei frühe Abnahmeläufe zeigten sechs rote, sonst stabile Tests mit SIGINT-Traps (`exec::tests::detects_stall…`, `sigint_then_grace…`, `edge_cases::{guard_stop…, silent_run…}`, `failures::{cancel_sends_sigint…, token_limit_stops_run_live}`), einzeln immer grün. Ursache belegt: Das Abnahmeskript war mit `( … &)` gestartet; asynchrone Kommandos einer nicht-interaktiven Shell erben `SIGINT` als *ignoriert*, und eine Shell kann ein beim Start ignoriertes Signal nicht mehr fangen (Gegenprobe: `(bash -c 'trap "echo caught" INT; kill -INT $$; echo after' &)` gibt nur `after` aus, mit zurückgesetztem `SIGINT` `caught`). Kein Produktfehler; die gewerteten Läufe starten über einen Wrapper, der `SIGINT`/`SIGQUIT` auf Standard setzt.

## 3. Integrationstests (Auszug)

| Test | Prüft |
|---|---|
| `totp_login_replay_and_lockout` | Login ⇒ `mfa_required` + Methoden, kein `qsid`, `/api/tasks` 401; Code der Einrichtung (Replay) und älterer Schritt 401, nächster Schritt 200; Flow einmalig; gleicher Code im zweiten Login 401; 5 Fehler ⇒ 429 auch mit richtigem Code; 10 Passwortfehler ⇒ 429 auch mit richtigem Passwort, ebenso für unbekannte Adressen (ohne Audit-Eintrag); bestehende Session unberührt; Audit enthält die Aktionen, aber weder Passwort, Codes noch Seed; Seed verschlüsselt |
| `recovery_codes_are_single_use` | nur Hashes, Eingabe in Kleinschrift mit Bindestrich, Benachrichtigung, zweite Nutzung 401, parallele Nutzung genau ein Erfolg, neuer Satz nur mit Passwort, alte Codes danach ungültig |
| `passkey_registration_and_login` | SoftPasskey-Registrierung, Codes beim ersten Faktor, Zeremonie einmalig, Assertion mit anderem Origin (gleiche RP-ID) 401 und keine Session, richtige 200, `last_used_at`, Entfernen nur mit Passwort, Codes verschwinden mit dem letzten Faktor |
| `sessions_are_listed_revoked_and_never_fixed` | gepflanzte gültige Session wird beim Login ersetzt und gelöscht; Liste mit genau einem „current“; Widerruf beendet den SSE-Stream ≤ 12 s; fremde Session 404; „alle anderen“; Passwortwechsel (falsches Passwort 403, zu kurz 400) meldet alle anderen ab und rotiert das eigene Token; MFA-Aktivierung meldet ab |
| `mfa_cannot_be_bypassed` | 13 Konto-/MFA-/Token-/Admin-Routen × (Token, Admin-Token) = 403; MFA-Pflicht „alle“: Session nur für Einrichtung, `me.mfa_setup_required`, Projekte 403 `mfa_setup_required`, Token 403; nach Einrichtung frei; letzter Faktor unter Pflicht nicht entfernbar |
| `oidc_login_still_needs_the_second_factor` | Fake-IdP (Token-Endpunkt mit id_token für die Nonce): Konto mit TOTP ⇒ Weiterleitung `/?mfa=1`, kein `qsid`, nach Code Session mit `oidc:Fake+totp`; ohne Faktor direkt; Einladungsmodus ohne Einladung abgewiesen, mit Einladung angelegt und verbraucht; gesperrtes Konto auch über OIDC abgewiesen |
| `lock_revokes_sessions_and_tokens` | Sperre ⇒ Sessions/Tokens 0, Session und Token 401, Token-Stream endet, Login 403, gesperrte Aufträge starten nicht; entsperren ⇒ Login ok, Token bleibt widerrufen, Auftrag läuft; auditiert |
| `last_admin_cannot_lock_out` | Selbst-Herabstufung/-Sperre 409; 5 Runden paralleles gegenseitiges Herabstufen ⇒ immer genau ein Erfolg und eine aktive Admin-Person |
| `invitations_and_registration_modes` | geschlossen/Einladung/offen inkl. `Meta`; gebundene Einladung (falsche Adresse 400), Einmalnutzung, Admin-Einladung, abgelaufen, widerrufen, Status-Liste, ungültige Dauer 400, Token nur gehasht |
| `quotas_bind_scheduler_and_projects` | Kosten 3,5 $ ≥ Grenze 3 $ ⇒ Auftrag bleibt `queued`, Projektgrenze 403, Erhöhung ⇒ Auftrag `done`; ungültiges Kontingent 400; Nicht-Admin 403 |
| `audit_log_is_append_only_chained_and_secret_free` | Marker (Anthropic-Schlüssel, Projekt-Secret, API-Token, Passwort) nirgends im Log; UPDATE/DELETE/TRUNCATE per SQL abgewiesen; Kette intakt; Filter Präfix/Person/Projekt, Cursor, ungültiger Filter 400, Export, Nicht-Admin 403; Änderung an abgeschalteten Triggern vorbei ⇒ `first_broken = 3` |
| `audit_retention_only_deletes_old_entries_in_order` | junge Einträge nicht löschbar, alte vom ältesten Ende her (`prune`) |
| `fleet_registration_isolation_drain_capacity_and_move` | Token `qr_…` einmal, verschlüsselt gespeichert, Name eindeutig, Adresse validiert; Online nach Heartbeat; Token-Isolation (Runner ⇄ Runner, Runner ⇒ App 401/403); Drain ⇒ neue Projekte auf Runner 2, wartende Aufträge warten, Projekt 2 nie auf dem Standard-Runner; `tasks.runner_id`; Kapazität 1 über zwei Projekte nie überschritten; Umzug: Refs identisch, alte Kopie weg, neue Arbeit dort; Widerruf: Token gelöscht, Aufträge warten, Löschen 409, Standard-Runner nicht widerrufbar, neue Projekte auf den Standard-Runner; auditiert |
| `runner_failure_requeues_without_double_run` | Runner hinter einem einfrierbaren TCP-Weiterleiter (keine Bytes, kein RST): nach < 15 s `queued`/`infra`, Runner „offline“, Runner beendet den verwaisten Lauf selbst, Auftrag wartet (nirgends sonst gestartet); nach dem Auftauen genau ein Agentenlauf; Audit offline/online |
| `backup_restore_roundtrip` | verschlüsseltes Backup (0700/0600, kein Klartext-Marker, kein `MASTER_KEY`); neue leere DB + leerer Runner: falscher Schlüssel und manipuliertes Bundle abgewiesen, DB danach weiter leer; Restore: **alle 50+ Tabellen md5-gleich** (Audit bis zum Stand vor dem Backup, `runners` ohne Heartbeat-Spalten), Kette intakt, Refs/HEAD/Tree/Log/Datei gleich, unversionierte Datei nicht enthalten (dokumentiert); wiederhergestellte Instanz: Migrationen aktuell, Dateien lesbar, TOTP-Seed entschlüsselbar; zweiter Restore „not empty“ |

## 4. Weitere Prüfungen

- **Audit-Rollentrennung (README-Rezept) real geprüft:** eigene Rolle als Tabelleneigentümer, App-Rolle mit `INSERT, SELECT` + Sequenz: Einfügen ergibt `seq` 1, 2 (Kette), `ALTER TABLE … DISABLE TRIGGER` ⇒ „must be owner“, `UPDATE` ⇒ „permission denied“, `audit_log_verify()` ⇒ 2 geprüft, intakt.
- **Passkey im echten Browser:** Chromium mit virtuellem CTAP2-Authenticator über `localhost` (E2E grün).

## 5. Ergebnisse (Abnahme: komplette Suite zweimal hintereinander auf dem Endstand)

Skript `accept.sh` (Scratchpad): Backend `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace --no-fail-fast`; E2E-Binaries; Frontend `cargo fmt --check`, `cargo clippy --target wasm32-unknown-unknown --all-targets -D warnings`, `cargo clippy --all-targets -D warnings`, `cargo test`, `trunk build --release`; Playwright alle Projekte (system, desktop, iphone, iphone-landscape inkl. axe-Tour). Gestartet über einen Wrapper mit Standard-`SIGINT` (siehe §2).

| Schritt | Lauf E (22.11., 04:54–05:15 UTC) | Lauf F (22.11., 05:15–05:35 UTC) |
|---|---|---|
| Backend fmt / clippy `-D warnings` | grün / grün | grün / grün |
| Backend Tests | **609 bestanden, 0 fehlgeschlagen** | **609 bestanden, 0 fehlgeschlagen** |
| Frontend fmt / clippy wasm32 / clippy nativ | grün / grün / grün | grün / grün / grün |
| Frontend Tests | **66 bestanden** | **66 bestanden** |
| `trunk build --release` | ok | ok |
| Playwright (system, desktop, iphone, iphone-landscape, inkl. axe) | **119 bestanden** (8,7 min) | **119 bestanden** (8,2 min) |

Vergleich WP10-Abnahme: Backend 579 → 609 (+30: `mfa.rs` 5, `backup.rs` 5, `api/security.rs` 3, Runner 2, `enterprise.rs` 12, `fleet.rs` 3), Frontend 62 → 66 (+4), Playwright 109 → 119 (+10: system 2, desktop 4, iphone 2, iphone-landscape 2). Kein bestehender Test wurde abgeschwächt; geändert: `edge_cases.rs` (Runner-Client pro Projekt), `api.rs` und `plan_steering.spec.ts` (Rennen behoben, Review B8).

Vorläufe, nicht gewertet: Lauf 1 und A – sechs SIGINT-Tests rot wegen des per `&` geerbten ignorierten `SIGINT` (§2); Lauf B grün (609/66/119); Lauf C – `api::more_than_two_generated_tasks_need_approval` und `plan_steering.spec.ts` rot (bestehende Test-Rennen, B8); Lauf D – axe-Fund an einem gerade einblendenden Toast (B9). Nach jedem Fix wurde neu gezählt; E und F liefen auf identischem Code direkt hintereinander.

## 6. Nicht getestet / nicht verifizierbar

- Echte Authenticator-Apps (QR scannen), Safari/iOS-Passkeys, Hardware-Schlüssel.
- Runner auf getrennten Hosts über echte Netze; rootless Podman (unverändert WP02-N4).
- systemd-Units `queued-backup.{service,timer}` unter echtem systemd; `install.sh` nicht ausgeführt (braucht Root und Netz).
- Docker-Image nicht gebaut (WP00: TLS-Interception/Platte); Laufzeitabhängigkeit `libssl3` nur am Dockerfile geprüft.
