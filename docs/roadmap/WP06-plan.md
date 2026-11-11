# WP06 – Plan-Modus, Steering & Inline-Kommentare: Plan

Stand: 2026-11-10, Branch `mvp` (Basis `45e1f30`, WP05 abgeschlossen). Ein Agent für Plan, Umsetzung, Test und Review.
Maßstab: `work-packages.md` (Gemeinsame Regeln, WP06), Auftrag des Koordinators, Report P1.3/P1.4/P1.7.

## 0. Befund vor dem Plan (am Code und an den CLIs geprüft)

| Thema | Befund |
|---|---|
| Plan-Schritt (WP13) | Es gibt `tasks.plan` und einen Einzelschritt „Planung“ per direkter API oder Batch (`runner::defer_single`, `batch.rs`). Er läuft **ohne Freigabe**: Ergebnis → `tasks.plan` → Auftrag sofort weiter. Ein agentischer Plan-Lauf existiert nicht. |
| Sessions (WP01) | Keine Session-Verwaltung. `HOME` des Laufs wird nach jedem Lauf gelöscht (`runner/src/exec.rs`), damit ist jede CLI-Sitzung weg. WP01 hat `--no-session-persistence` bewusst weggelassen („Resume → WP06“). |
| Nachrichten an Aufträge | Nur `POST /tasks/{id}/answer` für `waiting_input` (hängt Text ungeprüft und unbegrenzt an `context`). Kein Steering. |
| Diff | `GET /tasks/{id}/diff` liefert Text (Basis `diff_base()..snapshot_after`, WP04). Frontend zeigt `<pre>`, keine Zeilennummern, keine Kommentare. |
| Rechte | Steuer-Aktionen auf Aufträgen (`cancel`, `retry`, `answer`, `update`) nur Ersteller (`own_task`, sonst 404). Gate-Entscheidungen: Ersteller oder Projekt-Eigentümer (`api/gate.rs::decidable`). Auftrag anlegen: Rolle ≥ Bearbeiter. Läufe nutzen immer die Zugänge des Erstellers. |
| Held (WP03) | `status='held'` + `tasks.gate` (`TaskGate{stage,…}`) ist der eine Mechanismus „wartet auf eine Entscheidung“: Postfach, Queue-Sektion „Freigabe nötig“, Zeilenaktion „Entscheiden“, OnEmpty-Sperre, Abbrechen verwirft. |

### CLI-Fähigkeiten (Belege)

| CLI (gepinnt) | Plan/read-only | Session setzen | Fortsetzen | Speicherort | Beleg |
|---|---|---|---|---|---|
| Claude Code | `--permission-mode plan` | `--session-id <uuid>` | `--resume <id>` (auch mit anderem `--permission-mode`) | `~/.claude/projects/<cwd>/<id>.jsonl` | **live** mit dem installierten 2.1.322 (gepinnt ist 2.1.319, nicht verfügbar): Plan-Modus + `--json-schema` liefert `structured_output`, Arbeitsbaum unverändert (einzige Schreibaktion: `~/.claude/plans/*.md`); Resume nach gelöschtem HOME mit nur zurückkopiertem `projects/` funktioniert, auch aus anderem Arbeitsverzeichnis; unbekannte ID → `result` `error_during_execution`, `num_turns:0`, stderr „No conversation found with session ID“. Aufnahmen: `tests/fixtures/cli/claude-2.1.322/`. Kosten der Prüfung ≈ 0,40 USD. |
| Codex 0.167.0 | `--sandbox read-only` (Werte `read-only|workspace-write|danger-full-access`) | nicht setzbar, `thread.started.thread_id` (aufgezeichnet in WP01) | `codex exec resume <SESSION_ID> -` („Prompt … If `-` is used, read from stdin“) | `~/.codex/sessions/…`, `~/.codex/*.sqlite` | Strings im musl-Binary (npm-Paket `@openai/codex@0.167.0-linux-x64`), **nicht ausgeführt**. Ob die `exec`-Optionen (`--json`, `--output-schema`) vor `resume` wirken und welche Dateien der Resume braucht: **unverifiziert**. |
| Gemini CLI 0.67.0 | `--approval-mode plan` („plan (read-only mode)“) | `--session-id <id>` | `--resume <uuid>` | `~/.gemini/tmp/<projekt>/chats`, `~/.gemini/projects.json` | Bundle-Quelltext (`gemini-*.js`, `chunk-*.js`), **nicht ausgeführt**. |
| opencode 1.19.5 | `--agent plan` („Plan mode. Disallows all edit tools“, erlaubt aber `.opencode/plans/*.md` im Arbeitsbaum) | nicht setzbar, `sessionID` in jedem Event | `--session <id>` | `~/.local/share/opencode/{storage,opencode*.db}` | Strings im Binary (`opencode-linux-x64@1.19.5`), **nicht ausgeführt**. |
| Mock | `QUEUE_PLAN_MODE=1` | `QUEUE_SESSION_ID` | `QUEUE_RESUME` | `~/.mock-sessions/<id>` | eigene Implementierung (Tests) |

Folge: Das CLI-Flag ist nur die erste Schicht. Die Garantie „nichts geändert“ muss der Runner liefern, unabhängig davon, ob ein Flag wirkt.

## 1. Ziele und Nicht-Ziele

Ziele (WP06):
1. **Plan-Modus** pro Projekt (Voreinstellung) und pro Auftrag: Der Agent erstellt nur einen Plan; Mensch genehmigt / ändert (selbst bearbeiten oder überarbeiten lassen) / verwirft; optionale automatische Genehmigung nach Zeit (Standard aus); Umsetzung mit dem genehmigten Plan als Kontext.
2. **Steering**: Nachrichten an Aufträge, die noch nicht fertig sind; in der Queue zurückziehbar bis zur Zustellung; Zustellung per Session-Resume, wo die CLI es kann, sonst als Fortsetzungslauf mit Übergabe; „Sofort zustellen“ unterbricht den laufenden Agenten.
3. **„Weiter mit …“** für beendete Aufträge: neuer Fortsetzungs-Auftrag, der die Sitzung des Ursprungs fortsetzt (sofern möglich) oder per Übergabe startet.
4. **Inline-Kommentare** im Diff (Datei, Zeile oder Zeilenbereich, alte/neue Seite), gesammelt als Entwurf, als **eine** Nachricht bzw. ein Fortsetzungs-Auftrag gesendet; gültig / verschoben / veraltet je nach aktuellem Diff.
5. Mehrmandanten: Rechte wie in den bestehenden APIs, Längenlimits, Nutzereingaben getrennt und markiert im Prompt.

Nicht-Ziele: Live-Einspeisen in einen laufenden Turn über stdin (`--input-format stream-json`; braucht bidirektionales Runner-Protokoll und ist nur für Claude belegbar), Plan-Pflicht ab Kosten-/Pfad-Schwellen (Report P1.4 „Pflicht ab …“; bleibt Projekt-/Auftragswahl), Anhänge (P1.7, WP09), Kommentare in gehaltenen (nicht committeten) Änderungen, Lizenz/ToS.

## 2. Architektur

### 2.1 Plan-Modus

```
claim → run_inner
  plan_mode && plan_state ≠ approved
     ├─ Projekt-Routing plan = sync|batch → WP13-Einzelschritt (kein Arbeitsbaum) ──┐
     └─ sonst: Plan-Lauf (Runner, read_only)                                        │
            Wegwerf-Arbeitsbaum t<plan-id> (prepare_task, gleicher Start/Carry)     │
            CLI-Plan-Flag + ro-Mount (Container) + Vorher/Nachher-Abgleich          │
            Arbeitsbaum wird danach immer gelöscht                                  │
  → tasks.plan + plan_meta, status=held, gate.stage=plan  ◄──────────────────────────┘
       Genehmigen [mit Bearbeitung] → plan_state=approved, queued → normaler Lauf, Prompt „Approved plan“
       Überarbeiten lassen (Notiz)  → plan_revision+1, queued → neuer Plan-Lauf mit altem Plan + Notiz
       Verwerfen                    → cancelled („Plan verworfen“), Aufräumen
       Auto-Genehmigung (Projekt, optional) im Scheduler-Tick, nie bei offenen Fragen oder Änderungsversuchen
```

- **Durchsetzung „ändert nichts“ (drei Schichten):**
  1. CLI-Flag je Provider (Tabelle §0).
  2. **Runner**: `ExecRequest.read_only`. Container: Arbeitsbaum `:ro` gemountet, kein Cache-Mount. Alle Engines: vor dem Start Momentaufnahme des kompletten Baums inklusive `.git` und ignorierter Dateien (Pfad, Typ, Größe, Modus, Inode, mtime, **ctime** – ctime kann ein unprivilegierter Prozess nicht zurücksetzen; Symlink-Ziel), nach dem Ende erneut; Unterschiede kommen als `Exit.changed` (max. 50 Pfade + Anzahl) zurück. Unvollständig (> 200 000 Einträge) ⇒ gilt als geändert.
  3. **Wegwerf-Arbeitsbaum**: Der Plan-Lauf läuft in einem eigenen Arbeitsbaum (abgeleitete ID, gleicher Startstand wie der Auftrag inklusive `carry_from`/PR-Start), der danach immer gelöscht wird. Selbst wenn Flag und Mount versagen (Engine `none`), erreicht keine Änderung den Arbeitsbaum, den Branch oder das Hauptrepo des Auftrags.
  - Versuchte Änderungen ⇒ Ereignis + Warnung im Plan, keine Auto-Genehmigung.
- Der Plan-Lauf bekommt keine Setup-Phase und keine Secrets (wie jeder Agentenlauf), die Netzregel des Agenten (WP02) gilt.
- Ergebnis: Claude/Codex per Schema `PLAN_SCHEMA {plan, questions}` (structured output), Gemini/opencode/Mock aus dem Schlusstext. Plan ≤ 20 000 Zeichen, Secrets geschwärzt.
- Gate-Integration: `GateStage::Plan` mit Titel/Hilfe/Beschriftungen in `queued-api`. Allgemeines `gate/approve`/`gate/reject` delegiert für diese Stufe an die Plan-Logik. Abbrechen eines gehaltenen Plans wie bisher.
- WP13-Einzelschritt: Ist der Plan-Modus an und das Routing nutzt `sync|batch`, wird das Ergebnis nicht mehr direkt verwendet, sondern zur Freigabe gehalten (`batch::complete`).

### 2.2 Steering

Nachrichten (`task_messages`) mit Zustand `queued → delivered | withdrawn | dropped`. **Zustellung = atomares `UPDATE … WHERE state='queued' RETURNING`** unmittelbar vor dem Lauf, der sie liest; Zurückziehen = atomares `UPDATE … WHERE state='queued'` (409 sonst). Damit gibt es kein Rennen zwischen Zurückziehen und Zustellen.

Zustellpunkte:
1. **Start eines frischen Laufs** (Plan- oder Umsetzungslauf, auch nach Wiederholung): alle wartenden Nachrichten werden zugestellt; ein frischer Lauf sieht außerdem alle früher zugestellten Nachrichten dieses Auftrags (keine geht bei Re-Queue verloren).
2. **Ende eines Agentenlaufs** (normales Ende oder Unterbrechung durch „Sofort zustellen“): liegen Nachrichten vor, folgt eine **Steuerrunde** vor Gate/Commit: Resume der Sitzung (`--resume`/`exec resume`/`--session`) mit nur den neuen Nachrichten; ohne Resume-Fähigkeit bzw. ohne bekannte Sitzung ein frischer Lauf im selben Arbeitsbaum mit vollständigem Prompt + Übergabe („Stand bisher“: Zusammenfassung/Übergabe des Laufs, geänderte Dateien). Höchstens 10 Steuerrunden pro Auftrag, Limits (Zeit/Tokens/Kosten) gelten kumuliert.
3. **Auftrag `done`** mit noch wartenden Nachrichten (kamen nach der Arbeitsphase, z. B. während Verify/Review/Übernahme): automatisch ein Fortsetzungs-Auftrag („Weiter mit …“), Nachrichten dort zugestellt.
4. `cancelled` ⇒ `dropped`. `failed`/`blocked` ⇒ bleiben wartend und zurückziehbar; zugestellt bei „Wiederholen“ oder „Weiter mit …“ (kein automatischer bezahlter Folgelauf nach einem Limit-Stopp, WP01-R4).
- `waiting_input` + Nachricht = Antwort: Auftrag wird wieder eingereiht.
- Resume fehlgeschlagen (Sitzung unbekannt, Zeichen: kein Verbrauch und kein Tool-Aufruf, bzw. Claude „No conversation found“) ⇒ einmal frischer Lauf mit Übergabe.
- Jeder Resume-Prompt enthält zusätzlich eine kurze Erinnerung an die Aufgabe (falls eine unverifizierte CLI still eine neue Sitzung beginnt, arbeitet der Agent trotzdem an der richtigen Aufgabe) und den Ergebnis-Contract.

**Sitzungsspeicher (Runner):** `ExecRequest.session {scope, key, seed, paths, exclude}`. Vor dem Lauf kopiert der Runner `<data>/sessions/<scope>/<key>` (oder, wenn leer, `seed`) ins frische HOME – vor den `home_files` der App, die damit gewinnen; nach dem Lauf (immer, auch bei Abbruch) die freigegebenen Pfade zurück. Nur reguläre Dateien und Verzeichnisse, keine Symlinks, Ausschlussliste (Zugangsdateien, Konfiguration, Claude-`memory`), Obergrenzen 64 MiB / 20 000 Dateien / Tiefe 16, Schreiben über Temp-Verzeichnis + `rename`. Löschen mit dem Projekt-Workspace und nach 7 Tagen Inaktivität (Runner-Sweep). Pfade je CLI:
- Claude `.claude/projects` (ohne `memory`), Codex `.codex/sessions` + `.codex/*.sqlite*`, Gemini `.gemini/tmp` + `.gemini/projects.json`, opencode `.local/share/opencode/storage` + `opencode*.db*`, Mock `.mock-sessions`.
- Sitzungs-ID: Claude/Gemini/Mock setzt die App (`--session-id`), Codex/opencode melden sie im Stream (`AgentEvent::Session`). Gespeichert in `tasks.agent_session {provider, id}`.

### 2.3 „Weiter mit …“

`POST /api/tasks/{id}/continue {message, plan_mode?}` für beendete Aufträge (`done|failed|blocked|cancelled`), Rolle ≥ Bearbeiter. Neuer Auftrag `kind='continue'`, `parent_id` = Ursprung (Übergabe im Prompt), gleicher Anbieter/Modell/Zugang, `carry_from` = Ursprung, wenn dessen Änderungen nicht im Hauptstand sind, `pr_id` = Ursprung (PR-Modus: weiter auf dem PR-Branch), `session_from` = Ursprung **nur wenn derselbe Nutzer** (fremde Transkripte gehen nie an einen anderen Nutzer). Wartende Nachrichten des Ursprungs werden mitgenommen. Erster Lauf: Resume mit geseedeter Sitzung, Fallback frischer Lauf mit Übergabe.

### 2.4 Inline-Kommentare

`diff_comments` (Entwurf pro Nutzer). Anker = Text der kommentierten Zeilen beim Anlegen + `diff_rev` (`snapshot_after`). Beim Lesen und Senden wird gegen den aktuellen Diff verortet:
- gleicher Stand oder gleiche Zeilen an gleicher Stelle ⇒ **gültig**;
- gleiche Zeilenfolge eindeutig an anderer Stelle in derselben Datei/Seite ⇒ **verschoben** (neue Zeilennummern);
- sonst ⇒ **veraltet** (wird angezeigt, beim Senden mit Hinweis und Originalzeilen mitgeschickt).
Senden bündelt alle eigenen Entwürfe des Auftrags zu **einer** Nachricht: aktiver Auftrag ⇒ Steering-Nachricht (Quelle `comments`), beendeter Auftrag ⇒ ein „Weiter mit …“-Auftrag. Diff-Parser und Verortung liegen in `queued-api::diff` (Backend und Frontend identisch).

### 2.5 Prompt-Trennung

Nachrichten, Kommentare, Überarbeitungsnotiz und bearbeiteter Plan sind Nutzereingaben, aber sie kommen in eigene Blöcke `<<<USER_<ART>_<nonce>` … `USER_<ART>_<nonce>>>>` mit zufälliger Grenze (wie WP05), Grenzpräfixe im Inhalt entschärft, Secrets geschwärzt, Größen begrenzt; davor steht, was der Block ist und dass er die Regeln der Queue (Contract, keine Secrets, kein Push) nicht ändert. Zitierte Code-Zeilen in Kommentaren stammen aus dem Repository (Agenten-Ausgabe) und stehen im selben Datenblock.

### 2.6 Rechte

| Aktion | Wer |
|---|---|
| Plan genehmigen/ändern/verwerfen | Ersteller (mit Rolle ≥ Bearbeiter) oder Projekt-Eigentümer (wie Gate) |
| Nachricht senden / sofort zustellen | Ersteller (Rolle ≥ Bearbeiter) oder Eigentümer; Betrachter 403, Fremde 404 |
| Nachricht zurückziehen | Autor der Nachricht oder Eigentümer |
| Weiter mit … | Rolle ≥ Bearbeiter (neuer Auftrag gehört dem Aufrufer, läuft mit seinen Zugängen) |
| Kommentar anlegen/ändern/löschen/senden | Rolle ≥ Bearbeiter, nur eigene Entwürfe; Senden an einen laufenden Auftrag zusätzlich Steuerrecht |
| Lesen (Nachrichten, gesendete Kommentare, Plan) | Rolle ≥ Betrachter |
Ein Ersteller, der aus dem Projekt entfernt oder zum Betrachter wurde, steuert nicht mehr.

Längen: Nachricht ≤ 4 000 Zeichen, Kommentar ≤ 2 000, Überarbeitungsnotiz ≤ 4 000, bearbeiteter Plan ≤ 20 000, ≤ 20 wartende Nachrichten pro Auftrag, ≤ 100 Kommentare pro Nutzer und Auftrag, Bereich ≤ 200 Zeilen.

## 3. Datenmodell (Migration `0009_plan_steering.sql`, nur neu)

```sql
projects + plan_mode BOOLEAN NOT NULL DEFAULT FALSE
         + plan_auto_approve_mins INT NULL CHECK (5..10080)
tasks    + plan_mode BOOLEAN NOT NULL DEFAULT FALSE
         + plan_state TEXT NULL CHECK (planning|ready|approved|rejected)
         + plan_revision INT NOT NULL DEFAULT 0, plan_feedback TEXT, plan_meta JSONB
         + agent_session JSONB, session_from UUID → tasks ON DELETE SET NULL
         + steer_rounds INT NOT NULL DEFAULT 0
task_messages (id, task_id → CASCADE, author_id → users SET NULL, body CHECK 1..4000,
               source message|comments, state queued|delivered|withdrawn|dropped, urgent,
               delivery prompt|resume|handoff|continuation, continued_as → tasks SET NULL,
               created_at, delivered_at, withdrawn_at)   INDEX (task_id, state, created_at)
diff_comments (id, task_id → CASCADE, author_id → users CASCADE, path, side new|old,
               line_start ≥1, line_end ≥ line_start (≤ 200 Zeilen), body 1..2000,
               anchor TEXT (≤ 4 000), diff_rev, state draft|sent, message_id → task_messages,
               sent_task → tasks, created_at, updated_at, sent_at)   INDEX (task_id, author_id, state)
```
`tasks.kind` hat keinen CHECK; neuer Wert `continue`.

## 4. API (neu)

- `POST /api/tasks/{id}/plan/approve {plan?}`, `…/plan/revise {note}`, `…/plan/reject`
- `GET /api/tasks/{id}/messages`, `POST …/messages {body, now?}`, `DELETE …/messages/{mid}`
- `POST /api/tasks/{id}/continue {message, plan_mode?}`
- `GET /api/tasks/{id}/comments`, `POST …/comments`, `PATCH|DELETE …/comments/{cid}`, `POST …/comments/send {message?}`
- Projekt: `plan_mode`, `plan_auto_approve_mins` in `PATCH /api/projects/{id}` (Eigentümer); Auftrag: `plan_mode` in `CreateTask` (Standard = Projekt).
- `/api/meta`: `provider_capabilities.{plan,resume}` + Belegstufe.

## 5. UI (WP00-Designsystem)

- Composer → Erweitert: „Erst einen Plan zeigen“ (Voreinstellung aus dem Projekt) mit kurzer Hilfe.
- Projekt → Einstellungen: „Plan-Modus“ (Schalter) + „Automatisch genehmigen nach“ (Aus / 15 min / 1 h / 4 h / 24 h), nur Eigentümer.
- Drawer, Status „Freigabe nötig“ mit Stufe Plan: **Plan-Ansicht** (Plan als lesbarer Text, offene Fragen, Warnung bei Änderungsversuch, ggf. „wird automatisch genehmigt am …“), Buttons „Plan genehmigen“ (primär), „Ändern“ (Dialog mit Reitern „Selbst bearbeiten“ / „Überarbeiten lassen“), „Verwerfen“ (Bestätigung).
- Drawer, aktive Aufträge: **Nachrichtenfeld** „Nachricht an den Agenten“ + „Senden“; bei laufendem Agenten zusätzlich „Sofort zustellen (unterbricht kurz)“. Liste der Nachrichten mit Zustand („Wartet auf Zustellung“ + „Zurückziehen“, „Zugestellt in die laufende Sitzung/beim Start/als neuer Lauf/als Fortsetzung“, „Zurückgezogen“).
- Drawer, beendete Aufträge: **„Weiter mit …“** (Textfeld + Button, öffnet den neuen Auftrag).
- Diff: Knopf „Kommentieren“ schaltet in den Kommentiermodus (Zeilen werden antippbar, ≥ 44 px auf Touch, sichtbarer Fokus, `aria-pressed`); erste Zeile antippen = Auswahl, zweite Zeile derselben Datei/Seite = Bereich; Leiste unten (Safe-Area) „Zeilen 12–14 kommentieren“; Kommentare erscheinen unter ihren Zeilen mit Kennzeichen „verschoben“/„veraltet“; Sammelleiste „3 Kommentare senden“.
- iPhone: Safe Areas (`--sab` für die Leiste über der Tab-Bar), 16-px-Eingaben, keine horizontale Seiten-Scrollbar (Diff scrollt in sich), axe grün hell/dunkel.

## 6. Tests

| Ebene | Inhalt |
|---|---|
| Unit (api) | Diff-Parser (Zeilennummern alt/neu, neue/gelöschte Dateien, `\ No newline`), Anker-Auszug, Verortung gültig/verschoben/veraltet/mehrdeutig, Validierung |
| Unit (app) | Invocations je CLI für Plan/Resume/Session-ID, Parser `AgentEvent::Session` (Codex `thread.started`, opencode `sessionID`), Fixture-Tests der Claude-2.1.322-Aufnahmen (Plan-Schema-Ergebnis, Resume, „not found“), Prompt-Blöcke (Nonce, Entschärfung, Limits), Plan-Extraktion |
| Unit (runner) | `read_only`: Änderungserkennung (Datei, `.git`, ignorierte Datei, ctime-Fälschung per `touch -d`), `:ro`-Mount in den Container-Argumenten; Sitzungsspeicher: Restore/Save, Seed, Ausschlüsse, Symlink-Ablehnung, Größenlimit, TTL-Sweep |
| Integration (`tests/plan_steering.rs`) | Plan-Lauf ändert nichts (Mock versucht zu schreiben, Hauptrepo/Branch unverändert, Warnung), Genehmigen → Umsetzung mit Plan im Prompt, bearbeiteter Plan, Überarbeiten, Verwerfen, Auto-Genehmigung (an/aus, nie bei Fragen), Plan per Batch-Einzelschritt zur Freigabe, Nachricht zurückziehen (vor/nach Zustellung), Zustellung beim Start, am Laufende per Resume, „Sofort zustellen“ unterbricht und setzt fort, Resume-Fallback, Steuerrunden-Grenze, Nachricht nach Arbeitsphase ⇒ Fortsetzung, `cancelled` ⇒ verworfen, „Weiter mit …“ (Sitzung geseedet, fremder Nutzer ohne Sitzung, `carry_from`), Inline-Kommentare (Validierung, Senden als eine Nachricht/Fortsetzung, veraltet/verschoben), Rechte (Fremde 404, Betrachter 403, Bearbeiter ≠ Ersteller 403, Eigentümer ok, entfernter Ersteller), Längenlimits |
| System-E2E | über Prozessgrenze: Plan genehmigen und umsetzen, Nachricht an laufenden Auftrag (Resume), Weiter mit |
| UI-E2E | Plan-Ansicht Genehmigen/Ändern/Verwerfen, Nachricht senden/zurückziehen, Weiter mit, Kommentieren im Diff inkl. veraltet; iPhone hoch/quer per Touch (Tap), Safe Areas, Touch-Ziele, axe; Tour um die neuen Ansichten |

## 7. Risiken

| Risiko | Gegenmaßnahme |
|---|---|
| CLI ignoriert das Plan-Flag | Wegwerf-Arbeitsbaum + ro-Mount + Abgleich; Flag ist nur Komfort |
| Engine `none`: Agent kann außerhalb des Arbeitsbaums schreiben | Bestand (WP02/WP04: nur Entwicklung); Abgleich deckt den Arbeitsbaum ab, dokumentiert |
| Unverifizierte Resume-Aufrufe (Codex/Gemini/opencode) | Fallback bei fehlgeschlagenem Resume, Aufgabenerinnerung im Resume-Prompt, im UI/README als unverifiziert markiert |
| Sitzungsdateien als Einschleusweg (Konfiguration im HOME) | Allowlist je CLI, Ausschluss von Konfig/Zugangsdateien und Claude-Memory, Restore vor `home_files`, keine Symlinks, Größenlimits, nie an einen anderen Nutzer |
| Prompt-Injection über Kommentare (zitierter Code) | Datenblock mit Nonce, Regeln davor |
| Endlose Steuerrunden / Kosten | ≤ 10 Runden, kumulierte Limits, Prüfung vor jeder Runde |
| Rennen Zurückziehen ↔ Zustellen | atomare Zustandswechsel in der DB |
| Auto-Genehmigung genehmigt Unsinn | Standard aus; nie bei Fragen/Änderungsversuch; Ereignis im Protokoll |
| Plattenplatz (Wegwerf-Arbeitsbaum, Sitzungen) | Plan-Arbeitsbaum sofort weg; Sitzungen ≤ 64 MiB, TTL 7 Tage |
| Geteilte Projekte: fremdes Transkript | `session_from` nur bei gleichem Nutzer |
