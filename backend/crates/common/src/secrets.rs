//! Secret scanner (WP03): own rules for common token formats plus an
//! entropy check for generic assignments. Used by the runner (diffs before
//! commit/push, leftovers) and by the app (events, results, diff views).
//!
//! Hits never leave this module in clear text: callers get the position (to
//! redact), a masked preview (fixed prefix + length) and an HMAC fingerprint
//! (to remember an approval without storing the value).
use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// One detection rule.
#[derive(Debug)]
pub struct Rule {
    pub id: &'static str,
    /// German label for the UI.
    pub label: &'static str,
    /// Known, non-secret prefix shown in masked previews (characters).
    prefix: usize,
    /// Fixed token formats (low false-positive rate). Only these hold a task
    /// when they appear in the agent's log; all rules redact and gate diffs.
    pub high_confidence: bool,
    re: &'static str,
    /// Capture group holding the secret value (0 = whole match).
    group: usize,
    /// Post-filter on the value.
    check: Check,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Check {
    /// Fixed format: only obvious documentation values are dropped.
    Format,
    /// Free-form value: placeholders dropped, entropy and character mix required.
    Entropy,
}

const RULES: &[Rule] = &[
    Rule {
        id: "private_key",
        label: "Privater Schlüssel",
        prefix: 11,
        high_confidence: true,
        re: r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----(?:\s*[A-Za-z0-9+/=]{16,})?",
        group: 0,
        check: Check::Format,
    },
    Rule {
        id: "aws_access_key",
        label: "AWS-Zugangsschlüssel",
        prefix: 4,
        high_confidence: true,
        re: r"\b((?:AKIA|ASIA|ABIA|ACCA)[A-Z0-9]{16})\b",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "github_token",
        label: "GitHub-Token",
        prefix: 4,
        high_confidence: true,
        re: r"\b((?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,255})\b",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "github_pat",
        label: "GitHub-Token",
        prefix: 11,
        high_confidence: true,
        re: r"\b(github_pat_[A-Za-z0-9_]{22,255})\b",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "gitlab_token",
        label: "GitLab-Token",
        prefix: 6,
        high_confidence: true,
        re: r"\b(glpat-[A-Za-z0-9_\-]{20,255})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "slack_token",
        label: "Slack-Token",
        prefix: 5,
        high_confidence: true,
        re: r"\b(xox[abposr]-[A-Za-z0-9\-]{10,255})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "slack_webhook",
        label: "Slack-Webhook",
        prefix: 33,
        high_confidence: true,
        re: r"(https://hooks\.slack\.com/services/[A-Za-z0-9/_]{20,255})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "stripe_live",
        label: "Stripe-Schlüssel",
        prefix: 8,
        high_confidence: true,
        re: r"\b((?:sk|rk)_live_[A-Za-z0-9]{20,255})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "google_api_key",
        label: "Google-API-Schlüssel",
        prefix: 4,
        high_confidence: true,
        re: r"\b(AIza[0-9A-Za-z_\-]{35})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "anthropic_key",
        label: "Anthropic-API-Schlüssel",
        prefix: 7,
        high_confidence: true,
        re: r"\b(sk-ant-[A-Za-z0-9_\-]{20,255})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "openai_key",
        label: "OpenAI-API-Schlüssel",
        prefix: 3,
        high_confidence: true,
        re: r"\b(sk-(?:proj|svcacct|admin)-[A-Za-z0-9_\-]{20,255}|sk-[A-Za-z0-9]{20}T3BlbkFJ[A-Za-z0-9]{20})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "npm_token",
        label: "npm-Token",
        prefix: 4,
        high_confidence: true,
        re: r"\b(npm_[A-Za-z0-9]{36})\b",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "pypi_token",
        label: "PyPI-Token",
        prefix: 5,
        high_confidence: true,
        re: r"\b(pypi-AgEIcHlwaS5vcmc[A-Za-z0-9_\-]{50,})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "jwt",
        label: "JSON Web Token",
        prefix: 3,
        high_confidence: true,
        re: r"\b(eyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,})",
        group: 1,
        check: Check::Format,
    },
    Rule {
        id: "url_password",
        label: "Passwort in URL",
        prefix: 0,
        high_confidence: false,
        re: r#"\b[a-zA-Z][a-zA-Z0-9+.\-]{1,20}://[^\s:/@"'<>]{1,64}:([^\s:/@"'<>]{4,128})@[^\s/"'<>]+"#,
        group: 1,
        check: Check::Entropy,
    },
    Rule {
        id: "generic_assignment",
        label: "Mögliches Geheimnis (Zuweisung)",
        prefix: 0,
        high_confidence: false,
        re: r#"(?i)\b[a-z0-9_.\-]*(?:password|passwd|secret|token|api[_\-]?key|access[_\-]?key|private[_\-]?key|auth[_\-]?key|credential)[a-z0-9_.\-]*["']?\s*(?::=|=>|=|:)\s*["']([^"'\s]{10,200})["']"#,
        group: 1,
        check: Check::Entropy,
    },
    Rule {
        id: "generic_env",
        label: "Mögliches Geheimnis (Umgebungsvariable)",
        prefix: 0,
        high_confidence: false,
        re: r"(?m)^\s*(?:export\s+)?[A-Z0-9_]*(?:PASSWORD|PASSWD|SECRET|TOKEN|API_KEY|APIKEY|ACCESS_KEY|PRIVATE_KEY|AUTH_KEY|CREDENTIAL)[A-Z0-9_]*\s*=\s*([^\s\x22'#$]{10,200})",
        group: 1,
        check: Check::Entropy,
    },
];

fn compiled() -> &'static Vec<Regex> {
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        RULES
            .iter()
            .map(|r| Regex::new(r.re).expect("static secret rule"))
            .collect()
    })
}

/// Rule by id (for labels of stored findings).
pub fn rule(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.id == id)
}

/// One detected secret: byte range of the value inside the scanned text.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub rule: &'static str,
    pub start: usize,
    pub end: usize,
}

impl Hit {
    pub fn value<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start..self.end]
    }

    pub fn label(&self) -> &'static str {
        rule(self.rule).map_or("Geheimnis", |r| r.label)
    }

    pub fn high_confidence(&self) -> bool {
        rule(self.rule).is_some_and(|r| r.high_confidence)
    }

    /// Preview without the secret: label, the fixed format prefix (if the
    /// rule has one) and the length, e.g. `GitHub-Token ghp_… (40 Zeichen)`.
    pub fn masked(&self, text: &str) -> String {
        let v = self.value(text);
        let n = v.chars().count();
        let keep = rule(self.rule).map_or(0, |r| r.prefix).min(n / 3);
        let shown: String = v.chars().take(keep).collect();
        if shown.is_empty() {
            format!("{} ({n} Zeichen)", self.label())
        } else {
            format!("{} {shown}… ({n} Zeichen)", self.label())
        }
    }
}

/// Shannon entropy in bits per character.
pub fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    let mut n = 0usize;
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n as f64;
            -p * p.log2()
        })
        .sum()
}

fn char_classes(s: &str) -> usize {
    let lower = s.chars().any(|c| c.is_ascii_lowercase());
    let upper = s.chars().any(|c| c.is_ascii_uppercase());
    let digit = s.chars().any(|c| c.is_ascii_digit());
    let other = s.chars().any(|c| !c.is_ascii_alphanumeric());
    [lower, upper, digit, other].iter().filter(|b| **b).count()
}

/// Obvious placeholders and references instead of values.
fn is_placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    const WORDS: [&str; 12] = [
        "example",
        "xxxx",
        "your",
        "changeme",
        "placeholder",
        "dummy",
        "sample",
        "redacted",
        "geschwärzt",
        "process.env",
        "os.environ",
        "getenv",
    ];
    WORDS.iter().any(|w| l.contains(w))
        || v.starts_with('$')
        || v.contains("${")
        || v.contains("{{")
        || v.contains("$(")
        || (v.starts_with('<') && v.ends_with('>'))
        || v.chars()
            .all(|c| c == v.chars().next().unwrap_or('x') || c == '-' || c == '_')
}

fn accept(check: Check, v: &str) -> bool {
    match check {
        // AWS documentation keys end in EXAMPLE; all-same filler is never real.
        Check::Format => !v.ends_with("EXAMPLE") && !is_placeholder_filler(v),
        Check::Entropy => {
            v.chars().count() >= 8
                && !is_placeholder(v)
                && char_classes(v) >= 2
                && entropy(v) >= 3.0
        }
    }
}

fn is_placeholder_filler(v: &str) -> bool {
    // After the fixed prefix only one repeated character (e.g. `ghp_xxxx…`).
    let tail: Vec<char> = v.chars().rev().take(16).collect();
    tail.len() >= 8 && tail.iter().all(|c| *c == tail[0])
}

/// All hits in `text`, ordered by position, without overlaps (the first
/// rule in [`RULES`] order wins on equal start, so specific formats beat the
/// generic assignment rule).
pub fn scan(text: &str) -> Vec<Hit> {
    let mut hits: Vec<(usize, Hit)> = vec![];
    for (i, (rule, re)) in RULES.iter().zip(compiled()).enumerate() {
        for caps in re.captures_iter(text) {
            let Some(m) = caps.get(rule.group) else {
                continue;
            };
            if !accept(rule.check, m.as_str()) {
                continue;
            }
            hits.push((
                i,
                Hit {
                    rule: rule.id,
                    start: m.start(),
                    end: m.end(),
                },
            ));
        }
    }
    hits.sort_by_key(|(i, h)| (h.start, *i));
    let mut out: Vec<Hit> = vec![];
    for (_, h) in hits {
        match out.last_mut() {
            Some(last) if h.start < last.end => {
                // Overlap: keep the earlier one, extend to cover both.
                last.end = last.end.max(h.end);
            }
            _ => out.push(h),
        }
    }
    out
}

/// Only rules with fixed formats (log scanning that may hold a task).
pub fn scan_high_confidence(text: &str) -> Vec<Hit> {
    scan(text)
        .into_iter()
        .filter(Hit::high_confidence)
        .collect()
}

/// Replaces every hit with `[geschwärzt: <Art>]`.
pub fn redact(text: &str) -> String {
    let hits = scan(text);
    if hits.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for h in &hits {
        out.push_str(&text[pos..h.start]);
        out.push_str(&format!("[geschwärzt: {}]", h.label()));
        pos = h.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// HMAC-SHA256(key, rule ‖ 0 ‖ value), first 16 bytes as hex. Identifies an
/// approved value without storing it (and without a plain hash that would
/// allow guessing short passwords offline).
pub fn fingerprint(key: &[u8], rule: &str, value: &str) -> String {
    const BLOCK: usize = 64;
    let mut k = if key.len() > BLOCK {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    k.resize(BLOCK, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(rule.as_bytes())
        .chain_update([0u8])
        .chain_update(value.as_bytes())
        .finalize();
    let outer = Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize();
    outer[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Test values assembled at runtime from harmless parts (no real secret, and
/// no complete token literal in the source). Deterministic per `seed`.
#[doc(hidden)]
pub mod testdata {
    fn filler(seed: u64, n: usize) -> String {
        const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
        let mut x = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                A[(x % A.len() as u64) as usize] as char
            })
            .collect()
    }

    pub fn github(seed: u64) -> String {
        format!("{}{}", ["gh", "p_"].concat(), filler(seed, 36))
    }

    pub fn aws(seed: u64) -> String {
        format!(
            "{}{}",
            ["AK", "IA"].concat(),
            filler(seed, 16).to_ascii_uppercase()
        )
    }

    pub fn anthropic(seed: u64) -> String {
        format!("{}{}", ["sk-", "ant-"].concat(), filler(seed, 40))
    }

    pub fn generic(seed: u64) -> String {
        format!("{}9{}", filler(seed, 12), filler(seed + 1, 8))
    }
}

#[cfg(test)]
mod tests {
    use super::testdata::*;
    use super::*;

    #[test]
    fn rules_compile_and_ids_are_unique() {
        assert_eq!(compiled().len(), RULES.len());
        let mut ids: Vec<_> = RULES.iter().map(|r| r.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), RULES.len());
    }

    #[test]
    fn detects_fixed_formats() {
        let gh = github(1);
        let aws = aws(2);
        let ant = anthropic(3);
        let text = format!("token: {gh}\nkey={aws} and {ant}");
        let hits = scan(&text);
        let rules: Vec<_> = hits.iter().map(|h| h.rule).collect();
        assert_eq!(
            rules,
            vec!["github_token", "aws_access_key", "anthropic_key"]
        );
        assert_eq!(hits[0].value(&text), gh);
        assert!(hits.iter().all(Hit::high_confidence));
        let pk = format!(
            "-----BEGIN {}PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0B",
            "RSA "
        );
        let h = scan(&pk);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].rule, "private_key");
        // The fingerprint covers the key body, not only the header.
        let other = pk.replace("MIIEvQ", "MIIEvR");
        let k = b"k";
        assert_ne!(
            fingerprint(k, "private_key", h[0].value(&pk)),
            fingerprint(k, "private_key", scan(&other)[0].value(&other))
        );
    }

    #[test]
    fn generic_assignments_need_entropy() {
        let v = generic(7);
        for line in [
            format!("password = \"{v}\""),
            format!("  \"apiKey\": \"{v}\","),
            format!("DB_PASSWORD={v}"),
            format!("export GITHUB_TOKEN={v}"),
            format!("let secret_key = '{v}';"),
        ] {
            let h = scan(&line);
            assert_eq!(h.len(), 1, "{line}");
            assert_eq!(h[0].value(&line), v);
            assert!(!h[0].high_confidence());
        }
        for clean in [
            "password = \"changeme-please\"",
            "token = \"${GITHUB_TOKEN}\"",
            "api_key = process.env.API_KEY",
            "password = \"aaaaaaaaaaaaaa\"",
            "secret = \"<your-secret>\"",
            "token = \"short\"",
            "DB_PASSWORD=$DB_PASSWORD",
            "password = \"passwordpassword\"",
            "author = \"someone@example.org\"",
            "let tokens = count_tokens(input);",
        ] {
            assert!(scan(clean).is_empty(), "false positive: {clean}");
        }
    }

    #[test]
    fn documentation_values_and_fillers_are_ignored() {
        let doc = ["AKIA", "IOSFODNN7", "EXAMPLE"].concat();
        assert!(scan(&doc).is_empty());
        let filler = format!("{}{}", ["gh", "p_"].concat(), "x".repeat(36));
        assert!(scan(&filler).is_empty());
    }

    #[test]
    fn url_passwords() {
        let pw = generic(11);
        let t = format!("DATABASE_URL=postgres://app:{pw}@db.internal:5432/x");
        let h = scan(&t);
        assert!(h.iter().any(|h| h.value(&t) == pw));
        // Typical development defaults are not flagged.
        assert!(scan("postgres://queued:queued@localhost/queued").is_empty());
        assert!(scan("https://user:${PASS}@host/x").is_empty());
    }

    #[test]
    fn redaction_and_masking_never_show_the_value() {
        let gh = github(5);
        let text = format!("echo {gh} done");
        let r = redact(&text);
        assert_eq!(r, "echo [geschwärzt: GitHub-Token] done");
        let h = &scan(&text)[0];
        let m = h.masked(&text);
        assert_eq!(m, "GitHub-Token ghp_… (40 Zeichen)");
        assert!(!m.contains(&gh[4..10]));
        let v = generic(3);
        let t2 = format!("password=\"{v}\"");
        let m2 = scan(&t2)[0].masked(&t2);
        assert!(!m2.contains(&v[..3]), "{m2}");
        assert_eq!(redact("nothing here"), "nothing here");
        // Overlapping rules collapse into one redaction.
        let ant = anthropic(9);
        let t3 = format!("api_key = \"{ant}\"");
        assert_eq!(scan(&t3).len(), 1);
        assert_eq!(scan(&t3)[0].rule, "anthropic_key");
        assert!(!redact(&t3).contains(&ant));
    }

    #[test]
    fn fingerprint_is_keyed_and_stable() {
        let a = fingerprint(b"key-a", "github_token", "v");
        assert_eq!(a, fingerprint(b"key-a", "github_token", "v"));
        assert_ne!(a, fingerprint(b"key-b", "github_token", "v"));
        assert_ne!(a, fingerprint(b"key-a", "jwt", "v"));
        assert_eq!(a.len(), 32);
        assert!(fingerprint(&[7u8; 100], "r", "v").len() == 32);
    }

    #[test]
    fn entropy_values() {
        assert_eq!(entropy(""), 0.0);
        assert_eq!(entropy("aaaa"), 0.0);
        assert!((entropy("ab") - 1.0).abs() < 1e-9);
        assert!(entropy(&generic(1)) > 3.5);
    }

    #[test]
    fn scanning_is_utf8_safe() {
        let gh = github(2);
        let t = format!("ünïcödé → {gh} ✓ password=\"äöü\"");
        let r = redact(&t);
        assert!(r.contains("[geschwärzt: GitHub-Token]"));
        assert!(r.starts_with("ünïcödé → "));
    }
}
