//! Learning hooks of the chooser (WP15): outcomes of real runs, review
//! verdicts, user feedback, reverts and eval runs become observations of
//! (step × category × model). Every hook only logs its own errors – it
//! never changes the outcome of the task.
//!
//! What counts (WP01 failure classes):
//! * `execute` accepted (verify + review passed) = 1; a real task failure
//!   (`task`) or a cost/token/turn/runtime limit = 0; infrastructure,
//!   provider pauses, configuration and cancellations are **not** the
//!   model's result and are not recorded;
//! * `plan` = the first attempt with that plan was accepted;
//! * `review` = an approval (later negative feedback/revert adds a 0);
//! * `analyze` = the OnEmpty analysis finished without error.
use queued_api::{FailureClass, TaskCost};

use super::{features, store};
use crate::{models::Task, pricing, queue, state::AppState};

/// Category, size of a task for learning: from the chooser's report, else
/// the heuristic on the original task (fix tasks learn for their root).
async fn category_of(st: &AppState, task: &Task) -> (String, String) {
    if let Some(r) = super::report_of(task) {
        return (r.features.category, r.features.size);
    }
    let mut root = task.clone();
    for _ in 0..20 {
        if root.kind != "fix" {
            break;
        }
        match root.parent_id {
            Some(pid) => match queue::load_task(st, pid).await {
                Ok(Some(p)) => root = p,
                _ => break,
            },
            None => break,
        }
    }
    if let Some(r) = super::report_of(&root) {
        return (r.features.category, r.features.size);
    }
    let f = features::analyze(&features::Input {
        kind: &root.kind,
        title: &root.title,
        prompt: &root.prompt,
        context: &root.context,
        ..Default::default()
    });
    (f.category, f.size)
}

/// Eval runs (WP13): the case's own category (`security` cases are bugfixes
/// with a risk) and source `eval`.
async fn eval_category(st: &AppState, task: &Task) -> Option<(String, String)> {
    let case: Option<String> =
        sqlx::query_scalar("SELECT case_id FROM eval_results WHERE task_id=$1")
            .bind(task.id)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten();
    let case = case?;
    let c = crate::eval::cases().into_iter().find(|c| c.id == case)?;
    let cat = match c.category.as_str() {
        "security" => "bugfix".to_string(),
        x if queued_api::chooser::CATEGORIES.contains(&x) => x.to_string(),
        _ => "other".to_string(),
    };
    Some((cat, c.size))
}

fn usd_of(cost: Option<&serde_json::Value>) -> f64 {
    cost.and_then(|v| serde_json::from_value::<TaskCost>(v.clone()).ok())
        .map(|c| {
            c.totals
                .iter()
                .filter(|m| m.currency == "USD")
                .map(|m| m.amount)
                .sum()
        })
        .unwrap_or(0.0)
}

/// Catalog key and family of a model name.
fn key_of(model: Option<&str>) -> Option<(String, String)> {
    let e = pricing::find(pricing::builtin(), model?, chrono::Utc::now())?;
    Some((e.id.clone(), e.family.clone()))
}

#[allow(clippy::too_many_arguments)]
async fn put(
    st: &AppState,
    task: &Task,
    step: &str,
    category: &str,
    size: &str,
    model: Option<&str>,
    success: f64,
    source: &str,
    cost: f64,
) {
    let Some((key, family)) = key_of(model) else {
        return;
    };
    let r = store::observe(
        st,
        store::Observation {
            user: task.user_id,
            project: Some(task.project_id),
            task: Some(task.id),
            step,
            category,
            model_key: &key,
            family: &family,
            size,
            success,
            weight: 1.0,
            cost,
            source,
        },
    )
    .await;
    if let Err(e) = r {
        tracing::warn!("task {}: chooser observation not stored: {e}", task.id);
    }
}

/// The task's verdict after verify and review (`accepted`) or a real task
/// failure – agent loop, and the plan step that prepared it.
pub async fn run_outcome(st: &AppState, task: &Task, accepted: bool) {
    if task.kind == "onempty" {
        return analyze(st, task, accepted).await;
    }
    let fresh = queue::load_task(st, task.id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| task.clone());
    let (category, size, source) = match eval_category(st, &fresh).await {
        Some((c, s)) => (c, s, "eval"),
        None => {
            let (c, s) = category_of(st, &fresh).await;
            (c, s, "run")
        }
    };
    let s = if accepted { 1.0 } else { 0.0 };
    let cost = usd_of(fresh.cost.as_ref());
    put(
        st,
        &fresh,
        "execute",
        &category,
        &size,
        fresh.model.as_deref(),
        s,
        source,
        cost,
    )
    .await;
    // The plan step of a first attempt (fix tasks have no plan step).
    if fresh.kind == "task" && fresh.plan.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        if let Ok(project) = crate::api::projects::load(st, fresh.project_id).await {
            if let Some(r) = super::routing_for(&fresh, &project) {
                if super::single_shot(r.plan.mode) {
                    let m = r.plan.target.as_ref().and_then(|t| t.model.clone());
                    put(st, &fresh, "plan", &category, &size, m.as_deref(), s, source, 0.0).await;
                }
            }
        }
    }
}

/// A limit stopped the run (cost, tokens, turns, runtime): the model did
/// not deliver within the budget – a failure for the cost per accepted task.
pub async fn limit_stop(st: &AppState, task: &Task, class: FailureClass) {
    if matches!(
        class,
        FailureClass::LimitCost
            | FailureClass::LimitTokens
            | FailureClass::LimitTurns
            | FailureClass::LimitRuntime
    ) {
        run_outcome(st, task, false).await;
    }
}

/// A review verdict on `model` (approvals only; a rejection's correctness
/// is unknown until the fix).
pub async fn review_verdict(st: &AppState, task: &Task, model: Option<&str>, approved: bool) {
    if !approved {
        return;
    }
    let (category, size) = category_of(st, task).await;
    put(st, task, "review", &category, &size, model, 1.0, "run", 0.0).await;
}

async fn analyze(st: &AppState, task: &Task, ok: bool) {
    let model = match crate::api::projects::load(st, task.project_id).await {
        Ok(p) => super::routing_for(task, &p)
            .filter(|r| super::single_shot(r.analyze.mode))
            .and_then(|r| r.analyze.target.and_then(|t| t.model))
            .or(task.model.clone()),
        Err(_) => task.model.clone(),
    };
    put(
        st,
        task,
        "analyze",
        "analysis",
        "M",
        model.as_deref(),
        if ok { 1.0 } else { 0.0 },
        "run",
        usd_of(task.cost.as_ref()),
    )
    .await;
}

/// A human's verdict on a finished task ("Ergebnis gut/schlecht"): the agent
/// loop, and the review that approved it. The latest feedback replaces the
/// previous one (one row per task, step and source).
pub async fn feedback(st: &AppState, task: &Task, good: bool) -> sqlx::Result<()> {
    let (category, size) = category_of(st, task).await;
    let s = if good { 1.0 } else { 0.0 };
    put(st, task, "execute", &category, &size, task.model.as_deref(), s, "feedback", 0.0).await;
    let reviewed: Option<String> = sqlx::query_scalar(
        "SELECT model_key FROM chooser_observations WHERE task_id=$1 AND step='review' AND source='run'",
    )
    .bind(task.id)
    .fetch_optional(&st.db)
    .await?;
    if let Some(key) = reviewed {
        let e = pricing::builtin().entries.iter().find(|e| e.id == key);
        if let Some(e) = e {
            let model = e.models.first().cloned();
            put(st, task, "review", &category, &size, model.as_deref(), s, "feedback", 0.0).await;
        }
    }
    Ok(())
}

/// The current feedback of a task (`None` = none given).
pub async fn feedback_of(st: &AppState, task: uuid::Uuid) -> sqlx::Result<Option<bool>> {
    let v: Option<f32> = sqlx::query_scalar(
        "SELECT success FROM chooser_observations WHERE task_id=$1 AND step='execute' AND source='feedback'",
    )
    .bind(task)
    .fetch_optional(&st.db)
    .await?;
    Ok(v.map(|s| s >= 0.5))
}

/// The changes of a task were reverted (WP09): the result was not good –
/// for the agent loop and the review that approved it.
pub async fn revert(st: &AppState, task: &Task) {
    let (category, size) = category_of(st, task).await;
    put(st, task, "execute", &category, &size, task.model.as_deref(), 0.0, "revert", 0.0).await;
    let reviewed: Option<String> = sqlx::query_scalar(
        "SELECT model_key FROM chooser_observations WHERE task_id=$1 AND step='review' AND source='run'",
    )
    .bind(task.id)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();
    if let Some(e) = reviewed.and_then(|k| pricing::builtin().entries.iter().find(|e| e.id == k)) {
        let model = e.models.first().cloned();
        put(st, task, "review", &category, &size, model.as_deref(), 0.0, "revert", 0.0).await;
    }
}
