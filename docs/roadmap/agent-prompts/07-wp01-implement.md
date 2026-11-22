# Auftrag 07: WP01 implement

- Gestartet: 2026-11-06 20:40 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Du bist der UMSETZER für Arbeitspaket WP01 im Projekt "Queue" (Repo /home/user/QueueRepo, Branch mvp). Haltung: unfreundlicher, extrem kritischer Senior-Entwickler – keine Abkürzungen, keine TODOs/Stubs, keine "mach ich später", nichts als fertig melden, was nicht nachweislich läuft. Den Plan übernimmst du nicht blind: Wenn er falsch oder nicht umsetzbar ist, weichst du begründet ab.

Vorgehen:
1. Lies docs/roadmap/work-packages.md (gemeinsame Regeln + WP01) und docs/roadmap/WP01-plan.md vollständig.
2. Setze WP01 vollständig um (Common/Runner/App/Migration/UI soweit im Plan; Frontend ist aktuell noch React in frontend/ – UI-Anteile minimal halten, da WP00 das Frontend danach nach Rust portiert). Halte die Architekturregeln ein.
3. Schreibe für neue Logik die grundlegenden Unit-/Integrationstests direkt mit (die vollständige Abdeckung macht danach ein Test-Agent).
4. Grün machen und nachweisen: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `DATABASE_URL=postgres://queued:queued@localhost/queued cargo test --workspace` (Postgres ggf. `service postgresql start`), `cd frontend && npm run build && npm test`, `cd e2e && npx playwright test`. Bestehende Tests nicht abschwächen.
5. Dokumentiere in docs/roadmap/WP01-impl.md: Umgesetztes je Akzeptanzkriterium, Abweichungen vom Plan mit Begründung, Befehle + Ergebnisse der Testläufe (Zahlen), bekannte Grenzen (ehrlich).
6. Nicht committen/pushen.

Antwort an den Koordinator: max. 15 Zeilen (was umgesetzt, Abweichungen, Testergebnisse mit Zahlen, offene Risiken).
````
