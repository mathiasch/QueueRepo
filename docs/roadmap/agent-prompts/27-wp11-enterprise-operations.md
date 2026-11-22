# Auftrag 27: WP11 enterprise operations

- Gestartet: 2026-11-20 19:55 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP11 („Enterprise & Betrieb“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP11. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP11 relevanten Teile (MFA, Audit, Admin, Backups, Runner-Flotte).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0*-*.md`, `WP10-*.md` und `WP13-*.md`. Besonders wichtig:
   - Auth heute: lokal + OIDC (`oidc.rs`), Sessions, API-Tokens `qk_` (WP07).
   - Rollen/geteilte Projekte.
   - Runner-Modell: separater unprivilegierter Runner, Token-Auth, Egress-Proxy pro Lauf (WP02).
   - Per-Task-Repos und Merge-Queue (WP04).
   - Metriken/Health (WP10).
   - Offene Funde in den Reviews, die WP11 betreffen (Audit, Admin, Runner). Prüf sie und behebe sie, wenn sie hierher gehören.
4. Den relevanten Code: App (`main.rs`, `lib.rs`, `api/`, `auth`/`oidc`, `worker.rs`, `runner.rs`, `models.rs`, Migrationen bis 0013), Runner (`backend/crates/runner`), `backend/crates/api`, `queuectl`, `deploy/` (systemd, compose, env-Beispiele). Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`).

Harte Regeln:
- **MFA:**
  - TOTP nach RFC 6238 mit Replay-Schutz. WebAuthn/Passkeys über eine etablierte, gepflegte Crate (z. B. webauthn-rs); vor der Wahl Version und Wartungsstand prüfen.
  - Recovery-Codes nur gehasht speichern, Einmalnutzung.
  - MFA-Secrets verschlüsselt mit dem bestehenden Credential-Verschlüsselungsmechanismus.
  - Rate-Limit/Lockout gegen Brute Force.
  - Verhältnis zu OIDC begründet festlegen: kein MFA-Bypass über API-Tokens oder Session-Fixation.
  - Session-Verwaltung: aktive Sessions anzeigen und widerrufen (einzeln/alle anderen). Bei Passwort-/MFA-Änderung werden die anderen Sessions widerrufen.
- **Audit-Log:**
  - Append-only, auch auf DB-Ebene erzwungen (z. B. Trigger/Rechte gegen UPDATE/DELETE). Optional Hash-Kette zur Manipulationserkennung.
  - Umfang: Auth, Credentials, Projekt-/Mitgliederänderungen, Pushes, Freigaben, Admin-Aktionen.
  - Keine Secrets im Log.
  - Admin-UI mit Filter/Paging. Export ist optional.
  - Retention ist dokumentiert.
- **Admin-Konsole:**
  - Nutzer sperren/entsperren (Sperre widerruft Sessions + Tokens sofort), Rollen ändern, Einladungen (Einmal-Token mit Ablauf), Registrierungsmodus (offen/Einladung/geschlossen), Quotas pro Nutzer (an Budget/Kosten aus WP13 angebunden).
  - Der letzte Admin kann sich nicht aussperren.
  - Alle Aktionen werden auditiert.
- **Backups/Restore:**
  - `pg_dump` + Workspace-Bundles (`git bundle`).
  - Kommando (queuectl oder Binary-Subcommand) + systemd-Timer-Beispiel, ohne Root (User-Units oder dedizierter Service-User).
  - Verschlüsselung/Rechte der Backup-Dateien beachten; keine Klartext-Secrets ungeschützt rumliegen lassen.
  - Ein automatisierter Restore-Test beweist: Backup → leere DB → Restore → Daten und Repos identisch.
- **Runner-Flotte:**
  - Mehrere Runner-Hosts mit Registrierung (pro Runner eigenes Credential, widerrufbar), Lease/Heartbeat, Kapazität und Drain.
  - Der Scheduler verteilt Projekte auf Runner, Projekt-Affinität wegen der Workspaces.
  - Ausfall eines Runners → Läufe sauber als fehlgeschlagen/neu einreihbar markieren, kein Doppel-Lauf.
  - Ein kompromittierter Runner darf keine Daten anderer Runner/Projekte abgreifen können (Least Privilege). Den bestehenden Einzel-Runner-Betrieb abwärtskompatibel halten.
- Kein Root auf dem Host. Sicherheitsmodell nicht aufweichen. Keine echten externen Calls in Tests. Lizenz-/ToS-Thema bleibt ausgeklammert. Keine Live-Calls an LLMs oder bezahlte APIs; Kosten im Bericht nennen.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird und Rate-Limits Pausen erzwingen, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP11 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass es ein WP11-Snapshot ist und der Parent = HEAD (der Koordinator kann WIP-Commits machen), dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: Prüf den freien Platz. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP11-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP11-plan.md`: Plan, Architektur, Datenmodell, Bedrohungsmodell, Risiken.
- `docs/roadmap/WP11-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP11-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP11-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch): MFA, Admin, Audit, Backup/Restore inkl. Runbook, Runner-Flotte einrichten.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu:
- TOTP-Replay/Lockout
- Recovery-Code-Einmalnutzung
- WebAuthn-Registrierung/Login mit Software-Authenticator bzw. Testvektoren
- Audit-Append-only auf DB-Ebene
- Sperre widerruft Sessions/Tokens
- letzter Admin
- Backup→Restore-Roundtrip
- Runner-Ausfall ohne Doppel-Lauf
- Runner-Isolation

Prüfe mutativ, dass sicherheitskritische Tests wirklich anschlagen.

UI (MFA-Einrichtung mit QR-Code, Passkeys, Sessions, Admin-Konsole, Audit-Ansicht, Runner-Übersicht) folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
