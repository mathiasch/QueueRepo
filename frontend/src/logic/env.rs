//! Environment editor (WP02): form state, texts and validation. The rules
//! themselves come from `queued_api::env`, shared with the backend.
use queued_api::env::{
    parse_domains, validate, EnvVar, EnvironmentView, NetMode, NetworkPolicy, SandboxInfo,
    UpdateEnvironment, SETUP_TIMEOUT,
};

/// Which phase a network card configures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Agent,
}

/// Label of a mode in the segmented control.
pub fn mode_label(phase: Phase, m: NetMode) -> &'static str {
    match (phase, m) {
        (Phase::Setup, NetMode::Off) => "Kein Netz",
        (Phase::Agent, NetMode::Off) => "Nur Modell-API",
        (_, NetMode::Allowlist) => "Erlaubte Adressen",
        (_, NetMode::Open) => "Offen",
    }
}

/// One sentence what the mode means.
pub fn mode_help(phase: Phase, m: NetMode) -> &'static str {
    match (phase, m) {
        (Phase::Setup, NetMode::Off) => {
            "Das Setup-Skript läuft ganz ohne Internet."
        }
        (Phase::Agent, NetMode::Off) => {
            "Der Agent erreicht nur die Schnittstelle seines KI-Modells – sonst nichts. Empfohlen."
        }
        (_, NetMode::Allowlist) => {
            "Nur die angehakten Paketquellen und die Adressen in der Liste sind erreichbar."
        }
        (_, NetMode::Open) => {
            "Alle öffentlichen Adressen sind erreichbar. Interne Netze, der Server selbst und Cloud-Metadaten bleiben gesperrt."
        }
    }
}

pub const MODES: [NetMode; 3] = [NetMode::Off, NetMode::Allowlist, NetMode::Open];

/// Status banner: (tone class, title, explanation).
pub fn sandbox_status(s: &SandboxInfo) -> (&'static str, &'static str, String) {
    if s.engine.is_empty() {
        return (
            "tone-warning",
            "Status unbekannt",
            "Der Runner antwortet gerade nicht. Ob die Netzwerkregeln durchgesetzt werden, lässt sich nicht prüfen.".into(),
        );
    }
    if s.egress_enforced {
        (
            "tone-success",
            "Netzwerkregeln werden durchgesetzt",
            format!(
                "Aufträge laufen in Containern ({}) ohne eigenen Netzzugang. Jede Verbindung geht über den Prüf-Proxy der Queue und erscheint im Protokoll.",
                s.engine
            ),
        )
    } else {
        (
            "tone-warning",
            "Entwicklungsmodus: Netzwerkregeln werden nicht durchgesetzt",
            "Der Runner startet Aufträge ohne Container. Nur Programme, die den Proxy beachten, halten sich an die Regeln – alles andere erreicht das Internet ungehindert. Für echten Schutz den Runner mit Podman betreiben.".into(),
        )
    }
}

/// Editable copy of one phase's policy.
#[derive(Debug, Clone, PartialEq)]
pub struct NetForm {
    pub mode: NetMode,
    pub presets: Vec<String>,
    /// One host rule per line.
    pub domains: String,
    pub read_only: bool,
}

impl NetForm {
    pub fn from(p: &NetworkPolicy) -> Self {
        Self {
            mode: p.mode,
            presets: p.presets.clone(),
            domains: p.domains.join("\n"),
            read_only: p.read_only,
        }
    }

    pub fn toggle_preset(&mut self, id: &str, on: bool) {
        self.presets.retain(|p| p != id);
        if on {
            self.presets.push(id.to_string());
        }
    }

    fn policy(&self, label: &str, errors: &mut Vec<String>) -> NetworkPolicy {
        let domains = match parse_domains(&self.domains) {
            Ok(d) => d,
            Err(es) => {
                errors.extend(es.into_iter().map(|e| format!("{label}: {e}")));
                vec![]
            }
        };
        NetworkPolicy {
            mode: self.mode,
            presets: self.presets.clone(),
            domains,
            read_only: self.read_only,
        }
    }
}

/// Form state of the whole editor (secrets are saved separately).
#[derive(Debug, Clone, PartialEq)]
pub struct EnvForm {
    pub script: String,
    pub timeout: String,
    pub vars: Vec<EnvVar>,
    pub setup: NetForm,
    pub agent: NetForm,
    pub cache_enabled: bool,
    /// One path per line.
    pub cache_files: String,
}

impl EnvForm {
    pub fn from_view(v: &EnvironmentView) -> Self {
        Self {
            script: v.setup_script.clone(),
            timeout: v.setup_timeout_secs.to_string(),
            vars: v.env_vars.clone(),
            setup: NetForm::from(&v.setup_network),
            agent: NetForm::from(&v.agent_network),
            cache_enabled: v.cache_enabled,
            cache_files: v.cache_files.join("\n"),
        }
    }

    /// Request body, or every problem found (German, ready to show).
    pub fn to_update(&self) -> Result<UpdateEnvironment, Vec<String>> {
        let mut errors = vec![];
        let timeout = match self.timeout.trim().parse::<i32>() {
            Ok(t) if (SETUP_TIMEOUT.0..=SETUP_TIMEOUT.1).contains(&t) => t,
            _ => {
                errors.push(format!(
                    "Zeitlimit: ganze Sekunden von {} bis {}",
                    SETUP_TIMEOUT.0, SETUP_TIMEOUT.1
                ));
                SETUP_TIMEOUT.0
            }
        };
        let setup = self.setup.policy("Setup-Netz", &mut errors);
        let agent = self.agent.policy("Agent-Netz", &mut errors);
        let vars: Vec<EnvVar> = self
            .vars
            .iter()
            .filter(|v| !(v.name.trim().is_empty() && v.value.is_empty()))
            .map(|v| EnvVar {
                name: v.name.trim().to_string(),
                value: v.value.clone(),
            })
            .collect();
        let u = UpdateEnvironment {
            setup_script: self.script.clone(),
            setup_timeout_secs: timeout,
            env_vars: vars,
            setup_network: setup,
            agent_network: agent,
            cache_enabled: self.cache_enabled,
            cache_files: self
                .cache_files
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        validate(&u).map_err(|e| vec![e])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::env::default_setup_network;

    fn view() -> EnvironmentView {
        EnvironmentView {
            setup_script: "npm ci".into(),
            setup_timeout_secs: 600,
            env_vars: vec![EnvVar {
                name: "NODE_ENV".into(),
                value: "ci".into(),
            }],
            secrets: vec![],
            setup_network: default_setup_network(),
            agent_network: NetworkPolicy::off(),
            cache_enabled: true,
            cache_files: vec![],
            updated_at: None,
            sandbox: SandboxInfo {
                engine: "none".into(),
                egress_enforced: false,
            },
            provider_hosts: vec!["api.anthropic.com".into()],
        }
    }

    #[test]
    fn roundtrip_and_normalisation() {
        let mut f = EnvForm::from_view(&view());
        f.agent.mode = NetMode::Allowlist;
        f.agent.domains = "Docs.rs\n\n docs.rs, *.example.com".into();
        f.agent.toggle_preset("pypi", true);
        f.vars.push(EnvVar {
            name: "  ".into(),
            value: String::new(),
        });
        f.cache_files = "web/package-lock.json\n\n".into();
        let u = f.to_update().unwrap();
        assert_eq!(u.agent_network.domains, vec!["docs.rs", "*.example.com"]);
        assert_eq!(u.agent_network.presets, vec!["pypi"]);
        assert_eq!(u.env_vars.len(), 1, "empty rows are dropped");
        assert_eq!(u.cache_files, vec!["web/package-lock.json"]);
        f.agent.toggle_preset("pypi", false);
        assert!(f.agent.presets.is_empty());
    }

    #[test]
    fn collects_errors() {
        let mut f = EnvForm::from_view(&view());
        f.timeout = "5".into();
        f.setup.domains = "https://x.com\nok.com".into();
        let e = f.to_update().unwrap_err();
        assert_eq!(e.len(), 2, "{e:?}");
        assert!(e[0].starts_with("Zeitlimit"));
        assert!(e[1].starts_with("Setup-Netz"));
        let mut f = EnvForm::from_view(&view());
        f.vars[0].name = "HTTPS_PROXY".into();
        assert!(f.to_update().unwrap_err()[0].contains("HTTPS_PROXY"));
    }

    #[test]
    fn texts_for_every_mode_and_status() {
        for p in [Phase::Setup, Phase::Agent] {
            for m in MODES {
                assert!(!mode_label(p, m).is_empty() && !mode_help(p, m).is_empty());
            }
        }
        assert_eq!(mode_label(Phase::Agent, NetMode::Off), "Nur Modell-API");
        let s = |engine: &str, e| SandboxInfo {
            engine: engine.into(),
            egress_enforced: e,
        };
        assert_eq!(sandbox_status(&s("podman", true)).0, "tone-success");
        assert!(sandbox_status(&s("none", false))
            .1
            .contains("nicht durchgesetzt"));
        assert_eq!(sandbox_status(&s("", false)).1, "Status unbekannt");
    }
}
