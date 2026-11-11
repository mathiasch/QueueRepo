# WP07 – Trigger, API & CLI: Plan

Grundlage: `work-packages.md` (Gemeinsame Regeln, WP07), Report P1.6 („Cron und einmalige Termine, `POST /triggers/:id/fire` mit Token und Freitext, kleines CLI“), Vorarbeit WP05 (`pr::poke` als Webhook-Schnittstelle) und WP06 (Steering, „Weiter mit …“, Plan-Modus). Branch `mvp`, nicht committet.

## 1. Ziel und Abgrenzung

| Im Umfang | Nicht im Umfang (Begründung) |
|---|---|
| Persönliche API-Tokens (gehasht, feine Scopes, Ablauf, Widerruf, `last_used`) | Ausgehende Webhooks (started/finished) – P1.6 nennt sie, WP07 in `work-packages.md` nicht; Benachrichtigungen sind WP10 |
| REST-API für Aufträge mit Token (anlegen, lesen, abbrechen, Nachrichten, „Weiter mit …“, Plan entscheiden) | Status-Kommentare zurück an GitHub-Issues (WP08 „Intake“) |
| Auftrags-Vorlagen pro Projekt | Linear/Slack/Jira (WP08) |
| Auslöser pro Projekt: Zeitplan (Cron, min. stündlich, Zeitzone), einmalig, API, generischer Webhook (HMAC), GitHub-Events mit Filtern | Projektgebundene Tokens (Token gilt für alle Projekte des Nutzers, Rollen greifen wie gewohnt) – als nicht blockierend dokumentiert |
| `POST /api/triggers/{id}/fire` (Token-Scope `triggers:fire`, optional Freitext) | |
| `queuectl` (Rust-Binary im Backend-Workspace): `add`, `list`, `status`, `watch`, `cancel`, `steer`, `continue`, `plan`, `fire`, `projects`, `whoami` | |
| UI: Tokens (einmalige Anzeige), Vorlagen, Auslöser, Secret einmalig, Auslöse-Verlauf | |

## 2. Architektur

```
                 ┌──────────── queued (App, keine Host-Rechte) ───────────────────────────┐
 Browser ──Cookie┤ auth::AuthUser ── Cookie: Session wie bisher (SameSite=Lax, JSON-Bodies)  │
 queuectl ─Bearer┤                └─ Bearer qk_…: api_tokens (SHA-256), Scope je Route aus   │
 CI/Skript       │                   queued_api::tokens::ROUTES (Default: verboten)           │
                 │ api/tokens.rs    Token-Verwaltung (nur Cookie)                             │
                 │ api/triggers.rs  Vorlagen, Auslöser, Verlauf, fire                         │
 GitHub ─HMAC───►│ api/hooks.rs     POST /api/hooks/{id}: Body-Limit, HMAC, Zeit/Replay,      │
 Webhook-Sender  │                  Filter ─► triggers::fire()  und  pr::poke() (nur Abfrage) │
                 │ triggers.rs      fire(): eine Transaktion (Zeilensperre, Dedupe, Rate-Limit,│
                 │                  „läuft schon“, Auftrag, Verlauf); Scheduler-Schleife      │
                 │ cron.rs          Cron-Auswertung mit Zeitzone (chrono-tz)                  │
                 └──────────────────────────────────────────────────────────────────────────┘
 queued-api:  tokens.rs (Scopes, Routen-Tabelle, Token-Format/Prüfsumme), triggers.rs (Wire-
              Typen, Cron-Syntax ohne Zeitzone, GitHub-Filter, Texte) – geteilt von App, UI, queuectl
 queued-common/secrets.rs: neue Scanner-Regel `queue_key` (qk_…, qwh_…)
 crates/queuectl: Binary, hängt nur an queued-api (keine App-Interna)
```

Kein Teil braucht Host-Rechte oder den Runner: Auslöser legen nur Aufträge an, die der bestehende Scheduler wie jeden anderen Auftrag verarbeitet (Gate, Review, Merge-Queue, Output-Modus unverändert).

### 2.1 Token-Authentifizierung

- Format `qk_` + 43 Zeichen Base62 (≈ 256 Bit aus `OsRng`) + 6 Zeichen Base62-CRC32 der 43 Zeichen. Prüfsumme: Tippfehler und Scanner-Fehlalarme ohne DB-Abfrage erkennbar. Gespeichert: SHA-256 (hex) des ganzen Tokens, eindeutig indiziert; angezeigt: Präfix `qk_` + die ersten 6 Zeichen.
- `AuthUser` (einziger Auth-Extraktor aller geschützten Handler): Steht `Authorization` im Request, gilt **nur** das Token (Cookie wird ignoriert). Ablauf: Format → Routen-Scope über `MatchedPath` + Methode aus `queued_api::tokens::ROUTES` → **nicht gelistet = 403** (Default-Deny; Konto, Einstellungen, Zugänge, Token-Verwaltung, Mitglieder, Secrets, Gate-Freigaben sind nie gelistet) → Hash-Lookup → widerrufen/abgelaufen = 401 → fehlender Scope = 403 → `last_used_at` (höchstens einmal pro Minute geschrieben).
- Rollen und Mandantentrennung unverändert: Das Token handelt als sein Nutzer, `require()`/`own_task()` greifen wie bei Cookies.
- CSRF: Cookie-Pfad unverändert. Bearer-Header setzt ein Browser nie von selbst; es gibt keine CORS-Freigabe.
- SSE `/api/events` mit Token: Stream endet spätestens beim Ablauf des Tokens bzw. nach 10 min (Widerruf wirkt so auch auf offene Streams); `queuectl watch` verbindet neu.

### 2.2 Auslöser und Ausführung

- **Vorlage** (`task_templates`): Name, Titel, Prompt, optional Anbieter/Modell, Plan-Modus, Limits. Keine Verbindung (Connector) – die sind pro Nutzer; aufgelöst wird beim Auslösen mit dem Zugang des Auslöser-Eigentümers (`routing::resolve_execute`), genau wie beim Anlegen.
- **Auslöser** (`triggers`): gehört einer Person (Eigentümer = deren Zugang wird verbraucht, Report P0.1: nie das Abo eines anderen). Arten `schedule`, `once`, `api`, `webhook`, `github`. Jede Art kann zusätzlich von ihrem Eigentümer per UI/API ausgelöst werden.
- **`triggers::fire`** in **einer** Transaktion: Auslöser-Zeile `FOR UPDATE` → aktiv? → Eigentümer noch ≥ Bearbeiter (sonst Auslöser deaktiviert, Verlauf „abgelehnt“) → Dedupe-Schlüssel (Slot, Delivery-ID, Body-Hash, Ereignis-Schlüssel) über partielle UNIQUE-Indizes → Rate-Limit (erzeugte Aufträge der letzten Stunde, Standard 10, max. 100) → „überspringen, solange ein Auftrag dieses Auslösers offen ist“ (Standard an) → Auftrag + Verlaufseintrag → Commit → `announce`. Fehler beim Auflösen des Zugangs ⇒ Verlauf „fehlgeschlagen“ ohne Dedupe-Schlüssel (Wiederholung möglich).
- **Nutzlast ist nicht vertrauenswürdig:** Sie landet nie in Titel oder Prompt, sondern im Kontext als Block `<<<EVENT_DATA_<nonce>` … `EVENT_DATA_<nonce>>>>` mit Regeln davor (Nonce nach Empfang erzeugt, Grenzen/`<<<`/`>>>` im Inhalt entschärft, Secrets geschwärzt, ≤ 16 000 Bytes). Titel = Vorlagentitel + feste, nicht vom Absender stammende Quelle („Zeitplan“, „GitHub push“ aus Allowlist).

### 2.3 Zeitpläne

- Cron mit 5 Feldern, Minute **eine feste Zahl** (⇒ höchstens einmal pro Stunde, auch bei Zeitumstellung: lokale Stunde 02:xx kommt höchstens zweimal im Abstand von 1 h vor), Stunde/Tag/Monat/Wochentag mit `*`, Listen, Bereichen, Schritten, Namen (`MON`, `JAN`); Vixie-Regel für Tag-ODER-Wochentag. Zeitzone IANA (`chrono-tz`). Nicht existierende Ortszeit (Frühjahr) ⇒ eine Stunde später; doppelte Ortszeit (Herbst) ⇒ nur die erste.
- Scheduler-Schleife (eigener Tokio-Task, `TRIGGER_TICK_SECS`, Standard 15): `SELECT … WHERE next_run_at <= now() FOR UPDATE SKIP LOCKED` in einer Transaktion je Auslöser. Slot = letzter fälliger Termin; Verlaufseintrag mit `scheduled_for` (UNIQUE pro Auslöser), Auftrag und neues `next_run_at` in **derselben** Transaktion ⇒ Absturz vor Commit = nichts passiert (wird nach Neustart genau einmal nachgeholt), nach Commit = kein zweites Mal. Mehrere Instanzen: Zeilensperre + UNIQUE.
- **Verpasste Läufe** (Server aus): mehr als 10 min nach dem Termin gilt ein Lauf als verpasst. Einstellung „Verpasste Läufe einmal nachholen“ (Standard an): genau ein Lauf, egal wie viele Termine verpasst wurden, Verlauf „nachgeholt (N verpasst)“; aus: Verlaufseintrag „verpasst“, kein Auftrag. Einmalige Termine: gleiche Regel, danach deaktiviert.

### 2.4 Webhooks

- `POST /api/hooks/{id}` (öffentlich, nicht unter Cookie/Token): Body-Limit 1 MiB (axum `DefaultBodyLimit` ⇒ 413), unbekannte/falsche Art ⇒ 404, Signatur falsch ⇒ 401 (nichts gespeichert), deaktiviert ⇒ 409, Rate-Limit ⇒ 429 mit `Retry-After`, Duplikat/gefiltert ⇒ 200, erzeugt ⇒ 202.
- **Generisch:** `X-Queue-Timestamp` (Unix-Sekunden, ±300 s), `X-Queue-Delivery` (1–100 Zeichen `[A-Za-z0-9._:-]`), `X-Queue-Signature: sha256=<hex>` = HMAC-SHA256(Secret, `"{timestamp}.{delivery}.{body}"`). Die Delivery-ID ist signiert, sonst hebelte eine neue ID den Dedupe aus. Vergleich konstant (`hmac::Mac::verify_slice`).
- **GitHub:** `X-Hub-Signature-256: sha256=<hex>` über den rohen Body, `X-GitHub-Event`, `X-GitHub-Delivery`. GitHub signiert keinen Zeitstempel ⇒ Replay-Schutz über Delivery-ID **und** SHA-256 des Bodys (beide UNIQUE pro Auslöser, Aufbewahrung 90 Tage; Restrisiko dokumentiert). `ping` ⇒ 200. Content-Type JSON oder `application/x-www-form-urlencoded` (`payload=`).
- **GitHub-Filter:** Ereignisse `push`, `issues`, `pull_request`, Check-Fehler (`check_run` completed mit Fehler-Conclusion); Branch-Muster (Glob aus `queued_common::glob`), Labels, Repository (Standard: Repo aus WP05), Bots ignorieren (Standard), Entwürfe ignorieren (Standard). Feste, dokumentierte Aktionsmengen je Ereignis. Schleifenschutz: Pushes auf `queue/*` und Pushes, deren Kopf-Commit von Queue committet wurde (`queue@localhost`), lösen nichts aus; Check-Fehler höchstens ein Auftrag pro Commit; Rate-Limit + „läuft schon“ als Netz.
- **WP05-Anbindung:** Jedes signaturgültige GitHub-Ereignis zu einem von Queue verfolgten PR (Nummer bzw. Head-Branch) ruft `pr::poke` – nur „jetzt abfragen“; Zustand kommt weiter ausschließlich aus der Host-API.
- Feldnamen aus offizieller Quelle belegen (`github/rest-api-description`, `github/docs`), sonst als unverifiziert markieren.

### 2.5 `queuectl`

- Crate `backend/crates/queuectl`, Binary `queuectl`; Abhängigkeiten: `queued-api` (Wire-Typen, Scopes, Token-Format), `reqwest` (rustls), `tokio`, `serde_json`, `clap`.
- Konfiguration: `QUEUE_URL`/`QUEUE_TOKEN` (Env) oder Datei `$QUEUECTL_CONFIG` bzw. `$XDG_CONFIG_HOME/queuectl/config` (`url = …`, `token = …`, `project = …`). Datei mit Token muss dem Nutzer gehören und Modus `0600` (bzw. strenger) haben, sonst Abbruch. Token nie in argv (keine Option dafür). Token-Format inkl. Prüfsumme wird vor dem ersten Request geprüft.
- `watch`: SSE (`/api/events`) mit Neuverbindung, Fallback Polling (`--poll`); Exit-Code 0 bei `done`, 1 bei `failed/blocked/cancelled`, 2 bei Nutzungsfehlern, 3 bei Auth/Netz.

## 3. Datenmodell (Migration `0010_triggers_api.sql`)

```sql
api_tokens     (id, user_id→users CASCADE, name ≤80, prefix, token_hash UNIQUE, scopes TEXT[] (CHECK Teilmenge),
                expires_at NOT NULL, created_at, last_used_at, revoked_at)
task_templates (id, project_id→projects CASCADE, created_by→users SET NULL, name ≤80 (UNIQUE pro Projekt), title ≤200,
                prompt ≤20000, provider, model, plan_mode, max_runtime_secs, max_cost_usd, max_tokens, max_turns,
                created_at, updated_at)
triggers       (id, project_id CASCADE, user_id CASCADE (Eigentümer), template_id→task_templates RESTRICT,
                kind CHECK, name ≤80, enabled, cron, timezone, run_at, next_run_at, catch_up, secret_enc BYTEA,
                github JSONB (Filter), max_per_hour 1–100, skip_if_active, disabled_reason, last_fired_at,
                created_at, updated_at; CHECKs je Art)
trigger_runs   (id, trigger_id CASCADE, source CHECK, outcome CHECK (created|ignored|skipped|rejected|failed),
                reason ≤500, task_id→tasks SET NULL, actor_id→users SET NULL, token_id→api_tokens SET NULL,
                event ≤100, scheduled_for, delivery_id, body_sha256, dedupe_key, created_at;
                partielle UNIQUE (trigger_id, scheduled_for|delivery_id|body_sha256|dedupe_key))
tasks.trigger_id → triggers SET NULL   (Herkunft im Auftrag, „läuft schon“-Prüfung)
```

Index `triggers (next_run_at) WHERE enabled AND next_run_at IS NOT NULL`. Verlauf: Aufräumen > 90 Tage und > 500 Einträge pro Auslöser im Scheduler (stündlich).

## 4. Scopes und Endpunkte

| Scope | Endpunkte (Token) |
|---|---|
| (jedes gültige Token) | `GET /api/whoami` |
| `projects:read` | `GET /api/projects`, `GET /api/projects/{id}`, `GET /api/projects/{id}/templates` |
| `tasks:read` | `GET /api/tasks`, `GET /api/tasks/{id}`, `GET /api/projects/{id}/tasks`, `GET /api/tasks/{id}/diff`, `GET /api/tasks/{id}/messages`, `GET /api/events` |
| `tasks:write` | `POST /api/tasks`, `PATCH /api/tasks/{id}`, `POST /api/tasks/{id}/cancel`, `POST /api/tasks/{id}/retry` |
| `tasks:steer` | `POST /api/tasks/{id}/messages`, `DELETE /api/tasks/{id}/messages/{mid}`, `POST /api/tasks/{id}/continue`, `POST /api/tasks/{id}/answer`, `POST /api/tasks/{id}/plan/{approve,revise,reject}` |
| `triggers:read` | `GET /api/projects/{id}/triggers`, `GET /api/triggers/{id}`, `GET /api/triggers/{id}/runs`, `GET /api/timezones`, `POST /api/triggers/preview` |
| `triggers:write` | `POST /api/projects/{id}/templates`, `PATCH/DELETE /api/templates/{id}`, `POST /api/projects/{id}/triggers`, `PATCH/DELETE /api/triggers/{id}`, `POST /api/triggers/{id}/secret` |
| `triggers:fire` | `POST /api/triggers/{id}/fire` |
| nie per Token | alles andere, insbesondere `/api/tokens*`, `/api/auth/*`, `/api/settings`, `/api/credentials*`, `/api/connectors*`, `/api/admin/*`, Mitglieder, Umgebung/Secrets, Ergebnis/Git-Zugang, Gate-Freigaben, Push/Benachrichtigungen |

Rechte: Vorlagen lesen ≥ Betrachter, ändern ≥ Bearbeiter. Auslöser lesen ≥ Betrachter (Secret nie), anlegen ≥ Bearbeiter (Eigentümer = Anlegende), ändern/auslösen/Secret erneuern nur Eigentümer des Auslösers, deaktivieren/löschen zusätzlich die Projekt-Eigentümerin. Fremde ⇒ 404.

## 5. UI (WP00-Designsystem)

- Einstellungen → Karte „API-Zugang“: Liste (Name, Präfix, Scopes als Tags, läuft ab, zuletzt benutzt, widerrufen), Formular (Name, Scopes als Checkboxen mit Erklärung, Ablauf 7/30/90/365 Tage), **einmalige Anzeige** in einem Dialog mit „Kopieren“ und Warnung, Widerrufen mit Bestätigung, Beispiel für `queuectl`.
- Projekt → Reiter „Automatisierung“: Vorlagen (Liste, anlegen/bearbeiten/löschen), Auslöser (Art wählen: Zeitplan, Einmalig, Webhook, GitHub, Nur per API; Cron mit deutscher Beschreibung, Zeitzone mit Vorschlagsliste, nächste Termine; GitHub-Filter), Secret und Adresse **einmalig** nach dem Anlegen bzw. Erneuern, „Jetzt auslösen“, Verlauf mit Ergebnis, Grund und Link zum Auftrag. Composer: „Aus Vorlage“ füllt Titel/Prompt.
- Safe Areas, Touch-Ziele ≥ 44 pt auf Touch, 16-px-Eingaben, axe grün (Desktop hell/dunkel, iPhone hoch/quer).

## 6. Tests (Plan)

| Ebene | Inhalt |
|---|---|
| Unit `queued-api` | Token-Format/Prüfsumme, Scope-Parsing, Routen-Tabelle eindeutig, Cron-Parser (Felder, Fehler, „min. stündlich“, Tag/Wochentag-Regel), GitHub-Filter (Branch, Labels, Aktionen, Bots, Entwürfe, Schleifenschutz) |
| Unit App | Cron mit Zeitzone (DST vor/zurück, 30-min-Zonen, 29. Feb.), verpasste Läufe, HMAC generisch/GitHub (Doku-Testvektor), Zeitfenster, Datenblock entschärft |
| Unit common | Scanner erkennt `qk_`/`qwh_`, Platzhalter nicht |
| Integration (`tests/triggers.rs`, `tests/tokens.rs`) | jede Token-Route mit falschem Scope ⇒ 403, nicht gelistete Route ⇒ 403, abgelaufen/widerrufen ⇒ 401, Cookie+Token ⇒ Token gilt, Mandanten; Webhook falsche Signatur/alter Zeitstempel/Replay/Body-Limit/Rate-Limit; GitHub-Filter end-to-end + `poke`; Cron ohne Doppelauslösung über Neustart und mit zwei Instanzen; verpasste Läufe; einmalige Ausführung; Eigentümer verliert Rechte |
| queuectl | Unit (Config, 0600, Argumente), Integration gegen App in-process |
| System-E2E | `queuectl` als echtes Binary gegen den echten Stack (add → watch → done, list/status/cancel/steer, Token aus 0600-Datei, 0644 abgelehnt); Webhook signiert per Node-`crypto` |
| UI-E2E | Token anlegen (einmalig sichtbar), widerrufen; Vorlage + Zeitplan + Webhook (Secret einmalig), Verlauf; iPhone hoch/quer; axe |

## 7. Risiken

| Risiko | Gegenmaßnahme |
|---|---|
| Token-Eskalation über vergessene Route | Default-Deny-Tabelle, Test iteriert über alle Router-Routen |
| Doppelte Läufe bei Neustart/mehreren Instanzen | eine Transaktion + Zeilensperre + UNIQUE; Neustart- und Zwei-Instanzen-Test |
| Agenten-Schleifen über Push/Check-Ereignisse | Branch-/Committer-Filter, ein Auftrag pro Commit, „läuft schon“, Rate-Limit |
| Prompt-Injection über Nutzlast | Datenblock mit Nonce, nie im Titel, Secrets geschwärzt |
| GitHub-Replay ohne Zeitstempel | Delivery-ID + Body-Hash; Restrisiko nach 90 Tagen dokumentiert |
| Zeitzonen/DST | eigene Auswertung über Ortszeit-Stunden, Tests für beide Umstellungen |
| GitHub-Feldnamen falsch | aus OpenAPI/Doku belegen, unbelegte markieren |
| Platte (17 GB frei, Backend-target 11 GB) | `CARGO_INCREMENTAL=0`, alte Test-Binaries löschen, `frontend/target` nach Tests löschen |
