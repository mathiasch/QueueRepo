//! WP12: Best-of-N, preview and routing explanations – pure helpers.
use queued_api::bon::{AttemptView, Comparison, SelectedBy, VerifyState, MAX_ATTEMPTS};
use queued_api::{DecisionSource, Estimate, Money, ReviewResult, RoutingDecision, Target};

use super::costs::{fmt_money_list, parse_access};

/// Attempts the stepper allows (1 = a normal task).
pub fn clamp_attempts(n: i32) -> i32 {
    n.clamp(1, MAX_ATTEMPTS)
}

/// Access/model rows of the composer → request targets. All rows on the
/// standard access without model → empty list (every attempt like the task).
/// A row with an empty access means "like the task" (with its own model).
pub fn targets(rows: &[(String, String)]) -> Vec<Target> {
    if rows
        .iter()
        .all(|(a, m)| a.is_empty() && m.trim().is_empty())
    {
        return vec![];
    }
    rows.iter()
        .map(|(a, m)| {
            let (connector_id, provider) = parse_access(a);
            Target {
                connector_id,
                provider,
                model: Some(m.trim().to_string()).filter(|m| !m.is_empty()),
            }
        })
        .collect()
}

/// Cost preview of a Best-of-N task: "3 Versuche: ca. 1,20 $ (je ca. 0,40 $)".
pub fn estimate_text(e: &Estimate) -> Option<String> {
    if e.attempts.is_empty() {
        return None;
    }
    let n = e.attempts.len();
    if e.mid.is_empty() {
        return Some(format!(
            "{n} Versuche – Kosten unbekannt (kein Preis für das Modell)"
        ));
    }
    let same = e.attempts.windows(2).all(|w| w[0].mid == w[1].mid);
    let each = if same {
        format!(" (je ca. {})", fmt_money_list(&e.attempts[0].mid))
    } else {
        String::new()
    };
    Some(format!(
        "{n} Versuche zusammen: ca. {}{each}",
        fmt_money_list(&e.mid)
    ))
}

pub fn verify_label(v: VerifyState) -> &'static str {
    match v {
        VerifyState::None => "kein Prüfkommando",
        VerifyState::Passed => "bestanden",
        VerifyState::Failed => "fehlgeschlagen",
        VerifyState::NotReached => "nicht erreicht",
    }
}

pub fn review_label(r: Option<&ReviewResult>) -> String {
    match r {
        None => "–".into(),
        Some(r) if r.approved => "bestanden".into(),
        Some(r) => match r.issues.len() {
            0 => "abgelehnt".into(),
            1 => "abgelehnt (1 Punkt)".into(),
            n => format!("abgelehnt ({n} Punkte)"),
        },
    }
}

pub fn changes_label(a: &AttemptView) -> String {
    if a.files_changed == 0 {
        return "keine".into();
    }
    format!(
        "{} {} · +{} −{}",
        a.files_changed,
        if a.files_changed == 1 {
            "Datei"
        } else {
            "Dateien"
        },
        a.additions,
        a.deletions
    )
}

pub fn cost_label(m: &[Money]) -> String {
    if m.is_empty() {
        "–".into()
    } else {
        fmt_money_list(m)
    }
}

pub fn secs_label(s: Option<i64>) -> String {
    match s {
        None => "–".into(),
        Some(s) if s < 60 => format!("{s} s"),
        Some(s) if s < 3600 => format!("{} min {} s", s / 60, s % 60),
        Some(s) => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

/// The line above the attempts: what happens next.
pub fn comparison_status(c: &Comparison) -> String {
    if let Some(ch) = &c.choice {
        return match ch.by {
            SelectedBy::User => format!("Versuch {} wurde von dir übernommen.", ch.attempt_no),
            SelectedBy::Auto => format!(
                "Versuch {} wurde automatisch gewählt: {}",
                ch.attempt_no, ch.reason
            ),
        };
    }
    if let Some(a) = &c.aborted {
        return format!("{a}. Fertige Versuche kannst du trotzdem übernehmen.");
    }
    let open = c
        .attempts
        .iter()
        .filter(|a| {
            matches!(
                a.status.as_str(),
                "queued" | "running" | "waiting_batch" | "waiting_input" | "held"
            )
        })
        .count();
    let ready = c
        .attempts
        .iter()
        .filter(|a| a.status == "candidate")
        .count();
    let auto = if c.auto_select && c.select_error.is_none() {
        " Danach wählt ein Prüf-Agent automatisch den besten."
    } else {
        ""
    };
    match (open, ready) {
        (0, 0) => "Kein Versuch war erfolgreich.".into(),
        (0, r) if c.auto_select && c.select_error.is_none() => {
            format!("{r} erfolgreich – der Prüf-Agent wählt gerade.")
        }
        (0, r) => format!("Alle Versuche fertig: {r} erfolgreich. Bitte einen übernehmen."),
        (o, 0) => format!("{o} Versuche laufen noch.{auto}"),
        (o, r) => format!(
            "{r} erfolgreich, {o} laufen noch. Du kannst schon jetzt einen übernehmen – die anderen werden dann gestoppt."
        ),
    }
}

pub fn source_label(s: DecisionSource) -> &'static str {
    match s {
        DecisionSource::Explicit => "am Auftrag gewählt",
        DecisionSource::Attempt => "Best-of-N-Versuch",
        DecisionSource::Escalation => "Eskalation",
        DecisionSource::Rule => "Regel",
        DecisionSource::Route => "Projekt-Routing",
        DecisionSource::Inherited => "übernommen",
        DecisionSource::Default => "Standard",
        DecisionSource::Chooser => "Automatische Modellwahl",
        DecisionSource::Override => "von Hand übersteuert",
    }
}

/// "Warum dieses Modell?" in one line.
pub fn decision_text(d: &RoutingDecision) -> String {
    format!("{}: {}", source_label(d.source), d.reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::{AttemptEstimate, TaskFacts};
    use uuid::Uuid;

    fn usd(a: f64) -> Vec<Money> {
        vec![Money {
            currency: "USD".into(),
            amount: a,
        }]
    }

    #[test]
    fn targets_from_rows() {
        assert_eq!(clamp_attempts(0), 1);
        assert_eq!(clamp_attempts(12), 8);
        let empty = vec![(String::new(), String::new()); 3];
        assert!(targets(&empty).is_empty(), "all like the task");
        let id = Uuid::nil();
        let rows = vec![
            (String::new(), "claude-opus-5-5".to_string()),
            (format!("c:{id}"), " ".to_string()),
            ("p:codex".to_string(), "gpt-6.1-sol".to_string()),
        ];
        let t = targets(&rows);
        assert_eq!(t.len(), 3);
        assert_eq!(
            (
                t[0].connector_id,
                t[0].provider.clone(),
                t[0].model.as_deref()
            ),
            (None, None, Some("claude-opus-5-5"))
        );
        assert_eq!((t[1].connector_id, t[1].model.clone()), (Some(id), None));
        assert_eq!(t[2].provider.as_deref(), Some("codex"));
    }

    #[test]
    fn estimate_and_labels() {
        let mut e = Estimate {
            steps: vec![],
            low: usd(0.9),
            mid: usd(1.2),
            high: usd(2.0),
            location: String::new(),
            notes: vec![],
            attempts: vec![],
        };
        assert_eq!(estimate_text(&e), None);
        e.attempts = (1..=3)
            .map(|n| AttemptEstimate {
                attempt_no: n,
                label: "x".into(),
                low: usd(0.3),
                mid: usd(0.4),
                high: usd(0.6),
            })
            .collect();
        let t = estimate_text(&e).unwrap();
        assert!(
            t.starts_with("3 Versuche zusammen: ca. ") && t.contains("je ca."),
            "{t}"
        );
        e.attempts[2].mid = usd(0.5);
        assert!(!estimate_text(&e).unwrap().contains("je ca."));
        assert_eq!(verify_label(VerifyState::Failed), "fehlgeschlagen");
        assert_eq!(review_label(None), "–");
        assert_eq!(
            review_label(Some(&ReviewResult {
                approved: false,
                issues: vec!["a".into(), "b".into()]
            })),
            "abgelehnt (2 Punkte)"
        );
        assert_eq!(secs_label(Some(75)), "1 min 15 s");
        assert_eq!(secs_label(Some(3720)), "1 h 2 min");
        let d = RoutingDecision {
            step: "execute".into(),
            source: DecisionSource::Rule,
            target: Target::default(),
            rule: Some("Klein".into()),
            rule_index: Some(0),
            escalation_level: None,
            reason: "Regel 1 „Klein“ → haiku".into(),
            facts: TaskFacts::default(),
            chooser: None,
        };
        assert_eq!(decision_text(&d), "Regel: Regel 1 „Klein“ → haiku");
    }

    fn attempt(no: i32, status: &str) -> AttemptView {
        AttemptView {
            id: Uuid::from_u128(no as u128),
            attempt_no: no,
            status: status.into(),
            provider: "mock".into(),
            model: None,
            access: "mock".into(),
            files_changed: 2,
            additions: 5,
            deletions: 1,
            verify: VerifyState::Passed,
            review: None,
            cost: vec![],
            tokens: 0,
            duration_secs: None,
            summary: None,
            error: None,
            screenshots: vec![],
            selected: false,
        }
    }

    #[test]
    fn comparison_status_lines() {
        let mut c = Comparison {
            group: Uuid::nil(),
            attempts: vec![attempt(1, "running"), attempt(2, "candidate")],
            choice: None,
            auto_select: false,
            selector: None,
            selector_reason: None,
            aborted: None,
            select_error: None,
            can_select: true,
            total: vec![],
        };
        assert!(comparison_status(&c).contains("schon jetzt"));
        c.attempts[0].status = "failed".into();
        assert!(comparison_status(&c).contains("Bitte einen übernehmen"));
        c.auto_select = true;
        assert!(comparison_status(&c).contains("Prüf-Agent wählt"));
        assert_eq!(changes_label(&c.attempts[1]), "2 Dateien · +5 −1");
        c.aborted = Some("Budget aller Versuche erreicht".into());
        assert!(comparison_status(&c).starts_with("Budget"));
    }
}
