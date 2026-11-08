# WP02 – Environments, Setup-Phase & Egress-Kontrolle: Plan

Stand: 2026-11-08, Branch `mvp` (Basis `5a30c61`, WP13 abgeschlossen). Ein Agent für Plan, Umsetzung, Test und Review.
Verbindlich: `work-packages.md` (Gemeinsame Regeln, WP02), Report P0.3/P0.4/P1.5, offene Funde aus WP13 (N8 DNS-Rebinding).

## 1. Ausgangslage (am Code geprüft)

- Agent-, Verify-, Review-, Eval- und Probe-Läufe starten über `POST /v1/exec` im Runner. Container-Engines: `podman run --rm -i --init --cap-drop ALL --security-opt no-new-privileges --memory/--cpus/--pids-limit`, Workspace und ein Wegwerf-`$HOME` als Bind-Mounts. Secrets laufen nur über die Umgebung (`-e NAME`, Wert im Runner-Prozess-Env).
- **Netz:** Ohne `RUNNER_NETWORK` hat der Container das Standardnetz der Engine, also **uneingeschränktes Internet** inklusive Host-Loopback (bei slirp4netns/pasta je nach Konfiguration), LAN und Metadaten-IP. Engine `none` ist ein normaler Kindprozess ohne jede Isolation.
- Es gibt kein Setup-Skript, keine Projekt-Umgebungsvariablen, keine Projekt-Secrets und keinen Cache. Der Workspace (Git-Repo mit `node_modules/` & Co. in `.gitignore`) bleibt zwischen Aufträgen erhalten.
- App-seitiger SSRF-Schutz (`llm::check_target`) prüft Ziele vor dem Request; reqwest löst danach **erneut** auf (DNS-Rebinding, WP13-N8) und folgt **Redirects ungeprüft** (bis zu 10, Standard von reqwest). Letzteres steht in keinem bisherigen Bericht – Fund dieses Pakets.

## 2. Ziele / Nicht-Ziele

**Ziele (WP02):**
1. Pro Projekt ein Environment: Setup-Skript (+ Timeout), Umgebungsvariablen (nicht geheim), Secrets (AES-GCM wie bisher, nur Setup-Phase), Netzwerk-Policy je Phase (`off | allowlist | open`), Domain-Allowlist mit Presets (Paket-Registries), optional nur GET/HEAD/OPTIONS.
2. Zwei Phasen pro Auftrag: Setup-Container (Netz gemäß Setup-Policy, mit Secrets) → Agent-Container (ohne Secrets, Netz gemäß Agent-Policy). Verify und Review laufen wie der Agent ohne Secrets.
3. Egress-Durchsetzung im Runner **ohne Host-Root**: pro Lauf ein Allowlist-Proxy, Container ohne eigenes Netz.
4. Link-Local/Metadaten, Loopback, private Netze, CGNAT, Multicast und die eigenen Adressen des Hosts sind immer gesperrt (Ausnahme nur per Betreiber-Liste für LAN-Hosts). Jede Entscheidung des Proxys wird Task-Event.
5. Dependency-Cache pro Projekt, Schlüssel aus Setup-Skript, Image, Variablen und Lockfiles; nur in der Setup-Phase beschreibbar.
6. UI: Environment-Editor im Projekt (eigener Reiter „Umgebung“), Anzeige ob die Regeln durchgesetzt werden.
7. WP13-N8: DNS-Rebinding und Redirects im SSRF-Schutz der App schließen.

**Nicht-Ziele:** TLS-Inspektion für erlaubte Domains außer im Nur-Lesen-Modus; Snapshot des Containers nach dem Setup (`podman commit`, siehe Risiken); Secret-Injection per Proxy-Header (Report P1.5 – wäre ein eigenes Credential-Modell, das laut Regeln ausgeklammert ist); Credential-Modell der Provider (unverändert); Lizenz/ToS.

## 3. Architektur

### 3.1 Egress: Variante „kein Netz + Unix-Socket-Brücke“ (gewählt)

```
Container (--network none, nur lo)                     Runner (Host, unprivilegiert)
  Agent/CLI ──HTTPS_PROXY=http://127.0.0.1:3128──▶ bridge.js ──Unix-Socket──▶ Egress-Proxy (pro Lauf)
                                                  (Node, ro gemountet)        │ Allowlist (Hostname vor DNS)
                                                                              │ DNS → IP-Filter → connect(IP)
                                                                              ▼
                                                                         Internet (nur erlaubte Ziele)
```

- `podman|docker run … --network none`: Der Container hat nur Loopback. Kein Route nach außen, kein DNS, kein UDP/QUIC, keine abstrakten Unix-Sockets des Hosts (eigener Netz-Namespace). **Fail-closed:** Stirbt die Brücke oder fehlt `node` im Image, gibt es gar kein Netz.
- Der Runner startet pro Lauf einen Proxy auf einem Unix-Socket im Laufverzeichnis und mountet das Verzeichnis **read-only** nach `/queue-egress` (connect() auf Sockets funktioniert auch auf ro-Mounts). Ein kleines Node-Skript (vom Runner geschrieben, nicht im Image) lauscht im Container auf `127.0.0.1:3128` und reicht jede Verbindung an den Socket weiter. Ein Wrapper (`sh -c`) startet die Brücke und wartet auf ihre Bereitschaft, dann `exec` des eigentlichen Programms.
- Proxy-Protokoll: HTTP/1.1 `CONNECT host:port` (Tunnel) und absolute-form `http://`-Requests. **Allowlist-Prüfung auf dem Hostnamen vor jeder DNS-Auflösung** (kein DNS-Exfil über abgelehnte Namen), danach Auflösung im Runner, Filterung jeder IP (gesperrte Netze, Host-Adressen, `RUNNER_EGRESS_DENY`), Verbindung zur **geprüften IP** (kein Rebinding-Fenster).
- Warum nicht die Alternativen:
  - *Internes Podman-Netz + Proxy-Container:* braucht ein zweites Image, Netzverwaltung und Lebenszyklus pro Lauf; die Proxy-Logik müsste trotzdem wir schreiben.
  - *slirp4netns/pasta mit Host-Loopback + `HTTPS_PROXY`:* nicht durchgesetzt – jedes Programm kann direkt verbinden.
  - *iptables/nftables:* braucht Root bzw. `CAP_NET_ADMIN` auf dem Host – verboten.
- Ports: Domains ohne Port erlauben 443 und 80; `host:port` erlaubt genau diesen Port. `open` erlaubt jeden Port, aber nur zu öffentlichen Adressen.
- `NO_PROXY=localhost,127.0.0.1,::1`, damit lokale Dev-Server im Container funktionieren.

### 3.2 Policies pro Phase

| Phase | `off` | `allowlist` | `open` |
|---|---|---|---|
| Setup (mit Secrets) | kein Netz (`--network none`, kein Proxy) | Domains + Presets | alle öffentlichen Ziele |
| Agent / Review | **nur Provider-API** des Laufs | Provider-API + Domains + Presets | alle öffentlichen Ziele |
| Verify | kein Netz | Domains + Presets (ohne Provider-API) | alle öffentlichen Ziele |

- Provider-API-Hosts ergeben sich aus Provider und Zugang (z. B. `api.anthropic.com`, Connector-`base_url`, Bedrock-Region). Ohne sie könnte der Agent nicht arbeiten. Die Liste ist Wissen über die CLIs, **nicht live verifiziert** (wird so dokumentiert und in der UI angezeigt).
- Defaults für Projekte ohne Environment: Agent `off` (Default-Deny laut Report), Setup `allowlist` mit Presets der gängigen Paket-Registries. **Verhaltensänderung** gegenüber bisher (offenes Internet) → README-Upgrade-Hinweis.
- **Nur GET/HEAD/OPTIONS** (`read_only`): gilt für Domains/Presets/`open`, nie für Provider-API. Bei `http://` prüft der Proxy jeden Request-Kopf. Bei HTTPS terminiert der Proxy TLS mit einer **pro Lauf erzeugten CA** (rcgen), prüft jeden Request (Methode, kein Body, kein Upgrade, Host passt zum Tunnel) und spricht upstream TLS mit Zertifikatsprüfung (webpki-roots, optional `RUNNER_EGRESS_EXTRA_CA`). Die CA landet nur im Laufverzeichnis; Umgebungsvariablen (`NODE_EXTRA_CA_CERTS`, `SSL_CERT_FILE` u. a.) zeigen darauf. Nur HTTP/1.1 (ALPN). Programme mit Certificate-Pinning scheitern – dokumentiert.

### 3.3 Engine `none`

Kein Netz-Namespace, keine Durchsetzung. Der Runner startet trotzdem den Proxy (TCP auf 127.0.0.1) und setzt die Proxy-Variablen: Programme, die sie beachten, sehen dieselben Regeln und Protokolle (wichtig für Tests). `GET /v1/info` meldet `egress_enforced=false`; App, API und UI kennzeichnen das als **„nicht durchgesetzt“**, nie als geschützt. Die Setup-Policy `off` ist bei `none` ebenfalls nur ein Deny-all-Proxy.

### 3.4 Setup-Phase

- Eigener Lauf `<task>-setup` vor dem Agenten, im selben Workspace (installierte Abhängigkeiten bleiben), eigenes Wegwerf-`$HOME`.
- Programm: `sh -c <setup_script>` (Skript ist nicht geheim). Secrets und Variablen über die Umgebung.
- Runner-seitig: Ausgabezeilen werden um Secret-Werte bereinigt (`***`), bevor sie den Runner verlassen. Nach dem Lauf **Leck-Prüfung**: Workspace (inkl. `.git/config`) und Cache werden nach den Secret-Werten durchsucht (Dateien ≤ 8 MiB, Obergrenzen für Dateien/Bytes). Treffer → Setup gilt als fehlgeschlagen (Klasse `config`), der Agent startet nicht.
- Fehler/Timeout im Setup → Auftrag `blocked` mit Klasse `config` (kein Fix-Auftrag, kein verbrauchter Versuch), Meldung mit Exit-Code und Ausgabe-Ende. Abbruch durch Nutzer → `cancelled`.
- Kein Setup-Skript → keine Setup-Phase.

### 3.5 Cache

- Verzeichnis `RUNNER_DATA_DIR/caches/<projekt>/<schlüssel>/`, gemountet nach `/cache` **nur in der Setup-Phase** (der Agent ist nicht vertrauenswürdig und darf den Cache für spätere Setups mit Secrets nicht vergiften).
- Schlüssel (Runner berechnet, App liefert Material): SHA-256 über Setup-Skript, Image, Variablen, Policy-Material und Inhalt der Lockfiles (feste Liste im Repo-Wurzelverzeichnis + eigene Pfade). Änderung → neuer leerer Cache. Nach erfolgreichem Setup werden ältere Schlüssel des Projekts gelöscht. Fehlgeschlagenes Setup verwirft einen neu angelegten Cache.
- Variablen im Setup: `QUEUE_CACHE_DIR`, `XDG_CACHE_HOME`, `npm_config_cache`, `PIP_CACHE_DIR`, `CARGO_HOME`, `GOMODCACHE`, `GOCACHE`, `YARN_CACHE_FOLDER`, `UV_CACHE_DIR`.
- „Cache leeren“ per API/UI; Löschen des Projekts löscht auch die Caches.

### 3.6 Protokoll als Task-Events

Neuer Frame `ExecFrame::Egress { host, port, method, allowed, reason }`. Die App schreibt **jede Ablehnung** als Event `egress` (höchstens 100 pro Lauf, danach eine Sammelzeile) und am Laufende eine Zusammenfassung der erlaubten Ziele mit Anzahl (pro Phase). Setup-Ausgabe → Events `setup`.

## 4. Datenmodell (Migration `0005_environments.sql`)

```sql
CREATE TABLE project_environments (
  project_id UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  setup_script TEXT NOT NULL DEFAULT '',
  setup_timeout_secs INT NOT NULL DEFAULT 600 CHECK (setup_timeout_secs BETWEEN 10 AND 3600),
  env_vars JSONB NOT NULL DEFAULT '[]',          -- [{name, value}]
  setup_network JSONB NOT NULL,                  -- NetworkPolicy
  agent_network JSONB NOT NULL,
  cache_enabled BOOLEAN NOT NULL DEFAULT TRUE,
  cache_files TEXT[] NOT NULL DEFAULT '{}',
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  CHECK (setup_network->>'mode' IN ('off','allowlist','open')),
  CHECK (agent_network->>'mode' IN ('off','allowlist','open'))
);
CREATE TABLE project_secrets (
  project_id UUID REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT CHECK (name ~ '^[A-Z_][A-Z0-9_]{0,63}$'),
  value_enc BYTEA NOT NULL, updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (project_id, name)
);
```

`NetworkPolicy = { mode, presets: [String], domains: [String], read_only: bool }` (geteilt in `queued-api`, validiert in App und Frontend mit derselben Funktion).

## 5. Protokoll App ↔ Runner (`queued-common`)

- `ExecRequest` + `egress: EgressPolicy` (Default: `Off`, fail-closed), `cache: Option<CacheSpec>`, `secret_env: Vec<String>` (Namen der Env-Einträge, die Secrets sind: Redaktion + Leck-Prüfung).
- `ExecFrame::Egress {…}`; `Exit` + `cache: Option<CacheInfo{key, hit}>`, `leaks: Vec<String>`.
- `GET /v1/info` → `{engine, egress_enforced}`; `DELETE /v1/workspaces/{id}/cache`.
- Gemeinsame Netzlogik in `queued-common::net`: Hostregeln (Parsen/Validieren/Matching) und `forbidden_ip` (heute in `llm.rs`) – eine Implementierung für App-SSRF-Schutz und Runner-Proxy.

## 6. API der App

| Methode | Pfad | Rolle | Inhalt |
|---|---|---|---|
| GET | `/api/projects/{id}/environment` | Viewer | Environment ohne Secret-Werte (nur Namen + Datum), `egress_enforced`, Engine, Presets |
| PUT | `/api/projects/{id}/environment` | **Owner** | Skript, Timeout, Variablen, Policies, Cache-Einstellungen |
| PUT | `/api/projects/{id}/secrets/{name}` | **Owner** | Wert setzen/ersetzen (verschlüsselt) |
| DELETE | `/api/projects/{id}/secrets/{name}` | **Owner** | entfernen |
| POST | `/api/projects/{id}/environment/cache/clear` | Editor | Runner löscht die Caches |

Owner-only für Schreibzugriffe: Die Policy ist eine Sicherheitsgrenze (Vorbild WP13: „Lockern nur durch Eigentümer“). `/api/meta` bekommt `sandbox: {engine, egress_enforced}` und die Presets.

## 7. SSRF-Härtung der App (WP13-N8)

`llm::client(guard)` und der Gateway-Client bekommen einen eigenen DNS-Resolver, der jede aufgelöste Adresse mit `forbidden_ip` filtert (verbunden wird nur mit geprüften Adressen), und eine Redirect-Policy, die IP-Literale, fremde Schemata und mehr als 5 Sprünge ablehnt (Hostnamen gehen wieder durch den Resolver).

## 8. UI

Reiter „Umgebung“ im Projekt: Status-Banner (durchgesetzt / Entwicklungsmodus), Setup-Skript, Variablen (Zeilen), Geheimnisse (nur Namen, Ersetzen/Entfernen, Passwortfeld), zwei Netzwerk-Karten (Setup, Agent) mit Modus-Auswahl, Presets, Domainliste, „Nur lesen“, Hinweis auf immer erlaubte Provider-API, Cache-Schalter, Lockfile-Pfade, „Cache leeren“. Nicht-Eigentümer sehen alles schreibgeschützt. Drawer: Event-Arten „Setup“ und „Netzwerk“, Fortschrittsphase „Vorbereiten“ deckt das Setup ab. Designsystem WP00, Safe-Areas, axe.

## 9. Tests (Zielbild)

- **Unit (common/runner/app/frontend):** Hostregeln, `forbidden_ip`, Policy-Validierung, Proxy (CONNECT erlaubt/abgelehnt, private/Link-Local/Host-IP, `open`, `off`, absolute-form HTTP, Nur-Lesen HTTP und HTTPS-MITM gegen lokalen TLS-Server, Provider-Host ohne MITM, DNS erst nach Allowlist), Containerargumente (kein Netz, Mount, Wrapper, kein Secret in argv, Cache nur im Setup), Redaktion, Leck-Prüfung, Cache-Schlüssel/Prune, Node-Brücke mit echtem `node`, `/proc`-Parser für Host-Adressen, Resolver/Redirect-Policy der App.
- **Integration (App + echter Runner, Engine `none`, Postgres):** Environment-CRUD/Rollen/keine Secret-Werte in Antworten und Events; Setup vor Agent; Secret nur im Setup-Env; Setup-Fehler → `blocked/config` ohne Agent und ohne Fix; Leck → blockiert; Cache-Treffer/Invalidierung; Egress-Events (erlaubt per lokalem Fake-Upstream, abgelehnt ohne externen Call).
- **System/E2E (Playwright, alle Projekte):** Editor ausfüllen/speichern/neu laden, Secret nie sichtbar, Banner „nicht durchgesetzt“, Auftrag mit Setup zeigt Setup- und Netzwerk-Events im Drawer, axe hell/dunkel, iPhone hoch/quer.
- Kein echter externer Call: Ablehnungen passieren vor DNS, erlaubte Ziele sind lokale Fake-Server (Betreiberliste `RUNNER_EGRESS_PRIVATE_HOSTS=127.0.0.1` nur im Test-Stack).

## 10. Risiken und Grenzen (ehrlich)

1. **Repository-Code im Setup sieht die Secrets.** Das Setup führt Skripte des Repos aus (`npm install` → `preinstall`), und das Repo wird von früheren Agenten-Läufen verändert. Ein Agent kann also Code platzieren, der im nächsten Setup die Secrets liest und an eine *erlaubte* Domain schickt. Nicht lösbar ohne Review jedes Agenten-Commits vor dem nächsten Setup; Gegenmaßnahmen: enge Setup-Allowlist, Leck-Prüfung, Hinweis in der UI („nur Tokens mit minimalen Rechten“).
2. **Hostname-Proxy ohne TLS-Inspektion** (außer Nur-Lesen): Domain-Fronting über CDNs und Exfiltration an erlaubte Domains (z. B. github.com) bleiben möglich – wie bei Claudes Sandbox.
3. **Provider-Hosts aus Wissen**, nicht live geprüft; fehlt ein Host, scheitert der Agent mit Netzwerkfehler (sichtbar als Ablehnung im Protokoll).
4. **CLIs müssen `HTTPS_PROXY` beachten** (Claude Code, Codex, Gemini CLI, opencode, npm, pip, cargo, git, curl laut Doku ja) – nicht live verifiziert; das Agent-Image wurde nicht gebaut.
5. **Node im Image** ist Voraussetzung der Brücke (Agent-Image basiert auf `node:22`). Eigene Images ohne Node haben kein Netz (fail-closed).
6. **SELinux (`RUNNER_VOLUME_OPTS=Z`)** relabelt auch das Egress-Verzeichnis; das Socket-Verhalten unter SELinux ist ungetestet.
7. **Kein Snapshot nach Setup:** Setup läuft bei jedem Auftrag (mit warmem Cache). Snapshots bräuchten Image-Verwaltung und Plattenplatz; der Workspace behält die installierten Abhängigkeiten ohnehin.
8. **Engine `none`** setzt nichts durch – sichtbar gekennzeichnet.
