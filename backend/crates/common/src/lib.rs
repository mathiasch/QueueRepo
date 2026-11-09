//! Wire protocol between `queued` (orchestrator, no host privileges) and
//! `queued-runner` (owns workspaces + git, starts sandboxed agent processes).
//!
//! Transport: HTTP/JSON with `Authorization: Bearer <RUNNER_TOKEN>`.
//! `POST /v1/exec` answers with a stream of NDJSON [`ExecFrame`]s.
use serde::{Deserialize, Serialize};

pub mod gate;
pub mod glob;
pub mod net;
#[cfg(feature = "scan")]
pub mod secrets;
pub use gate::{CommitOutcome, FindingKind, GateFinding};
pub use net::{EgressPolicy, EgressVerdict, NetMode};

pub const API_PREFIX: &str = "/v1";

/// Git remote access for clone / pull / push. The token is only sent as an
/// HTTP header for the single git invocation, never written to disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GitRemote {
    pub url: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitWorkspace {
    #[serde(default)]
    pub remote: Option<GitRemote>,
    /// Branch for a fresh repository (ignored when cloning).
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginTask {
    /// Pull (fast-forward only) before the task if set.
    #[serde(default)]
    pub remote: Option<GitRemote>,
    /// Approved secret fingerprints of the project: leftovers are committed
    /// as WIP only if they contain no other secret (else quarantined).
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginResult {
    pub head: String,
    /// Non-fatal notes (e.g. "pull failed: …").
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRequest {
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    /// Approved secret fingerprints; any other secret blocks the commit.
    #[serde(default)]
    pub allow: Vec<String>,
    /// A human approved changes too large to scan completely.
    #[serde(default)]
    pub allow_large: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sha {
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeRequest {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffResult {
    pub diff: String,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueFile {
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub files: usize,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRequest {
    pub remote: GitRemote,
    /// Push exactly this commit (default: HEAD).
    #[serde(default)]
    pub sha: Option<String>,
    /// A human approved pushing to the remote's default branch.
    #[serde(default)]
    pub allow_protected: bool,
    /// Approved secret fingerprints for the scan of the pushed commits.
    #[serde(default)]
    pub allow: Vec<String>,
    /// A human approved the push as a whole (skips the secret scan).
    #[serde(default)]
    pub allow_secrets: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    pub ok: bool,
    pub output: String,
    /// Nothing was pushed: the push needs a human decision.
    #[serde(default)]
    pub held: Vec<GateFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// One sandboxed process execution inside a project workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    /// Unique run name (also the container name), `[a-z0-9-]`.
    pub name: String,
    pub workspace: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Passed via environment only (never argv).
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Files written into the run's private $HOME (relative path, content).
    #[serde(default)]
    pub home_files: Vec<(String, String)>,
    /// Home-relative files whose content is returned in [`ExecFrame::Exit`].
    #[serde(default)]
    pub read_back: Vec<String>,
    #[serde(default)]
    pub stdin: Option<String>,
    pub timeout_secs: u64,
    /// Stop the run when it produced no stdout/stderr line for this many
    /// seconds (stall detection). `None` = off.
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    /// Files written to `<workspace>/.queue/<name>` before the start (e.g. the
    /// result JSON schema for CLIs that take it as a file). Names must pass
    /// [`valid_queue_file_name`].
    #[serde(default)]
    pub queue_files: Vec<(String, String)>,
    /// Network of the process (default: none at all).
    #[serde(default)]
    pub egress: EgressPolicy,
    /// Dependency cache mounted read-write (setup phase only).
    #[serde(default)]
    pub cache: Option<CacheSpec>,
    /// Names of `env` entries that are secrets: their values are replaced by
    /// `***` in output lines, and the workspace (and cache) is searched for
    /// them after the run ([`ExecFrame::Exit::leaks`]).
    #[serde(default)]
    pub secret_env: Vec<String>,
    /// Variables whose value is a path inside the run's private $HOME
    /// (`(name, home-relative path)`); the runner prefixes the home path the
    /// process sees (container or host).
    #[serde(default)]
    pub home_env: Vec<(String, String)>,
}

/// Dependency cache of a setup run. The runner derives the key from
/// `key_material` and the content of `files` (workspace-relative lock files;
/// missing files count as absent) and mounts `caches/<workspace>/<key>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CacheSpec {
    pub key_material: String,
    #[serde(default)]
    pub files: Vec<String>,
    /// Workspace whose caches are used (WP04: the project's main workspace,
    /// shared by all task workspaces; `None` = the run's own workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheInfo {
    /// Hex prefix of the cache key.
    pub key: String,
    /// The cache existed before this run.
    pub hit: bool,
}

/// `GET /v1/info`: what the runner can enforce.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunnerInfo {
    /// podman | docker | none
    pub engine: String,
    /// Containers have no network of their own; only the egress proxy.
    /// `false` for engine `none` (rules are advisory there).
    pub egress_enforced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ExecFrame {
    Out {
        line: String,
    },
    Err {
        line: String,
    },
    Exit {
        success: bool,
        code: Option<i32>,
        timed_out: bool,
        cancelled: bool,
        #[serde(default)]
        files: Vec<(String, String)>,
        /// Stopped by the runner because no output arrived for `idle_timeout_secs`.
        #[serde(default)]
        stalled: bool,
        /// The process ignored SIGINT for the grace period and was SIGKILLed.
        #[serde(default)]
        killed: bool,
        #[serde(default)]
        cache: Option<CacheInfo>,
        /// Secret values found in files after the run (`"NAME in path"`).
        #[serde(default)]
        leaks: Vec<String>,
    },
    /// Decision of the egress proxy for one connection/request.
    Egress {
        host: String,
        port: u16,
        /// CONNECT, or the HTTP method of a plain/inspected request.
        method: String,
        verdict: EgressVerdict,
    },
    /// The runner could not start/finish the process.
    Fail {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Workspace id of a task's own working tree (WP04): `t` + the 32 hex
/// digits of the task id. Never a project workspace (those are UUIDs).
pub fn task_workspace_id(task_hex: &str) -> String {
    format!("t{task_hex}")
}

/// `t<32 lowercase hex>`.
pub fn is_task_workspace(id: &str) -> bool {
    id.len() == 33
        && id.starts_with('t')
        && id[1..]
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

/// Branch of a task: `queue/` + the first 12 hex digits of its id (or all
/// 32 when the short name is taken in the project).
pub fn task_branch(task_hex: &str, long: bool) -> String {
    let n = if long { 32 } else { 12 };
    format!("queue/{}", &task_hex[..n.min(task_hex.len())])
}

/// `queue/<12..32 lowercase hex>`.
pub fn valid_task_branch(b: &str) -> bool {
    b.strip_prefix("queue/").is_some_and(|h| {
        (12..=32).contains(&h.len())
            && h.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    })
}

/// Changes of another task that a new task workspace starts with (fix
/// tasks continue the work of the task they fix): `base..tip` is applied
/// 3-way on top of the current target branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Carry {
    pub base: String,
    pub tip: String,
    /// Commit message line of the carried commit.
    pub label: String,
}

/// `POST /v1/workspaces/{main}/tasks/{task_ws}`: creates (or continues) the
/// task's own working tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskWorkspaceRequest {
    pub branch: String,
    /// Known merge base of the branch (continuation of an earlier run).
    #[serde(default)]
    pub merge_base: Option<String>,
    #[serde(default)]
    pub carry: Option<Carry>,
    /// Approved secret fingerprints (leftovers are committed as WIP only
    /// without other secrets, else quarantined – like `begin`).
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TaskWorkspaceResult {
    pub head: String,
    /// Target-branch commit the task branch is based on.
    pub merge_base: String,
    /// The working tree was created now (not continued).
    pub created: bool,
    /// Earlier work of the task continues (existing working tree, or the
    /// task branch of the main repository).
    #[serde(default)]
    pub resumed: bool,
    /// Commit with the carried changes (`None` = nothing carried).
    #[serde(default)]
    pub carried: Option<String>,
    /// Files with conflict markers in the carried commit.
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// `POST …/tasks/{task_ws}/sync`: copies exactly `sha` from the task's
/// working tree to `refs/heads/<branch>` in the main repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncRequest {
    pub branch: String,
    pub sha: String,
}

/// `POST …/tasks/{task_ws}/merge`: squashes `base..tip` onto the target
/// branch (3-way, no working tree) and re-checks the result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeRequest {
    pub branch: String,
    pub base: String,
    pub tip: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    #[serde(default)]
    pub protected: Vec<String>,
    #[serde(default)]
    pub test_guard: bool,
    #[serde(default)]
    pub allow: Vec<String>,
    /// Check the merged commit out in the task's working tree when the
    /// target moved (verify runs there).
    #[serde(default)]
    pub checkout: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MergeResult {
    /// Merged commit (parent = `target`); `None` on conflicts.
    #[serde(default)]
    pub commit: Option<String>,
    /// Target-branch commit the merge is based on.
    pub target: String,
    /// The target branch moved since `base` (verify again).
    pub moved: bool,
    /// Files the 3-way merge could not combine.
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Files whose added lines still contain conflict markers.
    #[serde(default)]
    pub markers: Vec<String>,
    /// Gate findings of the merged change that the task's own change did
    /// not have (fail-closed re-check; normally empty).
    #[serde(default)]
    pub new_findings: Vec<GateFinding>,
    /// The target branch already contains the change (nothing to merge).
    #[serde(default)]
    pub empty: bool,
}

/// `POST …/tasks/{task_ws}/merge/finish`: fast-forward of the target branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeFinish {
    pub branch: String,
    pub commit: String,
    pub target: String,
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MergeFinishResult {
    /// `false`: the target branch moved meanwhile (merge again).
    pub merged: bool,
    pub head: String,
}

/// Name of a file the app may place in the workspace's `.queue/` directory:
/// `^[a-z0-9][a-z0-9._-]{0,63}$` without `..` (no paths, no traversal).
pub fn valid_queue_file_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.len() <= 64
        && !s.contains("..")
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// One CLI found (or not) in the agent sandbox (`GET /v1/cli-status`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CliProbe {
    pub cli: String,
    /// First line of `<cli> --version`, `None` = not installed or failed.
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip_as_ndjson() {
        let frames = vec![
            ExecFrame::Out {
                line: "hello".into(),
            },
            ExecFrame::Err {
                line: "warn".into(),
            },
            ExecFrame::Exit {
                success: true,
                code: Some(0),
                timed_out: false,
                cancelled: false,
                files: vec![("a".into(), "b".into())],
                stalled: false,
                killed: false,
                cache: Some(CacheInfo {
                    key: "ab12".into(),
                    hit: true,
                }),
                leaks: vec!["T in .npmrc".into()],
            },
            ExecFrame::Exit {
                success: false,
                code: None,
                timed_out: false,
                cancelled: false,
                files: vec![],
                stalled: true,
                killed: true,
                cache: None,
                leaks: vec![],
            },
            ExecFrame::Egress {
                host: "x.com".into(),
                port: 443,
                method: "CONNECT".into(),
                verdict: EgressVerdict::NotAllowed,
            },
            ExecFrame::Fail { error: "x".into() },
        ];
        for f in frames {
            let line = serde_json::to_string(&f).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(serde_json::from_str::<ExecFrame>(&line).unwrap(), f);
        }
        assert_eq!(
            serde_json::to_string(&ExecFrame::Out { line: "x".into() }).unwrap(),
            r#"{"t":"out","line":"x"}"#
        );
    }

    #[test]
    fn token_is_not_serialized_when_absent() {
        let r = GitRemote {
            url: "https://x".into(),
            branch: "main".into(),
            token: None,
        };
        assert!(!serde_json::to_string(&r).unwrap().contains("token"));
    }

    #[test]
    fn exit_frame_from_older_runner_still_parses() {
        let f: ExecFrame = serde_json::from_str(
            r#"{"t":"exit","success":true,"code":0,"timed_out":false,"cancelled":false}"#,
        )
        .unwrap();
        assert_eq!(
            f,
            ExecFrame::Exit {
                success: true,
                code: Some(0),
                timed_out: false,
                cancelled: false,
                files: vec![],
                stalled: false,
                killed: false,
                cache: None,
                leaks: vec![],
            }
        );
    }

    #[test]
    fn exec_request_new_fields_default_and_roundtrip() {
        let old: ExecRequest =
            serde_json::from_str(r#"{"name":"n","workspace":"w","program":"sh","timeout_secs":5}"#)
                .unwrap();
        assert_eq!(old.idle_timeout_secs, None);
        assert!(old.queue_files.is_empty());
        // A caller that sends no policy gets no network.
        assert_eq!(old.egress, EgressPolicy::default());
        assert!(old.cache.is_none() && old.secret_env.is_empty());
        let mut r = old.clone();
        r.idle_timeout_secs = Some(30);
        r.queue_files = vec![("result.schema.json".into(), "{}".into())];
        let back: ExecRequest = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.idle_timeout_secs, Some(30));
        assert_eq!(back.queue_files, r.queue_files);
    }

    #[test]
    fn validates_queue_file_names() {
        for ok in [
            "result.schema.json",
            "review.schema.json",
            "a",
            "x_1-2.json",
        ] {
            assert!(valid_queue_file_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "../x",
            "a/b",
            "a..b",
            "Result.json",
            "-x",
            "a b",
            &"a".repeat(65),
        ] {
            assert!(!valid_queue_file_name(bad), "{bad}");
        }
        assert!(valid_queue_file_name(&"a".repeat(64)));
    }

    #[test]
    fn task_workspaces_and_branches() {
        let hex = "0123456789abcdef0123456789abcdef";
        let ws = task_workspace_id(hex);
        assert!(is_task_workspace(&ws) && valid_name(&ws));
        for bad in [
            "t0123",
            "0123456789abcdef0123456789abcdef0",
            "tg123456789abcdef0123456789abcdef",
            "T0123456789abcdef0123456789abcdef",
            "1f0e0a6c-1111-4c3a-8f0e-0123456789ab",
        ] {
            assert!(!is_task_workspace(bad), "{bad}");
        }
        assert_eq!(task_branch(hex, false), "queue/0123456789ab");
        assert_eq!(task_branch(hex, true), format!("queue/{hex}"));
        assert!(valid_task_branch(&task_branch(hex, false)));
        assert!(valid_task_branch(&task_branch(hex, true)));
        for bad in [
            "main",
            "queue/",
            "queue/0123",
            "queue/0123456789AB",
            "queue/0123456789ab/x",
            "queue/../main0000000",
            "xqueue/0123456789ab",
        ] {
            assert!(!valid_task_branch(bad), "{bad}");
        }
        let c = CacheSpec::default();
        assert!(!serde_json::to_string(&c).unwrap().contains("scope"));
    }

    #[test]
    fn validates_names() {
        assert!(valid_name("q-abc-agent"));
        assert!(!valid_name("../etc"));
        assert!(!valid_name(""));
        assert!(!valid_name("A"));
    }
}
