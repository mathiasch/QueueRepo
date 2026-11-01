//! Integration tests: app + runner + Postgres, driven through the HTTP API.
//! Requires DATABASE_URL (a role that may create databases).
mod common;

use std::time::Duration;

use common::{start, status_of};
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn auth_register_login_logout(pool: PgPool) {
    let h = start(pool).await;
    let c = h.anon();
    assert_eq!(c.get("/api/auth/me").await.0, 401);
    assert_eq!(
        c.post(
            "/api/auth/register",
            json!({"email": "a@x.ch", "password": "short"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        c.post(
            "/api/auth/register",
            json!({"email": "A@x.ch", "password": "password123"})
        )
        .await
        .0,
        200
    );
    let me = c.get("/api/auth/me").await.1;
    assert_eq!(me["email"], "a@x.ch", "email is normalized");
    assert_eq!(me["is_admin"], true, "first user is admin");
    assert_eq!(c.post("/api/auth/logout", json!({})).await.0, 200);
    assert_eq!(c.get("/api/auth/me").await.0, 401);
    assert_eq!(
        c.post(
            "/api/auth/login",
            json!({"email": "a@x.ch", "password": "wrongpass1"})
        )
        .await
        .0,
        401
    );
    assert_eq!(
        c.post(
            "/api/auth/login",
            json!({"email": "a@x.ch", "password": "password123"})
        )
        .await
        .0,
        200
    );
    let c2 = h.anon();
    assert_eq!(
        c2.post(
            "/api/auth/register",
            json!({"email": "a@x.ch", "password": "password123"})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        c2.post(
            "/api/auth/register",
            json!({"email": "b@x.ch", "password": "password123"})
        )
        .await
        .0,
        200
    );
    assert_eq!(c2.get("/api/auth/me").await.1["is_admin"], false);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn tenants_are_isolated_and_roles_enforced(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let p = a.project(json!({"name": "Alpha"})).await;
    a.project(json!({"name": "Second project of A"})).await;
    assert_eq!(
        a.get("/api/projects").await.1.as_array().unwrap().len(),
        2,
        "multiple projects per user"
    );

    assert_eq!(b.get(&format!("/api/projects/{p}")).await.0, 404);
    assert!(b
        .get("/api/projects")
        .await
        .1
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        b.post("/api/tasks", json!({"project_id": p, "prompt": "x"}))
            .await
            .0,
        404
    );

    assert_eq!(
        a.post(
            &format!("/api/projects/{p}/members"),
            json!({"email": "b@x.ch", "role": "viewer"})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        b.get(&format!("/api/projects/{p}")).await.1["role"],
        "viewer"
    );
    assert_eq!(
        b.post("/api/tasks", json!({"project_id": p, "prompt": "x"}))
            .await
            .0,
        403
    );
    assert_eq!(b.get(&format!("/api/projects/{p}/files")).await.0, 200);

    a.post(
        &format!("/api/projects/{p}/members"),
        json!({"email": "b@x.ch", "role": "editor"}),
    )
    .await;
    let t = b.task(&p, "editor task").await;
    assert_eq!(b.delete(&format!("/api/projects/{p}")).await.0, 403);
    // A shares the project but not B's queue.
    assert!(a.tasks().await.iter().all(|x| x["id"] != t.as_str()));
    assert_eq!(
        a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await.0,
        404
    );
    b.wait_status(&t, &["done"]).await;
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn task_runs_verifies_reviews_and_commits_to_git(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a
        .project(json!({"name": "Demo", "goal": "demo goal"}))
        .await;
    a.patch(
        &format!("/api/projects/{p}"),
        json!({"verify_command": "test -f MOCK_LOG.md"}),
    )
    .await;
    let t = a.task(&p, "Implement hello").await;
    let d = a.wait_status(&t, &["done"]).await;

    let task = &d["task"];
    assert_eq!(task["summary"], "mock done");
    assert_eq!(task["handoff"], "mock handoff");
    assert_eq!(task["review"]["approved"], true);
    assert!(
        task["usage"]["input_tokens"].as_i64().unwrap() >= 2400,
        "agent + review usage"
    );
    let kinds: Vec<&str> = d["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    for k in ["status", "text", "tool", "verify", "review"] {
        assert!(kinds.contains(&k), "missing event {k}: {kinds:?}");
    }

    let commits = a.get(&format!("/api/projects/{p}/commits")).await.1;
    assert_eq!(commits[0]["subject"], "Implement hello");
    assert_eq!(commits[0]["author"], "a@x.ch");
    assert_eq!(
        commits.as_array().unwrap().last().unwrap()["subject"],
        "Initial commit"
    );
    assert!(
        h.workspace(&p).join(".git").is_dir(),
        "project is a real git repo"
    );

    let diff = a.get(&format!("/api/tasks/{t}/diff")).await.1;
    assert!(diff["diff"].as_str().unwrap().contains("MOCK_LOG.md"));
    assert!(!diff["diff"].as_str().unwrap().contains(".queue"));
    let files = a.get(&format!("/api/projects/{p}/files")).await.1;
    assert!(files.as_array().unwrap().iter().any(|f| f == "MOCK_LOG.md"));
    let f = a
        .get(&format!("/api/projects/{p}/file?path=MOCK_LOG.md"))
        .await
        .1;
    assert!(f["content"].as_str().unwrap().starts_with("run "));
    assert_eq!(
        a.get(&format!("/api/projects/{p}/file?path=../../etc/passwd"))
            .await
            .0,
        400
    );

    let limits = a.get("/api/limits").await.1;
    let mock = limits
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["provider"] == "mock")
        .unwrap();
    assert_eq!(
        mock["used_5h"], 2700,
        "1350 counted tokens per run (agent + review)"
    );
    assert_eq!(mock["runs_5h"], 2);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn failing_verification_creates_fix_tasks_until_blocked(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    a.task(&p, "MOCK_FAIL broken").await;
    let ts = a
        .wait_until("blocked fix chain", |ts| {
            status_of(ts, "Fix #2").contains(&"blocked".to_string())
        })
        .await;
    let root: Vec<_> = ts
        .iter()
        .filter(|t| t["title"] == "MOCK_FAIL broken")
        .map(|t| t["status"].clone())
        .collect();
    assert_eq!(root, vec![json!("failed")]);
    assert_eq!(status_of(&ts, "Fix #1"), vec!["failed"]);
    let fix1 = ts
        .iter()
        .find(|t| t["title"].as_str().unwrap().starts_with("Fix #1"))
        .unwrap();
    assert!(fix1["prompt"].as_str().unwrap().contains("root cause"));
    assert!(fix1["prompt"]
        .as_str()
        .unwrap()
        .contains("MOCK_FAIL broken"));
    assert!(a
        .notifications()
        .await
        .iter()
        .any(|n| n["kind"] == "blocked"));
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn question_is_answered_and_task_resumes(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    let t = a.task(&p, "MOCK_ASK which db").await;
    let w = a.wait_status(&t, &["waiting_input"]).await;
    assert_eq!(w["task"]["questions"][0], "Postgres or SQLite?");
    assert!(a
        .notifications()
        .await
        .iter()
        .any(|n| n["kind"] == "question"));
    assert_eq!(
        a.post(
            &format!("/api/tasks/{t}/answer"),
            json!({"answer": "Postgres"})
        )
        .await
        .0,
        200
    );
    let d = a.wait_status(&t, &["done"]).await;
    assert!(d["task"]["context"]
        .as_str()
        .unwrap()
        .contains("A: Postgres"));
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn more_than_two_generated_tasks_need_approval(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    let t = a.task(&p, "MOCK_FOLLOWUPS plan").await;
    a.wait_status(&t, &["done"]).await;
    let ts = a
        .wait_until("3 pending", |ts| {
            ts.iter()
                .filter(|t| t["status"] == "pending_approval")
                .count()
                == 3
        })
        .await;
    assert!(
        ts.iter()
            .filter(|x| x["status"] == "pending_approval")
            .all(|x| x["parent_id"] == t.as_str()),
        "handoff chain to parent"
    );
    assert!(a
        .notifications()
        .await
        .iter()
        .any(|n| n["kind"] == "approval"));
    // Nothing runs before approval.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        a.tasks()
            .await
            .iter()
            .filter(|t| t["status"] == "pending_approval")
            .count(),
        3
    );
    assert_eq!(
        a.post("/api/tasks/approve-all", json!({})).await.1["approved"],
        3
    );
    a.wait_until("approved tasks ran", |ts| {
        ts.iter()
            .filter(|t| t["created_by_agent"] == true && t["status"] == "done")
            .count()
            >= 3
    })
    .await;
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn up_to_two_generated_tasks_are_queued_automatically(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"max_auto_tasks": 3})).await;
    let p = a.project(json!({"name": "Demo"})).await;
    a.task(&p, "MOCK_FOLLOWUPS plan").await;
    let ts = a
        .wait_until("auto-queued follow ups", |ts| {
            ts.iter().filter(|t| t["created_by_agent"] == true).count() >= 3
        })
        .await;
    assert!(ts
        .iter()
        .filter(|t| t["created_by_agent"] == true)
        .all(|t| t["status"] != "pending_approval"));
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn subscription_limit_pauses_provider_and_records_observation(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    let warm = a.task(&p, "warm up").await;
    a.wait_status(&warm, &["done"]).await;
    let lim = a.task(&p, "MOCK_LIMIT hit").await;
    let other = a.task(&p, "after limit").await;

    let l = loop {
        let l = a.get("/api/limits").await.1;
        let m = l
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["provider"] == "mock")
            .unwrap()
            .clone();
        if !m["limited_until"].is_null() {
            break m;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    assert_eq!(l["blocked"], true);
    assert!(
        l["observed_limit_tokens"].as_i64().unwrap() >= 2700,
        "observed tokens at limit: {l}"
    );
    let t = a.wait_status(&lim, &["queued"]).await; // interrupted task is re-queued
    assert!(t["task"]["context"]
        .as_str()
        .unwrap()
        .contains("usage limit"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        a.task_json(&other).await["task"]["status"],
        "queued",
        "provider paused"
    );
    assert!(a.notifications().await.iter().any(|n| n["kind"] == "limit"));

    // Limit expires → queue resumes.
    a.post(&format!("/api/tasks/{lim}/cancel"), json!({})).await;
    sqlx::query("UPDATE provider_limits SET limited_until = now() - interval '1 second'")
        .execute(&h.st.db)
        .await
        .unwrap();
    h.st.wake.notify_one();
    a.wait_status(&other, &["done"]).await;
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn token_budget_blocks_provider_until_raised(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    a.patch(
        &format!("/api/projects/{p}"),
        json!({"review_enabled": false}),
    )
    .await;
    let (code, s) = a
        .put(
            "/api/budgets/mock",
            json!({"limit_5h_tokens": 1000, "limit_7d_tokens": null, "limit_30d_usd": null}),
        )
        .await;
    assert_eq!(code, 200, "{s}");
    assert_eq!(s["blocked"], false);
    assert_eq!(a.put("/api/budgets/unknown", json!({})).await.0, 400);

    let t1 = a.task(&p, "first").await;
    a.wait_status(&t1, &["done"]).await;
    let t2 = a.task(&p, "second").await;
    let st = a.get("/api/limits").await.1;
    let m = st
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["provider"] == "mock")
        .unwrap();
    assert_eq!(m["blocked"], true, "{m}");
    assert!(m["budget_reason"].as_str().unwrap().contains("5h"));
    assert!(!m["budget_until"].is_null());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(a.task_json(&t2).await["task"]["status"], "queued");
    let budget_notes = a
        .notifications()
        .await
        .into_iter()
        .filter(|n| n["kind"] == "budget")
        .count();
    assert_eq!(budget_notes, 1, "notified exactly once");

    a.put("/api/budgets/mock", json!({"limit_5h_tokens": 100000}))
        .await;
    a.wait_status(&t2, &["done"]).await;
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn running_task_can_be_cancelled(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    let t = a.task(&p, "MOCK_SLOW long").await;
    a.wait_status(&t, &["running"]).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = tokio::time::Instant::now();
    assert_eq!(
        a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await.0,
        200
    );
    a.wait_status(&t, &["cancelled"]).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cancel must not wait for the 30s sleep"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn revert_uses_git_and_is_atomic_on_conflict(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    a.patch(
        &format!("/api/projects/{p}"),
        json!({"review_enabled": false}),
    )
    .await;
    let t1 = a.task(&p, "one").await;
    a.wait_status(&t1, &["done"]).await;
    let t2 = a.task(&p, "two").await;
    a.wait_status(&t2, &["done"]).await;
    let log = h.workspace(&p).join("MOCK_LOG.md");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);

    let (code, body) = a.post(&format!("/api/tasks/{t1}/revert"), json!({})).await;
    assert_eq!(code, 409, "{body}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().lines().count(),
        2,
        "workspace untouched"
    );

    assert_eq!(
        a.post(&format!("/api/tasks/{t2}/revert"), json!({}))
            .await
            .0,
        200
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1);
    assert!(
        a.get(&format!("/api/projects/{p}/commits")).await.1[0]["subject"]
            .as_str()
            .unwrap()
            .starts_with("Revert")
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn credentials_are_encrypted_and_write_only(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    assert_eq!(
        a.put(
            "/api/credentials/claude",
            json!({"kind": "api_key", "secret": "sk-ant-SECRET-123"})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        a.put(
            "/api/credentials/codex",
            json!({"kind": "auth_json", "secret": "not json"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        a.put(
            "/api/credentials/claude",
            json!({"kind": "bogus", "secret": "x"})
        )
        .await
        .0,
        400
    );
    let list = a.get("/api/credentials").await.1;
    assert_eq!(list[0]["kind"], "api_key");
    assert!(!list.to_string().contains("SECRET"));
    let raw: Vec<u8> = sqlx::query_scalar("SELECT secret_enc FROM provider_credentials")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&raw).contains("SECRET"),
        "stored encrypted"
    );

    // A provider without credentials blocks the task with a clear message.
    let p = a.project(json!({"name": "Demo"})).await;
    let (_, t) = a
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "x", "provider": "gemini"}),
        )
        .await;
    let d = a.wait_status(t["id"].as_str().unwrap(), &["blocked"]).await;
    assert!(d["task"]["error"]
        .as_str()
        .unwrap()
        .contains("Keine Zugangsdaten"));
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn on_empty_queue_runs_once_when_idle(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo", "goal": "Ship it"})).await;
    let t = a.task(&p, "first real task").await;
    a.wait_status(&t, &["done"]).await;
    a.settings(json!({"onempty_enabled": true, "onempty_actions": ["review", "research"], "onempty_cooldown_minutes": 300})).await;
    let ts = a
        .wait_until("onempty done", |ts| {
            ts.iter()
                .any(|t| t["kind"] == "onempty" && t["status"] == "done")
        })
        .await;
    let oe = ts.iter().find(|t| t["kind"] == "onempty").unwrap();
    assert!(oe["title"].as_str().unwrap().contains("review + research"));
    // Cooldown: no second run.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        a.tasks()
            .await
            .iter()
            .filter(|t| t["kind"] == "onempty")
            .count(),
        1
    );
    // Verify the agent got the maintenance prompt (no code changes expected from it).
    let s = a.get("/api/settings").await.1;
    assert!(!s["last_onempty_at"].is_null());
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn clones_remote_and_auto_pushes(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let remote_dir = tempfile::tempdir().unwrap();
    let bare = remote_dir.path().join("origin.git");
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&bare)
        .status()
        .unwrap();
    assert!(ok.success());
    let url = format!("file://{}", bare.display());

    assert_eq!(
        a.post(
            "/api/projects",
            json!({"name": "Bad", "git_remote_url": "ftp://x"})
        )
        .await
        .0,
        400
    );
    let (code, _) = a
        .post(
            "/api/projects",
            json!({"name": "Missing", "git_remote_url": "file:///nonexistent/repo.git"}),
        )
        .await;
    assert_eq!(code, 400, "failed clone is reported, project not created");
    assert!(a
        .get("/api/projects")
        .await
        .1
        .as_array()
        .unwrap()
        .is_empty());

    let p = a.project(json!({"name": "Remote", "git_remote_url": url, "git_branch": "main", "git_token": "tok-123"})).await;
    let pj = a.get(&format!("/api/projects/{p}")).await.1;
    assert_eq!(pj["has_git_token"], true);
    assert!(
        pj.get("git_token_enc").is_none(),
        "token never leaves the server"
    );
    a.patch(
        &format!("/api/projects/{p}"),
        json!({"auto_push": true, "review_enabled": false}),
    )
    .await;
    let t = a.task(&p, "pushed change").await;
    let d = a.wait_status(&t, &["done"]).await;
    assert!(
        d["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["content"] == "git push: ok"),
        "{d}"
    );
    let log = std::process::Command::new("git")
        .args(["--git-dir"])
        .arg(&bare)
        .args(["log", "--format=%s", "main"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&log.stdout).contains("pushed change"));
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn parallelism_across_projects_but_one_task_per_project(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"parallelism": 2})).await;
    let p1 = a.project(json!({"name": "P1"})).await;
    let p2 = a.project(json!({"name": "P2"})).await;
    let t1 = a.task(&p1, "MOCK_SLOW a").await;
    let t1b = a.task(&p1, "MOCK_SLOW b").await;
    let t2 = a.task(&p2, "MOCK_SLOW c").await;
    a.wait_status(&t1, &["running"]).await;
    a.wait_status(&t2, &["running"]).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        a.task_json(&t1b).await["task"]["status"],
        "queued",
        "same project waits"
    );
    for t in [&t1, &t1b, &t2] {
        a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await;
    }
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn serial_by_default(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p1 = a.project(json!({"name": "P1"})).await;
    let p2 = a.project(json!({"name": "P2"})).await;
    let t1 = a.task(&p1, "MOCK_SLOW a").await;
    let t2 = a.task(&p2, "MOCK_SLOW b").await;
    a.wait_status(&t1, &["running"]).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(a.task_json(&t2).await["task"]["status"], "queued");
    a.post(&format!("/api/tasks/{t1}/cancel"), json!({})).await;
    a.wait_status(&t2, &["running"]).await;
    a.post(&format!("/api/tasks/{t2}/cancel"), json!({})).await;
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn queue_reorder_and_edit(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "P"})).await;
    let blocker = a.task(&p, "MOCK_SLOW blocker").await;
    a.wait_status(&blocker, &["running"]).await;
    let x = a.task(&p, "x").await;
    let y = a.task(&p, "y").await;
    let pos = |ts: &[serde_json::Value], id: &str| {
        ts.iter().find(|t| t["id"] == id).unwrap()["position"]
            .as_i64()
            .unwrap()
    };
    let ts = a.tasks().await;
    assert!(pos(&ts, &x) < pos(&ts, &y));
    a.post(&format!("/api/tasks/{y}/move"), json!({"direction": "top"}))
        .await;
    let ts = a.tasks().await;
    assert!(pos(&ts, &y) < pos(&ts, &x));
    assert_eq!(
        a.patch(&format!("/api/tasks/{x}"), json!({"prompt": "edited"}))
            .await
            .0,
        200
    );
    assert_eq!(a.task_json(&x).await["task"]["prompt"], "edited");
    assert_eq!(
        a.patch(&format!("/api/tasks/{blocker}"), json!({"prompt": "no"}))
            .await
            .0,
        409,
        "running task not editable"
    );
    a.post(&format!("/api/tasks/{blocker}/cancel"), json!({}))
        .await;
    a.wait_status(&y, &["done"]).await;
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn sse_stream_delivers_live_updates(pool: PgPool) {
    use futures::StreamExt;
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "P"})).await;
    let resp = a
        .http
        .get(format!("{}/api/events", a.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.bytes_stream();
    a.task(&p, "live").await;
    let mut seen = String::new();
    let got = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(Ok(chunk)) = stream.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk));
            if seen.contains("\"type\":\"task_event\"") && seen.contains("\"status\":\"done\"") {
                return true;
            }
        }
        false
    })
    .await;
    assert!(matches!(got, Ok(true)), "SSE did not deliver: {seen}");
    // Other users never receive these events.
    let b = h.user("b@x.ch").await;
    let resp = b
        .http
        .get(format!("{}/api/events", b.base))
        .send()
        .await
        .unwrap();
    let mut s2 = resp.bytes_stream();
    a.task(&p, "private").await;
    let leak = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(c)) = s2.next().await {
            if String::from_utf8_lossy(&c).contains("private") {
                return true;
            }
        }
        false
    })
    .await;
    assert!(!matches!(leak, Ok(true)), "event leaked to another tenant");
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn zip_import_and_export(pool: PgPool) {
    use std::io::Write;
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Zip"})).await;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file(
            "app-main/src/lib.rs",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(b"pub fn x() {}").unwrap();
        z.finish().unwrap();
    }
    let r = a
        .http
        .post(format!("{}/api/projects/{p}/import", a.base))
        .body(buf.into_inner())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let files = a.get(&format!("/api/projects/{p}/files")).await.1;
    assert!(
        files.as_array().unwrap().iter().any(|f| f == "src/lib.rs"),
        "{files}"
    );
    assert_eq!(
        a.get(&format!("/api/projects/{p}/commits")).await.1[0]["subject"],
        "Import ZIP"
    );
    let z = a
        .http
        .get(format!("{}/api/projects/{p}/zip", a.base))
        .send()
        .await
        .unwrap();
    assert_eq!(z.headers()["content-type"], "application/zip");
    assert!(z.bytes().await.unwrap().starts_with(b"PK"));
}
