//! WP12: preview settings, live previews and screenshots (see `crate::preview`).
use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    Json,
};
use queued_api::preview::{PreviewConfig, PreviewSettings, PreviewView};
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{require, Role};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    queue,
    state::AppState,
};

/// `GET /api/projects/{id}/preview` (members).
pub async fn get_settings(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PreviewConfig>> {
    require(&st, id, u.id, Role::Viewer).await?;
    Ok(Json(PreviewConfig {
        settings: crate::preview::settings(&st, id).await?,
        live_available: st.cfg.preview_origin.is_some(),
    }))
}

/// `PUT /api/projects/{id}/preview` (owner, editors).
pub async fn put_settings(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(s): Json<PreviewSettings>,
) -> ApiResult<Json<PreviewConfig>> {
    require(&st, id, u.id, Role::Editor).await?;
    s.validate().map_err(AppError::bad_request)?;
    crate::preview::save_settings(&st, id, &s).await?;
    crate::audit::log(
        &st,
        &u,
        "project.updated",
        ("project", &id.to_string()),
        Some(id),
        json!({"preview": {"enabled": s.enabled, "screenshots": s.screenshots}}),
    )
    .await;
    Ok(Json(PreviewConfig {
        settings: crate::preview::settings(&st, id).await?,
        live_available: st.cfg.preview_origin.is_some(),
    }))
}

async fn task_for(st: &AppState, id: Uuid, user: Uuid, min: Role) -> ApiResult<crate::models::Task> {
    let t = queue::load_task(st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    require(st, t.project_id, user, min).await?;
    Ok(t)
}

/// `POST /api/tasks/{id}/preview`: starts a live preview (owner, editors).
pub async fn start(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PreviewView>> {
    let t = task_for(&st, id, u.id, Role::Editor).await?;
    Ok(Json(
        crate::preview::start(&st, &t, u.id)
            .await?
            .map_err(AppError::bad_request)?,
    ))
}

/// `GET /api/tasks/{id}/preview`: the running preview with its link
/// (members only), or `null`.
pub async fn current(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Option<PreviewView>>> {
    let t = task_for(&st, id, u.id, Role::Viewer).await?;
    Ok(Json(crate::preview::current(&st, t.id).await?))
}

/// `DELETE /api/tasks/{id}/preview` (owner, editors).
pub async fn stop(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = task_for(&st, id, u.id, Role::Editor).await?;
    crate::preview::stop_for_task(&st, t.id).await;
    queue::event(&st, &t, "status", "Vorschau beendet").await;
    Ok(Json(json!({"ok": true})))
}

/// `GET /api/tasks/{id}/screenshots/{sid}`: the PNG (members). Served as an
/// image only: `nosniff`, a sandbox policy, never inline HTML.
pub async fn screenshot(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, sid)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let t = task_for(&st, id, u.id, Role::Viewer).await?;
    let png: Vec<u8> =
        sqlx::query_scalar("SELECT png FROM task_screenshots WHERE id=$1 AND task_id=$2")
            .bind(sid)
            .bind(t.id)
            .fetch_optional(&st.db)
            .await?
            .ok_or_else(AppError::not_found)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'"),
            (header::CACHE_CONTROL, "private, max-age=3600"),
        ],
        png,
    ))
}
