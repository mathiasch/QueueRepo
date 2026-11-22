# Auftrag 24: WP08 intake integrations

- Gestartet: 2026-11-11 12:39 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP08 („Intake-Integrationen“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP08. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP08 relevanten Teile (GitHub Issues, Linear Agent API, Slack, Jira).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0{0,1,2,3,4,5,6,7}-*.md` und `WP13-*.md`. Besonders wichtig:
   - WP07: Auslöser/Vorlagen, HMAC-Webhooks mit Replay-Schutz, GitHub-Webhook-Pfad mit Filtern und Schleifenschutz, Payload nur als `EVENT_DATA`-Datenblock, Token-Scopes. Die gemeinsame Abstraktion „Intake-Quelle“ muss darauf aufbauen oder sie sauber verallgemeinern, nicht duplizieren.
   - WP05: GitHub/GitLab/Gitea-Adapter, Rechteprüfung über die Host-API.
   - WP06: Rückfragen und Steering. Antworten aus Slack-Threads bzw. Kommentaren laufen über den Steering-/Antwort-Pfad mit Längenlimit und Trennung. Der WP06-Fund, dass der bestehende `answer`-Endpunkt Antworten ohne Trennung und Längenlimit anhängt, ist hier zu beheben, weil externe Antworten darüber laufen würden.
4. Den relevanten Code: App (`api/`, Trigger-/Webhook-Module aus WP07, `pr*.rs`, `steer.rs`, `notify.rs`, `crypto.rs`, SSRF-sicherer HTTP-Client, `models.rs`, Migrationen), `backend/crates/api`. Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`).

Harte Regeln:
- **Je Integration** (GitHub Issues, Linear, Slack, Jira): Signaturprüfung exakt nach offizieller Doku. Slack nutzt `X-Slack-Signature` v0 mit Timestamp ±5 min, Linear Webhook-Signatur, Jira je nach Variante (Cloud-Webhook mit Secret bzw. App-JWT). Was du nicht belegen kannst, markierst du als unverifiziert. Keine echten externen Calls in Tests, nur Fake-Server bzw. signierte Fixtures; Testvektoren der Doku nutzen, wo vorhanden.
- **Zuordnung extern → intern:** Ein externer Nutzer darf nur dann einen Auftrag auslösen oder Antworten geben, wenn er einem Queue-Nutzer mit Rechten am Projekt explizit zugeordnet ist bzw. die Integration das per Konfiguration erlaubt. Standard ist restriktiv. Die Mandantentrennung ist strikt: Ein Workspace oder eine Org wird genau einem Queue-Projekt bzw. Nutzer zugeordnet, kein Cross-Tenant.
- **Untrusted Input:** Issue-Texte, Slack-Nachrichten und Jira-Beschreibungen sind untrusted. Sie gehen nur als markierter Datenblock in den Prompt, mit Längenlimits, Dedupe und Rate-Limits.
- **Rückmeldungen nach außen** (Status-Kommentare, Thread-Updates, Linear-Activities): nie Secrets, Diffs nur gekürzt und durch den WP03-Scanner. Idempotenz und Retry mit Backoff, sie dürfen den Scheduler nicht blockieren. Ausgehende Calls nur über den SSRF-sicheren Client. Tokens/Secrets verschlüsselt (crypto.rs), nie in Logs, argv oder API-Antworten.
- Kein Root auf dem Host. Sicherheitsmodell nicht aufweichen. Lizenz-/ToS-Thema bleibt ausgeklammert. Keine Live-Calls an LLMs oder bezahlte APIs; falls doch unvermeidbar, Kosten im Bericht nennen.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP08 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass es ein WP08-Snapshot ist und der Parent = HEAD (der Koordinator kann WIP-Commits machen), dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: ca. 18 GB frei. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP08-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP08-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP08-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP08-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP08-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch): Einrichtung je Integration mit minimalen Rechten/Scopes.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu, je Integration mindestens:
- gültige/ungültige Signatur, Replay
- nicht zugeordneter externer Nutzer wird abgewiesen
- Auftrag entsteht mit Verknüpfung
- Statusrückmeldung idempotent
- Rückfrage-Antwort über den externen Kanal

UI (Integrationen verbinden, Zuordnung von Nutzern/Workspaces, Verknüpfung am Auftrag mit Link) folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
