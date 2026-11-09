mod credentials;
mod environment;
mod evals;
mod gate;
pub mod limits;
mod projects;
mod providers;
mod tasks;

use std::convert::Infallible;

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post, put},
    Json, Router,
};
use futures::Stream;
use queued_api::{
    Meta, Notification, OidcProvider, PushKey, ServerEvent, SettingsUpdate, ONEMPTY_ACTIONS,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
};
use uuid::Uuid;

use crate::{
    auth::{self, AuthUser},
    budget,
    error::{ApiResult, AppError},
    models::Settings,
    queue,
    state::AppState,
};

pub fn router(st: AppState) -> Router {
    let api = Router::new()
        .route("/meta", get(meta))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/oidc/{provider}/start", get(auth::oidc_start))
        .route(
            "/auth/oidc/{provider}/callback",
            get(auth::oidc_callback_get).post(auth::oidc_callback_post),
        )
        .route("/settings", get(get_settings).put(put_settings))
        .route("/limits", get(get_limits))
        .route("/limits/{provider}/resume", post(resume_provider))
        .route("/budgets/{provider}", put(put_budget))
        .route("/credentials", get(credentials::list))
        .route(
            "/credentials/{provider}",
            put(credentials::put).delete(credentials::remove),
        )
        .route("/connectors", get(providers::list).post(providers::create))
        .route(
            "/connectors/{id}",
            axum::routing::patch(providers::update).delete(providers::remove),
        )
        .route("/connectors/{id}/check", post(providers::check))
        .route("/connectors/{id}/resume", post(providers::resume))
        .route("/prices", get(providers::prices))
        .route(
            "/admin/prices",
            get(providers::get_overrides).put(providers::put_overrides),
        )
        .route("/admin/prices/history", get(providers::override_history))
        .route("/estimate", post(providers::estimate))
        .route("/costs", get(providers::costs))
        .route("/cli-status", get(providers::cli_status))
        .route("/projects/{id}/presets", get(providers::presets))
        .route("/projects/{id}/routing", put(providers::put_routing))
        .route("/evals/cases", get(evals::cases))
        .route("/evals", get(evals::list).post(evals::start))
        .route("/evals/{id}", get(evals::get).delete(evals::remove))
        .route("/notifications", get(list_notifications))
        .route("/notifications/read-all", post(read_all))
        .route("/notifications/{id}/read", post(read_one))
        .route("/push/key", get(push_key))
        .route("/push/subscribe", post(push_subscribe))
        .route("/push/test", post(push_test))
        .route("/events", get(events))
        .route("/projects", get(projects::list).post(projects::create))
        .route(
            "/projects/{id}",
            get(projects::get)
                .patch(projects::update)
                .delete(projects::remove),
        )
        .route(
            "/projects/{id}/members",
            get(projects::members).post(projects::add_member),
        )
        .route(
            "/projects/{id}/environment",
            get(environment::get).put(environment::put),
        )
        .route(
            "/projects/{id}/environment/cache/clear",
            post(environment::clear_cache),
        )
        .route(
            "/projects/{id}/secrets/{name}",
            put(environment::put_secret).delete(environment::delete_secret),
        )
        .route(
            "/projects/{id}/members/{user_id}",
            delete(projects::remove_member),
        )
        .route("/projects/{id}/files", get(projects::files))
        .route("/projects/{id}/file", get(projects::file))
        .route("/projects/{id}/zip", get(projects::zip))
        .route(
            "/projects/{id}/import",
            post(projects::import).layer(DefaultBodyLimit::max(512 * 1024 * 1024)),
        )
        .route("/projects/{id}/tasks", get(projects::tasks))
        .route("/projects/{id}/commits", get(projects::commits))
        .route("/projects/{id}/merge-queue", get(projects::merge_queue))
        .route("/projects/{id}/push", post(projects::push))
        .route("/tasks", get(tasks::list).post(tasks::create))
        .route("/tasks/approve-all", post(tasks::approve_all))
        .route("/tasks/{id}", get(tasks::get).patch(tasks::update))
        .route("/tasks/{id}/cancel", post(tasks::cancel))
        .route("/tasks/{id}/retry", post(tasks::retry))
        .route("/tasks/{id}/approve", post(tasks::approve))
        .route("/tasks/{id}/answer", post(tasks::answer))
        .route("/tasks/{id}/move", post(tasks::move_task))
        .route("/tasks/{id}/diff", get(tasks::diff))
        .route("/tasks/{id}/revert", post(tasks::revert))
        .route("/tasks/{id}/gate/approve", post(gate::approve))
        .route("/tasks/{id}/gate/reject", post(gate::reject))
        .route("/tasks/{id}/gate/diff", get(gate::diff))
        .route("/projects/{id}/protection", get(gate::get).put(gate::put))
        // Unknown API paths get a JSON 404 instead of the SPA's index.html.
        .fallback(|| async { AppError::not_found() });

    let dir = st.cfg.frontend_dir.clone();
    let spa = ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")));
    // The WebAssembly frontend is ~1.5 MB; gzip only the static files (the
    // API and its SSE stream stay uncompressed).
    let web = Router::new()
        .fallback_service(spa)
        .layer(CompressionLayer::new());
    Router::new()
        .nest("/api", api)
        .fallback_service(web)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(st)
}

async fn meta(State(st): State<AppState>) -> Json<Meta> {
    let providers = st.cfg.providers();
    Json(Meta {
        providers: providers.iter().map(|p| p.to_string()).collect(),
        credential_kinds: providers
            .iter()
            .map(|p| {
                let kinds = crate::agent::credential_kinds(p);
                (p.to_string(), kinds.iter().map(|k| k.to_string()).collect())
            })
            .collect(),
        oidc: st
            .cfg
            .oidc
            .iter()
            .map(|p| OidcProvider {
                id: p.id.clone(),
                name: p.name.clone(),
            })
            .collect(),
        allow_registration: st.cfg.allow_registration,
        provider_capabilities: providers
            .iter()
            .map(|p| (p.to_string(), crate::agent::capabilities(p)))
            .collect(),
        cli_versions: crate::agent::CLI_VERSIONS
            .iter()
            .map(|(p, v)| (p.to_string(), v.to_string()))
            .collect(),
        connector_types: crate::connectors::types(st.cfg.enable_mock_provider),
        cli_notes: crate::agent::cli_notes(),
        price_catalog_version: crate::pricing::builtin().version.clone(),
    })
}

/// Lifts an auth pause and a detected subscription limit of `provider`.
async fn resume_provider(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(provider): Path<String>,
) -> ApiResult<Json<budget::ProviderStatus>> {
    if !st.cfg.providers().contains(&provider.as_str()) {
        return Err(AppError::bad_request("unknown provider"));
    }
    budget::clear_pause(&st.db, u.id, &provider, true).await?;
    st.wake.notify_one();
    Ok(Json(budget::status(&st.db, u.id, &provider).await?))
}

async fn get_settings(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Settings>> {
    Ok(Json(queue::load_settings(&st, u.id).await?))
}

async fn put_settings(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(s): Json<SettingsUpdate>,
) -> ApiResult<Json<Settings>> {
    if !st.cfg.providers().contains(&s.default_provider.as_str())
        || crate::agent::credential_kinds(&s.default_provider).is_empty()
    {
        return Err(AppError::bad_request("unknown provider"));
    }
    let actions: Vec<String> = s
        .onempty_actions
        .into_iter()
        .filter(|a| ONEMPTY_ACTIONS.contains(&a.as_str()))
        .collect();
    queue::load_settings(&st, u.id).await?;
    sqlx::query(
        "UPDATE user_settings SET parallelism=$2, default_provider=$3, onempty_enabled=$4, onempty_actions=$5,
                onempty_cooldown_minutes=$6, max_auto_tasks=$7, max_fix_attempts=$8, ntfy_url=$9 WHERE user_id=$1",
    )
    .bind(u.id)
    .bind(s.parallelism.clamp(1, 16))
    .bind(&s.default_provider)
    .bind(s.onempty_enabled)
    .bind(actions)
    .bind(s.onempty_cooldown_minutes.clamp(5, 7 * 24 * 60))
    .bind(s.max_auto_tasks.clamp(0, 20))
    .bind(s.max_fix_attempts.clamp(0, 10))
    .bind(s.ntfy_url.filter(|u| !u.trim().is_empty()))
    .execute(&st.db)
    .await?;
    st.wake.notify_one();
    Ok(Json(queue::load_settings(&st, u.id).await?))
}

async fn get_limits(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<budget::ProviderStatus>>> {
    let mut out = vec![];
    for p in st.cfg.providers() {
        out.push(budget::status(&st.db, u.id, p).await?);
    }
    Ok(Json(out))
}

async fn put_budget(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(provider): Path<String>,
    Json(b): Json<budget::Budget>,
) -> ApiResult<Json<budget::ProviderStatus>> {
    if !st.cfg.providers().contains(&provider.as_str()) {
        return Err(AppError::bad_request("unknown provider"));
    }
    budget::set_budget(&st.db, u.id, &provider, &b).await?;
    st.wake.notify_one();
    Ok(Json(budget::status(&st.db, u.id, &provider).await?))
}

async fn list_notifications(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<Notification>>> {
    let rows: Vec<(Uuid, String, String, String, Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT id, kind, title, body, task_id, read_at, created_at FROM notifications WHERE user_id=$1 ORDER BY created_at DESC LIMIT 100",
        )
        .bind(u.id)
        .fetch_all(&st.db)
        .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(id, kind, title, body, task_id, read_at, created_at)| Notification {
                    id,
                    kind,
                    title,
                    body,
                    task_id,
                    read_at,
                    created_at,
                },
            )
            .collect(),
    ))
}

async fn read_all(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Value>> {
    sqlx::query("UPDATE notifications SET read_at=now() WHERE user_id=$1 AND read_at IS NULL")
        .bind(u.id)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

async fn read_one(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    sqlx::query("UPDATE notifications SET read_at=now() WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(u.id)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

async fn push_key(State(st): State<AppState>) -> Json<PushKey> {
    Json(PushKey {
        public_key: st.vapid.public_b64.clone(),
    })
}

#[derive(Deserialize)]
struct PushKeys {
    p256dh: String,
    auth: String,
}
#[derive(Deserialize)]
struct PushSub {
    endpoint: String,
    keys: PushKeys,
}

async fn push_subscribe(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(s): Json<PushSub>,
) -> ApiResult<Json<Value>> {
    if !s.endpoint.starts_with("https://") {
        return Err(AppError::bad_request("invalid endpoint"));
    }
    sqlx::query(
        "INSERT INTO push_subscriptions (id, user_id, endpoint, p256dh, auth) VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (endpoint) DO UPDATE SET user_id=EXCLUDED.user_id, p256dh=EXCLUDED.p256dh, auth=EXCLUDED.auth",
    )
    .bind(Uuid::new_v4())
    .bind(u.id)
    .bind(&s.endpoint)
    .bind(&s.keys.p256dh)
    .bind(&s.keys.auth)
    .execute(&st.db)
    .await?;
    Ok(Json(json!({"ok": true})))
}

async fn push_test(State(st): State<AppState>, AuthUser(u): AuthUser) -> Json<Value> {
    crate::notify::notify(
        &st,
        u.id,
        crate::notify::Note {
            kind: "test",
            title: "Test-Benachrichtigung".into(),
            body: "Push funktioniert 🎉".into(),
            task_id: None,
            push: true,
        },
    )
    .await;
    Json(json!({"ok": true}))
}

fn sse_data(e: &ServerEvent) -> Event {
    // Serializing these plain data types cannot fail.
    Event::default().data(serde_json::to_string(e).unwrap_or_default())
}

async fn events(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let uid = u.id;
    let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(move |m| match m {
        Ok(ev) if ev.user_id == uid => Some(Ok(sse_data(&ev.payload))),
        Ok(_) => None,
        // Lagged: tell the client to refetch.
        Err(_) => Some(Ok(sse_data(&ServerEvent::Resync))),
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
