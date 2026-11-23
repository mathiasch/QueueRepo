//! Per-step routing (WP13): presets, validation and resolution of the
//! access/model for plan, agent loop, review and OnEmpty analysis.
//!
//! Hard rules enforced on the server:
//! * project data residency (`any|eu|ch`) for every step's access;
//! * the review never runs on the cheapest model tier (report: a wrongly
//!   accepted diff costs more than the saved cents);
//! * single-shot steps need an access with a direct API (sync) or a batch API.
use queued_api::{
    DataLocation, DecisionSource, ExecuteRoute, ModelTier, PresetInfo, PriceCatalog, Residency,
    Routing, RoutingDecision, RoutingRule, RuleWhen, StepMode, StepRoute, TaskFacts, Target,
    MAX_RULES,
};
use uuid::Uuid;

use crate::{
    connectors::{self, Row},
    pricing,
    state::AppState,
};

/// An access a step can use: a connector or a legacy provider credential.
#[derive(Debug, Clone)]
pub struct Access {
    pub connector_id: Option<Uuid>,
    /// Connector kind, or `legacy:<provider>`.
    pub kind: String,
    pub label: String,
    pub harness: String,
    pub location: DataLocation,
    pub single_shot: bool,
    pub batch: bool,
    pub flex: bool,
    /// WP12: wire API of a mock connector (`anthropic|openai|mistral`).
    pub wire_api: Option<String>,
}

impl Access {
    pub fn target(&self, model: Option<&str>) -> Target {
        Target {
            connector_id: self.connector_id,
            provider: self.connector_id.is_none().then(|| self.harness.clone()),
            model: model.map(String::from),
        }
    }
}

/// Accesses `user` may use in `project`: own user-wide connectors, project
/// connectors (editors) and legacy credentials.
pub async fn accesses(st: &AppState, user: Uuid, project: Uuid) -> anyhow::Result<Vec<Access>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{} WHERE (user_id=$1 AND project_id IS NULL) OR project_id=$2 ORDER BY created_at",
        connectors::SELECT
    ))
    .bind(user)
    .bind(project)
    .fetch_all(&st.db)
    .await?;
    let mut out = vec![];
    for r in rows {
        if !connectors::can_use(st, &r, user, Some(project)).await? {
            continue;
        }
        out.push(access_of(&r));
    }
    let legacy: Vec<String> = sqlx::query_scalar(
        "SELECT provider FROM provider_credentials WHERE user_id=$1 ORDER BY provider",
    )
    .bind(user)
    .fetch_all(&st.db)
    .await?;
    for p in legacy
        .into_iter()
        .chain(st.cfg.enable_mock_provider.then(|| "mock".to_string()))
    {
        if !st.cfg.providers().contains(&p.as_str()) || p == "opencode" {
            continue;
        }
        out.push(Access {
            connector_id: None,
            kind: format!("legacy:{p}"),
            label: format!("{p} (Einstellungen → Zugänge)"),
            location: connectors::legacy_location(&p),
            harness: p,
            single_shot: false,
            batch: false,
            flex: false,
            wire_api: None,
        });
    }
    Ok(out)
}

pub fn access_of(r: &Row) -> Access {
    let (location, _) = connectors::location(&r.3, &r.5);
    Access {
        connector_id: Some(r.0),
        kind: r.3.clone(),
        label: r.4.clone(),
        harness: connectors::harness(&r.3, &r.5),
        location,
        single_shot: connectors::wire(&r.3, &r.5).is_some(),
        batch: connectors::batch_api(&r.3, &r.5).is_some(),
        flex: connectors::type_info(&r.3, true).is_some_and(|t| t.flex),
        wire_api: (r.3 == "mock").then(|| {
            r.5.get("api")
                .and_then(|v| v.as_str())
                .unwrap_or("anthropic")
                .to_string()
        }),
    }
}

/// Access for a target (from the list of usable accesses).
pub fn find<'a>(all: &'a [Access], t: &Target) -> Option<&'a Access> {
    match (t.connector_id, t.provider.as_deref()) {
        (Some(id), _) => all.iter().find(|a| a.connector_id == Some(id)),
        (None, Some(p)) => all
            .iter()
            .find(|a| a.connector_id.is_none() && a.harness == p),
        (None, None) => None,
    }
}

pub fn tier_of(cat: &PriceCatalog, model: &str) -> Option<ModelTier> {
    pricing::find(cat, model, chrono::Utc::now()).map(|e| e.tier)
}

/// Validates a routing against the caller's accesses and the residency.
/// German error messages (shown in the UI).
pub fn validate(
    r: &Routing,
    residency: Residency,
    all: &[Access],
    cat: &PriceCatalog,
) -> Result<(), String> {
    let exec_target = r
        .execute
        .target
        .as_ref()
        .ok_or("Für den Agenten (Umsetzung) muss ein Zugang gewählt sein")?;
    let exec = find(all, exec_target).ok_or("Zugang für die Umsetzung nicht gefunden")?;
    check_location(residency, exec, "Der Zugang für die Umsetzung")?;
    if exec.harness == "opencode" && model_of(exec_target).is_none() {
        return Err("Für opencode muss bei der Umsetzung ein Modell angegeben werden".into());
    }
    if r.execute.flex && !exec.flex {
        return Err("Flex gibt es nur mit einem OpenAI-API-Zugang".into());
    }
    if r.execute.escalation.len() > 5
        || r.execute
            .escalation
            .iter()
            .any(|m| m.trim().is_empty() || m.len() > 200)
    {
        return Err("Eskalation: höchstens 5 Modellnamen".into());
    }
    if exec_target.model.as_deref().is_some_and(|m| m.len() > 200) {
        return Err("Modellname zu lang".into());
    }
    let steps = [
        ("Planung", &r.plan, true),
        ("Review", &r.review, false),
        ("Analyse", &r.analyze, false),
    ];
    for (name, s, is_plan) in steps {
        match (s.mode, is_plan) {
            (StepMode::Off, false) => {
                return Err(format!("{name}: „aus“ gibt es nur für die Planung (Review abschalten: Projekt-Einstellung)"))
            }
            (StepMode::Harness, true) => {
                return Err("Planung: nur „aus“, „direkt“ oder „Batch“ möglich".into())
            }
            (StepMode::Off, true) => continue,
            _ => {}
        }
        let (acc, model) = match &s.target {
            Some(t) => (
                find(all, t).ok_or(format!("{name}: Zugang nicht gefunden"))?,
                model_of(t).or(model_of(exec_target)),
            ),
            None => (exec, model_of(exec_target)),
        };
        check_location(residency, acc, &format!("Der Zugang für „{name}“"))?;
        match s.mode {
            StepMode::Sync if !acc.single_shot => {
                return Err(format!(
                "{name}: „{}“ kann keine direkten API-Aufrufe (Zugang mit API-Schlüssel wählen)",
                acc.label
            ))
            }
            StepMode::Batch if !acc.batch => {
                return Err(format!(
                    "{name}: „{}“ bietet kein Batch (Anthropic API, OpenAI API oder Mistral)",
                    acc.label
                ))
            }
            StepMode::Sync | StepMode::Batch if model.is_none() => {
                return Err(format!(
                    "{name}: für direkte Aufrufe muss ein Modell angegeben werden"
                ))
            }
            StepMode::Harness if acc.harness == "opencode" && model.is_none() => {
                return Err(format!(
                    "{name}: für opencode muss ein Modell angegeben werden"
                ))
            }
            _ => {}
        }
        if !is_plan && name == "Review" {
            if let Some(m) = model {
                if tier_of(cat, m) == Some(ModelTier::Cheap) {
                    return Err(format!(
                        "Review: „{m}“ ist ein Modell der günstigsten Stufe – ein falsch akzeptierter Diff kostet mehr als die Ersparnis. Bitte ein stärkeres Modell wählen."
                    ));
                }
            }
        }
    }
    validate_rules(&r.rules, residency, all)
}

/// WP12: rule list checks (German messages).
pub fn validate_rules(rules: &[RoutingRule], residency: Residency, all: &[Access]) -> Result<(), String> {
    if rules.len() > MAX_RULES {
        return Err(format!("Höchstens {MAX_RULES} Regeln"));
    }
    for (i, r) in rules.iter().enumerate() {
        let name = r.name.trim();
        let label = format!("Regel {} („{}“)", i + 1, name);
        if name.is_empty() || name.chars().count() > 80 {
            return Err(format!("Regel {}: Name fehlt oder ist zu lang (max. 80 Zeichen)", i + 1));
        }
        let w = &r.when;
        if w.kinds.len() > 2 || w.kinds.iter().any(|k| k != "task" && k != "fix") {
            return Err(format!("{label}: Auftragsart nur „task“ oder „fix“"));
        }
        if let (Some(a), Some(b)) = (w.min_chars, w.max_chars) {
            if a > b {
                return Err(format!("{label}: „mindestens“ ist größer als „höchstens“"));
            }
        }
        if w.max_chars.is_some_and(|m| m > 1_000_000) || w.min_chars.is_some_and(|m| m > 1_000_000) {
            return Err(format!("{label}: Zeichenzahl zu groß"));
        }
        if w.keywords.len() > 10
            || w.keywords
                .iter()
                .any(|k| k.trim().is_empty() || k.chars().count() > 60)
        {
            return Err(format!("{label}: höchstens 10 Stichworte mit je 1–60 Zeichen"));
        }
        let acc = find(all, &r.target).ok_or(format!("{label}: Zugang nicht gefunden"))?;
        check_location(residency, acc, &format!("Der Zugang von {label}"))?;
        let model = model_of(&r.target);
        if model.is_some_and(|m| m.len() > 200) {
            return Err(format!("{label}: Modellname zu lang"));
        }
        if model.is_none() && (acc.harness == "opencode" || acc.kind == "bedrock") {
            return Err(format!("{label}: für „{}“ muss ein Modell angegeben werden", acc.label));
        }
    }
    Ok(())
}

/// Features of a task for the routing (deterministic, no model call).
pub fn facts(kind: &str, prompt: &str, context: &str, attachments: usize) -> TaskFacts {
    TaskFacts {
        kind: kind.to_string(),
        chars: u32::try_from(prompt.trim().chars().count() + context.trim().chars().count())
            .unwrap_or(u32::MAX),
        attachments: u32::try_from(attachments).unwrap_or(u32::MAX),
    }
}

/// What the routing decides on: the task's features and its text (title +
/// prompt, for keyword rules).
#[derive(Debug, Clone, Default)]
pub struct Ask {
    pub facts: TaskFacts,
    pub text: String,
}

impl Ask {
    pub fn new(kind: &str, title: &str, prompt: &str, context: &str, attachments: usize) -> Self {
        Self {
            facts: facts(kind, prompt, context, attachments),
            text: format!("{title}\n{prompt}"),
        }
    }
}

/// Whether a rule's conditions hold for the task.
pub fn rule_matches(w: &RuleWhen, f: &TaskFacts, text: &str) -> bool {
    let kind_ok = if w.kinds.is_empty() {
        f.kind == "task"
    } else {
        w.kinds.iter().any(|k| *k == f.kind)
    };
    let lower = text.to_lowercase();
    kind_ok
        && w.max_chars.is_none_or(|m| f.chars <= m)
        && w.min_chars.is_none_or(|m| f.chars >= m)
        && (w.keywords.is_empty()
            || w.keywords
                .iter()
                .any(|k| lower.contains(&k.trim().to_lowercase())))
        && w.has_attachments.is_none_or(|h| (f.attachments > 0) == h)
}

/// Short German description of a rule's conditions.
pub fn describe_when(w: &RuleWhen) -> String {
    let mut parts = vec![];
    if !w.kinds.is_empty() {
        parts.push(
            w.kinds
                .iter()
                .map(|k| if k == "fix" { "Fix-Aufträge" } else { "neue Aufträge" })
                .collect::<Vec<_>>()
                .join(" und "),
        );
    }
    if let Some(m) = w.max_chars {
        parts.push(format!("höchstens {m} Zeichen"));
    }
    if let Some(m) = w.min_chars {
        parts.push(format!("mindestens {m} Zeichen"));
    }
    if !w.keywords.is_empty() {
        parts.push(format!("Stichwort {}", w.keywords.join("/")));
    }
    match w.has_attachments {
        Some(true) => parts.push("mit Anhängen".into()),
        Some(false) => parts.push("ohne Anhänge".into()),
        None => {}
    }
    if parts.is_empty() {
        "alle neuen Aufträge".into()
    } else {
        parts.join(", ")
    }
}

fn target_text(t: &Target) -> String {
    match (t.model.as_deref(), t.provider.as_deref(), t.connector_id) {
        (Some(m), _, _) => m.to_string(),
        (None, Some(p), _) => format!("Standardmodell von {p}"),
        (None, None, Some(_)) => "Standardmodell des Zugangs".into(),
        _ => "Standardmodell".into(),
    }
}

/// The explicit choice of a task (composer, API, template, Best-of-N).
#[derive(Debug, Clone, Default)]
pub struct Explicit {
    pub connector: Option<Uuid>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Best-of-N attempt (changes only the explanation).
    pub attempt: bool,
}

/// WP12 routing decision of a new task's agent loop (pure, deterministic):
/// explicit choice > first matching rule > the project's agent-loop route >
/// the user's default provider. Access rights and residency are checked by
/// [`resolve_execute`] afterwards.
pub fn decide(
    routing: Option<&Routing>,
    ask: &Ask,
    explicit: &Explicit,
    default_provider: &str,
) -> RoutingDecision {
    let model = explicit
        .model
        .as_ref()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let route = routing.and_then(|r| r.execute.target.clone());
    let mk = |source, target: Target, rule: Option<(usize, &RoutingRule)>, reason: String| {
        RoutingDecision {
            step: "execute".into(),
            source,
            target,
            rule: rule.map(|(_, r)| r.name.trim().to_string()),
            rule_index: rule.and_then(|(i, _)| u32::try_from(i).ok()),
            escalation_level: None,
            reason,
            facts: ask.facts.clone(),
        }
    };
    let provider = explicit.provider.as_deref().filter(|p| !p.is_empty());
    let src = if explicit.attempt {
        DecisionSource::Attempt
    } else {
        DecisionSource::Explicit
    };
    let src_text = if explicit.attempt {
        "Für diesen Versuch gewählt"
    } else {
        "Am Auftrag gewählt"
    };
    match (explicit.connector, provider) {
        (Some(c), _) => {
            // The route's model applies when the task picked the route's access.
            let rm = route
                .as_ref()
                .filter(|t| t.connector_id == Some(c))
                .and_then(|t| t.model.clone());
            let t = Target {
                connector_id: Some(c),
                provider: None,
                model: model.or(rm),
            };
            let reason = format!("{src_text}: {}", target_text(&t));
            return mk(src, t, None, reason);
        }
        (None, Some(p)) => {
            let t = Target {
                connector_id: None,
                provider: Some(p.to_string()),
                model,
            };
            let reason = format!("{src_text}: {}", target_text(&t));
            return mk(src, t, None, reason);
        }
        (None, None) => {}
    }
    if let Some(r) = routing {
        if let Some((i, rule)) = r
            .rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule_matches(&rule.when, &ask.facts, &ask.text))
        {
            let mut t = rule.target.clone();
            if model.is_some() {
                t.model = model.clone();
            }
            let reason = format!(
                "Regel {} „{}“ ({}) → {}",
                i + 1,
                rule.name.trim(),
                describe_when(&rule.when),
                target_text(&t)
            );
            return mk(DecisionSource::Rule, t, Some((i, rule)), reason);
        }
    }
    match route {
        Some(mut t) => {
            if model.is_some() {
                t.model = model;
            }
            let preset = routing.and_then(|r| r.preset.as_deref()).filter(|p| *p != "custom");
            let reason = match (preset, routing.is_some_and(|r| !r.rules.is_empty())) {
                (Some(p), true) => format!("Keine Regel passt – Umsetzung laut Vorlage „{p}“: {}", target_text(&t)),
                (Some(p), false) => format!("Umsetzung laut Vorlage „{p}“: {}", target_text(&t)),
                (None, true) => format!("Keine Regel passt – Umsetzung laut Projekt-Routing: {}", target_text(&t)),
                (None, false) => format!("Umsetzung laut Projekt-Routing: {}", target_text(&t)),
            };
            mk(DecisionSource::Route, t, None, reason)
        }
        None => {
            let t = Target {
                connector_id: None,
                provider: Some(default_provider.to_string()),
                model,
            };
            let reason = format!("Kein Projekt-Routing – Standard-Anbieter: {}", target_text(&t));
            mk(DecisionSource::Default, t, None, reason)
        }
    }
}

/// Decision for tasks that keep the access of their origin (follow-ups,
/// pull-request fixes, continuations, OnEmpty analyses) as JSON.
pub fn inherited_json(
    kind: &str,
    provider: &str,
    connector: Option<Uuid>,
    model: Option<&str>,
    source: DecisionSource,
    why: &str,
) -> Option<serde_json::Value> {
    let target = Target {
        connector_id: connector,
        provider: connector.is_none().then(|| provider.to_string()),
        model: model.map(String::from),
    };
    serde_json::to_value(RoutingDecision {
        step: "execute".into(),
        source,
        reason: format!("{why}: {}", target_text(&target)),
        target,
        rule: None,
        rule_index: None,
        escalation_level: None,
        facts: TaskFacts {
            kind: kind.into(),
            ..Default::default()
        },
    })
    .ok()
}

/// WP12 routing decision of a fix task (pure): escalation along the chain
/// of the routing's agent-loop access > a rule for fix tasks > the failed
/// task's access and model.
pub fn decide_fix(
    routing: Option<&Routing>,
    failed: &crate::models::Task,
    ask: &Ask,
) -> RoutingDecision {
    let inherited = Target {
        connector_id: failed.connector_id,
        provider: failed.connector_id.is_none().then(|| failed.provider.clone()),
        model: failed.model.clone(),
    };
    if let Some((m, level)) = escalate(routing, failed) {
        let from = failed.model.clone().unwrap_or_else(|| "Standardmodell".into());
        return RoutingDecision {
            step: "execute".into(),
            source: DecisionSource::Escalation,
            target: Target {
                model: Some(m.clone()),
                ..inherited
            },
            rule: None,
            rule_index: None,
            escalation_level: Some(level),
            reason: format!("Eskalation nach Fehlversuch (Stufe {level}): {from} → {m}"),
            facts: ask.facts.clone(),
        };
    }
    if let Some(r) = routing {
        if let Some((i, rule)) = r
            .rules
            .iter()
            .enumerate()
            .find(|(_, rule)| !rule.when.kinds.is_empty() && rule_matches(&rule.when, &ask.facts, &ask.text))
        {
            return RoutingDecision {
                step: "execute".into(),
                source: DecisionSource::Rule,
                target: rule.target.clone(),
                rule: Some(rule.name.trim().to_string()),
                rule_index: u32::try_from(i).ok(),
                escalation_level: Some(failed.escalation_level),
                reason: format!(
                    "Regel {} „{}“ ({}) → {}",
                    i + 1,
                    rule.name.trim(),
                    describe_when(&rule.when),
                    target_text(&rule.target)
                ),
                facts: ask.facts.clone(),
            };
        }
    }
    let why = match routing.and_then(|r| r.execute.target.as_ref()) {
        Some(_) if !routing.is_some_and(|r| r.execute.escalation.is_empty()) => {
            "Eskalationskette ausgeschöpft oder anderer Zugang – wie der gescheiterte Auftrag"
        }
        _ => "Wie der gescheiterte Auftrag",
    };
    RoutingDecision {
        step: "execute".into(),
        source: DecisionSource::Inherited,
        reason: format!("{why}: {}", target_text(&inherited)),
        target: inherited,
        rule: None,
        rule_index: None,
        escalation_level: Some(failed.escalation_level),
        facts: ask.facts.clone(),
    }
}

fn model_of(t: &Target) -> Option<&str> {
    t.model.as_deref().map(str::trim).filter(|m| !m.is_empty())
}

fn check_location(r: Residency, a: &Access, what: &str) -> Result<(), String> {
    if r.allows(a.location) {
        Ok(())
    } else {
        Err(connectors::residency_error(r, a.location, what))
    }
}

/// Agent-loop access of a new task.
#[derive(Debug, Clone, PartialEq)]
pub struct Exec {
    pub provider: String,
    pub connector_id: Option<Uuid>,
    pub model: Option<String>,
    /// WP12: why (stored at the task, shown in the UI).
    pub decision: RoutingDecision,
}

impl Exec {
    pub fn decision_json(&self) -> Option<serde_json::Value> {
        serde_json::to_value(&self.decision).ok()
    }
}

/// Resolves the access of a new/edited task with [`decide`] (explicit >
/// rule > project route > user default), then checks access rights, data
/// residency and required models. `ask` = the task's features (`None`: no
/// rule applies, e.g. a continuation with the origin's access).
/// `Err` = German message for a 400.
pub async fn resolve_execute(
    st: &AppState,
    user: Uuid,
    project: &crate::models::Project,
    explicit: Explicit,
    ask: Option<&Ask>,
    default_provider: &str,
) -> anyhow::Result<Result<Exec, String>> {
    if explicit.model.as_ref().is_some_and(|m| m.trim().len() > 200) {
        return Ok(Err("Modellname zu lang".into()));
    }
    let none = Ask::default();
    let routing = project.routing();
    let decision = decide(routing.as_ref(), ask.unwrap_or(&none), &explicit, default_provider);
    check_decision(st, user, project, decision, default_provider).await
}

/// Access check of a decision (shared by new tasks, fix tasks and attempts).
pub async fn check_decision(
    st: &AppState,
    user: Uuid,
    project: &crate::models::Project,
    mut decision: RoutingDecision,
    default_provider: &str,
) -> anyhow::Result<Result<Exec, String>> {
    let model = decision
        .target
        .model
        .as_ref()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    if model.as_ref().is_some_and(|m| m.len() > 200) {
        return Ok(Err("Modellname zu lang".into()));
    }
    let residency = project.residency();
    let exec = match decision.target.connector_id {
        Some(id) => {
            let Some(c) = connectors::load_for(st, id, user, Some(project.id)).await? else {
                return Ok(Err("Zugang nicht gefunden".into()));
            };
            if !residency.allows(c.location) {
                return Ok(Err(connectors::residency_error(
                    residency,
                    c.location,
                    &format!("Der Zugang „{}“", c.label),
                )));
            }
            if model.is_none() && (c.harness == "opencode" || c.kind == "bedrock") {
                return Ok(Err(format!(
                    "Für „{}“ muss ein Modell angegeben werden",
                    c.label
                )));
            }
            decision.reason = format!("{} (Zugang „{}“)", decision.reason, c.label);
            decision.target.model = model.clone();
            Exec {
                provider: c.harness,
                connector_id: Some(c.id),
                model,
                decision,
            }
        }
        None => {
            let p = decision
                .target
                .provider
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| default_provider.to_string());
            if !st.cfg.providers().contains(&p.as_str()) {
                return Ok(Err("unknown provider".into()));
            }
            if crate::agent::credential_kinds(&p).is_empty() {
                return Ok(Err(format!(
                    "{p} kann nur über einen Zugang (Connector) genutzt werden"
                )));
            }
            let loc = connectors::legacy_location(&p);
            if !residency.allows(loc) {
                return Ok(Err(connectors::residency_error(
                    residency,
                    loc,
                    &format!("Der Standard-Zugang „{p}“"),
                )));
            }
            decision.target.provider = Some(p.clone());
            decision.target.model = model.clone();
            Exec {
                provider: p,
                connector_id: None,
                model,
                decision,
            }
        }
    };
    Ok(Ok(exec))
}

/// Model of the next fix attempt along the escalation chain, if the failed
/// task ran on the routing's agent-loop access.
pub fn escalate(routing: Option<&Routing>, failed: &crate::models::Task) -> Option<(String, i32)> {
    let r = routing?;
    let t = r.execute.target.as_ref()?;
    let same = match t.connector_id {
        Some(c) => failed.connector_id == Some(c),
        None => {
            failed.connector_id.is_none() && t.provider.as_deref() == Some(failed.provider.as_str())
        }
    };
    if !same {
        return None;
    }
    let next = failed.escalation_level + 1;
    r.execute
        .escalation
        .get(usize::try_from(next - 1).ok()?)
        .map(|m| (m.clone(), next))
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

struct Cand {
    kinds: &'static [&'static str],
    model: &'static str,
}

struct StepDef {
    mode: StepMode,
    cands: &'static [Cand],
    /// Fallback when no candidate fits: same access as the agent loop
    /// (harness) with this model, or `None` = step off (plan only).
    fallback_model: Option<&'static str>,
}

struct PresetDef {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    residency: Residency,
    plan: StepDef,
    execute: &'static [Cand],
    escalation: &'static [&'static str],
    flex: bool,
    review: StepDef,
    analyze: StepDef,
}

const CLAUDE_EXEC: &[&str] = &[
    "anthropic_api",
    "claude_subscription",
    "legacy:claude",
    "bedrock",
    "vertex",
    "anthropic_gateway",
    "legacy:mock",
    "mock",
];
const ANTHROPIC_BATCH: &[&str] = &["anthropic_api", "mock"];
const OPENAI: &[&str] = &["openai_api"];

const PRESETS: &[PresetDef] = &[
    PresetDef {
        id: "cheap",
        label: "Günstig",
        description: "Haiku arbeitet zuerst; scheitert ein Auftrag, übernimmt Sonnet, dann Opus. Opus plant per Batch, Sonnet prüft.",
        residency: Residency::Any,
        plan: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: ANTHROPIC_BATCH, model: "claude-opus-5-5" }], fallback_model: None },
        execute: &[Cand { kinds: CLAUDE_EXEC, model: "claude-haiku-5-5" }],
        escalation: &["claude-sonnet-5-5", "claude-opus-5-5"],
        flex: false,
        review: StepDef { mode: StepMode::Harness, cands: &[], fallback_model: Some("claude-sonnet-5-5") },
        analyze: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: ANTHROPIC_BATCH, model: "claude-sonnet-5-5" }], fallback_model: Some("claude-sonnet-5-5") },
    },
    PresetDef {
        id: "balanced",
        label: "Ausgewogen",
        description: "Opus plant per Batch, Sonnet setzt um und prüft. Gutes Verhältnis von Qualität und Kosten.",
        residency: Residency::Any,
        plan: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: ANTHROPIC_BATCH, model: "claude-opus-5-5" }], fallback_model: None },
        execute: &[Cand { kinds: CLAUDE_EXEC, model: "claude-sonnet-5-5" }],
        escalation: &["claude-opus-5-5"],
        flex: false,
        review: StepDef { mode: StepMode::Harness, cands: &[], fallback_model: Some("claude-sonnet-5-5") },
        analyze: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: ANTHROPIC_BATCH, model: "claude-sonnet-5-5" }], fallback_model: Some("claude-sonnet-5-5") },
    },
    PresetDef {
        id: "best",
        label: "Beste Qualität",
        description: "Opus für alles. Am teuersten, für schwierige Projekte.",
        residency: Residency::Any,
        plan: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: ANTHROPIC_BATCH, model: "claude-opus-5-5" }], fallback_model: None },
        execute: &[Cand { kinds: CLAUDE_EXEC, model: "claude-opus-5-5" }],
        escalation: &[],
        flex: false,
        review: StepDef { mode: StepMode::Harness, cands: &[], fallback_model: Some("claude-opus-5-5") },
        analyze: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: ANTHROPIC_BATCH, model: "claude-opus-5-5" }], fallback_model: Some("claude-opus-5-5") },
    },
    PresetDef {
        id: "eu",
        label: "Nur EU/CH",
        description: "Daten bleiben in der EU oder der Schweiz: Claude über Bedrock/Vertex EU oder Mistral über den EU-Endpunkt. Setzt die Projektregel auf „Nur EU/Schweiz“.",
        residency: Residency::Eu,
        plan: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: &["mistral"], model: "mistral-large-4" }], fallback_model: None },
        execute: &[
            Cand { kinds: &["bedrock", "vertex", "azure_openai"], model: "claude-sonnet-5-5" },
            Cand { kinds: &["mistral"], model: "mistral-large-4" },
            Cand { kinds: &["legacy:mock", "mock"], model: "claude-sonnet-5-5" },
        ],
        escalation: &[],
        flex: false,
        review: StepDef { mode: StepMode::Harness, cands: &[Cand { kinds: &["bedrock", "vertex"], model: "claude-sonnet-5-5" }], fallback_model: Some("") },
        analyze: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: &["mistral"], model: "mistral-large-4" }], fallback_model: Some("") },
    },
    PresetDef {
        id: "cheapest",
        label: "Maximal günstig",
        description: "GPT-6.1 Terra im Flex-Tarif (halber Preis, langsamer). Sol plant per Batch und prüft. Erfolgsquote noch nicht gemessen.",
        residency: Residency::Any,
        plan: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: OPENAI, model: "gpt-6.1-sol" }], fallback_model: None },
        execute: &[Cand { kinds: OPENAI, model: "gpt-6.1-terra" }],
        escalation: &["gpt-6.1-sol"],
        flex: true,
        review: StepDef { mode: StepMode::Harness, cands: &[], fallback_model: Some("gpt-6.1-sol") },
        analyze: StepDef { mode: StepMode::Batch, cands: &[Cand { kinds: OPENAI, model: "gpt-6.1-sol" }], fallback_model: Some("gpt-6.1-sol") },
    },
];

fn pick<'a>(
    all: &'a [Access],
    cands: &[Cand],
    residency: Residency,
    need: StepMode,
) -> Option<(&'a Access, &'static str)> {
    for c in cands {
        for k in c.kinds {
            if let Some(a) = all.iter().find(|a| {
                a.kind == *k
                    && residency.allows(a.location)
                    && match need {
                        StepMode::Batch => a.batch,
                        StepMode::Sync => a.single_shot,
                        _ => true,
                    }
            }) {
                return Some((a, c.model));
            }
        }
    }
    None
}

fn step(
    def: &StepDef,
    all: &[Access],
    residency: Residency,
    exec: &Access,
    exec_model: &str,
) -> StepRoute {
    if let Some((a, m)) = pick(all, def.cands, residency, def.mode) {
        return StepRoute {
            mode: def.mode,
            target: Some(a.target(Some(m))),
        };
    }
    match def.fallback_model {
        None => StepRoute::off(),
        // Same access as the agent loop, agentic; "" = same model as well.
        Some(m) => StepRoute {
            mode: StepMode::Harness,
            target: Some(exec.target(Some(if m.is_empty() { exec_model } else { m }))),
        },
    }
}

/// Presets resolved against the accesses of the caller. The review stays
/// agentic in every preset: the report recommends the batch review only as
/// an opt-in next to the agentic one (pass rate less stable, A/B first) –
/// it remains available through a hand-made routing.
pub fn presets(all: &[Access], cat: &PriceCatalog) -> Vec<PresetInfo> {
    PRESETS
        .iter()
        .map(|p| {
            let residency = p.residency;
            let Some((exec, exec_model)) = pick(all, p.execute, residency, StepMode::Harness)
            else {
                return PresetInfo {
                    id: p.id.into(),
                    label: p.label.into(),
                    description: p.description.into(),
                    available: false,
                    missing: Some(missing_text(p)),
                    routing: None,
                    residency,
                    estimate: vec![],
                };
            };
            let r = Routing {
                preset: Some(p.id.into()),
                plan: step(&p.plan, all, residency, exec, exec_model),
                execute: ExecuteRoute {
                    target: Some(exec.target(Some(exec_model))),
                    escalation: if exec.harness == "claude"
                        || exec.kind == "openai_api"
                        || exec.harness == "mock"
                    {
                        p.escalation.iter().map(|m| m.to_string()).collect()
                    } else {
                        vec![]
                    },
                    flex: p.flex && exec.flex,
                },
                review: step(&p.review, all, residency, exec, exec_model),
                analyze: step(&p.analyze, all, residency, exec, exec_model),
                rules: vec![],
            };
            let valid = validate(&r, residency, all, cat);
            PresetInfo {
                id: p.id.into(),
                label: p.label.into(),
                description: p.description.into(),
                available: valid.is_ok(),
                missing: valid.err(),
                estimate: crate::estimate::routing_estimate(&r, all, cat),
                routing: Some(r),
                residency,
            }
        })
        .collect()
}

fn missing_text(p: &PresetDef) -> String {
    match p.id {
        "eu" => "Dafür brauchst du einen Zugang mit Datenstandort EU/CH: Claude über AWS Bedrock oder Google Vertex (EU) oder Mistral (EU-Endpunkt).".into(),
        "cheapest" => "Dafür brauchst du einen Zugang „OpenAI API“.".into(),
        _ => "Dafür brauchst du einen Claude-Zugang (Anthropic API, Claude-Abo, Bedrock oder Vertex).".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(kind: &str, loc: DataLocation, single: bool, batch: bool) -> Access {
        let (connector_id, harness) = match kind.strip_prefix("legacy:") {
            Some(p) => (None, p.to_string()),
            None => (
                Some(Uuid::new_v4()),
                connectors::harness(kind, &serde_json::json!({})),
            ),
        };
        Access {
            connector_id,
            kind: kind.into(),
            label: kind.into(),
            harness,
            location: loc,
            single_shot: single,
            batch,
            flex: kind == "openai_api",
            wire_api: None,
        }
    }

    fn cat() -> &'static PriceCatalog {
        pricing::builtin()
    }

    #[test]
    fn presets_resolve_against_available_accesses() {
        // Only a legacy Claude subscription: balanced works without batch.
        let all = vec![acc("legacy:claude", DataLocation::Us, false, false)];
        let ps = presets(&all, cat());
        let bal = ps.iter().find(|p| p.id == "balanced").unwrap();
        assert!(bal.available, "{:?}", bal.missing);
        let r = bal.routing.as_ref().unwrap();
        assert_eq!(r.plan.mode, StepMode::Off, "no batch access → no plan step");
        assert_eq!(r.review.mode, StepMode::Harness);
        assert_eq!(
            r.execute.target.as_ref().unwrap().provider.as_deref(),
            Some("claude")
        );
        assert_eq!(r.execute.escalation, vec!["claude-opus-5-5"]);
        assert!(!ps.iter().find(|p| p.id == "eu").unwrap().available);
        let cheapest = ps.iter().find(|p| p.id == "cheapest").unwrap();
        assert!(!cheapest.available && cheapest.missing.as_ref().unwrap().contains("OpenAI"));

        // With an Anthropic API key: plan and review run as batches.
        let all = vec![acc("anthropic_api", DataLocation::Us, true, true)];
        let ps = presets(&all, cat());
        let cheap = ps.iter().find(|p| p.id == "cheap").unwrap();
        let r = cheap.routing.as_ref().unwrap();
        assert_eq!(
            (r.plan.mode, r.review.mode, r.analyze.mode),
            (StepMode::Batch, StepMode::Harness, StepMode::Batch),
            "batch review only as opt-in (report)"
        );
        assert_eq!(
            r.review.target.as_ref().unwrap().connector_id,
            all[0].connector_id
        );
        assert_eq!(
            r.execute.target.as_ref().unwrap().model.as_deref(),
            Some("claude-haiku-5-5")
        );
        assert_eq!(
            r.review.target.as_ref().unwrap().model.as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert!(!cheap.estimate.is_empty());

        // EU: Mistral only → Mistral executes and reviews (mid tier), residency EU.
        let all = vec![
            acc("anthropic_api", DataLocation::Us, true, true),
            acc("mistral", DataLocation::Eu, true, true),
        ];
        let eu = presets(&all, cat())
            .into_iter()
            .find(|p| p.id == "eu")
            .unwrap();
        assert!(eu.available, "{:?}", eu.missing);
        assert_eq!(eu.residency, Residency::Eu);
        let r = eu.routing.unwrap();
        assert_eq!(
            r.execute.target.as_ref().unwrap().model.as_deref(),
            Some("mistral-large-4")
        );
        assert_eq!(
            r.review.target.as_ref().unwrap().model.as_deref(),
            Some("mistral-large-4")
        );
        assert_eq!(r.plan.mode, StepMode::Batch);
        assert!(r.execute.escalation.is_empty());

        // Cheapest: OpenAI with Flex.
        let all = vec![acc("openai_api", DataLocation::Us, true, true)];
        let c = presets(&all, cat())
            .into_iter()
            .find(|p| p.id == "cheapest")
            .unwrap();
        assert!(c.available);
        assert!(c.routing.unwrap().execute.flex);
    }

    #[test]
    fn escalation_follows_the_chain_on_the_same_access() {
        let a = acc("anthropic_api", DataLocation::Us, true, true);
        let r = Routing {
            execute: ExecuteRoute {
                target: Some(a.target(Some("claude-haiku-5-5"))),
                escalation: vec!["claude-sonnet-5-5".into(), "claude-opus-5-5".into()],
                flex: false,
            },
            ..Default::default()
        };
        let mut t = crate::limits::tests::task();
        t.connector_id = a.connector_id;
        assert_eq!(
            escalate(Some(&r), &t),
            Some(("claude-sonnet-5-5".into(), 1))
        );
        t.escalation_level = 1;
        assert_eq!(escalate(Some(&r), &t), Some(("claude-opus-5-5".into(), 2)));
        t.escalation_level = 2;
        assert_eq!(escalate(Some(&r), &t), None, "end of the chain");
        t.escalation_level = 0;
        t.connector_id = Some(Uuid::new_v4());
        assert_eq!(escalate(Some(&r), &t), None, "other access: no escalation");
        assert_eq!(escalate(None, &t), None);
    }

    #[test]
    fn validation_enforces_residency_review_tier_and_capabilities() {
        let us = acc("anthropic_api", DataLocation::Us, true, true);
        let eu = acc("bedrock", DataLocation::Eu, false, false);
        let all = vec![us.clone(), eu.clone()];
        let mut r = Routing {
            execute: ExecuteRoute {
                target: Some(eu.target(Some("claude-sonnet-5-5"))),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(validate(&r, Residency::Eu, &all, cat()).is_ok());
        r.review.target = Some(us.target(Some("claude-sonnet-5-5")));
        let e = validate(&r, Residency::Eu, &all, cat()).unwrap_err();
        assert!(e.contains("USA") && e.contains("Review"), "{e}");
        assert!(validate(&r, Residency::Any, &all, cat()).is_ok());
        r.review = StepRoute {
            mode: StepMode::Batch,
            target: None,
        };
        assert!(validate(&r, Residency::Eu, &all, cat())
            .unwrap_err()
            .contains("kein Batch"));
        r.review = StepRoute {
            mode: StepMode::Harness,
            target: Some(eu.target(Some("claude-haiku-5-5"))),
        };
        assert!(validate(&r, Residency::Eu, &all, cat())
            .unwrap_err()
            .contains("günstigsten Stufe"));
        // Review defaults to the executor's model: a Haiku executor needs its own reviewer.
        r.review = StepRoute::default();
        r.execute.target = Some(eu.target(Some("claude-haiku-5-5")));
        assert!(validate(&r, Residency::Eu, &all, cat()).is_err());
        r.execute.target = Some(eu.target(Some("claude-sonnet-5-5")));
        r.plan = StepRoute {
            mode: StepMode::Harness,
            target: None,
        };
        assert!(validate(&r, Residency::Eu, &all, cat()).is_err());
        r.plan = StepRoute {
            mode: StepMode::Sync,
            target: Some(eu.target(Some("claude-opus-5-5"))),
        };
        assert!(validate(&r, Residency::Any, &all, cat())
            .unwrap_err()
            .contains("keine direkten"));
        // Without any model (neither step nor executor) a direct call is impossible.
        r.plan = StepRoute {
            mode: StepMode::Sync,
            target: Some(us.target(None)),
        };
        r.execute.target = Some(eu.target(None));
        assert!(validate(&r, Residency::Any, &all, cat())
            .unwrap_err()
            .contains("Modell"));
        r.execute.target = Some(eu.target(Some("claude-sonnet-5-5")));
        r.plan = StepRoute::off();
        r.analyze = StepRoute {
            mode: StepMode::Off,
            target: None,
        };
        assert!(validate(&r, Residency::Any, &all, cat()).is_err());
        r.analyze = StepRoute::default();
        r.execute.flex = true;
        assert!(validate(&r, Residency::Any, &all, cat())
            .unwrap_err()
            .contains("Flex"));
        r.execute.flex = false;
        r.execute.target = Some(Target {
            connector_id: Some(Uuid::new_v4()),
            provider: None,
            model: None,
        });
        assert!(validate(&r, Residency::Any, &all, cat())
            .unwrap_err()
            .contains("nicht gefunden"));
        r.execute.target = None;
        assert!(validate(&r, Residency::Any, &all, cat()).is_err());
    }
}
