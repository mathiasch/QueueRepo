//! WP10: Prometheus metrics of the app (`/metrics`).
//!
//! Labels only from fixed sets: harness (`claude|codex|gemini|opencode|mock`),
//! run kind, outcome, status, route pattern, catalog price id, currency,
//! channel. Never user, task or project ids, titles or free-text model names.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{MatchedPath, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use queued_common::metrics::{self, Family, Histogram, Registry};

use crate::state::AppState;

/// Database gauges are refreshed at most this often (scrape load).
const DB_REFRESH: Duration = Duration::from_secs(10);

pub struct AppMetrics {
    pub registry: Registry,
    pub http: Arc<Family>,
    pub http_seconds: Arc<Histogram>,
    pub tasks: Arc<Family>,
    pub agent_runs: Arc<Family>,
    pub agent_run_seconds: Arc<Histogram>,
    pub tokens: Arc<Family>,
    pub cost: Arc<Family>,
    pub pauses: Arc<Family>,
    pub notifications: Arc<Family>,
    pub outbox_pending: Arc<Family>,
    pub digests: Arc<Family>,
    pub database_up: Arc<Family>,
    pub runner_up: Arc<Family>,
    pub otel_dropped: Arc<Family>,
    pub otel_errors: Arc<Family>,
    /// WP11: audit entries that could not be written.
    pub audit_errors: Arc<Family>,
    /// WP11: login/second-factor failures and lockouts (`kind`).
    pub auth_failures: Arc<Family>,
    /// WP12: Best-of-N groups fanned out, selections (`by`), budget stops.
    pub bon_groups: Arc<Family>,
    pub bon_selections: Arc<Family>,
    pub bon_aborts: Arc<Family>,
    /// WP12: previews started/failed and screenshots taken (`event`).
    pub previews: Arc<Family>,
    refreshed: tokio::sync::Mutex<Option<Instant>>,
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Active task states reported as gauge (terminal states only grow and are
/// covered by the run counters).
pub const ACTIVE_STATES: &[&str] = &[
    "pending_approval",
    "queued",
    "running",
    "waiting_batch",
    "waiting_input",
    "held",
    "merging",
    "comparing",
    "candidate",
];

impl AppMetrics {
    pub fn new() -> Self {
        let r = Registry::default();
        r.gauge(
            "queued_build_info",
            "Version of queued (always 1).",
            &["version"],
        )
        .set(&[env!("CARGO_PKG_VERSION")], 1.0);
        Self {
            http: r.counter(
                "queued_http_requests_total",
                "HTTP requests by route pattern and status class.",
                &["method", "route", "status"],
            ),
            http_seconds: r.histogram(
                "queued_http_request_duration_seconds",
                "Time until the response head (SSE streams continue after it).",
                &[],
                metrics::HTTP_BUCKETS,
            ),
            tasks: r.gauge(
                "queued_tasks",
                "Tasks in an active state (all users).",
                &["status"],
            ),
            agent_runs: r.counter(
                "queued_agent_runs_total",
                "Finished agent runs (agent, review, plan) by outcome.",
                &["provider", "kind", "outcome"],
            ),
            agent_run_seconds: r.histogram(
                "queued_agent_run_duration_seconds",
                "Duration of agent runs.",
                &["kind"],
                metrics::RUN_BUCKETS,
            ),
            tokens: r.counter(
                "queued_tokens_total",
                "Tokens used (input without cache, output, cache read, cache write). model = price catalog id or unknown.",
                &["provider", "model", "type"],
            ),
            cost: r.counter(
                "queued_cost_total",
                "Money cost of model usage by currency (catalog prices).",
                &["provider", "currency"],
            ),
            pauses: r.gauge(
                "queued_provider_pauses",
                "Provider pauses in force now (all users) by reason.",
                &["reason"],
            ),
            notifications: r.counter(
                "queued_notifications_total",
                "Notification deliveries by channel and result.",
                &["channel", "result"],
            ),
            outbox_pending: r.gauge(
                "queued_notification_outbox_pending",
                "External notifications waiting for delivery.",
                &[],
            ),
            digests: r.counter(
                "queued_digests_total",
                "Daily summaries created (exactly one per user and local day).",
                &[],
            ),
            database_up: r.gauge(
                "queued_database_up",
                "1 if the database answered the last scrape.",
                &[],
            ),
            runner_up: r.gauge(
                "queued_runner_up",
                "1 if the runner answered the last readiness check.",
                &[],
            ),
            otel_dropped: r.counter(
                "queued_otel_spans_dropped_total",
                "Spans dropped because the export queue was full.",
                &[],
            ),
            otel_errors: r.counter(
                "queued_otel_export_errors_total",
                "Failed OTLP export requests.",
                &[],
            ),
            audit_errors: r.counter(
                "queued_audit_write_errors_total",
                "Audit log entries that could not be written.",
                &[],
            ),
            auth_failures: r.counter(
                "queued_auth_failures_total",
                "Failed sign-ins by kind (password, mfa, locked).",
                &["kind"],
            ),
            bon_groups: r.counter(
                "queued_bon_groups_total",
                "Best-of-N tasks that fanned out into attempts.",
                &[],
            ),
            bon_selections: r.counter(
                "queued_bon_selections_total",
                "Best-of-N winners by who picked them (user, auto).",
                &["by"],
            ),
            bon_aborts: r.counter(
                "queued_bon_budget_aborts_total",
                "Best-of-N groups stopped because the budget of all attempts was exhausted.",
                &[],
            ),
            previews: r.counter(
                "queued_preview_events_total",
                "Preview dev servers and screenshots (started, failed, screenshot).",
                &["event"],
            ),
            refreshed: tokio::sync::Mutex::new(None),
            registry: r,
        }
    }

    pub fn bon_group(&self) {
        self.bon_groups.inc(&[]);
    }

    pub fn bon_selection(&self, by: queued_api::bon::SelectedBy) {
        self.bon_selections.inc(&[match by {
            queued_api::bon::SelectedBy::User => "user",
            queued_api::bon::SelectedBy::Auto => "auto",
        }]);
    }

    pub fn bon_abort(&self) {
        self.bon_aborts.inc(&[]);
    }

    /// `event`: started | failed | screenshot.
    pub fn preview(&self, event: &'static str) {
        self.previews.inc(&[event]);
    }

    /// Token usage and cost of one priced step (from `runner::record_cost`).
    pub fn usage(&self, provider: &str, sc: &queued_api::StepCost) {
        let model = sc.price_id.as_deref().unwrap_or("unknown");
        let u = &sc.usage;
        for (t, v) in [
            ("input", u.input_tokens),
            ("output", u.output_tokens),
            ("cache_read", u.cache_read_tokens),
            ("cache_write", u.cache_write_tokens),
        ] {
            if v > 0 {
                self.tokens.add(&[provider, model, t], v as f64);
            }
        }
        if sc.amount > 0.0 {
            self.cost.add(&[provider, &sc.currency], sc.amount);
        }
    }

    /// Reads the database gauges (cached for [`DB_REFRESH`]).
    async fn refresh(&self, st: &AppState) {
        let mut last = self.refreshed.lock().await;
        if last.is_some_and(|t| t.elapsed() < DB_REFRESH) {
            return;
        }
        let q = async {
            let tasks: Vec<(String, i64)> = sqlx::query_as(
                "SELECT status, COUNT(*) FROM tasks WHERE status = ANY($1) GROUP BY status",
            )
            .bind(ACTIVE_STATES)
            .fetch_all(&st.db)
            .await?;
            let pauses: Vec<(String, i64)> = sqlx::query_as(
                "SELECT reason, COUNT(*) FROM provider_pauses
                  WHERE ends_at IS NULL OR ends_at > now() GROUP BY reason",
            )
            .fetch_all(&st.db)
            .await?;
            let outbox: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM notification_outbox WHERE status='pending'",
            )
            .fetch_one(&st.db)
            .await?;
            Ok::<_, sqlx::Error>((tasks, pauses, outbox))
        };
        match tokio::time::timeout(Duration::from_secs(2), q).await {
            Ok(Ok((tasks, pauses, outbox))) => {
                // Every active state appears, also with 0.
                self.tasks.replace(
                    ACTIVE_STATES
                        .iter()
                        .map(|s| {
                            let n = tasks.iter().find(|(x, _)| x == s).map(|r| r.1).unwrap_or(0);
                            (vec![s.to_string()], n as f64)
                        })
                        .collect(),
                );
                self.pauses.replace(
                    ["rate_limit", "auth", "budget"]
                        .iter()
                        .map(|r| {
                            let n = pauses
                                .iter()
                                .find(|(x, _)| x == r)
                                .map(|r| r.1)
                                .unwrap_or(0);
                            (vec![r.to_string()], n as f64)
                        })
                        .collect(),
                );
                self.outbox_pending.set(&[], outbox as f64);
                self.database_up.set(&[], 1.0);
                *last = Some(Instant::now());
            }
            _ => {
                self.database_up.set(&[], 0.0);
                // Retry at the next scrape.
                *last = None;
            }
        }
    }
}

/// Counts requests by route pattern (`/api/tasks/{id}`); paths without a
/// route (static files, unknown) share `unmatched`.
pub async fn track(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let method = metrics::method_label(req.method().as_str());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let t = Instant::now();
    let resp = next.run(req).await;
    st.metrics.http.inc(&[
        method,
        &route,
        metrics::status_class(resp.status().as_u16()),
    ]);
    st.metrics
        .http_seconds
        .observe(&[], t.elapsed().as_secs_f64());
    resp
}

/// `GET /metrics` (main listener with `METRICS_TOKEN`, or the separate
/// `METRICS_BIND` listener).
pub async fn serve(State(st): State<AppState>, req: Request) -> Response {
    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !st.cfg.metrics.allows(auth) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "unauthorized",
        )
            .into_response();
    }
    st.metrics.refresh(&st).await;
    (
        [(header::CONTENT_TYPE, metrics::CONTENT_TYPE)],
        st.metrics.registry.render(),
    )
        .into_response()
}

/// Router of the separate metrics listener (`METRICS_BIND`).
pub fn router(st: AppState) -> axum::Router {
    axum::Router::new()
        .route("/metrics", axum::routing::get(serve))
        .with_state(st)
}
