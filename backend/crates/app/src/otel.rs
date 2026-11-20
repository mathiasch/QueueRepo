//! WP10: OpenTelemetry traces over OTLP/HTTP with JSON encoding.
//!
//! Deliberately small and explicit instead of the `opentelemetry` SDK plus a
//! `tracing` bridge: only the spans built here are exported (a bridge would
//! also export HTTP spans with URIs such as OIDC callbacks with `code=`), no
//! extra runtime dependency, no version coupling between four crates. The
//! JSON is checked against the official proto types in the tests.
//!
//! Semantics: OpenTelemetry GenAI semantic conventions (status
//! *Development*), checked against `open-telemetry/semantic-conventions-genai`
//! commit `e6658f1` (2026-11-06): `invoke_workflow` (INTERNAL) per run of a
//! task, `invoke_agent {gen_ai.agent.name}` (CLIENT, the agent CLI runs
//! outside this process), `execute_tool {gen_ai.tool.name}` (INTERNAL),
//! `chat {model}` (CLIENT) for direct model calls.
//!
//! **Never content:** attribute keys are `&'static str` constants and the
//! values come from fixed sets, ids or numbers – no prompt, title, code,
//! tool argument/result, error text, path or URL. Content capture
//! (`OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT`) is not supported.
//!
//! Off unless an OTLP endpoint is configured: [`Span`] then holds nothing and
//! every call is an `Option` check (no allocation, no task, no socket).
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rand::RngCore;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

/// Spans waiting for export at most (more are dropped and counted).
pub const QUEUE_CAPACITY: usize = 2048;
/// Spans per export request.
pub const BATCH_SIZE: usize = 512;
/// Longest wait before a partial batch is sent.
pub const EXPORT_INTERVAL: Duration = Duration::from_secs(5);
/// Longest string attribute value.
const MAX_VALUE: usize = 128;

/// Operator configuration (standard `OTEL_*` variables).
#[derive(Clone, PartialEq)]
pub struct OtelConfig {
    /// Full URL of the traces endpoint (`…/v1/traces`).
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub service_name: String,
    pub resource: Vec<(String, String)>,
    pub timeout: Duration,
}

/// Header values may hold collector API keys: never printed.
impl std::fmt::Debug for OtelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtelConfig")
            .field("endpoint", &self.endpoint)
            .field(
                "headers",
                &self.headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("service_name", &self.service_name)
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(a), Some(c)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push((a * 16 + c) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `k=v,k2=v2` (W3C baggage-like, values percent-encoded).
fn kv_list(s: &str, what: &str) -> Result<Vec<(String, String)>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').ok_or_else(|| {
                format!(
                    "{what}: entry without '=': {:?}",
                    p.chars().take(40).collect::<String>()
                )
            })?;
            let k = pct_decode(k.trim());
            if k.is_empty() {
                return Err(format!("{what}: empty key"));
            }
            Ok((k, pct_decode(v.trim())))
        })
        .collect()
}

impl OtelConfig {
    /// `Ok(None)` = tracing off (nothing configured, `OTEL_SDK_DISABLED=true`
    /// or `OTEL_TRACES_EXPORTER=none`). A configuration this exporter cannot
    /// honour (another protocol or exporter) is an error: better no start
    /// than silently wrong telemetry.
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let on = |k: &str| {
            get(k).is_some_and(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1"))
        };
        if let Some(c) = get("OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT") {
            if !c.trim().eq_ignore_ascii_case("NO_CONTENT") {
                tracing::warn!(
                    "OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT={c} is ignored: queued never exports prompts, code or tool data"
                );
            }
        }
        if on("OTEL_SDK_DISABLED") {
            return Ok(None);
        }
        match get("OTEL_TRACES_EXPORTER").map(|v| v.trim().to_lowercase()) {
            Some(v) if v == "none" => return Ok(None),
            Some(v) if v != "otlp" => {
                return Err(format!(
                    "OTEL_TRACES_EXPORTER={v} is not supported (only otlp or none)"
                ))
            }
            _ => {}
        }
        let endpoint = match (
            get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            get("OTEL_EXPORTER_OTLP_ENDPOINT"),
        ) {
            (Some(e), _) => e.trim().to_string(),
            (None, Some(base)) => format!("{}/v1/traces", base.trim().trim_end_matches('/')),
            (None, None) => return Ok(None),
        };
        let u = url::Url::parse(&endpoint)
            .map_err(|_| format!("OTLP endpoint is not a URL: {endpoint}"))?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(format!("OTLP endpoint must be http(s): {endpoint}"));
        }
        let protocol = get("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL")
            .or_else(|| get("OTEL_EXPORTER_OTLP_PROTOCOL"));
        if let Some(p) = protocol {
            if p.trim() != "http/json" {
                return Err(format!(
                    "OTLP protocol {p} is not supported: queued exports http/json only (set OTEL_EXPORTER_OTLP_PROTOCOL=http/json)"
                ));
            }
        }
        let headers = match get("OTEL_EXPORTER_OTLP_TRACES_HEADERS")
            .or_else(|| get("OTEL_EXPORTER_OTLP_HEADERS"))
        {
            Some(h) => kv_list(&h, "OTEL_EXPORTER_OTLP_HEADERS")?,
            None => vec![],
        };
        let resource = match get("OTEL_RESOURCE_ATTRIBUTES") {
            Some(r) => kv_list(&r, "OTEL_RESOURCE_ATTRIBUTES")?,
            None => vec![],
        };
        let service_name = get("OTEL_SERVICE_NAME")
            .map(|s| s.trim().to_string())
            .or_else(|| {
                resource
                    .iter()
                    .find(|(k, _)| k == "service.name")
                    .map(|(_, v)| v.clone())
            })
            .unwrap_or_else(|| "queued".into());
        let timeout_ms = get("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT")
            .or_else(|| get("OTEL_EXPORTER_OTLP_TIMEOUT"))
            .map(|v| {
                v.trim()
                    .parse::<u64>()
                    .map_err(|_| format!("OTLP timeout must be milliseconds: {v}"))
            })
            .transpose()?
            .unwrap_or(10_000)
            .clamp(100, 60_000);
        Ok(Some(Self {
            endpoint,
            headers,
            service_name,
            resource,
            timeout: Duration::from_millis(timeout_ms),
        }))
    }
}

/// Span kind (OTLP enum values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Internal = 1,
    Client = 3,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
    Bool(bool),
}

/// Identifies a span as parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanCtx {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
}

#[derive(Clone, Debug)]
pub struct SpanData {
    pub ctx: SpanCtx,
    pub parent: Option<[u8; 8]>,
    pub name: String,
    pub kind: Kind,
    pub start_ns: u64,
    pub end_ns: u64,
    pub attrs: Vec<(&'static str, AttrValue)>,
    pub error: bool,
}

enum Msg {
    Span(Box<SpanData>),
    Flush(oneshot::Sender<()>),
}

/// Handle of the exporter (cheap to clone through `Arc`).
pub struct Tracer {
    tx: mpsc::Sender<Msg>,
    pub dropped: AtomicU64,
    /// Counters in the app's metrics (dropped spans, failed exports).
    on_drop: Option<Arc<queued_common::metrics::Family>>,
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn random_ctx(trace: Option<[u8; 16]>) -> SpanCtx {
    let mut rng = rand::thread_rng();
    let trace_id = trace.unwrap_or_else(|| loop {
        let mut t = [0u8; 16];
        rng.fill_bytes(&mut t);
        if t != [0; 16] {
            break t;
        }
    });
    let span_id = loop {
        let mut s = [0u8; 8];
        rng.fill_bytes(&mut s);
        if s != [0; 8] {
            break s;
        }
    };
    SpanCtx { trace_id, span_id }
}

impl Tracer {
    /// Starts the export loop.
    pub fn start(
        cfg: OtelConfig,
        on_drop: Option<Arc<queued_common::metrics::Family>>,
        on_error: Option<Arc<queued_common::metrics::Family>>,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        tokio::spawn(export_loop(cfg, rx, on_error));
        Arc::new(Self {
            tx,
            dropped: AtomicU64::new(0),
            on_drop,
        })
    }

    fn submit(&self, s: SpanData) {
        if self.tx.try_send(Msg::Span(Box::new(s))).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            if let Some(c) = &self.on_drop {
                c.inc(&[]);
            }
        }
    }

    /// Exports everything queued so far (shutdown, tests).
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Flush(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

async fn export_loop(
    cfg: OtelConfig,
    mut rx: mpsc::Receiver<Msg>,
    on_error: Option<Arc<queued_common::metrics::Family>>,
) {
    let http = match reqwest::Client::builder().timeout(cfg.timeout).build() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("OTLP exporter disabled: {e:#}");
            return;
        }
    };
    let mut batch: Vec<SpanData> = vec![];
    let mut tick = tokio::time::interval(EXPORT_INTERVAL);
    let mut last_warn: Option<std::time::Instant> = None;
    loop {
        let flush_reply = tokio::select! {
            m = rx.recv() => match m {
                Some(Msg::Span(s)) => {
                    batch.push(*s);
                    if batch.len() < BATCH_SIZE {
                        continue;
                    }
                    None
                }
                Some(Msg::Flush(reply)) => Some(reply),
                None => {
                    export(&http, &cfg, std::mem::take(&mut batch), &on_error, &mut last_warn).await;
                    return;
                }
            },
            _ = tick.tick() => None,
        };
        while !batch.is_empty() {
            let n = batch.len().min(BATCH_SIZE);
            let part: Vec<SpanData> = batch.drain(..n).collect();
            export(&http, &cfg, part, &on_error, &mut last_warn).await;
        }
        if let Some(r) = flush_reply {
            let _ = r.send(());
        }
    }
}

async fn export(
    http: &reqwest::Client,
    cfg: &OtelConfig,
    spans: Vec<SpanData>,
    on_error: &Option<Arc<queued_common::metrics::Family>>,
    last_warn: &mut Option<std::time::Instant>,
) {
    if spans.is_empty() {
        return;
    }
    let body = encode(cfg, &spans);
    let mut req = http
        .post(&cfg.endpoint)
        .header("content-type", "application/json")
        .body(body.to_string());
    for (k, v) in &cfg.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let res = req.send().await.and_then(|r| r.error_for_status());
    if let Err(e) = res {
        if let Some(c) = on_error {
            c.inc(&[]);
        }
        // At most one warning per minute; the reason without URL details.
        if last_warn.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            *last_warn = Some(std::time::Instant::now());
            let why = e.status().map(|s| format!("HTTP {s}")).unwrap_or_else(|| {
                if e.is_timeout() {
                    "timeout".into()
                } else {
                    "connection failed".into()
                }
            });
            tracing::warn!("OTLP export of {} span(s) failed: {why}", spans.len());
        }
    }
}

fn attr_json(k: &str, v: &AttrValue) -> Value {
    let value = match v {
        AttrValue::Str(s) => json!({"stringValue": s}),
        // OTLP JSON: 64-bit integers are strings.
        AttrValue::Int(i) => json!({"intValue": i.to_string()}),
        AttrValue::Bool(b) => json!({"boolValue": b}),
    };
    json!({"key": k, "value": value})
}

/// OTLP `ExportTraceServiceRequest` in the proto3 JSON mapping.
pub fn encode(cfg: &OtelConfig, spans: &[SpanData]) -> Value {
    let mut resource = vec![
        attr_json("service.name", &AttrValue::Str(cfg.service_name.clone())),
        attr_json(
            "service.version",
            &AttrValue::Str(env!("CARGO_PKG_VERSION").into()),
        ),
    ];
    for (k, v) in &cfg.resource {
        if k != "service.name" && k != "service.version" {
            resource.push(attr_json(k, &AttrValue::Str(v.clone())));
        }
    }
    let spans: Vec<Value> = spans
        .iter()
        .map(|s| {
            let mut o = json!({
                "traceId": hex(&s.ctx.trace_id),
                "spanId": hex(&s.ctx.span_id),
                "name": s.name,
                "kind": s.kind as i32,
                "startTimeUnixNano": s.start_ns.to_string(),
                "endTimeUnixNano": s.end_ns.max(s.start_ns).to_string(),
                "attributes": s.attrs.iter().map(|(k, v)| attr_json(k, v)).collect::<Vec<_>>(),
                // ERROR without message (messages may hold content).
                "status": {"code": if s.error { 2 } else { 0 }},
            });
            if let Some(p) = s.parent {
                o["parentSpanId"] = json!(hex(&p));
            }
            o
        })
        .collect();
    json!({"resourceSpans": [{
        "resource": {"attributes": resource},
        "scopeSpans": [{
            "scope": {"name": "queued", "version": env!("CARGO_PKG_VERSION")},
            "spans": spans,
        }],
    }]})
}

tokio::task_local! {
    static CURRENT: Option<SpanCtx>;
}

/// The span the current task runs in (set with [`scope`]).
pub fn current() -> Option<SpanCtx> {
    CURRENT.try_with(|c| *c).ok().flatten()
}

/// Runs `fut` with `ctx` as the current parent span.
pub async fn scope<F: std::future::Future>(ctx: Option<SpanCtx>, fut: F) -> F::Output {
    CURRENT.scope(ctx, fut).await
}

/// A span under construction; exported when ended or dropped. Without a
/// tracer it is empty and every method does nothing.
pub struct Span {
    inner: Option<(Arc<Tracer>, SpanData)>,
}

impl Span {
    pub fn none() -> Self {
        Self { inner: None }
    }

    /// Starts a span under `parent` (or under [`current`] when `None`).
    pub fn start(
        tracer: Option<&Arc<Tracer>>,
        name: impl FnOnce() -> String,
        kind: Kind,
        parent: Option<SpanCtx>,
    ) -> Self {
        let Some(t) = tracer else {
            return Self::none();
        };
        let parent = parent.or_else(current);
        let ctx = random_ctx(parent.map(|p| p.trace_id));
        Self {
            inner: Some((
                t.clone(),
                SpanData {
                    ctx,
                    parent: parent.map(|p| p.span_id),
                    name: name(),
                    kind,
                    start_ns: now_ns(),
                    end_ns: 0,
                    attrs: vec![],
                    error: false,
                },
            )),
        }
    }

    pub fn ctx(&self) -> Option<SpanCtx> {
        self.inner.as_ref().map(|(_, d)| d.ctx)
    }

    pub fn is_recording(&self) -> bool {
        self.inner.is_some()
    }

    fn set(&mut self, k: &'static str, v: AttrValue) {
        if let Some((_, d)) = &mut self.inner {
            d.attrs.retain(|(x, _)| *x != k);
            d.attrs.push((k, v));
        }
    }

    /// String attribute: values must come from fixed sets or ids (callers
    /// never pass user content); cut to 128 characters as a last guard.
    pub fn str(&mut self, k: &'static str, v: &str) {
        if self.inner.is_some() {
            self.set(k, AttrValue::Str(v.chars().take(MAX_VALUE).collect()));
        }
    }

    pub fn int(&mut self, k: &'static str, v: i64) {
        self.set(k, AttrValue::Int(v));
    }

    pub fn bool(&mut self, k: &'static str, v: bool) {
        self.set(k, AttrValue::Bool(v));
    }

    /// Marks the span failed with a low-cardinality `error.type`.
    pub fn error(&mut self, error_type: &str) {
        if let Some((_, d)) = &mut self.inner {
            d.error = true;
        }
        self.str("error.type", error_type);
    }

    /// Start time override (tool spans start at their event).
    pub fn set_start_ns(&mut self, ns: u64) {
        if let Some((_, d)) = &mut self.inner {
            d.start_ns = ns;
        }
    }

    pub fn end(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        if let Some((t, mut d)) = self.inner.take() {
            d.end_ns = now_ns();
            t.submit(d);
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.finish();
    }
}

pub fn now_unix_ns() -> u64 {
    now_ns()
}

/// `gen_ai.agent.name` of a harness (fixed set).
pub fn agent_name(provider: &str) -> &'static str {
    match provider {
        "claude" => "claude-code",
        "codex" => "codex",
        "gemini" => "gemini-cli",
        "opencode" => "opencode",
        "mock" => "mock",
        _ => "other",
    }
}

/// `gen_ai.provider.name`: well-known values of the conventions where one
/// applies, otherwise the connector type (a fixed set of WP13).
pub fn provider_name(harness: &str, connector_type: Option<&str>) -> &'static str {
    match connector_type {
        Some("bedrock") => "aws.bedrock",
        Some("vertex") => "gcp.vertex_ai",
        Some("azure_openai") => "azure.ai.openai",
        Some("mistral") => "mistral",
        Some("gemini_api" | "gemini_subscription") => "gcp.gemini",
        Some("openai_api" | "codex_subscription") => "openai",
        Some("anthropic_api" | "anthropic_gateway" | "claude_subscription") => "anthropic",
        Some("openrouter") => "openrouter",
        Some("infomaniak") => "infomaniak",
        Some("litellm") => "litellm",
        Some("openai_compatible") => "openai_compatible",
        Some("mock") => "mock",
        Some(_) => "other",
        None => match harness {
            "claude" => "anthropic",
            "codex" => "openai",
            "gemini" => "gcp.gemini",
            "mock" => "mock",
            _ => "other",
        },
    }
}

/// Model as attribute/label: the catalog id without vendor prefix, or
/// `None` – free text typed by a user never leaves the process.
pub fn model_name(price_id: Option<&str>) -> Option<&str> {
    price_id.map(|p| p.rsplit('/').next().unwrap_or(p))
}

/// Tool name as attribute: `[A-Za-z0-9_.:-]`, at most 64 characters,
/// otherwise `other` (names come from the CLI stream, which repository
/// configuration such as MCP servers can influence).
pub fn tool_name(raw: &str) -> &str {
    let ok = !raw.is_empty()
        && raw.len() <= 64
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'));
    if ok {
        raw
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn off_without_endpoint_or_when_disabled() {
        assert_eq!(OtelConfig::from_env(&env(&[])).unwrap(), None);
        assert_eq!(
            OtelConfig::from_env(&env(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_SDK_DISABLED", "true")
            ]))
            .unwrap(),
            None
        );
        assert_eq!(
            OtelConfig::from_env(&env(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_TRACES_EXPORTER", "none")
            ]))
            .unwrap(),
            None
        );
    }

    #[test]
    fn endpoint_headers_resource_and_protocol() {
        let c = OtelConfig::from_env(&env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://c.example:4318/"),
            (
                "OTEL_EXPORTER_OTLP_HEADERS",
                "authorization=Bearer%20s3cret, x-tenant=a",
            ),
            (
                "OTEL_RESOURCE_ATTRIBUTES",
                "deployment.environment=prod,service.name=q1",
            ),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(c.endpoint, "https://c.example:4318/v1/traces");
        assert_eq!(
            c.headers,
            vec![
                ("authorization".into(), "Bearer s3cret".into()),
                ("x-tenant".into(), "a".into())
            ]
        );
        assert_eq!(c.service_name, "q1");
        assert!(
            !format!("{c:?}").contains("s3cret"),
            "Debug hides header values"
        );
        // The signal-specific endpoint is used as is.
        let c = OtelConfig::from_env(&env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://ignored:1"),
            ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "http://c:4318/custom"),
            ("OTEL_SERVICE_NAME", "svc"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(c.endpoint, "http://c:4318/custom");
        assert_eq!(c.service_name, "svc");
        for bad in [
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ],
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
            ],
            vec![("OTEL_EXPORTER_OTLP_ENDPOINT", "ftp://c")],
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_TRACES_EXPORTER", "zipkin"),
            ],
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "novalue"),
            ],
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_TIMEOUT", "10s"),
            ],
        ] {
            assert!(OtelConfig::from_env(&env(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn no_tracer_means_empty_spans() {
        let mut s = Span::start(
            None,
            || unreachable!("name never built"),
            Kind::Internal,
            None,
        );
        s.str("gen_ai.operation.name", "invoke_agent");
        assert!(!s.is_recording());
        assert_eq!(s.ctx(), None);
        s.end();
    }

    #[test]
    fn names_are_sanitized_and_mapped() {
        assert_eq!(tool_name("Bash"), "Bash");
        assert_eq!(
            tool_name("mcp__github__create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(tool_name("rm -rf /"), "other");
        assert_eq!(tool_name(&"x".repeat(65)), "other");
        assert_eq!(tool_name(""), "other");
        assert_eq!(provider_name("claude", None), "anthropic");
        assert_eq!(provider_name("claude", Some("bedrock")), "aws.bedrock");
        assert_eq!(provider_name("opencode", Some("weird")), "other");
        assert_eq!(agent_name("gemini"), "gemini-cli");
        assert_eq!(
            model_name(Some("anthropic/claude-sonnet-5-5")),
            Some("claude-sonnet-5-5")
        );
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(pct_decode("a%20b%2Cc"), "a b,c");
        assert_eq!(pct_decode("100%"), "100%");
        assert_eq!(pct_decode("%zz"), "%zz");
        assert_eq!(
            pct_decode("%äb"),
            "%äb",
            "no panic inside a multibyte character"
        );
    }
}
