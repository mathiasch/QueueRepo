//! Gate findings (WP03): what the runner reports about a change before it
//! is committed or pushed, and the pure analysis of a diff.
use serde::{Deserialize, Serialize};

/// Why a change needs a human decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// Possible secret in an added line.
    Secret,
    /// Token format in the agent's output during the run.
    SecretInLog,
    /// Existing file under a protected path changed, renamed or deleted.
    ProtectedChanged,
    /// New skip/only/ignore marker in a test.
    TestSkipped,
    /// Test file deleted.
    TestDeleted,
    /// More assertions removed than added in a test file.
    AssertionsRemoved,
    /// Test runner/verifier configuration added or changed.
    VerifierConfig,
    /// Diff too large to check completely (fail-closed).
    TooLarge,
    /// Files the setup executes changed since the last approval.
    SetupFiles,
    /// Agent configuration of the repository changed since the last approval.
    AgentConfig,
    /// The review could not run.
    ReviewUnavailable,
    /// Push target is the default branch of the remote.
    PushProtected,
    /// The changes could not be applied any more (conflict).
    Conflict,
}

impl FindingKind {
    pub const ALL: [FindingKind; 13] = [
        Self::Secret,
        Self::SecretInLog,
        Self::ProtectedChanged,
        Self::TestSkipped,
        Self::TestDeleted,
        Self::AssertionsRemoved,
        Self::VerifierConfig,
        Self::TooLarge,
        Self::SetupFiles,
        Self::AgentConfig,
        Self::ReviewUnavailable,
        Self::PushProtected,
        Self::Conflict,
    ];

    pub fn is_secret(self) -> bool {
        matches!(self, Self::Secret | Self::SecretInLog)
    }
}

/// One finding. Never contains a secret in clear text: `detail` is masked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GateFinding {
    pub kind: FindingKind,
    /// Repository-relative path (empty for findings without a file).
    #[serde(default)]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Masked, human-readable detail (rule label, marker, counts …).
    #[serde(default)]
    pub detail: String,
    /// HMAC fingerprint of a secret value (approval allowlist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// What the runner checks in `POST /v1/workspaces/{id}/inspect`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InspectRequest {
    /// Base of the task (`snapshot_before`): protected paths and tests are
    /// compared against it (secrets only against HEAD; commits are scanned).
    pub base: String,
    /// Protected path globs ([`crate::glob`]).
    #[serde(default)]
    pub protected: Vec<String>,
    /// Detect skipped/deleted tests, removed assertions, verifier config.
    #[serde(default)]
    pub test_guard: bool,
    /// Approved secret fingerprints of the project.
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct InspectResult {
    pub findings: Vec<GateFinding>,
    /// Files changed against HEAD (0 = nothing to commit).
    pub changed_files: usize,
}

/// `POST /v1/workspaces/{id}/hold`: park the working tree under
/// `refs/queue/held/<task>` and reset it to HEAD.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HoldRequest {
    /// Task id (simple hex form, `[a-f0-9]{32}`).
    pub task: String,
    /// Task base: the held commit gets it as parent and the branch is reset
    /// to it, so commits the agent made itself are held as well.
    pub base: String,
}

/// `POST /v1/workspaces/{id}/release`: apply the held changes on top of
/// HEAD and commit them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseRequest {
    pub task: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    /// Approved secret fingerprints.
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CommitOutcome {
    /// HEAD after the call (unchanged when blocked).
    pub sha: String,
    /// Secrets outside the allowlist: nothing was committed.
    #[serde(default)]
    pub blocked: Vec<GateFinding>,
    /// Release only: the held changes conflict with later commits.
    #[serde(default)]
    pub conflict: bool,
}

/// Approved state of a set of files (`refs/queue/baseline/<name>`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineCheck {
    /// `setup` | `agent-config`
    pub name: String,
    /// Globs of the watched files.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BaselineResult {
    /// Commit the baseline points to (after a first-use initialisation: HEAD).
    pub baseline: String,
    pub head: String,
    /// Watched files changed between baseline and HEAD.
    pub changed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineApprove {
    pub name: String,
    /// Commit that was reviewed (never a later HEAD the human did not see).
    pub sha: String,
}

/// Valid baseline names.
pub fn valid_baseline(name: &str) -> bool {
    matches!(name, "setup" | "agent-config")
}

/// Valid task ids for held refs.
pub fn valid_task_ref(id: &str) -> bool {
    id.len() == 32
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Test file heuristic (independent of the configured protected paths).
pub fn is_test_path(path: &str) -> bool {
    const PATTERNS: [&str; 16] = [
        "tests/",
        "test/",
        "spec/",
        "__tests__/",
        "e2e/",
        "*_test.go",
        "*_test.py",
        "test_*.py",
        "*.test.*",
        "*.spec.*",
        "*Test.java",
        "*Tests.java",
        "*Test.kt",
        "*Tests.cs",
        "*_spec.rb",
        "*Test.php",
    ];
    crate::glob::matches_any(&PATTERNS, path)
}

/// Test runner / verifier configuration: flagged when added or changed.
pub fn is_verifier_config(path: &str) -> bool {
    const PATTERNS: [&str; 13] = [
        "conftest.py",
        "pytest.ini",
        "tox.ini",
        "jest.config.*",
        "vitest.config.*",
        "playwright.config.*",
        "karma.conf.*",
        ".mocharc*",
        "phpunit.xml*",
        ".config/nextest.toml",
        "nextest.toml",
        "noxfile.py",
        "cypress.config.*",
    ];
    crate::glob::matches_any(&PATTERNS, path)
}

/// Default protected paths (CI configuration and tests).
pub const DEFAULT_PROTECTED: [&str; 14] = [
    ".github/workflows/",
    ".gitlab-ci.yml",
    ".circleci/",
    "Jenkinsfile",
    "azure-pipelines.yml",
    "tests/",
    "test/",
    "__tests__/",
    "*_test.go",
    "*_test.py",
    "test_*.py",
    "*.test.*",
    "*.spec.*",
    "conftest.py",
];

/// Agent CLI configuration that can run code or redirect credentials when
/// loaded from the repository.
pub const AGENT_CONFIG_PATHS: [&str; 7] = [
    ".claude/",
    ".mcp.json",
    ".gemini/",
    "opencode.json",
    "opencode.jsonc",
    ".opencode/",
    ".codex/",
];

#[cfg(feature = "scan")]
pub use analysis::*;

#[cfg(feature = "scan")]
mod analysis {
    use std::sync::OnceLock;

    use regex::Regex;

    use super::*;
    use crate::secrets;

    /// One file of a `git diff -U0` (status from the header lines).
    #[derive(Debug, Default, Clone, PartialEq)]
    pub struct FileDiff {
        pub path: String,
        /// Source path of a deletion/rename (`--- a/…`).
        pub old_path: String,
        pub added: bool,
        pub deleted: bool,
        pub binary: bool,
        /// Hunks of added lines: (first new line number, lines).
        pub plus: Vec<(u32, Vec<String>)>,
        pub minus: Vec<String>,
    }

    fn unquote(p: &str) -> String {
        let p = p.trim();
        if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
            // C-style quoting of unusual names; good enough for matching.
            p[1..p.len() - 1]
                .replace("\\\"", "\"")
                .replace("\\\\", "\\")
        } else {
            p.to_string()
        }
    }

    /// Parses `git diff --no-color -U0 --src-prefix=a/ --dst-prefix=b/` output
    /// (also `git log -p` output; `commit` lines are ignored).
    pub fn parse_diff(diff: &str) -> Vec<FileDiff> {
        let mut files: Vec<FileDiff> = vec![];
        for line in diff.split('\n') {
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let mut f = FileDiff::default();
                // Fallback path from the header; overwritten by +++/---.
                if let Some(i) = rest.find(" b/") {
                    f.path = unquote(&rest[i + 3..]);
                    f.old_path = unquote(rest[..i].trim_start_matches("a/"));
                }
                files.push(f);
                continue;
            }
            let Some(f) = files.last_mut() else { continue };
            if line.starts_with("new file mode") {
                f.added = true;
            } else if line.starts_with("deleted file mode") {
                f.deleted = true;
            } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                f.binary = true;
            } else if let Some(p) = line.strip_prefix("+++ ") {
                if p != "/dev/null" {
                    f.path = unquote(p.trim_start_matches("b/"));
                }
            } else if let Some(p) = line.strip_prefix("--- ") {
                if p != "/dev/null" {
                    f.old_path = unquote(p.trim_start_matches("a/"));
                }
            } else if line.starts_with("@@") {
                // @@ -a,b +c,d @@
                let new_line = line
                    .split_whitespace()
                    .find(|t| t.starts_with('+'))
                    .and_then(|t| t[1..].split(',').next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                f.plus.push((new_line, vec![]));
            } else if let Some(text) = line.strip_prefix('+') {
                if let Some((_, h)) = f.plus.last_mut() {
                    h.push(text.to_string());
                }
            } else if let Some(text) = line.strip_prefix('-') {
                f.minus.push(text.to_string());
            }
        }
        for f in files.iter_mut() {
            f.plus.retain(|(_, lines)| !lines.is_empty());
        }
        if files.iter().any(|f| f.deleted) {
            for f in files.iter_mut().filter(|f| f.deleted && f.path.is_empty()) {
                f.path = f.old_path.clone();
            }
        }
        files
    }

    /// Secrets in added lines (each hunk scanned as one block, so multi-line
    /// private keys are caught with their body). Fingerprints use `key`.
    pub fn secret_findings(files: &[FileDiff], key: &[u8], allow: &[String]) -> Vec<GateFinding> {
        let mut out = vec![];
        for f in files {
            for (start, lines) in &f.plus {
                let block = lines.join("\n");
                for h in secrets::scan(&block) {
                    let fp = secrets::fingerprint(key, h.rule, h.value(&block));
                    if allow.contains(&fp) {
                        continue;
                    }
                    let line_no = start + block[..h.start].matches('\n').count() as u32;
                    let finding = GateFinding {
                        kind: FindingKind::Secret,
                        path: f.path.clone(),
                        line: Some(line_no),
                        detail: h.masked(&block),
                        fingerprint: Some(fp),
                    };
                    if !out.contains(&finding) {
                        out.push(finding);
                    }
                }
            }
        }
        out
    }

    fn skip_markers() -> &'static Vec<(Regex, &'static str)> {
        static RE: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        RE.get_or_init(|| {
            [
                (r"#\[\s*ignore\b", "#[ignore]"),
                (
                    r"@pytest\.mark\.(skip|skipif|xfail)\b",
                    "@pytest.mark.skip/xfail",
                ),
                (r"\bpytest\.(skip|xfail)\s*\(", "pytest.skip()/xfail()"),
                (
                    r"@unittest\.(skip|skipIf|skipUnless|expectedFailure)\b",
                    "@unittest.skip",
                ),
                (r"\bself\.skipTest\s*\(", "self.skipTest()"),
                (
                    r"\b(it|test|describe|context|suite|specify)\.(skip|only|todo|fixme)\s*\(",
                    ".skip()/.only()",
                ),
                (
                    r"\b(xit|xtest|xdescribe|xcontext|fit|fdescribe)\s*\(",
                    "xit()/fit()",
                ),
                (r"\b[tb]\.Skip(Now|f)?\s*\(", "t.Skip()"),
                (r"@(Disabled|Ignore)\b", "@Disabled/@Ignore"),
                (r"\[\s*(Ignore|Fact\s*\(\s*Skip\s*=)", "[Ignore]/Skip="),
                (r"^\s*(skip|pending)\b", "skip/pending"),
                (r"\bmarkTest(Skipped|Incomplete)\s*\(", "markTestSkipped()"),
            ]
            .into_iter()
            .map(|(r, l)| (Regex::new(r).expect("static marker"), l))
            .collect()
        })
    }

    fn assertion_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| {
            Regex::new(
                r"\b(assert\w*!?\s*\(|assert\s|expect\s*\(|\.should\b|\.to(Be|Equal|Have)\w*\s*\(|self\.assert\w+\s*\(|require\.\w+\s*\(|t\.(Error|Fatal)f?\s*\()",
            )
            .expect("static assertion regex")
        })
    }

    fn is_code(path: &str) -> bool {
        const EXT: [&str; 21] = [
            "rs", "py", "js", "jsx", "ts", "tsx", "mjs", "cjs", "go", "java", "kt", "kts", "rb",
            "php", "cs", "swift", "scala", "ex", "exs", "dart", "vue",
        ];
        path.rsplit_once('.')
            .is_some_and(|(_, e)| EXT.contains(&e.to_ascii_lowercase().as_str()))
    }

    /// Protected paths and weakened tests against the task's base.
    pub fn change_findings(
        files: &[FileDiff],
        protected: &[String],
        test_guard: bool,
    ) -> Vec<GateFinding> {
        let mut out = vec![];
        for f in files {
            let touched_existing = !f.added;
            let path = if f.deleted { &f.old_path } else { &f.path };
            let renamed = !f.added && !f.deleted && !f.old_path.is_empty() && f.old_path != f.path;
            if touched_existing
                && (crate::glob::matches_any(protected, path)
                    || (renamed && crate::glob::matches_any(protected, &f.old_path)))
            {
                out.push(GateFinding {
                    kind: FindingKind::ProtectedChanged,
                    path: path.clone(),
                    line: None,
                    detail: if f.deleted {
                        "gelöscht".into()
                    } else if renamed {
                        format!("umbenannt von {}", f.old_path)
                    } else {
                        "geändert".into()
                    },
                    fingerprint: None,
                });
            }
            if !test_guard {
                continue;
            }
            let test_file = is_test_path(path) || crate::glob::matches_any(protected, path);
            if f.deleted && test_file {
                out.push(GateFinding {
                    kind: FindingKind::TestDeleted,
                    path: path.clone(),
                    line: None,
                    detail: "Testdatei gelöscht".into(),
                    fingerprint: None,
                });
                continue;
            }
            if is_verifier_config(path) {
                out.push(GateFinding {
                    kind: FindingKind::VerifierConfig,
                    path: path.clone(),
                    line: None,
                    detail: if f.added {
                        "neu angelegt".into()
                    } else {
                        "geändert".into()
                    },
                    fingerprint: None,
                });
            }
            if is_code(path) {
                for (start, lines) in &f.plus {
                    for (i, l) in lines.iter().enumerate() {
                        if let Some((_, label)) =
                            skip_markers().iter().find(|(re, _)| re.is_match(l))
                        {
                            // `skip`/`pending` at line start only in Ruby specs.
                            if *label == "skip/pending" && !path.ends_with(".rb") {
                                continue;
                            }
                            out.push(GateFinding {
                                kind: FindingKind::TestSkipped,
                                path: path.clone(),
                                line: Some(start + i as u32),
                                detail: format!("{label}: {}", short(&secrets::redact(l.trim()))),
                                fingerprint: None,
                            });
                        }
                    }
                }
            }
            if test_file && !f.added {
                let removed = f
                    .minus
                    .iter()
                    .filter(|l| assertion_re().is_match(l))
                    .count();
                let added: usize = f
                    .plus
                    .iter()
                    .flat_map(|(_, ls)| ls)
                    .filter(|l| assertion_re().is_match(l))
                    .count();
                if removed > added {
                    out.push(GateFinding {
                        kind: FindingKind::AssertionsRemoved,
                        path: path.clone(),
                        line: None,
                        detail: format!("{removed} Prüfungen entfernt, {added} hinzugefügt"),
                        fingerprint: None,
                    });
                }
            }
        }
        out
    }

    /// `scripts` of a `package.json` changed (e.g. `"test": "exit 0"`).
    pub fn package_scripts_changed(old: &str, new: &str) -> bool {
        let scripts = |s: &str| {
            serde_json::from_str::<serde_json::Value>(s)
                .ok()
                .map(|v| v.get("scripts").cloned().unwrap_or(serde_json::Value::Null))
        };
        scripts(old) != scripts(new)
    }

    fn short(s: &str) -> String {
        if s.chars().count() <= 120 {
            s.to_string()
        } else {
            format!("{}…", s.chars().take(120).collect::<String>())
        }
    }
}

#[cfg(all(test, feature = "scan"))]
mod tests {
    use super::*;
    use crate::secrets::testdata;

    fn diff_of(files: &[(&str, &str, &[&str], &[&str])]) -> String {
        // (path, status a|m|d, plus lines, minus lines)
        let mut d = String::new();
        for (p, s, plus, minus) in files {
            d.push_str(&format!("diff --git a/{p} b/{p}\n"));
            match *s {
                "a" => d.push_str(&format!("new file mode 100644\n--- /dev/null\n+++ b/{p}\n")),
                "d" => d.push_str(&format!(
                    "deleted file mode 100644\n--- a/{p}\n+++ /dev/null\n"
                )),
                _ => d.push_str(&format!("--- a/{p}\n+++ b/{p}\n")),
            }
            if !minus.is_empty() {
                d.push_str(&format!("@@ -1,{} +0,0 @@\n", minus.len()));
                for m in *minus {
                    d.push_str(&format!("-{m}\n"));
                }
            }
            if !plus.is_empty() {
                d.push_str(&format!("@@ -0,0 +10,{} @@\n", plus.len()));
                for l in *plus {
                    d.push_str(&format!("+{l}\n"));
                }
            }
        }
        d
    }

    fn protected() -> Vec<String> {
        DEFAULT_PROTECTED.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_status_paths_and_line_numbers() {
        let d = diff_of(&[
            ("src/a.rs", "m", &["x", "y"], &["z"]),
            ("new.txt", "a", &["n"], &[]),
            ("tests/old.py", "d", &[], &["assert 1"]),
        ]);
        let f = parse_diff(&d);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].path, "src/a.rs");
        assert_eq!(
            f[0].plus,
            vec![(10, vec!["x".to_string(), "y".to_string()])]
        );
        assert_eq!(f[0].minus, vec!["z"]);
        assert!(f[1].added && !f[1].deleted);
        assert!(f[2].deleted);
        assert_eq!(f[2].path, "tests/old.py");
        let bin = "diff --git a/x.png b/x.png\nnew file mode 100644\nBinary files /dev/null and b/x.png differ\n";
        let b = parse_diff(bin);
        assert!(b[0].binary && b[0].added);
        assert_eq!(b[0].path, "x.png");
    }

    #[test]
    fn secrets_in_added_lines_with_line_and_allowlist() {
        let gh = testdata::github(4);
        let line = format!("TOKEN = \"{gh}\"");
        let d = diff_of(&[("cfg/settings.py", "a", &["# config", &line], &[])]);
        let files = parse_diff(&d);
        let f = secret_findings(&files, b"k", &[]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::Secret);
        assert_eq!(f[0].line, Some(11));
        assert_eq!(f[0].path, "cfg/settings.py");
        assert!(!f[0].detail.contains(&gh[5..]), "{}", f[0].detail);
        assert!(!serde_json::to_string(&f).unwrap().contains(&gh));
        let fp = f[0].fingerprint.clone().unwrap();
        assert!(secret_findings(&files, b"k", &[fp]).is_empty());
        // Removed lines are not scanned.
        let d2 = diff_of(&[("x.py", "m", &[], &[&line])]);
        assert!(secret_findings(&parse_diff(&d2), b"k", &[]).is_empty());
    }

    #[test]
    fn protected_changes_but_not_new_files() {
        let d = diff_of(&[
            ("tests/test_core.py", "m", &["x = 1"], &["x = 2"]),
            ("tests/test_new.py", "a", &["def test_x(): assert f()"], &[]),
            (".github/workflows/ci.yml", "d", &[], &["on: push"]),
            ("src/lib.rs", "m", &["fn a() {}"], &[]),
        ]);
        let f = change_findings(&parse_diff(&d), &protected(), false);
        let kinds: Vec<_> = f
            .iter()
            .map(|x| (x.kind, x.path.as_str(), x.detail.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (
                    FindingKind::ProtectedChanged,
                    "tests/test_core.py",
                    "geändert"
                ),
                (
                    FindingKind::ProtectedChanged,
                    ".github/workflows/ci.yml",
                    "gelöscht"
                ),
            ]
        );
        assert!(change_findings(&parse_diff(&d), &[], false).is_empty());
    }

    #[test]
    fn test_weakening_markers() {
        let d = diff_of(&[
            ("src/lib.rs", "m", &["    #[ignore]", "fn x() {}"], &[]),
            (
                "web/a.spec.ts",
                "a",
                &["test.only('x', () => {})", "it.skip('y')"],
                &[],
            ),
            ("pkg/x_test.go", "a", &["\tt.Skip(\"later\")"], &[]),
            (
                "tests/t.py",
                "a",
                &["@pytest.mark.xfail", "@unittest.skip('no')"],
                &[],
            ),
            ("README.md", "m", &["use it.skip( to skip"], &[]),
            ("lib/x.rb", "m", &["  skip 'flaky'"], &[]),
            ("lib/x.py", "m", &["skip = True"], &[]),
        ]);
        let f = change_findings(&parse_diff(&d), &[], true);
        let skipped: Vec<_> = f
            .iter()
            .filter(|x| x.kind == FindingKind::TestSkipped)
            .map(|x| (x.path.as_str(), x.line))
            .collect();
        assert_eq!(
            skipped,
            vec![
                ("src/lib.rs", Some(10)),
                ("web/a.spec.ts", Some(10)),
                ("web/a.spec.ts", Some(11)),
                ("pkg/x_test.go", Some(10)),
                ("tests/t.py", Some(10)),
                ("tests/t.py", Some(11)),
                ("lib/x.rb", Some(10)),
            ]
        );
    }

    #[test]
    fn deleted_tests_removed_assertions_and_verifier_config() {
        let d = diff_of(&[
            ("tests/test_a.py", "d", &[], &["def test(): assert 1"]),
            (
                "src/math.test.ts",
                "m",
                &["  // trust me"],
                &["  expect(add(1, 2)).toBe(3)", "  expect(add(0, 0)).toBe(0)"],
            ),
            ("conftest.py", "a", &["import pytest"], &[]),
            ("src/util.ts", "m", &[], &["expect(x)"]),
        ]);
        let f = change_findings(&parse_diff(&d), &[], true);
        let kinds: Vec<_> = f.iter().map(|x| (x.kind, x.path.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (FindingKind::TestDeleted, "tests/test_a.py"),
                (FindingKind::AssertionsRemoved, "src/math.test.ts"),
                (FindingKind::VerifierConfig, "conftest.py"),
            ]
        );
        assert_eq!(f[1].detail, "2 Prüfungen entfernt, 0 hinzugefügt");
        // Without the test guard only protected paths count.
        assert!(change_findings(&parse_diff(&d), &[], false).is_empty());
    }

    #[test]
    fn package_scripts() {
        assert!(!package_scripts_changed(
            r#"{"scripts":{"test":"jest"},"dependencies":{"a":"1"}}"#,
            r#"{"dependencies":{"a":"2"},"scripts":{"test":"jest"}}"#
        ));
        assert!(package_scripts_changed(
            r#"{"scripts":{"test":"jest"}}"#,
            r#"{"scripts":{"test":"exit 0"}}"#
        ));
    }

    #[test]
    fn helpers() {
        assert!(is_test_path("a/b/tests/x.rs"));
        assert!(is_test_path("x_test.go"));
        assert!(!is_test_path("src/testing.rs"));
        assert!(is_verifier_config("web/jest.config.js"));
        assert!(valid_task_ref(&"a".repeat(32)));
        assert!(!valid_task_ref(&"A".repeat(32)));
        assert!(!valid_task_ref("../x"));
        assert!(valid_baseline("setup") && !valid_baseline("x"));
        let all: std::collections::HashSet<_> = FindingKind::ALL.iter().collect();
        assert_eq!(all.len(), FindingKind::ALL.len());
    }
}
