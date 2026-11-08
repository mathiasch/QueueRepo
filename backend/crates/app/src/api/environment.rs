//! `/api/projects/{id}/environment` and `/secrets` (WP02).
//!
//! Reading is open to every member (secret *values* never leave the backend),
//! changes are owner-only: the network policy and the secrets are a security
//! boundary of the project (like loosening the data residency in WP13).
use axum::{
    extract::{Path, State},
    Json,
};
use queued_api::env::{self, EnvironmentView, SetSecret, UpdateEnvironment};
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{load, require, runner_err, Role};
use crate::{
    auth::AuthUser,
    crypto, environment,
    error::{ApiResult, AppError},
    state::AppState,
};

async fn view(st: &AppState, id: Uuid) -> ApiResult<EnvironmentView> {
    let project = load(st, id).await?;
    let e = environment::load(&st.db, id).await?;
    // Hosts of the project's default model access (what the agent phase
    // always reaches). Best effort: without an access the list is empty.
    let provider_hosts = provider_hosts(st, &project).await;
    Ok(EnvironmentView {
        setup_script: e.setup_script,
        setup_timeout_secs: e.setup_timeout_secs,
        env_vars: e.env_vars,
        secrets: environment::secret_infos(&st.db, id).await?,
        setup_network: e.setup_network,
        agent_network: e.agent_network,
        cache_enabled: e.cache_enabled,
        cache_files: e.cache_files,
        updated_at: e.updated_at,
        sandbox: environment::sandbox(st).await,
        provider_hosts,
    })
}

/// Model API hosts for the owner's default provider (legacy credential) and
/// the project's routing target, if any.
async fn provider_hosts(st: &AppState, p: &crate::models::Project) -> Vec<String> {
    let default: Option<String> =
        sqlx::query_scalar("SELECT default_provider FROM user_settings WHERE user_id=$1")
            .bind(p.owner_id)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten();
    let target = p
        .routing()
        .and_then(|r| r.execute.target.and_then(|t| t.connector_id));
    let mut hosts = vec![];
    if let Some(cid) = target {
        if let Ok(Some((kind, config))) =
            sqlx::query_as::<_, (String, Value)>("SELECT kind, config FROM connectors WHERE id=$1")
                .bind(cid)
                .fetch_optional(&st.db)
                .await
        {
            let cred = crate::agent::Credential {
                kind: format!("connector:{kind}"),
                secret: String::new(),
                conn: Some(crate::agent::ConnCfg {
                    id: Some(cid),
                    kind: kind.clone(),
                    label: String::new(),
                    config,
                    flex: false,
                }),
            };
            let harness = cred
                .conn
                .as_ref()
                .map(|c| crate::connectors::harness(&kind, &c.config))
                .unwrap_or_default();
            hosts.extend(crate::agent::egress_hosts(&harness, &cred));
        }
    }
    if let Some(provider) = default {
        let kind: Option<String> = sqlx::query_scalar(
            "SELECT kind FROM provider_credentials WHERE user_id=$1 AND provider=$2",
        )
        .bind(p.owner_id)
        .bind(&provider)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
        let cred = crate::agent::Credential::legacy(kind.unwrap_or_default(), "");
        for h in crate::agent::egress_hosts(&provider, &cred) {
            if !hosts.contains(&h) {
                hosts.push(h);
            }
        }
    }
    hosts
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EnvironmentView>> {
    require(&st, id, u.id, Role::Viewer).await?;
    Ok(Json(view(&st, id).await?))
}

pub async fn put(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateEnvironment>,
) -> ApiResult<Json<EnvironmentView>> {
    require(&st, id, u.id, Role::Owner).await?;
    let valid = env::validate(&body).map_err(AppError::bad_request)?;
    environment::save(&st.db, id, &valid).await?;
    Ok(Json(view(&st, id).await?))
}

pub async fn put_secret(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<SetSecret>,
) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Owner).await?;
    env::check_name(&name, true).map_err(AppError::bad_request)?;
    env::check_secret_value(&body.value).map_err(AppError::bad_request)?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM project_secrets WHERE project_id=$1 AND name<>$2")
            .bind(id)
            .bind(&name)
            .fetch_one(&st.db)
            .await?;
    if count as usize >= env::MAX_SECRETS {
        return Err(AppError::bad_request(format!(
            "höchstens {} Geheimnisse pro Projekt",
            env::MAX_SECRETS
        )));
    }
    let enc = crypto::encrypt(&st.cfg.master_key, body.value.as_bytes())?;
    sqlx::query(
        "INSERT INTO project_secrets (project_id, name, value_enc) VALUES ($1,$2,$3)
         ON CONFLICT (project_id, name) DO UPDATE SET value_enc=EXCLUDED.value_enc, updated_at=now()",
    )
    .bind(id)
    .bind(&name)
    .bind(enc)
    .execute(&st.db)
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn delete_secret(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, name)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Owner).await?;
    let n = sqlx::query("DELETE FROM project_secrets WHERE project_id=$1 AND name=$2")
        .bind(id)
        .bind(&name)
        .execute(&st.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(Json(json!({"ok": true})))
}

/// Next setup starts with an empty cache. Editors may do this (it only
/// costs time, it widens nothing).
pub async fn clear_cache(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require(&st, id, u.id, Role::Editor).await?;
    st.runner.clear_cache(id).await.map_err(runner_err)?;
    Ok(Json(json!({"ok": true})))
}
