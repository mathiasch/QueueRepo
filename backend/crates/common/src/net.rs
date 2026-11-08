//! Network rules shared by the app (SSRF guard for its own outbound calls)
//! and the runner (egress proxy of agent runs): host patterns of an
//! allowlist and the address ranges that are never reachable.
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// Network policy of one run phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetMode {
    /// No network (except the [`EgressPolicy::always`] hosts of the run).
    #[default]
    Off,
    /// Only hosts matching a rule.
    Allowlist,
    /// Every public address (private/link-local ranges stay blocked).
    Open,
}

impl NetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            NetMode::Off => "off",
            NetMode::Allowlist => "allowlist",
            NetMode::Open => "open",
        }
    }
}

/// What the runner enforces for one process (`ExecRequest::egress`).
/// The default denies everything (fail-closed for callers that send none).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EgressPolicy {
    pub mode: NetMode,
    /// Host rules for [`NetMode::Allowlist`] ([`HostRule::parse`] syntax).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Hosts reachable in every mode and never inspected (the model API the
    /// agent CLI itself needs).
    #[serde(default)]
    pub always: Vec<String>,
    /// Only GET/HEAD/OPTIONS without body to `allow`/`open` targets (HTTPS
    /// is terminated with a per-run CA to see the method).
    #[serde(default)]
    pub read_only: bool,
}

impl EgressPolicy {
    /// Whether the run needs a proxy at all (otherwise: no network).
    pub fn needs_proxy(&self) -> bool {
        self.mode != NetMode::Off || !self.always.is_empty()
    }
}

/// Why the proxy allowed or refused a connection (`ExecFrame::Egress`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressVerdict {
    /// Allowed: model API of the run.
    Provider,
    /// Allowed: matches the allowlist.
    Allowlist,
    /// Allowed: phase is `open`.
    Open,
    /// Refused: the phase has no network.
    PhaseOff,
    /// Refused: host not in the allowlist.
    NotAllowed,
    /// Refused: the name resolves to a blocked address (private, link-local,
    /// metadata, the host itself, operator deny list).
    BlockedAddress,
    /// Refused: read-only phase and a method other than GET/HEAD/OPTIONS,
    /// a request body, an upgrade or a different Host header.
    ReadOnly,
    /// Refused: not a valid proxy request.
    BadRequest,
    /// Refused: name could not be resolved.
    Unresolvable,
    /// Allowed, but the upstream connection failed.
    ConnectFailed,
}

impl EgressVerdict {
    pub fn allowed(self) -> bool {
        matches!(
            self,
            EgressVerdict::Provider
                | EgressVerdict::Allowlist
                | EgressVerdict::Open
                | EgressVerdict::ConnectFailed
        )
    }
}

/// One allowlist entry: `example.com`, `*.example.com` (subdomains only),
/// an IPv4/IPv6 literal, each optionally with `:port` (IPv6 as `[::1]:443`).
/// Without a port, 443 and 80 match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    pattern: Pattern,
    port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    Exact(String),
    /// `*.example.com` stored as `.example.com`.
    Suffix(String),
    Ip(IpAddr),
}

/// Lower-cases a host name, drops one trailing dot and checks the syntax
/// (letters, digits, `-`, `_` in labels of ≤ 63, total ≤ 253, at least one
/// dot or `localhost`). IP literals are returned unchanged.
pub fn normalize_host(host: &str) -> Option<String> {
    let h = host.trim().trim_matches(['[', ']']);
    if let Ok(ip) = h.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    let h = h.strip_suffix('.').unwrap_or(h).to_ascii_lowercase();
    if h.is_empty() || h.len() > 253 {
        return None;
    }
    let labels_ok = h.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    (labels_ok && (h.contains('.') || h == "localhost")).then_some(h)
}

impl HostRule {
    pub fn parse(raw: &str) -> Result<HostRule, String> {
        let s = raw.trim();
        if s.is_empty() {
            return Err("leerer Eintrag".into());
        }
        if s.contains("://") || s.contains('/') {
            return Err(format!("„{s}“: nur Hostnamen, ohne https:// und Pfad"));
        }
        let (host, port) = split_port(s).ok_or_else(|| format!("„{s}“: ungültiger Port"))?;
        if port == Some(0) {
            return Err(format!("„{s}“: ungültiger Port"));
        }
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            return Ok(HostRule {
                pattern: Pattern::Ip(ip),
                port,
            });
        }
        let pattern = match host.strip_prefix("*.") {
            Some(rest) => {
                let n = normalize_host(rest).ok_or_else(|| format!("„{s}“: ungültiger Name"))?;
                if !n.contains('.') {
                    return Err(format!("„{s}“: Platzhalter nur unterhalb einer Domain"));
                }
                Pattern::Suffix(format!(".{n}"))
            }
            None if host.contains('*') => {
                return Err(format!("„{s}“: Platzhalter nur als *.domain.tld"))
            }
            None => Pattern::Exact(
                normalize_host(host).ok_or_else(|| format!("„{s}“: ungültiger Name"))?,
            ),
        };
        Ok(HostRule { pattern, port })
    }

    /// `host` must be normalized ([`normalize_host`]).
    pub fn matches(&self, host: &str, port: u16) -> bool {
        let port_ok = match self.port {
            Some(p) => p == port,
            None => port == 443 || port == 80,
        };
        port_ok
            && match &self.pattern {
                Pattern::Exact(h) => h == host,
                Pattern::Suffix(s) => host.ends_with(s.as_str()),
                Pattern::Ip(ip) => host.parse::<IpAddr>().is_ok_and(|h| h == *ip),
            }
    }

    /// Like [`HostRule::matches`], but a rule without port matches every port
    /// (operator lists of internal hosts).
    pub fn matches_host(&self, host: &str, port: u16) -> bool {
        match self.port {
            Some(_) => self.matches(host, port),
            None => HostRule {
                pattern: self.pattern.clone(),
                port: Some(port),
            }
            .matches(host, port),
        }
    }

    /// Canonical spelling (for storage and display).
    pub fn canonical(&self) -> String {
        let host = match &self.pattern {
            Pattern::Exact(h) => h.clone(),
            Pattern::Suffix(s) => format!("*{s}"),
            Pattern::Ip(IpAddr::V6(v6)) if self.port.is_some() => format!("[{v6}]"),
            Pattern::Ip(ip) => ip.to_string(),
        };
        match self.port {
            Some(p) => format!("{host}:{p}"),
            None => host,
        }
    }
}

/// Splits `host[:port]` / `[v6]:port`; a bare IPv6 literal has no port.
/// `None` = malformed port.
pub fn split_port(s: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']')?;
        return match tail {
            "" => Some((h, None)),
            t => Some((h, Some(t.strip_prefix(':')?.parse().ok()?))),
        };
    }
    match s.matches(':').count() {
        0 => Some((s, None)),
        1 => {
            let (h, p) = s.split_once(':')?;
            Some((h, Some(p.parse().ok()?)))
        }
        // Bare IPv6 literal without port.
        _ => Some((s, None)),
    }
}

/// Parses rules, failing on the first invalid one.
pub fn parse_rules(raw: &[String]) -> Result<Vec<HostRule>, String> {
    raw.iter().map(|r| HostRule::parse(r)).collect()
}

pub fn any_match(rules: &[HostRule], host: &str, port: u16) -> bool {
    rules.iter().any(|r| r.matches(host, port))
}

/// Addresses no outbound call may reach. Link-local (incl. the cloud
/// metadata service 169.254.169.254), unspecified, multicast and broadcast
/// are always refused; loopback, private networks (RFC 1918, ULA) and CGNAT
/// only unless `allow_private`. IPv4-mapped IPv6 is checked as IPv4.
pub fn forbidden_ip(ip: IpAddr, allow_private: bool) -> Option<&'static str> {
    let ip = match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v) => {
            if v.is_unspecified() || v.is_link_local() || v.is_multicast() || v.is_broadcast() {
                return Some("Link-Local/Metadaten- oder Sonderadresse");
            }
            let o = v.octets();
            let cgnat = o[0] == 100 && (64..128).contains(&o[1]);
            // 0.0.0.0/8 ("this network") reaches the local host on Linux.
            if !allow_private && (v.is_loopback() || v.is_private() || cgnat || o[0] == 0) {
                return Some("interne Adresse");
            }
        }
        IpAddr::V6(v) => {
            let seg = v.segments();
            if v.is_unspecified() || v.is_multicast() || (seg[0] & 0xffc0) == 0xfe80 {
                return Some("Link-Local- oder Sonderadresse");
            }
            if !allow_private && (v.is_loopback() || (seg[0] & 0xfe00) == 0xfc00) {
                return Some("interne Adresse");
            }
        }
    }
    None
}

/// An IPv4/IPv6 network in CIDR notation (`10.1.2.0/24`, `2001:db8::/32`,
/// a bare address = single host).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Cidr> {
        let (a, p) = match s.trim().split_once('/') {
            Some((a, p)) => (a, Some(p.parse::<u8>().ok()?)),
            None => (s.trim(), None),
        };
        let net: IpAddr = a.parse().ok()?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let prefix = p.unwrap_or(max);
        (prefix <= max).then_some(Cidr { net, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) if self.net.is_ipv4() => match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => return false,
            },
            o => o,
        };
        match (self.net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_rules_parse_and_match() {
        let r = HostRule::parse("Registry.NPMJS.org.").unwrap();
        assert_eq!(r.canonical(), "registry.npmjs.org");
        assert!(r.matches("registry.npmjs.org", 443));
        assert!(r.matches("registry.npmjs.org", 80));
        assert!(!r.matches("registry.npmjs.org", 8443));
        assert!(!r.matches("evil-registry.npmjs.org", 443));
        assert!(!r.matches("registry.npmjs.org.evil.com", 443));

        let w = HostRule::parse("*.githubusercontent.com").unwrap();
        assert!(w.matches("raw.githubusercontent.com", 443));
        assert!(w.matches("a.b.githubusercontent.com", 443));
        assert!(
            !w.matches("githubusercontent.com", 443),
            "apex not included"
        );
        assert!(!w.matches("evilgithubusercontent.com", 443));

        let p = HostRule::parse("nexus.example.com:8443").unwrap();
        assert!(p.matches("nexus.example.com", 8443));
        assert!(!p.matches("nexus.example.com", 443));
        assert_eq!(p.canonical(), "nexus.example.com:8443");

        let ip = HostRule::parse("203.0.113.7").unwrap();
        assert!(ip.matches("203.0.113.7", 443));
        let ip6 = HostRule::parse("[2001:db8::1]:8080").unwrap();
        assert!(ip6.matches("2001:db8::1", 8080));
        assert_eq!(ip6.canonical(), "[2001:db8::1]:8080");
        assert!(HostRule::parse("2001:db8::1")
            .unwrap()
            .matches("2001:db8::1", 443));
        assert!(HostRule::parse("localhost").is_ok());
        let any = HostRule::parse("127.0.0.1").unwrap();
        assert!(any.matches_host("127.0.0.1", 18_123) && !any.matches("127.0.0.1", 18_123));
        assert!(!p.matches_host("nexus.example.com", 443));
        assert_eq!(split_port("[::1]:80"), Some(("::1", Some(80))));
        assert_eq!(split_port("x.com:"), None);
    }

    #[test]
    fn host_rules_reject_garbage() {
        for bad in [
            "",
            "https://x.com",
            "x.com/path",
            "*",
            "*.com",
            "a*.x.com",
            "x.*.com",
            "x.com:0",
            "x.com:99999",
            "x.com:abc",
            "-x.com",
            "x..com",
            "nodot",
            "x y.com",
            &format!("{}.com", "a".repeat(64)),
        ] {
            assert!(HostRule::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn normalizes_hosts() {
        assert_eq!(
            normalize_host("API.Example.COM.").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(normalize_host("[::1]").as_deref(), Some("::1"));
        assert_eq!(normalize_host("bad host"), None);
        assert_eq!(normalize_host(""), None);
    }

    #[test]
    fn forbidden_ranges() {
        let f = |s: &str, p| forbidden_ip(s.parse().unwrap(), p).is_some();
        for always in [
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "fe80::1",
            "::",
            "ff02::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(f(always, false) && f(always, true), "{always}");
        }
        for private in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "0.1.2.3",
            "::1",
            "fd00::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(f(private, false) && !f(private, true), "{private}");
        }
        for public in ["1.1.1.1", "203.0.113.7", "2606:4700::1111", "100.128.0.1"] {
            assert!(!f(public, false), "{public}");
        }
    }

    #[test]
    fn cidrs() {
        let c = Cidr::parse("10.1.2.0/24").unwrap();
        assert!(c.contains("10.1.2.200".parse().unwrap()));
        assert!(!c.contains("10.1.3.1".parse().unwrap()));
        assert!(c.contains("::ffff:10.1.2.3".parse().unwrap()));
        let h = Cidr::parse("198.51.100.4").unwrap();
        assert!(h.contains("198.51.100.4".parse().unwrap()));
        assert!(!h.contains("198.51.100.5".parse().unwrap()));
        let v6 = Cidr::parse("2001:db8::/32").unwrap();
        assert!(v6.contains("2001:db8:1::1".parse().unwrap()));
        assert!(!v6.contains("2001:db9::1".parse().unwrap()));
        assert!(Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains("8.8.8.8".parse().unwrap()));
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("nope").is_none());
    }

    #[test]
    fn default_policy_is_fail_closed() {
        let p: EgressPolicy = serde_json::from_str("{\"mode\":\"off\"}").unwrap();
        assert_eq!(p, EgressPolicy::default());
        assert!(!p.needs_proxy());
        assert!(EgressPolicy {
            always: vec!["api.anthropic.com".into()],
            ..Default::default()
        }
        .needs_proxy());
        assert!(EgressVerdict::Provider.allowed() && !EgressVerdict::NotAllowed.allowed());
    }
}
