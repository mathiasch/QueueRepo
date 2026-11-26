# WP12 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP12-Diff (Migration 0015, `queued-api`, `queued-common`, Runner, App, Frontend, E2E, Doku). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP12), Auftrag des Koordinators, Plan. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit benannten Restrisiken.** Alle blockierenden Funde sind behoben und durch Tests (teils mutativ) belegt. Die ehrliche Grenze: Der Container-Weg der Vorschau (Docker/Podman, Browser per `exec` im Image) ist nur als Kommando geprüft, nicht ausgeführt – getestet ist Engine `none`. Echte Modelle für die Auto-Auswahl wurden nicht aufgerufen (Fake-Server).

## Anforderungen ↔ Belege

| Anforderung | Stand | Beleg |
|---|---|---|
| N parallele Versuche, gleicher oder verschiedene Anbieter/Modelle, N wählbar, begrenzt durch Kontingent/Budget/Parallelität | erfüllt (2–8, `max_parallel`, gesperrte Zugänge abgewiesen, Nutzer-Parallelität/Runner-Kapazität je Versuch) | `bounds_quota_and_cost_preview`, `automatic_selection_…` (zwei Modelle), E2E Composer |
| Eigene Sandbox und eigenes Repo je Versuch, keiner sieht den anderen | erfüllt (WP04-Mechanik je Versuch; Container mountet nur `t<id>`) | `attempts_are_isolated_…` (keine Zeile, kein Commit des anderen, kein Remote/Alternates); Mutation M1 |
| Vergleichsansicht: Diffs, Verify, Review, Kosten, Dauer | erfüllt (Karten + Tabelle, Diff je Versuch über „Details & Änderungen“) | `GET …/attempts` im Integrationstest, E2E Desktop/iPhone mit axe |
| Auswahl durch Nutzer oder Review-Agent; Prüfer nie schwächer, bevorzugt anderer Anbieter | erfüllt | Unit `selector_is_never_weaker_and_prefers_another_provider`, `configured_review_step_…`; Integration mit Fake-Prüfer; Mutation M4 |
| Merge über Merge-Queue bzw. PR-Modus | erfüllt (Gewinner `candidate → merging`, unveränderter `merge_inner`/`publish`) | `attempts_are_isolated_…` (Hauptstand enthält nur den Gewinner) |
| Verworfene sauber aufgeräumt (Repos, Container, Branches) | erfüllt (laufende per Token gestoppt, Container über den bestehenden Exec-Stopp, Arbeitsbaum + Branch entfernt, Vorschauen gestoppt) | `attempts_are_isolated_…`, `cancelling_the_group_…`; Mutation M10 |
| Kostenvorschau N × | erfüllt | `bounds_quota_and_cost_preview` (3 × Einzelschätzung), E2E |
| Budget-Abbruch stoppt alle Versuche | erfüllt (Live-Konto über alle Versuche + DB-Nachprüfung) | `group_budget_stops_all_attempts` (jeder allein unter, zusammen über dem Limit); Mutation M3 |
| Preview: Dev-Server in der Sandbox, Projekt-Einstellung Kommando/Port/Ready/Timeout | erfüllt | Runner-Tests, Integration, E2E Einstellungen |
| Kein Port-Binding, Proxy der App nur für Mitglieder | erfüllt (Unix-Socket-Brücke, Link nur über authentifizierte API, Start nur Bearbeiter) | `container_has_no_network_and_no_port_binding`, `live_preview_is_member_only_…`; Mutation M7 |
| Strikte CSP/Sandbox-iframe, eigene Origin, Cookie-Isolation begründet | erfüllt (Plan §3.3) | `live_preview_…` (CSP, kein `Set-Cookie`, keine App-Credentials, Vorschau-Host nie API), E2E (`self.origin == "null"`, `document.cookie` gesperrt); Mutationen M5, M6 |
| Screenshots per Headless-Browser nach UI-Änderungen, auch mobil, im Auftrag | erfüllt (Desktop 1280, Mobil 390 @2x) | Runner `screenshots_are_real_pngs_…`, Integration `screenshots_after_ui_changes_…`, E2E |
| Ressourcen-/Zeitlimits, Prozess und Container immer beendet | erfüllt | `timeout_stops_the_whole_process_group` (Enkel ignoriert TERM/INT), `forwarding_…_lifetime_ends_it`; Mutation M8 |
| Routing-Regeln pro Projekt, auf WP13 aufbauend, deterministisch, Erklärung im Auftrag, Schnittstelle für WP15 | erfüllt (`RoutingDecision`) | Unit `decision_precedence_…`, `fix_decision_…`, `rule_validation`; Integration `routing_rules_escalation_and_explanations`; Mutation M9 |
| Gate/Secret-Scan für jeden Versuch | erfüllt (gleicher `run_inner`; Merge-Nachprüfung beim Gewinner) | Code (kein Sonderweg), bestehende Gate-Tests |
| Analytics: Versuche sauber zählen | erfüllt | `attempts_are_isolated_…` (1 Auftrag, 3 Versuche), `group_budget_…` (`budget_stops`) |
| Kein Root, Sicherheitsmodell nicht aufgeweicht | erfüllt (Dev-Server ohne Netz; Browser nur Loopback; App ohne Host-Rechte, Prozessarbeit im Runner) | Code-Review, Tests oben |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Prozess-Leck bei Zeitüberschreitung der Vorschau:** Stopp per SIGINT an die Gruppe, danach SIGKILL nur, wenn der Anführer nicht innerhalb der Frist endet. Hintergrund-Jobs einer nicht-interaktiven Shell ignorieren SIGINT; der Anführer endete, das Kind lief weiter. | eigener Runner-Test (rot) | SIGTERM an die Gruppe, Frist, **immer** SIGKILL an die Gruppe | `timeout_stops_the_whole_process_group` (jetzt mit Enkel, der TERM und INT ignoriert); Mutation M8 |
| B2 | **Leere Vergleiche, Gruppe sofort gescheitert:** Rundenbeginn war `Utc::now()` der App, `created_at` der Versuche ist die Transaktionszeit (früher) ⇒ keine Versuche in der Runde ⇒ `settle` meldete „kein Versuch erfolgreich“. | Integrationstests (6 rot) | Rundenbeginn = `SELECT now()` in derselben Transaktion; `settle` wartet bei leerer Runde | alle Best-of-N-Tests grün |
| B3 | **Stepper nicht bedienbar:** `disabled=move || n >= MAX` im `view!`-Makro – `>` beendete das Attribut, der Knopf war dauerhaft deaktiviert, Code-Text landete im DOM. | E2E (rot) | Ausdrücke in `{ … }` | E2E Desktop/iPhone grün |
| B4 | **Gruppe bearbeitbar:** Zugang/Modell einer wartenden Gruppe ließ sich per PATCH ändern, die Versuche hätten aber weiter die bei der Anlage geprüften Zugänge genutzt. | Selbst-Review | PATCH von Zugang/Modell einer Gruppe abgelehnt (400 mit Erklärung) | Code; kein eigener Test (Gruppe ist nur Sekundenbruchteile `queued`) – siehe N8 |
| B5 | **Gewinner ohne Änderungen** wurde `done`, sein Arbeitsbaum und Branch blieben liegen. | Selbst-Review | `cleanup_task` für diesen Fall | Code |
| B7 | **Zweiter „Speichern“-Knopf in den Projekt-Einstellungen:** Die neue Vorschau-Karte stand vor dem Projektformular und hieß ebenfalls „Speichern“ – für Menschen missverständlich, für 5 bestehende E2E-Tests mehrdeutig (`.first()` traf die falsche Karte). | erster voller Playwright-Lauf (5 rot) | Karte ans Ende der Einstellungen, Knopf „Vorschau-Einstellungen sichern“; bestehende Tests unverändert | betroffene Specs grün, volle Läufe |
| B6 | **WP10-Test tageszeitabhängig rot** (`daily_summary_is_sent_once_…`, auch auf `5241d25`): `notify::tick` startet die Tageszusammenfassung mit der echten Uhr. | erster voller Lauf | Test belegt die echten Tage vorab; Produktcode unverändert | Test grün zu jeder Tageszeit |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Begründung / Risiko |
|---|---|---|
| N1 | **Container-Vorschau nicht ausgeführt:** Brücke (mit echtem Node getestet), Kommando (Unit-Test) und Image-Erweiterung (`chromium`) sind geprüft, ein echter Docker/Podman-Lauf nicht (keine großen Image-Builds hier, wie WP13-N3). | Erster echter Lauf kann an Image-Details scheitern (Chromium-Pfad, Fonts); Fehler erscheinen als Ereignis, der Auftrag läuft weiter. |
| N2 | **Live-Budget je App-Instanz**; mehrere Instanzen gleichen nach jedem Lauf über `usage_log` ab. | Überziehung höchstens um einen laufenden Lauf je weiterer Instanz. |
| N3 | **Vorschau-Link ist ein Bearer-Geheimnis** für seine Laufzeit (≤ 60 min); eine Mitgliedschaft, die währenddessen entzogen wird, beendet die Vorschau nicht. | Opake Origin bekommt keine Cookies; nutzerspezifische Kennungen würden absolute Pfade je Nutzer brechen. Link nur über die API an Mitglieder, gehasht gespeichert, `no-referrer`. |
| N4 | **Kandidaten behalten ihren Arbeitsbaum**, bis gewählt oder verworfen wird. | Plattenplatz bei vergessenen Vergleichen; wie gehaltene Aufträge. |
| N5 | **Fixes des Gewinners** nach einem Übernahme-Konflikt hängen am Versuch, nicht an der Gruppe ⇒ zählen in der Auswertung nicht als Reparatur der Gruppe. | Erstversuchsquote leicht zu gut, wenn Konflikte häufig sind. |
| N6 | **Engine `none`:** Versuche sind nur über Verzeichnisse/Repositories getrennt; ein Agent könnte das Nachbarverzeichnis lesen (wie WP04/WP05-N10). Dev-Server und Browser liegen auf dem Host (Browser mit totem Proxy). | Nur Entwicklung, gekennzeichnet. |
| N7 | **Dev-Server schreibt im Arbeitsbaum** (Caches). | Was liegen bleibt, prüft das Gate des nächsten Laufs; übernommen wird nur der geprüfte Commit. |
| N8 | B4 ohne automatisierten Test. | Fenster `queued` einer Gruppe ist kurz; Logik eine Zeile, per Review geprüft. |
| N9 | **Regel-Merkmale ohne Modell**: Größe = Zeichenzahl, Stichworte als Teilstring. | Bewusst deterministisch; die intelligente Analyse ist WP15. |
| N10 | **Review-Schritt normaler Aufträge** prüft weiter nur „nie günstigste Stufe“, nicht „nie schwächer als der Ausführer“ (Eskalation auf Opus mit Sonnet-Review bleibt möglich). | Die Regel des Auftrags gilt für den Best-of-N-Prüfer; für das normale Review ist sie WP15 (Chooser) zugeordnet. |
| N11 | **Projekt-Umzug (WP11)** zählt Kandidaten nicht als „beschäftigt“; deren Arbeitsbäume gingen bei einem Umzug verloren (Branches sind im Bundle). | Selten; danach Diff/Merge weiter möglich, nur Vorschau nicht. |
| N12 | Kein WebSocket/Hot-Reload durch den Vorschau-Proxy. | Bewusst nur HTTP. |

## Mutative Gegenprobe

Je eine gezielte Verschlechterung, Test muss rot werden, danach zurückgesetzt (`git status` sauber geprüft).

Fund in der eigenen Werkzeugkette: Das Zurücksetzen per Kopie gab der Datei eine **ältere** Änderungszeit als dem mutierten Build – Cargo hielt den Mutanten für aktuell. So lief `queued-common` nach M5 kurz mit durchgelassenen Cookies weiter; der erste volle Lauf fiel genau daran rot aus (`live_preview_…`). Behoben durch `touch` nach jedem Zurücksetzen und einen Neubau aller Quellen vor den Abnahmeläufen. Die Rot-Ergebnisse der Mutationen selbst sind davon unberührt (der Mutant war jeweils neuer als der Build).

| # | Mutation | Test | Ergebnis |
|---|---|---|---|
| M1 | Versuche bereiten den Arbeitsbaum der Gruppe vor (geteilter Baum) | `attempts_are_isolated_…` | rot |
| M2 | Gruppen-Prüfung in `select` entfernt | `concurrent_selections_pick_exactly_one` | **grün** – die zweite Schutzschicht (Zeilensperre + Status `candidate` am Versuch) hält; erst M2b (beide Prüfungen weg) ⇒ rot. Zwei unabhängige Sperren, gewollt. |
| M3 | Live-Konto der Gruppe greift nie | `group_budget_stops_all_attempts` | rot |
| M4 | Prüfer-Stufe nicht gefiltert | `selector_is_never_weaker_…` | rot |
| M5 | `Cookie`/`Authorization` an den Dev-Server | `live_preview_…` | rot |
| M6 | CSP-Header fehlt | `live_preview_…` | rot |
| M7 | Vorschau-Link ohne Mitgliedsprüfung | `live_preview_…` | rot |
| M8 | kein SIGKILL an die Prozessgruppe | `timeout_stops_the_whole_process_group` | rot (nachdem der Test zunächst grün blieb und verschärft wurde: Enkel ignoriert TERM/INT) |
| M9 | Route vor Regel | `decision_precedence_…` | rot |
| M10 | Verlierer nicht aufgeräumt | `attempts_are_isolated_…` | rot |
| M11 | Versuche bekommen Fix-Aufträge | `failed_attempts_get_no_fix_…` | rot |

## Geprüft und für gut befunden

- **Gate je Versuch:** derselbe `run_inner`, `gate::check_changes`/`commit` ohne Ausnahme; der Gewinner wird beim Merge erneut geprüft (WP04).
- **Datenstandort:** Versuche über `check_decision`, Regeln beim Speichern und bei der Anlage, Prüfer nur mit erlaubtem Standort.
- **Prompt-Injection gegen den Prüfer:** Kandidaten als Datenblock mit Nonce, Trennzeichen entschärft, Antwort nur als Nummer aus der Liste; Auswahl führt nur in die normale, geprüfte Merge-Queue.
- **Keine Secrets in neuen Antworten:** Vorschau-Logausschnitte und Fehler werden redigiert; Projektvariablen gehen an den Dev-Server, Secrets nicht (er hat ohnehin kein Netz).
- **Alt-Verhalten:** keine bestehende Prüfung abgeschwächt. Angepasst: `task_roundtrip_and_accessors` (neue Felder), der WP10-Test (B6).
