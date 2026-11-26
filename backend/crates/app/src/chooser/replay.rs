//! Offline replay of the chooser (WP15) against the static presets.
//!
//! A stream of tasks is played through each policy. A policy decides the
//! chain of attempts (agent loop models), a hidden truth decides each
//! attempt's outcome (seeded RNG), and the costs come from the same cost
//! model the chooser uses (price catalog, report token profiles). The
//! chooser learns online from the simulated outcomes (Thompson sampling,
//! exploration budget); the presets play their fixed chains.
//!
//! Two data bases:
//! * **synthetic** – a generated task mix and a known truth per model,
//!   category, size and risk. Clearly labelled; the truth deliberately
//!   differs from the chooser's priors (it has to learn).
//! * **history** – finished tasks of this instance (their features) and the
//!   recorded outcomes per category × model as truth (Laplace-smoothed
//!   toward the prior; models never observed use the prior). Counterfactual
//!   outcomes are thereby estimates, not observations.
use std::collections::HashMap;

use queued_api::{
    chooser::{ReplayPolicy, ReplayReport, TaskFeatures},
    DataLocation, ModelTier, Residency, StepMode,
};

use super::{
    bandit::{Counts, Rng},
    decide::{self, Cand, Stats, FIX_FACTOR},
    model,
};
use crate::{pricing, routing::Access};

/// Truth of the replay: success probability of one attempt.
pub trait Truth {
    fn p(&self, price_id: &str, tier: ModelTier, f: &TaskFeatures) -> f64;
}

/// Synthetic truth (labelled as such): differs from the priors on purpose
/// – Haiku is better than assumed on docs/tests and worse on refactors,
/// Terra (no benchmark) is decent but only learnable by exploration.
pub struct Synthetic;

impl Truth for Synthetic {
    fn p(&self, price_id: &str, tier: ModelTier, f: &TaskFeatures) -> f64 {
        let base = match price_id {
            "anthropic/claude-opus-5-5" => 0.88,
            "anthropic/claude-sonnet-5-5" => 0.80,
            "anthropic/claude-haiku-5-5" => 0.50,
            "openai/gpt-6-1-terra" => 0.70,
            "openai/gpt-6-1-sol" => 0.78,
            "openai/gpt-6-astra" => 0.85,
            "openai/gpt-6-luna" => 0.35,
            _ => match tier {
                ModelTier::Cheap => 0.35,
                ModelTier::Mid => 0.68,
                ModelTier::Top => 0.84,
            },
        };
        let cheap = tier == ModelTier::Cheap;
        let cat = match f.category.as_str() {
            "docs" => 1.0,
            "tests" => 0.6,
            "research" => 0.3,
            "refactor" => {
                if cheap {
                    -1.1
                } else {
                    -0.6
                }
            }
            "ui" => -0.2,
            "feature" => -0.1,
            _ => 0.0,
        };
        let size = match f.size.as_str() {
            "S" => 0.9,
            "L" => {
                if cheap {
                    -1.6
                } else {
                    -1.0
                }
            }
            _ => 0.0,
        };
        let cx = if f.complexity >= 3 { -0.5 } else { 0.0 };
        let risk = if f.high_risk() { -0.4 } else { 0.0 };
        model::sigmoid(model::logit(base) + cat + size + cx + risk)
    }
}

/// Truth from recorded outcomes: (category, price id) → (successes, n).
pub struct Empirical {
    pub cells: HashMap<(String, String), (f64, f64)>,
}

impl Truth for Empirical {
    fn p(&self, price_id: &str, tier: ModelTier, f: &TaskFeatures) -> f64 {
        let (a0, b0, _) = model::cell_prior(price_id, tier, &f.category);
        let (s, n) = self
            .cells
            .get(&(f.category.clone(), price_id.to_string()))
            .copied()
            .unwrap_or((0.0, 0.0));
        let p = (a0 + s + 1.0) / (a0 + b0 + n + 2.0);
        model::sigmoid(model::logit(p) + model::context_offset(&f.size, f.complexity, f.high_risk()))
    }
}

/// The synthetic task mix (labelled as such in the report).
pub fn synthetic_tasks(n: u32, seed: u64) -> Vec<TaskFeatures> {
    let mut rng = Rng::new(seed ^ 0x5EED_7A5C);
    let cats = [
        ("bugfix", 25),
        ("feature", 25),
        ("refactor", 10),
        ("docs", 10),
        ("tests", 10),
        ("ui", 10),
        ("research", 5),
        ("other", 5),
    ];
    (0..n)
        .map(|_| {
            let mut r = (rng.uniform() * 100.0) as i32;
            let mut category = "other";
            for (c, w) in cats {
                if r < w {
                    category = c;
                    break;
                }
                r -= w;
            }
            let u = rng.uniform();
            let size = if u < 0.45 {
                "S"
            } else if u < 0.85 {
                "M"
            } else {
                "L"
            };
            let risky = rng.uniform() < 0.15;
            let complexity = match size {
                "S" => 1,
                "M" => 2,
                _ => 3,
            };
            TaskFeatures {
                category: category.into(),
                size: size.into(),
                complexity: if risky { complexity.max(2) } else { complexity },
                risks: if risky { vec!["security".into()] } else { vec![] },
                est_tokens: 30_000,
                ..Default::default()
            }
        })
        .collect()
}

/// Accesses of the replay user: Anthropic API and OpenAI API (flex).
pub fn replay_accesses(with_openai: bool) -> Vec<Access> {
    let mk = |kind: &str, harness: &str, n: u128, flex: bool| Access {
        connector_id: Some(uuid::Uuid::from_u128(n)),
        kind: kind.into(),
        label: kind.into(),
        harness: harness.into(),
        location: DataLocation::Us,
        single_shot: true,
        batch: true,
        flex,
        wire_api: None,
    };
    let mut v = vec![mk("anthropic_api", "claude", 1, false)];
    if with_openai {
        v.push(mk("openai_api", "codex", 2, true));
    }
    v
}

/// A static preset: plan model (batch), agent-loop chain, review model.
struct Preset {
    id: &'static str,
    label: &'static str,
    plan: &'static str,
    chain: &'static [&'static str],
    review: &'static str,
}

/// The presets of `routing.rs` (Anthropic part): Günstig, Ausgewogen, Beste Qualität.
const PRESETS: [Preset; 3] = [
    Preset {
        id: "cheap",
        label: "Vorlage „Günstig“ (Haiku → Sonnet → Opus)",
        plan: "claude-opus-5-5",
        chain: &["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"],
        review: "claude-sonnet-5-5",
    },
    Preset {
        id: "balanced",
        label: "Vorlage „Ausgewogen“ (Sonnet → Opus)",
        plan: "claude-opus-5-5",
        chain: &["claude-sonnet-5-5", "claude-opus-5-5"],
        review: "claude-sonnet-5-5",
    },
    Preset {
        id: "best",
        label: "Vorlage „Beste Qualität“ (Opus)",
        plan: "claude-opus-5-5",
        chain: &["claude-opus-5-5"],
        review: "claude-opus-5-5",
    },
];

#[derive(Default)]
struct Tally {
    tasks: u32,
    accepted: u32,
    first: u32,
    cost: f64,
}

impl Tally {
    fn policy(&self, id: &str, label: &str) -> ReplayPolicy {
        let acc = f64::from(self.accepted.max(1));
        ReplayPolicy {
            policy: id.into(),
            label: label.into(),
            tasks: self.tasks,
            accepted: self.accepted,
            first_pass: self.first,
            total_cost: round(self.cost),
            cost_per_accepted: round(self.cost / acc),
            acceptance_rate: round(f64::from(self.accepted) / f64::from(self.tasks.max(1))),
            first_pass_rate: round(f64::from(self.first) / f64::from(self.tasks.max(1))),
        }
    }
}

fn round(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn input<'a>(
    f: &'a TaskFeatures,
    cands: Vec<Cand>,
    stats: &'a Stats,
    explore_left: f64,
    seed: u64,
    quality: u8,
) -> decide::Input<'a> {
    decide::Input {
        features: f,
        kind: "task",
        cands,
        residency: Residency::Any,
        project_blocked: vec![],
        user_blocked: vec![],
        cost_limit: None,
        quality,
        explore_left,
        max_fix: 2,
        history: HashMap::new(),
        stats,
        catalog: pricing::builtin(),
        seed,
    }
}

/// Plays one chain: `stages` = (candidate index, review cost) per attempt.
#[allow(clippy::too_many_arguments)]
fn play(
    i: &decide::Input<'_>,
    stages: &[(usize, f64)],
    plan: f64,
    truth: &dyn Truth,
    rng: &mut Rng,
    t: &mut Tally,
    learn: Option<&mut Stats>,
) {
    let max_len = decide::MAX_CHAIN.min(1 + i.max_fix as usize);
    let mut cost = plan;
    let mut accepted = false;
    let mut seq: Vec<(usize, f64, bool)> = stages.iter().map(|&(k, r)| (k, r, false)).collect();
    if let Some(&(k, r, _)) = seq.last() {
        while seq.len() < max_len {
            seq.push((k, r, true));
        }
    }
    let mut observed: Vec<(String, f64)> = vec![];
    for (n, (k, review, repeat)) in seq.iter().enumerate() {
        let c = &i.cands[*k];
        let lc = decide::loop_cost(i, c);
        cost += if *repeat { FIX_FACTOR * lc } else { lc } + review;
        let ok = rng.uniform() < truth.p(&c.price.id, c.price.tier, i.features);
        observed.push((c.price.id.clone(), if ok { 1.0 } else { 0.0 }));
        if ok {
            accepted = true;
            if n == 0 {
                t.first += 1;
            }
            break;
        }
    }
    t.tasks += 1;
    t.cost += cost;
    if accepted {
        t.accepted += 1;
    }
    if let Some(stats) = learn {
        for (key, s) in observed {
            let e = stats
                .cells
                .entry(("execute".into(), i.features.category.clone(), key))
                .or_default();
            let c: &mut Counts = &mut e.0;
            c.add(s, 1.0);
        }
    }
}

fn index_of(cands: &[Cand], model: &str) -> Option<usize> {
    cands.iter().position(|c| c.model == model)
}

fn chooser_policy(
    tasks: &[TaskFeatures],
    truth: &dyn Truth,
    seed: u64,
    with_openai: bool,
    quality: u8,
) -> Tally {
    let cands = super::candidates_from(&replay_accesses(with_openai), pricing::builtin(), |_| false);
    let mut stats = Stats::default();
    let mut t = Tally::default();
    let mut rng = Rng::new(seed ^ 0x0C40_05E5);
    // Exploration budget as in a project: 2 $ per 300 tasks (30 days).
    let budget_per_task = queued_api::chooser::DEFAULT_EXPLORE_BUDGET_USD / 300.0;
    let mut spent = 0.0;
    for (n, f) in tasks.iter().enumerate() {
        let left = (budget_per_task * (n as f64 + 1.0) - spent).max(0.0);
        let snapshot = stats.clone();
        let i = input(f, cands.clone(), &snapshot, left, seed.wrapping_add(n as u64), quality);
        let out = decide::decide(&i);
        let Some(chosen) = &out.report.chosen else {
            // Fallback: the balanced preset's chain.
            let k = index_of(&cands, "claude-sonnet-5-5").unwrap_or(0);
            let r = decide::review_cost(&i, &cands[k]);
            play(&i, &[(k, r)], 0.0, truth, &mut rng, &mut t, Some(&mut stats));
            continue;
        };
        if out.report.explored {
            spent += out.report.explore_cost;
        }
        let review_of = |tier: ModelTier, k: usize| -> f64 {
            match &out.report.review {
                Some(r) if r.tier >= tier => r.cost,
                // The runtime guard upgrades a weaker reviewer: the stage
                // reviews on its own model.
                _ => decide::review_cost(&i, &cands[k]),
            }
        };
        let mut stages = vec![];
        let first = cands
            .iter()
            .position(|c| c.target() == chosen.target)
            .unwrap_or(0);
        stages.push((first, review_of(cands[first].price.tier, first)));
        for s in &out.report.chain {
            if let Some(k) = cands.iter().position(|c| c.target() == s.target) {
                stages.push((k, review_of(cands[k].price.tier, k)));
            }
        }
        let plan = out.report.plan.as_ref().map_or(0.0, |p| p.cost);
        play(&i, &stages, plan, truth, &mut rng, &mut t, Some(&mut stats));
    }
    t
}

fn preset_policy(p: &Preset, tasks: &[TaskFeatures], truth: &dyn Truth, seed: u64) -> Tally {
    let cands = super::candidates_from(&replay_accesses(false), pricing::builtin(), |_| false);
    let stats = Stats::default();
    let mut t = Tally::default();
    let mut rng = Rng::new(seed ^ 0x0C40_05E5);
    for f in tasks {
        let i = input(f, cands.clone(), &stats, 0.0, 0, queued_api::chooser::DEFAULT_QUALITY);
        let rk = index_of(&cands, p.review).unwrap_or(0);
        let review = decide::review_cost(&i, &cands[rk]);
        let stages: Vec<(usize, f64)> = p
            .chain
            .iter()
            .filter_map(|m| index_of(&cands, m))
            .map(|k| {
                // Never weaker than the executor (runtime guard).
                let r = if cands[k].price.tier > cands[rk].price.tier {
                    decide::review_cost(&i, &cands[k])
                } else {
                    review
                };
                (k, r)
            })
            .collect();
        let pk = index_of(&cands, p.plan).unwrap_or(0);
        let plan = decide::step_cost(
            i.catalog,
            &cands[pk],
            &crate::estimate::PLAN,
            StepMode::Batch,
            false,
            None,
        );
        play(&i, &stages, plan, truth, &mut rng, &mut t, None);
    }
    t
}

/// Runs the replay: the three presets, the chooser on the same accesses
/// (fair comparison) and the chooser with an additional OpenAI access.
pub fn run(source: &str, basis: &str, tasks: &[TaskFeatures], truth: &dyn Truth, seed: u64) -> ReplayReport {
    let mut policies: Vec<ReplayPolicy> = PRESETS
        .iter()
        .map(|p| preset_policy(p, tasks, truth, seed).policy(p.id, p.label))
        .collect();
    let q = queued_api::chooser::DEFAULT_QUALITY;
    let fair = chooser_policy(tasks, truth, seed, false, q)
        .policy("chooser", "Automatische Modellwahl (gleiche Zugänge wie die Vorlagen)");
    let quality = chooser_policy(tasks, truth, seed, false, 90).policy(
        "chooser_quality",
        "Automatische Modellwahl, Regler auf Qualität (90)",
    );
    let wide = chooser_policy(tasks, truth, seed, true, q)
        .policy("chooser_openai", "Automatische Modellwahl (zusätzlich OpenAI-Zugang)");
    let best_cpa = policies
        .iter()
        .min_by(|a, b| a.cost_per_accepted.total_cmp(&b.cost_per_accepted))
        .cloned();
    let balanced = policies.iter().find(|p| p.policy == "balanced").cloned();
    let cheaper_than_all = policies
        .iter()
        .all(|p| fair.cost_per_accepted < p.cost_per_accepted);
    let quality_held = balanced
        .as_ref()
        .is_some_and(|b| fair.acceptance_rate + 1e-9 >= b.acceptance_rate);
    let verdict = match &best_cpa {
        Some(b) => format!(
            "Automatische Modellwahl: {:.2} $ pro erledigtem Auftrag, Annahmequote {:.0} % – günstigste Vorlage („{}“): {:.2} $ bei {:.0} %. {}",
            fair.cost_per_accepted,
            fair.acceptance_rate * 100.0,
            b.label,
            b.cost_per_accepted,
            b.acceptance_rate * 100.0,
            match (cheaper_than_all, quality_held) {
                (true, true) => "Günstiger als jede Vorlage bei mindestens gleicher Annahmequote wie „Ausgewogen“.",
                (true, false) => "Günstiger als jede Vorlage, aber niedrigere Annahmequote als „Ausgewogen“.",
                (false, true) => "Nicht günstiger als jede Vorlage, Annahmequote wie „Ausgewogen“ gehalten.",
                (false, false) => "Weder günstiger noch gleiche Qualität – Ergebnis prüfen.",
            }
        ),
        None => "Keine Vorlage auswertbar".into(),
    };
    policies.push(fair);
    policies.push(quality);
    policies.push(wide);
    ReplayReport {
        source: source.into(),
        basis: basis.into(),
        seed,
        policies,
        verdict,
        passed: cheaper_than_all || quality_held,
    }
}

/// Synthetic replay (labelled).
pub fn synthetic(n: u32, seed: u64) -> ReplayReport {
    let tasks = synthetic_tasks(n.clamp(10, 5_000), seed);
    run(
        "synthetic",
        &format!(
            "Synthetische Historie (erzeugt, keine echten Läufe): {} Aufträge, Mischung aus Fehlerbehebung/Funktionen/Umbau/Doku/Tests/Oberfläche, 45 % klein, 40 % mittel, 15 % groß, 15 % riskant; verborgene Wahrheit weicht absichtlich von den Annahmen ab. Kosten aus Preiskatalog und Token-Profilen des Kostenberichts.",
            tasks.len()
        ),
        &tasks,
        &Synthetic,
        seed,
    )
}

/// Replay against the recorded history of this instance (admin): the
/// features of finished tasks and the outcomes per category × model.
pub async fn history(st: &crate::state::AppState, n: u32, seed: u64) -> anyhow::Result<ReplayReport> {
    let rows: Vec<(String, String, String, String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT kind, title, prompt, context, routing_decision FROM tasks
         WHERE kind='task' AND status IN ('done','failed','blocked') AND finished_at IS NOT NULL
         ORDER BY finished_at DESC LIMIT $1",
    )
    .bind(i64::from(n.clamp(10, 5_000)))
    .fetch_all(&st.db)
    .await?;
    let mut tasks: Vec<TaskFeatures> = rows
        .into_iter()
        .map(|(kind, title, prompt, context, d)| {
            d.and_then(|v| serde_json::from_value::<queued_api::RoutingDecision>(v).ok())
                .and_then(|d| d.chooser.map(|r| r.features))
                .unwrap_or_else(|| {
                    super::features::analyze(&super::features::Input {
                        kind: &kind,
                        title: &title,
                        prompt: &prompt,
                        context: &context,
                        ..Default::default()
                    })
                })
        })
        .collect();
    tasks.reverse();
    let cells: Vec<(String, String, f64, f64)> = sqlx::query_as(
        "SELECT category, model_key, SUM(success * weight)::DOUBLE PRECISION, SUM(weight)::DOUBLE PRECISION
         FROM chooser_observations WHERE step='execute' AND source IN ('run','eval')
         GROUP BY category, model_key",
    )
    .fetch_all(&st.db)
    .await?;
    let observed: f64 = cells.iter().map(|c| c.3).sum();
    let truth = Empirical {
        cells: cells
            .into_iter()
            .map(|(c, k, s, n)| ((c, k), (s, n)))
            .collect(),
    };
    if tasks.is_empty() {
        anyhow::bail!("Noch keine beendeten Aufträge für einen Replay");
    }
    Ok(run(
        "history",
        &format!(
            "Historie dieser Instanz: {} beendete Aufträge (Merkmale), Wahrheit je Art × Modell aus {:.0} erfassten Ergebnissen (geglättet zur Annahme); Ergebnisse von Modellen, die für einen Auftrag nie liefen, sind Schätzungen.",
            tasks.len(),
            observed
        ),
        &tasks,
        &truth,
        seed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_replay_beats_or_matches_the_presets() {
        let r = synthetic(600, 2026);
        for p in &r.policies {
            assert_eq!(p.tasks, 600);
            assert!(p.accepted <= p.tasks && p.first_pass <= p.accepted);
            assert!(p.total_cost > 0.0);
        }
        let fair = r.policies.iter().find(|p| p.policy == "chooser").unwrap();
        let presets: Vec<_> = r
            .policies
            .iter()
            .filter(|p| ["cheap", "balanced", "best"].contains(&p.policy.as_str()))
            .collect();
        assert_eq!(presets.len(), 3);
        assert!(r.passed, "{}", r.verdict);
        // The headline claim of WP15: cheaper per accepted task than every preset.
        for p in &presets {
            assert!(
                fair.cost_per_accepted < p.cost_per_accepted,
                "{} vs {}: {}",
                fair.cost_per_accepted,
                p.cost_per_accepted,
                r.verdict
            );
        }
        assert!(r.basis.contains("Synthetische"));
        // The quality slider trades money for acceptance and first pass.
        let q = r.policies.iter().find(|p| p.policy == "chooser_quality").unwrap();
        assert!(q.acceptance_rate > fair.acceptance_rate && q.first_pass_rate > fair.first_pass_rate);
        assert!(q.cost_per_accepted > fair.cost_per_accepted);
    }

    #[test]
    fn replay_is_deterministic() {
        assert_eq!(synthetic(120, 7), synthetic(120, 7));
        assert_ne!(synthetic(120, 7).policies, synthetic(120, 8).policies);
    }

    #[test]
    fn empirical_truth_smooths_toward_the_prior() {
        let mut cells = HashMap::new();
        cells.insert(("docs".to_string(), "anthropic/claude-haiku-5-5".to_string()), (0.0, 50.0));
        let t = Empirical { cells };
        let f = TaskFeatures {
            category: "docs".into(),
            size: "M".into(),
            complexity: 2,
            ..Default::default()
        };
        let bad = t.p("anthropic/claude-haiku-5-5", ModelTier::Cheap, &f);
        let unknown = t.p("anthropic/claude-sonnet-5-5", ModelTier::Mid, &f);
        assert!(bad < 0.2, "{bad}");
        assert!(unknown > 0.6, "{unknown}");
    }
}
