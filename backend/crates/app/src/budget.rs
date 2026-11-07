//! Token budgets per user and provider (Claude, Codex/GPT, Gemini).
//!
//! * Usage of every agent run is logged (`usage_log`).
//! * Rolling windows: 5 hours (subscription session window), 7 days (weekly
//!   cap) and 30 days in USD (API keys).
//! * Counted tokens = input + output + cache writes. Cache reads are shown
//!   separately (providers bill/limit them far lower).
//! * A provider is blocked for a user when a configured budget is exhausted or
//!   when the CLI reported a subscription limit (`limited_until`).
//! * When a real subscription limit is hit we store the tokens used in the 5h
//!   window as `observed_limit_tokens` – an empirical value to calibrate budgets.
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Usage;

pub const WINDOW_5H: i64 = 5;
pub const WINDOW_7D: i64 = 7 * 24;
pub const WINDOW_30D: i64 = 30 * 24;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Budget {
    pub limit_5h_tokens: Option<i64>,
    pub limit_7d_tokens: Option<i64>,
    pub limit_30d_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Window {
    pub used_5h: i64,
    pub used_7d: i64,
    pub cost_30d: f64,
    pub cache_read_5h: i64,
    pub runs_5h: i64,
    pub oldest_5h: Option<DateTime<Utc>>,
    pub oldest_7d: Option<DateTime<Utc>>,
    pub oldest_30d: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub provider: String,
    #[serde(flatten)]
    pub window: Window,
    #[serde(flatten)]
    pub budget: Budget,
    /// Subscription limit reported by the CLI.
    pub limited_until: Option<DateTime<Utc>>,
    /// Own budget exhausted until (approx., rolling window).
    pub budget_until: Option<DateTime<Utc>>,
    pub budget_reason: Option<String>,
    pub observed_limit_tokens: Option<i64>,
    pub observed_at: Option<DateTime<Utc>>,
    /// Paused by an authentication/billing failure until credentials are
    /// updated or the user resumes the provider.
    pub paused_reason: Option<String>,
    pub blocked: bool,
}

pub fn counted(u: &Usage) -> i64 {
    u.input_tokens
        .saturating_add(u.output_tokens)
        .saturating_add(u.cache_write_tokens)
}

/// Pure decision: is the budget exhausted, and until when (rolling window:
/// the oldest entry leaves the window)?
pub fn evaluate(w: &Window, b: &Budget, now: DateTime<Utc>) -> Option<(DateTime<Utc>, String)> {
    let mut hits: Vec<(DateTime<Utc>, String)> = vec![];
    if let Some(l) = b.limit_5h_tokens.filter(|l| *l > 0 && w.used_5h >= *l) {
        hits.push((
            w.oldest_5h.unwrap_or(now) + Duration::hours(WINDOW_5H),
            format!("5h-Budget erreicht ({} / {l} Tokens)", w.used_5h),
        ));
    }
    if let Some(l) = b.limit_7d_tokens.filter(|l| *l > 0 && w.used_7d >= *l) {
        hits.push((
            w.oldest_7d.unwrap_or(now) + Duration::hours(WINDOW_7D),
            format!("7-Tage-Budget erreicht ({} / {l} Tokens)", w.used_7d),
        ));
    }
    if let Some(l) = b.limit_30d_usd.filter(|l| *l > 0.0 && w.cost_30d >= *l) {
        hits.push((
            w.oldest_30d.unwrap_or(now) + Duration::hours(WINDOW_30D),
            format!("30-Tage-Budget erreicht (${:.2} / ${l:.2})", w.cost_30d),
        ));
    }
    // The latest reset wins – all exhausted windows must have recovered.
    hits.into_iter().max_by_key(|(t, _)| *t)
}

pub async fn record_usage(
    db: &PgPool,
    user: Uuid,
    provider: &str,
    task: Option<Uuid>,
    u: &Usage,
) -> sqlx::Result<()> {
    if counted(u) == 0 && u.cache_read_tokens == 0 && u.cost_usd == 0.0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO usage_log (user_id, provider, task_id, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(user)
    .bind(provider)
    .bind(task)
    .bind(u.input_tokens)
    .bind(u.output_tokens)
    .bind(u.cache_read_tokens)
    .bind(u.cache_write_tokens)
    .bind(u.cost_usd)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn window(db: &PgPool, user: Uuid, provider: &str) -> sqlx::Result<Window> {
    let row: (i64, i64, f64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT
            COALESCE(SUM(input_tokens + output_tokens + cache_write_tokens) FILTER (WHERE ts > now() - interval '5 hours'), 0)::BIGINT,
            COALESCE(SUM(input_tokens + output_tokens + cache_write_tokens) FILTER (WHERE ts > now() - interval '7 days'), 0)::BIGINT,
            COALESCE(SUM(cost_usd), 0)::DOUBLE PRECISION,
            COALESCE(SUM(cache_read_tokens) FILTER (WHERE ts > now() - interval '5 hours'), 0)::BIGINT,
            COUNT(*) FILTER (WHERE ts > now() - interval '5 hours'),
            MIN(ts) FILTER (WHERE ts > now() - interval '5 hours'),
            MIN(ts) FILTER (WHERE ts > now() - interval '7 days'),
            MIN(ts)
         FROM usage_log WHERE user_id = $1 AND provider = $2 AND ts > now() - interval '30 days'",
    )
    .bind(user)
    .bind(provider)
    .fetch_one(db)
    .await?;
    Ok(Window {
        used_5h: row.0,
        used_7d: row.1,
        cost_30d: row.2,
        cache_read_5h: row.3,
        runs_5h: row.4,
        oldest_5h: row.5,
        oldest_7d: row.6,
        oldest_30d: row.7,
    })
}

pub async fn get_budget(db: &PgPool, user: Uuid, provider: &str) -> sqlx::Result<Budget> {
    let r: Option<(Option<i64>, Option<i64>, Option<f64>)> =
        sqlx::query_as("SELECT limit_5h_tokens, limit_7d_tokens, limit_30d_usd FROM provider_budgets WHERE user_id=$1 AND provider=$2")
            .bind(user)
            .bind(provider)
            .fetch_optional(db)
            .await?;
    Ok(r.map(|(a, b, c)| Budget {
        limit_5h_tokens: a,
        limit_7d_tokens: b,
        limit_30d_usd: c,
    })
    .unwrap_or_default())
}

pub async fn set_budget(db: &PgPool, user: Uuid, provider: &str, b: &Budget) -> sqlx::Result<()> {
    let pos = |v: Option<i64>| v.filter(|x| *x > 0);
    sqlx::query(
        "INSERT INTO provider_budgets (user_id, provider, limit_5h_tokens, limit_7d_tokens, limit_30d_usd) VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (user_id, provider) DO UPDATE SET limit_5h_tokens=EXCLUDED.limit_5h_tokens,
             limit_7d_tokens=EXCLUDED.limit_7d_tokens, limit_30d_usd=EXCLUDED.limit_30d_usd",
    )
    .bind(user)
    .bind(provider)
    .bind(pos(b.limit_5h_tokens))
    .bind(pos(b.limit_7d_tokens))
    .bind(b.limit_30d_usd.filter(|x| *x > 0.0))
    .execute(db)
    .await?;
    Ok(())
}

pub async fn status(db: &PgPool, user: Uuid, provider: &str) -> sqlx::Result<ProviderStatus> {
    let w = window(db, user, provider).await?;
    let b = get_budget(db, user, provider).await?;
    let lim: Option<(
        Option<DateTime<Utc>>,
        Option<i64>,
        Option<DateTime<Utc>>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT CASE WHEN limited_until > now() THEN limited_until END, observed_limit_tokens, observed_at, paused_reason
         FROM provider_limits WHERE user_id=$1 AND provider=$2",
    )
    .bind(user)
    .bind(provider)
    .fetch_optional(db)
    .await?;
    let (limited_until, observed_limit_tokens, observed_at, paused_reason) =
        lim.unwrap_or((None, None, None, None));
    let ev = evaluate(&w, &b, Utc::now());
    Ok(ProviderStatus {
        provider: provider.to_string(),
        blocked: limited_until.is_some() || paused_reason.is_some() || ev.is_some(),
        paused_reason,
        budget_until: ev.as_ref().map(|e| e.0),
        budget_reason: ev.map(|e| e.1),
        window: w,
        budget: b,
        limited_until,
        observed_limit_tokens,
        observed_at,
    })
}

pub async fn blocked_providers(
    db: &PgPool,
    user: Uuid,
    providers: &[&str],
) -> sqlx::Result<Vec<String>> {
    let mut out = vec![];
    for p in providers {
        if status(db, user, p).await?.blocked {
            out.push(p.to_string());
        }
    }
    Ok(out)
}

pub async fn is_blocked(db: &PgPool, user: Uuid, provider: &str) -> sqlx::Result<bool> {
    Ok(status(db, user, provider).await?.blocked)
}

/// Subscription limit reported by a CLI: pause + remember observed usage.
pub async fn set_limited(
    db: &PgPool,
    user: Uuid,
    provider: &str,
    until: DateTime<Utc>,
) -> sqlx::Result<()> {
    let used = window(db, user, provider).await?.used_5h;
    sqlx::query(
        "INSERT INTO provider_limits (user_id, provider, limited_until, observed_limit_tokens, observed_at) VALUES ($1,$2,$3,$4,now())
         ON CONFLICT (user_id, provider) DO UPDATE SET limited_until=EXCLUDED.limited_until,
             observed_limit_tokens=EXCLUDED.observed_limit_tokens, observed_at=now()",
    )
    .bind(user)
    .bind(provider)
    .bind(until)
    .bind(used)
    .execute(db)
    .await?;
    Ok(())
}

/// Authentication/billing failure: pause the provider until credentials are
/// updated or the user resumes it explicitly.
pub async fn set_paused(db: &PgPool, user: Uuid, provider: &str, reason: &str) -> sqlx::Result<()> {
    let reason: String = reason.chars().take(500).collect();
    sqlx::query(
        "INSERT INTO provider_limits (user_id, provider, paused_reason) VALUES ($1,$2,$3)
         ON CONFLICT (user_id, provider) DO UPDATE SET paused_reason=EXCLUDED.paused_reason",
    )
    .bind(user)
    .bind(provider)
    .bind(reason)
    .execute(db)
    .await?;
    Ok(())
}

/// Lifts an auth pause (and with `with_limit` also a detected subscription
/// limit). Returns whether anything was paused.
pub async fn clear_pause(
    db: &PgPool,
    user: Uuid,
    provider: &str,
    with_limit: bool,
) -> sqlx::Result<bool> {
    Ok(sqlx::query(
        "UPDATE provider_limits SET paused_reason=NULL,
                limited_until = CASE WHEN $3 THEN NULL ELSE limited_until END
         WHERE user_id=$1 AND provider=$2
           AND (paused_reason IS NOT NULL OR ($3 AND limited_until > now()))",
    )
    .bind(user)
    .bind(provider)
    .bind(with_limit)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// Reset estimate when the CLI didn't say: the oldest usage leaves the 5h window.
pub async fn fallback_reset(
    db: &PgPool,
    user: Uuid,
    provider: &str,
    fallback_min: i64,
) -> DateTime<Utc> {
    let now = Utc::now();
    match window(db, user, provider)
        .await
        .ok()
        .and_then(|w| w.oldest_5h)
    {
        Some(t) if t + Duration::hours(WINDOW_5H) > now => {
            t + Duration::hours(WINDOW_5H) + Duration::minutes(2)
        }
        _ => now + Duration::minutes(fallback_min),
    }
}

/// Returns Some(reason, until) exactly once per exhaustion period.
pub async fn take_budget_notification(
    db: &PgPool,
    user: Uuid,
    provider: &str,
) -> sqlx::Result<Option<(String, DateTime<Utc>)>> {
    let s = status(db, user, provider).await?;
    let (Some(until), Some(reason)) = (s.budget_until, s.budget_reason) else {
        return Ok(None);
    };
    let updated = sqlx::query(
        "INSERT INTO provider_limits (user_id, provider, budget_notified_until) VALUES ($1,$2,$3)
         ON CONFLICT (user_id, provider) DO UPDATE SET budget_notified_until=EXCLUDED.budget_notified_until
         WHERE provider_limits.budget_notified_until IS NULL OR provider_limits.budget_notified_until < now()",
    )
    .bind(user)
    .bind(provider)
    .bind(until)
    .execute(db)
    .await?
    .rows_affected();
    Ok((updated > 0).then_some((reason, until)))
}

pub async fn prune(db: &PgPool) -> sqlx::Result<u64> {
    Ok(
        sqlx::query("DELETE FROM usage_log WHERE ts < now() - interval '35 days'")
            .execute(db)
            .await?
            .rows_affected(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(used_5h: i64, used_7d: i64, cost: f64) -> Window {
        let t = Utc::now() - Duration::hours(1);
        Window {
            used_5h,
            used_7d,
            cost_30d: cost,
            oldest_5h: Some(t),
            oldest_7d: Some(t - Duration::days(2)),
            oldest_30d: Some(t - Duration::days(10)),
            ..Default::default()
        }
    }

    #[test]
    fn unlimited_budget_never_blocks() {
        assert!(evaluate(
            &w(10_000_000, 10_000_000, 1000.0),
            &Budget::default(),
            Utc::now()
        )
        .is_none());
    }

    #[test]
    fn five_hour_budget_blocks_until_oldest_entry_leaves_window() {
        let win = w(1000, 1000, 0.0);
        let (until, reason) = evaluate(
            &win,
            &Budget {
                limit_5h_tokens: Some(1000),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(until, win.oldest_5h.unwrap() + Duration::hours(5));
        assert!(reason.contains("5h"));
        assert!(evaluate(
            &w(999, 999, 0.0),
            &Budget {
                limit_5h_tokens: Some(1000),
                ..Default::default()
            },
            Utc::now()
        )
        .is_none());
    }

    #[test]
    fn latest_reset_wins_when_multiple_windows_are_exhausted() {
        let win = w(500, 5000, 20.0);
        let b = Budget {
            limit_5h_tokens: Some(100),
            limit_7d_tokens: Some(1000),
            limit_30d_usd: Some(10.0),
        };
        let (until, reason) = evaluate(&win, &b, Utc::now()).unwrap();
        assert_eq!(until, win.oldest_30d.unwrap() + Duration::hours(WINDOW_30D));
        assert!(reason.contains("30-Tage"));
    }

    #[test]
    fn zero_or_negative_limits_are_ignored() {
        let b = Budget {
            limit_5h_tokens: Some(0),
            limit_7d_tokens: Some(-1),
            limit_30d_usd: Some(0.0),
        };
        assert!(evaluate(&w(10, 10, 10.0), &b, Utc::now()).is_none());
    }

    #[test]
    fn counted_excludes_cache_reads() {
        let u = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 1000,
            cache_write_tokens: 3,
            cost_usd: 0.0,
        };
        assert_eq!(counted(&u), 18);
    }
}
