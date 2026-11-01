//! Sandboxed process execution. The runner itself is unprivileged:
//!   podman – rootless Podman (container root = runner uid, no host root at all)
//!   docker – Docker, container process runs as the runner's uid:gid
//!   none   – plain child process (development only)
use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result};
use queued_common::{ExecFrame, ExecRequest};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::RunnerConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Podman,
    Docker,
    None,
}

impl std::str::FromStr for Engine {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "podman" => Engine::Podman,
            "docker" => Engine::Docker,
            "none" => Engine::None,
            o => anyhow::bail!("unknown engine '{o}' (podman|docker|none)"),
        })
    }
}

pub fn build_command(
    cfg: &RunnerConfig,
    req: &ExecRequest,
    workspace: &Path,
    home: &Path,
) -> Command {
    let mut cmd;
    match cfg.engine {
        Engine::Podman | Engine::Docker => {
            let bin = if cfg.engine == Engine::Podman {
                "podman"
            } else {
                "docker"
            };
            let vol = |src: &Path, dst: &str| {
                if cfg.volume_opts.is_empty() {
                    format!("{}:{dst}", src.display())
                } else {
                    format!("{}:{dst}:{}", src.display(), cfg.volume_opts)
                }
            };
            cmd = Command::new(bin);
            cmd.args(["run", "--rm", "-i", "--init", "--name", &req.name])
                .args([
                    "--memory",
                    &cfg.memory,
                    "--cpus",
                    &cfg.cpus,
                    "--pids-limit",
                    "4096",
                ])
                .args(["--security-opt", "no-new-privileges", "--cap-drop", "ALL"])
                .args([
                    "-v",
                    &vol(workspace, "/workspace"),
                    "-v",
                    &vol(home, "/home/agent"),
                ])
                .args([
                    "-w",
                    "/workspace",
                    "-e",
                    "HOME=/home/agent",
                    "-e",
                    "IS_SANDBOX=1",
                    "-e",
                    "CI=1",
                ]);
            if cfg.engine == Engine::Docker {
                if let Some(u) = &cfg.docker_user {
                    cmd.args(["--user", u]);
                }
            }
            if let Some(n) = &cfg.network {
                cmd.args(["--network", n]);
            }
            for (k, v) in &req.env {
                cmd.arg("-e").arg(k);
                cmd.env(k, v);
            }
            cmd.arg(&cfg.image).arg(&req.program).args(&req.args);
        }
        Engine::None => {
            cmd = Command::new(&req.program);
            cmd.args(&req.args)
                .current_dir(workspace)
                .env("HOME", home)
                .env("IS_SANDBOX", "1")
                .env("CI", "1")
                .envs(req.env.iter().cloned());
            // Own process group, so timeouts/cancels kill grandchildren too.
            #[cfg(unix)]
            cmd.process_group(0);
        }
    }
    cmd.stdin(if req.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    cmd
}

fn safe_rel(rel: &str) -> bool {
    let p = Path::new(rel);
    !rel.is_empty()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Runs the process; frames go to `tx`. Always ends with Exit or Fail.
pub async fn run(
    cfg: &RunnerConfig,
    req: ExecRequest,
    cancel: CancellationToken,
    tx: mpsc::Sender<ExecFrame>,
) {
    let run_dir = cfg.data_dir.join("runs").join(&req.name);
    let res = run_inner(cfg, &req, &run_dir, &cancel, &tx).await;
    let _ = tokio::fs::remove_dir_all(&run_dir).await;
    if cfg.engine != Engine::None
        && (cancel.is_cancelled()
            || matches!(
                res,
                Ok(ExecFrame::Exit {
                    timed_out: true,
                    ..
                })
            ))
    {
        let bin = if cfg.engine == Engine::Podman {
            "podman"
        } else {
            "docker"
        };
        let _ = Command::new(bin)
            .args(["rm", "-f", &req.name])
            .output()
            .await;
    }
    let frame = res.unwrap_or_else(|e| ExecFrame::Fail {
        error: format!("{e:#}"),
    });
    let _ = tx.send(frame).await;
}

async fn run_inner(
    cfg: &RunnerConfig,
    req: &ExecRequest,
    run_dir: &Path,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<ExecFrame>,
) -> Result<ExecFrame> {
    let workspace = cfg.workspaces().join(&req.workspace);
    anyhow::ensure!(workspace.is_dir(), "workspace does not exist");
    let home = run_dir.join("home");
    let _ = tokio::fs::remove_dir_all(run_dir).await;
    tokio::fs::create_dir_all(&home).await?;
    for (rel, content) in &req.home_files {
        anyhow::ensure!(safe_rel(rel), "invalid home file path");
        let p = home.join(rel);
        if let Some(d) = p.parent() {
            tokio::fs::create_dir_all(d).await?;
        }
        tokio::fs::write(&p, content).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).await;
        }
    }

    let mut child = build_command(cfg, req, &workspace, &home)
        .spawn()
        .with_context(|| format!("failed to start {}", req.program))?;
    if let (Some(input), Some(mut stdin)) = (req.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let out = child.stdout.take().context("no stdout")?;
    let err = child.stderr.take().context("no stderr")?;
    let (t1, t2) = (tx.clone(), tx.clone());
    let r1 = tokio::spawn(async move {
        let mut l = BufReader::new(out).lines();
        while let Ok(Some(line)) = l.next_line().await {
            if t1.send(ExecFrame::Out { line }).await.is_err() {
                break;
            }
        }
    });
    let r2 = tokio::spawn(async move {
        let mut l = BufReader::new(err).lines();
        while let Ok(Some(line)) = l.next_line().await {
            if t2.send(ExecFrame::Err { line }).await.is_err() {
                break;
            }
        }
    });

    let (mut success, mut code, mut timed_out, mut cancelled) = (false, None, false, false);
    tokio::select! {
        status = child.wait() => {
            let status = status?;
            success = status.success();
            code = status.code();
        }
        _ = tokio::time::sleep(Duration::from_secs(req.timeout_secs.max(1))) => { timed_out = true; }
        _ = cancel.cancelled() => { cancelled = true; }
    }
    if timed_out || cancelled {
        #[cfg(unix)]
        if cfg.engine == Engine::None {
            if let Some(pid) = child.id() {
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .output()
                    .await;
            }
        }
        let _ = child.kill().await;
    }
    for mut r in [r1, r2] {
        if tokio::time::timeout(Duration::from_secs(5), &mut r)
            .await
            .is_err()
        {
            r.abort();
        }
    }

    let mut files = vec![];
    for rel in &req.read_back {
        if safe_rel(rel) {
            if let Ok(c) = tokio::fs::read_to_string(home.join(rel)).await {
                files.push((rel.clone(), c));
            }
        }
    }
    Ok(ExecFrame::Exit {
        success,
        code,
        timed_out,
        cancelled,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path, engine: Engine) -> RunnerConfig {
        RunnerConfig {
            engine,
            ..RunnerConfig::for_tests(dir.to_path_buf())
        }
    }

    fn req(program: &str, args: &[&str]) -> ExecRequest {
        ExecRequest {
            name: "t-run".into(),
            workspace: "w1".into(),
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: vec![("SECRET".into(), "s3".into())],
            home_files: vec![(".cfg/auth.json".into(), "{\"v\":1}".into())],
            read_back: vec![".cfg/auth.json".into()],
            stdin: Some("from-stdin".into()),
            timeout_secs: 10,
        }
    }

    async fn collect(
        cfg: &RunnerConfig,
        r: ExecRequest,
        cancel: CancellationToken,
    ) -> Vec<ExecFrame> {
        tokio::fs::create_dir_all(cfg.workspaces().join("w1"))
            .await
            .unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        let c = cfg.clone();
        tokio::spawn(async move { run(&c, r, cancel, tx).await });
        let mut v = vec![];
        while let Some(f) = rx.recv().await {
            v.push(f);
        }
        v
    }

    #[tokio::test]
    async fn streams_stdout_stderr_env_stdin_and_reads_back_home_files() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let script = r#"read IN; echo "in=$IN env=$SECRET pwd=$(basename $PWD)"; echo oops >&2; echo '{"v":2}' > $HOME/.cfg/auth.json"#;
        let f = collect(&c, req("sh", &["-c", script]), CancellationToken::new()).await;
        assert!(
            f.contains(&ExecFrame::Out {
                line: "in=from-stdin env=s3 pwd=w1".into()
            }),
            "{f:?}"
        );
        assert!(f.contains(&ExecFrame::Err {
            line: "oops".into()
        }));
        match f.last().unwrap() {
            ExecFrame::Exit { success, files, .. } => {
                assert!(success);
                assert_eq!(files[0].1.trim(), "{\"v\":2}");
            }
            o => panic!("{o:?}"),
        }
        assert!(
            !c.data_dir.join("runs/t-run").exists(),
            "run dir is cleaned up"
        );
    }

    #[tokio::test]
    async fn times_out() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req("sh", &["-c", "sleep 30"]);
        r.timeout_secs = 1;
        let f = collect(&c, r, CancellationToken::new()).await;
        assert!(matches!(
            f.last().unwrap(),
            ExecFrame::Exit {
                timed_out: true,
                success: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn cancels() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let tok = CancellationToken::new();
        let t2 = tok.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            t2.cancel();
        });
        let f = collect(&c, req("sh", &["-c", "sleep 30"]), tok).await;
        assert!(matches!(
            f.last().unwrap(),
            ExecFrame::Exit {
                cancelled: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn rejects_path_traversal_in_home_files() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req("true", &[]);
        r.home_files = vec![("../../evil".into(), "x".into())];
        let f = collect(&c, r, CancellationToken::new()).await;
        assert!(matches!(f.last().unwrap(), ExecFrame::Fail { .. }));
    }

    #[tokio::test]
    async fn fails_for_unknown_program() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let f = collect(
            &c,
            req("definitely-not-a-binary-xyz", &[]),
            CancellationToken::new(),
        )
        .await;
        assert!(matches!(f.last().unwrap(), ExecFrame::Fail { .. }));
    }

    #[test]
    fn container_command_is_unprivileged_and_keeps_secrets_out_of_argv() {
        let t = tempfile::tempdir().unwrap();
        for engine in [Engine::Podman, Engine::Docker] {
            let mut c = cfg(t.path(), engine);
            c.docker_user = Some("1000:1000".into());
            let cmd = build_command(
                &c,
                &req("claude", &["-p"]),
                Path::new("/w"),
                Path::new("/h"),
            );
            let args: Vec<String> = cmd
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().to_string())
                .collect();
            let joined = args.join(" ");
            assert!(joined.contains("--cap-drop ALL"));
            assert!(joined.contains("no-new-privileges"));
            assert!(!joined.contains("--privileged"));
            assert!(
                !joined.contains("s3"),
                "secret value leaked into argv: {joined}"
            );
            assert!(args.windows(2).any(|w| w[0] == "-e" && w[1] == "SECRET"));
            assert_eq!(
                joined.contains("--user 1000:1000"),
                engine == Engine::Docker
            );
        }
    }
}
