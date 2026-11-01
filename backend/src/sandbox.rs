//! Process execution – either in a throwaway docker container per run or
//! directly on the host (dev). Lines from stdout/stderr are streamed via mpsc.
use std::{path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, SandboxMode};

pub struct ExecSpec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    /// Passed by name to docker (`-e KEY`), so secrets never appear in argv.
    pub env: Vec<(String, String)>,
    pub workspace: PathBuf,
    pub home: PathBuf,
    pub stdin: Option<String>,
    pub timeout: Duration,
}

pub enum Line {
    Out(String),
    Err(String),
}

#[derive(Debug, Default)]
pub struct ExecResult {
    pub success: bool,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
}

fn host_path(cfg: &Config, p: &std::path::Path) -> PathBuf {
    match p.strip_prefix(&cfg.data_dir) {
        Ok(rel) => cfg.host_data_dir.join(rel),
        Err(_) => p.to_path_buf(),
    }
}

fn build(cfg: &Config, spec: &ExecSpec) -> Command {
    let mut cmd;
    match cfg.sandbox {
        SandboxMode::Docker => {
            cmd = Command::new("docker");
            cmd.args(["run", "--rm", "-i", "--name", &spec.name, "--init"])
                .args(["--memory", &cfg.docker_memory, "--cpus", &cfg.docker_cpus, "--pids-limit", "2048"])
                .args(["--security-opt", "no-new-privileges"])
                .arg("-v")
                .arg(format!("{}:/workspace", host_path(cfg, &spec.workspace).display()))
                .arg("-v")
                .arg(format!("{}:/home/agent", host_path(cfg, &spec.home).display()))
                .args(["-w", "/workspace", "-e", "HOME=/home/agent", "-e", "IS_SANDBOX=1", "-e", "CI=1"]);
            if let Some(n) = &cfg.docker_network {
                cmd.args(["--network", n]);
            }
            for (k, v) in &spec.env {
                cmd.arg("-e").arg(k);
                cmd.env(k, v);
            }
            cmd.arg(&cfg.agent_image).arg(&spec.program).args(&spec.args);
        }
        SandboxMode::None => {
            cmd = Command::new(&spec.program);
            cmd.args(&spec.args)
                .current_dir(&spec.workspace)
                .env("HOME", &spec.home)
                .env("IS_SANDBOX", "1")
                .env("CI", "1")
                .envs(spec.env.iter().cloned());
        }
    }
    cmd.stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

pub async fn run(cfg: &Config, spec: ExecSpec, cancel: &CancellationToken, tx: mpsc::Sender<Line>) -> Result<ExecResult> {
    let mut child = build(cfg, &spec).spawn().with_context(|| format!("failed to start {}", spec.program))?;

    if let (Some(input), Some(mut stdin)) = (spec.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let tx2 = tx.clone();
    let r1 = tokio::spawn(async move {
        let mut l = BufReader::new(out).lines();
        while let Ok(Some(line)) = l.next_line().await {
            if tx.send(Line::Out(line)).await.is_err() {
                break;
            }
        }
    });
    let r2 = tokio::spawn(async move {
        let mut l = BufReader::new(err).lines();
        while let Ok(Some(line)) = l.next_line().await {
            if tx2.send(Line::Err(line)).await.is_err() {
                break;
            }
        }
    });

    let mut res = ExecResult::default();
    tokio::select! {
        status = child.wait() => {
            let status = status?;
            res.success = status.success();
            res.code = status.code();
        }
        _ = tokio::time::sleep(spec.timeout) => { res.timed_out = true; }
        _ = cancel.cancelled() => { res.cancelled = true; }
    }
    if res.timed_out || res.cancelled {
        let _ = child.kill().await;
        if cfg.sandbox == SandboxMode::Docker {
            let _ = Command::new("docker").args(["rm", "-f", &spec.name]).output().await;
        }
    }
    // Grandchildren may keep the pipes open; don't hang on them.
    for r in [r1, r2] {
        if tokio::time::timeout(Duration::from_secs(5), r).await.is_err() {
            tracing::warn!("output reader of {} did not finish", spec.name);
        }
    }
    Ok(res)
}
