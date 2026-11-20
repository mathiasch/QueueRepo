//! WP10: dashboard helpers – chart scaling, text alternatives of the
//! charts, labels of the notification settings.
use queued_api::analytics::{Analytics, DayRow};
use queued_api::notify::{Channel, Event};

/// Height/width of a bar in percent of the largest value (≥ 2 % when the
/// value is not zero, so a small value is still visible).
pub fn bar_pct(v: f64, max: f64) -> f64 {
    if v <= 0.0 || max <= 0.0 || !v.is_finite() || !max.is_finite() {
        0.0
    } else {
        (v / max * 100.0).clamp(2.0, 100.0)
    }
}

/// `18.11.` of a day.
pub fn day_label(d: &DayRow) -> String {
    d.day.format("%d.%m.").to_string()
}

/// Text alternative of the "tasks per day" chart.
pub fn tasks_chart_summary(days: &[DayRow]) -> String {
    let done: i64 = days.iter().map(|d| d.done).sum();
    let bad: i64 = days.iter().map(|d| d.failed + d.blocked).sum();
    let best = days
        .iter()
        .filter(|d| d.done > 0)
        .max_by_key(|d| (d.done, d.day));
    let mut s = format!(
        "Säulendiagramm über {} Tage: {done} erledigt, {bad} nicht erledigt.",
        days.len()
    );
    if let Some(b) = best {
        s.push_str(&format!(
            " Meiste erledigt am {}: {}.",
            day_label(b),
            b.done
        ));
    }
    s.push_str(" Die Tabelle darunter enthält alle Werte.");
    s
}

/// Text alternative of the "cost per day" chart.
pub fn cost_chart_summary(days: &[DayRow]) -> String {
    let total: f64 = days.iter().map(|d| d.cost_usd).sum();
    let top = days
        .iter()
        .filter(|d| d.cost_usd > 0.0)
        .max_by(|a, b| a.cost_usd.total_cmp(&b.cost_usd));
    let mut s = format!(
        "Säulendiagramm der Kosten in Dollar über {} Tage, zusammen {}.",
        days.len(),
        super::costs::fmt_money(total, "USD")
    );
    if let Some(t) = top {
        s.push_str(&format!(
            " Teuerster Tag: {} mit {}.",
            day_label(t),
            super::costs::fmt_money(t.cost_usd, "USD")
        ));
    }
    s
}

/// Nothing to show yet in the window.
pub fn is_empty(a: &Analytics) -> bool {
    a.outcomes.finished == 0 && a.tokens.total() == 0 && a.costs.is_empty()
}

/// Model column: catalog id without vendor, or "unbekannt".
pub fn model_label(m: Option<&str>) -> String {
    match m {
        Some(m) => m.rsplit('/').next().unwrap_or(m).to_string(),
        None => "unbekannt".into(),
    }
}

/// Query string of the dashboard.
pub fn query(days: i32, project: Option<&str>, tz: &str) -> String {
    let tz = String::from(js_sys::encode_uri_component(tz));
    match project {
        Some(p) => format!("/analytics?days={days}&scope=project&project_id={p}&tz={tz}"),
        None => format!("/analytics?days={days}&tz={tz}"),
    }
}

/// Channel labels in matrix order.
pub fn channel_columns() -> [(Channel, &'static str); 4] {
    Channel::ALL.map(|c| (c, c.label()))
}

/// The event list in the order of the settings (critical first).
pub fn event_rows() -> [Event; 12] {
    Event::ALL
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn day(d: u32, done: i64, failed: i64, cost: f64) -> DayRow {
        DayRow {
            day: NaiveDate::from_ymd_opt(2026, 11, d).unwrap(),
            done,
            failed,
            blocked: 0,
            cost_usd: cost,
        }
    }

    #[test]
    fn bars_scale_and_stay_visible() {
        assert_eq!(bar_pct(0.0, 10.0), 0.0);
        assert_eq!(bar_pct(10.0, 10.0), 100.0);
        assert_eq!(bar_pct(0.01, 1000.0), 2.0, "tiny but visible");
        assert_eq!(bar_pct(5.0, 0.0), 0.0);
        assert_eq!(bar_pct(f64::NAN, 1.0), 0.0);
    }

    #[test]
    fn chart_summaries_name_the_numbers() {
        let days = vec![day(17, 2, 1, 0.5), day(18, 4, 0, 1.25), day(19, 0, 0, 0.0)];
        let s = tasks_chart_summary(&days);
        assert!(
            s.contains("über 3 Tage: 6 erledigt, 1 nicht erledigt"),
            "{s}"
        );
        assert!(s.contains("Meiste erledigt am 18.11.: 4"), "{s}");
        let c = cost_chart_summary(&days);
        assert!(c.contains("zusammen 1,75 $"), "{c}");
        assert!(c.contains("Teuerster Tag: 18.11. mit 1,25 $"), "{c}");
        assert_eq!(
            model_label(Some("anthropic/claude-sonnet-5-5")),
            "claude-sonnet-5-5"
        );
        assert_eq!(model_label(None), "unbekannt");
    }
}
