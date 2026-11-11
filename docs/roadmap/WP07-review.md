# WP07 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP07-Diff gegen `f4cb172` (api, common, app inkl. Migration 0010, neue Crate `queuectl`, Frontend, E2E, README). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP07), Auftrag des Koordinators (Tokens gehasht mit Präfix und Scanner-Regel, feine Scopes pro Endpunkt, Ablauf/Widerruf/`last_used`, keine Eskalation per Token, CSRF unberührt; Webhooks mit HMAC, konstantem Vergleich, Zeit-/Replay-Schutz, Body-Limit, Datenblock, Rate-Limit, GitHub-Filter, belegte Felder; Zeitpläne min. stündlich mit Zeitzone, einmalig, neustartfest, mehrinstanzfähig, Vorlagen; `queuectl`), Report P1.6. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und mit Tests belegt; die komplette Suite ist auf dem finalen Stand zweimal hintereinander grün (`WP07-test.md`). Die ehrlichen Grenzen: Gegen GitHub selbst ist nichts gelaufen (Zustellungen nachgebaut, Formate aus OpenAPI/Doku belegt, ein Wert unbelegt), GitHub-Replays sind nur für die Aufbewahrungszeit (90 Tage) ausgeschlossen, und Tokens sind nicht auf Projekte beschränkbar.

## Akzeptanz gegen WP07

| Anforderung | Status | Beleg |
|---|---|---|
| Zeitpläne pro Projekt (Cron, min. stündlich, Zeitzone) | erfüllt: Minute fest, ≥ 1 h Abstand auch bei 30-min-Umstellung, IANA-Zonen, Sommer-/Winterzeit definiert | Unit `cron_*`, `dst_forward_and_back_in_berlin`, `lord_howe_…`; Integration `schedules_fire_exactly_once…` (Validierung 400) |
| Einmalige Ausführung | erfüllt (Vergangenheit ⇒ 400, genau einmal, danach erledigt, Bearbeiten löst nicht erneut aus) | Integration; System-E2E (Scheduler startet pünktlich) |
| Neustartfest, keine Doppelauslösung, verpasste Läufe definiert, mehrinstanzfähig über DB-Lock | erfüllt: eine Transaktion (Sperre `SKIP LOCKED`, Termin UNIQUE, Auftrag, nächster Termin); Absturz vor Commit ⇒ nichts; zwei Instanzen gleichzeitig ⇒ ein Auftrag; veraltete Instanz ⇒ Termin-Dedupe; nachholen genau einmal bzw. „verpasst“ | Integration `schedules_fire_exactly_once_across_restarts_and_instances`, Unit `missed_slots_collapse_into_one_decision` |
| Task-Vorlagen | erfüllt (Projekt, Rechte, Validierung, benutzte nicht löschbar, Composer „Aus Vorlage“, `queuectl add --template`) | Integration `templates_validation_rights_and_preview`, UI-E2E |
| Persönliche API-Tokens: gehasht, Präfix, Scanner-Regel | erfüllt: SHA-256 von 256 Bit, `qk_` + Prüfsumme, Scanner `queue_key` (auch `qwh_`) | Unit (api, common, app), Integration prüft gespeicherten Hash |
| Feine Scopes pro Endpunkt durchgesetzt | erfüllt: 7 Scopes, Default-Deny-Tabelle | Integration `every_token_route_needs_its_scope` (jede Route mit/ohne Scope) |
| Ablauf, Widerruf, `last_used` | erfüllt (Ablauf Pflicht ≤ 366 Tage, Widerruf sofort, Live-Stream endet mit dem Token) | Integration `expired_revoked_and_malformed_tokens_are_refused`, System-E2E (Widerruf ⇒ Exit 3), UI-E2E |
| Keine Konto-/Token-Verwaltung per Token | erfüllt (nicht gelistet ⇒ 403, Cookie wird bei Bearer ignoriert) | Integration `account_settings_and_token_management…`, `the_token_wins_over_the_cookie…` |
| CSRF der Cookie-Session unberührt | erfüllt: Cookie-Pfad unverändert; neue Cookie-Endpunkte nehmen JSON bzw. keinen Body wie die bestehenden | Code-Review |
| `POST /api/triggers/{id}/fire` mit Token | erfüllt (201/200/409/429 + `Retry-After`, Freitext als Datenblock, nur Eigentümer) | Integration `api_fire_with_token…`, System-E2E `queuectl fire` |
| Generische Webhooks: HMAC-SHA256, konstanter Vergleich, Zeit-/Replay-Schutz, Body-Limit, Rate-Limit | erfüllt (`verify_slice`; Zeitstempel ±300 s und Delivery-ID mitsigniert; Dedupe; 1 MiB; Limit pro Auslöser) | Integration `generic_webhook_signature_window_replay_and_limits`, Unit, System-E2E mit Node-HMAC |
| Payload untrusted, nur als markierter Datenblock (Zufallsgrenze) | erfüllt (Regeln davor, Grenzen/fremde Präfixe entschärft, Secrets geschwärzt, 16 KB; nie im Titel/Intro) | Unit `event_section_marks_payload…`, `intros_never_carry_sender_text`, Integration (Kontext/Titel) |
| GitHub-Events push/issue/PR/check failure mit Filtern, `X-Hub-Signature-256` | erfüllt (Branch, Label, Repository, Bots, Entwürfe; Schleifenschutz; Ping; Form-Kodierung; Dedupe ID + Body; ein Auftrag pro Commit) | Unit `push_filters…`, `issues_and_pull_requests…`, `check_failures…`, Doku-Testvektor; Integration `github_events_are_filtered…` |
| Eigene Felder nur mit Doku belegt, sonst markiert | erfüllt (OpenAPI/Doku per Skript geprüft; `sender.type == "Bot"` als unbelegt markiert) | `WP07-impl.md` §4, README |
| WP05-Webhook-Schnittstelle nutzen, Polling nicht untergraben | erfüllt: nur `pr::poke`; falsche Signatur poked nie | Integration (Poke per `check_run` und `pull_request_review`, nicht bei falscher Signatur) |
| WP06 über API/CLI (Steering, „Weiter mit …“, Plan) | erfüllt (Scope `tasks:steer`; `queuectl steer/continue/plan`) | Integration (Nachricht per Token), System-E2E (`steer`) |
| Rollen/Mandanten wie bestehende APIs | erfüllt (Token = Nutzer, `require`/`own_task` unverändert; Auslöser verbrauchen nie fremde Zugänge) | Integration (Fremde 404, Betrachter 403, Mitglied darf fremden Auslöser nicht starten, Eigentümer verliert Rechte ⇒ aus) |
| `queuectl` add/list/status/watch/cancel (+ steer), Datei/Env, Token nie in argv, 0600, SSE oder Polling | erfüllt (+ continue, plan, fire, projects, whoami) | Unit (Konfiguration, 0600, URL, SSE-Parser), System-E2E gegen den echten Stack |
| UI: Tokens (einmalig), Zeitpläne, Vorlagen, Webhooks (Secret einmalig), Verlauf; Designsystem, Safe Areas, axe | erfüllt | UI-E2E Desktop + iPhone hoch/quer, Tour hell/dunkel |
| README (Deutsch): API-Doku, Webhook-/GitHub-Einrichtung, `queuectl` | erfüllt | README „Automatisierung …“ |
| Keine echten externen Calls in Tests, kein Root, Sicherheitsmodell nicht aufgeweicht | erfüllt (Mock-Provider, lokale Zustellungen; App ohne neue Rechte; Runner unverändert) | — |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Absendertext außerhalb des Datenblocks:** Der Einleitungssatz des GitHub-Kontexts enthielt `repository.full_name` aus der Nutzlast. Ohne Repository-Filter ist das frei wählbarer Text vor den Regeln (Prompt-Injection-Pfad). | Selbst-Review | Einleitung ohne Absendertext, Repository nur im Block | Unit `intros_never_carry_sender_text` |
| B2 | **Widerruf wirkte nicht auf offene Live-Streams** (im Plan zugesagt, nicht gebaut): ein widerrufenes Token hätte `/api/events` unbegrenzt weiter lesen können. | Selbst-Review gegen den Plan | Token-Streams enden beim Ablauf, spätestens nach 10 min | Integration (Stream endet mit dem Token, < 10 s bei 2 s Restlaufzeit) |
| B3 | **Bearbeiten eines ausgeführten einmaligen Auslösers scheiterte** („Zeitpunkt in der Vergangenheit“), weil die UI das ganze Formular schickt und schon das Mitsenden als Zeitänderung galt; bei Zeitplänen verschob jedes Speichern den nächsten Termin neu. | Selbst-Review des UI-Formulars | Zeitänderung = echte Wertänderung oder Einschalten | Integration (Umbenennen nach Ausführung ⇒ 200, kein neuer Termin) |
| B4 | **Bot-Filter hätte jeden Check-Fehler verworfen** (CI meldet als Bot bzw. `ghost`, laut GitHub-Doku). | Planung/Doku | Bot-Filter nicht für Check-Fehler | Unit `check_failures…` (Sender `github-actions[bot]`) |
| B5 | **Replay mit neuer Delivery-ID** wäre beim generischen Webhook innerhalb des Zeitfensters möglich gewesen, wenn nur der Body signiert ist. | Planung | Delivery-ID mitsigniert | Integration (abgefangene Anfrage mit neuer ID ⇒ 401) |
| B6 | **`queuectl watch` ohne Totmann:** endete der SSE-Stream sofort (z. B. Proxy), drehte die Schleife ohne Pause; eine stumme tote Verbindung hätte ewig gehangen. | Selbst-Review | 1 s Pause vor Neuverbindung, 60 s ohne Daten ⇒ neu verbinden (Server sendet alle 15 s Keep-alive) | Code; System-E2E `watch` (SSE und Polling) |
| B7 | **iPhone: Vorlagenname auf einen Buchstaben geschrumpft** (Titel neben Tags/Buttons in einer Flex-Zeile). | Screenshot des iPhone-E2E | Titel umbrechen statt schrumpfen (CSS) | iPhone-E2E-Screenshot, Tour |
| B8 | **Desktop: neunter Projekt-Reiter verdeckte „Mitglieder“** (waagerecht scrollbar, aber ohne Scrollbalken – mit der Maus nicht auffindbar). | Tour-Screenshot | Reiter umbrechen bei Maus/Trackpad | Tour hell/dunkel |
| B9 | **Composer behielt den Vorlagentitel beim Projektwechsel** (Auftrag im anderen Projekt mit fremdem Titel). | Selbst-Review | Titel beim Projektwechsel verwerfen | Code |
| B10 | **Bestands-Testrennen (WP06):** `plan_mode_changes_nothing…` wartete auf `running` eines Auftrags, der im Plan-Modus-Projekt selbst geplant und nach ~0,1 s gehalten wird; Abfrage alle 150 ms ⇒ zufällig rot (im Vorlauf rot). | Vorlauf der Abnahme | Blocker-Auftrag ohne Plan-Modus (gleiche Prüfung) | `plan_steering` 13/13 in allen folgenden Läufen |
| B11 | **Tour-Seed wartete auf einen Auftrag, der hinter dem 300-s-Auftrag der Tour ansteht.** | Vorlauf (3 Tour-Tests rot) | Auslösen prüft 201, wartet nicht auf das Ende | Tour grün |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **Nicht gegen GitHub selbst geprüft.** Header, Felder, Enums aus OpenAPI/Doku belegt, Signatur mit dem Doku-Testvektor; unbelegt nur `sender.type == "Bot"` (zusätzlich `[bot]`-Suffix). | Netzzugang zu GitHub als Webhook-Absender gibt es hier nicht; Fehler zeigen sich im Verlauf („ignoriert“ mit Grund) bzw. im Zustellprotokoll von GitHub. |
| N2 | **GitHub-Replay nach 90 Tagen** (GitHub signiert keinen Zeitstempel; Dedupe nur so lange wie der Verlauf). „Ein Auftrag pro Commit“ für Checks gilt ebenso nur 90 Tage. | Angreifer bräuchte eine mitgeschnittene, gültig signierte Zustellung (TLS); Rate-Limit und „läuft schon“ begrenzen. |
| N3 | **Body-Limit 1 MiB** – GitHub erlaubt 25 MB; sehr große Push-Ereignisse werden mit 413 abgelehnt (in GitHubs Zustellprotokoll sichtbar). | Bewusst: DoS-Schutz vor jeder Signaturprüfung. |
| N4 | **Ungültig signierte Anfragen** kosten je einen DB-Lesezugriff und werden nicht gezählt oder gedrosselt. | Kein Speicherwachstum; Drosselung gehört in den Reverse-Proxy (README). |
| N5 | **Tokens nicht projektgebunden.** Ein Token wirkt in allen Projekten seiner Person (Rollen greifen). | Scopes sind fein; projektgebundene Tokens wären eine Erweiterung der Tabelle um Ressourcen-Auflösung. |
| N6 | **Widerruf wirkt auf offene SSE-Streams erst nach bis zu 10 min.** | Andere Anfragen sofort; Streams verbinden neu und scheitern dann. |
| N7 | **Vorlagen-Formular ohne Anbieter/Modell-Auswahl** (nur per API); im UI gilt die Projekt-/Nutzer-Vorgabe. | Bewusst schlank; die API kann es. |
| N8 | **Nur GitHub-Webhooks**; GitLab/Gitea-PRs werden weiter nur abgefragt (WP05). | WP07 verlangt GitHub-Events; generische Webhooks decken anderes ab. |
| N9 | **Eigentümer-Prüfung der `queuectl`-Datei** ist nur als root getestet (gleiche UID); der fremde-UID-Zweig ist Code-Review. | Container ohne zweiten Nutzer. |
| N10 | **Tick-Auflösung** der Zeitpläne `TRIGGER_TICK_SECS` (15 s); ein Termin kann bis dahin später starten (Kulanz 10 min bis „verpasst“). | Unkritisch. |
| N11 | **Ausgehende Webhooks** (started/finished, Report P1.6) gibt es nicht. | Nicht im WP07-Umfang von `work-packages.md`; Benachrichtigungen sind WP10. |
| N12 | Kopieren in die Zwischenablage braucht einen sicheren Kontext (HTTPS oder localhost); sonst Hinweis und markierbares Feld. | Betrieb läuft über HTTPS. |
| N13 | Auslösen eines gerade gelöschten Auslösers antwortet 409 statt 404 (Rennen zwischen Laden und Sperren). | Kosmetisch. |

## Geprüft und für gut befunden

- **Keine Eskalation per Token:** Default-Deny-Tabelle an der einzigen Auth-Stelle; Admin-, Konto-, Zugangs-, Token-, Mitglieder-, Geheimnis- und Gate-Endpunkte fehlen in der Tabelle; Test über alle Tabellenrouten plus Negativliste. Ein Token-Nutzer mit `is_admin` erreicht keine Admin-Route.
- **Genau einmal:** alle Dedupe-Entscheidungen in der Datenbank (UNIQUE), Zeilensperre pro Auslöser serialisiert Zustellungen, Rate-Limit-Zählung unter derselben Sperre.
- **Prompt-Injection:** Nutzlast, Freitext der API und Commit-/Issue-Texte nur im Block mit Zufallsgrenze; Titel aus Vorlage + feste Wörter + Zahlen; WP05/WP06-Blöcke entschärfen jetzt auch `EVENT_DATA_`.
- **Architektur:** keine Host-Rechte in der App, Runner unverändert, Wire-Typen und Texte in `queued-api`, nur neue Migration 0010, `queuectl` nur über die öffentliche API.
- **Alt-Verhalten:** Kein bestehender Test abgeschwächt. Angepasst: `task_roundtrip_and_accessors` (neues Feld `trigger_id`), WP06-Rennen (B10, gleiche Prüfung), Tour (zusätzliche Ansichten), Stack-Skript (baut `queuectl`, `TRIGGER_TICK_SECS=1`).
