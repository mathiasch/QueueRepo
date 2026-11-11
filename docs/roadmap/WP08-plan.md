# WP08 – Intake-Integrationen: Plan

Grundlage: `work-packages.md` (Gemeinsame Regeln, WP08), Report P2.6 („Forge-Issues per Label oder Assignment, dann ein Slack-Bot, dann Linear über die Agent-API mit Delegate-Modell: der Mensch bleibt Owner, der Agent muss binnen 10 s eine *thought*-Activity senden“), Vorarbeit WP05 (Host-Adapter, SSRF-Client ohne Redirects, Rechte über die Host-API), WP06 (Nachrichten an Aufträge mit Längenlimit und eigenem Datenblock, „Weiter mit …“), WP07 (HMAC-Prüfung in konstanter Zeit, Dedupe in der DB, Rate-Limit unter Zeilensperre, Nutzlast nur als `EVENT_DATA`-Block, Body-Limit, 404/401/409/429-Semantik). Branch `mvp`, nicht committet.

## 1. Ziel und Abgrenzung

| Im Umfang | Nicht im Umfang (Begründung) |
|---|---|
| **GitHub Issues:** Issue mit Label (beim Öffnen oder `labeled`) oder Erwähnung (Text im Issue oder neuer Kommentar) ⇒ Auftrag; Status-Kommentare zurück; Antwort auf Rückfragen per Issue-Kommentar | GitHub-App-Installation (JWT/Installation-Token): eine Integration nutzt ein Fine-grained-PAT; ein Repository = ein Mandant |
| **Linear Agent-API:** `AgentSessionEvent` `created` (Issue delegiert/Agent erwähnt) ⇒ Auftrag; Activities zurück (`thought` sofort, `elicitation` bei Rückfrage, `response`/`error` am Ende); `prompted` ⇒ Antwort/Nachricht/„Weiter mit …“ | OAuth-Flow (Installation der Linear-App): der Nutzer trägt das `actor=app`-Token und das Webhook-Secret ein |
| **Slack:** `app_mention` und Slash-Command ⇒ Auftrag; Updates als Antworten im Thread; Antworten im Thread ⇒ Rückfrage beantworten/Nachricht | Slack-OAuth-Installation, interaktive Buttons, `response_url`-Nachrichten |
| **Jira Cloud:** Issue mit Label (`jira:issue_created`/`jira:issue_updated`) ⇒ Auftrag; Kommentar zurück; Antwort per Kommentar (`comment_created`) | **App-JWT (Connect):** braucht den Installations-Handshake (`installed`-Lifecycle, Shared Secret pro Site) und ist ein eigenes App-Modell; umgesetzt wird die Variante **Cloud-Webhook mit Secret** (`X-Hub-Signature`). Jira Server/Data Center (keine signierten Webhooks belegt) |
| Gemeinsame Abstraktion „Intake-Quelle“ (Prüfung, Auswertung, Rückkanal je Art), Verknüpfung Auftrag ↔ externes Objekt, Zuordnung externer Nutzer ↔ Queue-Nutzer, Ausgangs-Warteschlange mit Retry/Backoff/Idempotenz | Mehrere Projekte pro Workspace/Org (verboten: strikte Mandantentrennung) |
| **Fix WP06-N8:** `POST /api/tasks/{id}/answer` läuft über den Nachrichtenpfad (Längenlimit, eigener Datenblock), weil externe Antworten denselben Weg nehmen | Benachrichtigungen allgemein (WP10) |
| UI: Projekt-Reiter „Anbindungen“ (verbinden, Einrichtungsschritte, Adresse/Secret einmalig, Zuordnung von Personen, Verlauf), Verknüpfung im Auftrag mit Link | |
| README (Deutsch): Einrichtung je Integration mit minimalen Rechten/Scopes | |

## 2. Architektur

```
                 ┌──────────────────────── queued (App, keine Host-Rechte) ────────────────────────────┐
 GitHub ──HMAC──►│ api/intake.rs  POST /api/intake/{id} (öffentlich, 1 MiB, Signatur = Authentisierung)  │
 Linear ──HMAC──►│   ├─ Integration laden (404) ─► Quelle::verify (401, nichts gespeichert)              │
 Slack  ──v0 ───►│   ├─ Quelle::parse ─► Ereignis {Neu | Antwort | Ignorieren | Challenge}              │
 Jira   ──HMAC──►│   └─ intake::accept: EINE Transaktion mit Integrations-Zeilensperre:                  │
                 │        Mandant prüfen → Dedupe (Delivery) → Person zuordnen (restriktiv) → Rechte     │
                 │        → Rate-Limit → offener Auftrag zum Objekt? → Auftrag + Verknüpfung + Outbox    │
                 │        bzw. Antwort/Nachricht/„Weiter mit …“ über steer::post_message/continuation    │
                 │ intake.rs     Hintergrund-Schleife (eigener Tokio-Task, blockiert den Scheduler nie): │
                 │   sync: Zustand der verknüpften Aufträge → Outbox-Zeilen (UNIQUE Schlüssel je Phase)   │
                 │   deliver: Outbox mit Lease (FOR UPDATE SKIP LOCKED), Retry mit Backoff, Retry-After   │
                 │ intake/{github,linear,slack,jira}.rs  Prüfung + Auswertung + Rückkanal je Art          │
                 │   Rückkanal nur über den SSRF-Client der App (llm::guarded_builder, keine Redirects)   │
                 └─────────────────────────────────────────────────────────────────────────────────────┘
 queued-api: intake.rs (Arten, Texte, Wire-Typen, Validierung, Phasen) – geteilt von App und UI
```

### 2.1 „Intake-Quelle“ (Verallgemeinerung von WP07, keine Kopie)

- **Signaturprüfung:** alle Arten benutzen `webhooks::verify`/`signature` (HMAC-SHA256, Vergleich per `verify_slice` in konstanter Zeit) aus WP07; neu ist nur, *was* signiert wird:
  - GitHub: `X-Hub-Signature-256: sha256=<hex>` über den rohen Body (wie WP07, Doku-Testvektor).
  - Slack: `X-Slack-Signature: v0=<hex>` über `v0:{X-Slack-Request-Timestamp}:{body}`, Zeitstempel ±300 s (Beleg: `slackapi/bolt-js` `verify-request.ts`, `python-slack-sdk` `signature/__init__.py`; **Testvektor** aus den SDK-Tests, der dem Beispiel der Slack-Doku entspricht).
  - Linear: `Linear-Signature: <hex>` = HMAC-SHA256 über den rohen Body; `webhookTimestamp` (ms) im signierten Body, höchstens 60 s Abweichung (Beleg: `linear/linear` `packages/sdk/src/webhooks/client.ts`).
  - Jira Cloud: `X-Hub-Signature: sha256=<hex>` über den rohen Body (Beleg: Sekundärquelle `jira.js` 6.4.0 `src/webhooks/{verify,headers}.ts`; die Atlassian-Doku ist hier nicht abrufbar ⇒ im Bericht als nicht primär belegt markiert).
- **Replay/Dedupe:** in der DB wie WP07 (partieller UNIQUE-Index pro Integration): GitHub `X-GitHub-Delivery` + Body-SHA-256 (kein signierter Zeitstempel), Slack `event_id` bzw. `trigger_id` (Slash-Command) + Zeitfenster, Linear Signatur (SDK-Empfehlung) + 60-s-Fenster, Jira `X-Atlassian-Webhook-Identifier` (bleibt über Wiederholungen gleich) + Body-SHA-256.
- **Untrusted Input:** Issue-Texte, Slack-Nachrichten, Linear-`promptContext`, Jira-Beschreibungen gehen nur über `prompt::event_section` in den Kontext (Nonce-Block, Grenzen entschärft, Secrets geschwärzt, ≤ 16 000 Bytes). Titel des Auftrags nur aus festen Wörtern + Nummer/Schlüssel (z. B. „GitHub-Issue #12“), nie Absendertext; der externe Titel steht nur im Block und – escaped – im UI als Linktext.
- **Antworten** (Kommentar, Thread-Antwort, `prompted`) laufen über den WP06-Nachrichtenpfad (`steer::post_message`): Längenlimit 4 000 Zeichen, eigener `USER_MSG`-Block mit Regeln, Quelle „über Slack/GitHub/…“ im Kopf. Wartet der Auftrag auf eine Antwort, ist die Nachricht die Antwort; läuft er, wird sie zugestellt; ist er fertig, entsteht ein „Weiter mit …“-Auftrag (WP06, gleiche Rechte), mit Verknüpfung zum selben Objekt.

### 2.2 Zuordnung extern → intern (restriktiv)

- Eine Integration gehört **einem** Projekt; `(Art, Mandant)` ist global eindeutig (GitHub: `owner/repo`, Slack: `team_id`, Linear: `organizationId`, Jira: Site-URL). Jedes Ereignis wird nach der Signatur gegen den Mandanten geprüft (eine Slack-/Linear-App kann in mehreren Workspaces installiert sein und signiert alle mit demselben Secret) ⇒ fremder Mandant = 403, nichts passiert.
- Personen-Zuordnung `intake_identities`: externe ID (GitHub numerische User-ID, Slack User-ID, Linear User-ID, Jira `accountId`) → Queue-Nutzer. **Aktiv nur mit beiden Zustimmungen:** Projekt-Eigentümerin genehmigt (wer darf im Projekt handeln) **und** die Person bestätigt selbst (deren Zugang wird verbraucht, Report P0.1: nie das Abo eines anderen). Wer sich selbst zuordnet und Eigentümerin ist, ist sofort aktiv.
- Beim Ereignis: Zuordnung aktiv **und** Person noch ≥ Bearbeiter (Auftrag anlegen) bzw. darf steuern (Antwort: Ersteller ≥ Bearbeiter oder Eigentümer). Sonst abgewiesen (Verlauf „abgelehnt“ mit Grund; Linear bekommt eine `error`-Activity, weil die Sitzung sonst hängt; GitHub/Jira/Slack bekommen nichts, um Spam/Verstärkung zu vermeiden).
- **Opt-in „Alle aus dem Workspace dürfen“** (`allow_unmapped`, Standard aus): nicht zugeordnete Personen handeln dann als die Eigentümerin der Integration (deren Zugang, deren ausdrückliche Einwilligung durch das Einschalten). GitHub verlangt dabei zusätzlich `author_association` ∈ {OWNER, MEMBER, COLLABORATOR} (Host-Signal ohne Zusatzaufruf, Enum aus OpenAPI belegt), weil öffentliche Repos sonst jedem offenstünden.

### 2.3 Rückmeldungen nach außen

- Nur über die Ausgangs-Warteschlange `intake_outbox`; Hintergrund-Schleife `intake::run` (eigener Tokio-Task, `INTAKE_TICK_SECS`, Standard 5 s; Weckruf bei neuem Eintrag). Der Scheduler ruft nie extern auf.
- Phasen je Verknüpfung: `accepted` (sofort – Linear verlangt eine Activity binnen 10 s), `question:<hash>` (Rückfrage), `done`, `failed` (failed/blocked), `cancelled`. Eine Outbox-Zeile pro (Integration, Schlüssel) – UNIQUE ⇒ jede Phase höchstens einmal, auch bei mehreren Instanzen/Neustart.
- Zustellung: Lease 60 s (`FOR UPDATE SKIP LOCKED`), Timeout 15 s, Backoff 5 s·2^n bis 1 h, `Retry-After`/429 respektiert, nach 8 Versuchen „fehlgeschlagen“ (sichtbar im Verlauf).
- **Idempotenz auch bei verlorener Antwort:** Linear: vorab erzeugte Activity-`id` (Schema: „identifier in UUID v4 format“); GitHub: unsichtbarer Marker `<!-- queue:<outbox-id> -->`, vor einer Wiederholung werden die Kommentare seit Erstellung danach durchsucht; Jira: Referenzzeile „Queue · Ref …“, gleiche Suche (`orderBy=-created`); Slack: `metadata.event_payload.ref`, Suche per `conversations.replies` mit `include_all_metadata` (Scope `channels:history` ist für Thread-Antworten ohnehin nötig).
- **Inhalt:** feste deutsche Texte + Zusammenfassung des Agenten (Secret-Scanner, ≤ 1 500 Zeichen) + Rückfragen (Scanner, je ≤ 500) + Link zum Auftrag (`PUBLIC_URL`). **Kein Diff** (nur Hinweis auf Queue) – damit kann kein Code-Ausschnitt mit Geheimnis hinaus; Tokens nie im Text (zusätzlich: Token-String wird vor dem Senden aus jedem Text entfernt).
- Eigene Rückmeldungen lösen nichts aus (Schleifenschutz): GitHub-Kommentare mit Marker und Bot-Absender werden ignoriert, Slack-Nachrichten mit `bot_id`/`subtype`, Linear nur `prompt`-Activities von Menschen, Jira-Kommentare mit Referenzzeile.

### 2.4 Secrets

- Webhook-Secret und API-Token AES-256-GCM (`crypto.rs`, `MASTER_KEY`), nie in API-Antworten (nur „gesetzt“), nie in Logs/argv/Events; Fehlertexte des Rückkanals über `secrets::redact` + Token-Ersetzung. GitHub-/Jira-Secret erzeugt Queue (`qwh_…`, Scanner-Regel aus WP07) und zeigt es einmal; Slack-/Linear-Secret kommt vom Anbieter und wird eingetragen.

## 3. Datenmodell (Migration `0011_intake.sql`)

```sql
integrations       (id, project_id CASCADE, owner_id→users CASCADE, kind CHECK(github|linear|slack|jira), name ≤80,
                    tenant ≤200, enabled, secret_enc BYTEA NOT NULL, token_enc BYTEA, account ≤200 (Jira-E-Mail),
                    config JSONB (Label, Erwähnung, allow_unmapped, Hinweise), max_per_hour 1–100, disabled_reason,
                    created_at, updated_at, UNIQUE (kind, tenant))
intake_identities  (id, integration_id CASCADE, external_id ≤200, external_name ≤200, user_id→users CASCADE,
                    owner_ok BOOL, user_ok BOOL, created_by SET NULL, created_at, UNIQUE (integration_id, external_id))
intake_links       (id, integration_id CASCADE, task_id UNIQUE→tasks CASCADE, external_ref ≤300, external_url ≤500,
                    external_title ≤300, reply_to JSONB, reported ≤200, created_at)
intake_outbox      (id, integration_id CASCADE, link_id→intake_links CASCADE NULL, key ≤200, kind CHECK, body ≤6000,
                    reply_to JSONB, remote_id ≤200, state CHECK(pending|sent|failed), attempts, next_attempt_at,
                    lease_until, last_error ≤500, created_at, sent_at, UNIQUE (integration_id, key))
intake_events      (id, integration_id CASCADE, outcome CHECK, reason ≤500, event ≤100, external_user ≤200,
                    task_id SET NULL, delivery_id ≤200, body_sha256, created_at; partielle UNIQUE je Integration)
task_messages.via  TEXT CHECK (github|linear|slack|jira)   -- Herkunft einer Nachricht (Anzeige, Prompt-Kopf)
```

Aufräumen: Ereignisse > 90 Tage / > 500 pro Integration, gesendete Outbox-Zeilen > 30 Tage (Intake-Schleife, stündlich).

## 4. Endpunkte

| Endpunkt | Wer |
|---|---|
| `POST /api/intake/{id}` | öffentlich, Signatur; Slack Events + Slash-Command an dieselbe Adresse |
| `GET/POST /api/projects/{id}/integrations` | lesen ≥ Betrachter (ohne Secrets), anlegen nur Eigentümerin |
| `PATCH/DELETE /api/integrations/{id}`, `POST …/secret` | nur Eigentümerin |
| `GET /api/integrations/{id}/events` | ≥ Betrachter |
| `GET/POST /api/integrations/{id}/identities`, `POST …/identities/{iid}/confirm`, `DELETE …/identities/{iid}` | lesen ≥ Betrachter; anlegen: Eigentümerin (für Mitglieder) oder Mitglied ≥ Bearbeiter für sich selbst; bestätigen: die zugeordnete Person bzw. die Eigentümerin; löschen: beide |
| `TaskDetail.intake` | Verknüpfung (Art, Bezeichnung, Link) im Auftrag |

Keine dieser Verwaltungsrouten in der Token-Tabelle (WP07 Default-Deny ⇒ nur Cookie).

## 5. UI (WP00-Designsystem)

- Projekt → Reiter **„Anbindungen“**: Karten je Integration (Art-Icon, Name, Mandant, Status, „Rückmeldungen aktiv“), Leerzustand mit Erklärung; „Verbinden“ ⇒ Art wählen (GitHub, Linear, Slack, Jira mit Ein-Satz-Erklärung) ⇒ Formular mit verständlichen Feldern und Hilfe (wo finde ich die Workspace-ID?), Erweitert (Limit, „alle dürfen“ mit Warnung); danach **einmaliger Dialog** mit Adresse, Secret (GitHub/Jira) und nummerierten Einrichtungsschritten inkl. minimaler Rechte.
- Je Integration: **Personen** (externe ID + Name → Projektmitglied; Zustand „aktiv“, „wartet auf Bestätigung von …“, Buttons Bestätigen/Entfernen; „Das bin ich“ für Mitglieder), **Verlauf** (Ergebnis, Grund, Auftrag öffnen), An/Aus, Löschen mit Bestätigung.
- Auftrags-Drawer: Chip „Aus GitHub-Issue #12“ mit externem Link (`rel="noopener noreferrer"`, nur http(s)), Nachrichten zeigen „über Slack“.
- Safe Areas, Touch-Ziele ≥ 44 pt, 16-px-Eingaben, axe grün (Desktop hell/dunkel, iPhone hoch/quer).

## 6. Tests (Plan)

| Ebene | Inhalt |
|---|---|
| Unit `queued-api` | Arten/Texte, Validierung (Mandant je Art, Label, Erwähnung, Jira-Site-URL), Phasen-Schlüssel |
| Unit App | Signatur je Art mit Doku-Testvektoren (GitHub, Slack), Zeitfenster Slack (±300 s) und Linear (60 s), Auswertung je Art (Label/Erwähnung/Bots/eigene Kommentare/Thread-Antwort/Slash-Command/`prompted`), Mandant, ADF→Text, Rückkanal-Texte ohne Secrets, Backoff |
| Integration `tests/intake.rs` | **je Integration:** gültige/ungültige Signatur, Replay ⇒ Duplikat, falscher Mandant, nicht zugeordnete Person abgewiesen (und mit Opt-in erlaubt), Auftrag mit Verknüpfung + Datenblock, **Statusrückmeldung idempotent** (Fake-Server zählt Aufrufe; zweimaliges Sync, verlorene Antwort ⇒ kein Duplikat), **Rückfrage-Antwort über den externen Kanal** (Mock `MOCK_ASK`) bis „erledigt“; Rechte der Verwaltungs-API, Mandanten, Secrets nie in Antworten; `answer`-Fix (Limit, Trennung) |
| System-E2E | echter Stack: signierte Zustellungen per Node-`crypto` je Art, Fake-Gegenstelle für Rückmeldungen (Node-HTTP-Server), Auftrag läuft durch, Rückmeldung kommt an |
| UI-E2E | Integration verbinden (einmaliger Dialog), Person zuordnen/bestätigen, Verlauf, Verknüpfung im Auftrag; iPhone hoch/quer; axe; Tour |

## 7. Risiken

| Risiko | Gegenmaßnahme |
|---|---|
| Fremder Workspace mit gleichem App-Secret | Mandant nach Signatur geprüft, `UNIQUE (kind, tenant)` |
| Externe Person verbraucht fremden Zugang | Zuordnung braucht Zustimmung der Person **und** der Eigentümerin; Opt-in nur auf Kosten der Eigentümerin |
| Prompt-Injection über Issue/Slack/Jira | nur Datenblock, Titel fest, Antworten im `USER_MSG`-Block, Längenlimits |
| Doppelte Kommentare (Retry nach verlorener Antwort) | Outbox-UNIQUE + Marker/ID-Suche vor jedem Wiederholungsversuch |
| Schleifen (eigene Kommentare lösen Aufträge aus) | Marker, Bot-Kennzeichen, nur Menschen-`prompt` |
| Externe Dienste hängen ⇒ Scheduler steht | eigener Tokio-Task, Timeouts, Lease, Backoff |
| SSRF über Jira-Site-URL | nur `https://<name>.atlassian.net` (bzw. mit `ALLOW_PRIVATE_GIT_HOSTS` frei für Tests), SSRF-Client, keine Redirects |
| Feldnamen falsch | Belege aus offiziellen SDKs/Schemas (`linear/linear` GraphQL-Schema, `slackapi/*`, `github/rest-api-description`), Jira nur Sekundärquelle ⇒ markiert |
| Platz auf der Platte | `CARGO_INCREMENTAL=0`, alte Test-Binaries löschen, `frontend/target` nach Tests löschen |
| Hängenbleiben von Workspaces durch „Besetzen“ eines Mandanten | Restrisiko (selbst gehostet, registrierte Nutzer); Eigentümerin der Integration ist sichtbar, Admin kann löschen – dokumentiert |
