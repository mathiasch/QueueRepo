//! WP11: instance-wide settings chosen in the admin console (registration
//! mode, MFA policy) and per-user quotas.
use queued_api::security::{InstanceSettings, MfaPolicy, Quota, QuotaStatus, RegistrationMode};
use uuid::Uuid;

use crate::state::AppState;

pub async fn settings(st: &AppState) -> sqlx::Result<InstanceSettings> {
    let (mode, policy): (Option<String>, String) =
        sqlx::query_as("SELECT registration_mode, mfa_policy FROM instance_settings WHERE id=1")
            .fetch_one(&st.db)
            .await?;
    let from_env = mode.is_none();
    let registration_mode =
        mode.as_deref()
            .and_then(RegistrationMode::parse)
            .unwrap_or(if st.cfg.allow_registration {
                RegistrationMode::Open
            } else {
                RegistrationMode::Closed
            });
    Ok(InstanceSettings {
        registration_mode,
        registration_from_env: from_env,
        mfa_policy: MfaPolicy::parse(&policy).unwrap_or(MfaPolicy::Off),
    })
}

pub async fn quota(db: &sqlx::PgPool, user: Uuid) -> sqlx::Result<Quota> {
    let row: Option<(Option<f64>, Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT max_cost_30d_usd, max_parallel, max_projects FROM user_quotas WHERE user_id=$1",
    )
    .bind(user)
    .fetch_optional(db)
    .await?;
    Ok(row
        .map(|(c, p, n)| Quota {
            max_cost_30d_usd: c,
            max_parallel: p,
            max_projects: n,
        })
        .unwrap_or_default())
}

/// USD cost of the last 30 days (WP13 cost accounting, like `limit_30d_usd`).
pub async fn cost_30d(db: &sqlx::PgPool, user: Uuid) -> sqlx::Result<f64> {
    sqlx::query_scalar(
        "SELECT coalesce(sum(cost_usd), 0)::float8 FROM usage_log
         WHERE user_id=$1 AND ts > now() - interval '30 days'",
    )
    .bind(user)
    .fetch_one(db)
    .await
}

pub async fn quota_status(db: &sqlx::PgPool, user: Uuid) -> sqlx::Result<QuotaStatus> {
    let quota = quota(db, user).await?;
    let cost = cost_30d(db, user).await?;
    let projects: i64 =
        sqlx::query_scalar("SELECT count(*) FROM projects WHERE owner_id=$1 AND NOT is_eval")
            .bind(user)
            .fetch_one(db)
            .await?;
    let exhausted = quota.max_cost_30d_usd.is_some_and(|m| cost >= m);
    Ok(QuotaStatus {
        quota,
        cost_30d_usd: cost,
        projects,
        exhausted,
    })
}
