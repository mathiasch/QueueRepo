//! WP08 endpoints: `POST /api/intake/{id}` (public, the signature is the
//! authentication) and the management of integrations, identities and
//! their history (cookie only – not in the token table, WP07 default deny).
//!
//! Rights: reading ≥ viewer (never a secret or token); connecting,
//! changing, deleting only the project owner; identities: the owner maps
//! members (approved by the owner, waiting for the person), a member ≥
//! editor maps themselves (confirmed by the person, waiting for the owner);
//! confirming: the person (their access) or the owner (who may act).
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use queued_api::{
    intake::{
        self as wire, CreatedIntegration, Identities, IdentityState, IdentityView, IntakeEventView,
        IntakeKind, IntakeOutcome, IntegrationView, MemberRef, NewIdentity, NewIntegration,
        UpdateIntegration, MAX_IDENTITIES, MAX_INTEGRATIONS,
    },
    tokens::SECRET_PREFIX,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{require, role, Role};
use crate::{
    auth::AuthUser,
    crypto,
    error::{ApiResult, AppError},
    intake::{self, Accepted, IntegrationRow, Parsed},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Inbound
// ---------------------------------------------------------------------------

pub async fn receive(
    State(st): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match receive_inner(&st, &id, &headers, &body).await {
        Ok(r) => r,
        Err(e) => AppError::from(e).into_response(),
    }
}

fn plain(code: StatusCode, msg: &str) -> Response {
    AppError(code, msg.into()).into_response()
}

async fn receive_inner(
    st: &AppState,
    id: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> anyhow::Result<Response> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(AppError::not_found().into_response());
    };
    let Some(row) = sqlx::query_as::<_, IntegrationRow>("SELECT * FROM integrations WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
    else {
        return Ok(AppError::not_found().into_response());
    };
    let kind = row.kind();
    let secret = crypto::decrypt(&st.cfg.master_key, &row.secret_enc)?;
    // Nothing is stored for a delivery with a bad signature.
    if let Err(why) = intake::verify(kind, headers, body, &secret, chrono::Utc::now()) {
        return Ok(plain(StatusCode::UNAUTHORIZED, &why));
    }
    let inc = match intake::parse(kind, headers, body, &row.config(), &row.tenant) {
        Ok(i) => i,
        Err(why) => return Ok(plain(StatusCode::BAD_REQUEST, &why)),
    };
    // Strict tenant separation: one app may be installed in several
    // workspaces and signs them all with the same secret.
    let needs_tenant = matches!(inc.parsed, Parsed::Request(_) | Parsed::Reply(_));
    match &inc.tenant {
        Some(t) if !t.eq_ignore_ascii_case(&row.tenant) => {
            intake::record_refused(st, &row, &inc, "Fremder Arbeitsbereich – nicht angenommen")
                .await;
            return Ok(plain(StatusCode::FORBIDDEN, "Fremder Arbeitsbereich"));
        }
        None if needs_tenant => {
            intake::record_refused(st, &row, &inc, "Ereignis ohne Arbeitsbereich").await;
            return Ok(plain(StatusCode::FORBIDDEN, "Ereignis ohne Arbeitsbereich"));
        }
        _ => {}
    }
    if let Parsed::Challenge(c) = &inc.parsed {
        return Ok(Json(c.clone()).into_response());
    }
    let slack = kind == IntakeKind::Slack;
    let slash = inc.slash;
    let Some(a) = intake::accept(st, id, inc).await? else {
        // Slack retries anything but 200 and switches events off after
        // repeated failures: an integration switched off in Queue says so.
        return Ok(if slash {
            Json(json!({"response_type": "ephemeral", "text": "Diese Anbindung ist in Queue ausgeschaltet."})).into_response()
        } else if slack {
            (StatusCode::OK, Json(json!({"outcome": "disabled"}))).into_response()
        } else {
            AppError::conflict("Anbindung ist ausgeschaltet").into_response()
        });
    };
    if slash {
        let text = slash_text(&a, row.token_enc.is_some());
        return Ok(Json(json!({"response_type": "ephemeral", "text": text})).into_response());
    }
    let code = match &a {
        Accepted::Done {
            outcome: IntakeOutcome::Created | IntakeOutcome::Continued,
            ..
        } => StatusCode::CREATED,
        Accepted::Done {
            outcome: IntakeOutcome::Rejected,
            reason,
            ..
        } if !slack && reason.as_deref().is_some_and(|r| r.starts_with("Limit")) => {
            StatusCode::TOO_MANY_REQUESTS
        }
        _ => StatusCode::OK,
    };
    Ok((code, Json(intake::answer(&a))).into_response())
}

fn slash_text(a: &Accepted, updates: bool) -> String {
    match a {
        Accepted::Done {
            outcome: IntakeOutcome::Created,
            ..
        } if updates => "Auftrag angelegt – Queue antwortet gleich im Kanal.".into(),
        Accepted::Done {
            outcome: IntakeOutcome::Created,
            ..
        } => "Auftrag angelegt – den Stand siehst du in Queue (Rückmeldungen nach Slack sind aus)."
            .into(),
        other => intake::outcome_text(other),
    }
}

// ---------------------------------------------------------------------------
// Management
// ---------------------------------------------------------------------------

async fn load(st: &AppState, id: Uuid) -> ApiResult<IntegrationRow> {
    sqlx::query_as("SELECT * FROM integrations WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::not_found)
}

/// The integration and the caller's role (strangers 404).
async fn visible(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<(IntegrationRow, Role)> {
    let row = load(st, id).await?;
    match role(st, row.project_id, user).await? {
        Some(r) => Ok((row, r)),
        None => Err(AppError::not_found()),
    }
}

async fn manageable(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<IntegrationRow> {
    let (row, r) = visible(st, id, user).await?;
    if r != Role::Owner {
        return Err(AppError::forbidden());
    }
    Ok(row)
}

fn event_view(r: EventRow) -> IntakeEventView {
    IntakeEventView {
        id: r.0,
        outcome: IntakeOutcome::parse(&r.1).unwrap_or(IntakeOutcome::Ignored),
        reason: r.2,
        event: r.3,
        external_user: r.4,
        task_id: r.5,
        created_at: r.6,
    }
}

type EventRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Uuid>,
    chrono::DateTime<chrono::Utc>,
);

const EVENT_COLS: &str = "id, outcome, reason, event, external_user, task_id, created_at";

async fn view(st: &AppState, row: &IntegrationRow, r: Role) -> ApiResult<IntegrationView> {
    let owner_name = crate::steer::user_name(st, Some(row.owner_id)).await;
    let (active, waiting): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE owner_ok AND user_ok), count(*) FILTER (WHERE NOT (owner_ok AND user_ok))
           FROM intake_identities WHERE integration_id=$1",
    )
    .bind(row.id)
    .fetch_one(&st.db)
    .await?;
    let failed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM intake_outbox WHERE integration_id=$1 AND state='failed'",
    )
    .bind(row.id)
    .fetch_one(&st.db)
    .await?;
    let last: Option<EventRow> = sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM intake_events WHERE integration_id=$1 ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(row.id)
    .fetch_optional(&st.db)
    .await?;
    Ok(IntegrationView {
        id: row.id,
        project_id: row.project_id,
        kind: row.kind(),
        name: row.name.clone(),
        tenant: row.tenant.clone(),
        enabled: row.enabled,
        owner_id: row.owner_id,
        owner_name,
        config: row.config(),
        max_per_hour: row.max_per_hour,
        has_token: row.token_enc.is_some(),
        account: row.account.clone(),
        hook_url: intake::hook_url(&st.cfg.public_url, row.id),
        disabled_reason: row.disabled_reason.clone(),
        active_people: active,
        waiting_people: waiting,
        failed_updates: failed,
        last_event: last.map(event_view),
        can_manage: r == Role::Owner,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// `GET /api/projects/{id}/integrations`.
pub async fn list(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(pid): Path<Uuid>,
) -> ApiResult<Json<Vec<IntegrationView>>> {
    require(&st, pid, u.id, Role::Viewer).await?;
    let r = role(&st, pid, u.id).await?.unwrap_or(Role::Viewer);
    let rows: Vec<IntegrationRow> =
        sqlx::query_as("SELECT * FROM integrations WHERE project_id=$1 ORDER BY created_at")
            .bind(pid)
            .fetch_all(&st.db)
            .await?;
    let mut out = vec![];
    for row in &rows {
        out.push(view(&st, row, r).await?);
    }
    Ok(Json(out))
}

fn bad(e: String) -> AppError {
    AppError::bad_request(e)
}

/// A provider secret or token: 8–4000 characters, no whitespace.
fn credential(what: &str, s: &str) -> ApiResult<String> {
    let s = s.trim();
    if s.len() < 8 || s.len() > 4000 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AppError::bad_request(format!(
            "{what}: 8–4000 Zeichen ohne Leerzeichen"
        )));
    }
    Ok(s.to_string())
}

fn account(s: &str) -> ApiResult<String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 200 || !s.contains('@') || s.chars().any(char::is_whitespace) {
        return Err(AppError::bad_request(
            "Bitte die E-Mail-Adresse des Jira-Kontos angeben, zu dem das Token gehört",
        ));
    }
    Ok(s.to_string())
}

fn limit(n: Option<i32>) -> ApiResult<i32> {
    match n.unwrap_or(10) {
        n @ 1..=100 => Ok(n),
        _ => Err(AppError::bad_request("Höchstens 1–100 Aufträge pro Stunde")),
    }
}

fn tenant(st: &AppState, kind: IntakeKind, raw: &str) -> ApiResult<String> {
    let t = wire::validate_tenant(kind, raw).map_err(bad)?;
    // SSRF: Jira calls go to the site – only Atlassian Cloud, unless private
    // hosts are allowed (tests, LAN proxies).
    if kind == IntakeKind::Jira && !wire::is_atlassian_site(&t) && !st.cfg.allow_private_git_hosts {
        return Err(AppError::bad_request(
            "Nur Jira Cloud (https://…atlassian.net) wird unterstützt",
        ));
    }
    Ok(t)
}

fn enc(st: &AppState, s: &str) -> ApiResult<Vec<u8>> {
    Ok(crypto::encrypt(&st.cfg.master_key, s.as_bytes())?)
}

fn unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("23505"))
}

const TAKEN: &str = "Dieser Arbeitsbereich ist schon mit einem Queue-Projekt verbunden (ein Arbeitsbereich gehört zu genau einem Projekt)";

/// `POST /api/projects/{id}/integrations` (project owner).
pub async fn create(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(pid): Path<Uuid>,
    Json(n): Json<NewIntegration>,
) -> ApiResult<Json<CreatedIntegration>> {
    require(&st, pid, u.id, Role::Owner).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM integrations WHERE project_id=$1")
        .bind(pid)
        .fetch_one(&st.db)
        .await?;
    if count >= MAX_INTEGRATIONS {
        return Err(AppError::conflict(format!(
            "Höchstens {MAX_INTEGRATIONS} Anbindungen pro Projekt"
        )));
    }
    let kind = n.kind;
    let name = wire::validate_name(&n.name).map_err(bad)?;
    let tenant = tenant(&st, kind, &n.tenant)?;
    let config = wire::validate_config(kind, &n.config).map_err(bad)?;
    let max = limit(n.max_per_hour)?;
    let (shown, secret) = if kind.queue_makes_secret() {
        let s = super::tokens::generate(SECRET_PREFIX);
        (Some(s.clone()), s)
    } else {
        let s = n.secret.as_deref().unwrap_or("");
        (None, credential(kind.secret_label(), s)?)
    };
    let token = match n.token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => Some(enc(&st, &credential(kind.token_label(), t)?)?),
        None => None,
    };
    let acct = match n
        .account
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(a) if kind.needs_account() => Some(account(a)?),
        _ => None,
    };
    if kind.needs_account() && token.is_some() && acct.is_none() {
        return Err(AppError::bad_request(
            "Für Jira gehört zum Token die E-Mail-Adresse des Kontos",
        ));
    }
    let id = Uuid::new_v4();
    let res = sqlx::query(
        "INSERT INTO integrations (id, project_id, owner_id, kind, name, tenant, secret_enc, token_enc, account, config, max_per_hour)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(id)
    .bind(pid)
    .bind(u.id)
    .bind(kind.as_str())
    .bind(&name)
    .bind(&tenant)
    .bind(enc(&st, &secret)?)
    .bind(token)
    .bind(acct)
    .bind(serde_json::to_value(&config).unwrap_or(Value::Null))
    .bind(max)
    .execute(&st.db)
    .await;
    match res {
        Err(e) if unique_violation(&e) => return Err(AppError::conflict(TAKEN)),
        r => {
            r?;
        }
    }
    let row = load(&st, id).await?;
    let view = view(&st, &row, Role::Owner).await?;
    let steps = wire::setup_steps(kind, &view.hook_url, &config);
    Ok(Json(CreatedIntegration {
        integration: view,
        secret: shown,
        steps,
    }))
}

/// `GET /api/integrations/{id}`.
pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<IntegrationView>> {
    let (row, r) = visible(&st, id, u.id).await?;
    Ok(Json(view(&st, &row, r).await?))
}

/// `PATCH /api/integrations/{id}` (project owner).
pub async fn update(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(p): Json<UpdateIntegration>,
) -> ApiResult<Json<IntegrationView>> {
    let row = manageable(&st, id, u.id).await?;
    let kind = row.kind();
    let name = match &p.name {
        Some(n) => wire::validate_name(n).map_err(bad)?,
        None => row.name.clone(),
    };
    let config = match &p.config {
        Some(c) => wire::validate_config(kind, c).map_err(bad)?,
        None => row.config(),
    };
    let max = match p.max_per_hour {
        Some(m) => limit(Some(m))?,
        None => row.max_per_hour,
    };
    let token_enc = match p.token.as_deref().map(str::trim) {
        Some("") => None,
        Some(t) => Some(enc(&st, &credential(kind.token_label(), t)?)?),
        None => row.token_enc.clone(),
    };
    let acct = match p.account.as_deref().map(str::trim) {
        Some("") => None,
        Some(a) if kind.needs_account() => Some(account(a)?),
        Some(_) => None,
        None => row.account.clone(),
    };
    if kind.needs_account() && token_enc.is_some() && acct.is_none() {
        return Err(AppError::bad_request(
            "Für Jira gehört zum Token die E-Mail-Adresse des Kontos",
        ));
    }
    let secret_enc = match p.secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(_) if kind.queue_makes_secret() => {
            return Err(AppError::bad_request(
                "Das Secret erzeugt Queue – nutze „Neues Secret“",
            ))
        }
        Some(s) => enc(&st, &credential(kind.secret_label(), s)?)?,
        None => row.secret_enc.clone(),
    };
    let enabled = p.enabled.unwrap_or(row.enabled);
    sqlx::query(
        "UPDATE integrations SET name=$2, config=$3, max_per_hour=$4, token_enc=$5, account=$6, secret_enc=$7,
                enabled=$8, disabled_reason = CASE WHEN $8 THEN NULL ELSE disabled_reason END, updated_at=now()
          WHERE id=$1",
    )
    .bind(id)
    .bind(&name)
    .bind(serde_json::to_value(&config).unwrap_or(Value::Null))
    .bind(max)
    .bind(token_enc)
    .bind(acct)
    .bind(secret_enc)
    .bind(enabled)
    .execute(&st.db)
    .await?;
    let row = load(&st, id).await?;
    Ok(Json(view(&st, &row, Role::Owner).await?))
}

/// `POST /api/integrations/{id}/secret`: a new Queue-made secret (GitHub,
/// Jira), shown once.
pub async fn renew_secret(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CreatedIntegration>> {
    let row = manageable(&st, id, u.id).await?;
    let kind = row.kind();
    if !kind.queue_makes_secret() {
        return Err(AppError::bad_request(format!(
            "Das Secret kommt von {} – trag das neue unter „Bearbeiten“ ein",
            kind.short()
        )));
    }
    let s = super::tokens::generate(SECRET_PREFIX);
    sqlx::query("UPDATE integrations SET secret_enc=$2, updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(enc(&st, &s)?)
        .execute(&st.db)
        .await?;
    let row = load(&st, id).await?;
    let view = view(&st, &row, Role::Owner).await?;
    let steps = wire::setup_steps(kind, &view.hook_url, &row.config());
    Ok(Json(CreatedIntegration {
        integration: view,
        secret: Some(s),
        steps,
    }))
}

/// `DELETE /api/integrations/{id}` (project owner). Tasks stay, their
/// links and pending updates go.
pub async fn remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    manageable(&st, id, u.id).await?;
    sqlx::query("DELETE FROM integrations WHERE id=$1")
        .bind(id)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `GET /api/integrations/{id}/events` (newest 100).
pub async fn events(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<IntakeEventView>>> {
    visible(&st, id, u.id).await?;
    let rows: Vec<EventRow> = sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM intake_events WHERE integration_id=$1 ORDER BY created_at DESC LIMIT 100"
    ))
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(rows.into_iter().map(event_view).collect()))
}

// ---------------------------------------------------------------------------
// Identities
// ---------------------------------------------------------------------------

type IdentityRow = (
    Uuid,
    String,
    String,
    Uuid,
    bool,
    bool,
    chrono::DateTime<chrono::Utc>,
);

async fn identity_views(
    st: &AppState,
    row: &IntegrationRow,
    user: Uuid,
    r: Role,
) -> ApiResult<Vec<IdentityView>> {
    let rows: Vec<IdentityRow> = sqlx::query_as(
        "SELECT id, external_id, external_name, user_id, owner_ok, user_ok, created_at
           FROM intake_identities WHERE integration_id=$1 ORDER BY created_at",
    )
    .bind(row.id)
    .fetch_all(&st.db)
    .await?;
    let mut out = vec![];
    for (id, xid, xname, uid, owner_ok, user_ok, at) in rows {
        let state = IdentityState::of(owner_ok, user_ok);
        let owner = r == Role::Owner;
        out.push(IdentityView {
            id,
            external_id: xid,
            external_name: xname,
            user_id: uid,
            user_name: crate::steer::user_name(st, Some(uid)).await,
            state,
            can_confirm: (state == IdentityState::WaitingUser && uid == user)
                || (state == IdentityState::WaitingOwner && owner),
            can_remove: owner || uid == user,
            created_at: at,
        });
    }
    Ok(out)
}

/// `GET /api/integrations/{id}/identities`.
pub async fn identities(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Identities>> {
    let (row, r) = visible(&st, id, u.id).await?;
    let members: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT u.id, COALESCE(NULLIF(u.name, ''), u.email), m.role FROM project_members m JOIN users u ON u.id=m.user_id
          WHERE m.project_id=$1 AND m.role IN ('owner','editor') ORDER BY 2",
    )
    .bind(row.project_id)
    .fetch_all(&st.db)
    .await?;
    let mappable = members
        .into_iter()
        .filter(|(uid, _, _)| r == Role::Owner || (r == Role::Editor && *uid == u.id))
        .map(|(user_id, name, _)| MemberRef { user_id, name })
        .collect();
    Ok(Json(Identities {
        identities: identity_views(&st, &row, u.id, r).await?,
        mappable,
    }))
}

/// `POST /api/integrations/{id}/identities`.
pub async fn add_identity(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(n): Json<NewIdentity>,
) -> ApiResult<Json<Vec<IdentityView>>> {
    let (row, r) = visible(&st, id, u.id).await?;
    let owner = r == Role::Owner;
    if !owner && !(r == Role::Editor && n.user_id == u.id) {
        return Err(AppError::forbidden());
    }
    // The person must be allowed to create tasks in the project.
    if !matches!(role(&st, row.project_id, n.user_id).await?, Some(x) if x >= Role::Editor) {
        return Err(AppError::bad_request(
            "Zuordnen lassen sich nur Personen, die im Projekt Aufträge anlegen dürfen",
        ));
    }
    let xid = wire::validate_external_id(row.kind(), &n.external_id).map_err(bad)?;
    let xname: String = n.external_name.trim().chars().take(200).collect();
    if xname.chars().any(char::is_control) {
        return Err(AppError::bad_request("Ungültiger Name"));
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM intake_identities WHERE integration_id=$1")
            .bind(id)
            .fetch_one(&st.db)
            .await?;
    if count >= MAX_IDENTITIES {
        return Err(AppError::conflict(format!(
            "Höchstens {MAX_IDENTITIES} Zuordnungen pro Anbindung"
        )));
    }
    let res = sqlx::query(
        "INSERT INTO intake_identities (id, integration_id, external_id, external_name, user_id, owner_ok, user_ok, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(&xid)
    .bind(&xname)
    .bind(n.user_id)
    .bind(owner)
    .bind(n.user_id == u.id)
    .bind(u.id)
    .execute(&st.db)
    .await;
    match res {
        Err(e) if unique_violation(&e) => {
            return Err(AppError::conflict(
                "Dieses Konto ist schon einer Person zugeordnet",
            ))
        }
        r => {
            r?;
        }
    }
    if n.user_id != u.id {
        crate::notify::notify(
            &st,
            n.user_id,
            crate::notify::Note {
                kind: "intake",
                title: format!("Zuordnung in „{}“ bestätigen", row.name),
                body: format!(
                    "Ein {}-Konto soll in deinem Namen Aufträge anlegen dürfen. Bestätige es im Projekt unter „Anbindungen“, sonst passiert nichts.",
                    row.kind().short()
                ),
                task_id: None,
                push: false,
            },
        )
        .await;
    }
    Ok(Json(identity_views(&st, &row, u.id, r).await?))
}

/// `POST /api/integrations/{id}/identities/{iid}/confirm`.
pub async fn confirm_identity(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, iid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Vec<IdentityView>>> {
    let (row, r) = visible(&st, id, u.id).await?;
    let n = if r == Role::Owner {
        // The owner approves; for themselves both at once.
        sqlx::query(
            "UPDATE intake_identities SET owner_ok=TRUE, user_ok = user_ok OR user_id=$3
              WHERE id=$1 AND integration_id=$2",
        )
        .bind(iid)
        .bind(id)
        .bind(u.id)
        .execute(&st.db)
        .await?
        .rows_affected()
    } else {
        sqlx::query(
            "UPDATE intake_identities SET user_ok=TRUE WHERE id=$1 AND integration_id=$2 AND user_id=$3",
        )
        .bind(iid)
        .bind(id)
        .bind(u.id)
        .execute(&st.db)
        .await?
        .rows_affected()
    };
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(Json(identity_views(&st, &row, u.id, r).await?))
}

/// `DELETE /api/integrations/{id}/identities/{iid}` (owner or the person).
pub async fn remove_identity(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path((id, iid)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Vec<IdentityView>>> {
    let (row, r) = visible(&st, id, u.id).await?;
    let n = sqlx::query(
        "DELETE FROM intake_identities WHERE id=$1 AND integration_id=$2 AND ($3 OR user_id=$4)",
    )
    .bind(iid)
    .bind(id)
    .bind(r == Role::Owner)
    .bind(u.id)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(Json(identity_views(&st, &row, u.id, r).await?))
}
