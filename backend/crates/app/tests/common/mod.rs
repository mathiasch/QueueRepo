//! Integration-test harness: fresh Postgres DB per test (sqlx::test), the
//! real runner in-process (engine `none`), the real app router + scheduler,
//! all talking HTTP over loopback.
#![allow(dead_code)]
use std::time::Duration;

use queued::{config::Config, state::AppState};
use queued_runner::{RunnerConfig, RunnerState};
use serde_json::{json, Value};
use sqlx::PgPool;

pub struct Harness {
    pub base: String,
    pub st: AppState,
    pub runner_dir: tempfile::TempDir,
    _app_dir: tempfile::TempDir,
}

pub struct Client {
    pub base: String,
    pub http: reqwest::Client,
}

pub async fn start(pool: PgPool) -> Harness {
    let runner_dir = tempfile::tempdir().unwrap();
    let rs = RunnerState::new(RunnerConfig::for_tests(runner_dir.path().to_path_buf()));
    let rl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runner_url = format!("http://{}", rl.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(rl, queued_runner::router(rs)).await.unwrap() });

    let app_dir = tempfile::tempdir().unwrap();
    let cfg = Config::for_tests(
        "unused",
        app_dir.path().to_path_buf(),
        &runner_url,
        "test-token",
    );
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
        _app_dir: app_dir,
    }
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
