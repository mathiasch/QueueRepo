//! `queuectl` – a small command-line client for the queue API.
//!
//! Authentication only with a personal API token (`qk_…`), never on the
//! command line: from `QUEUE_TOKEN` or from the configuration file, which
//! must belong to the user and must not be readable by others (`0600`).
//! Plain `http://` is only accepted for loopback addresses (or with
//! `insecure = true`), redirects are never followed (the token stays with
//! the configured server).
use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use queued_api::tokens;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

pub mod commands;

/// Exit codes (also in `--help`).
pub const EXIT_OK: i32 = 0;
/// Task ended without success, or the server refused the request.
pub const EXIT_FAILED: i32 = 1;
/// Wrong usage or configuration.
pub const EXIT_USAGE: i32 = 2;
/// Token missing, invalid, expired, revoked or without the needed scope.
pub const EXIT_AUTH: i32 = 3;

/// An error with its exit code and a German message.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub message: String,
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

pub fn exit(code: i32, message: impl Into<String>) -> Exit {
    Exit {
        code,
        message: message.into(),
    }
}

pub type Result<T> = std::result::Result<T, Exit>;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub url: String,
    pub token: String,
    pub project: Option<String>,
    /// Where the values came from (for messages).
    pub source: String,
}

/// `$QUEUECTL_CONFIG`, else `$XDG_CONFIG_HOME/queuectl/config`, else
/// `~/.config/queuectl/config`.
pub fn default_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = env("QUEUECTL_CONFIG") {
        return Some(PathBuf::from(p));
    }
    if let Some(x) = env("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(x).join("queuectl/config"));
    }
    env("HOME").map(|h| PathBuf::from(h).join(".config/queuectl/config"))
}

/// `key = value` lines, `#` comments, values optionally in double quotes.
pub fn parse_file(text: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(exit(
                EXIT_USAGE,
                format!(
                    "Konfiguration, Zeile {}: erwartet „schlüssel = wert“",
                    i + 1
                ),
            ));
        };
        let k = k.trim().to_ascii_lowercase();
        let mut v = v.trim();
        if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
            v = &v[1..v.len() - 1];
        }
        if !matches!(k.as_str(), "url" | "token" | "project" | "insecure") {
            return Err(exit(
                EXIT_USAGE,
                format!(
                    "Konfiguration, Zeile {}: unbekannter Schlüssel „{k}“ (erlaubt: url, token, project, insecure)",
                    i + 1
                ),
            ));
        }
        out.insert(k, v.to_string());
    }
    Ok(out)
}

/// A file that holds a token must belong to the user and must not be
/// readable or writable by group or others.
#[cfg(unix)]
pub fn check_private(path: &Path, meta: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(exit(
            EXIT_USAGE,
            format!(
                "{} enthält ein Token, ist aber für andere lesbar (Modus {mode:04o}). Bitte: chmod 600 {}",
                path.display(),
                path.display()
            ),
        ));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc_geteuid() };
    if meta.uid() != me {
        return Err(exit(
            EXIT_USAGE,
            format!("{} gehört nicht dir (uid {}).", path.display(), meta.uid()),
        ));
    }
    Ok(())
}

#[cfg(unix)]
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

#[cfg(not(unix))]
pub fn check_private(_path: &Path, _meta: &std::fs::Metadata) -> Result<()> {
    Ok(())
}

fn loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// `https://…`, or `http://` to loopback / with `insecure`.
pub fn check_url(url: &str, insecure: bool) -> Result<String> {
    let u = reqwest::Url::parse(url.trim())
        .map_err(|_| exit(EXIT_USAGE, format!("Ungültige Server-Adresse „{url}“")))?;
    let host = u.host_str().unwrap_or_default().to_string();
    match u.scheme() {
        "https" => {}
        "http" if loopback(&host) || insecure => {}
        "http" => {
            return Err(exit(
                EXIT_USAGE,
                "Unverschlüsseltes http:// nur für localhost – sonst würde das Token im Klartext übertragen (oder insecure = true setzen)",
            ))
        }
        _ => return Err(exit(EXIT_USAGE, "Server-Adresse muss mit https:// beginnen")),
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(exit(EXIT_USAGE, "Keine Zugangsdaten in der Server-Adresse"));
    }
    Ok(url.trim().trim_end_matches('/').to_string())
}

/// Environment (`QUEUE_URL`, `QUEUE_TOKEN`, `QUEUE_PROJECT`) over the file.
pub fn load(path: Option<&Path>, env: &dyn Fn(&str) -> Option<String>) -> Result<Config> {
    let path = path.map(Path::to_path_buf).or_else(|| default_path(env));
    let mut file = BTreeMap::new();
    let mut source = "Umgebung".to_string();
    if let Some(p) = &path {
        match std::fs::metadata(p) {
            Ok(meta) => {
                let mut text = String::new();
                std::fs::File::open(p)
                    .and_then(|mut f| f.read_to_string(&mut text))
                    .map_err(|e| exit(EXIT_USAGE, format!("{}: {e}", p.display())))?;
                file = parse_file(&text)?;
                if file.contains_key("token") {
                    check_private(p, &meta)?;
                }
                source = p.display().to_string();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(exit(EXIT_USAGE, format!("{}: {e}", p.display()))),
        }
    }
    let get = |k: &str, e: &str| {
        env(e)
            .or_else(|| file.get(k).cloned())
            .filter(|v| !v.trim().is_empty())
    };
    let insecure = get("insecure", "QUEUECTL_INSECURE")
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let url = get("url", "QUEUE_URL").ok_or_else(|| {
        exit(
            EXIT_USAGE,
            "Keine Server-Adresse: QUEUE_URL setzen oder „url = https://…“ in die Konfigurationsdatei schreiben",
        )
    })?;
    let token = get("token", "QUEUE_TOKEN").ok_or_else(|| {
        exit(
            EXIT_AUTH,
            "Kein API-Token: QUEUE_TOKEN setzen oder „token = qk_…“ in die Konfigurationsdatei (chmod 600) schreiben. Tokens legst du in Queue unter Einstellungen → API-Zugang an.",
        )
    })?;
    let token = token.trim().to_string();
    if !tokens::well_formed(&token, tokens::TOKEN_PREFIX) {
        return Err(exit(
            EXIT_AUTH,
            "Das API-Token hat nicht das erwartete Format (qk_… mit Prüfsumme) – Tippfehler?",
        ));
    }
    Ok(Config {
        url: check_url(&url, insecure)?,
        token,
        project: get("project", "QUEUE_PROJECT"),
        source,
    })
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

pub struct Api {
    pub base: String,
    token: String,
    http: reqwest::Client,
    stream: reqwest::Client,
}

impl Api {
    pub fn new(cfg: &Config) -> Result<Self> {
        let build = |timeout: Option<Duration>| {
            let mut b = reqwest::Client::builder()
                .user_agent(concat!("queuectl/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10));
            if let Some(t) = timeout {
                b = b.timeout(t);
            }
            b.build()
                .map_err(|e| exit(EXIT_USAGE, format!("HTTP-Client: {e}")))
        };
        Ok(Self {
            base: cfg.url.clone(),
            token: cfg.token.clone(),
            http: build(Some(Duration::from_secs(60)))?,
            stream: build(None)?,
        })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/api{path}", self.base))
            .bearer_auth(&self.token)
    }

    pub async fn send(&self, r: reqwest::RequestBuilder) -> Result<(u16, Value)> {
        let resp = r
            .send()
            .await
            .map_err(|e| exit(EXIT_FAILED, format!("Server nicht erreichbar: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok((status, v))
    }

    fn check(status: u16, v: &Value) -> Result<()> {
        if (200..300).contains(&status) {
            return Ok(());
        }
        let msg = v
            .get("error")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| v.to_string());
        Err(match status {
            401 => exit(EXIT_AUTH, format!("Nicht angemeldet: {} (Token abgelaufen, widerrufen oder falsch?)", explain(&msg))),
            403 => exit(EXIT_AUTH, format!("Nicht erlaubt: {}", explain(&msg))),
            404 => exit(EXIT_FAILED, "Nicht gefunden (oder kein Zugriff)"),
            300..=399 => exit(EXIT_FAILED, format!("Server leitet um (HTTP {status}) – die Adresse ist falsch, Weiterleitungen folgt queuectl nicht")),
            _ => exit(EXIT_FAILED, format!("Fehler (HTTP {status}): {}", explain(&msg))),
        })
    }

    pub async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T> {
        let mut r = self.req(method, path);
        if let Some(b) = body {
            r = r.json(b);
        }
        let (status, v) = self.send(r).await?;
        Self::check(status, &v)?;
        serde_json::from_value(v)
            .map_err(|e| exit(EXIT_FAILED, format!("Unerwartete Antwort: {e}")))
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.call::<(), T>(reqwest::Method::GET, path, None).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    /// POST that keeps the status (fire: 201/200/409/429 all carry a result).
    pub async fn post_raw<B: Serialize>(&self, path: &str, body: &B) -> Result<(u16, Value)> {
        let (status, v) = self
            .send(self.req(reqwest::Method::POST, path).json(body))
            .await?;
        if status == 401 || status == 403 || status == 404 {
            Self::check(status, &v)?;
        }
        Ok((status, v))
    }

    /// The SSE stream `/api/events` (no overall timeout).
    pub async fn events(&self) -> Result<reqwest::Response> {
        let resp = self
            .stream
            .get(format!("{}/api/events", self.base))
            .bearer_auth(&self.token)
            .header("accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| exit(EXIT_FAILED, format!("Server nicht erreichbar: {e}")))?;
        let status = resp.status().as_u16();
        if status != 200 {
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            Self::check(status, &v)?;
            return Err(exit(EXIT_FAILED, format!("Live-Verbindung: HTTP {status}")));
        }
        Ok(resp)
    }
}

/// German text for the server's English token errors.
pub fn explain(msg: &str) -> String {
    match msg {
        "token revoked" => "Token wurde widerrufen".into(),
        "token expired" => "Token ist abgelaufen".into(),
        "invalid token" => "Token ist ungültig".into(),
        "this endpoint cannot be used with an API token" => {
            "diese Funktion ist mit einem API-Token nicht verfügbar".into()
        }
        m if m.starts_with("token lacks scope ") => format!(
            "dem Token fehlt die Berechtigung „{}“",
            m.trim_start_matches("token lacks scope ")
        ),
        m => m.to_string(),
    }
}

/// Splits an SSE byte stream into the JSON of its `data:` lines.
#[derive(Default)]
pub struct SseParser {
    buf: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        let mut out = vec![];
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(d) = line.strip_prefix("data:") {
                self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            }
            // Comments (keep-alive) and other fields are ignored.
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn token() -> String {
        tokens::compose("qk_", &"a1B2c3".repeat(8)[..43])
    }

    fn env(m: HashMap<&'static str, String>) -> impl Fn(&str) -> Option<String> {
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn file_format() {
        let f =
            parse_file("# c\nurl = \"https://q.example\"\n\ntoken=qk_x\nPROJECT = Demo\n").unwrap();
        assert_eq!(f["url"], "https://q.example");
        assert_eq!(f["token"], "qk_x");
        assert_eq!(f["project"], "Demo");
        assert!(parse_file("nonsense").is_err());
        assert!(parse_file("password = x").is_err());
    }

    #[test]
    fn urls() {
        assert_eq!(
            check_url("https://q.example/", false).unwrap(),
            "https://q.example"
        );
        assert!(check_url("http://127.0.0.1:8080", false).is_ok());
        assert!(check_url("http://localhost", false).is_ok());
        assert!(check_url("http://q.example", false).is_err());
        assert!(check_url("http://q.example", true).is_ok());
        assert!(check_url("ftp://q.example", true).is_err());
        assert!(check_url("https://u:p@q.example", false).is_err());
        assert!(check_url("nope", false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn token_files_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config");
        std::fs::write(
            &p,
            format!("url = https://q.example\ntoken = {}\n", token()),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let none = env(HashMap::new());
        let e = load(Some(&p), &none).unwrap_err();
        assert_eq!(e.code, EXIT_USAGE);
        assert!(e.message.contains("chmod 600"), "{e}");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(load(Some(&p), &none).is_err(), "group-readable");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let c = load(Some(&p), &none).unwrap();
        assert_eq!(
            (c.url.as_str(), c.token.as_str()),
            ("https://q.example", token().as_str())
        );
        // Without a token in the file, the mode does not matter.
        std::fs::write(&p, "url = https://q.example\nproject = P\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let with_env = env(HashMap::from([("QUEUE_TOKEN", token())]));
        let c = load(Some(&p), &with_env).unwrap();
        assert_eq!(c.project.as_deref(), Some("P"));
    }

    #[test]
    fn environment_wins_and_tokens_are_checked_offline() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none");
        let e = env(HashMap::from([
            ("QUEUE_URL", "http://127.0.0.1:1".to_string()),
            ("QUEUE_TOKEN", token()),
        ]));
        let c = load(Some(&missing), &e).unwrap();
        assert_eq!(c.source, "Umgebung");
        let mut typo = token();
        typo.replace_range(5..6, if &typo[5..6] == "x" { "y" } else { "x" });
        let e = env(HashMap::from([
            ("QUEUE_URL", "https://q".to_string()),
            ("QUEUE_TOKEN", typo),
        ]));
        assert_eq!(load(Some(&missing), &e).unwrap_err().code, EXIT_AUTH);
        let e = env(HashMap::from([("QUEUE_URL", "https://q".to_string())]));
        assert_eq!(load(Some(&missing), &e).unwrap_err().code, EXIT_AUTH);
        let e = env(HashMap::from([("QUEUE_TOKEN", token())]));
        assert_eq!(load(Some(&missing), &e).unwrap_err().code, EXIT_USAGE);
        let e = env(HashMap::from([("XDG_CONFIG_HOME", "/x".to_string())]));
        assert_eq!(default_path(&e), Some(PathBuf::from("/x/queuectl/config")));
    }

    #[test]
    fn sse_parsing() {
        let mut p = SseParser::default();
        assert!(p.push(b"data: {\"a\"").is_empty());
        let out = p.push(b":1}\n\n: keep-alive\n\ndata: x\r\ndata: y\r\n\r\n");
        assert_eq!(out, vec!["{\"a\":1}".to_string(), "x\ny".to_string()]);
    }

    #[test]
    fn token_errors_are_translated() {
        assert_eq!(
            explain("token lacks scope tasks:write"),
            "dem Token fehlt die Berechtigung „tasks:write“"
        );
        assert_eq!(explain("token expired"), "Token ist abgelaufen");
    }
}
