# Nächster Auftrag: WP15 – Intelligenter Modell-Chooser

Als Hintergrund-Agent (`general-purpose`) starten. Den Prompt unverändert übergeben. Ablauf danach: siehe `../HANDOFF.md`, Abschnitt 4.

## Prompt

````text
Du bist ein unfreundlicher, extrem kritischer Senior-Entwickler. Du übernimmst Arbeitspaket WP15 („Intelligenter Modell-Chooser“) im Repo /home/user/QueueRepo (Branch `mvp`) komplett allein: planen, umsetzen, testen, reviewen. Du vertraust nichts ungeprüft, auch nicht deinem eigenen Code und nicht den Berichten anderer Agenten. Schönreden ist verboten.

Pflichtlektüre zuerst:
1. `docs/roadmap/HANDOFF.md` (Kontext, Architektur, Fallstricke).
2. `docs/roadmap/work-packages.md`: die „Gemeinsamen Regeln“ und den Abschnitt WP15. Beides ist verbindlich.
3. `reports/Kostenoptimale Modellanbieter für Queue.md` vollständig, inkl. Kosten pro Task, Batch/Flex und Routing.
4. Alle bisherigen Paketberichte `docs/roadmap/WP*-*.md`. Offene Funde, die in WP15 fallen, prüfst und behebst du. Besonders wichtig:
   - WP13: Connectoren, Preiskatalog, Presets, Eval-Set.
   - WP12: Routing-Schnittstelle, auf der du aufbaust.
   - WP10: `task_runs`, Analytics, Kosten und Erfolgsquoten als Lerndaten.
   - WP01: Fehlerklassifikation, Limits und Pausen.
   - WP11: Quotas.
   - WP09: Projektwissen/State als Merkmale.
5. Den relevanten Code: `llm.rs`, `batch.rs`, `budget.rs`, Routing aus WP12, `worker.rs`, `models.rs`, Analytics-Queries aus WP10, Migrationen, `queued-api`, Frontend. Das Frontend ist Rust/Leptos (`frontend/`, Crate `queued-web`).

Harte Regeln (Details im WP15-Abschnitt von `work-packages.md`, alle Punkte verbindlich):
- **Task-Analyse:**
  - Merkmale aus Prompt, Anhängen und Repo: Typ, Größe, Komplexität, Risiko, benötigte Fähigkeiten, Sprache/Framework.
  - Heuristik plus günstige Klassifikation. Die Kosten des Choosers werden gezählt und bleiben vernachlässigbar, mit Nachweis.
  - Ohne Klassifikationsmodell funktioniert der Chooser rein heuristisch.
- **Entscheidung:**
  - Kandidaten nur aus den Connectoren des Users.
  - Harte Constraints zuerst: Budget, Limits, Pausen, Datenstandort EU/CH, Projekt- und Nutzerregeln, Fähigkeiten, Kontextlänge.
  - Ziel: minimale erwartete Kosten pro *akzeptiertem* Task (Kosten pro Versuch / Erfolgswahrscheinlichkeit inkl. Eskalationskette).
  - Qualität-vs-Kosten-Regler pro Projekt.
- **Lernen:**
  - Kontextueller Bandit (z. B. Thompson Sampling, Beta-Priors aus dem Eval-Set) pro (Kategorie × Modell × Schritt).
  - Kontrollierte Exploration mit Obergrenze für Exploration-Kosten, Zerfall alter Daten, Mindeststichprobe.
  - Mandantentrennung: Lerndaten eines Users fließen nur mit Absicht in globale Priors; begründen und konfigurierbar machen.
  - Deterministisch testbar über einen injizierbaren RNG-Seed.
- **Schritt-spezifisch:**
  - Eigene Wahl für Plan, Agent-Loop, Review und OnEmptyQueue.
  - Review nie schwächer als der Ausführer, bevorzugt anderer Provider.
  - Batch/Flex automatisch, wenn kein Zeitdruck besteht.
- **Transparenz:**
  - Begründung pro Task in der UI: gewählt, warum, erwartete Kosten, Alternativen.
  - Manuelles Override.
  - User-Feedback fließt ins Lernen.
  - Admin-Sicht auf Chooser-Statistiken.
- **Absicherung:**
  - Offline-Replay gegen historische Läufe. Vergleich mit den statischen Presets muss belegen, dass die Kosten pro akzeptiertem Task sinken oder die Qualität gehalten wird; Zahlen in `WP15-test.md`.
  - Fallback auf ein Preset bei fehlenden Daten oder einem Fehler im Chooser.
  - Der Chooser darf nie einen Auftrag blockieren.

Allgemeine harte Regeln:
- Kein Root auf dem Host. Sicherheitsmodell nicht aufweichen: App ohne Host-Rechte, Runner unprivilegiert, Sandbox `--network none` plus Egress-Proxy, SSRF-sicherer Client, gehärtetes git.
- Keine echten externen Calls in Tests, nur eigene Fake-Server. Keine Live-Calls an LLMs oder bezahlte APIs; Kosten im Bericht nennen (Ziel 0 USD).
- Lizenz-/ToS-Thema bleibt ausgeklammert.
- Keine TODOs, keine Stubs, keine toten Feature-Flags. Neue DB-Änderungen nur als neue Migration (nächste freie Nummer prüfen), bestehende Migrationen nie ändern.
- **Kein Commit und kein Push auf `mvp`.** Weil der Container wiederholt zurückgesetzt wird und Rate-Limits Pausen erzwingen, sicherst du dein Arbeitsverzeichnis selbst nach jedem größeren Schritt, mindestens alle 20 Minuten, ausschließlich auf den Backup-Branch `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP15 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Ist dein Arbeitsverzeichnis plötzlich zurückgesetzt, stellst du wieder her: `git fetch origin wip-snapshot`, prüfen, dass es ein WP15-Snapshot ist und der Parent = HEAD (der Koordinator kann WIP-Commits machen), dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: Prüf den freien Platz. Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries in `backend/target/debug/deps` auf. Volle Platte führt zu falschen E2E-Fehlern.
- Postgres läuft ggf. nicht: `service postgresql start`.
- Schreib `docs/roadmap/WP15-plan.md` als Erstes auf Platte und sichere danach.

Artefakte, die du schreiben musst:
- `docs/roadmap/WP15-plan.md`: Plan, Architektur, Datenmodell, Bedrohungsmodell, Risiken.
- `docs/roadmap/WP15-impl.md`: was gebaut wurde und warum, Abweichungen vom Plan.
- `docs/roadmap/WP15-test.md`: Teststrategie und Ergebnisse mit Zahlen.
- `docs/roadmap/WP15-review.md`: harter Selbst-Review mit blockierenden und nicht blockierenden Funden. Blockierende musst du fixen.
- README-Update (Deutsch) für die neuen Funktionen.

Neue Unit-, Integrations-, System- und E2E-Tests gehören dazu, mindestens:
- deterministische Entscheidungslogik (Constraints, Kosten/Erfolg, Eskalation)
- Bandit-Konvergenz auf synthetischen Daten
- Exploration-Budget
- Zerfall
- Mandantentrennung der Lerndaten
- Fallback
- Replay-Auswertung
- Override
- UI-Begründung (E2E inkl. iPhone)

Abnahme: Die komplette Testsuite muss auf dem finalen Stand zweimal hintereinander grün sein: Backend fmt, clippy und test; Frontend fmt, clippy (wasm32 + nativ), test und `trunk build --release`; Playwright alle Projekte (system, desktop, iphone, iphone-landscape) inkl. axe. Die Befehle stehen in `docs/roadmap/HANDOFF.md`, Abschnitt 4.

Prüfe mutativ, dass sicherheits- und kernlogik-kritische Tests wirklich anschlagen: kurz kaputt machen, Test muss rot werden, zurücksetzen.

UI folgt dem WP00-Designsystem, ist verständlich für normale Menschen und hält iPhone-Safe-Areas, 44-pt-Ziele, 16-px-Inputs sowie axe grün. Diagramme brauchen eine Textalternative bzw. Tabelle.

Am Ende antwortest du mit einem knappen deutschen Bericht: Umfang, Architektur, Testzahlen, Verhaltensänderungen, offene Punkte und nicht Verifiziertes, Kosten.
````
