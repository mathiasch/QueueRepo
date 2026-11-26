//! Database side of the chooser (WP15): settings, observations (learning
//! data), decisions (statistics, exploration budget), posteriors with decay
//! and the opt-in pooling of learning data.
use std::collections::HashMap;

use queued_api::{
    chooser::{ChooserPrefs, ChooserSettings, MAX_BLOCKED_MODELS},
    Usage,
};
use uuid::Uuid;

use super::{
    bandit::{Counts, POOL_MIN_USERS},
    decide::Stats,
};
use crate::{estimate::Profile, state::AppState};

/// Project settings (absent row = defaults, chooser off).
pub async fn settings(st: &AppState, project: Uuid) -> sqlx::Result<ChooserSettings> {
    let row: Option<(bool, i16, f64, bool, Vec<String>)> = sqlx::query_as(
        "SELECT enabled, quality, explore_budget_usd, classify, blocked_models
         FROM chooser_settings WHERE project_id=$1",
    )
    .bind(project)
    .fetch_optional(&st.db)
    .await?;
    Ok(match row {
        Some((enabled, quality, budget, classify, blocked)) => ChooserSettings {
            enabled,
            quality: u8::try_from(quality.clamp(0, 100)).unwrap_or(50),
            explore_budget_usd: budget,
            classify,
            blocked_models: blocked,
        },
        None => ChooserSettings::default(),
    })
}

/// Normalised list of model names (trimmed, deduplicated, bounded).
pub fn clean_models(v: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for m in v {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        if m.chars().count() > 120 || m.chars().any(char::is_control) {
            return Err("Modellname zu lang oder ungültig (höchstens 120 Zeichen)".into());
        }
        if !out.iter().any(|x| x.eq_ignore_ascii_case(m)) {
            out.push(m.to_string());
        }
    }
    if out.len() > MAX_BLOCKED_MODELS {
        return Err(format!("Höchstens {MAX_BLOCKED_MODELS} gesperrte Modelle"));
    }
    Ok(out)
}

pub fn validate(s: &ChooserSettings) -> Result<ChooserSettings, String> {
    if s.quality > 100 {
        return Err("Der Regler geht von 0 bis 100".into());
    }
    if !s.explore_budget_usd.is_finite() || !(0.0..=1000.0).contains(&s.explore_budget_usd) {
        return Err("Budget zum Ausprobieren: 0 bis 1000 $".into());
    }
    Ok(ChooserSettings {
        blocked_models: clean_models(&s.blocked_models)?,
        ..s.clone()
    })
}

pub async fn put_settings(st: &AppState, project: Uuid, s: &ChooserSettings) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chooser_settings (project_id, enabled, quality, explore_budget_usd, classify, blocked_models, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,now())
         ON CONFLICT (project_id) DO UPDATE SET enabled=$2, quality=$3, explore_budget_usd=$4,
             classify=$5, blocked_models=$6, updated_at=now()",
    )
    .bind(project)
    .bind(s.enabled)
    .bind(i16::from(s.quality))
    .bind(s.explore_budget_usd)
    .bind(s.classify)
    .bind(&s.blocked_models)
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Instance settings: (pooling, half-life in days).
pub async fn instance(st: &AppState) -> sqlx::Result<(bool, i32)> {
    let r: Option<(bool, i32)> = sqlx::query_as(
        "SELECT chooser_pooling, chooser_half_life_days FROM instance_settings WHERE id=1",
    )
    .fetch_optional(&st.db)
    .await?;
    Ok(r.unwrap_or((false, 60)))
}

pub async fn prefs(st: &AppState, user: Uuid) -> sqlx::Result<ChooserPrefs> {
    let r: Option<(bool, Vec<String>)> = sqlx::query_as(
        "SELECT share_learning, blocked_models FROM chooser_user_prefs WHERE user_id=$1",
    )
    .bind(user)
    .fetch_optional(&st.db)
    .await?;
    let (pooling, _) = instance(st).await?;
    let (share, blocked) = r.unwrap_or_default();
    Ok(ChooserPrefs {
        share_learning: share,
        blocked_models: blocked,
        pooling_enabled: pooling,
    })
}

pub async fn put_prefs(st: &AppState, user: Uuid, p: &ChooserPrefs) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chooser_user_prefs (user_id, share_learning, blocked_models, updated_at)
         VALUES ($1,$2,$3,now())
         ON CONFLICT (user_id) DO UPDATE SET share_learning=$2, blocked_models=$3, updated_at=now()",
    )
    .bind(user)
    .bind(p.share_learning)
    .bind(&p.blocked_models)
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Exploration spent in a project over the last 30 days (USD).
pub async fn explore_spent(st: &AppState, project: Uuid) -> sqlx::Result<f64> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(explore_cost_usd), 0)::DOUBLE PRECISION FROM chooser_decisions
         WHERE project_id=$1 AND explored AND created_at > now() - interval '30 days'",
    )
    .bind(project)
    .fetch_one(&st.db)
    .await
}

/// Learning data of `user` for the given steps and category: own decayed
/// counts, plus – only when the admin enabled pooling – the decayed counts
/// of **other** users who opted in, per cell with at least
/// [`POOL_MIN_USERS`] distinct users.
pub async fn stats(
    st: &AppState,
    user: Uuid,
    categories: &[&str],
    steps: &[&str],
) -> sqlx::Result<Stats> {
    let (pooling, half_life) = instance(st).await?;
    let cats: Vec<String> = categories.iter().map(|s| s.to_string()).collect();
    let steps: Vec<String> = steps.iter().map(|s| s.to_string()).collect();
    let own: Vec<(String, String, String, f64, f64)> = sqlx::query_as(
        "SELECT step, category, model_key,
                SUM(success * weight * power(0.5, EXTRACT(EPOCH FROM now() - created_at) / 86400.0 / $4))::DOUBLE PRECISION,
                SUM((1 - success) * weight * power(0.5, EXTRACT(EPOCH FROM now() - created_at) / 86400.0 / $4))::DOUBLE PRECISION
         FROM chooser_observations
         WHERE user_id=$1 AND category = ANY($2) AND step = ANY($3)
         GROUP BY step, category, model_key",
    )
    .bind(user)
    .bind(&cats)
    .bind(&steps)
    .bind(f64::from(half_life))
    .fetch_all(&st.db)
    .await?;
    let mut out = Stats::default();
    for (step, cat, key, s, f) in own {
        out.cells.entry((step, cat, key)).or_default().0 = Counts {
            successes: s,
            failures: f,
        };
    }
    if pooling {
        let pooled: Vec<(String, String, String, f64, f64)> = sqlx::query_as(
            "SELECT o.step, o.category, o.model_key,
                    SUM(o.success * o.weight * power(0.5, EXTRACT(EPOCH FROM now() - o.created_at) / 86400.0 / $4))::DOUBLE PRECISION,
                    SUM((1 - o.success) * o.weight * power(0.5, EXTRACT(EPOCH FROM now() - o.created_at) / 86400.0 / $4))::DOUBLE PRECISION
             FROM chooser_observations o
             JOIN chooser_user_prefs p ON p.user_id = o.user_id AND p.share_learning
             WHERE o.user_id <> $1 AND o.category = ANY($2) AND o.step = ANY($3)
             GROUP BY o.step, o.category, o.model_key
             HAVING COUNT(DISTINCT o.user_id) >= $5",
        )
        .bind(user)
        .bind(&cats)
        .bind(&steps)
        .bind(f64::from(half_life))
        .bind(POOL_MIN_USERS)
        .fetch_all(&st.db)
        .await?;
        for (step, cat, key, s, f) in pooled {
            out.cells.entry((step, cat, key)).or_default().1 = Some(Counts {
                successes: s,
                failures: f,
            });
        }
    }
    Ok(out)
}

/// Own agent-loop token profiles per price id (finished tasks, ≥ 3 runs).
pub async fn history(st: &AppState, user: Uuid) -> sqlx::Result<HashMap<String, Profile>> {
    let rows: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT l.price_id, SUM(l.input_tokens)::BIGINT, SUM(l.output_tokens)::BIGINT,
                SUM(l.cache_read_tokens)::BIGINT, SUM(l.cache_write_tokens)::BIGINT
         FROM usage_log l JOIN tasks t ON t.id = l.task_id
         WHERE l.user_id=$1 AND l.step='execute' AND l.price_id IS NOT NULL AND t.status='done'
           AND l.ts > now() - interval '35 days'
         GROUP BY l.price_id, l.task_id ORDER BY max(l.ts) DESC LIMIT 2000",
    )
    .bind(user)
    .fetch_all(&st.db)
    .await?;
    let mut by: HashMap<String, Vec<Usage>> = HashMap::new();
    for (p, i, o, cr, cw) in rows {
        by.entry(p).or_default().push(Usage {
            input_tokens: i,
            output_tokens: o,
            cache_read_tokens: cr,
            cache_write_tokens: cw,
            cost_usd: 0.0,
        });
    }
    Ok(by
        .into_iter()
        .filter_map(|(k, v)| Profile::from_history(v).map(|p| (k, p)))
        .collect())
}

/// One chooser decision for statistics and the exploration budget.
pub struct DecisionRow<'a> {
    pub task: Uuid,
    pub user: Uuid,
    pub project: Uuid,
    pub source: &'a str,
    pub category: &'a str,
    pub model_key: Option<&'a str>,
    pub explored: bool,
    pub explore_cost: f64,
    pub expected_cost: f64,
    pub classify_cost: f64,
    pub fallback: Option<&'a str>,
}

pub async fn record_decision(st: &AppState, d: DecisionRow<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chooser_decisions (task_id, user_id, project_id, source, category, model_key, explored,
                                        explore_cost_usd, expected_cost_usd, classify_cost_usd, fallback_reason)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
         ON CONFLICT (task_id) DO UPDATE SET source=$4, model_key=$6, fallback_reason=$11",
    )
    .bind(d.task)
    .bind(d.user)
    .bind(d.project)
    .bind(d.source)
    .bind(d.category)
    .bind(d.model_key)
    .bind(d.explored)
    .bind(d.explore_cost.max(0.0))
    .bind(if d.expected_cost.is_finite() {
        d.expected_cost
    } else {
        0.0
    })
    .bind(d.classify_cost.max(0.0))
    .bind(d.fallback.map(|f| f.chars().take(500).collect::<String>()))
    .execute(&st.db)
    .await?;
    Ok(())
}

/// One learning observation.
pub struct Observation<'a> {
    pub user: Uuid,
    pub project: Option<Uuid>,
    pub task: Option<Uuid>,
    pub step: &'a str,
    pub category: &'a str,
    pub model_key: &'a str,
    pub family: &'a str,
    pub size: &'a str,
    pub success: f64,
    pub weight: f64,
    pub cost: f64,
    pub source: &'a str,
}

/// Records (or replaces: same task, step and source) an observation.
pub async fn observe(st: &AppState, o: Observation<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO chooser_observations (user_id, project_id, task_id, step, category, model_key, family, size,
                                           success, weight, cost_usd, source)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
         ON CONFLICT (task_id, step, source) DO UPDATE SET success=$9, weight=$10, cost_usd=$11, created_at=now()",
    )
    .bind(o.user)
    .bind(o.project)
    .bind(o.task)
    .bind(o.step)
    .bind(o.category)
    .bind(o.model_key)
    .bind(o.family)
    .bind(o.size)
    .bind(o.success.clamp(0.0, 1.0) as f32)
    .bind(o.weight.clamp(0.01, 5.0) as f32)
    .bind(if o.cost.is_finite() { o.cost.max(0.0) } else { 0.0 })
    .bind(o.source)
    .execute(&st.db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_lists_are_cleaned() {
        let v = vec![" haiku ".to_string(), "HAIKU".into(), "".into(), "opus".into()];
        assert_eq!(clean_models(&v).unwrap(), vec!["haiku", "opus"]);
        assert!(clean_models(&["x".repeat(121)]).is_err());
        assert!(clean_models(&(0..51).map(|i| format!("m{i}")).collect::<Vec<_>>()).is_err());
        let s = ChooserSettings {
            explore_budget_usd: -1.0,
            ..Default::default()
        };
        assert!(validate(&s).is_err());
        let s = ChooserSettings {
            quality: 101,
            ..Default::default()
        };
        assert!(validate(&s).is_err());
    }
}
