//! queued – orchestrator: auth, queues, scheduling, budgets, notifications.
//! Has no host privileges; all file/process work is delegated to queued-runner.
// sqlx row tuples are clearer inline than as type aliases.
#![allow(clippy::type_complexity)]
pub mod agent;
pub mod api;
pub mod auth;
pub mod batch;
pub mod budget;
pub mod config;
pub mod connectors;
pub mod crypto;
pub mod environment;
pub mod error;
pub mod estimate;
pub mod eval;
pub mod gateway;
pub mod githost;
pub mod limits;
pub mod llm;
pub mod models;
pub mod notify;
pub mod oidc;
pub mod pr;
pub mod pricing;
pub mod prompt;
pub mod push;
pub mod queue;
pub mod routing;
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
    batch::recover(st).await?;
    pr::recover(st).await?;
    tokio::spawn(worker::run(st.clone()));
    tokio::spawn(batch::run(st.clone()));
    // 0 = tests drive the pull request poller themselves.
    if st.cfg.pr_poll_secs > 0 {
        tokio::spawn(pr::run(st.clone()));
    }
    Ok(())
}
