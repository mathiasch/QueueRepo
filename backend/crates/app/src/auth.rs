use anyhow::anyhow;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, Path, Query, State},
    http::request::Parts,
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use queued_api::{LoginRequest, RegisterRequest};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    crypto::{random_token, sha256_hex},
    error::{ApiResult, AppError},
    models::User,
    oidc::ExternalIdentity,
    state::AppState,
};

pub const COOKIE: &str = "qsid";
const SESSION_DAYS: i64 = 30;

/// Authenticated user extractor (session cookie).
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(COOKIE)
            .map(|c| c.value().to_string())
            .ok_or_else(AppError::unauthorized)?;
        let user = sqlx::query_as::<_, User>(
            "SELECT u.id, u.email, u.name, u.is_admin FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1 AND s.expires_at > now()",
        )
        .bind(sha256_hex(&token))
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::unauthorized)?;
        Ok(AuthUser(user))
    }
}

async fn create_session(st: &AppState, jar: CookieJar, user_id: Uuid) -> ApiResult<CookieJar> {
    let token = random_token();
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, now() + make_interval(days => $3))")
        .bind(sha256_hex(&token))
        .bind(user_id)
        .bind(SESSION_DAYS as i32)
        .execute(&st.db)
        .await?;
    let cookie = Cookie::build((COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(st.cfg.secure_cookies())
        .max_age(time_days(SESSION_DAYS))
        .build();
    Ok(jar.add(cookie))
}

fn time_days(d: i64) -> time::Duration {
    time::Duration::days(d)
}

/// Creates the user row + default settings. First user becomes admin.
async fn insert_user(
    st: &AppState,
    email: &str,
    name: &str,
    pw_hash: Option<String>,
) -> ApiResult<Uuid> {
    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, email, name, password_hash, is_admin) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(email)
    .bind(name)
    .bind(pw_hash)
    .bind(count == 0)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO user_settings (user_id) VALUES ($1)")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

async fn hash_password(pw: String) -> ApiResult<String> {
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

pub async fn register(
    State(st): State<AppState>,
    jar: CookieJar,
    Json(r): Json<RegisterRequest>,
) -> ApiResult<impl IntoResponse> {
    if !st.cfg.allow_registration {
        return Err(AppError::forbidden());
    }
    let email = r.email.trim().to_lowercase();
    if !email.contains('@') || r.password.len() < 8 {
        return Err(AppError::bad_request(
            "valid email and password (min. 8 chars) required",
        ));
    }
    let hash = hash_password(r.password).await?;
    let name = if r.name.trim().is_empty() {
        email.split('@').next().unwrap_or("").to_string()
    } else {
        r.name.trim().to_string()
    };
    let id = insert_user(&st, &email, &name, Some(hash)).await?;
    let jar = create_session(&st, jar, id).await?;
    Ok((jar, Json(json!({"ok": true}))))
}

pub async fn login(
    State(st): State<AppState>,
    jar: CookieJar,
    Json(r): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    let row: Option<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, password_hash FROM users WHERE email = $1")
            .bind(r.email.trim().to_lowercase())
            .fetch_optional(&st.db)
            .await?;
    let invalid = || {
        AppError(
            axum::http::StatusCode::UNAUTHORIZED,
            "invalid credentials".into(),
        )
    };
    let (id, hash) = row.ok_or_else(invalid)?;
    let hash = hash.ok_or_else(invalid)?;
    let pw = r.password;
    let ok = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
            .unwrap_or(false)
    })
    .await
    .map_err(|e| anyhow!(e))?;
    if !ok {
        return Err(invalid());
    }
    let jar = create_session(&st, jar, id).await?;
    Ok((jar, Json(json!({"ok": true}))))
}

pub async fn logout(State(st): State<AppState>, jar: CookieJar) -> ApiResult<impl IntoResponse> {
    if let Some(c) = jar.get(COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(sha256_hex(c.value()))
            .execute(&st.db)
            .await?;
    }
    Ok((
        jar.remove(Cookie::build(COOKIE).path("/")),
        Json(json!({"ok": true})),
    ))
}

pub async fn me(AuthUser(u): AuthUser) -> Json<User> {
    Json(u)
}

// ---------------------------------------------------------------------------
// OIDC
// ---------------------------------------------------------------------------

pub async fn oidc_start(
    State(st): State<AppState>,
    Path(provider): Path<String>,
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
    sqlx::query(
        "INSERT INTO oauth_states (state, provider, nonce, pkce_verifier) VALUES ($1,$2,$3,$4)",
    )
    .bind(&state)
    .bind(&p.id)
    .bind(&nonce)
    .bind(&verifier)
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
    Query(q): Query<CallbackParams>,
) -> Response {
    oidc_callback(st, path, jar, q).await
}

pub async fn oidc_callback_post(
    st: State<AppState>,
    path: Path<String>,
    jar: CookieJar,
    Form(q): Form<CallbackParams>,
) -> Response {
    oidc_callback(st, path, jar, q).await
}

async fn oidc_callback(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    jar: CookieJar,
    q: CallbackParams,
) -> Response {
    match oidc_callback_inner(&st, &provider, jar, q).await {
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
    let (nonce, verifier): (String, String) = sqlx::query_as(
        "DELETE FROM oauth_states WHERE state=$1 AND provider=$2 AND created_at > now() - interval '15 minutes'
         RETURNING nonce, pkce_verifier",
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
    let user_id = find_or_create_user(st, provider, &ident).await?;
    let jar = create_session(st, jar, user_id).await?;
    Ok((jar, Redirect::to("/")).into_response())
}

async fn find_or_create_user(
    st: &AppState,
    provider: &str,
    id: &ExternalIdentity,
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
            if !st.cfg.allow_registration {
                return Err(AppError::bad_request("registration disabled"));
            }
            let email = match (&id.email, id.email_verified) {
                (Some(e), true) => e.clone(),
                _ => format!("{provider}-{}@users.invalid", id.subject),
            };
            let name = id
                .name
                .clone()
                .unwrap_or_else(|| email.split('@').next().unwrap_or("").to_string());
            insert_user(st, &email, &name, None).await?
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
