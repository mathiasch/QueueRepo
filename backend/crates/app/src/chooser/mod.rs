//! WP15 model chooser: picks access and model per task and step from the
//! user's own accesses – minimal expected cost per **accepted** task under
//! hard constraints (data residency, pauses/budgets, project and user
//! rules, capabilities, context length, cost limit), learning from the
//! outcomes (contextual bandit), explainable and overridable.
//!
//! The chooser never blocks a task: any error or timeout falls back to the
//! preset/route decision of [`crate::routing::decide`], with the reason.
pub mod bandit;
pub mod classify;
pub mod decide;
pub mod features;
pub mod learn;
pub mod model;
pub mod replay;
pub mod store;

use std::time::Duration;

use queued_api::{
    chooser::{ChooserReport, TaskFeatures},
    ExecuteRoute, ModelTier, Routing, RoutingDecision, StepCost, StepRoute, Target,
};
use uuid::Uuid;

use crate::{
    models::{Project, Task},
    pricing,
    routing::{self, Access, Ask},
    state::AppState,
};

/// Time the decision may take without/with the classification call.
pub const TIMEOUT: Duration = Duration::from_secs(3);
pub const TIMEOUT_CLASSIFY: Duration = Duration::from_secs(10);

/// Result of [`choose`].
#[derive(Debug, Clone)]
pub struct Choice {
    pub report: ChooserReport,
    pub execute: Option<Target>,
}

/// Candidates: the user's usable accesses × the current catalog models of
/// the families their harness can run, with the pause/budget state.
pub async fn candidates(
    st: &AppState,
    user: Uuid,
    project: Uuid,
) -> anyhow::Result<Vec<decide::Cand>> {
    let accesses = routing::accesses(st, user, project).await?;
    let blocked = crate::worker::Blocked::load(st, user).await?;
    let cat = pricing::current(st).await;
    Ok(candidates_from(&accesses, &cat, |a| {
        blocked.blocks(&a.harness, a.connector_id)
    }))
}

/// Pure part of [`candidates`].
pub fn candidates_from(
    accesses: &[Access],
    cat: &queued_api::PriceCatalog,
    paused: impl Fn(&Access) -> bool,
) -> Vec<decide::Cand> {
    let now = chrono::Utc::now();
    let mut out = vec![];
    for a in accesses {
        let p = paused(a);
        for fam in model::harness_families(a) {
            for e in &cat.entries {
                let Some(alias) = e.models.first() else {
                    continue;
                };
                // Current line only (announced price changes have two lines).
                if e.family != fam
                    || pricing::find(cat, alias, now).map(|x| x.id.as_str()) != Some(e.id.as_str())
                {
                    continue;
                }
                out.push(decide::Cand {
                    access: a.clone(),
                    model: alias.clone(),
                    price: e.clone(),
                    caps: model::caps(&e.id, &e.family),
                    paused: p,
                });
            }
        }
    }
    out
}

/// Seed of a new decision: fixed in tests (`CHOOSER_SEED`), else random;
/// stored in the report, so every decision can be replayed.
fn new_seed(st: &AppState) -> u64 {
    st.cfg
        .chooser_seed
        .unwrap_or_else(|| bandit::seed_of(Uuid::new_v4()))
}

/// The chooser's decision for a new task (`None` = chooser off in the
/// project). Never fails: errors and timeouts become a fallback report.
pub async fn choose(
    st: &AppState,
    user: Uuid,
    project: &Project,
    ask: &Ask,
) -> Option<Choice> {
    let settings = match store::settings(st, project.id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("chooser settings of {} not loaded: {e}", project.id);
            return None;
        }
    };
    if !settings.enabled {
        return None;
    }
    let heuristic = features_of(st, project, ask).await;
    let limit = if settings.classify {
        TIMEOUT_CLASSIFY
    } else {
        TIMEOUT
    };
    let fut = choose_inner(st, user, project, ask, &settings, heuristic.clone());
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(c)) => Some(c),
        Ok(Err(e)) => {
            tracing::warn!("chooser failed for project {}: {e:#}", project.id);
            Some(failed(
                heuristic,
                settings.quality,
                "Fehler in der automatischen Modellwahl",
            ))
        }
        Err(_) => Some(failed(
            heuristic,
            settings.quality,
            "Zeitüberschreitung der automatischen Modellwahl",
        )),
    }
}

fn failed(features: TaskFeatures, quality: u8, why: &str) -> Choice {
    let why = format!("{why} – Vorlage/Projekt-Routing gilt");
    Choice {
        report: ChooserReport {
            features,
            quality,
            fallback: Some(why.clone()),
            reasons: vec![why],
            ..Default::default()
        },
        execute: None,
    }
}

async fn features_of(st: &AppState, project: &Project, ask: &Ask) -> TaskFeatures {
    let knowledge = crate::knowledge::prefix(st, project.id).await;
    features::analyze(&features::Input {
        kind: &ask.facts.kind,
        title: &ask.title,
        prompt: &ask.prompt,
        context: &ask.context,
        attachments: &ask.attachment_mimes,
        attachment_bytes: ask.attachment_bytes,
        urgent: ask.urgent,
        project_text: &knowledge,
    })
}

async fn choose_inner(
    st: &AppState,
    user: Uuid,
    project: &Project,
    ask: &Ask,
    settings: &queued_api::chooser::ChooserSettings,
    mut feats: TaskFeatures,
) -> anyhow::Result<Choice> {
    let cands = candidates(st, user, project.id).await?;
    let prefs = store::prefs(st, user).await?;
    let residency = project.residency();
    let mut classify_cost: Option<(StepCost, Uuid)> = None;
    if settings.classify && ask.facts.kind != "onempty" {
        match classify_call(st, user, project, ask, &cands, settings, &prefs).await {
            Ok(Some((answer, cost, connector))) => {
                match answer {
                    Some(f) => {
                        let images = ask.attachment_mimes.iter().any(|m| m.starts_with("image/"));
                        let model = cost.model.clone().unwrap_or_default();
                        feats = classify::merge(feats, &f, &model, images);
                    }
                    None => feats
                        .signals
                        .push("Antwort der Einordnung nicht verwertbar – nur Heuristik".into()),
                }
                // Paid either way: booked on the task.
                classify_cost = Some((cost, connector));
            }
            Ok(None) => feats.signals.push(
                "Einordnung per Modell nicht möglich (kein günstiger Zugang) – nur Heuristik"
                    .into(),
            ),
            Err(why) => feats
                .signals
                .push(format!("Einordnung per Modell fehlgeschlagen ({why}) – nur Heuristik")),
        }
    }
    let stats = store::stats(
        st,
        user,
        &[feats.category.as_str(), "analysis"],
        &queued_api::chooser::CHOOSER_STEPS,
    )
    .await?;
    let history = store::history(st, user).await?;
    let spent = store::explore_spent(st, project.id).await?;
    let user_settings = crate::queue::load_settings(st, user).await?;
    let cat = pricing::current(st).await;
    let input = decide::Input {
        features: &feats,
        kind: &ask.facts.kind,
        cands,
        residency,
        project_blocked: settings.blocked_models.clone(),
        user_blocked: prefs.blocked_models.clone(),
        cost_limit: ask.cost_limit.or(project.max_cost_usd),
        quality: settings.quality,
        explore_left: (settings.explore_budget_usd - spent).max(0.0),
        max_fix: u32::try_from(user_settings.max_fix_attempts.max(0)).unwrap_or(0),
        history,
        stats: &stats,
        catalog: &cat,
        seed: new_seed(st),
    };
    let out = decide::decide(&input);
    let mut report = out.report;
    if let Some((c, connector)) = classify_cost {
        report.classify_cost = c.amount;
        report.reasons.push(format!(
            "Einordnung durch {} kostete ca. {:.5} $",
            c.model.as_deref().unwrap_or("ein günstiges Modell"),
            c.amount
        ));
        report.classify_step = Some(c);
        report.classify_connector = Some(connector);
    }
    Ok(Choice {
        report,
        execute: out.execute,
    })
}

/// The cheapest single-shot model allowed for the classification (tier
/// `cheap`, residency, not paused, not blocked).
pub fn classify_target<'a>(
    cands: &'a [decide::Cand],
    residency: queued_api::Residency,
    blocked: &[String],
) -> Option<&'a decide::Cand> {
    cands
        .iter()
        .filter(|c| {
            c.access.single_shot
                && c.access.connector_id.is_some()
                && c.price.tier == ModelTier::Cheap
                && residency.allows(c.access.location)
                && !c.paused
                && !blocked.iter().any(|b| {
                    pricing::normalize_model(b) == pricing::normalize_model(&c.model)
                })
        })
        .min_by(|a, b| a.price.input.total_cmp(&b.price.input))
}

async fn classify_call(
    st: &AppState,
    user: Uuid,
    project: &Project,
    ask: &Ask,
    cands: &[decide::Cand],
    settings: &queued_api::chooser::ChooserSettings,
    prefs: &queued_api::chooser::ChooserPrefs,
) -> Result<Option<(Option<classify::Answer>, StepCost, Uuid)>, String> {
    let blocked: Vec<String> = settings
        .blocked_models
        .iter()
        .chain(&prefs.blocked_models)
        .cloned()
        .collect();
    let Some(c) = classify_target(cands, project.residency(), &blocked) else {
        return Ok(None);
    };
    let Some(cid) = c.access.connector_id else {
        return Ok(None);
    };
    let conn = crate::connectors::load_for(st, cid, user, Some(project.id))
        .await
        .map_err(|e| format!("{e}"))?
        .ok_or("Zugang nicht mehr verfügbar")?;
    let Some((wire, base)) = conn.wire() else {
        return Ok(None);
    };
    let ep = crate::llm::Endpoint {
        wire,
        base,
        kind: conn.kind.clone(),
        secret: conn.secret.clone(),
        guard: st.cfg.llm_guard(),
    };
    let req = crate::llm::Request {
        model: c.model.clone(),
        system: classify::SYSTEM.into(),
        user: classify::request(&ask.title, &ask.prompt, &crate::prompt::nonce()),
        max_tokens: classify::MAX_TOKENS,
        cache_long: false,
    };
    let http = crate::llm::client(&ep.guard);
    let resp = crate::llm::call(&http, &ep, &req)
        .await
        .map_err(|e| queued_common::secrets::redact(&e.message))?;
    let cat = pricing::current(st).await;
    let ctx = pricing::PriceCtx {
        tier: pricing::Tier::Standard,
        region_mult: conn
            .region_rule()
            .and_then(|id| pricing::region(&cat, id))
            .map_or(1.0, |r| r.multiplier),
        long_ttl: false,
    };
    let mut m = pricing::Meter::new(
        &cat,
        Some(&c.model),
        ctx,
        "classify",
        Some(conn.label.clone()),
    );
    m.on_delta(&resp.usage);
    let cost = m.finish(&resp.usage);
    Ok(Some((classify::parse(&resp.text), cost, conn.id)))
}

/// Builds the stored decision from a chooser result and the router's own
/// decision (the fallback): `Chooser` with the chosen target, or the
/// router's decision with the report attached.
pub fn decision_of(router: RoutingDecision, c: &Choice) -> RoutingDecision {
    let mut report = c.report.clone();
    match (&c.execute, &report.chosen) {
        (Some(t), Some(chosen)) => {
            let reason = format!(
                "Automatische Modellwahl: {} – ca. {:.2} $ pro erledigtem Auftrag, Erfolgschance ca. {:.0} %{}",
                chosen.model,
                chosen.cost_per_accepted,
                chosen.p_success * 100.0,
                if report.explored {
                    " (ausprobiert)"
                } else {
                    ""
                }
            );
            report.reasons.dedup();
            RoutingDecision {
                step: "execute".into(),
                source: queued_api::DecisionSource::Chooser,
                target: t.clone(),
                rule: None,
                rule_index: None,
                escalation_level: Some(0),
                reason,
                facts: router.facts,
                chooser: Some(Box::new(report)),
            }
        }
        _ => {
            let why = report
                .fallback
                .clone()
                .unwrap_or_else(|| "Automatische Modellwahl nicht möglich".into());
            RoutingDecision {
                reason: format!("{} ({why})", router.reason),
                chooser: Some(Box::new(report)),
                ..router
            }
        }
    }
}

/// The report of a task's decision, if the chooser decided it.
pub fn report_of(task: &Task) -> Option<ChooserReport> {
    let d = task.routing_decision_parsed()?;
    d.chooser.map(|b| *b)
}

/// Effective routing of a task at run time: the chooser's step choices
/// (plan, review, analysis, flex) over the project routing; without a
/// chooser decision the project routing unchanged.
pub fn routing_for(task: &Task, project: &Project) -> Option<Routing> {
    let base = project.routing();
    let Some(rep) = report_of(task).filter(|r| r.chosen.is_some()) else {
        return base;
    };
    let target = Target {
        connector_id: task.connector_id,
        provider: task.connector_id.is_none().then(|| task.provider.clone()),
        model: task.model.clone(),
    };
    let step = |c: &Option<queued_api::chooser::StepChoice>| {
        c.as_ref().map(|s| StepRoute {
            mode: s.mode,
            target: Some(s.target.clone()),
        })
    };
    let b = base.unwrap_or_default();
    Some(Routing {
        preset: b.preset.clone(),
        plan: step(&rep.plan).unwrap_or_else(StepRoute::off),
        execute: ExecuteRoute {
            target: Some(target),
            escalation: vec![],
            flex: rep.flex,
        },
        review: step(&rep.review).unwrap_or(b.review),
        analyze: step(&rep.analyze).unwrap_or(b.analyze),
        rules: b.rules,
    })
}

/// Next stage of the chooser's escalation chain for a fix of `failed`
/// (`root` = the original task carrying the report). `None` = no chain
/// (or exhausted).
pub fn fix_decision(failed: &Task, root: &Task, ask: &Ask) -> Option<RoutingDecision> {
    let rep = report_of(failed).or_else(|| report_of(root))?;
    rep.chosen.as_ref()?;
    let level = usize::try_from(failed.escalation_level.max(0)).ok()?;
    let next = rep.chain.get(level)?;
    let from = failed.model.clone().unwrap_or_else(|| "Standardmodell".into());
    Some(RoutingDecision {
        step: "execute".into(),
        source: queued_api::DecisionSource::Escalation,
        target: next.target.clone(),
        rule: None,
        rule_index: None,
        escalation_level: Some(i32::try_from(level + 1).unwrap_or(i32::MAX)),
        reason: format!(
            "Eskalation laut automatischer Modellwahl (Stufe {}): {from} → {} (Erfolgschance ca. {:.0} %)",
            level + 1,
            next.model,
            next.p_success * 100.0
        ),
        facts: ask.facts.clone(),
        chooser: Some(Box::new(rep)),
    })
}

/// After the task row exists: books the classification cost (step
/// `classify`) and the decision row (statistics, exploration budget).
/// Errors are logged – the task is already queued.
pub async fn after_insert(st: &AppState, task: &Task) {
    let Some(d) = task.routing_decision_parsed() else {
        return;
    };
    let Some(rep) = d.chooser.as_deref() else {
        return;
    };
    if let (Some(c), Some(cid)) = (&rep.classify_step, rep.classify_connector) {
        let harness = match crate::connectors::load_row(st, cid).await {
            Ok(Some(row)) => crate::connectors::harness(&row.3, &row.5),
            _ => "unknown".to_string(),
        };
        if let Err(e) = crate::runner::record_cost(st, task, &harness, Some(cid), c).await {
            tracing::warn!("task {}: classification cost not booked: {e}", task.id);
        }
    }
    let source = match d.source {
        queued_api::DecisionSource::Chooser => "chooser",
        queued_api::DecisionSource::Override => "override",
        _ if rep.fallback.is_some() => "fallback",
        _ => return,
    };
    let key = task
        .model
        .as_deref()
        .and_then(|m| pricing::find(pricing::builtin(), m, chrono::Utc::now()))
        .map(|e| e.id.clone());
    let r = store::record_decision(
        st,
        store::DecisionRow {
            task: task.id,
            user: task.user_id,
            project: task.project_id,
            source,
            category: &rep.features.category,
            model_key: key.as_deref(),
            explored: rep.explored,
            explore_cost: rep.explore_cost,
            expected_cost: rep.chosen.as_ref().map_or(0.0, |c| c.cost_per_accepted),
            classify_cost: rep.classify_cost,
            fallback: rep.fallback.as_deref(),
        },
    )
    .await;
    if let Err(e) = r {
        tracing::warn!("task {}: chooser decision not recorded: {e}", task.id);
    }
}

/// WP12-N10: a reviewer at least as strong as the executor (tier `need`),
/// preferably another family; `None` = no such access (the caller falls
/// back to the executor's own access and model).
pub async fn review_pick(
    st: &AppState,
    task: &Task,
    project: &Project,
    need: ModelTier,
    exec_family: &str,
) -> anyhow::Result<Option<queued_api::chooser::StepChoice>> {
    let cands = candidates(st, task.user_id, project.id).await?;
    let settings = store::settings(st, project.id).await?;
    let prefs = store::prefs(st, task.user_id).await?;
    let cat = pricing::current(st).await;
    let stats = decide::Stats::default();
    let feats = TaskFeatures {
        category: "other".into(),
        size: "M".into(),
        complexity: 2,
        ..Default::default()
    };
    let input = decide::Input {
        features: &feats,
        kind: &task.kind,
        cands,
        residency: project.residency(),
        project_blocked: settings.blocked_models,
        user_blocked: prefs.blocked_models,
        cost_limit: None,
        quality: settings.quality,
        explore_left: 0.0,
        max_fix: 0,
        history: Default::default(),
        stats: &stats,
        catalog: &cat,
        seed: 0,
    };
    Ok(decide::choose_review_for(&input, need, exec_family, None))
}

/// Tier and family of a model name (catalog), if known.
pub fn tier_family(cat: &queued_api::PriceCatalog, model: Option<&str>) -> Option<(ModelTier, String)> {
    let e = pricing::find(cat, model?, chrono::Utc::now())?;
    Some((e.tier, e.family.clone()))
}

/// Whether a step runs as a direct/batch call (not agentic).
pub fn single_shot(m: queued_api::StepMode) -> bool {
    matches!(m, queued_api::StepMode::Sync | queued_api::StepMode::Batch)
}
