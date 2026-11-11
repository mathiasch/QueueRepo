# WP07 – Trigger, API & CLI: Umsetzung

Grundlage: `WP07-plan.md`. Branch `mvp`, nicht committet (Koordinator sichert als WIP-Commit). Testbericht: `WP07-test.md`, Urteil: `WP07-review.md`.

## 1. Was gebaut wurde

| Baustein | Dateien | Kurz |
|---|---|---|
| Geteilte Typen/Texte | `backend/crates/api/src/tokens.rs` (neu) | Token-Format `qk_` + 43 Base62 + 6 Base62-CRC32 (`compose`, `well_formed`, `checksum`, eigene CRC32 mit Standard-Prüfwert getestet), Präfix `qwh_` für Webhook-Secrets, `Scope` (7 Berechtigungen, deutsche Texte), **Routen-Tabelle `ROUTES`** (Methode, Route, Berechtigung; nicht gelistet = verboten), `required()`, Wire-Typen (`ApiTokenView`, `NewApiToken`, `CreatedApiToken`, `WhoAmI`), Validierung (Name, Scopes, Ablauf 1–366 Tage) |
| | `backend/crates/api/src/triggers.rs` (neu) | `TriggerKind`, `RunSource`, `RunOutcome` (Texte, Töne), **Cron-Dialekt** (`Cron::parse` mit Listen/Bereichen/Schritten/Namen/`@daily`…, Minute fest ⇒ höchstens stündlich, „trifft nie zu“ erkannt, Vixie-Regel Tag ODER Wochentag, `next_after` generisch über `chrono::TimeZone` mit Regeln für Zeitumstellung, `upcoming`, deutsche Beschreibung), `GithubEvent`, `GithubFilter` + `validate_github`, `branch_matches` (eigene Glob-Semantik für Branches), Wire-Typen für Vorlagen, Auslöser, Verlauf, Auslösen, Vorschau, Hilfetexte; `Task.trigger_id` |
| Secret-Scanner | `backend/crates/common/src/secrets.rs` | Regel `queue_key` (`qk_`/`qwh_` + 49 Base62, hohe Konfidenz) ⇒ Redaction in Events/Logs, Gate vor Commit/Push |
| Migration | `backend/crates/app/migrations/0010_triggers_api.sql` | `api_tokens` (nur SHA-256, Scopes per CHECK, Ablauf Pflicht), `task_templates`, `triggers` (CHECKs je Art, Secret verschlüsselt), `trigger_runs` (Ergebnis, Grund, Auftrag, Akteur, Token; **vier partielle UNIQUE-Indizes** als Dedupe: Termin, Delivery-ID, Body-SHA-256, Ereignis-Schlüssel), `tasks.trigger_id` |
| Token-Authentifizierung | `backend/crates/app/src/auth.rs` | `AuthUser` (einziger Auth-Extraktor aller geschützten Handler): mit `Authorization` nur Token; Routenprüfung über `MatchedPath` vor dem DB-Zugriff; widerrufen/abgelaufen/unbekannt ⇒ 401; fehlender Scope ⇒ 403; `last_used_at` höchstens einmal pro Minute; `TokenAuth` als Request-Extension |
| Token-Verwaltung | `backend/crates/app/src/api/tokens.rs` (neu) | Liste, Anlegen (OsRng, gleichverteilt, einmalige Antwort, max. 50), Widerrufen (idempotent), Löschen, `whoami`; Cookie-only (nicht in `ROUTES`) |
| SSE mit Token | `backend/crates/app/src/api/mod.rs` | Token-Streams enden beim Ablauf, spätestens nach 10 min (Widerruf wirkt so auch auf offene Streams) |
| Auslösen & Scheduler | `backend/crates/app/src/triggers.rs` (neu) | `fire_locked` (Dedupe → Eigentümer ≥ Bearbeiter, sonst Auslöser aus + Benachrichtigung → Rate-Limit → „läuft schon“ → Auftrag über `routing::resolve_execute` mit dem Zugang des Eigentümers → Verlauf, alles in der Transaktion des Aufrufers mit Zeilensperre), `fire` (Sperre `FOR UPDATE`), `record_outcome` (Ping/gefiltert), `due` (verpasste Termine zu einer Entscheidung zusammengefasst, 10-min-Kulanz), `next_slot` (mind. 1 h Abstand), `tick` (`FOR UPDATE SKIP LOCKED`, eine Transaktion pro Termin, DB-Zeit), `prune` (90 Tage / 500 Einträge), Hintergrund-Schleife (`TRIGGER_TICK_SECS`, 0 = Tests steuern selbst) |
| Webhook-Logik | `backend/crates/app/src/webhooks.rs` (neu) | HMAC-SHA256 (`hmac` 0.12, Vergleich über `verify_slice` = konstante Zeit), generische Signatur über `timestamp.delivery.body`, GitHub-Payload (JSON und Form-Kodierung), **GitHub-Auswertung** je Ereignis mit Filtern und Schleifenschutz, `pr_refs` für WP05-`poke` |
| Webhook-Endpunkt | `backend/crates/app/src/api/hooks.rs` (neu) | `POST /api/hooks/{id}` mit `DefaultBodyLimit` 1 MiB; Reihenfolge Existenz → Signatur → signierte Werte (Zeitfenster ±300 s, Delivery-ID) → aktiv → Filter → `triggers::fire`; gültige GitHub-Ereignisse zu Queues PRs ⇒ `pr::poke` |
| Vorlagen/Auslöser-API | `backend/crates/app/src/api/triggers.rs` (neu) | CRUD Vorlagen (≥ Bearbeiter, Namen eindeutig, benutzte Vorlage nicht löschbar), CRUD Auslöser (Eigentümer = Anlegende; Ausschalten/Löschen auch Projekt-Eigentümerin), Secret erneuern, Verlauf, `fire` (201/200/409/429 mit `Retry-After`), Cron-Vorschau, Zeitzonenliste |
| Prompt | `backend/crates/app/src/prompt.rs` | `event_section`: Regeln vor dem Block `<<<EVENT_DATA_<nonce>`, Grenzen/`<<<`/`>>>`/fremde Block-Präfixe entschärft, Secrets geschwärzt, ≤ 16 000 Bytes; `EVENT_DATA_` zusätzlich in den WP05/WP06-Blöcken entschärft |
| `queuectl` | `backend/crates/queuectl/` (neu) | `whoami`, `projects`, `add` (stdin, `--template`, `--plan`, `--max-cost`, `-w`), `list`, `status`, `watch` (SSE mit Neuverbindung und 60-s-Stille-Erkennung, `--poll`, `--timeout`), `cancel`, `steer` (`--now`), `continue`, `plan approve/revise/reject`, `fire`; Konfiguration Env > Datei, Datei mit Token nur `0600` und eigene UID, Token-Format offline geprüft, `http://` nur Loopback, keine Redirects, Exit-Codes 0/1/2/3 |
| Frontend | `frontend/src/pages/tokens.rs`, `pages/automation.rs` (neu), `logic/triggers.rs` (neu), `pages/{settings,project,composer}.rs`, `ui/{mod,icons}.rs`, `style/app.css` | Einstellungen → „API-Zugang“ (Liste, Formular mit Vorschlägen und Erklärungen je Berechtigung, Dialog **einmalige Anzeige** mit Kopieren und `queuectl`-Anleitung, Widerrufen mit Bestätigung); Projekt-Reiter „Automatisierung“ (Vorlagen, Auslöser je Art, Cron-Vorschläge mit Live-Beschreibung, Zeitzonen-Vorschläge, GitHub-Filter, An/Aus, „Jetzt auslösen“, Secret-Dialog **einmalig** mit GitHub-Schritten bzw. Signatur-Beispiel, Verlauf mit Link zum Auftrag); Composer „Aus Vorlage“; `SecretBox`, `copy_text`, Icon `Copy` |
| Tests | `tests/tokens.rs`, `tests/triggers.rs` (neu), Unit in api/common/app/queuectl/frontend, `e2e/tests/system/triggers.spec.ts`, `ui/automation{,-mobile}.spec.ts`, Tour | siehe `WP07-test.md` |
| Doku | `README.md` | Abschnitt „Automatisierung“ (Zeitpläne, Webhook- und GitHub-Einrichtung, API-Tokens mit Endpunkt-Tabelle, `queuectl`), Tests, Architektur, Grenzen |

## 2. Entscheidungen

1. **Default-Deny über eine Tabelle statt Annotation pro Handler.** Alle geschützten Handler nutzen bereits `AuthUser`; dort prüft eine Stelle Methode + `MatchedPath` gegen `queued_api::tokens::ROUTES`. Eine neue Route ist für Tokens automatisch gesperrt; die Tabelle ist im README und in den Tests dieselbe. Ein Integrationstest ruft **jede** Tabellenroute mit „nur dieser Scope“ und „alle anderen Scopes“ auf. Damit ist auch belegt, dass `MatchedPath` unter `nest("/api")` den vollen Pfad liefert.
2. **Mit `Authorization` zählt nur das Token.** Sonst könnte ein Browser mit Session-Cookie und einem schwachen Token die Rechte des Cookies bekommen (oder umgekehrt Verwirrung über die Herkunft). CSRF-Schutz der Cookie-Session unverändert (SameSite=Lax, JSON-Bodies, keine CORS-Freigabe); Bearer-Header setzt kein Browser von selbst.
3. **Kein Auslöser darf fremde Zugänge verbrauchen** (Report P0.1). Auslöser gehören einer Person, nur sie startet sie; der Auftrag gehört ihr und läuft mit ihrem Zugang. Vorlagen enthalten deshalb keine Verbindung (Connectors sind persönlich), nur Anbieter/Modell, aufgelöst beim Auslösen.
4. **Genau einmal über die Datenbank**, nicht über Prozesszustand: Termin/Delivery/Body/Ereignis-Schlüssel sind UNIQUE pro Auslöser, Auftrag und Verlauf entstehen in derselben Transaktion wie das Weiterschalten des Termins. Abgelehnte/fehlgeschlagene Zustellungen behalten keinen Dedupe-Schlüssel (Absender darf wiederholen), Termine schon (der Termin ist verbraucht).
5. **Verpasste Termine zusammenfassen.** Nach einer Ausfallzeit nie eine Lawine: entweder genau ein nachgeholter Lauf oder ein Verlaufseintrag „verpasst“. Grenze 10 min, damit ein normaler Tick-Verzug nicht als „verpasst“ zählt.
6. **Mindestabstand 1 h auch bei Zeitumstellung** (`next_slot`), weil die Ortszeit-Auswertung in Zonen mit 30-min-Umstellung (Lord Howe) zwei Läufe im Abstand von 30 min ergäbe (Unit-Test).
7. **Signierte Delivery-ID beim generischen Webhook.** Sonst hebelt eine neue ID für einen abgefangenen Request innerhalb des Zeitfensters das Dedupe aus. GitHub signiert weder Zeit noch ID ⇒ Dedupe über ID **und** Body-Hash (GitHub-Doku: Redelivery behält die ID).
8. **Nutzlast nie außerhalb des Datenblocks.** Titel nur aus Vorlage + fester Quelle + Zahlen; Intro-Satz ohne Absendertext; auch das Repository steht im Block.
9. **Schleifen verhindern statt erkennen.** Push auf `queue/…`, Pushes mit Queue-Committer, Check-Fehler auf `queue/…` (WP05 zuständig), ein Auftrag pro Commit bei Checks, „läuft schon“ und Rate-Limit als Netz.
10. **WP05 nicht untergraben:** Ein GitHub-Ereignis zu einem PR von Queue ruft nur `pr::poke` (respektiert `Retry-After`); der Zustand kommt weiter aus der Host-API.
11. **`queuectl` hängt nur an `queued-api`** (Wire-Typen, Token-Format), nicht an App-Interna; HTTP über reqwest/rustls wie die App.

## 3. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| Webhook erzeugt ⇒ `202` | `201` (wie `fire`) | ein Code für „Auftrag angelegt“ in allen Pfaden |
| Intro-Satz des GitHub-Blocks nannte das Repository | Repository nur im Datenblock | Selbst-Review B1: ohne Repository-Filter ist der Name Absendertext |
| — | Token-SSE endet beim Ablauf/nach 10 min | Selbst-Review B2: im Plan zugesagt, zuerst nicht gebaut |
| — | Änderungen am Zeitplan nur bei echter Wertänderung | Selbst-Review B3: Umbenennen eines ausgeführten einmaligen Auslösers scheiterte („Zeitpunkt in der Vergangenheit“), weil die UI das ganze Formular schickt |
| — | Bot-Filter gilt nicht für Check-Fehler | CI läuft als Bot bzw. `ghost` (GitHub-Doku zu `sender`); sonst hätte der Filter jeden Check-Fehler verworfen |
| — | Composer „Aus Vorlage“ | Vorlagen sollen auch von Hand nutzbar sein (Auftrag „Vorlagen für Aufträge“) |
| — | Titelzeilen in Karten umbrechen statt zu schrumpfen (CSS) | iPhone-Screenshot: Vorlagenname schrumpfte auf einen Buchstaben |

## 4. Belege (offizielle Quellen, in dieser Sitzung abgerufen)

| Was | Quelle | Ergebnis |
|---|---|---|
| `X-Hub-Signature-256`, Format `sha256=<hex>`, HMAC über den Body, konstanter Vergleich, **Testvektor** („It's a Secret to Everybody“/„Hello, World!“ ⇒ `757107ea…`) | `github/docs` `content/webhooks/using-webhooks/validating-webhook-deliveries.md` | Unit-Test `github_documented_test_vector` |
| Header `X-GitHub-Event`, `X-GitHub-Delivery` (GUID), JSON oder `x-www-form-urlencoded`, Payload ≤ 25 MB, `sender` kann `ghost` sein | `github/docs` `webhook-events-and-payloads.md`, `data/reusables/webhooks/payload_cap.md` | umgesetzt |
| Redelivery behält `X-GitHub-Delivery`, Antwort binnen 10 s | `github/docs` `best-practices-for-using-webhooks.md` | Dedupe-Design; Antwort synchron ohne externe Aufrufe |
| Felder `ref`, `before`, `after`, `deleted`, `compare`, `head_commit.{id,message,committer.email,author.name}`, `commits[].{id,message,author.name}`, `pusher.name`, `repository.full_name`, `sender.{login,type}`; `issue.{number,title,body,html_url,labels[].name,user.login,pull_request}`, `label.name`, `action`-Enums `opened/reopened/labeled`; `pull_request.{number,title,body,draft,head.ref,head.sha,base.ref,html_url,labels}`, `synchronize`, `ready_for_review`; `check_run.{conclusion (Enum inkl. failure, timed_out, startup_failure), name, head_sha, html_url, details_url, output.{title,summary}, check_suite.head_branch, pull_requests[].number}`, `check_suite.{head_branch,pull_requests}`, Ereignisnamen `check_run`, `issue_comment`, `pull_request_review(_comment)`, `ping` (`zen`, `hook_id`) | `github/rest-api-description` `api.github.com.json` (`components/schemas/webhook-*`, `x-webhooks`) per Skript ausgewertet | alle vorhanden |
| **Nicht belegt** | — | Wert `"Bot"` von `sender.type` (Schema: nur `string`, Beispiel `"User"`); zusätzlich `[bot]`-Suffix im Login (Beispiel `copilot-swe-agent[bot]` in der Beschreibung) |

Keine Live-Calls an LLM-CLIs oder bezahlte APIs (Kosten 0); kein Aufruf an GitHub außer dem Abruf der genannten Doku-/Schema-Dateien von `raw.githubusercontent.com`.
