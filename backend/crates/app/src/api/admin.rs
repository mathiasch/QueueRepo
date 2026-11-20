//! WP11: admin console – people (lock, role, quota), invitations,
//! registration mode and MFA policy, audit log, runner fleet. Cookie
//! sessions of admins only (no API token reaches `/api/admin/*`). Every
//! change is audited, in the same transaction where there is one.
use axum::{
    body::Body,
    extract::{FromRequestParts, Path, Query, State},
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use queued_api::security::{
    validate_capacity, validate_runner_name, validate_runner_url, AdminUser, AuditEntry, AuditPage,
    AuditVerify, CreatedInvitation, CreatedRunner, InstanceSettings, Invitation, InviteStatus,
    MoveProject, MoveResult, NewInvitation, NewRunner, Quota, RunnerProject, RunnerStatus,
    RunnerView, UpdateInstanceSettings, UpdateRunner, UpdateUser, AUDIT_PAGE,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    audit::{self, Entry},
    auth::AuthUser,
    crypto,
    error::{ApiResult, AppError},
    fleet, instance,
    models::User,
    state::AppState,
};

/// An admin's cookie session.
pub struct Admin(pub User);

impl FromRequestParts<AppState> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let AuthUser(u) = AuthUser::from_request_parts(parts, st).await?;
        if !u.is_admin || parts.extensions.get::<crate::auth::TokenAuth>().is_some() {
            return Err(AppError::forbidden());
        }
        Ok(Admin(u))
    }
}

fn entry<'a>(
    u: &'a User,
    action: &'a str,
    target: (&'a str, &'a str),
    details: Value,
) -> Entry<'a> {
    Entry {
        actor: Some((u.id, &u.email)),
        action,
        target_type: target.0,
        target_id: target.1,
        project: None,
        details,
    }
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct UserQuery {
    #[serde(default)]
    q: String,
}

type UserRow = (
    Uuid,
    String,
    String,
    bool,
    Option<DateTime<Utc>>,
    String,
    DateTime<Utc>,
    bool,
    Option<DateTime<Utc>>,
    i64,
    i64,
    i64,
    f64,
    Option<f64>,
    Option<i32>,
    Option<i32>,
);

const USER_SELECT: &str = "SELECT u.id, u.email, u.name, u.is_admin, u.disabled_at, u.disabled_reason, u.created_at,
        EXISTS (SELECT 1 FROM mfa_totp m WHERE m.user_id=u.id AND m.confirmed_at IS NOT NULL)
          OR EXISTS (SELECT 1 FROM webauthn_credentials w WHERE w.user_id=u.id),
        (SELECT max(last_seen_at) FROM sessions s WHERE s.user_id=u.id),
        (SELECT count(*) FROM sessions s WHERE s.user_id=u.id AND s.expires_at > now()),
        (SELECT count(*) FROM api_tokens t WHERE t.user_id=u.id AND t.revoked_at IS NULL AND t.expires_at > now()),
        (SELECT count(*) FROM projects p WHERE p.owner_id=u.id AND NOT p.is_eval),
        (SELECT coalesce(sum(cost_usd), 0)::float8 FROM usage_log l WHERE l.user_id=u.id AND l.ts > now() - interval '30 days'),
        q.max_cost_30d_usd, q.max_parallel, q.max_projects
     FROM users u LEFT JOIN user_quotas q ON q.user_id = u.id";

fn admin_user(r: UserRow) -> AdminUser {
    AdminUser {
        id: r.0,
        email: r.1,
        name: r.2,
        is_admin: r.3,
        disabled_at: r.4,
        disabled_reason: r.5,
        created_at: r.6,
        mfa_enabled: r.7,
        last_seen_at: r.8,
        sessions: r.9,
        active_tokens: r.10,
        projects: r.11,
        cost_30d_usd: r.12,
        quota: Quota {
            max_cost_30d_usd: r.13,
            max_parallel: r.14,
            max_projects: r.15,
        },
    }
}

pub async fn users(
    State(st): State<AppState>,
    Admin(_): Admin,
    Query(q): Query<UserQuery>,
) -> ApiResult<Json<Vec<AdminUser>>> {
    let pattern = format!("%{}%", q.q.trim().to_lowercase().replace(['%', '_'], ""));
    let rows: Vec<UserRow> = sqlx::query_as(&format!(
        "{USER_SELECT} WHERE lower(u.email) LIKE $1 OR lower(u.name) LIKE $1
         ORDER BY u.disabled_at IS NOT NULL, lower(u.email) LIMIT 500"
    ))
    .bind(pattern)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(rows.into_iter().map(admin_user).collect()))
}

async fn load_user(st: &AppState, id: Uuid) -> ApiResult<AdminUser> {
    let row: UserRow = sqlx::query_as(&format!("{USER_SELECT} WHERE u.id = $1"))
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(admin_user(row))
}

/// Lock/unlock, admin role. A lock deletes the person's sessions and login
/// flows and revokes their API tokens at once. The last active admin can
/// neither be locked nor demoted (checked under a lock of all active admin
/// rows, so two admins demoting each other at the same time cannot both win).
pub async fn update_user(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateUser>,
) -> ApiResult<Json<AdminUser>> {
    if b.disabled == Some(true) && id == me.id {
        return Err(AppError::conflict("Du kannst dich nicht selbst sperren."));
    }
    let reason: String = b
        .reason
        .unwrap_or_default()
        .trim()
        .chars()
        .take(500)
        .collect();
    let mut tx = st.db.begin().await?;
    let admins: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE is_admin AND disabled_at IS NULL ORDER BY id FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    let (email, is_admin, disabled): (String, bool, bool) = sqlx::query_as(
        "SELECT email, is_admin, disabled_at IS NOT NULL FROM users WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(AppError::not_found)?;
    let will_be_admin = b.is_admin.unwrap_or(is_admin);
    let will_be_disabled = b.disabled.unwrap_or(disabled);
    let active_admin_after = will_be_admin && !will_be_disabled;
    let remaining = admins.iter().filter(|a| **a != id).count() + usize::from(active_admin_after);
    if remaining == 0 {
        return Err(AppError::conflict(
            "Es muss immer mindestens eine aktive Admin-Person geben.",
        ));
    }
    if will_be_admin != is_admin {
        sqlx::query("UPDATE users SET is_admin=$2 WHERE id=$1")
            .bind(id)
            .bind(will_be_admin)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            entry(
                &me,
                "admin.role_changed",
                ("user", &id.to_string()),
                json!({"email": email, "is_admin": will_be_admin}),
            ),
        )
        .await?;
    }
    if will_be_disabled != disabled {
        if will_be_disabled {
            sqlx::query("UPDATE users SET disabled_at=now(), disabled_reason=$2 WHERE id=$1")
                .bind(id)
                .bind(&reason)
                .execute(&mut *tx)
                .await?;
            let sessions = sqlx::query("DELETE FROM sessions WHERE user_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            sqlx::query("DELETE FROM auth_flows WHERE user_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            let tokens = sqlx::query(
                "UPDATE api_tokens SET revoked_at=now() WHERE user_id=$1 AND revoked_at IS NULL",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            audit::record(
                &mut *tx,
                entry(
                    &me,
                    "admin.user_disabled",
                    ("user", &id.to_string()),
                    json!({"email": email, "reason": reason, "sessions_revoked": sessions, "tokens_revoked": tokens}),
                ),
            )
            .await?;
        } else {
            sqlx::query("UPDATE users SET disabled_at=NULL, disabled_reason='' WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            audit::record(
                &mut *tx,
                entry(
                    &me,
                    "admin.user_enabled",
                    ("user", &id.to_string()),
                    json!({"email": email}),
                ),
            )
            .await?;
        }
    }
    tx.commit().await?;
    if will_be_disabled != disabled {
        st.wake.notify_one();
    }
    Ok(Json(load_user(&st, id).await?))
}

pub async fn put_quota(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
    Json(q): Json<Quota>,
) -> ApiResult<Json<AdminUser>> {
    q.validate().map_err(AppError::bad_request)?;
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::not_found)?;
    let mut tx = st.db.begin().await?;
    if q.is_unlimited() {
        sqlx::query("DELETE FROM user_quotas WHERE user_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO user_quotas (user_id, max_cost_30d_usd, max_parallel, max_projects, updated_by, updated_at)
             VALUES ($1,$2,$3,$4,$5,now())
             ON CONFLICT (user_id) DO UPDATE SET max_cost_30d_usd=$2, max_parallel=$3, max_projects=$4,
                 updated_by=$5, updated_at=now()",
        )
        .bind(id)
        .bind(q.max_cost_30d_usd)
        .bind(q.max_parallel)
        .bind(q.max_projects)
        .bind(me.id)
        .execute(&mut *tx)
        .await?;
    }
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.quota_changed",
            ("user", &id.to_string()),
            json!({"email": email, "quota": q}),
        ),
    )
    .await?;
    tx.commit().await?;
    st.wake.notify_one();
    Ok(Json(load_user(&st, id).await?))
}

// ---------------------------------------------------------------------------
// Invitations, settings
// ---------------------------------------------------------------------------

type InviteRow = (
    Uuid,
    Option<String>,
    bool,
    String,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<String>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

const INVITE_SELECT: &str = "SELECT i.id, i.email, i.make_admin, i.note, i.created_at, i.expires_at,
        (SELECT email FROM users WHERE id=i.created_by), (SELECT email FROM users WHERE id=i.used_by),
        i.used_at, i.revoked_at
     FROM invitations i";

fn invitation(r: InviteRow) -> Invitation {
    let status = if r.9.is_some() {
        InviteStatus::Revoked
    } else if r.8.is_some() {
        InviteStatus::Used
    } else if r.5 <= Utc::now() {
        InviteStatus::Expired
    } else {
        InviteStatus::Open
    };
    Invitation {
        id: r.0,
        email: r.1,
        make_admin: r.2,
        note: r.3,
        created_at: r.4,
        expires_at: r.5,
        created_by: r.6,
        used_by: r.7,
        status,
    }
}

pub async fn invitations(
    State(st): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<Vec<Invitation>>> {
    let rows: Vec<InviteRow> = sqlx::query_as(&format!(
        "{INVITE_SELECT} ORDER BY i.created_at DESC LIMIT 200"
    ))
    .fetch_all(&st.db)
    .await?;
    Ok(Json(rows.into_iter().map(invitation).collect()))
}

pub async fn create_invitation(
    State(st): State<AppState>,
    Admin(me): Admin,
    Json(b): Json<NewInvitation>,
) -> ApiResult<Json<CreatedInvitation>> {
    b.validate().map_err(AppError::bad_request)?;
    let email = b
        .email
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty());
    let token = format!("qinv_{}", crypto::random_token());
    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "INSERT INTO invitations (id, token_hash, email, make_admin, note, created_by, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6, now() + make_interval(days => $7))",
    )
    .bind(id)
    .bind(crypto::sha256_hex(&token))
    .bind(&email)
    .bind(b.make_admin)
    .bind(b.note.trim())
    .bind(me.id)
    .bind(b.days)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.invite_created",
            ("invitation", &id.to_string()),
            json!({"email": email, "make_admin": b.make_admin, "days": b.days}),
        ),
    )
    .await?;
    tx.commit().await?;
    let row: InviteRow = sqlx::query_as(&format!("{INVITE_SELECT} WHERE i.id=$1"))
        .bind(id)
        .fetch_one(&st.db)
        .await?;
    Ok(Json(CreatedInvitation {
        invitation: invitation(row),
        link: format!("{}/?invite={token}", st.cfg.public_url),
    }))
}

pub async fn revoke_invitation(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let mut tx = st.db.begin().await?;
    let n = sqlx::query("UPDATE invitations SET revoked_at=now() WHERE id=$1 AND used_at IS NULL AND revoked_at IS NULL")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::conflict("Die Einladung ist nicht mehr offen."));
    }
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.invite_revoked",
            ("invitation", &id.to_string()),
            json!({}),
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn get_settings(
    State(st): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<InstanceSettings>> {
    Ok(Json(instance::settings(&st).await?))
}

pub async fn put_settings(
    State(st): State<AppState>,
    Admin(me): Admin,
    Json(b): Json<UpdateInstanceSettings>,
) -> ApiResult<Json<InstanceSettings>> {
    let before = instance::settings(&st).await?;
    let mut tx = st.db.begin().await?;
    if let Some(m) = b.registration_mode {
        sqlx::query(
            "UPDATE instance_settings SET registration_mode=$1, updated_at=now() WHERE id=1",
        )
        .bind(m.as_str())
        .execute(&mut *tx)
        .await?;
    }
    if let Some(p) = b.mfa_policy {
        sqlx::query("UPDATE instance_settings SET mfa_policy=$1, updated_at=now() WHERE id=1")
            .bind(p.as_str())
            .execute(&mut *tx)
            .await?;
    }
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.settings_changed",
            ("instance", "settings"),
            json!({
                "registration_mode": b.registration_mode.map(|m| m.as_str()),
                "mfa_policy": b.mfa_policy.map(|p| p.as_str()),
                "before": {"registration_mode": before.registration_mode.as_str(), "mfa_policy": before.mfa_policy.as_str()},
            }),
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(instance::settings(&st).await?))
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    /// Action prefix (`auth.`) or exact action.
    #[serde(default)]
    action: String,
    /// Part of the actor's e-mail.
    #[serde(default)]
    actor: String,
    #[serde(default)]
    project: Option<Uuid>,
    #[serde(default)]
    from: Option<DateTime<Utc>>,
    #[serde(default)]
    to: Option<DateTime<Utc>>,
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

type AuditRow = (
    i64,
    DateTime<Utc>,
    Option<Uuid>,
    String,
    String,
    String,
    String,
    Option<Uuid>,
    Value,
    Vec<u8>,
    Vec<u8>,
);

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

async fn audit_rows(st: &AppState, q: &AuditQuery, limit: i64) -> ApiResult<Vec<AuditEntry>> {
    let action = q.action.trim();
    if action.len() > 80
        || !action
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_')
    {
        return Err(AppError::bad_request("invalid action filter"));
    }
    let actor = format!(
        "%{}%",
        q.actor.trim().to_lowercase().replace(['%', '_'], "")
    );
    let rows: Vec<AuditRow> = sqlx::query_as(
        "SELECT seq, at, actor_id, actor_label, action, target_type, target_id, project_id, details, hash, prev_hash
         FROM audit_log
         WHERE ($1 = '' OR action = $1 OR (right($1, 1) = '.' AND starts_with(action, $1)))
           AND ($2 = '%%' OR lower(actor_label) LIKE $2)
           AND ($3::uuid IS NULL OR project_id = $3)
           AND ($4::timestamptz IS NULL OR at >= $4)
           AND ($5::timestamptz IS NULL OR at < $5)
           AND ($6::bigint IS NULL OR seq < $6)
         ORDER BY seq DESC LIMIT $7",
    )
    .bind(action)
    .bind(actor)
    .bind(q.project)
    .bind(q.from)
    .bind(q.to)
    .bind(q.before)
    .bind(limit)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| AuditEntry {
            seq: r.0,
            at: r.1,
            actor_id: r.2,
            actor_label: r.3,
            action: r.4,
            target_type: r.5,
            target_id: r.6,
            project_id: r.7,
            details: r.8,
            hash: hex(&r.9),
            prev_hash: hex(&r.10),
        })
        .collect())
}

pub async fn audit_log(
    State(st): State<AppState>,
    Admin(_): Admin,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<AuditPage>> {
    let limit = q.limit.unwrap_or(AUDIT_PAGE).clamp(1, 200);
    let entries = audit_rows(&st, &q, limit + 1).await?;
    let more = entries.len() as i64 > limit;
    let entries: Vec<AuditEntry> = entries.into_iter().take(limit as usize).collect();
    let next_before = if more {
        entries.last().map(|e| e.seq)
    } else {
        None
    };
    Ok(Json(AuditPage {
        entries,
        next_before,
    }))
}

/// JSON Lines with hashes (for external verification and archiving).
pub async fn audit_export(
    State(st): State<AppState>,
    Admin(me): Admin,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Response> {
    let entries = audit_rows(&st, &q, 100_000).await?;
    let mut out = String::new();
    for e in entries.iter().rev() {
        out.push_str(&serde_json::to_string(e).map_err(anyhow::Error::from)?);
        out.push('\n');
    }
    audit::log(
        &st,
        &me,
        "admin.audit_exported",
        ("audit", ""),
        None,
        json!({"entries": entries.len()}),
    )
    .await;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"audit-log.jsonl\"",
            ),
        ],
        Body::from(out),
    )
        .into_response())
}

pub async fn audit_verify(
    State(st): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<AuditVerify>> {
    Ok(Json(audit::verify(&st.db).await?))
}

// ---------------------------------------------------------------------------
// Runner fleet
// ---------------------------------------------------------------------------

pub async fn runners(
    State(st): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<Vec<RunnerView>>> {
    Ok(Json(fleet::list(&st).await?))
}

fn runner_conflict(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            AppError::conflict("Ein Runner mit diesem Namen existiert schon.")
        }
        _ => e.into(),
    }
}

pub async fn create_runner(
    State(st): State<AppState>,
    Admin(me): Admin,
    Json(b): Json<NewRunner>,
) -> ApiResult<Json<CreatedRunner>> {
    b.validate().map_err(AppError::bad_request)?;
    let token = fleet::new_token();
    let enc = crypto::encrypt(&st.cfg.master_key, token.as_bytes())?;
    let id = Uuid::new_v4();
    let url = b.url.trim().trim_end_matches('/').to_string();
    let mut tx = st.db.begin().await?;
    sqlx::query("INSERT INTO runners (id, name, url, token_enc, capacity) VALUES ($1,$2,$3,$4,$5)")
        .bind(id)
        .bind(b.name.trim())
        .bind(&url)
        .bind(enc)
        .bind(b.capacity)
        .execute(&mut *tx)
        .await
        .map_err(runner_conflict)?;
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.runner_created",
            ("runner", &id.to_string()),
            json!({"name": b.name.trim(), "url": url, "capacity": b.capacity}),
        ),
    )
    .await?;
    tx.commit().await?;
    fleet::refresh(&st).await?;
    let runner = fleet::get(&st, id).await?.ok_or_else(AppError::not_found)?;
    Ok(Json(CreatedRunner { runner, token }))
}

pub async fn update_runner(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateRunner>,
) -> ApiResult<Json<RunnerView>> {
    let cur = fleet::get(&st, id).await?.ok_or_else(AppError::not_found)?;
    if cur.status == RunnerStatus::Revoked {
        return Err(AppError::conflict(
            "Ein widerrufener Runner kann nicht mehr geändert werden.",
        ));
    }
    if let Some(n) = &b.name {
        validate_runner_name(n).map_err(AppError::bad_request)?;
    }
    if let Some(u) = &b.url {
        if cur.builtin {
            return Err(AppError::bad_request(
                "Die Adresse des Standard-Runners kommt aus RUNNER_URL.",
            ));
        }
        validate_runner_url(u).map_err(AppError::bad_request)?;
        if cur.running_tasks > 0 {
            return Err(AppError::conflict(
                "Die Adresse lässt sich nur ändern, wenn nichts darauf läuft.",
            ));
        }
    }
    if let Some(c) = b.capacity {
        validate_capacity(c).map_err(AppError::bad_request)?;
    }
    if b.status == Some(RunnerStatus::Revoked) {
        return Err(AppError::bad_request(
            "Zum Widerrufen gibt es eine eigene Aktion.",
        ));
    }
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "UPDATE runners SET name=coalesce($2, name), url=coalesce($3, url),
             capacity=CASE WHEN $4 THEN $5 ELSE capacity END, status=coalesce($6, status)
         WHERE id=$1",
    )
    .bind(id)
    .bind(b.name.as_ref().map(|n| n.trim().to_string()))
    .bind(
        b.url
            .as_ref()
            .map(|u| u.trim().trim_end_matches('/').to_string()),
    )
    .bind(b.capacity.is_some())
    .bind(b.capacity.flatten())
    .bind(b.status.map(|s| s.as_str()))
    .execute(&mut *tx)
    .await
    .map_err(runner_conflict)?;
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.runner_updated",
            ("runner", &id.to_string()),
            json!({"name": b.name, "url": b.url, "capacity": b.capacity, "status": b.status.map(|s| s.as_str())}),
        ),
    )
    .await?;
    tx.commit().await?;
    fleet::refresh(&st).await?;
    st.wake.notify_one();
    Ok(Json(
        fleet::get(&st, id).await?.ok_or_else(AppError::not_found)?,
    ))
}

/// A new token (the old one stops working for the app at once).
pub async fn rotate_runner_token(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CreatedRunner>> {
    let token = fleet::new_token();
    let enc = crypto::encrypt(&st.cfg.master_key, token.as_bytes())?;
    let mut tx = st.db.begin().await?;
    let n = sqlx::query(
        "UPDATE runners SET token_enc=$2 WHERE id=$1 AND NOT builtin AND status <> 'revoked'",
    )
    .bind(id)
    .bind(enc)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::conflict(
            "Nur für angemeldete, nicht widerrufene Runner (der Standard-Runner nutzt RUNNER_TOKEN).",
        ));
    }
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.runner_token_rotated",
            ("runner", &id.to_string()),
            json!({}),
        ),
    )
    .await?;
    tx.commit().await?;
    fleet::refresh(&st).await?;
    let runner = fleet::get(&st, id).await?.ok_or_else(AppError::not_found)?;
    Ok(Json(CreatedRunner { runner, token }))
}

/// Revokes a runner: its token is deleted, it never gets work or
/// credentials again, its open runs are abandoned (the runner stops them
/// when their lease expires). Its projects wait until they are restored
/// onto another runner from a backup.
pub async fn revoke_runner(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RunnerView>> {
    let mut tx = st.db.begin().await?;
    let n = sqlx::query(
        "UPDATE runners SET status='revoked', token_enc=NULL WHERE id=$1 AND NOT builtin AND status <> 'revoked'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::conflict(
            "Dieser Runner kann nicht widerrufen werden.",
        ));
    }
    let projects: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE runner_id=$1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.runner_revoked",
            ("runner", &id.to_string()),
            json!({"projects": projects}),
        ),
    )
    .await?;
    tx.commit().await?;
    fleet::refresh(&st).await?;
    Ok(Json(
        fleet::get(&st, id).await?.ok_or_else(AppError::not_found)?,
    ))
}

pub async fn delete_runner(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let mut tx = st.db.begin().await?;
    let busy: Option<(bool, i64)> = sqlx::query_as(
        "SELECT builtin, (SELECT count(*) FROM projects p WHERE p.runner_id = r.id) FROM runners r WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    match busy {
        None => return Err(AppError::not_found()),
        Some((true, _)) => {
            return Err(AppError::conflict(
                "Der Standard-Runner kann nicht gelöscht werden.",
            ))
        }
        Some((_, n)) if n > 0 => {
            return Err(AppError::conflict(
                "Auf diesem Runner liegen noch Projekte. Zieh sie zuerst um.",
            ))
        }
        _ => {}
    }
    sqlx::query("DELETE FROM runners WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        entry(
            &me,
            "admin.runner_deleted",
            ("runner", &id.to_string()),
            json!({}),
        ),
    )
    .await?;
    tx.commit().await?;
    fleet::refresh(&st).await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn projects(
    State(st): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<Vec<RunnerProject>>> {
    let rows: Vec<(Uuid, String, String, Uuid, bool)> = sqlx::query_as(
        "SELECT p.id, p.name, u.email, p.runner_id,
                EXISTS (SELECT 1 FROM tasks t WHERE t.project_id=p.id AND t.status IN ('running','merging','waiting_batch'))
         FROM projects p JOIN users u ON u.id = p.owner_id
         WHERE NOT p.is_eval ORDER BY lower(p.name)",
    )
    .fetch_all(&st.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, owner, runner_id, busy)| RunnerProject {
                id,
                name,
                owner,
                runner_id,
                busy,
            })
            .collect(),
    ))
}

pub async fn move_project(
    State(st): State<AppState>,
    Admin(me): Admin,
    Path(id): Path<Uuid>,
    Json(b): Json<MoveProject>,
) -> ApiResult<Json<MoveResult>> {
    let from = st.runner_id(id).await.map_err(|_| AppError::not_found())?;
    let (workspaces, bytes) = fleet::move_project(&st, id, b.runner_id)
        .await
        .map_err(|e| AppError(StatusCode::CONFLICT, format!("{e:#}")))?;
    audit::record(
        &st.db,
        Entry {
            actor: Some((me.id, &me.email)),
            action: "project.moved",
            target_type: "project",
            target_id: &id.to_string(),
            project: Some(id),
            details: json!({"from": from, "to": b.runner_id, "workspaces": workspaces, "bytes": bytes}),
        },
    )
    .await?;
    Ok(Json(MoveResult { workspaces, bytes }))
}
