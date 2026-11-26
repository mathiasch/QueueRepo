//! WP15: the model chooser's explanation in plain language.
use queued_api::chooser::{
    category_label, need_label, risk_label, size_label, step_label, tier_label, CandidateView,
    ChooserReport, StepChoice, TaskFeatures,
};
use queued_api::StepMode;

use super::costs::fmt_money;

/// `78 %`.
pub fn fmt_pct(p: f64) -> String {
    let p = if p.is_finite() {
        p.clamp(0.0, 1.0)
    } else {
        0.0
    };
    format!("{:.0} %", p * 100.0)
}

/// Feature chips: type, size, risks, needs, languages.
pub fn feature_chips(f: &TaskFeatures) -> Vec<(String, &'static str)> {
    let mut v = vec![
        (category_label(&f.category).to_string(), "tone-info"),
        (format!("Umfang: {}", size_label(&f.size)), "tone-neutral"),
    ];
    for r in &f.risks {
        v.push((format!("Risiko: {}", risk_label(r)), "tone-warning"));
    }
    for n in &f.needs {
        v.push((need_label(n).to_string(), "tone-neutral"));
    }
    if f.urgent {
        v.push(("Eilig".into(), "tone-warning"));
    }
    for l in f.languages.iter().take(3) {
        v.push((l.clone(), "tone-neutral"));
    }
    v
}

/// What the probability rests on.
pub fn evidence_label(c: &CandidateView) -> String {
    match c.evidence.as_str() {
        "learned" => format!("gelernt aus {:.0} eigenen Läufen", c.observations),
        "benchmark" => "Annahme (Benchmark)".into(),
        _ => "Annahme (noch keine Messung)".into(),
    }
}

/// One line for the headline: "Gewählt: X (Zugang) – ca. 0,05 $ …".
pub fn headline(r: &ChooserReport) -> String {
    match (&r.chosen, &r.fallback) {
        (Some(c), _) => format!(
            "Gewählt: {} über „{}“ – Erfolgschance ca. {}, ca. {} pro erledigtem Auftrag",
            c.model,
            c.access,
            fmt_pct(c.p_success),
            fmt_money(c.cost_per_accepted, "USD")
        ),
        (None, Some(f)) => format!("Keine automatische Wahl: {f}"),
        (None, None) => "Keine automatische Wahl".into(),
    }
}

pub fn mode_text(m: StepMode) -> &'static str {
    match m {
        StepMode::Batch => "Batch (−50 %, bis 24 h)",
        StepMode::Sync => "direkt",
        StepMode::Harness => "als Agent",
        StepMode::Off => "aus",
    }
}

/// "Planung: claude-opus-5-5 (Batch …), ca. 0,14 $".
pub fn step_line(s: &StepChoice) -> String {
    format!(
        "{}: {} ({}, Stufe „{}“), ca. {} – {}",
        step_label(&s.step),
        s.model,
        mode_text(s.mode),
        tier_label(s.tier),
        fmt_money(s.cost, "USD"),
        s.reason
    )
}

/// The escalation chain after the first attempt.
pub fn chain_text(r: &ChooserReport) -> Option<String> {
    (!r.chain.is_empty()).then(|| {
        r.chain
            .iter()
            .map(|s| format!("{} ({})", s.model, fmt_pct(s.p_success)))
            .collect::<Vec<_>>()
            .join(" → ")
    })
}

/// Cell texts of an alternative: (model, access, chance, per attempt, per
/// accepted task, note).
pub fn alternative_row(c: &CandidateView) -> [String; 6] {
    let allowed = c.excluded.is_none();
    [
        c.model.clone(),
        c.access.clone(),
        if allowed {
            fmt_pct(c.p_success)
        } else {
            "–".into()
        },
        fmt_money(c.attempt_cost, "USD"),
        if allowed {
            fmt_money(c.cost_per_accepted, "USD")
        } else {
            "–".into()
        },
        c.excluded.clone().unwrap_or_else(|| evidence_label(c)),
    ]
}

/// Quality slider text.
pub fn quality_text(q: u8) -> &'static str {
    match q {
        0..=20 => "so günstig wie möglich",
        21..=40 => "eher günstig",
        41..=60 => "ausgewogen",
        61..=80 => "eher Qualität",
        _ => "Qualität vor Kosten",
    }
}

/// Comma-separated model list → entries.
pub fn parse_models(s: &str) -> Vec<String> {
    s.split([',', '\n'])
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::{ModelTier, Target};

    fn cand(excluded: Option<&str>) -> CandidateView {
        CandidateView {
            target: Target::default(),
            access: "Anthropic API".into(),
            model: "claude-haiku-5-5".into(),
            family: "anthropic".into(),
            tier: ModelTier::Cheap,
            p_success: 0.774,
            p_accept: 0.99,
            observations: 6.0,
            evidence: "learned".into(),
            attempt_cost: 0.0305,
            cost_per_accepted: 0.05,
            score: 0.05,
            excluded: excluded.map(String::from),
        }
    }

    #[test]
    fn headline_and_rows() {
        let r = ChooserReport {
            chosen: Some(cand(None)),
            ..Default::default()
        };
        assert_eq!(
            headline(&r),
            "Gewählt: claude-haiku-5-5 über „Anthropic API“ – Erfolgschance ca. 77 %, ca. 0,05 $ pro erledigtem Auftrag"
        );
        let row = alternative_row(&cand(None));
        assert_eq!(row[2], "77 %");
        assert_eq!(row[5], "gelernt aus 6 eigenen Läufen");
        let ex = alternative_row(&cand(Some("im Projekt gesperrt")));
        assert_eq!(
            (ex[2].as_str(), ex[4].as_str(), ex[5].as_str()),
            ("–", "–", "im Projekt gesperrt")
        );
        let fb = ChooserReport {
            fallback: Some("Alle Kandidaten scheiden aus".into()),
            ..Default::default()
        };
        assert!(headline(&fb).starts_with("Keine automatische Wahl: Alle"));
    }

    #[test]
    fn chips_steps_and_helpers() {
        let f = TaskFeatures {
            category: "bugfix".into(),
            size: "L".into(),
            complexity: 3,
            risks: vec!["security".into()],
            needs: vec!["vision".into()],
            languages: vec!["Rust".into()],
            urgent: true,
            ..Default::default()
        };
        let c = feature_chips(&f);
        assert_eq!(c[0].0, "Fehlerbehebung");
        assert!(c
            .iter()
            .any(|x| x.0 == "Risiko: Sicherheit" && x.1 == "tone-warning"));
        assert!(c.iter().any(|x| x.0 == "Eilig"));
        assert!(c.iter().any(|x| x.0 == "Bilder verstehen"));
        let s = StepChoice {
            step: "plan".into(),
            mode: StepMode::Batch,
            target: Target::default(),
            access: "a".into(),
            model: "claude-opus-5-5".into(),
            tier: ModelTier::Top,
            cost: 0.14,
            reason: "größerer Auftrag".into(),
        };
        assert_eq!(step_line(&s), "Planung: claude-opus-5-5 (Batch (−50 %, bis 24 h), Stufe „stark“), ca. 0,14 $ – größerer Auftrag");
        assert_eq!(quality_text(50), "ausgewogen");
        assert_eq!(quality_text(100), "Qualität vor Kosten");
        assert_eq!(
            parse_models(" haiku, ,opus\nsonnet "),
            vec!["haiku", "opus", "sonnet"]
        );
        assert_eq!(fmt_pct(f64::NAN), "0 %");
    }
}
