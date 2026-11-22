# Auftrag 21: WP05 PR workflow

- Gestartet: 2026-11-09 09:25 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP05 („PR-Workflow & Feedback-Loop“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP05. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP05 relevanten Teile (PR-Output, Iteration über Review-Kommentare und CI, Rechteprüfung).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0{0,1,2,3,4}-*.md` und `WP13-*.md`. Besonders wichtig:
   - WP03: Gate, Secret-Scan vor Push, Push-Freigabe für den Default-Branch, gehärtetes Host-Git, Push nur an die konfigurierte URL.
   - WP04: Task-Branch `queue/<id>` im eigenen Repo pro Auftrag, Merge-Queue, Fix-Aufträge mit `carry_from`.

   Der PR-Modus muss darauf aufbauen und darf das Gate nicht umgehen: Kein Push eines Task-Branches ohne Gate-Prüfung, ein Folge-Auftrag aus PR-Feedback läuft auf demselben Branch.
4. Den relevanten Code: Runner (`backend/crates/runner`: git.rs, tasks.rs, Gate-Endpunkte), App (`runner.rs`, `runner_gate.rs`, `runner_merge.rs`, `worker.rs`, `models.rs`, `api/`, `crypto.rs`, SSRF-sicherer HTTP-Client aus WP13/WP02, Migrationen). Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`), die geteilten Wire-Typen und Texte liegen in `backend/crates/api`.

Harte Regeln:
- **Git-Host-Adapter** (GitHub, GitLab, Gitea/Forgejo) über REST, mit dem SSRF-sicheren Client der App. Keine echten externen Calls in Tests: Baue für jeden Host einen Fake-Server, der die relevanten Endpunkte nachbildet. API-Formate belegst du aus offizieller Doku, sonst markierst du sie als unverifiziert.
- **Sicherheit:** PR-Kommentare, Review-Texte, CI-Logs und PR-Beschreibungen sind nicht vertrauenswürdig (Prompt Injection). Daraus folgt:
  - Nur Kommentare von Nutzern mit Schreibrechten lösen etwas aus; die Rechte werden über die Host-API geprüft und nicht aus dem Kommentar geglaubt.
  - Der Inhalt geht klar als Daten markiert in den Prompt.
  - Der Agent approved und merged nie selbst.
  - Es gibt Obergrenzen für Folge-Aufträge pro PR (Loop-Schutz) und Dedupe pro Kommentar oder CI-Lauf.
  - Tokens sind verschlüsselt (crypto.rs) und erscheinen nie in Logs, Events, argv oder API-Antworten.
  - Mehrmandantenfähigkeit: Ein Nutzer sieht und steuert nur PRs seiner Projekte.
- **Polling:** Das Polling von CI und Kommentaren ist robust (Backoff, Rate-Limit-Header, ETag/If-None-Match wo möglich), übersteht Neustarts und blockiert den Scheduler nicht. Webhooks gibt es erst in WP07; sieh dafür eine saubere Schnittstelle vor.
- **Output-Modus pro Projekt** `commit` | `branch` | `pr`, mit Default wie bisher. Er muss mit Merge-Queue und Gate konsistent sein und dokumentiert, wer wann pusht.
- Kein Root auf dem Host. Kein Commit, kein Push durch dich, das macht der Koordinator. Lizenz-/ToS-Thema bleibt ausgeklammert.
- Platte: ca. 25 GB frei. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP05-plan.md` früh und alle Artefakte inkrementell auf Platte, weil der Container jederzeit neu starten kann.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP05-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP05-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP05-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP05-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch): Einrichtung pro Git-Host und welche Token-Rechte minimal nötig sind.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu:
- PR erstellen
- CI rot → Folge-Auftrag auf demselben Branch
- Kommentar eines Nutzers ohne Schreibrecht wird ignoriert
- Loop-Grenze
- Neustart während des Pollings

UI (PR-Link, Status, Checks, Output-Modus, Host-Zugang) folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
