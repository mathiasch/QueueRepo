# Auftrag 22: WP06 plan mode steering

- Gestartet: 2026-11-10 19:20 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP06 („Plan-Modus, Steering & Inline-Kommentare“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP06. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP06 relevanten Teile (Plan-Modus, Steering/Follow-ups, Session-Resume, Inline-Review-Kommentare).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0{0,1,2,3,4,5}-*.md` und `WP13-*.md`. Plan-Modus, Steering und Inline-Kommentare müssen sich in das Bestehende einfügen:
   - WP01: CLI-Vertrag, Session-IDs, Stall-Erkennung
   - WP13: Einzelschritt „plan“ über direkte API oder Batch, Routing-Presets pro Schritt
   - WP03: Gate, held-Zustände
   - WP04: Task-Branch, Merge-Queue, `carry_from`
   - WP05: PR-Folge-Aufträge, Prompt-Datenblöcke mit zufälliger Grenze
4. Den relevanten Code: App (`runner.rs`, `agent/` inkl. Session-Handling je CLI, `prompt.rs`, `runner_gate.rs`, `runner_merge.rs`, `worker.rs`, `queue.rs`, `models.rs`, `api/`, Migrationen), Runner (`backend/crates/runner`). Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`), die geteilten Wire-Typen und Texte liegen in `backend/crates/api`.

Harte Regeln:
- **Plan-Modus:** Der Agent darf dabei nachweislich nichts ändern. Erzwinge das technisch, nicht nur per Prompt: read-only Mount bzw. Verwerfen und Prüfen jeder Änderung, plus die Plan-Flags der CLIs, sofern belegt. Was je CLI wirklich existiert (Claude Code `--permission-mode plan`, Codex, Gemini, opencode), belegst du an den installierten Binaries bzw. am Quellcode; sonst markierst du es als unverifiziert. Heruntergeladene Binaries nicht ausführen, wenn der Modus das verbietet. Genehmigen, Ändern und Verwerfen sind möglich, Auto-Approve nach Timeout optional und standardmäßig aus.
- **Steering:** Nachrichten an laufende Aufträge (in der Queue zurückziehbar bis zur Zustellung) und „Weiter mit …“ für erledigte Aufträge. Die Zustellung läuft per Session-Resume, wo die CLI das kann, sonst als Fortsetzungs-Auftrag mit Handoff. Mehrmandantenfähigkeit: Steuern darf nur, wer am Projekt berechtigt ist. Rollen und Rechte prüfst du wie in den bestehenden APIs. Steering-Text und Inline-Kommentare sind Nutzereingaben; trotzdem gelten Längenlimits, und sie gehen sauber getrennt in den Prompt.
- **Inline-Kommentare im Diff** (Datei/Zeile, auch mehrzeilig) werden gesammelt und als ein Follow-up gesendet. Sie bleiben gültig bzw. werden als veraltet markiert, wenn sich der Diff ändert.
- Kein Root auf dem Host. Das Sicherheitsmodell von Runner, Egress, Gate und Branches darf nicht aufgeweicht werden. Keine echten externen Calls in Tests. Lizenz-/ToS-Thema bleibt ausgeklammert.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20–30 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP06 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich leer oder zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass der Parent = HEAD ist und es ein WP06-Snapshot ist, dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: ca. 22 GB frei. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP06-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP06-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP06-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP06-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP06-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch).

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu (Mock-Provider um Plan- und Resume-Verhalten erweitern):
- Plan-Modus ändert nichts, Genehmigen → Umsetzung mit Plan als Kontext
- Plan verwerfen
- Steering-Nachricht zurückziehen
- Zustellung an einen laufenden Auftrag
- „Weiter mit …“
- Inline-Kommentare inkl. veraltet

UI (Plan-Ansicht mit Genehmigen/Ändern/Verwerfen, Nachrichtenfeld im Drawer, Kommentieren im Diff auch per Touch auf dem iPhone) folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
