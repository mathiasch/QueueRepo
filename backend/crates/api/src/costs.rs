//! Price catalog, money costs and cost estimates (WP13).
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Usage;

/// How a price was verified (see `reports/Kostenoptimale Modellanbieter für Queue.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// Provider page read in full.
    Primary,
    /// Excerpt of the provider's own page from a web search (page not opened).
    SearchExcerpt,
    /// Aggregator, blog or tracker.
    Secondary,
    /// Own assumption / calculation.
    Assumption,
    /// Set by an administrator of this server.
    Admin,
}

impl Evidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Primary => "Anbieterseite gelesen",
            Self::SearchExcerpt => "Suchauszug der Anbieterseite",
            Self::Secondary => "Sekundärquelle",
            Self::Assumption => "Annahme",
            Self::Admin => "vom Admin gesetzt",
        }
    }
}

/// Quality tier used by routing rules ("review never on the cheapest tier").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Cheap,
    Mid,
    Top,
}

/// Request-level long-context rule: when the prompt of one request (input +
/// cache reads + cache writes) exceeds `threshold_tokens`, the whole request
/// is billed with these prices. Absolute prices win over multipliers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LongContext {
    pub threshold_tokens: i64,
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_write_short: Option<f64>,
    pub cache_write_long: Option<f64>,
    pub cache_read: Option<f64>,
    pub input_mult: Option<f64>,
    pub output_mult: Option<f64>,
    /// Multiplier for cache reads and writes.
    pub cache_mult: Option<f64>,
}

/// One price line of the catalog, in `currency` per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceEntry {
    pub id: String,
    pub label: String,
    /// anthropic | openai | google | mistral | infomaniak | deepseek
    pub family: String,
    /// Model ids/aliases this line applies to (normalised match).
    pub models: Vec<String>,
    pub tier: ModelTier,
    pub currency: String,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_write_short: Option<f64>,
    #[serde(default)]
    pub cache_write_long: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    /// e.g. "5m" (Anthropic) or "30m" (OpenAI).
    #[serde(default)]
    pub cache_ttl_short: Option<String>,
    #[serde(default)]
    pub cache_ttl_long: Option<String>,
    /// Batch price as a factor of the standard price (0.5 = −50 %).
    #[serde(default)]
    pub batch_factor: Option<f64>,
    /// OpenAI Flex service tier factor.
    #[serde(default)]
    pub flex_factor: Option<f64>,
    #[serde(default)]
    pub long_context: Option<LongContext>,
    /// Inclusive start date (UTC) – announced price changes.
    #[serde(default)]
    pub effective_from: Option<NaiveDate>,
    /// Exclusive end date (UTC).
    #[serde(default)]
    pub effective_until: Option<NaiveDate>,
    /// Key into [`PriceCatalog::sources`].
    pub source: String,
    pub evidence: Evidence,
    #[serde(default)]
    pub notes: String,
}

/// Regional or channel surcharge (e.g. Bedrock EU +10 %).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionRule {
    pub id: String,
    pub label: String,
    pub multiplier: f64,
    pub source: String,
    pub evidence: Evidence,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceSource {
    pub id: String,
    pub title: String,
    pub url: String,
}

/// `GET /api/prices`: the effective catalog (built-in + admin overrides).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCatalog {
    /// Version of the built-in catalog (date of the research).
    pub version: String,
    /// Latest admin override version applied (`None` = built-in only).
    #[serde(default)]
    pub override_version: Option<i32>,
    pub entries: Vec<PriceEntry>,
    pub regions: Vec<RegionRule>,
    pub sources: Vec<PriceSource>,
}

/// `PUT /api/admin/prices`: replaces the override set (new version).
/// Entries with the id of a built-in line replace it; others are added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PriceOverride {
    #[serde(default)]
    pub entries: Vec<PriceEntry>,
    #[serde(default)]
    pub regions: Vec<RegionRule>,
    #[serde(default)]
    pub note: String,
}

/// `GET /api/admin/prices/history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceOverrideVersion {
    pub version: i32,
    pub note: String,
    pub created_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub entries: usize,
}

/// Where a cost figure comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    /// Token counters × price catalog.
    Catalog,
    /// Dollar value reported by the CLI (no catalog price for the model).
    Cli,
    /// No price known.
    #[default]
    Unknown,
}

impl CostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Cli => "cli",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "catalog" => Self::Catalog,
            "cli" => Self::Cli,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Money {
    pub currency: String,
    pub amount: f64,
}

/// Adds `amount` to the entry of `currency` (kept sorted, USD first).
pub fn add_money(totals: &mut Vec<Money>, currency: &str, amount: f64) {
    match totals.iter_mut().find(|m| m.currency == currency) {
        Some(m) => m.amount += amount,
        None => {
            totals.push(Money {
                currency: currency.to_string(),
                amount,
            });
            totals.sort_by_key(|m| (m.currency != "USD", m.currency.clone()));
        }
    }
}

/// Cost of one pipeline step of a task (summed over its runs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepCost {
    /// execute | plan | review | analyze
    pub step: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub connector: Option<String>,
    pub usage: Usage,
    pub currency: String,
    pub amount: f64,
    pub source: CostSource,
    #[serde(default)]
    pub price_id: Option<String>,
    #[serde(default)]
    pub runs: i32,
    /// e.g. "Long-Context-Aufschlag nicht prüfbar".
    #[serde(default)]
    pub notes: Vec<String>,
}

/// `tasks.cost`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TaskCost {
    pub steps: Vec<StepCost>,
    pub totals: Vec<Money>,
    /// Some step has no known price.
    #[serde(default)]
    pub incomplete: bool,
    #[serde(default)]
    pub price_version: String,
}

/// `POST /api/estimate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct EstimateRequest {
    pub project_id: Uuid,
    #[serde(default)]
    pub connector_id: Option<Uuid>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepEstimate {
    pub step: String,
    /// sync | batch | harness
    pub mode: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub connector: Option<String>,
    /// Token profile of the middle estimate.
    pub tokens: Usage,
    pub currency: String,
    pub low: f64,
    pub mid: f64,
    pub high: f64,
    /// A price for the model is known.
    pub known: bool,
    /// Where the token profile comes from (history or report assumption).
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub steps: Vec<StepEstimate>,
    pub low: Vec<Money>,
    pub mid: Vec<Money>,
    pub high: Vec<Money>,
    /// Data location of the agent loop (e.g. "EU").
    pub location: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostRow {
    pub key: String,
    pub label: String,
    pub currency: String,
    pub amount: f64,
    pub tokens: i64,
    pub runs: i64,
}

/// `GET /api/costs?days=N[&project_id=…]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostSummary {
    pub days: i64,
    pub totals: Vec<Money>,
    pub by_project: Vec<CostRow>,
    pub by_model: Vec<CostRow>,
    pub by_step: Vec<CostRow>,
    /// Runs without a known price (cost counted as 0).
    pub unknown_runs: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_totals_per_currency_usd_first() {
        let mut t = vec![];
        add_money(&mut t, "CHF", 1.0);
        add_money(&mut t, "USD", 2.0);
        add_money(&mut t, "CHF", 0.5);
        assert_eq!(t[0].currency, "USD");
        assert_eq!(t[1].amount, 1.5);
    }

    #[test]
    fn cost_source_roundtrip() {
        for s in [CostSource::Catalog, CostSource::Cli, CostSource::Unknown] {
            assert_eq!(CostSource::parse(s.as_str()), s);
            assert_eq!(serde_json::to_value(s).unwrap(), s.as_str());
        }
    }
}
