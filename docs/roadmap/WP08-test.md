# WP08 – Intake-Integrationen: Testbericht

Stand: 2026-11-11, Branch `mvp` (Arbeitsverzeichnis, nicht committet; gesichert auf `wip-snapshot`). Alle Zahlen stammen aus Läufen in dieser Sitzung mit einem Skript, das die komplette Abnahme in einem Zug ausführt: Backend `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`; Frontend `cargo fmt --all -- --check`, `cargo clippy` für `wasm32-unknown-unknown` und nativ mit `-D warnings`, `cargo test`, `trunk build --release`; Playwright alle Projekte (system, desktop, iphone, iphone-landscape; inkl. axe-Tour hell/dunkel). Postgres 16 lokal, Runner-Engine `none`, Mock-Provider. **Keine echten externen Calls in Tests:** Zustellungen von GitHub, Slack, Linear und Jira sind nachgebaut und im Test signiert wie dokumentiert; die Rückkanäle laufen gegen den eigenen Fake-Server `tests/common/fake_intake.rs` (im E2E-Stack als `examples/fake_intake.rs`). Kosten: 0 (keine LLM- oder bezahlten API-Aufrufe).

## 1. Teststrategie

| Ebene | Was | Wo |
|---|---|---|
| Unit `queued-api` (+6) | Arten/Texte vollständig, Ergebnis-Enums rund; Mandanten je Art validiert und normalisiert (GitHub `owner/repo`, Slack `T…`/`E…`, Linear-UUID, Jira nur `https://host`, `is_atlassian_site` gegen `…atlassian.net.evil`); Konfiguration (Label oder Erwähnung Pflicht bei GitHub/Jira, Slack/Linear ohne), Personen-IDs je Art, Phasen/Zustände, `safe_url` (kein `javascript:`), Einrichtungsschritte nennen die Adresse | `api/src/intake.rs` |
| Unit App (+16) | **GitHub:** Doku-Testvektor, Label/Erwähnung, `author_association`, Kommentare (Antwort vs. Erwähnung), eigene Kommentare/PR-Kommentare/Bots ignoriert, `@` entschärft, Marker. **Slack:** **Testvektor der Slack-SDKs (= Doku-Beispiel)**, Zeitfenster ±300 s an beiden Rändern, geänderter Body/anderes Secret/`v1`/anderer Zeitstempel ⇒ abgelehnt, Slash-Befehl, Erwähnung, Thread-Antwort, Erwähnung im Thread, Bot-/System-/oberste Nachrichten ohne Schlüsselverbrauch, `url_verification`, Escaping. **Linear:** Signatur, signierter Zeitstempel (59 s ok, 61 s abgelehnt, fehlend abgelehnt), `created`/`prompted`, eigene Activities und Stopp-Signal ignoriert, ohne Ersteller, Identifier-Validierung für den Titel, Activity-Typen. **Jira:** `X-Hub-Signature`, Label beim Anlegen/neu im Changelog (nicht bei schon vorhandenem), Kommentar = Antwort, eigene Kommentare (Referenzzeile) ignoriert, ADF→Text, Schlüssel/Site. **Kern:** Backoff, Textlimit | `app/src/intake/*.rs` |
| Integration `tests/intake.rs` (8, neu) | siehe Matrix unten | `app/tests/intake.rs` |
| Unit Frontend (+2) | Zusammenfassungen in einfacher Sprache (worauf, wer, Rückmeldungen), Formular → Wire-Typ je Art (Secret nur Slack/Linear, Label/Erwähnung/Konto nur wo sinnvoll, Limit 1–100) | `frontend/src/logic/intake.rs` |
| System-E2E (4, neu) | echter Stack, je Art: falsche Signatur ⇒ 401, nicht zugeordnet ⇒ abgewiesen, Zuordnung, signierte Zustellung ⇒ Auftrag, Rückfrage kommt beim Fake an, Antwort über den externen Kanal ⇒ „erledigt“, Fertig-Meldung genau einmal | `e2e/tests/system/intake.spec.ts` |
| UI-E2E Desktop (2, neu) | GitHub verbinden (Art-Wechsel, Erweitert mit Kosten-Hinweis), **Secret und Adresse einmalig** mit Schritten und minimalen Rechten, danach nirgends sichtbar; eigenes Konto zuordnen (sofort aktiv); signiertes Issue ⇒ Auftrag ⇒ Verlauf ⇒ „Auftrag öffnen“ ⇒ Drawer „Aus GitHub-Issue #9“ mit Link (`rel=noopener noreferrer`); Mitglied ordnet sich selbst zu (wartet auf Freigabe), Eigentümerin bestätigt; axe an jedem Zustand | `e2e/tests/ui/integrations.spec.ts` |
| UI-E2E iPhone hoch/quer (1 × 2, neu) | Karte, Personen-Formular, Verbinden (Jira), einmaliger Dialog: Touch-Ziele ≥ 44 pt, Eingaben ≥ 16 px, innerhalb der Safe Areas, kein seitliches Scrollen, axe | `e2e/tests/ui/integrations-mobile.spec.ts` |
| Tour (axe hell/dunkel, Desktop + iPhone hoch/quer) | zusätzlich Reiter „Anbindungen“ mit Slack (zugeordnete Person, abgewiesene Nachricht im Verlauf) und GitHub (ohne Token) | `e2e/tests/tour.ts` |

### Matrix der Pflichtfälle (Integration, je Integration)

| Fall | GitHub | Slack | Linear | Jira |
|---|---|---|---|---|
| gültige/ungültige Signatur | 401 bei falschem Secret und ohne Header, nichts gespeichert | 401 bei falscher Signatur und bei korrekt signiertem, 400 s altem Zeitstempel | 401 bei falschem Secret und bei signiertem, 61 s altem `webhookTimestamp` | 401 bei falschem Secret |
| Replay | gleiche Delivery-ID und gleicher Body unter neuer ID ⇒ Duplikat | gleiche Nachricht mit neuer `event_id` ⇒ Duplikat | gleiche Sitzung erneut ⇒ Duplikat | gleicher Identifier ⇒ Duplikat |
| fremder Mandant | anderes Repo ⇒ 403 | anderer Workspace ⇒ 403 | nicht eigens getestet (derselbe Codepfad wie bei den anderen drei; `organizationId` wird geparst, Unit) | andere Site ⇒ 403 |
| nicht zugeordnet abgewiesen | ja (danach dieselbe Zustellung nach Zuordnung ⇒ 201) | ja (200, Slack würde sonst wiederholen) | ja + **genau eine** `error`-Activity trotz Wiederholung | ja |
| Auftrag mit Verknüpfung | Titel fest, Text nur im Block (Grenzen entschärft), `intake`-Link | Titel „Slack-Auftrag von …“, Link | Titel „Linear ENG-7“, Kontext aus `promptContext` | Link `…/browse/SHOP-7` |
| Status idempotent | „verlorene“ Antwort ⇒ Marker gefunden, 1 Kommentar; weitere Ticks ⇒ keine Schreibzugriffe | „verlorene“ Antwort bei der Rückfrage ⇒ Metadaten gefunden | „verlorene“ Antwort ⇒ gleiche Activity-ID abgelehnt ⇒ gesendet, 3 Activities | „verlorene“ Antwort ⇒ Referenz gefunden |
| Rückfrage-Antwort extern | Kommentar ⇒ `answered`, Nachricht `via=github` zugestellt ⇒ `done` ⇒ Fertig-Kommentar; fremde Person ⇒ abgewiesen; eigener Kommentar ⇒ ignoriert | Thread-Antwort ⇒ `answered` ⇒ `done`; späte Thread-Nachricht ⇒ ignoriert | `prompted` ⇒ `answered` ⇒ `response` | Kommentar ⇒ `answered` ⇒ `done`; eigener Kommentar ⇒ ignoriert |
| weiteres | Label erneut ⇒ ignoriert, `reopened` ⇒ neuer Auftrag; Ping; Token nur im Header | erst `message`, dann `app_mention` ⇒ Auftrag; Slash-Befehl startet den Thread; Erwähnung nach Ende ⇒ „Weiter mit …“ (Text nicht im Prompt); Escaping | `prompted` nach Ende ⇒ „Weiter mit …“ | Konto zum Token Pflicht, nur `https://…atlassian.net` |

Dazu: `github_opt_in_for_unmapped_people_needs_repository_rights` (Opt-in: `NONE` ⇒ abgewiesen, `COLLABORATOR` ⇒ Auftrag als Eigentümerin, Limit 1/h ⇒ **429**), `rights_identities_and_tenants` (nur Eigentümerin verbindet, Fremde 404, Betrachter ohne Secret, zweites Projekt mit gleichem Workspace ⇒ 409, Validierung, zweiseitige Zustimmung inkl. „wartet“ ⇒ abgewiesen, Rollenverlust ⇒ abgewiesen, ohne Token keine Outbox-Zeilen, ausgeschaltet ⇒ Slack 200 „disabled“, Löschen behält Aufträge, Secret nicht im Klartext in der DB), `updates_go_out_once_with_parallel_workers_and_repeat_for_a_repeated_question` (zwei Zusteller gleichzeitig ⇒ je Issue genau eine Meldung; gleiche Rückfrage erneut ⇒ erneut gemeldet), `answer_endpoint_goes_through_the_message_path` (> 4 000 Zeichen und leer ⇒ 400, Fremde 404, Antwort als zugestellte Nachricht, nicht im Kontext, beim Agenten in `SEEN.md`, danach 409).

## 2. Ergebnisse

| Lauf | Backend fmt | Backend clippy | Backend Tests | Frontend fmt | Frontend clippy wasm32 / nativ | Frontend Tests | Trunk | Playwright |
|---|---|---|---|---|---|---|---|---|
| Vorlauf (vor den letzten Fixes) | ok | ok | **rot: 499/500** – `api::question_is_answered_and_task_resumes` prüfte das alte Verhalten von B1 | **rot** (meine unformatierten Zwischenstände, über die Pfad-Abhängigkeit mitgeprüft) | ok / ok | 56/56 | ok | **94/94** (7,4 min) |
| Abnahme 1 (finaler Stand) | ok | ok | **500/500** | ok | ok / ok | **56/56** | ok | **94/94** (6,7 min) |
| Abnahme 2 (finaler Stand, ohne Codeänderung seit Abnahme 1) | ok | ok | **500/500** | ok | ok / ok | **56/56** | ok | **94/94** (5,8 min) |

Backend-Tests je Binary (finaler Stand): app lib **158** (+16), `api` 19, `branches` 9, `cli_contract` 26, `edge_cases` 11, `environments` 8, `failures` 19, `gate` 9, **`intake` 8 (neu)**, `parser_robustness` 6, `plan_steering` 13, `providers` 16, `pull_requests` 9, `tokens` 4, `triggers` 5, queued-api **48** (+6), queued-common 36, queued-runner 90, queuectl 6 = **500** (vor WP08: 470). Frontend **56** (vor WP08: 54). Playwright **94** (vor WP08: 86; neu: 4 System, 2 Desktop, 1 × 2 iPhone; Tour erweitert).

Zwischen Abnahme 1 und 2 wurde kein Code geändert (nur `WP08-test.md`/`WP08-review.md`; geprüft per `git diff` gegen den Snapshot, von dem Abnahme 1 lief). Ein erster Start von Abnahme 2 wurde extern abgebrochen (Sitzungsunterbrechung); Abnahme 2 wurde danach vollständig neu ausgeführt. Abnahme 1 wurde einmal nach ~1 min abgebrochen und neu gestartet, weil ich eine fehlende Testprüfung (Token nie im Text der Rückmeldungen) ergänzt habe; gezählt sind nur die beiden vollständigen Läufe danach.

Während der Entwicklung rot und behoben (Details `WP08-review.md`): Testerwartung zum Ablehnungstext, `MOCK_SLOW` blockierte den Requeue-Test (Parallelität 1), Summen-Assertion zeitabhängig (ersetzt durch Prüfung je Issue), UI-Selektor „Repository“ nicht exakt (traf auch die Checkbox), **Personen-Bereich klappte zu** (B8, echter Fehler), **Benachrichtigungs-Spam ohne Token** (B7, aus dem E2E-Log).

Stabilität: `tests/intake.rs` vor der Abnahme 3× hintereinander 8/8.

## 3. Was die Tests nicht belegen

- **Die echten Dienste** haben nie zugestellt oder geantwortet. Formate stammen aus offiziellen SDKs/Schemas (Slack, Linear, GitHub) bzw. für Jira aus der Sekundärquelle `jira.js`; geprüft ist gegen nachgebaute Zustellungen und einen eigenen Fake. Insbesondere unbelegt: Linears Fehlertext bei doppelter Activity-ID (der Fake antwortet so, wie Queue es erwartet), Jiras `issue.self` in allen Webhook-Varianten.
- **Zwei echte App-Prozesse:** getestet sind zwei gleichzeitige Zusteller in einem Prozess (getrennte Verbindungen, Lease und `SKIP LOCKED` wirken in Postgres).
- **Zeitverhalten der Backoffs** (5 s … 1 h) ist als Funktion getestet; in den Integrationstests wird der nächste Versuch per SQL fällig gemacht statt abzuwarten.
- **Echte iOS-Geräte/WebKit:** iPhone ist in Chromium emuliert (Viewport, Touch, simulierte Safe-Area-Insets).
- **Linear „thought binnen 10 s“:** belegt ist, dass die Outbox beim Anlegen geweckt wird und im Stack mit `INTAKE_TICK_SECS=1` zustellt (System-E2E); eine Zeitmessung gegen Linear gibt es nicht.
