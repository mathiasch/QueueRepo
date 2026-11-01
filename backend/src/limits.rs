//! Subscription window accounting (rolling 5h windows) and rate-limit pauses.
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::models::Usage;

pub async fn record_usage(db: &sqlx::PgPool, user: Uuid, provider: &str, u: &Usage) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO provider_limits (user_id, provider, window_started_at, input_tokens, output_tokens, cache_read_tokens, cost_usd, runs)
         VALUES ($1, $2, now(), $3, $4, $5, $6, 1)
         ON CONFLICT (user_id, provider) DO UPDATE SET
           window_started_at = CASE WHEN fresh THEN now() ELSE provider_limits.window_started_at END,
           input_tokens      = CASE WHEN fresh THEN 0 ELSE provider_limits.input_tokens END + EXCLUDED.input_tokens,
           output_tokens     = CASE WHEN fresh THEN 0 ELSE provider_limits.output_tokens END + EXCLUDED.output_tokens,
           cache_read_tokens = CASE WHEN fresh THEN 0 ELSE provider_limits.cache_read_tokens END + EXCLUDED.cache_read_tokens,
           cost_usd          = CASE WHEN fresh THEN 0 ELSE provider_limits.cost_usd END + EXCLUDED.cost_usd,
           runs              = CASE WHEN fresh THEN 0 ELSE provider_limits.runs END + 1"
            .replace(
                "fresh",
                "(provider_limits.window_started_at IS NULL OR provider_limits.window_started_at < now() - interval '5 hours')",
            )
            .as_str(),
    )
    .bind(user)
    .bind(provider)
    .bind(u.input_tokens)
    .bind(u.output_tokens)
    .bind(u.cache_read_tokens)
    .bind(u.cost_usd)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_limited(db: &sqlx::PgPool, user: Uuid, provider: &str, until: DateTime<Utc>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO provider_limits (user_id, provider, limited_until) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, provider) DO UPDATE SET limited_until = EXCLUDED.limited_until",
    )
    .bind(user)
    .bind(provider)
    .bind(until)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn is_limited(db: &sqlx::PgPool, user: Uuid, provider: &str) -> sqlx::Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM provider_limits WHERE user_id=$1 AND provider=$2 AND limited_until > now())",
    )
    .bind(user)
    .bind(provider)
    .fetch_one(db)
    .await?)
}

/// Fallback when no reset time is known: end of the current 5h window, else N minutes.
pub async fn fallback_reset(db: &sqlx::PgPool, user: Uuid, provider: &str, fallback_min: i64) -> DateTime<Utc> {
    let ws: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT window_started_at FROM provider_limits WHERE user_id=$1 AND provider=$2")
            .bind(user)
            .bind(provider)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .flatten();
    let now = Utc::now();
    match ws.map(|w| w + chrono::Duration::hours(5)) {
        Some(end) if end > now => end + chrono::Duration::minutes(2),
        _ => now + chrono::Duration::minutes(fallback_min),
    }
}
