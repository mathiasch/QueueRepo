//! WP01 tester: edge cases the implementation tests did not cover –
//! diff/revert continuity of interrupted tasks, ordering within a project
//! during backoff, authorization of the resume endpoint, budget accounting
//! on cancel/limits, migrations on existing data.
mod common;

use std::time::Duration;

use common::{start, start_with, Client, Harness};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn no_review_project(a: &Client) -> String {
    let p = a.project(json!({"name": "Demo"})).await;
    let (code, _) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"review_enabled": false}),
        )
        .await;
    assert_eq!(code, 200);
    p
}

async fn user_id(h: &Harness, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email=$1")
        .bind(email)
        .fetch_one(&h.st.db)
        .await
        .unwrap()
}

/// Full commit messages in `from..to` of the project's workspace.
fn git_log(h: &Harness, project: &str, from: &str, to: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(h.workspace(project))
        .args(["log", "--format=%B%x1e", &format!("{from}..{to}")])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

async fn wait_class(a: &Client, t: &str, class: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let j = a.task_json(t).await;
        if j["task"]["failure_class"] == class && j["task"]["status"] == "queued" {
            return j;
        }
        assert!(tokio::time::Instant::now() < deadline, "{j}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Deviation 11 of WP01-impl: the diff of a task that was interrupted and
/// continued covers the commits of all its runs.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn interrupted_task_diff_covers_all_its_runs(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a.task(&p, "MOCK_TRANSIENT alone").await;
    let d = a.wait_status(&t, &["done"]).await;
    let task = &d["task"];
    let log = git_log(
        &h,
        &p,
        task["snapshot_before"].as_str().unwrap(),
        task["snapshot_after"].as_str().unwrap(),
    );
    assert!(
        log.contains("(unterbrochen: provider_transient)"),
        "partial commit of the interrupted run is part of the task diff: {log}"
    );
    assert_eq!(
        log.matches(&format!("Queue-Task: {t}")).count(),
        2,
        "both runs' commits: {log}"
    );
}

/// Regression: while task A waits in backoff, task B of the same project
/// runs. A's continued diff/revert must not swallow B's commits.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn interrupted_task_diff_never_includes_other_tasks(pool: PgPool) {
    let h = start_with(pool, |c| {
        c.backoff_base_secs = 3;
        c.backoff_max_secs = 3;
    })
    .await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let ta = a.task(&p, "MOCK_TRANSIENT task A").await;
    let tb = a.task(&p, "plain task B").await;
    let b = a.wait_status(&tb, &["done"]).await;
    let da = a.wait_status(&ta, &["done"]).await;
    // B really ran in between (otherwise the test proves nothing).
    let b_finished = b["task"]["finished_at"].as_str().unwrap().to_string();
    let a_finished = da["task"]["finished_at"].as_str().unwrap().to_string();
    assert!(b_finished < a_finished, "B overtook A during the backoff");

    let task = &da["task"];
    let log = git_log(
        &h,
        &p,
        task["snapshot_before"].as_str().unwrap(),
        task["snapshot_after"].as_str().unwrap(),
    );
    assert!(
        !log.contains(&format!("Queue-Task: {tb}")),
        "A's diff contains B's commit: {log}"
    );
    // Reverting A must not touch B's changes.
    let (code, r) = a.post(&format!("/api/tasks/{ta}/revert"), json!({})).await;
    assert!(code == 200 || code == 409, "{code} {r}");
    let log_all = git_log(
        &h,
        &p,
        b["task"]["snapshot_before"].as_str().unwrap(),
        "HEAD",
    );
    if code == 200 {
        assert!(
            !log_all.contains(&format!(
                "This reverts commit {}",
                b["task"]["snapshot_after"].as_str().unwrap()
            )),
            "{log_all}"
        );
    }
}

/// Resume endpoint: only the caller's own provider state is touched.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn resume_only_lifts_own_pause(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let ua = user_id(&h, "a@x.ch").await;
    queued::budget::set_paused(&h.st.db, ua, "mock", "Invalid API key")
        .await
        .unwrap();
    // Someone else's resume does nothing to A.
    let (code, s) = b.post("/api/limits/mock/resume", json!({})).await;
    assert_eq!(code, 200, "{s}");
    assert!(s["paused_reason"].is_null());
    let l = a.limit("mock").await;
    assert_eq!(l["paused_reason"], "Invalid API key", "{l}");
    assert_eq!(l["blocked"], true);
    // Anonymous → 401, unknown provider → 400.
    let (code, _) = h.anon().post("/api/limits/mock/resume", json!({})).await;
    assert_eq!(code, 401);
    let (code, _) = a.post("/api/limits/nope/resume", json!({})).await;
    assert_eq!(code, 400);
    // A's own resume lifts it.
    let (code, s) = a.post("/api/limits/mock/resume", json!({})).await;
    assert_eq!(code, 200);
    assert!(s["paused_reason"].is_null() && s["blocked"] == false, "{s}");
}

/// A user's auth pause never stops another user's tasks on the same provider.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn auth_pause_is_per_user(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let pa = no_review_project(&a).await;
    let pb = no_review_project(&b).await;
    let ta = a.task(&pa, "MOCK_AUTH broken login").await;
    wait_class(&a, &ta, "provider_auth").await;
    let tb = b.task(&pb, "B unaffected").await;
    b.wait_status(&tb, &["done"]).await;
    assert_eq!(a.task_json(&ta).await["task"]["status"], "queued");
    assert!(b.limit("mock").await["paused_reason"].is_null());
}

/// Budget: the usage of a run stopped by the token guard is logged once,
/// and a retry starts the task budget from zero.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn guard_stop_logs_usage_once_and_retry_resets_task_budget(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a
        .task_with(&p, "MOCK_TOKENS burn", json!({"max_tokens": 2000}))
        .await;
    let b = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "limit_tokens");
    let id = Uuid::parse_str(&t).unwrap();
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT input_tokens FROM usage_log WHERE task_id=$1")
        .bind(id)
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "one usage_log row per run: {rows:?}");
    assert_eq!(
        rows[0].0,
        b["task"]["usage"]["input_tokens"].as_i64().unwrap(),
        "task usage == logged usage"
    );
    // Retry → runs again from a fresh task budget and is stopped again
    // (instead of being blocked before the start by the old usage).
    let (code, _) = a.post(&format!("/api/tasks/{t}/retry"), json!({})).await;
    assert_eq!(code, 200);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let n = loop {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_log WHERE task_id=$1")
            .bind(id)
            .fetch_one(&h.st.db)
            .await
            .unwrap();
        if n >= 2 || tokio::time::Instant::now() > deadline {
            break n;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(n, 2, "the retried task ran again");
    let b2 = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(b2["task"]["failure_class"], "limit_tokens");
    assert_eq!(
        b2["task"]["usage"]["input_tokens"], 2400,
        "retry started from a fresh task budget"
    );
}

/// Cumulative limits: a task that already used its budget in an earlier
/// (interrupted) run is blocked before the next run starts, without a run.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn exhausted_budget_blocks_before_the_next_run(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let uid = user_id(&h, "a@x.ch").await;
    let p = no_review_project(&a).await;
    // Provider paused so the task can be prepared before it is claimed.
    queued::budget::set_paused(&h.st.db, uid, "mock", "hold")
        .await
        .unwrap();
    let t = a
        .task_with(&p, "would run", json!({"max_tokens": 1000}))
        .await;
    sqlx::query("UPDATE tasks SET usage=$2 WHERE id=$1")
        .bind(Uuid::parse_str(&t).unwrap())
        .bind(json!({"input_tokens": 900, "output_tokens": 100, "cache_read_tokens": 0, "cache_write_tokens": 0, "cost_usd": 0.0}))
        .execute(&h.st.db)
        .await
        .unwrap();
    a.post("/api/limits/mock/resume", json!({})).await;
    let b = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "limit_tokens");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_log WHERE task_id=$1")
        .bind(Uuid::parse_str(&t).unwrap())
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "no run was started");
    let ev = a.events_of(&t).await;
    assert!(
        !ev.iter().any(|(k, _)| k == "text"),
        "agent never started: {ev:?}"
    );
}

/// Cancelling a task that waits in backoff: it never runs again.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cancel_during_backoff_is_final(pool: PgPool) {
    let h = start_with(pool, |c| {
        c.backoff_base_secs = 2;
        c.backoff_max_secs = 2;
    })
    .await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a.task(&p, "MOCK_TRANSIENT_ALWAYS x").await;
    wait_class(&a, &t, "provider_transient").await;
    let (code, _) = a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await;
    assert_eq!(code, 200);
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let j = a.task_json(&t).await;
    assert_eq!(j["task"]["status"], "cancelled", "{j}");
    assert_eq!(j["task"]["transient_retries"], 1);
}

/// Migration 0003 on existing data: rows created before it get the
/// defaults (no limits, no failure class, 0 retries) and keep working.
#[sqlx::test(migrations = false)]
async fn migration_0003_on_existing_rows(pool: PgPool) {
    // Apply only 0001 + 0002, insert old-style rows, then the rest.
    let m = &queued::MIGRATOR;
    let mut conn = pool.acquire().await.unwrap();
    for mig in m.iter().filter(|x| x.version < 3) {
        sqlx::raw_sql(&mig.sql).execute(&mut *conn).await.unwrap();
    }
    let uid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    let tid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, email, name, password_hash) VALUES ($1,'old@x.ch','old','x')",
    )
    .bind(uid)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO projects (id, owner_id, name) VALUES ($1,$2,'old')")
        .bind(pid)
        .bind(uid)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tasks (id, user_id, project_id, kind, title, prompt, provider, status, error)
         VALUES ($1,$2,$3,'task','t','p','mock','blocked','old failure')",
    )
    .bind(tid)
    .bind(uid)
    .bind(pid)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_limits (user_id, provider, limited_until) VALUES ($1,'mock',now())",
    )
    .bind(uid)
    .execute(&mut *conn)
    .await
    .unwrap();
    for mig in m.iter().filter(|x| x.version >= 3) {
        sqlx::raw_sql(&mig.sql).execute(&mut *conn).await.unwrap();
    }
    let t: queued::models::Task = sqlx::query_as("SELECT * FROM tasks WHERE id=$1")
        .bind(tid)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(t.transient_retries, 0);
    assert!(t.failure_class.is_none() && t.not_before.is_none());
    assert!(t.max_tokens.is_none() && t.max_runtime_secs.is_none());
    let p: queued::models::Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(pid)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert!(p.stall_timeout_secs.is_none() && p.max_cost_usd.is_none());
    let paused: Option<String> =
        sqlx::query_scalar("SELECT paused_reason FROM provider_limits WHERE user_id=$1")
            .bind(uid)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(paused.is_none());
}

/// Regression: the exec stream breaks while the agent is silent (long tool
/// call). The runner only notices the vanished client on the next output
/// line, so the orphaned run kept going and every retry got 409 until the
/// task was blocked. The app now cancels the run after a stream error.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn silent_run_after_stream_drop_is_stopped_and_retried(pool: PgPool) {
    let h = common::start_flaky(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a.task(&p, "MOCK_QUIET long tool call").await;
    let d = a.wait_status(&t, &["done", "blocked"]).await;
    assert_eq!(d["task"]["status"], "done", "{d}");
    assert_eq!(d["task"]["transient_retries"], 1, "exactly one re-queue");
    assert_eq!(
        h.exec_count
            .as_ref()
            .unwrap()
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "no retry was refused with 409"
    );
}

/// Regression: after an app restart, `recover` stops runs the runner still
/// executes for the re-queued tasks.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn recover_cancels_orphaned_runner_runs(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let uid = user_id(&h, "a@x.ch").await;
    let p = no_review_project(&a).await;
    let first = a.task(&p, "creates the workspace").await;
    a.wait_status(&first, &["done"]).await;
    queued::budget::set_paused(&h.st.db, uid, "mock", "hold")
        .await
        .unwrap();
    let t = a.task(&p, "was running before the restart").await;
    let id = Uuid::parse_str(&t).unwrap();
    sqlx::query("UPDATE tasks SET status='running' WHERE id=$1")
        .bind(id)
        .execute(&h.st.db)
        .await
        .unwrap();
    let req = queued_common::ExecRequest {
        name: queued::runner::run_name(id, "agent"),
        workspace: p.clone(),
        program: "sh".into(),
        args: vec!["-c".into(), "echo started; sleep 30".into()],
        env: vec![],
        home_files: vec![],
        read_back: vec![],
        stdin: None,
        timeout_secs: 60,
        idle_timeout_secs: None,
        queue_files: vec![],
        egress: Default::default(),
        cache: None,
        secret_env: vec![],
        home_env: vec![],
    };
    let runner = h.st.runner.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let orphan = tokio::spawn(async move { runner.exec(&req, tx).await });
    assert!(rx.recv().await.is_some(), "orphan is running");
    let t0 = std::time::Instant::now();
    queued::worker::recover(&h.st).await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), orphan)
        .await
        .expect("orphaned run was stopped")
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            exit,
            queued_common::ExecFrame::Exit {
                cancelled: true,
                ..
            }
        ),
        "{exit:?}"
    );
    assert!(t0.elapsed() < Duration::from_secs(8));
    assert_eq!(a.task_json(&t).await["task"]["status"], "queued");
}

/// Regression: a user cancel during the verify command ended as a task
/// failure with a fix task instead of `cancelled`.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cancel_during_verify_is_cancelled_without_fix(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    let (code, _) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"review_enabled": false, "verify_command": "echo verifying; sleep 20"}),
        )
        .await;
    assert_eq!(code, 200);
    let t = a.task(&p, "slow verify").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !a
        .events_of(&t)
        .await
        .iter()
        .any(|(k, c)| k == "verify" && c.starts_with('$'))
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (code, _) = a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await;
    assert_eq!(code, 200);
    let c = a
        .wait_status(&t, &["cancelled", "failed", "blocked", "done"])
        .await;
    assert_eq!(c["task"]["status"], "cancelled", "{}", c["task"]);
    assert_eq!(c["task"]["failure_class"], "cancelled");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !a.tasks().await.iter().any(|x| x["kind"] == "fix"),
        "no fix task for a cancel"
    );
}
