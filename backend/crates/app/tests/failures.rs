//! WP01 integration tests: failure classes, re-queue/backoff, provider pause,
//! per-task limits and clean (SIGINT) cancellation. App + real runner
//! (engine `none`) + Postgres, mock provider, driven through the HTTP API.
mod common;

use std::time::{Duration, Instant};

use common::{start, start_flaky, start_with, status_of, Client, Harness};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn user_id(h: &Harness, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE email=$1")
        .bind(email)
        .fetch_one(&h.st.db)
        .await
        .unwrap()
}

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

fn has_event(ev: &[(String, String)], kind: &str, needle: &str) -> bool {
    ev.iter().any(|(k, c)| k == kind && c.contains(needle))
}

fn fixes(ts: &[Value]) -> usize {
    ts.iter().filter(|t| t["kind"] == "fix").count()
}

async fn usage_rows(h: &Harness, task: &str) -> Vec<(i64, i64)> {
    sqlx::query_as("SELECT input_tokens, output_tokens FROM usage_log WHERE task_id=$1 ORDER BY id")
        .bind(Uuid::parse_str(task).unwrap())
        .fetch_all(&h.st.db)
        .await
        .unwrap()
}

/// AK12: transient provider error → re-queued with backoff, no fix attempt.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn transient_provider_error_requeues_without_fix_attempt(pool: PgPool) {
    let h = start_with(pool, |c| {
        c.backoff_base_secs = 3;
        c.backoff_max_secs = 3;
    })
    .await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    let t = a.task(&p, "MOCK_TRANSIENT flaky provider").await;
    let q = loop {
        let j = a.task_json(&t).await;
        if j["task"]["failure_class"] == "provider_transient" {
            break j;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let task = &q["task"];
    assert_eq!(task["status"], "queued", "{task}");
    assert_eq!(task["transient_retries"], 1);
    assert_eq!(task["attempt"], 0);
    let nb = chrono::DateTime::parse_from_rfc3339(task["not_before"].as_str().unwrap()).unwrap();
    let secs = (nb.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_milliseconds();
    assert!(
        (0..=3200).contains(&secs),
        "not_before ≈ now + 3 s: {secs} ms"
    );
    assert!(task["context"]
        .as_str()
        .unwrap()
        .contains("transient provider error"));

    let d = a.wait_status(&t, &["done"]).await;
    assert!(d["task"]["failure_class"].is_null());
    assert_eq!(d["task"]["attempt"], 0);
    let ev = a.events_of(&t).await;
    assert!(
        has_event(&ev, "retry", "overloaded – Versuch 1/3"),
        "{ev:?}"
    );
    assert!(has_event(&ev, "status", "kein Fix-Versuch verbraucht"));
    assert_eq!(fixes(&a.tasks().await), 0);
    assert!(
        !a.limit("mock").await["blocked"].as_bool().unwrap(),
        "transient errors never pause the provider"
    );
}

/// AK13: the scheduler does not claim a task before `not_before`.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn not_before_delays_claim(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let uid = user_id(&h, "a@x.ch").await;
    let p = no_review_project(&a).await;
    queued::budget::set_limited(
        &h.st.db,
        uid,
        "mock",
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    let t = a.task(&p, "delayed").await;
    sqlx::query("UPDATE tasks SET not_before = now() + interval '3 seconds' WHERE id=$1")
        .bind(Uuid::parse_str(&t).unwrap())
        .execute(&h.st.db)
        .await
        .unwrap();
    queued::budget::clear_pause(&h.st.db, uid, "mock", true)
        .await
        .unwrap();
    let lifted = Instant::now();
    h.st.wake.notify_one();
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(a.task_json(&t).await["task"]["status"], "queued");
    let d = a.wait_status(&t, &["done"]).await;
    assert!(lifted.elapsed() >= Duration::from_millis(2500));
    assert!(d["task"]["not_before"].is_null(), "cleared on claim");
}

/// AK14: transient retries are exhausted → blocked, still no fix attempt.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn transient_retries_exhaust_to_blocked(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    let t = a.task(&p, "MOCK_TRANSIENT_ALWAYS down").await;
    let b = a.wait_status(&t, &["blocked"]).await;
    let task = &b["task"];
    assert_eq!(task["failure_class"], "provider_transient");
    assert_eq!(task["transient_retries"], 3, "3 runs with max 2 retries");
    assert_eq!(task["attempt"], 0);
    assert!(task["error"]
        .as_str()
        .unwrap()
        .contains("Nach 2 Wiederholungen aufgegeben"));
    let starts = a
        .events_of(&t)
        .await
        .into_iter()
        .filter(|(k, c)| k == "status" && c.starts_with("Start #"))
        .count();
    assert_eq!(starts, 3);
    assert_eq!(fixes(&a.tasks().await), 0);
    assert!(a
        .notifications()
        .await
        .iter()
        .any(|n| n["kind"] == "blocked"));
}

/// AK15: the runner stream breaks → infra, re-queued, usage so far is kept.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn runner_stream_interruption_is_infra_and_retried(pool: PgPool) {
    let h = start_flaky(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a.task(&p, "survive a runner hiccup").await;
    let d = a.wait_status(&t, &["done"]).await;
    let task = &d["task"];
    assert_eq!(task["attempt"], 0);
    assert_eq!(task["transient_retries"], 1);
    assert!(task["failure_class"].is_null());
    let ev = a.events_of(&t).await;
    assert!(has_event(&ev, "error", "Infrastrukturfehler"), "{ev:?}");
    assert!(has_event(
        &ev,
        "status",
        "Erneut eingereiht (Infrastruktur)"
    ));
    assert!(
        h.exec_count
            .as_ref()
            .unwrap()
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2
    );
    assert_eq!(fixes(&a.tasks().await), 0);
    let rows = usage_rows(&h, &t).await;
    assert_eq!(
        rows.len(),
        2,
        "usage of the interrupted run is logged: {rows:?}"
    );
    assert_eq!(rows[0], (1200, 150));
    assert_eq!(
        task["usage"]["input_tokens"], 2400,
        "task usage is cumulative over both runs"
    );
    assert!(task["context"]
        .as_str()
        .unwrap()
        .contains("infrastructure error"));
}

/// AK16: worker::recover counts a retry, marks infra and adds its note once;
/// the rate-limit note is deduplicated as well.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn recover_counts_retry_and_dedupes_note(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let uid = user_id(&h, "a@x.ch").await;
    let p = no_review_project(&a).await;
    queued::budget::set_limited(
        &h.st.db,
        uid,
        "mock",
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    let t = a.task(&p, "was running").await;
    let id = Uuid::parse_str(&t).unwrap();
    for _ in 0..2 {
        sqlx::query("UPDATE tasks SET status='running' WHERE id=$1")
            .bind(id)
            .execute(&h.st.db)
            .await
            .unwrap();
        queued::worker::recover(&h.st).await.unwrap();
    }
    let task = &a.task_json(&t).await["task"];
    assert_eq!(task["status"], "queued");
    assert_eq!(task["failure_class"], "infra");
    assert_eq!(task["transient_retries"], 2);
    assert_eq!(
        task["context"]
            .as_str()
            .unwrap()
            .matches(queued::worker::RESTART_NOTE)
            .count(),
        1
    );
    // Max retries (2) reached → the next recover blocks the task.
    sqlx::query("UPDATE tasks SET status='running' WHERE id=$1")
        .bind(id)
        .execute(&h.st.db)
        .await
        .unwrap();
    queued::worker::recover(&h.st).await.unwrap();
    let task = &a.task_json(&t).await["task"];
    assert_eq!(task["status"], "blocked");
    assert_eq!(task["failure_class"], "infra");

    // Rate-limit note: two limits in a row, note only once.
    queued::budget::clear_pause(&h.st.db, uid, "mock", true)
        .await
        .unwrap();
    let l = a.task(&p, "MOCK_LIMIT twice").await;
    for round in 0..2 {
        loop {
            let j = a.task_json(&l).await;
            if j["task"]["status"] == "queued" && j["task"]["failure_class"] == "rate_limit" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if round == 0 {
            queued::budget::clear_pause(&h.st.db, uid, "mock", true)
                .await
                .unwrap();
            // Wait until the second run was claimed, then for its re-queue.
            loop {
                let s = a.task_json(&l).await["task"]["status"].clone();
                if s == "running" {
                    break;
                }
                h.st.wake.notify_one();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    let ctx = a.task_json(&l).await["task"]["context"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        ctx.matches(queued::runner::NOTE_RATE_LIMIT).count(),
        1,
        "{ctx}"
    );
}

/// AK17: auth failure pauses the provider until resumed; no fix attempt.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn auth_failure_pauses_provider_until_resumed(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a.task(&p, "MOCK_AUTH needs login").await;
    let lim = loop {
        let l = a.limit("mock").await;
        if !l["paused_reason"].is_null() {
            break l;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(lim["blocked"], true);
    assert!(lim["paused_reason"]
        .as_str()
        .unwrap()
        .contains("Invalid API key"));
    let q = a.wait_status(&t, &["queued"]).await;
    assert_eq!(q["task"]["failure_class"], "provider_auth");
    assert_eq!(q["task"]["attempt"], 0);
    assert_eq!(q["task"]["transient_retries"], 0);
    let other = a.task(&p, "waits for the provider").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(a.task_json(&other).await["task"]["status"], "queued");
    assert_eq!(a.task_json(&t).await["task"]["status"], "queued");
    assert!(a
        .notifications()
        .await
        .iter()
        .any(|n| n["kind"] == "provider_paused"));
    assert_eq!(fixes(&a.tasks().await), 0);

    let (code, _) = a
        .patch(
            &format!("/api/tasks/{t}"),
            json!({"title": "logged in", "prompt": "logged in now"}),
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(a.post("/api/limits/nope/resume", json!({})).await.0, 400);
    let (code, s) = a.post("/api/limits/mock/resume", json!({})).await;
    assert_eq!(code, 200, "{s}");
    assert_eq!(s["blocked"], false);
    assert!(s["paused_reason"].is_null());
    a.wait_status(&t, &["done"]).await;
    a.wait_status(&other, &["done"]).await;

    // Updating credentials lifts an auth pause of a real provider.
    let uid = user_id(&h, "a@x.ch").await;
    queued::budget::set_paused(&h.st.db, uid, "claude", "authentication failed")
        .await
        .unwrap();
    assert_eq!(a.limit("claude").await["blocked"], true);
    let (code, _) = a
        .put(
            "/api/credentials/claude",
            json!({"kind": "api_key", "secret": "sk-new"}),
        )
        .await;
    assert_eq!(code, 200);
    let l = a.limit("claude").await;
    assert!(l["paused_reason"].is_null());
    assert_eq!(l["blocked"], false);
}

/// AK18: a broken credential (unknown kind) blocks as `config` without a CLI start.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn unknown_credential_kind_blocks_as_config(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let uid = user_id(&h, "a@x.ch").await;
    let p = no_review_project(&a).await;
    queued::queue::store_credential(&h.st, uid, "claude", "bogus", "x")
        .await
        .unwrap();
    let t = a
        .task_with(&p, "use claude", json!({"provider": "claude"}))
        .await;
    let b = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "config");
    assert!(b["task"]["error"]
        .as_str()
        .unwrap()
        .contains("unsupported claude credential kind bogus"));
    assert_eq!(fixes(&a.tasks().await), 0);
    assert!(
        !h.workspace(&p).join("MOCK_LOG.md").exists(),
        "no agent was started"
    );
}

/// AK19 + AK10: real task failures keep the fix loop and are classed `task`;
/// a "429"/"quota" on stderr does not pause the provider.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn task_failures_keep_fix_loop_and_class(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a.task(&p, "MOCK_STDERR429 broken build").await;
    let ts = a
        .wait_until("fix chain blocked", |ts| {
            status_of(ts, "Fix #2").contains(&"blocked".to_string())
        })
        .await;
    let root = a.task_json(&t).await;
    assert_eq!(root["task"]["status"], "failed");
    assert_eq!(root["task"]["failure_class"], "task");
    assert!(root["task"]["error"]
        .as_str()
        .unwrap()
        .contains("HTTP 429 quota exceeded"));
    let fix2 = ts
        .iter()
        .find(|t| t["title"].as_str().unwrap().starts_with("Fix #2"))
        .unwrap();
    assert_eq!(fix2["failure_class"], "task");
    let l = a.limit("mock").await;
    assert_eq!(l["blocked"], false, "stderr is never classified: {l}");
    assert!(l["limited_until"].is_null());
}

/// AK20: a rate limit in the review run pauses the review provider.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn review_rate_limit_pauses_review_provider(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    a.patch(
        &format!("/api/projects/{p}"),
        json!({"review_provider": "mock"}),
    )
    .await;
    let t = a.task(&p, "MOCK_REVIEW_LIMIT change").await;
    // WP03 (WP01-N1): the review is retried as its own phase, never skipped
    // to `done`; the paused provider keeps the task waiting in the queue.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let tj = loop {
        let tj = a.task_json(&t).await;
        if tj["task"]["gate"]["resume"] == "review" && tj["task"]["status"] == "queued" {
            break tj;
        }
        assert!(tokio::time::Instant::now() < deadline, "{tj}");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    };
    assert_eq!(
        tj["task"]["gate"]["findings"][0]["kind"],
        "review_unavailable"
    );
    let ev = a.events_of(&t).await;
    assert!(has_event(&ev, "review", "nur das Review wird"), "{ev:?}");
    assert!(!has_event(&ev, "status", "Erledigt"), "{ev:?}");
    let l = a.limit("mock").await;
    assert!(!l["limited_until"].is_null(), "{l}");
    assert_eq!(l["blocked"], true);
}

/// AK21: fix tasks inherit task-level limits, follow-ups do not.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn fix_tasks_inherit_task_limits(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"max_auto_tasks": 3, "max_fix_attempts": 1}))
        .await;
    let p = no_review_project(&a).await;
    let lim =
        json!({"max_runtime_secs": 50, "max_tokens": 900000, "max_turns": 40, "max_cost_usd": 5.5});
    a.task_with(&p, "MOCK_FAIL limited", lim.clone()).await;
    let ts = a
        .wait_until("fix #1 exists", |ts| {
            ts.iter()
                .any(|t| t["title"].as_str().unwrap().starts_with("Fix #1"))
        })
        .await;
    let fix = ts
        .iter()
        .find(|t| t["title"].as_str().unwrap().starts_with("Fix #1"))
        .unwrap();
    for k in [
        "max_runtime_secs",
        "max_tokens",
        "max_turns",
        "max_cost_usd",
    ] {
        assert_eq!(fix[k], lim[k], "{k}");
    }
    let f = a.task_with(&p, "MOCK_FOLLOWUPS plan", lim.clone()).await;
    a.wait_status(&f, &["done"]).await;
    let ts = a
        .wait_until("follow-ups", |ts| {
            ts.iter().filter(|t| t["parent_id"] == f.as_str()).count() == 3
        })
        .await;
    for fu in ts.iter().filter(|t| t["parent_id"] == f.as_str()) {
        assert!(fu["max_tokens"].is_null() && fu["max_runtime_secs"].is_null());
    }
}

/// AK22: runtime limit → blocked without fix, partial commit kept.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn runtime_limit_blocks_without_fix(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a
        .task_with(&p, "MOCK_SLOW forever", json!({"max_runtime_secs": 2}))
        .await;
    a.wait_status(&t, &["running"]).await;
    let started = Instant::now();
    let b = a.wait_status(&t, &["blocked"]).await;
    assert!(
        started.elapsed() < Duration::from_secs(2 + 2 + 4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(b["task"]["failure_class"], "limit_runtime");
    assert_eq!(b["task"]["error"], "Limit überschritten: Laufzeit 2 s");
    assert_eq!(fixes(&a.tasks().await), 0);
    // WP04: the partial commit is on the task branch, never on the target.
    let commits = a.get(&format!("/api/projects/{p}/commits")).await.1;
    assert!(!commits.to_string().contains("unterbrochen"), "{commits}");
    let branch = b["task"]["branch"].as_str().unwrap();
    assert!(
        branch_subject(&h, &p, branch).contains("(unterbrochen: limit_runtime)"),
        "{branch}"
    );
    assert!(!b["task"]["snapshot_after"].is_null());
}

/// AK23: stall detection → re-queued twice, then blocked as `stalled`.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn stall_requeues_then_blocks(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let (code, pj) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"stall_timeout_secs": 2}),
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(pj["stall_timeout_secs"], 2);
    let t = a.task(&p, "MOCK_SLOW hangs").await;
    let b = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "stalled");
    assert_eq!(b["task"]["transient_retries"], 3);
    assert_eq!(b["task"]["attempt"], 0);
    let ev = a.events_of(&t).await;
    assert_eq!(
        ev.iter()
            .filter(|(k, c)| k == "status" && c.contains("Erneut eingereiht (Stillstand)"))
            .count(),
        2
    );
    assert!(b["task"]["context"]
        .as_str()
        .unwrap()
        .contains("produced no output for 2 seconds"));
    assert_eq!(fixes(&a.tasks().await), 0);
}

/// AK24: live token guard stops the run.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn token_limit_stops_run_live(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let started = Instant::now();
    let t = a
        .task_with(&p, "MOCK_TOKENS burn", json!({"max_tokens": 2000}))
        .await;
    let b = a.wait_status(&t, &["blocked"]).await;
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "stopped before the script's 4 s of sleeps: {:?}",
        started.elapsed()
    );
    let task = &b["task"];
    assert_eq!(task["failure_class"], "limit_tokens");
    let counted = task["usage"]["input_tokens"].as_i64().unwrap()
        + task["usage"]["output_tokens"].as_i64().unwrap();
    assert!(counted >= 2700, "{task}");
    assert!(counted < 4050, "third usage line never arrived: {task}");
    let ev = a.events_of(&t).await;
    assert!(
        has_event(&ev, "guard", "Token-Limit erreicht (2700 / 2000)"),
        "{ev:?}"
    );
    assert!(
        !ev.iter().any(|(k, _)| k == "contract"),
        "no contract evaluation"
    );
    assert_eq!(fixes(&a.tasks().await), 0);
}

/// Reviewer decision (open point of WP01-test): a token limit that is only
/// exceeded according to the final usage report (codex/gemini; mock via
/// MOCK_USAGE_TOTAL) blocks the task like the live guard – even with a
/// `done` contract and review enabled: no review run, no fix task, no
/// follow-ups, the changes stay as a partial commit inside the task diff.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn post_hoc_token_overrun_blocks_without_review_or_fix(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Demo"})).await;
    a.patch(
        &format!("/api/projects/{p}"),
        json!({"review_enabled": true, "review_provider": "mock"}),
    )
    .await;
    let t = a
        .task_with(&p, "MOCK_USAGE_TOTAL big", json!({"max_tokens": 3000}))
        .await;
    let b = a.wait_status(&t, &["blocked"]).await;
    let task = &b["task"];
    assert_eq!(task["failure_class"], "limit_tokens", "{task}");
    let err = task["error"].as_str().unwrap();
    assert!(
        err.contains("Limit überschritten: Tokens 3000")
            && err.contains("verbraucht 4500 Tokens")
            && err.contains("erst nach dem Lauf"),
        "{err}"
    );
    // The final report replaces the live deltas (1350) – counted once.
    assert_eq!(task["usage"]["input_tokens"], 4000);
    assert_eq!(usage_rows(&h, &t).await, vec![(4000, 500)]);
    let ev = a.events_of(&t).await;
    assert!(
        !ev.iter().any(|(k, _)| k == "review"),
        "no review run: {ev:?}"
    );
    assert!(
        !ev.iter().any(|(k, _)| k == "contract"),
        "result not evaluated: {ev:?}"
    );
    assert_eq!(fixes(&a.tasks().await), 0);
    // WP04: the partial commit is on the task branch, never on the target.
    let commits = a.get(&format!("/api/projects/{p}/commits")).await.1;
    assert!(!commits.to_string().contains("unterbrochen"), "{commits}");
    let branch = task["branch"].as_str().unwrap();
    assert!(
        branch_subject(&h, &p, branch).contains("(unterbrochen: limit_tokens)"),
        "{branch}"
    );
    assert_eq!(
        task["snapshot_after"].as_str().unwrap(),
        branch_sha(&h, &p, branch)
    );
    // Without a limit the same run is done (the overrun alone decides).
    let free = a.task(&p, "MOCK_USAGE_TOTAL free").await;
    a.wait_status(&free, &["done"]).await;
}

/// AK25: live cost guard.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cost_limit_stops_run_live(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a
        .task_with(&p, "MOCK_TOKENS spend", json!({"max_cost_usd": 0.01}))
        .await;
    let b = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "limit_cost");
    assert!(b["task"]["usage"]["cost_usd"].as_f64().unwrap() >= 0.012);
    assert!(has_event(
        &a.events_of(&t).await,
        "guard",
        "Kosten-Limit erreicht"
    ));
}

/// AK26: the turn limit reaches the CLI (mock: env var) and maps to `limit_turns`.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn turn_limit_is_passed_to_cli(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let limited = a
        .task_with(&p, "MOCK_TURNS limited", json!({"max_turns": 1}))
        .await;
    let free = a.task(&p, "MOCK_TURNS free").await;
    let b = a.wait_status(&limited, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "limit_turns");
    assert!(b["task"]["error"]
        .as_str()
        .unwrap()
        .contains("von der CLI gemeldet"));
    a.wait_status(&free, &["done"]).await;
    // Project default applies to tasks without override.
    a.patch(&format!("/api/projects/{p}"), json!({"max_turns": 3}))
        .await;
    let by_project = a.task(&p, "MOCK_TURNS project limit").await;
    let b = a.wait_status(&by_project, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "limit_turns");
}

/// AK29: cancel sends SIGINT; output after SIGINT (usage) is still recorded.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cancel_sends_sigint_and_records_usage(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let t = a.task(&p, "MOCK_TRAP long").await;
    loop {
        if has_event(&a.events_of(&t).await, "text", "waiting for SIGINT") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let started = Instant::now();
    assert_eq!(
        a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await.0,
        200
    );
    let c = a.wait_status(&t, &["cancelled"]).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(c["task"]["failure_class"], "cancelled");
    assert_eq!(
        c["task"]["usage"]["input_tokens"], 2400,
        "start usage + usage flushed in the SIGINT trap"
    );
    assert!(has_event(
        &a.events_of(&t).await,
        "text",
        "interrupted cleanly"
    ));
    let rows = usage_rows(&h, &t).await;
    assert_eq!(rows.iter().map(|r| r.0).sum::<i64>(), 2400, "{rows:?}");
}

/// AK06: structured output wins over the file; invalid structured output falls back.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn structured_output_wins_and_invalid_falls_back(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = no_review_project(&a).await;
    let s = a.task(&p, "MOCK_STRUCTURED x").await;
    let d = a.wait_status(&s, &["done"]).await;
    assert_eq!(d["task"]["summary"], "structured ok");
    assert!(has_event(
        &a.events_of(&s).await,
        "contract",
        "Ergebnis via structured output"
    ));
    let b = a.task(&p, "MOCK_STRUCTURED_BAD y").await;
    let d = a.wait_status(&b, &["done"]).await;
    assert_eq!(d["task"]["summary"], "file ok");
    let ev = a.events_of(&b).await;
    assert!(
        has_event(&ev, "contract", "structured output ungültig")
            && has_event(
                &ev,
                "contract",
                "Ergebnis via .queue/result.json (Fallback)"
            ),
        "{ev:?}"
    );
}

/// AK30: limit validation, reset with null, viewer forbidden, retry resets, meta.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn limit_fields_validation_reset_retry_and_meta(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let p = no_review_project(&a).await;
    let path = format!("/api/projects/{p}");
    for bad in [
        json!({"max_runtime_secs": 0}),
        json!({"max_runtime_secs": 86401}),
        json!({"stall_timeout_secs": -1}),
        json!({"max_cost_usd": 0}),
        json!({"max_tokens": 0}),
        json!({"max_turns": 1001}),
    ] {
        let (code, e) = a.patch(&path, bad.clone()).await;
        assert_eq!(code, 400, "{bad}: {e}");
    }
    let (code, pj) = a
        .patch(
            &path,
            json!({"max_runtime_secs": 600, "stall_timeout_secs": 0, "max_cost_usd": 1.5, "max_tokens": 50000, "max_turns": 25}),
        )
        .await;
    assert_eq!(code, 200, "{pj}");
    assert_eq!(pj["max_runtime_secs"], 600);
    assert_eq!(pj["stall_timeout_secs"], 0);
    // Absent fields stay, null resets.
    let (_, pj) = a.patch(&path, json!({"max_tokens": null})).await;
    assert!(pj["max_tokens"].is_null());
    assert_eq!(pj["max_turns"], 25);
    assert_eq!(pj["max_cost_usd"], 1.5);
    // Viewer may not change limits.
    a.post(
        &format!("/api/projects/{p}/members"),
        json!({"email": "b@x.ch", "role": "viewer"}),
    )
    .await;
    assert_eq!(b.patch(&path, json!({"max_turns": 2})).await.0, 403);

    // Task limits on create/patch.
    let (code, _) = a
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "x", "max_turns": 0}),
        )
        .await;
    assert_eq!(code, 400);
    let t = a
        .task_with(&p, "MOCK_TRANSIENT_ALWAYS x", json!({"max_tokens": 123456}))
        .await;
    let j = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(j["task"]["max_tokens"], 123456);
    assert_eq!(j["task"]["transient_retries"], 3);
    let (code, _) = a
        .patch(&format!("/api/tasks/{t}"), json!({"max_tokens": null}))
        .await;
    assert_eq!(code, 409, "only waiting tasks can be edited");
    // Retry resets the transient state (and the task's usage counter).
    sqlx::query("UPDATE user_settings SET parallelism=1")
        .execute(&h.st.db)
        .await
        .unwrap();
    let uid = user_id(&h, "a@x.ch").await;
    queued::budget::set_limited(
        &h.st.db,
        uid,
        "mock",
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    assert_eq!(
        a.post(&format!("/api/tasks/{t}/retry"), json!({})).await.0,
        200
    );
    let r = a.task_json(&t).await["task"].clone();
    assert_eq!(r["status"], "queued");
    assert_eq!(r["transient_retries"], 0);
    assert!(r["failure_class"].is_null() && r["not_before"].is_null() && r["usage"].is_null());
    let (code, _) = a
        .patch(
            &format!("/api/tasks/{t}"),
            json!({"max_tokens": null, "max_runtime_secs": 30}),
        )
        .await;
    assert_eq!(code, 200);
    let r = a.task_json(&t).await["task"].clone();
    assert!(r["max_tokens"].is_null());
    assert_eq!(r["max_runtime_secs"], 30);

    let m = a.get("/api/meta").await.1;
    assert_eq!(m["cli_versions"]["claude"], "2.1.319");
    assert_eq!(m["cli_versions"]["codex"], "0.167.0");
    assert_eq!(m["cli_versions"]["gemini"], "0.67.0");
    assert_eq!(m["provider_capabilities"]["claude"]["max_cost"], true);
    assert_eq!(m["provider_capabilities"]["codex"]["max_turns"], false);
    assert_eq!(
        m["provider_capabilities"]["gemini"]["structured_output"],
        false
    );
}

/// Subject of the newest commit of a task branch in the main repository.
fn branch_subject(h: &Harness, project: &str, branch: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(h.workspace(project))
        .args(["log", "-1", "--format=%s", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn branch_sha(h: &Harness, project: &str, branch: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(h.workspace(project))
        .args(["rev-parse", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}
