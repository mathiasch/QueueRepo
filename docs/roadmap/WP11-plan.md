# WP11 – Enterprise & Betrieb: Plan

Grundlage: `work-packages.md` (Gemeinsame Regeln + WP11), Report `Funktionsumfang agentischer Coding Queues.md` (Tabelle „Team/Enterprise“, „Betrieb“, P0.10, P0.11, P1.11, P2.7, P2.8), Berichte WP00–WP10, WP13. Branch `mvp`, nicht committet. Stand Code: Migrationen bis 0013.

## 1. Ausgangslage (am Code geprüft)

| Bereich | Ist | Lücke |
|---|---|---|
| Login | Passwort (Argon2) + OIDC (`oidc.rs`, Verknüpfung nur über *verifizierte* E-Mail). Session = Zufallstoken im Cookie `qsid` (HttpOnly, SameSite=Lax, Secure bei https), in `sessions` nur SHA-256, 30 Tage. | **Kein Rate-Limit**, kein Lockout, keine MFA, Sessions nicht einsehbar/widerrufbar, kein Passwortwechsel. |
| API-Tokens (WP07) | `qk_…`, nur SHA-256, Scopes, Default-Deny-Routentabelle `queued_api::tokens::ROUTES`. Konto-/Token-/Admin-Routen sind für Tokens gesperrt. | Gesperrte Nutzer könnten Tokens weiter nutzen (es gibt keine Sperre). |
| Rollen | `users.is_admin` (erster Nutzer), Projektrollen owner/editor/viewer. `is_admin` nur für Preis-Overrides (WP13). | Keine Admin-Konsole, keine Einladungen, Registrierung nur per Env `ALLOW_REGISTRATION`. |
| Audit | – | fehlt komplett. |
| Backups | – | fehlt komplett (Report P0.11). `MASTER_KEY` liegt in `DATA_DIR/master.key` oder Env. |
| Runner | **ein** Runner (`RUNNER_URL`/`RUNNER_TOKEN`), App ruft ihn per HTTP (Push-Modell); Läufe hängen an einem NDJSON-Stream; der Runner bricht einen Lauf nur ab, wenn ein Frame nicht mehr zugestellt werden kann. Workspaces (Hauptrepo pro Projekt + Arbeitsbaum pro Auftrag, WP04) liegen nur auf dem Runner. | Keine Registrierung, kein Heartbeat/Lease, keine Kapazität, kein Drain. Ein hängender Host (kein RST) lässt den Stream ewig offen. Ein Runner-Token für alles: ein zweiter Runner mit demselben Token könnte die Workspaces des ersten lesen. |

Offene Funde früherer Pakete mit WP11-Bezug: WP07-N4 (ungültige Signaturen ungedrosselt → bleibt Reverse-Proxy-Sache, aber Login-Drosselung gehört hierher), WP07-N6 (Token-Widerruf wirkt auf offene SSE-Streams erst nach ≤10 min → bei Sperre/Session-Widerruf **muss** der Stream sofort enden: wird hier behoben), WP02-N8/WP10-N11 (bleiben unverändert), Report P0.10 „Login-Rate-Limit … MFA“.

## 2. Architektur-Entscheidungen

1. **App bleibt ohne Host-Rechte.** Backup/Restore ist ein *Betriebs-Kommando* (`queued backup` / `queued restore`, Subcommand des App-Binaries), das ein Operator bzw. ein systemd-Timer als Service-User startet – nicht der Daemon. Git-Arbeit (Bundles erzeugen/einspielen) macht ausschließlich der Runner über neue `/v1`-Endpunkte; das Kommando holt/schickt die Bytes über HTTP. `pg_dump`/`pg_restore` sind die einzigen Fremdprozesse.
2. **Runner-Flotte im Push-Modell** (App ruft Runner), nicht Pull/Self-Registration: Der Runner bekommt **kein** Credential für die App. Registrierung = Admin legt Runner (Name, URL, Kapazität) an, die App erzeugt ein **eigenes Token pro Runner** (einmal angezeigt, in der DB mit dem bestehenden AES-GCM/`MASTER_KEY` verschlüsselt, weil die App es vorzeigen muss). Ein kompromittierter Runner kennt nur sein eigenes Token, kann also weder andere Runner noch die App-API ansprechen (Least Privilege). Widerruf = Token in der App gelöscht, Runner bekommt nie wieder Arbeit/Zugangsdaten.
3. **Heartbeat = App-Monitor + Lauf-Lease auf dem Runner.** Die App ruft alle `RUNNER_HEARTBEAT_SECS` (5 s) `POST /v1/heartbeat {lease_secs, runs[]}` jedes Runners. Der Runner verlängert damit die Lease seiner Läufe; ein Lauf, dessen Lease abläuft (App weg / Netz getrennt), wird vom Runner abgebrochen (SIGINT → Grace → SIGKILL). Die App erklärt einen Runner nach `RUNNER_LEASE_SECS` (30 s) ohne erfolgreichen Heartbeat für **offline**, bricht die Streams seiner Läufe ab und reiht die Aufträge als `infra` neu ein (kein Fix-Versuch verbraucht). Neu starten können sie nur auf demselben Runner (Projekt-Affinität), und der Runner lehnt einen zweiten Prozess im selben Workspace ab (409) ⇒ **kein Doppel-Lauf**, selbst wenn die Lease-Uhr falsch geht. Lease-Durchsetzung ist pro Lauf opt-in (`ExecRequest.lease_secs`), damit Alt-Aufrufer/Runner-Unit-Tests unverändert laufen.
4. **Projekt-Affinität:** `projects.runner_id` (Workspaces liegen nur dort). Neue Projekte bekommen beim Anlegen den aktiven, erreichbaren Runner mit den wenigsten Projekten. Der eingebaute Runner aus `RUNNER_URL`/`RUNNER_TOKEN` ist eine feste Zeile `builtin` – bestehende Projekte bleiben dort, Einzel-Runner-Betrieb unverändert. **Umziehen** (Admin) nur bei ruhendem Projekt: Bundles aller Workspaces des Projekts vom alten Runner → neuer Runner.
5. **Kapazität/Drain:** `runners.capacity` (NULL = unbegrenzt, Standard für `builtin` = bisheriges Verhalten). Der Scheduler startet einen Auftrag nur, wenn der Runner des Projekts `active`, online und unter Kapazität ist (Zeilensperre Runner nach Projekt, gleiche Lock-Reihenfolge wie WP04). Drain: keine neuen Läufe/Projekte, laufende und Merges laufen zu Ende.
6. **MFA:** TOTP (RFC 6238, SHA-1, 6 Stellen, 30 s, ±1 Schritt) selbst implementiert über die vorhandenen RustCrypto-Crates `hmac` + `sha1` (40 Zeilen, RFC-Testvektoren) statt einer weiteren Crate. Replay-Schutz: letzter verbrauchter Zeitschritt, atomar per `UPDATE … WHERE last_step < $step`. WebAuthn über **webauthn-rs 0.6.2** (kanidm, veröffentlicht 2026-10-31, ~9,7 Mio. Downloads, aktiv gepflegt; MPL-2.0; braucht OpenSSL 3, im System vorhanden). Zustände zwischen Start/Ende liegen serverseitig in der DB (Feature `danger-allow-state-serialisation`: der Zustand verlässt nie den Server, nur so funktioniert er nach einem Neustart), Test mit `webauthn-authenticator-rs` (`softpasskey`) und im Browser mit dem virtuellen Authenticator von Chromium (CDP).
7. **MFA-Verhältnis zu OIDC:** Lokale MFA gilt **auch nach OIDC-Login**. Begründung: OIDC verknüpft über verifizierte E-Mail mit bestehenden Konten; ein schwach gesicherter IdP-Account oder ein IdP mit laxer E-Mail-Verifikation wäre sonst ein Bypass. Ein IdP-eigener zweiter Faktor (`amr`) wird nicht ausgewertet (nicht standardisiert genug, IdP-spezifisch).
8. **Kein MFA-Bypass:** Login mit Passwort/OIDC erzeugt bei MFA-Nutzern **keine** Session, sondern einen kurzlebigen, einmal nutzbaren MFA-Vorgang (Cookie `qmfa`, 5 min, 5 Versuche); erst der zweite Faktor erzeugt eine **neue** Session (neues Zufallstoken ⇒ keine Session-Fixation; eine evtl. vorhandene alte Session des Browsers wird gelöscht). API-Tokens erreichen keine Konto-/MFA-/Admin-Route (Default-Deny bleibt); gesperrte Nutzer verlieren Tokens sofort. Instanz-Richtlinie „MFA-Pflicht“ (aus / Admins / alle): Wer betroffen ist und keine MFA hat, kann nach dem Login nur die MFA-Einrichtung erreichen (Session und Tokens).
9. **Recovery-Codes:** 10 Codes à 10 Zeichen (Base32 ohne Verwechsler, ≈50 bit), gespeichert als HMAC-SHA-256 mit einem aus `MASTER_KEY` abgeleiteten Pepper (ein DB-Dump allein reicht nicht zum Offline-Raten), Einmalnutzung atomar.
10. **Secrets der MFA** (TOTP-Seed) mit `crypto::encrypt` (AES-256-GCM, `MASTER_KEY`) – der bestehende Mechanismus.
11. **Drosselung:** Tabelle `auth_throttle` (Schlüssel `login:<email>`, `mfa:<user>`): 10 Passwortfehler bzw. 5 MFA-Fehler in 15 min ⇒ Sperre 15 min (429 mit Restzeit). Schlüssel auch für unbekannte E-Mails (kein Nutzer-Enumerations-Orakel). Kein IP-Limit: hinter Tunnel/Proxy ist die Client-IP nicht vertrauenswürdig – Reverse-Proxy-Aufgabe (README).
12. **Audit-Log:** Tabelle `audit_log` mit `seq` (lückenlos, in Kettenreihenfolge), Hash-Kette `hash = sha256(prev_hash || kanonischer Inhalt)` im `BEFORE INSERT`-Trigger (eingebautes `sha256()` ab PG 11, Advisory-Lock serialisiert Einfügungen). `BEFORE UPDATE/DELETE`-Trigger verbietet Änderungen; DELETE nur für Zeilen älter als die im Trigger fest verdrahtete Mindestfrist (365 Tage) und nur vom ältesten Ende her (Kette bleibt prüfbar); `TRUNCATE` per Statement-Trigger verboten; `REVOKE UPDATE, DELETE, TRUNCATE … FROM PUBLIC`. Prüfung der Kette als SQL-Funktion + Admin-Endpunkt. Ehrliche Grenze: Der Tabelleneigentümer kann Trigger abschalten → README beschreibt die Trennung in eigene DB-Rolle; die Kette macht Manipulation sichtbar.
13. **Audit-Umfang:** Auth (Login ok/Fehler/gesperrt, Logout, Registrierung, OIDC, MFA an/aus, Recovery-Nutzung, Passwortwechsel, Session-Widerruf), Zugangsdaten (Provider-Credentials, Connectoren, Projekt-Secrets, Umgebung, Git-Token), API-Tokens, Projekte/Mitglieder, Pushes (manuell + Auto-Push), Freigaben (Gate, Plan, Auftrag, Wissen), Admin-Aktionen (Sperre, Rolle, Einladung, Registrierungsmodus, Quota, MFA-Pflicht, Runner, Preis-Overrides), Backup/Restore. Details nur Metadaten – nie Werte (Test sucht Marker-Secrets im ganzen Log).
14. **Quotas pro Nutzer** hängen an der WP13-Kostenrechnung (`usage_log.cost_usd`, rollierend 30 Tage, wie `limit_30d_usd`): `max_cost_30d_usd`, `max_parallel`, `max_projects`. Erschöpft ⇒ der Scheduler startet keine Aufträge der Person (bleiben in der Queue, UI erklärt es). Nur Admins ändern sie.
15. **Backups:** Verzeichnis pro Lauf (`0700`, Dateien `0600`, `umask 077`): `db.dump` (`pg_dump -Fc`), `workspaces/<runner>/<ws>.bundle` (`git bundle create --all` im Runner), `app/vapid_private.key`, `manifest.json` (SHA-256 je Datei, Versionen, Head-Refs). **Verschlüsselung Pflicht** mit eigenem `BACKUP_KEY` (≠ `MASTER_KEY`, Report P0.11): AES-256-GCM in 64-KiB-Blöcken (STREAM-Konstruktion: Nonce = 7 B Zufall + 4 B Zähler + 1 B Letzter-Block-Flag; Kürzen/Vertauschen/Abschneiden wird erkannt). Klartext nur mit ausdrücklichem `--unencrypted`. `MASTER_KEY` wird **nicht** gesichert (README: separat aufbewahren). Restore nur in eine **leere** DB.

## 3. Datenmodell (Migration `0014_enterprise.sql`)

- `users`: `disabled_at`, `disabled_reason`, `password_changed_at`.
- `sessions`: `id UUID UNIQUE`, `last_seen_at`, `user_agent`, `auth_method` (z. B. `password+totp`).
- `mfa_totp(user_id PK, secret_enc, confirmed_at, last_step)`; `webauthn_credentials(id, user_id, name, cred_id UNIQUE, passkey JSONB, created_at, last_used_at)`; `recovery_codes(id, user_id, code_hash UNIQUE, used_at)`; `auth_flows(id, token_hash UNIQUE, user_id, kind, state JSONB, method, attempts, expires_at)` (MFA-Login, WebAuthn-Zeremonien); `auth_throttle(key PK, failures, window_start, locked_until)`.
- `instance_settings(id=1, registration_mode CHECK open|invite|closed NULL=Env, mfa_policy CHECK off|admins|all)`.
- `invitations(id, token_hash UNIQUE, email NULL, make_admin, created_by, created_at, expires_at, used_at, used_by, revoked_at)`; `oauth_states.invite_hash`.
- `user_quotas(user_id PK, max_cost_30d_usd, max_parallel, max_projects, updated_by, updated_at)`.
- `audit_log(seq BIGINT UNIQUE, id BIGSERIAL PK, at, actor_id, actor_label, action, target_type, target_id, project_id, details JSONB, prev_hash, hash)` + Funktionen/Trigger, Index `(action, seq)`, `(actor_id, seq)`, `(project_id, seq)`.
- `runners(id, name UNIQUE, url, token_enc, builtin, status CHECK active|draining|revoked, capacity, created_at, last_seen_at, offline_since, version, engine, egress_enforced, active_runs)`; feste Zeile `builtin` (`00000000-0000-0000-0000-000000000001`).
- `projects.runner_id NOT NULL DEFAULT builtin`, `tasks.runner_id` (Runner des laufenden Versuchs).

## 4. API (neu)

Konto (nur Cookie): `GET /account/security` (MFA-Status, Passkeys, Anzahl Codes), `POST /account/password`, `POST /account/mfa/totp/setup|confirm`, `DELETE /account/mfa/totp`, `POST /account/mfa/recovery` (neu erzeugen), `POST /account/webauthn/register/start|finish`, `DELETE /account/webauthn/{id}`, `GET /account/sessions`, `DELETE /account/sessions/{id}`, `POST /account/sessions/revoke-others`. Ändernde MFA-/Passwort-Aktionen verlangen Bestätigung mit Passwort **oder** aktuellem zweitem Faktor (Step-up).
Login: `POST /auth/login` → `{ok}` oder `{mfa_required, methods}`; `POST /auth/mfa/totp`, `/auth/mfa/recovery`, `/auth/mfa/webauthn/start|finish`; `POST /auth/register` mit `invite`; `GET /auth/invite/{token}` (Vorschau); `GET /auth/oidc/{p}/start?invite=`.
Admin (nur Cookie + `is_admin`): `GET /admin/users`, `PATCH /admin/users/{id}` (Sperre, Rolle), `PUT /admin/users/{id}/quota`, `GET/POST /admin/invitations`, `DELETE /admin/invitations/{id}`, `GET/PUT /admin/settings`, `GET /admin/audit` (Filter: Aktion-Präfix, Person, Projekt, Zeitraum; Cursor-Paging), `GET /admin/audit/export` (JSONL), `GET /admin/audit/verify`, `GET/POST /admin/runners`, `PATCH/DELETE /admin/runners/{id}`, `POST /admin/runners/{id}/token` (neues Token), `POST /admin/projects/{id}/move`.
Runner (`/v1`, Runner-Token): `POST /heartbeat`, `GET /workspaces/{id}/bundle`, `POST /workspaces/{id}/restore`, `GET /workspaces` (Liste für Backup).

## 5. UI (Leptos, WP00-Designsystem)

- Einstellungen → Karte **„Sicherheit“**: Passwort ändern, Zwei-Faktor (App einrichten mit QR-Code als SVG aus der `qrcode`-Crate im WASM + manueller Schlüssel, Codes bestätigen), Passkeys (hinzufügen/benennen/entfernen), Wiederherstellungscodes (einmal anzeigen, kopieren/herunterladen, neu erzeugen), **Angemeldete Geräte** (aktuelles markiert, einzeln / „alle anderen abmelden“).
- Login: zweiter Schritt „Bestätige, dass du es bist“ (Code-Feld mit `autocomplete=one-time-code`, Passkey-Button, Link „Wiederherstellungscode verwenden“), Registrierung per Einladungslink.
- Neuer Bereich **„Verwaltung“** (nur Admins; Tab-Bar mobil bzw. Seitenleiste): Personen (Suche, Sperren/Entsperren mit Bestätigung, Admin-Rolle, Kontingent), Einladungen (Link einmal anzeigen, Ablauf, widerrufen), Registrierung & MFA-Pflicht, **Protokoll** (Filter, „Mehr laden“, Kette prüfen, Export), **Runner** (Status in Klartext: „Bereit“, „Leert sich“, „Nicht erreichbar“, „Widerrufen“, Auslastung, letzter Kontakt, Projekte; Anlegen mit einmaligem Token + Env-Snippet; Drain/Aktivieren/Widerrufen; Projekt umziehen).
- Safe-Areas, Touch ≥ 44 pt, 16-px-Inputs, axe hell/dunkel Desktop + iPhone.

## 6. Bedrohungsmodell (Auszug)

| Bedrohung | Gegenmaßnahme | Test |
|---|---|---|
| Passwort-/TOTP-Brute-Force | Drosselung pro Konto/Faktor, 429 mit Restzeit, Audit | Integration: 11. Fehlversuch 429 trotz richtigem Passwort; 6. TOTP 429 |
| TOTP-Replay (abgefangener Code) | letzter Zeitschritt pro Nutzer, atomar | Integration: gleicher Code zweimal ⇒ 2. abgelehnt; Gegenprobe Mutation |
| Session-Fixation | neue Session erst nach 2. Faktor, Vorgangs-Token einmalig | Integration: Cookie vor/nach Login verschieden, alter Wert ungültig |
| MFA-Bypass per OIDC / API-Token / halbfertigem Login | lokale MFA nach OIDC; Token-Routentabelle; ohne 2. Faktor keine Session | Integration (Fake-IdP), Token auf `/account/*` und `/admin/*` 403 |
| Gestohlene Session nach Passwortwechsel | andere Sessions widerrufen, SSE-Stream endet | Integration |
| Gesperrter Nutzer arbeitet weiter | Sperre löscht Sessions, widerruft Tokens, Extraktor prüft `disabled_at`, SSE prüft periodisch, Scheduler startet nichts | Integration + E2E |
| Admin sperrt sich/letzten Admin aus | Transaktion mit Zeilensperre auf aktive Admins | Integration inkl. paralleler Gegen-Herabstufung |
| Audit-Manipulation | Trigger + Rechte + Hash-Kette, Prüf-Endpunkt | Integration: UPDATE/DELETE/TRUNCATE scheitern, manipulierte Zeile (Trigger als Superuser umgangen) wird erkannt |
| Secrets im Audit | nur Metadaten | Marker-Suche |
| Backup-Diebstahl | Verschlüsselung mit eigenem Schlüssel, 0600/0700, kein `MASTER_KEY` im Backup | Unit (Manipulation/Abschneiden erkannt), Integration (Rechte, kein Klartext-Marker) |
| Kompromittierter Runner | eigenes Token pro Runner, bekommt nur Arbeit seiner Projekte, kein App-Credential | Integration: Token A öffnet Runner B nicht, Anfragen für Projekt X gehen nie an Runner Y, Runner-Token öffnet App-API nicht |
| Runner-Ausfall / Netztrennung ⇒ Doppel-Lauf | Lauf-Lease auf dem Runner, Offline nach Lease, Workspace-Exklusivität | Integration: Runner eingefroren ⇒ Auftrag neu eingereiht, Runner beendet alten Lauf, genau ein Agentenlauf aktiv |

## 7. Tests (Plan)

Unit: TOTP RFC-6238-Vektoren, Fenster, Base32; Recovery-Code-Format/Normalisierung; Backup-Stream-Verschlüsselung (Roundtrip, Manipulation, Abschneiden, Blocktausch, falscher Schlüssel); Throttle-Rechnung; Audit-Kanonisierung; Kapazitätswahl.
Integration (App + Runner in-process + Postgres): alle Zeilen der Tabelle in §6, WebAuthn-Registrierung/Login mit Soft-Passkey, Recovery-Einmalnutzung, Session-Liste/Widerruf, Einladung/Modi, Quota blockiert Scheduler, Backup→leere DB→Restore→Daten+Repos identisch, Runner-Flotte (Zuweisung, Drain, Kapazität, Umzug, Ausfall, Isolation).
System/E2E (Playwright): MFA einrichten mit QR + TOTP (Code im Test berechnet), Login mit TOTP und Recovery, Passkey mit virtuellem Authenticator, Sessions widerrufen, Admin sperrt Person (deren Browser fliegt raus), Einladung, Audit-Filter, Runner-Übersicht; iPhone hoch/quer für Sicherheit + Verwaltung; Tour/axe hell/dunkel.
Gegenproben (Mutation) für: Replay-Schutz, Recovery-Einmalnutzung, Audit-Trigger, Sperre→Token, letzter Admin, Runner-Workspace-Exklusivität, Lauf-Lease.

## 8. Risiken

- **Umfang** (fünf Teilpakete). Gegenmittel: gemeinsame Bausteine (Audit-Helper, Admin-Extraktor, Bundle-Endpunkte für Umzug *und* Backup).
- **OpenSSL-Abhängigkeit** durch webauthn-rs: Build-Images brauchen `libssl-dev`/`pkg-config` (Dockerfile anpassen).
- **WebAuthn im echten Safari/iPhone** nicht testbar (nur Chromium-Emulation + Soft-Passkey).
- **Lease-Timing:** zu kurze Lease killt Läufe bei App-Hängern; Standard 30 s, Heartbeat 5 s, Grenzen validiert.
- **Owner kann Trigger abschalten:** dokumentiert, Kette erkennt es.
- **Restore** braucht passende `pg_restore`-Version (≥ Server).
