//! Cost estimate per task and step (WP13).
//!
//! Token profiles: the user's own history for the model (median, P10/P90 of
//! finished tasks, at least [`MIN_HISTORY`]) or the calibrated profiles of the
//! report (S/M/L for the agent loop, single-shot plan/review/analysis) –
//! these are **assumptions** and are labelled as such.
use queued_api::{
    add_money, Estimate, Money, PriceCatalog, Routing, StepEstimate, StepMode, Target, Usage,
};

use crate::{
    connectors,
    pricing::{self, PriceCtx, Tier},
    routing::{self, Access},
};

pub const MIN_HISTORY: usize = 3;

const fn u(i: i64, cw: i64, cr: i64, o: i64) -> Usage {
    Usage {
        input_tokens: i,
        output_tokens: o,
        cache_read_tokens: cr,
        cache_write_tokens: cw,
        cost_usd: 0.0,
    }
}

/// Report profiles (assumption): S 15 turns, M 40 turns, L 120 turns.
pub const LOOP_S: Usage = u(5_000, 60_000, 500_000, 25_000);
pub const LOOP_M: Usage = u(10_000, 150_000, 2_500_000, 60_000);
pub const LOOP_L: Usage = u(20_000, 400_000, 9_000_000, 150_000);
/// Single-shot steps (assumption of the report).
pub const PLAN: Usage = u(40_000, 0, 0, 6_000);
pub const REVIEW: Usage = u(35_000, 0, 0, 3_000);
pub const ANALYZE: Usage = u(150_000, 0, 0, 10_000);
pub const REPORT_BASIS: &str = "Annahme (Token-Profil S/M/L aus dem Kostenbericht)";

/// Profile of the agent loop: (low, mid, high, basis).
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub low: Usage,
    pub mid: Usage,
    pub high: Usage,
    pub basis: String,
}

impl Profile {
    pub fn report() -> Self {
        Self {
            low: LOOP_S,
            mid: LOOP_M,
            high: LOOP_L,
            basis: REPORT_BASIS.into(),
        }
    }

    pub fn single(u: Usage, basis: &str) -> Self {
        Self {
            low: u.clone(),
            mid: u.clone(),
            high: u,
            basis: basis.into(),
        }
    }

    /// Median and P10/P90 by total tokens of finished tasks.
    pub fn from_history(mut runs: Vec<Usage>) -> Option<Self> {
        if runs.len() < MIN_HISTORY {
            return None;
        }
        let total = |u: &Usage| {
            u.input_tokens
                .saturating_add(u.output_tokens)
                .saturating_add(u.cache_read_tokens)
                .saturating_add(u.cache_write_tokens)
        };
        runs.sort_by_key(total);
        let at = |q: f64| runs[((runs.len() - 1) as f64 * q).round() as usize].clone();
        Some(Self {
            low: at(0.1),
            mid: at(0.5),
            high: at(0.9),
            basis: format!(
                "Deine bisherigen Aufträge mit diesem Modell (n={})",
                runs.len()
            ),
        })
    }
}

/// Price context of an access for a step.
pub fn ctx(
    cat: &PriceCatalog,
    a: Option<&Access>,
    kind_cfg: Option<&serde_json::Value>,
    mode: StepMode,
    flex: bool,
) -> PriceCtx {
    let region_mult = match (a, kind_cfg) {
        (Some(a), Some(cfg)) => connectors::region_rule(&a.kind, cfg)
            .and_then(|r| pricing::region(cat, r))
            .map_or(1.0, |r| r.multiplier),
        (Some(a), None) => region_default(cat, &a.kind),
        _ => 1.0,
    };
    PriceCtx {
        tier: match mode {
            StepMode::Batch => Tier::Batch,
            _ if flex => Tier::Flex,
            _ => Tier::Standard,
        },
        region_mult,
        long_ttl: mode == StepMode::Batch,
    }
}

/// Region surcharge with the default config of a kind (EU endpoints).
fn region_default(cat: &PriceCatalog, kind: &str) -> f64 {
    connectors::type_info(kind, true)
        .map(|t| {
            let mut cfg = serde_json::Map::new();
            for f in t.fields {
                if let Some(d) = f.default {
                    cfg.insert(f.key, serde_json::Value::String(d));
                }
            }
            serde_json::Value::Object(cfg)
        })
        .and_then(|cfg| connectors::region_rule(kind, &cfg))
        .and_then(|r| pricing::region(cat, r))
        .map_or(1.0, |r| r.multiplier)
}

pub struct StepInput<'a> {
    pub step: &'static str,
    pub mode: StepMode,
    pub access: Option<&'a Access>,
    pub config: Option<&'a serde_json::Value>,
    pub model: Option<&'a str>,
    pub profile: Profile,
    pub flex: bool,
}

pub fn step(cat: &PriceCatalog, s: StepInput<'_>) -> StepEstimate {
    let entry = s
        .model
        .and_then(|m| pricing::find(cat, m, chrono::Utc::now()));
    let c = ctx(cat, s.access, s.config, s.mode, s.flex);
    let price = |u: &Usage| entry.map_or(0.0, |e| pricing::cost(e, u, &c, None));
    StepEstimate {
        step: s.step.into(),
        mode: s.mode.as_str().into(),
        model: s.model.map(String::from),
        connector: s.access.map(|a| a.label.clone()),
        tokens: s.profile.mid.clone(),
        currency: entry.map_or("USD".into(), |e| e.currency.clone()),
        low: price(&s.profile.low),
        mid: price(&s.profile.mid),
        high: price(&s.profile.high),
        known: entry.is_some(),
        basis: s.profile.basis,
    }
}

pub fn totals(steps: &[StepEstimate]) -> (Vec<Money>, Vec<Money>, Vec<Money>) {
    let (mut lo, mut mid, mut hi) = (vec![], vec![], vec![]);
    for s in steps.iter().filter(|s| s.known) {
        add_money(&mut lo, &s.currency, s.low);
        add_money(&mut mid, &s.currency, s.mid);
        add_money(&mut hi, &s.currency, s.high);
    }
    (lo, mid, hi)
}

fn model_or<'a>(t: Option<&'a Target>, fallback: Option<&'a str>) -> Option<&'a str> {
    t.and_then(|t| t.model.as_deref())
        .filter(|m| !m.is_empty())
        .or(fallback)
}

/// Steps of a routing for a given agent-loop profile.
pub fn routing_steps(
    r: &Routing,
    all: &[Access],
    cat: &PriceCatalog,
    loop_profile: Profile,
    review_enabled: bool,
) -> Vec<StepEstimate> {
    let exec_t = r.execute.target.as_ref();
    let exec_a = exec_t.and_then(|t| routing::find(all, t));
    let exec_m = model_or(exec_t, None);
    let mut out = vec![];
    if r.plan.mode != StepMode::Off {
        let t = r.plan.target.as_ref();
        out.push(step(
            cat,
            StepInput {
                step: "plan",
                mode: r.plan.mode,
                access: t.and_then(|t| routing::find(all, t)).or(exec_a),
                config: None,
                model: model_or(t, exec_m),
                profile: Profile::single(PLAN, REPORT_BASIS),
                flex: false,
            },
        ));
    }
    out.push(step(
        cat,
        StepInput {
            step: "execute",
            mode: StepMode::Harness,
            access: exec_a,
            config: None,
            model: exec_m,
            profile: loop_profile,
            flex: r.execute.flex,
        },
    ));
    if review_enabled {
        let t = r.review.target.as_ref();
        out.push(step(
            cat,
            StepInput {
                step: "review",
                mode: r.review.mode,
                access: t.and_then(|t| routing::find(all, t)).or(exec_a),
                config: None,
                model: model_or(t, exec_m),
                profile: Profile::single(REVIEW, REPORT_BASIS),
                // An agentic review on the agent loop's own access runs with its tier.
                flex: r.execute.flex
                    && r.review.mode == StepMode::Harness
                    && t.is_none_or(|t| t.connector_id == exec_t.and_then(|e| e.connector_id)),
            },
        ));
    }
    out
}

/// Middle estimate per attempt (report M profile) for preset cards.
pub fn routing_estimate(r: &Routing, all: &[Access], cat: &PriceCatalog) -> Vec<Money> {
    totals(&routing_steps(r, all, cat, Profile::report(), true)).1
}

/// Assembles the response.
pub fn assemble(steps: Vec<StepEstimate>, location: &str, extra_notes: Vec<String>) -> Estimate {
    let (low, mid, high) = totals(&steps);
    let mut notes = extra_notes;
    if steps.iter().any(|s| !s.known) {
        notes.push(
            "Für mindestens einen Schritt ist kein Preis bekannt – dieser fehlt in der Summe."
                .into(),
        );
    }
    if steps.iter().any(|s| s.basis == REPORT_BASIS) {
        notes.push("Spanne = kleiner bis großer Auftrag. Günstige Modelle brauchen oft mehr Schritte als angenommen; Fix-Runden sind nicht enthalten.".into());
    }
    Estimate {
        steps,
        low,
        mid,
        high,
        location: location.into(),
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::{DataLocation, ExecuteRoute, StepRoute};
    use uuid::Uuid;

    fn access(kind: &str) -> Access {
        Access {
            connector_id: Some(Uuid::new_v4()),
            kind: kind.into(),
            label: kind.into(),
            harness: connectors::harness(kind, &serde_json::json!({})),
            location: DataLocation::Us,
            single_shot: true,
            batch: true,
            flex: kind == "openai_api",
        }
    }

    #[test]
    fn history_profile_needs_enough_runs() {
        let runs = vec![u(1, 0, 0, 1), u(10, 0, 0, 10)];
        assert!(Profile::from_history(runs).is_none());
        let runs = vec![
            u(100, 0, 0, 1),
            u(1, 0, 0, 1),
            u(10, 0, 0, 1),
            u(1000, 0, 0, 1),
            u(50, 0, 0, 1),
        ];
        let p = Profile::from_history(runs).unwrap();
        assert_eq!(p.mid.input_tokens, 50);
        assert_eq!(p.low.input_tokens, 1);
        assert_eq!(p.high.input_tokens, 1000);
        assert!(p.basis.contains("n=5"));
    }

    /// Report route R2 (Claude standard): plan Opus batch, loop Sonnet,
    /// review Sonnet batch → $0.14 + $1.245 + $0.05 ≈ $1.43 per attempt
    /// (the report lists $1.56 incl. its own rounding of the batch steps).
    #[test]
    fn report_route_r2_estimate() {
        let a = access("anthropic_api");
        let all = vec![a.clone()];
        let r = Routing {
            preset: None,
            plan: StepRoute {
                mode: StepMode::Batch,
                target: Some(a.target(Some("claude-opus-5-5"))),
            },
            execute: ExecuteRoute {
                target: Some(a.target(Some("claude-sonnet-5-5"))),
                ..Default::default()
            },
            review: StepRoute {
                mode: StepMode::Batch,
                target: Some(a.target(Some("claude-sonnet-5-5"))),
            },
            analyze: StepRoute::default(),
        };
        let steps = routing_steps(&r, &all, pricing::builtin(), Profile::report(), true);
        assert_eq!(
            steps.iter().map(|s| s.step.as_str()).collect::<Vec<_>>(),
            ["plan", "execute", "review"]
        );
        let plan = &steps[0];
        assert!((plan.mid - 0.14).abs() < 1e-9, "{}", plan.mid);
        assert!((steps[1].mid - 1.245).abs() < 1e-9);
        assert!(
            (steps[1].low - 0.46).abs() < 1e-9,
            "S profile {}",
            steps[1].low
        );
        assert!((steps[2].mid - 0.05).abs() < 1e-9, "{}", steps[2].mid);
        let total = routing_estimate(&r, &all, pricing::builtin());
        assert!((total[0].amount - 1.435).abs() < 1e-9);
        // Without review the step is missing.
        assert_eq!(
            routing_steps(&r, &all, pricing::builtin(), Profile::report(), false).len(),
            2
        );
    }

    #[test]
    fn flex_region_and_unknown_models() {
        let o = access("openai_api");
        let all = vec![o.clone()];
        let r = Routing {
            execute: ExecuteRoute {
                target: Some(o.target(Some("gpt-6.1-terra"))),
                flex: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let s = routing_steps(&r, &all, pricing::builtin(), Profile::report(), false);
        // Report: Terra M $0.62 (writes 1.25×), Flex halves; our profile prices
        // cache writes at the 30-min write price.
        let std = pricing::cost(
            pricing::find(pricing::builtin(), "gpt-6.1-terra", chrono::Utc::now()).unwrap(),
            &LOOP_M,
            &PriceCtx::default(),
            None,
        );
        assert!((s[0].mid - std / 2.0).abs() < 1e-9);
        let m = access("mistral");
        let st = step(
            pricing::builtin(),
            StepInput {
                step: "execute",
                mode: StepMode::Harness,
                access: Some(&m),
                config: None,
                model: Some("mistral-large-4"),
                profile: Profile::report(),
                flex: false,
            },
        );
        let base = pricing::cost(
            pricing::find(pricing::builtin(), "mistral-large-4", chrono::Utc::now()).unwrap(),
            &LOOP_M,
            &PriceCtx::default(),
            None,
        );
        assert!(
            (st.mid - base * 1.1).abs() < 1e-9,
            "EU endpoint is the default (+10 %)"
        );
        let unknown = step(
            pricing::builtin(),
            StepInput {
                step: "execute",
                mode: StepMode::Harness,
                access: None,
                config: None,
                model: Some("llama-local"),
                profile: Profile::report(),
                flex: false,
            },
        );
        assert!(!unknown.known && unknown.mid == 0.0);
        let e = assemble(vec![unknown], "Unbekannt", vec![]);
        assert!(e.mid.is_empty() && e.notes.iter().any(|n| n.contains("kein Preis")));
    }
}
