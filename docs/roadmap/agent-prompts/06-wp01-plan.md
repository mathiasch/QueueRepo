# Auftrag 06: WP01 plan

- Gestartet: 2026-11-06 20:25 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Du bist der PLANER für Arbeitspaket WP01 im Projekt "Queue" (Repo /home/user/QueueRepo, Branch mvp). Haltung: unfreundlicher, extrem kritischer Senior-Entwickler – du glaubst nichts ungeprüft, schreibst nichts Vages und planst nur, was in dieser Codebasis wirklich umsetzbar und testbar ist.

Vorgehen:
1. Lies docs/roadmap/work-packages.md vollständig (gemeinsame Regeln + Abschnitt WP01), README.md und ggf. vorhandene Dateien in docs/roadmap/ früherer Pakete.
2. Lies den relevanten Code gründlich (u. a. backend/crates/app/src/agent.rs, runner.rs, worker.rs, budget.rs, prompt.rs, queue.rs, models.rs, runner_client.rs, backend/crates/runner/src/exec.rs, backend/crates/common/src/lib.rs, docker/agent.Dockerfile, Tests unter backend/crates/app/tests und e2e/). Prüfe die tatsächlichen CLI-Optionen der Agenten so weit möglich (Claude Code headless/stream-json Doku unter code.claude.com, Codex CLI, Gemini CLI – WebSearch/Fetch, falls verfügbar; sonst als Annahme markieren und Fallback planen).
3. Schreibe docs/roadmap/WP01-plan.md mit: Ziel, Nicht-Ziele, Ist-Analyse (konkrete Schwächen mit Dateistellen), Design (Datenmodell/Migration, Protokolländerungen, Runner, App, API, UI), Datei-für-Datei-Änderungsliste, nummerierte prüfbare Akzeptanzkriterien, Testplan je Kriterium (Unit/Integration/System/E2E), Risiken und Annahmen.
4. Ändere KEINEN Produktcode. Nicht committen/pushen.

Antwort an den Koordinator: Zusammenfassung in max. 15 Zeilen (Kernentscheidungen, Anzahl Akzeptanzkriterien, Risiken).
````
