//! Project environments (WP02): setup script, variables, secrets, network
//! policy per phase and dependency cache. Validation lives here so the
//! backend and the web frontend apply exactly the same rules.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use queued_common::net::{HostRule, NetMode};

/// Network policy of one phase as configured by the project owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub mode: NetMode,
    /// Ids of [`PRESETS`] (domain groups).
    #[serde(default)]
    pub presets: Vec<String>,
    /// Own host rules (`example.com`, `*.example.com`, `host:port`).
    #[serde(default)]
    pub domains: Vec<String>,
    /// Only GET/HEAD/OPTIONS to the allowed domains.
    #[serde(default)]
    pub read_only: bool,
}

impl NetworkPolicy {
    pub fn off() -> Self {
        Self {
            mode: NetMode::Off,
            presets: vec![],
            domains: vec![],
            read_only: false,
        }
    }

    /// Host rules: preset domains plus own domains (deduplicated, in order).
    pub fn rules(&self) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for p in &self.presets {
            if let Some(pr) = preset(p) {
                for d in pr.domains {
                    if !out.iter().any(|o| o == d) {
                        out.push(d.to_string());
                    }
                }
            }
        }
        for d in &self.domains {
            if !out.contains(d) {
                out.push(d.clone());
            }
        }
        out
    }
}

/// Defaults for projects without a stored environment: the agent phase
/// reaches nothing but its model API (default deny), the setup phase the
/// common package registries.
pub fn default_setup_network() -> NetworkPolicy {
    NetworkPolicy {
        mode: NetMode::Allowlist,
        presets: DEFAULT_SETUP_PRESETS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        domains: vec![],
        read_only: false,
    }
}

pub const DEFAULT_SETUP_PRESETS: [&str; 5] = ["npm", "pypi", "crates", "go", "github"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

/// A stored secret – the value never leaves the backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretInfo {
    pub name: String,
    pub updated_at: DateTime<Utc>,
}

/// Domain group offered in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub domains: &'static [&'static str],
}

/// Package registries and code hosts (state 2026; the hosts are the
/// documented registry endpoints, not verified live from this sandbox).
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "npm",
        label: "npm / Yarn",
        domains: &[
            "registry.npmjs.org",
            "registry.yarnpkg.com",
            "repo.yarnpkg.com",
        ],
    },
    Preset {
        id: "pypi",
        label: "Python (PyPI)",
        domains: &["pypi.org", "files.pythonhosted.org"],
    },
    Preset {
        id: "crates",
        label: "Rust (crates.io)",
        domains: &[
            "crates.io",
            "index.crates.io",
            "static.crates.io",
            "static.rust-lang.org",
        ],
    },
    Preset {
        id: "go",
        label: "Go-Module",
        domains: &["proxy.golang.org", "sum.golang.org"],
    },
    Preset {
        id: "github",
        label: "GitHub (Downloads)",
        domains: &[
            "github.com",
            "api.github.com",
            "codeload.github.com",
            "objects.githubusercontent.com",
            "raw.githubusercontent.com",
        ],
    },
    Preset {
        id: "rubygems",
        label: "Ruby (RubyGems)",
        domains: &["rubygems.org", "index.rubygems.org"],
    },
    Preset {
        id: "maven",
        label: "Java (Maven Central)",
        domains: &["repo.maven.apache.org", "repo1.maven.org"],
    },
    Preset {
        id: "debian",
        label: "Debian-Pakete",
        domains: &["deb.debian.org", "security.debian.org"],
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Lock files that are part of the cache key unless configured otherwise.
pub const DEFAULT_CACHE_FILES: [&str; 13] = [
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "Cargo.lock",
    "requirements.txt",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "go.sum",
    "Gemfile.lock",
    "composer.lock",
    "pom.xml",
];

/// What the runner can enforce (`GET /v1/info`, `/api/meta`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxInfo {
    /// podman | docker | none | "" (runner not reachable)
    pub engine: String,
    /// Network rules are enforced (containers without network of their
    /// own). `false` for engine `none`: rules are advisory.
    pub egress_enforced: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentView {
    pub setup_script: String,
    pub setup_timeout_secs: i32,
    pub env_vars: Vec<EnvVar>,
    pub secrets: Vec<SecretInfo>,
    pub setup_network: NetworkPolicy,
    pub agent_network: NetworkPolicy,
    pub cache_enabled: bool,
    /// Extra files of the cache key (in addition to [`DEFAULT_CACHE_FILES`]).
    pub cache_files: Vec<String>,
    /// `None` = never saved (defaults shown).
    pub updated_at: Option<DateTime<Utc>>,
    pub sandbox: SandboxInfo,
    /// Hosts the agent phase always reaches for the project's current
    /// model access (model API of the CLI).
    pub provider_hosts: Vec<String>,
}

/// `PUT /api/projects/{id}/environment` – replaces all non-secret fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateEnvironment {
    pub setup_script: String,
    pub setup_timeout_secs: i32,
    pub env_vars: Vec<EnvVar>,
    pub setup_network: NetworkPolicy,
    pub agent_network: NetworkPolicy,
    pub cache_enabled: bool,
    #[serde(default)]
    pub cache_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetSecret {
    pub value: String,
}

pub const MAX_SETUP_SCRIPT: usize = 64 * 1024;
pub const SETUP_TIMEOUT: (i32, i32) = (10, 3600);
pub const DEFAULT_SETUP_TIMEOUT: i32 = 600;
pub const MAX_ENV_VARS: usize = 50;
pub const MAX_ENV_VALUE: usize = 4096;
pub const MAX_SECRETS: usize = 20;
pub const SECRET_LEN: (usize, usize) = (8, 16 * 1024);
pub const MAX_DOMAINS: usize = 200;
pub const MAX_CACHE_FILES: usize = 32;

/// Names the queue sets itself or that would undermine the sandbox (proxy,
/// trust store, loader, home).
pub fn reserved_name(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    matches!(
        up.as_str(),
        "HOME"
            | "PATH"
            | "IS_SANDBOX"
            | "CI"
            | "NODE_EXTRA_CA_CERTS"
            | "NODE_OPTIONS"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            | "CURL_CA_BUNDLE"
            | "REQUESTS_CA_BUNDLE"
            | "PIP_CERT"
            | "GIT_SSL_CAINFO"
            | "CARGO_HTTP_CAINFO"
            | "XDG_CACHE_HOME"
            | "CARGO_HOME"
            | "GOMODCACHE"
            | "GOCACHE"
            | "PIP_CACHE_DIR"
            | "NPM_CONFIG_CACHE"
            | "YARN_CACHE_FOLDER"
            | "UV_CACHE_DIR"
    ) || up.ends_with("_PROXY")
        || up.starts_with("QUEUE_")
        || up.starts_with("LD_")
        || up.starts_with("GIT_CONFIG")
}

/// Prefixes of variables that steer a model access or a CLI's configuration
/// (endpoint, key, config/home directory). Project variables reach the
/// agent phase, which runs with the credentials of whoever created the task –
/// possibly an editor. An owner who could set e.g. `ANTHROPIC_BASE_URL` (and
/// allow that host) would receive the editor's API key. Secrets only exist in
/// the setup phase, without model credentials, so they are not limited here.
pub const PROVIDER_PREFIXES: [&str; 16] = [
    "ANTHROPIC_",
    "CLAUDE_",
    "OPENAI_",
    "CODEX_",
    "GEMINI_",
    "GOOGLE_",
    "VERTEX_",
    "CLOUD_ML_",
    "AWS_",
    "AZURE_",
    "MISTRAL_",
    "OPENROUTER_",
    "LITELLM_",
    "OPENCODE_",
    "XDG_",
    "DISABLE_",
];

/// `^[A-Za-z_][A-Za-z0-9_]{0,63}$`, not reserved. Secrets: upper case only.
pub fn check_name(name: &str, secret: bool) -> Result<(), String> {
    let mut c = name.chars();
    let first_ok = c
        .next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_');
    let rest_ok = name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if !first_ok || !rest_ok || name.len() > 64 {
        return Err(format!(
            "„{name}“: nur Buchstaben, Ziffern und _ (nicht mit einer Ziffer beginnen, höchstens 64 Zeichen)"
        ));
    }
    if secret && name.chars().any(|ch| ch.is_ascii_lowercase()) {
        return Err(format!(
            "„{name}“: Namen von Geheimnissen in GROSSBUCHSTABEN"
        ));
    }
    if reserved_name(name) {
        return Err(format!(
            "„{name}“ setzt die Queue selbst und kann nicht überschrieben werden"
        ));
    }
    let up = name.to_ascii_uppercase();
    if !secret && PROVIDER_PREFIXES.iter().any(|p| up.starts_with(p)) {
        return Err(format!(
            "„{name}“ steuert einen KI-Zugang oder ein Agenten-Programm und ist als Projektvariable gesperrt (Aufträge laufen mit den Zugängen der jeweiligen Person)"
        ));
    }
    Ok(())
}

pub fn check_policy(label: &str, p: &NetworkPolicy) -> Result<(), String> {
    for id in &p.presets {
        if preset(id).is_none() {
            return Err(format!("{label}: unbekannte Vorlage „{id}“"));
        }
    }
    if p.domains.len() > MAX_DOMAINS {
        return Err(format!("{label}: höchstens {MAX_DOMAINS} Domains"));
    }
    for d in &p.domains {
        HostRule::parse(d).map_err(|e| format!("{label}: {e}"))?;
    }
    Ok(())
}

/// Splits a text area (one rule per line, commas and spaces also separate)
/// into canonical rules; collects every error.
pub fn parse_domains(text: &str) -> Result<Vec<String>, Vec<String>> {
    let mut ok = vec![];
    let mut errs = vec![];
    for raw in text.split(|c: char| c == '\n' || c == ',' || c.is_whitespace()) {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        match HostRule::parse(t) {
            Ok(r) => {
                let c = r.canonical();
                if !ok.contains(&c) {
                    ok.push(c);
                }
            }
            Err(e) => errs.push(e),
        }
    }
    if errs.is_empty() {
        Ok(ok)
    } else {
        Err(errs)
    }
}

fn safe_rel(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        && !p.contains('\\')
}

/// Validates and normalizes an update (domains canonical, names checked,
/// duplicates refused).
pub fn validate(u: &UpdateEnvironment) -> Result<UpdateEnvironment, String> {
    if u.setup_script.len() > MAX_SETUP_SCRIPT {
        return Err("Setup-Skript ist zu lang (höchstens 64 KiB)".into());
    }
    if u.setup_script.contains('\0') {
        return Err("Setup-Skript enthält ein Nullzeichen".into());
    }
    if !(SETUP_TIMEOUT.0..=SETUP_TIMEOUT.1).contains(&u.setup_timeout_secs) {
        return Err(format!(
            "Zeitlimit des Setups: {} bis {} Sekunden",
            SETUP_TIMEOUT.0, SETUP_TIMEOUT.1
        ));
    }
    if u.env_vars.len() > MAX_ENV_VARS {
        return Err(format!("höchstens {MAX_ENV_VARS} Variablen"));
    }
    let mut names: Vec<&str> = vec![];
    for v in &u.env_vars {
        check_name(&v.name, false)?;
        if names.contains(&v.name.as_str()) {
            return Err(format!("Variable „{}“ ist doppelt", v.name));
        }
        names.push(&v.name);
        if v.value.len() > MAX_ENV_VALUE || v.value.contains('\0') {
            return Err(format!("Wert von „{}“ ist zu lang oder ungültig", v.name));
        }
    }
    check_policy("Setup-Netz", &u.setup_network)?;
    check_policy("Agent-Netz", &u.agent_network)?;
    if u.cache_files.len() > MAX_CACHE_FILES {
        return Err(format!("höchstens {MAX_CACHE_FILES} Cache-Dateien"));
    }
    for f in &u.cache_files {
        if !safe_rel(f) {
            return Err(format!("„{f}“: nur relative Pfade im Projekt"));
        }
    }
    let canon = |p: &NetworkPolicy| NetworkPolicy {
        domains: p
            .domains
            .iter()
            .filter_map(|d| HostRule::parse(d).ok().map(|r| r.canonical()))
            .fold(vec![], |mut acc: Vec<String>, d| {
                if !acc.contains(&d) {
                    acc.push(d);
                }
                acc
            }),
        presets: p.presets.iter().fold(vec![], |mut acc: Vec<String>, x| {
            if !acc.contains(x) {
                acc.push(x.clone());
            }
            acc
        }),
        ..p.clone()
    };
    Ok(UpdateEnvironment {
        setup_network: canon(&u.setup_network),
        agent_network: canon(&u.agent_network),
        ..u.clone()
    })
}

pub fn check_secret_value(v: &str) -> Result<(), String> {
    let n = v.chars().count();
    if n < SECRET_LEN.0 {
        return Err(format!(
            "Geheimnisse brauchen mindestens {} Zeichen (kürzere Werte lassen sich in Protokollen nicht zuverlässig schwärzen)",
            SECRET_LEN.0
        ));
    }
    if v.len() > SECRET_LEN.1 || v.contains('\0') || v.contains('\n') {
        return Err("Wert zu lang oder mit Zeilenumbruch".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upd() -> UpdateEnvironment {
        UpdateEnvironment {
            setup_script: "npm ci".into(),
            setup_timeout_secs: 600,
            env_vars: vec![EnvVar {
                name: "NODE_ENV".into(),
                value: "test".into(),
            }],
            setup_network: default_setup_network(),
            agent_network: NetworkPolicy::off(),
            cache_enabled: true,
            cache_files: vec!["web/package-lock.json".into()],
        }
    }

    #[test]
    fn validates_and_normalizes() {
        let mut u = upd();
        u.agent_network = NetworkPolicy {
            mode: NetMode::Allowlist,
            presets: vec!["pypi".into(), "pypi".into()],
            domains: vec![
                "Docs.RS".into(),
                "docs.rs".into(),
                "*.example.com:8443".into(),
            ],
            read_only: true,
        };
        let v = validate(&u).unwrap();
        assert_eq!(
            v.agent_network.domains,
            vec!["docs.rs", "*.example.com:8443"]
        );
        assert_eq!(v.agent_network.presets, vec!["pypi"]);
        assert_eq!(
            v.agent_network.rules(),
            vec![
                "pypi.org",
                "files.pythonhosted.org",
                "docs.rs",
                "*.example.com:8443"
            ]
        );
    }

    #[test]
    fn rejects_bad_input() {
        type Mutation = Box<dyn Fn(&mut UpdateEnvironment)>;
        let cases: Vec<Mutation> = vec![
            Box::new(|u| u.setup_timeout_secs = 5),
            Box::new(|u| u.setup_script = "x".repeat(MAX_SETUP_SCRIPT + 1)),
            Box::new(|u| u.env_vars[0].name = "1ABC".into()),
            Box::new(|u| u.env_vars[0].name = "HTTPS_PROXY".into()),
            Box::new(|u| u.env_vars[0].name = "https_proxy".into()),
            Box::new(|u| u.env_vars[0].name = "QUEUE_X".into()),
            Box::new(|u| u.env_vars[0].name = "LD_PRELOAD".into()),
            Box::new(|u| u.env_vars[0].name = "NODE_EXTRA_CA_CERTS".into()),
            Box::new(|u| u.env_vars.push(u.env_vars[0].clone())),
            Box::new(|u| u.setup_network.presets.push("nope".into())),
            Box::new(|u| u.setup_network.domains.push("https://x.com".into())),
            Box::new(|u| u.cache_files.push("../etc/passwd".into())),
            Box::new(|u| u.cache_files.push("/abs".into())),
        ];
        for (i, f) in cases.iter().enumerate() {
            let mut u = upd();
            f(&mut u);
            assert!(validate(&u).is_err(), "case {i}");
        }
        assert!(validate(&upd()).is_ok());
    }

    #[test]
    fn secret_names_and_values() {
        assert!(check_name("NPM_TOKEN", true).is_ok());
        assert!(check_name("npm_token", true).is_err());
        assert!(check_name("npm_token", false).is_ok());
        assert!(check_name("PATH", false).is_err());
        // Variables reach the agent (with the task creator's credentials):
        // nothing that redirects a model access or a CLI config.
        for steer in [
            "ANTHROPIC_BASE_URL",
            "openai_base_url",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "XDG_CONFIG_HOME",
            "AWS_REGION",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "OPENCODE_CONFIG_CONTENT",
        ] {
            assert!(check_name(steer, false).is_err(), "{steer}");
        }
        // Secrets live in the setup only (no model credentials there).
        assert!(check_name("AWS_ACCESS_KEY_ID", true).is_ok());
        assert!(check_secret_value("short").is_err());
        assert!(check_secret_value("long-enough").is_ok());
        assert!(check_secret_value("with\nnewline").is_err());
    }

    #[test]
    fn parses_domain_text() {
        assert_eq!(
            parse_domains("docs.rs\n  example.com, *.cdn.example.com\n\ndocs.rs").unwrap(),
            vec!["docs.rs", "example.com", "*.cdn.example.com"]
        );
        assert_eq!(
            parse_domains("ok.com\nhttps://bad.com\n*")
                .unwrap_err()
                .len(),
            2
        );
    }

    #[test]
    fn presets_are_valid_rules() {
        for p in PRESETS {
            for d in p.domains {
                assert!(HostRule::parse(d).is_ok(), "{} {d}", p.id);
            }
        }
        for id in DEFAULT_SETUP_PRESETS {
            assert!(preset(id).is_some());
        }
    }
}
