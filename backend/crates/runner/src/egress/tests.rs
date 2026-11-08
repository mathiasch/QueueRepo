use std::sync::Mutex as StdMutex;

use tokio::io::{AsyncBufReadExt, BufReader};

use super::*;

fn rules(mode: NetMode, allow: &[&str], always: &[&str], read_only: bool) -> Rules {
    Rules::compile(&EgressPolicy {
        mode,
        allow: allow.iter().map(|s| s.to_string()).collect(),
        always: always.iter().map(|s| s.to_string()).collect(),
        read_only,
    })
    .unwrap()
}

#[test]
fn decisions_follow_mode_allowlist_and_provider_hosts() {
    let api = "api.anthropic.com";
    let off = rules(NetMode::Off, &["pypi.org"], &[api], false);
    assert_eq!(
        off.decide(api, 443),
        Decision::Allow {
            verdict: EgressVerdict::Provider,
            inspect: false
        }
    );
    assert_eq!(
        off.decide("pypi.org", 443),
        Decision::Deny(EgressVerdict::PhaseOff)
    );

    let al = rules(NetMode::Allowlist, &["pypi.org"], &[api], true);
    assert_eq!(
        al.decide("pypi.org", 443),
        Decision::Allow {
            verdict: EgressVerdict::Allowlist,
            inspect: true
        }
    );
    // Provider hosts are never inspected, even in a read-only phase.
    assert_eq!(
        al.decide(api, 443),
        Decision::Allow {
            verdict: EgressVerdict::Provider,
            inspect: false
        }
    );
    assert_eq!(
        al.decide("evil.com", 443),
        Decision::Deny(EgressVerdict::NotAllowed)
    );
    assert_eq!(
        al.decide("pypi.org", 22),
        Decision::Deny(EgressVerdict::NotAllowed)
    );

    let open = rules(NetMode::Open, &[], &[], false);
    assert_eq!(
        open.decide("anything.example", 5432),
        Decision::Allow {
            verdict: EgressVerdict::Open,
            inspect: false
        }
    );
    assert!(Rules::compile(&EgressPolicy {
        mode: NetMode::Allowlist,
        allow: vec!["https://bad".into()],
        ..Default::default()
    })
    .is_err());
}

#[test]
fn address_filter_layers() {
    let cfg = EgressConfig {
        private_hosts: vec![
            HostRule::parse("nexus.lan").unwrap(),
            HostRule::parse("169.254.169.254").unwrap(),
        ],
        deny: vec![Cidr::parse("203.0.113.0/24").unwrap()],
        own: vec![
            Cidr::parse("198.51.100.7").unwrap(),
            Cidr::parse("10.0.0.5").unwrap(),
        ],
        extra_roots: vec![],
    };
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    // Metadata/link-local: never, not even for a listed private host.
    assert!(!cfg.address_ok("169.254.169.254", 80, ip("169.254.169.254")));
    // Private networks only for listed hosts.
    assert!(cfg.address_ok("nexus.lan", 8081, ip("10.0.0.9")));
    assert!(!cfg.address_ok("other.lan", 443, ip("10.0.0.9")));
    // The host's own addresses: public one refused, also for open egress.
    assert!(!cfg.address_ok("myhost.example", 443, ip("198.51.100.7")));
    assert!(cfg.address_ok("nexus.lan", 443, ip("10.0.0.5")));
    // Operator deny list wins over everything.
    assert!(!cfg.address_ok("nexus.lan", 443, ip("203.0.113.9")));
    assert!(cfg.address_ok("pypi.org", 443, ip("151.101.0.223")));
}

#[test]
fn parses_host_addresses_from_proc() {
    let fib = "Main:\n  +-- 0.0.0.0/0 3 0 5\n     |-- 0.0.0.0\n        /0 universe UNICAST\n     +-- 10.0.0.0/24 2 0 2\n        |-- 10.0.0.0\n           /24 link UNICAST\n        |-- 10.0.0.5\n           /32 host LOCAL\n     |-- 127.0.0.1\n        /32 host LOCAL\nLocal:\n        |-- 10.0.0.5\n           /32 host LOCAL\n";
    assert_eq!(
        parse_fib_trie(fib),
        vec![
            "10.0.0.5".parse::<IpAddr>().unwrap(),
            "127.0.0.1".parse().unwrap()
        ]
    );
    let v6 = "00000000000000000000000000000001 01 80 10 80       lo\nfe800000000000000000000000000001 02 40 20 80     eth0\nbad line\n";
    assert_eq!(
        parse_if_inet6(v6),
        vec!["::1".parse::<IpAddr>().unwrap(), "fe80::1".parse().unwrap()]
    );
    assert!(parse_fib_trie("").is_empty());
}

#[test]
fn read_only_request_rules() {
    let h = |raw: &str| parse_head(raw.as_bytes()).unwrap();
    let ok = h("GET /x HTTP/1.1\r\nHost: pypi.org\r\n\r\n");
    assert!(read_only_ok(&ok, "pypi.org", 443));
    assert!(read_only_ok(
        &h("HEAD / HTTP/1.1\r\nHost: PyPI.org:443\r\nContent-Length: 0\r\n\r\n"),
        "pypi.org",
        443
    ));
    for bad in [
        "POST /x HTTP/1.1\r\nHost: pypi.org\r\n\r\n",
        "GET /x HTTP/1.1\r\nHost: pypi.org\r\nContent-Length: 5\r\n\r\n",
        "GET /x HTTP/1.1\r\nHost: pypi.org\r\nTransfer-Encoding: chunked\r\n\r\n",
        "GET /x HTTP/1.1\r\nHost: pypi.org\r\nUpgrade: websocket\r\n\r\n",
        "GET /x HTTP/1.1\r\nHost: evil.com\r\n\r\n",
        "GET /x HTTP/1.1\r\nHost: pypi.org:8443\r\n\r\n",
        "GET /x HTTP/1.1\r\n\r\n",
    ] {
        assert!(!read_only_ok(&h(bad), "pypi.org", 443), "{bad:?}");
    }
}

/// Plain HTTP upstream on 127.0.0.1 answering `ok` to each request and
/// recording the request heads.
async fn upstream() -> (u16, Arc<StdMutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(StdMutex::new(vec![]));
    let s2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let seen = s2.clone();
            tokio::spawn(async move { serve_ok(s, seen).await });
        }
    });
    (port, seen)
}

async fn serve_ok<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, seen: Arc<StdMutex<Vec<String>>>) {
    let mut buf = vec![];
    while let Ok(Some(n)) = read_head(&mut s, &mut buf).await {
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        seen.lock().unwrap().push(head.clone());
        buf.drain(..n);
        if s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .is_err()
        {
            return;
        }
        if head.to_ascii_lowercase().contains("connection: close") {
            let _ = s.shutdown().await;
            return;
        }
    }
}

fn test_cfg(private: &[&str]) -> Arc<EgressConfig> {
    Arc::new(EgressConfig {
        private_hosts: private
            .iter()
            .map(|h| HostRule::parse(h).unwrap())
            .collect(),
        ..Default::default()
    })
}

async fn proxy(policy: EgressPolicy, cfg: Arc<EgressConfig>) -> (Proxy, mpsc::Receiver<ExecFrame>) {
    let (tx, rx) = mpsc::channel(64);
    (start(Listen::Tcp, &policy, cfg, tx).await.unwrap(), rx)
}

fn allow(mode: NetMode, allow: &[String], read_only: bool) -> EgressPolicy {
    EgressPolicy {
        mode,
        allow: allow.to_vec(),
        always: vec![],
        read_only,
    }
}

/// Sends a proxy request head, returns the stream (positioned after the
/// response head, nothing buffered away) and the status line.
async fn request(p: &Proxy, head: &str) -> (TcpStream, String) {
    let mut s = TcpStream::connect(p.tcp_addr.unwrap()).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    let mut got = vec![];
    let mut b = [0u8; 1];
    while !got.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).await.unwrap() == 0 {
            break;
        }
        got.push(b[0]);
    }
    let text = String::from_utf8_lossy(&got).to_string();
    (
        s,
        text.lines().next().unwrap_or_default().trim().to_string(),
    )
}

async fn verdict(rx: &mut mpsc::Receiver<ExecFrame>) -> (String, u16, String, EgressVerdict) {
    match tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ExecFrame::Egress {
            host,
            port,
            method,
            verdict,
        } => (host, port, method, verdict),
        o => panic!("{o:?}"),
    }
}

async fn read_all(s: &mut TcpStream) -> String {
    let mut out = vec![];
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).to_string()
}

#[tokio::test]
async fn connect_tunnel_to_allowed_host() {
    let (port, seen) = upstream().await;
    let (p, mut rx) = proxy(
        allow(NetMode::Allowlist, &[format!("127.0.0.1:{port}")], false),
        test_cfg(&["127.0.0.1"]),
    )
    .await;
    let (mut s, status) = request(
        &p,
        &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: x\r\n\r\n"),
    )
    .await;
    assert_eq!(status, "HTTP/1.1 200 Connection Established");
    s.write_all(b"GET /tunnel HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).ends_with("ok"));
    assert_eq!(
        verdict(&mut rx).await,
        (
            "127.0.0.1".into(),
            port,
            "CONNECT".into(),
            EgressVerdict::Allowlist
        )
    );
    assert!(seen.lock().unwrap()[0].starts_with("GET /tunnel"));
}

#[tokio::test]
async fn refuses_before_dns_and_logs() {
    let (p, mut rx) = proxy(
        allow(NetMode::Allowlist, &["pypi.org".into()], false),
        test_cfg(&[]),
    )
    .await;
    // `.invalid` never resolves: NotAllowed (not Unresolvable) proves the
    // decision was made on the name, before any lookup.
    let (mut s, status) = request(&p, "CONNECT exfil.secret.invalid:443 HTTP/1.1\r\n\r\n").await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert!(read_all(&mut s).await.contains("not_allowed"));
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::NotAllowed);
    // An allowed name that does not resolve is reported as such.
    let (p, mut rx) = proxy(
        allow(NetMode::Allowlist, &["nothing.invalid".into()], false),
        test_cfg(&[]),
    )
    .await;
    let (_, status) = request(&p, "CONNECT nothing.invalid:443 HTTP/1.1\r\n\r\n").await;
    assert_eq!(status, "HTTP/1.1 502 Bad Gateway");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::Unresolvable);
}

#[tokio::test]
async fn blocks_internal_addresses_even_when_allowlisted() {
    let (port, seen) = upstream().await;
    // `localhost` is allowlisted but resolves to loopback and is no private host.
    let (p, mut rx) = proxy(
        allow(
            NetMode::Allowlist,
            &[format!("localhost:{port}"), "169.254.169.254".into()],
            false,
        ),
        test_cfg(&["169.254.169.254"]),
    )
    .await;
    let (_, status) = request(&p, &format!("CONNECT localhost:{port} HTTP/1.1\r\n\r\n")).await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::BlockedAddress);
    let (_, status) = request(
        &p,
        "GET http://169.254.169.254/latest/meta-data/ HTTP/1.1\r\nHost: 169.254.169.254\r\n\r\n",
    )
    .await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::BlockedAddress);
    // Open mode: public only.
    let (p, mut rx) = proxy(allow(NetMode::Open, &[], false), test_cfg(&[])).await;
    let (_, status) = request(&p, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n")).await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::BlockedAddress);
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing reached the upstream"
    );
}

#[tokio::test]
async fn off_phase_allows_only_provider_hosts() {
    let (port, _) = upstream().await;
    let (p, mut rx) = proxy(
        EgressPolicy {
            mode: NetMode::Off,
            allow: vec![],
            always: vec![format!("127.0.0.1:{port}")],
            read_only: true,
        },
        test_cfg(&["127.0.0.1"]),
    )
    .await;
    let (_, status) = request(&p, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n")).await;
    assert_eq!(status, "HTTP/1.1 200 Connection Established");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::Provider);
    let (_, status) = request(&p, "CONNECT registry.npmjs.org:443 HTTP/1.1\r\n\r\n").await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::PhaseOff);
    let (_, status) = request(&p, "BREW coffee\r\n\r\n").await;
    assert_eq!(status, "HTTP/1.1 400 Bad Request");
}

#[tokio::test]
async fn plain_http_is_rewritten_to_origin_form() {
    let (port, seen) = upstream().await;
    let (p, mut rx) = proxy(
        allow(NetMode::Allowlist, &[format!("127.0.0.1:{port}")], false),
        test_cfg(&["127.0.0.1"]),
    )
    .await;
    let mut s = TcpStream::connect(p.tcp_addr.unwrap()).await.unwrap();
    s.write_all(
        format!(
            "POST http://127.0.0.1:{port}/simple?q=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nProxy-Authorization: Basic eDp5\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    assert!(read_all(&mut s).await.ends_with("ok"));
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::Allowlist);
    let head = seen.lock().unwrap()[0].clone();
    assert!(head.starts_with("POST /simple?q=1 HTTP/1.1\r\n"), "{head}");
    assert!(
        !head.to_lowercase().contains("proxy-authorization"),
        "{head}"
    );
}

#[tokio::test]
async fn read_only_plain_http_blocks_writes_and_pipelined_writes() {
    let (port, seen) = upstream().await;
    let (p, mut rx) = proxy(
        allow(NetMode::Allowlist, &[format!("127.0.0.1:{port}")], true),
        test_cfg(&["127.0.0.1"]),
    )
    .await;
    let (mut s, status) = request(
        &p,
        &format!("POST http://127.0.0.1:{port}/x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\n\r\n"),
    )
    .await;
    assert_eq!(status, "HTTP/1.1 403 Forbidden");
    assert!(read_all(&mut s).await.contains("read_only"));
    assert_eq!(
        verdict(&mut rx).await,
        (
            "127.0.0.1".into(),
            port,
            "POST".into(),
            EgressVerdict::ReadOnly
        )
    );
    assert!(seen.lock().unwrap().is_empty());

    // GET passes; a POST on the same keep-alive connection never reaches
    // the upstream.
    let mut s = TcpStream::connect(p.tcp_addr.unwrap()).await.unwrap();
    s.write_all(
        format!(
            "GET http://127.0.0.1:{port}/a HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\nPOST /b HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let _ = read_all(&mut s).await;
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::Allowlist);
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::ReadOnly);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].starts_with("GET /a"));
}

/// TLS upstream on 127.0.0.1 with its own CA (as `RUNNER_EGRESS_EXTRA_CA`).
async fn tls_upstream() -> (u16, CertificateDer<'static>, Arc<StdMutex<Vec<String>>>) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_string()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    let cfg = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(StdMutex::new(vec![]));
    let s2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let (acceptor, seen) = (acceptor.clone(), s2.clone());
            tokio::spawn(async move {
                if let Ok(t) = acceptor.accept(s).await {
                    serve_ok(t, seen).await;
                }
            });
        }
    });
    (port, ca.der().clone(), seen)
}

/// Opens a CONNECT tunnel and a TLS session trusting the run CA.
async fn tls_through(p: &Proxy, port: u16) -> tokio_rustls::client::TlsStream<TcpStream> {
    let (s, status) = request(p, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n")).await;
    assert_eq!(status, "HTTP/1.1 200 Connection Established");
    let mut roots = RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(p.ca_pem.as_ref().unwrap().as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let cc = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(cc))
        .connect(ServerName::try_from("127.0.0.1").unwrap(), s)
        .await
        .unwrap()
}

#[tokio::test]
async fn read_only_https_is_inspected_with_the_run_ca() {
    let (port, upstream_ca, seen) = tls_upstream().await;
    let cfg = Arc::new(EgressConfig {
        private_hosts: vec![HostRule::parse("127.0.0.1").unwrap()],
        extra_roots: vec![upstream_ca],
        ..Default::default()
    });
    let (p, mut rx) = proxy(
        allow(NetMode::Allowlist, &[format!("127.0.0.1:{port}")], true),
        cfg,
    )
    .await;
    assert!(p.ca_pem.as_deref().unwrap().contains("BEGIN CERTIFICATE"));

    let mut t = tls_through(&p, port).await;
    t.write_all(
        format!("GET /pkg HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut out = vec![];
    let _ = t.read_to_end(&mut out).await;
    assert!(String::from_utf8_lossy(&out).ends_with("ok"));
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::Allowlist);

    let mut t = tls_through(&p, port).await;
    t.write_all(
        format!("POST /upload HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 3\r\n\r\nabc")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut out = vec![];
    let _ = t.read_to_end(&mut out).await;
    let out = String::from_utf8_lossy(&out);
    assert!(out.starts_with("HTTP/1.1 403"), "{out}");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::Allowlist);
    assert_eq!(
        verdict(&mut rx).await,
        (
            "127.0.0.1".into(),
            port,
            "POST".into(),
            EgressVerdict::ReadOnly
        )
    );
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "POST never reached the upstream: {seen:?}");
}

#[tokio::test]
async fn untrusted_upstream_certificate_is_refused_in_inspection() {
    let (port, _ca, seen) = tls_upstream().await;
    // Upstream CA not trusted: the proxy must not hand out a valid tunnel.
    let (p, mut rx) = proxy(
        allow(NetMode::Allowlist, &[format!("127.0.0.1:{port}")], true),
        test_cfg(&["127.0.0.1"]),
    )
    .await;
    let (_, status) = request(&p, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n")).await;
    assert_eq!(status, "HTTP/1.1 502 Bad Gateway");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::ConnectFailed);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unix_socket_listener_and_node_bridge() {
    let (port, _) = upstream().await;
    let t = tempfile::tempdir().unwrap();
    let sock = t.path().join(SOCKET_NAME);
    let (tx, mut rx) = mpsc::channel(8);
    let _p = start(
        Listen::Unix(sock.clone()),
        &allow(NetMode::Allowlist, &[format!("127.0.0.1:{port}")], false),
        test_cfg(&["127.0.0.1"]),
        tx,
    )
    .await
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // The bridge from the container wrapper, run with the real `node`.
    write_dir(t.path(), None).await.unwrap();
    let bridge_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let ready = t.path().join("ready");
    let mut node = tokio::process::Command::new("node")
        .arg(t.path().join("bridge.js"))
        .arg(&sock)
        .arg(bridge_port.to_string())
        .arg(&ready)
        .kill_on_drop(true)
        .spawn()
        .expect("node is required for the egress bridge test (the agent image is node-based)");
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready.exists(), "bridge signalled readiness");
    let mut s = TcpStream::connect(("127.0.0.1", bridge_port))
        .await
        .unwrap();
    s.write_all(format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut r = BufReader::new(&mut s);
    let mut line = String::new();
    r.read_line(&mut line).await.unwrap();
    assert_eq!(line.trim(), "HTTP/1.1 200 Connection Established");
    assert_eq!(verdict(&mut rx).await.3, EgressVerdict::Allowlist);
    let _ = node.kill().await;
}

#[test]
fn wrapper_and_env() {
    assert!(CONTAINER_WRAPPER.contains("exec \"$@\""));
    assert!(CONTAINER_WRAPPER.contains("/queue-egress/p.sock"));
    let env = proxy_env("http://127.0.0.1:3128");
    for k in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
    ] {
        assert!(env.iter().any(|(n, _)| n == k), "{k}");
    }
    let ca = ca_env("/q/ca.pem", Some("/q/ca-bundle.pem"));
    assert!(ca.contains(&("NODE_EXTRA_CA_CERTS".into(), "/q/ca.pem".into())));
    assert!(ca.contains(&("SSL_CERT_FILE".into(), "/q/ca-bundle.pem".into())));
    assert_eq!(ca_env("/q/ca.pem", None).len(), 1);
}
