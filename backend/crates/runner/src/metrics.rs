//! WP10: Prometheus metrics of the runner. Labels come from fixed sets only:
//! the run kind is the label suffix of the run name (`agent`, `review`, …),
//! never the task id inside the name; routes are axum route patterns.
use std::{sync::Arc, time::Instant};

use axum::{
    extract::{MatchedPath, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use queued_common::{
    metrics::{self, Family, Histogram, Registry},
    ExecFrame,
};

use crate::RunnerState;

/// Run kinds as metric label (suffix of the run name).
const KINDS: &[&str] = &["agent", "review", "plan", "verify", "setup"];

pub fn exec_kind(name: &str) -> &'static str {
    let suffix = name.rsplit('-').next().unwrap_or("");
    KINDS
        .iter()
        .find(|k| **k == suffix)
        .copied()
        .unwrap_or("other")
}

pub struct RunnerMetrics {
    pub registry: Registry,
    http: Arc<Family>,
    http_seconds: Arc<Histogram>,
    exec_active: Arc<Family>,
    exec_total: Arc<Family>,
    exec_seconds: Arc<Histogram>,
    egress: Arc<Family>,
}

/// One run in flight (see [`RunnerMetrics::exec_started`]).
pub struct RunTrack {
    kind: &'static str,
    started: Instant,
    outcome: &'static str,
}

impl Default for RunnerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerMetrics {
    pub fn new() -> Self {
        let r = Registry::default();
        let info = r.gauge(
            "queued_runner_build_info",
            "Version of queued-runner (always 1).",
            &["version"],
        );
        info.set(&[env!("CARGO_PKG_VERSION")], 1.0);
        Self {
            http: r.counter(
                "queued_runner_http_requests_total",
                "HTTP requests by route pattern and status class.",
                &["method", "route", "status"],
            ),
            http_seconds: r.histogram(
                "queued_runner_http_request_duration_seconds",
                "Time until the response head (streamed exec bodies continue after it).",
                &[],
                metrics::HTTP_BUCKETS,
            ),
            exec_active: r.gauge(
                "queued_runner_exec_active",
                "Processes running now.",
                &["kind"],
            ),
            exec_total: r.counter(
                "queued_runner_exec_total",
                "Finished processes by kind and outcome.",
                &["kind", "outcome"],
            ),
            exec_seconds: r.histogram(
                "queued_runner_exec_duration_seconds",
                "Duration of processes.",
                &["kind"],
                metrics::RUN_BUCKETS,
            ),
            egress: r.counter(
                "queued_runner_egress_requests_total",
                "Decisions of the egress proxy.",
                &["decision"],
            ),
            registry: r,
        }
    }

    pub fn exec_started(&self, name: &str) -> RunTrack {
        let kind = exec_kind(name);
        self.exec_active.add(&[kind], 1.0);
        RunTrack {
            kind,
            started: Instant::now(),
            outcome: "aborted",
        }
    }

    pub fn on_frame(&self, run: &mut RunTrack, f: &ExecFrame) {
        match f {
            ExecFrame::Egress { verdict, .. } => self.egress.inc(&[if verdict.allowed() {
                "allowed"
            } else {
                "refused"
            }]),
            ExecFrame::Exit {
                success,
                timed_out,
                cancelled,
                stalled,
                ..
            } => {
                run.outcome = if *cancelled {
                    "cancelled"
                } else if *timed_out {
                    "timeout"
                } else if *stalled {
                    "stalled"
                } else if *success {
                    "success"
                } else {
                    "failure"
                }
            }
            ExecFrame::Fail { .. } => run.outcome = "error",
            _ => {}
        }
    }

    pub fn exec_ended(&self, run: RunTrack) {
        self.exec_active.add(&[run.kind], -1.0);
        self.exec_total.inc(&[run.kind, run.outcome]);
        self.exec_seconds
            .observe(&[run.kind], run.started.elapsed().as_secs_f64());
    }
}

/// Counts every request by route pattern (unmatched paths share one label).
pub async fn track(State(st): State<RunnerState>, req: Request, next: Next) -> Response {
    let method = metrics::method_label(req.method().as_str());
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let t = Instant::now();
    let resp = next.run(req).await;
    let m = &st.metrics;
    m.http.inc(&[
        method,
        &route,
        metrics::status_class(resp.status().as_u16()),
    ]);
    m.http_seconds.observe(&[], t.elapsed().as_secs_f64());
    resp
}

/// `GET /metrics` – only with the metrics token (or on a loopback-only
/// metrics listener without token). Never accepts `RUNNER_TOKEN`.
pub async fn serve(State(st): State<RunnerState>, req: Request) -> Response {
    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !st.cfg.metrics.allows(auth) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    (
        [(header::CONTENT_TYPE, metrics::CONTENT_TYPE)],
        st.metrics.registry.render(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_the_label_suffix_never_the_task_id() {
        assert_eq!(exec_kind("q-0123456789ab-agent"), "agent");
        assert_eq!(exec_kind("q-0123456789ab-verify"), "verify");
        assert_eq!(exec_kind("eval-x-0123456789abcdef"), "other");
        assert_eq!(exec_kind("q-0123456789ab"), "other");
    }
}
