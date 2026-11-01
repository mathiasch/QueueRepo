use std::{env, path::PathBuf};

use anyhow::{bail, Context, Result};
use queued_runner::{router, RunnerConfig, RunnerState};
use tracing_subscriber::EnvFilter;

fn var(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    #[cfg(unix)]
    let (uid, gid) = process_ids();
    #[cfg(unix)]
    if uid == 0 && var("RUNNER_ALLOW_ROOT").is_none() {
        bail!(
            "queued-runner must not run as root (set RUNNER_ALLOW_ROOT=1 to override, e.g. in CI)"
        );
    }

    let data_dir = PathBuf::from(var("RUNNER_DATA_DIR").unwrap_or_else(|| "./runner-data".into()));
    std::fs::create_dir_all(data_dir.join("workspaces"))?;
    let engine = var("RUNNER_ENGINE")
        .unwrap_or_else(|| "podman".into())
        .parse()?;
    if engine == queued_runner::exec::Engine::None {
        tracing::warn!("RUNNER_ENGINE=none: agents run without isolation. Development only!");
    }
    let cfg = RunnerConfig {
        data_dir: data_dir.canonicalize()?,
        token: var("RUNNER_TOKEN").context("RUNNER_TOKEN is required")?,
        engine,
        image: var("RUNNER_IMAGE").unwrap_or_else(|| "localhost/queued-agent:latest".into()),
        network: var("RUNNER_NETWORK"),
        memory: var("RUNNER_MEMORY").unwrap_or_else(|| "4g".into()),
        cpus: var("RUNNER_CPUS").unwrap_or_else(|| "2".into()),
        #[cfg(unix)]
        docker_user: var("RUNNER_DOCKER_USER").or(Some(format!("{uid}:{gid}"))),
        #[cfg(not(unix))]
        docker_user: var("RUNNER_DOCKER_USER"),
        volume_opts: var("RUNNER_VOLUME_OPTS").unwrap_or_default(),
    };
    if cfg.token.len() < 24 {
        bail!("RUNNER_TOKEN must be at least 24 characters");
    }
    let bind = var("RUNNER_BIND").unwrap_or_else(|| "127.0.0.1:8090".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(
        "queued-runner listening on {bind} (engine {:?})",
        cfg.engine
    );
    axum::serve(listener, router(RunnerState::new(cfg)))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(unix)]
fn process_ids() -> (u32, u32) {
    // Read real ids from /proc to avoid a libc dependency.
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let get = |k: &str| {
        status
            .lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(u32::MAX)
    };
    (get("Uid:"), get("Gid:"))
}
