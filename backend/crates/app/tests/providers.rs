//! WP13 integration tests: connectors, price catalog and money costs, data
//! residency, routing/presets, single-shot steps (sync + batch for Anthropic,
//! OpenAI, Mistral) against a local fake provider, escalation, estimate,
//! cost summary, LiteLLM task keys, CLI status and the eval set.
//! App + real runner (engine `none`) + Postgres; no real provider API calls.
mod common;

use std::{sync::atomic::Ordering, time::Duration};

use common::{fake_llm::start_fake, start, Client, Harness};
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

async fn connector(a: &Client, body: Value) -> Value {
    let (code, c) = a.post("/api/connectors", body).await;
    assert_eq!(code, 200, "{c}");
    c
}

/// Mock connector whose single-shot steps go to the fake server.
async fn fake_connector(a: &Client, url: &str, api: &str, location: &str) -> String {
    connector(
        a,
        json!({"kind": "mock", "label": format!("Fake {api}"), "config": {"base_url": url, "api": api, "location": location}}),
    )
    .await["id"]
    .as_str()
    .unwrap()
    .to_string()
}

async fn routing(a: &Client, p: &str, r: Value, residency: &str) -> (u16, Value) {
    a.put(
        &format!("/api/projects/{p}/routing"),
        json!({"routing": r, "data_residency": residency}),
    )
    .await
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

fn has_event(ev: &[(String, String)], kind: &str, needle: &str) -> bool {
    ev.iter().any(|(k, c)| k == kind && c.contains(needle))
}

async fn wait_for<F: Fn() -> Fut, Fut: std::future::Future<Output = bool>>(what: &str, f: F) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !f().await {
        assert!(tokio::time::Instant::now() < deadline, "timeout: {what}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// AK6: secrets are encrypted, write-only, and access is per owner/project.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn connector_crud_secrets_and_access(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let c = connector(
        &a,
        json!({"kind": "anthropic_api", "label": "Mein Key", "secret": "sk-ant-SECRET-42"}),
    )
    .await;
    let id = c["id"].as_str().unwrap().to_string();
    assert_eq!(c["has_secret"], true);
    assert_eq!(c["location"], "us");
    assert_eq!(
        (c["single_shot"].as_bool(), c["batch"].as_bool()),
        (Some(true), Some(true))
    );
    let (_, list) = a.get("/api/connectors").await;
    assert!(
        !list.to_string().contains("SECRET-42"),
        "secret never returned: {list}"
    );
    let enc: Vec<u8> = sqlx::query_scalar("SELECT secret_enc FROM connectors WHERE id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&enc).contains("SECRET-42"));
    assert_eq!(
        queued::crypto::decrypt(&h.st.cfg.master_key, &enc).unwrap(),
        b"sk-ant-SECRET-42"
    );

    // Validation (German messages).
    for (body, needle) in [
        (json!({"kind": "nope", "label": "x"}), "Unbekannt"),
        (
            json!({"kind": "anthropic_api", "label": "x"}),
            "Schlüssel fehlt",
        ),
        (
            json!({"kind": "bedrock", "label": "x", "secret": "t", "config": {"geo": "eu", "region": "us-east-1"}}),
            "EU",
        ),
        (
            json!({"kind": "vertex", "label": "x", "secret": "kein json", "config": {"project_id": "p"}}),
            "JSON",
        ),
        (
            json!({"kind": "infomaniak", "label": "x", "secret": "t", "config": {"base_url": "http://x.ch"}}),
            "https",
        ),
        (
            json!({"kind": "openai_api", "label": " ", "secret": "t"}),
            "Name",
        ),
    ] {
        let (code, e) = a.post("/api/connectors", body).await;
        assert_eq!(code, 400, "{e}");
        assert!(e.to_string().contains(needle), "{needle}: {e}");
    }

    // Others neither see nor change it.
    let (_, lb) = b.get("/api/connectors").await;
    assert_eq!(lb.as_array().unwrap().len(), 0);
    assert_eq!(
        b.patch(&format!("/api/connectors/{id}"), json!({"label": "x"}))
            .await
            .0,
        404
    );
    assert_eq!(b.delete(&format!("/api/connectors/{id}")).await.0, 404);
    assert_eq!(
        b.post(&format!("/api/connectors/{id}/check"), json!({}))
            .await
            .0,
        404
    );

    // Rename keeps the secret.
    let (code, c2) = a
        .patch(
            &format!("/api/connectors/{id}"),
            json!({"label": "Umbenannt"}),
        )
        .await;
    assert_eq!(
        (code, c2["label"].as_str(), c2["has_secret"].as_bool()),
        (200, Some("Umbenannt"), Some(true))
    );

    // Project connectors: only the owner creates them; editors may use them.
    let p = a.project(json!({"name": "Geteilt"})).await;
    a.post(
        &format!("/api/projects/{p}/members"),
        json!({"email": "b@x.ch", "role": "editor"}),
    )
    .await;
    let (code, _) = b
        .post(
            "/api/connectors",
            json!({"kind": "mock", "label": "B", "project_id": p}),
        )
        .await;
    assert_eq!(code, 403);
    let shared = connector(
        &a,
        json!({"kind": "mock", "label": "Projektzugang", "project_id": p}),
    )
    .await;
    let (_, lb) = b.get("/api/connectors").await;
    let seen = lb.as_array().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["can_manage"], false);
    // B can run a task with it in that project, not elsewhere.
    let t = b
        .task_with(
            &p,
            "use the shared access",
            json!({"connector_id": shared["id"]}),
        )
        .await;
    b.wait_status(&t, &["done"]).await;
    let other = b.project(json!({"name": "Andere"})).await;
    let (code, e) = b
        .post(
            "/api/tasks",
            json!({"project_id": other, "prompt": "x", "connector_id": shared["id"]}),
        )
        .await;
    assert_eq!(code, 400, "{e}");

    // Deleting an access with open tasks is refused.
    let busy = a
        .task_with(&p, "MOCK_SLOW wait", json!({"connector_id": shared["id"]}))
        .await;
    a.wait_status(&busy, &["running"]).await;
    let (code, e) = a
        .delete(&format!(
            "/api/connectors/{}",
            shared["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(code, 409, "{e}");
    a.post(&format!("/api/tasks/{busy}/cancel"), json!({}))
        .await;
    a.wait_status(&busy, &["cancelled"]).await;
    assert_eq!(
        a.delete(&format!(
            "/api/connectors/{}",
            shared["id"].as_str().unwrap()
        ))
        .await
        .0,
        200
    );
}

/// AK9: data residency is enforced on create, routing and run.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn residency_is_enforced_server_side(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let p = a.project(json!({"name": "EU"})).await;
    a.post(
        &format!("/api/projects/{p}/members"),
        json!({"email": "b@x.ch", "role": "editor"}),
    )
    .await;
    let us = connector(
        &a,
        json!({"kind": "anthropic_api", "label": "US", "secret": "k"}),
    )
    .await;
    let eu = connector(
        &a,
        json!({"kind": "mock", "label": "EU-Test", "config": {"location": "eu"}}),
    )
    .await;
    // Residency without routing (legacy providers).
    let (code, v) = routing(&a, &p, Value::Null, "eu").await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["data_residency"], "eu");
    let (code, e) = a
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "x", "provider": "claude"}),
        )
        .await;
    assert_eq!(code, 400);
    assert!(e.to_string().contains("Nur EU/Schweiz"), "{e}");
    let (code, e) = a
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "x", "connector_id": us["id"]}),
        )
        .await;
    assert_eq!(code, 400, "{e}");
    // EU access and the local mock are fine.
    let ok = a
        .task_with(&p, "eu work", json!({"connector_id": eu["id"]}))
        .await;
    a.wait_status(&ok, &["done"]).await;
    // Routing with a US step is refused.
    let r = json!({"execute": {"target": {"connector_id": eu["id"], "model": "claude-sonnet-5-5"}},
                   "review": {"mode": "harness", "target": {"connector_id": us["id"], "model": "claude-sonnet-5-5"}}});
    let (code, e) = routing(&a, &p, r, "eu").await;
    assert_eq!(code, 400);
    assert!(e.to_string().contains("USA"), "{e}");
    // The access moves to the US after the task was queued → blocked at run time.
    let queued = {
        let slow = a
            .task_with(
                &p,
                "MOCK_SLOW hold the project",
                json!({"connector_id": eu["id"]}),
            )
            .await;
        a.wait_status(&slow, &["running"]).await;
        let t = a
            .task_with(&p, "later", json!({"connector_id": eu["id"]}))
            .await;
        let (code, _) = a
            .patch(
                &format!("/api/connectors/{}", eu["id"].as_str().unwrap()),
                json!({"config": {"location": "us"}}),
            )
            .await;
        assert_eq!(code, 200);
        a.post(&format!("/api/tasks/{slow}/cancel"), json!({}))
            .await;
        t
    };
    let t = a.wait_status(&queued, &["blocked"]).await;
    assert_eq!(t["task"]["failure_class"], "config");
    assert!(
        t["task"]["error"]
            .as_str()
            .unwrap()
            .contains("Nur EU/Schweiz"),
        "{t}"
    );
    // Lowering the rule is an owner decision.
    let (code, _) = routing(&b, &p, Value::Null, "any").await;
    assert_eq!(code, 403);
    assert_eq!(routing(&a, &p, Value::Null, "any").await.0, 200);
}

/// AK4: costs per step from token counters × catalog, not CLI dollars.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn catalog_costs_are_recorded_per_step(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Kosten"})).await;
    // The mock emits 1200 input / 150 output / 800 cache-read tokens per run
    // (agent and review). Sonnet 5.5: $2 / $10 / $0.10 per MTok.
    let t = a
        .task_with(&p, "work", json!({"model": "claude-sonnet-5-5"}))
        .await;
    let t = a.wait_status(&t, &["done"]).await["task"].clone();
    let per_run = (1200.0 * 2.0 + 150.0 * 10.0 + 800.0 * 0.1) / 1e6;
    let cost = &t["cost"];
    let steps: Vec<&str> = cost["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap())
        .collect();
    assert_eq!(steps, ["execute", "review"], "{cost}");
    for s in cost["steps"].as_array().unwrap() {
        assert_eq!(s["source"], "catalog");
        assert_eq!(s["price_id"], "anthropic/claude-sonnet-5-5");
        approx(s["amount"].as_f64().unwrap(), per_run);
    }
    approx(cost["totals"][0]["amount"].as_f64().unwrap(), 2.0 * per_run);
    approx(t["usage"]["cost_usd"].as_f64().unwrap(), 2.0 * per_run);
    let rows: Vec<(String, String, String, f64, f64, Option<String>)> = sqlx::query_as(
        "SELECT step, cost_source, currency, cost_amount, cost_usd, model FROM usage_log WHERE task_id=$1 ORDER BY id",
    )
    .bind(Uuid::parse_str(t["id"].as_str().unwrap()).unwrap())
    .fetch_all(&h.st.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(
            (r.1.as_str(), r.2.as_str(), r.5.as_deref()),
            ("catalog", "USD", Some("claude-sonnet-5-5"))
        );
        approx(r.3, per_run);
        approx(r.4, per_run);
    }
    // Without a model the CLI's dollar value is the (marked) fallback.
    let t2 = a.task_with(&p, "MOCK_TOKENS no model", json!({})).await;
    let t2 = a.wait_status(&t2, &["done"]).await["task"].clone();
    let ex = &t2["cost"]["steps"][0];
    assert_eq!(ex["source"], "cli");
    approx(ex["amount"].as_f64().unwrap(), 0.012);
    assert!(ex["notes"][0].as_str().unwrap().contains("CLI"));
}

/// AK5: the live cost limit uses catalog prices (trips earlier than the CLI dollars would).
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn live_cost_limit_uses_catalog_prices(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Limit"})).await;
    // Opus 5.5: line 1 = $0.00796, line 2 = $0.0078 → $0.01576 > $0.0115.
    // With the CLI's $0.006 per line the limit would only trip on line 3.
    let t = a
        .task_with(
            &p,
            "MOCK_TOKENS",
            json!({"model": "claude-opus-5-5", "max_cost_usd": 0.0115}),
        )
        .await;
    let t = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(t["task"]["failure_class"], "limit_cost", "{t}");
    let ev: Vec<(String, String)> = t["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().into(),
                e["content"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(has_event(&ev, "guard", "$0.0158 / $0.0115"), "{ev:?}");
}

/// AK11: plan and review as Anthropic batches; the scheduler keeps working
/// (parallelism 1) while a task waits for its batch.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn plan_and_review_as_anthropic_batches(pool: PgPool) {
    let h = start(pool).await;
    let fake = start_fake().await;
    fake.state.polls_until_done.store(2, Ordering::SeqCst);
    fake.state.hold.store(true, Ordering::SeqCst);
    let a = h.user("a@x.ch").await;
    let fc = connector(
        &a,
        json!({"kind": "mock", "label": "Fake Anthropic", "secret": null,
               "config": {"base_url": fake.url, "api": "anthropic"}}),
    )
    .await;
    let p = a.project(json!({"name": "Batch", "goal": "Ship it"})).await;
    let r = json!({
        "preset": "balanced",
        "plan": {"mode": "batch", "target": {"connector_id": fc["id"], "model": "claude-opus-5-5"}},
        "execute": {"target": {"provider": "mock", "model": "claude-sonnet-5-5"}},
        "review": {"mode": "batch", "target": {"connector_id": fc["id"], "model": "claude-sonnet-5-5"}},
        "analyze": {"mode": "harness"}
    });
    let (code, v) = routing(&a, &p, r, "any").await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["routing"]["plan"]["mode"], "batch");
    let t = a.task(&p, "change something").await;
    a.wait_status(&t, &["waiting_batch"]).await;
    // Another project runs while the batch is pending (parallelism 1).
    let other = a.project(json!({"name": "Nebenher"})).await;
    let o = a.task(&other, "independent").await;
    a.wait_status(&o, &["done"]).await;
    assert_eq!(a.task_json(&t).await["task"]["status"], "waiting_batch");
    fake.state.hold.store(false, Ordering::SeqCst);
    let done = a.wait_status(&t, &["done"]).await;
    let task = &done["task"];
    assert!(
        task["plan"].as_str().unwrap().contains("FAKE PLAN"),
        "{task}"
    );
    assert_eq!(task["review"]["approved"], true);
    let ev: Vec<(String, String)> = done["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().into(),
                e["content"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(
        has_event(&ev, "batch", "Planung: claude-opus-5-5"),
        "{ev:?}"
    );
    assert!(has_event(&ev, "plan", "Plan erstellt"));
    assert!(has_event(&ev, "batch", "Review: claude-sonnet-5-5"));
    assert!(has_event(&ev, "review", "Review: OK"));
    // Wire: batch submission with a 1-hour cache on the stable prefix, never the
    // results URL of another host (the fake points to evil.example).
    let reqs = fake.requests();
    let sub: Vec<_> = reqs
        .iter()
        .filter(|r| r.path == "/v1/messages/batches")
        .collect();
    assert_eq!(sub.len(), 2, "{:?}", fake.paths());
    let body: Value = serde_json::from_str(&sub[0].body).unwrap();
    let params = &body["requests"][0]["params"];
    assert_eq!(
        params["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert!(params["system"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Ship it"));
    assert!(
        !params["system"][0]["text"].as_str().unwrap().contains(&t),
        "no task id in the cacheable prefix"
    );
    assert!(reqs.iter().any(|r| r.path.ends_with("/results")));
    assert!(!fake.paths().iter().any(|p| p.contains("evil")));
    // Costs: plan and review at batch price (−50 %, 1 h cache writes).
    let steps = task["cost"]["steps"].as_array().unwrap();
    let plan = steps.iter().find(|s| s["step"] == "plan").unwrap();
    // Opus batch: (1000 × 4 + 500 × 8 (1 h write) + 200 × 20) / 1e6 × 0.5
    approx(
        plan["amount"].as_f64().unwrap(),
        (1000.0 * 4.0 + 500.0 * 8.0 + 200.0 * 20.0) / 1e6 * 0.5,
    );
    assert!(
        steps.iter().any(|s| s["step"] == "execute") && steps.iter().any(|s| s["step"] == "review")
    );
}

/// AK11/AK12: review via OpenAI batch rejects → fix task escalates to the next model.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn openai_batch_review_rejection_escalates(pool: PgPool) {
    let h = start(pool).await;
    let fake = start_fake().await;
    fake.state.review_approved.store(false, Ordering::SeqCst);
    let a = h.user("a@x.ch").await;
    a.settings(json!({"max_fix_attempts": 1})).await;
    let fc = fake_connector(&a, &fake.url, "openai", "local").await;
    let p = a.project(json!({"name": "Eskalation"})).await;
    let r = json!({
        "execute": {"target": {"provider": "mock", "model": "claude-haiku-5-5"}, "escalation": ["claude-sonnet-5-5", "claude-opus-5-5"]},
        "review": {"mode": "batch", "target": {"connector_id": fc, "model": "gpt-6.1-sol"}},
    });
    assert_eq!(routing(&a, &p, r, "any").await.0, 200);
    let t = a.task(&p, "do it").await;
    let failed = a.wait_status(&t, &["failed"]).await;
    assert!(
        failed["task"]["error"]
            .as_str()
            .unwrap()
            .contains("missing test"),
        "{failed}"
    );
    let ts = a
        .wait_until("fix", |ts| {
            ts.iter()
                .any(|x| x["kind"] == "fix" && x["status"] == "blocked")
        })
        .await;
    let fix = ts.iter().find(|x| x["kind"] == "fix").unwrap();
    assert_eq!(
        (fix["model"].as_str(), fix["escalation_level"].as_i64()),
        (Some("claude-sonnet-5-5"), Some(1))
    );
    // OpenAI batch wire: file upload with purpose=batch, then /v1/batches.
    let paths = fake.paths();
    assert!(paths.contains(&"POST /v1/files".to_string()), "{paths:?}");
    assert!(paths.contains(&"POST /v1/batches".to_string()));
    assert!(paths
        .iter()
        .any(|p| p.starts_with("GET /v1/files/") && p.ends_with("/content")));
    let up = fake
        .requests()
        .into_iter()
        .find(|r| r.path == "/v1/files")
        .unwrap();
    assert!(
        up.body.contains("\"url\":\"/v1/chat/completions\"") && up.body.contains("gpt-6.1-sol")
    );
}

/// AK11: OnEmpty analysis as a Mistral batch creates the proposed follow-ups.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn onempty_analysis_as_mistral_batch(pool: PgPool) {
    let h = start(pool).await;
    let fake = start_fake().await;
    let a = h.user("a@x.ch").await;
    let fc = fake_connector(&a, &fake.url, "mistral", "eu").await;
    let p = a.project(json!({"name": "Leerlauf"})).await;
    let r = json!({
        "execute": {"target": {"provider": "mock", "model": "claude-sonnet-5-5"}},
        "review": {"mode": "harness"},
        "analyze": {"mode": "batch", "target": {"connector_id": fc, "model": "mistral-large-4"}},
    });
    assert_eq!(routing(&a, &p, r, "eu").await.0, 200);
    let t = a.task(&p, "first").await;
    a.wait_status(&t, &["done"]).await;
    a.settings(
        json!({"onempty_enabled": true, "onempty_cooldown_minutes": 5, "max_auto_tasks": 0}),
    )
    .await;
    let ts = a
        .wait_until("follow-ups", |ts| {
            ts.iter()
                .filter(|x| x["title"] == "Fake A" || x["title"] == "Fake B")
                .count()
                == 2
        })
        .await;
    let onempty = ts.iter().find(|x| x["kind"] == "onempty").unwrap();
    assert_eq!(onempty["status"], "done");
    assert!(ts
        .iter()
        .filter(|x| x["title"].as_str().unwrap().starts_with("Fake"))
        .all(|x| x["status"] == "pending_approval"));
    let paths = fake.paths();
    assert!(
        paths.contains(&"POST /v1/batch/jobs".to_string()),
        "{paths:?}"
    );
    let job = fake
        .requests()
        .into_iter()
        .find(|r| r.path == "/v1/batch/jobs")
        .unwrap();
    assert!(job.body.contains("mistral-large-4"));
    let rows: Vec<(String, String, f64)> =
        sqlx::query_as("SELECT step, currency, cost_amount FROM usage_log WHERE task_id=$1")
            .bind(Uuid::parse_str(onempty["id"].as_str().unwrap()).unwrap())
            .fetch_all(&h.st.db)
            .await
            .unwrap();
    // Mistral Large 4 batch, EU endpoint +10 %: (900 × 0.7 + 100 × 0.07 + 200 × 2.1)/1e6 × 0.5 × 1.1
    // (mock connectors carry no regional surcharge – only real Mistral connectors do)
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "analyze");
    approx(
        rows[0].2,
        (900.0 * 0.7 + 100.0 * 0.07 + 200.0 * 2.1) / 1e6 * 0.5,
    );
}

/// Sync calls: transient errors are retried, auth errors pause the access,
/// and the key never shows up in events (redacted).
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn sync_calls_retry_classify_and_redact(pool: PgPool) {
    let h = start(pool).await;
    let fake = start_fake().await;
    fake.state.fail_next.lock().unwrap().extend([429, 503]);
    let a = h.user("a@x.ch").await;
    let good = connector(&a, json!({"kind": "mock", "label": "Gut", "secret": "good-key-1", "config": {"base_url": fake.url, "api": "openai"}})).await;
    let p = a.project(json!({"name": "Sync"})).await;
    let r = json!({
        "plan": {"mode": "sync", "target": {"connector_id": good["id"], "model": "gpt-6.1-sol"}},
        "execute": {"target": {"provider": "mock"}},
        "review": {"mode": "harness"},
    });
    assert_eq!(routing(&a, &p, r, "any").await.0, 200);
    let t = a.task(&p, "plan me").await;
    let done = a.wait_status(&t, &["done"]).await;
    assert!(done["task"]["plan"].as_str().unwrap().contains("FAKE PLAN"));
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM batch_jobs WHERE task_id=$1")
        .bind(Uuid::parse_str(&t).unwrap())
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(attempts, 2, "429 and 503 were retried");
    let chat = fake
        .requests()
        .into_iter()
        .rfind(|r| r.path == "/v1/chat/completions")
        .unwrap();
    assert_eq!(chat.auth, "Bearer good-key-1");

    // Rejected key: plan skipped, access paused, key redacted.
    let bad = connector(&a, json!({"kind": "mock", "label": "Kaputt", "secret": "bad-key-999", "config": {"base_url": fake.url, "api": "anthropic"}})).await;
    let p2 = a.project(json!({"name": "Sync2"})).await;
    let r = json!({
        "plan": {"mode": "sync", "target": {"connector_id": bad["id"], "model": "claude-opus-5-5"}},
        "execute": {"target": {"provider": "mock"}},
        "review": {"mode": "harness"},
    });
    assert_eq!(routing(&a, &p2, r, "any").await.0, 200);
    let t2 = a.task(&p2, "plan me too").await;
    let d2 = a.wait_status(&t2, &["done"]).await;
    assert_eq!(d2["task"]["plan"], "");
    assert!(!d2.to_string().contains("bad-key-999"), "key leaked: {d2}");
    assert!(d2["events"].to_string().contains("***"), "{}", d2["events"]);
    let (_, list) = a.get("/api/connectors").await;
    let b = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["label"] == "Kaputt")
        .unwrap();
    assert!(b["paused_reason"].as_str().is_some(), "{b}");
    let (code, b2) = a
        .post(
            &format!("/api/connectors/{}/resume", bad["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!((code, b2["paused_reason"].is_null()), (200, true));
    // Connection check.
    let (_, ok) = a
        .post(
            &format!("/api/connectors/{}/check", good["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(ok["ok"], true, "{ok}");
    let (_, no) = a
        .post(
            &format!("/api/connectors/{}/check", bad["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(no["ok"], false);
    assert!(!no.to_string().contains("bad-key-999"));
}

/// Cancel while waiting for a batch; a pending job survives a restart (lease reset).
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn batch_cancel_and_restart(pool: PgPool) {
    let h = start(pool).await;
    let fake = start_fake().await;
    fake.state.hold.store(true, Ordering::SeqCst);
    let a = h.user("a@x.ch").await;
    let fc = fake_connector(&a, &fake.url, "anthropic", "local").await;
    let p = a.project(json!({"name": "Abbruch"})).await;
    let r = json!({
        "plan": {"mode": "batch", "target": {"connector_id": fc, "model": "claude-opus-5-5"}},
        "execute": {"target": {"provider": "mock"}},
        "review": {"mode": "harness"},
    });
    assert_eq!(routing(&a, &p, r, "any").await.0, 200);
    let t = a.task(&p, "cancel me").await;
    a.wait_status(&t, &["waiting_batch"]).await;
    wait_for("submitted", || async {
        fake.paths()
            .iter()
            .any(|p| p == "POST /v1/messages/batches")
    })
    .await;
    assert_eq!(
        a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await.0,
        200
    );
    a.wait_status(&t, &["cancelled"]).await;
    let st: String = sqlx::query_scalar("SELECT status FROM batch_jobs WHERE task_id=$1")
        .bind(Uuid::parse_str(&t).unwrap())
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(st, "cancelled");
    wait_for("remote cancel", || async {
        fake.paths().iter().any(|p| p.ends_with("/cancel"))
    })
    .await;

    // Restart: a leased job (the process died mid-call) is picked up after recover().
    let t2 = a.task(&p, "after restart").await;
    a.wait_status(&t2, &["waiting_batch"]).await;
    wait_for("second submission", || async {
        fake.paths()
            .iter()
            .filter(|p| *p == "POST /v1/messages/batches")
            .count()
            == 2
    })
    .await;
    sqlx::query("UPDATE batch_jobs SET next_poll_at = now() + interval '1 hour' WHERE task_id=$1")
        .bind(Uuid::parse_str(&t2).unwrap())
        .execute(&h.st.db)
        .await
        .unwrap();
    fake.state.hold.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        a.task_json(&t2).await["task"]["status"],
        "waiting_batch",
        "lease respected"
    );
    queued::batch::recover(&h.st).await.unwrap();
    h.st.batch_wake.notify_one();
    a.wait_status(&t2, &["done"]).await;
}

/// A task left in waiting_batch without an open job is blocked, not stuck forever.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn orphaned_waiting_batch_task_is_blocked(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Waise"})).await;
    let t = a.task(&p, "MOCK_SLOW hold").await;
    a.wait_status(&t, &["running"]).await;
    a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await;
    a.wait_status(&t, &["cancelled"]).await;
    let id = Uuid::parse_str(&t).unwrap();
    // Simulate: the job finished long ago, the task never continued.
    sqlx::query("UPDATE tasks SET status='waiting_batch', updated_at = now() - interval '1 hour' WHERE id=$1")
        .bind(id)
        .execute(&h.st.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO batch_jobs (id, task_id, user_id, step, mode, api, model, status, request, finished_at)
         SELECT $1, id, user_id, 'plan', 'sync', 'anthropic', 'm', 'succeeded', '{}', now() - interval '30 minutes' FROM tasks WHERE id=$2",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .execute(&h.st.db)
    .await
    .unwrap();
    let b = a.wait_status(&t, &["blocked"]).await;
    assert_eq!(b["task"]["failure_class"], "infra");
    assert!(b["task"]["error"]
        .as_str()
        .unwrap()
        .contains("erneut starten"));
}

/// AK10: presets resolved against the accesses; routing validation via API.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn presets_and_routing_validation(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = a.project(json!({"name": "Presets"})).await;
    let (code, ps) = a.get(&format!("/api/projects/{p}/presets")).await;
    assert_eq!(code, 200);
    let ps = ps.as_array().unwrap();
    let ids: Vec<&str> = ps.iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["cheap", "balanced", "best", "eu", "cheapest"]);
    // Only the legacy mock provider: Claude presets work without batch, EU too (local), OpenAI not.
    let bal = ps.iter().find(|x| x["id"] == "balanced").unwrap();
    assert_eq!(bal["available"], true, "{bal}");
    assert_eq!(bal["routing"]["plan"]["mode"], "off");
    let cheapest = ps.iter().find(|x| x["id"] == "cheapest").unwrap();
    assert_eq!(cheapest["available"], false);
    assert!(cheapest["missing"].as_str().unwrap().contains("OpenAI"));
    // Apply a preset as returned.
    let (code, v) = routing(&a, &p, bal["routing"].clone(), "any").await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["routing"]["preset"], "balanced");
    // Review on the cheapest tier is refused.
    let bad = json!({"execute": {"target": {"provider": "mock", "model": "claude-sonnet-5-5"}},
                     "review": {"mode": "harness", "target": {"provider": "mock", "model": "gpt-6-luna"}}});
    let (code, e) = routing(&a, &p, bad, "any").await;
    assert_eq!(code, 400);
    assert!(e.to_string().contains("günstigsten Stufe"), "{e}");
    // Batch with a legacy provider is impossible.
    let bad = json!({"execute": {"target": {"provider": "mock"}}, "plan": {"mode": "batch"}});
    assert_eq!(routing(&a, &p, bad, "any").await.0, 400);
    // A task without an explicit access uses the routing's agent loop and model.
    let t = a.task(&p, "routed").await;
    let tj = a.wait_status(&t, &["done"]).await;
    assert_eq!(tj["task"]["model"], "claude-sonnet-5-5");
    // Back to legacy.
    let (code, v) = routing(&a, &p, Value::Null, "any").await;
    assert_eq!((code, v["routing"].is_null()), (200, true));
}

/// AK13 + cost summary: estimate before submitting; costs per project/model/step.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn estimate_and_cost_summary(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let v = h.user("v@x.ch").await;
    let outsider = h.user("o@x.ch").await;
    let p = a.project(json!({"name": "Schätzung"})).await;
    a.post(
        &format!("/api/projects/{p}/members"),
        json!({"email": "v@x.ch", "role": "viewer"}),
    )
    .await;
    let (code, e) = a
        .post(
            "/api/estimate",
            json!({"project_id": p, "model": "claude-sonnet-5-5"}),
        )
        .await;
    assert_eq!(code, 200, "{e}");
    let ex = e["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == "execute")
        .unwrap();
    approx(ex["mid"].as_f64().unwrap(), 1.245);
    assert!(ex["basis"].as_str().unwrap().contains("Annahme"));
    assert_eq!(
        e["steps"].as_array().unwrap().len(),
        2,
        "execute + review: {e}"
    );
    assert_eq!(e["location"], "Lokal");
    // Unknown model → no price, explained.
    let (_, e) = a
        .post(
            "/api/estimate",
            json!({"project_id": p, "model": "my-llama"}),
        )
        .await;
    assert!(e["notes"].to_string().contains("kein Preis"), "{e}");
    // History: three finished tasks → the user's own token profile.
    for i in 0..3 {
        let t = a
            .task_with(
                &p,
                &format!("run {i}"),
                json!({"model": "claude-sonnet-5-5"}),
            )
            .await;
        a.wait_status(&t, &["done"]).await;
    }
    let (_, e) = a
        .post(
            "/api/estimate",
            json!({"project_id": p, "model": "claude-sonnet-5-5"}),
        )
        .await;
    let ex = e["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == "execute")
        .unwrap();
    assert!(ex["basis"].as_str().unwrap().contains("n=3"), "{ex}");
    approx(
        ex["mid"].as_f64().unwrap(),
        (1200.0 * 2.0 + 150.0 * 10.0 + 800.0 * 0.1) / 1e6,
    );
    // Viewers may not estimate (it is part of creating a task).
    assert_eq!(
        v.post("/api/estimate", json!({"project_id": p})).await.0,
        403
    );

    let (code, c) = a.get("/api/costs?days=30").await;
    assert_eq!(code, 200);
    let per_run = (1200.0 * 2.0 + 150.0 * 10.0 + 800.0 * 0.1) / 1e6;
    approx(c["totals"][0]["amount"].as_f64().unwrap(), 6.0 * per_run);
    assert_eq!(c["by_project"][0]["label"], "Schätzung");
    assert_eq!(c["by_model"][0]["key"], "claude-sonnet-5-5");
    let steps: Vec<&str> = c["by_step"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert!(steps.contains(&"Umsetzung") && steps.contains(&"Review"));
    // Project view for members, 404 for others.
    let (code, pc) = v.get(&format!("/api/costs?project_id={p}")).await;
    assert_eq!(code, 200);
    approx(pc["totals"][0]["amount"].as_f64().unwrap(), 6.0 * per_run);
    assert_eq!(
        outsider.get(&format!("/api/costs?project_id={p}")).await.0,
        404
    );
}

/// AK3: admin overrides are versioned and change the next run's price.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn price_overrides_are_admin_only_and_versioned(pool: PgPool) {
    let h = start(pool).await;
    let admin = h.user("admin@x.ch").await; // first user = admin
    let b = h.user("b@x.ch").await;
    let (_, cat) = b.get("/api/prices").await;
    assert_eq!(cat["version"], "2026-11-07");
    let mut sonnet = cat["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "anthropic/claude-sonnet-5-5")
        .unwrap()
        .clone();
    sonnet["input"] = json!(1.0);
    let body = json!({"entries": [sonnet], "note": "Vertragspreis"});
    assert_eq!(b.put("/api/admin/prices", body.clone()).await.0, 403);
    let (code, c) = admin.put("/api/admin/prices", body).await;
    assert_eq!(code, 200, "{c}");
    assert_eq!(c["override_version"], 1);
    let e = c["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "anthropic/claude-sonnet-5-5")
        .unwrap();
    assert_eq!(
        (e["input"].as_f64(), e["evidence"].as_str()),
        (Some(1.0), Some("admin"))
    );
    let (code, e) = admin.put("/api/admin/prices", json!({"entries": [{"id": "x", "label": "x", "family": "x", "models": ["m"], "tier": "mid", "currency": "EUR", "input": 1, "output": 1, "source": "s", "evidence": "admin"}]})).await;
    assert_eq!(code, 400, "{e}");
    let (_, hist) = admin.get("/api/admin/prices/history").await;
    assert_eq!(hist[0]["version"], 1);
    assert_eq!(hist[0]["note"], "Vertragspreis");
    assert_eq!(b.get("/api/admin/prices/history").await.0, 403);
    // The next run uses the override (input $1 instead of $2).
    let p = b.project(json!({"name": "Preis"})).await;
    b.patch(
        &format!("/api/projects/{p}"),
        json!({"review_enabled": false}),
    )
    .await;
    let t = b
        .task_with(&p, "x", json!({"model": "claude-sonnet-5-5"}))
        .await;
    let t = b.wait_status(&t, &["done"]).await;
    approx(
        t["task"]["cost"]["totals"][0]["amount"].as_f64().unwrap(),
        (1200.0 * 1.0 + 150.0 * 10.0 + 800.0 * 0.1) / 1e6,
    );
}

/// AK14: LiteLLM virtual key per run with the remaining cost limit as budget.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn litellm_task_key_is_issued_and_revoked(_pool: PgPool) {
    let fake = start_fake().await;
    let lan = queued::llm::Guard {
        allow_private: true,
        deny: vec![],
    };
    let task = Uuid::new_v4();
    let key = queued::gateway::issue(&fake.url, "master-1", task, Some(0.123456), &lan)
        .await
        .unwrap();
    assert!(key.starts_with("sk-task-"));
    queued::gateway::revoke(&fake.url, "master-1", &key, &lan).await;
    let reqs = fake.requests();
    let gen = reqs.iter().find(|r| r.path == "/key/generate").unwrap();
    assert_eq!(gen.auth, "Bearer master-1");
    let body: Value = serde_json::from_str(&gen.body).unwrap();
    assert_eq!(body["max_budget"], json!(0.1234));
    assert_eq!(body["metadata"]["queue_task"], task.to_string());
    let del = reqs.iter().find(|r| r.path == "/key/delete").unwrap();
    assert!(del.body.contains(&key));
    // A rejected master key is an error (the run is re-queued as infra).
    let e = queued::gateway::issue(&fake.url, "bad-key-7", task, None, &lan)
        .await
        .unwrap_err();
    assert!(!e.to_string().contains("bad-key-7"), "{e}");
    // Without the LAN switch the app never calls loopback/private targets (SSRF guard).
    let before = fake.requests().len();
    let e = queued::gateway::issue(
        &fake.url,
        "master-1",
        task,
        None,
        &queued::llm::Guard::default(),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("nicht erlaubt"), "{e}");
    assert_eq!(
        fake.requests().len(),
        before,
        "the guarded call never reached the server"
    );
}

/// AK15: CLI status through the runner probe.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cli_status_reports_every_probed_cli(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let (code, s) = a.get("/api/cli-status").await;
    assert_eq!(code, 200, "{s}");
    let clis: Vec<&str> = s
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["cli"].as_str().unwrap())
        .collect();
    assert_eq!(clis, ["claude", "codex", "gemini", "opencode", "agy"]);
    for x in s.as_array().unwrap() {
        let st = x["state"].as_str().unwrap();
        assert!(
            [
                "ok",
                "missing",
                "version_mismatch",
                "frozen",
                "replaced",
                "unexpected"
            ]
            .contains(&st),
            "{x}"
        );
        match x["installed"].as_str() {
            None => assert_eq!(st, "missing"),
            Some(v) if x["cli"] == "claude" && v.contains("2.1.319") => assert_eq!(st, "ok"),
            Some(_) => {}
        }
    }
    let (_, meta) = h.anon().get("/api/meta").await;
    assert_eq!(meta["cli_notes"]["gemini"]["state"], "frozen");
    assert!(meta["connector_types"].as_array().unwrap().len() >= 15);
    assert_eq!(meta["price_catalog_version"], "2026-11-07");
}

/// AK16: the eval set is reproducible in mock mode (2 runs → identical
/// results) and the simulated strength changes the success rate.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn eval_set_is_reproducible_in_mock_mode(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"parallelism": 4})).await;
    let (_, cases) = a.get("/api/evals/cases").await;
    assert_eq!(cases.as_array().unwrap().len(), 8);
    let mut outcomes = vec![];
    for model in ["mock-medium", "mock-medium", "mock-weak"] {
        let (code, run) = a
            .post("/api/evals", json!({"provider": "mock", "model": model}))
            .await;
        assert_eq!(code, 200, "{run}");
        let id = run["id"].as_str().unwrap().to_string();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        let run = loop {
            let (_, r) = a.get(&format!("/api/evals/{id}")).await;
            if r["status"] == "finished" {
                break r;
            }
            assert!(tokio::time::Instant::now() < deadline, "eval stuck: {r}");
            tokio::time::sleep(Duration::from_millis(300)).await;
        };
        let ok: Vec<String> = run["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["success"] == true)
            .map(|r| r["case_id"].as_str().unwrap().to_string())
            .collect();
        outcomes.push((model, ok, run["summary"].clone(), id));
    }
    assert_eq!(outcomes[0].1, outcomes[1].1, "same model, same result");
    assert_eq!(
        outcomes[0].1.len(),
        7,
        "medium solves S and M: {:?}",
        outcomes[0]
    );
    assert_eq!(outcomes[2].1.len(), 3, "weak solves S only");
    assert_eq!(outcomes[0].2["success_rate"].as_f64(), Some(7.0 / 8.0));
    assert_eq!(outcomes[2].2["finished"], 8);
    // No fix tasks, eval projects stay hidden, delete cleans up.
    assert!(a.tasks().await.iter().all(|t| t["kind"] != "fix"));
    let (_, projects) = a.get("/api/projects").await;
    assert!(projects.as_array().unwrap().is_empty(), "{projects}");
    let (code, _) = a.delete(&format!("/api/evals/{}", outcomes[2].3)).await;
    assert_eq!(code, 200);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE is_eval")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(left, 16);
    let _ = user_id(&h, "a@x.ch").await;
}
