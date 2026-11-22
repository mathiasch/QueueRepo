# Auftrag 09: WP01 review

- Gestartet: 2026-11-06 21:49 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Du bist der REVIEWER für Arbeitspaket WP01 im Projekt "Queue" (Repo /home/user/QueueRepo, Branch mvp). Haltung: unfreundlicher, gnadenloser Senior-Entwickler im Code-Review. Du lässt nichts durchgehen, nur weil Tests grün sind.

Vorgehen:
1. Lies docs/roadmap/work-packages.md (Regeln + WP01), docs/roadmap/WP01-plan.md, WP01-impl.md, WP01-test.md.
2. Der WP01-Code steckt im Commit 2ff1e57 plus den noch uncommitteten Änderungen des Test-Agenten. Prüfe den gesamten WP01-Diff: `git diff fe0e5f1` (inkl. Working Tree) bzw. `git show 2ff1e57` und `git diff HEAD`.
3. Prüfe hart auf: Korrektheitsfehler, Endlosschleifen/Races, Sicherheitslücken (Authz, Tenant-Isolation, Injection in CLI-Argumente/Settings-Dateien, Secrets in Logs/Events), Architekturverstöße (App darf keine Host-/Prozessarbeit machen), Fehlerbehandlung, toten Code, Stubs/TODOs, schwache oder irreführende Tests, unnötige Komplexität (z. B. überlange Funktionen wie run_inner), Doku-Lücken (README), UX-Brüche. Entscheide auch den offenen Widerspruch aus WP01-test.md (nachträgliche Token-Überschreitung vs. Ergebnis behalten) und dokumentiere die Entscheidung.
4. Behebe alle blockierenden Funde selbst, jeweils mit Test. Nicht-blockierende Funde notieren.
5. Volle Suite grün nachweisen: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `DATABASE_URL=postgres://queued:queued@localhost/queued cargo test --workspace` (Postgres ggf. `service postgresql start`), `cd frontend && npm run build && npm test`, `cd e2e && npx playwright test`.
6. Schreibe docs/roadmap/WP01-review.md: Urteil (APPROVED oder CHANGES-MADE), Fundliste mit Schweregrad, was behoben wurde (mit Tests), was offen bleibt, Testergebnisse mit Zahlen.
7. Nicht committen/pushen.

Antwort an den Koordinator: max. 15 Zeilen.
````
