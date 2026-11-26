# WP12 – Testbericht

Alle Tests laufen ohne echte externe Dienste: Agent = Mock-Provider, Modell-APIs = eigener Fake-Server (`tests/common/fake_llm.rs`), Dev-Server = `python3 -m http.server` bzw. ein Node-Einzeiler, Browser = Playwrights Headless-Chromium (`/opt/pw-browsers`). **Kosten: 0 USD** (kein Live-Aufruf eines Modells oder bezahlten Dienstes).

Builds dieser Läufe mit `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=line-tables-only` (Platte: 5 statt 16 GB im Backend-`target`; Testsemantik unverändert).

## 1. Neue Tests

| Ebene | Test | Prüft |
|---|---|---|
| Unit `queued-api` | `bon::bon_roundtrip_and_old_rows`, `bon::status_labels_…`, `preview::settings_validation`, `preview::viewports` | Wire-Typen, alte Zeilen, deutsche Texte, Validierung (Port, Pfade, Script-Injektion in Pfaden, Grenzen) |
| Unit `queued-common` | `preview_header_filters` | `Cookie`/`Authorization`/`X-Forwarded-*`/`Upgrade` nie zum Dev-Server; `Set-Cookie`/CSP/XFO/HSTS/`Clear-Site-Data`/CORS/COOP nie zum Browser |
| Unit Runner | `preview::validation_refuses_bad_requests`, `container_has_no_network_and_no_port_binding`, `browser_has_no_network_besides_loopback`, `bridge_forwards_unix_socket_to_loopback_port` (echtes Node), `timeout_stops_the_whole_process_group` (Enkel ignoriert TERM/INT), `screenshots_are_real_pngs_and_the_server_stops` (Chromium, Breite 1280 bzw. 780), `forwarding_filters_credentials_and_lifetime_ends_it` | Sandbox-Kommando, Brücke, Prozess-Aufräumen, Screenshots, Weiterleitung, Laufzeitende |
| Unit App | `routing::decision_precedence_…`, `conditions_attachments_and_min_chars`, `fix_decision_escalates_then_rules_then_inherits`, `rule_validation`; `bon::selector_is_never_weaker_and_prefers_another_provider`, `configured_review_step_…`, `answer_must_name_a_listed_candidate`, `select_request_fences_candidates_…`, `diffstat_and_verify_state`; `preview::base32_and_caps`, `origin_parsing_and_host_matching`, `csp_is_a_sandbox_…`, `paths_png_and_redirects` | Entscheidung, Prüfer-Wahl, Parser, Prompt-Abgrenzung, Origin, CSP |
| Integration `best_of_n.rs` | `attempts_are_isolated_selection_merges_and_losers_are_cleaned_up`, `concurrent_selections_pick_exactly_one`, `group_budget_stops_all_attempts`, `automatic_selection_by_a_fake_review_model`, `failed_attempts_get_no_fix_and_a_retry_starts_a_new_round`, `cancelling_the_group_stops_and_cleans_every_attempt`, `bounds_quota_and_cost_preview`, `routing_rules_escalation_and_explanations` | siehe Review-Tabelle |
| Integration `preview.rs` | `live_preview_is_member_only_sandboxed_and_never_sees_app_credentials`, `screenshots_after_ui_changes_are_real_pngs_for_members_only` | Proxy, Mitglieder, CSP, Cookie-Isolation, Stopp/Ablauf, Screenshots |
| Frontend | `logic::bon::targets_from_rows`, `estimate_and_labels`, `comparison_status_lines` | Composer-Ziele, Kostenvorschau-Text, Status-Zeile |
| E2E Desktop | `best-of-n.spec.ts`: Composer → Vergleich → Auswahl (axe hell/dunkel); Vorschau-Einstellungen, Screenshots, Vorschau im Sandbox-iframe auf `<cap>.localhost` (`self.origin == "null"`, `document.cookie` gesperrt, Link nach Stopp 404), axe | |
| E2E iPhone hoch/quer | `best-of-n-mobile.spec.ts`: Stepper ≥ 44 pt, Input ≥ 16 px, Karten innerhalb der Safe Areas, kein Querscrollen, Auswahl, axe | |

## 2. Angepasste bestehende Tests (mit Begründung)

- `queued_api::tests::task_roundtrip_and_accessors`: erwartet die neuen Felder (`attempts = 1` für alte Zeilen).
- `notifications::daily_summary_is_sent_once_across_instances_and_restarts` (WP10): schlug **tageszeitabhängig** fehl, nachweislich auch auf `5241d25` (eigener Worktree gebaut): `notify::tick` erzeugt mit der echten Uhr eine weitere Tageszusammenfassung, wenn der Test im echten Fälligkeitsfenster läuft. Der Test belegt die echten Tage vorab; geprüft wird unverändert.
- `branches::concurrent_claims_respect_project_parallelism` (WP04): im ersten vollen Lauf 1× rot (Runde 0: weniger als 5 Claims). Ursache ist im Design angelegt: Ein Claimer, dessen freie Projekte gerade alle gesperrt sind, gibt `None` zurück (`SKIP LOCKED`, der nächste Tick holt es nach). Unter Volllast tritt das auf; die zusätzliche Slot-Zählung von WP12 (`count(DISTINCT …)`) verlängert die Sperrzeit minimal. Der Test prüft weiter „nie überbucht“ in jeder Runde und füllt danach einzeln auf – **genau 5**, nicht mehr. Isoliert 5/5 grün vor der Änderung.

## 3. Mutative Gegenprobe

Siehe `WP12-review.md` (M1–M11): alle sicherheits- und kernlogik-kritischen Tests werden bei der Mutation rot; M2 bleibt grün, weil eine zweite, unabhängige Sperre greift (M2b = beide entfernt ⇒ rot).

## 4. Abnahmeläufe

Befehle wie `HANDOFF.md` §4 (Backend fmt/clippy/test, Frontend fmt/clippy wasm32+nativ/test/`trunk build --release`, Playwright alle Projekte inkl. axe), im Vordergrund nacheinander (Skript ohne `&`, über die Hintergrundfunktion der Sitzung gestartet).

| Suite | Lauf 1 | Lauf 2 | Referenz WP11 |
|---|---|---|---|
| Backend fmt / clippy | sauber | sauber | – |
| Backend `cargo test --workspace` | **644 bestanden**, 0 fehlgeschlagen | **644 bestanden**, 0 fehlgeschlagen | 609 |
| Frontend fmt / clippy wasm32 / clippy nativ | sauber | sauber | – |
| Frontend `cargo test` | **69 bestanden** | **69 bestanden** | 66 |
| `trunk build --release` | ok | ok | – |
| Playwright (system, desktop, iphone, iphone-landscape, inkl. axe) | **123 bestanden** (10,8 min) | **123 bestanden** (13,0 min) | 119 |

Beide Läufe auf demselben Stand (Baum identisch mit WIP-Commit `a07b51b`; Lauf 1 auf dem inhaltsgleichen Snapshot `3fa7062`). Zuwachs: Backend +35 (609 → 644), Frontend +3 (66 → 69), Playwright +4 (119 → 123: Desktop 2, iPhone 1, iPhone quer 1).

Rote Läufe davor (alle behoben, siehe Review): leere Vergleiche (B2), Prozess-Leck (B1), Stepper (B3), zweiter „Speichern“-Knopf (B7, 5 bestehende E2E-Tests rot), WP10-Tageszeit-Test, WP04-Claim-Test, sowie ein Lauf mit veraltetem Mutanten-Build (Werkzeug, siehe Review).

## 5. Nicht verifiziert

- Vorschau mit Docker/Podman (Brücke im Container, `exec` des Browsers, Chromium im Agent-Image): Kommando per Unit-Test, Brücke mit Host-Node, Image nicht gebaut.
- Echte Modelle als Prüfer der Auto-Auswahl (nur Fake-Server, Antwortformat ist unsere Annahme).
- Mehrere App-Instanzen gleichzeitig für das Live-Budget (nur die DB-Nachprüfung ist instanzübergreifend; im Test eine Instanz).
- Vorschau-Domain mit echtem Wildcard-DNS/-TLS (getestet mit `*.localhost`).
