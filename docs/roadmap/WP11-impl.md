# WP11 – Enterprise & Betrieb: Umsetzung

Grundlage: `WP11-plan.md`. Branch `mvp`, nicht committet (Sicherung nur auf `wip-snapshot`). Testbericht: `WP11-test.md`, Urteil: `WP11-review.md`.

## 1. Was gebaut wurde

| Baustein | Dateien | Kurz |
|---|---|---|
| Migration | `backend/crates/app/migrations/0014_enterprise.sql` | Konto-Sperre, Session-Metadaten (`id`, `last_seen_at`, `user_agent`, `auth_method`), `mfa_totp`, `webauthn_credentials`, `recovery_codes`, `auth_flows`, `auth_throttle`, `instance_settings`, `invitations`, `user_quotas`, `audit_log` mit Hash-Kette, Triggern und `audit_log_verify()`, `runners` (feste Zeile `builtin`), `projects.runner_id`, `tasks.runner_id` |
| Geteilte Typen | `backend/crates/api/src/security.rs` | Wire-Typen und deutsche Texte für Login-Schritt, Sicherheit, Sessions, Admin, Audit, Runner; gemeinsame Validierung (Passwort, Recovery-Code-Format, Kontingent, Einladung, Runner-Name/-Adresse/-Kapazität); `Meta.registration_mode`, `User.mfa_setup_required` |
| MFA-Kern | `backend/crates/app/src/mfa.rs` | TOTP (HOTP/RFC 4226 + RFC 6238, Fenster ±1, Replay über `last_step` atomar), Base32, `otpauth://`-URI, Recovery-Codes (HMAC-SHA-256 mit Pepper aus `MASTER_KEY`, Einmalnutzung atomar), WebAuthn-RP aus `PUBLIC_URL` (webauthn-rs 0.6.2), Flows (gehashter Token, Ablauf, Versuche, Einmalnutzung), Drosselung |
| Login/Sessions | `backend/crates/app/src/auth.rs` | Extraktor prüft Sperre und MFA-Pflicht (nur Einrichtungsrouten erlaubt), Session-Extension für SSE; Login → Session **oder** Zwischenschritt (`qmfa`, Pfad `/api/auth`, 5 min); zweiter Faktor (TOTP, Recovery mit Benachrichtigung, Passkey); neue Session immer mit neuem Token (alte Cookie-Session wird gelöscht); OIDC endet im selben Zwischenschritt (`/?mfa=1`); Registrierungsmodi und Einladungen (auch über OIDC: `invite_hash` im `oauth_states`); Zeitausgleich bei unbekannten Konten (Dummy-Argon2) |
| Konto-API | `backend/crates/app/src/api/account.rs` | Sicherheitsübersicht, Passwortwechsel (Step-up, alle Sessions neu), TOTP einrichten/bestätigen/entfernen, Recovery neu, Passkey hinzufügen/entfernen, Sessions (Liste, einzeln, alle anderen), eigenes Kontingent; letzter Faktor nicht entfernbar unter MFA-Pflicht |
| Admin-API | `backend/crates/app/src/api/admin.rs` | Personen (Suche, Sperre/Entsperre, Rolle, Kontingent) mit „letzter Admin“ unter Zeilensperre, Einladungen, Regeln, Audit (Filter, Cursor-Paging, JSONL-Export, Kettenprüfung), Runner (anlegen, ändern, Token erneuern, widerrufen, löschen), Projekte umziehen; jede Änderung im selben Transaktionsschritt auditiert |
| Audit | `backend/crates/app/src/audit.rs` + Aufrufstellen in `api/{credentials,providers,environment,projects,tokens,gate,tasks,knowledge,steer}.rs`, `runner_gate.rs`, `runner_publish.rs`, `fleet.rs`, `backup.rs` | `record` (in Transaktion), `log`/`log_as`/`system` (nach der Aktion, Fehler → Log + Metrik `queued_audit_write_errors_total`), `prune` (≥ 365 Tage, vom ältesten Ende), `verify` |
| Instanz/Kontingent | `backend/crates/app/src/instance.rs`, `worker.rs`, `api/projects.rs` | Registrierungsmodus (Env als Vorgabe), MFA-Pflicht, Kosten 30 Tage aus `usage_log` (WP13), Scheduler startet nichts für gesperrte Personen/erschöpfte Kosten, Parallelität gedeckelt, Projektanzahl begrenzt |
| Runner-Flotte (App) | `backend/crates/app/src/fleet.rs`, `runner_client.rs`, `state.rs`, alle Aufrufer von `st.runner` | `AppState::runner(project)` statt eines globalen Clients (~50 Aufrufstellen, Cache Projekt→Runner); Monitor (`POST /v1/heartbeat` je Runner, Lease, offline/online + Audit); Client: Lease pro Lauf, Liste offener Läufe, „verloren“-Signal beendet Streams und alle Anfragen sofort; Zuweisung neuer Projekte, Umzug per Bundle |
| Scheduler | `backend/crates/app/src/worker.rs` | `RUNNER_USABLE` (aktiv, erreichbar, unter Kapazität) im Kandidaten- und im Claim-Statement, Zeilensperre Runner nach Projekt, `tasks.runner_id`; Merges nur auf erreichbaren, nicht widerrufenen Runnern |
| Runner | `backend/crates/runner/src/lib.rs`, `git.rs`, `wp11_tests.rs`; Protokoll `common/src/lib.rs` | `POST /v1/heartbeat` (Leases verlängern, aktive Läufe melden), Lease-Ablauf ⇒ Abbruch des Laufs, **ein Prozess pro Workspace** (409), `GET /v1/workspaces`, `GET /v1/workspaces/{id}/bundle` (Streaming, Temp-Datei sofort gelöscht), `POST /v1/workspaces/{id}/restore` (nie über bestehende, HEAD-Ref geprüft, Aufräumen bei Fehler) |
| Backup/Restore | `backend/crates/app/src/backup.rs`, `main.rs` (`queued backup`, `queued restore`, `queued backup-key`) | Streaming-Verschlüsselung (AES-256-GCM, 64 KiB, STREAM-Nonce, Pfad als AAD), Manifest + HMAC, `pg_dump`/`pg_restore` mit `PG*`-Variablen (Passwort nie in argv), Rechte 0700/0600, `.partial` bis vollständig, Aufbewahrung, Restore: erst alles prüfen, nur leere DB, `--single-transaction`, Bundles auf die Runner, `--target-runner` |
| Härtung | `config.rs`, `push.rs` | `master.key` und `vapid_private.key` werden mit 0600 angelegt (vorher Umask, typischerweise 0644) |
| Frontend | `frontend/src/pages/{security,admin,login,settings,shell}.rs`, `src/webauthn.rs`, `src/logic/security.rs`, `src/store.rs`, `src/lib.rs`, `style/app.css` | Zweiter Anmeldeschritt (Code, Passkey, Wiederherstellungscode), Einladungs-Registrierung, Einrichtungsseite bei MFA-Pflicht, Karte „Sicherheit“ (QR-Code als SVG aus `qrcode` 0.14.1, Passkeys, Codes einmal anzeigen/kopieren/speichern, Passwort, Geräte), „Dein Kontingent“, Verwaltung mit fünf Reitern; abgemeldete Sessions landen nach Ende des Live-Streams auf der Anmeldeseite |
| Betrieb | `deploy/queued-backup.{service,timer}`, `install.sh`, `queued.env.example`, `runner.env.example`, `Dockerfile`, `.github/workflows/ci.yml`, `README.md` | Timer als Service-User mit Härtung, Backup-Schlüssel bei Installation, `libssl-dev`/`postgresql-client`, CI installiert `pg_dump`, README-Abschnitt mit Runbook |
| Tests | `backend/crates/app/tests/{enterprise,fleet}.rs`, Unit-Tests in `mfa.rs`, `backup.rs`, `api/security.rs`, `runner/wp11_tests.rs`, `frontend/src/logic/security.rs`; `e2e/global-setup.ts`, `e2e/tests/{system/enterprise,ui/security,ui/security-mobile,ui/admin}.spec.ts` | siehe `WP11-test.md` |

## 2. Entscheidungen (mit Begründung)

1. **Push-Modell für die Flotte, keine Selbstregistrierung.** Der Runner bekommt kein Credential für die App; damit hat ein kompromittierter Runner nichts, womit er an Daten anderer Projekte käme. Registrieren heißt: Admin legt Adresse an, App erzeugt das Token. Preis: Die App muss jeden Runner erreichen (Firewall-Regel nur für den App-Host).
2. **Lease auf beiden Seiten.** App: offline nach `RUNNER_LEASE_SECS` ohne Heartbeat ⇒ Streams enden, Aufträge `infra`. Runner: Lauf ohne verlängerte Lease ⇒ SIGINT/Grace/SIGKILL. Zusätzlich lehnt der Runner einen zweiten Prozess im selben Workspace ab. Ein Doppel-Lauf bräuchte also gleichzeitig falsche Uhren, einen Runner, der seine eigene Lease ignoriert, und ein Versagen der Workspace-Sperre.
3. **Lease opt-in pro Lauf** (`ExecRequest.lease_secs`), damit Runner-Unit-Tests und ältere Aufrufer unverändert bleiben; die App setzt sie immer.
4. **`AppState::runner()` gibt nie einen Fehler zurück**, sondern einen Client, der jeden Aufruf mit dem Grund ablehnt. Alle Runner-Methoden geben ohnehin `Result` zurück; so blieben die ~50 Aufrufstellen mechanisch und die Fehlerklassifikation (`infra`) unverändert.
5. **TOTP selbst implementiert** (40 Zeilen auf `hmac` + `sha1`, beide schon im Baum), statt `totp-rs`: weniger Abhängigkeiten, alle RFC-4226/6238-Testvektoren als Unit-Test.
6. **webauthn-rs 0.6.2** (kanidm, 31.10.2026, ~9,7 Mio. Downloads, MPL-2.0). Zustände serverseitig in `auth_flows` (Feature `danger-allow-state-serialisation`; der Zustand verlässt den Server nie). Benötigt OpenSSL 3 (im Build-Image vorhanden, Laufzeit `libssl3`).
7. **Passkeys als zweiter Faktor**, nicht passwortlos: erfüllt „MFA: TOTP + WebAuthn/Passkeys“ und hält den Login-Pfad einheitlich (auch nach OIDC).
8. **MFA auch nach OIDC** (Plan §2.7) – über dieselbe Funktion `after_first_factor` wie der Passwort-Login.
9. **Drosselung pro Konto/Faktor, nicht pro IP** (hinter Tunnel nicht vertrauenswürdig). Fehlgeschlagene Anmeldungen **unbekannter** E-Mail-Adressen werden nicht auditiert, nur gezählt (Metrik/Log) – sonst könnte jeder anonym das unlöschbare Audit-Log füllen (Review B1).
10. **Recovery-Codes per HMAC mit Pepper** statt Argon2 je Code: 10 Codes × Argon2 wären bei jeder Prüfung ~0,5 s; der Pepper aus `MASTER_KEY` macht einen reinen DB-Dump für Offline-Raten wertlos.
11. **Audit-Retention per Trigger erzwungen**: jüngere Einträge als 365 Tage sind auch für den Tabelleneigentümer per SQL nicht löschbar (nur durch Abschalten der Trigger – dafür die dokumentierte Rollentrennung, real geprüft, siehe Test-Bericht).
12. **Backup als Kommando, nicht im Server.** `pg_dump` ist ein Fremdprozess; der Server bleibt ohne Prozess-/Dateiarbeit (Architekturregel). Git-Arbeit bleibt im Runner (Bundle-Endpunkte).
13. **Verschlüsselung Pflicht**, Klartext nur mit `--unencrypted`; `MASTER_KEY` nie im Backup (Report P0.11).
14. **E2E-Admin per `globalSetup`**: Der erste Account einer Instanz wird Admin; parallele Tests würden sonst zufällig Admin. Das E2E-Konto `admin@e2e.test` wird vor allen Tests angelegt; E2E-Tests ändern keine instanzweiten Regeln (parallele Tests).
15. **`PUBLIC_URL` im E2E-Stack `http://localhost:…`**, weil WebAuthn keine IP-Adressen als RP-ID erlaubt. Browser-Tests laufen weiter gegen `127.0.0.1`; nur der Passkey-Test nutzt `localhost`.

## 3. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| `DELETE /account/mfa/totp`, `DELETE /account/webauthn/{id}` | `POST …/disable`, `POST …/{id}/remove` | Bestätigung (Passwort) im Body; DELETE mit Body ist unüblich und wird von Proxys teils verworfen |
| `GET /admin/audit/export` als CSV/JSONL | nur JSONL (mit Hashes) | Export ist optional; JSONL erlaubt die Kettenprüfung außerhalb |
| Step-up auch für API-Token-Erstellung | nicht umgesetzt | WP07-UI/E2E unverändert gelassen; Token-Erstellung braucht eine (bei MFA-Nutzern MFA-geprüfte) Session. Nicht blockierender Fund N2 |
| Umzug aus einem widerrufenen Runner | nicht direkt, nur über `queued restore --target-runner` | Der widerrufene Runner ist per Definition nicht vertrauenswürdig; seine Daten kommen aus dem Backup |
| Laufende Aufträge bei Sperre abbrechen | nicht umgesetzt (laufen zu Ende, nichts Neues startet) | Nicht verlangt; ein Abbruch mitten im Lauf hinterließe halbe Arbeit. Fund N5 |
| `readyz` über alle Runner | weiterhin nur der Standard-Runner | Abwärtskompatibel (Load-Balancer-Semantik aus WP10); Flotte in der Verwaltung sichtbar. Fund N7 |
| `postgresql-client` im App-Image | nicht enthalten | bookworm hat nur Version 15, die Datenbank ist 16; Dokumentiert |

## 4. Annahmen (nicht live verifiziert)

- WebAuthn mit echten Geräten (iPhone/Safari, Android, YubiKey) – geprüft nur mit `webauthn-authenticator-rs` (SoftPasskey) und Chromiums virtuellem CTAP2-Authenticator.
- Authenticator-Apps lesen den QR-Code (Format nach Key-Uri-Spezifikation; QR mit Fehlerkorrektur M) – geprüft ist die Erzeugung und das Rendern, nicht das Scannen mit einer echten App.
- Die systemd-Units (`queued-backup.*`) sind nicht unter echtem systemd gestartet worden (Container ohne systemd); `systemd-analyze verify` war hier nicht verfügbar.
- Mehrere Runner auf getrennten Hosts: getestet mit mehreren Runner-Prozessen/Instanzen auf einem Host (Loopback), inkl. eingefrorener Verbindung.
