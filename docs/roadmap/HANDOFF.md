# Übergabe: Koordination der Queue-Entwicklung

Dieses Dokument erlaubt es einem anderen Agenten oder Menschen, die Arbeit nahtlos fortzuführen.
Es beschreibt Auftrag, Stand, Arbeitsweise, Fallstricke und die nächsten Schritte.

- **Stand:** 2026-11-22
- **Branch:** `mvp`

## 1. Auftrag des Nutzers (Produktvision, chronologisch)

Die Zitate sind sinngemäss aus den Nutzer-Nachrichten übernommen. Sie gelten weiterhin.

1. **Grundidee:** Ein Werkzeug, in das man Coding-Arbeiten eingibt.
   - Aufträge werden in eine Queue gestellt und nacheinander abgearbeitet.
   - Ist die Queue leer, laufen die **OnEmptyQueue**-Aktionen. Sie dürfen die Queue wieder befüllen.
   - Voll agentisch, mit maximal effizientem Umgang mit Tokens und Token-Budget.
   - Benutzerfreundlichkeit ist wichtig.
2. **Agenten:** Jeder Auftrag bekommt einen **neuen Agenten**, der ihn korrekt abarbeitet. Kontext wird aufgebaut oder mitgegeben.
   - Übergabe vom Vorgänger-Auftrag; bei einem neuen Auftrag kommt der Kontext mit dem Auftrag.
3. **OnEmptyQueue:**
   - Mögliche Aktionen: Reviews, Internet-Recherche zu Neuem, Analyse der Arbeit, Verbesserungsvorschläge.
   - Braucht es Input, wird der User gefragt; dafür gibt es Benachrichtigungen.
   - Kommen mehr als 2 neue Aufträge zusammen, wird der User gefragt.
4. **Parallelität:** standardmässig 1 (seriell), vom User erhöhbar.
5. **Definition of Done:**
   - Verify-Kommando plus Review-Agent.
   - Bei Fehler wird das Problem analysiert und ein Fix-Auftrag in die Queue gestellt.
6. **Mandanten:**
   - Mehrbenutzerfähig mit lokalen Usern und IdP (OIDC).
   - User sind komplett getrennte Tenants, dürfen aber gemeinsam an Projekten arbeiten.
   - Pro User genau eine Queue.
   - **Jeder User nutzt sein eigenes Abo** bzw. seinen eigenen Key.
7. **Stack:** Rust, Postgres. Das GUI ist inzwischen Rust/Leptos; das ursprüngliche Vite/React wurde in WP00 ersetzt.
8. **iPhone:** Notch und Dynamic Island beachten (Safe Areas). Saubere Unit-, Integrations-, System- und E2E-Tests.
9. **Projekte:** Ein Projekt hat immer git; mehrere Projekte pro User sind möglich.
10. **Kein Root auf dem Host.** Ein separates Tool (`queued-runner`) erledigt die privilegierte Arbeit; die Queue-App triggert es nur.
11. **Token-Limits:** für Claude, Gemini und GPT implementiert.
12. **Konkurrenzfunktionen:** alle umsetzen (WP01–WP12).
    - Pro Paket ein Agent mit frischem Kontext für Planen, Umsetzen, Testen und Überprüfen.
    - Am Ende auf den Branch pushen.
    - **Jeder Agent verhält sich wie ein unfreundlicher Entwickler.**
13. **Lizenz-/ToS-Thema:** vorerst ignorieren; der Nutzer kommt darauf zurück. In WP14 wird es als Entscheidungspunkt zwingend.
14. **GUI:** auf Rust umstellen (WP00), benutzerfreundlich und optisch ansprechend. Ein normaler Mensch soll es schön und einfach finden; das Design darf modern sein.
15. **Kostenoptimierte Connectoren** bauen (WP13). Grundlage: `reports/Kostenoptimale Modellanbieter für Queue.md`.
16. Einen **richtig schlauen Modell-Chooser** bauen (WP15).
17. **„Ganz am Schluss“:** Das Produkt so anbieten, dass Kunden bezahlen müssen (WP14).
18. **Allerletzter Schritt:** Mit **frischem Kontext** prüfen, ob alles nach Best Practice gebaut ist; gängige Fehler und mögliche CVEs suchen (WP16).
19. **Dokumentation:** Alle Aufträge detailliert im Repo ablegen, damit ein anderer Agent weitermachen kann (dieses Dokument und `agent-prompts/`).

## 2. Feste Regeln für den Koordinator

- **Push:** nur auf `mvp`. **Keinen PR erstellen**, solange der Nutzer das nicht verlangt.
- **Commits:** keine Modell-Bezeichnungen in Commits, Code oder Doku. Commit-Nachrichten enden mit den Attribution-Zeilen aus der Session, etwa `Co-Authored-By: …` und `Claude-Session: …`.
- **Datenschutz:** persönliche Daten des Nutzers (E-Mail, Name) nicht in Repo oder externe Requests.
- **Paket-Agenten** committen und pushen nie. Das macht ausschliesslich der Koordinator nach der Abnahme.
- **Keine Live-Calls an bezahlte LLM-APIs** durch Agenten. Einmal entstanden in WP06 ca. 0.40 USD durch Claude-Code-Fixtures; das wurde dem Nutzer gemeldet. Kosten gehören immer in den Bericht.
- **Kommunikation:** Die Antworten an den Nutzer sind deutsch, knapp und technisch; der Nutzer ist IT-Architekt.

## 3. Stand der Arbeitspakete

Plan, Details und Reihenfolge stehen in `work-packages.md`. Die Artefakte liegen pro Paket in `docs/roadmap/WPxx-{plan,impl,test,review}.md`.

| Reihenfolge | Paket | Status | Commit |
|---|---|---|---|
| 1 | MVP | fertig | (frühe Commits) |
| 2 | WP01 Strukturierte CLI-Integration | fertig | (vor WP00) |
| 3 | WP00 GUI → Rust/Leptos + Redesign | fertig | `06dd9c2` |
| 4 | WP13 Provider-Connectoren & Kosten | fertig | `5a30c61` |
| 5 | WP02 Environments & Egress | fertig | `ae37b03` |
| 6 | WP03 Gate & Supply Chain | fertig | `87b1ba6` |
| 7 | WP04 Branch pro Task & Merge-Queue | fertig | `4a8da81` |
| 8 | WP05 PR-Workflow | fertig | `45e1f30` |
| 9 | WP06 Plan-Modus & Steering | fertig | `f4cb172` |
| 10 | WP07 Trigger, API, CLI | fertig | `04d7457` |
| 11 | WP08 Intake | fertig | `4eff80a` |
| 12 | WP09 Memory, Dedupe, Anhänge | fertig | `2b936f7` |
| 13 | WP10 Observability & Benachrichtigungen | fertig | `5543b95` |
| 14 | WP11 Enterprise & Betrieb | fertig (Migration 0014) | `389f657` |
| 15 | **WP12 Best-of-N, Preview, Routing** | **in Arbeit** | – |
| 16 | WP15 Modell-Chooser | offen | – |
| 17 | WP14 Bezahlzugang | offen | – |
| 18 | WP16 Abschluss-Audit | offen (allerletztes Paket) | – |

Die Recherche-Berichte, auf die sich die Pakete stützen, liegen in `reports/`. Die Recherche-Notizen liegen in `research_notes/`.

## 4. Arbeitsweise pro Paket (Ablauf des Koordinators)

1. **Agent starten:** Ein Hintergrund-Agent (`general-purpose`) bekommt den Prompt aus `agent-prompts/`.
   - Die bisherigen Pakete haben ihren wörtlichen Prompt in `agent-prompts/NN-*.md`.
   - Für die offenen Pakete liegen fertige Prompts in `agent-prompts/next-WPxx-*.md`.
2. **Prompt-Muster (immer gleich):**
   - **Haltung:** unfreundlicher, extrem kritischer Senior-Entwickler.
   - **Pflichtlektüre:** Gemeinsame Regeln, WP-Abschnitt, Recherche-Bericht und **alle** bisherigen WP-Berichte. Offene Funde früherer Pakete gehören dazu, weil sie oft ins nächste Paket fallen.
   - **Harte Regeln:** fachliche Pflichtanforderungen, kein Root, Sicherheitsmodell nicht aufweichen, keine externen Calls in Tests, keine Live-LLM-Calls, Lizenzthema ausgeklammert.
   - **Kein Commit und kein Push**, nur Selbst-Sicherung auf `wip-snapshot` (siehe 5.).
   - **Platz- und Postgres-Hinweise.**
   - **Artefakte:** Plan, Impl, Test, Review und README.
   - **Abnahme:** volle Suite zweimal hintereinander grün, dazu Pflicht-Tests und mutative Gegenprobe sicherheitskritischer Tests.
   - **UI:** WP00-Designsystem, iPhone Safe Areas, axe grün.
   - **Abschluss:** deutscher Kurzbericht.
3. **Nach der Rückmeldung des Agenten:**
   - `git status` prüfen und die Artefakte kontrollieren (`ls docs/roadmap/WPxx-*`).
   - Committen mit `feat: WPxx <Titel>` und Stichpunkten im Body, dann `git push origin mvp`.
4. **Bericht an den Nutzer:** deutsche Zusammenfassung mit Testzahlen, Umfang, Verhaltensänderungen, offenen Punkten und Nicht-Verifiziertem.
5. **Nächstes Paket:** sofort den nächsten Agenten starten.

### Abnahme-Suite (laut `work-packages.md`, Gemeinsame Regeln)

```bash
service postgresql start
export DATABASE_URL=postgres://queued:queued@localhost/queued CARGO_INCREMENTAL=0
cd backend && cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd ../frontend && cargo fmt --all -- --check && cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings \
  && cargo clippy --all-targets -- -D warnings && cargo test && trunk build --release
cd ../e2e && npx playwright test   # Projekte system, desktop, iphone, iphone-landscape (inkl. axe)
```

Referenzwerte nach WP11:

| Suite | Tests |
|---|---|
| Backend | 609 |
| Frontend | 66 |
| Playwright | 119 |

Die Suite nie per `&` aus einem Skript starten. Ein so gestarteter Prozess erbt SIGINT als ignoriert, und die Trap-Tests schlagen dann falsch fehl. Stattdessen im Vordergrund oder über einen Wrapper starten.

## 5. Betriebliche Fallstricke (unbedingt lesen)

- **Container-Resets** löschen uncommittete Arbeit. Das ist mehrfach passiert (WP13, WP03, WP05 zweimal, WP08). Deshalb gilt:
  - Agenten sichern mindestens alle 20 Minuten auf den Branch `wip-snapshot`. HEAD, Index und Arbeitsbaum bleiben dabei unberührt:
    ```bash
    cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD \
      && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) \
      && c=$(git commit-tree $t -p HEAD -m "wip snapshot WPxx $(date -u +%FT%TZ)") \
      && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c
    ```
  - **Wiederherstellung:** `git fetch origin wip-snapshot`.
    - Prüfen, dass der Snapshot vom richtigen WP stammt und der Parent gleich HEAD ist.
    - Dann `git checkout origin/wip-snapshot -- . && git reset -q`.
  - Bricht ein Agent ab (Rate-Limit, Reset), macht der Koordinator einen **WIP-Commit auf `mvp`** (`wip(wpxx): … (in progress)`) und pusht. Der Agent erfährt beim Fortsetzen, dass HEAD sich geändert hat.
- **Rate-Limits** (5-Stunden- und Wochenlimit) beenden Agenten. Fortsetzen nach der Reset-Zeit per `SendMessage` an die Agent-ID. Ist die ID verloren, einen neuen Agenten mit dem Paket-Prompt starten und ergänzen:
  - Der Stand liegt im WIP-Commit.
  - Vorhandene Artefakte zuerst lesen.
  - Nichts neu beginnen, was schon da ist.
- **Platte:** Das Kontingent ist fix. `backend/target` wächst auf über 18 GB, weil sich alte Test-Binaries ansammeln.
  - `frontend/target` löschen.
  - `backend/target/debug/incremental` löschen.
  - Doppelte Test-Binaries in `backend/target/debug/deps` löschen; das jeweils neueste behalten.
  - Mit `CARGO_INCREMENTAL=0` bauen.
  - Eine volle Platte erzeugt falsche E2E-Fehler.
- **Postgres** ist nach einem Reset oft gestoppt: `service postgresql start`.
- **Externe APIs** (GitHub, GitLab, Linear, Slack, Jira, Stripe, LLMs) sind nicht live erreichbar oder nicht erlaubt. Getestet wird nur gegen eigene Fake-Server im Test.
- **Docker** ist ggf. mit `dockerd &` startbar, Podman fehlt; in Tests ist die Sandbox-Engine `none`.

## 6. Architektur-Kurzüberblick

- **`backend/` (Rust-Workspace):**
  - `queued` (App, axum 0.8, sqlx 0.8, ohne Host-Rechte).
  - `queued-runner`: unprivilegiert; erledigt Datei-, Prozess-, Git- und Container-Arbeit.
  - `queued-common`: Protokoll, Scanner, Metrik-Registry.
  - `queued-api`: Wire-Typen und Texte, geteilt mit dem Frontend.
  - `queuectl`: CLI.
- **Migrationen:** `backend/crates/app/migrations/00NN_*.sql`, aktuell bis 0014. **Bestehende Migrationen nie ändern.**
- **`frontend/`:** Leptos 0.8 CSR (`queued-web`), Build mit Trunk.
- **`e2e/`:** Playwright inkl. axe; `scripts/stack.sh` startet den kompletten Stack.
- **Sicherheitsmodell:**
  - Rootless-Container mit `--network none` plus Egress-Proxy pro Lauf.
  - SSRF-sicherer HTTP-Client.
  - Gehärtetes Host-git.
  - Secret-Scanner mit Freigabe-Status.
  - Pro Task ein separates Repo (kein git worktree) und eine serielle Merge-Queue.
- **Bekannte Altlast für WP16:** Im Test-Code von `oidc.rs` steht ein hartkodierter privater Test-Key. Die übrigen offenen Funde stehen in den `WPxx-review.md`.

## 7. Nächste Schritte (konkret)

1. **WP12** läuft mit `agent-prompts/next-WP12-best-of-n-preview-routing.md`.
2. **WP15** mit `agent-prompts/next-WP15-model-chooser.md`.
3. **WP14** mit `agent-prompts/next-WP14-paid-access.md`.
   - Das Lizenz- und ToS-Thema wird hier als Entscheidungspunkt für den Betreiber dokumentiert, nicht entschieden.
4. **WP16** mit `agent-prompts/next-WP16-final-audit.md`.
   - Läuft mit frischem Kontext, ohne Vorwissen aus den Paketen.
   - Zusätzlich bekannt: Das Docker-Image hat `pg_dump` 15, die DB ist 16 (WP11-review).
5. **Abschluss:** Gesamtbericht an den Nutzer. Danach ggf. die offenen Themen ansprechen:
   - Lizenz- und Credential-Modell.
   - Funde, die nur dokumentiert und nicht behoben sind.
