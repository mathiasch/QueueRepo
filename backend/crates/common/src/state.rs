//! WP09: structured project state (`.queue/state.json`, schema version 1).
//!
//! A JSON feature list with status and `passes` flags plus an append-only
//! progress log (report P1.8). Agents get it with every run and may update
//! it; the **runner** validates the result against the schema, the state the
//! run started with and the working tree ([`parse`], [`reconcile`]); the app
//! applies an accepted update only once the task is done ([`merge`], three-way
//! per entry against parallel tasks). Only humans restructure (delete,
//! rename) – an agent's deletion or renaming is repaired, not applied.
//!
//! Pure functions without I/O (the file check is a callback), shared by the
//! runner, the app and the WASM frontend.
use serde::{Deserialize, Serialize};

pub const STATE_VERSION: u32 = 1;
/// Largest `state.json` (bytes).
pub const MAX_STATE_BYTES: usize = 64 * 1024;
pub const MAX_FEATURES: usize = 200;
pub const MAX_TITLE: usize = 200;
pub const MAX_NOTES: usize = 2000;
pub const MAX_LIST: usize = 20;
pub const MAX_ITEM: usize = 300;
pub const MAX_PROGRESS: usize = 50;
pub const MAX_PROGRESS_TEXT: usize = 500;
/// New progress entries one run may append.
pub const MAX_NEW_PROGRESS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureStatus {
    Todo,
    InProgress,
    Done,
    Blocked,
}

impl FeatureStatus {
    pub const ALL: [FeatureStatus; 4] = [Self::Todo, Self::InProgress, Self::Done, Self::Blocked];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }

    /// Transitions an agent may make. A finished feature does not go back
    /// to "todo" silently: a regression is `in_progress` or `blocked`.
    pub fn agent_may_change_to(self, to: FeatureStatus) -> bool {
        !(self == Self::Done && to == Self::Todo)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    pub id: String,
    pub title: String,
    pub status: FeatureStatus,
    #[serde(default)]
    pub passes: bool,
    /// Repository files the feature lives in (relative paths).
    #[serde(default)]
    pub files: Vec<String>,
    /// Test steps / acceptance checks.
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressEntry {
    /// Set by the queue (RFC 3339); what an agent writes here is replaced.
    #[serde(default)]
    pub at: String,
    /// Short id of the task that wrote the entry (set by the queue).
    #[serde(default)]
    pub task: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectState {
    pub version: u32,
    #[serde(default)]
    pub features: Vec<Feature>,
    #[serde(default)]
    pub progress: Vec<ProgressEntry>,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            features: vec![],
            progress: vec![],
        }
    }
}

impl ProjectState {
    pub fn feature(&self, id: &str) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }

    /// Pretty JSON as it is placed in the workspace.
    pub fn to_file(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// JSON Schema of version 1 (written next to the state as
/// `.queue/state.schema.json`, so every CLI can check its own output).
pub const STATE_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Queue project state v1","type":"object","additionalProperties":false,"required":["version","features","progress"],"properties":{"version":{"const":1},"features":{"type":"array","maxItems":200,"items":{"type":"object","additionalProperties":false,"required":["id","title","status"],"properties":{"id":{"type":"string","pattern":"^[a-z0-9][a-z0-9._-]{0,63}$"},"title":{"type":"string","maxLength":200},"status":{"enum":["todo","in_progress","done","blocked"]},"passes":{"type":"boolean","description":"true only after the feature was verified (tests/run); requires status done"},"files":{"type":"array","maxItems":20,"items":{"type":"string","maxLength":300}},"steps":{"type":"array","maxItems":20,"items":{"type":"string","maxLength":300}},"notes":{"type":"string","maxLength":2000}}}},"progress":{"type":"array","maxItems":50,"items":{"type":"object","additionalProperties":false,"required":["text"],"properties":{"at":{"type":"string"},"task":{"type":"string"},"text":{"type":"string","maxLength":500}}}}}}"#;

/// Feature id: `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub fn valid_feature_id(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(x) if x.is_ascii_lowercase() || x.is_ascii_digit())
        && s.len() <= 64
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || matches!(x, '.' | '_' | '-'))
}

/// A file reference must be a plain relative path inside the repository,
/// outside `.git/` and `.queue/` (no `..`, no absolute path, no backslash).
pub fn valid_file_ref(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_ITEM
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains('\0')
        && !p.chars().any(|c| c.is_control())
        && p.split('/')
            .all(|seg| !seg.is_empty() && seg != ".." && seg != ".")
        && !matches!(p.split('/').next(), Some(".git") | Some(".queue"))
}

/// Parses an agent's (or a human's) state. `Err` = rejected (German).
pub fn parse(text: &str) -> Result<ProjectState, String> {
    if text.len() > MAX_STATE_BYTES {
        return Err(format!(
            "Zustand ist zu groß ({} Bytes, höchstens {MAX_STATE_BYTES})",
            text.len()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("Zustand ist kein gültiges JSON: {e}"))?;
    match v.get("version").and_then(|v| v.as_u64()) {
        Some(n) if n == STATE_VERSION as u64 => {}
        Some(n) => {
            return Err(format!(
                "Unbekannte Schema-Version {n} (erwartet {STATE_VERSION})"
            ))
        }
        None => return Err("Feld „version“ fehlt".into()),
    }
    let s: ProjectState = serde_json::from_value(v)
        .map_err(|e| format!("Zustand passt nicht zum Schema v{STATE_VERSION}: {e}"))?;
    check_shape(&s)?;
    Ok(s)
}

/// Schema rules beyond types: version, counts, ids, lengths.
pub fn check_shape(s: &ProjectState) -> Result<(), String> {
    if s.version != STATE_VERSION {
        return Err(format!("Unbekannte Schema-Version {}", s.version));
    }
    if s.features.len() > MAX_FEATURES {
        return Err(format!("Höchstens {MAX_FEATURES} Einträge"));
    }
    if s.progress.len() > MAX_PROGRESS {
        return Err(format!("Höchstens {MAX_PROGRESS} Fortschritts-Einträge"));
    }
    let mut seen = std::collections::HashSet::new();
    for f in &s.features {
        if !valid_feature_id(&f.id) {
            return Err(format!(
                "Ungültige ID „{}“ (Kleinbuchstaben, Ziffern, . _ -; höchstens 64 Zeichen)",
                clip(&f.id, 70)
            ));
        }
        if !seen.insert(f.id.as_str()) {
            return Err(format!("ID „{}“ kommt doppelt vor", f.id));
        }
        if f.title.trim().is_empty() || f.title.chars().count() > MAX_TITLE {
            return Err(format!(
                "„{}“: Titel fehlt oder ist länger als {MAX_TITLE} Zeichen",
                f.id
            ));
        }
        if f.notes.chars().count() > MAX_NOTES {
            return Err(format!(
                "„{}“: Notizen länger als {MAX_NOTES} Zeichen",
                f.id
            ));
        }
        for (what, list) in [("Dateien", &f.files), ("Schritte", &f.steps)] {
            if list.len() > MAX_LIST {
                return Err(format!("„{}“: höchstens {MAX_LIST} {what}", f.id));
            }
            if list
                .iter()
                .any(|x| x.trim().is_empty() || x.chars().count() > MAX_ITEM)
            {
                return Err(format!(
                    "„{}“: {what} leer oder länger als {MAX_ITEM} Zeichen",
                    f.id
                ));
            }
        }
    }
    for p in &s.progress {
        if p.text.trim().is_empty() || p.text.chars().count() > MAX_PROGRESS_TEXT {
            return Err(format!(
                "Fortschritts-Eintrag leer oder länger als {MAX_PROGRESS_TEXT} Zeichen"
            ));
        }
    }
    Ok(())
}

/// What the agent's run may do with the state.
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    /// The run finished successfully (result status `done`): only then may
    /// features become `done` / `passes`.
    pub allow_done: bool,
    /// RFC 3339 time for new progress entries.
    pub now: String,
    /// Short task id for new progress entries.
    pub task: String,
}

/// Result of [`reconcile`].
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciled {
    pub state: ProjectState,
    /// What was repaired (German, for the task log).
    pub repairs: Vec<String>,
    /// Anything changed compared with the previous state.
    pub changed: bool,
}

/// Applies the agent rules to an agent's state (already parsed) against the
/// state the run started with. `file_ok(path)` tells whether a referenced
/// file exists in the working tree (regular file or directory, no symlink
/// out of it).
pub fn reconcile(
    prev: &ProjectState,
    mut next: ProjectState,
    ctx: &RunContext,
    file_ok: &dyn Fn(&str) -> bool,
) -> Reconciled {
    let mut repairs = vec![];
    // 1. Deleted features come back (only humans delete).
    for (i, p) in prev.features.iter().enumerate() {
        if next.feature(&p.id).is_none() {
            let at = i.min(next.features.len());
            next.features.insert(at, p.clone());
            repairs.push(format!(
                "„{}“ wiederhergestellt: Einträge löscht nur ein Mensch",
                p.id
            ));
        }
    }
    if next.features.len() > MAX_FEATURES {
        // Re-adding may exceed the limit: drop the newest additions.
        let keep: std::collections::HashSet<&str> =
            prev.features.iter().map(|f| f.id.as_str()).collect();
        let mut extra = next.features.len() - MAX_FEATURES;
        let mut dropped = vec![];
        next.features.retain(|f| {
            if extra > 0 && !keep.contains(f.id.as_str()) {
                extra -= 1;
                dropped.push(f.id.clone());
                false
            } else {
                true
            }
        });
        dropped.reverse();
        if !dropped.is_empty() {
            repairs.push(format!(
                "Zu viele Einträge – nicht übernommen: {}",
                dropped.join(", ")
            ));
        }
    }
    for f in &mut next.features {
        let before = prev.feature(&f.id);
        // 2. Titles of existing features are the human's.
        if let Some(b) = before {
            if b.title != f.title {
                repairs.push(format!(
                    "„{}“: Titel zurückgesetzt (umbenennen nur durch Menschen)",
                    f.id
                ));
                f.title = b.title.clone();
            }
            // 3. Forbidden transitions.
            if !b.status.agent_may_change_to(f.status) {
                repairs.push(format!(
                    "„{}“: Übergang {} → {} nicht erlaubt, zurückgesetzt",
                    f.id,
                    b.status.as_str(),
                    f.status.as_str()
                ));
                f.status = b.status;
                f.passes = b.passes;
            }
        }
        // 4. "done"/"passes" only after a successful run.
        let was_done = before.is_some_and(|b| b.status == FeatureStatus::Done);
        let was_passing = before.is_some_and(|b| b.passes);
        if !ctx.allow_done {
            if f.status == FeatureStatus::Done && !was_done {
                f.status = before.map_or(FeatureStatus::InProgress, |b| b.status);
                if f.status == FeatureStatus::Done {
                    f.status = FeatureStatus::InProgress;
                }
                repairs.push(format!(
                    "„{}“: „erledigt“ nur nach erfolgreichem Lauf – zurückgesetzt",
                    f.id
                ));
            }
            if f.passes && !was_passing {
                f.passes = false;
                repairs.push(format!(
                    "„{}“: „besteht“ nur nach erfolgreichem Lauf – zurückgesetzt",
                    f.id
                ));
            }
        }
        if f.passes && f.status != FeatureStatus::Done {
            f.passes = false;
            repairs.push(format!(
                "„{}“: „besteht“ ohne Status „erledigt“ – zurückgesetzt",
                f.id
            ));
        }
        // 5. File references must exist in the working tree.
        let mut gone = vec![];
        f.files.retain(|p| {
            let ok = valid_file_ref(p) && file_ok(p);
            if !ok {
                gone.push(clip(p, 80));
            }
            ok
        });
        if !gone.is_empty() {
            repairs.push(format!(
                "„{}“: Datei-Verweise ohne Datei im Arbeitsbaum entfernt: {}",
                f.id,
                gone.join(", ")
            ));
        }
    }
    // 6. The progress log is append-only; the queue stamps new entries.
    let prev_texts: Vec<&str> = prev.progress.iter().map(|p| p.text.as_str()).collect();
    let is_prefix = next.progress.len() >= prev.progress.len()
        && next
            .progress
            .iter()
            .zip(&prev.progress)
            .all(|(a, b)| a.text == b.text);
    let mut new_entries: Vec<ProgressEntry> = if is_prefix {
        next.progress[prev.progress.len()..].to_vec()
    } else {
        repairs.push("Fortschritts-Log: frühere Einträge wiederhergestellt (nur anhängen)".into());
        next.progress
            .iter()
            .filter(|p| !prev_texts.contains(&p.text.as_str()))
            .cloned()
            .collect()
    };
    if new_entries.len() > MAX_NEW_PROGRESS {
        repairs.push(format!(
            "Fortschritts-Log: nur {MAX_NEW_PROGRESS} neue Einträge pro Lauf übernommen"
        ));
        new_entries.truncate(MAX_NEW_PROGRESS);
    }
    let mut progress = prev.progress.clone();
    for mut e in new_entries {
        e.at = ctx.now.clone();
        e.task = ctx.task.clone();
        progress.push(e);
    }
    if progress.len() > MAX_PROGRESS {
        let drop = progress.len() - MAX_PROGRESS;
        progress.drain(..drop);
    }
    next.progress = progress;
    next.version = STATE_VERSION;
    let changed = &next != prev;
    Reconciled {
        state: next,
        repairs,
        changed,
    }
}

/// Three-way application of a task's accepted state (`theirs`, based on
/// `base`) onto the current state (`ours`): per feature, a change of the
/// task wins only where the current state still equals the base; parallel
/// changes keep the current state (conflict). New progress entries are
/// appended. Returns the new state and the conflicts (German).
pub fn merge(
    base: &ProjectState,
    ours: &ProjectState,
    theirs: &ProjectState,
) -> (ProjectState, Vec<String>) {
    let mut out = ours.clone();
    let mut conflicts = vec![];
    for t in &theirs.features {
        match (base.feature(&t.id), ours.feature(&t.id)) {
            (None, None) => {
                if out.features.len() < MAX_FEATURES {
                    out.features.push(t.clone());
                } else {
                    conflicts.push(format!("„{}“: Höchstzahl an Einträgen erreicht", t.id));
                }
            }
            (None, Some(o)) => {
                if o != t {
                    conflicts.push(format!(
                        "„{}“ wurde parallel angelegt – bestehender Eintrag bleibt",
                        t.id
                    ));
                }
            }
            (Some(b), _) if b == t => {}
            (Some(_), None) => {
                conflicts.push(format!(
                    "„{}“ wurde inzwischen gelöscht – Änderung verworfen",
                    t.id
                ));
            }
            (Some(b), Some(o)) => {
                if o == b {
                    if let Some(slot) = out.features.iter_mut().find(|f| f.id == t.id) {
                        *slot = t.clone();
                    }
                } else if o != t {
                    conflicts.push(format!(
                        "„{}“ wurde parallel geändert – der neuere Stand bleibt",
                        t.id
                    ));
                }
            }
        }
    }
    let base_n = base.progress.len().min(theirs.progress.len());
    out.progress
        .extend(theirs.progress[base_n..].iter().cloned());
    if out.progress.len() > MAX_PROGRESS {
        let drop = out.progress.len() - MAX_PROGRESS;
        out.progress.drain(..drop);
    }
    (out, conflicts)
}

/// Compact listing for prompts (bounded).
pub fn summary(s: &ProjectState, max: usize) -> String {
    let mut out = String::new();
    for f in &s.features {
        let line = format!(
            "- {} [{}{}] {}\n",
            f.id,
            f.status.as_str(),
            if f.passes { ", passes" } else { "" },
            f.title
        );
        if out.len() + line.len() > max {
            out.push_str(&format!(
                "- … {} more (see the file)\n",
                s.features.len() - out.lines().count()
            ));
            break;
        }
        out.push_str(&line);
    }
    out
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feat(id: &str, status: FeatureStatus, passes: bool, files: &[&str]) -> Feature {
        Feature {
            id: id.into(),
            title: format!("Feature {id}"),
            status,
            passes,
            files: files.iter().map(|s| s.to_string()).collect(),
            steps: vec![],
            notes: String::new(),
        }
    }

    fn st(features: Vec<Feature>, progress: &[&str]) -> ProjectState {
        ProjectState {
            version: 1,
            features,
            progress: progress
                .iter()
                .map(|t| ProgressEntry {
                    at: "t0".into(),
                    task: "x".into(),
                    text: t.to_string(),
                })
                .collect(),
        }
    }

    fn ctx(ok: bool) -> RunContext {
        RunContext {
            allow_done: ok,
            now: "2026-11-12T00:00:00Z".into(),
            task: "abcd1234".into(),
        }
    }

    #[test]
    fn parse_rejects_junk_versions_unknown_fields_size_and_duplicates() {
        assert!(parse("nope").unwrap_err().contains("kein gültiges JSON"));
        assert!(parse(r#"{"features":[]}"#).unwrap_err().contains("version"));
        assert!(parse(r#"{"version":2,"features":[],"progress":[]}"#)
            .unwrap_err()
            .contains("Schema-Version 2"));
        assert!(parse(r#"{"version":1,"features":[],"progress":[],"x":1}"#)
            .unwrap_err()
            .contains("Schema"));
        let dup = r#"{"version":1,"features":[{"id":"a","title":"A","status":"todo"},{"id":"a","title":"B","status":"todo"}],"progress":[]}"#;
        assert!(parse(dup).unwrap_err().contains("doppelt"));
        let bad_id =
            r#"{"version":1,"features":[{"id":"A b","title":"A","status":"todo"}],"progress":[]}"#;
        assert!(parse(bad_id).unwrap_err().contains("Ungültige ID"));
        let bad_status = r#"{"version":1,"features":[{"id":"a","title":"A","status":"finished"}],"progress":[]}"#;
        assert!(parse(bad_status).is_err());
        let big = format!(
            r#"{{"version":1,"features":[],"progress":[],"pad":"{}"}}"#,
            "x".repeat(MAX_STATE_BYTES)
        );
        assert!(parse(&big).unwrap_err().contains("zu groß"));
        let many: Vec<String> = (0..=MAX_FEATURES)
            .map(|i| format!(r#"{{"id":"f{i}","title":"T","status":"todo"}}"#))
            .collect();
        let s = format!(
            r#"{{"version":1,"features":[{}],"progress":[]}}"#,
            many.join(",")
        );
        assert!(parse(&s).unwrap_err().contains("Höchstens"));
        let ok = r#"{"version":1,"features":[{"id":"login","title":"Login","status":"in_progress","files":["src/a.rs"]}],"progress":[{"text":"x"}]}"#;
        assert_eq!(parse(ok).unwrap().features[0].id, "login");
        // The schema constant itself is valid JSON.
        let _: serde_json::Value = serde_json::from_str(STATE_SCHEMA).unwrap();
    }

    #[test]
    fn file_refs_are_relative_and_inside_the_tree() {
        for ok in ["src/a.rs", "a", "docs/x y.md", ".github/workflows/ci.yml"] {
            assert!(valid_file_ref(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a//b",
            ".git/config",
            ".queue/attachments/x.png",
            "a\\b",
            "./a",
        ] {
            assert!(!valid_file_ref(bad), "{bad}");
        }
    }

    #[test]
    fn reconcile_repairs_deletions_renames_transitions_files_and_log() {
        let prev = st(
            vec![
                feat("a", FeatureStatus::Done, true, &["src/a.rs"]),
                feat("b", FeatureStatus::Todo, false, &[]),
            ],
            &["one", "two"],
        );
        let mut next = st(
            vec![
                // a: done → todo (forbidden), renamed
                Feature {
                    title: "Renamed".into(),
                    ..feat("a", FeatureStatus::Todo, false, &["src/a.rs"])
                },
                // c: new, references a missing file and a traversal
                feat(
                    "c",
                    FeatureStatus::InProgress,
                    false,
                    &["src/c.rs", "src/missing.rs", "../etc/passwd"],
                ),
            ],
            &["two-changed", "three"],
        );
        next.progress[0].at = "agent-time".into();
        let exists = |p: &str| matches!(p, "src/a.rs" | "src/c.rs");
        let r = reconcile(&prev, next, &ctx(true), &exists);
        let a = r.state.feature("a").unwrap();
        assert_eq!(
            (a.title.as_str(), a.status, a.passes),
            ("Feature a", FeatureStatus::Done, true)
        );
        assert!(r.state.feature("b").is_some(), "deleted feature restored");
        assert_eq!(r.state.features[1].id, "b", "restored at its old position");
        assert_eq!(r.state.feature("c").unwrap().files, vec!["src/c.rs"]);
        let texts: Vec<&str> = r.state.progress.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["one", "two", "two-changed", "three"]);
        assert_eq!(r.state.progress[3].at, "2026-11-12T00:00:00Z");
        assert_eq!(r.state.progress[3].task, "abcd1234");
        let all = r.repairs.join("\n");
        for needle in [
            "wiederhergestellt",
            "Titel zurückgesetzt",
            "done → todo",
            "src/missing.rs",
            "../etc/passwd",
            "nur anhängen",
        ] {
            assert!(all.contains(needle), "{needle} in {all}");
        }
        assert!(r.changed);
    }

    #[test]
    fn done_and_passes_only_after_a_successful_run() {
        let prev = st(vec![feat("a", FeatureStatus::InProgress, false, &[])], &[]);
        let next = st(
            vec![
                feat("a", FeatureStatus::Done, true, &[]),
                feat("n", FeatureStatus::Done, true, &[]),
            ],
            &[],
        );
        let r = reconcile(&prev, next.clone(), &ctx(false), &|_| true);
        assert_eq!(
            r.state.feature("a").unwrap().status,
            FeatureStatus::InProgress
        );
        assert!(!r.state.feature("a").unwrap().passes);
        assert_eq!(
            r.state.feature("n").unwrap().status,
            FeatureStatus::InProgress
        );
        let r = reconcile(&prev, next, &ctx(true), &|_| true);
        assert_eq!(r.state.feature("a").unwrap().status, FeatureStatus::Done);
        assert!(r.repairs.is_empty(), "{:?}", r.repairs);
        // passes without done is never kept.
        let next = st(vec![feat("a", FeatureStatus::InProgress, true, &[])], &[]);
        let r = reconcile(&prev, next, &ctx(true), &|_| true);
        assert!(!r.state.feature("a").unwrap().passes);
    }

    #[test]
    fn log_growth_is_bounded_and_unchanged_state_is_not_a_change() {
        let prev = st(vec![feat("a", FeatureStatus::Todo, false, &[])], &[]);
        let r = reconcile(&prev, prev.clone(), &ctx(true), &|_| true);
        assert!(!r.changed && r.repairs.is_empty());
        let texts: Vec<String> = (0..9).map(|i| format!("e{i}")).collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let r = reconcile(&prev, st(prev.features.clone(), &refs), &ctx(true), &|_| {
            true
        });
        assert_eq!(r.state.progress.len(), MAX_NEW_PROGRESS);
        let full: Vec<String> = (0..MAX_PROGRESS).map(|i| format!("p{i}")).collect();
        let refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
        let prev = st(vec![], &refs);
        let mut more = refs.clone();
        more.push("new");
        let r = reconcile(&prev, st(vec![], &more), &ctx(true), &|_| true);
        assert_eq!(r.state.progress.len(), MAX_PROGRESS);
        assert_eq!(r.state.progress.last().unwrap().text, "new");
        assert_eq!(r.state.progress[0].text, "p1");
    }

    #[test]
    fn merge_applies_per_feature_and_reports_parallel_changes() {
        let base = st(
            vec![
                feat("a", FeatureStatus::Todo, false, &[]),
                feat("b", FeatureStatus::Todo, false, &[]),
                feat("gone", FeatureStatus::Todo, false, &[]),
            ],
            &["x"],
        );
        // Meanwhile: b changed by someone else, "gone" deleted by a human.
        let ours = st(
            vec![
                feat("a", FeatureStatus::Todo, false, &[]),
                feat("b", FeatureStatus::Blocked, false, &[]),
                feat("other", FeatureStatus::Todo, false, &[]),
            ],
            &["x", "y"],
        );
        let theirs = st(
            vec![
                feat("a", FeatureStatus::Done, true, &[]),
                feat("b", FeatureStatus::InProgress, false, &[]),
                feat("gone", FeatureStatus::InProgress, false, &[]),
                feat("new", FeatureStatus::Todo, false, &[]),
            ],
            &["x", "z"],
        );
        let (m, conflicts) = merge(&base, &ours, &theirs);
        assert_eq!(m.feature("a").unwrap().status, FeatureStatus::Done);
        assert_eq!(m.feature("b").unwrap().status, FeatureStatus::Blocked);
        assert!(m.feature("gone").is_none());
        assert!(m.feature("new").is_some() && m.feature("other").is_some());
        let t: Vec<&str> = m.progress.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(t, vec!["x", "y", "z"]);
        assert_eq!(conflicts.len(), 2, "{conflicts:?}");
    }

    #[test]
    fn summary_is_bounded() {
        let s = st(
            (0..100)
                .map(|i| feat(&format!("f{i}"), FeatureStatus::Todo, false, &[]))
                .collect(),
            &[],
        );
        let out = summary(&s, 500);
        assert!(out.len() < 600, "{}", out.len());
        assert!(out.contains("more (see the file)"));
    }
}
