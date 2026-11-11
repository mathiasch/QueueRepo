//! WP07: API tokens, templates and triggers – texts, presets and parsing
//! for the UI (pure, tested natively).
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use queued_api::tokens::{ApiTokenView, Scope};
use queued_api::triggers::{
    Cron, GithubFilter, RunOutcome, TriggerKind, TriggerRunView, TriggerView,
};

use super::format::fmt_time;
use super::labels::Tone;

/// Schedules offered as one click (cron, label).
pub const CRON_PRESETS: [(&str, &str); 5] = [
    ("15 7 * * 1-5", "Werktags um 07:15"),
    ("0 2 * * *", "Jede Nacht um 02:00"),
    ("0 8 * * 1", "Montags um 08:00"),
    ("0 */4 * * *", "Alle 4 Stunden"),
    ("0 * * * *", "Stündlich"),
];

/// Scope sets for common uses (label, scopes).
pub const SCOPE_PRESETS: [(&str, &[Scope]); 3] = [
    (
        "queuectl (Aufträge anlegen, verfolgen, steuern)",
        &[
            Scope::ProjectsRead,
            Scope::TasksRead,
            Scope::TasksWrite,
            Scope::TasksSteer,
        ],
    ),
    ("Nur lesen", &[Scope::ProjectsRead, Scope::TasksRead]),
    ("CI: Auslöser starten", &[Scope::TriggersFire]),
];

/// German description of a cron text or the parse error.
pub fn cron_feedback(cron: &str) -> Result<String, String> {
    Cron::parse(cron).map(|c| c.describe())
}

/// `datetime-local` value (`YYYY-MM-DDTHH:MM`) in `tz` → UTC.
pub fn parse_local<Tz: TimeZone>(s: &str, tz: &Tz) -> Option<DateTime<Utc>> {
    let n = NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%dT%H:%M").ok()?;
    tz.from_local_datetime(&n)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

/// UTC → `datetime-local` value in `tz`.
pub fn to_local_input<Tz: TimeZone>(t: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    t.with_timezone(tz).format("%Y-%m-%dT%H:%M").to_string()
}

pub fn outcome_tone(o: RunOutcome) -> Tone {
    match o {
        RunOutcome::Created => Tone::Success,
        RunOutcome::Ignored | RunOutcome::Skipped => Tone::Neutral,
        RunOutcome::Rejected => Tone::Warning,
        RunOutcome::Failed => Tone::Danger,
    }
}

/// Token state for the list (label, tone).
pub fn token_state(t: &ApiTokenView, now: DateTime<Utc>) -> (&'static str, Tone) {
    if t.revoked_at.is_some() {
        ("Widerrufen", Tone::Neutral)
    } else if t.expires_at <= now {
        ("Abgelaufen", Tone::Neutral)
    } else if t.expires_at - now < chrono::Duration::days(7) {
        ("Läuft bald ab", Tone::Warning)
    } else {
        ("Aktiv", Tone::Success)
    }
}

/// One line about when/how a trigger starts.
pub fn trigger_summary<Tz: TimeZone>(t: &TriggerView, now: DateTime<Utc>, tz: &Tz) -> String {
    let next = |n: Option<DateTime<Utc>>| {
        n.map(|n| format!(" · nächster Lauf (deine Uhrzeit) {}", fmt_time(n, now, tz)))
            .unwrap_or_default()
    };
    match t.kind {
        TriggerKind::Schedule => format!(
            "{} ({}){}",
            t.cron_text.clone().unwrap_or_default(),
            t.timezone.clone().unwrap_or_default(),
            next(t.next_run_at)
        ),
        TriggerKind::Once => match (t.next_run_at, t.run_at) {
            (Some(n), _) => format!("Einmalig am {}", fmt_time(n, now, tz)),
            (None, Some(r)) if t.last_fired_at.is_some() => {
                format!(
                    "Einmalig – ausgeführt (geplant für {})",
                    fmt_time(r, now, tz)
                )
            }
            (None, Some(r)) => format!("Einmalig am {} (ausgeschaltet)", fmt_time(r, now, tz)),
            (None, None) => "Einmalig".into(),
        },
        TriggerKind::Api => "Startet nur per API-Aufruf oder von Hand".into(),
        TriggerKind::Webhook => "Startet bei signierten Webhook-Aufrufen".into(),
        TriggerKind::Github => match &t.github {
            Some(f) => github_summary(f),
            None => "GitHub".into(),
        },
    }
}

pub fn github_summary(f: &GithubFilter) -> String {
    let ev: Vec<&str> = f.events.iter().map(|e| e.label()).collect();
    let mut s = format!("GitHub: {}", ev.join(", "));
    if !f.branches.is_empty() {
        s.push_str(&format!(" · Branches {}", f.branches.join(", ")));
    }
    if !f.labels.is_empty() {
        s.push_str(&format!(" · Labels {}", f.labels.join(", ")));
    }
    if let Some(r) = &f.repository {
        s.push_str(&format!(" · {r}"));
    }
    s
}

/// One history line (without time).
pub fn run_text(r: &TriggerRunView) -> String {
    // The outcome is shown as a tag next to it.
    let mut s = r.source.label().to_string();
    if let Some(e) = &r.event {
        s.push_str(&format!(" · {e}"));
    }
    s
}

/// Comma list → cleaned items.
pub fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Shell example to sign a generic webhook call (no secret inside).
pub fn webhook_example(url: &str) -> String {
    format!(
        "SECRET='qwh_…'   # das Secret von oben\n\
         BODY='{{\"service\":\"api\",\"status\":\"failed\"}}'\n\
         TS=$(date +%s); ID=$(uuidgen)\n\
         SIG=$(printf '%s.%s.%s' \"$TS\" \"$ID\" \"$BODY\" | openssl dgst -sha256 -hmac \"$SECRET\" | sed 's/^.* //')\n\
         curl -sS -X POST '{url}' \\\n  \
         -H 'Content-Type: application/json' \\\n  \
         -H \"X-Queue-Timestamp: $TS\" -H \"X-Queue-Delivery: $ID\" \\\n  \
         -H \"X-Queue-Signature: sha256=$SIG\" \\\n  \
         --data \"$BODY\""
    )
}

/// What to enter on GitHub (Settings → Webhooks → Add webhook).
pub fn github_steps(url: &str, f: Option<&GithubFilter>) -> Vec<String> {
    let mut events: Vec<&str> = vec![];
    if let Some(f) = f {
        for e in &f.events {
            let h = e.header();
            if !events.contains(&h) {
                events.push(h);
            }
        }
    }
    vec![
        "Im Repository auf GitHub: Settings → Webhooks → Add webhook.".into(),
        format!("Payload URL: {url}"),
        "Content type: application/json".into(),
        "Secret: das Secret von oben einfügen.".into(),
        format!(
            "Ereignisse: „Let me select individual events“ und {} wählen. Für Queues eigene Pull Requests zusätzlich „Pull request reviews“, „Issue comments“ und „Check suites“ – dann prüft Queue sie sofort statt erst beim nächsten Abruf.",
            if events.is_empty() { "die gewünschten".into() } else { events.join(", ") }
        ),
        "Speichern. GitHub schickt sofort einen „Ping“ – er erscheint hier im Verlauf.".into(),
    ]
}

/// Example `queuectl` configuration (the token goes into a 0600 file).
pub fn queuectl_example(origin: &str, token: &str) -> String {
    format!(
        "mkdir -p ~/.config/queuectl\n\
         umask 077\n\
         cat > ~/.config/queuectl/config <<'EOF'\n\
         url = {origin}\n\
         token = {token}\n\
         EOF\n\
         chmod 600 ~/.config/queuectl/config\n\
         queuectl whoami"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use queued_api::triggers::GithubEvent;

    #[test]
    fn presets_are_valid_schedules() {
        for (c, label) in CRON_PRESETS {
            assert!(cron_feedback(c).is_ok(), "{label}");
        }
        assert_eq!(cron_feedback("15 7 * * 1-5").unwrap(), "Mo–Fr um 07:15");
        assert!(cron_feedback("*/5 * * * *").unwrap_err().contains("Stunde"));
    }

    #[test]
    fn local_datetime_roundtrip() {
        let cet = FixedOffset::east_opt(3600).unwrap();
        let t = parse_local("2026-11-12T09:30", &cet).unwrap();
        assert_eq!(t.to_rfc3339(), "2026-11-12T08:30:00+00:00");
        assert_eq!(to_local_input(t, &cet), "2026-11-12T09:30");
        assert!(parse_local("12.11.2026", &cet).is_none());
    }

    #[test]
    fn lists_and_texts() {
        assert_eq!(parse_list(" main, release/* ,,"), vec!["main", "release/*"]);
        let f = GithubFilter {
            events: vec![GithubEvent::Push, GithubEvent::CheckFailure],
            branches: vec!["main".into()],
            ..Default::default()
        };
        assert_eq!(
            github_summary(&f),
            "GitHub: Push, Fehlgeschlagene Checks · Branches main"
        );
        let steps = github_steps("https://q/api/hooks/1", Some(&f));
        assert!(steps[1].ends_with("https://q/api/hooks/1"));
        assert!(steps[4].contains("push, check_run"), "{}", steps[4]);
        assert!(webhook_example("https://q/x").contains("X-Queue-Signature: sha256="));
        let q = queuectl_example("https://q", "qk_x");
        assert!(q.contains("chmod 600") && q.contains("token = qk_x"));
        for s in SCOPE_PRESETS {
            assert!(!s.1.is_empty());
        }
    }

    #[test]
    fn history_line_names_the_source_and_event() {
        let r = TriggerRunView {
            id: uuid::Uuid::nil(),
            trigger_id: uuid::Uuid::nil(),
            source: queued_api::triggers::RunSource::Github,
            outcome: RunOutcome::Ignored,
            reason: None,
            task_id: None,
            task_title: None,
            task_status: None,
            event: Some("issues.opened".into()),
            scheduled_for: None,
            delivery_id: None,
            actor_name: None,
            created_at: Utc::now(),
        };
        assert_eq!(run_text(&r), "GitHub · issues.opened");
        assert_eq!(outcome_tone(RunOutcome::Failed), Tone::Danger);
    }

    #[test]
    fn token_states() {
        let now = Utc::now();
        let mut t = ApiTokenView {
            id: uuid::Uuid::nil(),
            name: "x".into(),
            prefix: "qk_abc".into(),
            scopes: vec![Scope::TasksRead],
            created_at: now,
            expires_at: now + chrono::Duration::days(30),
            last_used_at: None,
            revoked_at: None,
        };
        assert_eq!(token_state(&t, now).0, "Aktiv");
        t.expires_at = now + chrono::Duration::days(2);
        assert_eq!(token_state(&t, now).0, "Läuft bald ab");
        t.expires_at = now - chrono::Duration::days(1);
        assert_eq!(token_state(&t, now).0, "Abgelaufen");
        t.revoked_at = Some(now);
        assert_eq!(token_state(&t, now).0, "Widerrufen");
    }
}
