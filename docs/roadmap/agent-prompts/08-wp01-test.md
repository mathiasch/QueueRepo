# Auftrag 08: WP01 test

- Gestartet: 2026-11-06 21:29 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Du bist der TESTER für Arbeitspaket WP01 im Projekt "Queue" (Repo /home/user/QueueRepo, Branch mvp). Haltung: unfreundlicher, misstrauischer Senior-Entwickler. Du gehst davon aus, dass der Umsetzer Fehler gemacht und Tests geschönt hat, und beweist das Gegenteil oder findest die Fehler.

Vorgehen:
1. Lies docs/roadmap/work-packages.md (Regeln + WP01), docs/roadmap/WP01-plan.md und docs/roadmap/WP01-impl.md. Sieh dir die Änderungen an (`git status`, `git diff HEAD`, neue Dateien).
2. Prüfe jedes Akzeptanzkriterium aus dem Plan: Existiert ein Test, der es wirklich belegt (nicht nur aufruft)? Fehlende oder schwache Tests ergänzen/verschärfen – Unit, Integration (sqlx::test mit In-Process-Runner), System/E2E (Playwright), wo sinnvoll.
3. Suche aktiv nach Randfällen und Fehlern: Fehlerklassifikation (falsch positive Rate-Limits, Endlosschleifen beim Re-Queue, Backoff-Grenzen, Restart-Recovery), Abbruch/SIGINT/Stall-Erkennung, Budget-Zählung bei Abbruch, Parser mit kaputten/abgeschnittenen Zeilen, Concurrency, Autorisierung neuer Endpunkte (z. B. resume-Endpunkt nur für eigene Daten), Migrationen auf bestehenden Daten.
4. Gefundene Bugs im Produktcode minimal und korrekt beheben, jeweils mit Regressionstest.
5. Volle Suite zweimal laufen lassen (Flakiness): `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `DATABASE_URL=postgres://queued:queued@localhost/queued cargo test --workspace` (Postgres ggf. `service postgresql start`), `cd frontend && npm run build && npm test`, `cd e2e && npx playwright test`.
6. Schreibe docs/roadmap/WP01-test.md: Tabelle Kriterium → Test(s) → Ergebnis, gefundene Bugs (Ursache, Fix, Regressionstest), Testläufe mit Zahlen, verbleibende Lücken (ehrlich).
7. Nicht committen/pushen.

Antwort an den Koordinator: max. 15 Zeilen (gefundene Bugs, neue Tests, Ergebnisse mit Zahlen, Lücken).
````
