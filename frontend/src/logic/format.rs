//! Number, time and duration formatting (German conventions).
use chrono::{DateTime, Datelike, TimeZone, Utc};

/// Rounds half away from zero to one decimal and uses a decimal comma.
fn one_decimal(v: f64) -> String {
    let r = (v * 10.0).round() / 10.0;
    format!("{r:.1}").replace('.', ",")
}

/// `999`, `12k`, `3,3M`.
pub fn fmt_tokens(n: i64) -> String {
    let a = n.unsigned_abs() as f64;
    let sign = if n < 0 { "-" } else { "" };
    if a >= 1e6 {
        format!("{sign}{}M", one_decimal(a / 1e6))
    } else if a >= 1e3 {
        format!("{sign}{}k", (a / 1e3).round() as i64)
    } else {
        n.to_string()
    }
}

/// `0,50 $` with the given number of decimals.
pub fn fmt_usd(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$} $").replace('.', ",")
}

/// Plain number with decimal comma, without trailing zeros (`0,25`, `2000`).
pub fn fmt_number(v: f64) -> String {
    let s = format!("{v}");
    s.replace('.', ",")
}

fn two(n: u32) -> String {
    format!("{n:02}")
}

/// `14:05` today, otherwise `03.01. 14:05`.
pub fn fmt_time<Tz: TimeZone>(ts: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String {
    let t = ts.with_timezone(tz);
    let n = now.with_timezone(tz);
    let hm = format!("{}:{}", two(t.hour_()), two(t.minute_()));
    if t.date_naive() == n.date_naive() {
        hm
    } else {
        format!("{}.{}. {hm}", two(t.day()), two(t.month()))
    }
}

/// `14:05:10` (for retry deadlines).
pub fn fmt_clock_secs<Tz: TimeZone>(ts: DateTime<Utc>, tz: &Tz) -> String {
    let t = ts.with_timezone(tz);
    format!(
        "{}:{}:{}",
        two(t.hour_()),
        two(t.minute_()),
        two(t.second_())
    )
}

/// `vor 3 Min.`, `gerade eben`, `gestern 14:05`, otherwise like [`fmt_time`].
pub fn fmt_relative<Tz: TimeZone>(ts: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String {
    let secs = (now - ts).num_seconds();
    if (-30..45).contains(&secs) {
        return "gerade eben".into();
    }
    if secs > 0 && secs < 3600 {
        return format!("vor {} Min.", (secs + 30) / 60);
    }
    let t = ts.with_timezone(tz);
    let n = now.with_timezone(tz);
    if secs > 0 && t.date_naive() == n.date_naive() {
        return format!("vor {} Std.", secs / 3600);
    }
    if secs > 0 && n.date_naive().pred_opt() == Some(t.date_naive()) {
        return format!("gestern {}:{}", two(t.hour_()), two(t.minute_()));
    }
    fmt_time(ts, now, tz)
}

/// `42 s`, `2 min 5 s`, `1 h 30 min`; empty without a start.
pub fn duration(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> String {
    let Some(a) = start else {
        return String::new();
    };
    let s = (end.unwrap_or(now) - a).num_seconds().max(0);
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    }
}

/// Small helpers so callers do not need `chrono::Timelike` in scope.
trait Hms {
    fn hour_(&self) -> u32;
    fn minute_(&self) -> u32;
    fn second_(&self) -> u32;
}

impl<Tz: TimeZone> Hms for DateTime<Tz> {
    fn hour_(&self) -> u32 {
        chrono::Timelike::hour(self)
    }
    fn minute_(&self) -> u32 {
        chrono::Timelike::minute(self)
    }
    fn second_(&self) -> u32 {
        chrono::Timelike::second(self)
    }
}

/// Parses an RFC 3339 timestamp (git commit dates) into UTC.
pub fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// German plural helper: `1 Auftrag`, `3 Aufträge`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn tokens() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(12_400), "12k");
        assert_eq!(fmt_tokens(3_250_000), "3,3M");
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(-1500), "-2k");
    }

    #[test]
    fn money_and_numbers() {
        assert_eq!(fmt_usd(0.5, 2), "0,50 $");
        assert_eq!(fmt_usd(1.23456, 3), "1,235 $");
        assert_eq!(fmt_number(0.25), "0,25");
        assert_eq!(fmt_number(2000.0), "2000");
    }

    #[test]
    fn durations() {
        let a = t("2026-01-01T00:00:00Z");
        let now = t("2026-01-01T05:00:00Z");
        assert_eq!(
            duration(Some(a), Some(t("2026-01-01T00:00:42Z")), now),
            "42 s"
        );
        assert_eq!(
            duration(Some(a), Some(t("2026-01-01T00:02:05Z")), now),
            "2 min 5 s"
        );
        assert_eq!(
            duration(Some(a), Some(t("2026-01-01T01:30:00Z")), now),
            "1 h 30 min"
        );
        assert_eq!(duration(Some(a), None, now), "5 h 0 min");
        assert_eq!(duration(None, None, now), "");
        // Clock skew never yields negative durations.
        assert_eq!(duration(Some(now), Some(a), now), "0 s");
    }

    #[test]
    fn times_use_the_given_zone() {
        let berlin = FixedOffset::east_opt(3600).unwrap();
        let now = t("2026-03-05T12:00:00Z");
        assert_eq!(fmt_time(t("2026-03-05T08:07:00Z"), now, &berlin), "09:07");
        assert_eq!(
            fmt_time(t("2026-01-02T23:30:00Z"), now, &berlin),
            "03.01. 00:30"
        );
        // Same UTC day but already the next local day.
        let late = t("2026-03-05T23:30:00Z");
        assert_eq!(fmt_time(late, now, &berlin), "06.03. 00:30");
        assert_eq!(
            fmt_clock_secs(t("2026-03-05T13:05:09Z"), &berlin),
            "14:05:09"
        );
    }

    #[test]
    fn relative_times() {
        let z = Utc;
        let now = t("2026-03-05T12:00:00Z");
        assert_eq!(
            fmt_relative(t("2026-03-05T11:59:40Z"), now, &z),
            "gerade eben"
        );
        assert_eq!(
            fmt_relative(t("2026-03-05T11:57:00Z"), now, &z),
            "vor 3 Min."
        );
        assert_eq!(
            fmt_relative(t("2026-03-05T09:00:00Z"), now, &z),
            "vor 3 Std."
        );
        assert_eq!(
            fmt_relative(t("2026-03-04T18:15:00Z"), now, &z),
            "gestern 18:15"
        );
        assert_eq!(
            fmt_relative(t("2026-03-01T18:15:00Z"), now, &z),
            "01.03. 18:15"
        );
    }

    #[test]
    fn misc() {
        assert_eq!(
            parse_rfc3339("2026-03-05T13:05:09+01:00"),
            Some(t("2026-03-05T12:05:09Z"))
        );
        assert_eq!(parse_rfc3339("nope"), None);
        assert_eq!(plural(1, "Auftrag", "Aufträge"), "1 Auftrag");
        assert_eq!(plural(0, "Auftrag", "Aufträge"), "0 Aufträge");
    }
}
