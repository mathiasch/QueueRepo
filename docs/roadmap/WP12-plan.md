# WP12 – Plan: Best-of-N, Preview & Modell-Routing

Stand: Start von WP12 auf `mvp` @ `5241d25` (nach WP11, Migration 0014).
Maßstab: `work-packages.md` (Gemeinsame Regeln, WP12), Auftrag des Koordinators,
Report P2.2/P2.4/P2.5 (`reports/Funktionsumfang …`) und der Kostenbericht
(Planer/Ausführer-Split, Eskalation, Review nie billigste Stufe).

## 1. Ausgangslage (gelesen, nicht übernommen)

- **Routing (WP13):** `projects.routing` (JSON `Routing`): Schritte Plan / Umsetzung
  (+ Eskalationskette, Flex) / Review / Analyse, fünf Presets. Entschieden wird an
  zwei Stellen: `routing::resolve_execute` (neue Aufträge: explizit > Projekt-Route >
  Standard-Anbieter) und `routing::escalate` (Fix-Auftrag). **Lücken:** keine Regeln
  nach Auftragsmerkmalen (klein/groß, Stichworte, Art), keine gespeicherte Begründung
  am Auftrag, keine Schnittstelle für WP15.
- **Pro Auftrag ein Repo (WP04):** `workspaces/t<task>` + Branch `queue/<id>`, nur
  dieses Verzeichnis wird gemountet. Das ist die Isolation, die Best-of-N braucht:
  N Versuche = N Aufträge = N Repos/Container/Branches.
- **Merge-Queue / PR-Modus (WP04/WP05):** `merge::enqueue` → `status='merging'` →
  `run_merge` (commit: Squash+FF; branch/pr: Push/PR). Wird für den gewählten
  Versuch unverändert benutzt.
- **Gate (WP03):** `gate::check_changes` + `gate::commit` laufen in `run_inner` für
  *jeden* Auftrag. Versuche durchlaufen denselben `run_inner` ⇒ Gate/Secret-Scan
  gilt pro Versuch ohne Sonderweg.
- **Scheduler (WP04/WP11):** Nutzer-Parallelität, Admin-Kontingent
  (`max_parallel`, Kosten 30 Tage), Projekt-Parallelität (running+merging),
  Runner-Kapazität (`RUNNER_USABLE`), alles race-frei per Zeilensperre.
- **Limits:** pro Auftrag (Kosten/Token, live per `Guard`), pro Nutzer/Anbieter
  (Fenster-Budgets, sperren nur *neue* Starts).
- **Sandbox (WP02):** Container `--network none`, einziger Weg nach draußen ist der
  Egress-Socket; Engine `none` nur Entwicklung/Tests.

## 2. Best-of-N

### 2.1 Modell

Ein Auftrag mit `attempts = N > 1` ist eine **Gruppe**. Die Gruppe ist der normale
Auftrag (Elternauftrag, `kind='task'`); er läuft nie selbst als Agent.

1. Der Elternauftrag wird wie jeder Auftrag eingeplant. Plan-Modus (WP06) und der
   Single-Shot-Plan-Schritt (WP13) laufen **einmal** auf ihm.
2. Kommt er zur Umsetzung (in `run_inner` nach Plan-Modus/Plan-Schritt, vor dem
   Arbeitsbaum), **fächert er auf**: in *einer* Transaktion entstehen N Versuche
   (`kind='attempt'`, `attempt_of`, `attempt_no`, eigener Zugang/Modell, Prompt,
   Kontext, Plan, Limits und Anhänge des Elternauftrags), und der Elternauftrag geht
   auf den neuen Status **`comparing`**. Alles oder nichts (Absturz ⇒ keine halben
   Gruppen).
3. Jeder Versuch ist ein vollwertiger Auftrag: eigener Arbeitsbaum `t<id>`, eigener
   Branch, eigener Container, Gate, Verify, Review – nur das Ende ist anders:
   - Erfolg (Gate, Verify, Review bestanden) ⇒ neuer Status **`candidate`**,
     `gate_pending` bleibt für die spätere Übernahme gespeichert, **kein** Merge.
   - Fehlschlag ⇒ `failed` **ohne** Fix-Auftrag und ohne Eskalation (Best-of-N
     filtert; Fix-Aufträge würden N weitere Läufe erzeugen).
   - Gate-Halt, Rückfrage, Limits, Pausen, Wiederholungen: wie bei jedem Auftrag.
4. Nach jedem Ende eines Versuchs (`run_task`-Ende) wird die Gruppe neu bewertet
   (`bon::settle`): Laufen alle nicht mehr (kein `queued/running/held/waiting_*`):
   - keine Kandidaten ⇒ Elternauftrag `failed` (bzw. `blocked` bei Budget-Abbruch);
   - Kandidaten und Auto-Auswahl ⇒ Review-Agent wählt;
   - sonst Benachrichtigung „Versuche fertig – bitte auswählen“.
   Der Nutzer darf schon wählen, sobald ein Kandidat existiert (Rest wird gestoppt).
5. **Auswahl** (`bon::select`, Nutzer oder Auto, dieselbe Funktion, Zeilensperre auf
   dem Elternauftrag ⇒ genau eine Auswahl gewinnt): Gewinner `candidate → merging`
   (bestehende Merge-Queue bzw. PR-Modus), Verlierer `cancelled` („verworfen“),
   laufende Verlierer werden abgebrochen, Arbeitsbäume **und** Branches der
   Verlierer werden entfernt, ihre Vorschauen gestoppt. Elternauftrag `done` mit
   `bon.choice` (Versuch, wer, Begründung, Zeitpunkt).
6. Abbrechen des Elternauftrags bricht alle Versuche ab und räumt auf. Wiederholen
   eines gescheiterten Elternauftrags fächert neu auf (alte Versuche bleiben als
   Historie, die Vergleichsansicht zeigt die jüngste Runde).

### 2.2 Planung und Grenzen

- **N:** 2–8 (`CHECK`), zusätzlich ≤ Admin-Kontingent `max_parallel` (falls gesetzt).
  Ist ein gewählter Zugang durch Budget/Pause gesperrt, wird die Anlage abgelehnt.
- **Projekt-Parallelität:** Versuche einer Gruppe belegen **einen** Projekt-Slot
  (sie starten alle vom selben Stand und berühren den Zielbranch nicht). Zählung im
  Claim: `count(DISTINCT COALESCE(attempt_of, id))`; ein Versuch darf starten, wenn
  seine Gruppe schon läuft oder ein Slot frei ist. Race-frei wie bisher
  (Projektzeile gesperrt, frischer Snapshot).
- **Nutzer-Parallelität, Kontingent, Runner-Kapazität** gelten unverändert pro
  Versuch (jeder Versuch ist ein Lauf). Bei Parallelität 1 laufen die Versuche
  nacheinander – die Kostenvorschau sagt das.
- Eine wartende Gruppe (`comparing`) belegt keinen Slot (wie `held`), damit die
  Queue nicht tagelang auf eine Auswahl wartet; Konflikte löst die Merge-Queue wie
  bisher per Fix-Auftrag.

### 2.3 Budget

- Gruppenbudget = Kosten-/Token-Limit des Elternauftrags (bzw. Projekt-Standard).
  Jeder Versuch erbt dasselbe Limit (kein Versuch allein darüber).
- **Live:** gemeinsames Konto pro Gruppe im Prozess (`AppState::bon_ledger`):
  in der DB verbuchte Kosten aller Versuche der Runde + Live-Deltas aller laufenden
  Läufe. Überschreitet die Summe das Limit, stoppt der auslösende Lauf sofort und
  `bon::abort` stoppt **alle** Versuche (laufende per Abbruch-Token, wartende direkt);
  sie enden `blocked` (`limit_cost`/`limit_tokens`), nicht „abgebrochen“.
- **Nach jedem Lauf** zusätzlich die DB-Summe (`usage_log`) prüfen – deckt mehrere
  App-Instanzen ab.
- Stoppt ein Versuch an seinem eigenen Limit, ist das ebenfalls ein Gruppen-Abbruch.
- Fertige Kandidaten bleiben nach einem Abbruch wählbar (Geld ist ausgegeben), es
  läuft aber keine Auto-Auswahl mehr (sie würde weiteres Geld kosten).

### 2.4 Automatische Auswahl (Review-Agent)

- Ein direkter API-Aufruf (`llm::call`, kein Werkzeug, eine Anfrage) mit Diffs
  (gekürzt), Verify-/Review-Ergebnis und Kosten je Kandidat; Antwort JSON
  `{"winner": k, "reason": "…"}`; Kosten als Schritt `select` verbucht.
- **Wahl des Prüfers** (`bon::pick_selector`, rein, deterministisch):
  1. Kandidaten = Zugänge mit Direkt-API und Modell mit bekannter Stufe,
     Datenstandort des Projekts erlaubt;
  2. **nie schwächer** als der stärkste Ausführer (Stufe ≥ max. Stufe der Versuche;
     unbekannte Ausführer-Stufe ⇒ nur oberste Stufe);
  3. **bevorzugt anderer Anbieter** (Modellfamilie) als alle Ausführer
     (Self-Preference-Bias); gibt es keinen, derselbe Anbieter mit Hinweis;
  4. sonst keine Auto-Auswahl möglich ⇒ Anlage mit `auto_select` wird abgelehnt.
  Ist im Projekt-Routing ein Review-Schritt mit Direktzugang gesetzt und erfüllt er
  2., hat er Vorrang (Routing baut auf WP13 auf).
- Ein Kandidat ⇒ ohne Aufruf gewählt („einziger erfolgreicher Versuch“).
- Lease `bon_select_at` (10 min) gegen Doppelaufrufe; der Scheduler-Tick holt
  liegengebliebene Auswahlen nach (Neustart).

### 2.5 Kostenvorschau

`POST /api/estimate` bekommt optional `attempts` (Liste von Zugängen/Modellen) und
`auto_select`; Antwort zusätzlich `per_attempt` und Summen = Plan einmal +
Σ (Umsetzung + Review) je Versuch + Auswahl. UI: „3 × ~0,40 $ ≈ 1,20 $“.

### 2.6 Analytics (WP10)

Versuche (`kind='attempt'`) zählen **nicht** als eigene Aufträge in Ergebnis-,
Erstversuchs- und Dauer-Kennzahlen; der Elternauftrag zählt einmal. Kosten
(`usage_log`) bleiben vollständig. Neuer Block `best_of_n`: Gruppen, Versuche,
Kandidaten, Auswahl manuell/automatisch. Metrik `queue_bon_selections_total{by}`.

## 3. Preview

### 3.1 Einstellungen pro Projekt (`preview_settings`)

`enabled`, `command` (Shell, ≤ 2000 Z.), `port` (1024–65535), `ready_path`
(beginnt mit `/`), `ready_timeout_secs` (5–600), `max_lifetime_secs` (60–3600),
`screenshots` (bool), `screenshot_paths` (≤ 5), `ui_globs` (Dateimuster, die als
UI-Änderung gelten). Nur Eigentümer/Bearbeiter ändern.

### 3.2 Runner (privilegierte Arbeit bleibt im Runner)

- Dev-Server = Sandbox-Lauf im **Arbeitsbaum des Auftrags**, gleiche Engine, gleiches
  Image, gleiche Limits (`--memory/--cpus/--pids-limit`, `--cap-drop ALL`,
  `no-new-privileges`), **`--network none`, kein Egress** (Abhängigkeiten kommen aus
  der Setup-Phase), **kein `-p`/Port-Binding**.
- Erreichbarkeit: umgekehrte Brücke im Container (`node`, wie die Egress-Brücke):
  Unix-Socket in einem eigenen Preview-Verzeichnis → `127.0.0.1:<port>` im Container.
  Engine `none` (nur Entwicklung): Prozess auf dem Host an `127.0.0.1:<freier Port>`
  (`PORT`/`HOST` gesetzt), eigene Prozessgruppe.
- Ready-Check: `GET ready_path` alle 250 ms bis Status < 500, sonst Abbruch bei
  Timeout oder Prozessende. Registry mit Ablaufzeit; Reaper alle 5 s; `stop`;
  höchstens `RUNNER_MAX_PREVIEWS` gleichzeitig. Beenden = SIGTERM, nach 3 s SIGKILL an
  die Prozessgruppe bzw. `docker/podman kill` + `rm -f`.
- Screenshots: Headless-Chromium. Container: `exec` in denselben Container (gleicher
  Netz-Namensraum, weiter `--network none`), Browser-Binary per
  `RUNNER_PREVIEW_BROWSER`. Engine `none`: Host-Chromium `RUNNER_CHROMIUM`.
  Desktop 1280×800 und Mobil 390×844 (iPhone-UA), PNG geprüft (Signatur, ≤ 8 MB).
- HTTP-Weiterleitung `ANY /v1/previews/{name}/http/{*path}` (Runner-Token), Body
  ≤ 2 MiB, Antwort ≤ 20 MiB, 30 s.

### 3.3 App: authentifizierter Proxy, eigene Origin

- **Eigene Origin pro Vorschau:** `PREVIEW_ORIGIN=https://*.preview.example.com`;
  jede Vorschau bekommt eine zufällige 128-Bit-Kennung als Subdomain
  (`<cap>.preview.example.com`). Anfragen mit passendem `Host` gehen **nur** an den
  Proxy (nie an API/SPA). Gespeichert wird nur der SHA-256 der Kennung.
- **Warum nicht ein Pfad unter der App-Origin:** Gleiche Origin hieße, dass genau
  eine Barriere (CSP-Sandbox) zwischen fremdem JS und der App steht. Außerdem lösen
  Dev-Server Assets absolut (`/assets/x.js`) auf – unter einem Pfadpräfix bricht das.
  Eine eigene Host-Origin trennt per Same-Origin-Policy (kein DOM-, Storage-,
  Service-Worker-Zugriff auf die App), absolute Pfade funktionieren.
- **Cookie-Isolation:** Der Proxy leitet **nie** `Cookie`/`Authorization` weiter und
  entfernt `Set-Cookie` (sonst könnte die Vorschau Cookies für den gemeinsamen Host
  setzen – Cookies trennen nicht nach Port). App-Cookies sind host-only, `SameSite=Lax`.
- **CSP-Sandbox zusätzlich:** jede Antwort trägt
  `Content-Security-Policy: sandbox allow-scripts allow-forms allow-popups allow-modals; frame-ancestors <App-Origin>`,
  dazu `nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`. Damit
  ist das Dokument auch bei direktem Aufruf eine *opake* Origin: Anfragen an die App
  sind cross-site, Lax-Cookies gehen nicht mit (kein CSRF, auch wenn die
  Preview-Domain dieselbe registrierbare Domain hätte). Das iframe hat zusätzlich
  `sandbox="allow-scripts allow-forms"` (ohne `allow-same-origin`).
- **Nur Projektmitglieder:** Die Kennung (= URL) gibt es nur über die
  authentifizierte API an Mitglieder (Viewer+); Starten/Stoppen Bearbeiter+. Jede
  Anfrage prüft: Vorschau aktiv, nicht abgelaufen, Projekt existiert. Die Kennung ist
  ein Bearer-Geheimnis für die Lebensdauer der Vorschau (max. 60 min) – **bewusst**:
  eine opake Origin bekommt keine Cookies, und nutzerspezifische Kennungen würden je
  Nutzer eine andere Origin erfordern (geht, aber dann nicht mehr cachebar und ohne
  Mehrwert gegenüber einer kurzlebigen, nur Mitgliedern ausgehändigten Kennung).
- Ohne `PREVIEW_ORIGIN` gibt es keine Live-Vorschau (UI erklärt das); Screenshots
  funktionieren trotzdem (sie brauchen keine Browser-Origin).

### 3.4 Screenshots im Ablauf

Nach bestandenem Verify, vor dem Review, wenn aktiviert und die geänderten Dateien
`ui_globs` treffen: Runner startet die Vorschau, nimmt Desktop+Mobil je Pfad auf,
beendet sie. PNGs in `task_screenshots` (bytea, ≤ 8 MB, max. 10 pro Auftrag),
ausgeliefert nur an Mitglieder mit `image/png`, `nosniff`, CSP `sandbox`. Fehler
brechen den Auftrag nicht ab (Ereignis im Verlauf).

## 4. Modell-Routing (Regeln, Entscheidung, Erklärung)

- `Routing.rules: Vec<RoutingRule>` (Default leer ⇒ altes Verhalten), max. 20.
  Regel: Name, Bedingungen (`kinds` ⊂ {task, fix}, `max_chars`, `min_chars`,
  `keywords` (beliebiges, Groß/klein egal, in Titel/Prompt), `has_attachments`),
  Ziel (Zugang + Modell). Erste passende Regel gewinnt.
- **Entscheidung** `routing::decide(&DecisionInput) -> RoutingDecision` (rein,
  deterministisch): explizite Wahl > Eskalation (Fix) > erste Regel > Route der
  Umsetzung > Standard-Anbieter. Danach prüft `resolve_execute` wie bisher Zugang,
  Datenstandort, Pflichtmodell.
- `RoutingDecision { step, target, source, rule, reason, facts }` (geteilter Typ in
  `queued-api`) wird am Auftrag gespeichert (`tasks.routing_decision`) und im
  Auftrag angezeigt („Warum dieses Modell?“). **Schnittstelle für WP15:** der
  Chooser liefert künftig dieselbe Struktur mit eigener `source`.
- Fix-Aufträge: Eskalation wie bisher, jetzt mit Begründung; Regeln mit `kinds=fix`
  greifen, wenn keine Eskalation greift; sonst Zugang/Modell des gescheiterten
  Auftrags („geerbt“).
- Validierung beim Speichern: Zugang vorhanden, Datenstandort, Pflichtmodell
  (opencode/Bedrock), Grenzen der Felder. Presets bleiben die WP13-Presets; Regeln
  hängen an der gewählten Route, nichts wird doppelt definiert.

## 5. Datenmodell (Migration `0015_best_of_n_preview_routing.sql`)

```sql
ALTER TABLE tasks
  ADD attempts INT NOT NULL DEFAULT 1 CHECK (attempts BETWEEN 1 AND 8),
  ADD attempt_of UUID REFERENCES tasks(id) ON DELETE CASCADE,
  ADD attempt_no INT CHECK (attempt_no BETWEEN 1 AND 8),
  ADD bon JSONB,                 -- Elternauftrag: Wahl der Zugänge, auto, Auswahl
  ADD bon_select_at TIMESTAMPTZ, -- Lease der Auto-Auswahl
  ADD routing_decision JSONB,
  CHECK ((attempt_of IS NULL) = (attempt_no IS NULL));
usage_log.step += 'select'
preview_settings(project_id PK, …)
previews(id, project_id, task_id, started_by, cap_hash UNIQUE, runner_name, status, expires_at, …)
task_screenshots(id, task_id, project_id, viewport, path, png, width, height, created_at)
```

Neue Status: `comparing` (Gruppe), `candidate` (erfolgreicher Versuch). Neue Art:
`attempt`. Keine CHECKs auf Status/Art (wie bisher).

## 6. API (neu/geändert)

- `POST /api/tasks` + `attempts`, `attempt_targets[]`, `auto_select`.
- `GET /api/tasks/{id}/attempts` – Vergleich: je Versuch Status, Zugang/Modell,
  Diffstat, Verify-Ergebnis, Review-Urteil, Kosten, Dauer, Screenshots.
- `POST /api/tasks/{id}/select {attempt_id}`.
- `POST /api/estimate` + `attempts`, `auto_select`.
- `GET/PUT /api/projects/{id}/preview`, `POST /api/tasks/{id}/preview` (start),
  `GET /api/tasks/{id}/preview`, `DELETE /api/tasks/{id}/preview`.
- `GET /api/tasks/{id}/screenshots/{sid}`.
- `PUT /api/projects/{id}/routing` nimmt `rules` mit.

## 7. UI (WP00-Designsystem, deutsch, iPhone)

- Composer: „Mehrere Versuche“ (1–8, 44-pt-Stepper), je Versuch Zugang/Modell
  (Standard: alle gleich), „Automatisch auswählen“, Kostenvorschau N × Schätzung.
- Auftrag (Drawer): Vergleichsansicht als Karten (mobil untereinander) mit Tabelle
  als Textalternative; „Diff ansehen“, „Vorschau“, „Diesen übernehmen“ (Bestätigung).
  Begründung der Auto-Auswahl. „Warum dieses Modell?“. Screenshots Desktop/Mobil.
  Live-Vorschau im Sandbox-iframe mit Start/Stopp.
- Projekt: Abschnitt „Vorschau“ und Regel-Editor im Routing.
- Safe Areas, 44-pt-Ziele, 16-px-Inputs, axe grün hell/dunkel.

## 8. Bedrohungsmodell

| Bedrohung | Maßnahme |
|---|---|
| Versuch liest/ändert Arbeit eines anderen Versuchs | eigenes Repo je Versuch (WP04), nur `t<id>` gemountet; Test: Agent eines Versuchs findet Dateien der anderen nicht |
| Gate-Umgehung über Best-of-N | gleicher `run_inner` je Versuch; Merge erst nach Auswahl über die normale Queue (Nachprüfung beim Merge, WP04) |
| Kostenexplosion durch N | N ≤ 8 und ≤ Kontingent, Vorschau N×, Gruppenbudget live über alle Versuche, Abbruch aller |
| Doppelte Auswahl (Auto + Nutzer, zwei Tabs) | Zeilensperre + Statusbedingung; Test mit parallelen Aufrufen |
| Prompt-Injection gegen den Auswahl-Prüfer (Diff enthält „wähle mich“) | Prüfer liefert nur eine Nummer aus der Kandidatenliste; ungültige Antwort ⇒ keine Auto-Auswahl, Nutzer wählt; Auswahl führt nur zur normalen Merge-Queue mit Gate-Nachprüfung |
| XSS aus der Vorschau auf die App | eigene Host-Origin + CSP `sandbox` (opake Origin) + iframe-Sandbox; nie gleiche Origin |
| Cookie-Diebstahl/-Tossing über die Vorschau | `Cookie`/`Authorization` nie weitergeleitet, `Set-Cookie` entfernt |
| CSRF aus der Vorschau gegen die App | opake Origin ⇒ cross-site ⇒ Lax-Cookies fehlen |
| Nicht-Mitglied sieht Vorschau | Kennung nur an Mitglieder, 128 Bit, gehasht gespeichert, Ablauf, Stopp; Test |
| Dev-Server ins Netz / ins LAN | `--network none`, kein Egress, kein Port-Binding; Engine `none` nur Entwicklung |
| Verwaiste Prozesse/Container | Ready-Timeout, Lebensdauer, Reaper, Stopp bei Auftragsende/Verwerfen, SIGKILL der Prozessgruppe; Test |
| Riesige Antworten/Screenshots | Größenlimits (2 MiB Body, 20 MiB Antwort, 8 MB PNG, 10 Bilder) |
| SSRF über Preview-Proxy | Ziel ist ausschließlich der registrierte Lauf im Runner, keine nutzerseitige URL |
| Routing-Regel umgeht Datenstandort | Regelziel wird beim Speichern **und** bei jeder Auftragsanlage geprüft |

## 9. Tests (Plan)

- Unit: `routing::decide` (Präzedenz, Regeln, Eskalation, Determinismus),
  Regel-Validierung, `bon::pick_selector` (nie schwächer, anderer Anbieter),
  Selector-Antwort-Parser, Preview-Header-Filter/CSP, Host-Erkennung, Runner-Brücke,
  Container-Kommando ohne Port-Binding.
- Integration (App + Runner in-process + Postgres, Mock-Agent): Isolation der
  Versuche, Auswahl → Merge (commit) und → Push (branch), Verwerfen → Aufräumen
  (Arbeitsbäume, Branches), Budget-Abbruch über alle Versuche, Auto-Auswahl mit
  Fake-Review-Server, parallele Auswahl genau einmal, Projekt-Slot-Zählung,
  Preview: nur Mitglieder, CSP/Origin-Isolation, Cookie-Filter, Timeout-Cleanup,
  Screenshot-Erzeugung (Headless-Chromium), Regeln inkl. Eskalation am Auftrag.
- E2E: Vergleichsansicht + Auswahl (Desktop, iPhone hoch/quer) mit axe; Vorschau
  im iframe (Origin `*.localhost`).
- Mutationen: Isolation, Auswahl-Sperre, Budget-Abbruch, Cookie-Filter/CSP,
  Mitglieder-Prüfung, Prozess-Cleanup, Selector „nie schwächer“, Regel-Präzedenz.

## 10. Risiken

- **Viele Statuspfade:** `candidate`/`comparing` müssen überall sauber auftauchen
  (Listen, Analytics, Recover, Sweep). Gegenmittel: Grep über alle Status-Listen,
  Tests für Recover/Sweep.
- **Docker/Podman-Pfad der Vorschau ist hier nicht ausführbar** (Tests mit Engine
  `none`); Kommando-Aufbau per Unit-Test, Image-Erweiterung (Chromium) nicht gebaut.
- **Live-Budget nur pro App-Instanz**; mehrere Instanzen ⇒ Nachprüfung nach jedem
  Lauf (DB). Überziehung bis zu einem laufenden Lauf je weiterer Instanz möglich.
- **Dev-Server, die WebSockets brauchen (HMR),** funktionieren im Proxy nicht
  (bewusst: nur HTTP, keine Upgrades).
