//! WP15 model chooser UI: the explanation at a task ("Warum dieses
//! Modell?" with alternatives, override and feedback), the project card,
//! the user's preferences and the admin view.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::chooser::{
    category_label, step_label, ChooserAdminUpdate, ChooserAdminView, ChooserPrefs,
    ChooserSettings, ChooserSettingsView, FeedbackView, ReplayReport, ReplayRequest, TaskFeedback,
};
use queued_api::{Task, UpdateTask};

use crate::api;
use crate::logic::bon::decision_text;
use crate::logic::chooser::{
    alternative_row, chain_text, feature_chips, fmt_pct, headline, parse_models, quality_text,
    step_line,
};
use crate::logic::costs::fmt_money;
use crate::store::use_store;
use crate::ui::{HelpTip, Ic, Icon};

const HELP_CHOOSER: &str = "Die automatische Modellwahl sucht für jeden Auftrag unter deinen Zugängen das Modell mit den geringsten erwarteten Kosten pro erledigtem Auftrag – Fehlversuche und Nachbesserungen eingerechnet. Datenstandort, Sperren, Budgets und Kostenlimits gelten immer. Sie lernt aus den Ergebnissen deiner Aufträge.";
const HELP_QUALITY: &str = "Links: so günstig wie möglich (Fehlversuche werden in Kauf genommen und nachgebessert). Rechts: lieber ein stärkeres Modell, das beim ersten Versuch trifft.";
const HELP_EXPLORE: &str = "Damit die Modellwahl dazulernt, probiert sie ab und zu ein wenig erprobtes Modell aus – nie bei riskanten Aufträgen und höchstens für diesen Betrag pro 30 Tage.";
const HELP_CLASSIFY: &str = "Ein sehr günstiges Modell liest den Auftrag und ordnet ihn ein (Art, Umfang, Risiko). Das kostet meist weniger als 0,001 $ und wird am Auftrag ausgewiesen. Ohne diese Option entscheidet die Modellwahl nur anhand fester Regeln.";
const HELP_SHARE: &str = "Wenn du zustimmst und der Betreiber es erlaubt, fließen deine Ergebnisse zusammengefasst (nur Zählwerte je Aufgabenart und Modell, nie Texte) in die gemeinsamen Startwerte ein. Gezählt wird erst, wenn mindestens drei Personen dieselbe Kombination beitragen.";

/// "Warum dieses Modell?" – the routing decision, and when the model
/// chooser decided: its reasons, costs, steps, chain and alternatives.
#[component]
pub fn DecisionPanel(t: Task) -> impl IntoView {
    let Some(d) = t.routing_decision_parsed() else {
        return ().into_any();
    };
    let store = use_store();
    let id = t.id;
    let mine = store.me.with_untracked(|u| u.id == t.user_id);
    let waiting = matches!(
        t.status.as_str(),
        "queued" | "pending_approval" | "waiting_input"
    ) && t.attempts <= 1;
    let finished = matches!(
        t.status.as_str(),
        "done" | "failed" | "blocked" | "candidate"
    );
    let busy = RwSignal::new(false);
    let line = decision_text(&d);
    let report = d.chooser.clone().map(|b| *b);
    let use_alt = move |target: queued_api::Target| {
        busy.set(true);
        spawn_local(async move {
            let body = UpdateTask {
                connector_id: Some(target.connector_id),
                provider: target.provider.clone(),
                model: Some(target.model.clone().unwrap_or_default()),
                ..Default::default()
            };
            match api::patch_ok(&format!("/tasks/{id}"), &body).await {
                Ok(()) => store.toast(format!(
                    "Modell geändert: {}",
                    target.model.as_deref().unwrap_or("Standardmodell")
                )),
                Err(e) => store.toast_error("Ändern fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };
    view! {
        <section class="block chooser" data-testid="routing-decision">
            <h3 class="row"><Icon icon=Ic::Route class="sm"/>"Warum dieses Modell?"
                {report.is_some().then(|| view! { <HelpTip topic="Modellwahl" text=HELP_CHOOSER align="align-start"/> })}
            </h3>
            <p class="box small">{line}</p>
            {report.map(|r| {
                let chips = feature_chips(&r.features);
                let steps: Vec<String> = [&r.plan, &r.review, &r.analyze]
                    .into_iter()
                    .flatten()
                    .map(step_line)
                    .collect();
                let chain = chain_text(&r);
                let alts = r.alternatives.clone();
                let n_alts = alts.len();
                view! {
                    <div class="chooser-report" data-testid="chooser-report">
                        <p class="chooser-head"><strong>{headline(&r)}</strong></p>
                        <ul class="chips-row" aria-label="Merkmale des Auftrags">
                            {chips.into_iter().map(|(text, tone)| view! { <li class=format!("tag {tone}")>{text}</li> }).collect_view()}
                        </ul>
                        {r.chosen.as_ref().map(|c| view! {
                            <dl class="chooser-facts">
                                <div><dt>"Pro Versuch"</dt><dd data-testid="chooser-attempt-cost">{format!("ca. {}", fmt_money(c.attempt_cost, "USD"))}</dd></div>
                                <div><dt>"Pro erledigtem Auftrag"</dt><dd data-testid="chooser-cpa">{format!("ca. {}", fmt_money(c.cost_per_accepted, "USD"))}</dd></div>
                                <div><dt>"Erfolg im 1. Versuch"</dt><dd>{fmt_pct(c.p_success)}</dd></div>
                                <div><dt>"Erfolg mit Nachbesserung"</dt><dd>{fmt_pct(c.p_accept)}</dd></div>
                            </dl>
                        })}
                        <ul class="chooser-reasons small">
                            {r.reasons.iter().skip(1).map(|x| view! { <li>{x.clone()}</li> }).collect_view()}
                        </ul>
                        {(!steps.is_empty()).then(|| view! {
                            <div class="small">
                                <strong>"Schritte"</strong>
                                <ul class="chooser-reasons">{steps.into_iter().map(|s| view! { <li>{s}</li> }).collect_view()}</ul>
                            </div>
                        })}
                        {chain.map(|c| view! { <p class="small"><strong>"Falls es scheitert: "</strong>{c}</p> })}
                        {r.flex.then(|| view! { <p class="small">"Umsetzung im Flex-Tarif (halber Preis, etwas langsamer) – der Auftrag ist nicht eilig."</p> })}
                        {r.explored.then(|| view! { <p class="small chip tone-warning" data-testid="chooser-explored">{format!("Ausprobiert, um dazuzulernen (Einsatz höchstens ca. {})", fmt_money(r.explore_cost, "USD"))}</p> })}
                        {r.fallback.clone().map(|f| view! { <p class="small chip tone-warning" data-testid="chooser-fallback">{f}</p> })}
                        {r.overridden.clone().map(|o| view! { <p class="small" data-testid="chooser-overridden">{format!("Von Hand geändert – automatisch gewählt war: {}", o.from_label)}</p> })}
                        {(n_alts > 0).then(|| view! {
                            <details class="block" data-testid="chooser-alternatives">
                                <summary><Icon icon=Ic::ChevronRight class="sm chev"/>{format!("Alternativen ({n_alts})")}</summary>
                                <div class="table-wrap" tabindex="0" role="region" aria-label="Alternativen der Modellwahl als Tabelle">
                                    <table class="table compact">
                                        <caption class="visually-hidden">"Alternativen der Modellwahl"</caption>
                                        <thead><tr>
                                            <th scope="col">"Modell"</th><th scope="col">"Zugang"</th><th scope="col">"Erfolg 1. Versuch"</th>
                                            <th scope="col">"Pro Versuch"</th><th scope="col">"Pro erledigtem Auftrag"</th><th scope="col">"Hinweis"</th>
                                            {(waiting && mine).then(|| view! { <th scope="col"><span class="visually-hidden">"Aktion"</span></th> })}
                                        </tr></thead>
                                        <tbody>
                                            {alts.into_iter().map(|a| {
                                                let cells = alternative_row(&a);
                                                let allowed = a.excluded.is_none();
                                                let target = a.target.clone();
                                                let model = a.model.clone();
                                                view! {
                                                    <tr class=if allowed { "" } else { "muted" }>
                                                        <th scope="row">{cells[0].clone()}</th>
                                                        <td>{cells[1].clone()}</td><td>{cells[2].clone()}</td><td>{cells[3].clone()}</td>
                                                        <td>{cells[4].clone()}</td><td class="small">{cells[5].clone()}</td>
                                                        {(waiting && mine).then(|| view! {
                                                            <td>{allowed.then(|| view! {
                                                                <button class="btn sm" type="button" disabled=move || busy.get()
                                                                    aria-label=format!("Stattdessen {model} verwenden")
                                                                    on:click=move |_| use_alt(target.clone())>"Verwenden"</button>
                                                            })}</td>
                                                        })}
                                                    </tr>
                                                }
                                            }).collect_view()}
                                        </tbody>
                                    </table>
                                </div>
                            </details>
                        })}
                    </div>
                }
            })}
            {(finished && mine && t.model.is_some()).then(|| view! { <Feedback id=id/> })}
        </section>
    }
    .into_any()
}

/// "War das Ergebnis gut?" – feeds the chooser's learning.
#[component]
fn Feedback(id: uuid::Uuid) -> impl IntoView {
    let store = use_store();
    let current = RwSignal::new(None::<bool>);
    let busy = RwSignal::new(false);
    spawn_local(async move {
        if let Ok(f) = api::get::<FeedbackView>(&format!("/tasks/{id}/feedback")).await {
            current.set(f.good);
        }
    });
    let send = move |good: bool| {
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, FeedbackView>(
                &format!("/tasks/{id}/feedback"),
                &TaskFeedback { good },
            )
            .await
            {
                Ok(f) => {
                    current.set(f.good);
                    store.toast("Danke – die Modellwahl lernt daraus");
                }
                Err(e) => store.toast_error("Rückmeldung nicht gespeichert", &e),
            }
            busy.set(false);
        });
    };
    view! {
        <div class="row wrap feedback" role="group" aria-label="War das Ergebnis gut?" data-testid="chooser-feedback">
            <span class="small">"War das Ergebnis gut?"</span>
            <button class="btn sm" type="button" aria-pressed=move || (current.get() == Some(true)).to_string()
                disabled=move || busy.get() on:click=move |_| send(true)>
                <Icon icon=Ic::Check class="sm"/>"Gut"
            </button>
            <button class="btn sm" type="button" aria-pressed=move || (current.get() == Some(false)).to_string()
                disabled=move || busy.get() on:click=move |_| send(false)>
                <Icon icon=Ic::X class="sm"/>"Schlecht"
            </button>
        </div>
    }
}

/// Project settings: automatic model choice.
#[component]
pub fn ChooserCard(p: queued_api::ProjectView) -> impl IntoView {
    let store = use_store();
    let id = p.id;
    let view_sig = RwSignal::new(None::<ChooserSettingsView>);
    let (enabled, quality, budget, classify, blocked) = (
        RwSignal::new(false),
        RwSignal::new(50u8),
        RwSignal::new(String::new()),
        RwSignal::new(false),
        RwSignal::new(String::new()),
    );
    let busy = RwSignal::new(false);
    let fill = move |s: &ChooserSettings| {
        enabled.set(s.enabled);
        quality.set(s.quality);
        budget.set(format!("{:.2}", s.explore_budget_usd));
        classify.set(s.classify);
        blocked.set(s.blocked_models.join(", "));
    };
    let load = move || {
        spawn_local(async move {
            if let Ok(v) = api::get::<ChooserSettingsView>(&format!("/projects/{id}/chooser")).await
            {
                fill(&v.settings);
                view_sig.set(Some(v));
            }
        });
    };
    load();
    let save = move |_| {
        let b = budget.get_untracked().replace(',', ".");
        let Ok(budget_usd) = b.trim().parse::<f64>() else {
            store.toast_error_text(
                "Nicht gespeichert",
                "Budget zum Ausprobieren: bitte eine Zahl in $ angeben",
            );
            return;
        };
        let body = ChooserSettings {
            enabled: enabled.get_untracked(),
            quality: quality.get_untracked(),
            explore_budget_usd: budget_usd,
            classify: classify.get_untracked(),
            blocked_models: parse_models(&blocked.get_untracked()),
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, ChooserSettings>(&format!("/projects/{id}/chooser"), &body).await {
                Ok(_) => {
                    store.toast("Automatische Modellwahl gespeichert");
                    load();
                }
                Err(e) => store.toast_error("Nicht gespeichert", &e),
            }
            busy.set(false);
        });
    };
    let ids = (
        crate::ui::uid("ch-quality"),
        crate::ui::uid("ch-budget"),
        crate::ui::uid("ch-blocked"),
    );
    view! {
        <section class="card settings-card" style="margin-top: var(--sp-4)" aria-labelledby="chooser-title" data-testid="chooser-card">
            <header>
                <span class="s-icon"><Icon icon=Ic::Sparkles/></span>
                <div>
                    <h2 id="chooser-title" class="row">"Automatische Modellwahl" <HelpTip topic="Modellwahl" text=HELP_CHOOSER align="align-start"/></h2>
                    <p>"Queue wählt für jeden Auftrag das passende Modell aus deinen Zugängen – günstig, wo es reicht, stark, wo es nötig ist. Regeln oben haben Vorrang."</p>
                </div>
            </header>
            {move || view_sig.get().map(|v| {
                let can_edit = v.can_edit;
                let ids = ids.clone();
                view! {
                    <label class="check">
                        <input type="checkbox" data-testid="chooser-enabled" disabled=!can_edit prop:checked=move || enabled.get() on:change=move |ev| enabled.set(event_target_checked(&ev))/>
                        <span class="check-text"><span>"Modell automatisch wählen"</span><small>"Gilt für neue Aufträge ohne eigene Modellwahl."</small></span>
                    </label>
                    <div class="field">
                        <label class="label row" for=ids.0.clone()>"Kosten ↔ Qualität" <HelpTip topic="Regler" text=HELP_QUALITY align="align-start"/></label>
                        <input id=ids.0.clone() class="range" type="range" min="0" max="100" step="10" data-testid="chooser-quality" disabled=!can_edit
                            aria-valuetext=move || format!("{} von 100: {}", quality.get(), quality_text(quality.get()))
                            prop:value=move || quality.get().to_string()
                            on:input=move |ev| quality.set(event_target_value(&ev).parse().unwrap_or(50))/>
                        <p class="small muted" aria-hidden="true">{move || format!("{} von 100 – {}", quality.get(), quality_text(quality.get()))}</p>
                    </div>
                    <div class="grid-3">
                        <div class="field">
                            <label class="label row" for=ids.1.clone()>"Budget zum Ausprobieren ($ / 30 Tage)" <HelpTip topic="Ausprobieren" text=HELP_EXPLORE align="align-start"/></label>
                            <input id=ids.1.clone() class="input" inputmode="decimal" disabled=!can_edit bind:value=budget/>
                            <p class="small muted">{format!("Bisher genutzt: {}", fmt_money(v.explore_spent_usd, "USD"))}</p>
                        </div>
                        <div class="field">
                            <label class="label" for=ids.2.clone()>"Nie verwenden (Modelle, Komma)"</label>
                            <input id=ids.2.clone() class="input" placeholder="z. B. claude-haiku-5-5" disabled=!can_edit bind:value=blocked/>
                        </div>
                    </div>
                    <label class="check">
                        <input type="checkbox" data-testid="chooser-classify" disabled=!can_edit prop:checked=move || classify.get() on:change=move |ev| classify.set(event_target_checked(&ev))/>
                        <span class="check-text">
                            <span class="row">"Auftrag zusätzlich von einem günstigen Modell einordnen lassen" <HelpTip topic="Einordnung" text=HELP_CLASSIFY align="align-start"/></span>
                            <small>{match v.classify_model.clone() {
                                Some(m) => format!("Würde {m} nutzen."),
                                None => "Kein günstiger Zugang mit API-Schlüssel vorhanden – dann nur feste Regeln.".to_string(),
                            }}</small>
                        </span>
                    </label>
                    {can_edit.then(|| view! {
                        <button class="btn primary" type="button" data-testid="chooser-save" disabled=move || busy.get() on:click=save>
                            <Icon icon=Ic::Check class="sm"/>"Modellwahl sichern"
                        </button>
                    })}
                }
            })}
        </section>
    }
}

/// Account settings: share learning data, blocked models.
#[component]
pub fn ChooserPrefsCard() -> impl IntoView {
    let store = use_store();
    let prefs = RwSignal::new(None::<ChooserPrefs>);
    let blocked = RwSignal::new(String::new());
    let share = RwSignal::new(false);
    let busy = RwSignal::new(false);
    spawn_local(async move {
        if let Ok(p) = api::get::<ChooserPrefs>("/chooser/prefs").await {
            share.set(p.share_learning);
            blocked.set(p.blocked_models.join(", "));
            prefs.set(Some(p));
        }
    });
    let save = move |_| {
        let body = ChooserPrefs {
            share_learning: share.get_untracked(),
            blocked_models: parse_models(&blocked.get_untracked()),
            pooling_enabled: false,
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, ChooserPrefs>("/chooser/prefs", &body).await {
                Ok(p) => {
                    prefs.set(Some(p));
                    store.toast("Einstellungen der Modellwahl gespeichert");
                }
                Err(e) => store.toast_error("Nicht gespeichert", &e),
            }
            busy.set(false);
        });
    };
    let bid = crate::ui::uid("prefs-blocked");
    view! {
        <section class="card settings-card" aria-labelledby="prefs-chooser-title" data-testid="chooser-prefs">
            <header>
                <span class="s-icon"><Icon icon=Ic::Sparkles/></span>
                <div>
                    <h2 id="prefs-chooser-title">"Automatische Modellwahl"</h2>
                    <p>"Was die Modellwahl aus deinen Aufträgen lernt, bleibt bei dir – außer du teilst es."</p>
                </div>
            </header>
            {move || prefs.get().map(|p| view! {
                <label class="check">
                    <input type="checkbox" data-testid="chooser-share" prop:checked=move || share.get() on:change=move |ev| share.set(event_target_checked(&ev))/>
                    <span class="check-text">
                        <span class="row">"Meine Lerndaten zusammengefasst teilen" <HelpTip topic="Teilen" text=HELP_SHARE align="align-start"/></span>
                        <small>{if p.pooling_enabled { "Der Betreiber erlaubt das Teilen." } else { "Der Betreiber hat das Teilen derzeit ausgeschaltet – deine Wahl gilt, sobald er es erlaubt." }}</small>
                    </span>
                </label>
                <div class="field">
                    <label class="label" for=bid.clone()>"Modelle, die ich nie nutzen will (Komma)"</label>
                    <input id=bid.clone() class="input" placeholder="z. B. gpt-6-luna" bind:value=blocked/>
                </div>
                <button class="btn" type="button" data-testid="chooser-prefs-save" disabled=move || busy.get() on:click=save>
                    <Icon icon=Ic::Check class="sm"/>"Sichern"
                </button>
            })}
        </section>
    }
}

/// Admin: statistics, pooling, half-life and the offline replay.
#[component]
pub fn AdminChooser() -> impl IntoView {
    let store = use_store();
    let data = RwSignal::new(None::<ChooserAdminView>);
    let pooling = RwSignal::new(false);
    let half = RwSignal::new(String::new());
    let replay = RwSignal::new(None::<ReplayReport>);
    let busy = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            if let Ok(v) = api::get::<ChooserAdminView>("/admin/chooser").await {
                pooling.set(v.pooling);
                half.set(v.half_life_days.to_string());
                data.set(Some(v));
            }
        });
    };
    load();
    let save = move |_| {
        let Ok(days) = half.get_untracked().trim().parse::<i32>() else {
            store.toast_error_text(
                "Nicht gespeichert",
                "Halbwertszeit: bitte Tage als Zahl angeben",
            );
            return;
        };
        let body = ChooserAdminUpdate {
            pooling: pooling.get_untracked(),
            half_life_days: days,
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, ChooserAdminView>("/admin/chooser", &body).await {
                Ok(v) => {
                    data.set(Some(v));
                    store.toast("Modellwahl-Einstellungen gespeichert");
                }
                Err(e) => store.toast_error("Nicht gespeichert", &e),
            }
            busy.set(false);
        });
    };
    let run = move |source: &'static str| {
        busy.set(true);
        spawn_local(async move {
            let body = ReplayRequest {
                source: source.into(),
                tasks: 600,
                seed: 2026,
            };
            match api::post::<_, ReplayReport>("/admin/chooser/replay", &body).await {
                Ok(r) => replay.set(Some(r)),
                Err(e) => store.toast_error("Replay fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };
    let hid = crate::ui::uid("ch-half");
    view! {
        <section class="card settings-card" aria-labelledby="admin-chooser-title" data-testid="admin-chooser">
            <header>
                <span class="s-icon"><Icon icon=Ic::Sparkles/></span>
                <div>
                    <h2 id="admin-chooser-title">"Automatische Modellwahl"</h2>
                    <p>"Was die Modellwahl entscheidet und lernt – zusammengefasst, ohne Auftragstexte."</p>
                </div>
            </header>
            {move || data.get().map(|v| {
                let decisions = v.decisions.iter().map(|d| format!("{}: {}", match d.label.as_str() {
                    "chooser" => "automatisch", "fallback" => "Rückfall auf Vorlage", "override" => "von Hand geändert", x => x,
                }, d.count)).collect::<Vec<_>>().join(" · ");
                view! {
                    <dl class="chooser-facts">
                        <div><dt>"Projekte mit Modellwahl"</dt><dd>{v.projects_enabled}</dd></div>
                        <div><dt>"Entscheidungen (30 Tage)"</dt><dd>{if decisions.is_empty() { "keine".to_string() } else { decisions }}</dd></div>
                        <div><dt>"Ausprobiert"</dt><dd>{format!("{} × (ca. {})", v.explored, fmt_money(v.explore_cost_usd, "USD"))}</dd></div>
                        <div><dt>"Einordnungen per Modell"</dt><dd>{format!("{} × (zusammen {})", v.classify_calls, fmt_money(v.classify_cost_usd, "USD"))}</dd></div>
                        <div><dt>"Erwartet pro erledigtem Auftrag"</dt><dd>{format!("ca. {}", fmt_money(v.expected_cost_usd, "USD"))}</dd></div>
                        <div><dt>"Personen, die teilen"</dt><dd>{v.sharing_users}</dd></div>
                    </dl>
                    {(!v.fallback_reasons.is_empty()).then(|| view! {
                        <div class="small"><strong>"Häufigste Gründe für einen Rückfall"</strong>
                            <ul class="chooser-reasons">{v.fallback_reasons.iter().map(|r| view! { <li>{format!("{} ({}×)", r.label, r.count)}</li> }).collect_view()}</ul>
                        </div>
                    })}
                    <div class="table-wrap" tabindex="0" role="region" aria-label="Gelernte Erfolgsquoten als Tabelle">
                        <table class="table compact" data-testid="chooser-cells">
                            <caption class="visually-hidden">"Gelernte Erfolgsquoten je Schritt, Aufgabenart und Modell"</caption>
                            <thead><tr><th scope="col">"Schritt"</th><th scope="col">"Art"</th><th scope="col">"Modell"</th><th scope="col">"Ergebnisse"</th><th scope="col">"Erfolg"</th><th scope="col">"Ø Kosten"</th><th scope="col">"Personen"</th></tr></thead>
                            <tbody>
                                {if v.cells.is_empty() {
                                    view! { <tr><td colspan="7" class="small muted">"Noch keine Ergebnisse gelernt."</td></tr> }.into_any()
                                } else {
                                    v.cells.iter().map(|c| view! {
                                        <tr><td>{step_label(&c.step)}</td><td>{category_label(&c.category)}</td><th scope="row">{c.model.clone()}</th>
                                            <td>{format!("{:.1}", c.observations)}</td><td>{fmt_pct(c.success_rate)}</td><td>{fmt_money(c.avg_cost, "USD")}</td><td>{c.users}</td></tr>
                                    }).collect_view().into_any()
                                }}
                            </tbody>
                        </table>
                    </div>
                }
            })}
            <div class="form-section">
                <label class="check">
                    <input type="checkbox" data-testid="chooser-pooling" prop:checked=move || pooling.get() on:change=move |ev| pooling.set(event_target_checked(&ev))/>
                    <span class="check-text"><span>"Gemeinsame Startwerte aus geteilten Lerndaten"</span>
                        <small>"Nur von Personen, die selbst zugestimmt haben; nur Zählwerte, erst ab drei Personen je Kombination, nie stärker als 20 eigene Ergebnisse."</small></span>
                </label>
                <div class="field">
                    <label class="label" for=hid.clone()>"Halbwertszeit alter Ergebnisse (Tage, 7–365)"</label>
                    <input id=hid.clone() class="input" inputmode="numeric" bind:value=half/>
                </div>
                <button class="btn" type="button" disabled=move || busy.get() on:click=save><Icon icon=Ic::Check class="sm"/>"Sichern"</button>
            </div>
            <div class="form-section">
                <header><h3>"Vergleich mit den Vorlagen (Replay)"</h3>
                    <p>"Spielt Aufträge mit jeder Vorlage und mit der automatischen Modellwahl durch und vergleicht die Kosten pro erledigtem Auftrag."</p></header>
                <div class="row wrap">
                    <button class="btn" type="button" data-testid="replay-synthetic" disabled=move || busy.get() on:click=move |_| run("synthetic")>"Mit erzeugten Aufträgen"</button>
                    <button class="btn" type="button" data-testid="replay-history" disabled=move || busy.get() on:click=move |_| run("history")>"Mit der eigenen Historie"</button>
                </div>
                {move || replay.get().map(|r| view! {
                    <p class="small muted">{r.basis.clone()}</p>
                    <div class="table-wrap" tabindex="0" role="region" aria-label="Ergebnis des Replays als Tabelle">
                        <table class="table compact" data-testid="replay-table">
                            <caption class="visually-hidden">"Ergebnis des Replays"</caption>
                            <thead><tr><th scope="col">"Verfahren"</th><th scope="col">"Erledigt"</th><th scope="col">"Im 1. Versuch"</th><th scope="col">"Kosten gesamt"</th><th scope="col">"Pro erledigtem Auftrag"</th></tr></thead>
                            <tbody>{r.policies.iter().map(|p| view! {
                                <tr><th scope="row">{p.label.clone()}</th><td>{fmt_pct(p.acceptance_rate)}</td><td>{fmt_pct(p.first_pass_rate)}</td>
                                    <td>{fmt_money(p.total_cost, "USD")}</td><td>{fmt_money(p.cost_per_accepted, "USD")}</td></tr>
                            }).collect_view()}</tbody>
                        </table>
                    </div>
                    <p class=if r.passed { "small chip tone-success" } else { "small chip tone-warning" } data-testid="replay-verdict">{r.verdict.clone()}</p>
                })}
            </div>
        </section>
    }
}
