//! Scheduler: per-user queues, configurable parallelism per user and per
//! project (default 1 each; every task has its own working tree, WP04),
//! the per-project merge queue, provider pauses on limits, and OnEmptyQueue
//! maintenance runs.
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    budget,
    models::{status, Settings, Task},
    queue::{self, NewTask},
    runner,
    state::AppState,
};

/// Note for the next agent after an app restart interrupted its run.
pub const RESTART_NOTE: &str = "(Note: a previous run was interrupted by a server restart; the workspace may contain partial changes.)";

/// Tasks that were running when the app stopped: re-queued as `infra`
/// failure (counts as a transient retry, never as a fix attempt); blocked
/// once the retries are exhausted.
pub async fn recover(st: &AppState) -> Result<()> {
    // The runner may outlive the app: stop orphaned runs of these tasks, or
    // they keep changing the workspace and every retry is refused with 409
    // until they end (best effort, the runner may be down as well).
    let orphans: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, project_id FROM tasks WHERE status='running' OR (status='merging' AND merge_state='merging')",
    )
    .fetch_all(&st.db)
    .await?;
    let mut clients = vec![];
    for (id, project) in &orphans {
        clients.push((*id, st.runner(*project).await));
    }
    let cancels = clients.iter().flat_map(|(id, client)| {
        ["agent", "review", "verify"].map(|l| {
            let name = runner::run_name(*id, l);
            async move { client.cancel(&name).await }
        })
    });
    if tokio::time::timeout(Duration::from_secs(10), futures::future::join_all(cancels))
        .await
        .is_err()
    {
        tracing::warn!("cancelling orphaned runs timed out");
    }
    let blocked: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "UPDATE tasks SET status='blocked', failure_class='infra', transient_retries=transient_retries+1,
                error=$2, finished_at=now(), updated_at=now()
         WHERE status='running' AND transient_retries + 1 > $1
         RETURNING id, user_id, title",
    )
    .bind(st.cfg.transient_max_retries)
    .bind(format!(
        "Nach {} Wiederholungen aufgegeben: Lauf durch Server-Neustart unterbrochen.",
        st.cfg.transient_max_retries
    ))
    .fetch_all(&st.db)
    .await?;
    for (id, user, title) in &blocked {
        crate::notify::notify(
            st,
            *user,
            crate::notify::Note {
                kind: "blocked",
                title: format!("Blockiert: {title}"),
                body: "Zu viele unterbrochene Läufe (Server-Neustart).".into(),
                task_id: Some(*id),
                push: true,
            },
        )
        .await;
    }
    let n = sqlx::query(
        "UPDATE tasks SET status='queued', started_at=NULL, updated_at=now(), failure_class='infra',
                transient_retries=transient_retries+1, not_before=now(),
                context = CASE WHEN position($1 in context) > 0 THEN context ELSE context || $1 END
         WHERE status='running'",
    )
    .bind(format!("\n\n{RESTART_NOTE}"))
    .execute(&st.db)
    .await?
    .rows_affected();
    // Interrupted merges go back into the merge queue (nothing was merged
    // unless the fast-forward happened; a repeated merge of a merged task
    // is detected by its merge state).
    let merges = sqlx::query(
        "UPDATE tasks SET merge_state='queued', updated_at=now() WHERE status='merging' AND merge_state='merging'",
    )
    .execute(&st.db)
    .await?
    .rows_affected();
    if merges > 0 {
        tracing::info!("re-queued {merges} interrupted merge(s)");
    }
    if n > 0 || !blocked.is_empty() {
        tracing::info!(
            "re-queued {n} interrupted task(s), blocked {}",
            blocked.len()
        );
    }
    Ok(())
}

pub async fn run(st: AppState) {
    let mut last_prune = std::time::Instant::now() - Duration::from_secs(7200);
    loop {
        if let Err(e) = tick(&st).await {
            tracing::error!("scheduler tick failed: {e:#}");
        }
        if last_prune.elapsed() > Duration::from_secs(3600) {
            last_prune = std::time::Instant::now();
            if let Err(e) = budget::prune(&st.db, st.cfg.usage_retention_days).await {
                tracing::warn!("usage prune failed: {e:#}");
            }
            if let Err(e) = runner::merge::sweep(&st).await {
                tracing::warn!("working tree sweep failed: {e:#}");
            }
            // WP09: composer drafts nobody turned into a task.
            if let Err(e) = crate::attachments::sweep_drafts(&st).await {
                tracing::warn!("attachment draft sweep failed: {e:#}");
            }
            // WP15: learning data beyond eight half-lives weighs < 0.4 %.
            if let Err(e) = crate::chooser::store::prune(&st).await {
                tracing::warn!("chooser prune failed: {e:#}");
            }
            // WP11: audit retention (the database refuses entries < 365 days).
            if let Err(e) = crate::audit::prune(&st.db, st.cfg.audit_retention_days).await {
                tracing::warn!("audit prune failed: {e:#}");
            }
            // WP11: expired login flows and old throttle rows.
            if let Err(e) = crate::mfa::sweep(&st.db).await {
                tracing::warn!("auth flow sweep failed: {e:#}");
            }
        }
        tokio::select! {
            _ = st.wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(st.cfg.tick_secs.max(1))) => {}
        }
    }
}

async fn tick(st: &AppState) -> Result<()> {
    // WP06: due automatic plan approvals; messages of tasks that ended.
    if let Err(e) = crate::plan::auto_approve_due(st).await {
        tracing::warn!("automatic plan approval failed: {e:#}");
    }
    if let Err(e) = crate::steer::sweep(st).await {
        tracing::warn!("message sweep failed: {e:#}");
    }
    // WP11: locked accounts start nothing (their tasks stay queued).
    let users: Vec<Settings> = sqlx::query_as(
        "SELECT s.* FROM user_settings s JOIN users u ON u.id = s.user_id WHERE u.disabled_at IS NULL",
    )
    .fetch_all(&st.db)
    .await?;
    for s in users {
        let running: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tasks WHERE user_id=$1 AND status='running'")
                .bind(s.user_id)
                .fetch_one(&st.db)
                .await?;
        // WP11: admin quota – parallelism cap, cost of the last 30 days.
        let quota = crate::instance::quota(&st.db, s.user_id).await?;
        let mut parallelism = s.parallelism.max(1);
        if let Some(cap) = quota.max_parallel {
            parallelism = parallelism.min(cap.max(1));
        }
        if let Some(max) = quota.max_cost_30d_usd {
            if crate::instance::cost_30d(&st.db, s.user_id).await? >= max {
                continue;
            }
        }
        let mut slots = parallelism as i64 - running;
        let mut started = 0;
        let blocked = if slots > 0 {
            Blocked::load(st, s.user_id).await?
        } else {
            Blocked::default()
        };
        while slots > 0 {
            match claim_next(&st.db, s.user_id, &blocked).await? {
                Some(task) => {
                    spawn(st, task).await;
                    slots -= 1;
                    started += 1;
                }
                None => break,
            }
        }
        if running == 0 && started == 0 {
            maybe_onempty(st, &s).await?;
        }
    }
    // WP12: expired live previews.
    if let Err(e) = crate::preview::sweep(st).await {
        tracing::warn!("preview sweep failed: {e:#}");
    }
    // WP12: automatic selections and groups that settled while the app was down.
    if let Err(e) = crate::bon::sweep(st).await {
        tracing::warn!("best-of-n sweep failed: {e:#}");
    }
    while let Some(task) = claim_merge(&st.db).await? {
        let token = CancellationToken::new();
        let gen = st.track(task.id, token.clone()).await;
        queue::emit_task(st, task.id).await;
        tokio::spawn(runner::merge::run_merge(st.clone(), task, token, gen));
    }
    Ok(())
}

/// What the scheduler must not start right now.
#[derive(Default)]
pub struct Blocked {
    /// Legacy credentials paused (limit/auth) or out of budget.
    legacy: Vec<String>,
    /// Harnesses whose token/cost budget is exhausted (applies to connectors too).
    budget: Vec<String>,
    /// Connectors paused by a limit or an auth failure (`connector:<id>`).
    connectors: Vec<String>,
}

impl Blocked {
    /// WP12: would a task on this access be held back right now?
    pub fn blocks(&self, provider: &str, connector: Option<Uuid>) -> bool {
        match connector {
            None => self.legacy.iter().any(|p| p == provider),
            Some(c) => {
                self.budget.iter().any(|p| p == provider)
                    || self.connectors.contains(&crate::connectors::limit_key(c))
            }
        }
    }

    pub async fn load(st: &AppState, user: Uuid) -> Result<Self> {
        let mut b = Blocked::default();
        for p in st.cfg.providers() {
            let s = budget::status(&st.db, user, p).await?;
            if s.blocked {
                b.legacy.push(p.to_string());
            }
            if s.budget_until.is_some() {
                b.budget.push(p.to_string());
            }
        }
        b.connectors = sqlx::query_scalar(
            "SELECT provider FROM provider_limits WHERE user_id=$1 AND provider LIKE 'connector:%'
               AND (paused_reason IS NOT NULL OR limited_until > now())",
        )
        .bind(user)
        .fetch_all(&st.db)
        .await?;
        Ok(b)
    }
}

/// Runnable queued tasks of the user (provider/connector not paused, not in
/// backoff); `$1` user, `$2..$4` [`Blocked`].
const RUNNABLE: &str = "t.user_id = $1 AND t.status = 'queued'
      AND (t.not_before IS NULL OR t.not_before <= now())
      AND CASE WHEN t.connector_id IS NULL THEN NOT (t.provider = ANY($2))
               ELSE NOT (t.provider = ANY($3)) AND NOT (('connector:' || t.connector_id::text) = ANY($4)) END";

/// Slots a project uses: running and merging tasks; the attempts of a
/// Best-of-N group share one slot (WP12: they all start from the same base
/// and never touch the target branch before the selection).
const PROJECT_SLOTS: &str = "(SELECT count(DISTINCT COALESCE(r.attempt_of, r.id)) FROM tasks r
      WHERE r.project_id = t.project_id AND r.status IN ('running','merging'))";

/// WP11: the runner `rn` takes new runs: active (not draining/revoked),
/// reachable, below its capacity (running tasks claimed on it).
const RUNNER_USABLE: &str = "rn.status = 'active' AND rn.offline_since IS NULL
      AND (rn.capacity IS NULL
           OR (SELECT count(*) FROM tasks c WHERE c.runner_id = rn.id AND c.status = 'running') < rn.capacity)";

/// Claims the next runnable task of the user. A project has as many tasks
/// in flight as its parallelism allows – running, or finished and waiting
/// in its merge queue (with the default 1 a task starts on the target
/// branch its predecessor was merged into, as before WP04: no conflicts in
/// serial projects). Race-free even with concurrent claimers (several app
/// instances): per candidate project, in one transaction,
/// 1. the project row is locked (`FOR UPDATE SKIP LOCKED`; locked = another
///    claimer is deciding for this project right now → skip it),
/// 2. a **new statement** (READ COMMITTED: fresh snapshot, taken after the
///    lock, so it sees every claim committed before) counts the running
///    tasks and claims the task (`FOR UPDATE SKIP LOCKED`) only below the
///    limit.
///
/// The lock order project → task matches `DELETE FROM projects` (cascade).
pub async fn claim_next(
    db: &sqlx::PgPool,
    user_id: Uuid,
    blocked: &Blocked,
) -> Result<Option<Task>> {
    let mut skip: Vec<Uuid> = vec![];
    loop {
        let mut tx = db.begin().await?;
        let candidate: Option<(Uuid, Uuid)> = sqlx::query_as(&format!(
            "SELECT t.project_id, p.runner_id FROM tasks t JOIN projects p ON p.id = t.project_id
             JOIN runners rn ON rn.id = p.runner_id
             WHERE {RUNNABLE} AND NOT (t.project_id = ANY($5))
               AND ({PROJECT_SLOTS} < p.parallelism
                    OR (t.attempt_of IS NOT NULL AND EXISTS (SELECT 1 FROM tasks r WHERE r.attempt_of = t.attempt_of AND r.status = 'running')))
               AND {RUNNER_USABLE}
             ORDER BY t.position LIMIT 1"
        ))
        .bind(user_id)
        .bind(&blocked.legacy)
        .bind(&blocked.budget)
        .bind(&blocked.connectors)
        .bind(&skip)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((project, runner)) = candidate else {
            return Ok(None);
        };
        let locked: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM projects WHERE id = $1 AND runner_id = $2 FOR UPDATE SKIP LOCKED",
        )
        .bind(project)
        .bind(runner)
        .fetch_optional(&mut *tx)
        .await?;
        if locked.is_none() {
            skip.push(project);
            continue;
        }
        // WP11: the runner's capacity is shared by its projects: its row is
        // locked (after the project, as everywhere) and the next statement
        // counts with a fresh snapshot.
        sqlx::query("SELECT id FROM runners WHERE id = $1 FOR UPDATE")
            .bind(runner)
            .execute(&mut *tx)
            .await?;
        let task = sqlx::query_as::<_, Task>(&format!(
            "UPDATE tasks SET status='running', started_at=now(), updated_at=now(), error=NULL, not_before=NULL,
                    runner_id=$6
             WHERE id = (
                SELECT t.id FROM tasks t WHERE {RUNNABLE} AND t.project_id = $5
                ORDER BY t.position LIMIT 1
                FOR UPDATE SKIP LOCKED)
               AND ((SELECT count(DISTINCT COALESCE(r.attempt_of, r.id)) FROM tasks r WHERE r.project_id = $5 AND r.status IN ('running','merging'))
                        < (SELECT parallelism FROM projects WHERE id = $5)
                    OR (tasks.attempt_of IS NOT NULL AND EXISTS (SELECT 1 FROM tasks r WHERE r.attempt_of = tasks.attempt_of AND r.status = 'running')))
               AND (SELECT {RUNNER_USABLE} FROM runners rn WHERE rn.id = $6)
             RETURNING *"
        ))
        .bind(user_id)
        .bind(&blocked.legacy)
        .bind(&blocked.budget)
        .bind(&blocked.connectors)
        .bind(project)
        .bind(runner)
        .fetch_optional(&mut *tx)
        .await?;
        match task {
            Some(t) => {
                tx.commit().await?;
                return Ok(Some(t));
            }
            None => skip.push(project),
        }
    }
}

/// Claims the next merge of the merge queues: per project at most one
/// merge at a time, oldest first (same locking scheme as [`claim_next`]).
pub async fn claim_merge(db: &sqlx::PgPool) -> Result<Option<Task>> {
    let mut skip: Vec<Uuid> = vec![];
    loop {
        let mut tx = db.begin().await?;
        let candidate: Option<Uuid> = sqlx::query_scalar(
            "SELECT t.project_id FROM tasks t
             JOIN projects p ON p.id = t.project_id JOIN runners rn ON rn.id = p.runner_id
             WHERE t.status = 'merging' AND t.merge_state = 'queued'
               AND (t.not_before IS NULL OR t.not_before <= now())
               AND rn.status <> 'revoked' AND rn.offline_since IS NULL
               AND NOT (t.project_id = ANY($1))
               AND NOT EXISTS (SELECT 1 FROM tasks m WHERE m.project_id = t.project_id
                               AND m.status = 'merging' AND m.merge_state = 'merging')
             ORDER BY t.merge_queued_at LIMIT 1",
        )
        .bind(&skip)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(project) = candidate else {
            return Ok(None);
        };
        let locked: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM projects WHERE id = $1 FOR UPDATE SKIP LOCKED")
                .bind(project)
                .fetch_optional(&mut *tx)
                .await?;
        if locked.is_none() {
            skip.push(project);
            continue;
        }
        let task = sqlx::query_as::<_, Task>(
            "UPDATE tasks SET merge_state='merging', updated_at=now()
             WHERE id = (
                SELECT t.id FROM tasks t
                WHERE t.project_id = $1 AND t.status = 'merging' AND t.merge_state = 'queued'
                  AND (t.not_before IS NULL OR t.not_before <= now())
                ORDER BY t.merge_queued_at LIMIT 1
                FOR UPDATE SKIP LOCKED)
               AND NOT EXISTS (SELECT 1 FROM tasks m WHERE m.project_id = $1
                               AND m.status = 'merging' AND m.merge_state = 'merging')
             RETURNING *",
        )
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?;
        match task {
            Some(t) => {
                tx.commit().await?;
                return Ok(Some(t));
            }
            None => skip.push(project),
        }
    }
}

async fn spawn(st: &AppState, task: Task) {
    let token = CancellationToken::new();
    let gen = st.track(task.id, token.clone()).await;
    queue::emit_task(st, task.id).await;
    tokio::spawn(runner::run_task(st.clone(), task, token, gen));
}

/// OnEmptyQueue: runs only if the queue is truly idle, nothing waits for the
/// user, the provider has quota left in the current window and the cooldown
/// has passed (default: once per 5h window → uses quota that would expire).
async fn maybe_onempty(st: &AppState, s: &Settings) -> Result<()> {
    if !s.onempty_enabled || s.onempty_actions.is_empty() {
        return Ok(());
    }
    if let Some(last) = s.last_onempty_at {
        if Utc::now() - last < chrono::Duration::minutes(s.onempty_cooldown_minutes.max(5) as i64) {
            return Ok(());
        }
    }
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tasks WHERE user_id=$1
                        AND status IN ('queued','running','waiting_batch','waiting_input','pending_approval','held','merging','comparing','candidate'))",
    )
    .bind(s.user_id)
    .fetch_one(&st.db)
    .await?;
    if busy {
        return Ok(());
    }
    // Round-robin over projects the user can edit that already have some history.
    let project: Option<(Uuid, String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT p.id, p.name, p.routing FROM projects p JOIN project_members m ON m.project_id = p.id
         WHERE m.user_id = $1 AND m.role IN ('owner','editor') AND NOT p.is_eval
           AND EXISTS (SELECT 1 FROM tasks t WHERE t.project_id = p.id AND t.status = 'done')
         ORDER BY (SELECT max(created_at) FROM tasks o WHERE o.project_id = p.id AND o.kind = 'onempty' AND o.user_id = $1) ASC NULLS FIRST
         LIMIT 1",
    )
    .bind(s.user_id)
    .fetch_optional(&st.db)
    .await?;
    let Some((project_id, name, routing)) = project else {
        return Ok(());
    };
    let title = format!("OnEmptyQueue: {}", s.onempty_actions.join(" + "));
    // WP15: the model chooser picks the analysis (batch, no time pressure)
    // and its fallback agent loop among the accesses that are not paused.
    let proj: crate::models::Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(project_id)
        .fetch_one(&st.db)
        .await?;
    let ask = crate::routing::Ask::new("onempty", &title, "", "", 0);
    let choice = crate::chooser::choose(st, s.user_id, &proj, &ask).await;
    let chosen = match choice.as_ref().and_then(|c| c.execute.clone()) {
        Some(t) => match t.connector_id {
            Some(c) => crate::connectors::load_row(st, c).await?.map(|row| {
                (
                    crate::connectors::harness(&row.3, &row.5),
                    Some(c),
                    t.model.clone(),
                )
            }),
            None => t.provider.clone().map(|p| (p, None, t.model.clone())),
        },
        None => None,
    };
    // Access: the routing's analysis (or agent-loop) target, else the default provider.
    let target = routing
        .and_then(|r| serde_json::from_value::<queued_api::Routing>(r).ok())
        .and_then(|r| r.analyze.target.or(r.execute.target));
    let routing_set = target.is_some();
    let (provider, connector_id, model) = match (chosen.clone(), target) {
        (Some(c), _) => c,
        (None, Some(t)) => match t.connector_id {
            Some(c) => match crate::connectors::load_row(st, c).await? {
                Some(row) => {
                    let harness = crate::connectors::harness(&row.3, &row.5);
                    let paused =
                        budget::status(&st.db, s.user_id, &crate::connectors::limit_key(c))
                            .await?
                            .blocked
                            || budget::status(&st.db, s.user_id, &harness)
                                .await?
                                .budget_until
                                .is_some();
                    if paused {
                        return Ok(());
                    }
                    (harness, Some(c), t.model)
                }
                None => return Ok(()),
            },
            None => {
                let p = t.provider.unwrap_or_else(|| s.default_provider.clone());
                if !legacy_ready(st, s.user_id, &p).await? {
                    return Ok(());
                }
                (p, None, t.model)
            }
        },
        (None, None) => {
            if !legacy_ready(st, s.user_id, &s.default_provider).await? {
                return Ok(());
            }
            (s.default_provider.clone(), None, None)
        }
    };

    let (decision_provider, decision_model) = (provider.clone(), model.clone());
    let (decision_source, decision_why) = if routing_set {
        (
            queued_api::DecisionSource::Route,
            "OnEmpty-Analyse laut Projekt-Routing",
        )
    } else {
        (
            queued_api::DecisionSource::Default,
            "OnEmpty-Analyse mit dem Standard-Anbieter",
        )
    };
    sqlx::query("UPDATE user_settings SET last_onempty_at=now() WHERE user_id=$1")
        .bind(s.user_id)
        .execute(&st.db)
        .await?;
    tracing::info!("OnEmptyQueue for user {} on project {name}", s.user_id);
    let task = queue::insert_task(
        st,
        NewTask {
            user_id: s.user_id,
            project_id,
            parent_id: None,
            kind: "onempty",
            title,
            prompt: String::new(),
            context: String::new(),
            provider,
            model,
            status: status::QUEUED,
            attempt: 0,
            depth: 0,
            created_by_agent: true,
            front: false,
            limits: Default::default(),
            connector_id,
            escalation_level: 0,
            allow_test_changes: false,
            carry_from: None,
            pr_id: None,
            plan_mode: false,
            routing_decision: {
                let router = crate::routing::inherited_json(
                    "onempty",
                    &decision_provider,
                    connector_id,
                    decision_model.as_deref(),
                    decision_source,
                    decision_why,
                );
                match (
                    &choice,
                    router.clone().and_then(|v| serde_json::from_value(v).ok()),
                ) {
                    (Some(c), Some(r)) => {
                        // A chosen access that vanished in between counts as
                        // a fallback (the router's access is used).
                        let c = if chosen.is_some() {
                            c.clone()
                        } else {
                            crate::chooser::Choice {
                                execute: None,
                                ..c.clone()
                            }
                        };
                        serde_json::to_value(crate::chooser::decision_of(r, &c)).ok()
                    }
                    _ => router,
                }
            },
        },
    )
    .await?;
    crate::chooser::after_insert(st, &task).await;
    Ok(())
}

/// Legacy provider usable: not paused/out of budget and credentials stored.
async fn legacy_ready(st: &AppState, user: Uuid, provider: &str) -> Result<bool> {
    Ok(!budget::is_blocked(&st.db, user, provider).await?
        && queue::load_credential(st, user, provider).await?.is_some())
}
