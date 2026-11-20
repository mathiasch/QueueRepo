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

    if let Some(addr) = st.cfg.metrics.bind {
        // WP10: own listener that serves only /metrics.
        let l = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("metrics on {addr}");
        let r = queued::metrics::router(st.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(l, r).await {
                tracing::error!("metrics listener failed: {e:#}");
            }
        });
    }
    if let Some(c) = &st.cfg.otel {
        tracing::info!("OTLP traces to {} (http/json)", c.endpoint);
    }
    let listener = tokio::net::TcpListener::bind(&st.cfg.bind).await?;
    tracing::info!(
        "listening on {} (public: {}, runner: {})",
        st.cfg.bind,
        st.cfg.public_url,
        st.cfg.runner_url
    );
    axum::serve(listener, queued::api::router(st.clone()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    if let Some(t) = &st.otel {
        // Export what is still queued (bounded by the export timeout).
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), t.flush()).await;
    }
    Ok(())
}
