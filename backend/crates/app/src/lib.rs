//! queued – orchestrator: auth, queues, scheduling, budgets, notifications.
//! Has no host privileges; all file/process work is delegated to queued-runner.
// sqlx row tuples are clearer inline than as type aliases.
#![allow(clippy::type_complexity)]
pub mod agent;
pub mod api;
pub mod auth;
pub mod budget;
pub mod config;
pub mod crypto;
pub mod error;
pub mod limits;
pub mod models;
pub mod notify;
pub mod oidc;
pub mod prompt;
pub mod push;
pub mod queue;
pub mod runner;
pub mod runner_client;
pub mod state;
pub mod worker;

use anyhow::Context;
use sqlx::postgres::PgPoolOptions;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Connects, migrates and returns the state (scheduler not started).
pub async fn init(cfg: config::Config) -> anyhow::Result<state::AppState> {
    let db = PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.database_url)
        .await
        .context("connect to postgres")?;
    MIGRATOR.run(&db).await.context("run migrations")?;
    state::AppState::new(cfg, db)
}

/// Starts the scheduler loop in the background.
pub async fn start_worker(st: &state::AppState) -> anyhow::Result<()> {
    worker::recover(st).await?;
    tokio::spawn(worker::run(st.clone()));
    Ok(())
}
