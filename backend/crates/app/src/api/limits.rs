//! Validation of per-task limit fields shared by the project and task APIs.
//! Ranges are checked here (400 with a German message) before SQL, so the
//! DB CHECK constraints are only the last line of defence.
use crate::error::{ApiResult, AppError};

pub use queued_api::{double_option, TaskLimitFields};

pub const RUNTIME: (i64, i64) = (1, 86_400);
pub const STALL: (i64, i64) = (0, 86_400);
pub const TOKENS: (i64, i64) = (1, 10_000_000_000);
pub const TURNS: (i64, i64) = (1, 1000);
pub const COST_MAX: f64 = 10_000.0;

pub fn int(label: &str, v: Option<i64>, (lo, hi): (i64, i64)) -> ApiResult<Option<i64>> {
    match v {
        Some(x) if x < lo || x > hi => Err(AppError::bad_request(format!(
            "{label} muss zwischen {lo} und {hi} liegen"
        ))),
        o => Ok(o),
    }
}

pub fn cost(v: Option<f64>) -> ApiResult<Option<f64>> {
    match v {
        Some(x) if !x.is_finite() || x <= 0.0 || x > COST_MAX => Err(AppError::bad_request(
            format!("Max. Kosten müssen größer als 0 und höchstens {COST_MAX} USD sein"),
        )),
        o => Ok(o),
    }
}

/// Validated task limits: outer `Option` = field present.
#[derive(Debug, Default, PartialEq)]
pub struct CheckedTaskLimits {
    pub max_runtime_secs: Option<Option<i32>>,
    pub max_cost_usd: Option<Option<f64>>,
    pub max_tokens: Option<Option<i64>>,
    pub max_turns: Option<Option<i32>>,
}

fn narrow(v: Option<Option<i64>>) -> Option<Option<i32>> {
    // Ranges are checked before, so the value always fits.
    v.map(|o| o.map(|x| x as i32))
}

/// Range checks of the task-level limit fields.
pub trait CheckLimits {
    fn check(self) -> ApiResult<CheckedTaskLimits>;
}

impl CheckLimits for TaskLimitFields {
    fn check(self) -> ApiResult<CheckedTaskLimits> {
        let flat = |v: Option<Option<i64>>| v.flatten();
        int(
            "Max. Laufzeit (Sekunden)",
            flat(self.max_runtime_secs),
            RUNTIME,
        )?;
        int("Max. Tokens", flat(self.max_tokens), TOKENS)?;
        int("Max. Turns", flat(self.max_turns), TURNS)?;
        cost(self.max_cost_usd.flatten())?;
        Ok(CheckedTaskLimits {
            max_runtime_secs: narrow(self.max_runtime_secs),
            max_cost_usd: self.max_cost_usd,
            max_tokens: self.max_tokens,
            max_turns: narrow(self.max_turns),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(j: &str) -> ApiResult<CheckedTaskLimits> {
        serde_json::from_str::<TaskLimitFields>(j).unwrap().check()
    }

    #[test]
    fn absent_null_and_value_are_distinguished() {
        let c = parse(r#"{"max_tokens": null, "max_turns": 5}"#).unwrap();
        assert_eq!(c.max_runtime_secs, None, "absent");
        assert_eq!(c.max_tokens, Some(None), "null resets");
        assert_eq!(c.max_turns, Some(Some(5)));
    }

    #[test]
    fn ranges_are_enforced() {
        for bad in [
            r#"{"max_runtime_secs": 0}"#,
            r#"{"max_runtime_secs": 86401}"#,
            r#"{"max_tokens": 0}"#,
            r#"{"max_turns": 1001}"#,
            r#"{"max_cost_usd": 0}"#,
            r#"{"max_cost_usd": -1}"#,
            r#"{"max_cost_usd": 10000.5}"#,
        ] {
            let e = parse(bad).unwrap_err();
            assert_eq!(e.0, 400, "{bad}");
        }
        assert!(parse(r#"{"max_runtime_secs": 86400, "max_cost_usd": 0.01, "max_tokens": 10000000000, "max_turns": 1}"#).is_ok());
        assert!(int("x", Some(0), STALL).is_ok());
    }
}
