# Auftrag 25: WP09 memory dedupe attachments

- Gestartet: 2026-11-12 05:10 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP09 („Memory, Wissen, Dedupe & Anhänge“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP09. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP09 relevanten Teile (AGENTS.md, strukturierter Zustand statt Freitext-Handoff, OnEmpty-Dedupe/Critic, Anhänge).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0{0,1,2,3,4,5,6,7,8}-*.md` und `WP13-*.md`. Besonders wichtig:
   - WP01: Handoff und `.queue/`-Dateien inkl. symlink-sicherer Queue-Datei-Ein-/Ausgabe.
   - WP03: Repo-Konfiguration der CLIs ist standardmäßig aus, Änderungen an Agent-Konfig (z. B. CLAUDE.md, AGENTS.md, GEMINI.md) brauchen Freigabe, geschützte Pfade. Die verwaltete AGENTS.md muss dazu passen und darf diesen Schutz nicht aushebeln.
   - WP04: Arbeitsbaum pro Auftrag.
   - WP06: Plan, Steering, Datenblöcke.
   - WP13: OnEmpty als Einzelschritt/Batch, Kosten.
4. Den relevanten Code: App (`runner.rs`, `prompt.rs`, OnEmpty-Logik, `api/`, `models.rs`, Migrationen), Runner (`backend/crates/runner`: Queue-Datei-I/O, tasks.rs), `backend/crates/api`. Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`).

Harte Regeln:
- **Anhänge:**
  - Upload mit Größen- und Anzahl-Limits, MIME-Sniffing statt Vertrauen in Content-Type, Allowlist von Typen.
  - Bilder serverseitig neu kodieren bzw. EXIF/GPS entfernen (Datenschutz).
  - Dateinamen sanitisieren, keine Path-Traversal, keine Symlinks beim Bereitstellen unter `.queue/attachments/` (gleiche Härtung wie Queue-Dateien).
  - Auslieferung mit `Content-Disposition: attachment`, `X-Content-Type-Options: nosniff`, ohne SVG-/HTML-Ausführung.
  - Rechteprüfung pro Projekt. Speicherplatz-Quota pro Nutzer, Löschen mit dem Auftrag bzw. Projekt.
  - Mobil per Kamera (`capture`), iPhone-tauglich.
  - Anhänge dürfen nicht in Commits landen; prüf das gegen Gate und Merge.
- **Wissen/AGENTS.md:**
  - Verwalteter Inhalt mit Versionierung. Agent-Vorschläge nur über Freigabe.
  - Die Brücke zu CLAUDE.md/GEMINI.md so, dass sie mit den WP03-Konfig-Abschaltungen zusammenspielt; dokumentier genau, was jede CLI liest.
  - Inhalte gehen als vertrauenswürdige Projektregeln nur dann in den Prompt, wenn ein Mensch sie freigegeben hat.
- **Strukturierter Projektzustand:**
  - Versioniertes JSON-Schema mit Validierung durch den Runner gegen den Repo-Zustand (z. B. referenzierte Dateien existieren, Status-Übergänge erlaubt).
  - Ungültiger Zustand des Agenten wird abgelehnt bzw. repariert, nicht blind übernommen.
  - Größenlimits.
- **OnEmpty-Dedupe:**
  - Ähnlichkeit gegen offene und erledigte Aufträge, deterministisch und ohne externe Embedding-API als Default, z. B. normalisierte Token-/Shingle-Jaccard oder MinHash. Optional über einen konfigurierten Connector; Kosten werden erfasst.
  - Critic-Schritt vor dem Vorschlag, Schwellwerte konfigurierbar, Begründung sichtbar.
- Kein Root auf dem Host. Sicherheitsmodell nicht aufweichen. Keine echten externen Calls in Tests. Lizenz-/ToS-Thema bleibt ausgeklammert. Keine Live-Calls an LLMs oder bezahlte APIs; falls unvermeidbar, Kosten im Bericht nennen.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP09 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass es ein WP09-Snapshot ist und der Parent = HEAD (der Koordinator kann WIP-Commits machen), dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: ca. 17 GB frei. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP09-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP09-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP09-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP09-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP09-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch).

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu:
- Upload-Limits und MIME-Fälschung
- EXIF entfernt
- Traversal/Symlink
- Anhang nicht im Commit
- AGENTS.md-Vorschlag mit Freigabe
- ungültiger Zustand abgelehnt
- Dedupe verhindert Duplikat
- Upload per Touch/Kamera-Input auf dem iPhone-Profil

UI (Wissen-Editor mit Versionen/Vorschlägen, Projektzustand-Ansicht, Anhänge im Composer und Drawer, Dedupe-Begründung) folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
