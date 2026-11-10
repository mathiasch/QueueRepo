# WP05 – Testbericht

Umgebung: Linux-Container, rustc/cargo 1.97.0, Git 2.43, Postgres 16 lokal, Trunk 0.22.1, Node 22, Playwright mit Chromium (`/opt/pw-browsers`), axe-core. Runner-Engine in allen automatisierten Tests: `none`. `CARGO_INCREMENTAL=0` wegen Plattenplatz.

**Keine externen Calls.** Git-Server sind ein eigener Fake-Server für GitHub, GitLab und Gitea (`backend/crates/app/tests/common/fake_git_host.rs`, im E2E-Stack als `examples/fake_git_host`), Git-Remotes lokale Bare-Repositories, Agenten der Mock-Provider. Die API-Formate des Fakes stammen aus der offiziellen GitHub-OpenAPI-Beschreibung, den GitLab-Dokumentationsquellen (`gitlabhq/doc/api`) und dem Gitea-Swagger-Template 1.22 (Abruf in dieser Sitzung); was dort nicht steht, ist in `WP05-impl.md` §4 als unverifiziert markiert.

## 1. Abnahmeläufe (finaler Stand, zweimal hintereinander)

Skript (Scratchpad `accept.sh`): Backend fmt → clippy → `cargo test --workspace` → Frontend fmt → clippy wasm32 → clippy nativ → `cargo test` → `trunk build --release` → `npx playwright test` (alle Projekte; `stack.sh` baut Binaries und den Fake-Host selbst). Zwischen Lauf 1 und 2 keine Codeänderung.

| Schritt | Lauf 1 | Lauf 2 |
|---|---|---|
| Backend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Backend `cargo clippy --workspace --all-targets -- -D warnings` | rc=0 | rc=0 |
| Backend `cargo test --workspace` | **391 passed, 0 failed** (171 s) | **391 passed, 0 failed** (120 s) |
| Frontend `cargo fmt --all -- --check` | rc=0 | rc=0 |
| Frontend `cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo clippy --all-targets -- -D warnings` | rc=0 | rc=0 |
| Frontend `cargo test` | **47 passed** | **47 passed** |
| Frontend `trunk build --release` | rc=0 | rc=0 |
| Playwright (system 21, desktop 32, iphone 10, iphone-landscape 10; inkl. axe) | **73 passed** (4,6 min) | **73 passed** (4,6 min) |

Vergleich mit WP04: Backend 368 → 391 (+23), Frontend 45 → 47, Playwright 66 → 73.
Backend je Testziel: `queued` lib 122 (+7), `api.rs` 19, `branches.rs` 9, `cli_contract.rs` 26, `edge_cases.rs` 11, `environments.rs` 8, `failures.rs` 19, `gate.rs` 9, `parser_robustness.rs` 6, `providers.rs` 16, **`pull_requests.rs` 9 (neu)**, `queued-api` 24 (+4), `queued-common` 34, `queued-runner` 79 (+3).

Ein Vorlauf (Lauf A, vor den letzten Korrekturen) hatte 2 rote Playwright-Tests: der WP04-UI-Test „editors see it read-only“ (Textkollision mit dem neuen Git-Hinweis, strict mode) und einmalig der PR-System-Test (beim Einzel-Nachlauf grün; Ursache im gekürzten Log nicht erhalten, Zeitgrenzen der PR-Specs von 20 s auf 45 s angehoben, Playwright-Log wird seitdem vollständig gesichert). Beide in Lauf 1 und 2 grün.

## 2. Neue Tests

### Unit
| Ort | Tests | Was |
|---|---|---|
| `runner/src/git.rs` | 2 | **Push eines Task-Branches:** ohne `scan_base` ganze Historie (altes Secret ⇒ Halt), mit Basis nur neue Commits; `expect=""` für neuen Branch; erneuter Push desselben Commits ⇒ ohne Push „bereits“; `expect` falsch ⇒ `moved`, richtig ⇒ Fast-Forward; unerreichbares Remote mit `expect` ⇒ Fehler. **Token nicht in argv:** Git liest `http.extraHeader` aus der Umgebung, ohne Token kein Header (auch nichts geerbt), `HARDEN` ohne Header, `Debug` ohne Token, Quelltext ohne `-c`-Header |
| `runner/src/tasks/tests.rs` | 1 | Folge-Auftrag startet auf dem Remote-Stand des PR-Branches (mit Commit eines Menschen, nicht auf dem bewegten Ziel-Branch), Carry landet auf diesem Stand, Start-Ref wird mit dem Arbeitsbaum gelöscht, fehlender Branch ⇒ Fehler statt Rückfall auf den Ziel-Branch |
| `api/src/pr.rs` | 4 | Enums/Texte, Remote-URL-Zerlegung (https/ssh/scp, Ports, Pfade, `file://`, `..`), API-Basis je Host (github.com, GHES, GitLab, Gitea mit Port, SSH-Port ≠ Web-Port), Validierung (Runden, Labels, API-Adresse, Repo-Pfad), CI-Aggregat |
| `app/src/githost.rs` | 5 | Rate-Limit-Header (`Retry-After`, `X-RateLimit-*`, `RateLimit-*`, Deckel 1 h, alte Resets), Schreibrecht je Host, PR-Zustände je Host (inkl. Link nur http(s)), Checks je Host (GitHub-Conclusions, GitLab `allow_failure`, Gitea `status`), Kommentare/Auslöser (Inline, Review-Zustände, `/queue`, Bots, GitLab-DiffNote und Systemnotiz, Kürzung) |
| `app/src/prompt.rs` | 1 | Datenblock mit Nonce, Regeln davor, Grenze im Inhalt unwirksam, Injektionstext bleibt nur als Daten, Secret geschwärzt, Gesamtgröße begrenzt |
| `app/src/pr.rs` | 1 | Backoff wächst und ist gedeckelt |
| `frontend/src/logic/pr.rs` | 2 | Runden-Texte, „weitere Runde“ nur bei offener PR und erreichter Grenze, Töne, Zustandszeile mit Konflikt, Host-Auswahltext, „Wer lädt wann hoch?“, Labels |
| `frontend/src/logic/labels.rs` | erweitert | Auftragsart „PR-Folge“, Ereignisart „Pull Request“ |
| `api/src/lib.rs` | erweitert | alte Task-Zeilen ohne `pr_id` parsen |

### Integration (`backend/crates/app/tests/pull_requests.rs`, App + echter Runner + Postgres + Fake-Host + Bare-Remote)
1. **GitHub – PR erstellen, CI rot ⇒ Folge-Auftrag auf demselben Branch:** `published`, genau der gegatete Commit auf `queue/<id>`, Ziel-Branch und lokaler Verlauf unberührt; PR #1 (Draft, Label, Body ohne Token); rote Checks mit Injektionstext ⇒ ein `pr_fix` (Titel, `created_by_agent`, Injektion nur im Datenblock); Folge-Auftrag startet auf dem PR-Head, pusht Fast-Forward auf denselben Branch, kein zweiter PR; Dedupe pro CI-Lauf; grüne Checks ⇒ kein Auftrag; fertige CI wird nicht erneut abgefragt; gemergt ⇒ keine Abfragen mehr; ETag/304 genutzt; nie Approve/Merge; Token in keiner Antwort, keinem Event (DB) und keiner Benachrichtigung.
2. **GitLab – Kommentar ohne Schreibrecht ignoriert:** Reporter (20) ⇒ `ignored_permission`, Developer ohne `/queue` ⇒ `ignored_untriggered`, eigenes Konto und Bot ⇒ `ignored_bot`, Systemnotiz gar nicht; Rechte über `members/all`; Developer-Inline-Kommentar ⇒ Folge-Auftrag, der den ignorierten Text nicht enthält; `Draft:`-Titel.
3. **Gitea – Loop-Grenze:** Grenze 1; „Änderungen anfordern“ ⇒ Folge-Auftrag; zweite Runde (Inline-Kommentar über das Review) ⇒ `ignored_limit` + Benachrichtigung; Fremde 404 auf „weitere Runde“; Eigentümer ⇒ Grenze 2 und zweiter Folge-Auftrag mit dem Kommentar; `read`-Nutzer bleibt ignoriert; `WIP:`-Titel.
4. **Neustart während des Pollings:** Kommentarlisten hängen, Poll läuft, Lease verhindert zweiten Zugriff, Poll wird abgebrochen (nichts geschrieben), neue App-Instanz auf derselben DB `recover()`, beide Instanzen pollen viermal parallel ⇒ genau ein Folge-Auftrag, genau eine Feedback-Zeile, Rundenzähler 1.
5. **Fremder Push auf den PR-Branch:** Mensch pusht während Verify ⇒ `moved` ⇒ Folge-Auftrag `failed/conflict`, fremder Commit bleibt; Korrektur-Auftrag (`pr_id`, `carry_from`) setzt auf dem neuen Stand fort und pusht obendrauf.
6. **Modus Branch:** Branch hochgeladen, kein PR, keine Host-API-Anfrage, Revert 409; Modus PR ohne Remote ⇒ 400 mit Text.
7. **Mehrmandanten/Rollen:** Fremde 404 auf PR, Liste, Poll, Delivery; Betrachter lesen, 403 beim Steuern; Bearbeiter steuern, 403 auf Delivery und Verbindungsprüfung; Bearbeiter kann die Remote-URL nicht mehr ändern; Host-Wechsel löscht das Token; Auto-Fix aus ⇒ `ignored_disabled`, wieder an ⇒ Folge-Auftrag.
8. **Verbindungsprüfung, Rate-Limit, falsches Token:** Prüfung zeigt das Konto; 429 mit `Retry-After: 5` ⇒ `rate_until`, „Jetzt prüfen“ springt nicht darüber, keine Anfrage während der Pause, Erholung danach; falsches Token ⇒ lesbares „Zugang abgelehnt“, Backoff, Token nirgends.
9. **GitHub ohne Entwürfe:** 422 auf Draft ⇒ normaler PR.

Angepasst (WP04, nicht abgeschwächt): `branches.rs` wartet auf den fertig vorbereiteten Arbeitsbaum bzw. auf das Löschen des Branches, bevor dieselben Endzustände geprüft werden (Rennen im Test, siehe Review B13).

### System/E2E (Playwright, Fake-Host als `examples/fake_git_host` im Stack)
- `system/pull_requests.spec.ts` (2): PR anlegen mit Label/Draft, rote CI ⇒ Folge-Auftrag ⇒ Fast-Forward auf denselben Branch, kein Token in Antworten, nie Approve/Merge; Kommentar ohne Schreibrecht ignoriert, Folge-Auftrag ohne fremden Text, Loop-Grenze, „weitere Runde“.
- `ui/pull_requests.spec.ts` (3, Desktop): Eigentümer stellt „Pull Request“ in Klartext ein (Erklärung „Wer lädt wann hoch?“, Token-Rechte je Host, Labels, Runden), Reload, „Verbindung prüfen“ zeigt Konto, kein Token auf der Seite, axe; Drawer mit PR-Link (https), „Entwurf“, „Checks rot“, Check-Namen, Runden-Text, „Eine weitere Runde erlauben“ ⇒ Folge-Auftrag, Verlauf-Karte „Pull Requests“, axe; Bearbeiter sieht „Ergebnis“ und Git-Felder nur lesend, axe.
- `ui/pull_requests-mobile.spec.ts` (iPhone hoch/quer): Auswahl ≥ 44 pt, 16-px-Schrift, innerhalb der Safe Areas, kein horizontales Scrollen, PR-Block im Drawer, axe.
- Tour (a11y Desktop/iPhone, hell/dunkel): neuer Reiter „Ergebnis“.

## 3. Während der Tests gefundene Fehler

| Fund | Ursache | Fix |
|---|---|---|
| Gitea: zweite Runde ohne Folge-Auftrag | bekannte Reviews wurden nie neu gelesen | Review B6 |
| `branches.rs` rot (1 von ~6 Läufen, auch auf `4a8da81`) | verlorenes Abbrechen nach Wiederholen; Testrennen | Review B4, B13 |
| Prompt-Test | Regeln zitieren die Grenze, Test suchte die erste Fundstelle | Test prüft die Grenzzeilen |
| E2E strict mode | „Git-Server“ trifft auch Hilfe-Button/Radiotext; WP04-Hinweistext doppelt | exakte Labels; Hinweis umformuliert |

## 4. Nicht automatisiert geprüft

- Echte GitHub-, GitLab-, Gitea/Forgejo-Server (Netz-Proxy sperrt sie; nur Fakes nach belegten Formaten, Rest markiert in `WP05-impl.md` §4).
- Rootless Podman/Docker (alle Tests Engine `none`).
- „Nicht hochladen“ im Branch-/PR-Modus (Review N9).
