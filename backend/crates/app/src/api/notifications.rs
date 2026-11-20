//! WP10: notification settings (events × channels, quiet hours, daily
//! summary, webhook). Cookie only: not in the API token route table.
use axum::{extract::State, Json};
use queued_api::notify::{
    validate, Channel, Event, NotificationSettings, NotificationSettingsUpdate, WebhookSecret,
};
use serde_json::{json, Value};

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    notify,
    state::AppState,
};

async fn view(st: &AppState, user: uuid::Uuid) -> ApiResult<NotificationSettings> {
    let p = notify::load_prefs(&st.db, user).await?;
    let ntfy: Option<String> =
        sqlx::query_scalar("SELECT ntfy_url FROM user_settings WHERE user_id=$1")
            .bind(user)
            .fetch_optional(&st.db)
            .await?
            .flatten();
    let devices: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions WHERE user_id=$1")
            .bind(user)
            .fetch_one(&st.db)
            .await?;
    Ok(NotificationSettings {
        timezone: p.tz.name().to_string(),
        quiet_enabled: p.quiet_enabled,
        quiet_start: p.quiet_start,
        quiet_end: p.quiet_end,
        quiet_critical: p.quiet_critical,
        digest_enabled: p.digest_enabled,
        digest_minute: p.digest_minute,
        webhook_url: p.webhook_url.clone(),
        webhook_secret_set: p.webhook_secret_enc.is_some(),
        matrix: p.matrix,
        ntfy_configured: ntfy.is_some(),
        push_devices: devices,
    })
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<NotificationSettings>> {
    Ok(Json(view(&st, u.id).await?))
}

pub async fn put(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(b): Json<NotificationSettingsUpdate>,
) -> ApiResult<Json<NotificationSettings>> {
    validate(&b).map_err(AppError::bad_request)?;
    let tz = notify::parse_tz(b.timezone.trim())
        .ok_or_else(|| AppError::bad_request("Unbekannte Zeitzone"))?;
    let webhook = b
        .webhook_url
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(String::from);
    if let Some(w) = &webhook {
        if w.starts_with("http://") && !st.cfg.allow_private_notify_targets {
            return Err(AppError::bad_request(
                "Webhook-Adresse muss mit https:// beginnen",
            ));
        }
        // SSRF guard also when saving (and again at every delivery).
        crate::llm::check_target(w, &st.cfg.notify_guard())
            .await
            .map_err(|e| {
                AppError::bad_request(format!(
                    "Webhook-Adresse nicht erlaubt: {}",
                    e.message.replace(
                        "ALLOW_PRIVATE_LLM_ENDPOINTS",
                        "ALLOW_PRIVATE_NOTIFY_TARGETS"
                    )
                ))
            })?;
    }
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "INSERT INTO notification_settings (user_id, timezone, quiet_enabled, quiet_start, quiet_end, quiet_critical,
                                            digest_enabled, digest_minute, webhook_url, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,now())
         ON CONFLICT (user_id) DO UPDATE SET timezone=EXCLUDED.timezone, quiet_enabled=EXCLUDED.quiet_enabled,
             quiet_start=EXCLUDED.quiet_start, quiet_end=EXCLUDED.quiet_end, quiet_critical=EXCLUDED.quiet_critical,
             digest_enabled=EXCLUDED.digest_enabled, digest_minute=EXCLUDED.digest_minute,
             webhook_url=EXCLUDED.webhook_url,
             -- A new address needs a new secret (the old one belongs to the old receiver).
             webhook_secret_enc = CASE WHEN notification_settings.webhook_url IS DISTINCT FROM EXCLUDED.webhook_url
                                       THEN NULL ELSE notification_settings.webhook_secret_enc END,
             updated_at=now()",
    )
    .bind(u.id)
    .bind(tz.name())
    .bind(b.quiet_enabled)
    .bind(b.quiet_start as i16)
    .bind(b.quiet_end as i16)
    .bind(b.quiet_critical)
    .bind(b.digest_enabled)
    .bind(b.digest_minute as i16)
    .bind(&webhook)
    .execute(&mut *tx)
    .await?;
    // Only deviations from the defaults are stored.
    sqlx::query("DELETE FROM notification_prefs WHERE user_id=$1")
        .bind(u.id)
        .execute(&mut *tx)
        .await?;
    for e in Event::ALL {
        for c in Channel::ALL {
            let on = b
                .matrix
                .get(&e)
                .and_then(|m| m.get(&c))
                .copied()
                .unwrap_or_else(|| e.default_on(c));
            if on != e.default_on(c) {
                sqlx::query(
                    "INSERT INTO notification_prefs (user_id, event, channel, enabled) VALUES ($1,$2,$3,$4)",
                )
                .bind(u.id)
                .bind(e.as_str())
                .bind(c.as_str())
                .bind(on)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;
    st.notify_wake.notify_one();
    Ok(Json(view(&st, u.id).await?))
}

/// `POST /api/notifications/webhook/secret`: a new signing secret, shown
/// once (`qwh_…`, recognised by the secret scanner of WP07).
pub async fn renew_secret(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<WebhookSecret>> {
    let secret = super::tokens::generate(queued_api::tokens::SECRET_PREFIX);
    let enc = crate::crypto::encrypt(&st.cfg.master_key, secret.as_bytes())?;
    let n = sqlx::query(
        "UPDATE notification_settings SET webhook_secret_enc=$2, updated_at=now()
          WHERE user_id=$1 AND webhook_url IS NOT NULL",
    )
    .bind(u.id)
    .bind(enc)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::bad_request(
            "Zuerst eine Webhook-Adresse speichern",
        ));
    }
    Ok(Json(WebhookSecret { secret }))
}

/// `POST /api/notifications/webhook/test`: one test delivery to the webhook.
pub async fn test_webhook(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Value>> {
    let p = notify::load_prefs(&st.db, u.id).await?;
    if p.webhook_url.is_none() || p.webhook_secret_enc.is_none() {
        return Err(AppError::bad_request(
            "Zuerst Adresse speichern und ein Secret erzeugen",
        ));
    }
    sqlx::query(
        "INSERT INTO notification_outbox (user_id, channel, event, payload) VALUES ($1,'webhook','test',$2)",
    )
    .bind(u.id)
    .bind(json!({
        "kind": "test",
        "event": "test",
        "title": "Test-Webhook",
        "body": "Die Queue erreicht deinen Webhook.",
        "task_id": null,
        "url": st.cfg.public_url,
        "created_at": chrono::Utc::now(),
    }))
    .execute(&st.db)
    .await?;
    st.notify_wake.notify_one();
    Ok(Json(json!({"ok": true})))
}

/// `GET /api/notifications/deliveries`: the last external deliveries
/// (status for the settings page; no payloads).
pub async fn deliveries(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<Value>>> {
    let rows: Vec<(
        String,
        String,
        String,
        i32,
        Option<String>,
        chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT channel, event, status, attempts, last_error, created_at FROM notification_outbox
              WHERE user_id=$1 ORDER BY id DESC LIMIT 20",
    )
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(c, e, s, a, err, at)| {
                json!({"channel": c, "event": e, "status": s, "attempts": a, "error": err, "created_at": at})
            })
            .collect(),
    ))
}
