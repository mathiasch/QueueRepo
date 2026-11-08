//! Eval set endpoints (WP13).
use axum::{
    extract::{Path, State},
    Json,
};
use queued_api::{EvalCaseInfo, EvalRunView, StartEval};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    eval,
    state::AppState,
};

pub async fn cases(AuthUser(_u): AuthUser) -> Json<Vec<EvalCaseInfo>> {
    Json(eval::infos())
}

pub async fn start(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(s): Json<StartEval>,
) -> ApiResult<Json<EvalRunView>> {
    let id = eval::start(&st, u.id, s)
        .await?
        .map_err(AppError::bad_request)?;
    Ok(Json(
        eval::view(&st, u.id, id)
            .await?
            .ok_or_else(AppError::not_found)?,
    ))
}

pub async fn list(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<EvalRunView>>> {
    Ok(Json(eval::list(&st, u.id).await?))
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EvalRunView>> {
    Ok(Json(
        eval::view(&st, u.id, id)
            .await?
            .ok_or_else(AppError::not_found)?,
    ))
}

pub async fn remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    match eval::delete(&st, u.id, id).await {
        Ok(true) => Ok(Json(json!({"ok": true}))),
        Ok(false) => Err(AppError::not_found()),
        Err(e) if e.to_string() == "running" => {
            Err(AppError::conflict("Der Testlauf arbeitet noch"))
        }
        Err(e) => Err(e.into()),
    }
}
