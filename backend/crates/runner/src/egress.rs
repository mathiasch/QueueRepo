//! Per-run egress proxy (WP02).
//!
//! Containers run with `--network none`: their only way out is a Unix socket
//! (bind-mounted read-only into the container) on which this proxy listens;
//! a tiny bridge in the container ([`BRIDGE_JS`]) offers it as
//! `http://127.0.0.1:3128`. The proxy speaks HTTP/1.1 `CONNECT` (tunnels) and
//! absolute-form `http://` requests and decides per connection:
//!
//! 1. host name against the policy – **before** any DNS lookup, so refused
//!    names never leave the host as queries;
//! 2. DNS lookup in the runner, every address filtered (link-local/metadata,
//!    loopback, private networks, CGNAT, the host's own addresses, operator
//!    deny list); the connection goes to a checked address (no rebinding
//!    window between check and connect);
//! 3. read-only phases: every request must be GET/HEAD/OPTIONS without body
//!    or upgrade, with a Host header for the tunnel's host. HTTPS is
//!    terminated with a CA generated per run (upstream TLS is verified
//!    against the Mozilla roots plus `RUNNER_EGRESS_EXTRA_CA`).
//!
//! Every decision is reported as [`ExecFrame::Egress`].
//!
//! Engine `none` has no network namespace: the proxy listens on loopback TCP
//! and only programs that honour `HTTPS_PROXY` are subject to it.
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use anyhow::{Context as _, Result};
use queued_common::{
    net::{self, Cidr, HostRule},
    EgressPolicy, EgressVerdict, ExecFrame, NetMode,
};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream, UnixListener},
    sync::{mpsc, Semaphore},
    task::JoinSet,
};

/// Mount point of the run's egress directory inside containers.
pub const CONTAINER_DIR: &str = "/queue-egress";
/// Port of the bridge inside the container (loopback only).
pub const BRIDGE_PORT: u16 = 3128;
pub const SOCKET_NAME: &str = "p.sock";

/// Bridge `127.0.0.1:<port>` → Unix socket, run with the image's `node`
/// (written by the runner, never baked into the image). Writes `ready` once
/// it listens. If it dies, the container simply has no network.
pub const BRIDGE_JS: &str = r#"'use strict';
const net = require('net'), fs = require('fs');
const [sock, port, ready] = process.argv.slice(2);
net.createServer((c) => {
  const u = net.connect(sock);
  const end = () => { c.destroy(); u.destroy(); };
  c.on('error', end); u.on('error', end); c.on('close', end); u.on('close', end);
  c.pipe(u); u.pipe(c);
}).listen(Number(port), '127.0.0.1', () => { try { fs.writeFileSync(ready, ''); } catch (e) {} });
"#;

/// `sh -c` wrapper for containers: start the bridge, wait (≤ 10 s) until it
/// listens, then `exec` the real program (`"$@"`).
pub const CONTAINER_WRAPPER: &str = "node /queue-egress/bridge.js /queue-egress/p.sock 3128 /tmp/.queue-egress-ready 2>/dev/null & i=0; while [ ! -e /tmp/.queue-egress-ready ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done; exec \"$@\"";

const MAX_HEAD: usize = 32 * 1024;
const MAX_CONNECTIONS: usize = 128;
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Operator settings of the runner (`RUNNER_EGRESS_*`).
#[derive(Clone, Debug, Default)]
pub struct EgressConfig {
    /// Hosts that may resolve to loopback/private addresses (LAN registries,
    /// self-hosted model servers). Never link-local/metadata.
    pub private_hosts: Vec<HostRule>,
    /// Always refused (operator list `RUNNER_EGRESS_DENY`), even for
    /// `private_hosts`.
    pub deny: Vec<Cidr>,
    /// The host's own addresses: refused unless the name is a private host.
    pub own: Vec<Cidr>,
    /// Extra trust anchors for upstream TLS in read-only inspection.
    pub extra_roots: Vec<CertificateDer<'static>>,
}

impl EgressConfig {
    /// Whether a resolved address may be connected to for `host`.
    fn address_ok(&self, host: &str, port: u16, ip: IpAddr) -> bool {
        if net::forbidden_ip(ip, true).is_some() || self.deny.iter().any(|c| c.contains(ip)) {
            return false;
        }
        if self
            .private_hosts
            .iter()
            .any(|r| r.matches_host(host, port))
        {
            return true;
        }
        !self.own.iter().any(|c| c.contains(ip)) && net::forbidden_ip(ip, false).is_none()
    }
}

/// The policy of one run, parsed.
#[derive(Debug)]
struct Rules {
    mode: NetMode,
    allow: Vec<HostRule>,
    always: Vec<HostRule>,
    read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Allow {
        verdict: EgressVerdict,
        inspect: bool,
    },
    Deny(EgressVerdict),
}

impl Rules {
    fn compile(p: &EgressPolicy) -> Result<Rules> {
        Ok(Rules {
            mode: p.mode,
            allow: net::parse_rules(&p.allow).map_err(anyhow::Error::msg)?,
            always: net::parse_rules(&p.always).map_err(anyhow::Error::msg)?,
            read_only: p.read_only,
        })
    }

    /// Decision on the host name alone (no DNS yet).
    fn decide(&self, host: &str, port: u16) -> Decision {
        if net::any_match(&self.always, host, port) {
            return Decision::Allow {
                verdict: EgressVerdict::Provider,
                inspect: false,
            };
        }
        match self.mode {
            NetMode::Off => Decision::Deny(EgressVerdict::PhaseOff),
            NetMode::Allowlist if net::any_match(&self.allow, host, port) => Decision::Allow {
                verdict: EgressVerdict::Allowlist,
                inspect: self.read_only,
            },
            NetMode::Allowlist => Decision::Deny(EgressVerdict::NotAllowed),
            NetMode::Open => Decision::Allow {
                verdict: EgressVerdict::Open,
                inspect: self.read_only,
            },
        }
    }
}

/// Per-run CA for read-only inspection of HTTPS.
struct RunCa {
    issuer: Issuer<'static, KeyPair>,
    ca_der: CertificateDer<'static>,
    pem: String,
    leaf_key: KeyPair,
    servers: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl RunCa {
    fn new() -> Result<RunCa> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::new(Vec::<String>::new())?;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params
            .distinguished_name
            .push(DnType::CommonName, "Queue egress inspection (one run)");
        let cert = params.self_signed(&key)?;
        Ok(RunCa {
            pem: cert.pem(),
            ca_der: cert.der().clone(),
            issuer: Issuer::new(params, key),
            leaf_key: KeyPair::generate()?,
            servers: Mutex::new(HashMap::new()),
        })
    }

    /// TLS server config presenting a certificate for `host` (the CONNECT
    /// target, not the client's SNI).
    fn server_for(&self, host: &str) -> Result<Arc<ServerConfig>> {
        if let Some(c) = self.servers.lock().expect("ca lock").get(host) {
            return Ok(c.clone());
        }
        let params = CertificateParams::new(vec![host.to_string()])?;
        let leaf = params.signed_by(&self.leaf_key, &self.issuer)?;
        let mut cfg = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone(), self.ca_der.clone()],
                PrivateKeyDer::Pkcs8(self.leaf_key.serialize_der().into()),
            )?;
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let cfg = Arc::new(cfg);
        self.servers
            .lock()
            .expect("ca lock")
            .insert(host.to_string(), cfg.clone());
        Ok(cfg)
    }
}

struct Ctx {
    rules: Rules,
    cfg: Arc<EgressConfig>,
    log: mpsc::Sender<ExecFrame>,
    ca: Option<RunCa>,
    upstream_tls: Option<Arc<ClientConfig>>,
}

impl Ctx {
    async fn log(&self, host: &str, port: u16, method: &str, verdict: EgressVerdict) {
        let _ = self
            .log
            .send(ExecFrame::Egress {
                host: host.to_string(),
                port,
                method: method.to_string(),
                verdict,
            })
            .await;
    }
}

pub enum Listen {
    Unix(PathBuf),
    Tcp,
}

/// A running proxy; dropping it stops the listener and every connection.
pub struct Proxy {
    task: tokio::task::JoinHandle<()>,
    /// Loopback address (engine `none`).
    pub tcp_addr: Option<SocketAddr>,
    /// PEM of the run CA (read-only phases).
    pub ca_pem: Option<String>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start(
    listen: Listen,
    policy: &EgressPolicy,
    cfg: Arc<EgressConfig>,
    log: mpsc::Sender<ExecFrame>,
) -> Result<Proxy> {
    let rules = Rules::compile(policy).context("invalid egress rule")?;
    let (ca, upstream_tls) = if rules.read_only {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for c in &cfg.extra_roots {
            roots.add(c.clone())?;
        }
        let mut cc = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        cc.alpn_protocols = vec![b"http/1.1".to_vec()];
        (Some(RunCa::new()?), Some(Arc::new(cc)))
    } else {
        (None, None)
    };
    let ca_pem = ca.as_ref().map(|c| c.pem.clone());
    let ctx = Arc::new(Ctx {
        rules,
        cfg,
        log,
        ca,
        upstream_tls,
    });
    let (listener, tcp_addr) = match listen {
        Listen::Unix(path) => {
            let _ = tokio::fs::remove_file(&path).await;
            let l = UnixListener::bind(&path)
                .with_context(|| format!("egress socket {}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
            }
            (Listener::Unix(l), None)
        }
        Listen::Tcp => {
            let l = TcpListener::bind("127.0.0.1:0").await?;
            let addr = l.local_addr()?;
            (Listener::Tcp(l), Some(addr))
        }
    };
    let task = tokio::spawn(accept_loop(ctx, listener));
    Ok(Proxy {
        task,
        tcp_addr,
        ca_pem,
    })
}

/// A client connection (Unix socket or loopback TCP).
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

enum Listener {
    Unix(UnixListener),
    Tcp(TcpListener),
}

impl Listener {
    async fn accept(&self) -> io::Result<Box<dyn Io>> {
        Ok(match self {
            Listener::Unix(l) => Box::new(l.accept().await?.0),
            Listener::Tcp(l) => Box::new(l.accept().await?.0),
        })
    }
}

async fn accept_loop(ctx: Arc<Ctx>, listener: Listener) {
    let sem = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut conns = JoinSet::new();
    loop {
        let Ok(permit) = sem.clone().acquire_owned().await else {
            return;
        };
        match listener.accept().await {
            Ok(s) => {
                let ctx = ctx.clone();
                conns.spawn(async move {
                    let _p = permit;
                    if let Err(e) = handle(s, &ctx).await {
                        tracing::debug!("egress connection: {e:#}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("egress accept: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        while conns.try_join_next().is_some() {}
    }
}

/// Reads until the end of an HTTP head. `Ok(None)` = clean EOF before any byte.
async fn read_head<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> io::Result<Option<usize>> {
    loop {
        if let Some(p) = memchr::memmem::find(buf, b"\r\n\r\n") {
            return Ok(Some(p + 4));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "head too large"));
        }
        let mut chunk = [0u8; 4096];
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

struct Head {
    method: String,
    target: String,
    version: u8,
    headers: Vec<(String, Vec<u8>)>,
}

fn parse_head(raw: &[u8]) -> Option<Head> {
    let mut hs = [httparse::EMPTY_HEADER; 96];
    let mut req = httparse::Request::new(&mut hs);
    match req.parse(raw) {
        Ok(httparse::Status::Complete(_)) => Some(Head {
            method: req.method?.to_string(),
            target: req.path?.to_string(),
            version: req.version?,
            headers: req
                .headers
                .iter()
                .map(|h| (h.name.to_string(), h.value.to_vec()))
                .collect(),
        }),
        _ => None,
    }
}

impl Head {
    fn header(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_slice())
    }
}

/// Read-only rule for one request inside a tunnel/connection to `host:port`.
fn read_only_ok(h: &Head, host: &str, port: u16) -> bool {
    if !matches!(h.method.as_str(), "GET" | "HEAD" | "OPTIONS") {
        return false;
    }
    for (name, value) in &h.headers {
        let n = name.to_ascii_lowercase();
        let v = String::from_utf8_lossy(value);
        match n.as_str() {
            "content-length" if v.trim() != "0" => return false,
            "transfer-encoding" | "upgrade" | "expect" => return false,
            _ => {}
        }
    }
    let Some(hh) = h
        .header("host")
        .map(|v| String::from_utf8_lossy(v).to_string())
    else {
        return false;
    };
    match net::split_port(hh.trim()) {
        Some((hn, p)) => {
            net::normalize_host(hn).as_deref() == Some(host)
                && p.is_none_or(|p| p == port)
                && (p.is_some() || port == 443 || port == 80)
        }
        None => false,
    }
}

fn deny_response(verdict: EgressVerdict, host: &str, port: u16) -> Vec<u8> {
    let code = serde_json::to_value(verdict)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let (status, text) = match verdict {
        EgressVerdict::BadRequest => ("400 Bad Request", "bad proxy request"),
        EgressVerdict::ConnectFailed | EgressVerdict::Unresolvable => {
            ("502 Bad Gateway", "upstream not reachable")
        }
        _ => ("403 Forbidden", "blocked by the Queue egress policy"),
    };
    let body = format!("{text} ({code}): {host}:{port}\n");
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nX-Queue-Egress: {code}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

async fn refuse<S: AsyncWrite + Unpin>(
    s: &mut S,
    ctx: &Ctx,
    host: &str,
    port: u16,
    method: &str,
    verdict: EgressVerdict,
) -> Result<()> {
    ctx.log(host, port, method, verdict).await;
    s.write_all(&deny_response(verdict, host, port)).await?;
    let _ = s.shutdown().await;
    Ok(())
}

/// Resolves (or parses) `host` and keeps only permitted addresses.
async fn resolve(
    cfg: &EgressConfig,
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, EgressVerdict> {
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => {
            match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::lookup_host((host, port))).await
            {
                Ok(Ok(a)) => a.collect(),
                _ => return Err(EgressVerdict::Unresolvable),
            }
        }
    };
    let ok: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| cfg.address_ok(host, port, a.ip()))
        .collect();
    if ok.is_empty() {
        Err(EgressVerdict::BlockedAddress)
    } else {
        Ok(ok)
    }
}

async fn connect(addrs: &[SocketAddr]) -> Option<TcpStream> {
    for a in addrs {
        if let Ok(Ok(s)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(a)).await {
            return Some(s);
        }
    }
    None
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut client: S,
    ctx: &Ctx,
) -> Result<()> {
    let mut buf = Vec::new();
    let len = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client, &mut buf)).await {
        Ok(Ok(Some(n))) => n,
        _ => return Ok(()),
    };
    let Some(head) = parse_head(&buf[..len]) else {
        return refuse(&mut client, ctx, "", 0, "?", EgressVerdict::BadRequest).await;
    };
    let leftover = buf.split_off(len);
    if head.method == "CONNECT" {
        connect_tunnel(client, ctx, &head, leftover).await
    } else {
        plain_http(client, ctx, head, buf, leftover).await
    }
}

/// Target of a proxy request: normalized host and port, or `None`.
fn target(authority: &str, default_port: Option<u16>) -> Option<(String, u16)> {
    if authority.contains('@') {
        return None;
    }
    let (h, p) = net::split_port(authority)?;
    Some((net::normalize_host(h)?, p.or(default_port)?))
}

async fn connect_tunnel<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut client: S,
    ctx: &Ctx,
    head: &Head,
    leftover: Vec<u8>,
) -> Result<()> {
    let Some((host, port)) = target(&head.target, None) else {
        return refuse(
            &mut client,
            ctx,
            &head.target,
            0,
            "CONNECT",
            EgressVerdict::BadRequest,
        )
        .await;
    };
    let (verdict, inspect) = match ctx.rules.decide(&host, port) {
        Decision::Deny(v) => return refuse(&mut client, ctx, &host, port, "CONNECT", v).await,
        Decision::Allow { verdict, inspect } => (verdict, inspect),
    };
    let addrs = match resolve(&ctx.cfg, &host, port).await {
        Ok(a) => a,
        Err(v) => return refuse(&mut client, ctx, &host, port, "CONNECT", v).await,
    };
    let Some(upstream) = connect(&addrs).await else {
        return refuse(
            &mut client,
            ctx,
            &host,
            port,
            "CONNECT",
            EgressVerdict::ConnectFailed,
        )
        .await;
    };
    if inspect {
        return inspect_tls(client, ctx, &host, port, upstream, leftover, verdict).await;
    }
    ctx.log(&host, port, "CONNECT", verdict).await;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let mut upstream = upstream;
    if !leftover.is_empty() {
        upstream.write_all(&leftover).await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

async fn inspect_tls<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut client: S,
    ctx: &Ctx,
    host: &str,
    port: u16,
    upstream: TcpStream,
    leftover: Vec<u8>,
    verdict: EgressVerdict,
) -> Result<()> {
    let (Some(ca), Some(tls)) = (&ctx.ca, &ctx.upstream_tls) else {
        anyhow::bail!("inspection without CA");
    };
    let name = ServerName::try_from(host.to_string())?;
    let up = match tokio_rustls::TlsConnector::from(tls.clone())
        .connect(name, upstream)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("egress upstream TLS {host}:{port}: {e}");
            return refuse(
                &mut client,
                ctx,
                host,
                port,
                "CONNECT",
                EgressVerdict::ConnectFailed,
            )
            .await;
        }
    };
    ctx.log(host, port, "CONNECT", verdict).await;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let server = ca.server_for(host)?;
    let down = tokio_rustls::TlsAcceptor::from(server)
        .accept(Prefixed::new(leftover, client))
        .await?;
    relay_read_only(down, up, Vec::new(), ctx, host, port).await
}

async fn plain_http<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut client: S,
    ctx: &Ctx,
    head: Head,
    raw: Vec<u8>,
    leftover: Vec<u8>,
) -> Result<()> {
    let lower = head.target.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("http://").map(|_| &head.target[7..]) else {
        return refuse(
            &mut client,
            ctx,
            &head.target,
            0,
            &head.method,
            EgressVerdict::BadRequest,
        )
        .await;
    };
    let split = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(split);
    let path = match path {
        "" => "/".to_string(),
        p if p.starts_with('?') => format!("/{p}"),
        p => p.to_string(),
    };
    let Some((host, port)) = target(authority, Some(80)) else {
        return refuse(
            &mut client,
            ctx,
            authority,
            0,
            &head.method,
            EgressVerdict::BadRequest,
        )
        .await;
    };
    let (verdict, inspect) = match ctx.rules.decide(&host, port) {
        Decision::Deny(v) => return refuse(&mut client, ctx, &host, port, &head.method, v).await,
        Decision::Allow { verdict, inspect } => (verdict, inspect),
    };
    if inspect && !read_only_ok(&head, &host, port) {
        return refuse(
            &mut client,
            ctx,
            &host,
            port,
            &head.method,
            EgressVerdict::ReadOnly,
        )
        .await;
    }
    let addrs = match resolve(&ctx.cfg, &host, port).await {
        Ok(a) => a,
        Err(v) => return refuse(&mut client, ctx, &host, port, &head.method, v).await,
    };
    let Some(mut upstream) = connect(&addrs).await else {
        return refuse(
            &mut client,
            ctx,
            &host,
            port,
            &head.method,
            EgressVerdict::ConnectFailed,
        )
        .await;
    };
    ctx.log(&host, port, &head.method, verdict).await;
    // Origin-form request line, proxy headers dropped.
    let mut out = format!("{} {path} HTTP/1.{}\r\n", head.method, head.version.min(1)).into_bytes();
    if head.header("host").is_none() {
        out.extend_from_slice(format!("Host: {authority}\r\n").as_bytes());
    }
    for (n, v) in &head.headers {
        if n.to_ascii_lowercase().starts_with("proxy-") {
            continue;
        }
        out.extend_from_slice(n.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    drop(raw);
    upstream.write_all(&out).await?;
    if inspect {
        relay_read_only(client, upstream, leftover, ctx, &host, port).await
    } else {
        if !leftover.is_empty() {
            upstream.write_all(&leftover).await?;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        Ok(())
    }
}

/// Relays a connection in which every client request must pass
/// [`read_only_ok`]. Requests have no body, so the client stream is a plain
/// sequence of heads. The first violation answers 403 if no response has
/// started yet, otherwise the connection is closed.
async fn relay_read_only<C, U>(
    client: C,
    upstream: U,
    mut buf: Vec<u8>,
    ctx: &Ctx,
    host: &str,
    port: u16,
) -> Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut cr, cw) = tokio::io::split(client);
    let (ur, mut uw) = tokio::io::split(upstream);
    // Both move into the response relay once the first request passed; the
    // client half is needed before that for a 403.
    let mut pending = Some((ur, cw));
    let mut responses: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let len = match tokio::time::timeout(Duration::from_secs(600), read_head(&mut cr, &mut buf))
            .await
        {
            Ok(Ok(Some(n))) => n,
            // Client finished (or broke): let the responses drain.
            _ => break,
        };
        let head = parse_head(&buf[..len]);
        if !head.as_ref().is_some_and(|h| read_only_ok(h, host, port)) {
            let method = head.map(|h| h.method).unwrap_or_else(|| "?".into());
            ctx.log(host, port, &method, EgressVerdict::ReadOnly).await;
            if let Some(r) = responses.take() {
                r.abort();
            }
            if let Some((_, mut w)) = pending.take() {
                let _ = w
                    .write_all(&deny_response(EgressVerdict::ReadOnly, host, port))
                    .await;
                let _ = w.shutdown().await;
            }
            return Ok(());
        }
        uw.write_all(&buf[..len]).await?;
        uw.flush().await?;
        buf.drain(..len);
        if let Some((mut r, mut w)) = pending.take() {
            responses = Some(tokio::spawn(async move {
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            }));
        }
    }
    let _ = uw.shutdown().await;
    if let Some(r) = responses {
        let _ = r.await;
    }
    Ok(())
}

/// A stream that first yields bytes already read from it.
struct Prefixed<S> {
    prefix: Vec<u8>,
    pos: usize,
    inner: S,
}

impl<S> Prefixed<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            pos: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pos < self.prefix.len() {
            let n = (self.prefix.len() - self.pos).min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.prefix[start..start + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, b)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Environment for a process behind the proxy at `proxy_url`.
pub fn proxy_env(proxy_url: &str) -> Vec<(String, String)> {
    let mut v = vec![];
    for k in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
        v.push((k.to_string(), proxy_url.to_string()));
        v.push((k.to_ascii_lowercase(), proxy_url.to_string()));
    }
    for k in ["NO_PROXY", "no_proxy"] {
        v.push((k.to_string(), "localhost,127.0.0.1,::1".to_string()));
    }
    v
}

/// Environment pointing TLS clients to the run CA (`ca`) and to a full
/// bundle (`bundle` = system roots + run CA, if a system bundle exists).
pub fn ca_env(ca: &str, bundle: Option<&str>) -> Vec<(String, String)> {
    let mut v = vec![("NODE_EXTRA_CA_CERTS".to_string(), ca.to_string())];
    if let Some(b) = bundle {
        for k in [
            "SSL_CERT_FILE",
            "CURL_CA_BUNDLE",
            "REQUESTS_CA_BUNDLE",
            "PIP_CERT",
            "GIT_SSL_CAINFO",
            "CARGO_HTTP_CAINFO",
        ] {
            v.push((k.to_string(), b.to_string()));
        }
    }
    v
}

/// The host's CA bundle (first one found).
pub fn system_bundle() -> Option<String> {
    [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/cert.pem",
    ]
    .iter()
    .find_map(|p| std::fs::read_to_string(p).ok())
}

/// Writes the files of a run's egress directory (bridge, CA, bundle).
pub async fn write_dir(dir: &Path, ca_pem: Option<&str>) -> Result<()> {
    tokio::fs::write(dir.join("bridge.js"), BRIDGE_JS).await?;
    if let Some(ca) = ca_pem {
        tokio::fs::write(dir.join("ca.pem"), ca).await?;
        if let Some(sys) = system_bundle() {
            tokio::fs::write(dir.join("ca-bundle.pem"), format!("{sys}\n{ca}")).await?;
        }
    }
    Ok(())
}

/// IPv4 addresses marked `host LOCAL` in `/proc/net/fib_trie`.
pub fn parse_fib_trie(s: &str) -> Vec<IpAddr> {
    let mut last: Option<IpAddr> = None;
    let mut out = vec![];
    for line in s.lines() {
        let t = line.trim();
        if let Some(ip) = t.strip_prefix("|-- ") {
            last = ip.trim().parse().ok();
        } else if t.contains("/32 host LOCAL") {
            if let Some(ip) = last {
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
        }
    }
    out
}

/// Addresses in `/proc/net/if_inet6`.
pub fn parse_if_inet6(s: &str) -> Vec<IpAddr> {
    s.lines()
        .filter_map(|l| {
            let hex = l.split_whitespace().next()?;
            (hex.len() == 32)
                .then(|| u128::from_str_radix(hex, 16).ok())
                .flatten()
                .map(|v| IpAddr::V6(std::net::Ipv6Addr::from(v)))
        })
        .collect()
}

/// The host's own addresses (blocked as egress targets: services bound to
/// a public interface of the host must not be reachable from agents).
pub fn host_addresses() -> Vec<Cidr> {
    let mut ips =
        parse_fib_trie(&std::fs::read_to_string("/proc/net/fib_trie").unwrap_or_default());
    ips.extend(parse_if_inet6(
        &std::fs::read_to_string("/proc/net/if_inet6").unwrap_or_default(),
    ));
    ips.into_iter()
        .filter_map(|ip| Cidr::parse(&ip.to_string()))
        .collect()
}

/// PEM certificates from a file (`RUNNER_EGRESS_EXTRA_CA`).
pub fn load_roots(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs: Vec<_> = CertificateDer::pem_file_iter(path)
        .with_context(|| format!("read {}", path.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("parse {}", path.display()))?;
    anyhow::ensure!(!certs.is_empty(), "no certificate in {}", path.display());
    Ok(certs)
}

#[cfg(test)]
mod tests;
