//! HTTP client for `/api` (same-origin cookies). All bodies and responses
//! are `queued_api` types shared with the backend.
use gloo_net::http::{Request, RequestBuilder, Response};
use serde::{de::DeserializeOwned, Serialize};

use crate::logic::errors;

#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    /// Already translated for the UI.
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

fn url(path: &str) -> String {
    format!("/api{path}")
}

fn network(_e: gloo_net::Error) -> ApiError {
    ApiError {
        status: 0,
        message: errors::NETWORK.into(),
    }
}

async fn check(r: Response) -> ApiResult<Response> {
    if r.ok() {
        return Ok(r);
    }
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    let raw = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
        .unwrap_or_else(|| {
            if text.len() < 200 {
                text.clone()
            } else {
                String::new()
            }
        });
    Err(ApiError {
        status,
        message: errors::translate(status, &raw),
    })
}

async fn send(b: RequestBuilder) -> ApiResult<Response> {
    check(b.send().await.map_err(network)?).await
}

async fn send_json<B: Serialize>(b: RequestBuilder, body: &B) -> ApiResult<Response> {
    let req = b.json(body).map_err(network)?;
    check(req.send().await.map_err(network)?).await
}

async fn parse<T: DeserializeOwned>(r: Response) -> ApiResult<T> {
    r.json::<T>().await.map_err(|e| ApiError {
        status: 0,
        message: format!("Unerwartete Antwort vom Server: {e}"),
    })
}

pub async fn get<T: DeserializeOwned>(path: &str) -> ApiResult<T> {
    parse(send(Request::get(&url(path))).await?).await
}

/// POST with a JSON body, response parsed as `T`.
pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    parse(send_json(Request::post(&url(path)), body).await?).await
}

/// POST without a meaningful response body.
pub async fn post_ok<B: Serialize>(path: &str, body: &B) -> ApiResult<()> {
    send_json(Request::post(&url(path)), body).await.map(|_| ())
}

/// POST with an empty JSON object.
pub async fn action(path: &str) -> ApiResult<()> {
    post_ok(path, &serde_json::json!({})).await
}

pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    parse(send_json(Request::put(&url(path)), body).await?).await
}

pub async fn put_ok<B: Serialize>(path: &str, body: &B) -> ApiResult<()> {
    send_json(Request::put(&url(path)), body).await.map(|_| ())
}

pub async fn patch<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    parse(send_json(Request::patch(&url(path)), body).await?).await
}

pub async fn patch_ok<B: Serialize>(path: &str, body: &B) -> ApiResult<()> {
    send_json(Request::patch(&url(path)), body)
        .await
        .map(|_| ())
}

pub async fn delete(path: &str) -> ApiResult<()> {
    send(Request::delete(&url(path))).await.map(|_| ())
}

/// Raw upload (ZIP import).
pub async fn upload<T: DeserializeOwned>(path: &str, file: &web_sys::Blob) -> ApiResult<T> {
    let req = Request::post(&url(path))
        .header("Content-Type", "application/octet-stream")
        .body(file.clone())
        .map_err(network)?;
    parse(check(req.send().await.map_err(network)?).await?).await
}

/// POST of an already serialized JSON string (push subscription from the browser).
pub async fn post_raw_json(path: &str, json: String) -> ApiResult<()> {
    let req = Request::post(&url(path))
        .header("Content-Type", "application/json")
        .body(json)
        .map_err(network)?;
    check(req.send().await.map_err(network)?).await.map(|_| ())
}

pub fn enc(s: &str) -> String {
    String::from(js_sys::encode_uri_component(s))
}
