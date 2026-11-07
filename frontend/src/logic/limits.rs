//! Limit input fields (minutes, USD, tokens, turns): parsing and the same
//! range checks as the backend, with German messages at the field
//! (closes WP01 review finding N6: no English 422 serde errors, no
//! "seconds" message for a minutes field).
use queued_api::{Task, TaskLimitFields, UpdateProject};

use super::format::{fmt_number, fmt_tokens};

/// Backend ranges (`backend/crates/app/src/api/limits.rs`).
pub const RUNTIME_SECS: (i64, i64) = (1, 86_400);
pub const STALL_SECS: (i64, i64) = (0, 86_400);
pub const TOKENS: (i64, i64) = (1, 10_000_000_000);
pub const TURNS: (i64, i64) = (1, 1000);
pub const COST_MAX: f64 = 10_000.0;

/// Seconds → minutes for an input field (`""` = not set, decimal comma).
pub fn secs_to_min(s: Option<i64>) -> String {
    match s {
        None => String::new(),
        Some(s) => fmt_number((s as f64 / 60.0 * 100.0).round() / 100.0),
    }
}

/// Number from an input field (`None` = empty, comma allowed).
pub fn num_or_null(v: &str) -> Result<Option<f64>, String> {
    let t = v.trim().replacen(',', ".", 1);
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<f64>() {
        Ok(n) if n.is_finite() => Ok(Some(n)),
        _ => Err(format!("„{}“ ist keine Zahl", v.trim())),
    }
}

/// Minutes from an input field → seconds (`None` = not set).
pub fn min_to_secs(v: &str) -> Result<Option<i64>, String> {
    match num_or_null(v)? {
        None => Ok(None),
        Some(n) if n < 0.0 => Err("Bitte keine negativen Minuten".into()),
        Some(n) => Ok(Some((n * 60.0).round() as i64)),
    }
}

fn minutes_in_range(v: &str, (lo, hi): (i64, i64)) -> Result<Option<i64>, String> {
    let s = min_to_secs(v)?;
    match s {
        Some(x) if x < lo => Err(if lo == 1 {
            "Mindestens 1 Sekunde (0,02 Min.)".to_string()
        } else {
            format!("Mindestens {} Min.", secs_to_min(Some(lo)))
        }),
        Some(x) if x > hi => Err(format!("Höchstens {} Min.", secs_to_min(Some(hi)))),
        o => Ok(o),
    }
}

fn whole_in_range(v: &str, (lo, hi): (i64, i64)) -> Result<Option<i64>, String> {
    match num_or_null(v)? {
        None => Ok(None),
        Some(n) if n.fract() != 0.0 => Err("Bitte eine ganze Zahl eingeben".into()),
        Some(n) if n < lo as f64 || n > hi as f64 => {
            Err(format!("Erlaubt: {lo} bis {}", fmt_tokens_plain(hi)))
        }
        Some(n) => Ok(Some(n as i64)),
    }
}

fn fmt_tokens_plain(n: i64) -> String {
    if n >= 1_000_000 {
        fmt_tokens(n)
    } else {
        n.to_string()
    }
}

fn cost(v: &str) -> Result<Option<f64>, String> {
    match num_or_null(v)? {
        Some(n) if n <= 0.0 || n > COST_MAX => Err(format!(
            "Muss größer als 0 und höchstens {} $ sein",
            fmt_number(COST_MAX)
        )),
        o => Ok(o),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitField {
    Runtime,
    Stall,
    Cost,
    Tokens,
    Turns,
}

/// Raw input of the limit fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LimitForm {
    pub runtime: String,
    pub stall: String,
    pub cost: String,
    pub tokens: String,
    pub turns: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LimitValues {
    pub runtime_secs: Option<i64>,
    pub stall_secs: Option<i64>,
    pub cost_usd: Option<f64>,
    pub tokens: Option<i64>,
    pub turns: Option<i64>,
}

pub type FieldErrors = Vec<(LimitField, String)>;

impl LimitForm {
    pub fn from_project(p: &queued_api::ProjectView) -> Self {
        Self {
            runtime: secs_to_min(p.max_runtime_secs.map(i64::from)),
            stall: secs_to_min(p.stall_timeout_secs.map(i64::from)),
            cost: p.max_cost_usd.map(fmt_number).unwrap_or_default(),
            tokens: p.max_tokens.map(|t| t.to_string()).unwrap_or_default(),
            turns: p.max_turns.map(|t| t.to_string()).unwrap_or_default(),
        }
    }

    pub fn from_task(t: &Task) -> Self {
        Self {
            runtime: secs_to_min(t.max_runtime_secs.map(i64::from)),
            stall: String::new(),
            cost: t.max_cost_usd.map(fmt_number).unwrap_or_default(),
            tokens: t.max_tokens.map(|t| t.to_string()).unwrap_or_default(),
            turns: t.max_turns.map(|t| t.to_string()).unwrap_or_default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        [
            &self.runtime,
            &self.stall,
            &self.cost,
            &self.tokens,
            &self.turns,
        ]
        .iter()
        .all(|v| v.trim().is_empty())
    }

    /// All fields checked at once so every error is shown at its field.
    pub fn parse(&self) -> Result<LimitValues, FieldErrors> {
        let mut errs = vec![];
        let mut take = |field, r: Result<Option<i64>, String>| match r {
            Ok(v) => v,
            Err(e) => {
                errs.push((field, e));
                None
            }
        };
        let runtime_secs = take(
            LimitField::Runtime,
            minutes_in_range(&self.runtime, RUNTIME_SECS),
        );
        let stall_secs = take(LimitField::Stall, minutes_in_range(&self.stall, STALL_SECS));
        let tokens = take(LimitField::Tokens, whole_in_range(&self.tokens, TOKENS));
        let turns = take(LimitField::Turns, whole_in_range(&self.turns, TURNS));
        let cost_usd = match cost(&self.cost) {
            Ok(v) => v,
            Err(e) => {
                errs.push((LimitField::Cost, e));
                None
            }
        };
        if errs.is_empty() {
            Ok(LimitValues {
                runtime_secs,
                stall_secs,
                cost_usd,
                tokens,
                turns,
            })
        } else {
            Err(errs)
        }
    }
}

impl LimitValues {
    /// Task limits for `POST /api/tasks`: only set fields are sent.
    pub fn create_fields(&self) -> TaskLimitFields {
        TaskLimitFields {
            max_runtime_secs: self.runtime_secs.map(Some),
            max_cost_usd: self.cost_usd.map(Some),
            max_tokens: self.tokens.map(Some),
            max_turns: self.turns.map(Some),
        }
    }

    /// Task limits for `PATCH /api/tasks/{id}`: empty fields reset to the default.
    pub fn patch_fields(&self) -> TaskLimitFields {
        TaskLimitFields {
            max_runtime_secs: Some(self.runtime_secs),
            max_cost_usd: Some(self.cost_usd),
            max_tokens: Some(self.tokens),
            max_turns: Some(self.turns),
        }
    }

    /// Project limits for `PATCH /api/projects/{id}`.
    pub fn apply_to_project(&self, u: &mut UpdateProject) {
        u.max_runtime_secs = Some(self.runtime_secs);
        u.stall_timeout_secs = Some(self.stall_secs);
        u.max_cost_usd = Some(self.cost_usd);
        u.max_tokens = Some(self.tokens);
        u.max_turns = Some(self.turns);
    }
}

/// Limits set directly on a task, human-readable.
pub fn task_limits(t: &Task) -> Vec<String> {
    let mut out = vec![];
    if let Some(s) = t.max_runtime_secs.filter(|s| *s > 0) {
        out.push(format!("Laufzeit {} Min.", secs_to_min(Some(s.into()))));
    }
    if let Some(c) = t.max_cost_usd.filter(|c| *c > 0.0) {
        out.push(format!("Kosten {} $", fmt_number(c)));
    }
    if let Some(n) = t.max_tokens.filter(|n| *n > 0) {
        out.push(format!("Tokens {}", fmt_tokens(n)));
    }
    if let Some(n) = t.max_turns.filter(|n| *n > 0) {
        out.push(format!("Turns {n}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_minutes_and_numbers_from_inputs() {
        assert_eq!(secs_to_min(None), "");
        assert_eq!(secs_to_min(Some(90)), "1,5");
        assert_eq!(secs_to_min(Some(0)), "0");
        assert_eq!(secs_to_min(Some(600)), "10");
        assert_eq!(min_to_secs(""), Ok(None));
        assert_eq!(min_to_secs("1,5"), Ok(Some(90)));
        assert_eq!(min_to_secs("1.5"), Ok(Some(90)));
        assert_eq!(min_to_secs("0"), Ok(Some(0)));
        assert!(min_to_secs("-1").is_err());
        assert!(min_to_secs("abc").is_err());
        assert_eq!(num_or_null(" 2000 "), Ok(Some(2000.0)));
        assert_eq!(num_or_null("0,25"), Ok(Some(0.25)));
        assert_eq!(num_or_null(""), Ok(None));
        assert!(num_or_null("1e").is_err());
        assert!(num_or_null("inf").is_err());
    }

    #[test]
    fn form_reports_every_invalid_field_in_german() {
        let f = LimitForm {
            runtime: "0,001".into(),
            stall: "-1".into(),
            cost: "0".into(),
            tokens: "1,5".into(),
            turns: "2000".into(),
        };
        let errs = f.parse().unwrap_err();
        let fields: Vec<LimitField> = errs.iter().map(|e| e.0).collect();
        assert_eq!(
            fields,
            vec![
                LimitField::Runtime,
                LimitField::Stall,
                LimitField::Tokens,
                LimitField::Turns,
                LimitField::Cost
            ]
        );
        assert_eq!(errs[0].1, "Mindestens 1 Sekunde (0,02 Min.)");
        assert_eq!(errs[2].1, "Bitte eine ganze Zahl eingeben");
        assert_eq!(errs[3].1, "Erlaubt: 1 bis 1000");
        assert!(errs[4].1.starts_with("Muss größer als 0"));
        let too_long = LimitForm {
            runtime: "1441".into(),
            ..Default::default()
        };
        assert_eq!(too_long.parse().unwrap_err()[0].1, "Höchstens 1440 Min.");
    }

    #[test]
    fn valid_form_maps_to_api_fields() {
        let f = LimitForm {
            runtime: "10".into(),
            stall: "0".into(),
            cost: "0,5".into(),
            tokens: "2000".into(),
            turns: String::new(),
        };
        assert!(!f.is_empty());
        let v = f.parse().unwrap();
        assert_eq!(v.runtime_secs, Some(600));
        assert_eq!(v.stall_secs, Some(0));
        assert_eq!(v.cost_usd, Some(0.5));
        let c = v.create_fields();
        assert_eq!(c.max_tokens, Some(Some(2000)));
        assert_eq!(c.max_turns, None, "empty fields are not sent on create");
        let p = v.patch_fields();
        assert_eq!(p.max_turns, Some(None), "empty fields reset on edit");
        let mut u = UpdateProject::default();
        v.apply_to_project(&mut u);
        assert_eq!(u.stall_timeout_secs, Some(Some(0)));
        assert!(LimitForm::default().is_empty());
        assert_eq!(LimitForm::default().parse(), Ok(LimitValues::default()));
    }
}
