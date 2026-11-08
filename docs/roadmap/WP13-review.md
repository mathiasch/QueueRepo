# WP13 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP13-Diff auf `mvp` (Backend, Runner, geteilte Typen, Frontend, E2E, Doku). Prüfmaßstab: `work-packages.md` (Gemeinsame Regeln, WP13), Auftrag des Koordinators, Report und Notes. Jede Feststellung unten ist am Code bzw. an einem Testlauf geprüft, nicht aus dem Gedächtnis.

## Urteil

**Abnahmefähig mit offenen, dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und durch Tests abgesichert. Was bleibt, sind vor allem **nicht live verifizierbare Fremdschnittstellen** – das ist die ehrliche Grenze dieses Pakets, kein Stilproblem. Wer WP13 produktiv mit Bedrock, Vertex, Azure, Mistral-Batch, Infomaniak oder LiteLLM nutzen will, muss den ersten echten Lauf beobachten.

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **SSRF:** Die App ruft seit WP13 nutzerkonfigurierte URLs auf (Verbindungsprüfung, Planung/Review/Analyse direkt und als Batch, LiteLLM-Admin). Ein angemeldeter Nutzer hätte Loopback-, Link-Local-/Cloud-Metadaten- (`169.254.169.254`) und interne Dienste abtasten können; Fehlertexte (bis 800 Zeichen) wären zurückgekommen. | Code-Review `llm.rs`/`api/providers.rs` | `llm::check_target` vor **jedem** app-seitigen Request (`send_json`, Batch-`fetch_text`, Gateway): Link-Local/Unspecified/Multicast immer gesperrt, Loopback/private Netze/CGNAT nur mit `ALLOW_PRIVATE_LLM_ENDPOINTS=true`, Runner-Adresse immer gesperrt; Hostnamen werden aufgelöst | Unit `llm::tests::ssrf_guard` (14 Fälle), Integration `litellm_task_key_is_issued_and_revoked` (gesperrter Aufruf erreicht den Server nicht) |
| B2 | **Hängende Aufträge:** Stirbt der Prozess zwischen „Job fertig“ und „Task fortgesetzt“, bleibt der Auftrag ewig in `waiting_batch` (kein Slot, keine Anzeige eines Fehlers). | Review des Ablaufs `batch::complete` | `batch::sweep_orphans` in jedem Poller-Tick: `waiting_batch` ohne offenen Job und ohne Job-Ende in den letzten 2 min → `blocked`/`infra` mit verständlicher Meldung | Integration `orphaned_waiting_batch_task_is_blocked` |
| B3 | **opencode als Standard-/Review-Anbieter wählbar** (UI-Selects zeigten alle `providers`), obwohl es nur über Zugänge funktioniert → 400 bzw. stilles Überspringen des Reviews. | UI-Review | Selects filtern auf Anbieter mit eigenen Zugangsarten; Server lehnt `default_provider`/`review_provider` ohne Zugangsarten ab | `resolve_execute` (Aufträge), `put_settings`, `projects::update` |
| B4 | **Batch-Review als Voreinstellung** in den Presets widersprach dem Report („Opt-in neben dem agentischen Review, per A/B messen“) und WP13 („optional Review“). | Abgleich mit Report beim Schreiben dieses Reviews | Alle Presets prüfen agentisch; Batch-Review nur per eigener Modellwahl (API) | Unit `routing::tests::presets_resolve_against_available_accesses` |
| B5 | **Rotierte OAuth-Dateien eines Abo-Connectors** (z. B. ChatGPT-Abo) wären in `provider_credentials` unter `codex` mit Art `connector:codex_subscription` geschrieben worden – das hätte den Legacy-Zugang überschrieben und unbrauchbar gemacht. | Code-Review `agent_run` | Rückschreiben in `connectors.secret_enc` des Connectors (`ConnCfg.id`) | Code; **kein** automatisierter Test (siehe N7) |
| B6 | **axe `scrollable-region-focusable`** auf den Kostentabellen (iPhone) | Playwright a11y | Tabellen `tabindex="0"` + sichtbarer Fokus | `mobile-a11y.spec.ts` grün |
| B7 | **iOS-Zoom:** Radio-Inputs der Datenstandort-Wahl hatten 15 px | `mobile.spec.ts` | 16 px | `mobile.spec.ts` grün |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Risiko / Begründung |
|---|---|---|
| N1 | **Nicht live verifizierte Schnittstellen:** Message Batches, OpenAI-Batch, Mistral-Batch, Azure-v1-Pfad + `api-key`, Infomaniak-Basis-URL, LiteLLM-Admin-API, `/v1/models`-Prüfung, Vertex-ADC-Ablage, Bedrock-/Vertex-Variablen (nur im Binary vorhanden), Codex `service_tier="flex"`, Codex-Custom-Provider. Alle Tests laufen gegen einen selbst geschriebenen Fake-Server, der **unsere Annahmen** spiegelt. | Erster echter Lauf kann an Formatdetails scheitern. Im UI unter „Nicht verifiziert“ je Zugangstyp sichtbar; jede Annahme steht an genau einer Stelle im Code. |
| N2 | **opencode wurde nie ausgeführt** (Auto-Mode verbietet das Ausführen heruntergeladener Binaries). Flags, Event-Schema und Fehlernamen sind aus dem gebündelten Quelltext 1.19.5 belegt, die Fixtures konstruiert. | Abweichungen im Echtbetrieb möglich (z. B. Exit-Code-Verhalten, Provider-Konfig-Validierung). |
| N3 | **Agent-Image nicht gebaut** (keine großen Docker-Builds erlaubt): `opencode-ai@1.19.5` + `opencode --version`-Check sind unbewiesen. | Der Build-Check scheitert laut, falls die Ausgabe anders aussieht. |
| N4 | **Long-Context-Aufschläge** nur dort exakt, wo die CLI Verbrauch pro Anfrage meldet (Claude, opencode). Bei Codex und bei Restmengen (Subagents) wird zur Basisstufe gerechnet → Untergrenze, im Kostendetail als Hinweis vermerkt. Innerhalb eines Claude-Requests werden spätere Output-Deltas zur Basisstufe gezählt. | Unterschätzung bei Haiku > 100K bzw. OpenAI > 272K möglich. |
| N5 | **Cache-Writes von Claude** werden mit dem 5-min-Preis bewertet; der Stream enthält `ephemeral_1h_input_tokens`, das wird nicht ausgewertet. | Unterschätzung, falls Claude Code 1-h-Caching nutzt (Annahme, dokumentiert). |
| N6 | **Abbruch-Rennen:** Bricht der Nutzer genau ab, während der Poller das Review abschließt, kann `finish()` (ohne Statusbedingung, bestehende Funktion) den Abbruch mit `done` überschreiben. Fenster: Millisekunden. | Kosmetisch/selten; `queue::finish` für alle Pfade umzubauen wäre ein Eingriff in WP01-Logik. |
| N7 | B5 (Rückschreiben rotierter Abo-Dateien eines Connectors) ist nicht automatisiert getestet: der Mock rotiert keine Dateien, echte CLIs mit gültigen Abos gibt es hier nicht. | Fehler würde erst bei einem echten ChatGPT-/Google-Abo-Connector auffallen. |
| N8 | **DNS-Rebinding** zwischen Prüfung und Request ist durch den SSRF-Schutz nicht abgedeckt (reqwest löst erneut auf). | Angriff braucht eine eigene Domain mit kurzer TTL; Link-Local bleibt bei Wechsel trotzdem nur mit Glück erreichbar. Härtung: eigener Resolver → WP02/WP03. |
| N9 | **Projekt-Zugänge:** Bearbeiter nutzen den Schlüssel der Eigentümerin, der Agent sieht ihn im Container (wie beim eigenen Schlüssel). | Im Formular erklärt („Projektzugänge dürfen alle Bearbeiter nutzen“). |
| N10 | **opencode liest `opencode.json` aus dem Repository** (wie Gemini den Workspace vertraut). | Ein Repo kann Konfiguration einschleusen → WP03. |
| N11 | **Modellwahl-UI nur per Vorlage**; freie Kombination nur über die API. | Bewusst für Bedienbarkeit; Server validiert identisch. |
| N12 | **Abo-Läufe werden zu API-Listenpreisen bewertet.** | Im Hilfetext erklärt („was der Auftrag per API gekostet hätte“). |
| N13 | **CHF ohne Wechselkurs:** CHF-Kosten zählen nicht in USD-Budgets/-Limits. | Kein belegter Kurs in den Quellen; getrennt ausgewiesen. |
| N14 | **Eval-Set klein** (8 Shell-Aufgaben) und nur im Mock-Modus gelaufen. Die echten Erfolgsquoten (Report: p-Werte sind Annahmen) sind damit **nicht** gemessen. | Grundlage für WP15 steht; echte Läufe kosten Geld und brauchen Schlüssel. |
| N15 | **Kosten unbekannter Modelle = 0** in Budgets (Quelle `unknown`), z. B. eigener vLLM ohne Admin-Preis oder Claude ohne Modellangabe ohne CLI-Dollarwert. | Im UI als „kein Preis bekannt“ bzw. „unvollständig“ markiert; Admin kann Preise ergänzen. |
| N16 | **Lizenz/ToS** der Abo-Nutzung bleibt ausgeklammert (Auftrag); Hinweistext an den Abo-Zugangstypen, nichts blockiert. | – |

## Geprüft und für gut befunden

- **Secrets:** nie in argv (Unit-Tests für alle 16 Zugangstypen und beide Laufarten), nie in API-Antworten (`has_secret`), nie in Task-Events (Redaktion `***` bei Fehlertexten, Integrationstest mit abgelehntem Schlüssel), verschlüsselt mit dem bestehenden `crypto.rs`, `Debug` von `Resolved`/`Endpoint` ohne Secret.
- **Architektur:** App ohne Host-Rechte; Datei-/Prozessarbeit (Eval-Setup, CLI-Probe) über den Runner; Runner-Probe nur mit fester CLI-Liste, ohne Nutzereingabe, gleiche Sandbox wie Agentenläufe; keine Änderung am Sicherheitsmodell des Runners; Migration nur neu (0004).
- **Residency serverseitig** an allen Stellen: Anlegen/Ändern von Aufträgen, Modellwahl, vor jedem Lauf, Review, Planung, Analyse, Folge- und Fix-Aufträge (über den Lauf), Lockern nur durch Eigentümer (Integrationstest).
- **Preise:** jede Zeile mit Quelle und Belegstufe; die Formel reproduziert die Tabelle des Reports (Opus 0.92/2.49/6.88 $, Sonnet 0.46/1.25/3.44 $, Terra 0.23/0.62/1.72 $, Large 4 0.13/0.41/1.24 $, Flash 0.18/0.53/1.55 $; Unit-Test für das M-Profil). Keine Preise ohne Quelle (Qwen3.5-397B bewusst weggelassen).
- **Scheduler:** `waiting_batch` belegt keinen Slot (Integrationstest mit Parallelität 1), Pausen pro Connector blockieren nur diesen Zugang.
- **Alt-Verhalten:** Alle WP01/WP00-Tests unverändert grün; keine E2E-Prüfung abgeschwächt (nur ergänzt: Tour prüft Zugangsformular, Preistabelle, Schätzungsdetails und Modellwahl mit axe).
