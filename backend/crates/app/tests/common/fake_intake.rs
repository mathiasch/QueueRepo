//! WP08 test double for the outbound APIs of the integrations: GitHub issue
//! comments, Slack `chat.postMessage`/`conversations.replies`, Linear
//! GraphQL `agentActivityCreate`, Jira issue comments. Formats as in the
//! sources named in `queued::intake::*`. Every request is logged (method,
//! path, auth header, body); `fail` makes the next requests fail in a
//! chosen way – `lost` processes the request and answers 500 (the answer
//! "got lost"), `500`/`429` do nothing.
#![allow(dead_code)]
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    body::Bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct Logged {
    pub method: String,
    pub path: String,
    pub auth: String,
    pub body: Value,
}

#[derive(Default)]
pub struct Inner {
    pub log: Vec<Logged>,
    /// Created objects per target (issue / thread / session): (id, body).
    pub created: HashMap<String, Vec<(String, Value)>>,
    /// Failure modes for the next write requests.
    pub fail: Vec<&'static str>,
    next: u64,
}

#[derive(Clone, Default)]
pub struct FakeIntake(pub Arc<Mutex<Inner>>);

impl FakeIntake {
    pub fn fail(&self, modes: &[&'static str]) {
        self.0.lock().unwrap().fail.extend_from_slice(modes);
    }
    /// Objects created for `target` (e.g. `github:acme/shop#12`).
    pub fn created(&self, target: &str) -> Vec<(String, Value)> {
        self.0
            .lock()
            .unwrap()
            .created
            .get(target)
            .cloned()
            .unwrap_or_default()
    }
    pub fn log(&self) -> Vec<Logged> {
        self.0.lock().unwrap().log.clone()
    }
    pub fn writes(&self) -> usize {
        self.log().iter().filter(|l| l.method == "POST").count()
    }
}

/// The fake as a router (also the `fake_intake` example of the E2E stack).
pub fn router(fake: FakeIntake) -> Router {
    Router::new().fallback(handle).with_state(fake)
}

pub async fn start() -> (String, FakeIntake) {
    let fake = FakeIntake::default();
    let app = router(fake.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, fake)
}

fn query(q: Option<&str>) -> HashMap<String, String> {
    url::form_urlencoded::parse(q.unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

async fn handle(State(f): State<FakeIntake>, req: Request) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let q = query(req.uri().query());
    let headers: HeaderMap = req.headers().clone();
    let body: Bytes = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut g = f.0.lock().unwrap();
    // Control API for the E2E stack (not logged).
    match (method.as_str(), path.as_str()) {
        ("GET", "/_fake/created") => {
            let t = q.get("target").cloned().unwrap_or_default();
            let list: Vec<Value> = g
                .created
                .get(&t)
                .map(|v| v.iter().map(|(_, b)| b.clone()).collect())
                .unwrap_or_default();
            return Json(Value::Array(list)).into_response();
        }
        ("GET", "/_fake/log") => {
            let list: Vec<Value> = g
                .log
                .iter()
                .map(
                    |l| json!({"method": l.method, "path": l.path, "auth": l.auth, "body": l.body}),
                )
                .collect();
            return Json(Value::Array(list)).into_response();
        }
        _ => {}
    }
    g.log.push(Logged {
        method: method.clone(),
        path: path.clone(),
        auth,
        body: body.clone(),
    });
    let mode = if method == "POST" && !g.fail.is_empty() {
        Some(g.fail.remove(0))
    } else {
        None
    };
    match mode {
        Some("500") => return (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
        Some("429") => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "1")],
                "slow down",
            )
                .into_response()
        }
        _ => {}
    }
    g.next += 1;
    let n = g.next;
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let resp: Response = match (method.as_str(), parts.as_slice()) {
        // GitHub
        ("POST", ["repos", o, r, "issues", num, "comments"]) => {
            let target = format!("github:{o}/{r}#{num}");
            g.created.entry(target).or_default().push((
                n.to_string(),
                json!({"id": n, "body": body["body"], "created_at": chrono::Utc::now()}),
            ));
            (StatusCode::CREATED, Json(json!({"id": n}))).into_response()
        }
        ("GET", ["repos", o, r, "issues", num, "comments"]) => {
            let target = format!("github:{o}/{r}#{num}");
            let list: Vec<Value> = g
                .created
                .get(&target)
                .map(|v| v.iter().map(|(_, b)| b.clone()).collect())
                .unwrap_or_default();
            Json(Value::Array(list)).into_response()
        }
        // Slack
        ("POST", ["chat.postMessage"]) => {
            let ch = body["channel"].as_str().unwrap_or("").to_string();
            let ts = format!("1700000{n:03}.000100");
            let thread = body["thread_ts"].as_str().unwrap_or(&ts).to_string();
            g.created.entry(format!("slack:{ch}:{thread}")).or_default().push((
                ts.clone(),
                json!({"ts": ts, "text": body["text"], "metadata": body["metadata"], "thread_ts": body["thread_ts"]}),
            ));
            Json(json!({"ok": true, "channel": ch, "ts": ts})).into_response()
        }
        ("GET", ["conversations.replies"]) => {
            let key = format!(
                "slack:{}:{}",
                q.get("channel").cloned().unwrap_or_default(),
                q.get("ts").cloned().unwrap_or_default()
            );
            let msgs: Vec<Value> = g
                .created
                .get(&key)
                .map(|v| v.iter().map(|(_, b)| b.clone()).collect())
                .unwrap_or_default();
            Json(json!({"ok": true, "messages": msgs})).into_response()
        }
        // Linear
        ("POST", ["graphql"]) => {
            let input = &body["variables"]["input"];
            let id = input["id"].as_str().unwrap_or("").to_string();
            let target = format!("linear:{}", input["agentSessionId"].as_str().unwrap_or(""));
            let exists = g.created.values().flatten().any(|(i, _)| *i == id);
            if exists {
                Json(json!({"errors": [{"message": "Entity already exists"}]})).into_response()
            } else {
                g.created
                    .entry(target)
                    .or_default()
                    .push((id.clone(), input["content"].clone()));
                Json(json!({"data": {"agentActivityCreate": {"success": true, "agentActivity": {"id": id}}}}))
                    .into_response()
            }
        }
        // Jira
        ("POST", ["rest", "api", "3", "issue", key, "comment"]) => {
            g.created.entry(format!("jira:{key}")).or_default().push((
                n.to_string(),
                json!({"id": n.to_string(), "body": body["body"]}),
            ));
            (StatusCode::CREATED, Json(json!({"id": n.to_string()}))).into_response()
        }
        ("GET", ["rest", "api", "3", "issue", key, "comment"]) => {
            let mut list: Vec<Value> = g
                .created
                .get(&format!("jira:{key}"))
                .map(|v| v.iter().map(|(_, b)| b.clone()).collect())
                .unwrap_or_default();
            list.reverse();
            Json(json!({"comments": list})).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "unknown").into_response(),
    };
    if mode == Some("lost") {
        return (StatusCode::INTERNAL_SERVER_ERROR, "answer lost").into_response();
    }
    resp
}
