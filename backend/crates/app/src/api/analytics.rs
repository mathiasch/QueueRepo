//! WP10: `GET /api/analytics` and `GET /api/tasks/{id}/insights`.
//!
//! Tenant separation: scope `mine` = the caller's own tasks and usage in
//! projects they are a member of *now*; scope `project` = every task of one
//! project, for any member (the same rule as `GET /api/projects/{id}/tasks`,
//! which shows viewers every task with its cost). Pauses are per user and
//! only part of `mine`. Everything is aggregated in SQL over a bounded time
//! window; the filters below are fixed strings (no user input in SQL text).
use std::collections::BTreeMap;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use queued_api::{
    add_money,
    analytics::{
        insight_notes, Analytics, BestOfN, DayRow, Durations, Outcomes, PauseRow, ProjectRow,
        ProviderRow, RunRow, Scope, TaskCostRow, TaskInsights, Tokens, WINDOWS,
    },
    Money,
};
use serde::Deserialize;
use uuid::Uuid;

use super::projects::{require, Role};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    state::AppState,
};

#[derive(Deserialize)]
pub struct Params {
    #[serde(default = "thirty")]
    days: i32,
    #[serde(default)]
    scope: Scope,
    #[serde(default)]
    project_id: Option<Uuid>,
    /// IANA zone of the day buckets (browser zone; default UTC).
    #[serde(default)]
    tz: Option<String>,
}

fn thirty() -> i32 {
    30
}

/// Task filter per scope ($1 = user, $2 = project, both always bound).
const TASKS_MINE: &str = "t.user_id = $1 AND $2::uuid IS NULL
    AND t.project_id IN (SELECT project_id FROM project_members WHERE user_id = $1)";
const TASKS_PROJECT: &str = "t.project_id = $2 AND $1::uuid IS NOT NULL";
/// Usage filter per scope.
const USAGE_MINE: &str = "u.user_id = $1 AND $2::uuid IS NULL
    AND u.project_id IN (SELECT project_id FROM project_members WHERE user_id = $1)";
const USAGE_PROJECT: &str = "u.project_id = $2 AND $1::uuid IS NOT NULL";

fn tokens(i: i64, o: i64, cr: i64, cw: i64) -> Tokens {
    Tokens {
        input: i,
        output: o,
        cache_read: cr,
        cache_write: cw,
    }
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Query(p): Query<Params>,
) -> ApiResult<Json<Analytics>> {
    if !WINDOWS.contains(&p.days) {
        return Err(AppError::bad_request("days must be 7, 30 or 90"));
    }
    let tz = match p.tz.as_deref().filter(|t| !t.trim().is_empty()) {
        None => chrono_tz::UTC,
        Some(t) => crate::notify::parse_tz(t)
            .ok_or_else(|| AppError::bad_request("Unbekannte Zeitzone"))?,
    };
    let project = match p.scope {
        Scope::Mine => None,
        Scope::Project => {
            let id = p
                .project_id
                .ok_or_else(|| AppError::bad_request("project_id required"))?;
            require(&st, id, u.id, Role::Viewer).await?;
            Some(id)
        }
    };
    let to = Utc::now();
    let from = to - Duration::days(p.days as i64);
    Ok(Json(
        compute(&st, u.id, project, from, to, tz, p.days).await?,
    ))
}

/// The dashboard numbers for `user` (scope `mine`) or `project`.
pub async fn compute(
    st: &AppState,
    user: Uuid,
    project: Option<Uuid>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    tz: chrono_tz::Tz,
    days: i32,
) -> sqlx::Result<Analytics> {
    let (tf_any, uf) = match project {
        None => (TASKS_MINE, USAGE_MINE),
        Some(_) => (TASKS_PROJECT, USAGE_PROJECT),
    };
    // WP12: Best-of-N attempts are not tasks of their own in outcomes,
    // durations and counts – the group task counts once; costs stay complete.
    let tf = format!("{tf_any} AND t.kind <> 'attempt'");
    let db = &st.db;

    // Outcomes incl. first pass and repairs (recursive over fix chains).
    let sql = format!(
        "WITH RECURSIVE s AS (
             SELECT t.id, t.kind, t.status FROM tasks t
              WHERE {tf} AND t.finished_at >= $3 AND t.finished_at < $4
                AND t.status IN ('done','failed','blocked','cancelled')),
         fx AS (
             SELECT s.id AS root, c.id, c.status FROM s JOIN tasks c ON c.parent_id = s.id AND c.kind = 'fix'
              WHERE s.kind <> 'fix'
             UNION ALL
             SELECT fx.root, c.id, c.status FROM fx JOIN tasks c ON c.parent_id = fx.id AND c.kind = 'fix')
         SELECT COUNT(*),
                COUNT(*) FILTER (WHERE status='done'),
                COUNT(*) FILTER (WHERE status='failed'),
                COUNT(*) FILTER (WHERE status='blocked'),
                COUNT(*) FILTER (WHERE status='cancelled'),
                COUNT(*) FILTER (WHERE kind<>'fix' AND status IN ('done','failed','blocked')),
                COUNT(*) FILTER (WHERE kind<>'fix' AND status='done'
                                   AND NOT EXISTS (SELECT 1 FROM fx WHERE fx.root = s.id)),
                COUNT(*) FILTER (WHERE kind<>'fix' AND status IN ('done','failed','blocked')
                                   AND (status='done' OR EXISTS (SELECT 1 FROM fx WHERE fx.root = s.id AND fx.status='done'))),
                (SELECT COUNT(DISTINCT id) FROM fx)
           FROM s"
    );
    let o: (i64, i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(&sql)
        .bind(user)
        .bind(project)
        .bind(from)
        .bind(to)
        .fetch_one(db)
        .await?;
    let outcomes = Outcomes {
        finished: o.0,
        done: o.1,
        failed: o.2,
        blocked: o.3,
        cancelled: o.4,
        first_pass_total: o.5,
        first_pass_done: o.6,
        eventual_done: o.7,
        fixes: o.8,
    };

    // Durations of done tasks; agent time from the run records.
    let sql = format!(
        "WITH d AS (
             SELECT t.id, EXTRACT(EPOCH FROM t.finished_at - t.created_at)::DOUBLE PRECISION AS wall,
                    (SELECT SUM(EXTRACT(EPOCH FROM r.ended_at - r.started_at)) FROM task_runs r WHERE r.task_id = t.id)::DOUBLE PRECISION AS agent
               FROM tasks t
              WHERE {tf} AND t.status='done' AND t.finished_at >= $3 AND t.finished_at < $4)
         SELECT COUNT(*),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY wall),
                percentile_cont(0.9) WITHIN GROUP (ORDER BY wall),
                percentile_cont(0.5) WITHIN GROUP (ORDER BY agent) FILTER (WHERE agent IS NOT NULL)
           FROM d"
    );
    let d: (i64, Option<f64>, Option<f64>, Option<f64>) = sqlx::query_as(&sql)
        .bind(user)
        .bind(project)
        .bind(from)
        .bind(to)
        .fetch_one(db)
        .await?;
    let durations = Durations {
        done: d.0,
        to_done_median: d.1,
        to_done_p90: d.2,
        agent_median: d.3,
    };

    // Usage by provider, model and currency (the basis of most tables).
    let sql = format!(
        "SELECT u.provider, u.price_id, u.currency,
                SUM(u.input_tokens)::BIGINT, SUM(u.output_tokens)::BIGINT,
                SUM(u.cache_read_tokens)::BIGINT, SUM(u.cache_write_tokens)::BIGINT,
                SUM(u.cost_amount)::DOUBLE PRECISION
           FROM usage_log u
          WHERE {uf} AND u.ts >= $3 AND u.ts < $4
          GROUP BY 1, 2, 3"
    );
    let rows: Vec<(String, Option<String>, String, i64, i64, i64, i64, f64)> = sqlx::query_as(&sql)
        .bind(user)
        .bind(project)
        .bind(from)
        .bind(to)
        .fetch_all(db)
        .await?;
    let mut total = Tokens::default();
    let mut costs: Vec<Money> = vec![];
    let mut by_provider: BTreeMap<(String, Option<String>), ProviderRow> = BTreeMap::new();
    for (prov, model, cur, i, o, cr, cw, amount) in rows {
        let t = tokens(i, o, cr, cw);
        add_tokens(&mut total, &t);
        add_money(&mut costs, &cur, amount);
        let row = by_provider
            .entry((prov.clone(), model.clone()))
            .or_insert_with(|| ProviderRow {
                provider: prov,
                model,
                tokens: Tokens::default(),
                costs: vec![],
            });
        add_tokens(&mut row.tokens, &t);
        add_money(&mut row.costs, &cur, amount);
    }
    let mut by_provider: Vec<ProviderRow> = by_provider.into_values().collect();
    by_provider.sort_by_key(|r| std::cmp::Reverse(r.tokens.total()));
    let cost_per_done = if outcomes.done > 0 {
        costs
            .iter()
            .map(|m| Money {
                currency: m.currency.clone(),
                amount: m.amount / outcomes.done as f64,
            })
            .collect()
    } else {
        vec![]
    };

    // Per project: finished/done (tasks) and usage.
    let sql = format!(
        "WITH tp AS (
             SELECT t.project_id, COUNT(*) AS finished, COUNT(*) FILTER (WHERE t.status='done') AS done
               FROM tasks t
              WHERE {tf} AND t.finished_at >= $3 AND t.finished_at < $4
                AND t.status IN ('done','failed','blocked','cancelled')
              GROUP BY 1),
         up AS (
             SELECT u.project_id, u.currency,
                    SUM(u.input_tokens)::BIGINT AS i, SUM(u.output_tokens)::BIGINT AS o,
                    SUM(u.cache_read_tokens)::BIGINT AS cr, SUM(u.cache_write_tokens)::BIGINT AS cw,
                    SUM(u.cost_amount)::DOUBLE PRECISION AS amount
               FROM usage_log u
              WHERE {uf} AND u.ts >= $3 AND u.ts < $4 AND u.project_id IS NOT NULL
              GROUP BY 1, 2)
         SELECT p.id, p.name, COALESCE(tp.finished, 0), COALESCE(tp.done, 0), up.currency,
                COALESCE(up.i, 0), COALESCE(up.o, 0), COALESCE(up.cr, 0), COALESCE(up.cw, 0), COALESCE(up.amount, 0)
           FROM projects p
           LEFT JOIN tp ON tp.project_id = p.id
           LEFT JOIN up ON up.project_id = p.id
          WHERE tp.project_id IS NOT NULL OR up.project_id IS NOT NULL"
    );
    let rows: Vec<(
        Uuid,
        String,
        i64,
        i64,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        f64,
    )> = sqlx::query_as(&sql)
        .bind(user)
        .bind(project)
        .bind(from)
        .bind(to)
        .fetch_all(db)
        .await?;
    let mut by_project: BTreeMap<Uuid, ProjectRow> = BTreeMap::new();
    for (id, name, fin, done, cur, i, o, cr, cw, amount) in rows {
        let row = by_project.entry(id).or_insert_with(|| ProjectRow {
            project_id: id,
            name,
            finished: fin,
            done,
            costs: vec![],
            tokens: Tokens::default(),
        });
        if let Some(cur) = cur {
            add_tokens(&mut row.tokens, &tokens(i, o, cr, cw));
            add_money(&mut row.costs, &cur, amount);
        }
    }
    let mut by_project: Vec<ProjectRow> = by_project.into_values().collect();
    by_project.sort_by(|a, b| {
        b.tokens
            .total()
            .cmp(&a.tokens.total())
            .then(b.finished.cmp(&a.finished))
    });

    // The ten most expensive tasks of the window.
    let sql = format!(
        "WITH top AS (
             SELECT COALESCE(a.attempt_of, u.task_id) AS task_id, SUM(u.cost_amount) AS amount,
                    SUM(u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens)::BIGINT AS tok
               FROM usage_log u JOIN tasks a ON a.id = u.task_id
              WHERE {uf} AND u.ts >= $3 AND u.ts < $4 AND u.task_id IS NOT NULL
              GROUP BY 1 ORDER BY 2 DESC, 3 DESC LIMIT 10)
         SELECT t.id, t.title, p.name, t.status, top.tok, u.currency, SUM(u.cost_amount)::DOUBLE PRECISION
           FROM top JOIN tasks t ON t.id = top.task_id JOIN projects p ON p.id = t.project_id
           JOIN tasks m ON m.id = top.task_id OR m.attempt_of = top.task_id
           JOIN usage_log u ON u.task_id = m.id AND u.ts >= $3 AND u.ts < $4
          GROUP BY 1, 2, 3, 4, 5, 6, top.amount
          ORDER BY top.amount DESC, top.tok DESC"
    );
    let rows: Vec<(Uuid, String, String, String, i64, String, f64)> = sqlx::query_as(&sql)
        .bind(user)
        .bind(project)
        .bind(from)
        .bind(to)
        .fetch_all(db)
        .await?;
    let mut top_tasks: Vec<TaskCostRow> = vec![];
    for (id, title, proj, status, tok, cur, amount) in rows {
        match top_tasks.iter_mut().find(|r| r.task_id == id) {
            Some(r) => add_money(&mut r.costs, &cur, amount),
            None => {
                let mut c = vec![];
                add_money(&mut c, &cur, amount);
                top_tasks.push(TaskCostRow {
                    task_id: id,
                    title,
                    project: proj,
                    status,
                    costs: c,
                    tokens: tok,
                });
            }
        }
    }

    // Days (in the viewer's time zone): done/failed tasks and USD cost.
    let sql = format!(
        "WITH days AS (
             SELECT generate_series((($3::timestamptz) AT TIME ZONE $5)::date, (($4::timestamptz) AT TIME ZONE $5)::date, interval '1 day')::date AS day),
         td AS (
             SELECT (t.finished_at AT TIME ZONE $5)::date AS day,
                    COUNT(*) FILTER (WHERE t.status='done') AS done,
                    COUNT(*) FILTER (WHERE t.status='failed') AS failed,
                    COUNT(*) FILTER (WHERE t.status='blocked') AS blocked
               FROM tasks t
              WHERE {tf} AND t.finished_at >= $3 AND t.finished_at < $4
              GROUP BY 1),
         ud AS (
             SELECT (u.ts AT TIME ZONE $5)::date AS day, SUM(u.cost_amount) FILTER (WHERE u.currency='USD') AS usd
               FROM usage_log u
              WHERE {uf} AND u.ts >= $3 AND u.ts < $4
              GROUP BY 1)
         SELECT days.day, COALESCE(td.done, 0), COALESCE(td.failed, 0), COALESCE(td.blocked, 0),
                COALESCE(ud.usd, 0)::DOUBLE PRECISION
           FROM days LEFT JOIN td ON td.day = days.day LEFT JOIN ud ON ud.day = days.day
          ORDER BY days.day"
    );
    let rows: Vec<(NaiveDate, i64, i64, i64, f64)> = sqlx::query_as(&sql)
        .bind(user)
        .bind(project)
        .bind(from)
        .bind(to)
        .bind(tz.name())
        .fetch_all(db)
        .await?;
    let daily = rows
        .into_iter()
        .map(|(day, done, failed, blocked, cost_usd)| DayRow {
            day,
            done,
            failed,
            blocked,
            cost_usd,
        })
        .collect();

    // Pauses (per user; overlapping pauses of one provider count once).
    let pauses = if project.is_none() {
        let rows: Vec<(String, String, i64, f64)> = sqlx::query_as(
            "WITH w AS (
                 SELECT provider, reason, tstzrange(GREATEST(started_at, $2), LEAST(COALESCE(ends_at, $3), $3)) AS r
                   FROM provider_pauses
                  WHERE user_id = $1 AND started_at < $3 AND COALESCE(ends_at, $3) > $2)
             SELECT w.provider, w.reason, COUNT(*),
                    COALESCE((SELECT EXTRACT(EPOCH FROM SUM(upper(x) - lower(x)))
                                FROM unnest(range_agg(w.r)) x), 0)::DOUBLE PRECISION
               FROM w GROUP BY 1, 2 ORDER BY 4 DESC",
        )
        .bind(user)
        .bind(from)
        .bind(to)
        .fetch_all(db)
        .await?;
        let mut out = vec![];
        for (prov, reason, n, secs) in rows {
            out.push(PauseRow {
                provider: provider_label(st, user, &prov).await?,
                reason,
                pauses: n,
                minutes: (secs / 60.0).round() as i64,
            });
        }
        Some(out)
    } else {
        None
    };

    // WP12: Best-of-N in the window (groups created in it).
    let sql = format!(
        "SELECT COUNT(*) FILTER (WHERE t.attempts > 1 AND t.attempt_of IS NULL),
                COUNT(*) FILTER (WHERE t.kind = 'attempt'),
                COUNT(*) FILTER (WHERE t.kind = 'attempt' AND (t.status IN ('candidate','merging','done')
                                       OR (t.status = 'cancelled' AND t.error ILIKE 'verworfen%'))),
                COUNT(*) FILTER (WHERE t.attempts > 1 AND t.bon->'choice'->>'by' = 'user'),
                COUNT(*) FILTER (WHERE t.attempts > 1 AND t.bon->'choice'->>'by' = 'auto'),
                COUNT(*) FILTER (WHERE t.attempts > 1 AND t.bon->>'aborted' IS NOT NULL)
           FROM tasks t
          WHERE {tf_any} AND t.created_at >= $3 AND t.created_at < $4"
    );
    let b: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(&sql)
        .bind(user)
        .bind(project)
        .bind(from)
        .bind(to)
        .fetch_one(db)
        .await?;
    let best_of_n = BestOfN {
        groups: b.0,
        attempts: b.1,
        successful_attempts: b.2,
        selected_by_user: b.3,
        selected_auto: b.4,
        budget_stops: b.5,
    };
    Ok(Analytics {
        best_of_n,
        days,
        scope: if project.is_some() {
            Scope::Project
        } else {
            Scope::Mine
        },
        project_id: project,
        from,
        to,
        outcomes,
        durations,
        costs,
        cost_per_done,
        tokens: total,
        by_project,
        by_provider,
        top_tasks,
        daily,
        pauses,
    })
}

fn add_tokens(a: &mut Tokens, b: &Tokens) {
    a.input = a.input.saturating_add(b.input);
    a.output = a.output.saturating_add(b.output);
    a.cache_read = a.cache_read.saturating_add(b.cache_read);
    a.cache_write = a.cache_write.saturating_add(b.cache_write);
}

/// `connector:<uuid>` → the connector's label (own connectors only).
async fn provider_label(st: &AppState, user: Uuid, key: &str) -> sqlx::Result<String> {
    let Some(id) = key
        .strip_prefix("connector:")
        .and_then(|i| Uuid::parse_str(i).ok())
    else {
        return Ok(key.to_string());
    };
    let label: Option<String> =
        sqlx::query_scalar("SELECT label FROM connectors WHERE id=$1 AND user_id=$2")
            .bind(id)
            .bind(user)
            .fetch_optional(&st.db)
            .await?;
    Ok(label.unwrap_or_else(|| "Gelöschter Zugang".into()))
}

/// `GET /api/tasks/{id}/insights` (owner or project member).
pub async fn insights(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TaskInsights>> {
    let t = crate::queue::load_task(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    if t.user_id != u.id {
        require(&st, t.project_id, u.id, Role::Viewer).await?;
    }
    let runs: Vec<(
        String,
        String,
        Option<String>,
        DateTime<Utc>,
        f64,
        String,
        i64,
        i64,
        i64,
        i64,
        i32,
    )> = sqlx::query_as(
        "SELECT kind, provider, price_id, started_at,
                    EXTRACT(EPOCH FROM ended_at - started_at)::DOUBLE PRECISION, outcome,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, tool_calls
               FROM task_runs WHERE task_id=$1 ORDER BY started_at, id",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    let tools: Vec<(String, i64)> = sqlx::query_as(
        "SELECT k, SUM(v::BIGINT)::BIGINT FROM task_runs, jsonb_each_text(tools) AS e(k, v)
          WHERE task_id=$1 GROUP BY k ORDER BY 2 DESC, 1 LIMIT 8",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    let fixes: i64 = sqlx::query_scalar(
        "WITH RECURSIVE fx AS (
             SELECT id FROM tasks WHERE parent_id=$1 AND kind='fix'
             UNION ALL
             SELECT c.id FROM fx JOIN tasks c ON c.parent_id = fx.id AND c.kind='fix')
         SELECT COUNT(*) FROM fx",
    )
    .bind(id)
    .fetch_one(&st.db)
    .await?;
    let usage: Vec<(String, i64, i64, i64, i64, f64)> = sqlx::query_as(
        "SELECT currency, SUM(input_tokens)::BIGINT, SUM(output_tokens)::BIGINT, SUM(cache_read_tokens)::BIGINT,
                SUM(cache_write_tokens)::BIGINT, SUM(cost_amount)::DOUBLE PRECISION
           FROM usage_log WHERE task_id=$1 GROUP BY 1",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    let mut tok = Tokens::default();
    let mut costs = vec![];
    for (cur, i, o, cr, cw, a) in usage {
        add_tokens(&mut tok, &tokens(i, o, cr, cw));
        add_money(&mut costs, &cur, a);
    }
    // Usage logs are pruned after the retention: fall back to the task total.
    if tok.total() == 0 {
        if let Some(u) = t
            .usage
            .clone()
            .and_then(|v| serde_json::from_value::<queued_api::Usage>(v).ok())
        {
            tok = tokens(
                u.input_tokens,
                u.output_tokens,
                u.cache_read_tokens,
                u.cache_write_tokens,
            );
        }
        if let Some(c) = t
            .cost
            .clone()
            .and_then(|v| serde_json::from_value::<queued_api::TaskCost>(v).ok())
        {
            costs = c.totals;
        }
    }
    let end = t.finished_at.unwrap_or_else(Utc::now);
    let runs: Vec<RunRow> = runs
        .into_iter()
        .map(
            |(kind, provider, model, started_at, secs, outcome, i, o, cr, cw, calls)| RunRow {
                kind,
                provider,
                model,
                started_at,
                seconds: secs,
                outcome,
                tokens: tokens(i, o, cr, cw),
                tool_calls: calls as i64,
            },
        )
        .collect();
    let mut out = TaskInsights {
        task_id: id,
        agent_seconds: runs.iter().map(|r| r.seconds).sum(),
        runs,
        wall_seconds: (end - t.created_at).num_milliseconds().max(0) as f64 / 1000.0,
        tokens: tok,
        costs,
        tools,
        transient_retries: t.transient_retries,
        fixes,
        notes: vec![],
    };
    out.notes = insight_notes(&out);
    Ok(Json(out))
}
