//! Versioned price catalog and money cost of runs (WP13).
//!
//! * Built-in catalog: `catalog.json` (research of 2026-11-07, every line with
//!   source and evidence level; see the report in `reports/`).
//! * Admin overrides: `price_overrides`, every change is a new version; the
//!   effective catalog = built-in lines replaced/extended by the latest override.
//! * Cost = token counters × prices (never the CLI's dollar value, which is
//!   only a fallback for models without a price, marked as `cli`).
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, NaiveDate, Utc};
use queued_api::{
    CostSource, Evidence, LongContext, PriceCatalog, PriceEntry, PriceOverride, RegionRule,
    StepCost, Usage,
};

use crate::state::AppState;

const BUILTIN_JSON: &str = include_str!("catalog.json");

pub fn builtin() -> &'static PriceCatalog {
    static C: OnceLock<PriceCatalog> = OnceLock::new();
    C.get_or_init(|| {
        let mut c: PriceCatalog =
            serde_json::from_str(BUILTIN_JSON).expect("built-in price catalog is valid JSON");
        c.override_version = None;
        c
    })
}

/// Pricing tier of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Standard,
    Batch,
    Flex,
}

/// Normalises provider/model ids for matching: lower case, without
/// `provider/` prefixes (OpenRouter, opencode, LiteLLM), Bedrock geo/vendor
/// prefixes (`eu.anthropic.`), Vertex `@date` and Bedrock `-v1:0` suffixes.
pub fn normalize_model(m: &str) -> String {
    let mut s = m.trim().to_lowercase();
    if let Some(i) = s.rfind('/') {
        s = s[i + 1..].to_string();
    }
    for p in ["eu.", "us.", "global.", "apac.", "jp.", "au."] {
        if let Some(r) = s.strip_prefix(p) {
            s = r.to_string();
        }
    }
    if let Some(r) = s.strip_prefix("anthropic.") {
        s = r.to_string();
    }
    if let Some(i) = s.find('@') {
        s.truncate(i);
    }
    for suf in ["-v1:0", "-v2:0", "-v1"] {
        if let Some(r) = s.strip_suffix(suf) {
            s = r.to_string();
        }
    }
    // Dated snapshots: claude-haiku-4-5-20251001 → claude-haiku-4-5
    if s.len() > 9 {
        let (head, tail) = s.split_at(s.len() - 9);
        if tail.starts_with('-') && tail[1..].chars().all(|c| c.is_ascii_digit()) {
            s = head.to_string();
        }
    }
    s
}

fn in_range(e: &PriceEntry, day: NaiveDate) -> bool {
    e.effective_from.is_none_or(|f| day >= f) && e.effective_until.is_none_or(|u| day < u)
}

/// Price line for `model` valid at `at`.
pub fn find<'a>(c: &'a PriceCatalog, model: &str, at: DateTime<Utc>) -> Option<&'a PriceEntry> {
    let m = normalize_model(model);
    if m.is_empty() {
        return None;
    }
    let day = at.date_naive();
    c.entries
        .iter()
        .find(|e| in_range(e, day) && e.models.iter().any(|a| normalize_model(a) == m))
}

pub fn region<'a>(c: &'a PriceCatalog, id: &str) -> Option<&'a RegionRule> {
    c.regions.iter().find(|r| r.id == id)
}

/// Prices of one request after the long-context rule.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rates {
    input: f64,
    output: f64,
    cw_short: f64,
    cw_long: f64,
    cache_read: f64,
}

fn rates(e: &PriceEntry, prompt_tokens: Option<i64>) -> (Rates, bool) {
    // Missing cache prices: the input price (conservative upper bound).
    let base = Rates {
        input: e.input,
        output: e.output,
        cw_short: e.cache_write_short.unwrap_or(e.input),
        cw_long: e
            .cache_write_long
            .or(e.cache_write_short)
            .unwrap_or(e.input),
        cache_read: e.cache_read.unwrap_or(e.input),
    };
    let Some(lc) = e.long_context.as_ref() else {
        return (base, false);
    };
    match prompt_tokens {
        Some(p) if p > lc.threshold_tokens => (long_rates(&base, lc), true),
        _ => (base, false),
    }
}

fn long_rates(b: &Rates, lc: &LongContext) -> Rates {
    let im = lc.input_mult.unwrap_or(1.0);
    let om = lc.output_mult.unwrap_or(1.0);
    let cm = lc.cache_mult.unwrap_or(im);
    Rates {
        input: lc.input.unwrap_or(b.input * im),
        output: lc.output.unwrap_or(b.output * om),
        cw_short: lc.cache_write_short.unwrap_or(b.cw_short * cm),
        cw_long: lc.cache_write_long.unwrap_or(b.cw_long * cm),
        cache_read: lc.cache_read.unwrap_or(b.cache_read * cm),
    }
}

/// How a run is priced.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceCtx {
    pub tier: Tier,
    /// Product of the regional surcharges.
    pub region_mult: f64,
    /// Cache writes with the long TTL (1 h at Anthropic; used for batches).
    pub long_ttl: bool,
}

impl Default for PriceCtx {
    fn default() -> Self {
        Self {
            tier: Tier::Standard,
            region_mult: 1.0,
            long_ttl: false,
        }
    }
}

/// Cost of `u` in the entry's currency. `prompt_tokens` = prompt size of the
/// single request `u` belongs to (enables the long-context rule).
pub fn cost(e: &PriceEntry, u: &Usage, ctx: &PriceCtx, prompt_tokens: Option<i64>) -> f64 {
    let (r, _) = rates(e, prompt_tokens);
    let cw = if ctx.long_ttl { r.cw_long } else { r.cw_short };
    let raw = (u.input_tokens as f64 * r.input
        + u.output_tokens as f64 * r.output
        + u.cache_write_tokens as f64 * cw
        + u.cache_read_tokens as f64 * r.cache_read)
        / 1_000_000.0;
    let tier = match ctx.tier {
        Tier::Standard => 1.0,
        Tier::Batch => e.batch_factor.unwrap_or(1.0),
        Tier::Flex => e.flex_factor.unwrap_or(1.0),
    };
    let v = raw * tier * ctx.region_mult;
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

/// Sums usage of one request for the long-context rule.
pub fn prompt_tokens(u: &Usage) -> i64 {
    u.input_tokens
        .saturating_add(u.cache_read_tokens)
        .saturating_add(u.cache_write_tokens)
}

fn sub(a: &Usage, b: &Usage) -> Usage {
    Usage {
        input_tokens: (a.input_tokens - b.input_tokens).max(0),
        output_tokens: (a.output_tokens - b.output_tokens).max(0),
        cache_read_tokens: (a.cache_read_tokens - b.cache_read_tokens).max(0),
        cache_write_tokens: (a.cache_write_tokens - b.cache_write_tokens).max(0),
        cost_usd: 0.0,
    }
}

/// Accumulates the money cost of one run while its usage streams in.
#[derive(Debug, Clone)]
pub struct Meter {
    pub entry: Option<PriceEntry>,
    pub ctx: PriceCtx,
    pub step: &'static str,
    pub model: Option<String>,
    pub connector: Option<String>,
    deltas: Usage,
    delta_cost: f64,
    long_seen: bool,
}

impl Meter {
    pub fn new(
        catalog: &PriceCatalog,
        model: Option<&str>,
        ctx: PriceCtx,
        step: &'static str,
        connector: Option<String>,
    ) -> Self {
        let entry = model.and_then(|m| find(catalog, m, Utc::now())).cloned();
        Self {
            entry,
            ctx,
            step,
            model: model.map(String::from),
            connector,
            deltas: Usage::default(),
            delta_cost: 0.0,
            long_seen: false,
        }
    }

    pub fn known(&self) -> bool {
        self.entry.is_some()
    }

    /// USD amounts can feed the cost limit (other currencies cannot).
    pub fn usd(&self) -> bool {
        self.entry.as_ref().is_some_and(|e| e.currency == "USD")
    }

    /// Cost of one request's usage delta (0 without a price).
    pub fn on_delta(&mut self, u: &Usage) -> f64 {
        self.deltas.add(&Usage {
            cost_usd: 0.0,
            ..u.clone()
        });
        let Some(e) = &self.entry else { return 0.0 };
        let p = prompt_tokens(u);
        if e.long_context
            .as_ref()
            .is_some_and(|lc| p > lc.threshold_tokens)
        {
            self.long_seen = true;
        }
        let c = cost(e, u, &self.ctx, Some(p));
        self.delta_cost += c;
        c
    }

    /// Final cost of the run: priced deltas + the remainder of the
    /// authoritative total (subagents, CLIs that only report totals).
    pub fn finish(&self, total: &Usage) -> StepCost {
        let mut notes = vec![];
        let (amount, currency, source, price_id) = match &self.entry {
            Some(e) => {
                let rest = sub(total, &self.deltas);
                let rest_cost = cost(e, &rest, &self.ctx, None);
                if e.long_context.is_some()
                    && (rest.input_tokens + rest.cache_read_tokens + rest.cache_write_tokens) > 0
                {
                    notes.push(
                        "Long-Context-Aufschlag für Teile ohne Request-Usage nicht prüfbar (Untergrenze)"
                            .to_string(),
                    );
                }
                if self.long_seen {
                    notes.push("Long-Context-Preis für einzelne Requests angewendet".to_string());
                }
                if e.evidence != Evidence::Primary && e.evidence != Evidence::Admin {
                    notes.push(format!("Preis: {}", e.evidence.label()));
                }
                (
                    self.delta_cost + rest_cost,
                    e.currency.clone(),
                    CostSource::Catalog,
                    Some(e.id.clone()),
                )
            }
            None if total.cost_usd > 0.0 => {
                notes.push("Kein Katalogpreis – Dollarwert der CLI übernommen".to_string());
                (total.cost_usd, "USD".to_string(), CostSource::Cli, None)
            }
            None => {
                notes.push("Kein Preis bekannt – in der Preistabelle ergänzen".to_string());
                (0.0, "USD".to_string(), CostSource::Unknown, None)
            }
        };
        StepCost {
            step: self.step.to_string(),
            model: self.model.clone(),
            connector: self.connector.clone(),
            usage: Usage {
                cost_usd: if currency == "USD" { amount } else { 0.0 },
                ..total.clone()
            },
            currency,
            amount,
            source,
            price_id,
            runs: 1,
            notes,
        }
    }
}

/// Built-in catalog with the override applied (entries/regions by id).
pub fn merge(base: &PriceCatalog, o: &PriceOverride, version: Option<i32>) -> PriceCatalog {
    let mut c = base.clone();
    for e in &o.entries {
        let mut e = e.clone();
        e.evidence = Evidence::Admin;
        match c.entries.iter_mut().find(|x| x.id == e.id) {
            Some(x) => *x = e,
            None => c.entries.insert(0, e),
        }
    }
    for r in &o.regions {
        let mut r = r.clone();
        r.evidence = Evidence::Admin;
        match c.regions.iter_mut().find(|x| x.id == r.id) {
            Some(x) => *x = r,
            None => c.regions.push(r),
        }
    }
    if !o.entries.is_empty() || !o.regions.is_empty() {
        c.override_version = version;
    }
    c
}

fn price_ok(v: f64) -> bool {
    v.is_finite() && (0.0..=10_000.0).contains(&v)
}

/// Validation of admin overrides (German messages for the API).
pub fn validate(o: &PriceOverride) -> Result<(), String> {
    if o.entries.len() > 500 || o.regions.len() > 100 {
        return Err("Zu viele Einträge".into());
    }
    for e in &o.entries {
        let id = &e.id;
        if id.trim().is_empty() || id.len() > 100 {
            return Err("Jeder Preis braucht eine ID (max. 100 Zeichen)".into());
        }
        if e.models.is_empty() || e.models.iter().any(|m| normalize_model(m).is_empty()) {
            return Err(format!("{id}: mindestens eine Modell-ID angeben"));
        }
        if !matches!(e.currency.as_str(), "USD" | "CHF") {
            return Err(format!("{id}: Währung muss USD oder CHF sein"));
        }
        let all = [
            Some(e.input),
            Some(e.output),
            e.cache_write_short,
            e.cache_write_long,
            e.cache_read,
        ];
        if all.iter().flatten().any(|v| !price_ok(*v)) {
            return Err(format!("{id}: Preise müssen zwischen 0 und 10000 liegen"));
        }
        for f in [e.batch_factor, e.flex_factor].iter().flatten() {
            if !(f.is_finite() && *f > 0.0 && *f <= 1.0) {
                return Err(format!("{id}: Batch-/Flex-Faktor muss in (0, 1] liegen"));
            }
        }
        if let (Some(a), Some(b)) = (e.effective_from, e.effective_until) {
            if a >= b {
                return Err(format!("{id}: „gültig ab“ muss vor „gültig bis“ liegen"));
            }
        }
        if let Some(lc) = &e.long_context {
            if lc.threshold_tokens <= 0 {
                return Err(format!("{id}: Long-Context-Schwelle muss > 0 sein"));
            }
        }
    }
    for r in &o.regions {
        if r.id.trim().is_empty()
            || !(r.multiplier.is_finite() && (0.1..=10.0).contains(&r.multiplier))
        {
            return Err(format!(
                "Region {}: Faktor muss zwischen 0.1 und 10 liegen",
                r.id
            ));
        }
    }
    Ok(())
}

/// Effective catalog (cached per override version).
pub async fn current(st: &AppState) -> Arc<PriceCatalog> {
    let latest: Option<(i32, serde_json::Value)> = match sqlx::query_as(
        "SELECT version, entries FROM price_overrides ORDER BY version DESC LIMIT 1",
    )
    .fetch_optional(&st.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("price overrides unavailable, using built-in catalog: {e:#}");
            None
        }
    };
    let version = latest.as_ref().map(|l| l.0);
    if let Some((v, c)) = st.prices.read().ok().and_then(|g| g.clone()) {
        if v == version {
            return c;
        }
    }
    let o: PriceOverride = latest
        .and_then(|(_, e)| serde_json::from_value(e).ok())
        .unwrap_or_default();
    let c = Arc::new(merge(builtin(), &o, version));
    if let Ok(mut g) = st.prices.write() {
        *g = Some((version, c.clone()));
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    fn u(i: i64, cw: i64, cr: i64, o: i64) -> Usage {
        Usage {
            input_tokens: i,
            output_tokens: o,
            cache_read_tokens: cr,
            cache_write_tokens: cw,
            cost_usd: 0.0,
        }
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn catalog_is_complete_and_sourced() {
        let c = builtin();
        assert_eq!(c.version, "2026-11-07");
        assert!(c.entries.len() >= 25, "{}", c.entries.len());
        for e in &c.entries {
            assert!(
                c.sources.iter().any(|s| s.id == e.source),
                "{}: unknown source {}",
                e.id,
                e.source
            );
            assert!(matches!(e.currency.as_str(), "USD" | "CHF"), "{}", e.id);
            assert!(e.input > 0.0 && e.output > 0.0, "{}", e.id);
            assert_ne!(e.evidence, Evidence::Admin, "{}", e.id);
        }
        for r in &c.regions {
            assert!(c.sources.iter().any(|s| s.id == r.source), "{}", r.id);
        }
        // Unique ids, and no two lines for the same model on the same day.
        let mut ids: Vec<&str> = c.entries.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), c.entries.len());
        for day in [
            at(2026, 10, 1),
            at(2026, 11, 7),
            at(2026, 12, 15),
            at(2027, 2, 1),
        ] {
            let mut seen = std::collections::HashSet::new();
            for e in c.entries.iter().filter(|e| in_range(e, day.date_naive())) {
                for m in &e.models {
                    assert!(seen.insert(normalize_model(m)), "{m} twice on {day}");
                }
            }
        }
        assert!(validate(&PriceOverride {
            entries: c.entries.clone(),
            regions: c.regions.clone(),
            note: String::new()
        })
        .is_ok());
    }

    /// Spot checks against report/notes (USD per MTok).
    #[test]
    fn prices_match_the_report() {
        let c = builtin();
        let now = at(2026, 11, 7);
        let e = find(c, "claude-opus-5-5", now).unwrap();
        assert_eq!((e.input, e.output, e.cache_read), (4.0, 20.0, Some(0.2)));
        assert_eq!(
            (e.cache_write_short, e.cache_write_long),
            (Some(5.0), Some(8.0))
        );
        let e = find(c, "sonnet", now).unwrap();
        assert_eq!((e.input, e.output, e.cache_read), (2.0, 10.0, Some(0.1)));
        let e = find(c, "claude-haiku-5-5", now).unwrap();
        assert_eq!((e.input, e.output), (0.1, 0.5));
        assert_eq!(e.long_context.as_ref().unwrap().threshold_tokens, 100_000);
        let e = find(c, "gpt-6-astra", now).unwrap();
        assert_eq!((e.input, e.output, e.cache_read), (10.0, 50.0, Some(1.0)));
        let e = find(c, "openai/gpt-6.1-terra", now).unwrap();
        assert_eq!((e.input, e.flex_factor), (1.0, Some(0.5)));
        let e = find(c, "gpt-6-luna", now).unwrap();
        assert_eq!(e.cache_read, Some(0.01));
        let e = find(c, "mistral-large-4", now).unwrap();
        assert_eq!((e.input, e.output, e.cache_read), (0.7, 2.1, Some(0.07)));
        assert_eq!(
            find(c, "mistral-large-4", at(2026, 10, 1)).unwrap().input,
            1.36
        );
        let e = find(c, "gemma-4-31B-it", now).unwrap();
        assert_eq!((e.currency.as_str(), e.input, e.output), ("CHF", 0.2, 0.4));
        // Announced price changes.
        assert_eq!(
            find(c, "gemini-4-argon", at(2026, 11, 30)).unwrap().input,
            2.0
        );
        assert_eq!(
            find(c, "gemini-4-argon", at(2026, 12, 1)).unwrap().input,
            4.0
        );
        assert_eq!(
            find(c, "gemini-3.8-flash", at(2026, 12, 31)).unwrap().input,
            0.75
        );
        assert_eq!(
            find(c, "gemini-3.8-flash", at(2027, 1, 1)).unwrap().output,
            7.5
        );
        assert_eq!(region(c, "bedrock_eu").unwrap().multiplier, 1.1);
        assert_eq!(
            region(c, "azure_switzerland_north").unwrap().multiplier,
            1.3
        );
        assert!(
            find(c, "qwen3.5-397b", now).is_none(),
            "no price without input price"
        );
    }

    #[test]
    fn model_ids_are_normalised() {
        for (raw, want) in [
            ("eu.anthropic.claude-opus-5-5", "claude-opus-5-5"),
            (
                "us.anthropic.claude-haiku-4-5-20251001-v1:0",
                "claude-haiku-4-5",
            ),
            ("claude-sonnet-4-5@20250929", "claude-sonnet-4-5"),
            ("anthropic/claude-sonnet-5-5", "claude-sonnet-5-5"),
            ("queue/Mistral-Large-4", "mistral-large-4"),
            ("  GPT-6.1-Sol ", "gpt-6.1-sol"),
        ] {
            assert_eq!(normalize_model(raw), want, "{raw}");
        }
        assert!(find(builtin(), "", at(2026, 11, 7)).is_none());
        assert!(find(builtin(), "my-local-llama", at(2026, 11, 7)).is_none());
    }

    /// Report: M profile on Sonnet 5.5 with caching = $1.25 per attempt
    /// (10k input, 150k cache writes, 2.5M cache reads, 60k output).
    #[test]
    fn report_m_profile_reproduces() {
        let c = builtin();
        let now = at(2026, 11, 7);
        let m = u(10_000, 150_000, 2_500_000, 60_000);
        let sonnet = find(c, "claude-sonnet-5-5", now).unwrap();
        approx(cost(sonnet, &m, &PriceCtx::default(), None), 1.245);
        let opus = find(c, "claude-opus-5-5", now).unwrap();
        approx(cost(opus, &m, &PriceCtx::default(), None), 2.49);
        // Bedrock EU +10 %
        let eu = PriceCtx {
            region_mult: 1.1,
            ..Default::default()
        };
        approx(cost(sonnet, &m, &eu, None), 1.245 * 1.1);
        // Batch −50 % stacks with caching; 1 h writes with the long TTL.
        let batch = PriceCtx {
            tier: Tier::Batch,
            long_ttl: true,
            ..Default::default()
        };
        approx(
            cost(sonnet, &u(0, 1_000_000, 0, 0), &batch, None),
            4.0 * 0.5,
        );
        // Single-shot plan on Opus (report: 40k/6k = $0.28 sync).
        approx(
            cost(opus, &u(40_000, 0, 0, 6_000), &PriceCtx::default(), None),
            0.28,
        );
        // Flex halves Terra.
        let terra = find(c, "gpt-6.1-terra", now).unwrap();
        let flex = PriceCtx {
            tier: Tier::Flex,
            ..Default::default()
        };
        approx(cost(terra, &u(1_000_000, 0, 0, 0), &flex, None), 0.5);
    }

    #[test]
    fn long_context_rules_per_request() {
        let c = builtin();
        let now = at(2026, 11, 7);
        let haiku = find(c, "claude-haiku-5-5", now).unwrap();
        let small = u(50_000, 0, 40_000, 1_000);
        let big = u(10_000, 0, 95_000, 1_000);
        let d = PriceCtx::default();
        approx(
            cost(haiku, &small, &d, Some(prompt_tokens(&small))),
            (50_000.0 * 0.1 + 40_000.0 * 0.01 + 1_000.0 * 0.5) / 1e6,
        );
        // > 100K incl. cache reads: whole request at $0.50/$2.50/$0.05.
        approx(
            cost(haiku, &big, &d, Some(prompt_tokens(&big))),
            (10_000.0 * 0.5 + 95_000.0 * 0.05 + 1_000.0 * 2.5) / 1e6,
        );
        let sol = find(c, "gpt-6.1-sol", now).unwrap();
        let huge = u(300_000, 0, 0, 10_000);
        approx(
            cost(sol, &huge, &d, Some(300_000)),
            (300_000.0 * 4.0 + 10_000.0 * 15.0) / 1e6,
        );
        approx(
            cost(sol, &huge, &d, None),
            (300_000.0 * 2.0 + 10_000.0 * 10.0) / 1e6,
        );
        // Opus has no long-context surcharge.
        let opus = find(c, "claude-opus-5-5", now).unwrap();
        approx(
            cost(opus, &huge, &d, Some(300_000)),
            cost(opus, &huge, &d, None),
        );
    }

    #[test]
    fn meter_prices_deltas_and_remainder() {
        let c = builtin();
        let mut m = Meter::new(
            c,
            Some("claude-haiku-5-5"),
            PriceCtx::default(),
            "execute",
            None,
        );
        assert!(m.known() && m.usd());
        let d1 = u(10_000, 0, 95_000, 1_000); // long request
        let c1 = m.on_delta(&d1);
        approx(c1, (10_000.0 * 0.5 + 95_000.0 * 0.05 + 1_000.0 * 2.5) / 1e6);
        // Total reports more (subagent) → remainder at base price.
        let mut total = d1.clone();
        total.add(&u(1_000, 0, 0, 100));
        total.cost_usd = 99.0; // CLI dollars are ignored with a catalog price
        let s = m.finish(&total);
        approx(s.amount, c1 + (1_000.0 * 0.1 + 100.0 * 0.5) / 1e6);
        assert_eq!(s.source, CostSource::Catalog);
        assert_eq!(s.price_id.as_deref(), Some("anthropic/claude-haiku-5-5"));
        assert_eq!(s.usage.cost_usd, s.amount);
        assert!(s.notes.iter().any(|n| n.contains("Long-Context-Preis")));

        // Unknown model: CLI dollars as fallback, else unknown.
        let m = Meter::new(c, Some("llama-local"), PriceCtx::default(), "execute", None);
        let mut t = u(10, 0, 0, 10);
        t.cost_usd = 0.25;
        let s = m.finish(&t);
        assert_eq!((s.source, s.amount), (CostSource::Cli, 0.25));
        let s = m.finish(&u(10, 0, 0, 10));
        assert_eq!((s.source, s.amount), (CostSource::Unknown, 0.0));
        // CHF prices never feed the USD cost columns.
        let mut m = Meter::new(
            c,
            Some("gemma-4-31b-it"),
            PriceCtx::default(),
            "execute",
            None,
        );
        assert!(!m.usd());
        m.on_delta(&u(1_000_000, 0, 0, 0));
        let s = m.finish(&u(1_000_000, 0, 0, 0));
        assert_eq!(s.currency, "CHF");
        approx(s.amount, 0.2);
        assert_eq!(s.usage.cost_usd, 0.0);
    }

    #[test]
    fn overrides_replace_and_extend_and_are_validated() {
        let mut opus = find(builtin(), "claude-opus-5-5", at(2026, 11, 7))
            .unwrap()
            .clone();
        opus.input = 3.0;
        let mut own = opus.clone();
        own.id = "custom/llama".into();
        own.models = vec!["llama-local".into()];
        let o = PriceOverride {
            entries: vec![opus, own],
            regions: vec![],
            note: "test".into(),
        };
        validate(&o).unwrap();
        let c = merge(builtin(), &o, Some(4));
        assert_eq!(c.override_version, Some(4));
        let e = find(&c, "opus", at(2026, 11, 7)).unwrap();
        assert_eq!((e.input, e.evidence), (3.0, Evidence::Admin));
        assert!(find(&c, "llama-local", at(2026, 11, 7)).is_some());
        assert_eq!(c.entries.len(), builtin().entries.len() + 1);

        let mut bad = o.clone();
        bad.entries[0].input = -1.0;
        assert!(validate(&bad).unwrap_err().contains("zwischen 0 und 10000"));
        let mut bad = o.clone();
        bad.entries[0].currency = "EUR".into();
        assert!(validate(&bad).is_err());
        let mut bad = o.clone();
        bad.entries[0].batch_factor = Some(1.5);
        assert!(validate(&bad).is_err());
        let mut bad = o.clone();
        bad.entries[0].models = vec![" ".into()];
        assert!(validate(&bad).is_err());
        let mut bad = o;
        bad.entries[0].effective_from = NaiveDate::from_ymd_opt(2027, 1, 1);
        bad.entries[0].effective_until = NaiveDate::from_ymd_opt(2026, 1, 1);
        assert!(validate(&bad).is_err());
    }
}
