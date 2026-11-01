mod agent;
mod api;
mod auth;
mod config;
mod crypto;
mod error;
mod limits;
mod models;
mod notify;
mod oidc;
mod prompt;
mod push;
mod queue;
mod runner;
mod sandbox;
mod state;
mod worker;
mod workspace;

use anyhow::Context;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,queued=debug")))
        .init();

    let cfg = config::Config::from_env().await?;
    let db = PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.database_url)
        .await
        .context("connect to postgres")?;
    sqlx::migrate!("./migrations").run(&db).await.context("run migrations")?;

    let st = state::AppState::new(cfg, db)?;
    worker::recover(&st).await?;
    tokio::spawn(worker::run(st.clone()));

    let listener = tokio::net::TcpListener::bind(&st.cfg.bind).await?;
    tracing::info!("listening on {} (public: {})", st.cfg.bind, st.cfg.public_url);
    axum::serve(listener, api::router(st)).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
