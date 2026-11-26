//! WP15: model chooser endpoints – project settings, Best-of-N suggestions,
//! the user's own preferences, feedback on tasks and the admin view
//! (statistics, pooling, replay).
use axum::{
    extract::{Path, Query, State},
    Json,
};
use queued_api::chooser::{
    AttemptSuggestion, ChooserAdminUpdate, ChooserAdminView, ChooserPrefs, ChooserSettings,
    ChooserSettingsView, CountRow, FeedbackView, LearnedCell, ReplayReport, ReplayRequest,
    TaskFeedback,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    admin::Admin,
    projects::{require, Role},
};
use crate::{
    audit,
    auth::AuthUser,
    chooser::{self, store},
    error::{ApiResult, AppError},
    queue,
    state::AppState,
};

/// `GET /api/projects/{id}/chooser` (members).
pub async fn get_project(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ChooserSettingsView>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let can_edit = require(&st, id, u.id, Role::Editor).await.is_ok();
    let project = super::projects::load(&st, id).await?;
    let settings = store::settings(&st, id).await?;
    let spent = store::explore_spent(&st, id).await?;
    let prefs = store::prefs(&st, u.id).await?;
    let classify_model = if can_edit {
        let cands = chooser::candidates(&st, u.id, id).await?;
        let blocked: Vec<String> = settings
            .blocked_models
            .iter()
            .chain(&prefs.blocked_models)
            .cloned()
            .collect();
        chooser::classify_target(&cands, project.residency(), &blocked).map(|c| c.model.clone())
    } else {
        None
    };
    Ok(Json(ChooserSettingsView {
        settings,
        explore_spent_usd: spent,
        classify_model,
        can_edit,
    }))
}

/// `PUT /api/projects/{id}/chooser` (editors; audited).
pub async fn put_project(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(s): Json<ChooserSettings>,
) -> ApiResult<Json<ChooserSettings>> {
    require(&st, id, u.id, Role::Editor).await?;
    let s = store::validate(&s).map_err(AppError::bad_request)?;
    store::put_settings(&st, id, &s).await?;
    audit::log(
        &st,
        &u,
        "project.chooser_changed",
        ("project", &id.to_string()),
        Some(id),
        json!({
            "enabled": s.enabled,
            "quality": s.quality,
            "explore_budget_usd": s.explore_budget_usd,
            "classify": s.classify,
            "blocked_models": s.blocked_models.len(),
        }),
    )
    .await;
    Ok(Json(s))
}

#[derive(Deserialize)]
pub struct SuggestQuery {
    #[serde(default = "two")]
    pub attempts: usize,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub title: String,
}

fn two() -> usize {
    2
}

/// `GET /api/projects/{id}/chooser/suggest?attempts=N` (editors): models
/// for the attempts of a Best-of-N task.
pub async fn suggest(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<SuggestQuery>,
) -> ApiResult<Json<AttemptSuggestion>> {
    require(&st, id, u.id, Role::Editor).await?;
    let n = q.attempts.clamp(2, queued_api::bon::MAX_ATTEMPTS as usize);
    let project = super::projects::load(&st, id).await?;
    let prompt: String = q.prompt.chars().take(20_000).collect();
    let title: String = q.title.chars().take(200).collect();
    let ask = crate::routing::Ask::new("task", &title, &prompt, "", 0);
    let picked = chooser::suggest(&st, u.id, &project, &ask, n).await?;
    let reason = if picked.len() < n {
        format!(
            "Nur {} passende Modelle unter deinen Zugängen – die übrigen Versuche nutzen den Zugang des Auftrags.",
            picked.len()
        )
    } else {
        "Die besten Modelle laut automatischer Modellwahl, möglichst von verschiedenen Anbietern."
            .into()
    };
    Ok(Json(AttemptSuggestion {
        targets: picked.iter().map(|c| c.target.clone()).collect(),
        labels: picked
            .iter()
            .map(|c| format!("{} · {}", c.model, c.access))
            .collect(),
        reason,
    }))
}

/// `GET /api/chooser/prefs`.
pub async fn get_prefs(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<ChooserPrefs>> {
    Ok(Json(store::prefs(&st, u.id).await?))
}

/// `PUT /api/chooser/prefs` (audited: sharing learning data is a privacy
/// decision).
pub async fn put_prefs(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Json(p): Json<ChooserPrefs>,
) -> ApiResult<Json<ChooserPrefs>> {
    let blocked = store::clean_models(&p.blocked_models).map_err(AppError::bad_request)?;
    let before = store::prefs(&st, u.id).await?;
    let p = ChooserPrefs {
        blocked_models: blocked,
        ..p
    };
    store::put_prefs(&st, u.id, &p).await?;
    if before.share_learning != p.share_learning {
        audit::log(
            &st,
            &u,
            "account.chooser_sharing_changed",
            ("user", &u.id.to_string()),
            None,
            json!({"share_learning": p.share_learning}),
        )
        .await;
    }
    Ok(Json(store::prefs(&st, u.id).await?))
}

/// Only the task's owner gives feedback: it feeds **their** learning data.
async fn owned(st: &AppState, id: Uuid, user: Uuid) -> ApiResult<crate::models::Task> {
    let t = queue::load_task(st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if t.user_id != user {
        return Err(AppError::not_found());
    }
    Ok(t)
}

/// `GET /api/tasks/{id}/feedback`.
pub async fn get_feedback(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FeedbackView>> {
    owned(&st, id, u.id).await?;
    Ok(Json(FeedbackView {
        good: chooser::learn::feedback_of(&st, id).await?,
    }))
}

/// `POST /api/tasks/{id}/feedback {good}`: finished tasks only.
pub async fn post_feedback(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(f): Json<TaskFeedback>,
) -> ApiResult<Json<FeedbackView>> {
    let t = owned(&st, id, u.id).await?;
    if !matches!(
        t.status.as_str(),
        "done" | "failed" | "blocked" | "candidate"
    ) {
        return Err(AppError::conflict(
            "Rückmeldung geht erst, wenn der Auftrag beendet ist",
        ));
    }
    if crate::pricing::find(
        crate::pricing::builtin(),
        t.model.as_deref().unwrap_or_default(),
        chrono::Utc::now(),
    )
    .is_none()
    {
        return Err(AppError::bad_request(
            "Für dieses Modell ist kein Preis bekannt – die Rückmeldung kann nicht zugeordnet werden",
        ));
    }
    chooser::learn::feedback(&st, &t, f.good).await?;
    queue::event(
        &st,
        &t,
        "status",
        if f.good {
            "Rückmeldung: Ergebnis gut"
        } else {
            "Rückmeldung: Ergebnis schlecht"
        },
    )
    .await;
    Ok(Json(FeedbackView {
        good: chooser::learn::feedback_of(&st, id).await?,
    }))
}

/// `GET /api/admin/chooser`: aggregated statistics (no task texts).
pub async fn admin_view(
    State(st): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<ChooserAdminView>> {
    let (pooling, half_life) = store::instance(&st).await?;
    let sharing: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chooser_user_prefs WHERE share_learning")
            .fetch_one(&st.db)
            .await?;
    let decisions: Vec<(String, i64)> = sqlx::query_as(
        "SELECT source, COUNT(*) FROM chooser_decisions WHERE created_at > now() - interval '30 days'
         GROUP BY source ORDER BY source",
    )
    .fetch_all(&st.db)
    .await?;
    let (explored, explore_cost, classify_calls, classify_cost, expected): (
        i64,
        f64,
        i64,
        f64,
        f64,
    ) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE explored),
                COALESCE(SUM(explore_cost_usd), 0)::DOUBLE PRECISION,
                COUNT(*) FILTER (WHERE classify_cost_usd > 0),
                COALESCE(SUM(classify_cost_usd), 0)::DOUBLE PRECISION,
                COALESCE(AVG(expected_cost_usd) FILTER (WHERE source = 'chooser'), 0)::DOUBLE PRECISION
         FROM chooser_decisions WHERE created_at > now() - interval '30 days'",
    )
    .fetch_one(&st.db)
    .await?;
    let reasons: Vec<(String, i64)> = sqlx::query_as(
        "SELECT left(fallback_reason, 120), COUNT(*) FROM chooser_decisions
         WHERE fallback_reason IS NOT NULL AND created_at > now() - interval '30 days'
         GROUP BY 1 ORDER BY 2 DESC LIMIT 10",
    )
    .fetch_all(&st.db)
    .await?;
    let cells: Vec<(String, String, String, f64, f64, f64, i64)> = sqlx::query_as(
        "SELECT step, category, model_key,
                SUM(weight * power(0.5, EXTRACT(EPOCH FROM now() - created_at) / 86400.0 / $1))::DOUBLE PRECISION AS n,
                SUM(success * weight * power(0.5, EXTRACT(EPOCH FROM now() - created_at) / 86400.0 / $1))::DOUBLE PRECISION,
                AVG(cost_usd)::DOUBLE PRECISION,
                COUNT(DISTINCT user_id)
         FROM chooser_observations GROUP BY step, category, model_key
         ORDER BY n DESC LIMIT 200",
    )
    .bind(f64::from(half_life))
    .fetch_all(&st.db)
    .await?;
    let enabled: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chooser_settings WHERE enabled")
        .fetch_one(&st.db)
        .await?;
    Ok(Json(ChooserAdminView {
        pooling,
        half_life_days: half_life,
        sharing_users: sharing,
        decisions: decisions
            .into_iter()
            .map(|(label, count)| CountRow { label, count })
            .collect(),
        explored,
        explore_cost_usd: explore_cost,
        classify_calls,
        classify_cost_usd: classify_cost,
        expected_cost_usd: expected,
        fallback_reasons: reasons
            .into_iter()
            .map(|(label, count)| CountRow { label, count })
            .collect(),
        cells: cells
            .into_iter()
            .map(|(step, category, model, n, s, cost, users)| LearnedCell {
                step,
                category,
                model,
                observations: n,
                success_rate: if n > 0.0 { s / n } else { 0.0 },
                avg_cost: cost,
                users,
            })
            .collect(),
        projects_enabled: enabled,
    }))
}

/// `PUT /api/admin/chooser` (audited).
pub async fn admin_put(
    State(st): State<AppState>,
    Admin(me): Admin,
    Json(b): Json<ChooserAdminUpdate>,
) -> ApiResult<Json<ChooserAdminView>> {
    if !(7..=365).contains(&b.half_life_days) {
        return Err(AppError::bad_request("Halbwertszeit: 7 bis 365 Tage"));
    }
    sqlx::query(
        "UPDATE instance_settings SET chooser_pooling=$1, chooser_half_life_days=$2, updated_at=now() WHERE id=1",
    )
    .bind(b.pooling)
    .bind(b.half_life_days)
    .execute(&st.db)
    .await?;
    audit::log(
        &st,
        &me,
        "admin.chooser_changed",
        ("instance", "1"),
        None,
        json!({"pooling": b.pooling, "half_life_days": b.half_life_days}),
    )
    .await;
    admin_view(State(st), Admin(me)).await
}

/// `POST /api/admin/chooser/replay`: offline evaluation against the
/// presets (synthetic or this instance's history).
pub async fn admin_replay(
    State(st): State<AppState>,
    Admin(_): Admin,
    Json(r): Json<ReplayRequest>,
) -> ApiResult<Json<ReplayReport>> {
    let n = r.tasks.clamp(10, 2_000);
    let report = match r.source.as_str() {
        "synthetic" => {
            let seed = r.seed;
            tokio::task::spawn_blocking(move || chooser::replay::synthetic(n, seed))
                .await
                .map_err(|e| anyhow::anyhow!("replay: {e}"))?
        }
        "history" => chooser::replay::history(&st, n, r.seed)
            .await
            .map_err(|e| AppError::bad_request(format!("{e}")))?,
        _ => return Err(AppError::bad_request("Quelle: synthetic oder history")),
    };
    Ok(Json(report))
}
