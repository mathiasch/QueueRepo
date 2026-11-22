use anyhow::anyhow;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, MatchedPath, Path, Query, State},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use queued_api::{
    security::{
        validate_password, InvitePreview, LoginOutcome, MfaCode, MfaPolicy, RegistrationMode,
        WebauthnChallenge, WebauthnResponse,
    },
    tokens, LoginRequest, RegisterRequest,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;
use webauthn_rs::prelude::{
    AuthenticationResult, Passkey, PasskeyAuthentication, PublicKeyCredential,
};

use crate::{
    audit,
    crypto::{random_token, sha256_hex},
    error::{ApiResult, AppError},
    instance, mfa,
    models::User,
    oidc::ExternalIdentity,
    state::AppState,
};

pub const COOKIE: &str = "qsid";
const SESSION_DAYS: i64 = 30;

/// Authenticated user extractor: session cookie, or (WP07) a personal API
/// token in `Authorization: Bearer qk_…`. With an `Authorization` header
/// only the token counts (the cookie is ignored), and only on the routes of
/// `queued_api::tokens::ROUTES` with the scope listed there. Every other
/// route – account, settings, credentials, token management, … – answers
/// 403 to a token. The token's request extension is [`TokenAuth`].
pub struct AuthUser(pub User);

/// Present in the request extensions when a token authenticated it.
#[derive(Debug, Clone)]
pub struct TokenAuth {
    pub token_id: Uuid,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

fn token_error(status: StatusCode, msg: &str) -> AppError {
    AppError(status, msg.into())
}

async fn token_user(parts: &mut Parts, st: &AppState, header: &str) -> Result<User, AppError> {
    let unauthorized = |m: &str| token_error(StatusCode::UNAUTHORIZED, m);
    let token = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .map(str::trim)
        .ok_or_else(|| unauthorized("Authorization must be `Bearer <token>`"))?;
    // Route first: a forbidden route needs no database lookup.
    let route = parts
        .extensions
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let needed = tokens::required(parts.method.as_str(), &route).ok_or_else(|| {
        token_error(
            StatusCode::FORBIDDEN,
            "this endpoint cannot be used with an API token",
        )
    })?;
    if !tokens::well_formed(token, tokens::TOKEN_PREFIX) {
        return Err(unauthorized("invalid token"));
    }
    let row: Option<(
        Uuid,
        Vec<String>,
        chrono::DateTime<chrono::Utc>,
        Option<chrono::DateTime<chrono::Utc>>,
        Uuid,
        String,
        String,
        bool,
    )> = sqlx::query_as(
        "SELECT t.id, t.scopes, t.expires_at, t.revoked_at, u.id, u.email, u.name, u.is_admin
             FROM api_tokens t JOIN users u ON u.id = t.user_id
             WHERE t.token_hash = $1 AND u.disabled_at IS NULL",
    )
    .bind(sha256_hex(token))
    .fetch_optional(&st.db)
    .await?;
    let (id, scopes, expires_at, revoked_at, uid, email, name, is_admin) =
        row.ok_or_else(|| unauthorized("invalid token"))?;
    if revoked_at.is_some() {
        return Err(unauthorized("token revoked"));
    }
    if expires_at <= chrono::Utc::now() {
        return Err(unauthorized("token expired"));
    }
    if let Some(scope) = needed {
        if !scopes.iter().any(|s| s == scope.as_str()) {
            return Err(token_error(
                StatusCode::FORBIDDEN,
                &format!("token lacks scope {}", scope.as_str()),
            ));
        }
    }
    // At most one write per minute and token.
    sqlx::query(
        "UPDATE api_tokens SET last_used_at=now()
         WHERE id=$1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
    )
    .bind(id)
    .execute(&st.db)
    .await?;
    // WP11: while the MFA policy requires a factor the person has not set
    // up, tokens are refused like the session (no detour around the policy).
    let (policy, has_mfa): (String, bool) = sqlx::query_as(
        "SELECT (SELECT mfa_policy FROM instance_settings WHERE id=1),
                EXISTS (SELECT 1 FROM mfa_totp m WHERE m.user_id=$1 AND m.confirmed_at IS NOT NULL)
                  OR EXISTS (SELECT 1 FROM webauthn_credentials w WHERE w.user_id=$1)",
    )
    .bind(uid)
    .fetch_one(&st.db)
    .await?;
    if MfaPolicy::parse(&policy).is_some_and(|p| p.applies(is_admin)) && !has_mfa {
        return Err(token_error(StatusCode::FORBIDDEN, MFA_SETUP_REQUIRED));
    }
    parts.extensions.insert(TokenAuth {
        token_id: id,
        expires_at,
    });
    Ok(User {
        id: uid,
        email,
        name,
        is_admin,
        mfa_setup_required: false,
    })
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        if let Some(h) = parts.headers.get(AUTHORIZATION) {
            let h = h
                .to_str()
                .map_err(|_| token_error(StatusCode::UNAUTHORIZED, "invalid token"))?
                .to_string();
            return Ok(AuthUser(token_user(parts, st, &h).await?));
        }
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(COOKIE)
            .map(|c| c.value().to_string())
            .ok_or_else(AppError::unauthorized)?;
        // WP11: locked accounts have no sessions (the lock deletes them);
        // `disabled_at` is checked anyway. The MFA policy comes along.
        let row: Option<(Uuid, String, String, bool, Uuid, chrono::DateTime<chrono::Utc>, String, bool)> =
            sqlx::query_as(
                "SELECT u.id, u.email, u.name, u.is_admin, s.id, s.last_seen_at,
                        (SELECT mfa_policy FROM instance_settings WHERE id=1),
                        EXISTS (SELECT 1 FROM mfa_totp m WHERE m.user_id=u.id AND m.confirmed_at IS NOT NULL)
                          OR EXISTS (SELECT 1 FROM webauthn_credentials w WHERE w.user_id=u.id)
                 FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = $1 AND s.expires_at > now() AND u.disabled_at IS NULL",
            )
            .bind(sha256_hex(&token))
            .fetch_optional(&st.db)
            .await?;
        let (id, email, name, is_admin, sid, seen, policy, has_mfa) =
            row.ok_or_else(AppError::unauthorized)?;
        if seen < chrono::Utc::now() - chrono::Duration::minutes(1) {
            sqlx::query("UPDATE sessions SET last_seen_at=now() WHERE id=$1")
                .bind(sid)
                .execute(&st.db)
                .await?;
        }
        let setup_required =
            MfaPolicy::parse(&policy).is_some_and(|p| p.applies(is_admin)) && !has_mfa;
        if setup_required && !mfa_setup_route(parts) {
            return Err(AppError(StatusCode::FORBIDDEN, MFA_SETUP_REQUIRED.into()));
        }
        parts.extensions.insert(SessionAuth { session_id: sid });
        Ok(AuthUser(User {
            id,
            email,
            name,
            is_admin,
            mfa_setup_required: setup_required,
        }))
    }
}

/// Error text while the MFA policy requires a factor that is not set up.
pub const MFA_SETUP_REQUIRED: &str = "mfa_setup_required";

/// Present in the request extensions when a session cookie authenticated it.
#[derive(Debug, Clone, Copy)]
pub struct SessionAuth {
    pub session_id: Uuid,
}

/// Routes a session may use while the MFA policy requires a factor that
/// is not set up yet: who am I, sign out, set up a factor.
fn mfa_setup_route(parts: &Parts) -> bool {
    let route = parts
        .extensions
        .get::<MatchedPath>()
        .map(|m| m.as_str())
        .unwrap_or_default();
    matches!(
        route,
        "/api/auth/me"
            | "/api/auth/logout"
            | "/api/account/security"
            | "/api/account/mfa/totp/setup"
            | "/api/account/mfa/totp/confirm"
            | "/api/account/webauthn/register/start"
            | "/api/account/webauthn/register/finish"
    )
}

/// Second-login-step cookie (the flow token, never a session).
pub const MFA_COOKIE: &str = "qmfa";

fn session_cookie(st: &AppState, token: String) -> Cookie<'static> {
    Cookie::build((COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(st.cfg.secure_cookies())
        .max_age(time_days(SESSION_DAYS))
        .build()
}

fn user_agent(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect()
}

/// Creates a **new** session (fresh random token: no session fixation);
/// a session the browser still carried is deleted.
pub async fn create_session(
    st: &AppState,
    jar: CookieJar,
    user_id: Uuid,
    method: &str,
    ua: &str,
) -> ApiResult<CookieJar> {
    if let Some(old) = jar.get(COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(sha256_hex(old.value()))
            .execute(&st.db)
            .await?;
    }
    let token = random_token();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at, user_agent, auth_method)
         VALUES ($1, $2, now() + make_interval(days => $3), $4, $5)",
    )
    .bind(sha256_hex(&token))
    .bind(user_id)
    .bind(SESSION_DAYS as i32)
    .bind(ua)
    .bind(method)
    .execute(&st.db)
    .await?;
    let jar = jar.remove(Cookie::build(MFA_COOKIE).path("/api/auth"));
    Ok(jar.add(session_cookie(st, token)))
}

/// Id of the session of this request's cookie.
pub async fn current_session(st: &AppState, jar: &CookieJar) -> sqlx::Result<Option<Uuid>> {
    match jar.get(COOKIE) {
        Some(c) => {
            sqlx::query_scalar("SELECT id FROM sessions WHERE token_hash=$1")
                .bind(sha256_hex(c.value()))
                .fetch_optional(&st.db)
                .await
        }
        None => Ok(None),
    }
}

/// Signs out every session of `user` except `keep`; returns how many.
pub async fn revoke_sessions(st: &AppState, user: Uuid, keep: Option<Uuid>) -> sqlx::Result<i64> {
    let n = sqlx::query("DELETE FROM sessions WHERE user_id=$1 AND ($2::uuid IS NULL OR id <> $2)")
        .bind(user)
        .bind(keep)
        .execute(&st.db)
        .await?
        .rows_affected();
    Ok(n as i64)
}

/// The session (or token) of a live stream is still valid (WP11: revoked
/// sessions, locked accounts and revoked tokens end SSE streams at once).
pub async fn still_valid(st: &AppState, session: Option<Uuid>, token: Option<Uuid>) -> bool {
    let q = match (session, token) {
        (Some(s), _) => sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM sessions s JOIN users u ON u.id=s.user_id
                            WHERE s.id=$1 AND s.expires_at > now() AND u.disabled_at IS NULL)",
        )
        .bind(s),
        (None, Some(t)) => sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM api_tokens t JOIN users u ON u.id=t.user_id
                            WHERE t.id=$1 AND t.revoked_at IS NULL AND t.expires_at > now() AND u.disabled_at IS NULL)",
        )
        .bind(t),
        (None, None) => return false,
    };
    // A database hiccup does not end the stream.
    q.fetch_one(&st.db).await.unwrap_or(true)
}

fn time_days(d: i64) -> time::Duration {
    time::Duration::days(d)
}

/// Creates the user row + default settings. First user becomes admin, as
/// does a person with an admin invitation.
async fn insert_user(
    st: &AppState,
    email: &str,
    name: &str,
    pw_hash: Option<String>,
    invite_hash: Option<&str>,
) -> ApiResult<Uuid> {
    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    let mut admin = count == 0;
    if let Some(hash) = invite_hash {
        let make_admin: Option<bool> = sqlx::query_scalar(
            "UPDATE invitations SET used_at=now()
             WHERE token_hash=$1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > now()
               AND (email IS NULL OR lower(email) = lower($2))
             RETURNING make_admin",
        )
        .bind(hash)
        .bind(email)
        .fetch_optional(&mut *tx)
        .await?;
        match make_admin {
            Some(a) => admin |= a,
            None => {
                return Err(AppError::bad_request(
                    "Die Einladung ist ungültig, abgelaufen, schon benutzt oder gilt für eine andere E-Mail-Adresse.",
                ))
            }
        }
    }
    sqlx::query(
        "INSERT INTO users (id, email, name, password_hash, is_admin, password_changed_at)
         VALUES ($1,$2,$3,$4,$5, CASE WHEN $4 IS NULL THEN NULL ELSE now() END)",
    )
    .bind(id)
    .bind(email)
    .bind(name)
    .bind(pw_hash)
    .bind(admin)
    .execute(&mut *tx)
    .await?;
    if let Some(hash) = invite_hash {
        sqlx::query("UPDATE invitations SET used_by=$1 WHERE token_hash=$2")
            .bind(id)
            .bind(hash)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO user_settings (user_id) VALUES ($1)")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn hash_password(pw: String) -> ApiResult<String> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(pw.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| anyhow!("{e}"))
    })
    .await
    .map_err(|e| anyhow!(e))?
    .map_err(Into::into)
}

/// Verifies against `hash`; without a hash a fixed dummy hash is checked,
/// so unknown accounts take as long as known ones.
pub async fn verify_password(hash: Option<String>, pw: String) -> ApiResult<bool> {
    let known = hash.is_some();
    let ok = tokio::task::spawn_blocking(move || {
        let hash = hash.unwrap_or_else(|| dummy_hash().to_string());
        PasswordHash::new(&hash)
            .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
            .unwrap_or(false)
    })
    .await
    .map_err(|e| anyhow!(e))?;
    Ok(ok && known)
}

/// Argon2 hash of a random password with the default parameters (timing
/// equalization only; computed once).
fn dummy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(random_token().as_bytes(), &salt)
            .map(|h| h.to_string())
            .unwrap_or_default()
    })
}

fn too_many(until: chrono::DateTime<chrono::Utc>) -> AppError {
    let mins = ((until - chrono::Utc::now()).num_seconds().max(1) + 59) / 60;
    AppError(
        StatusCode::TOO_MANY_REQUESTS,
        format!("Zu viele Fehlversuche. Bitte versuch es in {mins} Minute(n) erneut."),
    )
}

/// Counts a failed attempt for `key` (and locks it at `max`).
async fn throttle_failure(
    st: &AppState,
    key: &str,
    max: i32,
    actor: Option<(Uuid, &str)>,
) -> ApiResult<()> {
    if let Some(until) = mfa::failed(&st.db, key, max).await? {
        mfa::lock(&st.db, key, until).await?;
        st.metrics.auth_failures.inc(&["locked"]);
        // Unknown accounts: metric only (see `login`).
        if actor.is_some() {
            audit::log_as(
                st,
                actor,
                "auth.locked",
                ("throttle", key.split(':').next().unwrap_or_default()),
                json!({"minutes": mfa::LOCK_MIN}),
            )
            .await;
        }
    }
    Ok(())
}

async fn throttle_check(st: &AppState, key: &str) -> ApiResult<()> {
    match mfa::locked(&st.db, key).await? {
        Some(until) => Err(too_many(until)),
        None => Ok(()),
    }
}

#[derive(Deserialize)]
pub struct RegisterBody {
    #[serde(flatten)]
    pub r: RegisterRequest,
    #[serde(default)]
    pub invite: Option<String>,
}

pub async fn register(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Json(b): Json<RegisterBody>,
) -> ApiResult<impl IntoResponse> {
    let r = b.r;
    let invite = b
        .invite
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty());
    let mode = instance::settings(&st).await?.registration_mode;
    let invite = match mode {
        RegistrationMode::Closed => return Err(AppError::forbidden()),
        RegistrationMode::Invite if invite.is_none() => {
            return Err(AppError(
                StatusCode::FORBIDDEN,
                "Registrierung nur mit Einladung.".into(),
            ))
        }
        // An invitation still counts in open mode (admin invitations).
        _ => invite,
    };
    let email = r.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 320 {
        return Err(AppError::bad_request(
            "valid email and password (min. 8 chars) required",
        ));
    }
    validate_password(&r.password)
        .map_err(|_| AppError::bad_request("valid email and password (min. 8 chars) required"))?;
    let hash = hash_password(r.password).await?;
    let name = if r.name.trim().is_empty() {
        email.split('@').next().unwrap_or("").to_string()
    } else {
        r.name.trim().chars().take(200).collect()
    };
    let invite_hash = invite.as_deref().map(sha256_hex);
    let id = insert_user(&st, &email, &name, Some(hash), invite_hash.as_deref()).await?;
    let method = if invite.is_some() {
        "password+invite"
    } else {
        "password"
    };
    audit::log_as(
        &st,
        Some((id, &email)),
        "auth.register",
        ("user", &id.to_string()),
        json!({"invited": invite.is_some()}),
    )
    .await;
    let jar = create_session(&st, jar, id, method, &user_agent(&headers)).await?;
    Ok((jar, Json(json!({"ok": true}))))
}

/// Public preview of an invitation (registration form).
pub async fn invite_preview(
    State(st): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<InvitePreview>> {
    let row: Option<Option<String>> = sqlx::query_scalar(
        "SELECT email FROM invitations
         WHERE token_hash=$1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(sha256_hex(token.trim()))
    .fetch_optional(&st.db)
    .await?;
    Ok(Json(InvitePreview {
        valid: row.is_some(),
        email: row.flatten(),
    }))
}

/// After the first factor (password or OIDC): a session, or – for people
/// with a second factor – the flow of the second step.
async fn after_first_factor(
    st: &AppState,
    jar: CookieJar,
    user: Uuid,
    email: &str,
    method: &str,
    ua: &str,
) -> ApiResult<(CookieJar, LoginOutcome)> {
    let methods = mfa::methods(&st.db, user).await?;
    if methods.is_empty() {
        let jar = create_session(st, jar, user, method, ua).await?;
        audit::log_as(
            st,
            Some((user, email)),
            "auth.login",
            ("user", &user.to_string()),
            json!({"method": method}),
        )
        .await;
        return Ok((
            jar,
            LoginOutcome {
                ok: true,
                mfa_required: false,
                methods: vec![],
            },
        ));
    }
    let token = mfa::start_flow(&st.db, user, "mfa_login", method, None).await?;
    let cookie = Cookie::build((MFA_COOKIE, token))
        .path("/api/auth")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(st.cfg.secure_cookies())
        .max_age(time::Duration::minutes(mfa::FLOW_MINUTES))
        .build();
    Ok((
        jar.add(cookie),
        LoginOutcome {
            ok: false,
            mfa_required: true,
            methods,
        },
    ))
}

pub async fn login(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Json(r): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    let email = r.email.trim().to_lowercase();
    let key = mfa::login_key(&email);
    throttle_check(&st, &key).await?;
    let row: Option<(Uuid, Option<String>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT id, password_hash, disabled_at FROM users WHERE email = $1")
            .bind(&email)
            .fetch_optional(&st.db)
            .await?;
    let (uid, hash, disabled) = match row {
        Some((id, h, d)) => (Some(id), h, d),
        None => (None, None, None),
    };
    if !verify_password(hash, r.password).await? {
        st.metrics.auth_failures.inc(&["password"]);
        let actor = uid.map(|u| (u, email.as_str()));
        // Only for existing accounts (bounded by the throttle per account):
        // anonymous guesses with ever new addresses must not fill the
        // append-only log (they are counted in the metric).
        if let Some(u) = uid {
            audit::log_as(
                &st,
                actor,
                "auth.login_failed",
                ("user", &u.to_string()),
                json!({"reason": "password"}),
            )
            .await;
        }
        throttle_failure(&st, &key, mfa::LOGIN_MAX_FAILURES, actor).await?;
        return Err(AppError(
            StatusCode::UNAUTHORIZED,
            "invalid credentials".into(),
        ));
    }
    let uid =
        uid.ok_or_else(|| AppError(StatusCode::UNAUTHORIZED, "invalid credentials".into()))?;
    mfa::clear(&st.db, &key).await?;
    if disabled.is_some() {
        audit::log_as(
            &st,
            Some((uid, &email)),
            "auth.login_failed",
            ("user", &uid.to_string()),
            json!({"reason": "disabled"}),
        )
        .await;
        return Err(AppError(StatusCode::FORBIDDEN, ACCOUNT_DISABLED.into()));
    }
    let (jar, out) =
        after_first_factor(&st, jar, uid, &email, "password", &user_agent(&headers)).await?;
    Ok((jar, Json(out)))
}

pub const ACCOUNT_DISABLED: &str = "Dein Konto ist gesperrt. Wende dich an die Verwaltung.";

/// The pending second step of this browser.
async fn pending_flow(st: &AppState, jar: &CookieJar) -> ApiResult<mfa::Flow> {
    let token = jar
        .get(MFA_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| {
            AppError(
                StatusCode::UNAUTHORIZED,
                "Die Anmeldung ist abgelaufen. Bitte melde dich erneut an.".into(),
            )
        })?;
    mfa::flow_by_token(&st.db, &token, "mfa_login")
        .await?
        .ok_or_else(|| {
            AppError(
                StatusCode::UNAUTHORIZED,
                "Die Anmeldung ist abgelaufen. Bitte melde dich erneut an.".into(),
            )
        })
}

async fn user_label(st: &AppState, user: Uuid) -> ApiResult<(String, bool)> {
    let (email, disabled): (String, bool) =
        sqlx::query_as("SELECT email, disabled_at IS NOT NULL FROM users WHERE id=$1")
            .bind(user)
            .fetch_one(&st.db)
            .await?;
    Ok((email, disabled))
}

/// Methods of the pending second step (login page after an OIDC redirect).
pub async fn mfa_pending(
    State(st): State<AppState>,
    jar: CookieJar,
) -> ApiResult<Json<LoginOutcome>> {
    let flow = pending_flow(&st, &jar).await?;
    Ok(Json(LoginOutcome {
        ok: false,
        mfa_required: true,
        methods: mfa::methods(&st.db, flow.user_id).await?,
    }))
}

/// Ends the second step with a session (shared by all factors).
async fn second_factor_done(
    st: &AppState,
    jar: CookieJar,
    flow: &mfa::Flow,
    factor: &str,
    ua: &str,
) -> ApiResult<CookieJar> {
    let (email, disabled) = user_label(st, flow.user_id).await?;
    // Single use: a parallel request with the same flow gets nothing.
    if !mfa::end_flow(&st.db, flow.id).await? || disabled {
        return Err(AppError(
            StatusCode::UNAUTHORIZED,
            "Die Anmeldung ist abgelaufen. Bitte melde dich erneut an.".into(),
        ));
    }
    mfa::clear(&st.db, &mfa::mfa_key(flow.user_id)).await?;
    let method = format!("{}+{factor}", flow.method);
    let jar = create_session(st, jar, flow.user_id, &method, ua).await?;
    audit::log_as(
        st,
        Some((flow.user_id, &email)),
        "auth.login",
        ("user", &flow.user_id.to_string()),
        json!({"method": method}),
    )
    .await;
    Ok(jar)
}

/// A wrong second factor: counts against the flow and the person.
async fn second_factor_failed(
    st: &AppState,
    flow: &mfa::Flow,
    factor: &str,
) -> ApiResult<AppError> {
    let (email, _) = user_label(st, flow.user_id).await?;
    st.metrics.auth_failures.inc(&["mfa"]);
    audit::log_as(
        st,
        Some((flow.user_id, &email)),
        "auth.mfa_failed",
        ("user", &flow.user_id.to_string()),
        json!({"factor": factor}),
    )
    .await;
    throttle_failure(
        st,
        &mfa::mfa_key(flow.user_id),
        mfa::MFA_MAX_FAILURES,
        Some((flow.user_id, &email)),
    )
    .await?;
    let ended = mfa::flow_failed(&st.db, flow.id).await?;
    Ok(AppError(
        StatusCode::UNAUTHORIZED,
        if ended {
            "Zu viele falsche Codes. Bitte melde dich erneut an.".into()
        } else {
            "Der Code stimmt nicht.".into()
        },
    ))
}

pub async fn mfa_totp(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Json(b): Json<MfaCode>,
) -> ApiResult<impl IntoResponse> {
    let flow = pending_flow(&st, &jar).await?;
    throttle_check(&st, &mfa::mfa_key(flow.user_id)).await?;
    if !mfa::verify_totp(&st.db, &st.cfg.master_key, flow.user_id, &b.code, false).await? {
        return Err(second_factor_failed(&st, &flow, "totp").await?);
    }
    let jar = second_factor_done(&st, jar, &flow, "totp", &user_agent(&headers)).await?;
    Ok((jar, Json(json!({"ok": true}))))
}

pub async fn mfa_recovery(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Json(b): Json<MfaCode>,
) -> ApiResult<impl IntoResponse> {
    let flow = pending_flow(&st, &jar).await?;
    throttle_check(&st, &mfa::mfa_key(flow.user_id)).await?;
    if !mfa::use_recovery(&st.db, &st.cfg.master_key, flow.user_id, &b.code).await? {
        return Err(second_factor_failed(&st, &flow, "recovery").await?);
    }
    let (email, _) = user_label(&st, flow.user_id).await?;
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM recovery_codes WHERE user_id=$1 AND used_at IS NULL",
    )
    .bind(flow.user_id)
    .fetch_one(&st.db)
    .await?;
    audit::log_as(
        &st,
        Some((flow.user_id, &email)),
        "auth.recovery_used",
        ("user", &flow.user_id.to_string()),
        json!({"left": left}),
    )
    .await;
    crate::notify::notify(
        &st,
        flow.user_id,
        crate::notify::Note {
            kind: "security",
            title: "Wiederherstellungscode verwendet".into(),
            body: format!("Jemand hat sich mit einem deiner Wiederherstellungscodes angemeldet. Übrig: {left}."),
            task_id: None,
            push: true,
        },
    )
    .await;
    let jar = second_factor_done(&st, jar, &flow, "recovery", &user_agent(&headers)).await?;
    Ok((jar, Json(json!({"ok": true}))))
}

pub async fn mfa_webauthn_start(
    State(st): State<AppState>,
    jar: CookieJar,
) -> ApiResult<Json<WebauthnChallenge>> {
    let flow = pending_flow(&st, &jar).await?;
    throttle_check(&st, &mfa::mfa_key(flow.user_id)).await?;
    let keys: Vec<Passkey> = mfa::passkeys(&st.db, flow.user_id)
        .await?
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    if keys.is_empty() {
        return Err(AppError::bad_request(
            "Für dieses Konto ist kein Passkey eingerichtet.",
        ));
    }
    let wa = mfa::webauthn(&st.cfg.public_url)?;
    let (rcr, state) = wa
        .start_passkey_authentication(&keys)
        .map_err(|e| anyhow!("webauthn: {e:?}"))?;
    mfa::set_flow_state(
        &st.db,
        flow.id,
        serde_json::to_value(&state).map_err(anyhow::Error::from)?,
    )
    .await?;
    Ok(Json(WebauthnChallenge {
        options: serde_json::to_value(&rcr).map_err(anyhow::Error::from)?,
    }))
}

pub async fn mfa_webauthn_finish(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Json(b): Json<WebauthnResponse>,
) -> ApiResult<impl IntoResponse> {
    let flow = pending_flow(&st, &jar).await?;
    throttle_check(&st, &mfa::mfa_key(flow.user_id)).await?;
    let state: PasskeyAuthentication = flow
        .state
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| AppError::bad_request("Bitte den Passkey erneut anfragen."))?;
    let cred: PublicKeyCredential = serde_json::from_value(b.credential)
        .map_err(|_| AppError::bad_request("invalid credential"))?;
    let wa = mfa::webauthn(&st.cfg.public_url)?;
    let res = match wa.finish_passkey_authentication(&cred, &state) {
        Ok(r) => r,
        Err(e) => {
            tracing::info!("passkey login of {} failed: {e:?}", flow.user_id);
            return Err(second_factor_failed(&st, &flow, "webauthn").await?);
        }
    };
    passkey_used(&st, flow.user_id, &res).await?;
    let jar = second_factor_done(&st, jar, &flow, "webauthn", &user_agent(&headers)).await?;
    Ok((jar, Json(json!({"ok": true}))))
}

/// Stores the new signature counter / backup state of the used passkey.
pub async fn passkey_used(st: &AppState, user: Uuid, res: &AuthenticationResult) -> ApiResult<()> {
    for (id, mut pk) in mfa::passkeys(&st.db, user).await? {
        if pk.cred_id() == res.cred_id() {
            pk.update_credential(res);
            sqlx::query(
                "UPDATE webauthn_credentials SET passkey=$2, last_used_at=now() WHERE id=$1",
            )
            .bind(id)
            .bind(serde_json::to_value(&pk).map_err(anyhow::Error::from)?)
            .execute(&st.db)
            .await?;
        }
    }
    Ok(())
}

pub async fn logout(State(st): State<AppState>, jar: CookieJar) -> ApiResult<impl IntoResponse> {
    if let Some(c) = jar.get(COOKIE) {
        let who: Option<(Uuid, String)> = sqlx::query_as(
            "DELETE FROM sessions s USING users u WHERE s.token_hash = $1 AND u.id = s.user_id RETURNING u.id, u.email",
        )
        .bind(sha256_hex(c.value()))
        .fetch_optional(&st.db)
        .await?;
        if let Some((id, email)) = who {
            audit::log_as(
                &st,
                Some((id, &email)),
                "auth.logout",
                ("user", &id.to_string()),
                json!({}),
            )
            .await;
        }
    }
    Ok((
        jar.remove(Cookie::build(COOKIE).path("/"))
            .remove(Cookie::build(MFA_COOKIE).path("/api/auth")),
        Json(json!({"ok": true})),
    ))
}

pub async fn me(AuthUser(u): AuthUser) -> Json<User> {
    Json(u)
}

// ---------------------------------------------------------------------------
// OIDC
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct OidcStartParams {
    /// WP11: invitation token for a new account.
    #[serde(default)]
    invite: Option<String>,
}

pub async fn oidc_start(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<OidcStartParams>,
) -> ApiResult<Redirect> {
    let p = st
        .cfg
        .oidc
        .iter()
        .find(|p| p.id == provider)
        .ok_or_else(AppError::not_found)?;
    let (state, nonce, verifier) = (random_token(), random_token(), random_token());
    sqlx::query("DELETE FROM oauth_states WHERE created_at < now() - interval '15 minutes'")
        .execute(&st.db)
        .await?;
    let invite_hash = q
        .invite
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .map(|i| sha256_hex(&i));
    sqlx::query(
        "INSERT INTO oauth_states (state, provider, nonce, pkce_verifier, invite_hash) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(&state)
    .bind(&p.id)
    .bind(&nonce)
    .bind(&verifier)
    .bind(invite_hash)
    .execute(&st.db)
    .await?;
    Ok(Redirect::to(&p.authorize_url(
        &st.cfg.public_url,
        &state,
        &nonce,
        &verifier,
    )?))
}

#[derive(Deserialize)]
pub struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    /// Apple form_post: JSON with name on first login.
    user: Option<String>,
}

pub async fn oidc_callback_get(
    st: State<AppState>,
    path: Path<String>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Query(q): Query<CallbackParams>,
) -> Response {
    oidc_callback(st, path, jar, headers, q).await
}

pub async fn oidc_callback_post(
    st: State<AppState>,
    path: Path<String>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Form(q): Form<CallbackParams>,
) -> Response {
    oidc_callback(st, path, jar, headers, q).await
}

async fn oidc_callback(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    q: CallbackParams,
) -> Response {
    match oidc_callback_inner(&st, &provider, jar, &user_agent(&headers), q).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("OIDC login via {provider} failed: {}", e.1);
            Redirect::to(&format!("/?login_error={}", urlencode(&e.1))).into_response()
        }
    }
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn oidc_callback_inner(
    st: &AppState,
    provider: &str,
    jar: CookieJar,
    ua: &str,
    q: CallbackParams,
) -> ApiResult<Response> {
    if let Some(e) = q.error {
        return Err(AppError::bad_request(format!("provider error: {e}")));
    }
    let p = st
        .cfg
        .oidc
        .iter()
        .find(|p| p.id == provider)
        .ok_or_else(AppError::not_found)?;
    let (code, state) = (
        q.code
            .ok_or_else(|| AppError::bad_request("missing code"))?,
        q.state.unwrap_or_default(),
    );
    let (nonce, verifier, invite_hash): (String, String, Option<String>) = sqlx::query_as(
        "DELETE FROM oauth_states WHERE state=$1 AND provider=$2 AND created_at > now() - interval '15 minutes'
         RETURNING nonce, pkce_verifier, invite_hash",
    )
    .bind(&state)
    .bind(provider)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(|| AppError::bad_request("login expired, please retry"))?;

    let ident = p
        .exchange(
            &st.http,
            &st.cfg.public_url,
            &code,
            &verifier,
            &nonce,
            q.user.as_deref(),
        )
        .await
        .map_err(|e| {
            tracing::warn!("token exchange: {e:#}");
            AppError::bad_request("login failed")
        })?;
    let user_id = find_or_create_user(st, provider, &ident, invite_hash.as_deref()).await?;
    let (email, disabled) = user_label(st, user_id).await?;
    if disabled {
        audit::log_as(
            st,
            Some((user_id, &email)),
            "auth.login_failed",
            ("user", &user_id.to_string()),
            json!({"reason": "disabled", "method": format!("oidc:{}", p.name)}),
        )
        .await;
        return Err(AppError(StatusCode::FORBIDDEN, ACCOUNT_DISABLED.into()));
    }
    // WP11: a local second factor applies after OIDC as well.
    let method = format!("oidc:{}", p.name);
    let (jar, out) = after_first_factor(st, jar, user_id, &email, &method, ua).await?;
    let to = if out.mfa_required { "/?mfa=1" } else { "/" };
    Ok((jar, Redirect::to(to)).into_response())
}

async fn find_or_create_user(
    st: &AppState,
    provider: &str,
    id: &ExternalIdentity,
    invite_hash: Option<&str>,
) -> ApiResult<Uuid> {
    if let Some(uid) = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM identities WHERE provider=$1 AND subject=$2",
    )
    .bind(provider)
    .bind(&id.subject)
    .fetch_optional(&st.db)
    .await?
    {
        return Ok(uid);
    }
    // Link to an existing account only by *verified* email.
    let existing = match (&id.email, id.email_verified) {
        (Some(e), true) => {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE email=$1")
                .bind(e)
                .fetch_optional(&st.db)
                .await?
        }
        _ => None,
    };
    let uid = match existing {
        Some(uid) => uid,
        None => {
            match instance::settings(st).await?.registration_mode {
                RegistrationMode::Closed => {
                    return Err(AppError::bad_request("registration disabled"))
                }
                RegistrationMode::Invite if invite_hash.is_none() => {
                    return Err(AppError::bad_request("Registrierung nur mit Einladung."))
                }
                _ => {}
            }
            let email = match (&id.email, id.email_verified) {
                (Some(e), true) => e.clone(),
                _ => format!("{provider}-{}@users.invalid", id.subject),
            };
            let name = id
                .name
                .clone()
                .unwrap_or_else(|| email.split('@').next().unwrap_or("").to_string());
            let uid = insert_user(st, &email, &name, None, invite_hash).await?;
            audit::log_as(
                st,
                Some((uid, &email)),
                "auth.register",
                ("user", &uid.to_string()),
                json!({"invited": invite_hash.is_some(), "provider": provider}),
            )
            .await;
            uid
        }
    };
    sqlx::query("INSERT INTO identities (provider, subject, user_id, email) VALUES ($1,$2,$3,$4)")
        .bind(provider)
        .bind(&id.subject)
        .bind(uid)
        .bind(&id.email)
        .execute(&st.db)
        .await?;
    Ok(uid)
}
