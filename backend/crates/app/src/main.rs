use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,queued=debug")),
        )
        .init();

    let st = queued::init(queued::config::Config::from_env().await?).await?;
    queued::start_worker(&st).await?;

    let listener = tokio::net::TcpListener::bind(&st.cfg.bind).await?;
    tracing::info!(
        "listening on {} (public: {}, runner: {})",
        st.cfg.bind,
        st.cfg.public_url,
        st.cfg.runner_url
    );
    axum::serve(listener, queued::api::router(st))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
