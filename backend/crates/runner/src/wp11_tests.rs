//! WP11: bundles (backup, moving projects), run leases and heartbeats,
//! one process per workspace.
use std::time::{Duration, Instant};

use futures::StreamExt;
use queued_common::*;

use crate::{router, RunnerConfig, RunnerState};

async fn serve() -> (tempfile::TempDir, String) {
    let t = tempfile::tempdir().unwrap();
    let st = RunnerState::new(RunnerConfig::for_tests(t.path().to_path_buf()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, router(st)).await.unwrap() });
    (t, addr)
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@x")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@x")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn exec_req(name: &str, ws: &str, script: &str, lease: Option<u64>) -> ExecRequest {
    ExecRequest {
        name: name.into(),
        workspace: ws.into(),
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        env: vec![],
        home_files: vec![],
        read_back: vec![],
        stdin: None,
        timeout_secs: 120,
        idle_timeout_secs: None,
        queue_files: vec![],
        egress: Default::default(),
        cache: None,
        secret_env: vec![],
        home_env: vec![],
        read_only: false,
        session: None,
        lease_secs: lease,
    }
}

async fn init(c: &reqwest::Client, a: &str, ws: &str) {
    c.post(format!("{a}/v1/workspaces/{ws}"))
        .bearer_auth("test-token")
        .json(&InitWorkspace::default())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

/// Every ref (branches, held changes, baselines), the symbolic HEAD and
/// the checked-out tree survive bundle → restore; a restore never
/// overwrites an existing workspace; broken input leaves nothing behind.
#[tokio::test]
async fn bundle_and_restore_keep_every_ref_and_head() {
    let (t, a) = serve().await;
    let c = reqwest::Client::new();
    let ws = "11111111-2222-4333-8444-555555555555";
    init(&c, &a, ws).await;
    let dir = t.path().join("workspaces").join(ws);
    std::fs::write(dir.join("a.txt"), "eins\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "eins"]);
    git(&dir, &["checkout", "-q", "-b", "queue/feature"]);
    std::fs::write(dir.join("b.txt"), "zwei\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    git(&dir, &["commit", "-q", "-m", "zwei"]);
    let held = git(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["update-ref", "refs/queue/held/abc", held.trim()]);
    git(&dir, &["checkout", "-q", "main"]);

    let r = c
        .get(format!("{a}/v1/workspaces/{ws}/bundle"))
        .bearer_auth("test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let head = r.headers()[BUNDLE_HEAD_HEADER]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(head, "refs/heads/main");
    let bytes = r.bytes().await.unwrap();
    assert!(bytes.starts_with(b"# v2 git bundle") || bytes.starts_with(b"# v3 git bundle"));
    // The temporary bundle file is gone (unlinked while streaming).
    let tmp = t.path().join("tmp");
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);

    let copy = "11111111-2222-4333-8444-666666666666";
    let restore = |id: &'static str, head: &str, body: Vec<u8>| {
        c.post(format!("{a}/v1/workspaces/{id}/restore"))
            .bearer_auth("test-token")
            .header(BUNDLE_HEAD_HEADER, head)
            .body(body)
            .send()
    };
    let r = restore(copy, &head, bytes.to_vec()).await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let cdir = t.path().join("workspaces").join(copy);
    assert_eq!(
        git(&dir, &["for-each-ref", "--format=%(refname) %(objectname)"]),
        git(
            &cdir,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
    );
    assert_eq!(
        git(&cdir, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(
        std::fs::read_to_string(cdir.join("a.txt")).unwrap(),
        "eins\n"
    );
    assert!(
        !cdir.join("b.txt").exists(),
        "only the checked-out branch is in the tree"
    );
    assert_eq!(git(&cdir, &["status", "--porcelain"]).trim(), "");
    git(&cdir, &["fsck", "--no-progress"]);

    // Never over an existing workspace.
    assert_eq!(
        restore(copy, &head, bytes.to_vec()).await.unwrap().status(),
        409
    );
    // Invalid HEAD header, garbage bundle: refused, nothing left behind.
    let third = "11111111-2222-4333-8444-777777777777";
    assert_eq!(
        restore(third, "refs/heads/../../x", bytes.to_vec())
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        restore(third, "refs/heads/main", b"not a bundle".to_vec())
            .await
            .unwrap()
            .status(),
        422
    );
    assert!(!t.path().join("workspaces").join(third).exists());
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);

    // The list shows both workspaces, nothing else.
    let l: WorkspaceList = c
        .get(format!("{a}/v1/workspaces"))
        .bearer_auth("test-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(l.workspaces, vec![ws.to_string(), copy.to_string()]);
    // Without the token: nothing.
    assert_eq!(
        c.get(format!("{a}/v1/workspaces/{ws}/bundle"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

/// A run with a lease lives as long as heartbeats renew it and is stopped
/// by the runner once they stop (app gone or cut off); a second process in
/// the same workspace is refused while the first one runs.
#[tokio::test]
async fn lease_keeps_runs_alive_only_while_heartbeats_arrive() {
    let (_t, a) = serve().await;
    let c = reqwest::Client::new();
    let ws = "11111111-2222-4333-8444-555555555555";
    let other = "11111111-2222-4333-8444-666666666666";
    init(&c, &a, ws).await;
    init(&c, &a, other).await;
    let resp = c
        .post(format!("{a}/v1/exec"))
        .bearer_auth("test-token")
        .json(&exec_req(
            "q-lease-agent",
            ws,
            "sleep 100",
            Some(MIN_LEASE_SECS),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.bytes_stream();
    let started = Instant::now();

    // Same workspace: refused. Other workspace: fine.
    let second = c
        .post(format!("{a}/v1/exec"))
        .bearer_auth("test-token")
        .json(&exec_req("q-lease-other", ws, "true", None))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409);
    let ok = c
        .post(format!("{a}/v1/exec"))
        .bearer_auth("test-token")
        .json(&exec_req("q-elsewhere", other, "true", None))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let _ = ok.bytes().await;

    // Heartbeats for longer than the lease: the run stays.
    let hb = |runs: Vec<String>| {
        c.post(format!("{a}/v1/heartbeat"))
            .bearer_auth("test-token")
            .json(&HeartbeatRequest {
                lease_secs: MIN_LEASE_SECS,
                runs,
            })
            .send()
    };
    while started.elapsed() < Duration::from_secs(MIN_LEASE_SECS + 3) {
        let r: HeartbeatResponse = hb(vec!["q-lease-agent".into()])
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            r.active,
            vec!["q-lease-agent".to_string()],
            "run still alive"
        );
        assert_eq!(r.info.engine, "none");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // A heartbeat that does not list the run does not renew it.
    let silent = Instant::now();
    let mut ended = false;
    let deadline = Duration::from_secs(MIN_LEASE_SECS + 10);
    let mut buf = Vec::new();
    while silent.elapsed() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), stream.next()).await {
            Ok(Some(Ok(b))) => buf.extend_from_slice(&b),
            Ok(_) => {
                ended = true;
                break;
            }
            Err(_) => {
                let _ = hb(vec![]).await;
            }
        }
    }
    assert!(ended, "the runner stops a run whose lease expired");
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("\"t\":\"exit\""), "{text}");
    assert!(silent.elapsed() >= Duration::from_secs(MIN_LEASE_SECS - 2));
    // The workspace is free again.
    let again = c
        .post(format!("{a}/v1/exec"))
        .bearer_auth("test-token")
        .json(&exec_req("q-after", ws, "true", None))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 200);
}
