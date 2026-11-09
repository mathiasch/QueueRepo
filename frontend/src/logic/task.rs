//! Task-level presentation logic (tags, hints, chains).
use chrono::{DateTime, TimeZone, Utc};
use queued_api::{FailureClass, Task, TaskStatus};

use super::format::fmt_clock_secs;
use super::labels::{failure_label, Tone};

/// Tag for runs that ended for a reason other than a plain task failure
/// (those get a repair task instead).
pub fn failure_tag(t: &Task) -> Option<(&'static str, Tone)> {
    let c = t.failure()?;
    let s = t.status_kind();
    if matches!(c, FailureClass::Task | FailureClass::Cancelled) || s == TaskStatus::Done {
        return None;
    }
    Some((
        failure_label(c),
        if s == TaskStatus::Blocked {
            Tone::Danger
        } else {
            Tone::Warning
        },
    ))
}

/// The abort-reason block is shown for the same runs as [`failure_tag`].
pub fn shows_abort_reason(t: &Task) -> bool {
    failure_tag(t).is_some()
}

/// `wartet bis 14:05:10 (Wiederholung 2)` for a re-queued task in backoff.
pub fn retry_hint<Tz: TimeZone>(t: &Task, now: DateTime<Utc>, tz: &Tz) -> Option<String> {
    if t.status_kind() != TaskStatus::Queued {
        return None;
    }
    let at = t.not_before.filter(|at| *at > now)?;
    let mut s = format!("wartet bis {}", fmt_clock_secs(at, tz));
    if t.transient_retries > 0 {
        s.push_str(&format!(" (Wiederholung {})", t.transient_retries));
    }
    Some(s)
}

/// First line of a text, shortened to `max` characters.
pub fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        let mut out: String = line.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        line.to_string()
    }
}

/// Sort key for the history: newest first.
pub fn finished_key(t: &Task) -> DateTime<Utc> {
    t.finished_at.unwrap_or(t.updated_at)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use queued_api::Task;
    use uuid::Uuid;

    pub fn task(status: &str) -> Task {
        Task {
            id: Uuid::from_u128(1),
            user_id: Uuid::from_u128(2),
            project_id: Uuid::from_u128(3),
            parent_id: None,
            kind: "task".into(),
            title: "Titel".into(),
            prompt: "Prompt".into(),
            context: String::new(),
            provider: "mock".into(),
            model: None,
            status: status.into(),
            position: 0,
            attempt: 0,
            depth: 0,
            created_by_agent: false,
            questions: None,
            handoff: None,
            summary: None,
            error: None,
            review: None,
            usage: None,
            snapshot_before: None,
            snapshot_after: None,
            created_at: "2026-01-01T09:00:00Z".parse().unwrap(),
            started_at: None,
            finished_at: None,
            updated_at: "2026-01-01T09:00:00Z".parse().unwrap(),
            max_runtime_secs: None,
            max_cost_usd: None,
            max_tokens: None,
            max_turns: None,
            failure_class: None,
            transient_retries: 0,
            not_before: None,
            connector_id: None,
            escalation_level: 0,
            plan: None,
            cost: None,
            gate: None,
            allow_test_changes: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::task;
    use super::*;
    use crate::logic::limits::task_limits;

    fn with(status: &str, class: Option<&str>) -> Task {
        let mut t = task(status);
        t.failure_class = class.map(Into::into);
        t
    }

    #[test]
    fn tags_only_non_task_failures_of_unfinished_tasks() {
        assert_eq!(failure_tag(&with("queued", None)), None);
        assert_eq!(failure_tag(&with("failed", Some("task"))), None);
        assert_eq!(failure_tag(&with("cancelled", Some("cancelled"))), None);
        assert_eq!(failure_tag(&with("done", Some("provider_transient"))), None);
        assert_eq!(
            failure_tag(&with("queued", Some("provider_transient"))),
            Some(("Provider gestört", Tone::Warning))
        );
        assert_eq!(
            failure_tag(&with("blocked", Some("limit_tokens"))),
            Some(("Limit: Tokens", Tone::Danger))
        );
        assert!(shows_abort_reason(&with("blocked", Some("stalled"))));
        assert!(!shows_abort_reason(&with("blocked", Some("bogus"))));
    }

    #[test]
    fn retry_hint_only_while_the_backoff_is_pending() {
        let now: DateTime<Utc> = "2026-01-01T10:00:00Z".parse().unwrap();
        let mut t = task("queued");
        t.not_before = Some("2026-01-01T10:00:30Z".parse().unwrap());
        t.transient_retries = 2;
        assert_eq!(
            retry_hint(&t, now, &Utc).as_deref(),
            Some("wartet bis 10:00:30 (Wiederholung 2)")
        );
        let mut past = t.clone();
        past.not_before = Some("2026-01-01T09:59:59Z".parse().unwrap());
        assert_eq!(retry_hint(&past, now, &Utc), None);
        let mut running = t.clone();
        running.status = "running".into();
        assert_eq!(retry_hint(&running, now, &Utc), None);
        let mut none = t.clone();
        none.not_before = None;
        assert_eq!(retry_hint(&none, now, &Utc), None);
        let mut first = t.clone();
        first.transient_retries = 0;
        assert!(!retry_hint(&first, now, &Utc)
            .unwrap()
            .contains("Wiederholung"));
    }

    #[test]
    fn lists_task_limits() {
        let mut t = task("queued");
        t.max_runtime_secs = Some(120);
        t.max_cost_usd = Some(0.5);
        t.max_tokens = Some(20_000);
        t.max_turns = Some(3);
        assert_eq!(
            task_limits(&t),
            vec!["Laufzeit 2 Min.", "Kosten 0,5 $", "Tokens 20k", "Turns 3"]
        );
        assert!(task_limits(&task("queued")).is_empty());
    }

    #[test]
    fn text_helpers() {
        assert_eq!(first_line("\n  hello world \nsecond", 120), "hello world");
        assert_eq!(first_line("abcdef", 4), "abc…");
        assert_eq!(first_line("", 4), "");
        let mut t = task("done");
        assert_eq!(finished_key(&t), t.updated_at);
        t.finished_at = Some("2026-01-02T00:00:00Z".parse().unwrap());
        assert_eq!(finished_key(&t), t.finished_at.unwrap());
    }
}
