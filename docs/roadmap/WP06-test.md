# WP06 – Plan-Modus, Steering & Inline-Kommentare: Testbericht

Stand: 2026-11-11, Branch `mvp` (Arbeitsverzeichnis, nicht committet). Alle Zahlen stammen aus Läufen in dieser Sitzung (`fullrun`: Backend fmt/clippy/test, Frontend fmt/clippy wasm32 + nativ/test, Playwright alle Projekte; Postgres 16 lokal, Runner-Engine `none`, Mock-Provider). Keine echten externen Calls in Tests: Agent = Mock, Einzelschritt-Planung gegen den lokalen Fake-LLM-Server, Git-Hosts gegen den Fake-Host.

## 1. Teststrategie

| Ebene | Was | Wo |
|---|---|---|
| Unit Runner | Baumvergleich (unverändert; Inhalt mit zurückgesetzter mtime, Löschen, Neu, `.git`, ignorierte Datei, Symlink-Ziel, chmod, neues Verzeichnis; Begrenzung, unvollständig = Änderung); Sitzungsspeicher (nur erlaubte Pfade, Präfix-`*`, Ausschlüsse inkl. Claude-Memory/Zugangs-/Konfigurationsdateien, Seed nur bei leerem eigenen Speicher, Symlink-Datei/-Verzeichnis/-Elternteil nie gefolgt, zu groß ⇒ alter Stand bleibt, ungültige Specs, TTL-Sweep, Projektlöschung); Exec: `:ro`-Mount (auch `ro,Z`), Änderungsbericht eines Read-only-Laufs, Cache bei Read-only abgelehnt, Sitzung überlebt zwischen Läufen ohne Zugangsdatei, App-Dateien gewinnen | `runner/src/{snapshot,sessions,exec}.rs` (+11) |
| Unit common/api | `valid_session_path`, alte Gegenstellen ohne neue Felder; Längen/Zeichen, Auto-Genehmigung 5–10080, Bereiche ≤ 200 Zeilen, Zustände, `may_auto_approve`, CLI-Tabelle; Diff-Parser (Zeilennummern, Abschnitte, neue/gelöschte/binäre/gequotete Pfade), Bereich in einem Abschnitt, aktuell/verschoben/veraltet/mehrdeutig | `common/src/lib.rs` (+1), `api/src/{steer,diff}.rs` (+6) |
| Unit App | Aufrufe je CLI für Plan/Start/Resume (Claude, Codex, Gemini, opencode, Mock), Sitzungs-IDs aus dem Stream nie als Option, Session-Ereignis je CLI genau einmal, Sitzungsdateien je CLI; Plan aus structured output/Text inkl. Fragen-Abschnitt; Plan-Kopie nie die Auftrags-ID; **echte Claude-2.1.322-Aufnahmen**: Plan-Lauf liefert Plan, einzige Schreibaktion im HOME; Resume mit Werkzeugen und Verbrauch, unbekannte Sitzung = gescheitertes Resume | `agent/mod.rs` (+3), `runner_plan.rs` (+3), `runner_steer.rs` (+1) |
| Integration | 13 Szenarien (unten), App + echter Runner in-process + Postgres | `tests/plan_steering.rs` |
| Unit Frontend | Zeilenauswahl (Start, Bereich auf/ab, Neustart, andere Seite, über Abschnitte abgelehnt), Platzierung unter der aktuellen Zeile bzw. Liste „veraltet“ | `frontend/src/logic/comments.rs` (+2) |
| System-E2E | echte Binaries: Plan-Lauf ändert keine Projektdatei, Zurückziehen vor Zustellung, genehmigter Plan erreicht den Agenten; „Sofort zustellen“ in die laufende Sitzung (< 25 s statt 30 s Schlaf), „Weiter mit …“ setzt dieselbe Sitzung fort | `e2e/tests/system/plan_steering.spec.ts` (2) |
| UI-E2E Desktop | Plan-Modus in den Projekt-Einstellungen, Composer-Voreinstellung, Plan-Ansicht: neuen Plan anfordern, bearbeiten und genehmigen, verwerfen mit Bestätigung; Nachricht senden/zurückziehen/sofort zustellen, „Weiter mit …“ öffnet den neuen Auftrag; Kommentieren (Auswahl, Formular mit Fokus, Entwurf, Senden als Antwort), danach „veraltet“; axe an jedem Zustand | `e2e/tests/ui/plan_steering.spec.ts` (3) |
| UI-E2E iPhone hoch/quer | Plan-Ansicht: Touch-Ziele ≥ 44 pt innerhalb der Safe Areas, 16-px-Eingaben, kein seitliches Scrollen, axe; Kommentieren per `tap`: Zeile ≥ 44 pt, Auswahlleiste über dem Home-Indikator, Formular 16 px mit Fokus, Senden ⇒ Fortsetzung | `e2e/tests/ui/plan_steering-mobile.spec.ts` (1 × 2) |
| Tour (axe hell/dunkel, Desktop + iPhone) | zusätzlich: Plan-Ansicht mit offenem Editor, Nachrichtenfeld eines laufenden Auftrags, „Weiter mit …“ und Kommentarformular im Diff | `e2e/tests/tour.ts` |

Integrationstests (`tests/plan_steering.rs`): `plan_mode_changes_nothing_and_approval_implements_the_plan` (Änderungsversuch gemeldet, Hauptrepo-HEAD und Branch unverändert, Kopie gelöscht, Postfach-Benachrichtigung, Gate-Endpunkt delegiert, Plan im Umsetzungs-Prompt, Rest eines alten Plan-Laufs unsichtbar, ohne Plan-Modus direkt), `plan_revise_edit_and_reject` (Validierung, Überarbeitung, bearbeiteter Text ersetzt den Plan, 409 nach Entscheidung, Verwerfen ändert nichts und startet nie), `automatic_approval_only_when_due_and_never_with_questions`, `single_shot_plan_waits_for_approval_in_plan_mode`, `messages_wait_can_be_withdrawn_and_reach_the_next_start` (inkl. 409 nach Zustellung, 20er-Grenze, Abbruch ⇒ verworfen), `message_to_a_running_task_goes_into_its_session` (eine Steuerrunde, gleiche Sitzung, ein Commit), `send_now_interrupts_the_agent_and_resumes_at_once`, `late_messages_become_a_continuation`, `continue_resumes_the_session_only_for_its_author` (Betrachter 403, Fremde 404, laufend 409, fremder Bearbeiter ohne Sitzung), `failed_resume_falls_back_to_a_fresh_run`, `steering_rights_follow_the_project_roles` (inkl. herabgestufter Ersteller), `inline_comments_are_bundled_and_follow_the_diff` (Validierung, private Entwürfe, eine Nachricht als Antwort, veraltet/verschoben nach echter Diff-Änderung, Bereich, Fortsetzung bei fertigem Auftrag), `prompt_blocks_mark_user_input_and_comment_context` (Nonce-Block, zitierte Zeile, entschärfte Grenzen).

## 2. Ergebnisse

| Lauf | Backend fmt | Backend clippy | Backend Tests | Frontend fmt | Frontend clippy wasm32 / nativ | Frontend Tests | Playwright |
|---|---|---|---|---|---|---|---|
| Vorlauf (Arbeitsstand, vor letztem Fix) | ok | ok | 429/429 | **rot** (unformatierte Änderung in `runner_plan.rs` aus dem Fix B6, im selben Lauf entstanden) | ok / ok | 49/49 | 80/80 (5,3 min) |
| Abnahme 1 (finaler Stand) | ok | ok | 429/429 | ok | ok / ok | 49/49 | 80/80 (5,3 min) |
| Abnahme 2 (finaler Stand, direkt danach) | ok | ok | 429/429 | ok | ok / ok | 49/49 | 80/80 (5,2 min) |

Backend-Tests je Binary (finaler Stand): app lib 129 (+7), `api` 19, `branches` 9, `cli_contract` 26, `edge_cases` 11, `environments` 8, `failures` 19, `gate` 9, `parser_robustness` 6, `plan_steering` **13 (neu)**, `providers` 16, `pull_requests` 9, queued-api 30 (+6), queued-common 35 (+1), queued-runner 90 (+11) = **429** (vor WP06: 391). Frontend 49 (vor WP06: 47). Playwright 80 (vor WP06: 73; neu: 2 System, 3 Desktop, 1 × 2 iPhone; Tour erweitert).

Während der Entwicklung rot und behoben (siehe `WP06-review.md`): Plan sichtbar vor dem Löschen der Kopie (Integration), `echo` zerlegte `\n` im Mock-JSON (dash), `SEEN.md`-Konflikte paralleler Mock-Aufträge, iOS-Zoom im Plan-Editor, zu hohes sticky Kommentarformular, axe `label-content-name-mismatch` und `target-size`, Bestands-Kontrastfehler `.badge.attention`, Tour-Zeitfenster.

## 3. Was die Tests nicht belegen

- Codex, Gemini CLI und opencode wurden nie ausgeführt; ihre Plan-/Resume-Aufrufe sind nur als Argumentlisten getestet. Claude-Verhalten ist durch echte 2.1.322-Aufnahmen belegt (nicht die gepinnte 2.1.319); der Aufnahme-Lauf war ein manueller Beleglauf, kein Test.
- Container-Engines (Podman/Docker) mit ro-Mount und echten CLIs: nur die Container-Argumente sind getestet, kein Lauf im Agent-Image (nicht gebaut).
- Sitzungs-Fortsetzung über Prozess-Neustarts von Runner/App hinweg ist nur indirekt abgedeckt (Speicher liegt auf Platte; kein eigener Neustart-Test).
- Echte iOS-Geräte/WebKit: iPhone ist in Chromium emuliert (Viewport, Touch, simulierte Safe-Area-Insets).
