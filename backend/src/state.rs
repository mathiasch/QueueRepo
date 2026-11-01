use std::{collections::HashMap, sync::Arc};

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{broadcast, Mutex, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{config::Config, push::Vapid};

/// Event fanned out to the SSE stream of one user.
#[derive(Clone, Debug)]
pub struct BusEvent {
    pub user_id: Uuid,
    pub payload: Value,
}

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    pub bus: broadcast::Sender<BusEvent>,
    /// Wakes the scheduler immediately (new task, finished task, settings change).
    pub wake: Arc<Notify>,
    /// Cancellation handles of running tasks.
    pub running: Arc<Mutex<HashMap<Uuid, CancellationToken>>>,
    pub vapid: Arc<Vapid>,
}

impl AppState {
    pub fn new(cfg: Config, db: PgPool) -> anyhow::Result<Self> {
        let vapid = Vapid::load_or_create(&cfg.data_dir)?;
        let (bus, _) = broadcast::channel(1024);
        Ok(Self {
            db,
            cfg: Arc::new(cfg),
            http: reqwest::Client::builder()
                .user_agent("queued/0.1")
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            bus,
            wake: Arc::new(Notify::new()),
            running: Arc::new(Mutex::new(HashMap::new())),
            vapid: Arc::new(vapid),
        })
    }

    pub fn emit(&self, user_id: Uuid, payload: Value) {
        let _ = self.bus.send(BusEvent { user_id, payload });
    }
}
