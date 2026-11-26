use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;

use crate::oidc::{self, OidcProvider};

pub struct Config {
    /// WP12: own origin of live previews (`PREVIEW_ORIGIN`, e.g.
    /// `https://*.preview.example.com`); `None` = no live preview.
    pub preview_origin: Option<PreviewOrigin>,
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
    /// WP09: attachment storage per user (bytes, `ATTACHMENT_QUOTA_MB`).
    pub attachment_quota_bytes: i64,
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
    /// WP08: interval of the intake outbox loop (env `INTAKE_TICK_SECS`,
    /// default 5; 0 = no background loop, tests drive `intake::tick`).
    pub intake_tick_secs: u64,
    /// WP08: API base addresses of the integrations (defaults: the official
    /// APIs; env `INTAKE_{GITHUB,SLACK,LINEAR}_API_URL`). Only for tests and
    /// proxies – the calls still go through the SSRF guard.
    pub intake_github_api: String,
    pub intake_slack_api: String,
    pub intake_linear_api: String,
    /// WP08: replaces the Jira site address for API calls (env
    /// `INTAKE_JIRA_API_URL`; tests only, the site stays the tenant).
    pub intake_jira_api: Option<String>,
    /// WP10: who may read `/metrics` (`METRICS_TOKEN`, `METRICS_BIND`).
    pub metrics: queued_common::metrics::Access,
    /// WP10: OTLP trace export (`OTEL_*`); `None` = off, no overhead.
    pub otel: Option<crate::otel::OtelConfig>,
    /// WP10: interval of the notification loop (outbox, daily summary;
    /// env `NOTIFY_TICK_SECS`, default 15; 0 = tests drive `notify::tick`).
    pub notify_tick_secs: u64,
    /// WP10: ntfy/Web Push/webhook targets in loopback/private networks
    /// (self-hosted ntfy in the LAN; env `ALLOW_PRIVATE_NOTIFY_TARGETS`).
    pub allow_private_notify_targets: bool,
    /// WP10: days of `usage_log` kept for analytics (env
    /// `USAGE_RETENTION_DAYS`, default 400, at least 35 for the budgets).
    pub usage_retention_days: i64,
    /// WP11: interval of runner heartbeats (env `RUNNER_HEARTBEAT_SECS`,
    /// default 5).
    pub runner_heartbeat_secs: u64,
    /// WP11: a runner without a successful heartbeat for this long is
    /// offline, and runs whose lease the app no longer renews are stopped
    /// by the runner (env `RUNNER_LEASE_SECS`, default 30, at least three
    /// heartbeats).
    pub runner_lease_secs: u64,
    /// WP11: audit entries kept (env `AUDIT_RETENTION_DAYS`, default 730,
    /// at least 365 – the database refuses to delete younger entries).
    pub audit_retention_days: i64,
    /// WP15: fixed seed of the model chooser's Thompson sample (env
    /// `CHOOSER_SEED`; unset = a random seed per decision, stored in the
    /// decision so it can be replayed). Makes decisions reproducible in
    /// tests and when debugging.
    pub chooser_seed: Option<u64>,
}

/// WP11: the lease must outlive a few heartbeats, and the runner clamps it
/// to its own bounds.
pub fn check_lease(heartbeat: u64, lease: u64) -> Result<()> {
    if heartbeat == 0 || heartbeat > 60 {
        anyhow::bail!("RUNNER_HEARTBEAT_SECS must be between 1 and 60");
    }
    if lease < 3 * heartbeat
        || !(queued_common::MIN_LEASE_SECS..=queued_common::MAX_LEASE_SECS).contains(&lease)
    {
        anyhow::bail!(
            "RUNNER_LEASE_SECS must be at least 3 × RUNNER_HEARTBEAT_SECS and between {} and {}",
            queued_common::MIN_LEASE_SECS,
            queued_common::MAX_LEASE_SECS
        );
    }
    Ok(())
}

/// WP12: `PREVIEW_ORIGIN` = `<scheme>://*.<suffix>[:port]`. Every live
/// preview gets the host `<capability>.<suffix>` – an own origin, never the
/// app's.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewOrigin {
    pub scheme: String,
    pub suffix: String,
    pub port: Option<u16>,
    /// Origin of the app (frame-ancestors of preview responses).
    pub app_origin: String,
}

/// Length of a preview capability (base32 of 16 random bytes).
pub const PREVIEW_CAP_LEN: usize = 26;

impl PreviewOrigin {
    pub fn parse(v: &str, public_url: &str) -> Result<Self, String> {
        let err = || {
            format!("PREVIEW_ORIGIN must look like https://*.preview.example.com[:port], got {v}")
        };
        let (scheme, rest) = v
            .trim()
            .trim_end_matches('/')
            .split_once("://")
            .ok_or_else(err)?;
        if scheme != "https" && scheme != "http" {
            return Err(err());
        }
        let rest = rest.strip_prefix("*.").ok_or_else(err)?;
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (h, Some(p.parse::<u16>().map_err(|_| err())?)),
            None => (rest, None),
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty()
            || host.contains(['/', '*', '@', '?', '#'])
            || !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return Err(err());
        }
        let app = url::Url::parse(public_url).map_err(|_| "PUBLIC_URL is not a URL".to_string())?;
        let o = Self {
            scheme: scheme.to_string(),
            suffix: host,
            port,
            app_origin: app.origin().ascii_serialization(),
        };
        // The app's own host must never look like a preview host.
        let app_host = match app.port() {
            Some(p) => format!("{}:{p}", app.host_str().unwrap_or_default()),
            None => app.host_str().unwrap_or_default().to_string(),
        };
        if o.cap_of(&app_host).is_some() {
            return Err("PREVIEW_ORIGIN would include the app's own host".into());
        }
        Ok(o)
    }

    /// The capability of a request's `Host`, if it is a preview host.
    pub fn cap_of(&self, host: &str) -> Option<String> {
        let host = host.trim().to_ascii_lowercase();
        let (name, port) = match host.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') => (h.to_string(), p.parse::<u16>().ok()),
            _ => (host.clone(), None),
        };
        if port != self.port {
            return None;
        }
        let cap = name.strip_suffix(&format!(".{}", self.suffix))?;
        (cap.len() == PREVIEW_CAP_LEN
            && cap
                .chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)))
        .then(|| cap.to_string())
    }

    /// Public URL of a preview.
    pub fn url(&self, cap: &str) -> String {
        match self.port {
            Some(p) => format!("{}://{cap}.{}:{p}/", self.scheme, self.suffix),
            None => format!("{}://{cap}.{}/", self.scheme, self.suffix),
        }
    }
}

fn api_url(k: &str, default: &str) -> String {
    var(k)
        .unwrap_or_else(|| default.into())
        .trim_end_matches('/')
        .to_string()
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
        let runner_heartbeat_secs = num::<u64>("RUNNER_HEARTBEAT_SECS", 5);
        let runner_lease_secs = num::<u64>("RUNNER_LEASE_SECS", 30);
        check_lease(runner_heartbeat_secs, runner_lease_secs)?;
        let public_url = var("PUBLIC_URL")
            .unwrap_or_else(|| "http://localhost:8080".into())
            .trim_end_matches('/')
            .to_string();
        let preview_origin = match var("PREVIEW_ORIGIN") {
            Some(v) => Some(PreviewOrigin::parse(&v, &public_url).map_err(anyhow::Error::msg)?),
            None => None,
        };
        Ok(Self {
            preview_origin,
            database_url: var("DATABASE_URL").context("DATABASE_URL is required")?,
            bind: var("BIND").unwrap_or_else(|| "127.0.0.1:8080".into()),
            public_url,
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
            attachment_quota_bytes: num::<i64>(
                "ATTACHMENT_QUOTA_MB",
                queued_api::memory::DEFAULT_ATTACHMENT_QUOTA_MB as i64,
            )
            .max(1)
                * 1024
                * 1024,
            allow_private_llm_endpoints: flag("ALLOW_PRIVATE_LLM_ENDPOINTS", false),
            allow_local_remotes: flag("ALLOW_LOCAL_GIT_REMOTES", false),
            pr_poll_secs: num::<u64>("PR_POLL_SECS", 60),
            allow_private_git_hosts: flag("ALLOW_PRIVATE_GIT_HOSTS", false),
            trigger_tick_secs: num::<u64>("TRIGGER_TICK_SECS", 15),
            intake_tick_secs: num::<u64>("INTAKE_TICK_SECS", 5),
            intake_github_api: api_url("INTAKE_GITHUB_API_URL", "https://api.github.com"),
            intake_slack_api: api_url("INTAKE_SLACK_API_URL", "https://slack.com/api"),
            intake_linear_api: api_url("INTAKE_LINEAR_API_URL", "https://api.linear.app/graphql"),
            intake_jira_api: var("INTAKE_JIRA_API_URL")
                .map(|u| u.trim_end_matches('/').to_string()),
            metrics: queued_common::metrics::Access::parse(
                var("METRICS_TOKEN"),
                var("METRICS_BIND"),
                "",
                None,
            )
            .map_err(anyhow::Error::msg)?,
            otel: crate::otel::OtelConfig::from_env(&|k| var(k)).map_err(anyhow::Error::msg)?,
            notify_tick_secs: num::<u64>("NOTIFY_TICK_SECS", 15),
            allow_private_notify_targets: flag("ALLOW_PRIVATE_NOTIFY_TARGETS", false),
            usage_retention_days: num::<i64>("USAGE_RETENTION_DAYS", 400).max(35),
            runner_heartbeat_secs,
            runner_lease_secs,
            audit_retention_days: num::<i64>("AUDIT_RETENTION_DAYS", 730)
                .max(queued_api::security::AUDIT_MIN_RETENTION_DAYS),
            chooser_seed: var("CHOOSER_SEED").and_then(|v| v.trim().parse().ok()),
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
            preview_origin: None,
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
            attachment_quota_bytes: queued_api::memory::DEFAULT_ATTACHMENT_QUOTA_MB as i64
                * 1024
                * 1024,
            // Fake provider servers listen on 127.0.0.1.
            allow_private_llm_endpoints: true,
            allow_local_remotes: true,
            pr_poll_secs: 1,
            // Fake git host servers listen on 127.0.0.1.
            allow_private_git_hosts: true,
            trigger_tick_secs: 1,
            // Tests drive the intake loop themselves.
            intake_tick_secs: 0,
            intake_github_api: "https://api.github.com".into(),
            intake_slack_api: "https://slack.com/api".into(),
            intake_linear_api: "https://api.linear.app/graphql".into(),
            intake_jira_api: None,
            metrics: Default::default(),
            otel: None,
            // Tests drive the notification loop themselves.
            notify_tick_secs: 0,
            // Fake ntfy/webhook receivers listen on 127.0.0.1.
            allow_private_notify_targets: true,
            usage_retention_days: 400,
            runner_heartbeat_secs: 1,
            runner_lease_secs: 30,
            audit_retention_days: 730,
            chooser_seed: Some(1),
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

    /// SSRF guard for notification targets (ntfy, Web Push, webhooks).
    pub fn notify_guard(&self) -> crate::llm::Guard {
        crate::llm::Guard {
            allow_private: self.allow_private_notify_targets,
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
                // WP11: owner-only, like every other secret file.
                {
                    use std::io::Write;
                    use std::os::unix::fs::OpenOptionsExt;
                    let mut f = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&path)?;
                    f.write_all(enc.as_bytes())?;
                }
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
