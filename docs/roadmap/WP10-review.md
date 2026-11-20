# WP10 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP10-Diff auf `mvp` (Backend App/Runner/Common/API, Migration 0013, Frontend, E2E, Doku). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP10), Auftrag des Koordinators, Report P1.10/P1.13, offene Funde WP07-N11 und WP09-N11. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und durch Tests abgesichert (Gegenproben in `WP10-test.md`, Abschnitt 2). Die ehrliche Grenze: Weder ein echter OTLP-Collector noch ein echtes Prometheus noch ein echter Push-Dienst waren erreichbar – das Format ist gegen die offiziellen Proto-Typen, einen eigenen Exposition-Validator und RFC-8291-Testschlüssel geprüft, nicht gegen die Produkte.

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Doppelversand bei mehreren Instanzen:** Outbox-Durchlauf beanspruchte 50 Zeilen mit 60 s Lease, jede Zustellung darf 15 s dauern ⇒ nach 60 s hätte eine zweite Instanz noch nicht bearbeitete Zeilen erneut beansprucht. | Code-Review `notify::tick` | 10 Zeilen je Durchlauf, Lease 300 s (10 × 15 s < 300 s), Konstante dokumentiert | `daily_summary_is_sent_once_across_instances_and_restarts` (Lease einer „abgestürzten“ Instanz, parallele Ticks zweier Instanzen ⇒ genau 1 ntfy-Nachricht) |
| B2 | **SSRF über Benachrichtigungsziele (Bestand seit MVP):** ntfy-URL und Web-Push-Endpunkt sind Nutzereingaben und wurden ohne Prüfung vom App-Server aufgerufen (Loopback, LAN, Cloud-Metadaten). Mit dem neuen Webhook-Kanal wäre das ein drittes Loch gewesen. | Code-Review `notify.rs`/`push.rs` | Alle drei Kanäle über `llm::check_target` + `llm::client` (Resolver prüft beim Verbinden, Weiterleitungen geprüft); LAN nur mit `ALLOW_PRIVATE_NOTIFY_TARGETS`; Webhook-URL schon beim Speichern geprüft, `http://` nur mit dem Schalter | `outbox_retries_transient_failures_and_blocks_internal_targets` (Fake-ntfy auf 127.0.0.1 bekommt 0 Anfragen, Webhook auf 127.0.0.1/`http://` ⇒ 400), `settings_api_validates_…` (`169.254.169.254` ⇒ 400) |
| B3 | **90-Tage-Auswertung wäre leer gewesen:** `budget::prune` löschte `usage_log` nach 35 Tagen. | Lesen von `worker.rs` vor dem Bau | `USAGE_RETENTION_DAYS` (Standard 400, min. 35); `provider_pauses` mit derselben Frist | Lasttest über 120 Tage Daten, 90-Tage-Abfrage |
| B4 | **Gescheiterte Aufträge als erfolgreicher Trace:** Ein per Contract „failed“ gemeldeter Auftrag (Lauf selbst erfolgreich) hätte einen Wurzelspan ohne Fehler erzeugt. | Test `failed_runs_mark_spans_without_messages` war rot | Wurzelspan `ERROR` + `error.type=failed|blocked` nach Endstatus | derselbe Test grün |
| B5 | **Panic im Header-Parser:** `pct_decode` schnitt `&s[i+1..i+3]` – mitten in einem Mehrbyte-Zeichen (`%äb`) ⇒ Panic beim Start. | Selbst-Review | Byteweise Hex-Dekodierung | Unit `percent_decoding` |
| B6 | **WP09-N11:** `GET /api/tasks` ließ `merging` aus – API-Nutzer und die UI nach einem Neuladen sahen Aufträge in der Übernahme nicht (die Liste „Wird übernommen“ der Queue-Seite speist sich aus genau dieser Antwort). | Offener Fund | `merging` in der aktiven Menge | `task_list_includes_merging` |
| B7 | **Diagrammfarben nicht barrierefrei:** zuerst geplantes Grün/Rot-Paar fiel im Dunkelmodus durch (Helligkeitsband, CVD ΔE 7,9 deutan). | dataviz-Validator | Blau/Orange `--series-1/2`, beide Modi gegen die echten Flächen geprüft, plus Legende mit Text und Tabelle | Validator-Ausgabe (Test-Bericht), axe grün |
| B8 | **iPhone: Karten ohne Innenabstand, Betrag lief aus der Karte.** | Screenshot `portrait-analytics.png` angesehen | `card pad`, Listenlayout der teuersten Aufträge | Mobile-Spec (keine horizontale Scrollbar, Karten innerhalb der Safe-Area) |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Risiko / Begründung |
|---|---|---|
| N1 | Kein echter OTLP-Collector, kein echtes Prometheus, kein echter Push-Dienst getestet. | Format gegen offizielle Proto-Typen bzw. eigenen Validator; Collector-spezifische Eigenheiten (z. B. Header-Formate) unentdeckt. |
| N2 | `execute_tool`-Dauer ist eine Näherung (bis zum nächsten Ereignis des Agenten); die CLIs melden kein einheitliches Werkzeug-Ende. | Dauer kann um die Modell-Denkzeit zu lang sein. Dokumentiert. |
| N3 | Werkzeugnamen kommen aus dem CLI-Stream; MCP-Konfiguration eines Repos kann Namen beeinflussen. Bereinigt (Zeichensatz, 64 Zeichen), in `task_runs.tools` auf 50 Namen begrenzt, in Metriken gar nicht verwendet. | Ein Repo kann sinnlose, aber harmlose Namen in Traces bringen. |
| N4 | Runner exportiert keine Traces; kein Kontext-Weiterreichen (`traceparent`) an den Runner. | Prozessdauer im Runner steht als Metrik, nicht als Span. |
| N5 | Fenster für Doppelzustellung: Absturz zwischen „extern gesendet“ und „als gesendet markiert“. | At-least-once ist für Benachrichtigungen vertretbar; Webhook-Empfänger bekommen `delivery` zum Entdoppeln. |
| N6 | Digest-Schleife fragt pro aktivierter Person je Tick (15 s) eine `EXISTS`-Zeile ab. | Linear in Personen mit Digest; für selbst gehostete Größen unkritisch. Zeitzonen-Logik bewusst in Rust (chrono-tz), nicht in SQL (Postgres-Zonendatenbank kann abweichen). |
| N7 | Nachholen einer verpassten Zusammenfassung nur am selben lokalen Tag (≤ 12 h). Bei 23:00 also höchstens 1 h. | Dokumentiert; sonst würde nach langem Ausfall eine veraltete Zusammenfassung kommen. |
| N8 | `mine` zählt nur Verbrauch mit Projektzuordnung in aktuellen Mitgliedschaften; die Kostenkarte aus WP13 (`/api/costs`) zählt allen eigenen Verbrauch. | Verbrauch vor WP13 ohne `project_id` und Verbrauch in verlassenen Projekten fehlt in der Auswertung, nicht aber in der Kostenkarte. Bewusst strenger (keine Projektnamen fremder Projekte). |
| N9 | Zähler beginnen bei Neustart bei 0; `queued_tasks` nur für aktive Status. | Prometheus-üblich; beendete Aufträge stehen als Lauf-Zähler bzw. in der Auswertung. |
| N10 | Tagesdiagramm der Kosten nur in USD; CHF steht in der Summe. | Kein belegter Wechselkurs (WP13-N13). |
| N11 | `/readyz` ist öffentlich (über einen Tunnel erreichbar) und verrät „DB“/„Runner“ als ok/fail. | Keine Adressen, Texte oder Versionen; 1 s Cache. Für Load-Balancer nötig. |
| N12 | Benachrichtigungs- und Analytics-Routen sind nicht per API-Token erreichbar. | Bewusst (Konto-Einstellungen); ein Dashboard-Export per Token wäre ein eigener Scope. |
| N14 | „Teuerste Aufträge“ listet bei Kosten 0 (Mock, unbekannte Preise) nach Tokens sortiert – die Überschrift passt dann nur halb. | Kosmetisch; im Screenshot der Tour gesehen. |
| N15 | Die Dauer der Insights/„Dauer bis erledigt“ zählt ab Anlage; ein lange zurückgehaltener Auftrag (Freigabe, Limits) wirkt langsam, obwohl der Agent schnell war. Die Agentenzeit steht daneben, der Hinweis „meiste Zeit gewartet“ erklärt es. | Bewusst. |
| N13 | Webhook-Secret liegt verschlüsselt in der DB; ein Wechsel der URL löscht es, ein neues muss erzeugt werden. | Absicht: ein Secret gehört zu genau einem Empfänger. In der UI erklärt. |

## Geprüft und für gut befunden

- **Keine Inhalte in Spans:** Attributschlüssel sind `&'static str`; Werte nur aus festen Mengen, IDs und Zahlen; Fehlerstatus ohne Nachricht; Integrationstest mit Markern in Prompt, Titel, Projektname, Werkzeug-Argument, Agent-Ausgabe (gefälschtes Token) ⇒ kein Treffer in den rohen Export-Bodys; Gegenprobe (Werkzeug-Zusammenfassung als Attribut) wird erkannt.
- **Metriken ohne IDs:** Test sucht Task-, Projekt-, Nutzer-ID, E-Mail und Titel in `/metrics` (App) bzw. Task-ID-Präfix und Workspace (Runner) – keine Treffer; Deckel gegen Kardinalitätsfehler mit Unit-Test.
- **Zugriff:** `/metrics` ohne Konfiguration 404 (nie SPA), mit Token nur Bearer; Session-Cookie und API-Token öffnen es nicht; Metrik-Token öffnet keine API; Runner-Metrik-Token ≠ `RUNNER_TOKEN` erzwungen, gegenseitig ohne Wirkung (Unit- und Systemtest).
- **Mandantentrennung:** drei Personen, geteiltes Projekt mit Bearbeiter und Betrachter, privates Projekt, Entfernen aus dem Projekt – jede Kombination geprüft (Integration), dazu getrennte Prozesse (System).
- **Null Overhead ohne OTel:** keine Konfiguration ⇒ `st.otel == None`, kein Task gestartet, kein Socket; Span-Namen werden ohne Tracer nicht einmal gebaut (`FnOnce`, Unit-Test mit `unreachable!`).
- **Architektur:** App ohne Host-Rechte; keine Datei-/Prozessarbeit in der App; Runner-Auth-Modell unverändert (`/v1` weiterhin nur mit `RUNNER_TOKEN`); neue Migration 0013, alte unverändert; keine neue Laufzeit-Dependency (nur `opentelemetry-proto` als dev-dependency).
