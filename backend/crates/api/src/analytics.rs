//! WP10: analytics (dashboard) and session insights of one task, shared by
//! the app and the web frontend. All aggregation happens in SQL; these are
//! the wire types plus the plain-language helpers of the UI.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Money;

/// Allowed time windows in days.
pub const WINDOWS: [i32; 3] = [7, 30, 90];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// My own tasks in projects I am a member of.
    #[default]
    Mine,
    /// All tasks of one project (any role in the project).
    Project,
}

/// Query of `GET /api/analytics`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsQuery {
    #[serde(default = "default_days")]
    pub days: i32,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

fn default_days() -> i32 {
    30
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Outcomes {
    /// Finished tasks (done, failed, blocked, cancelled) in the window.
    pub finished: i64,
    pub done: i64,
    pub failed: i64,
    pub blocked: i64,
    pub cancelled: i64,
    /// Original tasks (no repair task) that ended done/failed/blocked.
    pub first_pass_total: i64,
    /// … of which done without any repair task.
    pub first_pass_done: i64,
    /// … of which done in the end, also through repair tasks.
    pub eventual_done: i64,
    /// Repair tasks created for tasks of the window.
    pub fixes: i64,
}

impl Outcomes {
    pub fn first_pass_rate(&self) -> Option<f64> {
        ratio(self.first_pass_done, self.first_pass_total)
    }
    pub fn eventual_rate(&self) -> Option<f64> {
        ratio(self.eventual_done, self.first_pass_total)
    }
}

pub fn ratio(a: i64, b: i64) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl Tokens {
    /// Share of input tokens served from the provider cache.
    pub fn cache_ratio(&self) -> Option<f64> {
        ratio(
            self.cache_read,
            self.input
                .saturating_add(self.cache_read)
                .saturating_add(self.cache_write),
        )
    }
    pub fn total(&self) -> i64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Durations {
    /// Done tasks with timing data.
    pub done: i64,
    /// Created → done (median, 90th percentile), seconds.
    pub to_done_median: Option<f64>,
    pub to_done_p90: Option<f64>,
    /// Pure agent time per done task (sum of its runs), median seconds.
    pub agent_median: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRow {
    pub project_id: Uuid,
    pub name: String,
    pub finished: i64,
    pub done: i64,
    pub costs: Vec<Money>,
    pub tokens: Tokens,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRow {
    /// Harness (claude, codex, …).
    pub provider: String,
    /// Price catalog id, `None` = unknown model.
    pub model: Option<String>,
    pub tokens: Tokens,
    pub costs: Vec<Money>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskCostRow {
    pub task_id: Uuid,
    pub title: String,
    pub project: String,
    pub status: String,
    pub costs: Vec<Money>,
    pub tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayRow {
    pub day: NaiveDate,
    pub done: i64,
    pub failed: i64,
    /// failed + blocked.
    #[serde(default)]
    pub blocked: i64,
    /// USD only (other currencies are in the totals).
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PauseRow {
    /// Harness or connector label.
    pub provider: String,
    /// rate_limit | auth | budget
    pub reason: String,
    pub pauses: i64,
    pub minutes: i64,
}

/// `GET /api/analytics`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analytics {
    pub days: i32,
    pub scope: Scope,
    pub project_id: Option<Uuid>,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub outcomes: Outcomes,
    pub durations: Durations,
    pub costs: Vec<Money>,
    pub cost_per_done: Vec<Money>,
    pub tokens: Tokens,
    pub by_project: Vec<ProjectRow>,
    pub by_provider: Vec<ProviderRow>,
    pub top_tasks: Vec<TaskCostRow>,
    pub daily: Vec<DayRow>,
    /// Pauses are per user: only for scope `mine`.
    pub pauses: Option<Vec<PauseRow>>,
    /// WP12: Best-of-N groups and attempts of the window.
    #[serde(default)]
    pub best_of_n: BestOfN,
}

/// WP12: Best-of-N numbers (groups created in the window). Attempts are not
/// counted as tasks anywhere else; their costs are part of every cost figure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BestOfN {
    pub groups: i64,
    pub attempts: i64,
    /// Attempts that passed gate, verify and review.
    pub successful_attempts: i64,
    pub selected_by_user: i64,
    pub selected_auto: i64,
    /// Groups stopped by the budget of all attempts.
    pub budget_stops: i64,
}

/// One agent run in the session insights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    /// agent | review | plan
    pub kind: String,
    pub provider: String,
    pub model: Option<String>,
    pub started_at: DateTime<Utc>,
    pub seconds: f64,
    pub outcome: String,
    pub tokens: Tokens,
    pub tool_calls: i64,
}

/// `GET /api/tasks/{id}/insights`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskInsights {
    pub task_id: Uuid,
    pub runs: Vec<RunRow>,
    /// Created → finished (or now), seconds.
    pub wall_seconds: f64,
    /// Sum of the runs.
    pub agent_seconds: f64,
    pub tokens: Tokens,
    pub costs: Vec<Money>,
    /// Most used tools: (name, calls), at most 8.
    pub tools: Vec<(String, i64)>,
    pub transient_retries: i32,
    /// Repair tasks created for this task (and their repairs).
    pub fixes: i64,
    /// Plain-language observations.
    pub notes: Vec<String>,
}

/// Plain-language observations for the insights (rules, no model).
pub fn insight_notes(i: &TaskInsights) -> Vec<String> {
    let mut out = vec![];
    if i.runs.is_empty() {
        out.push("Noch kein Agentenlauf aufgezeichnet.".into());
        return out;
    }
    let agent_runs = i.runs.iter().filter(|r| r.kind == "agent").count();
    if agent_runs > 1 {
        out.push(format!(
            "Der Agent lief {agent_runs}-mal (Wiederholungen nach Rückfragen, Unterbrechungen oder Steuerung)."
        ));
    }
    if let Some(r) = i.tokens.cache_ratio() {
        if r >= 0.5 {
            out.push(format!(
                "{} % der Eingabe kamen aus dem Cache des Anbieters – das spart Kosten.",
                (r * 100.0).round()
            ));
        } else if i.tokens.total() > 50_000 && r < 0.1 {
            out.push("Kaum Cache-Treffer: jeder Lauf hat den Kontext neu eingelesen.".into());
        }
    }
    if i.wall_seconds > 0.0 && i.agent_seconds > 0.0 && i.agent_seconds / i.wall_seconds < 0.2 {
        out.push(
            "Die meiste Zeit hat der Auftrag gewartet (Warteschlange, Freigaben, Limits) – nicht gearbeitet."
                .into(),
        );
    }
    if i.transient_retries > 0 {
        out.push(format!(
            "{} automatische Wiederholung{} wegen Anbieter- oder Infrastrukturproblemen (zählen nicht als Reparatur).",
            i.transient_retries,
            if i.transient_retries == 1 { "" } else { "en" }
        ));
    }
    if i.fixes > 0 {
        out.push(if i.fixes == 1 {
            "1 Reparaturauftrag war nötig.".to_string()
        } else {
            format!("{} Reparaturaufträge waren nötig.", i.fixes)
        });
    }
    let calls: i64 = i.runs.iter().map(|r| r.tool_calls).sum();
    if calls >= 100 {
        out.push(format!(
            "{calls} Werkzeugaufrufe – ein sehr langer Lauf; kleinere Aufträge sind oft günstiger."
        ));
    }
    if out.is_empty() {
        out.push("Unauffälliger Lauf.".into());
    }
    out
}

/// Seconds as short German text (`45 s`, `12 min`, `3 h 5 min`, `2 Tage`).
pub fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as i64;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else if s < 86_400 {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if m == 0 {
            format!("{h} h")
        } else {
            format!("{h} h {m} min")
        }
    } else {
        let d = s / 86_400;
        format!("{d} Tag{}", if d == 1 { "" } else { "e" })
    }
}

/// Percentage for display (`73 %`, `–` without data).
pub fn fmt_percent(r: Option<f64>) -> String {
    match r {
        Some(r) => format!("{} %", (r * 100.0).round() as i64),
        None => "–".into(),
    }
}

pub fn pause_reason_label(r: &str) -> &'static str {
    match r {
        "rate_limit" => "Limit des Anbieters",
        "auth" => "Anmeldeproblem",
        "budget" => "Eigenes Budget",
        _ => "Sonstiges",
    }
}

pub fn outcome_label(o: &str) -> &'static str {
    match o {
        "success" => "erfolgreich",
        "failure" | "task_error" | "task" => "Fehler im Auftrag",
        "rate_limit" => "Limit des Anbieters",
        "provider_auth" => "Anmeldeproblem",
        "provider_transient" => "Anbieter gestört",
        "infra" => "Infrastruktur",
        "stalled" => "keine Ausgabe mehr",
        "timeout" => "Zeit überschritten",
        "cancelled" => "abgebrochen",
        "limit" => "Limit des Auftrags",
        "steered" => "für Nachricht unterbrochen",
        "config" => "Konfigurationsfehler",
        _ => "unbekannt",
    }
}

pub const FIRST_PASS_HELP: &str = "Anteil der ursprünglichen Aufträge, die ohne Reparaturauftrag erledigt wurden. Abgebrochene Aufträge zählen nicht mit.";
pub const CACHE_HELP: &str = "Anteil der Eingabe-Tokens, die der Anbieter aus seinem Zwischenspeicher lieferte. Cache-Treffer kosten meist nur ein Zehntel.";
pub const PAUSE_HELP: &str = "Zeit, in der die Warteschlange für einen Anbieter angehalten war: Limit des Anbieters, Anmeldeproblem oder dein eigenes Budget. Überlappende Pausen zählen einmal.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_formatting() {
        let o = Outcomes {
            first_pass_total: 4,
            first_pass_done: 3,
            eventual_done: 4,
            ..Default::default()
        };
        assert_eq!(o.first_pass_rate(), Some(0.75));
        assert_eq!(o.eventual_rate(), Some(1.0));
        assert_eq!(Outcomes::default().first_pass_rate(), None);
        let t = Tokens {
            input: 100,
            output: 50,
            cache_read: 300,
            cache_write: 100,
        };
        assert_eq!(t.cache_ratio(), Some(0.6));
        assert_eq!(fmt_percent(t.cache_ratio()), "60 %");
        assert_eq!(fmt_percent(None), "–");
        assert_eq!(fmt_duration(42.0), "42 s");
        assert_eq!(fmt_duration(600.0), "10 min");
        assert_eq!(fmt_duration(3600.0 * 3.0 + 300.0), "3 h 5 min");
        assert_eq!(fmt_duration(86_400.0 * 2.5), "2 Tage");
    }

    #[test]
    fn notes_explain_in_plain_language() {
        let run = |kind: &str, calls: i64| RunRow {
            kind: kind.into(),
            provider: "mock".into(),
            model: None,
            started_at: Utc::now(),
            seconds: 10.0,
            outcome: "success".into(),
            tokens: Tokens::default(),
            tool_calls: calls,
        };
        let mut i = TaskInsights {
            task_id: Uuid::nil(),
            runs: vec![],
            wall_seconds: 0.0,
            agent_seconds: 0.0,
            tokens: Tokens::default(),
            costs: vec![],
            tools: vec![],
            transient_retries: 0,
            fixes: 0,
            notes: vec![],
        };
        assert_eq!(
            insight_notes(&i),
            vec!["Noch kein Agentenlauf aufgezeichnet."]
        );
        i.runs = vec![run("agent", 60), run("agent", 60), run("review", 0)];
        i.wall_seconds = 1000.0;
        i.agent_seconds = 30.0;
        i.tokens = Tokens {
            input: 10,
            cache_read: 90,
            ..Default::default()
        };
        i.transient_retries = 1;
        i.fixes = 2;
        let n = insight_notes(&i).join("\n");
        assert!(n.contains("2-mal"), "{n}");
        assert!(n.contains("90 % der Eingabe"), "{n}");
        assert!(n.contains("gewartet"), "{n}");
        assert!(n.contains("1 automatische Wiederholung "), "{n}");
        assert!(n.contains("2 Reparaturaufträge waren"), "{n}");
        assert!(n.contains("120 Werkzeugaufrufe"), "{n}");
    }
}
