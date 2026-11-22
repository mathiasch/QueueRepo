//! WP11: append-only audit log. The database enforces it (triggers forbid
//! UPDATE/TRUNCATE and DELETE of entries younger than 365 days) and chains
//! every entry to its predecessor (`hash = sha256(prev_hash || content)`,
//! computed by the insert trigger). Details carry metadata only – never a
//! secret, token, password, code or credential value.
use serde_json::Value;
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::{models::User, state::AppState};

/// One entry. `actor` = (id, label at the time); `None` = the system.
pub struct Entry<'a> {
    pub actor: Option<(Uuid, &'a str)>,
    pub action: &'a str,
    pub target_type: &'a str,
    pub target_id: &'a str,
    pub project: Option<Uuid>,
    pub details: Value,
}

/// Appends an entry inside the caller's transaction or connection.
pub async fn record<'e, E: PgExecutor<'e>>(db: E, e: Entry<'_>) -> sqlx::Result<()> {
    debug_assert!(e.details.is_object());
    sqlx::query(
        "INSERT INTO audit_log (seq, prev_hash, hash, actor_id, actor_label, action, target_type, target_id, project_id, details)
         VALUES (0, '\\x', '\\x', $1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(e.actor.map(|a| a.0))
    .bind(e.actor.map(|a| a.1).unwrap_or("System"))
    .bind(e.action)
    .bind(e.target_type)
    .bind(e.target_id)
    .bind(e.project)
    .bind(if e.details.is_object() {
        e.details
    } else {
        serde_json::json!({})
    })
    .execute(db)
    .await?;
    Ok(())
}

/// Appends an entry for an action of `user` that already happened. A
/// failure is logged and counted, never shown to the user (the action
/// itself succeeded).
pub async fn log(
    st: &AppState,
    user: &User,
    action: &str,
    target: (&str, &str),
    project: Option<Uuid>,
    details: Value,
) {
    let e = Entry {
        actor: Some((user.id, &user.email)),
        action,
        target_type: target.0,
        target_id: target.1,
        project,
        details,
    };
    if let Err(err) = record(&st.db, e).await {
        tracing::error!("audit entry {action} lost: {err:#}");
        st.metrics.audit_errors.inc(&[]);
    }
}

/// Like [`log`] for an actor known only by id and label (login attempts
/// before a session exists, unknown e-mail addresses).
pub async fn log_as(
    st: &AppState,
    actor: Option<(Uuid, &str)>,
    action: &str,
    target: (&str, &str),
    details: Value,
) {
    let e = Entry {
        actor,
        action,
        target_type: target.0,
        target_id: target.1,
        project: None,
        details,
    };
    if let Err(err) = record(&st.db, e).await {
        tracing::error!("audit entry {action} lost: {err:#}");
        st.metrics.audit_errors.inc(&[]);
    }
}

/// An action of the system itself (runner offline, scheduled jobs).
pub async fn system(
    st: &AppState,
    action: &str,
    target_type: &str,
    target_id: &str,
    project: Option<Uuid>,
    details: Value,
) {
    let e = Entry {
        actor: None,
        action,
        target_type,
        target_id,
        project,
        details,
    };
    if let Err(err) = record(&st.db, e).await {
        tracing::error!("audit entry {action} lost: {err:#}");
        st.metrics.audit_errors.inc(&[]);
    }
}

/// Removes entries older than the retention (at least 365 days; the
/// database refuses younger ones). Oldest first, so the chain stays
/// verifiable from the first remaining entry.
pub async fn prune(db: &sqlx::PgPool, retention_days: i64) -> sqlx::Result<u64> {
    let days = retention_days.max(queued_api::security::AUDIT_MIN_RETENTION_DAYS);
    let r = sqlx::query(
        "DELETE FROM audit_log WHERE seq IN (
            SELECT seq FROM audit_log WHERE at < now() - make_interval(days => $1)
            ORDER BY seq LIMIT 10000)",
    )
    .bind(days as i32)
    .execute(db)
    .await?;
    Ok(r.rows_affected())
}

/// First broken link of the chain.
pub async fn verify(db: &sqlx::PgPool) -> sqlx::Result<queued_api::security::AuditVerify> {
    let (checked, first_broken): (i64, Option<i64>) =
        sqlx::query_as("SELECT checked, first_broken FROM audit_log_verify()")
            .fetch_one(db)
            .await?;
    Ok(queued_api::security::AuditVerify {
        checked,
        intact: first_broken.is_none(),
        first_broken,
    })
}
