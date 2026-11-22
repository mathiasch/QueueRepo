# Auftrag 19: WP03 gate hardening

- Gestartet: 2026-11-08 11:54 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP03 („Gate- und Supply-Chain-Härtung“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP03. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP03 relevanten Teile (Secret-Scanning, untrusted Repo-Konfiguration, Reward-Hacking, Review-Gates, Push-Schutz).
3. Alle bisherigen Paketberichte: `docs/roadmap/WP01-*.md`, `WP00-*.md`, `WP13-*.md`, `WP02-*.md`. An WP03 verwiesene oder thematisch passende offene Funde musst du erledigen bzw. begründet einordnen, insbesondere:
   - WP01: Ein Fehler des Review-Providers markiert den Task ungeprüft als „done“.
   - WP13: opencode liest `opencode.json` aus dem Repo.
   - WP02: Code aus dem Repo, den das Setup ausführt, sieht Secrets; das Repo kann von Agenten für das nächste Setup präpariert werden. Prüfe, was WP03 hier realistisch härten kann, z. B. Freigabe bei Änderungen an Setup-relevanten Dateien.
4. Den relevanten Code: App (`runner.rs`, `agent/`, `prompt.rs`, `models.rs`, `api/`, Migrationen), Runner (`backend/crates/runner`: git.rs, exec.rs), `backend/crates/common`, `docker/agent.Dockerfile`. Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`), die geteilten Wire-Typen liegen in `backend/crates/api`.

Harte Regeln:
- Kein Root auf dem Host. Das Sicherheitsmodell von Runner und Egress (WP02) darf nicht aufgeweicht werden, nur gehärtet.
- Secret-Scanner: keine echten Secrets in Fixtures. Erzeuge Testwerte zur Laufzeit oder nimm offensichtlich falsche, die trotzdem dem Muster entsprechen. Treffer nie im Klartext in Logs, Events oder API-Antworten (maskieren).
- Prüfe für jede CLI (Claude Code, Codex, Gemini, opencode) anhand der installierten Binaries bzw. des vorhandenen Quellcodes, mit welchen Flags oder Settings sich Repo-Konfiguration (Hooks, MCP, Settings) abschalten lässt. Was du nicht belegen kannst, markierst du als unverifiziert, statt es zu erfinden. Heruntergeladene Binaries nicht ausführen, wenn der Modus das verbietet.
- Keine echten externen Calls in Tests. Lizenz-/ToS-Thema bleibt ausgeklammert.
- Kein Commit, kein Push. Das macht der Koordinator.
- Plattenplatz: ca. 17 GB frei. Räum auf, wenn es eng wird (`frontend/target` nach dem Testen). Keine großen Docker-Builds.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP03-plan.md` früh auf Platte, weil der Container jederzeit neu starten kann.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP03-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP03-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP03-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP03-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch), wo es Nutzer oder Betrieb betrifft.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests für die neuen Funktionen gehören dazu. UI-Erweiterungen (Freigabe-Dialoge, Schutzeinstellungen pro Projekt, Anzeige von Scanner-Treffern) folgen dem WP00-Designsystem, sind verständlich für normale Menschen und halten iPhone-Safe-Areas sowie axe grün.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
