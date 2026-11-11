//! WP07: personal API tokens. Management is cookie-only (none of these
//! routes is in `queued_api::tokens::ROUTES`, so a token gets 403); only
//! `GET /api/whoami` answers to any valid token.
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use queued_api::tokens::{
    self, ApiTokenView, CreatedApiToken, NewApiToken, Scope, WhoAmI, BASE62, BODY_LEN, MAX_TOKENS,
};
use rand::Rng;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{AuthUser, TokenAuth},
    crypto::sha256_hex,
    error::{ApiResult, AppError},
    state::AppState,
};

type Row = (
    Uuid,
    String,
    String,
    Vec<String>,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

const COLS: &str = "id, name, prefix, scopes, created_at, expires_at, last_used_at, revoked_at";

fn view(r: Row) -> ApiTokenView {
    let (id, name, prefix, scopes, created_at, expires_at, last_used_at, revoked_at) = r;
    ApiTokenView {
        id,
        name,
        prefix,
        scopes: scopes.iter().filter_map(|s| Scope::parse(s)).collect(),
        created_at,
        expires_at,
        last_used_at,
        revoked_at,
    }
}

/// A new random token: `qk_` + 43 base62 characters from the OS RNG
/// (uniform, ≈ 256 bit) + checksum.
pub fn generate(prefix: &str) -> String {
    let mut rng = rand::rngs::OsRng;
    let body: String = (0..BODY_LEN)
        .map(|_| BASE62[rng.gen_range(0..BASE62.len())] as char)
        .collect();
    tokens::compose(prefix, &body)
}

pub async fn list(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<ApiTokenView>>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM api_tokens WHERE user_id=$1 ORDER BY created_at DESC"
    ))
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(rows.into_iter().map(view).collect()))
}

pub async fn create(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(t): Json<NewApiToken>,
) -> ApiResult<Json<CreatedApiToken>> {
    let (name, scopes) = tokens::validate_new(&t).map_err(AppError::bad_request)?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens WHERE user_id=$1")
        .bind(u.id)
        .fetch_one(&st.db)
        .await?;
    if count >= MAX_TOKENS {
        return Err(AppError::conflict(format!(
            "höchstens {MAX_TOKENS} Tokens – lösche alte, bevor du neue anlegst"
        )));
    }
    let token = generate(tokens::TOKEN_PREFIX);
    let scopes: Vec<String> = scopes.iter().map(|s| s.as_str().to_string()).collect();
    let row: Row = sqlx::query_as(&format!(
        "INSERT INTO api_tokens (id, user_id, name, prefix, token_hash, scopes, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6, now() + make_interval(days => $7))
         RETURNING {COLS}"
    ))
    .bind(Uuid::new_v4())
    .bind(u.id)
    .bind(&name)
    .bind(tokens::shown_prefix(&token))
    .bind(sha256_hex(&token))
    .bind(&scopes)
    .bind(t.expires_in_days)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(CreatedApiToken {
        token,
        info: view(row),
    }))
}

pub async fn revoke(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ApiTokenView>> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "UPDATE api_tokens SET revoked_at=COALESCE(revoked_at, now())
         WHERE id=$1 AND user_id=$2 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(u.id)
    .fetch_optional(&st.db)
    .await?;
    Ok(Json(view(row.ok_or_else(AppError::not_found)?)))
}

pub async fn remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let n = sqlx::query("DELETE FROM api_tokens WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(u.id)
        .execute(&st.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(Json(json!({"ok": true})))
}

/// Who the token belongs to and what it may do (configuration check of
/// `queuectl`). Cookie sessions get 400: there is no token to describe.
pub async fn whoami(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    tok: Option<Extension<TokenAuth>>,
) -> ApiResult<Json<WhoAmI>> {
    let Some(Extension(tok)) = tok else {
        return Err(AppError::bad_request("only with an API token"));
    };
    let row: Row = sqlx::query_as(&format!("SELECT {COLS} FROM api_tokens WHERE id=$1"))
        .bind(tok.token_id)
        .fetch_one(&st.db)
        .await?;
    Ok(Json(WhoAmI {
        user_id: u.id,
        name: u.name,
        email: u.email,
        token: view(row),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_well_formed_and_distinct() {
        let a = generate(tokens::TOKEN_PREFIX);
        let b = generate(tokens::TOKEN_PREFIX);
        assert_ne!(a, b);
        assert!(tokens::well_formed(&a, tokens::TOKEN_PREFIX));
        let s = generate(tokens::SECRET_PREFIX);
        assert!(tokens::well_formed(&s, tokens::SECRET_PREFIX));
        // The secret scanner (WP03) recognises both.
        let text = format!("token={a}\nsecret={s}");
        let hits: Vec<_> = queued_common::secrets::scan(&text)
            .into_iter()
            .filter(|h| h.rule == "queue_key")
            .collect();
        assert_eq!(hits.len(), 2);
        // Roughly uniform alphabet use over many tokens.
        let all: String = (0..200)
            .map(|_| generate("qk_")[3..46].to_string())
            .collect();
        for c in BASE62 {
            let n = all.bytes().filter(|b| b == c).count();
            assert!((60..=220).contains(&n), "{} appears {n} times", *c as char);
        }
    }
}
