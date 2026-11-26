# WP15 – Umsetzung: Intelligenter Modell-Chooser

Basis: `mvp` @ `4f54b81` (WP12), Zwischenstand vom Koordinator als WIP-Commit `253123b` gesichert.
Plan: `WP15-plan.md`. Hier steht, was gebaut wurde, warum, und wo vom Plan abgewichen wurde.

## 1. Datenmodell (Migration `0016_model_chooser.sql`)

- `chooser_settings` (pro Projekt: an/aus, Regler 0–100, Explorationsbudget USD/30 Tage, Klassifikation, Sperrliste ≤ 50).
- `chooser_user_prefs` (pro Person: Lerndaten teilen, eigene Sperrliste).
- `chooser_observations` (Lerndaten: User, Projekt, Auftrag, Schritt, Kategorie, Preis-ID, Familie, Größe, Erfolg 0..1, Gewicht, Kosten, Quelle `run|feedback|revert|eval`; `UNIQUE(task_id, step, source)` ⇒ die jüngste Rückmeldung ersetzt die alte, kein Doppelzählen bei Wiederholungen).
- `chooser_decisions` (pro Auftrag: Quelle `chooser|fallback|override`, Exploration + Kosten, erwartete Kosten, Klassifikationskosten, Fallback-Grund) – Basis für Explorationsbudget und Admin-Statistik.
- `instance_settings` + `chooser_pooling`, `chooser_half_life_days` (7–365).
- `usage_log.step` + `classify`.
- Keine Spalte an `tasks`: Der Bericht hängt an der bestehenden `routing_decision` (`RoutingDecision.chooser`, optional ⇒ alte Zeilen unverändert gültig).

## 2. Geteilte Typen (`queued-api`)

- Neues Modul `chooser.rs`: `TaskFeatures`, `CandidateView`, `StepChoice`, `ChainStep`, `ChooserReport`, `OverrideInfo`, Einstellungen, Rückmeldung, Admin-Sicht, Replay; deutsche Labels.
- `DecisionSource::{Chooser, Override}`, `RoutingDecision.chooser` (deshalb `RoutingDecision` ohne `Eq`), `CreateTask.urgent`.

## 3. Backend (`crates/app/src/chooser/`)

| Datei | Inhalt |
|---|---|
| `features.rs` | Heuristische Analyse: Typ (Stichwortgruppen, Titel doppelt, kurze Wörter nur an Wortgrenzen), Größe (Länge, Aufzählungen, Dateien, Breite), Komplexität, Risiken (Migration/Datenverlust brauchen starke Signale), Bedarf (Vision nur mit Bildanhang, Long-Context aus Tokenschätzung, Web), Sprachen/Frameworks (Auftrag + Projektregeln WP09), Dateien, Zeitdruck. |
| `classify.rs` | Optionale Einordnung: Nonce-Block, strikter Parser (nur Aufzählungswerte), monotones Zusammenführen (Risiken/Bedarf nur hinzu, Größe/Komplexität ±1, Kategorie nur bei `other`). |
| `model.rs` | Harness → Modellfamilien, Web-Suche je Harness, Fähigkeiten (Kontext, Vision; Annahmen), Priors (Bericht-p + Benchmark-Beleg ⇒ Stärke 10, sonst 4), Kategorie- und Kontext-Offsets (Logit). |
| `bandit.rs` | SplitMix64-RNG, Gamma/Beta-Sampling, Zerfall, `Posterior` (Prior + gedeckeltes Pooling + eigene Daten), `trusted()` = Benchmark oder ≥ 5 eigene Beobachtungen. |
| `decide.rs` | Reine Entscheidung: Kosten je Versuch, harte Constraints (Reihenfolge fest, jeder Ausschluss mit Grund), Ketten über strikt stärkere Stufen + Auffüllen mit Wiederholung (`FIX_FACTOR` 0.32), Rettungskosten, Score (Log-Mischung Kosten/Erstversuch), Greedy vs. Thompson, Explorationsbudget, Schrittwahl Plan/Review/Analyse, Best-of-N-Vorschläge, Bericht. |
| `store.rs` | Einstellungen, Präferenzen, Posterior-Aggregation mit Zerfall in SQL, Pooling (nur Admin-Schalter **und** Opt-in, `o.user_id <> $1`, `HAVING COUNT(DISTINCT user_id) >= 3`), Historie-Profile je Preis-ID, Entscheidungen, Beobachtungen. |
| `learn.rs` | Haken: `run_outcome` (Verify+Review-Urteil; Plan-Schritt; OnEmpty ⇒ `analyze`), `limit_stop`, `review_verdict`, `feedback`, `revert`; Eval-Läufe mit der Kategorie des Falls (`security` ⇒ `bugfix`). |
| `replay.rs` | Offline-Replay (synthetisch, klar benannt; oder Historie mit geglätteter empirischer Wahrheit), Presets „Günstig“/„Ausgewogen“/„Beste Qualität“ vs. Chooser (gleiche Zugänge, Qualitätsregler 90, zusätzlich OpenAI). |
| `mod.rs` | Fassade: `choose` (Timeout 3 s/10 s, Fehler ⇒ Fallback-Bericht), `decision_of`, `routing_for` (effektive Schritt-Routen am Auftrag), `fix_decision` (Kette), `after_insert` (Klassifikationskosten + Entscheidungszeile), `review_pick`, `suggest`, `Data` (geteilte Eingabe). |

Einbaupunkte:

- `routing::resolve_execute`: Chooser nach `decide`, nur bei Quelle `Route|Default` und bekannten Merkmalen; der gewählte Zugang läuft durch `check_decision`; wird er abgelehnt, gilt die Router-Entscheidung mit Grund. `Ask` trägt jetzt Titel/Prompt/Kontext, Anhang-Typen/-Größe, Zeitdruck und Kostenlimit.
- `api/tasks.rs`: Anhang-Metadaten der Entwürfe (`attachments::draft_meta`), `urgent`, `after_insert` nach dem Commit; Best-of-N füllt leere `attempt_targets` mit Vorschlägen; **PATCH** mit neuem Zugang schreibt jetzt die neue `routing_decision` (vorher blieb die alte stehen – Fund beim Lesen) und markiert sie bei Chooser-Aufträgen als `override` mit der ursprünglichen Wahl.
- `triggers.rs`, `intake/mod.rs`: `after_insert` nach dem Commit (Transaktion vorher nicht sichtbar).
- `worker.rs` (OnEmpty): Chooser wählt Analyse-Schritt und Ausweich-Loop unter nicht pausierten Zugängen.
- `runner.rs`: `routing_for` statt `project.routing()` in `run_inner` und im Critic; `create_fix` nutzt zuerst die Chooser-Kette (geprüft), sonst `decide_fix` mit weitergereichtem Bericht; `block_running` lernt aus Limit-Stopps; **WP12-N10**: `review_not_weaker` vor jedem agentischen Review, `defer_review` fällt bei schwächerem Direkt-Review auf das agentische zurück; Review-Urteile ⇒ `review_verdict`; `complete_analyze` ⇒ Lernen.
- `runner_gate::finish_flow`: Lernhaken für das Urteil.
- `api/chooser.rs`: Projekt-Einstellungen (Viewer lesen, Editor ändern, auditiert), Vorschläge, Präferenzen (Teilen auditiert), Rückmeldung (nur Eigentümer, nur beendete Aufträge, nur Modelle mit Preis), Admin-Statistik/-Einstellungen (auditiert)/Replay.
- `config.rs`: `CHOOSER_SEED` (Tests: fest 1).
- `queuectl add --urgent`.

## 4. Frontend (Leptos)

- `pages/chooser.rs`: `DecisionPanel` ersetzt die einzeilige `DecisionLine` (Kurzbegründung, Merkmal-Chips, Kennzahlen, Gründe, Schritte, Kette, Exploration/Fallback/Override, Alternativen-Tabelle mit „Verwenden“, Rückmeldung „Gut/Schlecht“ mit `aria-pressed`), `ChooserCard` (Projekt), `ChooserPrefsCard` (Einstellungen), `AdminChooser` (neuer Reiter „Modellwahl“: Kennzahlen, Gründe, gelernte Zellen als Tabelle, Pooling/Halbwertszeit, Replay mit Tabelle).
- Composer: „Eilig“ unter „Erweitert“, Best-of-N „Modelle vorschlagen lassen“.
- `logic/chooser.rs` (reine Formatierung, getestet), `logic/costs.rs` kennt den Schritt `classify`.
- CSS: Kennzahlen-Raster, Chip-Zeile, 44-pt-Knöpfe, Regler mit 44 pt Höhe.

## 5. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| Kosten pro akzeptiertem Task = E[Kosten] / P(akzeptiert) | E[Kosten] + (1 − P(akzeptiert)) × Rettungskosten; Kette mit Wiederholungen aufgefüllt | Die reine Division belohnt Ketten, die Aufträge liegen lassen (erster Replay: Annahmequote 93 % statt 97 %). Ein liegengebliebener Auftrag muss trotzdem erledigt werden: Rettung = Kosten pro Erfolg auf der stärksten erlaubten Stufe, durch den Regler um Menschenzeit erhöht. Fix-Aufträge behalten nach der Kette das Modell ⇒ Auffüllen mit Wiederholung (0.32× Loop). |
| Score = Kosten/akz. × (1 + 4·q/100·(1 − p₁)) | Log-Mischung `exp((1−w)·ln K + w·2.5·ln(1/p₁))` | Der multiplikative Aufschlag konnte 10-fache Kostenunterschiede nie überwinden – der Regler war wirkungslos (Unit-Test zeigte Haiku bei q = 100 für große Umbauten). |
| Exploration-Kosten = Mehrkosten gegenüber Greedy | Einsatz = Kosten des ausprobierten Versuchs | Die Mehrkosten sind bei günstigen, unerprobten Modellen ≤ 0 und hätten jede Exploration „kostenlos“ gemacht; der Versuchspreis ist die ehrliche Obergrenze des Verlusts. |
| Klassifikationskosten im `Exec` | im Bericht (`classify_step`, `classify_connector`), gebucht in `after_insert` | Trigger/Intake fügen in einer offenen Transaktion ein; erst nach dem Commit existiert die Zeile. Bezahlte, aber unverwertbare Antworten werden trotzdem gebucht. |
| Review-Kandidat ≥ stärkste Kettenstufe | Review für den ersten Ausführer gewählt, Eskalationsstufen über den Laufzeit-Schutz | Ein Opus-Review für jeden Haiku-Versuch wäre teuer; der Laufzeit-Schutz greift für jede stärkere Stufe (und für alle Aufträge ohne Chooser). |
| Replay: Chooser vs. Presets | zusätzlich Regler 90 und „mit OpenAI-Zugang“ | Zeigt die Wirkung des Reglers und dass der Vorteil nicht nur aus mehr Anbietern kommt. |
| Override als eigener Endpunkt | über `PATCH /api/tasks/{id}` | Ein Weg für alle Zugangsänderungen; die Tabelle ruft ihn auf. |
