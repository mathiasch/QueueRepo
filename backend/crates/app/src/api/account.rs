//! WP11: account security of the signed-in person – password, second
//! factors (TOTP, passkeys, recovery codes), sessions, own quota. Cookie
//! only: API tokens never reach these routes (not in the token table).
use anyhow::anyhow;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_extra::extract::cookie::CookieJar;
use queued_api::security::{
    validate_password, ChangePassword, FactorAdded, PasskeyInfo, QuotaStatus, RecoveryCodes,
    SecurityOverview, SessionInfo, StartPasskey, StepUp, TotpConfirm, TotpSetup, WebauthnChallenge,
    WebauthnResponse, MAX_PASSKEYS, MAX_PASSKEY_NAME,
};
use serde_json::json;
use uuid::Uuid;
use webauthn_rs::prelude::{CredentialID, PasskeyRegistration, RegisterPublicKeyCredential};

use crate::{
    audit,
    auth::{self, AuthUser},
    crypto,
    error::{ApiResult, AppError},
    instance, mfa,
    models::User,
    state::AppState,
};

async fn password_hash(st: &AppState, user: Uuid) -> ApiResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id=$1")
            .bind(user)
            .fetch_one(&st.db)
            .await?,
    )
}

/// Re-authentication for sensitive changes: the current password, or a
/// current second factor (TOTP code / recovery code). Accounts with neither
/// (OIDC only, no factor) need nothing beyond their session. Wrong answers
/// count against the same throttle as the login.
async fn step_up(st: &AppState, u: &User, s: &StepUp) -> ApiResult<()> {
    let hash = password_hash(st, u.id).await?;
    let has_mfa = mfa::has_mfa(&st.db, u.id).await?;
    if hash.is_none() && !has_mfa {
        return Ok(());
    }
    let key = mfa::mfa_key(u.id);
    if let Some(until) = mfa::locked(&st.db, &key).await? {
        let mins = ((until - chrono::Utc::now()).num_seconds().max(1) + 59) / 60;
        return Err(AppError(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Zu viele Fehlversuche. Bitte versuch es in {mins} Minute(n) erneut."),
        ));
    }
    let ok = match (&s.password, &s.code) {
        (Some(pw), _) if hash.is_some() && !pw.is_empty() => {
            auth::verify_password(hash, pw.clone()).await?
        }
        (_, Some(code)) if has_mfa && !code.trim().is_empty() => {
            mfa::verify_totp(&st.db, &st.cfg.master_key, u.id, code, false).await?
                || mfa::use_recovery(&st.db, &st.cfg.master_key, u.id, code).await?
        }
        _ => {
            return Err(AppError(
                StatusCode::FORBIDDEN,
                if hash.is_some() {
                    "Bitte bestätige mit deinem Passwort.".into()
                } else {
                    "Bitte bestätige mit einem Code aus deiner Authenticator-App.".into()
                },
            ))
        }
    };
    if !ok {
        st.metrics.auth_failures.inc(&["step_up"]);
        if let Some(until) = mfa::failed(&st.db, &key, mfa::MFA_MAX_FAILURES).await? {
            mfa::lock(&st.db, &key, until).await?;
        }
        audit::log(
            st,
            u,
            "auth.mfa_failed",
            ("user", &u.id.to_string()),
            None,
            json!({"factor": "step_up"}),
        )
        .await;
        return Err(AppError(
            StatusCode::FORBIDDEN,
            "Die Bestätigung stimmt nicht.".into(),
        ));
    }
    Ok(())
}

pub async fn security(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<SecurityOverview>> {
    Ok(Json(overview(&st, &u).await?))
}

async fn overview(st: &AppState, u: &User) -> ApiResult<SecurityOverview> {
    let totp: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM mfa_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL)",
    )
    .bind(u.id)
    .fetch_one(&st.db)
    .await?;
    let passkeys: Vec<(Uuid, String, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as(
            "SELECT id, name, created_at, last_used_at FROM webauthn_credentials WHERE user_id=$1 ORDER BY created_at",
        )
        .bind(u.id)
        .fetch_all(&st.db)
        .await?;
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM recovery_codes WHERE user_id=$1 AND used_at IS NULL",
    )
    .bind(u.id)
    .fetch_one(&st.db)
    .await?;
    let policy = instance::settings(st).await?.mfa_policy;
    Ok(SecurityOverview {
        totp_enabled: totp,
        passkeys: passkeys
            .into_iter()
            .map(|(id, name, created_at, last_used_at)| PasskeyInfo {
                id,
                name,
                created_at,
                last_used_at,
            })
            .collect(),
        recovery_left: left,
        password_set: password_hash(st, u.id).await?.is_some(),
        mfa_required: policy.applies(u.is_admin),
    })
}

/// After a factor was added: recovery codes if none are left, other
/// sessions signed out (the factor protects from now on).
async fn factor_added(st: &AppState, u: &User, jar: &CookieJar) -> ApiResult<FactorAdded> {
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM recovery_codes WHERE user_id=$1 AND used_at IS NULL",
    )
    .bind(u.id)
    .fetch_one(&st.db)
    .await?;
    let recovery_codes = if left == 0 {
        let mut tx = st.db.begin().await?;
        let codes = mfa::replace_recovery(&mut tx, &st.cfg.master_key, u.id).await?;
        tx.commit().await?;
        Some(codes)
    } else {
        None
    };
    let current = auth::current_session(st, jar).await?;
    let revoked = auth::revoke_sessions(st, u.id, current).await?;
    Ok(FactorAdded {
        recovery_codes,
        revoked_sessions: revoked,
    })
}

/// The last factor may not go while the policy requires one.
async fn may_remove_factor(st: &AppState, u: &User, removing_totp: bool) -> ApiResult<()> {
    if !instance::settings(st).await?.mfa_policy.applies(u.is_admin) {
        return Ok(());
    }
    let (totp, passkeys): (bool, i64) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM mfa_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL),
                (SELECT count(*) FROM webauthn_credentials WHERE user_id=$1)",
    )
    .bind(u.id)
    .fetch_one(&st.db)
    .await?;
    let remaining = if removing_totp {
        passkeys
    } else {
        passkeys - 1 + i64::from(totp)
    };
    if remaining < 1 {
        return Err(AppError::conflict(
            "Zwei-Faktor-Anmeldung ist hier Pflicht. Richte zuerst einen anderen Faktor ein.",
        ));
    }
    Ok(())
}

pub async fn change_password(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    jar: CookieJar,
    Json(b): Json<ChangePassword>,
) -> ApiResult<impl IntoResponse> {
    validate_password(&b.new_password).map_err(AppError::bad_request)?;
    step_up(&st, &u, &b.step_up).await?;
    let hash = auth::hash_password(b.new_password).await?;
    sqlx::query("UPDATE users SET password_hash=$2, password_changed_at=now() WHERE id=$1")
        .bind(u.id)
        .bind(hash)
        .execute(&st.db)
        .await?;
    // Every session goes, this one gets a fresh token.
    let revoked = auth::revoke_sessions(&st, u.id, None).await? - 1;
    let jar = auth::create_session(&st, jar, u.id, "password", "").await?;
    audit::log(
        &st,
        &u,
        "account.password_changed",
        ("user", &u.id.to_string()),
        None,
        json!({"revoked_sessions": revoked.max(0)}),
    )
    .await;
    Ok((
        jar,
        Json(json!({"ok": true, "revoked_sessions": revoked.max(0)})),
    ))
}

pub async fn totp_setup(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(b): Json<StepUp>,
) -> ApiResult<Json<TotpSetup>> {
    let confirmed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM mfa_totp WHERE user_id=$1 AND confirmed_at IS NOT NULL)",
    )
    .bind(u.id)
    .fetch_one(&st.db)
    .await?;
    if confirmed {
        return Err(AppError::conflict(
            "Die Authenticator-App ist bereits eingerichtet.",
        ));
    }
    step_up(&st, &u, &b).await?;
    let secret = mfa::new_totp_secret();
    let enc = crypto::encrypt(&st.cfg.master_key, &secret)?;
    sqlx::query(
        "INSERT INTO mfa_totp (user_id, secret_enc) VALUES ($1,$2)
         ON CONFLICT (user_id) DO UPDATE SET secret_enc=$2, last_step=0, created_at=now()
         WHERE mfa_totp.confirmed_at IS NULL",
    )
    .bind(u.id)
    .bind(enc)
    .execute(&st.db)
    .await?;
    let b32 = mfa::base32_encode(&secret);
    let host = url::Url::parse(&st.cfg.public_url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();
    let issuer = if host.is_empty() {
        "Queue".to_string()
    } else {
        format!("Queue ({host})")
    };
    Ok(Json(TotpSetup {
        uri: mfa::totp_uri(&issuer, &u.email, &b32),
        secret: b32,
    }))
}

pub async fn totp_confirm(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    jar: CookieJar,
    Json(b): Json<TotpConfirm>,
) -> ApiResult<Json<FactorAdded>> {
    let key = mfa::mfa_key(u.id);
    if mfa::locked(&st.db, &key).await?.is_some() {
        return Err(AppError(
            StatusCode::TOO_MANY_REQUESTS,
            "Zu viele Fehlversuche. Bitte warte ein paar Minuten.".into(),
        ));
    }
    if !mfa::verify_totp(&st.db, &st.cfg.master_key, u.id, &b.code, true).await? {
        if let Some(until) = mfa::failed(&st.db, &key, mfa::MFA_MAX_FAILURES).await? {
            mfa::lock(&st.db, &key, until).await?;
        }
        return Err(AppError::bad_request(
            "Der Code stimmt nicht. Prüfe die Uhrzeit deines Geräts und versuch den nächsten Code.",
        ));
    }
    let n = sqlx::query(
        "UPDATE mfa_totp SET confirmed_at=now() WHERE user_id=$1 AND confirmed_at IS NULL",
    )
    .bind(u.id)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(AppError::conflict(
            "Die Authenticator-App ist bereits eingerichtet.",
        ));
    }
    let out = factor_added(&st, &u, &jar).await?;
    audit::log(
        &st,
        &u,
        "account.totp_enabled",
        ("user", &u.id.to_string()),
        None,
        json!({"revoked_sessions": out.revoked_sessions}),
    )
    .await;
    Ok(Json(out))
}

pub async fn totp_disable(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    jar: CookieJar,
    Json(b): Json<StepUp>,
) -> ApiResult<Json<serde_json::Value>> {
    step_up(&st, &u, &b).await?;
    may_remove_factor(&st, &u, true).await?;
    sqlx::query("DELETE FROM mfa_totp WHERE user_id=$1")
        .bind(u.id)
        .execute(&st.db)
        .await?;
    let revoked = after_factor_removed(&st, &u, &jar).await?;
    audit::log(
        &st,
        &u,
        "account.totp_disabled",
        ("user", &u.id.to_string()),
        None,
        json!({"revoked_sessions": revoked}),
    )
    .await;
    Ok(Json(json!({"ok": true, "revoked_sessions": revoked})))
}

/// Without any factor the recovery codes are worthless: they go. Other
/// sessions are signed out on every MFA change.
async fn after_factor_removed(st: &AppState, u: &User, jar: &CookieJar) -> ApiResult<i64> {
    if !mfa::has_mfa(&st.db, u.id).await? {
        sqlx::query("DELETE FROM recovery_codes WHERE user_id=$1")
            .bind(u.id)
            .execute(&st.db)
            .await?;
    }
    let current = auth::current_session(st, jar).await?;
    Ok(auth::revoke_sessions(st, u.id, current).await?)
}

pub async fn regenerate_recovery(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(b): Json<StepUp>,
) -> ApiResult<Json<RecoveryCodes>> {
    if !mfa::has_mfa(&st.db, u.id).await? {
        return Err(AppError::conflict(
            "Richte zuerst die Zwei-Faktor-Anmeldung ein.",
        ));
    }
    step_up(&st, &u, &b).await?;
    let mut tx = st.db.begin().await?;
    let codes = mfa::replace_recovery(&mut tx, &st.cfg.master_key, u.id).await?;
    tx.commit().await?;
    audit::log(
        &st,
        &u,
        "account.recovery_regenerated",
        ("user", &u.id.to_string()),
        None,
        json!({}),
    )
    .await;
    Ok(Json(RecoveryCodes { codes }))
}

pub async fn passkey_start(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(b): Json<StartPasskey>,
) -> ApiResult<Json<WebauthnChallenge>> {
    let existing = mfa::passkeys(&st.db, u.id).await?;
    if existing.len() as i64 >= MAX_PASSKEYS {
        return Err(AppError::conflict(format!(
            "Höchstens {MAX_PASSKEYS} Passkeys pro Konto."
        )));
    }
    step_up(&st, &u, &b.step_up).await?;
    let exclude: Vec<CredentialID> = existing.iter().map(|(_, p)| p.cred_id().clone()).collect();
    let wa = mfa::webauthn(&st.cfg.public_url)?;
    let (ccr, state) = wa
        .start_passkey_registration(u.id, &u.email, &u.name, Some(exclude))
        .map_err(|e| anyhow!("webauthn: {e:?}"))?;
    mfa::start_flow(
        &st.db,
        u.id,
        "webauthn_register",
        "",
        Some(serde_json::to_value(&state).map_err(anyhow::Error::from)?),
    )
    .await?;
    Ok(Json(WebauthnChallenge {
        options: serde_json::to_value(&ccr).map_err(anyhow::Error::from)?,
    }))
}

pub async fn passkey_finish(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    jar: CookieJar,
    Json(b): Json<WebauthnResponse>,
) -> ApiResult<Json<FactorAdded>> {
    let name = b.name.trim().to_string();
    let name = if name.is_empty() {
        "Passkey".to_string()
    } else {
        name.chars().take(MAX_PASSKEY_NAME).collect()
    };
    let flow = mfa::flow_of_user(&st.db, u.id, "webauthn_register")
        .await?
        .ok_or_else(|| AppError::bad_request("Bitte starte das Hinzufügen erneut."))?;
    if !mfa::end_flow(&st.db, flow.id).await? {
        return Err(AppError::bad_request("Bitte starte das Hinzufügen erneut."));
    }
    let state: PasskeyRegistration = flow
        .state
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| AppError::bad_request("Bitte starte das Hinzufügen erneut."))?;
    let cred: RegisterPublicKeyCredential = serde_json::from_value(b.credential)
        .map_err(|_| AppError::bad_request("invalid credential"))?;
    let wa = mfa::webauthn(&st.cfg.public_url)?;
    let pk = wa.finish_passkey_registration(&cred, &state).map_err(|e| {
        tracing::info!("passkey registration of {} failed: {e:?}", u.id);
        AppError::bad_request("Der Passkey konnte nicht geprüft werden. Bitte versuch es erneut.")
    })?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO webauthn_credentials (id, user_id, name, cred_id, passkey) VALUES ($1,$2,$3,$4,$5)")
        .bind(id)
        .bind(u.id)
        .bind(&name)
        .bind(pk.cred_id().as_ref() as &[u8])
        .bind(serde_json::to_value(&pk).map_err(anyhow::Error::from)?)
        .execute(&st.db)
        .await?;
    let out = factor_added(&st, &u, &jar).await?;
    audit::log(
        &st,
        &u,
        "account.passkey_added",
        ("passkey", &id.to_string()),
        None,
        json!({"name": name, "revoked_sessions": out.revoked_sessions}),
    )
    .await;
    Ok(Json(out))
}

pub async fn passkey_remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    jar: CookieJar,
    Path(id): Path<Uuid>,
    Json(b): Json<StepUp>,
) -> ApiResult<Json<serde_json::Value>> {
    let name: String =
        sqlx::query_scalar("SELECT name FROM webauthn_credentials WHERE id=$1 AND user_id=$2")
            .bind(id)
            .bind(u.id)
            .fetch_optional(&st.db)
            .await?
            .ok_or_else(AppError::not_found)?;
    step_up(&st, &u, &b).await?;
    may_remove_factor(&st, &u, false).await?;
    sqlx::query("DELETE FROM webauthn_credentials WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(u.id)
        .execute(&st.db)
        .await?;
    let revoked = after_factor_removed(&st, &u, &jar).await?;
    audit::log(
        &st,
        &u,
        "account.passkey_removed",
        ("passkey", &id.to_string()),
        None,
        json!({"name": name, "revoked_sessions": revoked}),
    )
    .await;
    Ok(Json(json!({"ok": true, "revoked_sessions": revoked})))
}

pub async fn sessions(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    jar: CookieJar,
) -> ApiResult<Json<Vec<SessionInfo>>> {
    let current = auth::current_session(&st, &jar).await?;
    let rows: Vec<(
        Uuid,
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
        String,
        String,
    )> = sqlx::query_as(
        "SELECT id, created_at, last_seen_at, expires_at, user_agent, auth_method FROM sessions
         WHERE user_id=$1 AND expires_at > now() ORDER BY last_seen_at DESC",
    )
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(id, created_at, last_seen_at, expires_at, user_agent, auth_method)| SessionInfo {
                    current: Some(id) == current,
                    id,
                    created_at,
                    last_seen_at,
                    expires_at,
                    user_agent,
                    auth_method,
                },
            )
            .collect(),
    ))
}

pub async fn revoke_session(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let n = sqlx::query("DELETE FROM sessions WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(u.id)
        .execute(&st.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    audit::log(
        &st,
        &u,
        "account.session_revoked",
        ("session", &id.to_string()),
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({"ok": true})))
}

pub async fn revoke_other_sessions(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    jar: CookieJar,
) -> ApiResult<Json<serde_json::Value>> {
    let current = auth::current_session(&st, &jar).await?;
    let n = auth::revoke_sessions(&st, u.id, current).await?;
    audit::log(
        &st,
        &u,
        "account.sessions_revoked",
        ("user", &u.id.to_string()),
        None,
        json!({"count": n}),
    )
    .await;
    Ok(Json(json!({"ok": true, "revoked_sessions": n})))
}

pub async fn quota(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<QuotaStatus>> {
    Ok(Json(instance::quota_status(&st.db, u.id).await?))
}
