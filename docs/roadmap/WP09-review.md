# WP09 – Memory, Wissen, Dedupe & Anhänge: Review

Selbst-Review des Umsetzers (kein Schönreden). Grundlage: Code-Stand dieses Berichts, `WP09-plan.md`, `WP09-impl.md`, `WP09-test.md`. Alle blockierenden Funde sind behoben und durch Tests abgedeckt; die nicht blockierenden sind offen und im README unter „Bekannte Grenzen“ genannt.

## Urteil

**Abnahmefähig mit offenen, benannten Grenzen.** Die harten Regeln des Auftrags sind umgesetzt und getestet: Upload-Limits, MIME-Sniffing mit Allowlist, Neu-Kodierung (EXIF/GPS weg), bereinigte Namen, keine Traversal/Symlinks bei der Bereitstellung, Schutz-Header, Rechte pro Projekt, Quota, Löschen mit Auftrag/Projekt, Kamera-Eingang auf dem iPhone, nie im Commit (Index, Historie, Übernahme); verwaltete Regeln mit Versionen und Freigabe, Prompt nur mit freigegebener Fassung, Brücke passend zu den WP03-Abschaltungen; versionierter Zustand mit Prüfung im Runner gegen den Arbeitsbaum, Ablehnung/Reparatur, Größenlimits; deterministisches Dedupe mit Critic, einstellbaren Schwellen, sichtbarer Begründung und optionalem Modell-Critic mit Kostenerfassung.

## Blockierende Funde (alle behoben)

| # | Fund | Behebung | Beleg |
|---|---|---|---|
| B1 | **Codex und Gemini lasen `AGENTS.md` bzw. `GEMINI.md` aus dem Repository immer** – auch mit WP03-Abschaltung. Damit wirkte agentengeschriebener Text als Systemanweisung in späteren Läufen, ohne Freigabe (Verstoß gegen „nur von Menschen freigegeben in den Prompt“). WP03 hatte das übersehen. | Codex `-c project_doc_max_bytes=0`, Gemini `context.fileName` pro Lauf unratbar; mit vertrauter Repo-Konfiguration alle Anweisungsdateien in `AGENT_CONFIG_PATHS` (Baseline + Gate) | `agent::tests::repo_config_is_restricted_per_cli` (erweitert), `gate::tests::watched_instruction_files`, Integration `rules_export_import_and_trusted_repo_config` (AGENTS.md-Änderung eines Agenten wird gehalten) |
| B2 | **`.queue/` konnte in Commits landen** (bestehend seit WP01, mit Anhängen gravierend): `!.queue/` in `.gitignore` (Repo oder Agent), gelöschte Exclude-Zeile, eigene Commits des Agenten mit `git add -f`; die Squash-Übernahme hätte das in den Hauptstand gebracht, der PR-Modus es gepusht. | `stage_all` an allen Stellen, Zusammenfassen der Agenten-Commits ab Gate-Basis, fail-closed in `merge_prepare` | Runner `queue_dir_is_never_staged` (Mutant ohne `rm --cached` wird erkannt), `agent_commits_with_queue_files_are_squashed`, `merge_refuses_a_tip_with_queue_files`; Integration + System „never a commit“ |
| B3 | Ein liegengebliebenes `.queue/knowledge.md` eines früheren Laufs hätte nach einer **Ablehnung** beim nächsten Lauf denselben Vorschlag erneut erzeugt. | Datei am Anfang jedes Auftragslaufs leeren; exakt abgelehnter Text wird nicht erneut gespeichert | Integration `agent_rule_proposal_needs_owner_approval` (Teil „schon abgelehnt“) |
| B4 | **Entwürfe gingen nach einem Neuladen im Composer verloren**, zählten aber 24 h weiter gegen das Limit von 10 – Nutzer saß bis zu einem Tag fest. | `GET /api/projects/{id}/attachments/drafts`, Composer lädt die Entwürfe des gewählten Projekts | Integration (Liste, nur eigene, Betrachter 403), Desktop-E2E (Anhang bleibt nach Reload) |
| B5 | Der Prompt listete bis zu 10 Anhänge, der Runner bekam wegen der 25-MiB-Grenze eventuell weniger (geerbte Anhänge eines Fix-Auftrags) – der Agent hätte nach fehlenden Dateien gesucht. | eine Auswahl (`attachments::select`) für Prompt und Runner | `selection_respects_count_size_and_unique_names` |
| B6 | iPhone: `scrollIntoView` (und damit Tastaturfokus) landete **unter der Tab-Bar** (WCAG 2.4.11). | `scroll-padding-bottom` über Tab-Bar + Home-Indikator | `memory-mobile.spec.ts` (Speichern-Knopf über dem Home-Indikator), alle Mobile-Specs grün |
| B7 | Bestehender E2E-Test `automation.spec.ts` (WP07) war racy: Die Zeile enthält den Knopf „Widerrufen“, das Warten auf den Text „Widerrufen“ war sofort erfüllt; unter Last kam der Aufruf vor dem Widerruf (im ersten Volllauf rot). | Test wartet, bis der Widerrufen-Knopf verschwunden ist | 4× wiederholt grün, beide Abnahmeläufe grün |
| B8 | Integrationstests warteten auf „Leerlauf-Auftrag erledigt“, der Prüfbericht wird aber erst danach geschrieben (Reihenfolge des Bestands) – im Volllauf rot. | Tests warten auf den Bericht | beide Abnahmeläufe grün |
| B9 | WP09-Tests suchten erzeugte Aufträge in `GET /api/tasks`; diese Liste lässt Aufträge im Status `merging` aus (Bestand seit WP04). Der eingeplante Vorschlag lief schon und stand gerade in der Übernahme – im zweiten Volllauf rot (System-Spec). | Tests holen die Aufträge per ID aus dem Prüfbericht | Spec 2× wiederholt grün, Abnahmeläufe 5 und 6 |
| B10 | Bestehender WP06-Test `messages_wait_can_be_withdrawn_and_reach_the_next_start` war racy: „running“ beginnt beim Claim, Nachrichten vor dem Prompt-Bau werden beim Start zugestellt; unter Last war die 21. Nachricht dann erlaubt (Abnahmelauf 3 rot). Kein Produktfehler (so spezifiziert). | Test wartet, bis der Agent arbeitet (wie die übrigen Steering-Tests) | `plan_steering` 13/13, Abnahmeläufe 5 und 6 |

## Nicht blockierende Funde (offen)

| # | Fund | Warum nicht blockierend |
|---|---|---|
| N1 | CLI-Verhalten (Claude `--setting-sources user` ohne Projekt-`CLAUDE.md`/`AGENTS.md`, Codex `project_doc_max_bytes=0`, Gemini `context.fileName`, opencode `OPENCODE_DISABLE_PROJECT_CONFIG`, `@`-Importe der Brücken) ist aus Binaries/Bundles abgeleitet, **nicht ausgeführt** (kein Live-Lauf, keine Kosten). | Im README und in der UI („Was liest welcher Agent?“) so gekennzeichnet; Unit-Tests prüfen die Aufrufe. Muss beim nächsten Live-Contract-Test (WP01-Nightly) bestätigt werden. |
| N2 | Mit vertrauter Repo-Konfiguration und exportierter AGENTS.md lesen Claude/Codex/Gemini die Regeln zusätzlich selbst – doppelt im Kontext (Tokens). | Funktional korrekt; Standard ist „nicht vertraut“. |
| N3 | Freigabe eines Vorschlags auf veralteter Basis ersetzt die aktuelle Fassung vollständig (kein 3-Wege-Merge von Text). | Die UI zeigt den Unterschied **zur aktuellen** Fassung inkl. entfernter Zeilen; die Eigentümerin sieht, was verloren geht. |
| N4 | Projekte, die selbst ein Verzeichnis `.queue/` versionieren, werden nicht unterstützt (Index-Bereinigung löscht es aus Commits, die Übernahme hält an). | Seit WP01 reserviert (`begin` leert `.queue/` schon immer); dokumentiert. |
| N5 | Ähnlichkeit ist lexikalisch: Umformulierungen mit ganz anderen Wörtern bleiben unerkannt; Schwellen sind aus Testfällen kalibriert, nicht aus Echtdaten. | Konservativ (Zwischenzone ⇒ Freigabe statt Verwerfen), einstellbar, optionaler Modell-Critic. |
| N6 | Der Modell-Critic läuft synchron in der Abschlussphase des Leerlauf-Auftrags (bis zum LLM-Timeout); er nutzt nur den direkten Zugang des Analyse-Schritts, kein eigenes Routing. | Nur bei aktivierter Option; Fehler fallen auf die Regeln zurück (protokolliert). |
| N7 | PDFs werden nicht bereinigt (Metadaten bleiben), animierte GIF/WebP auf das erste Bild reduziert, HEIC abgelehnt. | PDFs nur als Download (`octet-stream`, `attachment`, CSP sandbox); Safari liefert mit `accept="image/*"` JPEG. Echtes iPhone nicht getestet (nur Chromium mit iPhone-Profil). |
| N8 | API-Tokens (WP07) können die neuen Endpunkte nicht nutzen. | Sichere Voreinstellung der Allowlist; Erweiterung um Scopes ist eigene Arbeit. |
| N9 | Der Zustandsabschnitt steht in jedem Auftrags-Prompt, auch wenn ein Projekt den Zustand nicht nutzt (~0,6 KB). | So gewollt (Report P1.8: Startroutine); Agenten beginnen dadurch, Einträge anzulegen – geprüft, begrenzt, für Menschen sichtbar. |
| N11 | `GET /api/tasks` enthält keine Aufträge im Status `merging` (Bestand WP04; die UI zeigt sie über die Übernahme-Warteschlange). | Nicht Teil von WP09; für API-Nutzer überraschend, gehört ins nächste Paket mit API-Bezug. |
| N10 | Die Entwürfe zählen pro Projekt gegen das Limit 10; wer viele Projekte nutzt, kann Speicher bis zur Quota mit Entwürfen belegen (24 h). | Quota pro Person greift; Sweep stündlich. |

## Geprüfte Annahmen anderer Pakete

- WP01: `.queue/`-Dateien symlink-sicher – stimmt für Lesen/Schreiben einzelner Dateien; **nicht** für den Commit-Pfad (B2).
- WP03: „Repo-Konfiguration standardmäßig aus“ – stimmte für Hooks/MCP/Settings, **nicht** für Anweisungsdateien von Codex und Gemini (B1). `AGENT_CONFIG_PATHS` kannte keine Anweisungsdatei außerhalb von `.claude/`.
- WP04: Squash-Übernahme nimmt den Baum des Branch-Endes – deshalb reicht ein sauberer letzter Baum nicht für den PR-Modus (Historie wird gepusht), daher das Zusammenfassen der Agenten-Commits.
- WP13: Kosten pro Schritt erweiterbar – `usage_log.step` hatte einen CHECK, per neuer Migration ergänzt.
