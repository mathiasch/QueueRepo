# WP15 – Plan: Intelligenter Modell-Chooser

Stand: Start von WP15 auf `mvp` @ `4f54b81` (nach WP12, Migration 0015).
Maßstab: `work-packages.md` (Gemeinsame Regeln, WP15), Auftrag des Koordinators,
`reports/Kostenoptimale Modellanbieter für Queue.md` (Kosten pro akzeptiertem Task,
Batch/Flex, Routing, „p-Werte sind Annahmen“), offene Funde aus WP01/WP09–WP13.

## 1. Ausgangslage (gelesen und geprüft, nicht übernommen)

- **Routing (WP13/WP12):** `projects.routing` (Presets + Schritte Plan/Umsetzung/Review/Analyse,
  Eskalationskette, Flex, Regeln). `routing::decide` (explizit › Regel › Route › Standard),
  `decide_fix` (Eskalation › Fix-Regel › geerbt), `check_decision` (Zugang, Datenstandort,
  Pflichtmodell). `RoutingDecision` liegt als JSON in `tasks.routing_decision`, die UI zeigt
  „Warum dieses Modell?“ als **eine Zeile**. Die Schnittstelle für WP15 ist „dieselbe
  Struktur mit eigener `DecisionSource`“.
- **Wo wird entschieden?** Die Umsetzung (Zugang/Modell) wird **bei der Anlage** festgelegt
  (`resolve_execute` in API, Triggern, Intake, Eval, Schätzung). Plan/Review/Analyse liest
  `run_inner` **zur Laufzeit** aus `project.routing()`. Fix-Aufträge entscheidet `create_fix`.
- **Kosten (WP13):** Preiskatalog mit Stufen (`cheap|mid|top`), Batch-/Flex-Faktor,
  Long-Context pro Request, Regionsaufschlag; `estimate.rs` mit Report-Profilen S/M/L und
  eigener Historie (Median ab 3 Läufen).
- **Eval-Set (WP13):** 8 Fälle (bugfix, feature, docs, tests, refactor, security; S/M/L),
  **nur im Mock gelaufen** (WP13-N14). Echte Erfolgsquoten existieren nicht. Der Bericht sagt
  ausdrücklich: „Alle p-Werte sind Annahmen“, für Terra, Luna, Large 4 „keine belastbaren
  Coding-Benchmarks“.
- **Lerndaten (WP10):** `task_runs` (Lauf, Ergebnis, Tokens, Preis-ID), `usage_log` (Kosten
  pro Schritt), `tasks.failure_class` (WP01: nur `task` ist ein echter Fehlschlag;
  Infra/Limit/Pause werden ohne Fix neu eingereiht).
- **Sperren (WP01/WP11/WP13):** `worker::Blocked` (Pausen, Budgets je Harness/Zugang),
  Kontingent `user_quotas`, Kostenlimit pro Auftrag/Projekt (`max_cost_usd`).
- **Offene Funde, die in WP15 fallen:**
  - **WP12-N10:** Das normale Review prüft nur „nie günstigste Stufe“, nicht „nie schwächer
    als der Ausführer“ (Opus-Eskalation mit Sonnet-Review möglich). → wird behoben (§5.3).
  - **WP12-N9:** Regel-Merkmale ohne Modell (Zeichenzahl, Teilstring). → Die Task-Analyse
    liefert echte Merkmale; Regeln bleiben bewusst deterministisch.
  - **WP13-N14:** Eval-Set nur im Mock. → Priors sind als Annahme markiert; echte Eval-Läufe
    fließen als Beobachtungen ein.
  - Fund beim Lesen: `PATCH /api/tasks/{id}` mit neuem Zugang lässt die alte
    `routing_decision` stehen (UI erklärt dann ein Modell, das nicht mehr gilt). → wird mit
    dem Override behoben.

## 2. Ziel und Abgrenzung

Ein Chooser, der pro Auftrag und pro Schritt (Plan, Agent-Loop, Review, OnEmpty-Analyse)
aus den **Zugängen des Users** das Modell mit den **geringsten erwarteten Kosten pro
akzeptiertem Auftrag** wählt, unter harten Constraints, lernend, erklärbar, übersteuerbar –
und der **nie** einen Auftrag blockiert (Fallback auf Preset/Route).

Nicht in WP15: neue Connectoren, Lizenzfragen, Änderung des Sicherheitsmodells,
Live-Aufrufe echter Modelle.

## 3. Architektur

Neues Modul `backend/crates/app/src/chooser/` (App, keine Host-Rechte, keine Prozesse):

| Datei | Inhalt | rein? |
|---|---|---|
| `features.rs` | Task-Analyse (Heuristik): Typ, Größe, Komplexität, Risiko, Fähigkeiten, Sprachen/Frameworks, Dateien/Zeilen, geschätzte Kontext-Tokens, Zeitdruck | ja |
| `classify.rs` | optionale günstige Klassifikation per Direkt-API (Fake-Server im Test), strikter Parser, monotones Zusammenführen | Aufruf async, Merge rein |
| `model.rs` | Fähigkeiten je Modell (Kontextfenster, Vision; als Annahme markiert), Priors (Bericht: p-Annahmen + Benchmarks) | ja |
| `bandit.rs` | Beta-Posterior je (Schritt × Kategorie × Modell), Thompson Sampling mit **eigenem seedbarem RNG** (SplitMix64 + Marsaglia-Tsang-Gamma), Zerfall | ja |
| `decide.rs` | Kandidaten, harte Constraints, Kosten pro Versuch, Eskalationskette, Kosten pro akzeptiertem Task, Qualitätsregler, Exploration, Schritt-Wahl, Best-of-N-Vorschläge | **ja, deterministisch** |
| `store.rs` | DB: Einstellungen, Beobachtungen, Entscheidungen, Posterior-Abfrage (mit Zerfall in SQL), Pooling | async |
| `learn.rs` | Lernhaken (Ende eines Laufs, Review, Feedback, Revert, Eval) | async |
| `replay.rs` | Offline-Replay gegen Historie bzw. synthetische Historie, Vergleich mit Presets | ja (+ Lader) |
| `mod.rs` | Fassade: `choose` (mit Timeout + Fallback), `routing_for` (effektive Schritte am Auftrag), `fix_target` | async |

Einbindung:

1. **Anlage** (`routing::resolve_execute`): nach `decide`. Ist die Quelle `Route`/`Default`
   (also weder explizit noch Regel), der Chooser im Projekt aktiv und sind Merkmale bekannt
   (`ask`), entscheidet der Chooser. Präzedenz damit: **explizit › Regel › Chooser › Route ›
   Standard**. Regeln sind Projektregeln des Menschen und gewinnen bewusst.
2. **Laufzeit** (`run_inner`): `chooser::routing_for(task, project)` ersetzt `project.routing()`:
   die am Auftrag gespeicherten Schritt-Entscheidungen (Plan/Review/Analyse, Flex) werden zu
   einer effektiven `Routing`; ohne Chooser-Entscheidung unverändert das Projekt-Routing.
3. **Fix** (`create_fix`): nächste Stufe der gespeicherten Chooser-Eskalationskette (jedes Mal
   neu gegen Constraints geprüft); erschöpft oder unbrauchbar ⇒ bisherige `decide_fix`.
4. **OnEmpty** (`worker`): Chooser wählt den Analyse-Schritt (bei aktivem Chooser).
5. **Best-of-N:** `GET /api/projects/{id}/chooser/suggest?attempts=N` und automatisches
   Befüllen leerer `attempt_targets` bei aktivem Chooser (verschiedene Modelle, bevorzugt
   verschiedene Anbieter).
6. **Lernen:** Haken in `gate::finish_flow` (akzeptiert/abgelehnt), `block_running`
   (Limit-Abbrüche), Feedback-Endpunkt, Revert, Eval-Ende.

Fehler oder Zeitüberschreitung (2 s ohne, 8 s mit Klassifikation) im Chooser ⇒ Ergebnis von
`decide` (Preset/Route/Standard) mit `fallback`-Begründung. Der Chooser gibt **nie** einen
Fehler an die Anlage weiter.

## 4. Task-Analyse (Merkmale)

Heuristik (deutsch + englisch, Wortgrenzen, Titel + Prompt + Kontext + Anhänge):

- **Typ** (= Bandit-Kategorie): `bugfix`, `feature`, `refactor`, `docs`, `tests`, `research`,
  `ui`, `analysis` (OnEmpty), `other`. Punkte je Stichwortgruppe, Gleichstand ⇒ feste Ordnung.
- **Größe** S/M/L: Länge, Aufzählungspunkte, genannte Dateien, Breite („überall“, „alle
  Dateien“, „gesamte Codebasis“), Anhänge.
- **Komplexität** 1–3: Architektur/Nebenläufigkeit/Performance/Algorithmus/Migration, Größe.
- **Risiko:** `security`, `migration`, `infra`, `data_loss`.
- **Fähigkeiten:** `vision` (Bildanhänge, „Screenshot“), `long_context` (Schätzung > 100k
  Tokens), `web` (Recherche, „neueste Version“, URLs).
- **Sprachen/Frameworks** aus Dateiendungen und Stichworten (Rust, Python, TS/JS, Go, Java,
  SQL, Shell, CSS/HTML; Leptos, React, Vue, Django, …) – erklärend und für Kontext.
- **Dateien/Zeilen** (Pfad-Muster, `:123`, `L123`).
- **Zeitdruck:** Flag am Auftrag (Composer/API „Eilig“) oder Stichworte (dringend, asap,
  hotfix, sofort). Ohne Zeitdruck: Batch für Plan/Analyse, Flex für den Loop (wo vorhanden).
- **Repo (WP09):** Projektzustand (offene Einträge) und Regeln (AGENTS.md) als zusätzlicher
  Kontext für Sprache/Framework; keine Runner-Aufrufe in der Anlage (Latenz, Blockade).

**Günstige Klassifikation (optional pro Projekt):** ein Direkt-Aufruf an das günstigste
erlaubte Modell (Stufe `cheap`, Datenstandort/Sperrlisten geprüft), Auftrag als Datenblock mit
Nonce (`prompt::user_block`), Antwort nur als JSON mit Aufzählungswerten. **Monotones
Zusammenführen:** Risiken und Fähigkeiten nur hinzufügen (Vision nur mit Bildanhang),
Größe/Komplexität höchstens ±1 Stufe gegenüber der Heuristik, Kategorie nur, wenn die
Heuristik unsicher ist. Kosten werden als Schritt `classify` am Auftrag gebucht
(Migration erweitert `usage_log.step`). Ohne Modell: rein heuristisch.

## 5. Entscheidung

### 5.1 Kandidaten und harte Constraints (in dieser Reihenfolge, jeder Ausschluss mit Grund)

Kandidaten = jede nutzbare Zugang-×-Modell-Kombination: Zugänge aus `routing::accesses`
(Connectoren + gespeicherte Zugänge des Users, Projektzugänge für Bearbeiter), Modelle aus
dem aktuellen Preiskatalog der Familie, die der Harness des Zugangs bedienen kann. Frei
benannte Endpunkte (OpenRouter, OpenAI-kompatibel, LiteLLM) sind nur Kandidat, wenn das
Projekt-Routing dort ein Modell nennt.

1. **Datenstandort** (EU/CH) des Projekts.
2. **Pausen/Limits/Budgets** (`worker::Blocked`), **Kontingent** (WP11).
3. **Projekt- und Nutzerregeln:** Sperrliste des Projekts und des Users (Modelle).
4. **Fähigkeiten:** Vision, Web-Recherche (Harness), **Kontextlänge** (geschätzte Tokens ≤
   Kontextfenster).
5. **Kostenlimit** des Auftrags/Projekts: erwartete Kosten eines Versuchs ≤ Limit.
6. **Risiko:** bei `security|migration|infra|data_loss` nie die günstigste Stufe als erster
   Ausführer.

Ist kein Kandidat übrig ⇒ Fallback (Preset/Route) mit Begründung.

### 5.2 Ziel: minimale erwartete Kosten pro akzeptiertem Auftrag

Für eine Kette `c1 → c2 → … → ck` (Eskalation, `k ≤ 1 + max_fix_attempts`, höchstens 3):

- Kosten je Versuch `C_i` = Loop (Profil S/M/L bzw. eigene Historie ab 3 Läufen, Flex-Faktor,
  Long-Context-Stufe bei `long_context`, Regionsaufschlag) + Review (agentisch, Profil) und
  einmal Plan (Batch-Faktor ohne Zeitdruck).
- Erfolgswahrscheinlichkeit `p_i` aus dem Bandit (Kategorie × Modell × Schritt `execute`),
  kontextuell verschoben (Logit-Offsets für Größe, Komplexität, Risiko).
- `E[Kosten] = P + Σ_i Π_{j<i}(1−p_j) · C_i`, `P(akzeptiert) = 1 − Π(1−p_i)`,
  **Kosten pro akzeptiertem Auftrag = E[Kosten] / P(akzeptiert)**.
- Ketten: je Startkandidat die günstigste Fortsetzung über strikt stärkere Stufen.
- **Qualitätsregler** pro Projekt `q ∈ [0,100]` (Standard 50):
  `Score = Kosten/akzeptiert × (1 + 4·q/100 · (1 − p_1))` – Erstversuchs-Fehlschläge kosten
  Wanduhrzeit, Menschen-Review und Risiko falsch akzeptierter Diffs (Bericht).

### 5.3 Schritt-spezifisch

- **Plan:** an für Größe M/L oder Komplexität ≥ 2, wenn der Ausführer nicht schon oberste
  Stufe ist; Modell = beste erwartete Planqualität (Bandit Schritt `plan`) unter Stufe ≥ Mid,
  nur Direkt-/Batch-Zugänge; **Batch ohne Zeitdruck**, sonst direkt.
- **Agent-Loop:** §5.2; **Flex automatisch** ohne Zeitdruck auf Zugängen mit Flex.
- **Review:** **nie schwächer als der Ausführer** (Stufe ≥ Stufe des Ausführers, auch für jede
  Eskalationsstufe – maßgeblich ist die stärkste Stufe der Kette), nie günstigste Stufe,
  **bevorzugt anderer Anbieter** (Self-Preference-Bias), dann geringste Kosten; agentisch
  (Bericht/WP13-B4: Batch-Review nur als Opt-in). Kein passender Zugang ⇒ derselbe Zugang und
  dasselbe Modell wie der Ausführer (nie schwächer) mit Hinweis.
- **Laufzeit-Schutz für alle Aufträge (WP12-N10):** `run_review` prüft vor jedem Review, ob
  das Review-Modell schwächer ist als das Modell des Ausführers (z. B. Eskalation auf Opus,
  Route sagt Sonnet). Dann wird ein nicht schwächerer Prüfer gewählt (gleiche Regel), sonst
  Zugang+Modell des Ausführers; als Ereignis sichtbar. Ein Direkt-/Batch-Review mit
  schwächerem Modell fällt auf das agentische Review mit diesem Schutz zurück.
- **OnEmpty-Analyse:** Kategorie `analysis`, Stufe ≥ Mid, Batch (nie Zeitdruck), sonst
  agentisch.

### 5.4 Exploration

- Greedy-Wahl `g` (Posterior-Mittel) und Thompson-Wahl `t` (Stichprobe). `t ≠ g` ist
  Exploration und nur erlaubt, wenn (a) kein hohes Risiko, (b) **Explorationsbudget** des
  Projekts (USD / 30 Tage, Standard 2 USD) die Mehrkosten `max(0, K(t) − K(g))` noch deckt.
- **Mindeststichprobe:** Ein Kandidat ohne belastbaren Prior (nur „Annahme“) gilt erst ab
  5 effektiven eigenen Beobachtungen als belastbar; greedy werden nur belastbare Kandidaten
  gewählt, unbelastbare nur per Exploration. Kein belastbarer Kandidat ⇒ Fallback.
- **Seed:** pro Auftrag aus der Auftrags-ID (reproduzierbar), in Tests injizierbar.

## 6. Lernen

- **Beobachtung** = (User, Projekt, Auftrag, Schritt, Kategorie, Modell-Schlüssel = Preis-ID,
  Ergebnis 0/1, Gewicht, Kosten, Quelle `run|feedback|revert|eval`, Zeit).
  - `execute`: akzeptiert (Verify + Review bestanden) = 1; echter Fehlschlag (`task`) oder
    Kosten-/Token-/Turn-/Laufzeitlimit = 0; Infra/Pause/Abbruch/Konfiguration = **keine**
    Beobachtung (WP01-Klassen).
  - `plan`: Erfolg des ersten Versuchs mit diesem Plan.
  - `review`: Freigabe ohne spätere negative Rückmeldung = 1; negative Rückmeldung oder
    Revert nach Freigabe = 0.
  - `analyze`: Analyse ohne Fehler beendet = 1, fehlgeschlagen = 0.
  - **Feedback** „Ergebnis gut/schlecht“ an erledigten Aufträgen und **Revert** (WP09) = eigene
    Beobachtung (Gewicht 1, letzte Rückmeldung zählt).
  - **Eval-Läufe** (WP13): Ergebnis je Fall mit dessen Kategorie.
- **Posterior** = Prior (α₀, β₀) + Σ Gewicht × Zerfall, Zerfall `0.5^(Alter/Halbwertszeit)`
  (Standard 60 Tage, Admin einstellbar), in SQL aggregiert.
- **Priors:** p aus dem Bericht (Annahmen: Opus 0.85, Sonnet 0.75, Haiku 0.45, Terra 0.65,
  Large 4 0.60) + Benchmarks (SWE-bench Pro: Opus 89.9 %, Sonnet 81.3 %, Haiku 64.8 %), je
  Kategorie leicht verschoben (Doku/Tests leichter, Refactor schwerer); Stärke 10
  Pseudo-Beobachtungen bei Benchmark-Beleg, 4 bei reiner Annahme; Stufen-Fallback für
  Katalogmodelle ohne eigenen Eintrag. Alle als „Annahme“ ausgewiesen.
- **Mandantentrennung:** Beobachtungen gehören dem User. In **globale Priors** fließen Daten
  nur, wenn (1) der Admin Pooling einschaltet **und** (2) der User „Lerndaten teilen“ aktiviert
  hat. Geteilt werden nur Zählwerte je Zelle (keine Texte), nur Zellen mit ≥ 3 verschiedenen
  Usern (keine Rückschlüsse auf einzelne), gedeckelt auf 20 Pseudo-Beobachtungen (fremde
  Daten sind Prior, nie Übersteuerung). Begründung: Opt-in schützt Geschäftsgeheimnisse
  (Aufgabentypen, Erfolgsquoten eines Kunden), k-Anonymität verhindert Rückrechnung.
- **Projektzugriff:** Beobachtungen eines gemeinsamen Projekts zählen für den User, dessen
  Auftrag es war (er hat den Zugang bezahlt), nicht für Mitglieder.

## 7. Transparenz und Kontrolle

- **Begründung am Auftrag** (erweitert „Warum dieses Modell?“): gewählt (Modell, Zugang),
  warum (Merkmale, Erfolgschance, Datenlage), erwartete Kosten (pro Versuch, pro akzeptiertem
  Auftrag), Schritte (Plan/Review/Analyse, Batch/Flex), Eskalationskette, Alternativen als
  **Tabelle** (mit Ausschlussgrund), Exploration/Fallback-Hinweis.
- **Override:** in der Alternativen-Tabelle „Stattdessen verwenden“ (wartende Aufträge) bzw.
  wie bisher Zugang/Modell bearbeiten; gespeichert als Quelle `override` mit der ursprünglichen
  Wahl. Übersteuerungen sind keine Qualitätsdaten (kein Lernsignal).
- **Feedback:** „Ergebnis gut“ / „Ergebnis schlecht“ an beendeten Aufträgen ⇒ Lernen.
- **Admin-Sicht:** Entscheidungen (30 Tage) nach Quelle, Exploration und deren Kosten,
  Fallback-Gründe, Klassifikationskosten, gelernte Erfolgsquoten je Kategorie × Modell ×
  Schritt (aggregiert), Pooling/Halbwertszeit, Replay.

## 8. Absicherung

- **Offline-Replay** (`replay.rs`, Admin-Endpunkt + Test): Auftragsstrom (aus der Historie:
  Merkmale + Ergebnis je Modell, geglättet; oder **synthetisch, klar benannt** mit bekannter
  Wahrheit je Kategorie × Größe × Modell). Chooser lernt online mit Seed; Presets „Günstig“,
  „Ausgewogen“, „Beste Qualität“ fahren ihre feste Kette. Kennzahlen: Kosten pro akzeptiertem
  Auftrag, Annahmequote, Erstversuchsquote. Abnahmekriterium: Chooser senkt Kosten pro
  akzeptiertem Auftrag gegenüber jedem Preset **oder** hält die Annahmequote.
- **Fallback** bei fehlenden Daten, Fehler, Timeout; Chooser blockiert nie.
- **Deterministische Tests** der Entscheidungslogik (reine Funktionen, Seed).

## 9. Datenmodell (Migration `0016_model_chooser.sql`)

```sql
chooser_settings(project_id PK → projects, enabled, quality 0..100, explore_budget_usd ≥ 0,
                 classify, blocked_models text[] ≤ 50, updated_at)
chooser_user_prefs(user_id PK → users, share_learning, blocked_models text[] ≤ 50, updated_at)
chooser_observations(id, user_id, project_id NULL, task_id NULL, step CHECK, category CHECK,
                     model_key, family, success real 0..1, weight > 0, cost_usd ≥ 0,
                     source CHECK run|feedback|revert|eval, created_at,
                     UNIQUE(task_id, step, source))
chooser_decisions(task_id PK → tasks, user_id, project_id, source chooser|fallback|override,
                  category, model_key, explored, explore_cost_usd, expected_cost_usd,
                  classify_cost_usd, fallback_reason, created_at)
instance_settings + chooser_pooling bool, chooser_half_life_days 7..365
usage_log.step + 'classify'
```

Geteilte Typen (`queued-api`): `RoutingDecision.chooser: Option<ChooserReport>` (alte Zeilen
bleiben gültig), `DecisionSource::{Chooser, Override}`, `CreateTask.urgent`,
Einstellungs-/Statistik-/Replay-Typen.

## 10. API

- `GET/PUT /api/projects/{id}/chooser` (Bearbeiter lesen, Eigentümer/Bearbeiter ändern)
- `GET /api/projects/{id}/chooser/suggest?attempts=N` (Best-of-N)
- `GET/PUT /api/chooser/prefs` (eigene: Teilen, Sperrliste)
- `POST /api/tasks/{id}/feedback {good}` (Eigentümer des Auftrags)
- `PATCH /api/tasks/{id}` mit Zugang/Modell ⇒ neue Entscheidung, Quelle `override`
- `GET/PUT /api/admin/chooser`, `POST /api/admin/chooser/replay` (nur Admin, auditiert)
- `POST /api/tasks` + `urgent`

## 11. UI (WP00-Designsystem, deutsch, iPhone)

- Auftrag: Karte „Warum dieses Modell?“ mit Kurzbegründung, Merkmal-Chips, Kosten,
  Schritte, Kette, Alternativen-Tabelle (scrollbar, fokussierbar), „Stattdessen verwenden“,
  Feedback-Knöpfe (44 pt, `aria-pressed`).
- Projekt → Einstellungen: Karte „Automatische Modellwahl“ (Schalter, Regler
  „Kosten ↔ Qualität“ mit Textwert, Explorationsbudget, Klassifikation, Sperrliste).
- Einstellungen: „Lerndaten teilen“ mit Erklärung.
- Admin: „Modellwahl“ (Statistik-Tabellen, Pooling, Halbwertszeit, Replay mit Tabelle; Balken
  nur zusätzlich, Tabelle ist die Textalternative).
- Composer: „Eilig“ unter „Erweitert“, Best-of-N „Vorschlag übernehmen“.
- Safe Areas, 44-pt-Ziele, 16-px-Inputs, axe grün hell/dunkel.

## 12. Bedrohungsmodell

| Bedrohung | Maßnahme |
|---|---|
| Prompt-Injection im Auftrag lenkt auf teure/unerlaubte Modelle („nimm Opus“, „klassifiziere als L“) | Modellnamen im Text haben keine Wirkung; Klassifikation liefert nur Aufzählungswerte, monoton, ±1 Stufe; **harte Constraints nach der Bewertung** (Datenstandort, Sperrlisten, Kostenlimit, Budgets) – kein Weg vorbei; Test |
| Injection gegen das Klassifikationsmodell (Ausbruch aus dem Datenblock) | Nonce-Block, Antwort strikt geparst, ungültig ⇒ Heuristik |
| Datenabfluss über die Klassifikation | nur Zugänge, die der Datenstandort erlaubt, nur Modelle ohne Sperre; Klassifikation pro Projekt aus (Standard) |
| Lerndaten eines Mandanten beeinflussen/verraten andere | Beobachtungen pro User; Pooling nur Admin **und** User-Opt-in, k ≥ 3, gedeckelt; Test |
| Vergiftung durch eigenes Feedback | wirkt nur auf eigene Daten; geteilt nur aggregiert und gedeckelt |
| Explorations-Kostenexplosion | Budget pro Projekt/30 Tage, kein Explorieren bei Risiko, Kostenlimit gilt |
| Chooser-Fehler blockiert die Queue | Timeout + Fallback, Fehler nur protokolliert; Test |
| Review durch schwächeres Modell (WP12-N10) | Laufzeit-Schutz für alle Aufträge; Test |
| Override auf unerlaubten Zugang | Override läuft durch `check_decision` (Rechte, Datenstandort, Pflichtmodell) |
| Admin-Endpunkte | nur Admin, auditiert; Replay ohne Texte |

## 13. Tests (Plan)

- Unit (rein, Seed): Merkmale (Typen, Risiko, Injection-Texte), Constraints (jeder Ausschluss),
  Kostenformel inkl. Kette, Qualitätsregler, Review nie schwächer/anderer Anbieter, Batch/Flex
  ohne Zeitdruck, Bandit-Konvergenz (synthetisch), Exploration-Budget, Zerfall,
  Mindeststichprobe, Klassifikations-Merge (monoton), RNG-Determinismus, Replay-Kennzahlen.
- Integration (Postgres, Mock-Agent, Fake-LLM): Anlage mit Chooser (Entscheidung gespeichert,
  Schritte wirken im Lauf), Lernen aus echten Läufen (Erfolg/Fehlschlag/Infra ignoriert),
  Fix über die Kette, Mandantentrennung inkl. Pooling-Regeln, Fallback bei Fehler/fehlenden
  Daten, Override, Feedback, Klassifikation mit gebuchten Kosten, Review-Schutz WP12-N10,
  Admin-Statistik/Replay (Rechte), Best-of-N-Vorschläge.
- E2E: Projekt-Einstellung, Begründung inkl. Alternativen-Tabelle, Override, Feedback, Admin;
  Desktop + iPhone hoch/quer, axe hell/dunkel.
- Mutationen: Constraint-Prüfung, Review-Stufe, Exploration-Budget, Mandantenfilter,
  Fallback, Kostenformel, Zerfall.

## 14. Risiken

- **Priors sind Annahmen** (keine echten Erfolgsquoten, WP13-N14). Gegenmittel: als Annahme
  markiert, Mindeststichprobe, Lernen aus eigenen Läufen, Replay. Ehrlich: die Qualität der
  ersten Wochen hängt von den Annahmen ab.
- **Größe/Komplexität als feste Logit-Offsets** statt eigener Zellen (Datendichte). Lernen
  pro Kategorie mischt Größen.
- **Replay ist Simulation:** Kontrafaktische Ergebnisse (Modell, das nie lief) kommen aus
  geglätteten Schätzern bzw. der synthetischen Wahrheit.
- **Viele Einbaupunkte** (Anlage, Laufzeit, Fix, OnEmpty, Review). Gegenmittel: eine Fassade,
  Integrationstests über den echten Ablauf.
- **Latenz der Anlage** mit Klassifikation (bis 8 s): nur bei aktivierter Klassifikation.
