//! Pull requests (WP05): the block in the task drawer and the list in the
//! project history. Texts from the host (check names) are shown as text
//! only; comment and CI texts never reach the browser.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::pr::{PullRequestDetail, PullRequestView, SetAutoFix, AUTO_FIX_HELP, ROUNDS_HELP};
use queued_api::ProjectRole;
use uuid::Uuid;

use crate::api;
use crate::logic::format::fmt_relative;
use crate::logic::labels::help;
use crate::logic::pr::{
    can_extend, check_tone, ci_tone, outcome_tone, pr_tone, rounds_text, state_line,
};
use crate::nav::use_open_task;
use crate::store::{tz, use_store};
use crate::ui::{HelpTip, Ic, Icon, Tag};

/// Role of the current user in a project (from the project list).
fn role_in(project: Uuid) -> Option<ProjectRole> {
    let store = use_store();
    store
        .projects
        .with_untracked(|ps| ps.iter().find(|p| p.id == project).map(|p| p.role))
}

/// Drawer block: link, state, checks, rounds and the steering actions.
#[component]
pub fn PrBlock(pr: PullRequestView) -> impl IntoView {
    let store = use_store();
    let id = pr.id;
    let data = RwSignal::new(None::<PullRequestDetail>);
    let busy = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            if let Ok(d) = api::get::<PullRequestDetail>(&format!("/pulls/{id}")).await {
                data.set(Some(d));
            }
        });
    };
    load();
    // Refresh while the request is open (the poller works in the background).
    let last = StoredValue::new(store.now.get_untracked());
    Effect::new(move |_| {
        let now = store.now.get();
        let open = data.with(|d| d.as_ref().is_none_or(|d| d.pr.state.is_open()));
        if open && (now - last.get_value()).num_seconds() >= 5 {
            last.set_value(now);
            load();
        }
    });
    let can_steer = role_in(pr.project_id).is_some_and(|r| r.can_edit());
    let act = move |path: &'static str, body: serde_json::Value, done: &'static str| {
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, PullRequestDetail>(&format!("/pulls/{id}/{path}"), &body).await {
                Ok(d) => {
                    data.set(Some(d));
                    store.toast(done);
                }
                Err(e) => store.toast_error("Aktion fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };
    let open_task = use_open_task();
    let initial = pr;
    view! {
        <section class="block" data-testid="pr-block">
            <h3>"Pull Request"<HelpTip topic="Pull Request" text=help::PR align="align-start"/></h3>
            {move || {
                let d = data.get();
                let p = d.as_ref().map(|d| d.pr.clone()).unwrap_or_else(|| initial.clone());
                let feedback = d.map(|d| d.feedback).unwrap_or_default();
                let open_task = open_task.clone();
                let title = p.title();
                let active = p.active_task;
                view! {
                    <div class="box stack-sm">
                        <p class="row wrap">
                            {match p.url.clone() {
                                Some(u) => view! {
                                    <a class="btn sm" href=u target="_blank" rel="noopener noreferrer" data-testid="pr-link">
                                        <Icon icon=Ic::Branch class="sm"/>{format!("{title} öffnen")}
                                    </a>
                                }.into_any(),
                                None => view! { <strong>{title.clone()}</strong> }.into_any(),
                            }}
                            <Tag text=p.state.label(p.draft) tone=pr_tone(p.state) test_id="pr-state"/>
                            <Tag text=p.ci_state.label() tone=ci_tone(p.ci_state) test_id="pr-ci"/>
                        </p>
                        <p class="small mono-wrap">{state_line(&p)}</p>
                        {(!p.checks.is_empty()).then(|| view! {
                            <ul class="plain small checks" aria-label="Checks" data-testid="pr-checks">
                                {p.checks.iter().map(|c| {
                                    let name = c.name.clone();
                                    view! {
                                        <li class="row wrap">
                                            <Tag text=c.state.label() tone=check_tone(c.state)/>
                                            {match c.url.clone() {
                                                Some(u) => view! { <a href=u target="_blank" rel="noopener noreferrer">{name}</a> }.into_any(),
                                                None => view! { <span>{name}</span> }.into_any(),
                                            }}
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                        })}
                        <p class="small row wrap" data-testid="pr-rounds">
                            {rounds_text(&p)}
                            <HelpTip topic="Folge-Runden" text=ROUNDS_HELP/>
                        </p>
                        {active.map(|a| {
                            let open_task = open_task.clone();
                            view! {
                                <button class="btn sm" type="button" on:click=move |_| open_task(a)>
                                    <Icon icon=Ic::Loader class="sm"/>"Laufenden Folge-Auftrag öffnen"
                                </button>
                            }
                        })}
                        {p.last_error.clone().map(|e| view! {
                            <p class="alert tone-warning small" role="status" data-testid="pr-error"><Icon icon=Ic::Alert class="sm"/>{e}</p>
                        })}
                        {p.last_polled_at.map(|t| view! {
                            <p class="xs muted">"Zuletzt geprüft: "{move || fmt_relative(t, store.now.get(), &tz())}</p>
                        })}
                        {(can_steer && p.state.is_open()).then(|| {
                            let auto = p.auto_fix;
                            let extend = can_extend(&p);
                            view! {
                                <label class="switch">
                                    <input type="checkbox" role="switch" data-testid="pr-autofix"
                                        prop:checked=auto disabled=move || busy.get()
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            act("auto-fix", serde_json::to_value(SetAutoFix { enabled: on }).unwrap_or_default(),
                                                if on { "Auto-Fix eingeschaltet" } else { "Auto-Fix ausgeschaltet" });
                                        }/>
                                    <span class="check-text">"Auto-Fix"<small>"Rote CI und Review-Kommentare starten Folge-Aufträge"</small></span>
                                </label>
                                <div class="row wrap">
                                    <button class="btn sm" type="button" disabled=move || busy.get()
                                        on:click=move |_| act("poll", serde_json::json!({}), "Wird gleich geprüft")>
                                        <Icon icon=Ic::Retry class="sm"/>"Jetzt prüfen"
                                    </button>
                                    {extend.then(|| view! {
                                        <button class="btn sm primary" type="button" data-testid="pr-extra-round" disabled=move || busy.get()
                                            on:click=move |_| act("extra-round", serde_json::json!({}), "Eine weitere Runde erlaubt")>
                                            <Icon icon=Ic::Plus class="sm"/>"Eine weitere Runde erlauben"
                                        </button>
                                    })}
                                    <HelpTip topic="Auto-Fix" text=AUTO_FIX_HELP/>
                                </div>
                            }
                        })}
                        {(!feedback.is_empty()).then(|| view! {
                            <details class="block">
                                <summary><Icon icon=Ic::ChevronRight class="sm chev"/>{format!("Rückmeldungen ({})", feedback.len())}</summary>
                                <ul class="plain small" data-testid="pr-feedback">
                                    {feedback.into_iter().map(|f| view! {
                                        <li class="row wrap">
                                            <Tag text=f.outcome.label() tone=outcome_tone(f.outcome)/>
                                            <span>{f.source.label()}</span>
                                            {f.author.map(|a| view! { <span class="muted">{format!("@{a}")}</span> })}
                                        </li>
                                    }).collect_view()}
                                </ul>
                            </details>
                        })}
                    </div>
                }
            }}
        </section>
    }
}

/// Project → Verlauf: the project's pull requests.
#[component]
pub fn PullRequestsCard(project: Uuid) -> impl IntoView {
    let store = use_store();
    let list = RwSignal::new(None::<Vec<PullRequestView>>);
    spawn_local(async move {
        if let Ok(l) = api::get::<Vec<PullRequestView>>(&format!("/projects/{project}/pulls")).await
        {
            list.set(Some(l));
        }
    });
    let open = use_open_task();
    view! {
        <section class="card stack" aria-labelledby="prs-title" data-testid="pull-requests">
            <h2 id="prs-title" class="row" style="font-size: var(--fs-lg)">
                <Icon icon=Ic::Branch class="sm"/>"Pull Requests"
                <HelpTip topic="Pull Requests" text=help::PR align="align-start"/>
            </h2>
            {move || match list.get() {
                None => view! { <div class="list"><p class="empty-line">"Lade …"</p></div> }.into_any(),
                Some(l) if l.is_empty() => view! {
                    <div class="list"><p class="empty-line">"Noch keine Pull Requests. Unter Einstellungen → Ergebnis lässt sich „Pull Request“ wählen."</p></div>
                }.into_any(),
                Some(l) => view! {
                    <ul class="list">
                        {l.into_iter().map(|p| {
                            let open = open.clone();
                            let task = p.task_id;
                            let created = p.created_at;
                            view! {
                                <li class="item">
                                    <div class="item-body">
                                        <button class="open" type="button" on:click=move |_| open(task)>{p.task_title.clone()}</button>
                                        <p class="row wrap small">
                                            {match p.url.clone() {
                                                Some(u) => view! { <a href=u target="_blank" rel="noopener noreferrer">{p.title()}</a> }.into_any(),
                                                None => view! { <span>{p.title()}</span> }.into_any(),
                                            }}
                                            <Tag text=p.state.label(p.draft) tone=pr_tone(p.state)/>
                                            <Tag text=p.ci_state.label() tone=ci_tone(p.ci_state)/>
                                            <span class="muted">{format!("{}/{} Runden", p.followups, p.max_followups)}</span>
                                            <span class="muted">{move || fmt_relative(created, store.now.get(), &tz())}</span>
                                        </p>
                                    </div>
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any(),
            }}
        </section>
    }
}
