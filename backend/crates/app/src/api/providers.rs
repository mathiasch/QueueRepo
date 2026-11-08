//! WP13 endpoints: connectors, price catalog, cost estimate, cost summary,
//! per-step routing, CLI status.
use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use queued_api::{
    add_money, CliStatus, ConnectorCheck, ConnectorView, CostRow, CostSummary, Estimate,
    EstimateRequest, Money, NewConnector, PresetInfo, PriceCatalog, PriceOverride,
    PriceOverrideVersion, ProjectView, RoutingUpdate, StepMode, UpdateConnector, Usage,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{require, Role};
use crate::{
    auth::AuthUser,
    budget, connectors,
    error::{ApiResult, AppError},
    estimate::{self, Profile, StepInput},
    llm, pricing, routing,
    state::AppState,
};

// ---------------------------------------------------------------------------
// Connectors
// ---------------------------------------------------------------------------

async fn with_status(st: &AppState, user: Uuid, mut v: ConnectorView) -> ApiResult<ConnectorView> {
    let s = budget::status(&st.db, user, &connectors::limit_key(v.id)).await?;
    v.paused_reason = s.paused_reason;
    v.limited_until = s.limited_until;
    Ok(v)
}

/// Own connectors and project connectors of projects the caller edits.
pub async fn list(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<ConnectorView>>> {
    let rows: Vec<connectors::Row> = sqlx::query_as(
        "SELECT c.id, c.user_id, c.project_id, c.kind, c.label, c.config, c.secret_enc, c.created_at, c.updated_at
         FROM connectors c
         WHERE c.user_id=$1 OR c.project_id IN
             (SELECT project_id FROM project_members WHERE user_id=$1 AND role IN ('owner','editor'))
         ORDER BY c.project_id NULLS FIRST, c.created_at",
    )
    .bind(u.id)
    .fetch_all(&st.db)
    .await?;
    let mut out = vec![];
    for r in rows {
        let v = connectors::view(&r, r.1 == u.id);
        out.push(with_status(&st, u.id, v).await?);
    }
    Ok(Json(out))
}

fn type_or_400(st: &AppState, kind: &str) -> ApiResult<queued_api::ConnectorTypeInfo> {
    connectors::type_info(kind, st.cfg.enable_mock_provider)
        .ok_or_else(|| AppError::bad_request("Unbekannter Zugangstyp"))
}

fn label_or_400(label: &str) -> ApiResult<String> {
    let l = label.trim();
    if l.is_empty() || l.chars().count() > 100 {
        return Err(AppError::bad_request("Name: 1–100 Zeichen"));
    }
    Ok(l.to_string())
}

pub async fn create(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(c): Json<NewConnector>,
) -> ApiResult<Json<ConnectorView>> {
    let t = type_or_400(&st, &c.kind)?;
    let label = label_or_400(&c.label)?;
    if let Some(pid) = c.project_id {
        // Only the project owner shares an access with the project.
        require(&st, pid, u.id, Role::Owner).await?;
    }
    let config = connectors::normalize_config(&t, &c.config).map_err(AppError::bad_request)?;
    connectors::check_secret(&t, c.secret.as_deref(), true).map_err(AppError::bad_request)?;
    let enc = match c.secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(crate::crypto::encrypt(&st.cfg.master_key, s.as_bytes())?),
        None => None,
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO connectors (id, user_id, project_id, kind, label, config, secret_enc) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(u.id)
    .bind(c.project_id)
    .bind(&c.kind)
    .bind(&label)
    .bind(&config)
    .bind(enc)
    .execute(&st.db)
    .await?;
    let row = connectors::load_row(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(Json(
        with_status(&st, u.id, connectors::view(&row, true)).await?,
    ))
}

async fn own_row(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<connectors::Row> {
    let row = connectors::load_row(st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if row.1 != user {
        return Err(AppError::not_found());
    }
    Ok(row)
}

pub async fn update(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(c): Json<UpdateConnector>,
) -> ApiResult<Json<ConnectorView>> {
    let row = own_row(&st, id, u.id).await?;
    let t = type_or_400(&st, &row.3)?;
    let label = match &c.label {
        Some(l) => label_or_400(l)?,
        None => row.4.clone(),
    };
    let config = match &c.config {
        Some(cfg) => connectors::normalize_config(&t, cfg).map_err(AppError::bad_request)?,
        None => row.5.clone(),
    };
    connectors::check_secret(&t, c.secret.as_deref(), false).map_err(AppError::bad_request)?;
    let enc = match c.secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(crate::crypto::encrypt(&st.cfg.master_key, s.as_bytes())?),
        None => row.6.clone(),
    };
    sqlx::query(
        "UPDATE connectors SET label=$2, config=$3, secret_enc=$4, updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .bind(&label)
    .bind(&config)
    .bind(enc)
    .execute(&st.db)
    .await?;
    if c.secret.is_some() {
        // A new key lifts an auth pause of this access.
        budget::clear_pause(&st.db, u.id, &connectors::limit_key(id), false).await?;
        st.wake.notify_one();
    }
    let row = connectors::load_row(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(Json(
        with_status(&st, u.id, connectors::view(&row, true)).await?,
    ))
}

pub async fn remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    own_row(&st, id, u.id).await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tasks WHERE connector_id=$1 AND status IN ('queued','running','waiting_batch','waiting_input','pending_approval')",
    )
    .bind(id)
    .fetch_one(&st.db)
    .await?;
    if open > 0 {
        return Err(AppError::conflict(format!(
            "Der Zugang wird noch von {open} offenen Aufträgen benutzt"
        )));
    }
    let in_routing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM projects WHERE routing::text LIKE '%' || $1 || '%'",
    )
    .bind(id.to_string())
    .fetch_one(&st.db)
    .await?;
    if in_routing > 0 {
        return Err(AppError::conflict(
            "Der Zugang ist in der Modellwahl eines Projekts eingetragen – dort zuerst ändern",
        ));
    }
    sqlx::query("DELETE FROM connectors WHERE id=$1")
        .bind(id)
        .execute(&st.db)
        .await?;
    sqlx::query("DELETE FROM provider_limits WHERE provider=$1")
        .bind(connectors::limit_key(id))
        .execute(&st.db)
        .await?;
    Ok(Json(json!({"ok": true})))
}

pub async fn resume(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ConnectorView>> {
    let row = connectors::load_row(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if !connectors::can_use(&st, &row, u.id, row.2).await? {
        return Err(AppError::not_found());
    }
    budget::clear_pause(&st.db, u.id, &connectors::limit_key(id), true).await?;
    st.wake.notify_one();
    Ok(Json(
        with_status(&st, u.id, connectors::view(&row, row.1 == u.id)).await?,
    ))
}

/// Cheap reachability/authentication check (lists models). Agent-only
/// accesses (subscriptions, Bedrock, Vertex) cannot be checked without a run.
pub async fn check(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ConnectorCheck>> {
    let row = own_row(&st, id, u.id).await?;
    let c = connectors::resolve(&st, row)?;
    let Some((wire, base)) = c.wire() else {
        return Ok(Json(ConnectorCheck {
            ok: true,
            message:
                "Dieser Zugang lässt sich nur mit einem echten Auftrag prüfen (keine direkte API)."
                    .into(),
        }));
    };
    let ep = llm::Endpoint {
        wire,
        base: base.clone(),
        kind: c.kind.clone(),
        secret: c.secret.clone(),
        guard: st.cfg.llm_guard(),
    };
    let url = match wire {
        connectors::Wire::Anthropic => format!("{base}/v1/models"),
        connectors::Wire::OpenAiChat => format!("{base}/models"),
    };
    let http = llm::client();
    let res = llm::send_json::<Value>(
        &ep,
        ep.req(&http, reqwest::Method::GET, &url)
            .timeout(std::time::Duration::from_secs(20)),
    )
    .await;
    Ok(Json(match res {
        Ok(_) => ConnectorCheck {
            ok: true,
            message: "Verbindung und Schlüssel funktionieren.".into(),
        },
        Err(e) => ConnectorCheck {
            ok: false,
            message: match e.category {
                crate::agent::ErrorCategory::Auth => "Schlüssel wurde abgelehnt.".into(),
                crate::agent::ErrorCategory::Network => {
                    format!("Server nicht erreichbar: {}", e.message)
                }
                _ => e.message,
            },
        },
    }))
}

// ---------------------------------------------------------------------------
// Prices
// ---------------------------------------------------------------------------

pub async fn prices(
    State(st): State<AppState>,
    AuthUser(_u): AuthUser,
) -> ApiResult<Json<PriceCatalog>> {
    Ok(Json((*pricing::current(&st).await).clone()))
}

fn admin(u: &queued_api::User) -> ApiResult<()> {
    if u.is_admin {
        Ok(())
    } else {
        Err(AppError::forbidden())
    }
}

pub async fn get_overrides(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<PriceOverride>> {
    admin(&u)?;
    let v: Option<Value> =
        sqlx::query_scalar("SELECT entries FROM price_overrides ORDER BY version DESC LIMIT 1")
            .fetch_optional(&st.db)
            .await?;
    Ok(Json(
        v.and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
    ))
}

pub async fn put_overrides(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(o): Json<PriceOverride>,
) -> ApiResult<Json<PriceCatalog>> {
    admin(&u)?;
    pricing::validate(&o).map_err(AppError::bad_request)?;
    sqlx::query("INSERT INTO price_overrides (entries, note, created_by) VALUES ($1,$2,$3)")
        .bind(serde_json::to_value(&o).unwrap_or_default())
        .bind(o.note.chars().take(500).collect::<String>())
        .bind(u.id)
        .execute(&st.db)
        .await?;
    Ok(Json((*pricing::current(&st).await).clone()))
}

pub async fn override_history(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<Vec<PriceOverrideVersion>>> {
    admin(&u)?;
    let rows: Vec<(
        i32,
        String,
        Option<String>,
        chrono::DateTime<chrono::Utc>,
        Value,
    )> = sqlx::query_as(
        "SELECT o.version, o.note, u.email, o.created_at, o.entries FROM price_overrides o
         LEFT JOIN users u ON u.id = o.created_by ORDER BY o.version DESC LIMIT 50",
    )
    .fetch_all(&st.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(version, note, created_by, created_at, e)| PriceOverrideVersion {
                    version,
                    note,
                    created_by,
                    created_at,
                    entries: e["entries"].as_array().map_or(0, |a| a.len()),
                },
            )
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Estimate and costs
// ---------------------------------------------------------------------------

/// Finished-task token profiles of the user for a model (agent loop only).
async fn history(st: &AppState, user: Uuid, model: &str) -> sqlx::Result<Vec<Usage>> {
    let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT SUM(l.input_tokens)::BIGINT, SUM(l.output_tokens)::BIGINT, SUM(l.cache_read_tokens)::BIGINT, SUM(l.cache_write_tokens)::BIGINT
         FROM usage_log l JOIN tasks t ON t.id = l.task_id
         WHERE l.user_id=$1 AND l.step='execute' AND lower(l.model)=lower($2) AND t.status='done'
           AND l.ts > now() - interval '35 days'
         GROUP BY l.task_id ORDER BY max(l.ts) DESC LIMIT 200",
    )
    .bind(user)
    .bind(model)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(i, o, cr, cw)| Usage {
            input_tokens: i,
            output_tokens: o,
            cache_read_tokens: cr,
            cache_write_tokens: cw,
            cost_usd: 0.0,
        })
        .collect())
}

pub async fn estimate(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(r): Json<EstimateRequest>,
) -> ApiResult<Json<Estimate>> {
    require(&st, r.project_id, u.id, Role::Editor).await?;
    let project = super::projects::load(&st, r.project_id).await?;
    let settings = crate::queue::load_settings(&st, u.id).await?;
    let exec = routing::resolve_execute(
        &st,
        u.id,
        &project,
        r.connector_id,
        r.provider.as_deref(),
        r.model.clone(),
        &settings.default_provider,
    )
    .await?
    .map_err(AppError::bad_request)?;
    let all = routing::accesses(&st, u.id, project.id).await?;
    let cat = pricing::current(&st).await;
    let target = queued_api::Target {
        connector_id: exec.connector_id,
        provider: exec.connector_id.is_none().then(|| exec.provider.clone()),
        model: exec.model.clone(),
    };
    let exec_acc = routing::find(&all, &target);
    let exec_cfg = match exec.connector_id {
        Some(id) => connectors::load_row(&st, id).await?.map(|r| r.5),
        None => None,
    };
    let profile = match &exec.model {
        Some(m) => {
            Profile::from_history(history(&st, u.id, m).await?).unwrap_or_else(Profile::report)
        }
        None => Profile::report(),
    };
    let route = project.routing();
    let mut notes = vec![];
    let mut steps = vec![];
    let flex = route.as_ref().is_some_and(|r| {
        r.execute.flex
            && r.execute.target.as_ref().and_then(|t| t.connector_id) == exec.connector_id
            && exec.connector_id.is_some()
    });
    let single = |name: &'static str, sr: &queued_api::StepRoute, u: Usage| {
        let t = sr.target.as_ref();
        // An agentic step on the agent loop's own access runs with its (Flex) tier.
        let same_access = t.is_none_or(|t| t.connector_id == exec.connector_id);
        let acc = t.and_then(|t| routing::find(&all, t)).or(exec_acc);
        let model = t
            .and_then(|t| t.model.as_deref())
            .filter(|m| !m.is_empty())
            .or(exec.model.as_deref());
        estimate::step(
            &cat,
            StepInput {
                step: name,
                mode: sr.mode,
                access: acc,
                config: None,
                model,
                profile: Profile::single(u, estimate::REPORT_BASIS),
                flex: flex && sr.mode == StepMode::Harness && same_access,
            },
        )
    };
    if let Some(r) = route.as_ref().filter(|r| r.plan.mode != StepMode::Off) {
        steps.push(single("plan", &r.plan, estimate::PLAN));
    }
    steps.push(estimate::step(
        &cat,
        StepInput {
            step: "execute",
            mode: StepMode::Harness,
            access: exec_acc,
            config: exec_cfg.as_ref(),
            model: exec.model.as_deref(),
            profile,
            flex,
        },
    ));
    if project.review_enabled {
        let rr =
            route
                .as_ref()
                .map(|r| r.review.clone())
                .unwrap_or_else(|| queued_api::StepRoute {
                    mode: StepMode::Harness,
                    target: project
                        .review_provider
                        .clone()
                        .filter(|p| !p.is_empty())
                        .map(|p| queued_api::Target {
                            connector_id: None,
                            provider: Some(p),
                            model: project.review_model.clone(),
                        }),
                });
        steps.push(single("review", &rr, estimate::REVIEW));
    }
    if exec.model.is_none() {
        notes.push(
            "Ohne Modellangabe ist der Preis unbekannt (die CLI wählt ihr Standardmodell).".into(),
        );
    }
    let location = match exec.connector_id {
        Some(id) => connectors::load_row(&st, id)
            .await?
            .map(|r| connectors::location(&r.3, &r.5).0),
        None => Some(connectors::legacy_location(&exec.provider)),
    }
    .map(|l| l.label().to_string())
    .unwrap_or_default();
    Ok(Json(estimate::assemble(steps, &location, notes)))
}

#[derive(Deserialize)]
pub struct CostQuery {
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

pub async fn costs(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Query(q): Query<CostQuery>,
) -> ApiResult<Json<CostSummary>> {
    let days = q.days.unwrap_or(30).clamp(1, 35);
    // Own usage; with a project: every member's usage in that project (members see project costs).
    if let Some(pid) = q.project_id {
        require(&st, pid, u.id, Role::Viewer).await?;
    }
    let rows: Vec<(Option<Uuid>, Option<String>, Option<String>, String, String, String, f64, i64, i64)> = sqlx::query_as(
        "SELECT l.project_id, p.name, l.model, l.step, l.currency, l.cost_source,
                SUM(l.cost_amount)::DOUBLE PRECISION,
                SUM(l.input_tokens + l.output_tokens + l.cache_write_tokens + l.cache_read_tokens)::BIGINT,
                COUNT(*)
         FROM usage_log l LEFT JOIN projects p ON p.id = l.project_id
         WHERE l.ts > now() - make_interval(days => $2::int)
           AND CASE WHEN $3::uuid IS NULL THEN l.user_id = $1 ELSE l.project_id = $3 END
         GROUP BY 1,2,3,4,5,6",
    )
    .bind(u.id)
    .bind(days as i32)
    .bind(q.project_id)
    .fetch_all(&st.db)
    .await?;
    let mut totals: Vec<Money> = vec![];
    let mut by: [HashMap<(String, String), CostRow>; 3] = Default::default();
    let mut unknown_runs = 0;
    for (pid, pname, model, step, cur, source, amount, tokens, runs) in rows {
        add_money(&mut totals, &cur, amount);
        if source == "unknown" {
            unknown_runs += runs;
        }
        let keys = [
            (
                pid.map(|p| p.to_string()).unwrap_or_default(),
                pname.unwrap_or_else(|| "(gelöscht)".into()),
            ),
            (
                model.clone().unwrap_or_default(),
                model.unwrap_or_else(|| "(ohne Modellangabe)".into()),
            ),
            (step.clone(), step_label(&step).into()),
        ];
        for (i, (k, label)) in keys.into_iter().enumerate() {
            let e = by[i].entry((k.clone(), cur.clone())).or_insert(CostRow {
                key: k,
                label,
                currency: cur.clone(),
                amount: 0.0,
                tokens: 0,
                runs: 0,
            });
            e.amount += amount;
            e.tokens = e.tokens.saturating_add(tokens);
            e.runs += runs;
        }
    }
    let sorted = |m: HashMap<(String, String), CostRow>| {
        let mut v: Vec<CostRow> = m.into_values().collect();
        v.sort_by(|a, b| b.amount.total_cmp(&a.amount).then(a.label.cmp(&b.label)));
        v
    };
    let [p, m, s] = by;
    Ok(Json(CostSummary {
        days,
        totals,
        by_project: sorted(p),
        by_model: sorted(m),
        by_step: sorted(s),
        unknown_runs,
    }))
}

pub fn step_label(step: &str) -> &'static str {
    match step {
        "plan" => "Planung",
        "execute" => "Umsetzung",
        "review" => "Review",
        "analyze" => "Analyse",
        _ => "Sonstiges",
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

pub async fn presets(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<PresetInfo>>> {
    require(&st, id, u.id, Role::Editor).await?;
    let all = routing::accesses(&st, u.id, id).await?;
    Ok(Json(routing::presets(&all, &*pricing::current(&st).await)))
}

pub async fn put_routing(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(r): Json<RoutingUpdate>,
) -> ApiResult<Json<ProjectView>> {
    require(&st, id, u.id, Role::Editor).await?;
    if let Some(routing) = &r.routing {
        let all = routing::accesses(&st, u.id, id).await?;
        routing::validate(
            routing,
            r.data_residency,
            &all,
            &*pricing::current(&st).await,
        )
        .map_err(AppError::bad_request)?;
    }
    // Lowering the residency rule is an owner decision.
    let project = super::projects::load(&st, id).await?;
    if r.data_residency != project.residency() && r.data_residency == queued_api::Residency::Any {
        require(&st, id, u.id, Role::Owner).await?;
    }
    sqlx::query("UPDATE projects SET routing=$2, data_residency=$3 WHERE id=$1")
        .bind(id)
        .bind(
            r.routing
                .as_ref()
                .map(|x| serde_json::to_value(x).unwrap_or_default()),
        )
        .bind(r.data_residency.as_str())
        .execute(&st.db)
        .await?;
    st.wake.notify_one();
    let role = super::projects::role(&st, id, u.id)
        .await?
        .ok_or_else(AppError::not_found)?;
    Ok(Json(
        super::projects::load(&st, id).await?.view(role.into()),
    ))
}

// ---------------------------------------------------------------------------
// CLI status (runner probe)
// ---------------------------------------------------------------------------

pub async fn cli_status(
    State(st): State<AppState>,
    AuthUser(_u): AuthUser,
) -> ApiResult<Json<Vec<CliStatus>>> {
    let found = st
        .runner
        .cli_status()
        .await
        .map_err(|e| AppError::from(e.context("Runner nicht erreichbar")))?;
    Ok(Json(crate::agent::interpret_cli_status(&found)))
}
