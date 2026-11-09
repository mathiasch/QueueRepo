//! Project environments (WP02): storage, secrets and the per-phase egress
//! policies handed to the runner.
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use queued_api::env::{
    default_setup_network, EnvVar, NetworkPolicy, SandboxInfo, SecretInfo, UpdateEnvironment,
    DEFAULT_CACHE_FILES, DEFAULT_SETUP_TIMEOUT,
};
use queued_common::{CacheSpec, EgressPolicy, EgressVerdict, ExecFrame};
use sqlx::types::Json;
use uuid::Uuid;

use crate::{crypto, models::Task, queue::event, state::AppState};

/// Effective environment of a project (stored row or defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct Env {
    pub setup_script: String,
    pub setup_timeout_secs: i32,
    pub env_vars: Vec<EnvVar>,
    pub setup_network: NetworkPolicy,
    pub agent_network: NetworkPolicy,
    pub cache_enabled: bool,
    pub cache_files: Vec<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for Env {
    fn default() -> Self {
        Self {
            setup_script: String::new(),
            setup_timeout_secs: DEFAULT_SETUP_TIMEOUT,
            env_vars: vec![],
            setup_network: default_setup_network(),
            agent_network: NetworkPolicy::off(),
            cache_enabled: true,
            cache_files: vec![],
            updated_at: None,
        }
    }
}

type Row = (
    String,
    i32,
    Json<Vec<EnvVar>>,
    Json<NetworkPolicy>,
    Json<NetworkPolicy>,
    bool,
    Vec<String>,
    DateTime<Utc>,
);

pub async fn load(db: &sqlx::PgPool, project: Uuid) -> sqlx::Result<Env> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT setup_script, setup_timeout_secs, env_vars, setup_network, agent_network,
                cache_enabled, cache_files, updated_at
         FROM project_environments WHERE project_id=$1",
    )
    .bind(project)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        None => Env::default(),
        Some((script, timeout, vars, setup, agent, cache, files, at)) => Env {
            setup_script: script,
            setup_timeout_secs: timeout,
            env_vars: vars.0,
            setup_network: setup.0,
            agent_network: agent.0,
            cache_enabled: cache,
            cache_files: files,
            updated_at: Some(at),
        },
    })
}

/// Stores a validated update (see [`queued_api::env::validate`]).
pub async fn save(db: &sqlx::PgPool, project: Uuid, u: &UpdateEnvironment) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO project_environments
            (project_id, setup_script, setup_timeout_secs, env_vars, setup_network, agent_network, cache_enabled, cache_files, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now())
         ON CONFLICT (project_id) DO UPDATE SET
            setup_script=EXCLUDED.setup_script, setup_timeout_secs=EXCLUDED.setup_timeout_secs,
            env_vars=EXCLUDED.env_vars, setup_network=EXCLUDED.setup_network,
            agent_network=EXCLUDED.agent_network, cache_enabled=EXCLUDED.cache_enabled,
            cache_files=EXCLUDED.cache_files, updated_at=now()",
    )
    .bind(project)
    .bind(&u.setup_script)
    .bind(u.setup_timeout_secs)
    .bind(Json(&u.env_vars))
    .bind(Json(&u.setup_network))
    .bind(Json(&u.agent_network))
    .bind(u.cache_enabled)
    .bind(&u.cache_files)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn secret_infos(db: &sqlx::PgPool, project: Uuid) -> sqlx::Result<Vec<SecretInfo>> {
    let rows: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT name, updated_at FROM project_secrets WHERE project_id=$1 ORDER BY name",
    )
    .bind(project)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, updated_at)| SecretInfo { name, updated_at })
        .collect())
}

/// Decrypted secrets of a project (only ever sent to the setup phase).
/// The project has at least one secret (setup runs with secrets).
pub async fn has_secrets(st: &AppState, project: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM project_secrets WHERE project_id=$1)")
        .bind(project)
        .fetch_one(&st.db)
        .await
}

pub async fn secret_values(st: &AppState, project: Uuid) -> anyhow::Result<Vec<(String, String)>> {
    let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT name, value_enc FROM project_secrets WHERE project_id=$1 ORDER BY name",
    )
    .bind(project)
    .fetch_all(&st.db)
    .await?;
    rows.into_iter()
        .map(|(n, enc)| {
            let v = crypto::decrypt(&st.cfg.master_key, &enc)?;
            Ok((n, String::from_utf8(v)?))
        })
        .collect()
}

impl Env {
    pub fn has_setup(&self) -> bool {
        !self.setup_script.trim().is_empty()
    }

    /// Variables for every phase (setup, agent, verify, review).
    pub fn vars(&self) -> Vec<(String, String)> {
        self.env_vars
            .iter()
            .map(|v| (v.name.clone(), v.value.clone()))
            .collect()
    }

    pub fn setup_policy(&self) -> EgressPolicy {
        policy(&self.setup_network, vec![])
    }

    /// Agent/review runs: the model API of the run is always reachable.
    pub fn agent_policy(&self, provider_hosts: Vec<String>) -> EgressPolicy {
        policy(&self.agent_network, provider_hosts)
    }

    /// Verify command: the agent's network without the model API.
    pub fn verify_policy(&self) -> EgressPolicy {
        policy(&self.agent_network, vec![])
    }

    /// Cache key material: everything that changes what the setup installs
    /// (script, variables, setup network); lock files are hashed by the runner.
    pub fn cache_spec(&self) -> Option<CacheSpec> {
        if !self.cache_enabled {
            return None;
        }
        let vars: BTreeMap<&str, &str> = self
            .env_vars
            .iter()
            .map(|v| (v.name.as_str(), v.value.as_str()))
            .collect();
        let material = serde_json::json!({
            "script": self.setup_script,
            "vars": vars,
            "network": self.setup_network,
        })
        .to_string();
        let mut files: Vec<String> = DEFAULT_CACHE_FILES.iter().map(|s| s.to_string()).collect();
        for f in &self.cache_files {
            if !files.contains(f) {
                files.push(f.clone());
            }
        }
        Some(CacheSpec {
            key_material: material,
            files,
            scope: None,
        })
    }
}

fn policy(p: &NetworkPolicy, always: Vec<String>) -> EgressPolicy {
    EgressPolicy {
        mode: p.mode,
        allow: p.rules(),
        always,
        read_only: p.read_only,
    }
}

/// How long a runner answer is trusted: a runner restarted with another
/// engine must not keep showing an outdated protection status.
const SANDBOX_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// What the runner enforces; cached for [`SANDBOX_TTL`].
pub async fn sandbox(st: &AppState) -> SandboxInfo {
    if let Some((at, s)) = st.sandbox.read().ok().and_then(|g| g.clone()) {
        if at.elapsed() < SANDBOX_TTL {
            return s;
        }
    }
    match st.runner.info().await {
        Ok(i) => {
            let s = SandboxInfo {
                engine: i.engine,
                egress_enforced: i.egress_enforced,
            };
            if let Ok(mut g) = st.sandbox.write() {
                *g = Some((std::time::Instant::now(), s.clone()));
            }
            s
        }
        Err(e) => {
            tracing::warn!("runner info unavailable: {e:#}");
            SandboxInfo {
                engine: String::new(),
                egress_enforced: false,
            }
        }
    }
}

pub fn verdict_text(v: EgressVerdict) -> &'static str {
    match v {
        EgressVerdict::Provider => "Modell-API",
        EgressVerdict::Allowlist => "Allowlist",
        EgressVerdict::Open => "offenes Netz",
        EgressVerdict::PhaseOff => "Netz in dieser Phase aus",
        EgressVerdict::NotAllowed => "nicht in der Allowlist",
        EgressVerdict::BlockedAddress => "interne oder gesperrte Adresse",
        EgressVerdict::ReadOnly => "nur GET/HEAD/OPTIONS erlaubt",
        EgressVerdict::BadRequest => "ungültige Proxy-Anfrage",
        EgressVerdict::Unresolvable => "Name nicht auflösbar",
        EgressVerdict::ConnectFailed => "Verbindung fehlgeschlagen",
    }
}

/// Most refusals written as single events per run (the rest is counted).
const MAX_DENY_EVENTS: u32 = 100;

/// Collects the proxy decisions of one run: every refusal becomes an event
/// (bounded), allowed targets are summarised at the end.
pub struct EgressLog {
    phase: &'static str,
    allowed: BTreeMap<String, (u32, EgressVerdict)>,
    denied: u32,
}

impl EgressLog {
    pub fn new(phase: &'static str) -> Self {
        Self {
            phase,
            allowed: BTreeMap::new(),
            denied: 0,
        }
    }

    /// Handles an `Egress` frame; other frames are ignored.
    pub async fn on(&mut self, st: &AppState, task: &Task, frame: &ExecFrame) {
        let ExecFrame::Egress {
            host,
            port,
            method,
            verdict,
        } = frame
        else {
            return;
        };
        let target = format!("{host}:{port}");
        if verdict.allowed() && *verdict != EgressVerdict::ConnectFailed {
            let e = self.allowed.entry(target).or_insert((0, *verdict));
            e.0 += 1;
            return;
        }
        self.denied += 1;
        if self.denied <= MAX_DENY_EVENTS {
            event(
                st,
                task,
                "egress",
                &format!(
                    "Blockiert ({}): {method} {target} – {}",
                    self.phase,
                    verdict_text(*verdict)
                ),
            )
            .await;
        }
    }

    pub async fn finish(self, st: &AppState, task: &Task, enforced: bool) {
        if self.allowed.is_empty() && self.denied == 0 {
            return;
        }
        let mut parts: Vec<String> = self
            .allowed
            .iter()
            .map(|(t, (n, v))| format!("{t} ×{n} ({})", verdict_text(*v)))
            .collect();
        if self.denied > 0 {
            parts.push(format!("{} blockiert", self.denied));
        }
        let suffix = if enforced {
            ""
        } else {
            " – nicht durchgesetzt (Entwicklungsmodus ohne Container)"
        };
        event(
            st,
            task,
            "egress",
            &format!("Netzwerk ({}): {}{suffix}", self.phase, parts.join(", ")),
        )
        .await;
    }
}

/// One line about the network of a phase (for the start event).
pub fn describe(p: &EgressPolicy) -> String {
    let base = match p.mode {
        queued_common::NetMode::Off if p.always.is_empty() => "kein Netz".to_string(),
        queued_common::NetMode::Off => "nur Modell-API".to_string(),
        queued_common::NetMode::Allowlist => format!("Allowlist mit {} Einträgen", p.allow.len()),
        queued_common::NetMode::Open => "offenes Netz (ohne interne Adressen)".to_string(),
    };
    if p.read_only && p.mode != queued_common::NetMode::Off {
        format!("{base}, nur lesend")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::env::NetMode;

    #[test]
    fn policies_per_phase() {
        let e = Env {
            agent_network: NetworkPolicy {
                mode: NetMode::Allowlist,
                presets: vec!["pypi".into()],
                domains: vec!["docs.rs".into()],
                read_only: true,
            },
            ..Env::default()
        };
        let a = e.agent_policy(vec!["api.anthropic.com".into()]);
        assert_eq!(a.always, vec!["api.anthropic.com"]);
        assert_eq!(
            a.allow,
            vec!["pypi.org", "files.pythonhosted.org", "docs.rs"]
        );
        assert!(a.read_only);
        let v = e.verify_policy();
        assert!(v.always.is_empty() && v.allow == a.allow);
        // Defaults: agent default-deny, setup registries.
        let d = Env::default();
        assert_eq!(d.agent_policy(vec![]), EgressPolicy::default());
        assert!(d
            .setup_policy()
            .allow
            .contains(&"registry.npmjs.org".to_string()));
        assert_eq!(
            describe(&d.agent_policy(vec!["x.com".into()])),
            "nur Modell-API"
        );
        assert_eq!(describe(&d.agent_policy(vec![])), "kein Netz");
    }

    #[test]
    fn cache_spec_changes_with_script_and_vars() {
        let a = Env {
            setup_script: "npm ci".into(),
            cache_files: vec!["web/package-lock.json".into(), "Cargo.lock".into()],
            ..Env::default()
        };
        let s = a.cache_spec().unwrap();
        assert!(s.files.contains(&"package-lock.json".to_string()));
        assert_eq!(s.files.iter().filter(|f| *f == "Cargo.lock").count(), 1);
        assert!(s.files.contains(&"web/package-lock.json".to_string()));
        let b = Env {
            env_vars: vec![EnvVar {
                name: "NODE_ENV".into(),
                value: "ci".into(),
            }],
            ..a.clone()
        };
        assert_ne!(s.key_material, b.cache_spec().unwrap().key_material);
        assert!(Env {
            cache_enabled: false,
            ..a
        }
        .cache_spec()
        .is_none());
    }
}
