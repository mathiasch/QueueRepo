# WP05 – PR-Workflow & Feedback-Loop: Umsetzung

Grundlage: `WP05-plan.md`. Branch `mvp`, nicht committet. Testbericht: `WP05-test.md`, Urteil: `WP05-review.md`.

## 1. Was gebaut wurde

| Baustein | Dateien | Kurz |
|---|---|---|
| Protokoll | `backend/crates/common/src/lib.rs` | `PushRequest.{scan_base, expect}`, `PushResult.moved`, `TaskWorkspaceRequest.start` (Remote-Branch als Start), `GitRemote` mit eigenem `Debug` ohne Token |
| Runner: Push | `backend/crates/runner/src/git.rs` | `push_checked`: Remote-Stand per einem `ls-remote` (Default-Branch, Ziel-Branch, Scan-Basis); `expect` ⇒ `moved` ohne Push, Remote schon auf `sha` ⇒ Erfolg ohne Push (idempotent), unerreichbares Remote mit `expect` ⇒ Fehler statt Raten; Secret-Scan `sha --not <bekannte Remote-Stände>` statt ganzer Historie; nie Force |
| Runner: Token aus argv | `git.rs::auth_env` | Token geht als `http.extraHeader` über `GIT_CONFIG_COUNT/KEY_0/VALUE_0` in die Umgebung des einen Git-Aufrufs, nicht mehr als `-c` auf die Kommandozeile (`/proc/<pid>/cmdline` ist für alle Prozesse lesbar) |
| Runner: Start auf PR-Branch | `backend/crates/runner/src/tasks.rs` | `fetch_start`: holt `refs/heads/<pr-branch>` von der App-URL (fsck, gehärtet, Token per Umgebung) nach `refs/queue/start/<tws>`; dient als Start und als Basis eines `carry`; wird mit dem Arbeitsbaum gelöscht |
| Geteilte Typen/Texte | `backend/crates/api/src/pr.rs`, `branch.rs`, `lib.rs` | `OutputMode`, `GitHost` (Texte, minimale Token-Rechte), Remote-URL-Zerlegung, API-Basis je Host, Validierung (Labels, Runden, API-Adresse, Repo-Pfad), `PrState`, `CiState`, `Check`, `ci_state`, `FeedbackSource/Outcome`, `PullRequestView/Detail`, `DeliveryView/UpdateDelivery`, `HostCheck`; `MergeState::Published`; `Task.pr_id`, `ProjectView.output_mode`, `TaskDetail.pull_request` |
| Migration | `backend/crates/app/migrations/0008_pull_requests.sql` | Projekt: `output_mode` (Standard `commit`), `git_host`, `git_api_url`, `git_repo_path`, `pr_draft`, `pr_labels`, `pr_auto_fix`, `pr_max_followups` (0–10); Tabellen `pull_requests` (Zustand, Checks, ETags, Lease, Backoff, `rate_until`, Runden) und `pr_feedback` (UNIQUE pro PR/Quelle/externe ID, nur Ergebnis, nie Text); `tasks.pr_id`; `merge_state` um `published` erweitert |
| Host-Adapter | `backend/crates/app/src/githost.rs` | GitHub, GitLab, Gitea/Forgejo über den SSRF-geschützten Client der App, **ohne Redirects**, Timeout 20 s, Token nur im Header, Fehlertexte geschwärzt; `whoami`, `can_write` (Rechte per Host-API), `find_open`, `create` (Draft/Labels), `add_labels`, `pr`, `ci` (Checks + nicht vertrauenswürdige Fehlerdetails), `comments` (Kommentare, Reviews, Inline-Kommentare); ETag/If-None-Match für PR und Listen (nur einseitige Listen), Rate-Limit-Header (`Retry-After`, `X-RateLimit-*`, `RateLimit-*`). Kein Approve-/Merge-/Kommentar-Aufruf existiert |
| Poller & Feedback | `backend/crates/app/src/pr.rs` | eigener Tokio-Task (`PR_POLL_SECS`), Lease pro Zeile (`FOR UPDATE SKIP LOCKED`), `recover()` beim Start, Backoff bis 1 h, Pause bei Rate-Limit; Anlegen idempotent; Zustand/CI/Kommentare; Entscheidung je Kommentar (Bot/eigenes Konto, Auslöser, Schreibrecht); **eine Transaktion** mit gesperrter PR-Zeile für Feedback-Zeilen, Rundenzähler und Folge-Auftrag; höchstens ein unfertiger Folge-Auftrag pro PR; Runden-Grenze mit Benachrichtigung; „Eine weitere Runde“, Auto-Fix an/aus; Konflikt-Anzeige; Aufräumen bei Merge/Schließen; `poke()` als Webhook-Schnittstelle (WP07) |
| Prompt | `backend/crates/app/src/prompt.rs` | `pr_followup`: Regeln vor dem Block, Inhalte Dritter in `<<<PR_DATA_<nonce>` … `PR_DATA_<nonce>>>>`, Grenzen im Inhalt entschärft, Secrets geschwärzt, Gesamtgröße begrenzt |
| Hochladen | `backend/crates/app/src/runner_publish.rs`, `runner_merge.rs`, `runner_gate.rs` | Merge-Queue-Schritt für `branch`/`pr` und jede PR-Folge: pusht exakt den gegateten Commit vom Hauptrepo; Treffer ⇒ `held`/Stufe `push`; `moved` ⇒ Konflikt + Korrektur-Auftrag (`carry_from`); Infrastruktur ⇒ Requeue mit Backoff; danach `published`, PR anlegen bzw. neuer Head; `finish_flow` reiht Branch-/PR-Aufträge ein; Folge-Aufträge starten auf dem aktuellen Remote-Stand des PR-Branches |
| API | `backend/crates/app/src/api/pulls.rs`, `api/mod.rs`, `api/tasks.rs`, `api/projects.rs` | `GET/PUT /api/projects/{id}/delivery`, `POST …/delivery/check`, `GET /api/projects/{id}/pulls`, `GET /api/pulls/{id}`, `POST /api/pulls/{id}/{poll,auto-fix,extra-round}`; PR im Task-Detail; Git-Felder des Projekts nur noch für Eigentümer, Token fällt beim Host-Wechsel weg |
| Abbrechen (Bestandsfehler) | `state.rs`, `api/tasks.rs`, `runner.rs`, `runner_merge.rs`, `worker.rs` | Laufgeneration pro Abbruch-Token: ein endender Lauf entfernt nur sein eigenes Token; Abbrechen ignoriert bereits abgebrochene Tokens alter Läufe und wartet kurz auf das Token eines gerade beanspruchten Laufs; Abbrechen eines `queued`-Auftrags, den der Scheduler im selben Moment beansprucht, bricht den Lauf ab statt 409 |
| Fake-Hosts (Test) | `backend/crates/app/tests/common/fake_git_host.rs` | ein Server für alle drei Hosts, Zustand pro Repo, Head live aus dem Bare-Repo, Steuer-API `/_fake/*`, Request-Log, ETag/304, 429 mit `Retry-After`, Hängen für Neustart-Tests |

## 2. Entscheidungen

1. **Hochladen über die Merge-Queue.** Für `branch`/`pr` ersetzt ein Push-Schritt das lokale Squash-Merge. So bleibt die Queue die eine Stelle, an der Ergebnisse den Arbeitsbaum verlassen: seriell pro Projekt, abbrechbar, `recover` nach Neustart, Backoff, Projekt-Parallelität. Jeder Push ist der gegatete Commit (Gate → Commit → Verify → Review) und läuft durch `push_checked`.
2. **Alle Host-API-Aufrufe im Poller**, auch das Anlegen. Eine Stelle für Lease, Backoff und Rate-Limit; ein Absturz zwischen Push und Anlegen führt nur zu einem späteren, idempotenten Anlegen.
3. **Folge-Aufträge auf demselben Branch.** Jeder Auftrag behält seinen lokalen Arbeits-Branch (`queue/<eigene id>`, eindeutiger Index aus WP04), startet aber auf dem *Remote*-Stand des PR-Branches und pusht Fast-Forward dorthin (`expect` = Startstand). Fremde Pushes werden nie überschrieben: Korrektur-Auftrag mit `carry_from` auf dem neuen Stand.
4. **Rechte über die Host-API, fail-closed.** Unbekannte Rechte (Fehler) ⇒ nichts aufgezeichnet, später erneut. Logins mit unerwarteten Zeichen ⇒ kein Schreibrecht.
5. **Auslöser:** Inline-Kommentare und „Änderungen anfordern“ wirken immer, allgemeine Kommentare nur mit `/queue`. Sonst würde jedes „LGTM“ einen Agentenlauf kosten.
6. **Dedupe in der Datenbank** (UNIQUE `pr_id, source, external_id`); CI pro Head-Commit. Aufgeschobene Arbeit (aktiver Folge-Auftrag) wird nicht aufgezeichnet, ihre Listen-ETags werden verworfen, damit sie beim nächsten Poll wieder gelesen wird.
7. **Keine Redirects im Host-Client.** reqwest entfernt `Authorization` bei Cross-Origin-Redirects, eigene Header wie GitLabs `PRIVATE-TOKEN` aber nicht.
8. **Ein Token für Git und API**, verschlüsselt wie bisher (`projects.git_token_enc`). Minimale Rechte je Host im README und in der UI.
9. **Git-Zugang ist Eigentümer-Sache.** Vorher durfte ein Bearbeiter die Remote-URL ändern, während das Token des Eigentümers weiter mitgeschickt wurde (Token-Abfluss an fremden Host). Jetzt: Git-Felder nur Eigentümer; neuer Host ohne neues Token ⇒ Token gelöscht.

## 3. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| — | Token aus argv (Bestand seit WP01/WP03) entfernt | Auftrag „Tokens nie in argv“; beim Lesen von `auth_args` gefunden |
| — | `GitRemote::Debug` ohne Token | jede `{:?}`-Ausgabe einer Push-Anfrage hätte das Token geloggt |
| — | Editor-Token-Abfluss geschlossen | beim Lesen von `projects::update` gefunden |
| — | `rate_until` getrennt von `next_poll_at` | Selbst-Review: „Jetzt prüfen“ hätte `Retry-After` übersprungen |
| — | Gitea-Reviews nur bei erhaltener Listen-ETag überspringen | Integrationstest: nach „Eine weitere Runde“ wurden Inline-Kommentare nie neu gelesen |
| — | Abbrech-Rennen (Bestand, WP04) behoben | beim Stabilisieren der Suite reproduziert (auch auf `4a8da81`, 1 von 6 Läufen) |
| — | GitHub: Entwurf abgelehnt (422) ⇒ normaler Pull Request | laut OpenAPI-Beschreibung gibt es Entwürfe bei privaten Repositories erst ab GitHub Team; sonst wäre nie ein PR entstanden |
| — | Fertige, behandelte CI eines Commits wird nicht erneut abgefragt | Selbst-Review: ~6 Anfragen pro PR und Poll hätten ein GitHub-Kontingent (5000/h) schon bei ~13 offenen PRs erschöpft |
| — | Feedback wird nach dem Sperren der PR-Zeile erneut gegen die Datenbank abgeglichen | Selbst-Review: bei abgelaufener Lease hätten zwei Poller denselben Kommentar zweimal in einen Folge-Auftrag geben können |
| — | „Nicht hochladen“ im Branch-/PR-Modus setzt den Übernahme-Zustand zurück | Selbst-Review: erledigter Auftrag zeigte sonst „Wartet auf Übernahme“ |
| Status `merging` heißt im PR-Modus „Wird hochgeladen“ | Text bleibt „Wird übernommen“ | der Status kennt den Projektmodus nicht; Drawer und Ereignisse sagen „Hochladen“ (Review N7) |

## 4. API-Belege (Doku in dieser Sitzung abgerufen) und Unverifiziertes

Die Doku-Seiten der Hersteller sperrt der Netz-Proxy; abrufbar waren die offiziellen Quellen auf GitHub: `github/rest-api-description` (OpenAPI), `gitlabhq/gitlabhq/doc/api/*.md` (Doku-Quellen von GitLab) und `go-gitea/gitea/templates/swagger/v1_json.tmpl` (Release 1.22).

| Host | Belegt | Nicht belegt (aus Vorwissen, nur gegen den Fake getestet) |
|---|---|---|
| GitHub | `POST/GET /repos/{o}/{r}/pulls` (`head`, `base`, `draft`, Filter `head=owner:branch`), `GET …/pulls/{n}` (`state`, `merged`, `draft`, `mergeable`, `mergeable_state`, `head.sha`), `POST …/issues/{n}/labels`, `GET …/commits/{ref}/check-runs` (`status`/`conclusion`-Werte, `output.title/summary/text`), `GET …/commits/{ref}/status` (`failure` bei `error`/`failure`), `GET …/issues/{n}/comments`, `…/pulls/{n}/comments` (`path`, `line`), `…/pulls/{n}/reviews` (`state`), `GET …/collaborators/{u}/permission` (`admin/write/read/none`, `maintain`→`write`), `GET /user`, Entwürfe nur auf bestimmten Plänen, klassisches Token braucht `repo` | Namen der Fine-grained-Rechte; `mergeable_state = "dirty"` als Konfliktsignal; ob 304-Antworten das Kontingent schonen |
| GitLab | `POST /projects/:id/merge_requests` (`source_branch`, `target_branch`, `title`, `description`, `labels` als Kommaliste), Liste mit `source_branch`/`target_branch`/`state`, MR-Felder (`iid`, `state`, `draft`, `sha`, `has_conflicts`), `GET …/merge_requests/:iid/pipelines` (`sha`, `status`), `GET …/pipelines/:id/jobs`, `GET …/jobs/:id/trace`, `GET …/merge_requests/:iid/notes` (`system`, `sort`, `order_by`), `GET /projects/:id/members/all/:user_id` (`access_level` 30 = Developer), `PRIVATE-TOKEN`-Header, `RateLimit-*`/`Retry-After`, Entwurf per Titel-Präfix `Draft:`, Git über HTTPS mit beliebigem Benutzernamen + Token, Bereiche `api`/`write_repository` | `type: "DiffNote"` und `position.new_path/new_line` für Code-Kommentare (nur aus Beispielen anderer Endpunkte), `author.bot` |
| Gitea/Forgejo | `GET/POST /repos/{o}/{r}/pulls`, `GET …/pulls/{n}` (`merged`, `mergeable`, `draft`, `head.sha`), `POST …/issues/{n}/labels` (Namen erlaubt), `GET …/commits/{ref}/status` (Feld **`status`**, nicht `state`), `GET …/issues/{n}/comments`, `GET …/pulls/{n}/reviews`, `…/reviews/{id}/comments`, `GET …/collaborators/{c}/permission` (`permission`), `Authorization: token …`, Parameter `limit`/`page` | Werte der Review-Zustände (`REQUEST_CHANGES`, `PENDING`), Berechtigungswert `owner`, Entwurf über Titel-Präfix `WIP:`, Basic-Auth mit Token als Passwort für Git über HTTPS, Rate-Limit-Header |
