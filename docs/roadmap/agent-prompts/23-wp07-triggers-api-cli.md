# Auftrag 23: WP07 triggers API CLI

- Gestartet: 2026-11-11 05:44 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP07 („Trigger, API & CLI“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP07. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP07 relevanten Teile (Schedules, API-Tokens, Webhooks, CLI).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0{0,1,2,3,4,5,6}-*.md` und `WP13-*.md`. Besonders wichtig:
   - WP05 hat eine Webhook-Schnittstelle vorbereitet (`pr::poke` löst nur eine sofortige Abfrage aus, vertraut dem Payload nie). GitHub-Events müssen darauf aufbauen und dürfen das Polling-Modell nicht untergraben.
   - WP06: Steering/„Weiter mit …“ und Plan-Modus sind auch über API und CLI sinnvoll.
   - Rollen und Rechte sowie Mandantentrennung wie in den bestehenden APIs.
4. Den relevanten Code: App (`api/`, `auth.rs`, `crypto.rs`, `worker.rs`, `queue.rs`, `models.rs`, `pr*.rs`, `steer.rs`, `plan.rs`, Migrationen), `backend/crates/api` (geteilte Wire-Typen und Texte, ideal auch für `queuectl`). Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`).

Harte Regeln:
- **API-Tokens:**
  - Gehasht speichern (SHA-256 eines Tokens mit genug Entropie), Präfix für Secret-Scanner erkennbar (z. B. `qk_…`; den WP03-Scanner um die Regel erweitern).
  - Scopes sind fein und werden pro Endpunkt durchgesetzt.
  - Ablaufdatum, Widerruf, `last_used`.
  - Token-Auth funktioniert nie für Konto-/Token-Verwaltung selbst (keine Eskalation per Token).
  - CSRF-Schutz der Cookie-Session bleibt unberührt, Token-Requests brauchen ihn nicht.
- **Webhooks:**
  - HMAC-SHA256 mit konstantem Vergleich, Timestamp-/Replay-Schutz (Delivery-ID-Dedupe), Body-Limit.
  - Payload ist untrusted und geht nur als markierter Datenblock (Zufallsgrenze wie in WP05) in den Prompt.
  - Rate-Limit pro Trigger.
  - GitHub-Events (push/issue/PR/check failure) mit Filtern, Signatur `X-Hub-Signature-256`.
  - Eigene Felder nur mit offizieller Doku als Beleg, sonst als unverifiziert markieren.
- **Zeitpläne:**
  - Cron mit min. stündlich und Zeitzone, einmalige Ausführung.
  - Robust gegen Neustart (keine doppelten Ausführungen, verpasste Läufe definiert behandeln), mehrinstanzfähig über DB-Lock.
  - Vorlagen für Aufträge.
- **`queuectl`:** Rust-Binary im Backend-Workspace mit add/list/status/watch/cancel (+ sinnvoll: steer). Konfiguration über Datei/Env, Token nie in argv, Datei mit 0600 prüfen. `watch` über SSE oder Polling.
- Kein Root auf dem Host. Sicherheitsmodell nicht aufweichen. Keine echten externen Calls in Tests. Lizenz-/ToS-Thema bleibt ausgeklammert. Keine Live-Calls an echte LLM-CLIs oder bezahlte APIs ohne Not; falls doch, Kosten im Bericht nennen.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20–30 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP07 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass der Parent = HEAD ist und es ein WP07-Snapshot ist, dann `git checkout origin/wip-snapshot -- . && git reset -q`. HEAD kann sich verschieben, wenn der Koordinator WIP-Commits macht; prüf das.
- Platte: ca. 17 GB frei. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP07-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP07-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP07-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP07-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP07-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch): API-Doku, Webhook-Einrichtung inkl. GitHub, `queuectl`-Nutzung.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu:
- Scope-Verletzung → 403
- abgelaufenes/widerrufenes Token
- falsche Signatur, Replay
- Cron ohne Doppelauslösung über Neustart
- GitHub-Event-Filter
- `queuectl` end-to-end gegen den echten Stack

UI (Tokens verwalten mit einmaliger Anzeige, Zeitpläne, Vorlagen, Webhooks inkl. Secret einmalig anzeigen, Auslöse-Historie) folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
