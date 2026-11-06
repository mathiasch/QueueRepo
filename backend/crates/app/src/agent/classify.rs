//! Provider error categories, their failure class, and the (deliberately
//! narrow) text fallback for CLIs that only report errors as prose.
//!
//! The text fallback is applied **only** to messages from a CLI's error
//! channel (Codex `turn.failed`/`error`, Gemini `error`/`result.error`, the
//! error text of a Claude error result, mock `error`) – never to agent text,
//! tool output or stderr.
use std::sync::OnceLock;

use chrono::{DateTime, Duration, TimeZone, Utc};
use regex::Regex;
use serde_json::Value;

use crate::models::FailureClass;

#[derive(Debug, Clone, PartialEq)]
pub enum ErrorCategory {
    /// Subscription/API limit, with the reset time if the CLI reported one.
    RateLimit(Option<DateTime<Utc>>),
    Overloaded,
    ServerError,
    /// Connection problems (DNS, reset, stream disconnected, timeouts).
    Network,
    /// Authentication / account problems (`authentication_failed`,
    /// `oauth_org_not_allowed`, `account_on_hold`, `verification_required`,
    /// `cloud_credential_error`).
    Auth,
    Billing,
    InvalidRequest,
    ModelNotFound,
    MaxOutputTokens,
    /// Unclassified API error with its HTTP status (if any).
    Unknown(Option<u16>),
}

impl ErrorCategory {
    /// Maps the Claude Code error enum (`system/api_retry.error`,
    /// `assistant.error`; same strings in the mock provider).
    pub fn from_claude(s: &str, status: Option<u16>) -> Self {
        match s {
            "rate_limit" => Self::RateLimit(None),
            "overloaded" => Self::Overloaded,
            "server_error" => Self::ServerError,
            "authentication_failed"
            | "oauth_org_not_allowed"
            | "account_on_hold"
            | "verification_required"
            | "cloud_credential_error" => Self::Auth,
            "billing_error" => Self::Billing,
            "invalid_request" => Self::InvalidRequest,
            "model_not_found" => Self::ModelNotFound,
            "max_output_tokens" => Self::MaxOutputTokens,
            "network" => Self::Network,
            _ => Self::Unknown(status),
        }
    }

    pub fn class(&self) -> FailureClass {
        match self {
            Self::RateLimit(_) => FailureClass::RateLimit,
            Self::Overloaded | Self::ServerError | Self::Network => FailureClass::ProviderTransient,
            Self::Unknown(None) => FailureClass::ProviderTransient,
            Self::Unknown(Some(s)) if *s == 408 || *s == 409 || *s >= 500 => {
                FailureClass::ProviderTransient
            }
            Self::Unknown(Some(_)) => FailureClass::Task,
            Self::Auth | Self::Billing => FailureClass::ProviderAuth,
            Self::InvalidRequest | Self::ModelNotFound => FailureClass::Config,
            Self::MaxOutputTokens => FailureClass::Task,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::RateLimit(_) => "rate_limit".into(),
            Self::Overloaded => "overloaded".into(),
            Self::ServerError => "server_error".into(),
            Self::Network => "network".into(),
            Self::Auth => "authentication".into(),
            Self::Billing => "billing".into(),
            Self::InvalidRequest => "invalid_request".into(),
            Self::ModelNotFound => "model_not_found".into(),
            Self::MaxOutputTokens => "max_output_tokens".into(),
            Self::Unknown(Some(s)) => format!("unknown (HTTP {s})"),
            Self::Unknown(None) => "unknown".into(),
        }
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// Classifies an error message from a CLI's error channel. `None` = no
/// provider-level cause recognisable → the caller treats it as a task error.
pub fn classify_text(text: &str) -> Option<ErrorCategory> {
    static HTTP_429: OnceLock<Regex> = OnceLock::new();
    static HTTP_401: OnceLock<Regex> = OnceLock::new();
    static HTTP_5XX: OnceLock<Regex> = OnceLock::new();
    let t = text.to_lowercase().replace('\u{2019}', "'");
    let has = |ks: &[&str]| ks.iter().any(|k| t.contains(k));
    let http_ctx = has(&["status", "http", "code"]);
    if has(&[
        "usage limit",
        "hit your limit",
        "rate limit",
        "rate_limit",
        "resource_exhausted",
        "too many requests",
        "quota exceeded",
        "exceeded your current quota",
        "exhausted your capacity",
    ]) || (http_ctx && re(&HTTP_429, r"\b429\b").is_match(&t))
    {
        return Some(ErrorCategory::RateLimit(parse_reset(text)));
    }
    if has(&["credit balance", "billing"]) {
        return Some(ErrorCategory::Billing);
    }
    if has(&[
        "unauthorized",
        "invalid api key",
        "invalid x-api-key",
        "api key not valid",
        "api_key_invalid",
        "authentication",
        "not logged in",
        "refresh token",
    ]) || re(&HTTP_401, r"\b401\b").is_match(&t)
    {
        return Some(ErrorCategory::Auth);
    }
    if has(&["overloaded"]) {
        return Some(ErrorCategory::Overloaded);
    }
    if has(&[
        "service unavailable",
        "internal server error",
        "bad gateway",
        "gateway timeout",
    ]) || (has(&["status", "http"]) && re(&HTTP_5XX, r"\b5\d\d\b").is_match(&t))
    {
        return Some(ErrorCategory::ServerError);
    }
    if has(&[
        "stream disconnected",
        "connection reset",
        "connection refused",
        "connection failed",
        "error sending request",
        "timed out",
        "econnreset",
        "econnrefused",
        "etimedout",
        "eai_again",
        "name does not resolve",
        "failed to lookup address",
    ]) {
        return Some(ErrorCategory::Network);
    }
    None
}

/// Extracts a reset timestamp from typical limit messages.
pub fn parse_reset(text: &str) -> Option<DateTime<Utc>> {
    static EPOCH: OnceLock<Regex> = OnceLock::new();
    static REL: OnceLock<Regex> = OnceLock::new();
    static PART: OnceLock<Regex> = OnceLock::new();
    // "…limit reached|1730000000"
    if let Some(c) = re(&EPOCH, r"\|(\d{10})\b").captures(text) {
        return Utc.timestamp_opt(c[1].parse().ok()?, 0).single();
    }
    // "try again in 2 hours 5 minutes" / "in 3 days"
    let rel = re(
        &REL,
        r"(?i)(?:try again|resets?|retry)\s+in\s+((?:\d+\s*(?:days?|d|hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s)[\s,and]*)+)",
    );
    if let Some(c) = rel.captures(text) {
        let mut d = Duration::zero();
        for p in re(&PART, r"(?i)(\d+)\s*([a-z]+)").captures_iter(&c[1]) {
            let v: i64 = p[1].parse().unwrap_or(0);
            d += match p[2].chars().next().unwrap_or('m') {
                'd' => Duration::days(v),
                'h' => Duration::hours(v),
                's' => Duration::seconds(v),
                _ => Duration::minutes(v),
            };
        }
        if d > Duration::zero() {
            return Some(Utc::now() + d);
        }
    }
    None
}

/// Unix seconds or milliseconds (number or numeric string), or RFC 3339.
pub fn epoch_value(v: &Value) -> Option<DateTime<Utc>> {
    if let Some(n) = v
        .as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    {
        let secs = if n > 10_000_000_000 { n / 1000 } else { n };
        return Utc.timestamp_opt(secs, 0).single();
    }
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_reset() {
        let t = parse_reset("Claude AI usage limit reached|1900000000").unwrap();
        assert_eq!(t.timestamp(), 1_900_000_000);
    }

    #[test]
    fn parses_relative_reset() {
        let t =
            parse_reset("You've hit your usage limit. Try again in 2 hours 30 minutes.").unwrap();
        let d = t - Utc::now();
        assert!(d > Duration::minutes(149) && d <= Duration::minutes(150));
    }

    /// Table 5.3.4 of the plan, plus `verification_required` (present in the
    /// Claude 2.1.319 enum, missing from the plan).
    #[test]
    fn category_mapping_table() {
        use FailureClass::*;
        let cases: [(&str, Option<u16>, FailureClass); 17] = [
            ("rate_limit", None, RateLimit),
            ("overloaded", Some(529), ProviderTransient),
            ("server_error", Some(500), ProviderTransient),
            ("unknown", None, ProviderTransient),
            ("unknown", Some(408), ProviderTransient),
            ("unknown", Some(409), ProviderTransient),
            ("unknown", Some(502), ProviderTransient),
            ("unknown", Some(400), Task),
            ("authentication_failed", Some(401), ProviderAuth),
            ("oauth_org_not_allowed", None, ProviderAuth),
            ("account_on_hold", None, ProviderAuth),
            ("verification_required", None, ProviderAuth),
            ("billing_error", None, ProviderAuth),
            ("cloud_credential_error", None, ProviderAuth),
            ("model_not_found", Some(404), Config),
            ("invalid_request", Some(400), Config),
            ("max_output_tokens", None, Task),
        ];
        for (s, status, want) in cases {
            assert_eq!(
                ErrorCategory::from_claude(s, status).class(),
                want,
                "{s} {status:?}"
            );
        }
    }

    #[test]
    fn text_fallback_categories() {
        let rl = |t: &str| matches!(classify_text(t), Some(ErrorCategory::RateLimit(_)));
        assert!(rl("Quota exceeded for model"));
        assert!(rl(
            "You\u{2019}ve hit your usage limit. Try again in 3 hours."
        ));
        assert!(rl("[API Error: got status: 429 Too Many Requests]"));
        assert!(rl("RESOURCE_EXHAUSTED"));
        assert!(!rl("compiled 429 files"), "bare 429 without HTTP context");
        assert_eq!(
            classify_text("API key not valid. Please pass a valid API key. API_KEY_INVALID"),
            Some(ErrorCategory::Auth)
        );
        assert_eq!(
            classify_text("HTTP 401 Unauthorized"),
            Some(ErrorCategory::Auth)
        );
        assert_eq!(
            classify_text("Your credit balance is too low"),
            Some(ErrorCategory::Billing)
        );
        assert_eq!(
            classify_text("API Error: 529 overloaded"),
            Some(ErrorCategory::Overloaded)
        );
        assert_eq!(
            classify_text("unexpected status 503 Service Unavailable"),
            Some(ErrorCategory::ServerError)
        );
        assert_eq!(
            classify_text(
                "stream disconnected before completion: failed to lookup address information"
            ),
            Some(ErrorCategory::Network)
        );
        assert_eq!(classify_text("compilation failed"), None);
        assert_eq!(classify_text("tests failed: 3 of 500"), None);
    }

    #[test]
    fn epoch_values() {
        assert_eq!(
            epoch_value(&serde_json::json!(1_900_000_000))
                .unwrap()
                .timestamp(),
            1_900_000_000
        );
        assert_eq!(
            epoch_value(&serde_json::json!(1_900_000_000_000i64))
                .unwrap()
                .timestamp(),
            1_900_000_000
        );
        assert_eq!(
            epoch_value(&serde_json::json!("2030-01-01T00:00:00Z"))
                .unwrap()
                .timestamp(),
            1_893_456_000
        );
        assert!(epoch_value(&serde_json::json!(null)).is_none());
    }
}
