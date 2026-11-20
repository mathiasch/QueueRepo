//! WP10 integration: analytics – tenant separation, numbers on a known
//! data set, pause history, session insights and a load test with
//! realistic data volumes.
mod common;

use std::time::{Duration, Instant};

use chrono::Utc;
use common::start;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn uid(c: &common::Client) -> Uuid {
    Uuid::parse_str(c.get("/api/auth/me").await.1["id"].as_str().unwrap()).unwrap()
}

/// A finished task row (SQL, deterministic times and outcomes).
#[allow(clippy::too_many_arguments)]
async fn task_row(
    db: &PgPool,
    user: Uuid,
    project: &str,
    kind: &str,
    status: &str,
    parent: Option<Uuid>,
    hours_ago: i64,
    minutes_to_finish: i64,
    title: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tasks (id, user_id, project_id, parent_id, kind, title, prompt, provider, status, created_at, finished_at, updated_at)
         VALUES ($1,$2,$3::uuid,$4,$5,$6,'p','mock',$7, now() - make_interval(hours => $8::int), now() - make_interval(hours => $8::int) + make_interval(mins => $9::int), now())",
    )
    .bind(id)
    .bind(user)
    .bind(project)
    .bind(parent)
    .bind(kind)
    .bind(title)
    .bind(status)
    .bind(hours_ago as i32)
    .bind(minutes_to_finish as i32)
    .execute(db)
    .await
    .unwrap();
    id
}

#[allow(clippy::too_many_arguments)]
async fn usage_row(
    db: &PgPool,
    user: Uuid,
    project: &str,
    task: Option<Uuid>,
    provider: &str,
    price: Option<&str>,
    input: i64,
    cache_read: i64,
    currency: &str,
    amount: f64,
    hours_ago: i64,
) {
    sqlx::query(
        "INSERT INTO usage_log (user_id, provider, task_id, ts, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                                cost_usd, project_id, price_id, currency, cost_amount, cost_source)
         VALUES ($1,$2,$3, now() - make_interval(hours => $4::int), $5, 10, $6, 0, 0, $7::uuid, $8, $9, $10, 'catalog')",
    )
    .bind(user)
    .bind(provider)
    .bind(task)
    .bind(hours_ago as i32)
    .bind(input)
    .bind(cache_read)
    .bind(project)
    .bind(price)
    .bind(currency)
    .bind(amount)
    .execute(db)
    .await
    .unwrap();
}

fn money(v: &Value, cur: &str) -> f64 {
    v.as_array()
        .unwrap()
        .iter()
        .find(|m| m["currency"] == cur)
        .map(|m| m["amount"].as_f64().unwrap())
        .unwrap_or(0.0)
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn analytics_are_separated_per_tenant_and_role(pool: PgPool) {
    let h = start(pool.clone()).await;
    let a = h.user("ana-a@x.ch").await;
    let b = h.user("ana-b@x.ch").await;
    let c = h.user("ana-c@x.ch").await;
    let (ua, ub) = (uid(&a).await, uid(&b).await);
    let shared = a.project(json!({"name": "Shared-A"})).await;
    let private = a.project(json!({"name": "Private-A"})).await;
    let pb = b.project(json!({"name": "Own-B"})).await;
    a.post(
        &format!("/api/projects/{shared}/members"),
        json!({"email": "ana-b@x.ch", "role": "editor"}),
    )
    .await;
    a.post(
        &format!("/api/projects/{shared}/members"),
        json!({"email": "ana-c@x.ch", "role": "viewer"}),
    )
    .await;
    let db = &h.st.db;
    // A: shared – done first pass; private – failed root with a done fix,
    // blocked root, cancelled root (not counted for first pass).
    let t1 = task_row(
        db,
        ua,
        &shared,
        "task",
        "done",
        None,
        30,
        20,
        "A shared done",
    )
    .await;
    let t2 = task_row(
        db,
        ua,
        &private,
        "task",
        "failed",
        None,
        20,
        10,
        "A private failed",
    )
    .await;
    task_row(
        db,
        ua,
        &private,
        "fix",
        "done",
        Some(t2),
        19,
        10,
        "A private fix",
    )
    .await;
    task_row(
        db,
        ua,
        &private,
        "task",
        "blocked",
        None,
        10,
        5,
        "A private blocked",
    )
    .await;
    task_row(
        db,
        ua,
        &private,
        "task",
        "cancelled",
        None,
        5,
        1,
        "A cancelled",
    )
    .await;
    // Outside the 7-day window.
    task_row(db, ua, &private, "task", "done", None, 24 * 10, 5, "A old").await;
    // B: own project and a task in A's shared project (as editor).
    let tb = task_row(db, ub, &pb, "task", "done", None, 3, 3, "B own done").await;
    let tbs = task_row(db, ub, &shared, "task", "done", None, 2, 3, "B in shared").await;
    usage_row(
        db,
        ua,
        &shared,
        Some(t1),
        "claude",
        Some("anthropic/claude-sonnet-5-5"),
        1000,
        3000,
        "USD",
        1.5,
        30,
    )
    .await;
    usage_row(
        db,
        ua,
        &private,
        Some(t2),
        "codex",
        None,
        500,
        0,
        "USD",
        0.25,
        20,
    )
    .await;
    usage_row(
        db,
        ua,
        &private,
        Some(t2),
        "opencode",
        None,
        100,
        0,
        "CHF",
        0.4,
        20,
    )
    .await;
    usage_row(
        db,
        ub,
        &pb,
        Some(tb),
        "claude",
        None,
        7000,
        0,
        "USD",
        7.0,
        3,
    )
    .await;
    usage_row(
        db,
        ub,
        &shared,
        Some(tbs),
        "gemini",
        None,
        50,
        0,
        "USD",
        0.05,
        2,
    )
    .await;

    // --- A, scope mine, 7 days.
    let (code, m) = a.get("/api/analytics?days=7").await;
    assert_eq!(code, 200, "{m}");
    let o = &m["outcomes"];
    assert_eq!(o["finished"], 5, "{o}");
    assert_eq!(o["done"], 2);
    assert_eq!(o["cancelled"], 1);
    assert_eq!(
        o["first_pass_total"], 3,
        "done + failed + blocked originals"
    );
    assert_eq!(o["first_pass_done"], 1);
    assert_eq!(o["eventual_done"], 2, "the failed task was repaired");
    assert_eq!(o["fixes"], 1);
    assert!(
        (money(&m["costs"], "USD") - 1.75).abs() < 1e-9,
        "{}",
        m["costs"]
    );
    assert!((money(&m["costs"], "CHF") - 0.4).abs() < 1e-9);
    assert_eq!(m["tokens"]["cache_read"], 3000);
    let names: Vec<&str> = m["by_project"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Shared-A") && names.contains(&"Private-A"));
    assert!(!names.contains(&"Own-B"), "{names:?}");
    let tops: Vec<&str> = m["top_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(tops.first(), Some(&"A shared done"));
    assert!(!tops.iter().any(|t| t.starts_with("B ")), "{tops:?}");
    let claude = m["by_provider"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "claude")
        .unwrap();
    assert_eq!(claude["model"], "anthropic/claude-sonnet-5-5");
    assert_eq!(
        m["pauses"],
        json!([]),
        "mine includes the (empty) pause list"
    );
    assert_eq!(m["daily"].as_array().unwrap().len(), 8, "7 days + today");
    // The old task counts in the 30-day window.
    let m30 = a.get("/api/analytics?days=30").await.1;
    assert_eq!(m30["outcomes"]["finished"], 6);

    // --- B, scope mine: only B's own tasks and usage (incl. B's work in A's
    // shared project – B is a member), never A's.
    let mb = b.get("/api/analytics?days=7").await.1;
    assert_eq!(mb["outcomes"]["finished"], 2, "{}", mb["outcomes"]);
    assert!(
        (money(&mb["costs"], "USD") - 7.05).abs() < 1e-9,
        "{}",
        mb["costs"]
    );
    let tb_titles = mb["top_tasks"].to_string();
    assert!(!tb_titles.contains("A shared"), "{tb_titles}");

    // --- Project scope: every member sees the whole project (as the task
    // list does), nobody sees projects they are not a member of.
    let ps = b
        .get(&format!(
            "/api/analytics?days=7&scope=project&project_id={shared}"
        ))
        .await
        .1;
    assert_eq!(ps["outcomes"]["finished"], 2);
    assert!(
        (money(&ps["costs"], "USD") - 1.55).abs() < 1e-9,
        "{}",
        ps["costs"]
    );
    assert_eq!(ps["pauses"], Value::Null, "pauses are per user");
    let pc = c
        .get(&format!(
            "/api/analytics?days=7&scope=project&project_id={shared}"
        ))
        .await;
    assert_eq!(pc.0, 200, "viewer may read the project analytics");
    for (who, p) in [(&b, &private), (&c, &private), (&c, &pb), (&a, &pb)] {
        assert_eq!(
            who.get(&format!(
                "/api/analytics?days=7&scope=project&project_id={p}"
            ))
            .await
            .0,
            404
        );
    }
    // C has no tasks: nothing of A or B in C's own view.
    let mc = c.get("/api/analytics?days=7").await.1;
    assert_eq!(mc["outcomes"]["finished"], 0);
    assert_eq!(mc["costs"], json!([]));
    assert_eq!(mc["by_project"], json!([]));

    // Removed from the project: B's own work there leaves B's view too.
    assert_eq!(
        a.delete(&format!("/api/projects/{shared}/members/{ub}"))
            .await
            .0,
        200
    );
    let mb = b.get("/api/analytics?days=7").await.1;
    assert_eq!(mb["outcomes"]["finished"], 1);
    assert!((money(&mb["costs"], "USD") - 7.0).abs() < 1e-9);
    assert_eq!(
        b.get(&format!(
            "/api/analytics?days=7&scope=project&project_id={shared}"
        ))
        .await
        .0,
        404
    );

    // Validation and access paths.
    assert_eq!(a.get("/api/analytics?days=5").await.0, 400);
    assert_eq!(a.get("/api/analytics?days=7&scope=project").await.0, 400);
    assert_eq!(a.get("/api/analytics?days=7&tz=Mars/Base").await.0, 400);
    assert_eq!(h.anon().get("/api/analytics").await.0, 401);
    let tok = a.token(&["tasks:read", "projects:read"]).await;
    assert_eq!(h.bearer(&tok).get("/api/analytics").await.0, 403);
    assert_eq!(
        h.bearer(&tok)
            .get(&format!("/api/tasks/{t1}/insights"))
            .await
            .0,
        403
    );

    // Insights: owner and members, nobody else.
    assert_eq!(a.get(&format!("/api/tasks/{t1}/insights")).await.0, 200);
    assert_eq!(c.get(&format!("/api/tasks/{t1}/insights")).await.0, 200);
    assert_eq!(c.get(&format!("/api/tasks/{t2}/insights")).await.0, 404);
    assert_eq!(
        b.get(&format!("/api/tasks/{t1}/insights")).await.0,
        404,
        "removed"
    );
    let i = a.get(&format!("/api/tasks/{t2}/insights")).await.1;
    assert_eq!(i["fixes"], 1);
    assert!((money(&i["costs"], "USD") - 0.25).abs() < 1e-9);
    assert!((money(&i["costs"], "CHF") - 0.4).abs() < 1e-9);
}

/// Real runs: `task_runs` rows, tool counts and the insights text.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn insights_of_a_real_run(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("ins@x.ch").await;
    let p = a
        .project(json!({"name": "p", "review_enabled": true}))
        .await;
    let id = a.task(&p, "make a change").await;
    a.wait_status(&id, &["done"]).await;
    let (code, i) = a.get(&format!("/api/tasks/{id}/insights")).await;
    assert_eq!(code, 200, "{i}");
    let runs = i["runs"].as_array().unwrap();
    assert!(!runs.is_empty(), "{i}");
    let agent = runs.iter().find(|r| r["kind"] == "agent").unwrap();
    assert_eq!(agent["outcome"], "success");
    assert_eq!(agent["provider"], "mock");
    assert_eq!(agent["tokens"]["cache_read"], 800);
    assert!(agent["tool_calls"].as_i64().unwrap() >= 1);
    assert_eq!(i["tools"][0][0], "edit", "{}", i["tools"]);
    assert!(!i["notes"].as_array().unwrap().is_empty());
    // Dashboard: agent time median exists now.
    let m = a.get("/api/analytics?days=7").await.1;
    assert!(
        m["durations"]["agent_median"].as_f64().is_some(),
        "{}",
        m["durations"]
    );
    assert!(m["durations"]["to_done_median"].as_f64().is_some());
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn pause_history_merges_overlaps(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("pause@x.ch").await;
    let b = h.user("pause-b@x.ch").await;
    let ua = uid(&a).await;
    let db = &h.st.db;
    // Rate limit for 30 min, extended to 60 min (one pause, not two).
    queued::budget::set_limited(db, ua, "mock", Utc::now() + chrono::Duration::minutes(30))
        .await
        .unwrap();
    queued::budget::set_limited(db, ua, "mock", Utc::now() + chrono::Duration::minutes(60))
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM provider_pauses WHERE user_id=$1")
        .bind(ua)
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(n, 1);
    // Lifted now: ends now.
    assert!(queued::budget::clear_pause(db, ua, "mock", true)
        .await
        .unwrap());
    // Historic pauses: two overlapping auth+rate-limit pauses on claude
    // (90 min union), one budget pause.
    sqlx::query(
        "INSERT INTO provider_pauses (user_id, provider, reason, started_at, ends_at) VALUES
           ($1,'claude','rate_limit', now() - interval '5 hours', now() - interval '4 hours'),
           ($1,'claude','rate_limit', now() - interval '4 hours 30 minutes', now() - interval '3 hours 30 minutes'),
           ($1,'codex','budget', now() - interval '2 hours', now() - interval '1 hour'),
           ($1,'codex','budget', now() - interval '20 days', now() - interval '19 days')",
    )
    .bind(ua)
    .execute(db)
    .await
    .unwrap();
    // An open auth pause.
    queued::budget::set_paused(db, ua, "gemini", "key revoked")
        .await
        .unwrap();
    queued::budget::set_paused(db, ua, "gemini", "key revoked")
        .await
        .unwrap();
    let m = a.get("/api/analytics?days=7").await.1;
    let pauses = m["pauses"].as_array().unwrap();
    let find = |p: &str, r: &str| {
        pauses
            .iter()
            .find(|x| x["provider"] == p && x["reason"] == r)
            .unwrap_or_else(|| panic!("{p}/{r}: {pauses:?}"))
            .clone()
    };
    let c = find("claude", "rate_limit");
    assert_eq!(c["pauses"], 2);
    assert_eq!(c["minutes"], 90, "overlap counted once");
    assert_eq!(
        find("codex", "budget")["minutes"],
        60,
        "older one outside the window"
    );
    assert_eq!(
        find("gemini", "auth")["pauses"],
        1,
        "no duplicate open pause"
    );
    assert!(find("mock", "rate_limit")["minutes"].as_i64().unwrap() <= 1);
    // Metrics gauge: open pauses (all users).
    assert_eq!(b.get("/api/analytics?days=7").await.1["pauses"], json!([]));
}

/// Load test: 50 users, 60 000 tasks, ≈ 200 000 usage rows. The dashboard
/// must stay fast and the usage queries must use the indexes.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn analytics_stay_fast_with_realistic_volumes(pool: PgPool) {
    let h = start(pool.clone()).await;
    let a = h.user("big@x.ch").await;
    let ua = uid(&a).await;
    let p = a.project(json!({"name": "Big"})).await;
    let t = Instant::now();
    sqlx::query(
        "INSERT INTO users (id, email, name) SELECT gen_random_uuid(), 'load' || g || '@x.ch', 'load' FROM generate_series(1, 49) g",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO projects (id, owner_id, name)
         SELECT gen_random_uuid(), u.id, 'load project' FROM users u WHERE u.email LIKE 'load%'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role) SELECT id, owner_id, 'owner' FROM projects WHERE name='load project'",
    )
    .execute(&pool)
    .await
    .unwrap();
    // 1 200 tasks per user (the test user included), over 120 days.
    sqlx::query(
        "INSERT INTO tasks (id, user_id, project_id, kind, title, prompt, provider, status, created_at, finished_at, updated_at)
         SELECT gen_random_uuid(), m.user_id, m.project_id, CASE WHEN g % 7 = 0 THEN 'fix' ELSE 'task' END, 'load task', 'p', 'claude',
                (ARRAY['done','done','done','failed','blocked','cancelled'])[1 + g % 6],
                now() - make_interval(hours => (g * 7919) % 2880),
                now() - make_interval(hours => (g * 7919) % 2880) + interval '20 minutes', now()
           FROM project_members m, generate_series(1, 1200) g",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO usage_log (user_id, provider, task_id, ts, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                                cost_usd, project_id, price_id, currency, cost_amount)
         SELECT t.user_id, (ARRAY['claude','codex','gemini'])[1 + (g % 3)], t.id, t.finished_at - interval '5 minutes',
                1000, 200, 3000, 100, 0.01, t.project_id, 'anthropic/claude-sonnet-5-5', 'USD', 0.01
           FROM tasks t, generate_series(1, 4) g
          WHERE t.prompt = 'p' AND t.title = 'load task' AND (t.id::text < 'd')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();
    let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(&pool)
        .await
        .unwrap();
    let usage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(tasks >= 60_000, "{tasks}");
    assert!(usage >= 150_000, "{usage}");
    eprintln!(
        "load data: {tasks} tasks, {usage} usage rows in {:?}",
        t.elapsed()
    );

    for (path, label) in [
        ("/api/analytics?days=90".to_string(), "mine 90d"),
        ("/api/analytics?days=30".to_string(), "mine 30d"),
        (
            format!("/api/analytics?days=90&scope=project&project_id={p}"),
            "project 90d",
        ),
    ] {
        // Warm up once (prepared statements), then measure.
        assert_eq!(a.get(&path).await.0, 200);
        let t = Instant::now();
        let (code, m) = a.get(&path).await;
        let took = t.elapsed();
        eprintln!("{label}: {took:?}");
        assert_eq!(code, 200);
        assert!(
            m["outcomes"]["finished"].as_i64().unwrap() >= 300,
            "{label}: {}",
            m["outcomes"]
        );
        assert!(took < Duration::from_secs(3), "{label} took {took:?}");
    }
    // The usage aggregation of one user uses an index, not a scan of all rows.
    let plan: Vec<(String,)> = sqlx::query_as(
        "EXPLAIN SELECT u.provider, SUM(u.input_tokens) FROM usage_log u
          WHERE u.user_id = $1 AND u.ts >= now() - interval '30 days' AND u.ts < now()
            AND u.project_id IN (SELECT project_id FROM project_members WHERE user_id = $1)
          GROUP BY 1",
    )
    .bind(ua)
    .fetch_all(&pool)
    .await
    .unwrap();
    let plan = plan.into_iter().map(|r| r.0).collect::<Vec<_>>().join("\n");
    assert!(!plan.contains("Seq Scan on usage_log"), "{plan}");
    let plan: Vec<(String,)> = sqlx::query_as(
        "EXPLAIN SELECT COUNT(*) FROM tasks t
          WHERE t.user_id = $1 AND t.finished_at >= now() - interval '30 days' AND t.finished_at < now()",
    )
    .bind(ua)
    .fetch_all(&pool)
    .await
    .unwrap();
    let plan = plan.into_iter().map(|r| r.0).collect::<Vec<_>>().join("\n");
    assert!(!plan.contains("Seq Scan on tasks"), "{plan}");
}
