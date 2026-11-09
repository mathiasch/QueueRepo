# WP04 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP04-Diff gegen `87b1ba6` (common, runner, api, app, Migration 0007, Frontend, E2E, README). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP04), Auftrag des Koordinators (kein Host-Root, Sicherheitsmodell von Runner/Egress/Gate nicht aufweichen, jeder Arbeitsbaum ist vom Agenten kontrolliert, Container sehen nur den eigenen Arbeitsbaum, race-freier Scheduler mit Nebenläufigkeitstests), Report P1.1, offene Funde WP03-N2/N6. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und mit Tests belegt; die Suite ist auf dem finalen Stand zweimal grün (`WP04-test.md`). Die zentrale Abweichung – kein `git worktree`, sondern ein isoliertes Repository pro Auftrag – ist keine Bequemlichkeit, sondern die einzige Variante, die „Agent sieht nur seinen Arbeitsbaum“ und ein funktionierendes Git im Container gleichzeitig erfüllt (Plan §3.1). Ehrliche Grenzen: Plattenplatz pro laufendem Auftrag, Konflikte kosten einen Korrektur-Auftrag (Modellzeit), und Engine `none` isoliert weiterhin nichts.

## Akzeptanz gegen WP04

| Anforderung | Status | Beleg |
|---|---|---|
| Runner: Arbeitsbaum pro Task (`queue/<task-kurz-id>`), Agent isoliert | erfüllt (eigenes Repo statt `git worktree`, s. o.); Container mounten nur `workspaces/t<task>` | `tasks::tests::task_workspace_is_an_isolated_repository`, `agent_cannot_reach_main_repo_or_other_tasks`, Integration `parallel_tasks_…` (Agent-`pwd` = eigener Arbeitsbaum), `lib::tests::task_endpoints_validate_workspace_kinds` |
| Parallelität pro Projekt (Standard 1), Scheduler berücksichtigt sie | erfüllt, race-frei auch bei gleichzeitigen Claimern; Nutzer-Parallelität und Anbieter-Pausen unverändert | `concurrent_claims_respect_project_parallelism` (24 parallele Claimer, 3 Runden), `concurrent_merge_claims_…`, `default_parallelism_keeps_a_project_serial_including_its_merge`, System-E2E |
| Serielle Merge-Queue: Rebase → Verify erneut → Fast-Forward | erfüllt (Squash-Rebase per `merge-tree`, Gate-Nachprüfung, Verify bei bewegtem Ziel, nur Fast-Forward) | `merge_queue_squashes_rebases_and_fast_forwards`, `merge_verifies_again_when_the_target_moved`, System-E2E |
| Konflikt → Fix-Task mit Konfliktkontext | erfüllt (Fix startet mit den Änderungen und Markern, Prompt nennt Dateien, Marker blockieren die Übernahme) | `conflicts_become_a_fix_workspace_with_markers`, `merge_conflict_becomes_a_fix_task_that_carries_the_changes`, System-/UI-E2E |
| Branch-Aufräumen | erfüllt (sofort bzw. nach 7 Tagen für fehlgeschlagene; Projekt-Löschung) | `cancelling_a_task_removes_its_working_tree_and_branch`, Integration `parallel_tasks_…` |
| Diff/Revert/Log korrekt (Diff gegen Merge-Base) | erfüllt | `diff_and_revert_against_the_merge_base`, `interrupted_task_diff_*`, Unit `task_roundtrip_and_accessors` |
| UI: Parallelität, Merge-Queue-Status, Branch pro Auftrag, Konflikte; Designsystem, Safe Areas, axe | erfüllt | `ui/branches.spec.ts`, `ui/branches-mobile.spec.ts` (iPhone hoch/quer), Tour (axe hell/dunkel) |
| Gate wird nicht umgangen, Verify vor dem Merge prüft erneut | erfüllt: alle Gate-Schritte im Arbeitsbaum, übernommen wird exakt der gegatete Commit (nicht der Branch-Kopf nach Verify/Review), Nachprüfung fail-closed | `sync_copies_exactly_the_given_commit_…`, `merge_rechecks_the_merged_change`, Gate-Integrationstests unverändert grün |

## WP03-N6 (gehaltene Secrets lesbar): verschlimmert oder verbessert?

- **Mit `git worktree` wäre es schlimmer geworden:** Alle Worktrees teilen Objektdatenbank und `refs/` – jeder parallel laufende Agent hätte gehaltene Änderungen (und Quarantäne) jedes anderen Auftrags lesen *und* löschen/umbiegen können, und zwar gleichzeitig statt nur nacheinander.
- **Mit der Umsetzung ist es geschlossen:** `hold`/`release`/`discard` laufen im Arbeitsbaum des Auftrags. Ein anderer Auftrag bekommt ein neues Repository, gefüllt per Upload-Pack nur mit der Historie des Hauptstands; gehaltene, quarantänierte und fremde Branch-Objekte kommen nicht mit (Test prüft `cat-file -e` auf Quarantäne-, Held- und Fremd-Objekte). Alt-Holds aus WP03 im gemeinsamen Repo werden bei der Freigabe in den Arbeitsbaum verschoben und im Hauptrepo geprunt (`legacy_held_changes_move_into_the_task_workspace`). Rest: Ein Agent *desselben* Auftrags sieht seine eigenen gehaltenen Änderungen (er hat sie geschrieben).

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **WP03-Baselines vom Agenten fälschbar.** `refs/queue/baseline/{setup,agent-config}` lagen im gemounteten `.git`; `git update-ref refs/queue/baseline/setup HEAD` im Container hätte die Setup-Freigabe vor Secrets ausgehebelt. | Lesen von `git.rs` beim Planen | Baselines nur im Hauptrepo, Prüfung gegen den ins Hauptrepo geholten Task-HEAD (`BaselineCheck.task`) | `agent_cannot_reach_main_repo_or_other_tasks` (gefälschte Baseline wirkungslos) |
| B2 | **Fehlgeschlagene Arbeit und Teil-Commits auf dem Ziel-Branch** (Bestand, WP03-N2). | Plan | Alles auf dem Task-Branch; Übernahme nur nach Gate/Verify/Review | `runtime_limit_blocks_without_fix`, `post_hoc_token_overrun_…` (Teil-Commit auf dem Branch, nicht im Verlauf) |
| B3 | **Scheduler-Race:** `NOT EXISTS running` im selben Snapshot – zwei Claimer starteten zwei Aufträge im selben Projekt. | Lesen von `worker.rs` | Projektzeilen-Sperre + neue Anweisung (frischer Snapshot) | `concurrent_claims_respect_project_parallelism`, `concurrent_merge_claims_…` |
| B4 | **Parallel-1 erzeugte Konflikte:** Der nächste Auftrag startete, während der vorige noch in der Übernahme stand (beide hängen an `MOCK_LOG.md` an ⇒ Konflikt). | Integrationstest | `merging` zählt zur Projekt-Parallelität | `default_parallelism_keeps_a_project_serial_including_its_merge` |
| B5 | **Fix-Auftrag zu früh gestartet ohne Carry:** `carry_from` wurde nach `insert_task` (das den Scheduler weckt) gesetzt. | Code-Review | `carry_from` im `INSERT` | `merge_conflict_becomes_a_fix_task_…` |
| B6 | **Branch des Konflikt-Auftrags blieb nach erfolgreichem Fix:** die Aufräumprüfung „wird noch fortgesetzt“ zählte den gerade übernommenen Fix selbst mit. | Integrationstest | `clean_carried` entfernt ohne diese Prüfung | `merge_conflict_becomes_a_fix_task_…` |
| B7 | **Aufräumen bekam 409:** Der Runner gab den Workspace erst nach Stream-Ende frei, die App räumte schon nach dem `Exit`-Frame auf. | Integrationstest (sporadisch) | Freigabe beim `Exit`-/`Fail`-Frame, ohne einen neuen Lauf gleichen Namens auszutragen | `cancelling_a_task_…` stabil |
| B8 | **Abbruch während der Vorbereitung ging verloren** (Bestand): Der Watcher schickte Cancel, bevor der Lauf beim Runner registriert war; der Agent lief 30 s weiter. | Integrationstest | kein Start bei bereits abgebrochenem Token, Cancel wird bis Laufende wiederholt | `cancelling_a_task_…` |
| B9 | **Leerer Commit nach App-Absturz** zwischen Fast-Forward und DB-Update (`recover` reiht die Übernahme neu ein). | Selbst-Review | `MergeResult.empty` ⇒ „bereits im Hauptstand“ | `merge_queue_squashes_…` (zweite Übernahme ⇒ `empty`, HEAD unverändert) |
| B10 | **Merge-Verify umlenkbar:** `export_to_task` holte `C` per `git fetch` im (vom Agenten kontrollierten) Arbeitsbaum ohne vorheriges `harden()` – `url.<x>.insteadOf` hätte Verify auf fremden Inhalt laufen lassen. | Selbst-Review | `harden()` vor dem Fetch | Code; `harden()` ist durch WP03-Tests abgedeckt |
| B11 | **Merge-Requeue überschrieb den Verbrauch** (`block_running` mit leerem `Usage`). | Selbst-Review | echten Verbrauch durchreichen | Code |
| B12 | **Symlink-Lecks (Bestand):** Dateien-API folgte Symlinks aus dem Workspace (Host-Dateien für jedes Mitglied lesbar), ZIP-Import schrieb durch Symlink-Verzeichnisse. Mit WP04 kommen Bäume der Aufträge in den Hauptstand. | Lesen von `git.rs` | canonicalize + Präfix/`.git`-Prüfung; Import überspringt Symlink-Pfade | `file_api_and_zip_import_never_follow_symlinks` |
| B13 | **„Verwerfen“ ohne Rückfrage für Aufträge in der Übernahme** hätte fertige, geprüfte Arbeit samt Branch gelöscht; „Wiederherstellen“ startet nur neu. | UI-Review | eigener Button „Übernahme abbrechen“ mit Bestätigung | E2E (Drawer) |
| B14 | **Projekt löschen ließ Arbeitsbäume liegen** (`workspaces/t…`). | Selbst-Review | Arbeitsbäume vor dem Löschen erfassen und entfernen (auch Eval) | `cancelling_a_task_…` (Teil Projekt löschen) |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **Plattenplatz:** pro laufendem Auftrag eine volle Kopie der Historie (kein Hardlink/Alternates, s. Plan §3.1). Keine Obergrenze. | Bei großen Repos und hoher Parallelität spürbar; Aufräumen sofort bei Endzustand. Shallow-Clones würden Agenten die Historie nehmen. |
| N2 | **Ignorierte Dateien im Arbeitsbaum** (z. B. `node_modules/`, vom Agenten verändert) bleiben beim Merge-Verify liegen (`clean -fd` ohne `-x`, damit Abhängigkeiten nicht neu installiert werden müssen). Ein Agent könnte damit Verify beeinflussen. | Bestand (WP03-Verify lief im selben Zustand); Review-Agent und Gate bleiben. Sauber wäre Verify in einem frischen Arbeitsbaum mit Setup – kostet ein Setup pro Übernahme. |
| N3 | **Abbrechen einer laufenden Übernahme ohne Verify** wirkt erst beim nächsten abbrechbaren Schritt; ohne Verify wird sie zu Ende geführt. | Übernahme ohne Verify dauert Sekunden. |
| N4 | **Konflikte kosten Modellzeit** und Fix-Versuche; Agent muss Marker selbst auflösen. Keine Dateibereichs-Tags (Report P1.1, optional). | Standard 1 = keine Konflikte zwischen eigenen Aufträgen. |
| N5 | **Gate-Basis bei Wiederaufnahme = erster Ausgangsstand:** Nach einer Freigabe und einem späteren erneuten Lauf desselben Auftrags (Rückfrage, Retry) wird eine schon freigegebene Änderung an geschützten Dateien erneut vorgelegt (Secrets nicht: Fingerabdruck). | Sicher statt bequem; dokumentiert. |
| N6 | **Squash:** Zwischen-Commits des Branches gehen beim Übernehmen auf. | Gewolltes Produktmodell (ein Commit pro Auftrag). |
| N7 | **Merge-Läufe zählen nicht zur Nutzer-Parallelität** (kein Anbieter), belegen aber den Runner (Verify). | Pro Projekt seriell. |
| N8 | **Branch-Namenskollision** (gleiche 12 Hex-Zeichen im Projekt) fällt auf die volle ID zurück; der Zweig ist nicht durch einen Test abgedeckt (Kollision praktisch nicht herstellbar ohne DB-Manipulation). | Eindeutiger Index sichert ab. |
| N9 | **Engine `none`:** Der Agent läuft als Runner-User auf dem Host und kann alle Arbeitsbäume und das Hauptrepo lesen und schreiben. | Unverändert gekennzeichnet („nicht durchgesetzt“), nur Entwicklung. |
| N10 | Upload-Pack beim Sync läuft im vom Agenten kontrollierten Repo (nach `harden()`, mit `HARDEN`, fsck). Symlinks *innerhalb* von `.git/objects/xx` (WP03-N9) könnten Host-Dateien als Objekte anbieten; deren Inhalt müsste zur Objekt-ID passen und landet nur im Hauptrepo, nie beim Agenten. | Kein Leck-Pfad erkennbar. |
| N11 | Rootless Podman/Docker mit Arbeitsbaum-Mounts nicht live geprüft (Engine `none` in allen Tests); die Container-Argumente sind unverändert (nur ein anderer Pfad). | Siehe WP02-N4. |

## Geprüft und für gut befunden

- **Kein Host-Root, Sicherheitsmodell nur gehärtet:** keine neuen Capabilities, keine Netzänderung, Container-Argumente unverändert (WP02-Tests grün); jeder Git-Aufruf weiter mit `HARDEN`, jede Operation auf einem Arbeitsbaum nach `harden()`; Objekte nur per exakter SHA mit fsck ins Hauptrepo.
- **Architektur:** Git/Dateien nur im Runner, Protokoll in `queued-common`, Texte/Typen in `queued-api`, App ohne Host-Rechte; nur neue Migration (0007).
- **Alt-Tests:** angepasst nur dort, wo WP04 das Verhalten absichtlich ändert (Teil-Commits nicht mehr im Hauptstand, ein Commit pro Auftrag, gehaltene Änderungen im Arbeitsbaum); jede Anpassung prüft das neue Verhalten mindestens so streng.
