//! Scheduler: per-user queues, configurable parallelism (default 1), one
//! running task per project (shared workspace), provider pauses on limits,
//! and OnEmptyQueue maintenance runs.
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
    let orphans: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM tasks WHERE status='running'")
        .fetch_all(&st.db)
        .await?;
    let cancels = orphans.iter().flat_map(|id| {
        ["agent", "review", "verify"].map(|l| {
            let name = runner::run_name(*id, l);
            async move { st.runner.cancel(&name).await }
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
            if let Err(e) = budget::prune(&st.db).await {
                tracing::warn!("usage prune failed: {e:#}");
            }
        }
        tokio::select! {
            _ = st.wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(st.cfg.tick_secs.max(1))) => {}
        }
    }
}

async fn tick(st: &AppState) -> Result<()> {
    let users: Vec<Settings> = sqlx::query_as("SELECT * FROM user_settings")
        .fetch_all(&st.db)
        .await?;
    for s in users {
        let running: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tasks WHERE user_id=$1 AND status='running'")
                .bind(s.user_id)
                .fetch_one(&st.db)
                .await?;
        let mut slots = s.parallelism.max(1) as i64 - running;
        let mut started = 0;
        let blocked = if slots > 0 {
            Blocked::load(st, s.user_id).await?
        } else {
            Blocked::default()
        };
        while slots > 0 {
            match claim_next(st, s.user_id, &blocked).await? {
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
    Ok(())
}

/// What the scheduler must not start right now.
#[derive(Default)]
struct Blocked {
    /// Legacy credentials paused (limit/auth) or out of budget.
    legacy: Vec<String>,
    /// Harnesses whose token/cost budget is exhausted (applies to connectors too).
    budget: Vec<String>,
    /// Connectors paused by a limit or an auth failure (`connector:<id>`).
    connectors: Vec<String>,
}

impl Blocked {
    async fn load(st: &AppState, user: Uuid) -> Result<Self> {
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

/// Claims the next runnable task. Legacy tasks: provider paused (limit,
/// auth) or out of budget; connector tasks: connector paused or the
/// harness out of budget. Tasks in backoff (`not_before` in the future) are skipped.
async fn claim_next(st: &AppState, user_id: Uuid, blocked: &Blocked) -> Result<Option<Task>> {
    Ok(sqlx::query_as::<_, Task>(
        "UPDATE tasks SET status='running', started_at=now(), updated_at=now(), error=NULL, not_before=NULL
         WHERE id = (
            SELECT t.id FROM tasks t
            WHERE t.user_id = $1 AND t.status = 'queued'
              AND (t.not_before IS NULL OR t.not_before <= now())
              AND NOT EXISTS (SELECT 1 FROM tasks r WHERE r.project_id = t.project_id AND r.status = 'running')
              AND CASE WHEN t.connector_id IS NULL THEN NOT (t.provider = ANY($2))
                       ELSE NOT (t.provider = ANY($3)) AND NOT (('connector:' || t.connector_id::text) = ANY($4)) END
            ORDER BY t.position
            LIMIT 1
            FOR UPDATE SKIP LOCKED)
         RETURNING *",
    )
    .bind(user_id)
    .bind(&blocked.legacy)
    .bind(&blocked.budget)
    .bind(&blocked.connectors)
    .fetch_optional(&st.db)
    .await?)
}

async fn spawn(st: &AppState, task: Task) {
    let token = CancellationToken::new();
    st.running.lock().await.insert(task.id, token.clone());
    queue::emit_task(st, task.id).await;
    tokio::spawn(runner::run_task(st.clone(), task, token));
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
                        AND status IN ('queued','running','waiting_batch','waiting_input','pending_approval','held'))",
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
    // Access: the routing's analysis (or agent-loop) target, else the default provider.
    let target = routing
        .and_then(|r| serde_json::from_value::<queued_api::Routing>(r).ok())
        .and_then(|r| r.analyze.target.or(r.execute.target));
    let (provider, connector_id, model) = match target {
        Some(t) => match t.connector_id {
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
        None => {
            if !legacy_ready(st, s.user_id, &s.default_provider).await? {
                return Ok(());
            }
            (s.default_provider.clone(), None, None)
        }
    };

    sqlx::query("UPDATE user_settings SET last_onempty_at=now() WHERE user_id=$1")
        .bind(s.user_id)
        .execute(&st.db)
        .await?;
    tracing::info!("OnEmptyQueue for user {} on project {name}", s.user_id);
    queue::insert_task(
        st,
        NewTask {
            user_id: s.user_id,
            project_id,
            parent_id: None,
            kind: "onempty",
            title: format!("OnEmptyQueue: {}", s.onempty_actions.join(" + ")),
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
        },
    )
    .await?;
    Ok(())
}

/// Legacy provider usable: not paused/out of budget and credentials stored.
async fn legacy_ready(st: &AppState, user: Uuid, provider: &str) -> Result<bool> {
    Ok(!budget::is_blocked(&st.db, user, provider).await?
        && queue::load_credential(st, user, provider).await?.is_some())
}
