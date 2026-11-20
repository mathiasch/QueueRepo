//! WP10: Auswertung – success on the first attempt, cost, cache, duration
//! and pauses, per own tasks or per project. Every chart has a text
//! summary (`role="img"`) and a table with all values.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::analytics::{
    fmt_duration, fmt_percent, pause_reason_label, Analytics, DayRow, CACHE_HELP, FIRST_PASS_HELP,
    PAUSE_HELP,
};
use queued_api::TaskStatus;

use crate::api;
use crate::logic::analytics::{
    bar_pct, cost_chart_summary, day_label, is_empty, model_label, query, tasks_chart_summary,
};
use crate::logic::costs::{fmt_money, fmt_money_list};
use crate::logic::format::fmt_tokens;
use crate::logic::labels::provider_label;
use crate::nav::{set_title, use_open_task};
use crate::store::use_store;
use crate::ui::{uid, Empty, HelpTip, Ic, Icon, StatusPill};

const WINDOWS: [(i32, &str); 3] = [(7, "7 Tage"), (30, "30 Tage"), (90, "90 Tage")];

#[component]
pub fn AnalyticsPage() -> impl IntoView {
    set_title("Auswertung");
    let store = use_store();
    let days = RwSignal::new(30);
    let scope = RwSignal::new(String::new());
    let data = RwSignal::new(None::<Result<Analytics, String>>);
    let tz = super::automation::browser_tz();
    Effect::new(move |_| {
        let (d, s) = (days.get(), scope.get());
        let path = query(d, Some(s.as_str()).filter(|s| !s.is_empty()), &tz);
        data.set(None);
        spawn_local(async move {
            data.set(Some(
                api::get::<Analytics>(&path).await.map_err(|e| e.message),
            ));
        });
    });
    let range_name = uid("range");
    let scope_id = uid("scope");
    view! {
        <div class="page-head">
            <div>
                <h1>"Auswertung"</h1>
                <p>"Wie gut und wie günstig die Agenten arbeiten – in Zahlen, die man versteht."</p>
            </div>
        </div>
        <div class="card pad analytics-filters" role="group" aria-label="Auswahl">
            <div class="segmented" role="radiogroup" aria-label="Zeitraum">
                {WINDOWS.into_iter().map(|(d, label)| {
                    let name = range_name.clone();
                    view! {
                        <label class="seg-radio">
                            <input type="radio" name=name value=d.to_string() prop:checked=move || days.get() == d on:change=move |_| days.set(d)/>
                            <span>{label}</span>
                        </label>
                    }
                }).collect_view()}
            </div>
            <div class="field analytics-scope">
                <label class="label" for=scope_id.clone()>"Bereich"</label>
                <select id=scope_id class="select" prop:value=move || scope.get() on:change=move |ev| scope.set(event_target_value(&ev))>
                    <option value="">"Meine Aufträge"</option>
                    {move || store.projects.get().into_iter().map(|p| view! {
                        <option value=p.id.to_string()>{format!("Projekt: {}", p.name)}</option>
                    }).collect_view()}
                </select>
            </div>
        </div>
        {move || match data.get() {
            None => view! { <p class="muted" role="status">"Lade Auswertung …"</p> }.into_any(),
            Some(Err(e)) => view! { <p class="alert tone-warning" role="alert">{e}</p> }.into_any(),
            Some(Ok(a)) if is_empty(&a) => view! {
                <div class="card">
                    <Empty icon=Ic::Chart title="Noch keine Daten" text="In diesem Zeitraum wurde nichts erledigt und nichts verbraucht. Sobald Aufträge laufen, erscheinen hier die Zahlen."/>
                </div>
            }.into_any(),
            Some(Ok(a)) => view! { <Dashboard a=a/> }.into_any(),
        }}
    }
}

#[component]
fn Kpi(
    title: &'static str,
    #[prop(into)] value: String,
    #[prop(into)] detail: String,
    #[prop(optional)] help: Option<&'static str>,
    testid: &'static str,
) -> impl IntoView {
    view! {
        <div class="kpi" data-testid=testid>
            <div class="kpi-title">
                <span>{title}</span>
                {help.map(|h| view! { <HelpTip topic=title text=h/> })}
            </div>
            <div class="kpi-value">{value}</div>
            <div class="kpi-detail small muted">{detail}</div>
        </div>
    }
}

#[component]
fn Dashboard(a: Analytics) -> impl IntoView {
    let o = a.outcomes.clone();
    let first = fmt_percent(o.first_pass_rate());
    let first_detail = if o.first_pass_total > 0 {
        format!(
            "{} von {} ohne Reparatur · am Ende erledigt: {}",
            o.first_pass_done,
            o.first_pass_total,
            fmt_percent(o.eventual_rate())
        )
    } else {
        "Noch keine beendeten Aufträge".into()
    };
    let per_done = if a.cost_per_done.is_empty() {
        "Kein erledigter Auftrag".to_string()
    } else {
        format!(
            "{} pro erledigtem Auftrag",
            fmt_money_list(&a.cost_per_done)
        )
    };
    let d = a.durations.clone();
    let dur = d
        .to_done_median
        .map(fmt_duration)
        .unwrap_or_else(|| "–".into());
    let dur_detail = match (d.to_done_p90, d.agent_median) {
        (Some(p90), Some(ag)) => format!(
            "9 von 10 schneller als {} · davon Agent: {}",
            fmt_duration(p90),
            fmt_duration(ag)
        ),
        (Some(p90), None) => format!("9 von 10 schneller als {}", fmt_duration(p90)),
        _ => "Noch nichts erledigt".into(),
    };
    let pause_total: Option<i64> = a.pauses.as_ref().map(|p| p.iter().map(|r| r.minutes).sum());
    view! {
        <section class="kpis" aria-label="Kennzahlen">
            <Kpi title="Beim ersten Versuch erledigt" value=first detail=first_detail help=FIRST_PASS_HELP testid="kpi-first-pass"/>
            <Kpi title="Kosten" value=fmt_money_list(&a.costs) detail=per_done testid="kpi-costs"/>
            <Kpi title="Cache-Treffer" value=fmt_percent(a.tokens.cache_ratio())
                detail=format!("{} Tokens insgesamt", fmt_tokens(a.tokens.total())) help=CACHE_HELP testid="kpi-cache"/>
            <Kpi title="Dauer bis erledigt" value=dur detail=dur_detail testid="kpi-duration"/>
            {pause_total.map(|m| view! {
                <Kpi title="Pausen durch Limits" value=format!("{m} min") detail="Warteschlange angehalten" help=PAUSE_HELP testid="kpi-pauses"/>
            })}
        </section>
        <div class="analytics-grid">
            <TasksChart days=a.daily.clone()/>
            <CostChart days=a.daily.clone()/>
            <ProjectTable a=a.clone()/>
            <ProviderTable a=a.clone()/>
            <TopTasks a=a.clone()/>
            {a.pauses.clone().map(|p| view! { <PauseTable rows=p/> })}
        </div>
    }
}

fn dense(n: usize) -> &'static str {
    if n > 31 {
        "bars dense"
    } else {
        "bars"
    }
}

#[component]
fn TasksChart(days: Vec<DayRow>) -> impl IntoView {
    let max = days
        .iter()
        .map(|d| (d.done + d.failed + d.blocked) as f64)
        .fold(0.0, f64::max);
    let summary = tasks_chart_summary(&days);
    let title = uid("chart-tasks");
    let labelled = title.clone();
    view! {
        <section class="card pad chart-card" aria-labelledby=labelled data-testid="chart-tasks">
            <h2 id=title>"Aufträge pro Tag"</h2>
            <div class="chart-legend" aria-hidden="true">
                <span><i class="swatch s1"></i>"Erledigt"</span>
                <span><i class="swatch s2"></i>"Nicht erledigt (fehlgeschlagen, blockiert)"</span>
            </div>
            <div class=dense(days.len()) role="img" aria-label=summary>
                {days.iter().map(|d| {
                    let bad = d.failed + d.blocked;
                    let tip = format!("{}: {} erledigt, {} nicht erledigt", day_label(d), d.done, bad);
                    view! {
                        <div class="bar-col" title=tip>
                            <div class="bar-stack" style=format!("height: {}%", bar_pct((d.done + bad) as f64, max))>
                                {(bad > 0).then(|| view! { <div class="seg s2" style=format!("flex-grow: {bad}")></div> })}
                                {(d.done > 0).then(|| view! { <div class="seg s1" style=format!("flex-grow: {}", d.done)></div> })}
                            </div>
                        </div>
                    }
                }).collect_view()}
            </div>
            <div class="chart-axis small muted" aria-hidden="true">
                <span>{days.first().map(day_label)}</span><span>{days.last().map(day_label)}</span>
            </div>
            <details class="chart-table">
                <summary>"Als Tabelle anzeigen"</summary>
                <table class="table compact" tabindex="0">
                    <caption class="visually-hidden">"Aufträge pro Tag"</caption>
                    <thead><tr><th scope="col">"Tag"</th><th scope="col">"Erledigt"</th><th scope="col">"Fehlgeschlagen"</th><th scope="col">"Blockiert"</th></tr></thead>
                    <tbody>
                        {days.iter().rev().map(|d| view! {
                            <tr><th scope="row">{day_label(d)}</th><td>{d.done}</td><td>{d.failed}</td><td>{d.blocked}</td></tr>
                        }).collect_view()}
                    </tbody>
                </table>
            </details>
        </section>
    }
}

#[component]
fn CostChart(days: Vec<DayRow>) -> impl IntoView {
    let max = days.iter().map(|d| d.cost_usd).fold(0.0, f64::max);
    let summary = cost_chart_summary(&days);
    let title = uid("chart-cost");
    let labelled = title.clone();
    view! {
        <section class="card pad chart-card" aria-labelledby=labelled data-testid="chart-cost">
            <h2 id=title>"Kosten pro Tag (Dollar)"</h2>
            <div class=dense(days.len()) role="img" aria-label=summary>
                {days.iter().map(|d| {
                    let tip = format!("{}: {}", day_label(d), fmt_money(d.cost_usd, "USD"));
                    view! {
                        <div class="bar-col" title=tip>
                            <div class="bar-stack" style=format!("height: {}%", bar_pct(d.cost_usd, max))>
                                {(d.cost_usd > 0.0).then(|| view! { <div class="seg s1" style="flex-grow: 1"></div> })}
                            </div>
                        </div>
                    }
                }).collect_view()}
            </div>
            <div class="chart-axis small muted" aria-hidden="true">
                <span>{days.first().map(day_label)}</span><span>{days.last().map(day_label)}</span>
            </div>
            <details class="chart-table">
                <summary>"Als Tabelle anzeigen"</summary>
                <table class="table compact" tabindex="0">
                    <caption class="visually-hidden">"Kosten pro Tag"</caption>
                    <thead><tr><th scope="col">"Tag"</th><th scope="col">"Kosten"</th></tr></thead>
                    <tbody>
                        {days.iter().rev().map(|d| view! {
                            <tr><th scope="row">{day_label(d)}</th><td>{fmt_money(d.cost_usd, "USD")}</td></tr>
                        }).collect_view()}
                    </tbody>
                </table>
            </details>
            <p class="small muted">"Andere Währungen (z. B. CHF) stehen in der Summe oben."</p>
        </section>
    }
}

#[component]
fn ProjectTable(a: Analytics) -> impl IntoView {
    let title = uid("by-project");
    let labelled = title.clone();
    let max = a
        .by_project
        .iter()
        .map(|p| p.tokens.total() as f64)
        .fold(0.0, f64::max);
    view! {
        <section class="card pad" aria-labelledby=labelled data-testid="by-project">
            <h2 id=title>"Nach Projekt"</h2>
            {if a.by_project.is_empty() {
                view! { <p class="small muted">"Keine Projekte mit Aktivität."</p> }.into_any()
            } else {
                view! {
                    <table class="table compact hbar-table" tabindex="0">
                        <caption class="visually-hidden">"Aufträge, Tokens und Kosten nach Projekt"</caption>
                        <thead><tr><th scope="col">"Projekt"</th><th scope="col">"Erledigt"</th><th scope="col">"Tokens"</th><th scope="col">"Kosten"</th></tr></thead>
                        <tbody>
                            {a.by_project.iter().map(|p| view! {
                                <tr>
                                    <th scope="row">
                                        <span class="truncate">{p.name.clone()}</span>
                                        <span class="hbar" aria-hidden="true"><i style=format!("width: {}%", bar_pct(p.tokens.total() as f64, max))></i></span>
                                    </th>
                                    <td>{format!("{} von {}", p.done, p.finished)}</td>
                                    <td>{fmt_tokens(p.tokens.total())}</td>
                                    <td>{fmt_money_list(&p.costs)}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                }.into_any()
            }}
        </section>
    }
}

#[component]
fn ProviderTable(a: Analytics) -> impl IntoView {
    let title = uid("by-provider");
    let labelled = title.clone();
    view! {
        <section class="card pad" aria-labelledby=labelled data-testid="by-provider">
            <h2 id=title>"Nach Anbieter und Modell"</h2>
            {if a.by_provider.is_empty() {
                view! { <p class="small muted">"Noch kein Verbrauch."</p> }.into_any()
            } else {
                view! {
                    <table class="table compact" tabindex="0">
                        <caption class="visually-hidden">"Tokens, Cache-Treffer und Kosten nach Anbieter und Modell"</caption>
                        <thead><tr><th scope="col">"Anbieter"</th><th scope="col">"Modell"</th><th scope="col">"Tokens"</th><th scope="col">"Cache"</th><th scope="col">"Kosten"</th></tr></thead>
                        <tbody>
                            {a.by_provider.iter().map(|r| view! {
                                <tr>
                                    <th scope="row">{provider_label(&r.provider)}</th>
                                    <td>{model_label(r.model.as_deref())}</td>
                                    <td>{fmt_tokens(r.tokens.total())}</td>
                                    <td>{fmt_percent(r.tokens.cache_ratio())}</td>
                                    <td>{fmt_money_list(&r.costs)}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                }.into_any()
            }}
        </section>
    }
}

#[component]
fn TopTasks(a: Analytics) -> impl IntoView {
    let open = use_open_task();
    let title = uid("top-tasks");
    let labelled = title.clone();
    view! {
        <section class="card pad" aria-labelledby=labelled data-testid="top-tasks">
            <h2 id=title>"Teuerste Aufträge"</h2>
            {if a.top_tasks.is_empty() {
                view! { <p class="small muted">"Noch keine Kosten."</p> }.into_any()
            } else {
                view! {
                    <ol class="top-list">
                        {a.top_tasks.into_iter().map(|t| {
                            let open = open.clone();
                            let id = t.task_id;
                            view! {
                                <li>
                                    <button type="button" class="link-btn grow truncate" on:click=move |_| open(id)>{t.title.clone()}</button>
                                    <span class="small muted truncate">{t.project.clone()}</span>
                                    <StatusPill status=TaskStatus::parse(&t.status).unwrap_or(TaskStatus::Done)/>
                                    <strong>{fmt_money_list(&t.costs)}</strong>
                                </li>
                            }
                        }).collect_view()}
                    </ol>
                }.into_any()
            }}
        </section>
    }
}

#[component]
fn PauseTable(rows: Vec<queued_api::analytics::PauseRow>) -> impl IntoView {
    let title = uid("pauses");
    let labelled = title.clone();
    view! {
        <section class="card pad" aria-labelledby=labelled data-testid="pauses">
            <h2 id=title>"Pausen durch Limits"</h2>
            {if rows.is_empty() {
                view! { <p class="small muted"><Icon icon=Ic::CheckCircle class="sm"/>" Keine Pausen in diesem Zeitraum."</p> }.into_any()
            } else {
                view! {
                    <table class="table compact" tabindex="0">
                        <caption class="visually-hidden">"Pausen der Warteschlange je Anbieter und Grund"</caption>
                        <thead><tr><th scope="col">"Anbieter"</th><th scope="col">"Grund"</th><th scope="col">"Anzahl"</th><th scope="col">"Dauer"</th></tr></thead>
                        <tbody>
                            {rows.into_iter().map(|r| view! {
                                <tr>
                                    <th scope="row">{provider_label(&r.provider)}</th>
                                    <td>{pause_reason_label(&r.reason)}</td>
                                    <td>{r.pauses}</td>
                                    <td>{format!("{} min", r.minutes)}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                }.into_any()
            }}
        </section>
    }
}

/// Drawer block: session insights of one task (loaded when opened).
#[component]
pub fn TaskInsightsBlock(id: uuid::Uuid) -> impl IntoView {
    let data = RwSignal::new(None::<Result<queued_api::analytics::TaskInsights, String>>);
    let loaded = RwSignal::new(false);
    let load = move || {
        if loaded.get_untracked() {
            return;
        }
        loaded.set(true);
        spawn_local(async move {
            data.set(Some(
                api::get(&format!("/tasks/{id}/insights"))
                    .await
                    .map_err(|e| e.message),
            ));
        });
    };
    view! {
        <details class="block insights" data-testid="task-insights" on:toggle=move |_| load()>
            <summary><Icon icon=Ic::ChevronRight class="sm chev"/>"Auswertung dieses Auftrags"</summary>
            {move || match data.get() {
                None => view! { <p class="small muted" role="status">"Lade …"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="alert tone-warning">{e}</p> }.into_any(),
                Some(Ok(i)) => {
                    let agent_runs = i.runs.len();
                    view! {
                        <dl class="kv small">
                            <dt>"Läufe"</dt><dd>{agent_runs}</dd>
                            <dt>"Arbeitszeit der Agenten"</dt><dd>{fmt_duration(i.agent_seconds)}</dd>
                            <dt>"Gesamtdauer"</dt><dd>{fmt_duration(i.wall_seconds)}</dd>
                            <dt>"Tokens"</dt><dd>{fmt_tokens(i.tokens.total())}</dd>
                            <dt>"Cache-Treffer"</dt><dd>{fmt_percent(i.tokens.cache_ratio())}</dd>
                            <dt>"Kosten"</dt><dd>{fmt_money_list(&i.costs)}</dd>
                        </dl>
                        <ul class="insight-notes small">
                            {i.notes.iter().map(|n| view! { <li>{n.clone()}</li> }).collect_view()}
                        </ul>
                        {(!i.tools.is_empty()).then(|| view! {
                            <p class="small"><strong>"Häufigste Werkzeuge: "</strong>
                                {i.tools.iter().map(|(n, c)| format!("{n} ({c}×)")).collect::<Vec<_>>().join(", ")}
                            </p>
                        })}
                        {(!i.runs.is_empty()).then(|| view! {
                            <table class="table compact" tabindex="0">
                                <caption class="visually-hidden">"Läufe dieses Auftrags"</caption>
                                <thead><tr><th scope="col">"Lauf"</th><th scope="col">"Dauer"</th><th scope="col">"Ergebnis"</th><th scope="col">"Tokens"</th><th scope="col">"Werkzeuge"</th></tr></thead>
                                <tbody>
                                    {i.runs.iter().map(|r| view! {
                                        <tr>
                                            <th scope="row">{run_kind_label(&r.kind)}</th>
                                            <td>{fmt_duration(r.seconds)}</td>
                                            <td>{queued_api::analytics::outcome_label(&r.outcome)}</td>
                                            <td>{fmt_tokens(r.tokens.total())}</td>
                                            <td>{r.tool_calls}</td>
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        })}
                    }.into_any()
                }
            }}
        </details>
    }
}

fn run_kind_label(k: &str) -> &'static str {
    match k {
        "agent" => "Agent",
        "review" => "Prüfung",
        "plan" => "Plan",
        _ => "Lauf",
    }
}
