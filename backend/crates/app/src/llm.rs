//! Direct single-shot model calls (WP13) for plan, review and OnEmpty
//! analysis: Anthropic Messages API and OpenAI-compatible chat completions.
//! No tools, one request. The system part is the stable, cacheable prefix
//! (project goal, file list – no task ids or timestamps before it).
//!
//! Wire formats are prior knowledge (the provider pages could not be read
//! here) and are only tested against local fake servers.
use std::time::Duration;

use queued_api::Usage;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::{
    agent::{classify_text, ErrorCategory},
    connectors::{self, Wire},
};

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    /// Stable prefix (cache breakpoint after it).
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
    /// 1-hour cache TTL (Anthropic; recommended for batches).
    pub cache_long: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub text: String,
    pub usage: Usage,
}

/// Failure of a direct call, already classified.
#[derive(Debug, Clone, PartialEq)]
pub struct CallError {
    pub category: ErrorCategory,
    pub message: String,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}]", self.message, self.category.label())
    }
}

/// HTTP client for model APIs (long timeout: a single answer can take
/// minutes), with the SSRF guard applied at DNS resolution and redirects.
pub fn client(guard: &Guard) -> reqwest::Client {
    guarded_builder(guard)
        .user_agent("queued/0.2")
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .expect("http client")
}

/// Builder with [`GuardedResolver`] and [`redirect_policy`] (also used by
/// the LiteLLM gateway client).
pub fn guarded_builder(guard: &Guard) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .dns_resolver(std::sync::Arc::new(GuardedResolver {
            allow_private: guard.allow_private,
        }))
        .redirect(redirect_policy(guard))
}

/// Connection to one provider endpoint.
#[derive(Clone)]
pub struct Endpoint {
    pub wire: Wire,
    pub base: String,
    pub kind: String,
    pub secret: Option<String>,
    /// Which targets the app may call (SSRF guard, see [`check_target`]).
    pub guard: Guard,
}

/// SSRF guard for app-side calls to user-configured URLs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Guard {
    /// Loopback and private networks allowed (`ALLOW_PRIVATE_LLM_ENDPOINTS`,
    /// for self-hosted models in the LAN; tests).
    pub allow_private: bool,
    /// `host:port` that is never called (the runner).
    pub deny: Vec<String>,
}

use queued_common::net::forbidden_ip;

/// DNS resolver of the guarded clients: every address is checked with
/// [`forbidden_ip`] *at connection time*, so a name that passed
/// [`check_target`] cannot be rebound to an internal address before the
/// request (WP13-N8). Only the checked addresses are handed to the connector.
struct GuardedResolver {
    allow_private: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow = self.allow_private;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| forbidden_ip(a.ip(), allow).is_none())
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{host}: nur interne oder gesperrte Adressen"),
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Most redirects a guarded client follows.
const MAX_REDIRECTS: usize = 5;

/// Redirect policy of the guarded clients: http(s) only, at most
/// [`MAX_REDIRECTS`] hops, no IP literal in a forbidden range (host names
/// go through [`GuardedResolver`] again), never the runner.
fn redirect_policy(guard: &Guard) -> reqwest::redirect::Policy {
    let g = guard.clone();
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("zu viele Weiterleitungen");
        }
        let u = attempt.url();
        if !matches!(u.scheme(), "http" | "https") {
            return attempt.error("Weiterleitung nur zu http/https");
        }
        let host = u
            .host_str()
            .unwrap_or_default()
            .trim_matches(['[', ']'])
            .to_string();
        let port = u.port_or_known_default().unwrap_or(443);
        if g.deny
            .iter()
            .any(|d| d.eq_ignore_ascii_case(&format!("{host}:{port}")))
        {
            return attempt.error("Weiterleitung zu einem internen Dienst");
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if forbidden_ip(ip, g.allow_private).is_some() {
                return attempt.error("Weiterleitung zu einer internen Adresse");
            }
        }
        attempt.follow()
    })
}

/// Refuses calls to link-local/metadata addresses always, to loopback and
/// private networks unless allowed, and to the runner. Resolves host names
/// (a later DNS change between check and request is not covered).
pub async fn check_target(url: &str, guard: &Guard) -> Result<(), CallError> {
    let deny = |why: &str| {
        CallError {
        category: ErrorCategory::InvalidRequest,
        message: format!("Ziel {url} nicht erlaubt ({why}). Für eigene Server im internen Netz: ALLOW_PRIVATE_LLM_ENDPOINTS=true."),
    }
    };
    let u = url::Url::parse(url).map_err(|_| deny("ungültige URL"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(deny("nur http/https"));
    }
    let host = u
        .host_str()
        .ok_or_else(|| deny("ohne Host"))?
        .trim_matches(['[', ']'])
        .to_string();
    let port = u.port_or_known_default().unwrap_or(443);
    let hp = format!("{host}:{port}");
    if guard.deny.iter().any(|d| d.eq_ignore_ascii_case(&hp)) {
        return Err(deny("interner Dienst"));
    }
    let addrs: Vec<std::net::IpAddr> = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| CallError {
                category: ErrorCategory::Network,
                message: format!("Name {host} nicht auflösbar: {e}"),
            })?
            .map(|a| a.ip())
            .collect(),
    };
    for ip in addrs {
        if let Some(why) = forbidden_ip(ip, guard.allow_private) {
            return Err(deny(why));
        }
        if guard
            .deny
            .iter()
            .any(|d| d.eq_ignore_ascii_case(&format!("{ip}:{port}")))
        {
            return Err(deny("interner Dienst"));
        }
    }
    Ok(())
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("wire", &self.wire)
            .field("base", &self.base)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Endpoint {
    /// Request builder with the authentication headers of this endpoint.
    pub fn req(
        &self,
        http: &reqwest::Client,
        method: reqwest::Method,
        url: &str,
    ) -> reqwest::RequestBuilder {
        let mut r = http.request(method, url);
        let key = self.secret.as_deref().unwrap_or("").trim();
        match self.wire {
            Wire::Anthropic => {
                r = r.header("anthropic-version", ANTHROPIC_VERSION);
                if !key.is_empty() {
                    r = if matches!(self.kind.as_str(), "anthropic_api" | "mock") {
                        r.header("x-api-key", key)
                    } else {
                        r.bearer_auth(key)
                    };
                }
            }
            Wire::OpenAiChat => {
                if !key.is_empty() {
                    r = if connectors::auth_header(&self.kind) == "api-key" {
                        r.header("api-key", key)
                    } else {
                        r.bearer_auth(key)
                    };
                }
            }
        }
        r
    }

    pub fn redact(&self, text: &str) -> String {
        connectors::redact(text, self.secret.as_deref())
    }
}

/// Body of one Anthropic Messages request (also used inside batches).
pub fn anthropic_body(r: &Request) -> Value {
    let mut cache = json!({"type": "ephemeral"});
    if r.cache_long {
        cache["ttl"] = json!("1h");
    }
    json!({
        "model": r.model,
        "max_tokens": r.max_tokens,
        "system": [{"type": "text", "text": r.system, "cache_control": cache}],
        "messages": [{"role": "user", "content": r.user}],
    })
}

/// Body of one chat-completions request (also used inside batches).
pub fn chat_body(r: &Request, with_model: bool) -> Value {
    let mut b = json!({
        "messages": [
            {"role": "system", "content": r.system},
            {"role": "user", "content": r.user},
        ],
    });
    if with_model {
        b["model"] = json!(r.model);
    }
    b
}

fn num(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_u64()).map_or(0, |x| {
        x.min(crate::agent::MAX_REPORTED_TOKENS as u64) as i64
    })
}

/// Text + usage of an Anthropic message object.
pub fn parse_anthropic(v: &Value) -> Option<Response> {
    let text: String = v["content"]
        .as_array()?
        .iter()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let u = &v["usage"];
    Some(Response {
        text,
        usage: Usage {
            input_tokens: num(u, "input_tokens"),
            output_tokens: num(u, "output_tokens"),
            cache_read_tokens: num(u, "cache_read_input_tokens"),
            cache_write_tokens: num(u, "cache_creation_input_tokens"),
            cost_usd: 0.0,
        },
    })
}

/// Text + usage of a chat completion (cached tokens are part of
/// `prompt_tokens` and are split off).
pub fn parse_chat(v: &Value) -> Option<Response> {
    let text = v["choices"][0]["message"]["content"].as_str()?.to_string();
    let u = &v["usage"];
    let prompt = num(u, "prompt_tokens");
    let cached = num(&u["prompt_tokens_details"], "cached_tokens").min(prompt);
    Some(Response {
        text,
        usage: Usage {
            input_tokens: prompt - cached,
            output_tokens: num(u, "completion_tokens"),
            cache_read_tokens: cached,
            cache_write_tokens: 0,
            cost_usd: 0.0,
        },
    })
}

/// Classifies a failed HTTP response.
pub fn http_error(status: u16, body: &str) -> CallError {
    let msg: String = body.chars().take(800).collect();
    let category = match status {
        401 | 403 => ErrorCategory::Auth,
        402 => ErrorCategory::Billing,
        429 => ErrorCategory::RateLimit(None),
        404 => ErrorCategory::ModelNotFound,
        400 | 422 => classify_text(&msg)
            .filter(|c| matches!(c, ErrorCategory::Billing | ErrorCategory::Auth))
            .unwrap_or(ErrorCategory::InvalidRequest),
        529 => ErrorCategory::Overloaded,
        s if s >= 500 => ErrorCategory::ServerError,
        s => ErrorCategory::Unknown(Some(s)),
    };
    CallError {
        category,
        message: format!("HTTP {status}: {msg}"),
    }
}

pub fn net_error(e: &reqwest::Error) -> CallError {
    CallError {
        category: ErrorCategory::Network,
        message: format!("Verbindung fehlgeschlagen: {e}"),
    }
}

/// Sends a request and decodes JSON, mapping errors (secret redacted).
pub async fn send_json<T: DeserializeOwned>(
    ep: &Endpoint,
    rb: reqwest::RequestBuilder,
) -> Result<T, CallError> {
    let rb = guarded(ep, rb).await?;
    let resp = rb.send().await.map_err(|e| {
        let mut c = net_error(&e);
        c.message = ep.redact(&c.message);
        c
    })?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| net_error(&e))?;
    if !(200..300).contains(&status) {
        let mut c = http_error(status, &text);
        c.message = ep.redact(&c.message);
        return Err(c);
    }
    serde_json::from_str(&text).map_err(|_| CallError {
        category: ErrorCategory::Unknown(Some(status)),
        message: format!(
            "Unerwartete Antwort: {}",
            ep.redact(&text.chars().take(300).collect::<String>())
        ),
    })
}

/// Builds the request and checks its target against the SSRF guard.
pub async fn guarded(
    ep: &Endpoint,
    rb: reqwest::RequestBuilder,
) -> Result<reqwest::RequestBuilder, CallError> {
    let (client, req) = rb.build_split();
    let req = req.map_err(|e| CallError {
        category: ErrorCategory::InvalidRequest,
        message: format!("Ungültige Anfrage: {e}"),
    })?;
    check_target(req.url().as_str(), &ep.guard).await?;
    Ok(reqwest::RequestBuilder::from_parts(client, req))
}

/// One synchronous call.
pub async fn call(
    http: &reqwest::Client,
    ep: &Endpoint,
    r: &Request,
) -> Result<Response, CallError> {
    match ep.wire {
        Wire::Anthropic => {
            let v: Value = send_json(
                ep,
                ep.req(
                    http,
                    reqwest::Method::POST,
                    &format!("{}/v1/messages", ep.base),
                )
                .json(&anthropic_body(r)),
            )
            .await?;
            parse_anthropic(&v).ok_or_else(|| unexpected(&v))
        }
        Wire::OpenAiChat => {
            let v: Value = send_json(
                ep,
                ep.req(
                    http,
                    reqwest::Method::POST,
                    &format!("{}/chat/completions", ep.base),
                )
                .json(&chat_body(r, true)),
            )
            .await?;
            parse_chat(&v).ok_or_else(|| unexpected(&v))
        }
    }
}

fn unexpected(v: &Value) -> CallError {
    CallError {
        category: ErrorCategory::Unknown(None),
        message: format!(
            "Antwort ohne Text: {}",
            v.to_string().chars().take(300).collect::<String>()
        ),
    }
}

/// First JSON object in a model answer (models like to wrap JSON in prose
/// or code fences).
pub fn extract_json<T: DeserializeOwned>(text: &str) -> Option<T> {
    if let Ok(v) = serde_json::from_str(text.trim()) {
        return Some(v);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start)
        .then(|| serde_json::from_str(&text[start..=end]).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local server: `/hop?to=<url>` answers 302 to `to`, everything else 200.
    async fn redirector() -> String {
        use axum::{extract::Query, response::IntoResponse, routing::get, Router};
        async fn hop(
            Query(q): Query<std::collections::HashMap<String, String>>,
        ) -> axum::response::Response {
            match q.get("to") {
                Some(to) => (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, to.clone())],
                )
                    .into_response(),
                None => "landed".into_response(),
            }
        }
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(l, Router::new().route("/hop", get(hop)))
                .await
                .unwrap()
        });
        base
    }

    #[tokio::test]
    async fn guarded_client_refuses_internal_addresses_at_connect_time() {
        let base = redirector().await;
        let port = base.rsplit(':').next().unwrap();
        // `localhost` resolves to loopback: refused by the resolver itself,
        // even without `check_target` (a rebinding name behaves the same).
        let strict = client(&Guard::default());
        let e = strict
            .get(format!("http://localhost:{port}/hop"))
            .send()
            .await
            .unwrap_err();
        assert!(format!("{e:?}").contains("interne"), "{e:?}");
        let lax = client(&Guard {
            allow_private: true,
            deny: vec![],
        });
        let ok = lax
            .get(format!("http://localhost:{port}/hop"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.text().await.unwrap(), "landed");
    }

    #[tokio::test]
    async fn guarded_client_checks_redirect_targets() {
        let base = redirector().await;
        let hp = base.trim_start_matches("http://").to_string();
        let lax = client(&Guard {
            allow_private: true,
            deny: vec!["127.0.0.1:1".into()],
        });
        let enc = |u: &str| url::form_urlencoded::byte_serialize(u.as_bytes()).collect::<String>();
        // Metadata address: never followed.
        let e = lax
            .get(format!(
                "{base}/hop?to={}",
                enc("http://169.254.169.254/latest/meta-data/")
            ))
            .send()
            .await
            .unwrap_err();
        assert!(e.is_redirect(), "{e:?}");
        // The runner (deny list) is never a redirect target.
        assert!(lax
            .get(format!(
                "{base}/hop?to={}",
                enc("http://127.0.0.1:1/v1/exec")
            ))
            .send()
            .await
            .unwrap_err()
            .is_redirect());
        // Other schemes are never followed (reqwest hands back the 302).
        match lax
            .get(format!("{base}/hop?to={}", enc("file:///etc/passwd")))
            .send()
            .await
        {
            Ok(r) => assert_eq!(r.status(), 302),
            Err(e) => assert!(e.is_redirect(), "{e:?}"),
        }
        // An allowed redirect is followed.
        let ok = lax
            .get(format!(
                "{base}/hop?to={}",
                enc(&format!("http://{hp}/hop"))
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.text().await.unwrap(), "landed");
        // Chains longer than MAX_REDIRECTS are cut off.
        let mut chain = format!("http://{hp}/hop");
        for _ in 0..=MAX_REDIRECTS {
            chain = format!("http://{hp}/hop?to={}", enc(&chain));
        }
        assert!(lax.get(chain).send().await.unwrap_err().is_redirect());
    }

    fn req() -> Request {
        Request {
            model: "claude-sonnet-5-5".into(),
            system: "stable prefix".into(),
            user: "task".into(),
            max_tokens: 1000,
            cache_long: true,
        }
    }

    #[test]
    fn bodies_put_the_stable_prefix_first_with_cache_control() {
        let b = anthropic_body(&req());
        assert_eq!(
            b["system"][0]["cache_control"],
            json!({"type":"ephemeral","ttl":"1h"})
        );
        assert_eq!(b["messages"][0]["content"], "task");
        let mut r = req();
        r.cache_long = false;
        assert_eq!(
            anthropic_body(&r)["system"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        let c = chat_body(&r, false);
        assert!(c.get("model").is_none());
        assert_eq!(c["messages"][0]["role"], "system");
    }

    #[test]
    fn responses_and_errors_parse() {
        let a = parse_anthropic(&json!({"content":[{"type":"text","text":"a"},{"type":"tool_use"},{"type":"text","text":"b"}],
            "usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":7}})).unwrap();
        assert_eq!(a.text, "a\nb");
        assert_eq!(
            (a.usage.cache_read_tokens, a.usage.cache_write_tokens),
            (100, 7)
        );
        let c = parse_chat(&json!({"choices":[{"message":{"content":"x"}}],"usage":{"prompt_tokens":100,"completion_tokens":9,"prompt_tokens_details":{"cached_tokens":60}}})).unwrap();
        assert_eq!(
            (
                c.usage.input_tokens,
                c.usage.cache_read_tokens,
                c.usage.output_tokens
            ),
            (40, 60, 9)
        );
        assert!(parse_chat(&json!({"choices":[]})).is_none());
        assert_eq!(http_error(401, "no").category, ErrorCategory::Auth);
        assert_eq!(
            http_error(429, "slow").category,
            ErrorCategory::RateLimit(None)
        );
        assert_eq!(http_error(529, "").category, ErrorCategory::Overloaded);
        assert_eq!(
            http_error(503, "").category.class(),
            queued_api::FailureClass::ProviderTransient
        );
        assert_eq!(
            http_error(400, "bad").category,
            ErrorCategory::InvalidRequest
        );
        assert_eq!(
            http_error(400, "credit balance too low").category,
            ErrorCategory::Billing
        );
    }

    #[test]
    fn json_is_extracted_from_prose() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct R {
            approved: bool,
        }
        assert_eq!(
            extract_json::<R>("{\"approved\":true}"),
            Some(R { approved: true })
        );
        assert_eq!(
            extract_json::<R>("Here:\n```json\n{\"approved\": false}\n```"),
            Some(R { approved: false })
        );
        assert_eq!(extract_json::<R>("no json"), None);
        assert_eq!(extract_json::<R>("} {"), None);
    }

    #[test]
    fn endpoint_auth_headers_and_redaction() {
        let http = reqwest::Client::new();
        let ep = |kind: &str, wire| Endpoint {
            wire,
            base: "http://x".into(),
            kind: kind.into(),
            secret: Some("sk-secret-1".into()),
            guard: Guard::default(),
        };
        let r = ep("anthropic_api", Wire::Anthropic)
            .req(&http, reqwest::Method::POST, "http://x/v1/messages")
            .build()
            .unwrap();
        assert_eq!(r.headers()["x-api-key"], "sk-secret-1");
        assert_eq!(r.headers()["anthropic-version"], ANTHROPIC_VERSION);
        let r = ep("anthropic_gateway", Wire::Anthropic)
            .req(&http, reqwest::Method::POST, "http://x")
            .build()
            .unwrap();
        assert_eq!(r.headers()["authorization"], "Bearer sk-secret-1");
        let r = ep("azure_openai", Wire::OpenAiChat)
            .req(&http, reqwest::Method::POST, "http://x")
            .build()
            .unwrap();
        assert_eq!(r.headers()["api-key"], "sk-secret-1");
        assert!(r.headers().get("authorization").is_none());
        assert_eq!(
            ep("mistral", Wire::OpenAiChat).redact("key sk-secret-1 bad"),
            "key *** bad"
        );
        assert!(!format!("{:?}", ep("mistral", Wire::OpenAiChat)).contains("sk-secret"));
    }

    #[tokio::test]
    async fn ssrf_guard() {
        let strict = Guard::default();
        let lan = Guard {
            allow_private: true,
            deny: vec!["127.0.0.1:8090".into()],
        };
        for (url, g, ok) in [
            ("http://169.254.169.254/latest/meta-data", &lan, false),
            ("http://[fe80::1]/x", &lan, false),
            ("http://0.0.0.0:80/", &lan, false),
            ("http://127.0.0.1:9000/v1", &strict, false),
            ("http://10.1.2.3:8000/v1", &strict, false),
            ("http://100.64.0.1/", &strict, false),
            ("http://[::ffff:127.0.0.1]/", &strict, false),
            ("http://[fd00::1]/", &strict, false),
            ("http://10.1.2.3:8000/v1", &lan, true),
            ("http://127.0.0.1:9000/v1", &lan, true),
            ("http://127.0.0.1:8090/v1/exec", &lan, false),
            ("http://localhost:8090/v1/exec", &strict, false),
            ("https://1.1.1.1/v1", &strict, true),
            ("ftp://1.1.1.1/", &lan, false),
        ] {
            assert_eq!(
                check_target(url, g).await.is_ok(),
                ok,
                "{url} allow_private={}",
                g.allow_private
            );
        }
        let e = check_target("http://127.0.0.1:1/", &strict)
            .await
            .unwrap_err();
        assert!(e.message.contains("ALLOW_PRIVATE_LLM_ENDPOINTS"));
    }
}
