//! Sandboxed process execution. The runner itself is unprivileged:
//!   podman – rootless Podman (container root = runner uid, no host root at all)
//!   docker – Docker, container process runs as the runner's uid:gid
//!   none   – plain child process (development only)
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use queued_common::{is_task_workspace, valid_name, CliProbe, ExecFrame, ExecRequest};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{egress, setup, RunnerConfig};

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

/// Per-run additions to the sandbox, prepared by [`run`].
#[derive(Debug, Default)]
pub struct Sandbox {
    /// Containers: host directory with the egress socket, bridge and CA,
    /// mounted read-only at [`egress::CONTAINER_DIR`] (`None` = no network).
    pub egress_dir: Option<PathBuf>,
    /// Containers: dependency cache mounted at [`setup::CONTAINER_CACHE`].
    pub cache_dir: Option<PathBuf>,
    /// Added after the request's own env (proxy, CA and cache variables).
    pub env: Vec<(String, String)>,
}

pub fn build_command(
    cfg: &RunnerConfig,
    req: &ExecRequest,
    workspace: &Path,
    home: &Path,
    sb: &Sandbox,
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
            // No network of its own: the only way out is the egress socket.
            cmd.args(["--network", "none"]);
            if let Some(d) = &sb.egress_dir {
                cmd.arg("-v")
                    .arg(format!("{}:{}:ro", d.display(), egress::CONTAINER_DIR));
            }
            if let Some(c) = &sb.cache_dir {
                cmd.arg("-v").arg(vol(c, setup::CONTAINER_CACHE));
            }
            for (k, v) in req.env.iter().chain(&sb.env) {
                cmd.arg("-e").arg(k);
                cmd.env(k, v);
            }
            cmd.arg(&cfg.image);
            if sb.egress_dir.is_some() {
                cmd.args(["sh", "-c", egress::CONTAINER_WRAPPER, "queue-egress"]);
            }
            cmd.arg(&req.program).args(&req.args);
        }
        Engine::None => {
            cmd = Command::new(&req.program);
            cmd.args(&req.args)
                .current_dir(workspace)
                .env("HOME", home)
                .env("IS_SANDBOX", "1")
                .env("CI", "1")
                .envs(req.env.iter().cloned())
                .envs(sb.env.iter().cloned());
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

/// `ExecRequest::home_env`: absolute paths inside the HOME the process sees
/// (`/home/agent` in containers, the run's home directory with engine `none`).
pub fn home_env(
    cfg: &RunnerConfig,
    req: &ExecRequest,
    home: &Path,
) -> Result<Vec<(String, String)>> {
    let base = match cfg.engine {
        Engine::None => home.display().to_string(),
        _ => "/home/agent".to_string(),
    };
    req.home_env
        .iter()
        .map(|(k, rel)| {
            anyhow::ensure!(safe_rel(rel), "invalid home path");
            anyhow::ensure!(
                !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "invalid variable name"
            );
            Ok((k.clone(), format!("{base}/{rel}")))
        })
        .collect()
}

/// CLIs probed by `GET /v1/cli-status`: a fixed list (no caller input).
/// `agy` = Antigravity, the reported successor of the Gemini CLI.
pub const PROBED_CLIS: [&str; 5] = ["claude", "codex", "gemini", "opencode", "agy"];

static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Runs `<cli> --version` in the sandbox (same engine, image and limits as
/// agent runs, empty workspace/home, no env, no stdin, 20 s timeout).
pub async fn probe(cfg: &RunnerConfig) -> Result<Vec<CliProbe>> {
    let seq = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = cfg.data_dir.join("probe").join(seq.to_string());
    let (ws, home) = (dir.join("workspace"), dir.join("home"));
    tokio::fs::create_dir_all(&ws).await?;
    tokio::fs::create_dir_all(&home).await?;
    let mut out = vec![];
    for cli in PROBED_CLIS {
        let req = ExecRequest {
            name: format!("queue-probe-{cli}-{}-{seq}", std::process::id()),
            workspace: String::new(),
            program: cli.into(),
            args: vec!["--version".into()],
            env: vec![],
            home_files: vec![],
            read_back: vec![],
            stdin: None,
            timeout_secs: 20,
            idle_timeout_secs: None,
            queue_files: vec![],
            egress: Default::default(),
            cache: None,
            secret_env: vec![],
            home_env: vec![],
        };
        let mut cmd = build_command(cfg, &req, &ws, &home, &Sandbox::default());
        let version = match tokio::time::timeout(Duration::from_secs(20), cmd.output()).await {
            Ok(Ok(o)) if o.status.success() => {
                let text = String::from_utf8_lossy(if o.stdout.is_empty() {
                    &o.stderr
                } else {
                    &o.stdout
                })
                .to_string();
                text.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .map(|l| l.chars().take(200).collect())
            }
            Ok(_) => None,
            Err(_) => {
                if cfg.engine != Engine::None {
                    send(cfg.engine, &req.name, None, Signal::Kill).await;
                }
                None
            }
        };
        out.push(CliProbe {
            cli: cli.into(),
            version,
        });
    }
    let _ = tokio::fs::remove_dir_all(&dir).await;
    Ok(out)
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
    let egress_dir = egress_dir(cfg, &req.name);
    let res = run_inner(cfg, &req, &run_dir, &egress_dir, &cancel, &tx).await;
    let _ = tokio::fs::remove_dir_all(&run_dir).await;
    let _ = tokio::fs::remove_dir_all(&egress_dir).await;
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

/// Short per-run directory for the egress socket: Unix socket paths are
/// limited to 108 bytes, run names to 64 (checked at startup: the data dir
/// path is at most [`crate::MAX_DATA_DIR_LEN`] bytes).
pub fn egress_dir(cfg: &RunnerConfig, name: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(name.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    cfg.data_dir.join("e").join(h)
}

/// Starts the run's egress proxy (if the run has network or the engine
/// cannot enforce anything) and fills the sandbox accordingly.
async fn start_egress(
    cfg: &RunnerConfig,
    req: &ExecRequest,
    dir: &Path,
    tx: &mpsc::Sender<ExecFrame>,
    sb: &mut Sandbox,
) -> Result<Option<egress::Proxy>> {
    let container = cfg.engine != Engine::None;
    // Engine none: always a proxy (deny-all for `off`), so programs that
    // honour the proxy variables see the same rules as in a container.
    if container && !req.egress.needs_proxy() {
        return Ok(None);
    }
    let _ = tokio::fs::remove_dir_all(dir).await;
    tokio::fs::create_dir_all(dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let listen = if container {
        egress::Listen::Unix(dir.join(egress::SOCKET_NAME))
    } else {
        egress::Listen::Tcp
    };
    let proxy = egress::start(listen, &req.egress, cfg.egress.clone(), tx.clone()).await?;
    egress::write_dir(dir, proxy.ca_pem.as_deref()).await?;
    let (url, prefix) = match proxy.tcp_addr {
        Some(a) => (format!("http://{a}"), dir.display().to_string()),
        None => (
            format!("http://127.0.0.1:{}", egress::BRIDGE_PORT),
            egress::CONTAINER_DIR.to_string(),
        ),
    };
    sb.env.extend(egress::proxy_env(&url));
    if proxy.ca_pem.is_some() {
        let bundle = dir
            .join("ca-bundle.pem")
            .exists()
            .then(|| format!("{prefix}/ca-bundle.pem"));
        sb.env.extend(egress::ca_env(
            &format!("{prefix}/ca.pem"),
            bundle.as_deref(),
        ));
    }
    if container {
        sb.egress_dir = Some(dir.to_path_buf());
    }
    Ok(Some(proxy))
}

async fn write_queue_files(workspace: &Path, files: &[(String, String)]) -> Result<()> {
    for (name, content) in files {
        crate::git::write_queue_file(workspace, name, content).await?;
    }
    Ok(())
}

/// Milliseconds without output. A reader may store a timestamp newer than
/// the `now` read just before it (race between the two loads) – that means
/// "output just now", never an underflow (panic in debug, a huge idle time
/// and thus a false stall in release builds).
fn idle_for(now_ms: u64, last_output_ms: u64) -> u64 {
    now_ms.saturating_sub(last_output_ms)
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
    egress_dir: &Path,
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
    for n in &req.secret_env {
        anyhow::ensure!(
            req.env.iter().any(|(k, _)| k == n),
            "secret {n} is not part of env"
        );
    }
    let secrets: Vec<(String, String)> = req
        .env
        .iter()
        .filter(|(k, _)| req.secret_env.contains(k))
        .cloned()
        .collect();

    let mut sb = Sandbox::default();
    // WP04: the caches belong to the project (`scope`), shared by its task
    // workspaces; setups of one scope run one at a time (two would write
    // the same cache, and `finish` drops the other keys of the scope).
    let scope = req
        .cache
        .as_ref()
        .and_then(|c| c.scope.clone())
        .filter(|s| valid_name(s) && !is_task_workspace(s))
        .unwrap_or_else(|| req.workspace.clone());
    let _cache_lock = match &req.cache {
        Some(_) => Some(setup::scope_lock(&scope).await),
        None => None,
    };
    let cache = match &req.cache {
        Some(spec) => Some(setup::prepare(cfg, &scope, &workspace, spec).await?),
        None => None,
    };
    if let Some(c) = &cache {
        if cfg.engine == Engine::None {
            sb.env
                .extend(setup::cache_env(&c.dir.display().to_string()));
        } else {
            sb.env.extend(setup::cache_env(setup::CONTAINER_CACHE));
            sb.cache_dir = Some(c.dir.clone());
        }
    }
    sb.env.extend(home_env(cfg, req, &home)?);
    let proxy = start_egress(cfg, req, egress_dir, tx, &mut sb).await?;

    let mut child = build_command(cfg, req, &workspace, &home, &sb)
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
    let secret_values: Arc<Vec<String>> =
        Arc::new(secrets.iter().map(|(_, v)| v.clone()).collect());
    let reader = |stream: Box<dyn tokio::io::AsyncRead + Send + Unpin>, is_err: bool| {
        let tx = tx.clone();
        let last = last_output.clone();
        let secret_values = secret_values.clone();
        tokio::spawn(async move {
            let mut l = BufReader::new(stream).lines();
            while let Ok(Some(line)) = l.next_line().await {
                last.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
                // Secret values never leave the runner in output lines.
                let line = setup::redact(line, &secret_values);
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
                let idle = idle_for(started.elapsed().as_millis() as u64, last_output.load(Ordering::Relaxed));
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

    drop(proxy);
    let success = reason.is_none() && status.map(|s| s.success()).unwrap_or(false);
    if let Some(c) = &cache {
        setup::finish(c, success).await;
    }
    let mut leaks = vec![];
    if !secrets.is_empty() {
        let mut roots = vec![("workspace".to_string(), workspace.clone())];
        if let Some(c) = cache.as_ref().filter(|_| success) {
            roots.push(("cache".to_string(), c.dir.clone()));
        }
        let scan_secrets = secrets.clone();
        let found = tokio::task::spawn_blocking(move || setup::scan(&roots, &scan_secrets)).await?;
        if !found.complete {
            let _ = tx
                .send(ExecFrame::Err {
                    line: "queue: secret search stopped at its limits (not every file was checked)"
                        .into(),
                })
                .await;
        }
        if !found.leaks.is_empty() {
            if let Some(c) = &cache {
                if found.leaks.iter().any(|l| l.contains(" in cache/")) {
                    let _ = tokio::fs::remove_dir_all(&c.dir).await;
                }
            }
            leaks = setup::scrub(&workspace, &found.leaks, &secrets).await;
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
        success,
        code: status.and_then(|s| s.code()),
        timed_out: reason == Some(StopReason::Timeout),
        cancelled: reason == Some(StopReason::Cancel),
        stalled: reason == Some(StopReason::Stall),
        killed,
        files,
        cache: cache.map(|c| c.info),
        leaks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_time_never_underflows() {
        assert_eq!(idle_for(5000, 1000), 4000);
        // Output stored after `now` was read: no false stall.
        assert_eq!(idle_for(1000, 1001), 0);
        assert_eq!(idle_for(0, u64::MAX), 0);
    }

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
            egress: Default::default(),
            cache: None,
            secret_env: vec![],
            home_env: vec![],
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
    async fn home_env_points_into_the_run_home() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req(
            "sh",
            &[
                "-c",
                r#"echo "$GEMINI_CLI_SYSTEM_SETTINGS_PATH"; cat "$GEMINI_CLI_SYSTEM_SETTINGS_PATH""#,
            ],
        );
        r.home_files = vec![(".gemini/s.json".into(), "{\"a\":1}".into())];
        r.home_env = vec![(
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH".into(),
            ".gemini/s.json".into(),
        )];
        let frames = collect(&c, r, CancellationToken::new()).await;
        let out: Vec<String> = frames
            .iter()
            .filter_map(|f| match f {
                ExecFrame::Out { line } => Some(line.clone()),
                _ => None,
            })
            .collect();
        assert!(out[0].ends_with("/home/.gemini/s.json"), "{out:?}");
        assert_eq!(out[1], "{\"a\":1}");
        // Containers see the fixed home path; traversal is refused.
        let dc = cfg(t.path(), Engine::Docker);
        let mut r2 = req("x", &[]);
        r2.home_env = vec![("P".into(), "a/b".into())];
        assert_eq!(
            home_env(&dc, &r2, t.path()).unwrap(),
            vec![("P".to_string(), "/home/agent/a/b".to_string())]
        );
        r2.home_env = vec![("P".into(), "../x".into())];
        assert!(home_env(&dc, &r2, t.path()).is_err());
        r2.home_env = vec![("A=B".into(), "x".into())];
        assert!(home_env(&dc, &r2, t.path()).is_err());
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
                &Sandbox::default(),
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

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn containers_have_no_network_and_reach_out_only_through_the_egress_socket() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::Podman);
        let r = req("claude", &["-p"]);
        // Without network: no mount, no wrapper, still --network none.
        let plain = args_of(&build_command(
            &c,
            &r,
            Path::new("/w"),
            Path::new("/h"),
            &Sandbox::default(),
        ));
        let j = plain.join(" ");
        assert!(j.contains("--network none"), "{j}");
        assert!(!j.contains("queue-egress") && !j.contains("/cache"), "{j}");
        let sb = Sandbox {
            egress_dir: Some("/d/e/abc".into()),
            cache_dir: Some("/d/caches/p/k".into()),
            env: vec![("HTTPS_PROXY".into(), "http://127.0.0.1:3128".into())],
        };
        let a = args_of(&build_command(
            &c,
            &r,
            Path::new("/w"),
            Path::new("/h"),
            &sb,
        ));
        let j = a.join(" ");
        assert!(j.contains("--network none"));
        assert!(
            a.windows(2)
                .any(|w| w[0] == "-v" && w[1] == "/d/e/abc:/queue-egress:ro"),
            "{j}"
        );
        assert!(
            a.windows(2)
                .any(|w| w[0] == "-v" && w[1] == "/d/caches/p/k:/cache"),
            "{j}"
        );
        assert!(a.windows(2).any(|w| w[0] == "-e" && w[1] == "HTTPS_PROXY"));
        // Wrapper runs after the image, then the real program and its args.
        let img = a.iter().position(|x| x == &c.image).unwrap();
        assert_eq!(&a[img + 1..img + 3], &["sh".to_string(), "-c".to_string()]);
        assert_eq!(a[img + 3], egress::CONTAINER_WRAPPER);
        assert_eq!(
            &a[img + 4..],
            &["queue-egress".to_string(), "claude".into(), "-p".into()]
        );
        assert!(!j.contains("s3"), "secret value never in argv");
        assert!(j.contains("--cap-drop ALL") && j.contains("no-new-privileges"));
    }

    #[tokio::test]
    async fn engine_none_runs_behind_the_proxy_and_reports_decisions() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req(
            "sh",
            &["-c", "curl -s -o /dev/null -w 'connect=%{http_connect}\\n' https://blocked.invalid/ ; echo \"proxy=${HTTPS_PROXY%:*}\""],
        );
        r.egress = queued_common::EgressPolicy {
            mode: queued_common::NetMode::Allowlist,
            allow: vec!["pypi.org".into()],
            ..Default::default()
        };
        let f = collect(&c, r, CancellationToken::new()).await;
        assert!(
            f.contains(&ExecFrame::Egress {
                host: "blocked.invalid".into(),
                port: 443,
                method: "CONNECT".into(),
                verdict: queued_common::EgressVerdict::NotAllowed,
            }),
            "{f:?}"
        );
        assert!(
            f.contains(&ExecFrame::Out {
                line: "connect=403".into()
            }),
            "{f:?}"
        );
        assert!(
            f.contains(&ExecFrame::Out {
                line: "proxy=http://127.0.0.1".into()
            }),
            "{f:?}"
        );
        assert!(
            !c.data_dir
                .join("e")
                .read_dir()
                .is_ok_and(|mut d| d.next().is_some()),
            "egress dir removed"
        );
    }

    #[tokio::test]
    async fn secrets_are_redacted_and_leaks_reported() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let mut r = req(
            "sh",
            &[
                "-c",
                "echo token=$NPM_TOKEN; echo \"//r/:_authToken=$NPM_TOKEN\" > .npmrc",
            ],
        );
        r.env = vec![
            ("NPM_TOKEN".into(), "npm-secret-4711".into()),
            ("PLAIN".into(), "x".into()),
        ];
        r.secret_env = vec!["NPM_TOKEN".into()];
        let f = collect(&c, r, CancellationToken::new()).await;
        assert!(
            f.contains(&ExecFrame::Out {
                line: "token=***".into()
            }),
            "{f:?}"
        );
        let all = format!("{f:?}");
        assert!(!all.contains("npm-secret-4711"), "{all}");
        match f.last().unwrap() {
            ExecFrame::Exit { leaks, success, .. } => {
                assert!(success);
                assert_eq!(
                    leaks,
                    &vec!["NPM_TOKEN in workspace/.npmrc: entfernt".to_string()]
                );
            }
            o => panic!("{o:?}"),
        }
        assert!(
            !c.workspaces().join("w1/.npmrc").exists(),
            "leaked file removed"
        );
        // A secret name that is not part of env is a caller error.
        let mut r = req("true", &[]);
        r.secret_env = vec!["MISSING".into()];
        let f = collect(&c, r, CancellationToken::new()).await;
        assert!(matches!(f.last().unwrap(), ExecFrame::Fail { .. }));
    }

    /// WP04: task workspaces of one project share the project's cache
    /// (`scope`), and their setups never run at the same time.
    #[tokio::test]
    async fn setups_of_one_cache_scope_share_the_cache_and_run_one_at_a_time() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        for ws in [
            "ta0000000000000000000000000000000",
            "tb0000000000000000000000000000000",
        ] {
            tokio::fs::create_dir_all(c.workspaces().join(ws))
                .await
                .unwrap();
        }
        let run = |ws: &str, n: &str| {
            let mut r = req(
                "sh",
                &["-c", &format!("mkdir -p \"$npm_config_cache\"; echo start-{n} >> \"$npm_config_cache/log\"; sleep 0.4; echo end-{n} >> \"$npm_config_cache/log\"")],
            );
            r.name = format!("run-{n}");
            r.workspace = ws.into();
            r.cache = Some(queued_common::CacheSpec {
                key_material: "npm ci".into(),
                files: vec![],
                scope: Some("p-shared".into()),
            });
            r
        };
        let (a, b) = tokio::join!(
            collect(
                &c,
                run("ta0000000000000000000000000000000", "a"),
                CancellationToken::new()
            ),
            collect(
                &c,
                run("tb0000000000000000000000000000000", "b"),
                CancellationToken::new()
            )
        );
        for f in [&a, &b] {
            assert!(
                matches!(f.last().unwrap(), ExecFrame::Exit { success: true, .. }),
                "{f:?}"
            );
        }
        let root = setup::caches_root(&c, "p-shared");
        let key = std::fs::read_dir(&root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let log = std::fs::read_to_string(key.join("npm/log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 4, "{log}");
        assert!(
            lines[0].replace("start", "end") == lines[1]
                && lines[2].replace("start", "end") == lines[3],
            "setups overlapped: {log}"
        );
        assert!(!setup::caches_root(&c, "ta0000000000000000000000000000000").exists());
        // An invalid or task scope falls back to the run's own workspace.
        let mut r = run("ta0000000000000000000000000000000", "c");
        r.cache.as_mut().unwrap().scope = Some("tb0000000000000000000000000000000".into());
        let f = collect(&c, r, CancellationToken::new()).await;
        assert!(matches!(f.last().unwrap(), ExecFrame::Exit { .. }));
        assert!(setup::caches_root(&c, "ta0000000000000000000000000000000").exists());
    }

    #[tokio::test]
    async fn setup_cache_is_mounted_via_env_and_reported() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let run = |marker: &'static str| {
            let mut r = req("sh", &["-c", &format!("ls \"$npm_config_cache\" 2>/dev/null; mkdir -p \"$npm_config_cache\" && touch \"$npm_config_cache/{marker}\"")]);
            r.cache = Some(queued_common::CacheSpec {
                key_material: "npm ci".into(),
                files: vec!["package-lock.json".into()],
                scope: None,
            });
            r
        };
        let f = collect(&c, run("first"), CancellationToken::new()).await;
        let info = |f: &[ExecFrame]| match f.last().unwrap() {
            ExecFrame::Exit { cache, .. } => cache.clone().unwrap(),
            o => panic!("{o:?}"),
        };
        assert!(!info(&f).hit);
        let f2 = collect(&c, run("second"), CancellationToken::new()).await;
        assert!(info(&f2).hit && info(&f2).key == info(&f).key);
        assert!(
            f2.contains(&ExecFrame::Out {
                line: "first".into()
            }),
            "cache content survives: {f2:?}"
        );
        std::fs::write(c.workspaces().join("w1/package-lock.json"), "{}").unwrap();
        let f3 = collect(&c, run("third"), CancellationToken::new()).await;
        assert!(
            !info(&f3).hit && info(&f3).key != info(&f).key,
            "lockfile change = new cache"
        );
    }

    #[tokio::test]
    async fn probe_reports_missing_and_present_clis() {
        let t = tempfile::tempdir().unwrap();
        let c = cfg(t.path(), Engine::None);
        let found = probe(&c).await.unwrap();
        assert_eq!(
            found.iter().map(|p| p.cli.as_str()).collect::<Vec<_>>(),
            PROBED_CLIS
        );
        // `agy` (Antigravity) is not installed in this environment.
        assert!(found
            .iter()
            .find(|p| p.cli == "agy")
            .unwrap()
            .version
            .is_none());
        assert!(
            !t.path().join("probe").join("0").exists(),
            "probe dir cleaned up"
        );
    }
}
