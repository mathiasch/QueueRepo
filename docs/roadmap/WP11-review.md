# WP11 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP11-Diff auf `mvp` (Migration 0014, App, Runner, Common, API-Typen, Frontend, E2E, Deploy, README). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP11), Auftrag (MFA, Audit, Admin, Backups, Runner-Flotte, kein Root, Sicherheitsmodell nicht aufweichen), Report P0.10/P0.11/P1.11/P2.7/P2.8. Jede Feststellung ist am Code, an einem Test oder an einem Lauf geprüft; was nur gelesen wurde, steht so da.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und mit Tests belegt; fünfzehn sicherheitskritische Mutationen (M1–M13, R1, R2) werden von den Tests erkannt (`WP11-test.md` §2). Die ehrlichen Grenzen: WebAuthn ist nur gegen einen Software- und Chromiums virtuellen Authenticator geprüft, nicht gegen echte Geräte; die Flotte ist auf einem Host mit mehreren Runner-Prozessen geprüft, nicht über echte Netze; der Tabelleneigentümer kann die Audit-Trigger abschalten (die Rollentrennung dagegen ist dokumentiert und real geprüft, aber nicht Standard).

## Akzeptanz gegen den Auftrag

| Anforderung | Status | Beleg |
|---|---|---|
| TOTP RFC 6238 mit Replay-Schutz | erfüllt | Unit `rfc6238_test_vectors` (RFC 6238 + 4226), `totp_window_and_newest_step`; Integration `totp_login_replay_and_lockout` (gleicher und älterer Schritt abgewiesen, zweiter Login mit gleichem Code abgewiesen); Mutation M1 |
| WebAuthn/Passkeys über gepflegte Crate | erfüllt | webauthn-rs 0.6.2 (Version/Wartung im Plan geprüft); `passkey_registration_and_login` (SoftPasskey, falscher Origin abgewiesen, Zähler gespeichert); E2E `security.spec.ts` (virtueller Authenticator in Chromium) |
| Recovery-Codes gehasht, Einmalnutzung | erfüllt | `recovery_codes_are_single_use` (kein Klartext in der DB, zweite Nutzung 401, parallele Nutzung genau ein Erfolg); Mutation M2 |
| MFA-Secrets verschlüsselt (bestehender Mechanismus) | erfüllt | `crypto::encrypt` (AES-256-GCM, `MASTER_KEY`); Test sucht das Seed-Byte-Muster in `secret_enc` |
| Rate-Limit/Lockout | erfüllt | Passwort 10/15 min, Faktor 5/15 min, je Konto (auch unbekannte Adressen gleich); Integration |
| Verhältnis zu OIDC, kein Bypass über Tokens/Fixation | erfüllt | `oidc_login_still_needs_the_second_factor` (Fake-IdP: OIDC endet im Zwischenschritt, keine Session davor; Einladungsmodus und Sperre auch über OIDC; Mutation M12), `mfa_cannot_be_bypassed` (13 Routen × 2 Tokens = 403, MFA-Pflicht sperrt auch Tokens), `sessions_are_listed_revoked_and_never_fixed` (gepflanztes Cookie wird ersetzt und ungültig); Mutationen M6, M7 |
| Sessions anzeigen/widerrufen, Widerruf bei Passwort-/MFA-Änderung | erfüllt | Integration + E2E (zweiter Browser wird abgemeldet, Live-Stream endet ≤ 10 s) |
| Audit append-only auf DB-Ebene, Hash-Kette | erfüllt | `audit_log_is_append_only_chained_and_secret_free` (UPDATE/DELETE/TRUNCATE per SQL abgewiesen, Manipulation an den Triggern vorbei von `audit_log_verify` gefunden), `audit_retention_only_deletes_old_entries_in_order`, Rollentrennung real geprüft (Test-Bericht §4); Mutation M3 |
| Audit-Umfang, keine Secrets, Admin-UI mit Filter/Paging, Retention dokumentiert | erfüllt | Marker-Suche (Zugang, Projekt-Secret, Token, Passwort) im ganzen Log; Filter/Cursor/Export-Tests; E2E Protokoll-Reiter; README |
| Admin: sperren (Sessions + Tokens sofort), Rollen, Einladungen, Registrierungsmodus, Quotas an WP13-Kosten, letzter Admin, alles auditiert | erfüllt | `lock_revokes_sessions_and_tokens` (+ Stream, Scheduler), `last_admin_cannot_lock_out` (5 Runden gegenseitiges Herabstufen parallel), `invitations_and_registration_modes`, `quotas_bind_scheduler_and_projects`; E2E Verwaltung; Mutationen M4, M5 |
| Backup: pg_dump + git bundle, Kommando + Timer ohne Root, Verschlüsselung/Rechte, automatisierter Restore-Test | erfüllt | `backup_restore_roundtrip` (alle Tabellen md5-gleich, Refs/HEAD/Tree/Log gleich, Kette intakt, wiederhergestellte Instanz arbeitet, falscher Schlüssel/Manipulation/nicht leere DB abgewiesen, 0700/0600, keine Klartext-Marker); System-Test mit echten Binaries (`queued backup`/`restore`, zwei Runner-Prozesse); Unit-Tests der Stream-Verschlüsselung; Mutation M11 |
| Runner-Flotte: Registrierung mit eigenem Credential (widerrufbar), Lease/Heartbeat, Kapazität, Drain, Verteilung mit Affinität, Ausfall ohne Doppel-Lauf, Least Privilege, Einzel-Runner kompatibel | erfüllt | `fleet_registration_isolation_drain_capacity_and_move`, `runner_failure_requeues_without_double_run` (eingefrorene Verbindung, kein RST), Runner-Unit `lease_keeps_runs_alive_only_while_heartbeats_arrive`; Alt-Tests unverändert grün (Einzel-Runner); Mutationen M8–M10 |
| UI: MFA mit QR, Passkeys, Sessions, Admin, Audit, Runner; WP00-Design, Safe-Areas, axe | erfüllt | E2E Desktop (hell/dunkel axe), iPhone hoch/quer (Safe-Area, 44 pt, 16 px, kein horizontales Scrollen, axe) |
| Kein Root, Sicherheitsmodell nicht aufgeweicht | erfüllt | Runner-Container-Argumente unverändert (WP02-Tests grün); neue Runner-Endpunkte hinter `RUNNER_TOKEN`; Backup-Timer als Service-User mit Härtung; Schlüsseldateien neu 0600 |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Anonymer DoS gegen das unlöschbare Audit-Log:** Jeder fehlgeschlagene Login (auch für frei erfundene E-Mail-Adressen) und jede Drosselungs-Sperre schrieb einen Eintrag; mit wechselnden Adressen unbegrenzt viele, die 365 Tage nicht gelöscht werden dürfen. | Selbst-Review „was kann ein Anonymer schreiben?“ | Audit nur für existierende Konten (durch die Drosselung je Konto begrenzt); Unbekannte nur als Metrik `queued_auth_failures_total` | `totp_login_replay_and_lockout`: 10 Fehlversuche für `nobody@x.org` ⇒ 0 anonyme `auth.*`-Einträge; Mutation M13 |
| B2 | **Hängende Aufrufe an einen toten Runner:** Nach dem Abbruch des Streams rief die App `cancel` und Aufräum-Endpunkte des verlorenen Runners mit 300-s-Timeout auf; der Auftrag blieb „läuft“ (Test war rot). | Integrationstest `runner_failure_…` (erste Fassung rot) | Client lehnt jede Anfrage an einen als verloren markierten Runner sofort ab und bricht laufende Anfragen beim Verlust ab (`send()`); nur der Heartbeat geht weiter durch | derselbe Test grün; Mutation M10 |
| B3 | **Zwei Prozesse im selben Workspace möglich:** Der Runner verbot nur denselben Laufnamen, nicht einen zweiten Lauf im selben Arbeitsverzeichnis. Mit der Flotte (verlorene Läufe, Wiederholung) wäre das der Doppel-Lauf. | Planung des Ausfallmodells | `exec_start` lehnt mit 409 ab, solange ein Prozess im Workspace läuft (atomar unter derselben Sperre) | Runner-Unit `lease_keeps_runs_alive…`; Gegenprobe R1 (Test-Bericht) |
| B4 | **Hängender Lauf nach App-Ausfall:** Der Runner beendete einen Lauf nur, wenn eine Zeile nicht mehr zugestellt werden konnte; ein stiller Agent lief nach einer Netztrennung weiter. | Lesen von `exec_start` | Lauf-Lease mit Heartbeat-Verlängerung | Runner-Unit; Gegenprobe R2 (Test-Bericht) |
| B5 | **Session-Widerruf wirkte nicht auf offene Live-Streams** (WP07-N6: bis zu 10 min; Cookie-Streams gar nicht). Eine Sperre hätte den Browser der Person weiter mit Live-Daten versorgt. | Offener Fund WP07-N6 | Stream prüft alle 5 s Session/Token/Sperre; Frontend geht nach Stream-Ende bei 401 auf die Anmeldung | Integration (Stream endet ≤ 12 s), E2E (Browser landet auf „abgemeldet“) |
| B6 | **`master.key` und `vapid_private.key` mit Umask-Rechten** (typisch 0644) angelegt – der Schlüssel, der alle Zugangsdaten schützt, lesbar für andere lokale Benutzer. | Review der Dateien, die Backup/Restore berührt | `create_new` + `mode(0o600)` | Code; System-Test prüft die Rechte der Backup-Dateien, nicht dieser Datei (s. N12) |
| B8 | **Zwei bestehende Test-Rennen (WP06/Altbestand) machten die Abnahme unzuverlässig:** `api::more_than_two_generated_tasks_need_approval` prüfte die Benachrichtigung sofort nach dem dritten wartenden Auftrag, sie wird aber erst danach angelegt; `plan_steering.spec.ts` las `/api/tasks` direkt nach dem Klick auf „Einreihen“, bevor die Anfrage fertig war (`undefined.id`). Je einmal rot in Abnahmeläufen. | Abnahmelauf C (rot), Code gelesen | beide warten jetzt auf den Zustand (Polling mit Frist), Prüfung unverändert streng | Läufe D und E grün |
| B9 | **Einblend-Animation der Toasts mit Transparenz:** Erscheint eine Meldung (z. B. „Auftrag fertig“) während einer axe-Prüfung, misst axe die halbtransparenten Knöpfe als zu kontrastarm (Abnahmelauf D, iPhone quer, Ansicht „Zustand“, WP09-Test). Auch für Menschen ist ein halbtransparenter Knopf schlecht lesbar. | Abnahmelauf D (rot), `error-context.md` | Animation nur noch Bewegung, keine Deckkraft | Läufe E und F grün |
| B7 | **Testfehler, die echte UI-Probleme verdeckt hätten:** „Bestätigen“ war als Name nicht eindeutig (zweiter Knopf „Doch mit App … bestätigen“), Abmelden + sofortiges Navigieren kollidierten. | E2E (erste Fassung rot) | Selektoren exakt, auf die Anmeldeseite warten | E2E grün |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **Konto-Sperre durch Dritte (Drosselung je Konto):** Wer eine E-Mail-Adresse kennt, kann deren Passwort-Login 15 min sperren, wiederholt. | Bekannter Zielkonflikt; bestehende Sessions laufen weiter, Passkey/OIDC-Zwischenschritt nicht betroffen. IP-Limits gehören in den Reverse-Proxy (README). |
| N2 | **Kein Step-up für das Anlegen von API-Tokens.** Eine gestohlene Session kann Tokens erzeugen. | Bei MFA-Nutzern ist jede Session MFA-geprüft; Token-Erstellung ist auditiert. Step-up hätte WP07-UI/E2E geändert. |
| N3 | **WebAuthn nur emuliert geprüft** (SoftPasskey, Chromium-CTAP2), kein Safari/iOS, kein Hardware-Schlüssel. | Format über webauthn-rs; Ein Fehler zeigt sich als verständliche Meldung, Wiederherstellungscodes bleiben. |
| N4 | **Tabelleneigentümer kann Audit-Trigger abschalten.** | Hash-Kette macht es sichtbar; Rollentrennung im README (real geprüft), nicht Standard, weil Migrationen sonst eine zweite DB-Rolle bräuchten. |
| N5 | **Gesperrte Personen:** laufende Aufträge laufen zu Ende; Zeitpläne/Anbindungen legen weiter Aufträge an (sie starten nicht). | Nicht verlangt; dokumentiert. |
| N6 | **Backup scheitert komplett, wenn ein Runner nicht erreichbar ist** (keine DB-Sicherung in dieser Nacht). | Bewusst strikt (unvollständige Backups sind gefährlich); Timer-Fehler im Journal. Ein `--allow-partial` wäre die Erweiterung. |
| N7 | **`/readyz` prüft nur den Standard-Runner;** Metrik `queued_runner_up` ebenso. | WP10-Semantik unverändert; Flotte in der Verwaltung. |
| N8 | **Backup nicht atomar über DB und Workspaces** (Dump zuerst, dann Bundles). Ein Auftrag kann dazwischen fortschreiten. | Restore reiht unterbrochene Läufe neu ein; ein Branch, den die DB kennt, kann fehlen – die Diff-Anzeige behandelt fehlende Branches bereits. |
| N9 | **Workspaces widerrufener Runner** sind nicht im Backup (der Runner wird nicht mehr angesprochen). | Folgerichtig; vor dem Widerruf umziehen. |
| N10 | **Unversionierte Dateien** in Arbeitsverzeichnissen sind nicht im Backup/Umzug (`git bundle`). | Vorgabe „git bundle“; dokumentiert, Test prüft es ausdrücklich. |
| N11 | **Kapazität zählt nur Agentenläufe**, nicht Übernahmen (Verify in der Merge-Queue). | Damit leert sich ein Runner auch bei Drain; Merges sind pro Projekt seriell. |
| N12 | Die neuen 0600-Rechte von `master.key`/`vapid_private.key` sind nicht durch einen eigenen Test abgedeckt. | Zwei Zeilen, gelesen; bestehende Dateien werden nicht umgestellt (Betreiber: `chmod 600`). |
| N13 | **Docker-Image ohne `pg_dump`** (bookworm: 15, DB: 16). | Dokumentiert; Backup auf dem Host oder mit `postgres:16`-Client. |
| N14 | **systemd-Units nicht unter echtem systemd gestartet** (Container ohne systemd). | Syntax nach bestehenden Units; Kommando selbst real geprüft (System-Test). |
| N15 | **Audit nach der Aktion** (außer Admin-Aktionen in derselben Transaktion): Fällt das Schreiben aus, bleibt die Aktion ohne Eintrag (Log + Metrik). | Abwägung gegen Verfügbarkeit; Admin-Aktionen sind strikt. |
| N16 | Session-Liste zeigt keine IP-Adresse. | Hinter dem Tunnel nicht vertrauenswürdig; Gerät/Anmeldeart/letzte Aktivität stattdessen. |

## Geprüft und für gut befunden

- **Least Privilege der Flotte:** Token pro Runner, verschlüsselt (Test sucht Klartext in `token_enc`), Runner-Token öffnet weder andere Runner noch die App (Test), Anfragen eines Projekts gehen nur an seinen Runner (Workspace existiert nur dort, Test), Widerruf löscht das Token.
- **Kein Doppel-Lauf:** drei unabhängige Sperren (App-Lease, Runner-Lease, Workspace-Exklusivität), Test mit eingefrorener TCP-Verbindung.
- **Architektur:** App ohne Host-Rechte (Backup ist ein eigenes Kommando, Git nur im Runner), Protokoll in `queued-common`, Typen in `queued-api`, nur neue Migration 0014, alte unverändert.
- **Alt-Verhalten:** Einzel-Runner-Betrieb ohne neue Konfiguration; `Meta.allow_registration` unverändert für alte Clients; bestehende Tests nur dort angepasst, wo sich ein Typ geändert hat (`edge_cases.rs`: Client pro Projekt).
