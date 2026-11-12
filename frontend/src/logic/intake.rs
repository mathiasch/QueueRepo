//! WP08 presentation logic for the project tab "Anbindungen" (intake
//! integrations): tones, summaries, the form → wire mapping.
use queued_api::intake::{
    IdentityState, IntakeConfig, IntakeKind, IntakeOutcome, IntegrationView, NewIntegration,
};

use super::labels::Tone;

pub fn outcome_tone(o: IntakeOutcome) -> Tone {
    match o {
        IntakeOutcome::Created
        | IntakeOutcome::Answered
        | IntakeOutcome::Steered
        | IntakeOutcome::Continued => Tone::Success,
        IntakeOutcome::Ignored => Tone::Neutral,
        IntakeOutcome::Rejected => Tone::Warning,
        IntakeOutcome::Failed => Tone::Danger,
    }
}

pub fn identity_tone(s: IdentityState) -> Tone {
    match s {
        IdentityState::Active => Tone::Success,
        _ => Tone::Warning,
    }
}

/// One line about what the integration reacts to.
pub fn trigger_summary(i: &IntegrationView) -> String {
    let c = &i.config;
    match i.kind {
        IntakeKind::Github | IntakeKind::Jira => match (&c.label, &c.mention) {
            (Some(l), Some(m)) => format!("Reagiert auf das Label „{l}“ und auf „{m}“"),
            (Some(l), None) => format!("Reagiert auf das Label „{l}“"),
            (None, Some(m)) => format!("Reagiert auf „{m}“ im Issue oder in Kommentaren"),
            (None, None) => "Reagiert auf nichts – bitte Label angeben".into(),
        },
        IntakeKind::Slack => "Reagiert auf Erwähnungen der App und den Slash-Befehl".into(),
        IntakeKind::Linear => "Reagiert, wenn ein Issue an Queue delegiert wird".into(),
    }
}

/// Who may start tasks (plain words).
pub fn who_summary(i: &IntegrationView) -> String {
    let people = match (i.active_people, i.waiting_people) {
        (0, 0) => "Noch niemand zugeordnet".to_string(),
        (a, 0) => format!("{a} Person(en) zugeordnet"),
        (a, w) => format!("{a} Person(en) zugeordnet, {w} warten auf Bestätigung"),
    };
    if i.config.allow_unmapped {
        format!(
            "{people} · alle anderen handeln als {} (auf deren Kosten)",
            i.owner_name
        )
    } else {
        format!("{people} · andere werden abgewiesen")
    }
}

/// Status of the way back (comments, thread replies, activities).
pub fn feedback_summary(i: &IntegrationView) -> (String, Tone) {
    if !i.has_token {
        return (
            "Rückmeldungen aus – kein Token eingetragen".into(),
            Tone::Warning,
        );
    }
    if i.failed_updates > 0 {
        return (
            format!(
                "{} Rückmeldung(en) nicht zugestellt – Token und Rechte prüfen",
                i.failed_updates
            ),
            Tone::Danger,
        );
    }
    ("Rückmeldungen aktiv".into(), Tone::Success)
}

/// Form state of "Verbinden".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConnectForm {
    pub name: String,
    pub tenant: String,
    pub secret: String,
    pub token: String,
    pub account: String,
    pub label: String,
    pub mention: String,
    pub instructions: String,
    pub allow_unmapped: bool,
    pub per_hour: String,
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Form → request; `Err` = German message for fields the server would
/// refuse anyway (it validates everything again).
pub fn new_integration(kind: IntakeKind, f: &ConnectForm) -> Result<NewIntegration, String> {
    if f.name.trim().is_empty() {
        return Err("Bitte einen Namen angeben".into());
    }
    if f.tenant.trim().is_empty() {
        return Err(format!("Bitte „{}“ angeben", kind.tenant_label()));
    }
    if !kind.queue_makes_secret() && f.secret.trim().is_empty() {
        return Err(format!("Bitte das {} eintragen", kind.secret_label()));
    }
    let max_per_hour = match f.per_hour.trim() {
        "" => None,
        s => match s.parse::<i32>() {
            Ok(n) if (1..=100).contains(&n) => Some(n),
            _ => return Err("Aufträge pro Stunde: eine Zahl von 1 bis 100".into()),
        },
    };
    Ok(NewIntegration {
        kind,
        name: f.name.trim().to_string(),
        tenant: f.tenant.trim().to_string(),
        secret: (!kind.queue_makes_secret()).then(|| f.secret.trim().to_string()),
        token: opt(&f.token),
        account: opt(&f.account).filter(|_| kind.needs_account()),
        config: IntakeConfig {
            label: opt(&f.label).filter(|_| matches!(kind, IntakeKind::Github | IntakeKind::Jira)),
            mention: opt(&f.mention).filter(|_| kind == IntakeKind::Github),
            allow_unmapped: f.allow_unmapped,
            instructions: opt(&f.instructions),
        },
        max_per_hour,
    })
}

/// Defaults for a new form of `kind`.
pub fn defaults(kind: IntakeKind) -> ConnectForm {
    ConnectForm {
        name: kind.label().to_string(),
        label: match kind {
            IntakeKind::Github | IntakeKind::Jira => "queue".into(),
            _ => String::new(),
        },
        mention: if kind == IntakeKind::Github {
            queued_api::intake::DEFAULT_MENTION.into()
        } else {
            String::new()
        },
        per_hour: "10".into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn view(kind: IntakeKind) -> IntegrationView {
        IntegrationView {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            kind,
            name: "x".into(),
            tenant: "t".into(),
            enabled: true,
            owner_id: Uuid::nil(),
            owner_name: "Ada".into(),
            config: IntakeConfig {
                label: Some("queue".into()),
                ..Default::default()
            },
            max_per_hour: 10,
            has_token: true,
            account: None,
            hook_url: "https://q/api/intake/x".into(),
            disabled_reason: None,
            active_people: 0,
            waiting_people: 0,
            failed_updates: 0,
            last_event: None,
            can_manage: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn summaries_speak_plainly() {
        let mut v = view(IntakeKind::Github);
        assert_eq!(trigger_summary(&v), "Reagiert auf das Label „queue“");
        assert!(who_summary(&v).contains("Noch niemand") && who_summary(&v).contains("abgewiesen"));
        v.active_people = 2;
        v.waiting_people = 1;
        v.config.allow_unmapped = true;
        let w = who_summary(&v);
        assert!(w.contains("2 Person(en)") && w.contains("1 warten") && w.contains("Ada"));
        assert_eq!(feedback_summary(&v).1, Tone::Success);
        v.failed_updates = 3;
        assert_eq!(feedback_summary(&v).1, Tone::Danger);
        v.has_token = false;
        assert!(feedback_summary(&v).0.contains("kein Token"));
        assert!(trigger_summary(&view(IntakeKind::Slack)).contains("Erwähnungen"));
        assert_eq!(outcome_tone(IntakeOutcome::Rejected), Tone::Warning);
        assert_eq!(identity_tone(IdentityState::WaitingUser), Tone::Warning);
    }

    #[test]
    fn form_maps_to_the_wire_type() {
        let mut f = defaults(IntakeKind::Github);
        f.tenant = " acme/shop ".into();
        let n = new_integration(IntakeKind::Github, &f).unwrap();
        assert_eq!(n.tenant, "acme/shop");
        assert_eq!(n.secret, None, "Queue makes GitHub's secret");
        assert_eq!(n.config.label.as_deref(), Some("queue"));
        assert_eq!(n.config.mention.as_deref(), Some("@queue"));
        assert_eq!(n.max_per_hour, Some(10));
        let mut f = defaults(IntakeKind::Slack);
        f.tenant = "T1".into();
        assert!(new_integration(IntakeKind::Slack, &f)
            .unwrap_err()
            .contains("Signing Secret"));
        f.secret = "abc".into();
        f.label = "ignored".into();
        f.account = "ignored@x".into();
        let n = new_integration(IntakeKind::Slack, &f).unwrap();
        assert_eq!(n.config.label, None);
        assert_eq!(n.account, None);
        assert_eq!(n.secret.as_deref(), Some("abc"));
        f.per_hour = "0".into();
        assert!(new_integration(IntakeKind::Slack, &f).is_err());
        let mut f = defaults(IntakeKind::Jira);
        f.tenant = "https://a.atlassian.net".into();
        f.account = "bot@a".into();
        assert_eq!(
            new_integration(IntakeKind::Jira, &f)
                .unwrap()
                .account
                .as_deref(),
            Some("bot@a")
        );
        f.name = " ".into();
        assert!(new_integration(IntakeKind::Jira, &f).is_err());
    }
}
