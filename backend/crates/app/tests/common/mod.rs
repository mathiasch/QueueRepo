//! Integration-test harness: fresh Postgres DB per test (sqlx::test), the
//! real runner in-process (engine `none`), the real app router + scheduler,
//! all talking HTTP over loopback.
#![allow(dead_code)]
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::StreamExt;
use queued::{config::Config, state::AppState};
use queued_runner::{RunnerConfig, RunnerState};
use serde_json::{json, Value};
use sqlx::PgPool;

pub struct Harness {
    pub base: String,
    pub st: AppState,
    pub runner_dir: tempfile::TempDir,
    /// Set with [`start_flaky`]: number of `/v1/exec` requests seen by the proxy.
    pub exec_count: Option<Arc<AtomicUsize>>,
    _app_dir: tempfile::TempDir,
}

pub struct Client {
    pub base: String,
    pub http: reqwest::Client,
}

pub async fn start(pool: PgPool) -> Harness {
    start_with(pool, |_| {}).await
}

/// Like [`start`], with adjusted app config.
pub async fn start_with(pool: PgPool, f: impl FnOnce(&mut Config)) -> Harness {
    boot(pool, f, false).await
}

/// App talks to the runner through [`flaky_proxy`]: the first exec stream is
/// cut after the first usage frame (runner restart / network drop).
pub async fn start_flaky(pool: PgPool) -> Harness {
    boot(pool, |_| {}, true).await
}

async fn boot(pool: PgPool, f: impl FnOnce(&mut Config), flaky: bool) -> Harness {
    let runner_dir = tempfile::tempdir().unwrap();
    let rs = RunnerState::new(RunnerConfig::for_tests(runner_dir.path().to_path_buf()));
    let rl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut runner_url = format!("http://{}", rl.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(rl, queued_runner::router(rs)).await.unwrap() });
    let mut exec_count = None;
    if flaky {
        let (url, count) = flaky_proxy(runner_url).await;
        runner_url = url;
        exec_count = Some(count);
    }

    let app_dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_tests(
        "unused",
        app_dir.path().to_path_buf(),
        &runner_url,
        "test-token",
    );
    f(&mut cfg);
    let st = AppState::new(cfg, pool).unwrap();
    queued::start_worker(&st).await.unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let router = queued::api::router(st.clone());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    Harness {
        base,
        st,
        runner_dir,
        exec_count,
        _app_dir: app_dir,
    }
}

/// Reverse proxy in front of the runner. Everything is forwarded; the body of
/// the first `/v1/exec` response is aborted right after the first frame that
/// carries token usage.
async fn flaky_proxy(upstream: String) -> (String, Arc<AtomicUsize>) {
    use axum::{body::Body, extract::Request, response::Response};
    let count = Arc::new(AtomicUsize::new(0));
    let client = reqwest::Client::new();
    let c2 = count.clone();
    let app = axum::Router::new().fallback(move |req: Request| {
        let (client, upstream, count) = (client.clone(), upstream.clone(), c2.clone());
        async move {
            let path = req
                .uri()
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_default();
            let cut = path == "/v1/exec" && count.fetch_add(1, Ordering::SeqCst) == 0;
            let mut fwd = client.request(req.method().clone(), format!("{upstream}{path}"));
            for (k, v) in req.headers() {
                if k != "host" && k != "content-length" {
                    fwd = fwd.header(k, v);
                }
            }
            let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                .await
                .unwrap();
            let resp = fwd.body(body).send().await.unwrap();
            let mut out = Response::builder().status(resp.status().as_u16());
            for (k, v) in resp.headers() {
                out = out.header(k, v);
            }
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(64);
            tokio::spawn(async move {
                let mut s = resp.bytes_stream();
                let mut buf: Vec<u8> = vec![];
                while let Some(Ok(chunk)) = s.next().await {
                    if !cut {
                        if tx.send(Ok(chunk.to_vec())).await.is_err() {
                            return;
                        }
                        continue;
                    }
                    buf.extend_from_slice(&chunk);
                    while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=pos).collect();
                        let usage = String::from_utf8_lossy(&line).contains("input_tokens");
                        let _ = tx.send(Ok(line)).await;
                        if usage {
                            // Let the frame reach the client before the reset.
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            let _ = tx
                                .send(Err(std::io::Error::other("connection reset (test)")))
                                .await;
                            return; // drops the upstream body → the runner cancels the run
                        }
                    }
                }
            });
            out.body(Body::from_stream(
                tokio_stream::wrappers::ReceiverStream::new(rx),
            ))
            .unwrap()
        }
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, count)
}

impl Harness {
    pub fn anon(&self) -> Client {
        Client {
            base: self.base.clone(),
            http: reqwest::Client::builder()
                .cookie_store(true)
                .build()
                .unwrap(),
        }
    }

    /// Client that sends `Authorization: Bearer <token>` (WP07) and no cookie.
    pub fn bearer(&self, token: &str) -> Client {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        Client {
            base: self.base.clone(),
            http: reqwest::Client::builder()
                .default_headers(h)
                .build()
                .unwrap(),
        }
    }

    /// Registers a user and returns a logged-in client. The provider is set to `mock`.
    pub async fn user(&self, email: &str) -> Client {
        let c = self.anon();
        let r = c
            .post(
                "/api/auth/register",
                json!({"email": email, "password": "password123", "name": email}),
            )
            .await;
        assert_eq!(r.0, 200, "register: {}", r.1);
        c.settings(json!({"default_provider": "mock", "onempty_enabled": false}))
            .await;
        c
    }

    pub fn workspace(&self, project: &str) -> std::path::PathBuf {
        self.runner_dir.path().join("workspaces").join(project)
    }
}

impl Client {
    pub async fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut r = self.http.request(method, format!("{}{path}", self.base));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    pub async fn get(&self, path: &str) -> (u16, Value) {
        self.req(reqwest::Method::GET, path, None).await
    }
    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.req(reqwest::Method::POST, path, Some(body)).await
    }
    pub async fn put(&self, path: &str, body: Value) -> (u16, Value) {
        self.req(reqwest::Method::PUT, path, Some(body)).await
    }
    pub async fn patch(&self, path: &str, body: Value) -> (u16, Value) {
        self.req(reqwest::Method::PATCH, path, Some(body)).await
    }
    pub async fn delete(&self, path: &str) -> (u16, Value) {
        self.req(reqwest::Method::DELETE, path, None).await
    }

    /// Creates an API token (cookie session) and returns it.
    pub async fn token(&self, scopes: &[&str]) -> String {
        let (code, v) = self
            .post(
                "/api/tokens",
                json!({"name": "test", "scopes": scopes, "expires_in_days": 30}),
            )
            .await;
        assert_eq!(code, 200, "{v}");
        v["token"].as_str().unwrap().to_string()
    }

    /// Only the status (the body is not read: works for SSE streams).
    pub async fn status_of(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> u16 {
        let mut r = self.http.request(method, format!("{}{path}", self.base));
        if let Some(b) = body {
            r = r.json(&b);
        }
        r.send().await.unwrap().status().as_u16()
    }

    pub async fn settings(&self, patch: Value) -> Value {
        let (_, mut s) = self.get("/api/settings").await;
        for (k, v) in patch.as_object().unwrap() {
            s[k] = v.clone();
        }
        let (code, s) = self.put("/api/settings", s).await;
        assert_eq!(code, 200, "{s}");
        s
    }

    pub async fn project(&self, body: Value) -> String {
        let (code, p) = self.post("/api/projects", body).await;
        assert_eq!(code, 200, "{p}");
        p["id"].as_str().unwrap().to_string()
    }

    pub async fn task_with(&self, project: &str, prompt: &str, extra: Value) -> String {
        let mut body = json!({"project_id": project, "prompt": prompt});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (code, t) = self.post("/api/tasks", body).await;
        assert_eq!(code, 200, "{t}");
        t["id"].as_str().unwrap().to_string()
    }

    pub async fn limit(&self, provider: &str) -> Value {
        self.get("/api/limits")
            .await
            .1
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["provider"] == provider)
            .unwrap()
            .clone()
    }

    pub async fn events_of(&self, id: &str) -> Vec<(String, String)> {
        self.task_json(id).await["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["kind"].as_str().unwrap().to_string(),
                    e["content"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    pub async fn task(&self, project: &str, prompt: &str) -> String {
        let (code, t) = self
            .post(
                "/api/tasks",
                json!({"project_id": project, "prompt": prompt}),
            )
            .await;
        assert_eq!(code, 200, "{t}");
        t["id"].as_str().unwrap().to_string()
    }

    pub async fn task_json(&self, id: &str) -> Value {
        self.get(&format!("/api/tasks/{id}")).await.1
    }

    pub async fn tasks(&self) -> Vec<Value> {
        self.get("/api/tasks")
            .await
            .1
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Polls until the task reaches one of `states`.
    pub async fn wait_status(&self, id: &str, states: &[&str]) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let t = self.task_json(id).await;
            let s = t["task"]["status"].as_str().unwrap_or_default().to_string();
            if states.contains(&s.as_str()) {
                return t;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "task {id} stuck in {s}, wanted {states:?}: {t}"
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    pub async fn wait_until<F: Fn(&[Value]) -> bool>(&self, what: &str, f: F) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let ts = self.tasks().await;
            if f(&ts) {
                return ts;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timeout waiting for {what}: {ts:#?}"
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    pub async fn notifications(&self) -> Vec<Value> {
        self.get("/api/notifications")
            .await
            .1
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

pub fn status_of(ts: &[Value], title_contains: &str) -> Vec<String> {
    ts.iter()
        .filter(|t| {
            t["title"]
                .as_str()
                .unwrap_or_default()
                .contains(title_contains)
        })
        .map(|t| t["status"].as_str().unwrap_or_default().to_string())
        .collect()
}
pub mod fake_git_host;
pub mod fake_intake;
pub mod fake_llm;
