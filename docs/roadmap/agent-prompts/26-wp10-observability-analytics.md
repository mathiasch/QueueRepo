# Auftrag 26: WP10 observability analytics

- Gestartet: 2026-11-17 06:05 UTC
- Agent-Typ: `general-purpose`, Hintergrund: True

## Prompt (wörtlich)

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP10 („Observability, Analytics & Benachrichtigungen“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP10. Beides ist verbindlich.
2. `reports/Funktionsumfang agentischer Coding Queues.md`, die für WP10 relevanten Teile (Health, Metriken, OTel GenAI, Analytics, Benachrichtigungen).
3. Alle bisherigen Paketberichte `docs/roadmap/WP0{0..9}-*.md` und `WP13-*.md`. Besonders wichtig:
   - WP13: Kosten in Geld pro Lauf/Schritt/Auftrag/Projekt, Cache-Tokens.
   - WP01: Limits/Pausen.
   - Bestehende Kanäle: In-App, Web Push, ntfy (`notify.rs`, `push.rs`).
   - WP08: Rückmeldungen nach außen.
   - WP07: Ausgehende Webhooks waren an WP10 verwiesen; prüf, ob sie hierher passen, und setz sie um oder ordne sie begründet ein.
   - Offener Fund WP09: `GET /api/tasks` zeigt keine Aufträge im Status `merging`. Prüf, ob das für Analytics/Status relevant ist, und behebe ihn, wenn er hierher gehört.
4. Den relevanten Code: App (`main.rs`, `lib.rs`, `api/`, `notify.rs`, `push.rs`, `budget.rs`, Kostenrechnung aus WP13, `worker.rs`, `models.rs`, Migrationen), Runner (`backend/crates/runner/src/lib.rs`, `main.rs`), `backend/crates/api`, `deploy/` (systemd, compose). Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`).

Harte Regeln:
- **Endpunkte:**
  - `/healthz` (Liveness, ohne Abhängigkeiten) und `/readyz` (DB + Runner erreichbar, mit Timeout und ohne Interna/Fehlertexte nach außen).
  - `/metrics` im Prometheus-Format für App und Runner, mit niedriger Kardinalität: keine User-IDs, Task-IDs oder Titel als Labels; Provider/Modell/Status sind ok. Zugriff ist geschützt (eigenes Bearer-Token oder nur auf Loopback/Bind-Adresse konfigurierbar), nie öffentlich ohne Absicht. Der Runner-Endpunkt darf das bestehende Runner-Auth-Modell nicht schwächen.
- **OpenTelemetry:**
  - Optional per Env (OTLP). Ohne Konfiguration null Overhead und keine Netzwerkversuche.
  - Semantik nach den OTel GenAI-Konventionen (`invoke_agent`, `execute_tool`, `gen_ai.*`-Attribute). Den aktuellen Stand der Konventionen belegst du aus der offiziellen Spezifikation, sonst markierst du ihn als unverifiziert.
  - Niemals Prompts, Code, Secrets oder Nutzerinhalte in Spans, Standard aus. Ein Opt-in für Inhalte ist höchstens für den Betreiber und mit Warnung erlaubt.
  - Dependency-Größe beachten. Die Crate-Versionen müssen untereinander kompatibel sein.
- **Analytics:**
  - First-Pass-Erfolgsquote, Kosten/Tokens pro Auftrag/Projekt/Provider, Cache-Hit-Ratio, Dauer bis erledigt, Pausenzeiten durch Limits, Session-Insights pro Auftrag.
  - Strikt mandantengetrennt: Ein Nutzer sieht nur seine Projekte bzw. geteilte Projekte gemäß Rolle.
  - Effiziente Queries (Indizes, Zeitfenster, Aggregation in SQL), getestet mit realistischen Datenmengen auf Performance-Regressionen.
- **Benachrichtigungen:**
  - Pro Ereignistyp und Kanal konfigurierbar, Ruhezeiten mit Zeitzone, tägliche Zusammenfassung zur gewählten Uhrzeit.
  - Robust gegen Neustart und mehrere Instanzen (keine doppelte Zusammenfassung).
  - Kritische Ereignisse (z. B. Freigabe nötig, blockiert) dürfen optional Ruhezeiten durchbrechen.
- Kein Root auf dem Host. Sicherheitsmodell nicht aufweichen. Keine echten externen Calls in Tests (Fake-OTLP-Collector, Fake-ntfy). Lizenz-/ToS-Thema bleibt ausgeklammert. Keine Live-Calls an LLMs oder bezahlte APIs; Kosten im Bericht nennen.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird und Rate-Limits Pausen erzwingen, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP10 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass es ein WP10-Snapshot ist und der Parent = HEAD (der Koordinator kann WIP-Commits machen), dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: Prüf den freien Platz. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP10-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP10-plan.md`: Plan, Architektur, Datenmodell, Risiken.
- `docs/roadmap/WP10-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP10-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP10-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch): Monitoring-Einrichtung, Metrik-Liste, OTel-Konfiguration, Benachrichtigungen.

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ) und test; Playwright alle Projekte inkl. a11y. Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu:
- `/readyz` bei DB-/Runner-Ausfall
- `/metrics`-Schutz und Format
- keine Inhalte in Spans
- Mandantentrennung der Analytics
- Ruhezeiten und Zusammenfassung ohne Doppelversand über Neustart

UI (Analytics-Dashboard mit verständlichen Diagrammen, auch auf dem iPhone, Benachrichtigungs-Einstellungen pro Ereignis/Kanal) folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas sowie axe grün. Diagramme brauchen eine Textalternative bzw. Tabelle für Screenreader.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, offene Punkte und nicht Verifiziertes.
````
