//! WP12 Preview: the dev server of a task's working tree in the sandbox.
//!
//! * Same engine, image and limits as agent runs (`--memory/--cpus/
//!   --pids-limit`, `--cap-drop ALL`, `no-new-privileges`), **`--network
//!   none` and no egress at all**, and **no port binding**: inside the
//!   container a small bridge ([`BRIDGE_JS`], run with the image's `node`)
//!   offers `127.0.0.1:<port>` as a Unix socket in the preview directory,
//!   which the runner connects to. Engine `none` (development only): a host
//!   process on a free loopback port in its own process group.
//! * Ready check, screenshots with a headless Chromium (inside the same
//!   container, i.e. the same network namespace – or the host browser for
//!   engine `none`), then either stop or keep it as a live preview with a
//!   hard lifetime. A reaper stops expired or exited previews; stopping is
//!   SIGINT → grace → SIGKILL of the process group, `rm -f` for containers.
//! * HTTP forwarding for the app's authenticated proxy ([`forward`]); the
//!   app filters headers and sets the security policy.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use queued_common::{
    is_task_workspace, preview_request_header_ok, preview_response_header_ok, valid_name,
    PreviewStart, PreviewStarted, Shot, ShotSpec, ShotViewport, MAX_PREVIEW_REQUEST,
    MAX_PREVIEW_RESPONSE, MAX_SHOTS, MAX_SHOT_BYTES,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpStream, UnixStream},
    process::{Child, Command},
    sync::Mutex,
    time::Instant,
};

use crate::{
    exec::{self, Engine, Signal},
    RunnerConfig,
};

/// Mount point of the preview directory in the container.
pub const CONTAINER_DIR: &str = "/queue-preview";
const SOCKET_NAME: &str = "s.sock";
/// Longest ready timeout / lifetime the runner accepts.
pub const MAX_READY_SECS: u64 = 600;
pub const MAX_LIFETIME_SECS: u64 = 3600;
/// Time per screenshot.
const SHOT_TIMEOUT: Duration = Duration::from_secs(60);
/// Grace between SIGINT and SIGKILL when a preview stops.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// Log tail kept per preview.
const LOG_MAX: usize = 8 * 1024;
const MOBILE_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";

/// Bridge in the container: Unix socket in the preview directory →
/// `127.0.0.1:<port>` of the dev server (same network namespace). Written
/// by the runner, never baked into the image.
pub const BRIDGE_JS: &str = r#"'use strict';
const net = require('net'), fs = require('fs');
const [sock, port] = process.argv.slice(2);
try { fs.unlinkSync(sock); } catch (e) {}
net.createServer((c) => {
  const u = net.connect(Number(port), '127.0.0.1');
  const end = () => { c.destroy(); u.destroy(); };
  c.on('error', end); u.on('error', end); c.on('close', end); u.on('close', end);
  c.pipe(u); u.pipe(c);
}).listen(sock);
"#;

/// `sh -c` wrapper in the container: bridge in the background, then the
/// project's command (`$1`).
pub const CONTAINER_WRAPPER: &str =
    "node /queue-preview/bridge.js /queue-preview/s.sock \"$PORT\" 2>/dev/null & exec sh -c \"$1\"";

/// Where the dev server answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Tcp(u16),
    Unix(PathBuf),
}

struct Handle {
    child: Child,
    pid: Option<u32>,
    target: Target,
    /// Port inside the sandbox (Host header, browser URL).
    port: u16,
    deadline: Instant,
    dir: PathBuf,
    workspace: String,
}

/// Running previews of this runner.
#[derive(Default)]
pub struct Registry {
    map: Mutex<HashMap<String, Handle>>,
    reaper: AtomicBool,
}

impl Registry {
    pub async fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map.lock().await.keys().cloned().collect();
        v.sort();
        v
    }

    /// A preview runs in this working tree.
    pub async fn uses_workspace(&self, ws: &str) -> bool {
        self.map.lock().await.values().any(|h| h.workspace == ws)
    }
}

fn path_ok(p: &str) -> bool {
    p.starts_with('/')
        && p.len() <= 300
        && !p.contains("//")
        && p.chars().all(|c| c.is_ascii_graphic() && c != '\\')
}

/// Checks a start request (the app validated it too – the runner trusts
/// nothing it executes).
pub fn validate(req: &PreviewStart) -> Result<()> {
    if !valid_name(&req.name) {
        bail!("invalid preview name");
    }
    if req.command.trim().is_empty() || req.command.len() > 2000 || req.command.contains('\0') {
        bail!("invalid command");
    }
    if req.port < 1024 {
        bail!("port must be 1024–65535");
    }
    if !path_ok(&req.ready_path) {
        bail!("invalid ready path");
    }
    if req.ready_timeout_secs == 0 || req.ready_timeout_secs > MAX_READY_SECS {
        bail!("ready timeout must be 1–{MAX_READY_SECS} s");
    }
    if req.max_lifetime_secs == 0 || req.max_lifetime_secs > MAX_LIFETIME_SECS {
        bail!("lifetime must be 1–{MAX_LIFETIME_SECS} s");
    }
    if req.shots.len() > MAX_SHOTS || req.shots.iter().any(|s| !path_ok(&s.path)) {
        bail!("invalid screenshots");
    }
    if req.env.iter().any(|(k, v)| {
        k.is_empty()
            || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || matches!(k.as_str(), "PORT" | "HOST" | "HOME" | "PATH")
            || v.contains('\0')
    }) {
        bail!("invalid environment variable");
    }
    Ok(())
}

/// Short per-preview directory (Unix socket paths are limited to 108 bytes).
pub fn preview_dir(cfg: &RunnerConfig, name: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(name.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    cfg.data_dir.join("pv").join(h)
}

/// `docker|podman run` arguments of a preview container: no network, no
/// published port, the working tree and the preview directory mounted.
pub fn container_args(
    cfg: &RunnerConfig,
    req: &PreviewStart,
    ws: &Path,
    dir: &Path,
) -> Vec<String> {
    let vol = |src: &Path, dst: &str| {
        if cfg.volume_opts.is_empty() {
            format!("{}:{dst}", src.display())
        } else {
            format!("{}:{dst}:{}", src.display(), cfg.volume_opts)
        }
    };
    let mut a: Vec<String> = [
        "run",
        "--rm",
        "-i",
        "--init",
        "--name",
        &req.name,
        "--memory",
        &cfg.memory,
        "--cpus",
        &cfg.cpus,
        "--pids-limit",
        "4096",
        "--security-opt",
        "no-new-privileges",
        "--cap-drop",
        "ALL",
        "--network",
        "none",
        "-v",
        &vol(ws, "/workspace"),
        "-v",
        &vol(dir, CONTAINER_DIR),
        "-w",
        "/workspace",
        "-e",
        "HOME=/tmp",
        "-e",
        "IS_SANDBOX=1",
        "-e",
        "CI=1",
        "-e",
        &format!("PORT={}", req.port),
        "-e",
        "HOST=127.0.0.1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if cfg.engine == Engine::Docker {
        if let Some(u) = &cfg.docker_user {
            a.extend(["--user".into(), u.clone()]);
        }
    }
    for (k, _) in &req.env {
        // Values via the environment of the docker/podman process (never argv).
        a.extend(["-e".into(), k.clone()]);
    }
    a.push(cfg.image.clone());
    a.extend([
        "sh".into(),
        "-c".into(),
        CONTAINER_WRAPPER.into(),
        "queue-preview".into(),
        req.command.clone(),
    ]);
    a
}

/// Headless Chromium arguments for one screenshot.
pub fn browser_args(
    url: &str,
    viewport: ShotViewport,
    out: &str,
    profile: &str,
    no_sandbox: bool,
) -> Vec<String> {
    let (w, h) = viewport.size();
    let mut a: Vec<String> = vec![
        "--headless".into(),
        "--disable-gpu".into(),
        "--hide-scrollbars".into(),
        "--mute-audio".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-extensions".into(),
        "--disable-background-networking".into(),
        "--disable-sync".into(),
        // Everything except loopback (the dev server) goes to a dead proxy:
        // the page cannot load anything from the network.
        "--proxy-server=http://127.0.0.1:9".into(),
        format!("--user-data-dir={profile}"),
        format!("--window-size={w},{h}"),
        "--virtual-time-budget=5000".into(),
        format!("--screenshot={out}"),
    ];
    if viewport == ShotViewport::Mobile {
        a.push(format!("--user-agent={MOBILE_UA}"));
        a.push("--force-device-scale-factor=2".into());
    }
    if no_sandbox {
        a.push("--no-sandbox".into());
    }
    a.push(url.into());
    a
}

fn push_log(log: &std::sync::Mutex<String>, line: &str) {
    let mut l = log.lock().unwrap_or_else(|e| e.into_inner());
    l.push_str(line);
    l.push('\n');
    if l.len() > LOG_MAX {
        let cut = l.len() - LOG_MAX / 2;
        let cut = (cut..l.len()).find(|i| l.is_char_boundary(*i)).unwrap_or(0);
        l.drain(..cut);
    }
}

fn spawn_log<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    r: R,
    log: Arc<std::sync::Mutex<String>>,
) {
    use tokio::io::AsyncBufReadExt;
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            push_log(&log, &l);
        }
    });
}

async fn free_port() -> Result<u16> {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    Ok(l.local_addr()?.port())
}

/// One HTTP/1.1 request to the dev server.
async fn request(
    target: &Target,
    req: hyper::Request<Full<Bytes>>,
    timeout: Duration,
) -> Result<hyper::Response<hyper::body::Incoming>> {
    async fn go<S>(
        s: S,
        req: hyper::Request<Full<Bytes>>,
    ) -> Result<hyper::Response<hyper::body::Incoming>>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (mut send, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(s)).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(send.send_request(req).await?)
    }
    tokio::time::timeout(timeout, async {
        match target {
            Target::Tcp(p) => go(TcpStream::connect(("127.0.0.1", *p)).await?, req).await,
            Target::Unix(path) => go(UnixStream::connect(path).await?, req).await,
        }
    })
    .await
    .context("timeout")?
}

async fn ready_probe(target: &Target, port: u16, path: &str) -> bool {
    let Ok(req) = hyper::Request::get(path)
        .header("host", format!("127.0.0.1:{port}"))
        .header("user-agent", "queue-preview-ready")
        .body(Full::new(Bytes::new()))
    else {
        return false;
    };
    matches!(request(target, req, Duration::from_secs(3)).await, Ok(r) if r.status().as_u16() < 500)
}

/// Stops a preview process (SIGINT, grace, SIGKILL of the process group /
/// `rm -f` of the container) and removes its directory.
async fn kill(engine: Engine, name: &str, h: &mut Handle) {
    if engine == Engine::None {
        // SIGTERM to the group (background jobs of a non-interactive shell
        // ignore SIGINT), a grace for the leader, then SIGKILL to the whole
        // group in any case: members may outlive the leader.
        if let Some(pid) = h.pid {
            let _ = Command::new("kill")
                .args(["-TERM", "--", &format!("-{pid}")])
                .output()
                .await;
        }
        let _ = tokio::time::timeout(STOP_GRACE, h.child.wait()).await;
        if let Some((p, a)) = exec::stop_command(engine, name, h.pid, Signal::Kill) {
            let _ = Command::new(p).args(a).output().await;
        }
        let _ = h.child.kill().await;
    } else {
        if let Some((p, a)) = exec::stop_command(engine, name, None, Signal::Kill) {
            let _ = Command::new(p).args(a).output().await;
        }
        let _ = h.child.kill().await;
    }
    let _ = tokio::fs::remove_dir_all(&h.dir).await;
}

/// Stops one preview. Returns whether it ran.
pub async fn stop(cfg: &RunnerConfig, reg: &Registry, name: &str) -> bool {
    let h = reg.map.lock().await.remove(name);
    match h {
        Some(mut h) => {
            kill(cfg.engine, name, &mut h).await;
            true
        }
        None => false,
    }
}

fn start_reaper(cfg: Arc<RunnerConfig>, reg: Arc<Registry>) {
    if reg.reaper.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = Instant::now();
            let mut gone = vec![];
            {
                let mut m = reg.map.lock().await;
                let names: Vec<String> = m.keys().cloned().collect();
                for n in names {
                    let h = m.get_mut(&n).expect("present");
                    let exited = h.child.try_wait().ok().flatten().is_some();
                    if exited || h.deadline <= now {
                        gone.push((n.clone(), m.remove(&n).expect("present")));
                    }
                }
            }
            for (n, mut h) in gone {
                tracing::info!("preview {n}: lifetime over or process exited, stopping");
                kill(cfg.engine, &n, &mut h).await;
            }
        }
    });
}

/// Starts the dev server of `ws` (a task's working tree), waits until it is
/// ready, takes the screenshots and either stops it or keeps it running.
pub async fn start(
    cfg: Arc<RunnerConfig>,
    reg: Arc<Registry>,
    ws: &str,
    req: PreviewStart,
) -> Result<PreviewStarted> {
    validate(&req)?;
    if !is_task_workspace(ws) {
        bail!("previews run only in a task's working tree");
    }
    let ws_dir = cfg.workspaces().join(ws);
    if !ws_dir.is_dir() {
        bail!("the working tree of the task does not exist (any more)");
    }
    if req.keep {
        let m = reg.map.lock().await;
        if m.len() >= cfg.max_previews {
            bail!(
                "too many previews on this runner (at most {})",
                cfg.max_previews
            );
        }
        if m.contains_key(&req.name) {
            bail!("preview already running");
        }
    }
    let dir = preview_dir(&cfg, &req.name);
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(dir.join("home")).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let log = Arc::new(std::sync::Mutex::new(String::new()));
    let (mut child, target, port) = match cfg.engine {
        Engine::None => {
            let port = free_port().await?;
            let mut c = Command::new("sh");
            c.arg("-c")
                .arg(&req.command)
                .current_dir(&ws_dir)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", dir.join("home"))
                .env("PORT", port.to_string())
                .env("HOST", "127.0.0.1")
                .env("IS_SANDBOX", "1")
                .env("CI", "1")
                .envs(req.env.iter().cloned())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(unix)]
            c.process_group(0);
            (
                c.spawn().context("start dev server")?,
                Target::Tcp(port),
                port,
            )
        }
        Engine::Docker | Engine::Podman => {
            tokio::fs::write(dir.join("bridge.js"), BRIDGE_JS).await?;
            let bin = if cfg.engine == Engine::Podman {
                "podman"
            } else {
                "docker"
            };
            let mut c = Command::new(bin);
            c.args(container_args(&cfg, &req, &ws_dir, &dir))
                .envs(req.env.iter().cloned())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            (
                c.spawn().context("start preview container")?,
                Target::Unix(dir.join(SOCKET_NAME)),
                req.port,
            )
        }
    };
    if let Some(o) = child.stdout.take() {
        spawn_log(o, log.clone());
    }
    if let Some(e) = child.stderr.take() {
        spawn_log(e, log.clone());
    }
    let pid = child.id();
    let mut h = Handle {
        child,
        pid,
        target: target.clone(),
        port,
        deadline: Instant::now() + Duration::from_secs(req.max_lifetime_secs),
        dir: dir.clone(),
        workspace: ws.to_string(),
    };
    // Ready check.
    let until = Instant::now() + Duration::from_secs(req.ready_timeout_secs);
    let mut error = None;
    let mut ready = false;
    loop {
        if let Ok(Some(s)) = h.child.try_wait() {
            error = Some(format!("Der Dev-Server hat sich beendet ({s})"));
            break;
        }
        if ready_probe(&target, port, &req.ready_path).await {
            ready = true;
            break;
        }
        if Instant::now() >= until {
            error = Some(format!(
                "Der Dev-Server war nach {} s nicht bereit",
                req.ready_timeout_secs
            ));
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let mut shots = vec![];
    if ready {
        for (i, s) in req.shots.iter().enumerate() {
            shots.push(screenshot(&cfg, &req.name, &h, s, i).await);
        }
    }
    let log_tail = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let keep = ready && req.keep;
    if keep {
        reg.map.lock().await.insert(req.name.clone(), h);
        start_reaper(cfg.clone(), reg.clone());
    } else {
        kill(cfg.engine, &req.name, &mut h).await;
    }
    Ok(PreviewStarted {
        ready,
        error,
        log_tail,
        shots,
        running: keep,
    })
}

async fn screenshot(cfg: &RunnerConfig, name: &str, h: &Handle, s: &ShotSpec, i: usize) -> Shot {
    let mut shot = Shot {
        path: s.path.clone(),
        viewport: s.viewport,
        png_b64: String::new(),
        error: None,
    };
    match take(cfg, name, h, s, i).await {
        Ok(png) => shot.png_b64 = base64::engine::general_purpose::STANDARD.encode(png),
        Err(e) => shot.error = Some(format!("{e:#}")),
    }
    shot
}

async fn take(
    cfg: &RunnerConfig,
    name: &str,
    h: &Handle,
    s: &ShotSpec,
    i: usize,
) -> Result<Vec<u8>> {
    let file = format!("shot-{i}.png");
    let profile = format!("chrome-{i}");
    let url = format!("http://127.0.0.1:{}{}", h.port, s.path);
    let mut c = match cfg.engine {
        Engine::None => {
            let Some(bin) = &cfg.chromium else {
                bail!("kein Headless-Browser konfiguriert (RUNNER_CHROMIUM)");
            };
            let mut c = Command::new(bin);
            c.args(browser_args(
                &url,
                s.viewport,
                &h.dir.join(&file).display().to_string(),
                &h.dir.join(&profile).display().to_string(),
                cfg.chromium_no_sandbox,
            ))
            .env_clear()
            .env("HOME", h.dir.join("home"))
            .env("PATH", std::env::var("PATH").unwrap_or_default());
            #[cfg(unix)]
            c.process_group(0);
            c
        }
        Engine::Docker | Engine::Podman => {
            let bin = if cfg.engine == Engine::Podman {
                "podman"
            } else {
                "docker"
            };
            let mut c = Command::new(bin);
            c.args(["exec", name, &cfg.preview_browser]);
            // The container is the sandbox (no capabilities for Chromium's own).
            c.args(browser_args(
                &url,
                s.viewport,
                &format!("{CONTAINER_DIR}/{file}"),
                &format!("{CONTAINER_DIR}/{profile}"),
                true,
            ));
            c
        }
    };
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = c.spawn().context("start headless browser")?;
    let mut err = String::new();
    let mut stderr = child.stderr.take();
    let waited = tokio::time::timeout(SHOT_TIMEOUT, async {
        if let Some(e) = stderr.as_mut() {
            let _ = e.take(64 * 1024).read_to_string(&mut err).await;
        }
        child.wait().await
    })
    .await;
    match waited {
        Ok(Ok(st)) if st.success() => {}
        Ok(Ok(st)) => bail!("Browser beendet mit {st}: {}", tail(&err, 300)),
        Ok(Err(e)) => bail!("Browser: {e}"),
        Err(_) => {
            if cfg.engine == Engine::None {
                if let Some((p, a)) =
                    exec::stop_command(Engine::None, name, child.id(), Signal::Kill)
                {
                    let _ = Command::new(p).args(a).output().await;
                }
            }
            let _ = child.kill().await;
            bail!("Screenshot nach {} s abgebrochen", SHOT_TIMEOUT.as_secs());
        }
    }
    let path = h.dir.join(&file);
    let meta = tokio::fs::symlink_metadata(&path)
        .await
        .context("kein Screenshot erzeugt")?;
    if !meta.file_type().is_file() || meta.len() as usize > MAX_SHOT_BYTES {
        bail!("Screenshot ungültig oder zu groß");
    }
    let png = tokio::fs::read(&path).await?;
    let _ = tokio::fs::remove_file(&path).await;
    let _ = tokio::fs::remove_dir_all(h.dir.join(&profile)).await;
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        bail!("Screenshot ist kein PNG");
    }
    Ok(png)
}

fn tail(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

/// Forwarded response: status, filtered headers, body.
pub struct Forwarded {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Bytes,
}

/// Forwards one request to a running preview. `path` includes the query.
pub async fn forward(
    reg: &Registry,
    name: &str,
    method: &str,
    path: &str,
    headers: &[(String, Vec<u8>)],
    body: Bytes,
) -> Result<Forwarded> {
    if !path.starts_with('/')
        || path.len() > 8 * 1024
        || path.bytes().any(|b| b < 0x21 || b == 0x7f)
    {
        bail!("invalid path");
    }
    let method = hyper::Method::from_bytes(method.as_bytes()).context("invalid method")?;
    if method == hyper::Method::CONNECT || method == hyper::Method::TRACE {
        bail!("method not allowed");
    }
    if body.len() > MAX_PREVIEW_REQUEST {
        bail!("request too large");
    }
    let (target, port) = {
        let m = reg.map.lock().await;
        let h = m.get(name).context("preview not running")?;
        (h.target.clone(), h.port)
    };
    let mut b = hyper::Request::builder().method(method).uri(path);
    for (k, v) in headers {
        if preview_request_header_ok(k) {
            b = b.header(k.as_str(), v.as_slice());
        }
    }
    let req = b
        .header("host", format!("127.0.0.1:{port}"))
        .header("connection", "close")
        .body(Full::new(body))?;
    let resp = request(&target, req, Duration::from_secs(30)).await?;
    let status = resp.status().as_u16();
    let headers: Vec<(String, Vec<u8>)> = resp
        .headers()
        .iter()
        .filter(|(k, _)| preview_response_header_ok(k.as_str()))
        .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
        .collect();
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        Limited::new(resp.into_body(), MAX_PREVIEW_RESPONSE).collect(),
    )
    .await
    .context("timeout")?
    .map_err(|e| anyhow::anyhow!("response: {e}"))?
    .to_bytes();
    Ok(Forwarded {
        status,
        headers,
        body,
    })
}

/// Stops the live previews that run in working tree `ws` (an agent or
/// verify run starts there, or the tree is removed).
pub async fn stop_workspace(cfg: &RunnerConfig, reg: &Registry, ws: &str) {
    let names: Vec<String> = reg
        .map
        .lock()
        .await
        .iter()
        .filter(|(_, h)| h.workspace == ws)
        .map(|(n, _)| n.clone())
        .collect();
    for n in names {
        stop(cfg, reg, &n).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(cmd: &str) -> PreviewStart {
        PreviewStart {
            name: "q-0123456789ab-preview".into(),
            command: cmd.into(),
            port: 3000,
            ready_path: "/".into(),
            ready_timeout_secs: 10,
            max_lifetime_secs: 60,
            env: vec![("APP_MODE".into(), "dev".into())],
            shots: vec![],
            keep: false,
        }
    }

    #[test]
    fn validation_refuses_bad_requests() {
        assert!(validate(&req("npm run dev")).is_ok());
        let mut r = req("x");
        r.port = 80;
        assert!(validate(&r).is_err());
        let mut r = req("x");
        r.name = "../etc".into();
        assert!(validate(&r).is_err());
        let mut r = req("x");
        r.ready_path = "http://evil".into();
        assert!(validate(&r).is_err());
        let mut r = req("x");
        r.max_lifetime_secs = MAX_LIFETIME_SECS + 1;
        assert!(validate(&r).is_err());
        let mut r = req("x");
        r.env = vec![("PATH".into(), "/tmp".into())];
        assert!(
            validate(&r).is_err(),
            "PATH/PORT/HOST/HOME are the runner's"
        );
        let mut r = req("");
        r.command = " ".into();
        assert!(validate(&r).is_err());
    }

    /// The container has no network and publishes no port; values of the
    /// project variables never appear in argv.
    #[test]
    fn container_has_no_network_and_no_port_binding() {
        let mut cfg = RunnerConfig::for_tests(PathBuf::from("/data"));
        cfg.engine = Engine::Docker;
        cfg.docker_user = Some("1000:1000".into());
        let a = container_args(
            &cfg,
            &req("npm run dev"),
            Path::new("/w/t1"),
            Path::new("/data/pv/x"),
        );
        let j = a.join(" ");
        assert!(j.contains("--network none"), "{j}");
        assert!(
            !a.iter()
                .any(|x| x == "-p" || x.starts_with("--publish") || x == "-P"),
            "{j}"
        );
        assert!(j.contains("--cap-drop ALL") && j.contains("no-new-privileges"));
        assert!(j.contains("--memory 1g") && j.contains("--pids-limit 4096"));
        assert!(j.contains("-v /w/t1:/workspace") && j.contains("-v /data/pv/x:/queue-preview"));
        assert!(j.contains("--user 1000:1000"));
        assert!(a.iter().any(|x| x == "APP_MODE"));
        assert!(
            !a.iter().any(|x| x.contains("APP_MODE=")),
            "values only via the environment"
        );
        assert_eq!(a.last().unwrap(), "npm run dev");
    }

    #[test]
    fn browser_has_no_network_besides_loopback() {
        let a = browser_args(
            "http://127.0.0.1:3000/",
            ShotViewport::Mobile,
            "/o.png",
            "/p",
            false,
        );
        assert!(a.iter().any(|x| x == "--proxy-server=http://127.0.0.1:9"));
        assert!(a.iter().any(|x| x == "--window-size=390,844"));
        assert!(a
            .iter()
            .any(|x| x.starts_with("--user-agent=") && x.contains("iPhone")));
        assert!(!a.iter().any(|x| x == "--no-sandbox"));
        assert_eq!(a.last().unwrap(), "http://127.0.0.1:3000/");
        let d = browser_args("u", ShotViewport::Desktop, "o", "p", true);
        assert!(
            d.iter().any(|x| x == "--window-size=1280,800")
                && d.iter().any(|x| x == "--no-sandbox")
        );
    }

    /// The container bridge (run here with the host's node): Unix socket →
    /// loopback port, real HTTP through it.
    #[tokio::test]
    async fn bridge_forwards_unix_socket_to_loopback_port() {
        let t = tempfile::tempdir().unwrap();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = axum::Router::new().route("/", axum::routing::get(|| async { "bridged" }));
            axum::serve(l, app).await.unwrap();
        });
        let js = t.path().join("bridge.js");
        std::fs::write(&js, BRIDGE_JS).unwrap();
        let sock = t.path().join("s.sock");
        let mut node = Command::new("node")
            .arg(&js)
            .arg(&sock)
            .arg(port.to_string())
            .kill_on_drop(true)
            .spawn()
            .expect("node");
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let target = Target::Unix(sock);
        assert!(ready_probe(&target, port, "/").await);
        let r = request(
            &target,
            hyper::Request::get("/")
                .header("host", "x")
                .body(Full::new(Bytes::new()))
                .unwrap(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let b = r.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&b[..], b"bridged");
        let _ = node.kill().await;
    }

    fn chromium() -> PathBuf {
        if let Ok(p) = std::env::var("RUNNER_CHROMIUM") {
            return PathBuf::from(p);
        }
        let base = Path::new("/opt/pw-browsers");
        let mut found: Vec<PathBuf> = std::fs::read_dir(base)
            .expect("Chromium under /opt/pw-browsers (or RUNNER_CHROMIUM)")
            .filter_map(|e| e.ok())
            .map(|e| {
                e.path()
                    .join("chrome-headless-shell-linux64/chrome-headless-shell")
            })
            .filter(|p| p.exists())
            .collect();
        found.sort();
        found.pop().expect("chrome-headless-shell")
    }

    fn is_root() -> bool {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
    }

    fn setup() -> (tempfile::TempDir, Arc<RunnerConfig>, Arc<Registry>, String) {
        let t = tempfile::tempdir().unwrap();
        let mut cfg = RunnerConfig::for_tests(t.path().to_path_buf());
        cfg.chromium = Some(chromium());
        cfg.chromium_no_sandbox = is_root();
        let ws = format!("t{}", "a".repeat(32));
        std::fs::create_dir_all(cfg.workspaces().join(&ws)).unwrap();
        (t, Arc::new(cfg), Arc::new(Registry::default()), ws)
    }

    fn dead(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            // A zombie (no reaping init in some containers) is dead too.
            Ok(s) => s
                .rsplit(')')
                .next()
                .is_some_and(|r| r.trim_start().starts_with('Z')),
        }
    }

    /// A server that never answers is stopped at the ready timeout – the
    /// whole process group, including a grandchild it forked.
    #[tokio::test]
    async fn timeout_stops_the_whole_process_group() {
        let (_t, cfg, reg, ws) = setup();
        // The grandchild ignores SIGTERM/SIGINT: only SIGKILL to the group ends it.
        let mut r = req("(trap '' TERM INT; exec sleep 300) & echo $! > child.pid; sleep 300");
        r.ready_timeout_secs = 2;
        r.keep = true;
        let out = start(cfg.clone(), reg.clone(), &ws, r).await.unwrap();
        assert!(!out.ready && !out.running);
        assert!(out.error.unwrap().contains("nicht bereit"));
        assert!(reg.names().await.is_empty());
        let pid: u32 = std::fs::read_to_string(cfg.workspaces().join(&ws).join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut ok = false;
        for _ in 0..50 {
            if dead(pid) {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ok, "grandchild {pid} survived the timeout");
        assert!(
            !preview_dir(&cfg, "q-0123456789ab-preview").exists(),
            "directory removed"
        );
    }

    /// Ready check, desktop and mobile screenshots (real PNGs from the
    /// headless Chromium), then the server is gone.
    #[tokio::test]
    async fn screenshots_are_real_pngs_and_the_server_stops() {
        let (_t, cfg, reg, ws) = setup();
        std::fs::write(
            cfg.workspaces().join(&ws).join("index.html"),
            "<!doctype html><title>x</title><h1 style='color:#c00'>Vorschau</h1>",
        )
        .unwrap();
        let mut r =
            req("echo $$ > server.pid; exec python3 -m http.server \"$PORT\" --bind 127.0.0.1");
        r.shots = vec![
            ShotSpec {
                path: "/".into(),
                viewport: ShotViewport::Desktop,
            },
            ShotSpec {
                path: "/index.html".into(),
                viewport: ShotViewport::Mobile,
            },
        ];
        let out = start(cfg.clone(), reg.clone(), &ws, r).await.unwrap();
        assert!(out.ready, "{:?} {}", out.error, out.log_tail);
        assert_eq!(out.shots.len(), 2);
        for s in &out.shots {
            assert!(s.error.is_none(), "{:?}", s.error);
            let png = base64::engine::general_purpose::STANDARD
                .decode(&s.png_b64)
                .unwrap();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
            // IHDR width: desktop 1280, mobile 390 × device scale 2.
            let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
            match s.viewport {
                ShotViewport::Desktop => assert_eq!(w, 1280),
                ShotViewport::Mobile => assert_eq!(w, 780),
            }
        }
        assert!(!out.running && reg.names().await.is_empty());
        let pid: u32 = std::fs::read_to_string(cfg.workspaces().join(&ws).join("server.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(dead(pid), "dev server still running");
    }

    /// Live preview: requests reach the server without the app's
    /// credentials, cookies and security headers of the server are dropped;
    /// the lifetime ends it.
    #[tokio::test]
    async fn forwarding_filters_credentials_and_lifetime_ends_it() {
        let (_t, cfg, reg, ws) = setup();
        let mut r = req("exec node -e \"require('http').createServer((q,s)=>{s.setHeader('Set-Cookie','sid=evil; Path=/');s.setHeader('X-Frame-Options','DENY');s.setHeader('Content-Security-Policy','none');s.setHeader('X-Own','1');s.statusCode=q.url==='/missing'?404:200;s.end(JSON.stringify({url:q.url,method:q.method,h:q.headers}))}).listen(process.env.PORT,'127.0.0.1')\"");
        r.keep = true;
        r.max_lifetime_secs = 3;
        let out = start(cfg.clone(), reg.clone(), &ws, r).await.unwrap();
        assert!(out.ready && out.running, "{:?} {}", out.error, out.log_tail);
        let hdr = |k: &str, v: &str| (k.to_string(), v.as_bytes().to_vec());
        let f = forward(
            &reg,
            "q-0123456789ab-preview",
            "POST",
            "/a/b?x=1",
            &[
                hdr("cookie", "qsession=secret"),
                hdr("authorization", "Bearer app-token"),
                hdr("x-forwarded-for", "1.2.3.4"),
                hdr("accept", "text/html"),
                hdr("content-type", "text/plain"),
            ],
            Bytes::from_static(b"body"),
        )
        .await
        .unwrap();
        assert_eq!(f.status, 200);
        let v: serde_json::Value = serde_json::from_slice(&f.body).unwrap();
        assert_eq!(v["url"], "/a/b?x=1");
        assert_eq!(v["method"], "POST");
        let h = v["h"].as_object().unwrap();
        assert!(
            !h.contains_key("cookie") && !h.contains_key("authorization"),
            "{h:?}"
        );
        assert!(!h.contains_key("x-forwarded-for"));
        assert_eq!(h["accept"], "text/html");
        let names: Vec<&str> = f.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            !names.contains(&"set-cookie") && !names.contains(&"x-frame-options"),
            "{names:?}"
        );
        assert!(!names.contains(&"content-security-policy"));
        assert!(names.contains(&"x-own"));
        let nf = forward(
            &reg,
            "q-0123456789ab-preview",
            "GET",
            "/missing",
            &[],
            Bytes::new(),
        )
        .await
        .unwrap();
        assert_eq!(nf.status, 404, "the server's status passes through");
        assert!(forward(
            &reg,
            "q-0123456789ab-preview",
            "GET",
            "//evil\r\nx",
            &[],
            Bytes::new()
        )
        .await
        .is_err());
        assert!(forward(
            &reg,
            "q-0123456789ab-preview",
            "CONNECT",
            "/",
            &[],
            Bytes::new()
        )
        .await
        .is_err());
        // Lifetime 3 s: the reaper stops it.
        let mut gone = false;
        for _ in 0..40 {
            if reg.names().await.is_empty() {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(gone, "lifetime not enforced");
        assert!(forward(
            &reg,
            "q-0123456789ab-preview",
            "GET",
            "/",
            &[],
            Bytes::new()
        )
        .await
        .is_err());
    }
}
