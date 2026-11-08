//! Money, cost estimates, access choices and routing in plain language (WP13).
use queued_api::{
    ConnectorTypeInfo, ConnectorView, CostSource, Estimate, Money, Residency, StepMode, Target,
};
use uuid::Uuid;

/// `0,0040 $`, `1,25 $`, `0,20 CHF` – more decimals for tiny amounts.
pub fn fmt_money(amount: f64, currency: &str) -> String {
    let a = if amount.is_finite() {
        amount.max(0.0)
    } else {
        0.0
    };
    let decimals = if a == 0.0 {
        0
    } else if a < 0.01 {
        4
    } else if a < 100.0 {
        2
    } else {
        0
    };
    let num = format!("{a:.decimals$}").replace('.', ",");
    match currency {
        "USD" => format!("{num} $"),
        c => format!("{num} {c}"),
    }
}

pub fn fmt_money_list(m: &[Money]) -> String {
    if m.is_empty() {
        return "–".into();
    }
    m.iter()
        .map(|x| fmt_money(x.amount, &x.currency))
        .collect::<Vec<_>>()
        .join(" + ")
}

/// "ca. 1,25 $ (0,46 $ bis 3,44 $) pro Versuch".
pub fn estimate_summary(e: &Estimate) -> String {
    if e.mid.is_empty() {
        return "Kosten unbekannt – für dieses Modell steht kein Preis in der Preistabelle".into();
    }
    let range = if e.low != e.high {
        format!(
            " ({} bis {})",
            fmt_money_list(&e.low),
            fmt_money_list(&e.high)
        )
    } else {
        String::new()
    };
    format!("ca. {}{range} pro Versuch", fmt_money_list(&e.mid))
}

pub fn step_label(step: &str) -> &'static str {
    match step {
        "plan" => "Planung",
        "execute" => "Umsetzung",
        "review" => "Prüfung",
        "analyze" => "Analyse",
        _ => "Schritt",
    }
}

pub fn mode_label(mode: &str) -> &'static str {
    match mode {
        "off" => "aus",
        "sync" => "direkt",
        "batch" => "Batch (−50 %, bis 24 h)",
        _ => "Agent",
    }
}

pub fn step_mode_label(m: StepMode) -> &'static str {
    mode_label(m.as_str())
}

pub fn source_label(s: CostSource) -> &'static str {
    match s {
        CostSource::Catalog => "aus der Preistabelle",
        CostSource::Cli => "Angabe der CLI",
        CostSource::Unknown => "kein Preis bekannt",
    }
}

pub const RESIDENCIES: [Residency; 3] = [Residency::Any, Residency::Eu, Residency::Ch];

/// Value of the access `<select>`: `""` = standard, `p:<provider>`, `c:<uuid>`.
pub fn parse_access(v: &str) -> (Option<Uuid>, Option<String>) {
    if let Some(id) = v.strip_prefix("c:") {
        return (Uuid::parse_str(id).ok(), None);
    }
    if let Some(p) = v.strip_prefix("p:") {
        return (None, Some(p.to_string()).filter(|p| !p.is_empty()));
    }
    (None, None)
}

pub fn access_value(connector: Option<Uuid>, provider: Option<&str>) -> String {
    match (connector, provider) {
        (Some(id), _) => format!("c:{id}"),
        (None, Some(p)) => format!("p:{p}"),
        _ => String::new(),
    }
}

pub fn type_label(types: &[ConnectorTypeInfo], kind: &str) -> String {
    types
        .iter()
        .find(|t| t.kind == kind)
        .map(|t| t.label.clone())
        .unwrap_or_else(|| kind.to_string())
}

pub fn group_label(group: &str) -> &'static str {
    match group {
        "claude" => "Claude (Anthropic)",
        "openai" => "OpenAI / ChatGPT",
        "google" => "Google Gemini",
        "eu_ch" => "Datenstandort EU / Schweiz",
        _ => "Offen, Gateways und eigene Server",
    }
}

pub const GROUPS: [&str; 5] = ["claude", "openai", "eu_ch", "google", "open"];

/// "Mein Key · EU (selbst angegeben)".
pub fn connector_label(c: &ConnectorView) -> String {
    let loc = c.location.label();
    if c.location_declared {
        format!("{} · {loc} (selbst angegeben)", c.label)
    } else {
        format!("{} · {loc}", c.label)
    }
}

/// Access + model of a routing step for display.
pub fn target_text(
    t: Option<&Target>,
    connectors: &[ConnectorView],
    provider_label: impl Fn(&str) -> String,
    fallback: &str,
) -> String {
    let Some(t) = t else {
        return fallback.to_string();
    };
    let access = match (t.connector_id, t.provider.as_deref()) {
        (Some(id), _) => connectors
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.label.clone())
            .unwrap_or_else(|| "(Zugang nicht verfügbar)".into()),
        (None, Some(p)) => provider_label(p),
        (None, None) => fallback.to_string(),
    };
    match t.model.as_deref().filter(|m| !m.is_empty()) {
        Some(m) => format!("{access} · {m}"),
        None => access,
    }
}

/// "Mistral EU · mistral-large-4" (connector) or "Claude · opus" (legacy).
pub fn access_text(
    t: &queued_api::Task,
    connectors: &[ConnectorView],
    provider_label: impl Fn(&str) -> String,
) -> String {
    let access = t
        .connector_id
        .and_then(|id| connectors.iter().find(|c| c.id == id))
        .map(|c| c.label.clone())
        .unwrap_or_else(|| provider_label(&t.provider));
    match &t.model {
        Some(m) => format!("{access} · {m}"),
        None => access,
    }
}

/// Model suggestions for an access (type examples).
pub fn model_suggestions(
    types: &[ConnectorTypeInfo],
    connector: Option<&ConnectorView>,
    provider: Option<&str>,
) -> Vec<String> {
    if let Some(c) = connector {
        return types
            .iter()
            .find(|t| t.kind == c.kind)
            .map(|t| t.models.clone())
            .unwrap_or_default();
    }
    match provider {
        Some("claude") | Some("mock") => vec![
            "claude-sonnet-5-5".into(),
            "claude-opus-5-5".into(),
            "claude-haiku-5-5".into(),
        ],
        Some("codex") => vec!["gpt-6.1-terra".into(), "gpt-6.1-sol".into()],
        Some("gemini") => vec!["gemini-3.8-flash".into(), "gemini-4-argon".into()],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_formatting() {
        assert_eq!(fmt_money(0.0, "USD"), "0 $");
        assert_eq!(fmt_money(0.003_98, "USD"), "0,0040 $");
        assert_eq!(fmt_money(1.245, "USD"), "1,25 $");
        assert_eq!(fmt_money(0.2, "CHF"), "0,20 CHF");
        assert_eq!(fmt_money(1234.5, "USD"), "1234 $");
        assert_eq!(fmt_money(f64::NAN, "USD"), "0 $");
        assert_eq!(fmt_money(-1.0, "USD"), "0 $");
        let l = vec![
            Money {
                currency: "USD".into(),
                amount: 1.0,
            },
            Money {
                currency: "CHF".into(),
                amount: 0.5,
            },
        ];
        assert_eq!(fmt_money_list(&l), "1,00 $ + 0,50 CHF");
        assert_eq!(fmt_money_list(&[]), "–");
    }

    #[test]
    fn estimate_text() {
        let m = |a: f64| {
            vec![Money {
                currency: "USD".into(),
                amount: a,
            }]
        };
        let e = Estimate {
            steps: vec![],
            low: m(0.46),
            mid: m(1.245),
            high: m(3.44),
            location: "USA".into(),
            notes: vec![],
        };
        assert_eq!(
            estimate_summary(&e),
            "ca. 1,25 $ (0,46 $ bis 3,44 $) pro Versuch"
        );
        let e = Estimate {
            low: vec![],
            mid: vec![],
            high: vec![],
            ..e
        };
        assert!(estimate_summary(&e).contains("unbekannt"));
    }

    #[test]
    fn access_values_roundtrip() {
        let id = Uuid::nil();
        assert_eq!(
            parse_access(&access_value(Some(id), None)),
            (Some(id), None)
        );
        assert_eq!(parse_access("p:claude"), (None, Some("claude".into())));
        assert_eq!(parse_access(""), (None, None));
        assert_eq!(parse_access("c:kaputt"), (None, None));
        assert_eq!(access_value(None, None), "");
    }

    #[test]
    fn labels() {
        assert_eq!(step_label("execute"), "Umsetzung");
        assert_eq!(mode_label("batch"), "Batch (−50 %, bis 24 h)");
        assert_eq!(step_mode_label(StepMode::Sync), "direkt");
        assert_eq!(source_label(CostSource::Cli), "Angabe der CLI");
        assert_eq!(group_label("eu_ch"), "Datenstandort EU / Schweiz");
        let t = Target {
            connector_id: None,
            provider: Some("mock".into()),
            model: Some("claude-sonnet-5-5".into()),
        };
        assert_eq!(
            target_text(Some(&t), &[], |p| p.to_uppercase(), "wie Umsetzung"),
            "MOCK · claude-sonnet-5-5"
        );
        assert_eq!(
            target_text(None, &[], |p| p.into(), "wie Umsetzung"),
            "wie Umsetzung"
        );
        assert_eq!(
            model_suggestions(&[], None, Some("codex"))[0],
            "gpt-6.1-terra"
        );
    }
}
