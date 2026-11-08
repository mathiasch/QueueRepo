# WP02 – Environments, Setup-Phase & Egress-Kontrolle: Umsetzung

Grundlage: `WP02-plan.md`. Branch `mvp`, nicht committet. Testbericht: `WP02-test.md`, Urteil: `WP02-review.md`.

## 1. Was gebaut wurde

| Baustein | Dateien | Kurz |
|---|---|---|
| Gemeinsame Netzlogik | `backend/crates/common/src/net.rs` | `NetMode`, `EgressPolicy` (Default = kein Netz, fail-closed), `EgressVerdict`, `HostRule` (exakt, `*.domain`, IP, Port), `forbidden_ip` (vorher nur in `llm.rs`), `Cidr`. Von App-SSRF-Schutz, Runner-Proxy und Frontend-Validierung gemeinsam genutzt |
| Protokoll | `backend/crates/common/src/lib.rs` | `ExecRequest.{egress, cache, secret_env}`, `ExecFrame::Egress`, `Exit.{cache, leaks}`, `RunnerInfo`, `CacheSpec`, `CacheInfo` |
| Egress-Proxy | `backend/crates/runner/src/egress.rs` (+ `egress/tests.rs`) | Pro Lauf; Unix-Socket (Container) bzw. Loopback-TCP (Engine `none`). CONNECT + absolute-form HTTP; Entscheidung auf dem Namen **vor** DNS; Auflösung im Runner, IP-Filter (immer gesperrt / Betreiber-Sperrliste / LAN-Ausnahmen / eigene Host-Adressen / private Netze), Verbindung zur geprüften IP. Nur-Lesen: Kopfprüfung jedes Requests, HTTPS per Lauf-CA (rcgen) mit Upstream-Prüfung (webpki-roots + `RUNNER_EGRESS_EXTRA_CA`). Node-Brücke + `sh`-Wrapper für den Container. Host-Adressen aus `/proc/net/fib_trie` + `/proc/net/if_inet6` |
| Setup-Unterstützung | `backend/crates/runner/src/setup.rs` | Cache-Schlüssel (Material + Image + Lock-Dateien, Symlinks nicht gefolgt), Anlegen/Wiederverwenden/Aufräumen, Cache-Variablen, Leck-Suche (memchr, Obergrenzen), **Bereinigung** gefundener Lecks, Zeilen-Redaktion |
| Runner-Integration | `backend/crates/runner/src/{exec,lib,main}.rs` | Container immer `--network none`, Egress-Verzeichnis ro, Wrapper, Cache-Mount nur bei `cache`, Redaktion der Ausgabezeilen, Leck-Suche nach Läufen mit Secrets, `GET /v1/info`, `DELETE /v1/workspaces/{id}/cache` (auch beim Löschen des Workspace), Betreiber-Variablen `RUNNER_EGRESS_PRIVATE_HOSTS/DENY/EXTRA_CA`, harte Fehler bei `RUNNER_NETWORK` und zu langem `RUNNER_DATA_DIR` |
| Migration | `backend/crates/app/migrations/0005_environments.sql` | `project_environments` (CHECKs für Modus, Timeout, JSON-Typ), `project_secrets` (Namens-CHECK, verschlüsselt) |
| Geteilte Typen | `backend/crates/api/src/env.rs` | `NetworkPolicy`, `EnvironmentView`, `UpdateEnvironment`, `SecretInfo`, `SandboxInfo`, 8 Presets, Lock-Datei-Liste, **eine** Validierung für Backend und Frontend (Namen, reservierte Namen, Anbieter-Variablen, Domains, Pfade, Grenzen) |
| App | `backend/crates/app/src/environment.rs`, `api/environment.rs`, `runner.rs`, `agent/mod.rs`, `runner_client.rs`, `state.rs` | Laden/Speichern, Secrets entschlüsseln (nur für Setup), Policies je Phase, Cache-Spec, Sandbox-Status (TTL 30 s), `EgressLog` (Ablehnungen als Events, max. 100/Lauf, Zusammenfassung erlaubter Ziele), Setup-Phase `run_setup`, Variablen + Policy für Agent/Review/Verify, `agent::egress_hosts` (Modell-API je Zugang) |
| SSRF (WP13-N8) | `backend/crates/app/src/{llm,gateway,batch}.rs`, `api/providers.rs` | `GuardedResolver` filtert jede DNS-Antwort zur Verbindungszeit, `redirect_policy` (≤ 5, nur http/https, keine gesperrten IP-Literale, nie der Runner); alle App-Clients für Nutzer-URLs nutzen sie |
| Mock | `backend/crates/app/src/agent/mock.rs` | `MOCK_ENV`, `MOCK_NET:<url>` |
| Frontend | `frontend/src/pages/environment.rs`, `logic/env.rs`, `pages/project.rs`, `logic/{labels,progress}.rs`, `style/app.css` | Reiter „Umgebung“: Status-Banner, Setup-Skript, Variablen (stabile Zeilen-Signale), zwei Netz-Abschnitte mit Modus, Presets, Domains, „Nur lesen“, Anzeige der Modell-API-Hosts, Cache, Geheimnisse (nur Namen). Events „Setup“/„Netzwerk“, Fortschritt „Vorbereiten“ bis zur ersten Agenten-Ausgabe |
| Betrieb | `README.md`, `deploy/runner.env.example`, `docker/agent.Dockerfile`, `e2e/scripts/stack.sh` | Abschnitt „Umgebung, Setup und Netzwerk“, Upgrade-Hinweis, Grenzen; Test-Stack mit `RUNNER_EGRESS_PRIVATE_HOSTS=127.0.0.1` |

## 2. Entscheidungen

1. **`--network none` + Unix-Socket-Brücke** statt internem Podman-Netz oder Paketfilter: einzig durchsetzbare Variante ohne Host-Root und ohne zweites Image. Fail-closed: Ohne laufende Brücke gibt es kein Netz. Mit Docker 29 real geprüft (`WP02-test.md` §4).
2. **Allowlist auf dem Namen vor DNS.** Abgelehnte Namen verlassen den Host nie als DNS-Anfrage (Test: `.invalid`-Name liefert `not_allowed`, nicht `unresolvable`).
3. **Modell-API immer erlaubt, nie inspiziert.** Ohne sie arbeitet kein Agent. „Aus“ heißt im Agenten daher „Nur Modell-API“, im Setup „Kein Netz“. Die Hostliste ist Wissen über CLIs/Anbieter und als ungeprüft markiert.
4. **Verify bekommt das Agent-Netz ohne Modell-API**, Review das Agent-Netz mit der Modell-API des Reviewers. Variablen gelten in allen Phasen, Secrets nur im Setup.
5. **Owner-only für Schreibzugriffe** (Umgebung, Secrets): Die Policy ist eine Sicherheitsgrenze (Vorbild WP13-Datenstandort). Cache leeren dürfen Bearbeiter.
6. **Setup mit `sh -e`**: Ein mehrzeiliges Skript, dessen letzte Zeile gelingt, darf einen früheren Fehler nicht verdecken.
7. **Setup-Fehler = `blocked`/`config`** (kein Fix, kein Versuchsverbrauch). Ein Fix-Agent kann ein kaputtes Setup-Skript nicht reparieren, denn das Skript liegt in der Projektkonfiguration, nicht im Repo.
8. **Leck ⇒ blockieren *und* bereinigen.** Nur blockieren hätte die Datei liegen lassen: Das nächste `begin` committet Reste als WIP (und pusht bei `auto_push`), ein späterer Agent könnte sie lesen.
9. **Cache nur im Setup beschreibbar und sichtbar.** Der Agent ist nicht vertrauenswürdig und darf keine Pakete für spätere Setups (mit Secrets) vergiften.
10. **Engine `none` bekommt immer einen Proxy** (auch bei `off` als Deny-all), damit Tests und Entwicklung dieselben Regeln zeigen. Das ist **nie** als „geschützt“ markiert (Banner, Event-Suffix, `egress_enforced=false`).
11. **Sandbox-Status nicht in `/api/meta`**: Der Endpunkt ist öffentlich, der Schutzstatus geht nur an Projektmitglieder.

## 3. Abweichungen vom Plan

| Plan | Umsetzung | Grund |
|---|---|---|
| Leck ⇒ Setup fehlgeschlagen | zusätzlich Bereinigung der Dateien (`setup::scrub`) | Selbst-Review: WIP-Commit/Auto-Push und spätere Agenten |
| Host-Adressen in der Sperrliste | eigene Liste `own`, durch `RUNNER_EGRESS_PRIVATE_HOSTS` übersteuerbar; Betreiber-`DENY` gilt immer | Mit nur einer Liste wäre ein LAN-Dienst auf dem eigenen Host nie freigebbar gewesen (Fund beim ersten Bauen) |
| `sandbox` in `/api/meta` | entfernt, nur über `/environment` | öffentlicher Endpunkt |
| Sandbox-Status gecacht | TTL 30 s | Runner-Neustart mit anderer Engine darf keinen veralteten Schutzstatus zeigen |
| – | Anbieter-Variablen (`ANTHROPIC_*`, `OPENAI_*`, `XDG_*` …) als Projektvariablen gesperrt | Selbst-Review B1 (siehe Review): Eigentümerin hätte Schlüssel von Bearbeitern umleiten können |
| – | Cache-Schlüssel enthält das Image | anderes Image = andere Toolchain |
| – | Redirect-Policy im App-Client | Redirects wurden bisher ungeprüft gefolgt (zweite SSRF-Lücke neben DNS-Rebinding) |
| Fortschritt unverändert | „Start #…“ setzt auf „Vorbereiten“ zurück | Setup gehört sichtbar zur Vorbereitung |
| Snapshot nach Setup (Report P1.5) | nicht umgesetzt | siehe Plan §10.7; Workspace behält Abhängigkeiten, Cache beschleunigt |
| Secret-Injection per Proxy-Header (Report P1.5) | nicht umgesetzt | eigenes Credential-Modell, laut Regeln ausgeklammert |

## 4. Annahmen (nicht live verifiziert)

- Modell-API-Hosts je Zugang (`agent::egress_hosts`), insbesondere Vertex-Multiregion (`aiplatform.eu.rep.googleapis.com`) und Abo-Refresh-Endpunkte (`auth.openai.com`, `oauth2.googleapis.com`).
- Dass Claude Code, Codex, Gemini CLI, opencode, npm, pip, cargo und git die Proxy-Variablen beachten bzw. `NODE_EXTRA_CA_CERTS`/`SSL_CERT_FILE` für „Nur lesen“ nutzen. Geprüft ist nur: curl (Unit/Integration), busybox `nc`, Node `http` (Docker-Gegenprobe).
- Preset-Domains der Registries (aus Dokumentation, nicht aus dieser Sandbox abrufbar).
- Rootless Podman und SELinux (`:Z`) mit dem Socket-Mount; geprüft ist Docker 29 (rootful Daemon, Container mit `--user` = Runner-UID 0 im Testlauf).
