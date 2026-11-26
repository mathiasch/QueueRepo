# WP15 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP15-Diff (Migration 0016, `queued-api::chooser`, `crates/app/src/chooser/`, Einbau in Routing/Runner/Worker/API, Frontend, E2E, README). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP15), Auftrag des Koordinators, Plan. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit benannten Restrisiken.** Die blockierenden Funde sind behoben und durch Tests (teils mutativ) belegt; die volle Suite ist auf dem finalen Stand zweimal hintereinander grün (Zahlen in `WP15-test.md`). Die ehrliche Grenze: **Die Erfolgswahrscheinlichkeiten, auf denen jede Wahl am Anfang beruht, sind Annahmen** (Kostenbericht: „Alle p-Werte sind Annahmen“; Eval-Set nur im Mock gelaufen, WP13-N14). Der Chooser macht daraus das Beste (Mindeststichprobe, Lernen, Exploration mit Budget), aber kein Test kann beweisen, dass er mit echten Modellen Geld spart – das Replay belegt es nur gegen eine synthetische, klar benannte Wahrheit.

## Anforderungen ↔ Belege

| Anforderung | Stand | Beleg |
|---|---|---|
| Merkmale aus Prompt, Anhängen, Repo (Typ, Größe, Komplexität, Risiko, Fähigkeiten, Sprache/Framework, Dateien) | erfüllt (Repo = Projekt-Regeln WP09; keine Runner-Aufrufe in der Anlage) | Unit `features::*` (7), Integration `chooser_decides_…` |
| Heuristik + günstige Klassifikation, Kosten gezählt und vernachlässigbar | erfüllt (Schritt `classify`, gebucht auch bei unverwertbarer Antwort) | `classification_is_cheap_booked_and_bounded`: 0,00026 $ < 0,5 % der erwarteten Kosten pro erledigtem Auftrag |
| Ohne Klassifikationsmodell rein heuristisch | erfüllt (Standard) | alle übrigen Integrationstests |
| Prompt-Injection lenkt nicht auf teure/unerlaubte Modelle | erfüllt (Aufzählungswerte, monoton, ±1 Stufe, Constraints danach) | Unit `injection_cannot_reach_…`, `model_names_in_the_text_…`, `merge_is_monotone_…`; Integration mit gesperrtem Opus; Mutation M10 |
| Kandidaten nur aus den Connectoren des Users | erfüllt (`routing::accesses`) | Unit/Integration |
| Harte Constraints: Budget, Limits, Pausen, Datenstandort, Projekt-/Nutzerregeln, Fähigkeiten, Kontextlänge | erfüllt, jeder Ausschluss mit Grund in der Tabelle | `hard_constraints_each_exclude_with_a_reason`; Mutation M1, M2 |
| Minimale erwartete Kosten pro akzeptiertem Task inkl. Eskalationskette | erfüllt (Formel + Rettungskosten, siehe Impl §5) | `economics_formula`, `hard_tasks_at_low_quality_get_an_escalation_chain`, Integration `fix_tasks_follow_the_escalation_chain`; Mutation M9 |
| Qualitätsregler pro Projekt | erfüllt | Unit `large_hard_tasks_…`, Replay (Regler 90: Annahme 98–99 %) |
| Best-of-N: Kandidaten vorschlagen | erfüllt (Endpunkt + automatisches Füllen leerer Ziele + Composer-Knopf) | `best_of_n_suggestions`, Unit `…suggestions_are_diverse` |
| Kontextueller Bandit, Thompson, Beta-Priors | erfüllt (Priors aus Bericht/Benchmarks, Eval-Läufe als Beobachtungen) | `thompson_converges_to_the_better_arm`, `learning_moves_the_choice` |
| Exploration mit Kostenobergrenze, Zerfall, Mindeststichprobe | erfüllt | `exploration_budget_caps_spend`, `untrusted_models_only_by_exploration…`, `decay_halves…`, `old_learning_data_decays_and_is_pruned`, `trust_needs_benchmark_or_minimum_sample`; Mutationen M5, M8 |
| Mandantentrennung, Pooling nur mit Absicht, begründet, konfigurierbar | erfüllt (Admin-Schalter **und** Opt-in, k ≥ 3, Deckel 20, nie eigene Zeilen, auditiert) | `learning_data_is_per_tenant_and_pooling_is_opt_in`; Mutation M6 |
| Deterministisch testbar (Seed) | erfüllt (`Rng`, Seed im Bericht, `CHOOSER_SEED`) | `rng_is_deterministic…`, `deterministic_for_a_seed…`, `replay_is_deterministic` |
| Eigene Wahl für Plan, Agent-Loop, Review, OnEmpty | erfüllt (`routing_for` am Auftrag) | Unit `onempty_picks_a_batch_analysis`, Integration `onempty_analysis_is_chosen_as_batch`, `chooser_decides_…` |
| Review nie schwächer, bevorzugt anderer Provider (inkl. **WP12-N10**) | erfüllt (Wahl + Laufzeit-Schutz für **alle** Aufträge) | `review_never_weaker_and_prefers_another_provider`, `review_is_never_weaker_than_the_executor`; Mutationen M3, M4 |
| Batch/Flex automatisch ohne Zeitdruck | erfüllt (Plan/Analyse Batch, Loop Flex; „Eilig“ schaltet ab) | `batch_and_flex_only_without_time_pressure`, OnEmpty-Integration (Fake-Batch) |
| Begründung pro Task (gewählt, warum, Kosten, Alternativen) | erfüllt | E2E Desktop + iPhone hoch/quer mit axe |
| Manuelles Override | erfüllt (Quelle `override`, ursprüngliche Wahl sichtbar) | `override_and_feedback`, E2E |
| User-Feedback fließt ins Lernen | erfüllt (nur Eigentümer, letzte zählt) | `override_and_feedback`; Mutation M11 |
| Admin-Sicht | erfüllt | `admin_statistics_and_replay`, E2E Admin |
| Offline-Replay vs. Presets, Zahlen | erfüllt (synthetisch benannt + Historie) | `synthetic_replay_beats_or_matches_the_presets`, Zahlen in `WP15-test.md` |
| Fallback bei fehlenden Daten / Fehler; blockiert nie | erfüllt (Timeout, Fehler, keine Kandidaten, kein belastbarer Kandidat, abgelehnter Zugang) | `fallback_on_missing_candidates_and_on_errors_never_blocks` (echter DB-Fehler); Mutation M7 |
| Sicherheitsmodell unverändert | erfüllt (nur App-Code, keine Host-Rechte, SSRF-Client für die Klassifikation) | Code-Review |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Qualitätsregler wirkungslos:** Score `K × (1 + λ(1−p))` konnte Kostenunterschiede von Faktor 10 nie überwinden – bei q = 100 wählte der Chooser für große Umbauten Haiku (p ≈ 0,21). | eigener Debug-Lauf der Entscheidung | Log-Mischung Kosten/Erstversuch | `economics_formula`, `large_hard_tasks_…` |
| B2 | **Kosten pro akzeptiertem Task belohnte liegengelassene Aufträge:** `E/P(akz.)` wertet nicht erledigte Aufträge als kostenlos; Ketten ohne Eskalation sahen billiger aus, Replay-Annahmequote 93 % (Vorlagen 97–99 %). Außerdem ignorierte das Modell, dass Fix-Aufträge nach der Kette mit demselben Modell weiterlaufen. | Replay | Rettungskosten (stärkstes Modell, + Menschenzeit über den Regler) und Auffüllen mit Wiederholungen (0,32×) | Replay jetzt 95–96 %; `hard_tasks_at_low_quality_get_an_escalation_chain` |
| B3 | **Exploration wäre „kostenlos“ gewesen:** Mehrkosten gegenüber Greedy sind bei billigen, unerprobten Modellen ≤ 0 – das Budget hätte nie gegriffen. | Review der Budgetlogik | Einsatz = Preis des ausprobierten Versuchs | `exploration_budget_caps_spend` (Summe ≤ Budget über 200 Seeds); Mutation M5 |
| B4 | **Klassifikationskosten gingen verloren:** bei unverwertbarer Antwort (bezahlt, aber verworfen) und bei Triggern/Intake (Buchung vor dem Commit der Auftragszeile). | Review | Kosten im Bericht, Buchung in `after_insert` nach dem Commit, auch ohne verwertbare Antwort | `classification_is_cheap_booked_and_bounded` |
| B5 | **Veraltete Begründung nach Zugangsänderung** (Bestand seit WP12): `PATCH` änderte Zugang/Modell, `routing_decision` blieb – die UI erklärte ein Modell, das nicht mehr lief. | Lesen von `api/tasks.rs` | PATCH schreibt die neue Entscheidung, bei Chooser-Aufträgen als `override` | `override_and_feedback` |
| B6 | **Doppelte Begründung im UI** (Review-Zeile in den Gründen und in den Schritten). | iPhone-Screenshot | Schritte nur noch einmal | E2E-Screenshot |
| B7 | **E2E: offener Drawer überlebt Reload** und verdeckte die Liste (eigener Test). | E2E rot | Drawer schließen, dann navigieren | E2E grün |
| B8 | **Unbegrenztes Wachstum der Lerndaten.** | Review | `store::prune` stündlich (> 8 Halbwertszeiten bzw. Usage-Aufbewahrung) | `old_learning_data_decays_and_is_pruned` |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Begründung / Risiko |
|---|---|---|
| N1 | **Priors sind Annahmen** (Bericht-p, Sekundär-Benchmarks); Kontextfenster und Vision je Modell ebenso. | Ehrlich gekennzeichnet (UI „Annahme“), Mindeststichprobe, Lernen. Erste Wochen hängen davon ab. |
| N2 | **Größe/Komplexität als feste Logit-Offsets,** gelernt wird je Kategorie über alle Größen. | Datendichte; ein Modell, das fast nur kleine Aufträge bekommt, wird für große überschätzt, bis es dort scheitert (Eskalation fängt es). |
| N3 | **Replay ist Simulation;** die Historie-Variante schätzt kontrafaktische Ergebnisse (geglättet). | Benannt im Bericht der Replay-Antwort. |
| N4 | **Kostenschätzung im Composer** (`POST /api/estimate`) kennt die Chooser-Wahl vor dem Einreihen nicht (sie zeigt Route/Standard); die erwarteten Kosten stehen nach dem Einreihen am Auftrag. | Die Wahl braucht die Merkmale; eine zweite Entscheidung beim Tippen wäre teuer (Klassifikation). |
| N5 | **Frei benannte Endpunkte** (OpenRouter, OpenAI-kompatibel, LiteLLM) wählt der Chooser nicht von selbst. | Modellnamen dort sind frei; nur über Projekt-Routing/Regeln. |
| N6 | **CHF-Preise ohne Wechselkurs** verglichen (WP13-N13). | Hinweis im Bericht der Wahl. |
| N7 | **Web-Suche je Harness ist eine Annahme** (opencode ohne Suche). | Schlimmstenfalls wird ein opencode-Zugang für Recherche ausgeschlossen. |
| N8 | **Review-Lernsignal ist einseitig:** nur Freigaben (+ spätere negative Rückmeldung/Revert) zählen, Ablehnungen nicht. | Ob eine Ablehnung richtig war, ist erst am Fix erkennbar; bewusst konservativ. |
| N9 | **Overrides sind kein Lernsignal.** | Vorlieben ≠ Qualität; dokumentiert. |
| N10 | **Laufzeit-Schutz „Review nie schwächer“** kann nur bei bekannten Stufen urteilen (Modellname im Katalog); ein Review mit dem CLI-Standardmodell (kein Name) bleibt ungeprüft. | Wie WP12; der Chooser setzt immer Modellnamen. |
| N11 | **Klassifikation verlängert die Anlage** um bis zu 10 s (Timeout). | Nur bei eingeschalteter Option; danach Heuristik. |
| N12 | **Best-of-N-Versuche** bekommen keinen eigenen Chooser-Bericht (Quelle „Versuch“); ihr Review folgt Projekt-Routing + Laufzeit-Schutz. | Versuche sind bewusst explizite Ziele. |

## Mutative Gegenprobe

Je eine gezielte Verschlechterung, Test muss rot werden, danach zurückgesetzt (Datei-Zeit erneuert, damit Cargo neu baut – WP12-Fund). Ergebnisse siehe `WP15-test.md` §3.

## Geprüft und für gut befunden

- **Mandanten:** Beobachtungen tragen `user_id`; Abfragen filtern immer darauf; Pooling schließt eigene Zeilen aus und verlangt Opt-in der **Beitragenden**. Admin-Sicht zeigt nur Aggregate (keine Texte, keine Nutzer-IDs).
- **Rechte:** Projekt-Einstellungen lesen Viewer, ändern Editor (auditiert); Rückmeldung nur Eigentümer des Auftrags; Admin-Endpunkte nur Admins ohne API-Token (bestehender Extraktor).
- **Datenabfluss:** Klassifikation nur über Zugänge, die der Datenstandort erlaubt, nur ungesperrte Modelle, über den SSRF-geschützten Client; Fehlermeldungen redigiert.
- **Nie blockierend:** jede Fehlerquelle der Entscheidung endet in einem Fallback-Bericht; Lernhaken protokollieren nur.
- **Alt-Verhalten:** Ohne eingeschalteten Chooser ändert sich nur (a) der Laufzeit-Schutz des Reviews (behebt WP12-N10) und (b) die korrekte Begründung nach PATCH; alle bestehenden Tests unverändert grün.
