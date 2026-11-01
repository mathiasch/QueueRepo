use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;

use crate::oidc::{self, OidcProvider};

pub struct Config {
    pub database_url: String,
    pub bind: String,
    pub public_url: String,
    /// App-local state only (keys). Workspaces live in the runner.
    pub data_dir: PathBuf,
    pub frontend_dir: PathBuf,
    pub master_key: [u8; 32],
    pub allow_registration: bool,
    pub runner_url: String,
    pub runner_token: String,
    pub task_timeout_min: u64,
    pub verify_timeout_min: u64,
    /// Fallback pause when a rate limit is detected but no reset time could be parsed.
    pub limit_fallback_min: i64,
    pub enable_mock_provider: bool,
    pub vapid_subject: String,
    pub oidc: Vec<OidcProvider>,
    /// Scheduler idle poll interval.
    pub tick_secs: u64,
    /// Allow `file://` / local path git remotes (tests, single-host setups).
    pub allow_local_remotes: bool,
}

pub fn var(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.trim().is_empty())
}

fn flag(k: &str, default: bool) -> bool {
    var(k)
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

fn num<T: std::str::FromStr>(k: &str, default: T) -> T {
    var(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    pub async fn from_env() -> Result<Self> {
        let data_dir = PathBuf::from(var("DATA_DIR").unwrap_or_else(|| "./data".into()));
        std::fs::create_dir_all(&data_dir).context("create DATA_DIR")?;
        let data_dir = data_dir.canonicalize()?;
        let http = reqwest::Client::new();
        Ok(Self {
            database_url: var("DATABASE_URL").context("DATABASE_URL is required")?,
            bind: var("BIND").unwrap_or_else(|| "127.0.0.1:8080".into()),
            public_url: var("PUBLIC_URL")
                .unwrap_or_else(|| "http://localhost:8080".into())
                .trim_end_matches('/')
                .to_string(),
            master_key: load_or_create_key(&data_dir)?,
            frontend_dir: PathBuf::from(
                var("FRONTEND_DIR").unwrap_or_else(|| "../frontend/dist".into()),
            ),
            allow_registration: flag("ALLOW_REGISTRATION", true),
            runner_url: var("RUNNER_URL").unwrap_or_else(|| "http://127.0.0.1:8090".into()),
            runner_token: var("RUNNER_TOKEN").context("RUNNER_TOKEN is required")?,
            task_timeout_min: num("TASK_TIMEOUT_MIN", 60),
            verify_timeout_min: num("VERIFY_TIMEOUT_MIN", 15),
            limit_fallback_min: num("LIMIT_FALLBACK_MIN", 60),
            enable_mock_provider: flag("ENABLE_MOCK_PROVIDER", false),
            vapid_subject: var("VAPID_SUBJECT").unwrap_or_else(|| "mailto:admin@localhost".into()),
            oidc: oidc::load_providers(&http).await,
            tick_secs: num("TICK_SECS", 15),
            allow_local_remotes: flag("ALLOW_LOCAL_GIT_REMOTES", false),
            data_dir,
        })
    }

    /// Config for integration tests (mock provider, fast ticks).
    pub fn for_tests(
        database_url: &str,
        data_dir: PathBuf,
        runner_url: &str,
        runner_token: &str,
    ) -> Self {
        Self {
            database_url: database_url.into(),
            bind: "127.0.0.1:0".into(),
            public_url: "http://localhost".into(),
            data_dir,
            frontend_dir: PathBuf::from("/nonexistent"),
            master_key: [7u8; 32],
            allow_registration: true,
            runner_url: runner_url.into(),
            runner_token: runner_token.into(),
            task_timeout_min: 1,
            verify_timeout_min: 1,
            limit_fallback_min: 60,
            enable_mock_provider: true,
            vapid_subject: "mailto:test@localhost".into(),
            oidc: vec![],
            tick_secs: 1,
            allow_local_remotes: true,
        }
    }

    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    pub fn providers(&self) -> Vec<&'static str> {
        let mut p = vec!["claude", "codex", "gemini"];
        if self.enable_mock_provider {
            p.push("mock");
        }
        p
    }
}

/// MASTER_KEY (base64, 32 bytes) encrypts provider credentials and git tokens.
/// If not set, a key is generated once and persisted in DATA_DIR.
fn load_or_create_key(data_dir: &std::path::Path) -> Result<[u8; 32]> {
    let raw = match var("MASTER_KEY") {
        Some(k) => k,
        None => {
            let path = data_dir.join("master.key");
            if path.exists() {
                std::fs::read_to_string(&path)?.trim().to_string()
            } else {
                let mut k = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut k);
                let enc = STANDARD.encode(k);
                std::fs::write(&path, &enc)?;
                tracing::warn!("generated MASTER_KEY at {} – back it up!", path.display());
                enc
            }
        }
    };
    let bytes = STANDARD
        .decode(raw.trim())
        .context("MASTER_KEY must be base64")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("MASTER_KEY must be 32 bytes"))
}
