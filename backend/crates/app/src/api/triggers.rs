//! WP07: task templates and triggers of a project, their history and
//! `POST /api/triggers/{id}/fire`.
//!
//! Rights: reading ≥ viewer (never a secret), templates ≥ editor; a trigger
//! belongs to the person who created it (their access is used): only they
//! change, fire it or renew its secret; switching off and deleting is also
//! open to the project owner. Strangers get 404.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use queued_api::{
    tokens::SECRET_PREFIX,
    triggers::{
        self as tr, CreatedTrigger, CronPreview, CronPreviewRequest, FireRequest, FireResult,
        NewTemplate, NewTrigger, RunOutcome, RunSource, TemplateView, TriggerKind, TriggerRunView,
        TriggerView, UpdateTemplate, UpdateTrigger, MAX_FIRE_TEXT_CHARS, MAX_PER_HOUR,
        MAX_TEMPLATES, MAX_TRIGGERS,
    },
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    limits::CheckLimits,
    projects::{require, role, Role},
};
use crate::{
    auth::{AuthUser, TokenAuth},
    crypto,
    error::{ApiResult, AppError},
    state::AppState,
    triggers::{self, next_slot, parse_tz, schedule_of, Fired, Firing, TemplateRow, TriggerRow},
};

fn bad(e: String) -> AppError {
    AppError::bad_request(e)
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

async fn template_view(st: &AppState, t: TemplateRow) -> ApiResult<TemplateView> {
    let (used_by, by): (i64, Option<String>) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM triggers WHERE template_id=$1),
                (SELECT name FROM users WHERE id=$2)",
    )
    .bind(t.id)
    .bind(t.created_by)
    .fetch_one(&st.db)
    .await?;
    Ok(TemplateView {
        id: t.id,
        project_id: t.project_id,
        name: t.name,
        title: t.title,
        prompt: t.prompt,
        provider: t.provider,
        model: t.model,
        plan_mode: t.plan_mode,
        max_runtime_secs: t.max_runtime_secs,
        max_cost_usd: t.max_cost_usd,
        max_tokens: t.max_tokens,
        max_turns: t.max_turns,
        created_by_name: by,
        used_by,
        created_at: t.created_at,
        updated_at: t.updated_at,
    })
}

fn check_provider(st: &AppState, p: Option<String>) -> ApiResult<Option<String>> {
    match p.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()) {
        Some(p) if st.cfg.providers().contains(&p.as_str()) => Ok(Some(p)),
        Some(_) => Err(AppError::bad_request("unknown provider")),
        None => Ok(None),
    }
}

fn check_model(m: Option<String>) -> ApiResult<Option<String>> {
    match m.map(|m| m.trim().to_string()).filter(|m| !m.is_empty()) {
        Some(m) if m.chars().count() > 200 || m.chars().any(char::is_control) => {
            Err(AppError::bad_request("Modellname ungültig"))
        }
        o => Ok(o),
    }
}

async fn name_taken(
    st: &AppState,
    project: Uuid,
    name: &str,
    except: Option<Uuid>,
) -> ApiResult<()> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM task_templates WHERE project_id=$1 AND name=$2 AND id IS DISTINCT FROM $3)",
    )
    .bind(project)
    .bind(name)
    .bind(except)
    .fetch_one(&st.db)
    .await?;
    if taken {
        return Err(AppError::conflict(format!(
            "Eine Vorlage „{name}“ gibt es in diesem Projekt schon"
        )));
    }
    Ok(())
}

pub async fn list_templates(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(pid): Path<Uuid>,
) -> ApiResult<Json<Vec<TemplateView>>> {
    require(&st, pid, u.id, Role::Viewer).await?;
    let rows: Vec<TemplateRow> =
        sqlx::query_as("SELECT * FROM task_templates WHERE project_id=$1 ORDER BY name")
            .bind(pid)
            .fetch_all(&st.db)
            .await?;
    let mut out = vec![];
    for r in rows {
        out.push(template_view(&st, r).await?);
    }
    Ok(Json(out))
}

pub async fn create_template(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(pid): Path<Uuid>,
    Json(t): Json<NewTemplate>,
) -> ApiResult<Json<TemplateView>> {
    require(&st, pid, u.id, Role::Editor).await?;
    let name = tr::validate_line("Name", &t.name, tr::MAX_NAME_CHARS).map_err(bad)?;
    let title = tr::validate_line("Titel", &t.title, tr::MAX_TITLE_CHARS).map_err(bad)?;
    let prompt = tr::validate_text("Beschreibung", &t.prompt, tr::MAX_PROMPT_CHARS).map_err(bad)?;
    let provider = check_provider(&st, t.provider)?;
    let model = check_model(t.model)?;
    let lim = t.limits.check()?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM task_templates WHERE project_id=$1")
        .bind(pid)
        .fetch_one(&st.db)
        .await?;
    if count >= MAX_TEMPLATES {
        return Err(AppError::conflict(format!(
            "höchstens {MAX_TEMPLATES} Vorlagen pro Projekt"
        )));
    }
    name_taken(&st, pid, &name, None).await?;
    let row: TemplateRow = sqlx::query_as(
        "INSERT INTO task_templates (id, project_id, created_by, name, title, prompt, provider, model, plan_mode,
                                     max_runtime_secs, max_cost_usd, max_tokens, max_turns)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(pid)
    .bind(u.id)
    .bind(&name)
    .bind(&title)
    .bind(&prompt)
    .bind(provider)
    .bind(model)
    .bind(t.plan_mode)
    .bind(lim.max_runtime_secs.flatten())
    .bind(lim.max_cost_usd.flatten())
    .bind(lim.max_tokens.flatten())
    .bind(lim.max_turns.flatten())
    .fetch_one(&st.db)
    .await?;
    Ok(Json(template_view(&st, row).await?))
}

async fn editable_template(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<TemplateRow> {
    let t: TemplateRow = sqlx::query_as("SELECT * FROM task_templates WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::not_found)?;
    require(st, t.project_id, user, Role::Editor).await?;
    Ok(t)
}

pub async fn update_template(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(c): Json<UpdateTemplate>,
) -> ApiResult<Json<TemplateView>> {
    let t = editable_template(&st, id, u.id).await?;
    let name = match c.name {
        Some(n) => {
            let n = tr::validate_line("Name", &n, tr::MAX_NAME_CHARS).map_err(bad)?;
            name_taken(&st, t.project_id, &n, Some(id)).await?;
            n
        }
        None => t.name,
    };
    let title = match c.title {
        Some(x) => tr::validate_line("Titel", &x, tr::MAX_TITLE_CHARS).map_err(bad)?,
        None => t.title,
    };
    let prompt = match c.prompt {
        Some(x) => tr::validate_text("Beschreibung", &x, tr::MAX_PROMPT_CHARS).map_err(bad)?,
        None => t.prompt,
    };
    let provider = match c.provider {
        Some(p) => check_provider(&st, p)?,
        None => t.provider,
    };
    let model = match c.model {
        Some(m) => check_model(m)?,
        None => t.model,
    };
    let plan_mode = c.plan_mode.unwrap_or(t.plan_mode);
    let lim = c.limits.check()?;
    let row: TemplateRow = sqlx::query_as(
        "UPDATE task_templates SET name=$2, title=$3, prompt=$4, provider=$5, model=$6, plan_mode=$7,
                max_runtime_secs=$8, max_cost_usd=$9, max_tokens=$10, max_turns=$11, updated_at=now()
         WHERE id=$1 RETURNING *",
    )
    .bind(id)
    .bind(name)
    .bind(title)
    .bind(prompt)
    .bind(provider)
    .bind(model)
    .bind(plan_mode)
    .bind(lim.max_runtime_secs.unwrap_or(t.max_runtime_secs))
    .bind(lim.max_cost_usd.unwrap_or(t.max_cost_usd))
    .bind(lim.max_tokens.unwrap_or(t.max_tokens))
    .bind(lim.max_turns.unwrap_or(t.max_turns))
    .fetch_one(&st.db)
    .await?;
    Ok(Json(template_view(&st, row).await?))
}

pub async fn delete_template(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    editable_template(&st, id, u.id).await?;
    let used: i64 = sqlx::query_scalar("SELECT count(*) FROM triggers WHERE template_id=$1")
        .bind(id)
        .fetch_one(&st.db)
        .await?;
    if used > 0 {
        return Err(AppError::conflict(format!(
            "Die Vorlage wird von {used} Auslöser(n) verwendet – zuerst dort eine andere Vorlage wählen oder die Auslöser löschen"
        )));
    }
    sqlx::query("DELETE FROM task_templates WHERE id=$1")
        .bind(id)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

type RunRow = (
    Uuid,
    Uuid,
    String,
    String,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<String>,
    DateTime<Utc>,
);

const RUN_SELECT: &str = "SELECT r.id, r.trigger_id, r.source, r.outcome, r.reason, r.task_id, t.title, t.status, r.event,
        r.scheduled_for, r.delivery_ref, a.name, r.created_at
   FROM trigger_runs r LEFT JOIN tasks t ON t.id = r.task_id LEFT JOIN users a ON a.id = r.actor_id";

fn run_view(r: RunRow) -> TriggerRunView {
    let (
        id,
        trigger_id,
        source,
        outcome,
        reason,
        task_id,
        task_title,
        task_status,
        event,
        scheduled_for,
        delivery_id,
        actor_name,
        created_at,
    ) = r;
    TriggerRunView {
        id,
        trigger_id,
        source: RunSource::parse(&source).unwrap_or(RunSource::Manual),
        outcome: RunOutcome::parse(&outcome).unwrap_or(RunOutcome::Failed),
        reason,
        task_id,
        task_title,
        task_status,
        event,
        scheduled_for,
        delivery_id,
        actor_name,
        created_at,
    }
}

async fn trigger_view(st: &AppState, t: &TriggerRow, viewer: Uuid) -> ApiResult<TriggerView> {
    let (owner_name, template_name): (String, String) = sqlx::query_as(
        "SELECT (SELECT name FROM users WHERE id=$1), (SELECT name FROM task_templates WHERE id=$2)",
    )
    .bind(t.user_id)
    .bind(t.template_id)
    .fetch_one(&st.db)
    .await?;
    let last: Option<RunRow> = sqlx::query_as(&format!(
        "{RUN_SELECT} WHERE r.trigger_id=$1 ORDER BY r.created_at DESC LIMIT 1"
    ))
    .bind(t.id)
    .fetch_optional(&st.db)
    .await?;
    let kind = t.kind();
    let schedule = (kind == TriggerKind::Schedule)
        .then(|| schedule_of(t).ok())
        .flatten();
    let upcoming = match (&schedule, t.next_run_at, t.enabled) {
        (Some((c, tz)), Some(n), true) => {
            let mut v = vec![];
            let mut last = n;
            for _ in 0..3 {
                match next_slot(c, tz, last, Some(last)) {
                    Some(x) => {
                        v.push(x);
                        last = x;
                    }
                    None => break,
                }
            }
            v
        }
        _ => vec![],
    };
    let viewer_role = role(st, t.project_id, viewer).await?;
    let can_manage = t.user_id == viewer && viewer_role.is_some_and(|r| r >= Role::Editor);
    Ok(TriggerView {
        id: t.id,
        project_id: t.project_id,
        owner_id: t.user_id,
        owner_name,
        template_id: t.template_id,
        template_name,
        kind,
        name: t.name.clone(),
        enabled: t.enabled,
        cron: t.cron.clone(),
        cron_text: schedule.as_ref().map(|(c, _)| c.describe()),
        timezone: t.timezone.clone(),
        run_at: t.run_at,
        next_run_at: if t.enabled { t.next_run_at } else { None },
        upcoming,
        catch_up: t.catch_up,
        github: t.github_filter(),
        max_per_hour: t.max_per_hour,
        skip_if_active: t.skip_if_active,
        hook_url: kind
            .has_secret()
            .then(|| format!("{}/api/hooks/{}", st.cfg.public_url, t.id)),
        disabled_reason: t.disabled_reason.clone(),
        last_fired_at: t.last_fired_at,
        last_run: last.map(run_view),
        can_manage,
        can_disable: t.user_id == viewer || viewer_role == Some(Role::Owner),
        created_at: t.created_at,
        updated_at: t.updated_at,
    })
}

/// The trigger if the caller sees the project (else 404).
async fn visible(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<TriggerRow> {
    let t: TriggerRow = sqlx::query_as("SELECT * FROM triggers WHERE id=$1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(AppError::not_found)?;
    require(st, t.project_id, user, Role::Viewer).await?;
    Ok(t)
}

/// Owner of the trigger who is still at least editor.
async fn managed(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<TriggerRow> {
    let t = visible(st, id, user).await?;
    if t.user_id != user {
        return Err(AppError(
            StatusCode::FORBIDDEN,
            "Nur die Person, der der Auslöser gehört, kann ihn ändern oder starten".into(),
        ));
    }
    require(st, t.project_id, user, Role::Editor).await?;
    Ok(t)
}

pub async fn list_triggers(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(pid): Path<Uuid>,
) -> ApiResult<Json<Vec<TriggerView>>> {
    require(&st, pid, u.id, Role::Viewer).await?;
    let rows: Vec<TriggerRow> =
        sqlx::query_as("SELECT * FROM triggers WHERE project_id=$1 ORDER BY created_at")
            .bind(pid)
            .fetch_all(&st.db)
            .await?;
    let mut out = vec![];
    for r in &rows {
        out.push(trigger_view(&st, r, u.id).await?);
    }
    Ok(Json(out))
}

pub async fn get_trigger(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TriggerView>> {
    let t = visible(&st, id, u.id).await?;
    Ok(Json(trigger_view(&st, &t, u.id).await?))
}

#[derive(Deserialize)]
pub struct RunsQuery {
    limit: Option<i64>,
}

pub async fn runs(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<Json<Vec<TriggerRunView>>> {
    visible(&st, id, u.id).await?;
    let rows: Vec<RunRow> = sqlx::query_as(&format!(
        "{RUN_SELECT} WHERE r.trigger_id=$1 ORDER BY r.created_at DESC LIMIT $2"
    ))
    .bind(id)
    .bind(q.limit.unwrap_or(50).clamp(1, 200))
    .fetch_all(&st.db)
    .await?;
    Ok(Json(rows.into_iter().map(run_view).collect()))
}

/// The settings a trigger ends up with, validated as a whole.
struct Spec {
    template_id: Uuid,
    name: String,
    enabled: bool,
    cron: Option<String>,
    timezone: Option<String>,
    run_at: Option<DateTime<Utc>>,
    catch_up: bool,
    github: Option<Value>,
    max_per_hour: i32,
    skip_if_active: bool,
}

async fn check_spec(
    st: &AppState,
    project: Uuid,
    kind: TriggerKind,
    s: &mut Spec,
) -> ApiResult<()> {
    s.name = tr::validate_line("Name", &s.name, tr::MAX_NAME_CHARS).map_err(bad)?;
    let ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM task_templates WHERE id=$1 AND project_id=$2)",
    )
    .bind(s.template_id)
    .bind(project)
    .fetch_one(&st.db)
    .await?;
    if !ok {
        return Err(AppError::bad_request(
            "Vorlage gehört nicht zu diesem Projekt",
        ));
    }
    if !(1..=MAX_PER_HOUR).contains(&s.max_per_hour) {
        return Err(AppError::bad_request(format!(
            "Aufträge pro Stunde: 1 bis {MAX_PER_HOUR}"
        )));
    }
    if let Some(tz) = &s.timezone {
        if !tr::plausible_timezone(tz) || parse_tz(tz).is_none() {
            return Err(AppError::bad_request(format!("Unbekannte Zeitzone „{tz}“")));
        }
    }
    match kind {
        TriggerKind::Schedule => {
            let cron = s
                .cron
                .as_deref()
                .ok_or_else(|| AppError::bad_request("Zeitplan fehlt"))?;
            tr::Cron::parse(cron).map_err(bad)?;
            s.cron = Some(cron.trim().to_string());
            if s.timezone.is_none() {
                return Err(AppError::bad_request("Zeitzone fehlt"));
            }
            s.run_at = None;
            s.github = None;
        }
        TriggerKind::Once => {
            if s.run_at.is_none() {
                return Err(AppError::bad_request("Zeitpunkt fehlt"));
            }
            s.cron = None;
            s.github = None;
        }
        TriggerKind::Github => {
            let f: tr::GithubFilter = s
                .github
                .clone()
                .and_then(|g| serde_json::from_value(g).ok())
                .ok_or_else(|| AppError::bad_request("GitHub-Filter fehlt"))?;
            let f = tr::validate_github(&f).map_err(bad)?;
            s.github = Some(serde_json::to_value(f).map_err(anyhow::Error::from)?);
            s.cron = None;
            s.run_at = None;
        }
        TriggerKind::Webhook | TriggerKind::Api => {
            s.cron = None;
            s.run_at = None;
            s.github = None;
        }
    }
    Ok(())
}

/// `next_run_at` of a (re)configured, enabled timed trigger.
fn next_due(kind: TriggerKind, s: &Spec, now: DateTime<Utc>) -> ApiResult<Option<DateTime<Utc>>> {
    if !s.enabled {
        return Ok(None);
    }
    match kind {
        TriggerKind::Schedule => {
            let cron = tr::Cron::parse(s.cron.as_deref().unwrap_or_default()).map_err(bad)?;
            let tz = s
                .timezone
                .as_deref()
                .and_then(parse_tz)
                .ok_or_else(|| AppError::bad_request("Unbekannte Zeitzone"))?;
            next_slot(&cron, &tz, now, None)
                .map(Some)
                .ok_or_else(|| AppError::bad_request("Dieser Zeitplan trifft nie zu"))
        }
        TriggerKind::Once => match s.run_at {
            Some(t) if t > now => Ok(Some(t)),
            _ => Err(AppError::bad_request(
                "Der Zeitpunkt liegt in der Vergangenheit",
            )),
        },
        _ => Ok(None),
    }
}

fn new_secret(st: &AppState) -> ApiResult<(String, Vec<u8>)> {
    let secret = super::tokens::generate(SECRET_PREFIX);
    let enc = crypto::encrypt(&st.cfg.master_key, secret.as_bytes())?;
    Ok((secret, enc))
}

pub async fn create_trigger(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(pid): Path<Uuid>,
    Json(n): Json<NewTrigger>,
) -> ApiResult<Json<CreatedTrigger>> {
    require(&st, pid, u.id, Role::Editor).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM triggers WHERE project_id=$1")
        .bind(pid)
        .fetch_one(&st.db)
        .await?;
    if count >= MAX_TRIGGERS {
        return Err(AppError::conflict(format!(
            "höchstens {MAX_TRIGGERS} Auslöser pro Projekt"
        )));
    }
    let mut s = Spec {
        template_id: n.template_id,
        name: n.name,
        enabled: n.enabled,
        cron: n.cron,
        timezone: n
            .timezone
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        run_at: n.run_at,
        catch_up: n.catch_up,
        github: n
            .github
            .map(|g| serde_json::to_value(g).map_err(anyhow::Error::from))
            .transpose()?,
        max_per_hour: n.max_per_hour,
        skip_if_active: n.skip_if_active,
    };
    check_spec(&st, pid, n.kind, &mut s).await?;
    let next = next_due(n.kind, &s, chrono::Utc::now())?;
    let (secret, enc) = if n.kind.has_secret() {
        let (s, e) = new_secret(&st)?;
        (Some(s), Some(e))
    } else {
        (None, None)
    };
    let row: TriggerRow = sqlx::query_as(
        "INSERT INTO triggers (id, project_id, user_id, template_id, kind, name, enabled, cron, timezone, run_at,
                               next_run_at, catch_up, secret_enc, github, max_per_hour, skip_if_active)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) RETURNING *",
    )
    .bind(Uuid::new_v4())
    .bind(pid)
    .bind(u.id)
    .bind(s.template_id)
    .bind(n.kind.as_str())
    .bind(&s.name)
    .bind(s.enabled)
    .bind(&s.cron)
    .bind(&s.timezone)
    .bind(s.run_at)
    .bind(next)
    .bind(s.catch_up)
    .bind(enc)
    .bind(&s.github)
    .bind(s.max_per_hour)
    .bind(s.skip_if_active)
    .fetch_one(&st.db)
    .await?;
    st.trigger_wake.notify_one();
    Ok(Json(CreatedTrigger {
        trigger: trigger_view(&st, &row, u.id).await?,
        secret,
    }))
}

pub async fn update_trigger(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(c): Json<UpdateTrigger>,
) -> ApiResult<Json<TriggerView>> {
    let t = visible(&st, id, u.id).await?;
    // Switching off is open to the project owner too; everything else only
    // to the trigger's owner.
    let only_off = c.enabled == Some(false)
        && UpdateTrigger {
            enabled: None,
            ..c.clone()
        } == UpdateTrigger::default();
    let project_owner = role(&st, t.project_id, u.id).await? == Some(Role::Owner);
    if !(only_off && project_owner) {
        managed(&st, id, u.id).await?;
    }
    let kind = t.kind();
    let timing_changed = c.cron.is_some()
        || c.timezone.is_some()
        || c.run_at.is_some()
        || (c.enabled == Some(true) && !t.enabled);
    let mut s = Spec {
        template_id: c.template_id.unwrap_or(t.template_id),
        name: c.name.unwrap_or_else(|| t.name.clone()),
        enabled: c.enabled.unwrap_or(t.enabled),
        cron: c.cron.or_else(|| t.cron.clone()),
        timezone: c
            .timezone
            .map(|z| z.trim().to_string())
            .filter(|z| !z.is_empty())
            .or_else(|| t.timezone.clone()),
        run_at: c.run_at.or(t.run_at),
        catch_up: c.catch_up.unwrap_or(t.catch_up),
        github: match c.github {
            Some(g) => Some(serde_json::to_value(g).map_err(anyhow::Error::from)?),
            None => t.github.clone(),
        },
        max_per_hour: c.max_per_hour.unwrap_or(t.max_per_hour),
        skip_if_active: c.skip_if_active.unwrap_or(t.skip_if_active),
    };
    check_spec(&st, t.project_id, kind, &mut s).await?;
    let next = if timing_changed {
        next_due(kind, &s, chrono::Utc::now())?
    } else {
        t.next_run_at
    };
    let row: TriggerRow = sqlx::query_as(
        "UPDATE triggers SET template_id=$2, name=$3, enabled=$4, cron=$5, timezone=$6, run_at=$7, next_run_at=$8,
                catch_up=$9, github=$10, max_per_hour=$11, skip_if_active=$12,
                disabled_reason = CASE WHEN $4 THEN NULL ELSE disabled_reason END, updated_at=now()
         WHERE id=$1 RETURNING *",
    )
    .bind(id)
    .bind(s.template_id)
    .bind(&s.name)
    .bind(s.enabled)
    .bind(&s.cron)
    .bind(&s.timezone)
    .bind(s.run_at)
    .bind(next)
    .bind(s.catch_up)
    .bind(&s.github)
    .bind(s.max_per_hour)
    .bind(s.skip_if_active)
    .fetch_one(&st.db)
    .await?;
    st.trigger_wake.notify_one();
    Ok(Json(trigger_view(&st, &row, u.id).await?))
}

pub async fn delete_trigger(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let t = visible(&st, id, u.id).await?;
    let owner = role(&st, t.project_id, u.id).await? == Some(Role::Owner);
    if t.user_id != u.id && !owner {
        return Err(AppError::forbidden());
    }
    sqlx::query("DELETE FROM triggers WHERE id=$1")
        .bind(id)
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn renew_secret(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CreatedTrigger>> {
    let t = managed(&st, id, u.id).await?;
    if !t.kind().has_secret() {
        return Err(AppError::bad_request("Dieser Auslöser hat kein Secret"));
    }
    let (secret, enc) = new_secret(&st)?;
    let row: TriggerRow = sqlx::query_as(
        "UPDATE triggers SET secret_enc=$2, updated_at=now() WHERE id=$1 RETURNING *",
    )
    .bind(id)
    .bind(enc)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(CreatedTrigger {
        trigger: trigger_view(&st, &row, u.id).await?,
        secret: Some(secret),
    }))
}

/// HTTP answer of a firing: 201 task created, 200 skipped/ignored/duplicate,
/// 429 rate limit, 409 refused or failed (body = [`FireResult`] + `error`).
pub fn fire_response(fired: Option<Fired>, rate_retry: Option<i64>) -> Response {
    let (status, result) = match fired {
        None => {
            return AppError::conflict("Auslöser ist ausgeschaltet").into_response();
        }
        Some(Fired::Duplicate) => (
            StatusCode::OK,
            FireResult {
                run_id: None,
                outcome: RunOutcome::Ignored,
                reason: Some("Schon verarbeitet (Duplikat)".into()),
                task_id: None,
            },
        ),
        Some(Fired::Done {
            run,
            outcome,
            reason,
            task,
        }) => {
            let status = match outcome {
                RunOutcome::Created => StatusCode::CREATED,
                RunOutcome::Ignored | RunOutcome::Skipped => StatusCode::OK,
                RunOutcome::Rejected
                    if reason.as_deref().is_some_and(|r| r.starts_with("Limit")) =>
                {
                    StatusCode::TOO_MANY_REQUESTS
                }
                RunOutcome::Rejected | RunOutcome::Failed => StatusCode::CONFLICT,
            };
            (
                status,
                FireResult {
                    run_id: Some(run),
                    outcome,
                    reason,
                    task_id: task,
                },
            )
        }
    };
    let mut body = serde_json::to_value(&result).unwrap_or_default();
    if !status.is_success() {
        body["error"] = json!(result.reason.clone().unwrap_or_default());
    }
    let mut resp = (status, Json(body)).into_response();
    if status == StatusCode::TOO_MANY_REQUESTS {
        if let Ok(v) = rate_retry.unwrap_or(3600).to_string().parse() {
            resp.headers_mut()
                .insert(axum::http::header::RETRY_AFTER, v);
        }
    }
    resp
}

/// Seconds until the oldest task of the last hour leaves the window.
pub async fn retry_after(st: &AppState, trigger: Uuid) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT GREATEST(1, CEIL(EXTRACT(EPOCH FROM (min(created_at) + interval '1 hour' - now()))))::BIGINT
         FROM trigger_runs WHERE trigger_id=$1 AND outcome='created' AND created_at > now() - interval '1 hour'",
    )
    .bind(trigger)
    .fetch_one(&st.db)
    .await
    .ok()
    .flatten()
}

pub async fn fire(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    tok: Option<Extension<TokenAuth>>,
    Path(id): Path<Uuid>,
    body: Option<Json<FireRequest>>,
) -> ApiResult<Response> {
    let t = managed(&st, id, u.id).await?;
    let text = body
        .and_then(|Json(b)| b.text)
        .filter(|x| !x.trim().is_empty());
    let text = match text {
        Some(x) => Some(tr::validate_text("Text", &x, MAX_FIRE_TEXT_CHARS).map_err(bad)?),
        None => None,
    };
    let via_api = tok.is_some();
    let f = Firing {
        source: Some(if via_api {
            RunSource::Api
        } else {
            RunSource::Manual
        }),
        actor: Some(u.id),
        token: tok.map(|Extension(t)| t.token_id),
        title_suffix: if via_api { "API" } else { "von Hand" }.into(),
        data: text.map(|x| {
            (
                format!(
                    "The trigger \"{}\" was started {} with the following text.",
                    t.name,
                    if via_api { "via the API" } else { "by hand" }
                ),
                vec![("Text".to_string(), x)],
            )
        }),
        ..Default::default()
    };
    let fired = triggers::fire(&st, id, f).await?;
    let retry = retry_after(&st, id).await;
    Ok(fire_response(fired, retry))
}

pub async fn preview(
    AuthUser(_u): AuthUser,
    Json(r): Json<CronPreviewRequest>,
) -> Json<CronPreview> {
    let err = |e: String| CronPreview {
        description: None,
        next_runs: vec![],
        error: Some(e),
    };
    let cron = match tr::Cron::parse(&r.cron) {
        Ok(c) => c,
        Err(e) => return Json(err(e)),
    };
    let Some(tz) = parse_tz(r.timezone.trim()) else {
        return Json(err(format!("Unbekannte Zeitzone „{}“", r.timezone.trim())));
    };
    let mut runs = vec![];
    let mut last: Option<DateTime<Utc>> = None;
    let mut from = chrono::Utc::now();
    for _ in 0..5 {
        match next_slot(&cron, &tz, from, last) {
            Some(n) => {
                runs.push(n);
                last = Some(n);
                from = n;
            }
            None => break,
        }
    }
    Json(CronPreview {
        description: Some(cron.describe()),
        next_runs: runs,
        error: None,
    })
}

pub async fn timezones(AuthUser(_u): AuthUser) -> Json<Vec<&'static str>> {
    let mut v: Vec<&'static str> = chrono_tz::TZ_VARIANTS.iter().map(|z| z.name()).collect();
    v.sort_unstable();
    Json(v)
}
