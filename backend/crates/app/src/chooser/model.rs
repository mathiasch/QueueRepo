//! Model knowledge of the chooser (WP15): which harness serves which model
//! family, capabilities (context window, vision, web search) and the
//! success priors.
//!
//! **Everything here is an assumption or a secondary benchmark** – no public
//! data on first-pass rates of autonomous queues exists (report: "Alle
//! p-Werte sind Annahmen"). The values are labelled as such in the UI; own
//! runs replace them (bandit), and a model without benchmark evidence is
//! only trusted after [`super::bandit::MIN_SAMPLE`] own observations.
use queued_api::ModelTier;

use crate::routing::Access;

/// Model families the agent harness of an access can run.
pub fn harness_families(a: &Access) -> Vec<&'static str> {
    match a.kind.as_str() {
        "anthropic_api" | "claude_subscription" | "legacy:claude" | "bedrock" | "vertex"
        | "anthropic_gateway" => vec!["anthropic"],
        "openai_api" | "codex_subscription" | "legacy:codex" | "azure_openai" => vec!["openai"],
        "gemini_subscription" | "legacy:gemini" | "gemini_api" => vec!["google"],
        "mistral" => vec!["mistral"],
        "infomaniak" => vec!["infomaniak"],
        "legacy:mock" => vec!["anthropic"],
        "mock" => match a.wire_api.as_deref() {
            Some("openai") => vec!["openai"],
            Some("mistral") => vec!["mistral"],
            _ => vec!["anthropic"],
        },
        // OpenRouter, OpenAI-compatible endpoints and LiteLLM name models
        // freely: only through the project routing, never guessed.
        _ => vec![],
    }
}

/// Whether the harness has a web search tool (assumption from the CLI
/// documentation: Claude Code, Codex and Gemini CLI search; opencode only
/// fetches given URLs).
pub fn harness_web_search(harness: &str) -> bool {
    matches!(harness, "claude" | "codex" | "gemini" | "mock")
}

/// Capabilities of a model (assumptions, see module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caps {
    pub context_tokens: u32,
    pub vision: bool,
}

/// Capabilities by price catalog id and family.
pub fn caps(price_id: &str, family: &str) -> Caps {
    let ctx = match price_id {
        // Report: "Large 4 … 1M Kontext" (search excerpt).
        "mistral/mistral-large-4" | "mistral/mistral-large-4-pre-oss" => 1_000_000,
        _ => match family {
            // OpenAI bills a tier above 272K input, so the window is larger.
            "openai" => 400_000,
            "google" => 1_000_000,
            "anthropic" => 200_000,
            "mistral" | "deepseek" | "infomaniak" => 128_000,
            _ => 128_000,
        },
    };
    let vision = match price_id {
        "mistral/devstral-2"
        | "mistral/devstral-small-2"
        | "infomaniak/apertus-v1-5-70b"
        | "deepseek/deepseek-v4-flash" => false,
        _ => true,
    };
    Caps {
        context_tokens: ctx,
        vision,
    }
}

/// Basis of a prior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// A (secondary) coding benchmark exists: trusted from the start.
    Benchmark,
    /// Only the report's assumption or the tier default.
    Assumption,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Benchmark => "benchmark",
            Self::Assumption => "assumption",
        }
    }
}

/// Prior of a model: mean first-pass acceptance of an M task and strength
/// (pseudo observations).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prior {
    pub mean: f64,
    pub strength: f64,
    pub evidence: Evidence,
}

/// Priors by price id. Means = the report's end-to-end acceptance
/// assumptions per route (R1 Opus 0.85, R2 Sonnet 0.75, R3 Haiku 0.45,
/// R4 Terra 0.65, R6 Large 4 0.60); evidence = whether the report lists a
/// coding benchmark for the model (SWE-bench Pro: Opus 89.9 %, Sonnet
/// 81.3 %, Haiku 64.8 % secondary; Gemini 3.8 Flash 80 % Verified;
/// Medium 3.5 77.6 % self-reported).
pub fn prior(price_id: &str, tier: ModelTier) -> Prior {
    let (mean, ev) = match price_id {
        "anthropic/claude-opus-5-5" => (0.85, Evidence::Benchmark),
        "anthropic/claude-sonnet-5-5" => (0.75, Evidence::Benchmark),
        "anthropic/claude-haiku-5-5" => (0.45, Evidence::Benchmark),
        "openai/gpt-6-1-terra" => (0.65, Evidence::Assumption),
        "openai/gpt-6-1-sol" => (0.72, Evidence::Assumption),
        "openai/gpt-6-astra" => (0.82, Evidence::Assumption),
        "openai/gpt-6-luna" => (0.40, Evidence::Assumption),
        "mistral/mistral-large-4" | "mistral/mistral-large-4-pre-oss" => {
            (0.60, Evidence::Assumption)
        }
        "mistral/mistral-medium-3-5" => (0.62, Evidence::Benchmark),
        "google/gemini-3-8-flash" | "google/gemini-3-8-flash-2026" => {
            (0.62, Evidence::Benchmark)
        }
        _ => (
            match tier {
                ModelTier::Cheap => 0.40,
                ModelTier::Mid => 0.62,
                ModelTier::Top => 0.80,
            },
            Evidence::Assumption,
        ),
    };
    Prior {
        mean,
        strength: match ev {
            Evidence::Benchmark => 10.0,
            Evidence::Assumption => 4.0,
        },
        evidence: ev,
    }
}

/// Logit shift of a category (docs/tests are easier than refactors).
pub fn category_offset(category: &str) -> f64 {
    match category {
        "docs" => 0.5,
        "tests" => 0.3,
        "research" | "analysis" => 0.3,
        "ui" => -0.1,
        "refactor" => -0.3,
        _ => 0.0,
    }
}

/// Context shift of size, complexity and risk (applied to the learned or
/// sampled cell probability, which is learned across sizes).
pub fn context_offset(size: &str, complexity: u8, high_risk: bool) -> f64 {
    let s = match size {
        "S" => 0.7,
        "L" => -0.8,
        _ => 0.0,
    };
    let c = match complexity {
        0 | 1 => 0.2,
        2 => 0.0,
        _ => -0.5,
    };
    s + c - if high_risk { 0.3 } else { 0.0 }
}

pub fn logit(p: f64) -> f64 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    (p / (1.0 - p)).ln()
}

pub fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// Prior of a cell (model × category) as Beta parameters.
pub fn cell_prior(price_id: &str, tier: ModelTier, category: &str) -> (f64, f64, Evidence) {
    let p = prior(price_id, tier);
    let m = sigmoid(logit(p.mean) + category_offset(category));
    (m * p.strength, (1.0 - m) * p.strength, p.evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::DataLocation;

    fn acc(kind: &str) -> Access {
        Access {
            connector_id: None,
            kind: kind.into(),
            label: kind.into(),
            harness: "x".into(),
            location: DataLocation::Us,
            single_shot: false,
            batch: false,
            flex: false,
            wire_api: None,
        }
    }

    #[test]
    fn families_per_harness() {
        assert_eq!(harness_families(&acc("bedrock")), vec!["anthropic"]);
        assert_eq!(harness_families(&acc("openai_api")), vec!["openai"]);
        assert_eq!(harness_families(&acc("mistral")), vec!["mistral"]);
        assert!(harness_families(&acc("openrouter")).is_empty());
        assert!(harness_web_search("claude") && !harness_web_search("opencode"));
    }

    #[test]
    fn priors_order_and_evidence() {
        let o = prior("anthropic/claude-opus-5-5", ModelTier::Top);
        let s = prior("anthropic/claude-sonnet-5-5", ModelTier::Mid);
        let h = prior("anthropic/claude-haiku-5-5", ModelTier::Cheap);
        assert!(o.mean > s.mean && s.mean > h.mean);
        assert_eq!(o.evidence, Evidence::Benchmark);
        // No benchmark for Terra (report): an assumption, weaker prior.
        let t = prior("openai/gpt-6-1-terra", ModelTier::Mid);
        assert_eq!(t.evidence, Evidence::Assumption);
        assert!(t.strength < o.strength);
        // Unknown catalog line: tier default.
        assert_eq!(prior("x/y", ModelTier::Cheap).mean, 0.40);
    }

    #[test]
    fn offsets() {
        let (a, b, _) = cell_prior("anthropic/claude-haiku-5-5", ModelTier::Cheap, "docs");
        let (c, d, _) = cell_prior("anthropic/claude-haiku-5-5", ModelTier::Cheap, "refactor");
        assert!(a / (a + b) > c / (c + d));
        assert!(context_offset("S", 1, false) > context_offset("L", 3, true));
        assert!((sigmoid(logit(0.3)) - 0.3).abs() < 1e-9);
        assert!(!caps("mistral/devstral-2", "mistral").vision);
        assert_eq!(caps("mistral/mistral-large-4", "mistral").context_tokens, 1_000_000);
    }
}
