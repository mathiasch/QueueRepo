# WP04 – Branch/Worktree pro Task, Parallelität im Repo, Merge-Queue: Plan

Stand: 2026-11-09, Branch `mvp` (Basis `87b1ba6`, WP03 abgeschlossen). Ein Agent für Plan, Umsetzung, Test und Review.
Verbindlich: `work-packages.md` (Gemeinsame Regeln, WP04), Report P1.1/P0.5, offene Funde WP03-N2 (Teil-Commits ungeprüft auf dem Branch) und WP03-N6 (gehaltene Secrets für andere Aufträge lesbar).

## 1. Ausgangslage (am Code geprüft)

- **Ein Workspace pro Projekt** (`<data>/workspaces/<projekt-uuid>`), der komplett (inkl. `.git`) beschreibbar in jeden Container gemountet wird (`exec.rs::build_command`, `-v <ws>:/workspace`). Setup, Agent, Verify und Review aller Aufträge eines Projekts laufen nacheinander in diesem einen Verzeichnis.
- **Scheduler** (`worker.rs::claim_next`): globale Parallelität pro Nutzer (`user_settings.parallelism`), dazu hart „ein laufender Auftrag pro Projekt“ per `NOT EXISTS (… status='running')` in einer `FOR UPDATE SKIP LOCKED`-Abfrage. Das ist nur deshalb race-frei, weil genau ein Scheduler-Loop sequenziell claimt; zwei gleichzeitige Claimer (zweite App-Instanz, Test) sehen im Snapshot beide „nichts läuft“ und starten zwei Aufträge im selben Projekt.
- **Commits landen direkt auf dem Ziel-Branch** (`git_branch`), auch von fehlgeschlagenen Läufen (Verify/Review abgelehnt → Commit bleibt, Fix-Auftrag arbeitet obendrauf). Teil-Commits unterbrochener Läufe ebenso (WP03-N2).
- **WP03-N6:** gehaltene Änderungen (`refs/queue/held/<task>`) und Quarantäne liegen im gemeinsamen `.git`; der Agent des nächsten Auftrags liest sie mit `git show`.
- **Neuer Fund (schwer, WP03-Lücke): Baselines sind vom Agenten fälschbar.** `refs/queue/baseline/{setup,agent-config}` liegen im gemounteten `.git`. Ein Agent kann `git update-ref refs/queue/baseline/setup HEAD` ausführen, nachdem er Setup-Dateien geändert hat; der nächste Setup-Lauf mit Secrets prüft dann gegen die gefälschte Baseline und läuft ohne Freigabe. Gleiches gilt für `agent-config`. Mit WP04 werden Baselines nur noch im nie gemounteten Hauptrepo geführt.
- **Neuer Fund (mittel, Bestand): Symlinks im Arbeitsbaum.** `Repo::read_file` (Dateien-API, auch für Betrachter) folgt Symlinks → ein Agent legt `x -> /etc/passwd` an, jedes Projektmitglied liest Host-Dateien des Runner-Users. `extract_zip` (ZIP-Import) schreibt durch symlinkte Verzeichnisse hindurch → Schreiben außerhalb des Workspace. Beides wird mitgehärtet, weil WP04 fremde Bäume in den Haupt-Arbeitsbaum übernimmt.
- `revert` verweigert, solange irgendein Auftrag im Projekt läuft (geteilter Arbeitsbaum).

## 2. Ziele / Nicht-Ziele

**Ziele**
1. Jeder Auftrag arbeitet in einem eigenen Arbeitsverzeichnis auf Branch `queue/<12 hex der Task-ID>`; nur dieses Verzeichnis wird in seine Container gemountet.
2. Parallelität pro Projekt (`projects.parallelism`, Standard 1, 1–8), race-frei auch bei gleichzeitigen Claimern; Nutzer-Parallelität, Provider-Pausen/Budgets gelten weiter.
3. Serielle Merge-Queue pro Projekt: Rebase auf den Ziel-Branch → Gate-Nachprüfung → Verify erneut (wenn sich das Ziel bewegt hat) → Fast-Forward. Konflikt → Fix-Auftrag mit Konfliktkontext. Branch und Arbeitsverzeichnis werden aufgeräumt.
4. Diff/Revert/Log korrekt: Diff gegen die Merge-Base, Revert nur übernommener Aufträge, Log des Ziel-Branches.
5. Gate (WP03) bleibt vollständig wirksam und wird nicht umgangen; WP03-N2, WP03-N6 und die Baseline-Fälschung werden geschlossen.
6. UI: Parallelität pro Projekt, Status „Wird übernommen“ mit Platz in der Merge-Queue, Branch pro Auftrag, Konflikte verständlich erklärt; WP00-Designsystem, axe, Safe-Areas.

**Nicht-Ziele:** PR-Workflow und Push von Task-Branches ins Remote (WP05). Dateibereichs-Tags/„disjunkte Bereiche“ (Report P1.1, optional; Konflikte werden stattdessen per Fix-Auftrag gelöst). Best-of-N (WP12).

## 3. Architektur

### 3.1 Warum kein `git worktree` (harte Begründung)
`git worktree add` legt im Arbeitsbaum eine `.git`-**Datei** an (`gitdir: <haupt>/.git/worktrees/<n>`), Objekte, Refs, Hooks und Config teilt er über `commondir` mit dem Hauptrepo. Für einen Agenten im Container gibt es nur zwei Möglichkeiten:
- nur den Worktree mounten → `gitdir` zeigt ins Leere, Git funktioniert im Container nicht (Agent-CLIs nutzen `git status/diff/commit`);
- das gemeinsame `.git` mitmounten → der Agent schreibt in die gemeinsame Objektdatenbank, in Refs anderer Aufträge, in `refs/queue/held/*` (Secrets anderer Aufträge), in Baselines, Hooks und Config. Genau das verbietet der Auftrag („ein Agent darf nur seinen eigenen Worktree sehen“).
Auch `clone --shared`/Alternates (Objektdatenbank per Verweis) und `clone --local` mit Hardlinks scheiden aus: Alternates zeigen auf einen nicht gemounteten Pfad bzw. werden von `harden()` entfernt; Hardlinks teilen Inodes, der Agent (gleiche UID) könnte per `chmod` + Schreiben Objekte des Hauptrepos verändern.

**Umsetzung daher als isolierter Arbeitsbaum pro Auftrag:** ein eigenständiges Repository mit eigenem echtem `.git`-Verzeichnis, befüllt per `git fetch` (Upload-Pack, nur Objekte, die vom Startpunkt erreichbar sind) aus dem Hauptrepo. Fachlich ist das ein Worktree (eigener Arbeitsbaum + eigener Branch `queue/<id>`), technisch ohne geteilten Zustand. Kosten: Objekte der Historie liegen pro aktivem Auftrag einmal auf der Platte (Risiko 6.1).

### 3.2 Verzeichnisse und Namen
- Hauptrepo `workspaces/<projekt-uuid>` (wie bisher): Ziel-Branch ausgecheckt, Quelle für Dateien/ZIP/Verlauf/Import/Push, Ort der Task-Branches `refs/heads/queue/<id>`, der Baselines und der Quarantäne. **Wird nie mehr in einen Container gemountet.**
- Arbeitsbaum eines Auftrags `workspaces/t<32 hex task-uuid>` (Workspace-ID erfüllt `valid_name`), eigenes Repo, HEAD = `refs/heads/queue/<12 hex>`.
- Abhängigkeits-Cache bleibt pro Projekt: `CacheSpec.scope = <projekt-uuid>`; Setups mit Cache laufen pro Scope seriell (Mutex im Runner), weil parallele Setups sonst gleichzeitig in denselben Cache schreiben bzw. `finish()` den Cache eines laufenden Setups löscht.

### 3.3 Runner-Endpunkte (neu)
| Endpunkt | Wirkung |
|---|---|
| `POST /v1/workspaces/{haupt}/tasks/{tws}` `TaskWorkspaceRequest{branch, merge_base?, carry?, allow}` | legt den Arbeitsbaum an oder setzt einen bestehenden fort. Neu: Start = Task-Branch im Hauptrepo (Wiederaufnahme nach Löschen), sonst `carry` (Änderungen eines anderen Auftrags per `merge-tree` auf den aktuellen Ziel-Stand, Konflikte als Marker), sonst Ziel-HEAD. Bestehend: Reste als WIP committen/Quarantäne (wie `begin`). Antwort: `head`, `merge_base`, `carried`, `conflicts`, `warnings`. Legacy-`refs/queue/held/<task>` im Hauptrepo werden in den Arbeitsbaum übernommen und im Hauptrepo gelöscht + geprunt. |
| `POST …/tasks/{tws}/sync` `{branch, sha}` | holt genau `sha` aus dem Arbeitsbaum (`fetch` mit `transfer.fsckObjects`) nach `refs/heads/<branch>` im Hauptrepo. |
| `POST …/tasks/{tws}/merge` `MergePrepare{branch, base, tip, message, author, protected, test_guard, allow}` | im Hauptrepo: `merge-tree --write-tree --merge-base=base HEAD tip` → ein Squash-Commit `C` auf dem Ziel-HEAD `T` (kein Arbeitsbaum, keine Hooks). Antwort: `commit`, `target`, `moved` (`T≠base`), `conflicts`, `markers` (Konfliktmarker in hinzugefügten Zeilen), `new_findings` (Gate-Nachprüfung, s. 3.5). Bei `moved` wird `C` im Arbeitsbaum ausgecheckt (für Verify). |
| `POST …/tasks/{tws}/merge/finish` `{commit, target, branch}` | Fast-Forward des Ziel-Branches im Hauptrepo auf `C`, nur wenn HEAD noch `T` ist (sonst `moved=true`, App wiederholt ab Rebase). Löscht danach den Task-Branch. |
| `DELETE …/tasks/{tws}?branch=` | löscht Arbeitsbaum und Task-Branch (Aufräumen). |
| `BaselineCheck.task` | Baseline-Prüfung im Hauptrepo gegen den HEAD des Arbeitsbaums (vorher per Fetch geholt). |

Alle bestehenden Endpunkte (begin/commit/inspect/hold/release/held/queue-file/exec) arbeiten unverändert auf der Workspace-ID des Arbeitsbaums.

### 3.4 Ablauf eines Auftrags (App)
1. Claim (3.6). 2. `init` + `begin` am Hauptrepo (Pull, Alt-Reste) → `tasks/{tws}` anlegen/fortsetzen → `merge_base`, `branch`, `snapshot_before` (Gate-Basis) speichern. 3. Setup-/Config-Baselines (im Hauptrepo, gegen den Arbeitsbaum-HEAD) → Setup → Agent → Gate (`inspect`/`hold`/`commit` im Arbeitsbaum) → `sync` → Verify → Review – alles im Arbeitsbaum. 4. Statt Push + „Erledigt“: Status **`merging`** (`merge_state=queued`), Ergebnis (Zusammenfassung, Folgeaufträge, Review) in `gate_pending`. 5. Merge-Worker (3.6) pro Projekt seriell: `merge` → Konflikt/Marker → Auftrag `failed` + Fix-Auftrag mit `carry_from`; neue Gate-Funde → `blocked`/`gate`; `moved` + Verify-Befehl → Verify im Arbeitsbaum auf `C` (fehlgeschlagen → `failed` + Fix mit Kontext); `finish` → `snapshot_before=merge_base=T`, `snapshot_after=C` → gegateter Auto-Push (WP03, vom Hauptrepo, exakt `C`) → `done` + Folgeaufträge → Arbeitsbaum/Branch löschen (auch die von Vorgängern, deren Änderungen `C` enthält).
- **Fix-Aufträge** übernehmen die Änderungen des fehlgeschlagenen Auftrags (`carry_from`): ihr Arbeitsbaum startet auf dem aktuellen Ziel-Stand plus einem Commit „Übernommen aus …“ (bei Konflikt mit Markern). Gate-Basis (`snapshot_before`) ist dieser Commit (die übernommenen Änderungen sind bereits gegatet), Diff-/Merge-Basis (`merge_base`) der Ziel-Stand davor.
- **Unterbrochene Läufe**: Teil-Commits bleiben im Arbeitsbaum/Task-Branch, nie auf dem Ziel-Branch (schließt WP03-N2).
- **Gehaltene Aufträge**: `hold`/`release`/`discard` im eigenen Arbeitsbaum; andere Agenten sehen ihn nicht (schließt WP03-N6). Verwerfen löscht den ganzen Arbeitsbaum.

### 3.5 Gate-Nachprüfung vor dem Merge („Verify erneut, Gate wo nötig“)
- Inhalt: Ein sauberer 3-Wege-Merge überträgt die Hunks des Auftrags unverändert; Secrets/geschützte Pfade/Test-Schwächungen des Auftrags-Deltas sind daher schon geprüft (oder freigegeben). Der Runner prüft das trotzdem fail-closed nach: Secret-Scan von `T..C` (Allowlist des Projekts) und `change_findings(T..C)` gegen `change_findings(base..tip)`; alles, was nur in `T..C` auftaucht, stoppt den Merge (`blocked`/`gate`, verständliche Meldung). Konfliktmarker in hinzugefügten Zeilen stoppen den Merge (Fix-Auftrag).
- Verhalten: Verify läuft erneut, wenn `T≠base` und ein Verify-Befehl gesetzt ist; sonst ist `tree(C)=tree(tip)` exakt der bereits verifizierte Stand.
- Setup-Baselines liegen im Hauptrepo; `C` ändert sie nicht, die nächste Setup-Prüfung vergleicht wie bisher.
- Push-Schutz (WP03) unverändert: Auto-Push von `C` auf den Default-Branch braucht Freigabe (Stufe `push`).

### 3.6 Scheduler
- Agent-Claim pro Kandidat in einer Transaktion: Kandidat wählen (ohne Sperre) → `SELECT … FROM projects WHERE id=$p FOR UPDATE SKIP LOCKED` (gesperrt ⇒ Projekt überspringen) → in einer **neuen Anweisung** (READ COMMITTED ⇒ frischer Snapshot nach Erhalt der Sperre) `UPDATE tasks … WHERE id = (SELECT … FOR UPDATE SKIP LOCKED) AND (SELECT count(*) … running) < parallelism`. Wer die Projektzeile hält, ist der einzige Claimer des Projekts bis zum Commit; der nächste sieht dessen Claim. Projekt-vor-Task-Sperrreihenfolge (wie `DELETE projects … CASCADE`) vermeidet Deadlocks.
- Merge-Claim analog: höchstens ein `merge_state='merging'` pro Projekt, ältester `queued` zuerst (`merge_queued_at`). Merge-Läufe belegen keinen Agenten-Slot des Nutzers (kein Provider), sind aber pro Projekt seriell.
- `recover`: `merging` → zurück nach `queued` in der Merge-Queue.

## 4. Datenmodell (Migration `0007_branches.sql`)
```sql
ALTER TABLE projects ADD COLUMN parallelism INT NOT NULL DEFAULT 1 CHECK (parallelism BETWEEN 1 AND 8);
ALTER TABLE tasks ADD COLUMN branch TEXT,
  ADD COLUMN merge_base TEXT,
  ADD COLUMN merge_state TEXT CHECK (merge_state IN ('queued','merging','merged','conflict','failed')),
  ADD COLUMN merge_queued_at TIMESTAMPTZ,
  ADD COLUMN carry_from UUID REFERENCES tasks(id) ON DELETE SET NULL,
  ADD COLUMN workspace_cleaned BOOLEAN NOT NULL DEFAULT FALSE;
CREATE UNIQUE INDEX tasks_branch_idx ON tasks (project_id, branch) WHERE branch IS NOT NULL;
CREATE INDEX tasks_merge_idx ON tasks (project_id, merge_queued_at) WHERE status = 'merging';
```
Neuer Status `merging` („Wird übernommen“). Alt-Aufträge (`branch IS NULL`) behalten Diff/Revert über `snapshot_before..snapshot_after`.

## 5. UI
- Projekt → Einstellungen: „Gleichzeitige Aufträge“ (1–8, Hilfe: mehr = schneller, aber mehr Konflikte und Last); Eigentümer.
- Status `merging`: „Wird übernommen“, Erklärung „Fertig und geprüft – wird gerade in den Hauptstand übernommen“, Platz in der Warteschlange.
- Drawer: Bereich „Branch“ (Name, Basis, Übernahme-Status, bei Konflikt betroffene Dateien und Link zum Korrektur-Auftrag).
- Queue: Sektion „Wird übernommen“. Projekt → Verlauf: „Übernahme-Warteschlange“.

## 6. Risiken / Grenzen
1. Plattenplatz: pro aktivem Auftrag eine Kopie der Objekte (kein Hardlink, s. 3.1); Aufräumen sofort bei Endzustand, fehlgeschlagene Branches nach 7 Tagen.
2. Squash: ein Commit pro Auftrag auf dem Ziel-Branch (wie bisher), die Zwischen-Commits des Branches gehen beim Übernehmen auf.
3. Mehr Parallelität = mehr Konflikte; Fix-Aufträge kosten Modellzeit.
4. Engine `none` isoliert nichts (Prozess auf dem Host) – unverändert gekennzeichnet.
5. Upload-Pack läuft beim `sync` im vom Agenten kontrollierten Repo (nach `harden()`, mit `HARDEN`-Overrides, `transfer.fsckObjects`).
