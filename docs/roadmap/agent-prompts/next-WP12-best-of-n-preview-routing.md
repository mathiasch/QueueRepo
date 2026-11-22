# Nächster Auftrag: WP12 – Best-of-N, Preview & Modell-Routing

Als Hintergrund-Agent (`general-purpose`) starten. Den Prompt unverändert übergeben. Ablauf danach: siehe `../HANDOFF.md`, Abschnitt 4.

## Prompt

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP12 („Best-of-N, Preview & Modell-Routing“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code und nicht den Berichten anderer Agenten. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/HANDOFF.md` (Kontext, Architektur, Fallstricke).
2. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP12. Beides ist verbindlich.
3. `reports/Funktionsumfang agentischer Coding Queues.md` (Best-of-N, Preview, Routing) und `reports/Kostenoptimale Modellanbieter für Queue.md` (Planer/Ausführer-Split, Routing).
4. Alle bisherigen Paketberichte `docs/roadmap/WP*-*.md`. Offene Funde, die in WP12 fallen, prüfst und behebst du. Besonders wichtig:
   - WP13: Connectoren, Preiskatalog, Routing-Presets, Kosten pro Schritt.
   - WP04: Per-Task-Repos und Merge-Queue; Best-of-N braucht N isolierte Repos, Auswahl → Merge über die Merge-Queue.
   - WP02: Environments, Setup-Phase, Egress; der Dev-Server läuft in der Sandbox.
   - WP03: Gate/Secret-Scan muss für jeden Versuch gelten.
   - WP05: PR-Modus.
   - WP06: Plan-Modus.
   - WP10: Analytics/Metriken; Best-of-N-Versuche sauber zählen.
   - WP11: Quotas pro Nutzer, Runner-Flotte; Kapazität bei N parallelen Versuchen.
5. Den relevanten Code: `worker.rs`, `runner.rs`, `runner_plan.rs`, `batch.rs`, `llm.rs`, `budget.rs`, `agent/`, `api/`, Migrationen, Runner-Crate (Sandbox, Proxy), `queued-api`, `e2e/`. Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`).

Harte Regeln:
- **Best-of-N:**
  - Ein Auftrag läuft als N parallele Versuche: gleicher Provider oder verschiedene Provider/Modelle, N vom User wählbar, begrenzt durch Quota, Budget und Parallelität.
  - Jeder Versuch läuft in eigener Sandbox mit eigenem Repo; kein Versuch sieht einen anderen.
  - Vergleichsansicht: Diffs, Verify-Ergebnis, Review-Urteil, Kosten und Dauer pro Versuch.
  - Auswahl durch den User oder optional automatisch per Review-Agent. Der Review-Agent ist nie schwächer als die Ausführer und bevorzugt einen anderen Provider (Self-Preference-Bias).
  - Danach Merge über die bestehende Merge-Queue bzw. den PR-Modus. Verworfene Versuche werden sauber aufgeräumt (Repos, Container, Branches).
  - Kosten-Vorschau vor dem Start: N × erwartete Kosten.
  - Budget-Abbruch stoppt alle Versuche.
- **Preview:**
  - Dev-Server im Sandbox-Container. Projekt-Einstellung: Kommando, Port, Ready-Check, Timeout.
  - Kein Port-Binding nach außen ohne Absicht. Die Vorschau geht über einen authentifizierten Proxy der App, nur für Projektmitglieder. Strikte CSP/Sandbox-iframe, damit Vorschau-Inhalte kein XSS auf die App-Origin bekommen; eigene Origin bzw. Pfad mit Cookie-Isolation begründen.
  - Screenshots per Headless-Browser (Chromium vorhanden) nach UI-Änderungen, im Auftrag angezeigt, auch mobil.
  - Ressourcen- und Zeitlimits; Prozess und Container werden immer beendet.
- **Modell-Routing:**
  - Regeln pro Projekt, z. B. Plan/Review mit anderem Modell als die Umsetzung, günstiges Modell für kleine Aufträge, Eskalation auf ein stärkeres Modell nach einem Fehlversuch.
  - Baut auf den WP13-Presets auf und ersetzt sie nicht doppelt.
  - Deterministisch und testbar. Erklärung, welche Regel gegriffen hat, sichtbar im Auftrag.
  - WP15 (Chooser) baut darauf auf, daher eine saubere Schnittstelle „Routing-Entscheidung“ schaffen.

Allgemeine harte Regeln:
- Kein Root auf dem Host. Sicherheitsmodell nicht aufweichen: App ohne Host-Rechte, Runner unprivilegiert, Sandbox `--network none` plus Egress-Proxy, SSRF-sicherer Client, gehärtetes git.
- Keine echten externen Calls in Tests, nur eigene Fake-Server. Keine Live-Calls an LLMs oder bezahlte APIs; Kosten im Bericht nennen (Ziel 0 USD).
- Lizenz-/ToS-Thema bleibt ausgeklammert.
- Keine TODOs, keine Stubs, keine toten Feature-Flags. Neue DB-Änderungen nur als neue Migration (nächste freie Nummer prüfen), bestehende Migrationen nie ändern.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird und Rate-Limits Pausen erzwingen, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP12 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass es ein WP12-Snapshot ist und der Parent = HEAD (der Koordinator kann WIP-Commits machen), dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: Prüf den freien Platz. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries in `backend/target/debug/deps` auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP12-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP12-plan.md`: Plan, Architektur, Datenmodell, Bedrohungsmodell, Risiken.
- `docs/roadmap/WP12-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP12-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP12-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch) für die neuen Funktionen.

Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu, mindestens:
- Isolation der N Versuche (kein gegenseitiger Dateizugriff)
- Auswahl → Merge, Verwerfen → Aufräumen
- Budget-Abbruch über alle Versuche
- automatische Auswahl mit Fake-Review
- Preview-Proxy nur für Mitglieder, CSP/Origin-Isolation, Prozess-Cleanup bei Timeout
- Screenshot-Erzeugung
- Routing-Regeln inkl. Eskalation
- E2E: Best-of-N-Vergleichsansicht und Auswahl, auch iPhone

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ), test und `trunk build --release`; Playwright alle Projekte (system, desktop, iphone, iphone-landscape) inkl. axe. Die Befehle stehen in `docs/roadmap/HANDOFF.md`, Abschnitt 4.

Prüfe mutativ, dass sicherheits- und kernlogik-kritische Tests wirklich anschlagen: kurz kaputt machen, Test muss rot werden, zurücksetzen.

UI folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas, 44-pt-Ziele, 16-px-Inputs sowie axe grün. Diagramme brauchen eine Textalternative bzw. Tabelle.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, Verhaltensänderungen, offene Punkte und nicht Verifiziertes, Kosten.
````
