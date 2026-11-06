//! Sandboxed process execution. The runner itself is unprivileged:
//!   podman – rootless Podman (container root = runner uid, no host root at all)
//!   docker – Docker, container process runs as the runner's uid:gid
//!   none   – plain child process (development only)
use std::{
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use queued_common::{ExecFrame, ExecRequest};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc,
    time::Instant,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Polite stop: the agent CLI finishes its turn (Claude still emits its
    /// `result` event with usage).
    Int,
    /// Hard stop after the grace period.
    Kill,
}

fn engine_bin(engine: Engine) -> &'static str {
    if engine == Engine::Podman {
        "podman"
    } else {
        "docker"
    }
}

/// Command that delivers `sig` to a run. `None` engine: the whole process
/// group (`pid` is its leader). Containers: SIGINT goes to the container's
/// init (`--init` forwards it to the agent), SIGKILL = `rm -f`.
/// Returns `None` if there is nothing to signal (no pid for engine `none`).
pub fn stop_command(
    engine: Engine,
    name: &str,
    pid: Option<u32>,
    sig: Signal,
) -> Option<(String, Vec<String>)> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match engine {
        Engine::None => {
            let pid = pid?;
            let flag = if sig == Signal::Int { "-INT" } else { "-KILL" };
            Some(("kill".into(), v(&[flag, "--", &format!("-{pid}")])))
        }
        Engine::Podman | Engine::Docker => Some((
            engine_bin(engine).into(),
            match sig {
                Signal::Int => v(&["kill", "--signal", "SIGINT", name]),
                Signal::Kill => v(&["rm", "-f", name]),
            },
        )),
    }
}

async fn send(engine: Engine, name: &str, pid: Option<u32>, sig: Signal) {
    if let Some((prog, args)) = stop_command(engine, name, pid, sig) {
        if let Err(e) = Command::new(&prog).args(&args).output().await {
            tracing::warn!("{prog} {args:?} failed: {e}");
        }
    }
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
    let stopped = matches!(
        res,
        Ok(ExecFrame::Exit {
            timed_out: true,
            ..
        }) | Ok(ExecFrame::Exit { stalled: true, .. })
            | Ok(ExecFrame::Exit {
                cancelled: true,
                ..
            })
    );
    if cfg.engine != Engine::None && (cancel.is_cancelled() || stopped) {
        // Make sure no container outlives a stopped run.
        send(cfg.engine, &req.name, None, Signal::Kill).await;
    }
    let frame = res.unwrap_or_else(|e| ExecFrame::Fail {
        error: format!("{e:#}"),
    });
    let _ = tx.send(frame).await;
}

async fn write_queue_files(workspace: &Path, files: &[(String, String)]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let dir = workspace.join(".queue");
    tokio::fs::create_dir_all(&dir).await?;
    for (name, content) in files {
        anyhow::ensure!(
            queued_common::valid_queue_file_name(name),
            "invalid queue file name"
        );
        tokio::fs::write(dir.join(name), content).await?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StopReason {
    Timeout,
    Cancel,
    Stall,
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
    write_queue_files(&workspace, &req.queue_files).await?;

    let mut child = build_command(cfg, req, &workspace, &home)
        .spawn()
        .with_context(|| format!("failed to start {}", req.program))?;
    let pid = child.id();
    if let (Some(input), Some(mut stdin)) = (req.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let out = child.stdout.take().context("no stdout")?;
    let err = child.stderr.take().context("no stderr")?;
    // Milliseconds since `started` at which the last output line arrived.
    let started = Instant::now();
    let last_output = Arc::new(AtomicU64::new(0));
    let reader = |stream: Box<dyn tokio::io::AsyncRead + Send + Unpin>, is_err: bool| {
        let tx = tx.clone();
        let last = last_output.clone();
        tokio::spawn(async move {
            let mut l = BufReader::new(stream).lines();
            while let Ok(Some(line)) = l.next_line().await {
                last.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                let f = if is_err {
                    ExecFrame::Err { line }
                } else {
                    ExecFrame::Out { line }
                };
                if tx.send(f).await.is_err() {
                    break;
                }
            }
        })
    };
    let r1 = reader(Box::new(out), false);
    let r2 = reader(Box::new(err), true);

    let idle_ms = req.idle_timeout_secs.filter(|s| *s > 0).map(|s| s * 1000);
    let deadline = tokio::time::sleep(Duration::from_secs(req.timeout_secs.max(1)));
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut status = None;
    let mut reason = None;
    while status.is_none() && reason.is_none() {
        tokio::select! {
            s = child.wait() => status = Some(s?),
            _ = &mut deadline => reason = Some(StopReason::Timeout),
            _ = cancel.cancelled() => reason = Some(StopReason::Cancel),
            _ = tick.tick(), if idle_ms.is_some() => {
                let idle = started.elapsed().as_millis() as u64 - last_output.load(Ordering::Relaxed);
                if idle >= idle_ms.unwrap_or(u64::MAX) {
                    reason = Some(StopReason::Stall);
                }
            }
        }
    }

    let mut killed = false;
    if reason.is_some() {
        // 1) SIGINT, 2) keep streaming while waiting for the grace period, 3) SIGKILL.
        send(cfg.engine, &req.name, pid, Signal::Int).await;
        match tokio::time::timeout(Duration::from_secs(cfg.kill_grace_secs), child.wait()).await {
            Ok(Ok(s)) => status = Some(s),
            Ok(Err(e)) => tracing::warn!("wait after SIGINT failed: {e}"),
            Err(_) => {
                killed = true;
                send(cfg.engine, &req.name, pid, Signal::Kill).await;
                let _ = child.kill().await;
            }
        }
    }
    #[cfg(unix)]
    if cfg.engine == Engine::None && (reason.is_some() || status.is_none()) {
        // Stragglers of the process group (e.g. background jobs that ignore
        // SIGINT) would otherwise keep the output pipes open.
        send(cfg.engine, &req.name, pid, Signal::Kill).await;
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
        // A stopped run never counts as a natural success, whatever its exit code.
        success: reason.is_none() && status.map(|s| s.success()).unwrap_or(false),
        code: status.and_then(|s| s.code()),
        timed_out: reason == Some(StopReason::Timeout),
        cancelled: reason == Some(StopReason::Cancel),
        stalled: reason == Some(StopReason::Stall),
        killed,
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
            idle_timeout_secs: None,
            queue_files: vec![],
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

    fn exit_of(f: &[ExecFrame]) -> (bool, bool, bool, bool, bool, Option<i32>) {
        match f.last().unwrap() {
            ExecFrame::Exit {
                success,
                timed_out,
                cancelled,
                stalled,
                killed,
                code,
                ..
            } => (*success, *timed_out, *cancelled, *stalled, *killed, *code),
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test]
    async fn detects_stall_and_sends_sigint_first() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req(
            "sh",
            &[
                "-c",
                "trap 'echo stopped-politely; exit 130' INT; echo start; sleep 30 & wait $!",
            ],
        );
        r.idle_timeout_secs = Some(1);
        let started = Instant::now();
        let f = collect(&c, r, CancellationToken::new()).await;
        let (success, timed_out, cancelled, stalled, killed, code) = exit_of(&f);
        assert!(
            stalled && !timed_out && !cancelled && !killed && !success,
            "{f:?}"
        );
        assert_eq!(code, Some(130));
        assert!(f.contains(&ExecFrame::Out {
            line: "stopped-politely".into()
        }));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "background sleep must not hold the pipes"
        );
    }

    #[tokio::test]
    async fn output_resets_the_stall_timer() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req(
            "sh",
            &["-c", "for i in 1 2 3 4; do echo $i; sleep 0.6; done"],
        );
        r.idle_timeout_secs = Some(1);
        let f = collect(&c, r, CancellationToken::new()).await;
        let (success, _, _, stalled, _, _) = exit_of(&f);
        assert!(success && !stalled, "{f:?}");
    }

    #[tokio::test]
    async fn sigint_then_grace_lets_process_flush() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req(
            "sh",
            &[
                "-c",
                "trap 'echo flushed-after-int; exit 130' INT; sleep 30 & wait $!",
            ],
        );
        r.timeout_secs = 1;
        let f = collect(&c, r, CancellationToken::new()).await;
        let (success, timed_out, _, stalled, killed, _) = exit_of(&f);
        assert!(timed_out && !killed && !stalled && !success, "{f:?}");
        assert!(f.contains(&ExecFrame::Out {
            line: "flushed-after-int".into()
        }));
    }

    #[tokio::test]
    async fn sigkill_after_grace_when_int_ignored() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let tok = CancellationToken::new();
        let t2 = tok.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            t2.cancel();
        });
        let started = Instant::now();
        let f = collect(&c, req("sh", &["-c", "trap '' INT; sleep 30"]), tok).await;
        let (_, _, cancelled, _, killed, _) = exit_of(&f);
        assert!(cancelled && killed, "{f:?}");
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(300) + Duration::from_secs(c.kill_grace_secs)
                && took < Duration::from_secs(10),
            "{took:?}"
        );
    }

    #[test]
    fn stop_commands_for_engines() {
        let s = |e, sig| stop_command(e, "q-run", Some(42), sig).unwrap();
        assert_eq!(
            s(Engine::None, Signal::Int),
            (
                "kill".to_string(),
                vec!["-INT".into(), "--".into(), "-42".into()]
            )
        );
        assert_eq!(
            s(Engine::None, Signal::Kill),
            (
                "kill".to_string(),
                vec!["-KILL".into(), "--".into(), "-42".into()]
            )
        );
        assert!(stop_command(Engine::None, "q-run", None, Signal::Int).is_none());
        for (e, bin) in [(Engine::Podman, "podman"), (Engine::Docker, "docker")] {
            assert_eq!(
                stop_command(e, "q-run", None, Signal::Int).unwrap(),
                (
                    bin.to_string(),
                    vec![
                        "kill".into(),
                        "--signal".into(),
                        "SIGINT".into(),
                        "q-run".into()
                    ]
                )
            );
            assert_eq!(
                stop_command(e, "q-run", Some(1), Signal::Kill).unwrap(),
                (
                    bin.to_string(),
                    vec!["rm".into(), "-f".into(), "q-run".into()]
                )
            );
        }
    }

    #[tokio::test]
    async fn writes_queue_files_and_rejects_bad_names() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req("sh", &["-c", "cat .queue/result.schema.json"]);
        r.queue_files = vec![("result.schema.json".into(), "{\"s\":1}".into())];
        let f = collect(&c, r, CancellationToken::new()).await;
        assert!(
            f.contains(&ExecFrame::Out {
                line: "{\"s\":1}".into()
            }),
            "{f:?}"
        );
        for bad in ["../evil", "a/b", ".x"] {
            let mut r = req("true", &[]);
            r.queue_files = vec![(bad.into(), "x".into())];
            let f = collect(&c, r, CancellationToken::new()).await;
            assert!(matches!(f.last().unwrap(), ExecFrame::Fail { .. }), "{bad}");
        }
        assert!(!c.workspaces().join("evil").exists());
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
