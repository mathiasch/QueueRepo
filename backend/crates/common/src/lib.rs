//! Wire protocol between `queued` (orchestrator, no host privileges) and
//! `queued-runner` (owns workspaces + git, starts sandboxed agent processes).
//!
//! Transport: HTTP/JSON with `Authorization: Bearer <RUNNER_TOKEN>`.
//! `POST /v1/exec` answers with a stream of NDJSON [`ExecFrame`]s.
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    pub ok: bool,
    pub output: String,
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
    fn validates_names() {
        assert!(valid_name("q-abc-agent"));
        assert!(!valid_name("../etc"));
        assert!(!valid_name(""));
        assert!(!valid_name("A"));
    }
}
