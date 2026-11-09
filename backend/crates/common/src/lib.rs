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
    fn validates_names() {
        assert!(valid_name("q-abc-agent"));
        assert!(!valid_name("../etc"));
        assert!(!valid_name(""));
        assert!(!valid_name("A"));
    }
}
