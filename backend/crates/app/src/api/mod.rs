pub(crate) mod account;
pub(crate) mod admin;
mod analytics;
mod attachments;
mod chooser;
mod credentials;
mod environment;
mod evals;
mod gate;
mod hooks;
mod intake;
mod knowledge;
pub mod limits;
mod notifications;
mod preview;
pub(crate) mod projects;
mod providers;
mod pulls;
pub(crate) mod steer;
mod tasks;
pub(crate) mod tokens;
mod triggers;

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
        // WP11: second login step, invitations.
        .route("/auth/mfa", get(auth::mfa_pending))
        .route("/auth/mfa/totp", post(auth::mfa_totp))
        .route("/auth/mfa/recovery", post(auth::mfa_recovery))
        .route("/auth/mfa/webauthn/start", post(auth::mfa_webauthn_start))
        .route("/auth/mfa/webauthn/finish", post(auth::mfa_webauthn_finish))
        .route("/auth/invite/{token}", get(auth::invite_preview))
        // WP11: account security (cookie only).
        .route("/account/security", get(account::security))
        .route("/account/password", post(account::change_password))
        .route("/account/mfa/totp/setup", post(account::totp_setup))
        .route("/account/mfa/totp/confirm", post(account::totp_confirm))
        .route("/account/mfa/totp/disable", post(account::totp_disable))
        .route("/account/mfa/recovery", post(account::regenerate_recovery))
        .route(
            "/account/webauthn/register/start",
            post(account::passkey_start),
        )
        .route(
            "/account/webauthn/register/finish",
            post(account::passkey_finish),
        )
        .route(
            "/account/webauthn/{id}/remove",
            post(account::passkey_remove),
        )
        .route("/account/sessions", get(account::sessions))
        .route(
            "/account/sessions/revoke-others",
            post(account::revoke_other_sessions),
        )
        .route("/account/sessions/{id}", delete(account::revoke_session))
        .route("/account/quota", get(account::quota))
        // WP11: admin console (cookie sessions of admins only).
        .route("/admin/users", get(admin::users))
        .route(
            "/admin/users/{id}",
            axum::routing::patch(admin::update_user),
        )
        .route("/admin/users/{id}/quota", put(admin::put_quota))
        .route(
            "/admin/invitations",
            get(admin::invitations).post(admin::create_invitation),
        )
        .route(
            "/admin/invitations/{id}/revoke",
            post(admin::revoke_invitation),
        )
        .route(
            "/admin/settings",
            get(admin::get_settings).put(admin::put_settings),
        )
        .route("/admin/audit", get(admin::audit_log))
        .route("/admin/audit/export", get(admin::audit_export))
        .route("/admin/audit/verify", get(admin::audit_verify))
        .route(
            "/admin/runners",
            get(admin::runners).post(admin::create_runner),
        )
        .route(
            "/admin/runners/{id}",
            axum::routing::patch(admin::update_runner).delete(admin::delete_runner),
        )
        .route(
            "/admin/runners/{id}/token",
            post(admin::rotate_runner_token),
        )
        .route("/admin/runners/{id}/revoke", post(admin::revoke_runner))
        .route("/admin/projects", get(admin::projects))
        .route("/admin/projects/{id}/move", post(admin::move_project))
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
        .route(
            "/projects/{id}/chooser",
            get(chooser::get_project).put(chooser::put_project),
        )
        .route("/projects/{id}/chooser/suggest", get(chooser::suggest))
        .route(
            "/chooser/prefs",
            get(chooser::get_prefs).put(chooser::put_prefs),
        )
        .route(
            "/tasks/{id}/feedback",
            get(chooser::get_feedback).post(chooser::post_feedback),
        )
        .route(
            "/admin/chooser",
            get(chooser::admin_view).put(chooser::admin_put),
        )
        .route("/admin/chooser/replay", post(chooser::admin_replay))
        .route("/evals/cases", get(evals::cases))
        .route("/evals", get(evals::list).post(evals::start))
        .route("/evals/{id}", get(evals::get).delete(evals::remove))
        .route("/notifications", get(list_notifications))
        // WP10: notification settings, analytics, session insights (cookie only).
        .route(
            "/notifications/settings",
            get(notifications::get).put(notifications::put),
        )
        .route(
            "/notifications/webhook/secret",
            post(notifications::renew_secret),
        )
        .route(
            "/notifications/webhook/test",
            post(notifications::test_webhook),
        )
        .route("/notifications/deliveries", get(notifications::deliveries))
        .route("/analytics", get(analytics::get))
        .route("/tasks/{id}/insights", get(analytics::insights))
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
        .route("/tasks/{id}/attempts", get(tasks::attempts))
        .route(
            "/tasks/{id}/preview",
            get(preview::current)
                .post(preview::start)
                .delete(preview::stop),
        )
        .route("/tasks/{id}/screenshots/{sid}", get(preview::screenshot))
        .route(
            "/projects/{id}/preview",
            get(preview::get_settings).put(preview::put_settings),
        )
        .route("/tasks/{id}/select", post(tasks::select))
        .route("/tasks/{id}/approve", post(tasks::approve))
        .route("/tasks/{id}/answer", post(steer::answer))
        .route("/tasks/{id}/move", post(tasks::move_task))
        // WP09: attachments, project rules, project state.
        .route(
            "/projects/{id}/attachments",
            post(attachments::upload).layer(DefaultBodyLimit::max(
                queued_api::memory::MAX_ATTACHMENT_BYTES + 64 * 1024,
            )),
        )
        .route(
            "/projects/{id}/attachments/drafts",
            get(attachments::drafts),
        )
        .route("/attachments/quota", get(attachments::quota))
        .route(
            "/attachments/{id}",
            get(attachments::download).delete(attachments::remove),
        )
        .route("/tasks/{id}/attachments", get(attachments::of_task))
        .route(
            "/projects/{id}/knowledge",
            get(knowledge::get).put(knowledge::save),
        )
        .route("/projects/{id}/knowledge/import", post(knowledge::import))
        .route("/projects/{id}/knowledge/export", post(knowledge::export))
        .route(
            "/projects/{id}/knowledge/{kid}/approve",
            post(knowledge::approve),
        )
        .route(
            "/projects/{id}/knowledge/{kid}/reject",
            post(knowledge::reject),
        )
        .route(
            "/projects/{id}/state",
            get(knowledge::get_state).put(knowledge::put_state),
        )
        .route("/tasks/{id}/diff", get(tasks::diff))
        .route("/tasks/{id}/revert", post(tasks::revert))
        .route("/tasks/{id}/plan/approve", post(steer::plan_approve))
        .route("/tasks/{id}/plan/revise", post(steer::plan_revise))
        .route("/tasks/{id}/plan/reject", post(steer::plan_reject))
        .route(
            "/tasks/{id}/messages",
            get(steer::messages).post(steer::send),
        )
        .route("/tasks/{id}/messages/{mid}", delete(steer::withdraw))
        .route("/tasks/{id}/continue", post(steer::continue_task))
        .route(
            "/tasks/{id}/comments",
            get(steer::comments).post(steer::add_comment),
        )
        .route("/tasks/{id}/comments/send", post(steer::send_comments))
        .route(
            "/tasks/{id}/comments/{cid}",
            axum::routing::patch(steer::update_comment).delete(steer::delete_comment),
        )
        .route("/tasks/{id}/gate/approve", post(gate::approve))
        .route("/tasks/{id}/gate/reject", post(gate::reject))
        .route("/tasks/{id}/gate/diff", get(gate::diff))
        .route("/projects/{id}/protection", get(gate::get).put(gate::put))
        .route(
            "/projects/{id}/delivery",
            get(pulls::get_delivery).put(pulls::put_delivery),
        )
        .route("/projects/{id}/delivery/check", post(pulls::check_delivery))
        .route("/projects/{id}/pulls", get(pulls::list))
        .route("/pulls/{id}", get(pulls::get))
        .route("/pulls/{id}/poll", post(pulls::poll_now))
        .route("/pulls/{id}/auto-fix", post(pulls::auto_fix))
        .route("/pulls/{id}/extra-round", post(pulls::extra_round))
        // WP07: API tokens (cookie only), templates, triggers, webhooks.
        .route("/tokens", get(tokens::list).post(tokens::create))
        .route("/tokens/{id}", delete(tokens::remove))
        .route("/tokens/{id}/revoke", post(tokens::revoke))
        .route("/whoami", get(tokens::whoami))
        .route(
            "/projects/{id}/templates",
            get(triggers::list_templates).post(triggers::create_template),
        )
        .route(
            "/templates/{id}",
            axum::routing::patch(triggers::update_template).delete(triggers::delete_template),
        )
        .route(
            "/projects/{id}/triggers",
            get(triggers::list_triggers).post(triggers::create_trigger),
        )
        .route("/triggers/preview", post(triggers::preview))
        .route("/timezones", get(triggers::timezones))
        .route(
            "/triggers/{id}",
            get(triggers::get_trigger)
                .patch(triggers::update_trigger)
                .delete(triggers::delete_trigger),
        )
        .route("/triggers/{id}/runs", get(triggers::runs))
        .route("/triggers/{id}/secret", post(triggers::renew_secret))
        .route("/triggers/{id}/fire", post(triggers::fire))
        .route(
            "/hooks/{id}",
            post(hooks::receive).layer(DefaultBodyLimit::max(
                queued_api::triggers::WEBHOOK_MAX_BODY,
            )),
        )
        // WP08: intake integrations (management cookie only, receive public).
        .route(
            "/projects/{id}/integrations",
            get(intake::list).post(intake::create),
        )
        .route(
            "/integrations/{id}",
            get(intake::get)
                .patch(intake::update)
                .delete(intake::remove),
        )
        .route("/integrations/{id}/secret", post(intake::renew_secret))
        .route("/integrations/{id}/events", get(intake::events))
        .route(
            "/integrations/{id}/identities",
            get(intake::identities).post(intake::add_identity),
        )
        .route(
            "/integrations/{id}/identities/{iid}",
            delete(intake::remove_identity),
        )
        .route(
            "/integrations/{id}/identities/{iid}/confirm",
            post(intake::confirm_identity),
        )
        .route(
            "/intake/{id}",
            post(intake::receive).layer(DefaultBodyLimit::max(queued_api::intake::MAX_BODY)),
        )
        // Unknown API paths get a JSON 404 instead of the SPA's index.html.
        .fallback(|| async { AppError::not_found() });

    let dir = st.cfg.frontend_dir.clone();
    let spa = ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")));
    // The WebAssembly frontend is ~1.5 MB; gzip only the static files (the
    // API and its SSE stream stay uncompressed).
    let web = Router::new()
        .fallback_service(spa)
        .layer(CompressionLayer::new());
    let mut root = Router::new()
        // WP10: health and metrics, before the SPA fallback.
        .route("/healthz", get(crate::health::healthz))
        .route("/readyz", get(crate::health::readyz));
    root = if st.cfg.metrics.on_main() {
        root.route("/metrics", get(crate::metrics::serve))
    } else {
        // Not enabled here: a plain 404, never the SPA's index.html.
        root.route("/metrics", get(|| async { AppError::not_found() }))
    };
    root.nest("/api", api)
        .fallback_service(web)
        // WP12: preview hosts (own origin) never reach the API or the SPA.
        .layer(axum::middleware::from_fn_with_state(
            st.clone(),
            crate::preview::layer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            st.clone(),
            crate::metrics::track,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(st)
}

async fn meta(State(st): State<AppState>) -> ApiResult<Json<Meta>> {
    let providers = st.cfg.providers();
    let mode = crate::instance::settings(&st).await?.registration_mode;
    Ok(Json(Meta {
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
        allow_registration: mode == queued_api::security::RegistrationMode::Open,
        registration_mode: mode,
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
    }))
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
    let cur = queue::load_settings(&st, u.id).await?;
    let threshold = s.dedupe_threshold.unwrap_or(cur.dedupe_threshold);
    let review = s
        .dedupe_review_threshold
        .unwrap_or(cur.dedupe_review_threshold);
    let days = s.dedupe_window_days.unwrap_or(cur.dedupe_window_days);
    queued_api::memory::validate_dedupe(threshold, review, days).map_err(AppError::bad_request)?;
    sqlx::query(
        "UPDATE user_settings SET dedupe_threshold=$2, dedupe_review_threshold=$3, dedupe_window_days=$4, critic_llm=$5
          WHERE user_id=$1",
    )
    .bind(u.id)
    .bind(threshold)
    .bind(review)
    .bind(days)
    .bind(s.critic_llm.unwrap_or(cur.critic_llm))
    .execute(&st.db)
    .await?;
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

/// Longest SSE connection of an API token (WP07): a revoked token loses
/// its live stream at the latest after this; clients reconnect.
const TOKEN_STREAM_MAX: std::time::Duration = std::time::Duration::from_secs(600);

async fn events(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    tok: Option<axum::Extension<auth::TokenAuth>>,
    session: Option<axum::Extension<auth::SessionAuth>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let uid = u.id;
    let stream = BroadcastStream::new(st.bus.subscribe()).filter_map(move |m| match m {
        Ok(ev) if ev.user_id == uid => Some(Ok(sse_data(&ev.payload))),
        Ok(_) => None,
        // Lagged: tell the client to refetch.
        Err(_) => Some(Ok(sse_data(&ServerEvent::Resync))),
    });
    // Token streams end at the token's expiry or after TOKEN_STREAM_MAX.
    let expiry: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> = match &tok {
        Some(axum::Extension(t)) => {
            let left = (t.expires_at - chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
                .min(TOKEN_STREAM_MAX);
            Box::pin(tokio::time::sleep(left))
        }
        None => Box::pin(futures::future::pending()),
    };
    // WP11 (WP07-N6): a revoked session or token or a locked account ends
    // the stream within STREAM_RECHECK, not only at the next reconnect.
    let (sid, tid) = (session.map(|s| s.0.session_id), tok.map(|t| t.0.token_id));
    let st2 = st.clone();
    let revoked = async move {
        loop {
            tokio::time::sleep(STREAM_RECHECK).await;
            if !auth::still_valid(&st2, sid, tid).await {
                return;
            }
        }
    };
    let stop = async move {
        tokio::select! {
            _ = expiry => {}
            _ = revoked => {}
        }
    };
    Sse::new(futures::StreamExt::take_until(stream, stop)).keep_alive(KeepAlive::default())
}

/// WP11: how often a live stream re-checks its session/token.
const STREAM_RECHECK: std::time::Duration = std::time::Duration::from_secs(5);
