# Auftrag 20: WP04 worktrees merge queue

- Gestartet: 2026-11-09 08:02 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP04 („Branch/Worktree pro Task, Parallelität im Repo, Merge-Queue“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP04. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP04 relevanten Teile (Worktrees, parallele Agenten, Merge-Queue, Konflikte).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0{0,1,2,3}-*.md` und `WP13-*.md`. Besonders wichtig ist WP03:
   - die Git-Härtung im Runner (Config-Overrides, Allowlist für `.git/config`, Symlink-Checks, Push nur an die konfigurierte URL)
   - das Gate mit „held“-Zuständen unter `refs/queue/held/<task>`, 3-Wege-Merge bei Freigabe, Push-Freigabe für den Default-Branch
   - die Setup-Baselines

   Worktrees und Merge-Queue müssen sich sauber darin einfügen; sie dürfen das Gate nicht umgehen. Verify vor dem Merge heißt: Das Gate prüft erneut, wo nötig. Beachte den WP03-Fund, dass gehaltene Secrets für andere Aufträge im selben Projekt aus `.git` lesbar sind. Prüfe, ob Worktrees das verschlimmern oder verbessern, und dokumentiere es.
4. Den relevanten Code: Runner (`backend/crates/runner`: git.rs, exec.rs, lib.rs, Gate-Endpunkte), `backend/crates/common`, App (`runner.rs`, `runner_gate.rs`, `worker.rs`/Scheduler, `queue.rs`, `models.rs`, `api/`, Migrationen). Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`), die geteilten Wire-Typen und Texte liegen in `backend/crates/api`.

Harte Regeln:
- Kein Root auf dem Host. Das Sicherheitsmodell von Runner, Egress (WP02) und Gate (WP03) darf nicht aufgeweicht werden. Jeder Worktree ist ein vom Agenten kontrollierter Pfad. Prüfe deshalb speziell:
  - `.git`-Datei bzw. gitdir-Verweise im Worktree, Symlinks und `commondir`
  - Hooks und Config im gemeinsamen Repo
  - ob ein Agent in Worktree A den Worktree B oder die gemeinsame Objektdatenbank manipulieren kann

  Container-Mounts: Ein Agent darf nur seinen eigenen Worktree sehen. Die Härtung gehört in den Code und in Tests.
- Scheduler: Parallelität pro Projekt (Default 1) mit `FOR UPDATE SKIP LOCKED`, ohne Races. Die bestehende globale Parallelität und die Provider-Sperren gelten weiter. Schreib Nebenläufigkeitstests.
- Keine echten externen Calls in Tests. Lizenz-/ToS-Thema bleibt ausgeklammert.
- Kein Commit, kein Push. Das macht der Koordinator.
- Plattenplatz ist knapp (ca. 13 GB frei). Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Keine großen Docker-Builds. Volle Platte führt zu falschen E2E-Fehlern („Target crashed“).
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP04-plan.md` früh und alle Artefakte inkrementell auf Platte, weil der Container jederzeit neu starten kann.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP04-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP04-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP04-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP04-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch), wo es Nutzer oder Betrieb betrifft.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu, inklusive paralleler Aufträge im selben Projekt, Merge-Konflikt mit Fix-Auftrag und Diff/Revert gegen die Merge-Base. UI-Erweiterungen (Parallelität pro Projekt, Merge-Queue-Status, Branch pro Auftrag, Konflikte) folgen dem WP00-Designsystem, sind verständlich für normale Menschen und halten iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
