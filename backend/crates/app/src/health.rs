//! WP10: liveness (`/healthz`) and readiness (`/readyz`).
//!
//! `/readyz` checks the database and the runner (with the runner token, so a
//! wrong token is "not ready" as well), 2 s each, in parallel. The answer
//! names the checks with `ok`/`fail` only – no error text, address or
//! version leaves the server; details go to the log. Results are cached for
//! one second and computed by one caller at a time, so an unauthenticated
//! flood cannot hammer the database or the runner.
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::state::AppState;

pub const CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const CACHE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub database: bool,
    pub runner: bool,
}

impl Readiness {
    pub fn ready(&self) -> bool {
        self.database && self.runner
    }
}

pub type ReadyCache = tokio::sync::Mutex<Option<(Instant, Readiness)>>;

/// `GET /healthz`: the process serves requests (no dependency checked).
pub async fn healthz() -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"status": "ok"})),
    )
        .into_response()
}

async fn check(st: &AppState) -> Readiness {
    let db = async {
        match tokio::time::timeout(
            CHECK_TIMEOUT,
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&st.db),
        )
        .await
        {
            Ok(Ok(_)) => true,
            Ok(Err(e)) => {
                tracing::warn!("readiness: database: {e}");
                false
            }
            Err(_) => {
                tracing::warn!("readiness: database: timeout");
                false
            }
        }
    };
    let runner = async {
        match tokio::time::timeout(CHECK_TIMEOUT, st.fleet.builtin().info()).await {
            Ok(Ok(_)) => true,
            Ok(Err(e)) => {
                tracing::warn!("readiness: runner: {e:#}");
                false
            }
            Err(_) => {
                tracing::warn!("readiness: runner: timeout");
                false
            }
        }
    };
    let (database, runner) = tokio::join!(db, runner);
    Readiness { database, runner }
}

/// Current readiness (cached for one second, single flight).
pub async fn readiness(st: &AppState) -> Readiness {
    let mut c = st.ready.lock().await;
    if let Some((t, r)) = *c {
        if t.elapsed() < CACHE {
            return r;
        }
    }
    let r = check(st).await;
    st.metrics
        .runner_up
        .set(&[], if r.runner { 1.0 } else { 0.0 });
    st.metrics
        .database_up
        .set(&[], if r.database { 1.0 } else { 0.0 });
    *c = Some((Instant::now(), r));
    r
}

/// `GET /readyz`.
pub async fn readyz(State(st): State<AppState>) -> Response {
    let r = readiness(&st).await;
    let word = |ok: bool| if ok { "ok" } else { "fail" };
    let body = json!({
        "status": if r.ready() { "ready" } else { "not_ready" },
        "checks": {"database": word(r.database), "runner": word(r.runner)},
    });
    let code = if r.ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, [(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}
