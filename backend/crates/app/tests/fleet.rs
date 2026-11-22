//! WP11 integration tests: runner fleet (registration, own tokens,
//! isolation, assignment, drain, capacity, move, failure without double
//! run) and backup → empty database → restore. App + runners in-process,
//! fresh Postgres per test.
mod common;

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use common::{Client, Harness};
use queued_common::{HeartbeatRequest, HeartbeatResponse};
use queued_runner::{RunnerConfig, RunnerState};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const BUILTIN: &str = "00000000-0000-0000-0000-000000000001";

async fn fast(pool: PgPool) -> Harness {
    common::start_with(pool, |c| {
        c.runner_heartbeat_secs = 1;
        c.runner_lease_secs = 5;
    })
    .await
}

/// Starts a runner with `token` on an already bound listener.
fn serve_runner(l: tokio::net::TcpListener, dir: &std::path::Path, token: &str) {
    let mut cfg = RunnerConfig::for_tests(dir.to_path_buf());
    cfg.token = token.to_string();
    let st = RunnerState::new(cfg);
    tokio::spawn(async move { axum::serve(l, queued_runner::router(st)).await.unwrap() });
}

async fn runner_view(admin: &Client, name: &str) -> Value {
    admin
        .get("/api/admin/runners")
        .await
        .1
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .cloned()
        .unwrap_or(Value::Null)
}

async fn wait_runner(admin: &Client, name: &str, online: bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let r = runner_view(admin, name).await;
        if r["online"] == online {
            return r;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "runner {name} never online={online}: {r}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn runner_of(h: &Harness, project: &str) -> String {
    sqlx::query_scalar::<_, Uuid>("SELECT runner_id FROM projects WHERE id=$1::uuid")
        .bind(project)
        .fetch_one(&h.st.db)
        .await
        .unwrap()
        .to_string()
}

async fn active_runs(url: &str, token: &str) -> Vec<String> {
    let r: HeartbeatResponse = reqwest::Client::new()
        .post(format!("{url}/v1/heartbeat"))
        .bearer_auth(token)
        .json(&HeartbeatRequest {
            lease_secs: 5,
            runs: vec![],
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    r.active
}

/// Registration with a token per runner (shown once, stored encrypted),
/// least privilege between runners and against the app, assignment of new
/// projects, drain, capacity shared by a runner's projects, moving an idle
/// project, revocation.
#[sqlx::test]
async fn fleet_registration_isolation_drain_capacity_and_move(pool: PgPool) {
    let h = fast(pool).await;
    let admin = h.user("admin@x.org").await;
    let p1 = admin.project(json!({"name": "on-builtin"})).await;
    assert_eq!(runner_of(&h, &p1).await, BUILTIN);

    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url2 = format!("http://{}", l.local_addr().unwrap());
    let (code, created) = admin
        .post(
            "/api/admin/runners",
            json!({"name": "zwei", "url": url2, "capacity": 1}),
        )
        .await;
    assert_eq!(code, 200, "{created}");
    let token2 = created["token"].as_str().unwrap().to_string();
    assert!(token2.starts_with("qr_") && token2.len() > 40);
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_enc FROM runners WHERE name='zwei'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert!(
        !stored.windows(token2.len()).any(|w| w == token2.as_bytes()),
        "token stored encrypted"
    );
    assert_eq!(
        admin
            .post("/api/admin/runners", json!({"name": "zwei", "url": url2}))
            .await
            .0,
        409
    );
    assert_eq!(
        admin
            .post(
                "/api/admin/runners",
                json!({"name": "drei", "url": "ftp://x"})
            )
            .await
            .0,
        400
    );
    let dir2 = tempfile::tempdir().unwrap();
    serve_runner(l, dir2.path(), &token2);
    let zwei = wait_runner(&admin, "zwei", true).await;
    assert_eq!(zwei["engine"], "none");
    let zid = zwei["id"].as_str().unwrap().to_string();

    // Least privilege: a runner's token opens neither another runner nor the app.
    let http = reqwest::Client::new();
    let st = |url: String, tok: &str| {
        let r = http.get(url).bearer_auth(tok.to_string()).send();
        async move { r.await.unwrap().status().as_u16() }
    };
    assert_eq!(st(format!("{url2}/v1/info"), &token2).await, 200);
    assert_eq!(
        st(format!("{url2}/v1/info"), "test-token").await,
        401,
        "builtin token on runner 2"
    );
    assert_eq!(
        st(format!("{}/v1/info", h.st.cfg.runner_url), &token2).await,
        401,
        "runner 2 token on builtin"
    );
    assert_eq!(
        st(format!("{}/v1/workspaces", h.st.cfg.runner_url), &token2).await,
        401
    );
    assert_eq!(
        st(format!("{}/api/tasks", h.base), &token2).await,
        401,
        "runner token at the app"
    );
    // Not a token route at all (refused before any lookup).
    assert_eq!(
        st(format!("{}/api/admin/runners", h.base), &token2).await,
        403
    );

    // Drain the built-in runner: new projects go to `zwei`, its queued work waits.
    let (code, v) = admin
        .patch(
            &format!("/api/admin/runners/{BUILTIN}"),
            json!({"status": "draining"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    let t1 = admin.task(&p1, "waits for the drained runner").await;
    let p2 = admin.project(json!({"name": "on-zwei"})).await;
    assert_eq!(runner_of(&h, &p2).await, zid);
    assert!(dir2
        .path()
        .join("workspaces")
        .join(&p2)
        .join(".git")
        .exists());
    assert!(
        !h.workspace(&p2).exists(),
        "project 2 never touches the built-in runner"
    );
    let t2 = admin.task(&p2, "runs on zwei").await;
    admin.wait_status(&t2, &["done"]).await;
    let ran_on: Option<Uuid> = sqlx::query_scalar("SELECT runner_id FROM tasks WHERE id=$1::uuid")
        .bind(&t2)
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(ran_on.map(|u| u.to_string()).as_deref(), Some(zid.as_str()));
    let log = std::fs::read_to_string(dir2.path().join("workspaces").join(&p2).join("MOCK_LOG.md"))
        .unwrap();
    assert!(!log.is_empty());
    assert_eq!(admin.task_json(&t1).await["task"]["status"], "queued");
    assert!(!h.workspace(&p1).join("MOCK_LOG.md").exists());

    // Capacity 1 is shared by all projects of `zwei`.
    admin.settings(json!({"parallelism": 3})).await;
    let p3 = admin.project(json!({"name": "also-zwei"})).await;
    assert_eq!(runner_of(&h, &p3).await, zid);
    let a = admin.task(&p2, "MOCK_QUIET a").await;
    let b = admin.task(&p3, "MOCK_QUIET b").await;
    let mut both_seen_done = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while tokio::time::Instant::now() < deadline {
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tasks WHERE runner_id=$1::uuid AND status='running'",
        )
        .bind(&zid)
        .fetch_one(&h.st.db)
        .await
        .unwrap();
        assert!(running <= 1, "capacity 1 exceeded");
        let sa = admin.task_json(&a).await["task"]["status"].clone();
        let sb = admin.task_json(&b).await["task"]["status"].clone();
        if sa == "done" && sb == "done" {
            both_seen_done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(both_seen_done);

    // Back to active: the waiting task on the built-in runner runs.
    admin
        .patch(
            &format!("/api/admin/runners/{BUILTIN}"),
            json!({"status": "active"}),
        )
        .await;
    admin.wait_status(&t1, &["done"]).await;

    // Move the idle project 2 to the built-in runner: history and refs move,
    // the old copy goes, new work runs there.
    let refs_before = git(
        &dir2.path().join("workspaces").join(&p2),
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );
    let (code, v) = admin
        .post(
            &format!("/api/admin/projects/{p2}/move"),
            json!({"runner_id": BUILTIN}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert!(v["workspaces"].as_u64().unwrap() >= 1);
    assert_eq!(runner_of(&h, &p2).await, BUILTIN);
    assert_eq!(
        git(
            &h.workspace(&p2),
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before
    );
    assert!(!dir2.path().join("workspaces").join(&p2).exists());
    let t4 = admin.task(&p2, "after the move").await;
    admin.wait_status(&t4, &["done"]).await;
    assert!(
        std::fs::read_to_string(h.workspace(&p2).join("MOCK_LOG.md"))
            .unwrap()
            .lines()
            .count()
            > log.lines().count()
    );
    assert_eq!(
        admin
            .post(
                &format!("/api/admin/projects/{p2}/move"),
                json!({"runner_id": BUILTIN})
            )
            .await
            .0,
        409,
        "already there"
    );

    // Revoke `zwei`: token gone, no new projects, delete refused while a project lives there.
    let (code, v) = admin
        .post(&format!("/api/admin/runners/{zid}/revoke"), json!({}))
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["status"], "revoked");
    let enc: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT token_enc FROM runners WHERE id=$1::uuid")
            .bind(&zid)
            .fetch_one(&h.st.db)
            .await
            .unwrap();
    assert!(enc.is_none());
    let t5 = admin.task(&p3, "on a revoked runner").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(admin.task_json(&t5).await["task"]["status"], "queued");
    assert_eq!(
        admin.delete(&format!("/api/admin/runners/{zid}")).await.0,
        409
    );
    assert_eq!(
        admin
            .post(&format!("/api/admin/runners/{BUILTIN}/revoke"), json!({}))
            .await
            .0,
        409
    );
    let p4 = admin.project(json!({"name": "new"})).await;
    assert_eq!(runner_of(&h, &p4).await, BUILTIN);
    // Non-admins see nothing of the fleet.
    let u = h.user("u@x.org").await;
    assert_eq!(u.get("/api/admin/runners").await.0, 403);
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY seq")
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    for a in [
        "admin.runner_created",
        "admin.runner_updated",
        "project.moved",
        "admin.runner_revoked",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} not audited");
    }
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// TCP forwarder that can freeze: connections stay open but no byte moves
/// (a hung host or a cut network, no RST).
async fn freezable_proxy(upstream: String) -> (String, Arc<AtomicBool>) {
    let frozen = Arc::new(AtomicBool::new(false));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let f = frozen.clone();
    let up = upstream.trim_start_matches("http://").to_string();
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = l.accept().await else {
                return;
            };
            let (f, up) = (f.clone(), up.clone());
            tokio::spawn(async move {
                let Ok(server) = tokio::net::TcpStream::connect(&up).await else {
                    return;
                };
                let (cr, cw) = client.into_split();
                let (sr, sw) = server.into_split();
                let pump = |mut r: tokio::net::tcp::OwnedReadHalf,
                            mut w: tokio::net::tcp::OwnedWriteHalf,
                            f: Arc<AtomicBool>| async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 16 * 1024];
                    loop {
                        while f.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        let n = match r.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        while f.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        if w.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                };
                tokio::join!(pump(cr, sw, f.clone()), pump(sr, cw, f));
            });
        }
    });
    (url, frozen)
}

/// A runner that stops answering (hung host): after the lease it is
/// offline, the stream of its run ends, the task is re-queued as an
/// infrastructure failure and waits (affinity); the runner itself stops
/// the orphaned run when the lease expires; after the runner returns the
/// task runs again – never two agents at the same time.
#[sqlx::test]
async fn runner_failure_requeues_without_double_run(pool: PgPool) {
    let h = fast(pool).await;
    let admin = h.user("admin@x.org").await;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct = format!("http://{}", l.local_addr().unwrap());
    let (proxy, frozen) = freezable_proxy(direct.clone()).await;
    let (_, created) = admin
        .post(
            "/api/admin/runners",
            json!({"name": "wackel", "url": proxy}),
        )
        .await;
    let token = created["token"].as_str().unwrap().to_string();
    let dir = tempfile::tempdir().unwrap();
    serve_runner(l, dir.path(), &token);
    wait_runner(&admin, "wackel", true).await;
    admin
        .patch(
            &format!("/api/admin/runners/{BUILTIN}"),
            json!({"status": "draining"}),
        )
        .await;
    let p = admin.project(json!({"name": "p"})).await;
    let t = admin.task(&p, "MOCK_SLOW long agent").await;
    admin.wait_status(&t, &["running"]).await;
    let name = queued::runner::run_name(t.parse().unwrap(), "agent");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !active_runs(&direct, &token).await.contains(&name) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "agent never started"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    frozen.store(true, Ordering::SeqCst);
    let lost_at = tokio::time::Instant::now();
    let v = admin.wait_status(&t, &["queued"]).await;
    assert!(
        lost_at.elapsed() < Duration::from_secs(15),
        "re-queued after the lease"
    );
    assert_eq!(v["task"]["failure_class"], "infra");
    assert!(
        v["task"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("runner unreachable"),
        "{v}"
    );
    assert_eq!(runner_view(&admin, "wackel").await["online"], false);
    // The runner stops the orphan on its own once the lease is over.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while active_runs(&direct, &token).await.contains(&name) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "orphaned run still alive"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // While the runner is gone the task waits: no run elsewhere.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(admin.task_json(&t).await["task"]["status"], "queued");
    assert!(!h.workspace(&p).exists());

    // Back: the task runs again on its runner, exactly one agent.
    frozen.store(false, Ordering::SeqCst);
    wait_runner(&admin, "wackel", true).await;
    admin.wait_status(&t, &["running"]).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let act = active_runs(&direct, &token).await;
        if act.contains(&name) {
            assert_eq!(
                act.iter()
                    .filter(|r| r.contains(&t.replace('-', "")[..12]))
                    .count(),
                1,
                "{act:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "retry never started"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        admin
            .post(&format!("/api/tasks/{t}/cancel"), json!({}))
            .await
            .0,
        200
    );
    admin.wait_status(&t, &["cancelled"]).await;
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY seq")
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    assert!(actions.iter().any(|a| a == "admin.runner_offline"));
    assert!(actions.iter().any(|a| a == "admin.runner_online"));
}

/// Backup (encrypted) → new empty database + empty runner → restore: every
/// table row and every repository ref identical, the audit chain intact,
/// the restored app works; wrong key, tampering and a non-empty target are
/// refused before anything changes.
#[sqlx::test]
async fn backup_restore_roundtrip(pool: PgPool) {
    let base_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let url = format!("{}/{db}", base_url.rsplit_once('/').unwrap().0);
    let u2 = url.clone();
    let h = common::start_with(pool, move |c| c.database_url = u2).await;
    let a = h.user("admin@x.org").await;
    let p = a.project(json!({"name": "gesichert"})).await;
    let t = a.task(&p, "first change").await;
    a.wait_status(&t, &["done"]).await;
    let t = a.task(&p, "second change").await;
    a.wait_status(&t, &["done"]).await;
    let _token = a.token(&["tasks:read"]).await;
    let (_, setup) = a
        .post(
            "/api/account/mfa/totp/setup",
            json!({"password": "password123"}),
        )
        .await;
    let secret = setup["secret"].as_str().unwrap().to_string();
    let s = queued::mfa::base32_decode(&secret).unwrap();
    let code = format!(
        "{:06}",
        queued::mfa::hotp(
            &s,
            queued::mfa::step_at(chrono::Utc::now().timestamp()) as u64,
            6
        )
    );
    assert_eq!(
        a.post("/api/account/mfa/totp/confirm", json!({"code": code}))
            .await
            .0,
        200
    );
    assert_eq!(
        a.put(
            "/api/credentials/claude",
            json!({"kind": "api_key", "secret": "sk-MARKER-SECRET-123"})
        )
        .await
        .0,
        200
    );
    std::fs::write(h.workspace(&p).join("UNTRACKED-NOTE.txt"), "not committed").unwrap();

    let max_seq: i64 = sqlx::query_scalar("SELECT max(seq) FROM audit_log")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    let out = tempfile::tempdir().unwrap();
    let key = [42u8; 32];
    let dir = queued::backup::run(
        &h.st,
        &queued::backup::BackupOptions {
            out: out.path().join("b"),
            key: Some(key),
            keep: 3,
        },
    )
    .await
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let mut files = 0;
    for e in walk(&dir) {
        let meta = std::fs::metadata(&e).unwrap();
        if meta.is_file() {
            files += 1;
            assert_eq!(
                meta.permissions().mode() & 0o077,
                0,
                "{} readable by others",
                e.display()
            );
            let bytes = std::fs::read(&e).unwrap();
            for m in [
                "sk-MARKER-SECRET-123",
                "admin@x.org",
                "MOCK_LOG",
                "gesichert",
            ] {
                if e.ends_with("manifest.json") {
                    continue;
                }
                assert!(
                    !bytes.windows(m.len()).any(|w| w == m.as_bytes()),
                    "{m} in clear text in {}",
                    e.display()
                );
            }
        } else {
            assert_eq!(
                meta.permissions().mode() & 0o077,
                0,
                "{} readable by others",
                e.display()
            );
        }
    }
    assert!(files >= 4, "dump, workspace bundle, manifest, mac");
    let manifest = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
    assert!(!manifest.contains("sk-MARKER"));
    assert!(
        !dir.join("app").join("master.key.qbk").exists(),
        "MASTER_KEY is never backed up"
    );

    // Target: new empty database, new empty runner.
    let admin_url = format!("{}/postgres", base_url.rsplit_once('/').unwrap().0);
    let admin_pool = sqlx::PgPool::connect(&admin_url).await.unwrap();
    let new_db = format!("wp11_restore_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {new_db}"))
        .execute(&admin_pool)
        .await
        .unwrap();
    let new_url = format!("{}/{new_db}", base_url.rsplit_once('/').unwrap().0);
    let rl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rurl = format!("http://{}", rl.local_addr().unwrap());
    let rdir = tempfile::tempdir().unwrap();
    serve_runner(rl, rdir.path(), "test-token");
    let app2 = tempfile::tempdir().unwrap();
    let cfg2 =
        queued::config::Config::for_tests(&new_url, app2.path().to_path_buf(), &rurl, "test-token");
    let opts = |key: [u8; 32]| queued::backup::RestoreOptions {
        from: dir.clone(),
        key: Some(key),
        target_runner: None,
    };
    let client = || queued::runner_client::RunnerClient::new(&rurl, "test-token");

    // Wrong key: refused, nothing changed.
    let e = queued::backup::restore(&cfg2, client(), &opts([1u8; 32]))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("does not belong"), "{e:#}");
    // Tampered bundle: refused before the database is touched.
    let bundle = walk(&dir)
        .into_iter()
        .find(|p| p.to_string_lossy().ends_with(".bundle.qbk"))
        .unwrap();
    let orig = std::fs::read(&bundle).unwrap();
    let mut bad = orig.clone();
    let n = bad.len() / 2;
    bad[n] ^= 0x40;
    std::fs::write(&bundle, &bad).unwrap();
    let e = queued::backup::restore(&cfg2, client(), &opts(key))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("damaged"), "{e:#}");
    std::fs::write(&bundle, &orig).unwrap();
    let new_pool = sqlx::PgPool::connect(&new_url).await.unwrap();
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'",
    )
    .fetch_one(&new_pool)
    .await
    .unwrap();
    assert_eq!(tables, 0, "nothing restored after refusals");

    let report = queued::backup::restore(&cfg2, client(), &opts(key))
        .await
        .unwrap();
    assert!(report.workspaces >= 1);

    // Every table identical (the restore adds one audit entry at the end).
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' ORDER BY 1",
    )
    .fetch_all(&h.st.db)
    .await
    .unwrap();
    assert!(names.len() > 40, "{names:?}");
    // (The backup itself is audited after the dump: not in the backup.)
    for t in &names {
        // The live source keeps changing the heartbeat columns of its runners.
        let (cols, filter) = match t.as_str() {
            "audit_log" => ("*", format!(" WHERE seq <= {max_seq}")),
            "runners" => (
                "id, name, url, token_enc, builtin, status, capacity, created_at",
                String::new(),
            ),
            _ => ("*", String::new()),
        };
        let q = format!("SELECT md5(coalesce(string_agg(x::text, '|' ORDER BY x::text), '')) FROM (SELECT {cols} FROM {t}{filter}) x");
        let before: String = sqlx::query_scalar(&q).fetch_one(&h.st.db).await.unwrap();
        let after: String = sqlx::query_scalar(&q).fetch_one(&new_pool).await.unwrap();
        assert_eq!(before, after, "table {t} differs after restore");
    }
    let v: (i64, Option<i64>) =
        sqlx::query_as("SELECT checked, first_broken FROM audit_log_verify()")
            .fetch_one(&new_pool)
            .await
            .unwrap();
    assert_eq!(v.1, None, "audit chain intact after restore");
    assert_eq!(v.0, max_seq + 1);

    // Repositories identical (refs, HEAD, tree); uncommitted files are not
    // part of a backup (documented).
    let (src, dst) = (h.workspace(&p), rdir.path().join("workspaces").join(&p));
    for args in [
        &["for-each-ref", "--format=%(refname) %(objectname)"][..],
        &["symbolic-ref", "HEAD"][..],
        &["rev-parse", "HEAD^{tree}"][..],
        &["log", "--format=%H %s"][..],
    ] {
        assert_eq!(git(&src, args), git(&dst, args), "git {args:?}");
    }
    assert_eq!(
        std::fs::read_to_string(src.join("MOCK_LOG.md")).unwrap(),
        std::fs::read_to_string(dst.join("MOCK_LOG.md")).unwrap()
    );
    assert!(!dst.join("UNTRACKED-NOTE.txt").exists());

    // The restored instance works: migrations are current, files readable,
    // the TOTP seed decrypts with the same MASTER_KEY.
    let st2 = queued::init(queued::config::Config::for_tests(
        &new_url,
        app2.path().to_path_buf(),
        &rurl,
        "test-token",
    ))
    .await
    .unwrap();
    let pid: Uuid = p.parse().unwrap();
    let files = st2.runner(pid).await.files(pid).await.unwrap();
    assert!(files.iter().any(|f| f == "MOCK_LOG.md"), "{files:?}");
    let uid: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email='admin@x.org'")
        .fetch_one(&new_pool)
        .await
        .unwrap();
    let step = queued::mfa::step_at(chrono::Utc::now().timestamp()) + 1;
    let next = format!("{:06}", queued::mfa::hotp(&s, step as u64, 6));
    assert!(
        queued::mfa::verify_totp(&new_pool, &st2.cfg.master_key, uid, &next, false)
            .await
            .unwrap()
    );

    // A second restore into the now filled database is refused.
    let e = queued::backup::restore(&cfg2, client(), &opts(key))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("not empty"), "{e:#}");
    st2.db.close().await;
    new_pool.close().await;
    sqlx::query(&format!("DROP DATABASE {new_db} WITH (FORCE)"))
        .execute(&admin_pool)
        .await
        .unwrap();
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        }
        out.push(p);
    }
    out
}
