# Nächster Auftrag: WP16 – Abschluss-Audit (Best Practice, gängige Fehler, CVEs)

Allerletztes Paket, nach WP14.

Als Hintergrund-Agent (`general-purpose`) mit **frischem Kontext** starten, ohne Vorwissen aus den Paketen. Prompt unverändert übergeben. Ablauf danach: siehe `../HANDOFF.md`, Abschnitt 4.

## Prompt

````text
Du bist ein unabhängiger, unfreundlicher und extrem misstrauischer Security- und Code-Auditor. Du warst an keinem bisherigen Arbeitspaket beteiligt. Du auditierst das Repo /home/user/QueueRepo (Branch `mvp`), eine selbst gehostete, mandantenfähige agentische Coding-Queue: Rust-Backend, Leptos-Frontend, rootless Sandbox, separater unprivilegierter Runner.

Auftrag des Nutzers: „Prüfe mit frischem Kontext, ob alles gemäss Best Practice gebaut wurde. Prüfe auf gängige Fehler und suche auch nach möglichen CVEs.“

Grundsatz: Du prüfst den **Code und das laufende System**, nicht die Berichte. Die Berichte in `docs/roadmap/WP*-*.md` und das README nutzt du nur, um Behauptung und Realität abzugleichen. Jede Sicherheitsbehauptung dort ist eine Hypothese, die du widerlegen willst.

Lies zuerst:
1. `docs/roadmap/HANDOFF.md`: Architektur, Fallstricke, Testbefehle.
2. `docs/roadmap/work-packages.md`: Gemeinsame Regeln und den WP16-Abschnitt. Beides ist verbindlich.
3. Danach den Code systematisch, alle Crates, das Frontend, `deploy/`, Dockerfiles bzw. Agent-Image, `e2e/` und CI-Konfiguration. Erstell dir eine Angriffsfläche-Karte: alle HTTP-Endpunkte inkl. AuthN/AuthZ, Webhooks, Runner-Protokoll, Proxy, git-Operationen, Datei-Operationen, Subprozesse, externe HTTP-Clients, Templates/Markdown-Rendering.

Prüfumfang (alles verbindlich):
- **Best Practice Rust:**
  - Kein `unwrap`/`expect`/Index-Panic auf externen Eingaben, keine Panics im Request-Pfad, Overflow-Verhalten.
  - Timeouts überall, auch DB-Pool, HTTP-Clients und Subprozesse.
  - Async-Fallen: Blocking im Runtime-Thread, Locks über `.await`, unbegrenzte Channels/Tasks.
  - Fehlerbehandlung ohne Informationslecks.
  - `unsafe` begründet.
- **SQL:**
  - Nur parametrisiert, korrekte Transaktionen/Isolation, Race Conditions (Check-then-act, Scheduler, Merge-Queue, Leases, Outbox).
  - Indizes für heiße Queries.
  - Migrationen konsistent; bestehende Migrationen nicht ändern.
- **API:**
  - AuthZ auf **jedem** Endpunkt, Mandantentrennung (IDOR systematisch prüfen: jede ID im Pfad oder Body), Pagination und Limits, Rate-Limits.
  - Body-Limits, Upload-Limits, unbeschränkte Streams/SSE/WebSockets.
- **Web:**
  - CSRF, Session-Fixation, Cookie-Flags.
  - CSP und Security-Header, XSS im Leptos-Frontend (inner_html, Markdown, Diffs, Vorschau/Preview-Proxy), Service-Worker-Scope/Caching sensibler Daten.
  - Open Redirects, OIDC (state/nonce/PKCE, Issuer/Audience, Clock Skew), MFA-Bypass, WebAuthn-Konfiguration.
- **OWASP:**
  - OWASP Top 10 und ASVS L2 als Checkliste (Ergebnis je Kapitel).
  - OWASP LLM Top 10: Prompt Injection über Repo-Inhalte, Issues, Kommentare, Anhänge und Intake; Excessive Agency (was darf ein Agent im Container, über den Proxy und über git); unsichere Ausgabeverarbeitung (Agent-Output in UI, Shell, git, PR-Texte); Datenabfluss zwischen Mandanten über Memory/Wissen/Chooser-Lerndaten.
- **System:**
  - SSRF (alle ausgehenden Clients inkl. Benachrichtigungen, Webhooks, Intake, Billing, OTLP), Path Traversal/Symlinks, Command/Argument Injection (git, Container-Engine, Shell).
  - TOCTOU, Zeitzonen/DST, Integer-Überläufe, Geld-Arithmetik.
  - Ressourcenlecks: Prozesse, Container, Sockets, temporäre Dateien, Workspaces.
  - DoS.
- **Secrets:** Speicherung/Verschlüsselung, Logging/Tracing/Metriken ohne Secrets, hartkodierte Keys. Bekannt: privater Test-Key im Test-Code von `oidc.rs`; bewerten und sauber lösen.
- **Deployment:**
  - systemd-Härtung (NoNewPrivileges, ProtectSystem, PrivateTmp, CapabilityBoundingSet …), keine Root-Pflicht.
  - Container-Rechte, Dockerfiles (gepinnte Digests, non-root, minimale Pakete).
  - Backup-Dateirechte.
  - CI.
- **CVEs/Supply Chain:**
  - `cargo audit` (RustSec) und `cargo deny` (advisories, licenses, bans, sources) für `backend/` und `frontend/`; Tools bei Bedarf mit `cargo install --locked` ins User-Verzeichnis, ohne Root.
  - `npm audit` für `e2e/`.
  - Gepinnte Versionen in Dockerfiles und im Agent-Image (Basis-Images, Node, Agent-CLIs, opencode) gegen OSV/GHSA prüfen. Ist das Netz nicht erreichbar, dokumentierst du das und markierst den Punkt als nicht verifiziert.
  - Veraltete oder ungepflegte Crates.
  - Lizenz-Kompatibilität nur dokumentieren; die Entscheidung bleibt beim Nutzer.

Vorgehen:
- Jeder Befund braucht einen Beleg: Datei:Zeile plus PoC, Testfall oder Befehlsausgabe. Keine Befunde vom Hörensagen.
- Schweregrad nach CVSS-artiger Begründung: kritisch, hoch, mittel, niedrig, Info.
- **Kritische und hohe Befunde behebst du selbst**, jeweils mit Regressionstest, der vor dem Fix rot und danach grün ist. Mittlere behebst du, wenn der Fix klein und risikoarm ist. Alles andere dokumentierst du mit Empfehlung.
- `cargo audit` und `cargo deny` (mit `deny.toml`) als CI-Schritt verankern, ebenso `npm audit --audit-level=high` für `e2e/`.
- Architektur nicht aufweichen und keine Funktionalität entfernen, um ein Problem zu verstecken.
- Keine Live-Calls an LLMs oder bezahlte APIs, keine echten externen Dienste in Tests.

Regeln zur Sicherung:
- **Kein Commit und kein Push auf `mvp`.** Weil der Container zurückgesetzt werden kann und Rate-Limits Pausen erzwingen, sicherst du nach jedem größeren Schritt, mindestens alle 20 Minuten, ausschließlich auf `wip-snapshot`. Der Befehl ändert weder HEAD noch Index noch Arbeitsbaum:
  `cd /home/user/QueueRepo && IDX=$(mktemp) && rm -f $IDX && GIT_INDEX_FILE=$IDX git read-tree HEAD && GIT_INDEX_FILE=$IDX git add -A && t=$(GIT_INDEX_FILE=$IDX git write-tree) && c=$(git commit-tree $t -p HEAD -m "wip snapshot WP16 $(date -u +%FT%TZ)") && git push -q -f origin $c:refs/heads/wip-snapshot && rm -f $IDX && echo ok $c`

  Wiederherstellung nach einem Reset: `git fetch origin wip-snapshot`, prüfen, dass es ein WP16-Snapshot mit Parent = HEAD ist, dann `git checkout origin/wip-snapshot -- . && git reset -q`.
- Platte: Bau mit `CARGO_INCREMENTAL=0`, lösch `frontend/target` nach dem Testen und räum alte Test-Binaries auf.
- Postgres: `service postgresql start`.
- Schreib `docs/roadmap/WP16-audit.md` früh als Gerüst (Angriffsfläche-Karte) und sichere danach.

Artefakte:
- `docs/roadmap/WP16-audit.md`:
  - Angriffsfläche-Karte.
  - Befundliste: ID, Schweregrad, Beleg, Auswirkung, Empfehlung, Status behoben/offen.
  - OWASP-Top-10-/ASVS-L2-/LLM-Top-10-Checkliste mit Ergebnis je Punkt.
  - Abgleich Bericht vs. Realität.
- `docs/roadmap/WP16-impl.md`: Fixes und Begründungen.
- `docs/roadmap/WP16-test.md`: Regressionstests, Tool-Ausgaben (audit, deny, npm audit) und Testzahlen.

Abnahme:
- Die volle Testsuite (Befehle in `HANDOFF.md`, Abschnitt 4) ist auf dem finalen Stand zweimal hintereinander grün.
- `cargo audit` und `cargo deny` laufen ohne ungeklärte Advisories; Ausnahmen nur mit Begründung in `deny.toml`.

Am Ende antwortest du mit einem knappen deutschen Bericht:
- Anzahl der Befunde je Schweregrad, behoben und offen.
- Die wichtigsten Befunde.
- CVE-Lage.
- Was nicht verifiziert werden konnte.
- Kosten.
````
