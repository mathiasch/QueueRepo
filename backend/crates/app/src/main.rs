use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage:
  queued                         run the server
  queued backup [--out DIR] [--keep N] [--unencrypted]
                                 back up database, workspaces and app keys
                                 (BACKUP_DIR, BACKUP_KEY / BACKUP_KEY_FILE, BACKUP_KEEP)
  queued restore --from DIR [--target-runner builtin|<uuid>]
                                 restore a backup into an EMPTY database and empty runners
  queued backup-key FILE         write a new random backup key (0600)";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,queued=debug")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => serve().await,
        Some("backup") => backup(&args[1..]).await,
        Some("restore") => restore(&args[1..]).await,
        Some("backup-key") => {
            let path = args.get(1).ok_or_else(|| anyhow::anyhow!("{USAGE}"))?;
            queued::backup::generate_key_file(std::path::Path::new(path))?;
            println!("new backup key written to {path} – keep a copy outside this host");
            Ok(())
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other}\n{USAGE}"),
    }
}

fn flag_value(args: &[String], name: &str) -> anyhow::Result<Option<String>> {
    match args.iter().position(|a| a == name) {
        Some(i) => {
            Ok(Some(args.get(i + 1).cloned().ok_or_else(|| {
                anyhow::anyhow!("{name} needs a value\n{USAGE}")
            })?))
        }
        None => Ok(None),
    }
}

fn check_args(args: &[String], known: &[&str], with_value: &[&str]) -> anyhow::Result<()> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if with_value.contains(&a) {
            i += 2;
            continue;
        }
        if !known.contains(&a) {
            anyhow::bail!("unknown argument {a}\n{USAGE}");
        }
        i += 1;
    }
    Ok(())
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

async fn backup(args: &[String]) -> anyhow::Result<()> {
    check_args(args, &["--unencrypted"], &["--out", "--keep"])?;
    let cfg = queued::config::Config::from_env().await?;
    let out = flag_value(args, "--out")?
        .or_else(|| env("BACKUP_DIR"))
        .unwrap_or_else(|| "./backups".into());
    let keep: usize = flag_value(args, "--keep")?
        .or_else(|| env("BACKUP_KEEP"))
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(14);
    let key = queued::backup::load_key(&env)?;
    let unencrypted = args.iter().any(|a| a == "--unencrypted");
    if key.is_none() && !unencrypted {
        anyhow::bail!(
            "no BACKUP_KEY / BACKUP_KEY_FILE: backups are encrypted (create a key with `queued backup-key FILE`), \
             or pass --unencrypted explicitly"
        );
    }
    if key.is_some() && unencrypted {
        anyhow::bail!("--unencrypted together with a backup key: choose one");
    }
    let st = queued::init(cfg).await?;
    let dir = queued::backup::run(
        &st,
        &queued::backup::BackupOptions {
            out: out.into(),
            key,
            keep,
        },
    )
    .await?;
    println!("backup complete: {}", dir.display());
    Ok(())
}

async fn restore(args: &[String]) -> anyhow::Result<()> {
    check_args(args, &[], &["--from", "--target-runner"])?;
    let from = flag_value(args, "--from")?
        .ok_or_else(|| anyhow::anyhow!("--from DIR is required\n{USAGE}"))?;
    let target = match flag_value(args, "--target-runner")?.as_deref() {
        None => None,
        Some("builtin") => Some(queued::fleet::BUILTIN),
        Some(id) => Some(id.parse()?),
    };
    let cfg = queued::config::Config::from_env().await?;
    let key = queued::backup::load_key(&env)?;
    let builtin = queued::runner_client::RunnerClient::new(&cfg.runner_url, &cfg.runner_token);
    let r = queued::backup::restore(
        &cfg,
        builtin,
        &queued::backup::RestoreOptions {
            from: from.into(),
            key,
            target_runner: target,
        },
    )
    .await?;
    println!(
        "restore complete: database and {} workspace(s) ({} bytes). Start the server now (it migrates if the backup is older).",
        r.workspaces, r.bytes
    );
    Ok(())
}

async fn serve() -> anyhow::Result<()> {
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
