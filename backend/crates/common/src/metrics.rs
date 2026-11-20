//! Minimal Prometheus registry (text exposition format 0.0.4) shared by the
//! app and the runner (WP10). No dependency: counters, gauges and
//! histograms with a fixed set of label names, plus the access rules of the
//! `/metrics` endpoints.
//!
//! Cardinality is bounded twice: callers only pass values from fixed sets
//! (provider, status, route pattern, …), and every family keeps at most
//! [`MAX_SERIES`] label combinations – anything beyond is folded into one
//! series whose labels are all [`OVERFLOW`].
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

/// Most label combinations per family.
pub const MAX_SERIES: usize = 500;
/// Label value of the overflow series.
pub const OVERFLOW: &str = "_other";
/// `Content-Type` of the exposition.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
/// Default buckets for durations in seconds (HTTP requests).
pub const HTTP_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
/// Buckets for agent runs and runner processes (seconds up to 4 h).
pub const RUN_BUCKETS: &[f64] = &[
    1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1200.0, 1800.0, 3600.0, 7200.0, 14400.0,
];

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Escapes a label value (`\`, `"`, line feed).
pub fn escape_label(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Escapes a HELP text (`\`, line feed).
pub fn escape_help(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\n', "\\n")
}

/// Prometheus number formatting (`+Inf`, `-Inf`, `NaN`).
pub fn fmt_value(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v == f64::INFINITY {
        "+Inf".into()
    } else if v == f64::NEG_INFINITY {
        "-Inf".into()
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn label_str(names: &[&str], values: &[String], extra: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = names
        .iter()
        .zip(values)
        .map(|(n, v)| format!("{n}=\"{}\"", escape_label(v)))
        .collect();
    if let Some((n, v)) = extra {
        parts.push(format!("{n}=\"{}\"", escape_label(v)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

/// Picks the key for `values`, folding new combinations into the overflow
/// series once the family is full.
fn key_for<V>(map: &BTreeMap<Vec<String>, V>, names: usize, values: &[&str]) -> Vec<String> {
    debug_assert_eq!(values.len(), names, "label count mismatch");
    let k: Vec<String> = (0..names)
        .map(|i| values.get(i).copied().unwrap_or("").to_string())
        .collect();
    if map.contains_key(&k) || map.len() < MAX_SERIES {
        k
    } else {
        vec![OVERFLOW.to_string(); names]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Counter,
    Gauge,
}

/// Counter or gauge with labels.
pub struct Family {
    name: &'static str,
    help: &'static str,
    labels: &'static [&'static str],
    kind: Kind,
    values: Mutex<BTreeMap<Vec<String>, f64>>,
}

impl Family {
    /// Adds `v` (counters ignore negative values).
    pub fn add(&self, labels: &[&str], v: f64) {
        if self.kind == Kind::Counter && (v < 0.0 || !v.is_finite()) {
            return;
        }
        let mut m = lock(&self.values);
        let k = key_for(&m, self.labels.len(), labels);
        *m.entry(k).or_insert(0.0) += v;
    }

    pub fn inc(&self, labels: &[&str]) {
        self.add(labels, 1.0);
    }

    /// Sets a gauge value.
    pub fn set(&self, labels: &[&str], v: f64) {
        let mut m = lock(&self.values);
        let k = key_for(&m, self.labels.len(), labels);
        m.insert(k, v);
    }

    /// Replaces all series of a gauge (values read from a database).
    pub fn replace(&self, rows: Vec<(Vec<String>, f64)>) {
        let mut m = lock(&self.values);
        m.clear();
        for (labels, v) in rows {
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let k = key_for(&m, self.labels.len(), &refs);
            *m.entry(k).or_insert(0.0) += v;
        }
    }

    pub fn get(&self, labels: &[&str]) -> f64 {
        let k: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
        lock(&self.values).get(&k).copied().unwrap_or(0.0)
    }

    fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, escape_help(self.help));
        let _ = writeln!(
            out,
            "# TYPE {} {}",
            self.name,
            match self.kind {
                Kind::Counter => "counter",
                Kind::Gauge => "gauge",
            }
        );
        for (k, v) in lock(&self.values).iter() {
            let _ = writeln!(
                out,
                "{}{} {}",
                self.name,
                label_str(self.labels, k, None),
                fmt_value(*v)
            );
        }
    }
}

#[derive(Default, Clone)]
struct HistData {
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

/// Histogram with labels (cumulative buckets are computed when rendering).
pub struct Histogram {
    name: &'static str,
    help: &'static str,
    labels: &'static [&'static str],
    buckets: &'static [f64],
    values: Mutex<BTreeMap<Vec<String>, HistData>>,
}

impl Histogram {
    pub fn observe(&self, labels: &[&str], v: f64) {
        if !v.is_finite() {
            return;
        }
        let mut m = lock(&self.values);
        let k = key_for(&m, self.labels.len(), labels);
        let d = m.entry(k).or_insert_with(|| HistData {
            counts: vec![0; self.buckets.len()],
            ..Default::default()
        });
        if let Some(i) = self.buckets.iter().position(|b| v <= *b) {
            d.counts[i] += 1;
        }
        d.sum += v;
        d.count += 1;
    }

    pub fn count(&self, labels: &[&str]) -> u64 {
        let k: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
        lock(&self.values).get(&k).map(|d| d.count).unwrap_or(0)
    }

    fn render(&self, out: &mut String) {
        let _ = writeln!(out, "# HELP {} {}", self.name, escape_help(self.help));
        let _ = writeln!(out, "# TYPE {} histogram", self.name);
        for (k, d) in lock(&self.values).iter() {
            let mut cum = 0;
            for (i, b) in self.buckets.iter().enumerate() {
                cum += d.counts[i];
                let _ = writeln!(
                    out,
                    "{}_bucket{} {cum}",
                    self.name,
                    label_str(self.labels, k, Some(("le", &fmt_value(*b))))
                );
            }
            let _ = writeln!(
                out,
                "{}_bucket{} {}",
                self.name,
                label_str(self.labels, k, Some(("le", "+Inf"))),
                d.count
            );
            let ls = label_str(self.labels, k, None);
            let _ = writeln!(out, "{}_sum{ls} {}", self.name, fmt_value(d.sum));
            let _ = writeln!(out, "{}_count{ls} {}", self.name, d.count);
        }
    }
}

enum Entry {
    Family(Arc<Family>),
    Histogram(Arc<Histogram>),
}

/// A set of metric families, rendered in registration order.
#[derive(Default)]
pub struct Registry {
    entries: Mutex<Vec<Entry>>,
}

impl Registry {
    fn family(
        &self,
        name: &'static str,
        help: &'static str,
        labels: &'static [&'static str],
        kind: Kind,
    ) -> Arc<Family> {
        let f = Arc::new(Family {
            name,
            help,
            labels,
            kind,
            values: Mutex::new(BTreeMap::new()),
        });
        lock(&self.entries).push(Entry::Family(f.clone()));
        f
    }

    pub fn counter(
        &self,
        name: &'static str,
        help: &'static str,
        labels: &'static [&'static str],
    ) -> Arc<Family> {
        self.family(name, help, labels, Kind::Counter)
    }

    pub fn gauge(
        &self,
        name: &'static str,
        help: &'static str,
        labels: &'static [&'static str],
    ) -> Arc<Family> {
        self.family(name, help, labels, Kind::Gauge)
    }

    pub fn histogram(
        &self,
        name: &'static str,
        help: &'static str,
        labels: &'static [&'static str],
        buckets: &'static [f64],
    ) -> Arc<Histogram> {
        let h = Arc::new(Histogram {
            name,
            help,
            labels,
            buckets,
            values: Mutex::new(BTreeMap::new()),
        });
        lock(&self.entries).push(Entry::Histogram(h.clone()));
        h
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for e in lock(&self.entries).iter() {
            match e {
                Entry::Family(f) => f.render(&mut out),
                Entry::Histogram(h) => h.render(&mut out),
            }
        }
        out
    }
}

/// Status class of an HTTP status code (`2xx` …) – never the exact code
/// combined with a route (bounded series).
pub fn status_class(code: u16) -> &'static str {
    match code {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

/// HTTP methods as label values (anything else is `other`).
pub fn method_label(m: &str) -> &'static str {
    match m {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        _ => "other",
    }
}

/// Who may read `/metrics` (env `<PREFIX>METRICS_TOKEN`, `<PREFIX>METRICS_BIND`).
#[derive(Clone, Default, PartialEq)]
pub struct Access {
    /// Bearer token; required on the main listener and on a non-loopback
    /// metrics listener.
    pub token: Option<String>,
    /// Own listener that serves only `/metrics`.
    pub bind: Option<SocketAddr>,
}

impl std::fmt::Debug for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Access")
            .field("token", &self.token.as_ref().map(|_| "***"))
            .field("bind", &self.bind)
            .finish()
    }
}

/// Shortest metrics token.
pub const MIN_TOKEN_LEN: usize = 24;

impl Access {
    /// Validates the operator settings. `forbidden_token` is a secret that
    /// must not double as the metrics token (the runner's exec token).
    pub fn parse(
        token: Option<String>,
        bind: Option<String>,
        prefix: &str,
        forbidden_token: Option<&str>,
    ) -> Result<Self, String> {
        let token = token
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        if let Some(t) = &token {
            if t.len() < MIN_TOKEN_LEN {
                return Err(format!(
                    "{prefix}METRICS_TOKEN must be at least {MIN_TOKEN_LEN} characters"
                ));
            }
            if forbidden_token.is_some_and(|f| f == t) {
                return Err(format!(
                    "{prefix}METRICS_TOKEN must differ from {prefix}TOKEN (Prometheus must never hold the token that starts processes)"
                ));
            }
        }
        let bind = match bind.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) {
            None => None,
            Some(b) => Some(
                b.parse::<SocketAddr>()
                    .map_err(|_| format!("{prefix}METRICS_BIND must be ip:port, got {b:?}"))?,
            ),
        };
        if let Some(b) = bind {
            if !b.ip().is_loopback() && token.is_none() {
                return Err(format!(
                    "{prefix}METRICS_BIND {b} is not a loopback address: set {prefix}METRICS_TOKEN as well"
                ));
            }
        }
        Ok(Self { token, bind })
    }

    /// `/metrics` is served on the main listener (token required).
    pub fn on_main(&self) -> bool {
        self.bind.is_none() && self.token.is_some()
    }

    /// Checks an `Authorization` header value (constant time). Without a
    /// configured token every request is allowed (only possible on a
    /// loopback metrics listener).
    pub fn allows(&self, authorization: Option<&str>) -> bool {
        match &self.token {
            None => self.bind.is_some(),
            Some(t) => authorization
                .and_then(|h| h.strip_prefix("Bearer "))
                .map(|got| constant_time_eq(got.trim().as_bytes(), t.as_bytes()))
                .unwrap_or(false),
        }
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Checks a text exposition line by line (used by tests of app and runner):
/// every sample belongs to a family announced by `# TYPE`, names and labels
/// are well-formed, values parse. Returns the sample lines.
pub fn validate_exposition(text: &str) -> Result<Vec<String>, String> {
    let mut types: BTreeMap<String, String> = BTreeMap::new();
    let mut samples = vec![];
    let name_ok = |n: &str| {
        !n.is_empty()
            && n.chars().enumerate().all(|(i, c)| {
                c.is_ascii_alphabetic() || c == '_' || c == ':' || (i > 0 && c.is_ascii_digit())
            })
    };
    for (no, line) in text.lines().enumerate() {
        let err = |m: &str| format!("line {}: {m}: {line}", no + 1);
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            let mut it = rest.splitn(3, ' ');
            let (kw, name) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            if !name_ok(name) {
                return Err(err("bad metric name"));
            }
            match kw {
                "HELP" => {}
                "TYPE" => {
                    let t = it.next().unwrap_or("");
                    if !["counter", "gauge", "histogram", "summary", "untyped"].contains(&t) {
                        return Err(err("bad type"));
                    }
                    if types.insert(name.to_string(), t.to_string()).is_some() {
                        return Err(err("duplicate TYPE"));
                    }
                }
                _ => return Err(err("unknown comment")),
            }
            continue;
        }
        let (series, value) = line.rsplit_once(' ').ok_or_else(|| err("no value"))?;
        if !matches!(value, "+Inf" | "-Inf" | "NaN") && value.parse::<f64>().is_err() {
            return Err(err("bad value"));
        }
        let (name, labels) = match series.find('{') {
            Some(i) => {
                let l = series[i..]
                    .strip_prefix('{')
                    .and_then(|s| s.strip_suffix('}'))
                    .ok_or_else(|| err("bad label braces"))?;
                (&series[..i], Some(l))
            }
            None => (series, None),
        };
        if !name_ok(name) {
            return Err(err("bad sample name"));
        }
        let family = [name, name.trim_end_matches("_bucket")]
            .into_iter()
            .chain(name.strip_suffix("_sum"))
            .chain(name.strip_suffix("_count"))
            .find(|f| types.contains_key(*f))
            .ok_or_else(|| err("sample without TYPE"))?;
        if types[family] == "histogram" && name == family {
            return Err(err("histogram sample without suffix"));
        }
        if let Some(l) = labels {
            // name="value" pairs; values may contain escaped quotes.
            let mut rest = l;
            while !rest.is_empty() {
                let eq = rest.find("=\"").ok_or_else(|| err("bad label"))?;
                if !name_ok(&rest[..eq]) || rest[..eq].contains(':') {
                    return Err(err("bad label name"));
                }
                let mut i = eq + 2;
                let b = rest.as_bytes();
                loop {
                    match b.get(i) {
                        None => return Err(err("unterminated label value")),
                        Some(b'\\') => i += 2,
                        Some(b'"') => break,
                        Some(b'\n') => return Err(err("raw newline")),
                        Some(_) => i += 1,
                    }
                }
                rest = &rest[i + 1..];
                rest = rest.strip_prefix(',').unwrap_or(rest);
            }
        }
        samples.push(line.to_string());
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_gauges_and_histograms_in_text_format() {
        let r = Registry::default();
        let c = r.counter(
            "t_requests_total",
            "Requests.\nSecond \\ line",
            &["route", "status"],
        );
        c.inc(&["/api/tasks/{id}", "2xx"]);
        c.add(&["/api/tasks/{id}", "2xx"], 2.0);
        c.add(&["/x", "5xx"], -5.0); // ignored: counters never go down
        let g = r.gauge("t_up", "Up.", &[]);
        g.set(&[], 1.0);
        let h = r.histogram("t_seconds", "Durations.", &["kind"], &[0.1, 1.0]);
        h.observe(&["agent"], 0.05);
        h.observe(&["agent"], 0.5);
        h.observe(&["agent"], 7.0);
        let text = r.render();
        assert!(text.contains("# HELP t_requests_total Requests.\\nSecond \\\\ line\n"));
        assert!(text.contains("# TYPE t_requests_total counter\n"));
        assert!(text.contains("t_requests_total{route=\"/api/tasks/{id}\",status=\"2xx\"} 3\n"));
        assert!(!text.contains("5xx"));
        assert!(text.contains("t_up 1\n"));
        assert!(text.contains("t_seconds_bucket{kind=\"agent\",le=\"0.1\"} 1\n"));
        assert!(text.contains("t_seconds_bucket{kind=\"agent\",le=\"1\"} 2\n"));
        assert!(text.contains("t_seconds_bucket{kind=\"agent\",le=\"+Inf\"} 3\n"));
        assert!(text.contains("t_seconds_sum{kind=\"agent\"} 7.55\n"));
        assert!(text.contains("t_seconds_count{kind=\"agent\"} 3\n"));
        assert_eq!(validate_exposition(&text).unwrap().len(), 7);
    }

    #[test]
    fn label_values_are_escaped() {
        let r = Registry::default();
        let c = r.counter("t_total", "x", &["v"]);
        c.inc(&["a\"b\\c\nd"]);
        let text = r.render();
        assert!(text.contains(r#"t_total{v="a\"b\\c\nd"} 1"#), "{text}");
        validate_exposition(&text).unwrap();
    }

    #[test]
    fn cardinality_is_capped() {
        let r = Registry::default();
        let c = r.counter("t_total", "x", &["id"]);
        for i in 0..(MAX_SERIES + 50) {
            c.inc(&[&i.to_string()]);
        }
        assert_eq!(c.get(&[OVERFLOW]), 50.0);
        assert_eq!(r.render().lines().count(), MAX_SERIES + 1 + 2);
        // Known series keep counting after the cap.
        c.inc(&["7"]);
        assert_eq!(c.get(&["7"]), 2.0);
    }

    #[test]
    fn validator_rejects_broken_expositions() {
        assert!(validate_exposition("x 1\n").is_err(), "sample without TYPE");
        assert!(validate_exposition("# TYPE x counter\nx{a=\"b} 1\n").is_err());
        assert!(validate_exposition("# TYPE x counter\nx abc\n").is_err());
        assert!(validate_exposition("# TYPE 1x counter\n").is_err());
        assert!(validate_exposition("# TYPE h histogram\nh 1\n").is_err());
        assert!(validate_exposition("# TYPE x counter\n# TYPE x counter\n").is_err());
        assert!(validate_exposition("# TYPE x counter\nx{a=\"\\\"\"} 1\n").is_ok());
    }

    #[test]
    fn access_rules() {
        let tok = "m".repeat(24);
        // Nothing configured: no endpoint at all.
        let a = Access::parse(None, None, "", None).unwrap();
        assert!(!a.on_main() && a.bind.is_none());
        // Token only: main listener, bearer required.
        let a = Access::parse(Some(tok.clone()), None, "", None).unwrap();
        assert!(a.on_main());
        assert!(a.allows(Some(&format!("Bearer {tok}"))));
        assert!(!a.allows(Some("Bearer wrong")));
        assert!(!a.allows(Some(&tok)));
        assert!(!a.allows(None));
        // Loopback listener without token: open there only.
        let a = Access::parse(None, Some("127.0.0.1:9464".into()), "", None).unwrap();
        assert!(!a.on_main() && a.allows(None));
        // Public listener without token: refused.
        assert!(Access::parse(None, Some("0.0.0.0:9464".into()), "", None).is_err());
        assert!(Access::parse(Some(tok.clone()), Some("0.0.0.0:9464".into()), "", None).is_ok());
        // Short token, garbage bind, token reuse.
        assert!(Access::parse(Some("short".into()), None, "", None).is_err());
        assert!(Access::parse(None, Some("localhost".into()), "", None).is_err());
        let e = Access::parse(Some(tok.clone()), None, "RUNNER_", Some(&tok)).unwrap_err();
        assert!(e.contains("differ"), "{e}");
        let a = Access::parse(Some(tok.clone()), None, "", None).unwrap();
        assert!(
            !format!("{a:?}").contains(&tok),
            "Debug never prints the token"
        );
    }

    #[test]
    fn values_and_classes() {
        assert_eq!(fmt_value(3.0), "3");
        assert_eq!(fmt_value(0.25), "0.25");
        assert_eq!(fmt_value(f64::INFINITY), "+Inf");
        assert_eq!(status_class(204), "2xx");
        assert_eq!(status_class(503), "5xx");
        assert_eq!(method_label("BREW"), "other");
    }
}
