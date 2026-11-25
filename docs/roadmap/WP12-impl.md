# WP12 – Umsetzung: Best-of-N, Preview & Modell-Routing

Basis: `mvp` @ `5241d25` (WP11), Zwischenstand vom Koordinator als WIP-Commit `ae7eedf` gesichert.
Plan: `WP12-plan.md`. Hier steht, was gebaut wurde, warum, und wo vom Plan abgewichen wurde.

## 1. Datenmodell (Migration `0015_best_of_n_preview_routing.sql`)

- `tasks`: `attempts` (1–8), `attempt_of` (→ Gruppe, `ON DELETE CASCADE`), `attempt_no`, `bon` (JSON: Zugänge der Versuche, Auto-Auswahl, Prüfer + Begründung, Rundenbeginn, Auswahl, Budget-Abbruch, Auswahlfehler, „bereit“ gemeldet), `bon_select_at` (Lease der Auto-Auswahl), `routing_decision` (JSON). CHECKs: `attempt_of`/`attempt_no` nur gemeinsam, ein Versuch ist nie selbst Gruppe.
- `usage_log.step` + `select`.
- `preview_settings` (pro Projekt), `previews` (Kennung nur als SHA-256 + verschlüsselt mit dem Master-Key zum erneuten Anzeigen), `task_screenshots` (PNG ≤ 8 MB).
- Neue Status `comparing` (Gruppe) und `candidate` (erfolgreicher Versuch), neue Art `attempt` – wie bisher ohne CHECK.

## 2. Modell-Routing (`routing.rs`, `queued-api`)

- `Routing.rules: Vec<RoutingRule>` (Default leer, `skip_serializing_if` ⇒ alte Zeilen unverändert). Regel = Name + `RuleWhen` (`kinds`, `max_chars`, `min_chars`, `keywords`, `has_attachments`) + `Target`.
- `routing::decide(routing, ask, explicit, default)` ist rein und deterministisch: explizit › erste passende Regel › Route der Umsetzung › Standard-Anbieter. `decide_fix`: Eskalation › Regel mit `kinds=fix` › geerbt. `check_decision` prüft wie bisher Zugang, Datenstandort, Pflichtmodell und ergänzt den Zugangsnamen in der Begründung.
- `resolve_execute` hat dafür eine neue Signatur (`Explicit`, `Option<&Ask>`); alle Aufrufer angepasst (API, Trigger, Intake, Steering, Eval, Schätzung). Regeln greifen nur, wo Merkmale bekannt sind (Composer/API, Trigger, Intake); Fortsetzungen und PR-Folgeaufträge erben den Zugang und speichern das als `inherited`.
- `create_fix` entscheidet über `decide_fix`; eine Fix-Regel mit nicht nutzbarem Zugang fällt mit Begründung auf „geerbt“ zurück.
- `RoutingDecision` (`step`, `source`, `target`, `rule`, `rule_index`, `escalation_level`, `reason`, `facts`) wird an **jedem** neuen Auftrag gespeichert (auch OnEmpty, Folgeaufträge, Eval). **Schnittstelle für WP15:** dieselbe Struktur, eine neue `DecisionSource` für den Chooser.
- `validate_rules` (max. 20, Name, Arten, Grenzen, Stichworte, Zugang, Datenstandort, Pflichtmodell) läuft in `routing::validate`, also bei jedem `PUT …/routing`.
- UI: Karte „Regeln für die Modellwahl“ (Projekt → Einstellungen), „Warum dieses Modell?“ im Auftrag; eine Vorlage zu übernehmen behält die Regeln.

## 3. Best-of-N (`bon.rs`)

- **Anlage** (`POST /api/tasks` mit `attempts`, `attempt_targets`, `auto_select`): `plan_group` prüft N (2–8), Admin-Kontingent `max_parallel`, je Versuch Zugang/Datenstandort/Pflichtmodell (Eintrag ohne Zugang = wie der Auftrag), pausierte/ausgeschöpfte Zugänge (`worker::Blocked::blocks`), bei Auto-Auswahl den Prüfer. Gespeichert in `tasks.bon`.
- **Auffächern** (`fan_out`, aus `run_inner` nach Plan-Modus und Planungsschritt): eine Transaktion mit Zeilensperre (`status='running' FOR UPDATE`), N Versuche (Prompt, Kontext, Plan, Limits, Testauftrag-Flag), `parent_id` = Gruppe (dadurch erben Versuche die Anhänge über die bestehende Kette, siehe `attachments::source_tasks`), Positionen vorn (Versuch 1 zuerst), Gruppe → `comparing`. Rundenbeginn = **Transaktionszeit** (`SELECT now()`), weil `created_at` der Versuche `now()` ist (siehe Test-Fund T1).
- **Ende eines Versuchs**: `runner_gate::finish_flow` → `candidate` (statt Merge-Queue, `gate_pending` bleibt für die spätere Übernahme). Hat die Gruppe inzwischen gewählt (andere Instanz) → verworfen. `finish_failed` → `failed` ohne Fix. Am Ende von `run_task` für Versuche: Budget-Stopp eines Versuchs ⇒ `bon::abort`, gebuchte Gruppensumme prüfen, `bon::settle`.
- **Settle**: offene Versuche ⇒ warten; keiner erfolgreich ⇒ Gruppe `failed` (bzw. `blocked` bei Budget-Abbruch); sonst Auto-Auswahl oder einmalige Meldung „bitte auswählen“.
- **Auswahl** (`select`, Nutzer und Auto über denselben Pfad): Zeilensperren auf Gruppe und Versuch, Status- und Rundenprüfung; Gewinner `candidate → merging` (bestehende Merge-Queue bzw. PR-Modus über `merge_inner`/`publish`) oder `done` ohne Änderungen; Verlierer `cancelled` („Verworfen: Versuch k wurde übernommen“), laufende per Abbruch-Token, gehaltene Diffs verworfen, Batch-Jobs abgebrochen, Vorschauen gestoppt, Arbeitsbaum + Branch entfernt. Gruppe `done` mit `bon.choice`. Audit `approval.attempt_selected`.
- **Auto-Auswahl** (`pick_selector`, `auto_select`): Kandidaten = Zugänge mit Direkt-API × Modelle der Preistabelle je Familie (`families_of`) + der Review-Schritt des Routings, gefiltert auf Stufe ≥ stärkster Ausführer (unbekannt ⇒ oberste), sortiert: andere Familie zuerst, konfigurierter Review-Schritt, niedrigste ausreichende Stufe, Preis, Reihenfolge. Aufruf mit abgegrenzten Kandidaten (`prompt::user_block`, Nonce), Diff je Kandidat gekürzt, Antwort muss eine Nummer der Liste sein, Kosten als Schritt `select` auf der Gruppe. Lease 10 min; `bon::sweep` (Scheduler-Tick) holt liegengebliebene Gruppen nach.
- **Budget**: `AppState::bon_ledger` (gebuchte Summe der Runde + Live-Deltas laufender Läufe). Jeder Lauf eines Versuchs meldet sich an (`cap_for`), jedes Delta geht in `ledger_delta`; überschritten ⇒ Lauf gestoppt, `abort` für alle (wartende sofort `blocked`, laufende per Token → `finish_cancelled` erkennt den Gruppen-Abbruch und setzt `blocked` statt `cancelled`). `record_cost` bucht und nimmt den Live-Eintrag heraus.
- **Scheduler**: Versuche einer Gruppe teilen sich einen Projekt-Platz (`count(DISTINCT COALESCE(attempt_of, id))`; ein Versuch darf starten, wenn seine Gruppe schon läuft). Nutzer-Parallelität, Kontingent und Runner-Kapazität unverändert pro Versuch.
- **API**: `GET /api/tasks/{id}/attempts` (Vergleich: Diffstat aus dem Runner-Diff, Verify-Zustand, Review, Kosten, Tokens, Dauer, Screenshots), `POST /api/tasks/{id}/select`, Abbrechen einer Gruppe/eines Kandidaten, Wiederholen (Gruppe = neue Runde; Versuch nur solange die Gruppe offen ist), Bearbeiten von Zugang/Modell einer Gruppe abgelehnt.
- **Schätzung**: `POST /api/estimate` mit `attempts`/`auto_select` (Planung einmal, Umsetzung + Review je Versuch, Auswahl), Antwort `attempts` je Versuch.
- **Analytics**: Versuche aus Ergebnis-, Dauer-, Projekt- und Tageszahlen ausgenommen, Top-Kosten fassen Versuche unter der Gruppe zusammen, neuer Block `best_of_n`. Metriken `queued_bon_groups_total`, `queued_bon_selections_total{by}`, `queued_bon_budget_aborts_total`, Gauges um `comparing`/`candidate` ergänzt.
- **CLI**: `queuectl add --attempts N [--auto-select]`, Statustexte.

## 4. Preview

- **Runner** (`runner/src/preview.rs`): Start mit Validierung (Name, Kommando, Port ≥ 1024, Pfade, Zeitgrenzen, keine Variablen `PORT/HOST/HOME/PATH`), nur in Task-Arbeitsbäumen, nicht während dort ein Lauf ist; Container-Argumente ohne Netz und ohne Port-Freigabe, Brücke Unix-Socket → `127.0.0.1:$PORT`; Engine `none` mit freiem Loopback-Port, leerer Umgebung + `PATH`, eigener Prozessgruppe. Ready-Check per HTTP (hyper, Unix- oder TCP-Verbindung), Screenshots mit Headless-Chromium (Proxy auf toten Port, nur Loopback erreichbar), PNG-Prüfung. Registry mit Ablauf, Reaper (2 s), `stop`, `stop_workspace` (Exec-Start und Entfernen eines Arbeitsbaums stoppen Vorschauen darin). Routen `POST …/tasks/{task}/preview`, `GET /previews`, `POST /previews/{name}/stop`, `POST /previews/{name}/http` (Methode/Pfad in Headern, Status in `x-queue-preview-status`).
- **App** (`preview.rs`, `api/preview.rs`): Einstellungen, Start (Bearbeiter, Arbeitsbaum muss bestehen: `candidate|held|waiting_input`, max. 2 je Projekt), aktueller Stand mit Link (Mitglieder), Stopp, Ablauf-Sweep, Stopp bei Run-Start und `cleanup_task`. Middleware vor dem ganzen Router: Vorschau-Host ⇒ nur Proxy. Header-Filter in beide Richtungen (gemeinsam in `queued-common`, App und Runner filtern), CSP `sandbox …; frame-ancestors <App>`, `nosniff`, `no-referrer`, `no-store`, `X-Robots-Tag`, `Location` auf die eigene Origin umgeschrieben. Screenshots nach bestandenem Verify bei UI-Dateien (Globs), ersetzen die des vorigen Laufs, Fehler nur als Ereignis.
- **Konfiguration**: `PREVIEW_ORIGIN` (App), `RUNNER_CHROMIUM`, `RUNNER_CHROMIUM_NO_SANDBOX`, `RUNNER_PREVIEW_BROWSER`, `RUNNER_MAX_PREVIEWS` (Runner); Agent-Image um `chromium` erweitert (nicht gebaut).

## 5. Frontend (Leptos)

- Composer: „Mehrere Versuche“ (Stepper 1–8 mit 44-pt-Knöpfen, Zugang/Modell je Versuch, „Besten Versuch automatisch wählen“, Hinweis bei Parallelität 1), Kostenvorschau „N Versuche zusammen: ca. … (je ca. …)“.
- Aufträge: Abschnitt „Mehrere Versuche“, Tags „N Versuche“/„Versuch k“, Versuche nicht im Verlauf (stehen im Vergleich).
- Auftrag: Vergleich (Karten + Tabelle als Textalternative, Status, Prüfer, Summen), „Diesen übernehmen“ mit Bestätigung, Rücksprung vom Versuch zum Vergleich, „Warum dieses Modell?“, Screenshots (Desktop/Mobil, in voller Größe in neuem Tab), Vorschau im iframe `sandbox="allow-scripts allow-forms"` mit Start/Stopp/Ablaufzeit.
- Projekt: „Regeln für die Modellwahl“, „Vorschau & Screenshots“.
- Reine Logik in `logic/bon.rs` (getestet).

## 6. Weitere Änderungen

- `e2e/scripts/stack.sh`: `PREVIEW_ORIGIN=http://*.localhost:$APP_PORT`, `RUNNER_CHROMIUM` (Playwright-Headless-Shell), ohne Chromium-Sandbox nur als root.
- Testharness: `start_full` (Runner-Konfiguration), Fake-LLM beantwortet Auswahl-Anfragen.
- **WP10-Test repariert:** `notifications::daily_summary_is_sent_once_across_instances_and_restarts` schlug tageszeitabhängig fehl (auch auf der Basis `5241d25`, nachgeprüft): `notify::tick` startet die Tageszusammenfassung zusätzlich mit der echten Uhr; lief der Test im echten Fälligkeitsfenster (08:00–20:00 Berlin), entstand eine dritte Zeile. Die echten Tage werden im Test jetzt als erledigt vorbelegt; Produktcode unverändert.

## 7. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| Gruppe bleibt bis zum Merge des Gewinners offen | Gruppe ist mit der Auswahl `done`, der Gewinner läuft als normaler Auftrag weiter | Merge-Fehler des Gewinners erzeugen einen Fix-Auftrag am Gewinner; eine Gruppe, die auf eine Fix-Kette wartet, hätte einen zweiten Lebenszyklus gebraucht. |
| Pfad-Variante der Vorschau als Rückfall | nur eigene Origin (`PREVIEW_ORIGIN`), sonst nur Screenshots | Gleiche Origin hätte eine einzige Barriere (CSP-Sandbox) und bricht absolute Asset-Pfade; siehe Plan §3.3. |
| Live-Budget prüft per Lauf und DB | zusätzlich Gruppen-Abbruch, wenn ein Versuch an seinem eigenen Limit stoppt | Jeder Versuch erbt das Gruppenlimit; sein eigener Stopp bedeutet dasselbe Budget. |
| Verify-Ergebnis gespeichert | aus Status/Fehler/Review abgeleitet (`verify_state`) | Kein neues Feld nötig, deterministisch, getestet. |
| Kandidaten-Arbeitsbäume aufräumen nach Frist | bleiben bis Auswahl/Verwerfen | Vorschau und Diff brauchen sie; wie gehaltene Aufträge (WP03). Offen benannt. |
