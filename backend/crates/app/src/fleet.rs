//! WP11: runner fleet. The app talks to every runner (push model): the
//! built-in runner from `RUNNER_URL`/`RUNNER_TOKEN` plus runners an admin
//! registered, each with its **own** token (encrypted with `MASTER_KEY`,
//! the app must present it). A runner holds no credential for the app.
//!
//! - Projects are bound to one runner (`projects.runner_id`): their
//!   workspaces live there. New projects go to the least loaded usable runner.
//! - The monitor sends `POST /v1/heartbeat` to every runner; it renews the
//!   leases of the runs the app streams from it. Without a successful
//!   heartbeat for `RUNNER_LEASE_SECS` a runner is *offline*: its open
//!   streams end (the tasks are re-queued as infrastructure failures), the
//!   scheduler starts nothing there, and the runner itself stops the runs
//!   whose lease expired. A retry can only start on the same runner, which
//!   refuses a second process in a workspace: no double run.
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use queued_api::security::{RunnerStatus, RunnerView};
use uuid::Uuid;

use crate::{audit, crypto, runner_client::RunnerClient, state::AppState};

/// Id of the built-in runner row (migration 0014).
pub const BUILTIN: Uuid = Uuid::from_u128(1);

#[derive(Clone)]
struct Entry {
    client: RunnerClient,
    /// URL + token the client was built from (rebuilt when they change).
    key: (String, Vec<u8>),
    status: RunnerStatus,
    last_ok: Instant,
}

/// Clients of all runners and the project → runner cache.
#[derive(Clone)]
pub struct Fleet {
    builtin: RunnerClient,
    runners: Arc<RwLock<HashMap<Uuid, Entry>>>,
    /// Projects never change their runner except by [`move_project`] in
    /// this process, so the cache needs no expiry (ids are never reused).
    projects: Arc<RwLock<HashMap<Uuid, Uuid>>>,
    builtin_last_ok: Arc<RwLock<Instant>>,
    started: Instant,
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

impl Fleet {
    pub fn new(builtin: RunnerClient) -> Self {
        Self {
            builtin,
            runners: Default::default(),
            projects: Default::default(),
            builtin_last_ok: Arc::new(RwLock::new(Instant::now())),
            started: Instant::now(),
        }
    }

    pub fn builtin(&self) -> RunnerClient {
        self.builtin.clone()
    }

    /// Client of a runner (`None` = unknown or revoked).
    pub fn client(&self, id: Uuid) -> Option<RunnerClient> {
        if id == BUILTIN {
            return Some(self.builtin.clone());
        }
        read(&self.runners)
            .get(&id)
            .filter(|e| e.status != RunnerStatus::Revoked)
            .map(|e| e.client.clone())
    }

    fn cached_project(&self, project: Uuid) -> Option<Uuid> {
        read(&self.projects).get(&project).copied()
    }

    fn remember(&self, project: Uuid, runner: Uuid) {
        write(&self.projects).insert(project, runner);
    }
}

impl AppState {
    /// The runner client of a project's workspaces. Never fails: when the
    /// runner cannot be used (revoked, unknown project, database error) the
    /// returned client fails every call with the reason.
    pub async fn runner(&self, project: Uuid) -> RunnerClient {
        match self.runner_id(project).await {
            Ok(rid) => match self.fleet.client(rid) {
                Some(c) => c,
                None => match refresh(self)
                    .await
                    .ok()
                    .and_then(|_| self.fleet.client(rid))
                {
                    Some(c) => c,
                    None => RunnerClient::faulty(
                        "Der Runner dieses Projekts ist widerrufen oder unbekannt.",
                    ),
                },
            },
            Err(e) => RunnerClient::faulty(&format!("runner of project {project}: {e:#}")),
        }
    }

    /// Runner id of a project (cached).
    pub async fn runner_id(&self, project: Uuid) -> Result<Uuid> {
        if let Some(r) = self.fleet.cached_project(project) {
            return Ok(r);
        }
        let r: Uuid = sqlx::query_scalar("SELECT runner_id FROM projects WHERE id=$1")
            .bind(project)
            .fetch_optional(&self.db)
            .await?
            .ok_or_else(|| anyhow!("project not found"))?;
        self.fleet.remember(project, r);
        Ok(r)
    }
}

/// Runner token: like a personal API token, recognizable by secret scanners.
pub fn new_token() -> String {
    format!("qr_{}", crypto::random_token())
}

/// Loads the registered runners from the database and (re)builds clients
/// whose address or token changed.
pub async fn refresh(st: &AppState) -> Result<()> {
    let rows: Vec<(Uuid, Option<String>, Option<Vec<u8>>, String)> =
        sqlx::query_as("SELECT id, url, token_enc, status FROM runners WHERE NOT builtin")
            .fetch_all(&st.db)
            .await?;
    let lease = st.cfg.runner_lease_secs;
    let mut map = write(&st.fleet.runners);
    let mut seen = vec![];
    for (id, url, token_enc, status) in rows {
        seen.push(id);
        let status = RunnerStatus::parse(&status).unwrap_or(RunnerStatus::Revoked);
        let (Some(url), Some(enc)) = (url, token_enc) else {
            if let Some(e) = map.get_mut(&id) {
                e.status = RunnerStatus::Revoked;
                e.client.set_lost(true);
            }
            continue;
        };
        let key = (url.clone(), enc.clone());
        match map.get_mut(&id) {
            Some(e) if e.key == key => e.status = status,
            Some(e) => {
                let token = String::from_utf8(crypto::decrypt(&st.cfg.master_key, &enc)?)?;
                e.client = e.client.renewed(&url, &token);
                e.key = key;
                e.status = status;
            }
            None => {
                let token = String::from_utf8(crypto::decrypt(&st.cfg.master_key, &enc)?)?;
                map.insert(
                    id,
                    Entry {
                        client: RunnerClient::new(&url, &token).with_lease(lease),
                        key,
                        status,
                        last_ok: st.fleet.started,
                    },
                );
            }
        }
    }
    map.retain(|id, _| seen.contains(id));
    Ok(())
}

/// The heartbeat loop (spawned by `start_worker`).
pub async fn run(st: AppState) {
    let every = Duration::from_secs(st.cfg.runner_heartbeat_secs.max(1));
    loop {
        if let Err(e) = tick(&st).await {
            tracing::warn!("runner heartbeat round failed: {e:#}");
        }
        tokio::time::sleep(every).await;
    }
}

/// One heartbeat round over all runners (tests call it directly).
pub async fn tick(st: &AppState) -> Result<()> {
    if let Err(e) = refresh(st).await {
        // Keep the known runners: renewing leases must not depend on the DB.
        tracing::warn!("runner list not refreshed: {e:#}");
    }
    let mut targets: Vec<(Uuid, RunnerClient, Instant)> =
        vec![(BUILTIN, st.fleet.builtin.clone(), Instant::now())];
    for (id, e) in read(&st.fleet.runners).iter() {
        if e.status != RunnerStatus::Revoked {
            targets.push((*id, e.client.clone(), e.last_ok));
        }
    }
    let timeout = Duration::from_secs(st.cfg.runner_heartbeat_secs.clamp(2, 10));
    let lease = st.cfg.runner_lease_secs;
    let beats = targets.into_iter().map(|(id, c, _)| async move {
        let r = c.heartbeat(lease, timeout).await;
        (id, c, r)
    });
    for (id, client, res) in futures::future::join_all(beats).await {
        match res {
            Ok(hb) => {
                if id == BUILTIN {
                    *write(&st.fleet.builtin_last_ok) = Instant::now();
                } else if let Some(e) = write(&st.fleet.runners).get_mut(&id) {
                    e.last_ok = Instant::now();
                }
                let was_lost = client.is_lost();
                client.set_lost(false);
                let came_back: Option<bool> = sqlx::query_scalar(
                    "WITH old AS (SELECT offline_since FROM runners WHERE id=$1)
                     UPDATE runners SET last_seen_at=now(), engine=$2, egress_enforced=$3, version=$4,
                            active_runs=$5, offline_since=NULL
                     WHERE id=$1 RETURNING (SELECT offline_since IS NOT NULL FROM old)",
                )
                .bind(id)
                .bind(&hb.info.engine)
                .bind(hb.info.egress_enforced)
                .bind(&hb.version)
                .bind(hb.active.len() as i32)
                .fetch_optional(&st.db)
                .await
                .unwrap_or(None);
                if was_lost || came_back == Some(true) {
                    tracing::info!("runner {id} is reachable again");
                    audit::system(
                        st,
                        "admin.runner_online",
                        "runner",
                        &id.to_string(),
                        None,
                        serde_json::json!({}),
                    )
                    .await;
                    st.wake.notify_one();
                }
            }
            Err(e) => {
                let last_ok = if id == BUILTIN {
                    *read(&st.fleet.builtin_last_ok)
                } else {
                    read(&st.fleet.runners)
                        .get(&id)
                        .map(|e| e.last_ok)
                        .unwrap_or(st.fleet.started)
                };
                if last_ok.elapsed() < Duration::from_secs(lease) || client.is_lost() {
                    tracing::debug!("runner {id}: heartbeat failed: {e:#}");
                    continue;
                }
                tracing::warn!("runner {id} lost (no heartbeat for {lease} s): {e:#}");
                client.set_lost(true);
                let newly: Option<Uuid> = sqlx::query_scalar(
                    "UPDATE runners SET offline_since=now() WHERE id=$1 AND offline_since IS NULL RETURNING id",
                )
                .bind(id)
                .fetch_optional(&st.db)
                .await
                .unwrap_or(None);
                if newly.is_some() {
                    audit::system(
                        st,
                        "admin.runner_offline",
                        "runner",
                        &id.to_string(),
                        None,
                        serde_json::json!({"lease_secs": lease}),
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

/// Runner for a new project: active, reachable, fewest projects (the
/// built-in runner wins ties, so single-runner setups never change).
pub async fn assign(st: &AppState) -> Result<Uuid> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT r.id FROM runners r
         WHERE r.status = 'active' AND r.offline_since IS NULL
           AND (r.builtin OR r.last_seen_at IS NOT NULL)
         ORDER BY (SELECT count(*) FROM projects p WHERE p.runner_id = r.id), NOT r.builtin, r.created_at
         LIMIT 1",
    )
    .fetch_optional(&st.db)
    .await?;
    id.ok_or_else(|| {
        anyhow!("Kein Runner nimmt gerade neue Projekte an (alle pausiert oder nicht erreichbar).")
    })
}

pub fn view_query() -> &'static str {
    "SELECT r.id, r.name, r.url, r.builtin, r.status, r.capacity, r.offline_since IS NULL, r.last_seen_at,
            r.offline_since, r.engine, r.egress_enforced, r.version, r.active_runs,
            (SELECT count(*) FROM projects p WHERE p.runner_id = r.id),
            (SELECT count(*) FROM tasks t WHERE t.runner_id = r.id AND t.status = 'running')
     FROM runners r"
}

type ViewRow = (
    Uuid,
    String,
    Option<String>,
    bool,
    String,
    Option<i32>,
    bool,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
    String,
    bool,
    String,
    i32,
    i64,
    i64,
);

pub fn view(r: ViewRow, st: &AppState) -> RunnerView {
    let builtin = r.3;
    RunnerView {
        id: r.0,
        name: r.1,
        url: if builtin {
            Some(st.cfg.runner_url.clone())
        } else {
            r.2
        },
        builtin,
        status: RunnerStatus::parse(&r.4).unwrap_or(RunnerStatus::Revoked),
        capacity: r.5,
        online: r.6 && (builtin || r.7.is_some()),
        last_seen_at: r.7,
        offline_since: r.8,
        engine: r.9,
        egress_enforced: r.10,
        version: r.11,
        active_runs: r.12,
        projects: r.13,
        running_tasks: r.14,
    }
}

pub async fn list(st: &AppState) -> Result<Vec<RunnerView>> {
    let rows: Vec<ViewRow> = sqlx::query_as(&format!(
        "{} ORDER BY NOT r.builtin, r.created_at",
        view_query()
    ))
    .fetch_all(&st.db)
    .await?;
    Ok(rows.into_iter().map(|r| view(r, st)).collect())
}

pub async fn get(st: &AppState, id: Uuid) -> Result<Option<RunnerView>> {
    let row: Option<ViewRow> = sqlx::query_as(&format!("{} WHERE r.id = $1", view_query()))
        .bind(id)
        .fetch_optional(&st.db)
        .await?;
    Ok(row.map(|r| view(r, st)))
}

/// Workspaces of a project on its runner: the main repository plus the
/// working trees of its unfinished tasks that exist there.
pub async fn project_workspaces(
    st: &AppState,
    project: Uuid,
    from: &RunnerClient,
) -> Result<Vec<String>> {
    let on_runner = from.workspaces().await?;
    let tasks: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tasks WHERE project_id=$1 AND status NOT IN ('done','failed','cancelled')",
    )
    .bind(project)
    .fetch_all(&st.db)
    .await?;
    let mut out = vec![project.to_string()];
    for t in tasks {
        let ws = crate::runner_client::task_ws(t);
        if on_runner.contains(&ws) {
            out.push(ws);
        }
    }
    if !on_runner.contains(&out[0]) {
        bail!("the project's repository does not exist on its runner");
    }
    Ok(out)
}

/// Copies one workspace from runner `from` to runner `to` (bundle stream).
pub async fn transfer(from: &RunnerClient, to: &RunnerClient, ws: &str) -> Result<u64> {
    let (head, resp) = from
        .bundle(ws)
        .await
        .with_context(|| format!("bundle of {ws}"))?;
    let len = resp.content_length().unwrap_or(0);
    let body = reqwest::Body::wrap_stream(
        resp.bytes_stream()
            .map(|r| r.map_err(std::io::Error::other)),
    );
    to.restore(ws, &head, body)
        .await
        .with_context(|| format!("restore of {ws}"))?;
    Ok(len)
}

/// Moves an idle project to another runner: bundles of its workspaces are
/// restored there, the binding changes, the old copies are removed.
pub async fn move_project(st: &AppState, project: Uuid, target: Uuid) -> Result<(usize, u64)> {
    let mut tx = st.db.begin().await?;
    let (current, busy): (Uuid, bool) = sqlx::query_as(
        "SELECT p.runner_id, EXISTS (SELECT 1 FROM tasks t WHERE t.project_id = p.id
                    AND (t.status IN ('running','merging') OR t.status = 'waiting_batch'))
         FROM projects p WHERE p.id = $1 FOR UPDATE",
    )
    .bind(project)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("project not found"))?;
    if busy {
        bail!("Im Projekt läuft gerade etwas. Warte, bis die Aufträge fertig sind.");
    }
    if current == target {
        bail!("Das Projekt liegt bereits auf diesem Runner.");
    }
    let (status, offline): (String, bool) = sqlx::query_as(
        "SELECT status, offline_since IS NOT NULL OR (NOT builtin AND last_seen_at IS NULL) FROM runners WHERE id=$1",
    )
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow!("runner not found"))?;
    if status != "active" || offline {
        bail!("Der Ziel-Runner nimmt gerade keine Projekte an.");
    }
    refresh(st).await?;
    let from = st.fleet.client(current).ok_or_else(|| {
        anyhow!("Der bisherige Runner ist widerrufen; ein Umzug ist nur aus einem Backup möglich.")
    })?;
    let to = st
        .fleet
        .client(target)
        .ok_or_else(|| anyhow!("runner not found"))?;
    let workspaces = project_workspaces(st, project, &from).await?;
    let mut bytes = 0;
    let mut done: Vec<&String> = vec![];
    for ws in &workspaces {
        match transfer(&from, &to, ws).await {
            Ok(n) => {
                bytes += n;
                done.push(ws);
            }
            Err(e) => {
                // Nothing changes: remove the partial copies again.
                for d in done {
                    let _ = to.delete(d).await;
                }
                return Err(e);
            }
        }
    }
    sqlx::query("UPDATE projects SET runner_id=$2 WHERE id=$1")
        .bind(project)
        .bind(target)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    st.fleet.remember(project, target);
    for ws in workspaces.iter().rev() {
        if let Err(e) = from.delete(ws).await {
            tracing::warn!("old copy {ws} not removed from runner {current}: {e:#}");
        }
    }
    Ok((workspaces.len(), bytes))
}
