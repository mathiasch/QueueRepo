//! WP15: wire types of the model chooser (task analysis, decision report,
//! settings, statistics and the offline replay).
use serde::{Deserialize, Serialize};

use crate::{ModelTier, StepMode, Target};

/// Task type = learning category of the chooser.
pub const CATEGORIES: [&str; 9] = [
    "bugfix", "feature", "refactor", "docs", "tests", "research", "ui", "analysis", "other",
];

/// Risk flags of a task (high risk: never the cheapest tier first, no exploration).
pub const RISKS: [&str; 4] = ["security", "migration", "infra", "data_loss"];

/// Capabilities a task may need.
pub const NEEDS: [&str; 3] = ["vision", "long_context", "web"];

/// Pipeline steps the chooser decides.
pub const CHOOSER_STEPS: [&str; 4] = ["plan", "execute", "review", "analyze"];

/// Default of the quality/cost slider.
pub const DEFAULT_QUALITY: u8 = 50;
/// Default exploration budget per project and 30 days (USD).
pub const DEFAULT_EXPLORE_BUDGET_USD: f64 = 2.0;
/// Longest blocked-model lists.
pub const MAX_BLOCKED_MODELS: usize = 50;

/// German label of a category.
pub fn category_label(c: &str) -> &'static str {
    match c {
        "bugfix" => "Fehlerbehebung",
        "feature" => "Neue Funktion",
        "refactor" => "Umbau",
        "docs" => "Dokumentation",
        "tests" => "Tests",
        "research" => "Recherche",
        "ui" => "Oberfläche",
        "analysis" => "Analyse",
        _ => "Sonstiges",
    }
}

pub fn risk_label(r: &str) -> &'static str {
    match r {
        "security" => "Sicherheit",
        "migration" => "Datenbank/Migration",
        "infra" => "Infrastruktur",
        "data_loss" => "Datenverlust möglich",
        _ => "Risiko",
    }
}

pub fn need_label(n: &str) -> &'static str {
    match n {
        "vision" => "Bilder verstehen",
        "long_context" => "Sehr viel Kontext",
        "web" => "Web-Recherche",
        _ => "Fähigkeit",
    }
}

pub fn size_label(s: &str) -> &'static str {
    match s {
        "S" => "klein",
        "M" => "mittel",
        _ => "groß",
    }
}

pub fn step_label(s: &str) -> &'static str {
    match s {
        "plan" => "Planung",
        "execute" => "Umsetzung",
        "review" => "Review",
        "analyze" => "Analyse",
        _ => "Schritt",
    }
}

pub fn tier_label(t: ModelTier) -> &'static str {
    match t {
        ModelTier::Cheap => "günstig",
        ModelTier::Mid => "mittel",
        ModelTier::Top => "stark",
    }
}

/// Features of a task (heuristic, optionally refined by a cheap model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TaskFeatures {
    /// One of [`CATEGORIES`].
    pub category: String,
    /// `S` | `M` | `L`.
    pub size: String,
    /// 1 (simple) … 3 (hard).
    pub complexity: u8,
    /// Subset of [`RISKS`].
    #[serde(default)]
    pub risks: Vec<String>,
    /// Subset of [`NEEDS`].
    #[serde(default)]
    pub needs: Vec<String>,
    /// Languages and frameworks mentioned (explanation and context).
    #[serde(default)]
    pub languages: Vec<String>,
    /// File paths mentioned (at most 20).
    #[serde(default)]
    pub files: Vec<String>,
    /// Rough prompt size in tokens of one request (context + task).
    #[serde(default)]
    pub est_tokens: u32,
    /// Time pressure: no batch, no flex.
    #[serde(default)]
    pub urgent: bool,
    /// Model that refined the heuristic (`None` = heuristic only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classified_by: Option<String>,
    /// Why the heuristic decided (short German notes).
    #[serde(default)]
    pub signals: Vec<String>,
}

impl TaskFeatures {
    pub fn high_risk(&self) -> bool {
        !self.risks.is_empty()
    }

    pub fn needs(&self, n: &str) -> bool {
        self.needs.iter().any(|x| x == n)
    }
}

/// One candidate (access × model) as shown in the explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateView {
    pub target: Target,
    /// Access label ("Anthropic API", "claude (Einstellungen → Zugänge)").
    pub access: String,
    pub model: String,
    pub family: String,
    pub tier: ModelTier,
    /// Expected first-pass success (posterior mean, context applied).
    pub p_success: f64,
    /// Acceptance including the escalation chain.
    #[serde(default)]
    pub p_accept: f64,
    /// Effective own observations behind `p_success`.
    pub observations: f64,
    /// `benchmark` | `assumption` | `learned` – what `p_success` rests on.
    pub evidence: String,
    /// Expected cost of one attempt (plan once + loop + review), USD.
    pub attempt_cost: f64,
    /// Expected cost per accepted task including the escalation chain, USD.
    pub cost_per_accepted: f64,
    /// Ranking score (lower is better).
    pub score: f64,
    /// Hard constraint that excludes it (German), `None` = allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded: Option<String>,
}

/// Choice of a single step (plan, review, analysis).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepChoice {
    pub step: String,
    pub mode: StepMode,
    pub target: Target,
    pub access: String,
    pub model: String,
    pub tier: ModelTier,
    pub cost: f64,
    pub reason: String,
}

/// One stage of the escalation chain (fix attempts after a failure).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainStep {
    pub target: Target,
    pub access: String,
    pub model: String,
    pub tier: ModelTier,
    pub p_success: f64,
}

/// A manual override of the chooser's choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverrideInfo {
    /// What the chooser had chosen.
    pub from: Target,
    pub from_label: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// The chooser's full report, stored in `RoutingDecision.chooser`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChooserReport {
    pub features: TaskFeatures,
    /// The chosen agent loop (`None` = fallback, see `fallback`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<CandidateView>,
    /// Escalation after a failed attempt (without the first stage).
    #[serde(default)]
    pub chain: Vec<ChainStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<StepChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<StepChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyze: Option<StepChoice>,
    /// OpenAI Flex for the agent loop (no time pressure).
    #[serde(default)]
    pub flex: bool,
    /// Best alternatives incl. excluded ones (at most 12).
    #[serde(default)]
    pub alternatives: Vec<CandidateView>,
    /// Explored (Thompson sample differs from the best mean).
    #[serde(default)]
    pub explored: bool,
    /// Expected extra cost of the exploration (USD).
    #[serde(default)]
    pub explore_cost: f64,
    pub quality: u8,
    /// Why the chooser fell back to the preset/route (German).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Cost of the classification call (USD).
    #[serde(default)]
    pub classify_cost: f64,
    /// Cost line of the classification call and its access (booked on the
    /// task once the task row exists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classify_step: Option<crate::StepCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classify_connector: Option<uuid::Uuid>,
    /// Seed of the Thompson sample (reproducible).
    #[serde(default)]
    pub seed: u64,
    /// Short German explanation, one point per line.
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overridden: Option<OverrideInfo>,
}

/// `GET/PUT /api/projects/{id}/chooser`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChooserSettings {
    pub enabled: bool,
    /// 0 = cheapest, 100 = quality first.
    pub quality: u8,
    /// Exploration budget per 30 days (USD).
    pub explore_budget_usd: f64,
    /// Refine the heuristic with a cheap model (costs are booked).
    pub classify: bool,
    /// Models never chosen in this project.
    #[serde(default)]
    pub blocked_models: Vec<String>,
}

impl Default for ChooserSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            quality: DEFAULT_QUALITY,
            explore_budget_usd: DEFAULT_EXPLORE_BUDGET_USD,
            classify: false,
            blocked_models: vec![],
        }
    }
}

/// `GET /api/projects/{id}/chooser`: settings plus what the project spent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChooserSettingsView {
    pub settings: ChooserSettings,
    /// Exploration spent in the last 30 days (USD).
    pub explore_spent_usd: f64,
    /// Model the classification would use (`None` = heuristic only).
    #[serde(default)]
    pub classify_model: Option<String>,
    pub can_edit: bool,
}

/// `GET/PUT /api/chooser/prefs`: the user's own choices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChooserPrefs {
    /// Own learning data may flow (aggregated) into the global priors.
    pub share_learning: bool,
    #[serde(default)]
    pub blocked_models: Vec<String>,
    /// Read-only: the admin allows pooling at all.
    #[serde(default)]
    pub pooling_enabled: bool,
}

/// `POST /api/tasks/{id}/feedback`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskFeedback {
    pub good: bool,
}

/// `GET /api/tasks/{id}/feedback`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FeedbackView {
    /// `None` = none given yet.
    pub good: Option<bool>,
}

/// `GET /api/projects/{id}/chooser/suggest`: targets for Best-of-N.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AttemptSuggestion {
    pub targets: Vec<Target>,
    pub labels: Vec<String>,
    pub reason: String,
}

/// One learned cell (admin statistics).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearnedCell {
    pub step: String,
    pub category: String,
    pub model: String,
    /// Effective (decayed) observations.
    pub observations: f64,
    /// Decayed successes / observations.
    pub success_rate: f64,
    pub avg_cost: f64,
    /// Distinct users behind the cell.
    pub users: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CountRow {
    pub label: String,
    pub count: i64,
}

/// `GET /api/admin/chooser`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChooserAdminView {
    pub pooling: bool,
    pub half_life_days: i32,
    /// Users who share their learning data.
    pub sharing_users: i64,
    /// Decisions of the last 30 days by source (chooser/fallback/override).
    pub decisions: Vec<CountRow>,
    pub explored: i64,
    pub explore_cost_usd: f64,
    pub classify_calls: i64,
    pub classify_cost_usd: f64,
    /// Mean expected cost per accepted task of the chooser decisions.
    pub expected_cost_usd: f64,
    pub fallback_reasons: Vec<CountRow>,
    pub cells: Vec<LearnedCell>,
    pub projects_enabled: i64,
}

/// `PUT /api/admin/chooser`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChooserAdminUpdate {
    pub pooling: bool,
    pub half_life_days: i32,
}

/// `POST /api/admin/chooser/replay`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayRequest {
    /// `synthetic` (labelled as such) or `history` (finished tasks).
    pub source: String,
    #[serde(default = "default_replay_tasks")]
    pub tasks: u32,
    #[serde(default)]
    pub seed: u64,
}

fn default_replay_tasks() -> u32 {
    600
}

/// One policy's result in the replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayPolicy {
    pub policy: String,
    pub label: String,
    pub tasks: u32,
    pub accepted: u32,
    pub first_pass: u32,
    pub total_cost: f64,
    pub cost_per_accepted: f64,
    pub acceptance_rate: f64,
    pub first_pass_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub source: String,
    /// German description of the data basis (synthetic or which history).
    pub basis: String,
    pub seed: u64,
    pub policies: Vec<ReplayPolicy>,
    /// Chooser vs. the best preset (German).
    pub verdict: String,
    /// Lower cost per accepted task than every preset, or acceptance held.
    pub passed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_roundtrip_and_old_rows() {
        let r: ChooserReport = serde_json::from_str(r#"{"features":{"category":"docs","size":"S","complexity":1},"quality":50}"#).unwrap();
        assert_eq!(r.features.category, "docs");
        assert!(r.chosen.is_none() && r.chain.is_empty() && !r.explored);
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<ChooserReport>(&s).unwrap(), r);
        assert_eq!(ChooserSettings::default().quality, 50);
    }

    #[test]
    fn labels_are_german() {
        for c in CATEGORIES {
            assert!(!category_label(c).is_empty());
        }
        assert_eq!(risk_label("security"), "Sicherheit");
        assert_eq!(need_label("vision"), "Bilder verstehen");
        assert_eq!(size_label("L"), "groß");
        assert_eq!(step_label("review"), "Review");
        assert_eq!(tier_label(ModelTier::Top), "stark");
    }
}
