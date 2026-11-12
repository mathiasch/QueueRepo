//! Local fake of the provider APIs used by WP13 (no real API calls in tests):
//! Anthropic Messages + Message Batches, OpenAI chat completions + Files +
//! Batch, Mistral Files + Batch jobs, model lists, LiteLLM key admin.
//! The wire shapes are our assumptions (see src/batch.rs), not recordings.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub auth: String,
    pub body: String,
}

pub struct FakeState {
    pub requests: Mutex<Vec<Recorded>>,
    /// Polls answered "in progress" before a batch ends.
    pub polls_until_done: AtomicUsize,
    /// While set, batches never end.
    pub hold: AtomicBool,
    pub review_approved: AtomicBool,
    /// Statuses returned (in order) for the next model calls/submissions.
    pub fail_next: Mutex<Vec<u16>>,
    polls: Mutex<HashMap<String, usize>>,
    /// batch id → (custom_id, request body of the single request)
    batches: Mutex<HashMap<String, (String, Value)>>,
    files: Mutex<HashMap<String, String>>,
    seq: AtomicUsize,
}

pub struct Fake {
    pub url: String,
    pub state: Arc<FakeState>,
}

impl Fake {
    pub fn requests(&self) -> Vec<Recorded> {
        self.state.requests.lock().unwrap().clone()
    }
    pub fn paths(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect()
    }
}

pub async fn start_fake() -> Fake {
    let state = Arc::new(FakeState {
        requests: Mutex::new(vec![]),
        polls_until_done: AtomicUsize::new(1),
        hold: AtomicBool::new(false),
        review_approved: AtomicBool::new(true),
        fail_next: Mutex::new(vec![]),
        polls: Mutex::new(HashMap::new()),
        batches: Mutex::new(HashMap::new()),
        files: Mutex::new(HashMap::new()),
        seq: AtomicUsize::new(0),
    });
    let app = axum::Router::new()
        .fallback(handle)
        .with_state(state.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    Fake { url, state }
}

/// Model answer depending on the prompt (plan / review / analysis).
pub fn answer(s: &FakeState, text: &str) -> String {
    if text.contains("strict critic of an autonomous coding queue") {
        // WP09 critic: the first proposal is dropped, the rest kept.
        let first = text
            .split("index ")
            .nth(1)
            .and_then(|r| r.split(' ').next())
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        json!({"verdicts": [
            {"index": first, "verdict": "drop", "reason": "Fake critic: not valuable"},
            {"index": first + 1, "verdict": "keep", "reason": "fine"}
        ]})
        .to_string()
    } else if text.contains("strict code reviewer") {
        if s.review_approved.load(Ordering::SeqCst) {
            r#"Sure: {"approved": true, "issues": []}"#.into()
        } else {
            r#"```json
{"approved": false, "issues": ["missing test for the edge case"]}
```"#
                .into()
        }
    } else if text.contains("implementation plan") {
        "FAKE PLAN: 1. read the code 2. change it 3. run the tests".into()
    } else if text.contains("queue of this project is empty") {
        json!({"status":"done","summary":"analysed","handoff":"","questions":[],
               "follow_up_tasks":[
                   {"title":"Fake A","prompt":"Implement feature A of the backlog with unit tests and a changelog entry."},
                   {"title":"Fake B","prompt":"Write the user documentation for feature B, including every dialog."}]})
            .to_string()
    } else {
        "ok".into()
    }
}

fn anthropic_message(s: &FakeState, body: &Value) -> Value {
    json!({
        "type": "message",
        "content": [{"type": "text", "text": answer(s, &body.to_string())}],
        "usage": {"input_tokens": 1000, "output_tokens": 200, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 500}
    })
}

fn chat_completion(s: &FakeState, body: &Value) -> Value {
    json!({
        "choices": [{"message": {"role": "assistant", "content": answer(s, &body.to_string())}}],
        "usage": {"prompt_tokens": 1000, "completion_tokens": 200, "prompt_tokens_details": {"cached_tokens": 100}}
    })
}

fn next_id(s: &FakeState, prefix: &str) -> String {
    format!("{prefix}_{}", s.seq.fetch_add(1, Ordering::SeqCst))
}

fn err(status: u16, msg: &str) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({"error": {"message": msg}})),
    )
        .into_response()
}

/// Finished? (counts polls per batch)
fn done(s: &FakeState, id: &str) -> bool {
    if s.hold.load(Ordering::SeqCst) {
        return false;
    }
    let mut p = s.polls.lock().unwrap();
    let n = p.entry(id.to_string()).or_insert(0);
    *n += 1;
    *n > s.polls_until_done.load(Ordering::SeqCst)
}

async fn handle(
    State(s): State<Arc<FakeState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let auth = headers
        .get("x-api-key")
        .or_else(|| headers.get("authorization"))
        .or_else(|| headers.get("api-key"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = String::from_utf8_lossy(&body).to_string();
    s.requests.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        auth: auth.clone(),
        body: text.clone(),
    });
    if auth.contains("bad-key") {
        return err(401, &format!("invalid api key {auth}"));
    }
    let json_body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let model_call = matches!(
        (method.as_str(), path.as_str()),
        (
            "POST",
            "/v1/messages"
                | "/v1/messages/batches"
                | "/v1/chat/completions"
                | "/v1/batches"
                | "/v1/batch/jobs"
        )
    );
    if model_call {
        let mut f = s.fail_next.lock().unwrap();
        if !f.is_empty() {
            let code = f.remove(0);
            return err(code, "injected failure");
        }
    }
    match (method.as_str(), parts.as_slice()) {
        ("GET", ["v1", "models"]) => Json(json!({"data": [{"id": "fake-model"}]})).into_response(),
        ("POST", ["v1", "messages"]) => Json(anthropic_message(&s, &json_body)).into_response(),
        ("POST", ["v1", "messages", "batches"]) => {
            let id = next_id(&s, "msgbatch");
            let r = &json_body["requests"][0];
            s.batches.lock().unwrap().insert(
                id.clone(),
                (
                    r["custom_id"].as_str().unwrap_or_default().into(),
                    r["params"].clone(),
                ),
            );
            Json(json!({"id": id, "processing_status": "in_progress"})).into_response()
        }
        ("GET", ["v1", "messages", "batches", id]) => {
            let id = id.to_string();
            if done(&s, &id) {
                Json(json!({"id": id, "processing_status": "ended",
                            "results_url": format!("http://evil.example/{id}/results")}))
                .into_response()
            } else {
                Json(json!({"id": id, "processing_status": "in_progress"})).into_response()
            }
        }
        ("GET", ["v1", "messages", "batches", id, "results"]) => {
            let Some((cid, params)) = s.batches.lock().unwrap().get(*id).cloned() else {
                return err(404, "no batch");
            };
            let line = json!({"custom_id": cid, "result": {"type": "succeeded", "message": anthropic_message(&s, &params)}});
            format!("{line}\n").into_response()
        }
        ("POST", ["v1", "messages", "batches", _, "cancel"])
        | ("POST", ["v1", "batches", _, "cancel"])
        | ("POST", ["v1", "batch", "jobs", _, "cancel"]) => {
            Json(json!({"status": "cancelling"})).into_response()
        }
        ("POST", ["v1", "chat", "completions"]) => {
            Json(chat_completion(&s, &json_body)).into_response()
        }
        ("POST", ["v1", "files"]) => {
            // Multipart: keep the JSONL line.
            let line = text
                .lines()
                .find(|l| {
                    serde_json::from_str::<Value>(l.trim())
                        .is_ok_and(|v| v.get("custom_id").is_some())
                })
                .unwrap_or_default()
                .trim()
                .to_string();
            if !text.contains("name=\"purpose\"") || !text.contains("batch") {
                return err(400, "purpose missing");
            }
            let id = next_id(&s, "file");
            s.files.lock().unwrap().insert(id.clone(), line);
            Json(json!({"id": id})).into_response()
        }
        ("POST", ["v1", "batches"]) | ("POST", ["v1", "batch", "jobs"]) => {
            let file = json_body["input_file_id"]
                .as_str()
                .or_else(|| json_body["input_files"][0].as_str())
                .unwrap_or_default()
                .to_string();
            let Some(line) = s.files.lock().unwrap().get(&file).cloned() else {
                return err(400, "unknown file");
            };
            let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let id = next_id(&s, "batch");
            s.batches.lock().unwrap().insert(
                id.clone(),
                (
                    req["custom_id"].as_str().unwrap_or_default().into(),
                    req["body"].clone(),
                ),
            );
            Json(json!({"id": id, "status": "validating"})).into_response()
        }
        ("GET", ["v1", "batches", id]) | ("GET", ["v1", "batch", "jobs", id]) => {
            let id = id.to_string();
            let mistral = parts[1] == "batch";
            if done(&s, &id) {
                let out = format!("out_{id}");
                let Some((cid, body)) = s.batches.lock().unwrap().get(&id).cloned() else {
                    return err(404, "no batch");
                };
                let line = json!({"custom_id": cid, "response": {"status_code": 200, "body": chat_completion(&s, &body)}});
                s.files
                    .lock()
                    .unwrap()
                    .insert(out.clone(), line.to_string());
                if mistral {
                    Json(json!({"id": id, "status": "SUCCESS", "output_file": out})).into_response()
                } else {
                    Json(json!({"id": id, "status": "completed", "output_file_id": out}))
                        .into_response()
                }
            } else if mistral {
                Json(json!({"id": id, "status": "RUNNING"})).into_response()
            } else {
                Json(json!({"id": id, "status": "in_progress"})).into_response()
            }
        }
        ("GET", ["v1", "files", id, "content"]) => match s.files.lock().unwrap().get(*id) {
            Some(c) => format!("{c}\n").into_response(),
            None => err(404, "no file"),
        },
        ("POST", ["key", "generate"]) => {
            Json(json!({"key": format!("sk-task-{}", s.seq.fetch_add(1, Ordering::SeqCst))}))
                .into_response()
        }
        ("POST", ["key", "delete"]) => {
            Json(json!({"deleted_keys": json_body["keys"]})).into_response()
        }
        _ => err(404, &format!("fake: no route for {method} {path}")),
    }
}
