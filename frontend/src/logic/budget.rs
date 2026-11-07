//! Provider budgets and the status chips in the navigation.
use chrono::{DateTime, TimeZone, Utc};
use queued_api::ProviderStatus;

use super::format::{fmt_time, fmt_tokens, fmt_usd};
use super::labels::{provider_label, Tone};

/// Highest utilisation (0..1+) across configured budgets, `None` without budgets.
pub fn budget_usage(l: &ProviderStatus) -> Option<f64> {
    let mut parts = vec![];
    if let Some(b) = l.budget.limit_5h_tokens.filter(|b| *b > 0) {
        parts.push(l.window.used_5h as f64 / b as f64);
    }
    if let Some(b) = l.budget.limit_7d_tokens.filter(|b| *b > 0) {
        parts.push(l.window.used_7d as f64 / b as f64);
    }
    if let Some(b) = l.budget.limit_30d_usd.filter(|b| *b > 0.0) {
        parts.push(l.window.cost_30d / b);
    }
    parts.into_iter().reduce(f64::max)
}

/// Chips are shown only for providers that were used, are blocked or have a budget.
pub fn chip_visible(l: &ProviderStatus) -> bool {
    l.window.runs_5h > 0 || l.blocked || budget_usage(l).is_some()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    pub text: String,
    pub tone: Tone,
    /// Longer explanation (tooltip / screen readers).
    pub detail: String,
}

pub fn limit_chip<Tz: TimeZone>(l: &ProviderStatus, now: DateTime<Utc>, tz: &Tz) -> Chip {
    let name = provider_label(&l.provider);
    let detail = {
        let w = &l.window;
        let mut d = format!(
            "5 Std.: {} Tokens ({} Läufe, Cache {}) · 7 Tage: {} · 30 Tage: {}",
            fmt_tokens(w.used_5h),
            w.runs_5h,
            fmt_tokens(w.cache_read_5h),
            fmt_tokens(w.used_7d),
            fmt_usd(w.cost_30d, 2)
        );
        if let Some(r) = &l.budget_reason {
            d.push_str(" · ");
            d.push_str(r);
        }
        d
    };
    let (text, tone) = if l.paused_reason.is_some() {
        (format!("{name} pausiert (Anmeldung)"), Tone::Danger)
    } else if let Some(t) = l.limited_until {
        (
            format!("{name} Abo-Limit bis {}", fmt_time(t, now, tz)),
            Tone::Danger,
        )
    } else if let Some(t) = l.budget_until {
        (
            format!("{name} Budget bis {}", fmt_time(t, now, tz)),
            Tone::Danger,
        )
    } else if let Some(u) = budget_usage(l) {
        (
            format!("{name} {} %", (u * 100.0).round() as i64),
            if u >= 0.8 {
                Tone::Warning
            } else {
                Tone::Neutral
            },
        )
    } else {
        (
            format!("{name} {}", fmt_tokens(l.window.used_5h)),
            Tone::Neutral,
        )
    };
    Chip { text, tone, detail }
}

/// Fill level of one budget bar in percent (0..=100) and its tone.
pub fn bar(used: f64, limit: Option<f64>) -> (f64, Tone) {
    let Some(l) = limit.filter(|l| *l > 0.0) else {
        return (0.0, Tone::Neutral);
    };
    let pct = (used / l * 100.0).clamp(0.0, 100.0);
    let tone = if pct >= 100.0 {
        Tone::Danger
    } else if pct >= 80.0 {
        Tone::Warning
    } else {
        Tone::Info
    };
    (pct, tone)
}

/// 90 % of the observed subscription limit as a starting point for the 5h budget.
pub fn suggested_budget(observed: i64) -> i64 {
    (observed as f64 * 0.9).floor() as i64
}

/// Budget inputs → API body; whole tokens, positive USD, empty = unlimited.
pub fn parse_budget(b5: &str, b7: &str, usd: &str) -> Result<queued_api::Budget, String> {
    use super::limits::num_or_null;
    let tokens = |label: &str, v: &str| -> Result<Option<i64>, String> {
        match num_or_null(v).map_err(|e| format!("{label}: {e}"))? {
            Some(n) if n.fract() != 0.0 || n < 1.0 => {
                Err(format!("{label}: bitte eine ganze Zahl ab 1 eingeben"))
            }
            Some(n) => Ok(Some(n as i64)),
            None => Ok(None),
        }
    };
    let limit_30d_usd = match num_or_null(usd).map_err(|e| format!("30-Tage-Budget: {e}"))? {
        Some(n) if n <= 0.0 => return Err("30-Tage-Budget: muss größer als 0 sein".into()),
        o => o,
    };
    Ok(queued_api::Budget {
        limit_5h_tokens: tokens("5h-Budget", b5)?,
        limit_7d_tokens: tokens("7-Tage-Budget", b7)?,
        limit_30d_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::{Budget, Window};

    fn limit(f: impl FnOnce(&mut ProviderStatus)) -> ProviderStatus {
        let mut l = ProviderStatus {
            provider: "claude".into(),
            ..Default::default()
        };
        f(&mut l);
        l
    }

    fn now() -> DateTime<Utc> {
        "2026-01-01T09:00:00Z".parse().unwrap()
    }

    #[test]
    fn usage_is_none_without_budgets_and_takes_the_most_exhausted_window() {
        assert_eq!(budget_usage(&limit(|l| l.window.used_5h = 100)), None);
        let u = budget_usage(&limit(|l| {
            l.window = Window {
                used_5h: 50,
                used_7d: 900,
                ..Default::default()
            };
            l.budget = Budget {
                limit_5h_tokens: Some(100),
                limit_7d_tokens: Some(1000),
                limit_30d_usd: None,
            };
        }))
        .unwrap();
        assert!((u - 0.9).abs() < 1e-9);
        let u = budget_usage(&limit(|l| {
            l.window.cost_30d = 15.0;
            l.budget.limit_30d_usd = Some(10.0);
        }))
        .unwrap();
        assert!((u - 1.5).abs() < 1e-9);
    }

    #[test]
    fn chip_reflects_subscription_limit_budget_pause_and_warning_level() {
        let z = Utc;
        let c = limit_chip(
            &limit(|l| l.limited_until = Some("2026-01-01T10:00:00Z".parse().unwrap())),
            now(),
            &z,
        );
        assert_eq!(c.tone, Tone::Danger);
        assert_eq!(c.text, "Claude Abo-Limit bis 10:00");
        let c = limit_chip(
            &limit(|l| l.budget_until = Some("2026-01-01T10:00:00Z".parse().unwrap())),
            now(),
            &z,
        );
        assert!(c.text.contains("Budget"));
        let warn = limit(|l| {
            l.window.used_5h = 85;
            l.budget.limit_5h_tokens = Some(100);
        });
        assert_eq!(limit_chip(&warn, now(), &z).text, "Claude 85 %");
        assert_eq!(limit_chip(&warn, now(), &z).tone, Tone::Warning);
        let low = limit(|l| {
            l.window.used_5h = 10;
            l.budget.limit_5h_tokens = Some(100);
        });
        assert_eq!(limit_chip(&low, now(), &z).tone, Tone::Neutral);
        let codex = limit(|l| {
            l.provider = "codex".into();
            l.window.used_5h = 2000;
        });
        assert_eq!(limit_chip(&codex, now(), &z).text, "Codex (GPT) 2k");
    }

    #[test]
    fn chip_shows_an_auth_pause_before_anything_else() {
        let c = limit_chip(
            &limit(|l| {
                l.paused_reason = Some("Invalid API key".into());
                l.limited_until = Some("2026-01-01T10:00:00Z".parse().unwrap());
            }),
            now(),
            &Utc,
        );
        assert_eq!(c.text, "Claude pausiert (Anmeldung)");
        assert_eq!(c.tone, Tone::Danger);
    }

    #[test]
    fn chip_detail_and_visibility() {
        let l = limit(|l| {
            l.window.runs_5h = 2;
            l.window.used_5h = 1500;
            l.budget_reason = Some("5h-Budget erreicht".into());
        });
        let c = limit_chip(&l, now(), &Utc);
        assert!(c.detail.starts_with("5 Std.: 2k Tokens (2 Läufe"));
        assert!(c.detail.ends_with("5h-Budget erreicht"));
        assert!(chip_visible(&l));
        assert!(!chip_visible(&limit(|_| {})));
        assert!(chip_visible(&limit(|l| l.blocked = true)));
    }

    #[test]
    fn budget_inputs() {
        let b = parse_budget("1000", "", "2,5").unwrap();
        assert_eq!(b.limit_5h_tokens, Some(1000));
        assert_eq!(b.limit_7d_tokens, None);
        assert_eq!(b.limit_30d_usd, Some(2.5));
        assert!(parse_budget("1,5", "", "")
            .unwrap_err()
            .starts_with("5h-Budget"));
        assert!(parse_budget("", "x", "")
            .unwrap_err()
            .starts_with("7-Tage-Budget"));
        assert!(parse_budget("", "", "0").is_err());
        assert_eq!(
            parse_budget("", "", "").unwrap(),
            queued_api::Budget::default()
        );
    }

    #[test]
    fn bars_and_suggestion() {
        assert_eq!(bar(5.0, None), (0.0, Tone::Neutral));
        assert_eq!(bar(50.0, Some(100.0)), (50.0, Tone::Info));
        assert_eq!(bar(85.0, Some(100.0)), (85.0, Tone::Warning));
        assert_eq!(bar(150.0, Some(100.0)), (100.0, Tone::Danger));
        assert_eq!(suggested_budget(1001), 900);
    }
}
