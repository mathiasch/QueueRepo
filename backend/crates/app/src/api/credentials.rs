use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    agent::credential_kinds,
    auth::AuthUser,
    error::{ApiResult, AppError},
    queue,
    state::AppState,
};

/// Secrets are write-only: only provider/kind/updated_at are ever returned.
pub async fn list(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT provider, kind, updated_at FROM provider_credentials WHERE user_id=$1 ORDER BY provider")
            .bind(u.id)
            .fetch_all(&st.db)
            .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(p, k, t)| json!({"provider": p, "kind": k, "updated_at": t}))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
pub struct PutCred {
    kind: String,
    secret: String,
}

pub async fn put(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(provider): Path<String>,
    Json(c): Json<PutCred>,
) -> ApiResult<Json<Value>> {
    if !credential_kinds(&provider).contains(&c.kind.as_str()) {
        return Err(AppError::bad_request(format!(
            "invalid kind for {provider}"
        )));
    }
    let secret = c.secret.trim();
    if secret.is_empty() {
        return Err(AppError::bad_request("secret required"));
    }
    if c.kind.ends_with("_json") || c.kind == "oauth_creds" {
        serde_json::from_str::<Value>(secret)
            .map_err(|_| AppError::bad_request("expected the JSON file content"))?;
    }
    queue::store_credential(&st, u.id, &provider, &c.kind, secret).await?;
    // New credentials lift a pause caused by an authentication/billing failure.
    crate::budget::clear_pause(&st.db, u.id, &provider, false).await?;
    // Clear a stale limit and let the scheduler retry blocked work.
    sqlx::query("UPDATE tasks SET status='queued', error=NULL, failure_class=NULL, updated_at=now() WHERE user_id=$1 AND provider=$2 AND status='blocked' AND error LIKE 'Keine Zugangsdaten%'")
        .bind(u.id)
        .bind(&provider)
        .execute(&st.db)
        .await?;
    st.wake.notify_one();
    Ok(Json(json!({"ok": true})))
}

pub async fn remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<Value>> {
    sqlx::query("DELETE FROM provider_credentials WHERE user_id=$1 AND provider=$2")
        .bind(u.id)
        .bind(provider)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}
