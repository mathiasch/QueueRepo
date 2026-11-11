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
    /// Default runtime limit per agent run (project/task limits override it).
    pub task_timeout_min: u64,
    pub verify_timeout_min: u64,
    /// Stop a run after this many seconds without any output (0 = off).
    /// Env: `STALL_TIMEOUT_MIN`.
    pub stall_timeout_secs: u64,
    /// Re-queues for transient provider/infra/stall failures before a task is blocked.
    pub transient_max_retries: i32,
    pub backoff_base_secs: u64,
    pub backoff_max_secs: u64,
    /// Fallback pause when a rate limit is detected but no reset time could be parsed.
    pub limit_fallback_min: i64,
    pub enable_mock_provider: bool,
    pub vapid_subject: String,
    pub oidc: Vec<OidcProvider>,
    /// Scheduler idle poll interval.
    pub tick_secs: u64,
    /// Poll interval of single-shot/batch jobs (env `BATCH_POLL_SECS`).
    pub batch_poll_secs: u64,
    /// App-side model calls may go to loopback/private networks
    /// (`ALLOW_PRIVATE_LLM_ENDPOINTS`, default off – SSRF guard).
    pub allow_private_llm_endpoints: bool,
    /// Allow `file://` / local path git remotes (tests, single-host setups).
    pub allow_local_remotes: bool,
    /// WP05: poll interval of pull requests (env `PR_POLL_SECS`, default 60;
    /// 0 = no background poller, only for tests that drive it themselves).
    pub pr_poll_secs: u64,
    /// WP05: git host APIs in loopback/private networks (self-hosted
    /// Gitea/GitLab in the LAN; env `ALLOW_PRIVATE_GIT_HOSTS`, default off).
    /// Also allows plain `http://` API addresses there.
    pub allow_private_git_hosts: bool,
    /// WP07: interval of the trigger scheduler (env `TRIGGER_TICK_SECS`,
    /// default 15; 0 = no background loop, tests drive `triggers::tick`).
    pub trigger_tick_secs: u64,
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
            stall_timeout_secs: num::<u64>("STALL_TIMEOUT_MIN", 20).saturating_mul(60),
            transient_max_retries: num::<i32>("TRANSIENT_MAX_RETRIES", 5).max(0),
            backoff_base_secs: num::<u64>("TRANSIENT_BACKOFF_BASE_SECS", 30).max(1),
            backoff_max_secs: num::<u64>("TRANSIENT_BACKOFF_MAX_SECS", 900).max(1),
            limit_fallback_min: num("LIMIT_FALLBACK_MIN", 60),
            enable_mock_provider: flag("ENABLE_MOCK_PROVIDER", false),
            vapid_subject: var("VAPID_SUBJECT").unwrap_or_else(|| "mailto:admin@localhost".into()),
            oidc: oidc::load_providers(&http).await,
            tick_secs: num("TICK_SECS", 15),
            batch_poll_secs: num::<u64>("BATCH_POLL_SECS", 60).max(1),
            allow_private_llm_endpoints: flag("ALLOW_PRIVATE_LLM_ENDPOINTS", false),
            allow_local_remotes: flag("ALLOW_LOCAL_GIT_REMOTES", false),
            pr_poll_secs: num::<u64>("PR_POLL_SECS", 60),
            allow_private_git_hosts: flag("ALLOW_PRIVATE_GIT_HOSTS", false),
            trigger_tick_secs: num::<u64>("TRIGGER_TICK_SECS", 15),
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
            // Off, so long-running mock scenarios (MOCK_SLOW) behave as before.
            stall_timeout_secs: 0,
            transient_max_retries: 2,
            backoff_base_secs: 1,
            backoff_max_secs: 2,
            limit_fallback_min: 60,
            enable_mock_provider: true,
            vapid_subject: "mailto:test@localhost".into(),
            oidc: vec![],
            tick_secs: 1,
            batch_poll_secs: 1,
            // Fake provider servers listen on 127.0.0.1.
            allow_private_llm_endpoints: true,
            allow_local_remotes: true,
            pr_poll_secs: 1,
            // Fake git host servers listen on 127.0.0.1.
            allow_private_git_hosts: true,
            trigger_tick_secs: 1,
        }
    }

    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    /// SSRF guard for app-side calls to user-configured endpoints.
    pub fn llm_guard(&self) -> crate::llm::Guard {
        let runner = url::Url::parse(&self.runner_url)
            .ok()
            .and_then(|u| Some(format!("{}:{}", u.host_str()?, u.port_or_known_default()?)));
        crate::llm::Guard {
            allow_private: self.allow_private_llm_endpoints,
            deny: runner.into_iter().collect(),
        }
    }

    /// SSRF guard for calls to git host APIs (never the runner).
    pub fn git_host_guard(&self) -> crate::llm::Guard {
        crate::llm::Guard {
            allow_private: self.allow_private_git_hosts,
            ..self.llm_guard()
        }
    }

    pub fn providers(&self) -> Vec<&'static str> {
        let mut p = vec!["claude", "codex", "gemini", "opencode"];
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
