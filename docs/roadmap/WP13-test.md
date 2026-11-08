# WP13 – Teststrategie und Ergebnisse

Umgebung: Linux-Container, rustc 1.97.0, Postgres 16 lokal, Trunk 0.22.1, Playwright mit Chromium, axe-core. **Keine echten Anbieter-APIs:** Agentenläufe über den Mock-Provider, Planung/Review/Analyse/Batch/LiteLLM gegen den lokalen Fake-Server `backend/crates/app/tests/common/fake_llm.rs` (Anthropic Messages + Message Batches, OpenAI Chat + Files + Batch, Mistral Files + Batch-Jobs, `/v1/models`, LiteLLM `/key/*`). Der Fake bildet **unsere Annahmen** über die Wire-Formate ab, keine Aufzeichnungen.

## 1. Abnahmeläufe (vollständiges Skript, ohne `E2E_SKIP_BUILD`)

Skript: fmt → clippy → `cargo test --workspace` (Backend) → fmt → clippy wasm32 → clippy nativ → `cargo test` → `trunk build --release` (Frontend) → `npx playwright test` (baut den Stack selbst). `CARGO_INCREMENTAL=0` wegen Plattenplatz.

| Gate | Runde 1 | Runde 2 | Runde 3 (nach Container-Neustart, Stand `d05dee5`) |
|---|---|---|---|
| Backend `cargo fmt --all -- --check` | rc=0 | rc=0 | rc=0 |
| Backend `cargo clippy --workspace --all-targets -- -D warnings` | rc=0 | rc=0 | rc=0 |
| Backend `cargo test --workspace` | **247 passed, 0 failed** (93 s) | **247 passed** (71 s) | **247 passed** (117 s) |
| Frontend `cargo fmt --all -- --check` | rc=0 | rc=0 | rc=0 |
| Frontend `cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings` | rc=0 | rc=0 | rc=0 |
| Frontend `cargo clippy --all-targets -- -D warnings` | rc=0 | rc=0 | rc=0 |
| Frontend `cargo test` | **39 passed** | **39 passed** | **39 passed** |
| `trunk build --release` | rc=0 | rc=0 | rc=0 |
| Playwright (alle Projekte) | **46 passed** (2,4 min) | **46 passed** (2,2 min) | **46 passed** (1,9 min) |

Runde 1 und 2 liefen direkt hintereinander ohne Codeänderung; Runde 3 nach dem Container-Neustart auf dem unveränderten Stand (WIP-Commit `d05dee5`, Arbeitsbaum ohne Abweichung) mit frisch gestartetem Postgres. Davor war eine Runde wegen einer inhaltlichen Korrektur (Batch-Review nur Opt-in, siehe Review B4) abgebrochen und neu gestartet worden.

Zahlen vor WP13 (WP00-Testbericht): Backend 190, Frontend 35, Playwright 37 → **+57 / +4 / +9**.

| Backend-Testbinary | Anzahl | davon neu |
|---|---|---|
| `queued` lib (Unit) | 106 | Preisformel/Katalog (8), Connectoren (4), Routing (3), Schätzung (3), Einzelschritte `llm` (5 inkl. SSRF), Batch (2), Gateway (1), Eval (3), opencode (2), Agent-Connector-Modi (4), CLI-Status (1) |
| `tests/providers.rs` (Integration WP13) | 16 | alle |
| `tests/cli_contract.rs` | 26 | opencode (2) |
| `tests/api.rs`, `failures.rs`, `edge_cases.rs`, `parser_robustness.rs` | 19 / 19 / 11 / 6 | – (unverändert grün) |
| `queued-api` | 10 | 4 |
| `queued-runner` | 28 | CLI-Probe (1) |
| `queued-common` | 6 | – |

Playwright 46 = system 12 (+2) + desktop 22 (+5) + iphone 6 (+1) + iphone-landscape 6 (+1).

## 2. Was die neuen Tests prüfen

**Unit (Backend):**
- Katalog vollständig, jede Zeile mit bekannter Quelle, IDs eindeutig, keine Doppelbelegung eines Modells an Stichtagen; Stichproben gegen Report/Notes (Opus/Sonnet/Haiku inkl. >100K, GPT-6/6.1, Mistral Large 4 vor/nach 29.10., Gemini-Sprünge 1.12./1.1., Infomaniak CHF, Regionsfaktoren).
- Formel reproduziert das M-Profil des Reports (Sonnet 1,245 $, Opus 2,49 $), Plan 0,28 $ (Opus), Batch −50 % mit 1-h-Write, Flex, EU +10 %, Long-Context pro Request (Haiku, Sol), `Meter` (Delta + Rest, CLI-Fallback, unbekannt, CHF nicht in USD).
- Admin-Overrides: ersetzen/ergänzen, Validierung (negative Preise, Währung, Faktor, Datumsreihenfolge, leere Modelle).
- Connectoren: Konsistenz aller Typen (Wire/Batch-Fähigkeit vs. Beschreibung), Datenstandort je Konfiguration, Konfig-/Secret-Validierung, Redaktion.
- Agent-Invocations: **kein Secret in argv für alle Zugangstypen**, Bedrock-Geo-Profil erzwungen (US-Profil am EU-Zugang abgelehnt, Modellpflicht), Vertex-ADC, Gateway ohne `--max-budget-usd`, Codex Flex/Azure/LiteLLM-`config.toml` ohne Schlüssel in der Datei, opencode-Konfig mit `{env:}`.
- Routing: Presets je vorhandenem Zugang (Legacy-Abo, Anthropic API, Mistral EU, OpenAI), Validierung (Residency, Review-Billigstufe, Batch/Direkt-Fähigkeit, Modellpflicht, Flex), Eskalationskette.
- Schätzung: Historienprofil ab 3 Läufen, Route R2 des Reports, Flex/EU-Aufschlag, unbekanntes Modell.
- SSRF-Schutz (14 Fälle), JSON aus Modellantworten, Auth-Header je Anbieter, Batch-JSONL je API.

**Contract (Backend):** opencode 1.19.5 – 7 konstruierte Fixtures (Herkunft in `PROVENANCE.md`: aus dem gebündelten Quelltext, Binary nie ausgeführt): Erfolg mit Tools/Usage, 429, `ProviderAuthError`, 503, `isRetryable` ohne Status, `ContextOverflowError`, Werkzeugfehler ≠ Lauf-Fehler.

**Integration (`tests/providers.rs`, App + echter Runner + Postgres + Fake-Server):**
1. Connector-CRUD: verschlüsselt in der DB, nie in Antworten, Validierungsmeldungen, fremde Nutzer 404, Projektzugänge nur durch Eigentümer, Nutzung nur im eigenen Projekt, Löschen bei offenen Aufträgen 409.
2. Residency: Legacy- und US-Zugang im EU-Projekt abgelehnt, US-Schritt in der Modellwahl abgelehnt, Zugang zieht nach dem Einreihen in die USA → Lauf blockiert (`config`), Lockern nur durch Eigentümer.
3. Kosten je Schritt aus dem Katalog (`usage_log`, `tasks.cost`, `usage.cost_usd`), CLI-Fallback markiert.
4. Live-Kostenlimit greift mit Katalogpreisen eine Usage-Zeile früher als mit CLI-Dollar.
5. Planung + Review als **Anthropic-Batch**: Auftrag wartet „beim Anbieter“, anderer Auftrag läuft bei Parallelität 1 trotzdem, 1-h-Cache am stabilen Präfix, keine Task-ID im Präfix, fremde `results_url` wird nicht aufgerufen, Batch-Preise im Kostendetail.
6. Review als **OpenAI-Batch** lehnt ab → Fix-Auftrag eskaliert auf das nächste Modell, Datei-Upload mit `purpose=batch`.
7. OnEmpty-Analyse als **Mistral-Batch** → vorgeschlagene Folgeaufträge (Freigabe nötig), Kosten `analyze`.
8. Direkte Aufrufe: 429/503 werden wiederholt, abgelehnter Schlüssel → Planung übersprungen, Zugang pausiert, Schlüssel nirgends sichtbar (`***`), Pause aufheben, Verbindungsprüfung ok/abgelehnt.
9. Abbruch während des Batches (Remote-Cancel), Neustart: Lease wird respektiert und durch `recover()` aufgehoben.
10. Verwaister `waiting_batch`-Auftrag → blockiert.
11. Presets/Validierung über die API, Auftrag ohne Angabe nutzt die Modellwahl.
12. Schätzung (Report-Profil, Historie n=3, unbekanntes Modell, Rollen) und Kostenübersicht (Projekt/Modell/Schritt, Mitglieder vs. Fremde).
13. Admin-Preise: nur Admin, Version + Historie, nächster Lauf nutzt den neuen Preis.
14. LiteLLM-Schlüssel: `max_budget`, Bearer-Master, Löschen, Redaktion, SSRF-Sperre ohne LAN-Freigabe.
15. CLI-Status über die Runner-Probe, `/api/meta` mit Typen, Notizen, Katalogversion.
16. **Eval-Set reproduzierbar:** zwei Läufe `mock-medium` identisch (7/8), `mock-weak` 3/8, keine Fix-Aufträge, Eval-Projekte versteckt, Löschen räumt auf.

**Frontend-Unit:** Geldformat (Kleinstbeträge, CHF, NaN/negativ), Schätzungstext, Zugangsauswahl-Werte, Beschriftungen.

**System (Playwright, getrennte Prozesse):** `/api/meta` (15 Typen, opencode-Pin, Gemini eingefroren, Katalogversion); Secret nicht im Response, Katalogpreis, Residency 400, Schätzung, Kosten nach echtem Lauf über die Prozessgrenze.

**E2E desktop:** Zugang anlegen (Formular, „Nicht verifiziert“, Datenstandort, kein Schlüssel im DOM/API, deutsche Servermeldung, Entfernen mit Bestätigung); EU-Vorlage übernehmen → Datenstandort EU, Composer-Schätzung mit „Daten: EU“ und Schritt-Tabelle, Server lehnt Claude ab, zurück zum Standard; Kosten in Details und Einstellungen; Preistabelle mit Quelle und Preissprung; CLI-Prüfung; Modell-Test mit dem Mock (3 von 8).

**E2E iPhone (hoch/quer, simulierte Safe Areas):** Zugangsformular und Modellwahl mit 16-px-Inputs, ≥ 44-pt-Zielen, ohne seitliches Scrollen, Vorlagen-Buttons frei von Insets, Schätzungsdetails passen.

**Accessibility:** Die Tour prüft zusätzlich Composer-Schätzung mit Tabelle, Zugangsformular, Preistabelle und Projekt-Modellwahl – axe WCAG 2.0/2.1/2.2 A+AA, hell und dunkel, Desktop und iPhone: 0 Verstöße.

## 3. Nicht getestet / nicht testbar hier

- Kein Aufruf echter Anbieter (Anthropic, OpenAI, Azure, Mistral, Infomaniak, OpenRouter, LiteLLM, Bedrock, Vertex, Gemini).
- opencode nie ausgeführt; Agent-Image nicht gebaut (opencode-Pin unbewiesen).
- Rückschreiben rotierter Abo-Dateien eines Connectors (kein Test, siehe Review N7).
- Eval-Set nur im Mock-Modus.
