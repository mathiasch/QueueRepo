use std::{collections::HashMap, sync::Arc};

use sqlx::PgPool;
use tokio::sync::{broadcast, Mutex, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{config::Config, push::Vapid, runner_client::RunnerClient};

/// Event fanned out to the SSE stream of one user.
#[derive(Clone, Debug)]
pub struct BusEvent {
    pub user_id: Uuid,
    pub payload: queued_api::ServerEvent,
}

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    pub runner: RunnerClient,
    pub bus: broadcast::Sender<BusEvent>,
    /// Wakes the scheduler immediately (new task, finished task, settings change).
    pub wake: Arc<Notify>,
    /// Wakes the poller of single-shot/batch jobs.
    pub batch_wake: Arc<Notify>,
    /// Wakes the pull request poller (WP05; webhooks of WP07 use it too).
    pub pr_wake: Arc<Notify>,
    /// Wakes the trigger scheduler (WP07: a schedule was changed).
    pub trigger_wake: Arc<Notify>,
    /// Cancellation handles of running tasks (with the generation of the run
    /// that registered it, see [`AppState::track`]).
    pub running: Arc<Mutex<HashMap<Uuid, (u64, CancellationToken)>>>,
    run_gen: Arc<std::sync::atomic::AtomicU64>,
    /// WP06 "Sofort zustellen": a token per agent run of a task that stops
    /// only that run (the task continues with the user's messages).
    steers: Arc<std::sync::Mutex<HashMap<Uuid, CancellationToken>>>,
    pub vapid: Arc<Vapid>,
    /// Effective price catalog, cached per override version (see `pricing::current`).
    pub prices: Arc<std::sync::RwLock<Option<PriceCache>>>,
    /// What the runner enforces (`GET /v1/info`), cached (see `environment::sandbox`).
    pub sandbox: Arc<std::sync::RwLock<Option<(std::time::Instant, queued_api::env::SandboxInfo)>>>,
}

pub type PriceCache = (Option<i32>, Arc<queued_api::PriceCatalog>);

impl AppState {
    pub fn new(cfg: Config, db: PgPool) -> anyhow::Result<Self> {
        let vapid = Vapid::load_or_create(&cfg.data_dir)?;
        let (bus, _) = broadcast::channel(1024);
        Ok(Self {
            db,
            runner: RunnerClient::new(&cfg.runner_url, &cfg.runner_token),
            cfg: Arc::new(cfg),
            http: reqwest::Client::builder()
                .user_agent("queued/0.2")
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            bus,
            wake: Arc::new(Notify::new()),
            batch_wake: Arc::new(Notify::new()),
            pr_wake: Arc::new(Notify::new()),
            trigger_wake: Arc::new(Notify::new()),
            running: Arc::new(Mutex::new(HashMap::new())),
            run_gen: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            steers: Arc::new(std::sync::Mutex::new(HashMap::new())),
            vapid: Arc::new(vapid),
            prices: Arc::new(std::sync::RwLock::new(None)),
            sandbox: Arc::new(std::sync::RwLock::new(None)),
        })
    }

    /// Registers the cancel token of a new run of `id`; returns the run's
    /// generation for [`AppState::untrack`].
    pub async fn track(&self, id: Uuid, token: CancellationToken) -> u64 {
        let gen = self
            .run_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.running.lock().await.insert(id, (gen, token));
        gen
    }

    /// Forgets the token of run `gen` – never the token of a newer run of
    /// the same task (a retry claimed while the previous run was still
    /// cleaning up; before, its token was removed and a cancel was lost).
    pub async fn untrack(&self, id: Uuid, gen: u64) {
        let mut m = self.running.lock().await;
        if m.get(&id).is_some_and(|(g, _)| *g == gen) {
            m.remove(&id);
        }
    }

    /// Cancels the current run of `id`; `false` = no live run registered.
    /// An already cancelled token belongs to a previous run that is still
    /// cleaning up (a retry may already be claimed): it does not count.
    pub async fn cancel_run(&self, id: Uuid) -> bool {
        match self.running.lock().await.get(&id) {
            Some((_, tok)) if !tok.is_cancelled() => {
                tok.cancel();
                true
            }
            _ => false,
        }
    }

    /// A fresh steering token for the agent run of `id` that starts now.
    pub fn steer_arm(&self, id: Uuid) -> CancellationToken {
        let t = CancellationToken::new();
        self.steers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, t.clone());
        t
    }

    /// The agent run of `id` ended (verify/review/merge cannot be steered).
    pub fn steer_disarm(&self, id: Uuid) {
        self.steers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    /// Interrupts the running agent of `id`; `false` = no agent runs now.
    pub fn steer_now(&self, id: Uuid) -> bool {
        match self
            .steers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            Some(t) if !t.is_cancelled() => {
                t.cancel();
                true
            }
            _ => false,
        }
    }

    pub fn emit(&self, user_id: Uuid, payload: queued_api::ServerEvent) {
        let _ = self.bus.send(BusEvent { user_id, payload });
    }
}
