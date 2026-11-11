//! Per-task limits (task override > project default > global config), the
//! retry backoff and the live token/cost guard. Pure functions, unit-tested.
use std::time::Duration;

use crate::{
    agent::LimitKind,
    budget::counted,
    config::Config,
    models::{Project, Task, Usage},
};

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveLimits {
    /// Per run (agent run and review run each).
    pub runtime_secs: u64,
    /// Per run; `None` = stall detection off.
    pub stall_secs: Option<u64>,
    /// Cumulative per task (agent + review runs).
    pub cost_usd: Option<f64>,
    /// Cumulative per task, counted tokens (input + output + cache writes).
    pub tokens: Option<i64>,
    /// Per run.
    pub turns: Option<u32>,
}

pub fn effective(task: &Task, project: &Project, cfg: &Config) -> EffectiveLimits {
    let pos_u64 = |v: Option<i32>| v.filter(|x| *x > 0).map(|x| x as u64);
    EffectiveLimits {
        runtime_secs: pos_u64(task.max_runtime_secs)
            .or(pos_u64(project.max_runtime_secs))
            .unwrap_or(cfg.task_timeout_min * 60)
            .max(1),
        stall_secs: match project.stall_timeout_secs {
            Some(s) => s,
            None => cfg.stall_timeout_secs as i32,
        }
        .try_into()
        .ok()
        .filter(|s: &u64| *s > 0),
        cost_usd: task
            .max_cost_usd
            .or(project.max_cost_usd)
            .filter(|c| *c > 0.0),
        tokens: task.max_tokens.or(project.max_tokens).filter(|t| *t > 0),
        turns: task
            .max_turns
            .or(project.max_turns)
            .filter(|t| *t > 0)
            .map(|t| t as u32),
    }
}

impl EffectiveLimits {
    /// Remaining (tokens, cost) budget after `used`; `None` = unlimited.
    pub fn remaining(&self, used: &Usage) -> (Option<i64>, Option<f64>) {
        (
            self.tokens.map(|t| t.saturating_sub(counted(used))),
            self.cost_usd.map(|c| c - used.cost_usd),
        )
    }

    /// Which cumulative limit is already used up by `used` (cost first).
    pub fn exhausted(&self, used: &Usage) -> Option<LimitKind> {
        let (tokens, cost) = self.remaining(used);
        if cost.is_some_and(|c| c <= 0.0) {
            Some(LimitKind::Cost)
        } else if tokens.is_some_and(|t| t <= 0) {
            Some(LimitKind::Tokens)
        } else {
            None
        }
    }
}

/// `min(base · 2^(n−1), max)`, `n` = retry count after increment (1-based).
pub fn backoff(n: i32, base_secs: u64, max_secs: u64) -> Duration {
    let exp = n.saturating_sub(1).clamp(0, 30) as u32;
    let secs = base_secs.saturating_mul(1u64 << exp).min(max_secs);
    Duration::from_secs(secs)
}

/// Live guard for one run: trips once the run's usage reaches the remaining
/// task budget.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    pub tokens_left: Option<i64>,
    pub cost_left: Option<f64>,
    pub seen: Usage,
    tripped: bool,
}

impl Guard {
    pub fn new(tokens_left: Option<i64>, cost_left: Option<f64>) -> Self {
        Self {
            tokens_left,
            cost_left,
            ..Default::default()
        }
    }

    /// Adds `d`; returns the limit that was reached (only the first time).
    pub fn on_delta(&mut self, d: &Usage) -> Option<LimitKind> {
        self.seen.add(d);
        if self.tripped {
            return None;
        }
        let hit = if self.cost_left.is_some_and(|c| self.seen.cost_usd >= c) {
            Some(LimitKind::Cost)
        } else if self.tokens_left.is_some_and(|t| counted(&self.seen) >= t) {
            Some(LimitKind::Tokens)
        } else {
            None
        };
        self.tripped = hit.is_some();
        hit
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    pub(crate) fn project() -> Project {
        Project {
            id: Uuid::nil(),
            owner_id: Uuid::nil(),
            name: "p".into(),
            description: String::new(),
            goal: String::new(),
            verify_command: None,
            review_enabled: false,
            review_provider: None,
            review_model: None,
            created_at: Utc::now(),
            git_remote_url: None,
            git_branch: "main".into(),
            git_token_enc: None,
            auto_push: false,
            max_runtime_secs: None,
            stall_timeout_secs: None,
            max_cost_usd: None,
            max_tokens: None,
            max_turns: None,
            routing: None,
            data_residency: "any".into(),
            is_eval: false,
            trust_repo_config: false,
            protected_paths: None,
            test_guard: true,
            setup_guard: true,
            secret_allowlist: vec![],
            parallelism: 1,
            output_mode: "commit".into(),
            git_host: None,
            git_api_url: None,
            git_repo_path: None,
            pr_draft: true,
            pr_labels: vec![],
            pr_auto_fix: true,
            pr_max_followups: 3,
            plan_mode: false,
            plan_auto_approve_mins: None,
        }
    }

    pub(crate) fn task() -> Task {
        Task {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            project_id: Uuid::nil(),
            parent_id: None,
            kind: "task".into(),
            title: "t".into(),
            prompt: "p".into(),
            context: String::new(),
            provider: "mock".into(),
            model: None,
            status: "running".into(),
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
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            updated_at: Utc::now(),
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
            branch: None,
            merge_base: None,
            merge_state: None,
            merge_queued_at: None,
            carry_from: None,
            pr_id: None,
            plan_mode: false,
            plan_state: None,
            plan_revision: 0,
            plan_feedback: None,
            plan_meta: None,
            agent_session: None,
            session_from: None,
            steer_rounds: 0,
            trigger_id: None,
        }
    }

    fn cfg() -> Config {
        let mut c = Config::for_tests("x", "/tmp".into(), "http://x", "t");
        c.task_timeout_min = 60;
        c.stall_timeout_secs = 1200;
        c
    }

    #[test]
    fn effective_precedence() {
        let c = cfg();
        let (mut t, mut p) = (task(), project());
        let e = effective(&t, &p, &c);
        assert_eq!(
            e,
            EffectiveLimits {
                runtime_secs: 3600,
                stall_secs: Some(1200),
                cost_usd: None,
                tokens: None,
                turns: None
            },
            "global defaults"
        );
        p.max_runtime_secs = Some(600);
        p.stall_timeout_secs = Some(0);
        p.max_cost_usd = Some(2.0);
        p.max_tokens = Some(5000);
        p.max_turns = Some(20);
        let e = effective(&t, &p, &c);
        assert_eq!(e.runtime_secs, 600);
        assert_eq!(
            e.stall_secs, None,
            "0 on the project switches stall detection off"
        );
        assert_eq!(
            (e.cost_usd, e.tokens, e.turns),
            (Some(2.0), Some(5000), Some(20))
        );
        t.max_runtime_secs = Some(30);
        t.max_cost_usd = Some(0.5);
        t.max_tokens = Some(100);
        t.max_turns = Some(3);
        let e = effective(&t, &p, &c);
        assert_eq!(
            (e.runtime_secs, e.cost_usd, e.tokens, e.turns),
            (30, Some(0.5), Some(100), Some(3)),
            "task overrides project"
        );
        p.stall_timeout_secs = Some(45);
        assert_eq!(effective(&t, &p, &c).stall_secs, Some(45));
        let mut c0 = cfg();
        c0.stall_timeout_secs = 0;
        assert_eq!(effective(&task(), &project(), &c0).stall_secs, None);
        // Task-level limit without project default.
        let mut t2 = task();
        t2.max_tokens = Some(7);
        assert_eq!(effective(&t2, &project(), &c).tokens, Some(7));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let b = |n| backoff(n, 30, 900).as_secs();
        assert_eq!(
            [b(1), b(2), b(3), b(4), b(5), b(6), b(7)],
            [30, 60, 120, 240, 480, 900, 900]
        );
        assert_eq!(b(0), 30);
        assert_eq!(backoff(1000, 30, 900).as_secs(), 900);
        assert_eq!(backoff(2, 1, 2).as_secs(), 2);
    }

    #[test]
    fn guard_trips_once_on_tokens_or_cost() {
        let u = |i, o, c| Usage {
            input_tokens: i,
            output_tokens: o,
            cost_usd: c,
            ..Default::default()
        };
        let mut g = Guard::new(Some(2000), None);
        assert_eq!(g.on_delta(&u(1200, 150, 0.0)), None);
        assert_eq!(g.on_delta(&u(1200, 150, 0.0)), Some(LimitKind::Tokens));
        assert_eq!(g.on_delta(&u(1, 1, 0.0)), None, "trips only once");
        assert_eq!(counted(&g.seen), 2702);
        let mut g = Guard::new(Some(1_000_000), Some(0.01));
        assert_eq!(g.on_delta(&u(1, 0, 0.006)), None);
        assert_eq!(g.on_delta(&u(1, 0, 0.006)), Some(LimitKind::Cost));
        let mut g = Guard::new(None, None);
        assert_eq!(g.on_delta(&u(1_000_000_000, 0, 1e6)), None);
        // Cache reads never count.
        let mut g = Guard::new(Some(10), None);
        assert_eq!(
            g.on_delta(&Usage {
                cache_read_tokens: 1000,
                ..Default::default()
            }),
            None
        );
    }

    #[test]
    fn remaining_and_exhausted() {
        let e = EffectiveLimits {
            runtime_secs: 1,
            stall_secs: None,
            cost_usd: Some(1.0),
            tokens: Some(1000),
            turns: None,
        };
        let used = Usage {
            input_tokens: 400,
            output_tokens: 100,
            cost_usd: 0.25,
            ..Default::default()
        };
        assert_eq!(e.remaining(&used), (Some(500), Some(0.75)));
        assert_eq!(e.exhausted(&used), None);
        let used = Usage {
            input_tokens: 1000,
            ..Default::default()
        };
        assert_eq!(e.exhausted(&used), Some(LimitKind::Tokens));
        let used = Usage {
            cost_usd: 1.0,
            ..Default::default()
        };
        assert_eq!(e.exhausted(&used), Some(LimitKind::Cost));
    }
}
