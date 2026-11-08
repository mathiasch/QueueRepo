# WP02 – Review (Selbst-Review, harte Lesart)

Gegenstand: kompletter WP02-Diff auf `mvp` (common, runner, api, app, Migration 0005, Frontend, E2E, Doku). Maßstab: `work-packages.md` (Gemeinsame Regeln, WP02), Auftrag des Koordinators (kein Host-Root, Sicherheitsmodell nur härten, Engine `none` ehrlich kennzeichnen), Report P0.3/P0.4/P1.5, WP13-N8. Jede Feststellung ist am Code oder an einem Lauf geprüft.

## Urteil

**Abnahmefähig mit dokumentierten Restrisiken.** Alle blockierenden Funde sind behoben und durch Tests belegt; die Suite ist auf dem finalen Stand zweimal grün (`WP02-test.md`). Das Sicherheitsmodell ist strenger als vorher: Container haben kein eigenes Netz mehr, `--cap-drop ALL`, `no-new-privileges`, Limits und „Secrets nie in argv“ sind unverändert (Unit-Test prüft argv), der Runner bleibt unprivilegiert. Die ehrliche Grenze: Der Proxy filtert nach Hostnamen. Wer eine Domain erlaubt, erlaubt Abfluss dorthin. Und die Durchsetzung ist mit Docker real geprüft, mit rootless Podman nicht.

## Akzeptanz gegen WP02

| Anforderung | Status | Beleg |
|---|---|---|
| Environment pro Projekt: Setup-Skript, Variablen, Secrets (verschlüsselt, nur Setup), Policy `off/allowlist/open`, Presets, GET/HEAD/OPTIONS | erfüllt | `environments.rs` (8 Tests), `queued-api::env`-Tests, E2E Desktop |
| Zwei Phasen: Setup (Setup-Netz, Secrets) → Agent (ohne Secrets, Agent-Netz) | erfüllt | `setup_runs_first_and_only_it_sees_the_secrets`, System-Test |
| Allowlist-Proxy pro Lauf, Container ohne direkten Netzzugang | erfüllt (Docker real, Podman nur per Argument-Test) | `egress::tests` (11), `containers_have_no_network…`, Docker-Gegenprobe |
| Link-Local/Metadata, private Netze blockiert | erfüllt, zusätzlich eigene Host-Adressen | `blocks_internal_addresses_even_when_allowlisted`, `address_filter_layers` |
| Proxy-Log als Task-Events | erfüllt | `egress_decisions_of_every_phase_become_task_events`, Drawer-E2E |
| Cache pro Projekt, Invalidierung bei Setup-Skript/Lockfiles | erfüllt (+ Variablen, Setup-Netz, Image) | `dependency_cache_is_reused_and_invalidated`, `setup::tests` |
| UI: Environment-Editor | erfüllt, axe hell/dunkel Desktop + iPhone, Safe-Areas | Tour, `environment-mobile.spec.ts` |
| Engine `none` sichtbar „nicht durchgesetzt“ | erfüllt | Banner, Event-Suffix, `/environment.sandbox`, System-Test |
| WP13-N8 DNS-Rebinding | behoben (+ Redirects) | `llm::tests::guarded_client_*` |

## Blockierende Funde (alle behoben)

| # | Fund | Wie gefunden | Behebung | Beleg |
|---|---|---|---|---|
| B1 | **Schlüsseldiebstahl über Projektvariablen.** Variablen gelten auch im Agentenlauf, und der läuft mit den Zugängen der Person, die den Auftrag anlegt (z. B. eines Bearbeiters). Die Eigentümerin hätte `ANTHROPIC_BASE_URL=https://ihr-server` setzen und den Host im Agent-Netz freigeben können. Der API-Schlüssel des Bearbeiters wäre dann an sie gegangen. | Review der Rechte-/Datenflüsse | Projektvariablen mit Anbieter-/CLI-Präfixen (`ANTHROPIC_`, `OPENAI_`, `CODEX_`, `CLAUDE_`, `GEMINI_`, `GOOGLE_`, `AWS_`, `AZURE_`, `XDG_`, `OPENCODE_` …) sind gesperrt, in Backend und Frontend über dieselbe Funktion. Secrets bleiben frei, weil sie nur im Setup ohne Modell-Zugang existieren. | `env::tests::secret_names_and_values`, Integration (400 für `ANTHROPIC_BASE_URL`) |
| B2 | **Geleakte Secret-Dateien blieben liegen.** Erste Fassung: Leck ⇒ blockiert. Die Datei blieb aber im Workspace; das nächste `begin` committet sie als WIP (bei `auto_push` gepusht), ein späterer Agent liest sie. | Durchdenken des Folgeauftrags | `setup::scrub`: untracked löschen, getrackte auf HEAD zurücksetzen, `.git/*` zeilenweise bereinigen, betroffenen Cache verwerfen; Meldung nennt die Aktion | `scrub_removes_untracked_restores_tracked_and_cleans_git_config`, `secret_written_to_a_file_blocks_and_is_scrubbed`, `secrets_are_redacted_and_leaks_reported` |
| B3 | **LAN-Freigabe wirkungslos für Dienste auf dem eigenen Host.** Erkannte Host-Adressen (inkl. 127.0.0.1) lagen in derselben Sperrliste wie `RUNNER_EGRESS_DENY` und wurden vor `RUNNER_EGRESS_PRIVATE_HOSTS` geprüft. | Code-Review vor dem ersten Test | getrennte Liste `own`, Reihenfolge: immer gesperrt → Betreiber-DENY → LAN-Ausnahme → eigene Adressen/private Netze | `address_filter_layers` |
| B4 | **SSRF-Lücken der App:** DNS-Rebinding (WP13-N8) und **ungeprüft gefolgte Redirects** (reqwest-Standard, bis zu 10 Sprünge, auch zu `169.254.169.254`). | Auftrag + Lesen von `llm::client` | `GuardedResolver` (Filter zur Verbindungszeit), `redirect_policy`; alle App-Clients für Nutzer-URLs (direkt, Batch, Verbindungsprüfung, LiteLLM) | `guarded_client_refuses_internal_addresses_at_connect_time`, `guarded_client_checks_redirect_targets` |
| B5 | **Schutzstatus an Anonyme:** `sandbox` stand im öffentlichen `/api/meta`. | Review der API-Oberfläche | entfernt; nur `/projects/{id}/environment` (Mitglieder) | Integration + System-Test prüfen das Fehlen |
| B6 | **Veralteter Schutzstatus:** Runner-Info war für die ganze App-Laufzeit gecacht. | Review | TTL 30 s | Code (kein eigener Test: Zeitverhalten trivial, `sandbox()` von allen Tests genutzt) |
| B7 | **UI:** Hilfe-Button lag innerhalb des `<label>` des Schalters „Nur lesen“ (Klick auf Hilfe schaltet um); `fieldset disabled` hätte für Lesende auch die Hilfe-Buttons gesperrt; Netz-Abschnitte hatten eine kleinere Überschriftenstufe als die übrigen (Screenshot). | Code- und Screenshot-Review | Hilfe neben dem Label, kein `fieldset disabled` (Felder einzeln), Abschnitte als `section` mit `h2` | Tour/axe, Screenshots |

## Nicht blockierende Funde (offen, bewusst)

| # | Fund | Bewertung |
|---|---|---|
| N1 | **Secrets im Setup sind für Repository-Code erreichbar** (z. B. `preinstall`), und das Repository wird von früheren Agenten verändert. Ein Agent kann Code für das nächste Setup platzieren, der die Secrets an eine *erlaubte* Adresse schickt. | Grundsätzlich, ohne Review jedes Agenten-Commits vor dem Setup nicht lösbar. In UI (Warnkasten bei Geheimnissen) und README erklärt; Gegenmittel: enge Setup-Allowlist, minimal berechtigte Tokens. Gilt sinngemäß für Bearbeiter, die Aufträge anlegen (sie schreiben schreibend ins Repo). |
| N2 | **Hostname-Proxy ohne Inhaltsprüfung** (außer „Nur lesen“): Abfluss an erlaubte Domains, Domain-Fronting über CDNs; im Klartext-HTTP-Modus werden Folge-Requests derselben Verbindung roh weitergereicht (gleiche Ziel-IP). `open` erlaubt jeden Port und löst beliebige Namen auf (DNS als Kanal). | Wie bei Claudes Sandbox (Report). Dokumentiert. |
| N3 | **Nicht live verifiziert:** Modell-API-Hosts je Zugang, Proxy-Unterstützung der echten CLIs, Preset-Domains, Agent-Image (nicht gebaut). | Ein fehlender Host zeigt sich sofort als „Blockiert (Agent): …“ im Protokoll; Abhilfe ist dann eine Codezeile in `agent::egress_hosts`. |
| N4 | **Rootless Podman und SELinux nicht getestet.** Real geprüft: Docker 29 (rootful Daemon) mit `--network none`, ro-Socket-Mount, Node-Brücke, Cache-Mount, Redaktion. Podman nur über die Argumentliste. | Rootless Podman mappt Container-root auf die Runner-UID, der Socket (0600, Runner-UID) ist damit erreichbar. Das ist plausibel, aber unbewiesen. Ein Image mit eigenem `USER` hätte kein Netz (fail-closed). |
| N5 | **Leck-Suche:** nur wörtliche Werte (kein base64/URL-Encoding), Dateien ≤ 8 MiB, Obergrenzen 300 000 Einträge / 4 GiB pro Lauf; bei großen `node_modules` kostet sie Sekunden pro Setup mit Secrets. Im Fehlerfall wird ein bereits bestehender Cache nicht durchsucht (er ist nur dem Setup sichtbar). | Dokumentiert; Abbruch an den Grenzen erscheint als Hinweis im Protokoll. |
| N6 | **Engine `none`** setzt nichts durch. | Gewollt (Entwicklung), überall sichtbar gekennzeichnet. |
| N7 | **„Nur lesen“ für HTTPS** bricht Programme mit Zertifikat-Pinning und HTTP/2-only-Clients; Upstream ohne HTTP/1.1 scheitert. | Dokumentiert in UI-Hilfe und README. |
| N8 | Mit `ALLOW_PRIVATE_LLM_ENDPOINTS=true` könnte ein Rebinding auf den **Port** des Runners zielen (der Resolver sieht keine Ports). | Der Runner verlangt sein Bearer-Token, die App schickt dort nur Provider-Schlüssel. Restrisiko gering. |
| N9 | **Verhaltensänderung:** Agenten ohne Internet außer Modell-API; `RUNNER_NETWORK` beendet den Start; LAN-Modellserver (opencode/Codex mit Basis-URL im LAN) brauchen `RUNNER_EGRESS_PRIVATE_HOSTS`. | Ziel des Pakets (Default-Deny). Upgrade-Hinweis im README. |
| N10 | **Kein Plattenlimit für Caches.** Alte Schlüssel eines Projekts werden gelöscht, ein einzelner Cache kann aber beliebig wachsen. | Betreiberaufgabe (Monitoring), „Cache leeren“ vorhanden. |
| N11 | Erlaubte Requests innerhalb eines Tunnels werden nicht einzeln protokolliert (nur Verbindungen; im Nur-Lesen-Modus jede Ablehnung). Mehr als 100 Ablehnungen pro Lauf werden nur gezählt. | Bewusst gegen Event-Fluten. |
| N12 | Kein Container-Snapshot nach dem Setup; das Setup läuft bei jedem Auftrag (mit warmem Cache). | Plan §10.7. |
| N13 | Fehlt `node` im Image, wartet der Wrapper 10 s und startet dann ohne Netz. | fail-closed, dokumentiert. |
| N14 | Die Leck-Bereinigung setzt einen *getrackten* Treffer auf HEAD zurück. Steht das Secret schon in HEAD, bleibt es dort; das meldet die Queue nicht gesondert. | Das Secret ist dann bereits im Repository; das zu erkennen ist Aufgabe von WP03 (Secret-Scanning). |

## Geprüft und für gut befunden

- **Kein Host-Root:** Proxy, Brücke, Socket, Cache und Leck-Suche laufen als Runner-User. Es gibt keine Netzwerk-Namespaces außer dem der Engine, keine iptables-Regeln und keine neuen Capabilities.
- **Secrets:** nie in argv (Container-Argumente geprüft), nie in API-Antworten (nur Namen + Datum, Integration + E2E prüfen Seite und JSON), nie in Events (Runner schwärzt die Ausgabe, App schwärzt erneut, Tests prüfen), verschlüsselt (Test liest die DB), nur im Setup-Env (Agent-`env` per `MOCK_ENV` geprüft; Verify prüft `test -z "$DEPLOY_TOKEN"`).
- **Fail-closed:** Ein `ExecRequest` ohne Policy bekommt kein Netz (Test). Ungültige Regeln ⇒ Lauf schlägt fehl statt offen zu laufen. Verbindungen gehen nur an geprüfte IPs. Der Name wird vor DNS geprüft.
- **Architektur:** Datei-, Prozess- und Netzarbeit im Runner, Protokoll in `queued-common`, App ohne Host-Rechte. Migration nur neu (0005). Alt-Tests unverändert grün.
- **Engine-Ehrlichkeit:** Bei Engine `none` steht überall „nicht durchgesetzt“; `egress_enforced` hängt ausschließlich an der Engine.
