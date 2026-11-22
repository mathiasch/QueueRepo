# Auftrag 16: WP00 Rust GUI redesign

- Gestartet: 2026-11-07 19:57 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Du bist allein verantwortlich für Arbeitspaket WP00 im Projekt "Queue" (Repo /home/user/QueueRepo, Branch mvp): Planen, Umsetzen, Testen und Review in einem Durchgang. Haltung: unfreundlicher, extrem kritischer Senior-Entwickler und Designer – keine Abkürzungen, keine TODOs/Stubs, nichts als fertig melden, was nicht nachweislich läuft, und am Ende ein gnadenloser Review deiner eigenen Arbeit, als wäre sie von jemand anderem.

Auftrag (Details in docs/roadmap/work-packages.md, Abschnitt WP00 + gemeinsame Regeln – vollständig lesen): Das komplette Frontend (heute React/TS in frontend/) nach Rust portieren (Leptos CSR/WASM, Build mit Trunk) UND dabei neu gestalten: benutzerfreundlich und optisch ansprechend für normale Menschen, modernes Design erwünscht. Volle Funktionsparität inkl. allem, was WP01 ergänzt hat (siehe docs/roadmap/WP01-*.md). React/TS danach vollständig entfernen; Build/Test/Deploy (e2e/scripts/stack.sh, deploy/install.sh, Dockerfile, .github/workflows/ci.yml, README) umstellen.

Phasen und Artefakte (alles in docs/roadmap/):
1. PLAN → WP00-plan.md: Ist-Inventar aller Screens/Funktionen/API-Aufrufe des React-UI, Leptos-Architektur (Crate-Struktur, Router, State/Store, SSE, API-Client, geteilte DTOs mit dem Backend wo sinnvoll), Design-System (Tokens, Komponenten, Light/Dark, Icons), UX-Konzept (Onboarding, verständliche Sprache, Leerzustände, Fortschrittsanzeige, Erweitert-Bereiche, Tastaturkürzel), Accessibility (WCAG 2.2 AA), Teststrategie, nummerierte Akzeptanzkriterien.
2. UMSETZUNG → WP00-impl.md (Abweichungen begründet). Toolchain: `trunk` 0.22.1 und Target `wasm32-unknown-unknown` sind installiert; wasm-bindgen-cli ggf. per `cargo install wasm-bindgen-cli --version <exakt passend>`. Service Worker (sw.js) bleibt JS. Kein CDN, Icons als Inline-SVG.
3. TEST → WP00-test.md: Rust-Tests für Logik (ersetzen Vitest), alle Playwright-System- und E2E-Tests inkl. iPhone hoch/quer grün; Tests an neues Markup anpassen, aber fachlich mindestens gleich streng (Abläufe, Safe Areas, Touch ≥44pt, kein horizontales Scrollen, 16px-Inputs auf Touch) und um Accessibility-Checks ergänzen (z. B. axe-core per npm im e2e-Paket). Screenshots Desktop + iPhone, hell + dunkel, ablegen unter docs/roadmap/WP00-screenshots/ und im Bericht verlinken. Volle Suite zweimal: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` (Backend) plus clippy fürs Frontend-Crate, `DATABASE_URL=postgres://queued:queued@localhost/queued cargo test --workspace` (Postgres ggf. `service postgresql start`), Frontend-Tests, `cd e2e && npx playwright test`.
4. REVIEW → WP00-review.md: Urteil, Fundliste (blockierend/nicht blockierend), behobene Funde, finale Testergebnisse mit Zahlen. Schau dir deine Screenshots kritisch an (Layout, Kontrast, Abstände, Verständlichkeit) und korrigiere Schwächen.

Regeln: Nicht committen/pushen (macht der Koordinator). Backend-Architektur nicht aufweichen. Wenn etwas technisch nicht geht (z. B. Netzwerk blockiert einen Download), dokumentiere es ehrlich und finde einen sauberen Weg.

Antwort an den Koordinator: max. 20 Zeilen (Architektur, Design-Highlights, Testergebnisse mit Zahlen, Pfade zu Screenshots, offene Punkte).
````
