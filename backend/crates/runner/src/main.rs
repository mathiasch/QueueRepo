use std::{env, path::PathBuf};

use anyhow::{bail, Context, Result};
use queued_common::net::{Cidr, HostRule};
use queued_runner::{egress, router, RunnerConfig, RunnerState};
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

    if var("RUNNER_NETWORK").is_some() {
        bail!(
            "RUNNER_NETWORK is no longer supported: containers run with --network none and reach \
             the network only through the per-run egress proxy (project environment, WP02). \
             Remove the variable; see RUNNER_EGRESS_PRIVATE_HOSTS for LAN hosts."
        );
    }
    let data_dir = PathBuf::from(var("RUNNER_DATA_DIR").unwrap_or_else(|| "./runner-data".into()));
    std::fs::create_dir_all(data_dir.join("workspaces"))?;
    let data_dir = data_dir.canonicalize()?;
    if data_dir.as_os_str().len() > queued_runner::MAX_DATA_DIR_LEN {
        bail!(
            "RUNNER_DATA_DIR path is longer than {} bytes (Unix socket path limit for the egress proxy)",
            queued_runner::MAX_DATA_DIR_LEN
        );
    }
    let egress_cfg = egress_config()?;
    let engine = var("RUNNER_ENGINE")
        .unwrap_or_else(|| "podman".into())
        .parse()?;
    if engine == queued_runner::exec::Engine::None {
        tracing::warn!("RUNNER_ENGINE=none: agents run without isolation. Development only!");
    }
    let cfg = RunnerConfig {
        data_dir,
        token: var("RUNNER_TOKEN").context("RUNNER_TOKEN is required")?,
        engine,
        image: var("RUNNER_IMAGE").unwrap_or_else(|| "localhost/queued-agent:latest".into()),
        memory: var("RUNNER_MEMORY").unwrap_or_else(|| "4g".into()),
        cpus: var("RUNNER_CPUS").unwrap_or_else(|| "2".into()),
        #[cfg(unix)]
        docker_user: var("RUNNER_DOCKER_USER").or(Some(format!("{uid}:{gid}"))),
        #[cfg(not(unix))]
        docker_user: var("RUNNER_DOCKER_USER"),
        volume_opts: var("RUNNER_VOLUME_OPTS").unwrap_or_default(),
        kill_grace_secs: match var("RUNNER_KILL_GRACE_SECS") {
            Some(v) => v
                .parse()
                .context("RUNNER_KILL_GRACE_SECS must be a number of seconds")?,
            None => 15,
        },
        egress: std::sync::Arc::new(egress_cfg),
    };
    if cfg.kill_grace_secs > 60 {
        // The app's exec HTTP timeout allows for at most 60 s of grace.
        bail!("RUNNER_KILL_GRACE_SECS must be at most 60");
    }
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

fn list(k: &str) -> Vec<String> {
    var(k)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// `RUNNER_EGRESS_PRIVATE_HOSTS` (hosts that may resolve to internal
/// addresses), `RUNNER_EGRESS_DENY` (CIDRs, always refused) and
/// `RUNNER_EGRESS_EXTRA_CA` (PEM file, extra roots for inspected HTTPS).
fn egress_config() -> Result<egress::EgressConfig> {
    let private_hosts = list("RUNNER_EGRESS_PRIVATE_HOSTS")
        .iter()
        .map(|h| HostRule::parse(h).map_err(anyhow::Error::msg))
        .collect::<Result<Vec<_>>>()
        .context("RUNNER_EGRESS_PRIVATE_HOSTS")?;
    let deny = list("RUNNER_EGRESS_DENY")
        .iter()
        .map(|c| Cidr::parse(c).with_context(|| format!("RUNNER_EGRESS_DENY: invalid CIDR {c}")))
        .collect::<Result<Vec<_>>>()?;
    let own = egress::host_addresses();
    if own.is_empty() {
        tracing::warn!(
            "could not read the host's own addresses (/proc/net); set RUNNER_EGRESS_DENY"
        );
    }
    let extra_roots = match var("RUNNER_EGRESS_EXTRA_CA") {
        Some(p) => egress::load_roots(std::path::Path::new(&p))?,
        None => vec![],
    };
    Ok(egress::EgressConfig {
        private_hosts,
        deny,
        own,
        extra_roots,
    })
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
