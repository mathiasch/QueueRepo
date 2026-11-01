mod credentials;
mod projects;
mod tasks;

use std::convert::Infallible;

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post, put},
    Json, Router,
};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

use crate::{
    auth::{self, AuthUser},
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
        .route("/auth/oidc/{provider}/callback", get(auth::oidc_callback_get).post(auth::oidc_callback_post))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/limits", get(get_limits))
        .route("/credentials", get(credentials::list))
        .route("/credentials/{provider}", put(credentials::put).delete(credentials::remove))
        .route("/notifications", get(list_notifications))
        .route("/notifications/read-all", post(read_all))
        .route("/notifications/{id}/read", post(read_one))
        .route("/push/key", get(push_key))
        .route("/push/subscribe", post(push_subscribe))
        .route("/push/test", post(push_test))
        .route("/events", get(events))
        .route("/projects", get(projects::list).post(projects::create))
        .route("/projects/{id}", get(projects::get).patch(projects::update).delete(projects::remove))
        .route("/projects/{id}/members", get(projects::members).post(projects::add_member))
        .route("/projects/{id}/members/{user_id}", delete(projects::remove_member))
        .route("/projects/{id}/files", get(projects::files))
        .route("/projects/{id}/file", get(projects::file))
        .route("/projects/{id}/zip", get(projects::zip))
        .route("/projects/{id}/import", post(projects::import).layer(DefaultBodyLimit::max(512 * 1024 * 1024)))
        .route("/projects/{id}/tasks", get(projects::tasks))
        .route("/tasks", get(tasks::list).post(tasks::create))
        .route("/tasks/approve-all", post(tasks::approve_all))
        .route("/tasks/{id}", get(tasks::get).patch(tasks::update))
        .route("/tasks/{id}/cancel", post(tasks::cancel))
        .route("/tasks/{id}/retry", post(tasks::retry))
        .route("/tasks/{id}/approve", post(tasks::approve))
        .route("/tasks/{id}/answer", post(tasks::answer))
        .route("/tasks/{id}/move", post(tasks::move_task))
        .route("/tasks/{id}/diff", get(tasks::diff))
        .route("/tasks/{id}/revert", post(tasks::revert));

    let dir = st.cfg.frontend_dir.clone();
    let spa = ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")));
    Router::new()
        .nest("/api", api)
        .fallback_service(spa)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(st)
}

async fn meta(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "providers": st.cfg.providers(),
        "credential_kinds": st.cfg.providers().iter().map(|p| (p.to_string(), crate::agent::credential_kinds(p))).collect::<std::collections::HashMap<_, _>>(),
        "oidc": st.cfg.oidc.iter().map(|p| json!({"id": p.id, "name": p.name})).collect::<Vec<_>>(),
        "allow_registration": st.cfg.allow_registration,
    }))
}

async fn get_settings(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Settings>> {
    Ok(Json(queue::load_settings(&st, u.id).await?))
}

async fn put_settings(State(st): State<AppState>, AuthUser(u): AuthUser, Json(s): Json<Settings>) -> ApiResult<Json<Settings>> {
    if !st.cfg.providers().contains(&s.default_provider.as_str()) {
        return Err(AppError::bad_request("unknown provider"));
    }
    let actions: Vec<String> = s.onempty_actions.into_iter().filter(|a| ["review", "research", "improve"].contains(&a.as_str())).collect();
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

async fn get_limits(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(String, Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>, i64, i64, i64, f64, i32)> = sqlx::query_as(
        "SELECT provider,
                CASE WHEN limited_until > now() THEN limited_until END,
                CASE WHEN window_started_at > now() - interval '5 hours' THEN window_started_at END,
                input_tokens, output_tokens, cache_read_tokens, cost_usd, runs
         FROM provider_limits WHERE user_id=$1 ORDER BY provider",
    )
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(provider, limited_until, window, i, o, c, cost, runs)| {
            let active = window.is_some();
            json!({
                "provider": provider, "limited_until": limited_until, "window_started_at": window,
                "input_tokens": if active { i } else { 0 }, "output_tokens": if active { o } else { 0 },
                "cache_read_tokens": if active { c } else { 0 }, "cost_usd": if active { cost } else { 0.0 },
                "runs": if active { runs } else { 0 },
            })
        })
        .collect::<Vec<_>>())))
}

async fn list_notifications(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, String, String, String, Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT id, kind, title, body, task_id, read_at, created_at FROM notifications WHERE user_id=$1 ORDER BY created_at DESC LIMIT 100",
        )
        .bind(u.id)
        .fetch_all(&st.db)
        .await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(id, kind, title, body, task_id, read_at, created_at)| json!({
            "id": id, "kind": kind, "title": title, "body": body, "task_id": task_id, "read_at": read_at, "created_at": created_at
        }))
        .collect::<Vec<_>>())))
}

async fn read_all(State(st): State<AppState>, AuthUser(u): AuthUser) -> ApiResult<Json<Value>> {
    sqlx::query("UPDATE notifications SET read_at=now() WHERE user_id=$1 AND read_at IS NULL").bind(u.id).execute(&st.db).await?;
    Ok(Json(json!({"ok": true})))
}

async fn read_one(State(st): State<AppState>, AuthUser(u): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    sqlx::query("UPDATE notifications SET read_at=now() WHERE id=$1 AND user_id=$2").bind(id).bind(u.id).execute(&st.db).await?;
    Ok(Json(json!({"ok": true})))
}

async fn push_key(State(st): State<AppState>) -> Json<Value> {
    Json(json!({"public_key": st.vapid.public_b64}))
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

async fn push_subscribe(State(st): State<AppState>, AuthUser(u): AuthUser, Json(s): Json<PushSub>) -> ApiResult<Json<Value>> {
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
    crate::notify::notify(&st, u.id, crate::notify::Note {
        kind: "test",
        title: "Test-Benachrichtigung".into(),
        body: "Push funktioniert 🎉".into(),
        task_id: None,
        push: true,
    })
    .await;
    Json(json!({"ok": true}))
}

async fn events(State(st): State<AppState>, AuthUser(u): AuthUser) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let uid = u.id;
    let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(move |m| match m {
        Ok(ev) if ev.user_id == uid => Some(Ok(Event::default().data(ev.payload.to_string()))),
        Ok(_) => None,
        // Lagged: tell the client to refetch.
        Err(_) => Some(Ok(Event::default().data(r#"{"type":"resync"}"#))),
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
