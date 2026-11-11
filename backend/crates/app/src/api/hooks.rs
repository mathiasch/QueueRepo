//! WP07: `POST /api/hooks/{id}` – incoming webhooks (generic HMAC and
//! GitHub). No cookie, no token: the signature is the authentication.
//!
//! Order: body limit (413, by the router) → trigger exists and takes
//! webhooks (404) → signature, timestamp, delivery ID (401/400, nothing
//! stored) → enabled (409) → filters/dedupe/rate limit in one transaction.
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use queued_api::triggers::{
    RunOutcome, RunSource, TriggerKind, HEADER_DELIVERY, HEADER_SIGNATURE, HEADER_TIMESTAMP,
    WEBHOOK_TOLERANCE_SECS,
};
use uuid::Uuid;

use super::triggers::{fire_response, retry_after};
use crate::{
    crypto,
    error::AppError,
    state::AppState,
    triggers::{self, Firing, TriggerRow},
    webhooks::{self, Verdict},
};

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::trim)
}

/// A refused delivery (boxed: a `Response` is large).
type Refused = Box<Response>;

fn unauthorized(msg: &str) -> Refused {
    Box::new(AppError(StatusCode::UNAUTHORIZED, msg.into()).into_response())
}

fn refused(e: AppError) -> Refused {
    Box::new(e.into_response())
}

pub async fn receive(
    State(st): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match receive_inner(&st, &id, &headers, &body).await {
        Ok(r) => r,
        Err(e) => AppError::from(e).into_response(),
    }
}

async fn receive_inner(
    st: &AppState,
    id: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> anyhow::Result<Response> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(AppError::not_found().into_response());
    };
    let Some(t) = sqlx::query_as::<_, TriggerRow>("SELECT * FROM triggers WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
    else {
        return Ok(AppError::not_found().into_response());
    };
    let kind = t.kind();
    let Some(enc) = t.secret_enc.as_deref().filter(|_| kind.has_secret()) else {
        return Ok(AppError::not_found().into_response());
    };
    let secret = crypto::decrypt(&st.cfg.master_key, enc)?;
    let firing = match kind {
        TriggerKind::Github => match github(st, &t, &secret, headers, body).await? {
            Ok(f) => f,
            Err(resp) => return Ok(*resp),
        },
        _ => match generic(&t, &secret, headers, body) {
            Ok(f) => f,
            Err(resp) => return Ok(*resp),
        },
    };
    if !t.enabled {
        return Ok(AppError::conflict("Auslöser ist ausgeschaltet").into_response());
    }
    let fired = match firing {
        Hook::Fire(f) => triggers::fire(st, t.id, f).await?,
        Hook::Record(f, outcome, reason) => {
            // Ping / filtered event: one history row (dedupe keys kept),
            // no task.
            let mut tx = st.db.begin().await?;
            let row: Option<TriggerRow> =
                sqlx::query_as("SELECT * FROM triggers WHERE id=$1 AND enabled FOR UPDATE")
                    .bind(t.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some(row) = row else {
                return Ok(AppError::conflict("Auslöser ist ausgeschaltet").into_response());
            };
            let r = triggers::record_outcome(&mut tx, &row, &f, outcome, &reason).await?;
            tx.commit().await?;
            Some(r)
        }
    };
    let retry = retry_after(st, t.id).await;
    Ok(fire_response(fired, retry))
}

enum Hook {
    Fire(Firing),
    Record(Firing, RunOutcome, String),
}

fn generic(
    t: &TriggerRow,
    secret: &[u8],
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Hook, Refused> {
    let (Some(ts), Some(delivery), Some(sig)) = (
        header(headers, HEADER_TIMESTAMP),
        header(headers, HEADER_DELIVERY),
        header(headers, HEADER_SIGNATURE),
    ) else {
        return Err(unauthorized(
            "signature headers missing (X-Queue-Timestamp, X-Queue-Delivery, X-Queue-Signature)",
        ));
    };
    if !webhooks::verify(secret, &webhooks::generic_parts(ts, delivery, body), sig) {
        return Err(unauthorized("invalid signature"));
    }
    // Signed values are checked after the signature (no oracle for
    // unsigned guesses).
    let Ok(at) = ts.parse::<i64>() else {
        return Err(
            AppError::bad_request("X-Queue-Timestamp must be Unix seconds").into_response(),
        );
    };
    if (chrono::Utc::now().timestamp() - at).abs() > WEBHOOK_TOLERANCE_SECS {
        return Err(unauthorized(
            "timestamp outside the allowed window (±300 s)",
        ));
    }
    if !webhooks::valid_delivery(delivery) {
        return Err(AppError::bad_request(
            "X-Queue-Delivery: 1–100 characters A-Z a-z 0-9 . _ : -",
        )
        .into_response());
    }
    Ok(Hook::Fire(Firing {
        source: Some(RunSource::Webhook),
        delivery_id: Some(delivery.to_string()),
        title_suffix: "Webhook".into(),
        data: Some((
            format!("The webhook trigger \"{}\" received a call.", t.name),
            vec![(format!("Aufruf {delivery}"), webhooks::generic_text(body))],
        )),
        ..Default::default()
    }))
}

async fn github(
    st: &AppState,
    t: &TriggerRow,
    secret: &[u8],
    headers: &HeaderMap,
    body: &[u8],
) -> anyhow::Result<Result<Hook, Refused>> {
    let Some(sig) = header(headers, "x-hub-signature-256") else {
        return Ok(Err(unauthorized("X-Hub-Signature-256 missing")));
    };
    if !webhooks::verify(secret, &[body], sig) {
        return Ok(Err(unauthorized("invalid signature")));
    }
    let (Some(event), Some(delivery)) = (
        header(headers, "x-github-event"),
        header(headers, "x-github-delivery"),
    ) else {
        return Ok(Err(AppError::bad_request(
            "X-GitHub-Event and X-GitHub-Delivery required",
        )
        .into_response()));
    };
    if !webhooks::valid_delivery(delivery) || !webhooks::valid_delivery(event) {
        return Ok(Err(
            AppError::bad_request("invalid GitHub headers").into_response()
        ));
    }
    let content_type = header(headers, "content-type").unwrap_or("application/json");
    let Some(payload) = webhooks::github_payload(content_type, body) else {
        return Ok(Err(
            AppError::bad_request("payload is not JSON").into_response()
        ));
    };
    let filter = t.github_filter().unwrap_or_default();
    let project: crate::models::Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(t.project_id)
        .fetch_one(&st.db)
        .await?;
    let repo = filter.repository.clone().or_else(|| {
        project.git_repo_path.clone().or_else(|| {
            project
                .git_remote_url
                .as_deref()
                .and_then(queued_api::pr::remote_parts)
                .map(|p| p.path)
        })
    });
    // WP05: an authentic event about one of Queue's pull requests only
    // makes the poller look now; the state still comes from the host API.
    if t.enabled && repo_matches(&payload, repo.as_deref()) {
        poke_pulls(st, t.project_id, event, &payload).await?;
    }
    let base = Firing {
        source: Some(RunSource::Github),
        event: Some(webhooks::event_label(event, &payload)),
        delivery_id: Some(delivery.to_string()),
        body_sha256: Some(crypto::sha256_hex_bytes(body)),
        ..Default::default()
    };
    Ok(Ok(
        match webhooks::github_verdict(event, &payload, &filter, repo.as_deref()) {
            Verdict::Ping => Hook::Record(
                base,
                RunOutcome::Ignored,
                "Ping von GitHub – Verbindung steht".into(),
            ),
            Verdict::Ignore(why) => Hook::Record(base, RunOutcome::Ignored, why),
            Verdict::Match(m) => Hook::Fire(Firing {
                dedupe_key: m.dedupe_key,
                title_suffix: m.title_suffix,
                data: Some((m.intro, m.entries)),
                ..base
            }),
        },
    ))
}

fn repo_matches(p: &serde_json::Value, repo: Option<&str>) -> bool {
    match repo {
        None => true,
        Some(r) => p
            .pointer("/repository/full_name")
            .and_then(|v| v.as_str())
            .is_some_and(|h| h.eq_ignore_ascii_case(r)),
    }
}

async fn poke_pulls(
    st: &AppState,
    project: Uuid,
    event: &str,
    payload: &serde_json::Value,
) -> anyhow::Result<()> {
    let (numbers, branches) = webhooks::pr_refs(event, payload);
    if numbers.is_empty() && branches.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM pull_requests WHERE project_id=$1 AND host='github' AND state IN ('creating','open')
           AND (number = ANY($2) OR head_branch = ANY($3))",
    )
    .bind(project)
    .bind(&numbers)
    .bind(&branches)
    .fetch_all(&st.db)
    .await?;
    for id in ids {
        crate::pr::poke(st, id).await?;
    }
    Ok(())
}
