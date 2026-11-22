# Auftrag 18: WP02 environments egress

- Gestartet: 2026-11-08 10:15 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP02 („Environments, Setup-Phase & Egress-Kontrolle“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP02. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP02 relevanten Teile (Environments, Setup-Skripte, Netzwerk-/Egress-Policies, Secrets, Caches).
3. `docs/roadmap/WP01-*.md`, `WP00-*.md` und `WP13-*.md` für den aktuellen Stand und die offenen Funde. WP13 hat Egress-Themen ausdrücklich an WP02 verwiesen; dazu gehört auch die DNS-Rebinding-Lücke im SSRF-Schutz (`WP13-review.md`), wenn sie thematisch passt.
4. Den relevanten Code: Runner (`backend/crates/runner`: exec.rs, git.rs, lib.rs), `backend/crates/common`, App (`runner.rs`, `runner_client.rs`, `models.rs`, `api/projects.rs`, Migrationen), `docker/agent.Dockerfile`, `deploy/`. Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`), die geteilten Wire-Typen liegen in `backend/crates/api`.

Harte Regeln:
- Kein Root auf dem Host. Der Runner bleibt unprivilegiert, Container rootless; das bestehende Sicherheitsmodell (cap-drop ALL, no-new-privileges, Limits, Secrets nie in argv) darf nicht aufgeweicht werden, nur gehärtet. Egress-Durchsetzung muss ohne Host-Root funktionieren (z. B. Podman-Netzwerk ohne Egress plus App-/Runner-seitiger Allowlist-Proxy o. Ä.). Begründe die Wahl und dokumentiere die Grenzen ehrlich. Wo die Engine `none` (Dev) nichts durchsetzen kann, muss das klar sichtbar sein und darf nicht als „geschützt“ gelten.
- Keine echten externen Calls in Tests. Secrets nie in Logs oder API-Antworten.
- Lizenz-/ToS-Thema bleibt ausgeklammert.
- Kein Commit, kein Push. Das macht der Koordinator.
- Plattenplatz ist knapp (ca. 14 GB frei, `backend/target` 14 GB). Räum auf, wenn es eng wird (`frontend/target` nach dem Testen; notfalls `cargo clean` für einzelne Pakete). Keine großen Docker-Builds.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP02-plan.md` früh auf Platte, weil der Container jederzeit neu starten kann.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP02-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP02-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP02-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP02-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch), wo es den Betrieb betrifft.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests für die neuen Funktionen gehören dazu. UI-Erweiterungen folgen dem WP00-Designsystem, sind verständlich für normale Menschen und halten iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
