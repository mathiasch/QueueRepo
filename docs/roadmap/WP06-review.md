# WP06 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP06-Diff gegen `45e1f30` (common, runner, api, app, Migration 0009, Frontend, E2E, README, CLI-Aufnahmen). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP06), Auftrag des Koordinators (technische Durchsetzung des Plan-Modus, belegte CLI-Flags, Genehmigen/Ändern/Verwerfen, Auto-Approve optional und aus, Steering mit Zurückziehen und Resume bzw. Handoff, Mehrmandanten, Längenlimits, getrennte Prompt-Blöcke, Inline-Kommentare mit „veraltet“, Sicherheitsmodell nicht aufweichen), Report P1.3/P1.4/P1.7. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und, wo automatisierbar, mit Tests belegt; die komplette Suite ist auf dem finalen Stand zweimal hintereinander grün (`WP06-test.md`). Die ehrliche Grenze: Nur Claude Code ist für Plan-Modus und Session-Resume live belegt (mit 2.1.322, nicht der gepinnten 2.1.319). Für Codex, Gemini und opencode sind Aufrufe und Sitzungsorte aus Programmen bzw. Quelltext gelesen, nie ausgeführt. Die Garantie „Plan-Lauf ändert nichts“ hängt deshalb bewusst **nicht** an diesen Schaltern, sondern an Wegwerf-Kopie, ro-Mount und Baumvergleich.

## Akzeptanz gegen WP06

| Anforderung | Status | Beleg |
|---|---|---|
| Plan-Modus pro Projekt und Auftrag | erfüllt | Integration `plan_mode_changes_nothing…`, `plan_revise_edit_and_reject`; UI-E2E Einstellungen + Composer |
| Agent ändert nachweislich nichts, technisch erzwungen | erfüllt: Wegwerf-Kopie (immer gelöscht, alte Reste vorher entfernt), Container-ro-Mount, Baumvergleich mit ctime auf jeder Engine, CLI-Schalter wo belegt | Runner `read_only_runs_mount_…`, `read_only_run_reports_every_change…`, `snapshot::tests` (inkl. `touch -r`), Integration (Hauptrepo-HEAD unverändert, Warnung, Rest unsichtbar), System-E2E |
| CLI-Flags belegt / unverifiziert markiert | erfüllt | `WP06-impl.md` §4, `CLI_SESSION_SUPPORT` (im UI/README), Claude-Aufnahmen `claude-2.1.322/` |
| Genehmigen / Ändern (bearbeiten, neu anfordern) / Verwerfen | erfüllt | Integration, UI-E2E Desktop + iPhone |
| Auto-Approve optional, Standard aus | erfüllt; nie bei Fragen (auch Text-Fragen) oder Änderungsversuch | `automatic_approval_only_when_due…`, Unit `plan_from_structured_output_or_final_text` |
| Umsetzung mit Plan als Kontext | erfüllt (fenced Block „Approved plan“) | `SEEN.md` enthält `MOCKPLAN-*` bzw. den bearbeiteten Text |
| Steering: Nachricht an laufende Aufträge, zurückziehbar bis Zustellung | erfüllt (atomare Zustandswechsel) | `messages_wait_can_be_withdrawn…`, System-E2E, UI-E2E |
| Zustellung per Session-Resume, sonst Fortsetzung mit Handoff | erfüllt (Resume, Fallback bei gescheitertem Resume, Handoff-Runde ohne Resume-Fähigkeit) | `message_to_a_running_task_goes_into_its_session`, `send_now_interrupts…`, `failed_resume_falls_back…`, Unit `recorded_unknown_session_counts_as_failed_resume` |
| „Weiter mit …“ | erfüllt (Sitzung nur des eigenen Auftrags) | `continue_resumes_the_session_only_for_its_author`, System-/UI-E2E |
| Rechte wie bestehende APIs | erfüllt (Ersteller mit Rolle ≥ Bearbeiter oder Eigentümer; Betrachter 403, Fremde 404; Plan-Einstellungen nur Eigentümer) | `steering_rights_follow_the_project_roles` |
| Längenlimits, getrennt im Prompt | erfüllt | Validierungs-Tests (API + Unit), `prompt_blocks_mark_user_input…` |
| Inline-Kommentare (Datei/Zeile, mehrzeilig), gesammelt, ein Follow-up, gültig/verschoben/veraltet | erfüllt | `inline_comments_are_bundled_and_follow_the_diff`, `diff::tests`, `logic::comments`, UI-E2E inkl. Touch |
| UI: Designsystem, verständlich, Safe Areas, axe | erfüllt | Tour (hell/dunkel, Desktop/iPhone), `plan_steering-mobile.spec.ts` |
| Mock-Provider um Plan/Resume erweitert | erfüllt | `agent/mock.rs` |
| Keine echten externen Calls in Tests | erfüllt (Mock, Fake-LLM); die 4 Claude-Läufe waren manuelle Belegläufe, keine Tests | — |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Rest eines abgestürzten Plan-Laufs wäre wiederverwendet worden.** Die Plan-Kopie hat eine feste, abgeleitete ID; nach einem App-Absturz hätte `prepare_task` den alten Arbeitsbaum samt Änderungen des Agenten fortgesetzt, der nächste Plan-Lauf hätte sie gesehen. | Selbst-Review | Plan-Kopie wird vor jedem Plan-Lauf entfernt und beim Aufräumen des Auftrags mitgelöscht | Integration (vorher angelegter Rest bleibt unsichtbar und ist danach weg) |
| B2 | **Plan sichtbar, bevor die Kopie weg war.** `hold` lief vor dem Löschen. | Integrationstest | Löschen zuerst, dann halten | `plan_mode_changes_nothing…` |
| B3 | **Sitzungs-ID aus dem Stream als CLI-Option.** Codex/opencode melden ihre ID im (agentengesteuerten) Stream; eine ID wie `--dangerously-…` wäre beim Resume als Option in argv gelandet. | Selbst-Review | `valid_session_id` (nur Id-Zeichen, kein `-` am Anfang) beim Parsen und beim Bauen des Aufrufs | `session_ids_from_the_stream_never_become_options` |
| B4 | **Kommentarbündel ohne Kommentare.** Nachricht und Zuordnung der Kommentare waren zwei Schritte; der Scheduler konnte die Nachricht dazwischen zustellen. | Selbst-Review | Zuordnung in derselben Transaktion wie die Nachricht | Code (`post_message`) |
| B5 | **Alle Kommentare fälschlich „veraltet“ im Prompt**, wenn der Diff des Ursprungsauftrags nicht mehr verfügbar ist (leerer Diff ⇒ keine Datei gefunden). | Selbst-Review | leerer Diff ⇒ Ort unbekannt ⇒ wie geschrieben | Code (`current_diff_files`) |
| B6 | **Text-Pläne mit offenen Fragen hätten automatisch genehmigt werden können** (Gemini/opencode/Batch liefern keine strukturierten Fragen). | Selbst-Review | Fragen-Abschnitt („Questions“/„Fragen“) im Text wird ausgewertet | Unit `plan_from_structured_output_or_final_text` |
| B7 | **Falsche Zustellart** „in die laufende Sitzung“ nach gescheitertem Resume. | Selbst-Review | auf „als neuer Lauf mit Übergabe“ korrigiert | `failed_resume_falls_back…` (Ereignisse) |
| B8 | **iPhone: Kommentarformular passte nicht über den Home-Indikator** (sticky und höher als der sichtbare Bereich). | iPhone-E2E | kompakte sticky Auswahlleiste, Formular im Fluss mit Fokus | `plan_steering-mobile.spec.ts` |
| B9 | **iOS-Zoom im Plan-Editor** (Monospace 13 px). | iPhone-E2E | 16 px auf Touch | dito |
| B10 | **axe:** `label-content-name-mismatch` an Diff-Zeilen (aria-label ohne sichtbaren Text), `target-size` < 24 px auf Desktop. | UI-E2E / Tour | sichtbarer Text im Namen, Mindesthöhe 24 px | Tour, UI-E2E |
| B11 | **Bestandsfehler `.badge.attention`**: Regel `:root:not([data-theme="dark"])` griff auch im System-Dunkelmodus ⇒ dunkle Schrift auf dunklem Amber (axe `color-contrast`). Sichtbar wurde er erst durch den zusätzlichen gehaltenen Auftrag in der Tour. | Tour | `light-dark()` | Tour hell/dunkel Desktop + iPhone |
| B12 | Testinfrastruktur: Tour-Auftrag „läuft“ war nach 30 s fertig (zweiter Farbdurchgang), parallele Mock-Aufträge kollidierten an `SEEN.md`. | E2E | `MOCK_LONG`, `SEEN-<datei>` | grüne Läufe |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **Codex/Gemini/opencode nie ausgeführt.** Resume-Aufruf (`codex exec … resume <id> -`, `--resume`, `--session`), Speicherorte (v. a. Codex-SQLite-Namen) und Plan-Schalter sind gelesen, nicht erprobt. Scheitert ein Resume ohne Verbrauch, folgt ein frischer Lauf; beginnt eine CLI still eine **neue** Sitzung, merkt Queue das nicht (der Resume-Prompt enthält deshalb immer die Aufgabe). | Im UI/README als unverifiziert markiert. |
| N2 | **Claude live mit 2.1.322, gepinnt ist 2.1.319.** | Optionen in beiden Versionen vorhanden (WP01/WP03 nutzen dieselben); Verhalten nur für 2.1.322 belegt. |
| N3 | **Engine `none`:** Ein Plan-Lauf kann außerhalb seiner Kopie schreiben; erkannt wird nur die Kopie. | Bestand: `none` ist nur für Entwicklung. |
| N4 | **Ro-Mount mit echten CLIs im Container nicht ausgeführt** (Agent-Image nicht gebaut). Falls eine CLI beim Start ins Arbeitsverzeichnis schreiben will, scheitert der Plan-Lauf sichtbar. | Kein Sicherheitsproblem (fail-closed). |
| N5 | **Zustellung nur an Laufgrenzen.** Kein Einspeisen mitten in einen Turn; „Sofort zustellen“ unterbricht nur den Agenten, nicht Setup/Verify/Review. | Bewusste Entscheidung (impl §2.5). |
| N6 | **Rennen an der Rundengrenze:** Zwei „Sofort“-Nachrichten, deren zweite genau die 10. Runde überschreitet, könnten einen unterbrochenen Lauf in die normale Auswertung schicken (dann Fix-Auftrag statt Resume). | Sehr selten; Grenze wird beim Senden geprüft. |
| N7 | **Live-Updates nur für den Ersteller** (SSE pro Nutzer): Eine Eigentümerin, die den Auftrag eines Mitglieds steuert, sieht neue Zustände erst nach eigener Aktion bzw. Neuladen. | Bestand des SSE-Modells. |
| N8 | `POST /tasks/{id}/answer` (Bestand) hängt die Antwort weiterhin ungetrennt und unbegrenzt an den Kontext. | Nicht Teil von WP06; Nachrichten an `waiting_input` sind getrennt und begrenzt. |
| N9 | Automatische Fortsetzung für späte Nachrichten (`done`) läuft ohne Plan-Modus. | Die Nachricht ist eine direkte Anweisung; per „Weiter mit …“ ist der Plan wählbar. |
| N10 | Auto-Genehmigung mit Tick-Auflösung (15 s in Produktion). | Unkritisch bei Minuten-Fristen. |
| N11 | Kommentare nur auf den Diff des Auftrags, nicht auf gehaltene (nicht committete) Änderungen. | Nicht-Ziel im Plan. |
| N12 | Toasts überdecken auf dem iPhone Teile des Drawers (Bestand). | Verschwinden selbst; nicht WP06. |
| N13 | Sitzungsspeicher bis 64 MiB pro Auftrag, Aufräumen per TTL beim nächsten Lauf mit Sitzung, nicht per Zeitplan. | Plattenbedarf begrenzt; Projektlöschung räumt sofort. |

## Geprüft und für gut befunden

- **Sicherheitsmodell nicht aufgeweicht:** keine neuen Capabilities oder Netzfreigaben; Plan-Lauf ohne Setup und Secrets, mit der Netzregel des Agenten; ro-Mount zusätzlich; Sitzungsspeicher nur runnerseitig, Allowlist, keine Symlinks, Zugangs-/Konfigurationsdateien und Claude-Memory ausgeschlossen, App-Dateien gewinnen; fremde Transkripte nie an andere Nutzer; `key = workspace` erzwungen.
- **Gate/Branches/Merge-Queue:** Steuerrunden liegen vor dem Gate; Commit, Verify, Review und Übernahme unverändert; Fortsetzungen nutzen `carry_from`/PR-Branch wie Fix- bzw. PR-Folgeaufträge.
- **Architektur:** Datei-/Prozessarbeit nur im Runner, Protokoll in `queued-common`, Typen/Texte/Diff-Parser in `queued-api` (UI und Server identisch), nur neue Migration 0009.
- **Alt-Verhalten:** Kein bestehender Test abgeschwächt. Angepasst: `opencode::parses_events` (neues erstes Event `Session`), `task_roundtrip_and_accessors` (neue Felder), Tour (Auswahl des gehaltenen Auftrags per Titel, weil ein zweiter gehaltener Auftrag dazukam; lang laufender Seed-Auftrag), Hinweistext im Plan-Bereich so formuliert, dass der WP04-Test `branches.spec.ts` eindeutig bleibt.
