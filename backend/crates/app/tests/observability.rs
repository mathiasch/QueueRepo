//! WP10 integration: health/readiness, Prometheus metrics (protection and
//! format) and OTLP traces (GenAI semantics, never content) – app and
//! runner in-process, real Postgres, a fake OTLP collector on loopback.
mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use common::{start, start_with};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyValue;
use serde_json::{json, Value};
use sqlx::PgPool;

const METRICS_TOKEN: &str = "metrics-token-abcdefghijklmnopqrstuvwxyz";

async fn raw(url: &str, bearer: Option<&str>) -> (u16, String, String) {
    let mut r = reqwest::Client::new().get(url);
    if let Some(b) = bearer {
        r = r.bearer_auth(b);
    }
    let resp = r.send().await.unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    (resp.status().as_u16(), ct, resp.text().await.unwrap())
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn healthz_and_readyz_report_failures_without_internals(pool: PgPool) {
    let h = start(pool.clone()).await;
    let (code, _, body) = raw(&format!("{}/healthz", h.base), None).await;
    assert_eq!(code, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["status"],
        "ok"
    );
    let (code, _, body) = raw(&format!("{}/readyz", h.base), None).await;
    assert_eq!(code, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v,
        json!({"status": "ready", "checks": {"database": "ok", "runner": "ok"}})
    );

    // Runner down (nothing listens on port 9).
    let down = start_with(pool.clone(), |c| c.runner_url = "http://127.0.0.1:9".into()).await;
    let t = std::time::Instant::now();
    let (code, _, body) = raw(&format!("{}/readyz", down.base), None).await;
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "bounded by the check timeout"
    );
    assert_eq!(code, 503, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["checks"], json!({"database": "ok", "runner": "fail"}));
    assert_eq!(v["status"], "not_ready");
    for leak in ["127.0.0.1", ":9", "error", "connect", "refused"] {
        assert!(!body.contains(leak), "no internals in {body}");
    }
    // Liveness does not depend on the runner.
    assert_eq!(raw(&format!("{}/healthz", down.base), None).await.0, 200);

    // Runner reachable, but the app holds a wrong token: not ready either.
    let wrong = start_with(pool.clone(), |c| c.runner_token = "wrong-token".into()).await;
    let (code, _, body) = raw(&format!("{}/readyz", wrong.base), None).await;
    assert_eq!(code, 503);
    assert!(body.contains("\"runner\":\"fail\""), "{body}");

    // Database gone: readiness fails (after the 1 s cache), liveness stays.
    h.st.db.close().await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (code, _, body) = raw(&format!("{}/readyz", h.base), None).await;
    assert_eq!(code, 503, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["checks"], json!({"database": "fail", "runner": "ok"}));
    assert!(!body.contains("closed") && !body.contains("pool"), "{body}");
    assert_eq!(raw(&format!("{}/healthz", h.base), None).await.0, 200);
}

/// Readiness is cached for a second: a burst of probes costs one check.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn readyz_is_cached_against_floods(pool: PgPool) {
    let h = start(pool).await;
    let before = h.st.metrics.http.get(&["GET", "/readyz", "2xx"]);
    let url = format!("{}/readyz", h.base);
    let calls = (0..40).map(|_| raw(&url, None));
    for (code, _, _) in futures::future::join_all(calls).await {
        assert_eq!(code, 200);
    }
    assert_eq!(
        h.st.metrics.http.get(&["GET", "/readyz", "2xx"]) - before,
        40.0
    );
    // One check fed the runner gauge; the runner saw at most a couple of info calls.
    let runner_calls = h.st.metrics.registry.render();
    assert!(
        runner_calls.contains("queued_runner_up 1"),
        "{runner_calls}"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn metrics_are_off_by_default_and_never_the_spa(pool: PgPool) {
    let h = start(pool).await;
    let (code, _, body) = raw(&format!("{}/metrics", h.base), Some(METRICS_TOKEN)).await;
    assert_eq!(code, 404);
    assert!(!body.contains("<html"), "not the SPA: {body}");
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn metrics_need_their_token_and_carry_no_ids_or_content(pool: PgPool) {
    let h = start_with(pool, |c| {
        c.metrics =
            queued_common::metrics::Access::parse(Some(METRICS_TOKEN.into()), None, "", None)
                .unwrap();
    })
    .await;
    let url = format!("{}/metrics", h.base);
    assert_eq!(raw(&url, None).await.0, 401);
    assert_eq!(raw(&url, Some("wrong")).await.0, 401);
    // A logged-in session or an API token is no metrics token.
    let a = h.user("metrics-a@x.ch").await;
    assert_eq!(a.get("/metrics").await.0, 401);
    let tok = a.token(&["tasks:read", "projects:read"]).await;
    assert_eq!(raw(&url, Some(&tok)).await.0, 401);
    // The metrics token opens nothing else.
    assert_eq!(h.bearer(METRICS_TOKEN).get("/api/tasks").await.0, 401);

    let p = a.project(json!({"name": "PROJECT-SECRET-NAME"})).await;
    let id = a
        .task_with(&p, "do it", json!({"title": "TITLE-SECRET-4711"}))
        .await;
    a.wait_status(&id, &["done"]).await;
    a.get(&format!("/api/tasks/{id}")).await;

    let (code, ct, text) = raw(&url, Some(METRICS_TOKEN)).await;
    assert_eq!(code, 200);
    assert!(ct.starts_with("text/plain; version=0.0.4"), "{ct}");
    queued_common::metrics::validate_exposition(&text).unwrap();
    for want in [
        "queued_build_info{version=",
        "queued_agent_runs_total{provider=\"mock\",kind=\"agent\",outcome=\"success\"} 1",
        "queued_tokens_total{provider=\"mock\",model=\"unknown\",type=\"input\"}",
        "queued_tokens_total{provider=\"mock\",model=\"unknown\",type=\"cache_read\"}",
        "queued_agent_run_duration_seconds_count{kind=\"agent\"} 1",
        "queued_tasks{status=\"queued\"} 0",
        "queued_tasks{status=\"merging\"} 0",
        "queued_http_requests_total{method=\"GET\",route=\"/api/tasks/{id}\",status=\"2xx\"}",
        "queued_database_up 1",
    ] {
        assert!(text.contains(want), "missing {want}:\n{text}");
    }
    let me = a.get("/api/auth/me").await.1;
    for leak in [
        id.as_str(),
        p.as_str(),
        me["id"].as_str().unwrap(),
        "metrics-a@x.ch",
        "TITLE-SECRET",
        "PROJECT-SECRET",
        "do it",
    ] {
        assert!(!text.contains(leak), "{leak} leaked into /metrics");
    }
}

/// `METRICS_BIND`: a loopback listener of its own; the main listener has
/// no `/metrics` then.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn metrics_on_their_own_listener(pool: PgPool) {
    let h = start_with(pool, |c| {
        c.metrics =
            queued_common::metrics::Access::parse(None, Some("127.0.0.1:0".into()), "", None)
                .unwrap();
    })
    .await;
    assert_eq!(raw(&format!("{}/metrics", h.base), None).await.0, 404);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let r = queued::metrics::router(h.st.clone());
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    let (code, _, text) = raw(&format!("http://{addr}/metrics"), None).await;
    assert_eq!(code, 200);
    queued_common::metrics::validate_exposition(&text).unwrap();
}

/// Fake OTLP/HTTP collector: stores every request body.
async fn collector() -> (String, Arc<Mutex<Vec<(String, String)>>>) {
    use axum::{body::Bytes, http::HeaderMap, routing::post, Router};
    let seen: Arc<Mutex<Vec<(String, String)>>> = Default::default();
    let s2 = seen.clone();
    let app = Router::new().route(
        "/v1/traces",
        post(move |headers: HeaderMap, body: Bytes| {
            let s = s2.clone();
            async move {
                let ct = headers
                    .get("content-type")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default();
                let auth = headers
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default();
                assert_eq!(ct, "application/json");
                s.lock()
                    .unwrap()
                    .push((auth, String::from_utf8(body.to_vec()).unwrap()));
                "{}"
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, seen)
}

type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// Flushes until a span named `name` arrived (the root span ends after the
/// task's status changed); returns all spans and raw bodies.
async fn spans_until(
    h: &common::Harness,
    seen: &Seen,
    name: &str,
) -> (
    Vec<opentelemetry_proto::tonic::trace::v1::Span>,
    Vec<(String, String)>,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        h.st.otel.as_ref().unwrap().flush().await;
        let bodies = seen.lock().unwrap().clone();
        let mut spans = vec![];
        for (_, b) in &bodies {
            let req: ExportTraceServiceRequest = serde_json::from_str(b).expect("valid OTLP JSON");
            for rs in req.resource_spans {
                for ss in rs.scope_spans {
                    spans.extend(ss.spans);
                }
            }
        }
        if spans.iter().any(|s| s.name == name) {
            return (spans, bodies);
        }
        assert!(std::time::Instant::now() < deadline, "no span {name}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn attr<'a>(
    attrs: &'a [opentelemetry_proto::tonic::common::v1::KeyValue],
    k: &str,
) -> Option<&'a AnyValue> {
    attrs
        .iter()
        .find(|a| a.key == k)
        .and_then(|a| a.value.as_ref())
        .and_then(|v| v.value.as_ref())
}

fn s_attr(attrs: &[opentelemetry_proto::tonic::common::v1::KeyValue], k: &str) -> String {
    match attr(attrs, k) {
        Some(AnyValue::StringValue(s)) => s.clone(),
        other => panic!("{k}: {other:?}"),
    }
}

fn i_attr(attrs: &[opentelemetry_proto::tonic::common::v1::KeyValue], k: &str) -> i64 {
    match attr(attrs, k) {
        Some(AnyValue::IntValue(i)) => *i,
        other => panic!("{k}: {other:?}"),
    }
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn spans_follow_genai_conventions_and_never_carry_content(pool: PgPool) {
    let (url, seen) = collector().await;
    let h = start_with(pool, |c| {
        c.otel = queued::otel::OtelConfig::from_env(&|k: &str| match k {
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(url.clone()),
            "OTEL_EXPORTER_OTLP_HEADERS" => Some("authorization=Bearer collector-key".into()),
            "OTEL_SERVICE_NAME" => Some("queued-test".into()),
            _ => None,
        })
        .unwrap();
    })
    .await;
    let a = h.user("otel-a@x.ch").await;
    let p = a.project(json!({"name": "PROJECT-SECRET-NAME"})).await;
    // Prompt with a marker, a fake token printed by the agent, a tool call
    // with an argument (the mock edits MOCK_LOG.md), title with a marker.
    let id = a
        .task_with(
            &p,
            "PROMPT-SECRET-0815 MOCK_SECRET_LOG please",
            json!({"title": "TITLE-SECRET-4711"}),
        )
        .await;
    let t = a
        .wait_status(&id, &["done", "blocked", "failed", "held"])
        .await;
    let leaked_token = t["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["content"].as_str())
        .find(|c| c.contains("found key"))
        .map(String::from);
    let (_, bodies) = spans_until(&h, &seen, "invoke_workflow queue_task").await;
    assert!(!bodies.is_empty(), "spans exported");
    assert!(bodies
        .iter()
        .all(|(auth, _)| auth == "Bearer collector-key"));

    let mut spans = vec![];
    // Collect again after the flush loop (all bodies so far).
    let bodies = seen.lock().unwrap().clone();
    for (_, b) in &bodies {
        for leak in [
            "PROMPT-SECRET",
            "TITLE-SECRET",
            "PROJECT-SECRET",
            "please",
            "MOCK_LOG.md",
            "otel-a@x.ch",
            "found key",
            "ghp_",
        ] {
            assert!(!b.contains(leak), "{leak} leaked into a span: {b}");
        }
        if let Some(tok) = &leaked_token {
            // The agent's log line is redacted in events; nothing of it in spans.
            assert!(!b.contains(tok.as_str()));
        }
        // Official proto types accept the JSON.
        let req: ExportTraceServiceRequest = serde_json::from_str(b).expect("valid OTLP JSON");
        let res = req.resource_spans[0].resource.as_ref().unwrap();
        assert_eq!(s_attr(&res.attributes, "service.name"), "queued-test");
        for rs in req.resource_spans {
            for ss in rs.scope_spans {
                assert_eq!(ss.scope.as_ref().unwrap().name, "queued");
                spans.extend(ss.spans);
            }
        }
    }
    let root = spans
        .iter()
        .find(|s| s.name == "invoke_workflow queue_task")
        .expect("root span");
    assert_eq!(root.kind, 1, "INTERNAL");
    assert_eq!(
        s_attr(&root.attributes, "gen_ai.operation.name"),
        "invoke_workflow"
    );
    assert_eq!(s_attr(&root.attributes, "queue.task.id"), id);
    assert!(root.parent_span_id.is_empty());
    let agents: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "invoke_agent mock")
        .collect();
    assert!(
        !agents.is_empty(),
        "{:?}",
        spans.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    let agent = agents
        .iter()
        .find(|s| s_attr(&s.attributes, "queue.run.kind") == "agent")
        .unwrap();
    assert_eq!(agent.kind, 3, "CLIENT");
    assert_eq!(agent.trace_id, root.trace_id);
    assert_eq!(agent.parent_span_id, root.span_id);
    assert_eq!(
        s_attr(&agent.attributes, "gen_ai.operation.name"),
        "invoke_agent"
    );
    assert_eq!(s_attr(&agent.attributes, "gen_ai.agent.name"), "mock");
    assert_eq!(s_attr(&agent.attributes, "gen_ai.provider.name"), "mock");
    // Mock agent run: 1200 input + 800 cache read; input includes the cache.
    assert_eq!(i_attr(&agent.attributes, "gen_ai.usage.input_tokens"), 2000);
    assert_eq!(
        i_attr(&agent.attributes, "gen_ai.usage.cache_read.input_tokens"),
        800
    );
    assert_eq!(i_attr(&agent.attributes, "gen_ai.usage.output_tokens"), 150);
    assert!(agent.end_time_unix_nano >= agent.start_time_unix_nano);
    let tool = spans
        .iter()
        .find(|s| s.name.starts_with("execute_tool "))
        .expect("tool span");
    assert_eq!(tool.name, "execute_tool edit");
    assert_eq!(tool.kind, 1);
    assert_eq!(tool.parent_span_id, agent.span_id);
    assert_eq!(s_attr(&tool.attributes, "gen_ai.tool.name"), "edit");
    assert_eq!(s_attr(&tool.attributes, "gen_ai.tool.type"), "function");
    // Only constant keys of the conventions or our `queue.*` namespace.
    for s in &spans {
        for kv in &s.attributes {
            assert!(
                kv.key.starts_with("gen_ai.")
                    || kv.key.starts_with("queue.")
                    || kv.key == "error.type",
                "unexpected attribute {}",
                kv.key
            );
        }
    }
}

/// A failing run: status ERROR with a class, no error text.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn failed_runs_mark_spans_without_messages(pool: PgPool) {
    let (url, seen) = collector().await;
    let h = start_with(pool, |c| {
        c.otel = queued::otel::OtelConfig::from_env(&|k: &str| {
            (k == "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").then(|| format!("{url}/v1/traces"))
        })
        .unwrap();
    })
    .await;
    let a = h.user("otel-b@x.ch").await;
    a.settings(json!({"max_fix_attempts": 0})).await;
    let p = a.project(json!({"name": "p"})).await;
    let id = a.task(&p, "MOCK_FAIL with ERRTEXT-SECRET").await;
    a.wait_status(&id, &["failed", "blocked"]).await;
    let (spans, bodies) = spans_until(&h, &seen, "invoke_workflow queue_task").await;
    let all: Vec<&str> = bodies.iter().map(|(_, b)| b.as_str()).collect();
    assert!(!all.join("\n").contains("ERRTEXT-SECRET"));
    assert!(
        !all.join("\n").contains("mock failure"),
        "contract summary is content"
    );
    // The agent run itself worked (it reported "failed" in its contract) …
    let agent = spans
        .iter()
        .find(|s| s.name == "invoke_agent mock")
        .unwrap();
    assert_eq!(agent.status.as_ref().unwrap().code, 0);
    // … the task failed: the root span says so, by class only.
    let root = spans
        .iter()
        .find(|s| s.name == "invoke_workflow queue_task")
        .unwrap();
    let status = root.status.as_ref().unwrap();
    assert_eq!(status.code, 2, "ERROR");
    assert!(status.message.is_empty());
    let et = s_attr(&root.attributes, "error.type");
    assert!(et == "failed" || et == "blocked", "{et}");
}

/// No OTLP configuration: no tracer, no task, no request.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn tracing_is_off_without_configuration(pool: PgPool) {
    let h = start(pool).await;
    assert!(h.st.otel.is_none());
    assert!(h.st.cfg.otel.is_none());
}

/// WP09-N11: `GET /api/tasks` also lists tasks in the merge queue.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn task_list_includes_merging(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("merging@x.ch").await;
    let p = a.project(json!({"name": "p"})).await;
    let id = a.task(&p, "x").await;
    a.wait_status(&id, &["done"]).await;
    sqlx::query("UPDATE tasks SET status='merging', merge_state='queued' WHERE id=$1::uuid")
        .bind(&id)
        .execute(&h.st.db)
        .await
        .unwrap();
    let ts = a.tasks().await;
    assert!(
        ts.iter().any(|t| t["id"] == id && t["status"] == "merging"),
        "{ts:?}"
    );
}
