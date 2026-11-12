# WP09 – Memory, Wissen, Dedupe & Anhänge: Plan

Grundlage: `work-packages.md` (Gemeinsame Regeln, WP09), Report P1.7 (Anhänge im Workspace, im Prompt referenziert), P1.8 (strukturierter Projektzustand: JSON-Feature-Liste mit `passes`, Append-only-Progress-Log, nur der Orchestrator strukturiert um), P1.9 (OnEmpty-Dedupe gegen offene/erledigte/revertierte Aufträge, Critic vor der Einreihung, revertierte Aufträge als Negativbeispiele), P0.6 („explizites Einspeisen von CLAUDE.md/AGENTS.md durch Queue“), P2.9 (Regelpflege nur mit Freigabe). Vorarbeit: WP01 (`.queue/`-Dateien symlink-sicher), WP03 (Repo-Konfiguration der CLIs aus, Agent-Konfig unter Baseline/Gate, geschützte Pfade), WP04 (Arbeitsbaum pro Auftrag, Squash-Übernahme), WP06 (Datenblöcke mit Nonce), WP13 (Einzelschritte über Connectoren, Kosten pro Schritt). Branch `mvp`, nicht committet.

## 1. Umfang

| Im Umfang | Nicht im Umfang (Begründung) |
|---|---|
| **Wissen:** verwaltete Projekt-Regeln (Inhalt einer AGENTS.md) mit Versionen; Eigentümer schreibt/stellt wieder her (= freigegeben), Bearbeiter und Agenten schlagen vor, Eigentümer gibt frei/lehnt ab; nur die neueste **freigegebene** Version geht als vertrauenswürdiger Block in jeden Prompt | Automatisches Zusammenführen paralleler Vorschläge (jeder Vorschlag ist ein Volltext; Freigabe zeigt den Unterschied zur aktuellen Version) |
| **Brücke:** Import von `AGENTS.md`/`CLAUDE.md`/`GEMINI.md` aus dem Repository als *Vorschlag*; Export der freigegebenen Version als `AGENTS.md` plus Brückendateien `CLAUDE.md`/`GEMINI.md` (`@AGENTS.md`) per Commit im Hauptrepo (nur Eigentümer, Secret-Scan, Baseline-Update); Abschaltung des Lesens von Repo-Anweisungsdateien in Codex und Gemini, solange die Repo-Konfiguration nicht vertraut ist (bisher lasen beide sie immer) | Schreiben in Nutzer-HOME-Speicher der CLIs (`~/.claude/CLAUDE.md` …): ein zweiter Kanal mit unbelegter Wirkung; der Prompt ist der einzige vertrauenswürdige Kanal |
| **Projektzustand:** `.queue/state.json` (Schema v1) wird jedem Agentenlauf mitgegeben; der **Runner** liest und prüft das Ergebnis gegen das Schema, die Vorversion und den Arbeitsbaum (Dateien existieren, Übergänge erlaubt, nichts gelöscht/umbenannt, Log nur angehängt, Größen); repariert, was reparierbar ist, lehnt den Rest ab; die App übernimmt erst, wenn der Auftrag erledigt ist (3-Wege-Übernahme pro Eintrag gegen parallele Aufträge); Revisionen; Ansicht/Bearbeitung durch Menschen | Ausführung von `init.sh`/Smoke-Test durch Queue (Verify existiert; die Startroutine steht im Prompt) |
| **OnEmpty-Dedupe + Critic:** deterministische Ähnlichkeit (normalisierte Tokens + Wort-Bigramme + Zeichen-3-Gramme des Titels, Jaccard) gegen offene, erledigte, abgelehnte und revertierte Aufträge des Projekts (Zeitfenster) und gegeneinander; Critic-Regeln; optionaler LLM-Critic über den direkten Zugang des Analyse-Schritts (Kosten als Schritt `critic`); Schwellen pro Nutzer; Begründung pro Vorschlag sichtbar; revertierte Aufträge als Negativbeispiele im OnEmpty-Prompt | Externe Embedding-APIs (Default deterministisch; ein LLM-Critic über den vorhandenen Connector deckt den „optionalen Connector“ ab, ohne neue Anbieter-Integration) |
| **Anhänge:** Upload (Composer, Drawer) mit Limits, MIME-Sniffing, Allowlist, Neu-Kodierung von Bildern (EXIF/GPS weg, Ausrichtung angewendet), Quota pro Nutzer, Rechte pro Projekt, Auslieferung als Download mit Schutz-Headern, Bereitstellung unter `.queue/attachments/` (symlink-sicher, vor jedem Lauf neu), nie im Commit (Runner entfernt `.queue/` aus jedem Index, zerdrückt Agenten-Commits, die `.queue/` berühren, Übernahme prüft fail-closed), Löschen mit Auftrag/Projekt, Entwürfe verfallen | HEIC/HEIF (kein Dekoder im Rust-Ökosystem ohne C-Abhängigkeit; iOS liefert mit `accept="image/*"` JPEG), SVG/HTML (Ausführung), Office-Dokumente, Virenscan |
| UI (Leptos, WP00-Designsystem): Projekt-Reiter „Wissen“ (Editor, Versionen, Vorschläge mit Unterschied, Import/Export, CLI-Tabelle) und „Zustand“, Anhänge im Composer (Kamera + Datei, Vorschau, Entfernen) und im Drawer, OnEmpty-Einstellungen (Schwellen, LLM-Prüfung), Prüfbericht im OnEmpty-Auftrag, Begründung am erzeugten Auftrag | |

## 2. Architektur

```
 Browser (queued-web)
   Composer: <input type=file multiple> + <input accept=image/* capture=environment>
        │ POST /api/projects/{p}/attachments?name=… (roher Body, ≤ 10 MiB)
        ▼
 queued (App, keine Host-Rechte) ─────────────────────────────────────────────────────────────
   attachments.rs  sniff (Magic Bytes) → Allowlist → Bild dekodieren (Limits) + neu kodieren
                   (EXIF/GPS/ICC/Text weg, Ausrichtung angewendet) → Quota (Zeilensperre Nutzer)
                   → Postgres bytea (ON DELETE CASCADE Auftrag/Projekt) ; Entwürfe ohne Auftrag 24 h
   knowledge.rs    Versionen/Vorschläge/Freigabe ; prefix() = freigegebene Version für Prompts
   state.rs        Revisionen ; apply_on_done() 3-Wege pro Eintrag ; Prompt-Abschnitt
   dedupe.rs       reine Ähnlichkeit + Critic-Regeln ; screen() vor create_followups (OnEmpty)
   runner.rs       vor jedem Agenten-/Review-/Plan-Lauf: Anhänge bereitstellen, state.json
                   (+ Schema) als queue_file ; nach dem Agentenlauf: state/check, knowledge.md
        │ HTTP (Runner-Token)
        ▼
 queued-runner (Datei/Git)
   PUT  /v1/workspaces/{ws}/attachments   `.queue/attachments/` neu, keine Symlinks, 409 wenn belegt
   POST /v1/workspaces/{ws}/state/check   `.queue/state.json` lesen (≤ 64 KiB, kein Symlink),
                                          queued_common::state::validate + Dateiexistenz (lstat,
                                          im Baum, nicht .git/.queue) → {state, repairs, rejected}
   GET  /v1/workspaces/{ws}/queue/knowledge.md   (Allowlist um knowledge.md erweitert)
   POST /v1/workspaces/{ws}/knowledge/export     AGENTS.md + Brücken schreiben, scannen, committen
   git.rs: stage_all() = add -A + rm --cached .queue ; commit mit `squash_base` zerdrückt
           Agenten-Commits, die .queue/ berühren ; merge_prepare lehnt Bäume mit .queue ab
 queued-common: state.rs (Schema v1, Validierung, Reparatur – WASM-tauglich), Protokolltypen
 queued-api:    knowledge/state/attachments/dedupe Wire-Typen + deutsche Texte, Limits
```

### 2.1 Was jede CLI liest (Belege: Strings/Bundles der gepinnten bzw. installierten Versionen, nichts ausgeführt)

| CLI | Repo-Anweisungsdateien, die sie von sich aus liest | Queue-Standard (Repo-Konfig nicht vertraut) | Repo-Konfig vertraut |
|---|---|---|---|
| Claude Code 2.1.322 (installiert; gepinnt 2.1.319) | `CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/rules/`, `CLAUDE.local.md`, `AGENTS.md`, `.claude/AGENTS.md` – alles nur mit Quelle `projectSettings`/`localSettings` (`pL("projectSettings")` im Binary) | `--setting-sources user` (WP03) ⇒ keine Projektdatei; `~/.claude/CLAUDE.md` liegt im frischen HOME nicht vor | liest die Dateien; sie sind beobachtete Agent-Konfig (Baseline + Gate) |
| Codex 0.167.0 | `AGENTS.md`/`AGENTS.override.md` vom Git-Root bis zum Arbeitsverzeichnis, **unabhängig vom Vertrauen** (Schlüssel `project_doc_max_bytes` im Binary) | **neu:** `-c project_doc_max_bytes=0` ⇒ keine Projektanweisungen | wie links ohne Schalter; Dateien beobachtet |
| Gemini CLI 0.67.0 | `GEMINI.md` (bzw. `context.fileName`) aufwärts bis zum Git-Root und „JIT“ in Unterordnern, im vertrauten Arbeitsbereich **immer** (Queue muss vertrauen, WP03) | **neu:** System-Settings `context.fileName` = unratbarer Name pro Lauf ⇒ kein GEMINI.md | `GEMINI.md` in jeder Tiefe beobachtet |
| opencode 1.19.5 | `AGENTS.md`, `CLAUDE.md`, `CONTEXT.md` per `globUp`, nur ohne `OPENCODE_DISABLE_PROJECT_CONFIG` | WP03-Schalter ⇒ nichts | Dateien beobachtet |

Folge: Im Standard erreicht **kein** agentengeschriebener Anweisungstext den Agenten als Systemanweisung. Freigegebene Regeln kommen ausschließlich über den Prompt (Abschnitt „Project rules“ ganz vorn, byte-stabil für Prompt-Caching). Mit vertrauter Repo-Konfig kommen die Repo-Dateien zusätzlich, aber nur in einer von Menschen freigegebenen Fassung (Baseline vor dem Lauf, Gate vor dem Commit; neue Muster in `AGENT_CONFIG_PATHS`).

### 2.2 Datenmodell (Migration `0012_knowledge_state_attachments.sql`)

- `knowledge_versions(id, project_id → projects CASCADE, version int NULL (gesetzt bei Freigabe, UNIQUE je Projekt), status proposed|approved|rejected, content ≤ 32 768 Bytes, source human|agent|repo, author_id → users SET NULL, task_id → tasks SET NULL, base_version, note ≤ 2000, created_at, decided_by, decided_at)`; aktuelle Fassung = höchste freigegebene Version.
- `project_state_revisions(project_id, rev, data jsonb, source human|agent, task_id, author_id, note, created_at, PK(project_id, rev))`; `tasks.state_rev` (Basis des Laufs), `tasks.state_proposal` (geprüfter Vorschlag bis „erledigt“).
- `attachments(id, project_id CASCADE, task_id NULL CASCADE, user_id CASCADE, name (Workspace-Name), display_name, mime, size, sha256, width, height, data bytea, created_at)`; Indizes Auftrag/Nutzer/Projekt.
- `user_settings.dedupe_threshold` (0.6), `dedupe_review_threshold` (0.35), `dedupe_window_days` (90), `critic_llm` (false); `tasks.dedupe jsonb` (Begründung am erzeugten Auftrag), `tasks.proposals jsonb` (Prüfbericht am OnEmpty-Auftrag), `tasks.reverted_at`.

### 2.3 Regeln im Detail

**Wissen.** Eigentümer: Speichern = neue freigegebene Version; Wiederherstellen = neue Version mit altem Inhalt; Freigeben/Ablehnen von Vorschlägen (Freigabe eines Vorschlags auf veralteter Basis ist erlaubt, der Unterschied wird gegen die *aktuelle* Version gezeigt). Bearbeiter: Vorschlag. Betrachter: lesen. Agenten: `.queue/knowledge.md` (vollständiger neuer Inhalt, ≤ 32 KiB, Secrets geschwärzt) ⇒ Vorschlag mit Auftrag; höchstens 5 offene Agentenvorschläge pro Projekt, ein Vorschlag pro Auftrag (spätere ersetzen den eigenen offenen). Prompt: `# Project rules (version N, approved by a human)` vor dem Auftrag, in Agenten-, Plan-, Review-, OnEmpty- und Einzelschritt-Prompts. Export: Eigentümer, `AGENTS.md` mit Kopfzeile `<!-- Verwaltet von Queue: Projekt-Regeln Version N -->`; `CLAUDE.md`/`GEMINI.md` nur, wenn nicht vorhanden oder bereits Queue-Brücke (sonst Hinweis, nichts überschrieben); Commit im Hauptrepo mit Secret-Scan; danach Baseline `agent-config` für diese Pfade auf den neuen Stand (Mensch hat freigegeben).

**Projektzustand (Schema v1).** `{"version":1,"features":[{"id","title","status":"todo|in_progress|done|blocked","passes":bool,"files":[…],"steps":[…],"notes"}],"progress":[{"at","task","text"}]}`. Limits: 64 KiB, 200 Einträge, id `^[a-z0-9][a-z0-9._-]{0,63}$`, Titel 200, Notizen 2000, 20 Dateien/Schritte je ≤ 300, Log 50 Einträge je ≤ 500, höchstens 5 neue Log-Einträge pro Lauf. Ablehnung (Vorschlag verworfen, alter Stand bleibt): kein JSON, falsche Version, zu groß, doppelte IDs, ungültige IDs/Felder. Reparatur (Vorschlag übernommen, Reparaturen protokolliert): gelöschte Einträge wiederhergestellt, Titel/IDs bestehender Einträge zurückgesetzt, Datei-Referenzen, die im Arbeitsbaum fehlen/aus ihm herausführen, entfernt, `passes:true` ohne `done` bzw. `done` bei gescheitertem Lauf zurückgesetzt, verbotene Übergänge (`done → todo`) zurückgesetzt, geänderte/gelöschte Log-Einträge wiederhergestellt, Überzählige gekürzt. Übernahme erst bei „erledigt“; Einträge, die parallel geändert wurden, gewinnt der frühere Stand (Konflikt protokolliert). Prompt: kompakte Liste (≤ 4 KiB) als Datenblock mit Nonce + Hinweis, wie aktualisiert wird; die Datei liegt vollständig in `.queue/state.json`.

**Dedupe/Critic.** Kandidaten (≤ 10) ⇒ Critic-Regeln (leerer/zu kurzer/zu langer Auftrag, Arbeit an CI/Tests/Secrets/Schutzpfaden ⇒ Freigabe) ⇒ Ähnlichkeit gegen offene (alle), erledigte/abgebrochene/abgelehnte (Fenster) und revertierte Aufträge sowie frühere Kandidaten desselben Laufs ⇒ `score ≥ Schwelle` verworfen (bei revertiert schon ab Prüfschwelle), `≥ Prüfschwelle` Freigabe nötig ⇒ optional LLM-Critic (nur „verwerfen“ oder „Freigabe“, nie „durchwinken“) ⇒ Rest wie bisher (Anzahl/Tiefe). Bericht am OnEmpty-Auftrag, Begründung am erzeugten Auftrag, Benachrichtigung nennt Verworfene.

**Anhänge.** Pro Datei 10 MiB, pro Auftrag 10 Dateien und 25 MiB, pro Nutzer 200 MiB (`ATTACHMENT_QUOTA_MB`), Bilder ≤ 40 Mio. Pixel, je Seite ≤ 10 000. Allowlist: PNG, JPEG, GIF, WebP (⇒ PNG/JPEG neu kodiert), PDF (Signatur `%PDF-`), Text (UTF-8 ohne NUL, Endungen txt/md/csv/json/log/diff/patch/yaml/toml). Dateiname: Anzeige ≤ 120 Zeichen ohne Steuerzeichen; Workspace-Name `NN-<ascii>.<ext aus Sniffing>` ⊂ `^[a-z0-9][a-z0-9._-]{0,79}$`. Rechte: Hochladen ab Bearbeiter (bei bestehendem Auftrag zusätzlich Ersteller oder Eigentümer, nicht während er läuft), Lesen alle Mitglieder (Entwürfe nur der Hochladende), Löschen Hochladender oder Eigentümer. Auslieferung `GET /api/attachments/{id}`: `Content-Disposition: attachment; filename=…; filename*=UTF-8''…`, `X-Content-Type-Options: nosniff`, `Content-Security-Policy: default-src 'none'; sandbox`, `Cache-Control: private, no-store`, Typ nur Bild-MIME oder `application/octet-stream`.

## 3. Risiken

| Risiko | Gegenmaßnahme |
|---|---|
| Dekompressionsbombe/Speicher beim Neu-Kodieren | `image::Limits` (Pixel, Allokation 256 MiB), Größe der Datei vorab, Dekodieren in `spawn_blocking` |
| Agent pflanzt Symlinks in `.queue/` | gleiche lstat-/create_new-Logik wie WP01, Ersetzen statt Folgen, Provisionierung nur ohne laufenden Prozess |
| Anhänge im Commit über `.gitignore`-Negation oder Agenten-Commit | `stage_all` an allen `add -A`-Stellen, Squash bei `.queue` in `base..HEAD`, fail-closed in `merge_prepare` |
| Agent schreibt „Regeln“, die später ungeprüft wirken | Repo-Anweisungsdateien im Standard abgeschaltet (neu für Codex/Gemini), mit Vertrauen unter Baseline/Gate; Prompt nur mit freigegebener Version |
| Zustand als Injektionskanal | als Datenblock mit Nonce, Regeln davor, Größenlimit |
| Falsch-positive Dedupe verwerfen gute Vorschläge | konservative Standard-Schwelle 0,6, Zwischenzone ⇒ Freigabe statt Verwerfen, Bericht sichtbar, Schwellen einstellbar |
| Unverifizierte CLI-Schalter (Codex/Gemini) | im README/Bericht als „aus Binary/Bundle abgeleitet, nicht ausgeführt“ gekennzeichnet; Unit-Tests prüfen Aufrufe |

## 4. Tests (Abnahmeumfang)

Unit: Sniffing/Allowlist/Fälschung, EXIF/GPS weg (JPEG mit GPS-IFD, PNG mit eXIf/tEXt), Namen, Zustand (jede Ablehnungs-/Reparaturregel), Ähnlichkeit/Critic, Prompt-Abschnitte, CLI-Schalter, Glob-Watch. Runner: Bereitstellung mit Symlink-Fallen, Zustandsprüfung gegen echten Baum, `stage_all` mit `!.queue/`-Negation, Squash von Agenten-Commits. Integration (`tests/knowledge.rs`, `tests/attachments.rs`): Upload-Limits/Quota/Rechte/Header, Bereitstellung im Lauf (Mock `SEEN-attachments`), Anhang nicht im Commit (Mock `MOCK_COMMIT_ATTACHMENT`), Vorschlag + Freigabe ⇒ im nächsten Prompt, ungültiger Zustand abgelehnt/repariert, Dedupe verhindert Duplikat. Playwright: System (API über Prozessgrenze), Desktop (Wissen, Zustand, Anhänge, Dedupe-Begründung), iPhone/Landscape (Kamera-Input per Tippen, Safe-Areas), axe.
