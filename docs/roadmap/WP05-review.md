# WP05 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP05-Diff gegen `4a8da81` (common, runner, api, app, Migration 0008, Frontend, E2E, README). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP05), Auftrag des Koordinators (Host-Adapter über REST mit dem SSRF-Client, Fake-Server je Host, Prompt-Injection, Rechteprüfung über die Host-API, nie Approve/Merge, Loop-Schutz, Dedupe, Tokens verschlüsselt und nie in Logs/Events/argv/API, Mehrmandanten, robustes Polling, Output-Modus konsistent mit Merge-Queue und Gate), Report P1.2. Jede Feststellung ist am Code oder an einem Testlauf geprüft.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und mit Tests belegt; die Suite ist auf dem finalen Stand zweimal hintereinander grün (`WP05-test.md`). Die schwersten Funde lagen nicht im neuen Code, sondern im Bestand, den WP05 benutzt: Das Git-Token stand (base64) auf der Kommandozeile jedes Git-Aufrufs, ein Bearbeiter konnte die Remote-URL umbiegen und das Token des Eigentümers mitschicken lassen, und ein Abbrechen konnte nach einem Wiederholen still verloren gehen. Ehrliche Grenzen: Gegen echte GitHub-/GitLab-/Gitea-Server ist nichts gelaufen, einzelne Gitea-Details sind nicht aus Doku belegt, und Konflikte zwischen Pull Request und Ziel-Branch löst Queue nicht selbst.

## Akzeptanz gegen WP05

| Anforderung | Status | Beleg |
|---|---|---|
| Host-Adapter GitHub, GitLab, Gitea/Forgejo (REST), PR/MR erstellen, Draft optional, Titel/Body aus dem Auftrag, Labels | erfüllt; Draft bei GitHub-Plänen ohne Entwürfe ⇒ normaler PR | Integration (je Host ein Szenario), `github_without_drafts_gets_a_normal_pull_request`, Unit `githost::tests` (5), System-E2E |
| SSRF-sicherer Client der App | erfüllt; zusätzlich ohne Redirects, Runner nie Ziel, private Netze nur mit `ALLOW_PRIVATE_GIT_HOSTS` | `githost.rs` (`guarded_builder`, `check_target`); Unit-Tests des Guards aus WP02 unverändert grün |
| API-Formate aus offizieller Doku, sonst markiert | erfüllt | `WP05-impl.md` §4, README „Bekannte Grenzen“ |
| CI rot ⇒ Folge-Auftrag auf demselben Branch | erfüllt (Start auf Remote-Stand des PR-Branches, Fast-Forward, nie Force, fremde Pushes nie überschrieben) | `github_pr_is_created_and_red_ci…`, `foreign_push_to_the_pr_branch_is_never_overwritten`, Runner `pull_request_follow_up_starts_on_the_remote_branch`, `task_branch_push_scans_from_the_base…`, System-E2E |
| Nur Kommentare mit Schreibrecht, Rechte per Host-API | erfüllt, fail-closed | `gitlab_comments_without_write_access_are_ignored`, Gitea-Test (eve/read), Unit `permissions_per_host`, System-E2E |
| Inhalte als Daten markiert | erfüllt (Nonce-Grenze, Regeln davor, Grenzen im Inhalt entschärft, Secrets geschwärzt, begrenzt; ignorierte Kommentare erreichen den Agenten nie) | Unit `pr_followup_marks_third_party_text_as_data`, Integration prüft Reihenfolge und Abwesenheit |
| Agent approved/merged nie | erfüllt: kein solcher Aufruf im Code; jede Anfrage an die Fakes wird geprüft | `assert_never_approves_or_merges` in allen PR-Integrationstests, System-E2E |
| Loop-Schutz, Dedupe pro Kommentar/CI-Lauf | erfüllt (Grenze pro PR, ein aktiver Folge-Auftrag, UNIQUE in der DB, Abgleich unter Zeilensperre) | `gitea_loop_limit…`, Dedupe-Teil des GitHub-Tests, Neustart-Test |
| Tokens verschlüsselt, nie in Logs/Events/argv/API | erfüllt; argv-Leck im Bestand geschlossen | Runner `git_token_goes_through_the_environment_not_argv`, Integration durchsucht Task-JSON, Events (DB), PR-, Projekt-, Delivery- und Benachrichtigungs-API, E2E-Seiteninhalt |
| Mehrmandanten | erfüllt (Fremde 404, Betrachter lesen, Bearbeiter steuern, Eigentümer konfiguriert) | `pull_requests_are_visible_only_inside_the_project`, UI-E2E „read-only“ |
| Polling robust: Backoff, Rate-Limit, ETag, Neustart, blockiert Scheduler nicht | erfüllt | `restart_during_polling_processes_feedback_exactly_once`, `host_check_rate_limits_and_bad_tokens`, ETag-Zähler im GitHub-Test, Unit `rate_limit_headers`, `backoff_grows_and_is_capped` |
| Webhook-Schnittstelle (WP07) | erfüllt: `pr::poke` (nur „jetzt abfragen“, respektiert `Retry-After`) | Code, über „Jetzt prüfen“ getestet |
| Output-Modus `commit`/`branch`/`pr`, konsistent mit Merge-Queue und Gate, „wer pusht wann“ | erfüllt; Standard `commit` unverändert (alle WP04-Tests grün) | `branch_mode_pushes…`, README-Tabelle, UI „Wer lädt wann hoch?“ |
| UI: PR-Link, Status, Checks, Output-Modus, Host-Zugang; Designsystem, Safe Areas, axe | erfüllt | `ui/pull_requests.spec.ts` (3), `ui/pull_requests-mobile.spec.ts` (iPhone hoch/quer), Tour (Reiter „Ergebnis“, hell/dunkel, Desktop/iPhone) |
| README Einrichtung pro Host, minimale Token-Rechte | erfüllt (Fine-grained-Rechtenamen als unbelegt markiert) | README „Ergebnis als Branch oder Pull Request …“ |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Git-Token auf der Kommandozeile** (Bestand): `-c http.extraHeader=Authorization: Basic base64(…)` – `/proc/<pid>/cmdline` ist für jeden Prozess lesbar, bei Engine `none` auch für den Agenten. | Lesen von `git.rs::auth_args` | Header über `GIT_CONFIG_COUNT/KEY/VALUE` in die Umgebung des einen Aufrufs | `git_token_goes_through_the_environment_not_argv` (Git liest den Header, Quelltext ohne `-c`-Header) |
| B2 | **`GitRemote` druckte das Token per `Debug`.** Jede `{:?}`-Ausgabe einer Push-/Fetch-Anfrage hätte es geloggt. | Lesen von `common/lib.rs` | eigenes `Debug` mit `***` | gleicher Test |
| B3 | **Token-Abfluss durch Bearbeiter** (Bestand): `PATCH /api/projects/{id}` ließ Bearbeiter die Remote-URL ändern; das gespeicherte Token des Eigentümers ging beim nächsten Push/Pull an den neuen Host. Mit WP05 hätte zusätzlich die API-Adresse daran gehangen. | Lesen von `api/projects.rs` | Git-Felder nur Eigentümer; Host-Wechsel ohne neues Token löscht es; Delivery-Einstellungen nur Eigentümer | Mandanten-Test (Bearbeiter 403, Token weg nach Host-Wechsel), UI-E2E |
| B4 | **Abbrechen ging verloren** (Bestand, WP04): Der alte Lauf entfernte beim Aufräumen das Abbruch-Token des schon gestarteten Wiederholungslaufs; Abbrechen traf das bereits abgebrochene Token des alten Laufs; Abbrechen eines gerade beanspruchten `queued`-Auftrags endete in 409. Der Auftrag lief 30 s weiter. Auch auf `4a8da81` reproduziert (1 von 6 Läufen von `branches.rs`). | Stabilisieren der Suite, Debug-Ausgaben | Laufgeneration pro Token, abgebrochene Tokens zählen nicht, kurzes Warten auf das Token, 409-Fall bricht den Lauf ab | `branches.rs` 20 von 20 Läufen grün (vorher ca. 1 von 6 rot) |
| B5 | **Secret-Scan der ganzen Historie** beim ersten Push eines Task-Branches (Remote-Stand unbekannt ⇒ `git log -p <sha>`) ⇒ bei großen Repos `too_large` ⇒ jeder PR hätte eine Freigabe gebraucht. | Lesen von `push_checked` | `scan_base` (Ziel-Branch des Remotes) ausgeschlossen, fail-closed bei unbekanntem Stand | `task_branch_push_scans_from_the_base…` |
| B6 | **Gitea-Inline-Kommentare nach „weitere Runde“ nie neu gelesen** (bekannte Reviews übersprungen). | Integrationstest | Überspringen nur bei erhaltener Listen-ETag | `gitea_loop_limit…` |
| B7 | **Aufgeschobene Kommentare nie wieder gelesen**: Bei aktivem Folge-Auftrag wurden sie nicht aufgezeichnet, die Listen-ETag aber gespeichert ⇒ 304 ⇒ verloren. | Selbst-Review | ETags der Listen verwerfen, wenn Arbeit aufgeschoben wurde | Code; Gitea-/Dedupe-Tests |
| B8 | **„Jetzt prüfen“ übersprang `Retry-After`**; Rate-Limit-Header erfolgreicher Antworten wurden ignoriert. | Selbst-Review | `rate_until`, `quota_pause` | `host_check_rate_limits…` (keine Anfrage während der Pause) |
| B9 | **Doppelter Folge-Auftrag bei abgelaufener Lease** (zwei Poller entscheiden über denselben Kommentar; der erste Folge-Auftrag schon fertig). | Selbst-Review | Abgleich gegen `pr_feedback` unter Zeilensperre | Code; Neustart-Test mit parallelen Pollern |
| B10 | **GitHub-Entwürfe auf Free-Plänen privater Repos ⇒ 422 ⇒ nie ein PR.** | OpenAPI-Beschreibung von `pulls/create` | einmal ohne Entwurf wiederholen | `github_without_drafts…` |
| B11 | **Kontingent:** ~6 Anfragen pro PR und Poll (60 s) ⇒ GitHub-Limit 5000/h bei ~13 offenen PRs erschöpft. | Selbst-Review (Rechnung) | fertige, behandelte CI eines Commits nicht erneut abfragen; ETag für PR und Listen | GitHub-Test (keine weiteren Check-Abfragen) |
| B12 | **Token-Weitergabe über Redirects**: reqwest entfernt bei Cross-Origin-Redirects nur Standard-Header, nicht `PRIVATE-TOKEN`. | Planung | Host-Client folgt keinen Redirects | Code (`redirect::Policy::none`) |
| B13 | Testrennen in WP04-Tests (`HEAD` vor Ende der Vorbereitung, Branch nach dem Arbeitsbaum gelöscht) und Textkollision im WP04-UI-Test durch den neuen Git-Hinweis. | Abnahmelauf | Tests warten auf den Endzustand (gleiche Prüfung), Hinweis umformuliert | `branches.rs`, `ui/branches.spec.ts` grün |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **Nicht gegen echte Dienste geprüft.** Fakes bilden die belegten Formate nach; nicht belegte Gitea-Details (Review-Zustände, `WIP:`, `owner`, Basic-Auth mit Token) und GitHubs `mergeable_state = "dirty"` sind markiert. | Netz-Proxy sperrt die Dienste; Fehler zeigen sich als lesbare `last_error` mit Backoff, nicht als stilles Fehlverhalten. |
| N2 | **Konflikt PR ↔ Ziel-Branch** wird angezeigt und gemeldet, aber nicht automatisch behoben. | Ein Merge-Commit des Ziel-Branches auf den PR-Branch wäre ein eigener Ablauf mit eigenem Gate; Report P1.2 nennt Polling, nicht Auto-Fix. |
| N3 | **Rechte sind eine Momentaufnahme** zum Zeitpunkt der Verarbeitung. | Wer danach das Schreibrecht verliert, dessen Kommentar bleibt verarbeitet. |
| N4 | **Ein Token für Git und API** (breitere Rechte als nur Push). | Bestehendes Credential-Modell (ausgeklammert); minimale Rechte im README. |
| N5 | **Reaktionszeit** bis `PR_POLL_SECS` (Standard 60 s); eine Re-Run-Änderung der CI desselben Commits wird nach dem ersten Endzustand nicht mehr angezeigt. | Bis WP07 (Webhooks über `poke`). |
| N6 | **`/queue`-Pflicht** für allgemeine Kommentare ist eine Produktentscheidung. | In UI und README erklärt. |
| N7 | Status `merging` heißt auch im Branch-/PR-Modus „Wird übernommen“. | Drawer-Ereignisse und Merge-Zustand („Hochgeladen“) sind richtig; der Status kennt den Projektmodus nicht. |
| N8 | Folge-Aufträge übernehmen Anbieter/Modell/Zugang des Ursprungsauftrags, nicht die aktuelle Projekt-Routing-Vorgabe. | Bewusst: gleicher Agent für dieselbe Arbeit. |
| N9 | „Nicht hochladen“ im Branch-/PR-Modus ist nur per Code-Review geprüft (ein Push-Halt auf einen `queue/…`-Branch braucht einen Geheimnis-Fund nur im Push-Bereich, der im Mock nicht herstellbar ist). | Ein-Zeilen-Zustandsänderung. |
| N10 | Engine `none` (alle Tests): Agent auf dem Host, kann theoretisch das Umgebungs-Token eines parallel laufenden Git-Prozesses desselben Nutzers lesen. | Wie WP02/WP04: nur Entwicklung, gekennzeichnet. |

## Geprüft und für gut befunden

- **Gate nie umgangen:** Gepusht wird ausschließlich in der Merge-Queue, exakt `snapshot_after` nach Gate/Verify/Review, vom Hauptrepo, an die URL der App, mit Secret-Scan und Default-Branch-Schutz; Funde ⇒ `held`/Stufe `push`, Fortsetzung nur nach Freigabe. Setup-/Agent-Konfigurations-Baselines gelten auch für Folge-Aufträge (Start auf dem PR-Stand, geänderte Setup-Dateien brauchen eine Freigabe).
- **Architektur:** Git nur im Runner, Host-API nur in der App (kein Prozess, kein Dateisystem), Protokoll in `queued-common`, Texte/Typen in `queued-api`, nur neue Migration (0008).
- **Alt-Verhalten:** Standardmodus `commit` unverändert; Alt-Tests nur dort angepasst, wo sie Rennen hatten (gleiche Endprüfung).
