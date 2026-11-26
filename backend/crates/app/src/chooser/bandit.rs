//! Contextual bandit of the chooser (WP15): Beta posteriors per
//! (step × category × model), Thompson sampling with an own seedable RNG
//! (deterministic tests, reproducible decisions), decay of old data and a
//! minimum sample before an assumption-only model is trusted.
use super::model::{cell_prior, Evidence};
use queued_api::ModelTier;

/// Own observations before a model without benchmark evidence counts as
/// trusted (greedy choice); below that it is only explored.
pub const MIN_SAMPLE: f64 = 5.0;
/// Pooled data of other users is a prior, never an override: at most this
/// many pseudo observations per cell.
pub const POOL_CAP: f64 = 20.0;
/// A pooled cell needs this many distinct users (no inference on one user).
pub const POOL_MIN_USERS: i64 = 3;

/// SplitMix64: small, fast, good enough for sampling, fully deterministic.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in (0, 1).
    pub fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// Standard normal (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.uniform(), self.uniform());
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Gamma(shape, 1) by Marsaglia-Tsang (shape < 1 via boosting).
    pub fn gamma(&mut self, shape: f64) -> f64 {
        if shape < 1.0 {
            let g = self.gamma(shape + 1.0);
            return g * self.uniform().powf(1.0 / shape);
        }
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();
        loop {
            let x = self.normal();
            let v = (1.0 + c * x).powi(3);
            if v <= 0.0 {
                continue;
            }
            let u = self.uniform();
            if u.ln() < 0.5 * x * x + d - d * v + d * v.ln() {
                return d * v;
            }
        }
    }

    /// Beta(a, b).
    pub fn beta(&mut self, a: f64, b: f64) -> f64 {
        let x = self.gamma(a.max(1e-3));
        let y = self.gamma(b.max(1e-3));
        if x + y <= 0.0 {
            0.5
        } else {
            x / (x + y)
        }
    }
}

/// Seed of a task's decision: reproducible per task, injectable in tests.
pub fn seed_of(id: uuid::Uuid) -> u64 {
    let b = id.as_u128();
    (b as u64) ^ ((b >> 64) as u64)
}

/// Weight of an observation of age `days` (half-life decay).
pub fn decay(weight: f64, age_days: f64, half_life_days: f64) -> f64 {
    weight * 0.5f64.powf(age_days.max(0.0) / half_life_days.max(1.0))
}

/// Decayed sums of a cell.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counts {
    pub successes: f64,
    pub failures: f64,
}

impl Counts {
    pub fn n(&self) -> f64 {
        self.successes + self.failures
    }

    pub fn add(&mut self, success: f64, weight: f64) {
        self.successes += success * weight;
        self.failures += (1.0 - success) * weight;
    }
}

/// Posterior of one cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Posterior {
    pub alpha: f64,
    pub beta: f64,
    /// Own effective observations.
    pub own: f64,
    /// Pooled pseudo observations (other users, opted in).
    pub pooled: f64,
    pub evidence: Evidence,
}

impl Posterior {
    /// Prior (catalog/report) + pooled (capped) + own data.
    pub fn new(
        price_id: &str,
        tier: ModelTier,
        category: &str,
        own: Counts,
        pooled: Option<Counts>,
    ) -> Self {
        let (a0, b0, evidence) = cell_prior(price_id, tier, category);
        let (pa, pb) = match pooled {
            Some(p) if p.n() > 0.0 => {
                let scale = (POOL_CAP / p.n()).min(1.0);
                (p.successes * scale, p.failures * scale)
            }
            _ => (0.0, 0.0),
        };
        Self {
            alpha: a0 + pa + own.successes,
            beta: b0 + pb + own.failures,
            own: own.n(),
            pooled: pa + pb,
            evidence,
        }
    }

    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    pub fn sample(&self, rng: &mut Rng) -> f64 {
        rng.beta(self.alpha, self.beta)
    }

    /// Greedy choice allowed: benchmark evidence or enough own data.
    pub fn trusted(&self) -> bool {
        self.evidence == Evidence::Benchmark || self.own >= MIN_SAMPLE
    }

    /// What the probability rests on (UI).
    pub fn basis(&self) -> &'static str {
        if self.own >= MIN_SAMPLE {
            "learned"
        } else {
            self.evidence.as_str()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_uniform() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let xs: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_ne!(Rng::new(43).next_u64(), xs[0]);
        let mut r = Rng::new(7);
        let mean = (0..20_000).map(|_| r.uniform()).sum::<f64>() / 20_000.0;
        assert!((mean - 0.5).abs() < 0.01, "{mean}");
    }

    #[test]
    fn beta_samples_have_the_right_mean() {
        let mut r = Rng::new(1);
        for (a, b) in [(2.0, 8.0), (30.0, 10.0), (0.5, 0.5)] {
            let m = (0..20_000).map(|_| r.beta(a, b)).sum::<f64>() / 20_000.0;
            assert!((m - a / (a + b)).abs() < 0.015, "Beta({a},{b}) mean {m}");
        }
    }

    #[test]
    fn decay_halves_per_half_life() {
        assert!((decay(1.0, 0.0, 60.0) - 1.0).abs() < 1e-12);
        assert!((decay(1.0, 60.0, 60.0) - 0.5).abs() < 1e-12);
        assert!((decay(2.0, 120.0, 60.0) - 0.5).abs() < 1e-12);
        // Old failures stop dominating: a model that failed 10× a year ago
        // and succeeded 5× last week is believed again.
        let mut c = Counts::default();
        for _ in 0..10 {
            c.add(0.0, decay(1.0, 365.0, 60.0));
        }
        for _ in 0..5 {
            c.add(1.0, decay(1.0, 7.0, 60.0));
        }
        let p = Posterior::new("anthropic/claude-haiku-5-5", ModelTier::Cheap, "bugfix", c, None);
        let mut fresh = Counts::default();
        for _ in 0..10 {
            fresh.add(0.0, 1.0);
        }
        for _ in 0..5 {
            fresh.add(1.0, 1.0);
        }
        let q = Posterior::new("anthropic/claude-haiku-5-5", ModelTier::Cheap, "bugfix", fresh, None);
        assert!(p.mean() > 0.6 && q.mean() < 0.45, "{} {}", p.mean(), q.mean());
    }

    #[test]
    fn trust_needs_benchmark_or_minimum_sample() {
        let none = Counts::default();
        let terra = Posterior::new("openai/gpt-6-1-terra", ModelTier::Mid, "bugfix", none, None);
        assert!(!terra.trusted());
        let mut four = Counts::default();
        for _ in 0..4 {
            four.add(1.0, 1.0);
        }
        assert!(!Posterior::new("openai/gpt-6-1-terra", ModelTier::Mid, "bugfix", four, None).trusted());
        four.add(1.0, 1.0);
        let t = Posterior::new("openai/gpt-6-1-terra", ModelTier::Mid, "bugfix", four, None);
        assert!(t.trusted());
        assert_eq!(t.basis(), "learned");
        assert!(Posterior::new("anthropic/claude-sonnet-5-5", ModelTier::Mid, "x", none, None).trusted());
    }

    #[test]
    fn pooled_data_is_capped() {
        let mut huge = Counts::default();
        huge.add(0.0, 1000.0);
        let p = Posterior::new("anthropic/claude-opus-5-5", ModelTier::Top, "bugfix", Counts::default(), Some(huge));
        assert!((p.pooled - POOL_CAP).abs() < 1e-9);
        // Pooled failures pull the mean down, but own data still decides.
        assert!(p.mean() > 0.2);
        assert_eq!(p.own, 0.0);
    }

    #[test]
    fn thompson_converges_to_the_better_arm() {
        // Synthetic: arm A succeeds 80 %, arm B 40 %; both start from the
        // same weak prior. Thompson picks A in most of the last 200 rounds.
        let truth = [0.8, 0.4];
        let mut counts = [Counts::default(), Counts::default()];
        let mut rng = Rng::new(2026);
        let mut picks_a = 0;
        for round in 0..1000 {
            let s: Vec<f64> = (0..2)
                .map(|i| {
                    Posterior::new("x/unknown", ModelTier::Mid, "other", counts[i], None).sample(&mut rng)
                })
                .collect();
            let arm = usize::from(s[1] > s[0]);
            let ok = rng.uniform() < truth[arm];
            counts[arm].add(if ok { 1.0 } else { 0.0 }, 1.0);
            if round >= 800 && arm == 0 {
                picks_a += 1;
            }
        }
        assert!(picks_a >= 180, "picked A {picks_a}/200");
        let pa = counts[0].successes / counts[0].n();
        assert!((pa - 0.8).abs() < 0.05, "estimated {pa}");
    }
}
