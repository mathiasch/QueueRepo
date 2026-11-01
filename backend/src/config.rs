use std::{env, path::PathBuf};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;

use crate::oidc::{self, OidcProvider};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SandboxMode {
    /// One `docker run` per agent/verify execution (production default).
    Docker,
    /// Run CLIs directly on the host (development only – no isolation!).
    None,
}

pub struct Config {
    pub database_url: String,
    pub bind: String,
    pub public_url: String,
    pub data_dir: PathBuf,
    /// Path of `data_dir` as seen by the docker daemon (for bind mounts).
    pub host_data_dir: PathBuf,
    pub frontend_dir: PathBuf,
    pub master_key: [u8; 32],
    pub allow_registration: bool,
    pub sandbox: SandboxMode,
    pub agent_image: String,
    pub docker_network: Option<String>,
    pub docker_memory: String,
    pub docker_cpus: String,
    pub task_timeout_min: u64,
    pub verify_timeout_min: u64,
    /// Fallback pause when a rate limit is detected but no reset time could be parsed.
    pub limit_fallback_min: i64,
    pub enable_mock_provider: bool,
    pub vapid_subject: String,
    pub oidc: Vec<OidcProvider>,
}

pub fn var(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.trim().is_empty())
}

fn flag(k: &str, default: bool) -> bool {
    var(k).map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on")).unwrap_or(default)
}

fn num<T: std::str::FromStr>(k: &str, default: T) -> T {
    var(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    pub async fn from_env() -> Result<Self> {
        let data_dir = PathBuf::from(var("DATA_DIR").unwrap_or_else(|| "./data".into()));
        std::fs::create_dir_all(&data_dir).context("create DATA_DIR")?;
        let data_dir = data_dir.canonicalize()?;
        let host_data_dir = var("HOST_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir.clone());

        let sandbox = match var("SANDBOX").as_deref().unwrap_or("docker") {
            "docker" => SandboxMode::Docker,
            "none" => SandboxMode::None,
            other => bail!("unknown SANDBOX mode '{other}' (docker|none)"),
        };
        if sandbox == SandboxMode::None {
            tracing::warn!("SANDBOX=none: agents run unisolated on the host. Development only!");
        }

        let http = reqwest::Client::new();
        Ok(Self {
            database_url: var("DATABASE_URL").context("DATABASE_URL is required")?,
            bind: var("BIND").unwrap_or_else(|| "0.0.0.0:8080".into()),
            public_url: var("PUBLIC_URL")
                .unwrap_or_else(|| "http://localhost:8080".into())
                .trim_end_matches('/')
                .to_string(),
            master_key: load_or_create_key(&data_dir)?,
            frontend_dir: PathBuf::from(var("FRONTEND_DIR").unwrap_or_else(|| "../frontend/dist".into())),
            allow_registration: flag("ALLOW_REGISTRATION", true),
            sandbox,
            agent_image: var("AGENT_IMAGE").unwrap_or_else(|| "queued-agent:latest".into()),
            docker_network: var("DOCKER_NETWORK"),
            docker_memory: var("DOCKER_MEMORY").unwrap_or_else(|| "4g".into()),
            docker_cpus: var("DOCKER_CPUS").unwrap_or_else(|| "2".into()),
            task_timeout_min: num("TASK_TIMEOUT_MIN", 60),
            verify_timeout_min: num("VERIFY_TIMEOUT_MIN", 15),
            limit_fallback_min: num("LIMIT_FALLBACK_MIN", 60),
            enable_mock_provider: flag("ENABLE_MOCK_PROVIDER", false),
            vapid_subject: var("VAPID_SUBJECT").unwrap_or_else(|| "mailto:admin@localhost".into()),
            oidc: oidc::load_providers(&http).await,
            data_dir,
            host_data_dir,
        })
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

/// MASTER_KEY (base64, 32 bytes) encrypts provider credentials at rest.
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
    let bytes = STANDARD.decode(raw.trim()).context("MASTER_KEY must be base64")?;
    bytes.try_into().map_err(|_| anyhow::anyhow!("MASTER_KEY must be 32 bytes"))
}
