//! Project tab "Automatisierung" (WP07): task templates, triggers (schedule,
//! one-off, webhook, GitHub, API), secrets shown once and the history.
//! Everybody in the project sees it; editors create templates and their
//! own triggers; the server enforces all rights.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::triggers::{
    CreatedTrigger, GithubEvent, GithubFilter, NewTemplate, NewTrigger, TemplateView, TriggerKind,
    TriggerRunView, TriggerView, UpdateTemplate, UpdateTrigger, CATCH_UP_HELP, MAX_PER_HOUR,
    PAYLOAD_HELP, RATE_HELP, SECRET_ONCE, SKIP_ACTIVE_HELP, TEMPLATES_HELP, TRIGGERS_HELP,
};
use queued_api::{ProjectView, TaskLimitFields};
use uuid::Uuid;

use crate::api;
use crate::logic::format::{fmt_relative, fmt_time};
use crate::logic::triggers::{
    cron_feedback, github_steps, outcome_tone, parse_list, parse_local, run_text, to_local_input,
    trigger_summary, webhook_example, CRON_PRESETS,
};
use crate::nav::use_open_task;
use crate::store::{tz, use_store};
use crate::ui::dialog::{Confirm, Modal};
use crate::ui::{uid, Empty, Field, FieldIds, HelpTip, Ic, Icon, SecretBox};

/// Time zone of the browser (IANA name), `UTC` if unknown.
fn browser_tz() -> String {
    let o = js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new())
        .resolved_options();
    js_sys::Reflect::get(&o, &"timeZone".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "UTC".into())
}

#[component]
pub fn ProjectAutomation(p: ProjectView) -> impl IntoView {
    let store = use_store();
    let pid = p.id;
    let can_edit = p.role.can_edit();
    let templates = RwSignal::new(None::<Vec<TemplateView>>);
    let triggers = RwSignal::new(None::<Vec<TriggerView>>);
    let secret = RwSignal::new(None::<CreatedTrigger>);
    let load_templates = move || {
        spawn_local(async move {
            match api::get::<Vec<TemplateView>>(&format!("/projects/{pid}/templates")).await {
                Ok(t) => templates.set(Some(t)),
                Err(e) => store.toast_error("Vorlagen konnten nicht geladen werden", &e),
            }
        })
    };
    let load_triggers = move || {
        spawn_local(async move {
            match api::get::<Vec<TriggerView>>(&format!("/projects/{pid}/triggers")).await {
                Ok(t) => triggers.set(Some(t)),
                Err(e) => store.toast_error("Auslöser konnten nicht geladen werden", &e),
            }
        })
    };
    load_templates();
    load_triggers();
    let reload = Callback::new(move |_| {
        load_templates();
        load_triggers();
    });
    let show_secret = Callback::new(move |c: CreatedTrigger| {
        secret.set(Some(c));
        load_triggers();
    });
    let new_template = RwSignal::new(false);
    let new_trigger = RwSignal::new(false);
    view! {
        <div class="stack" style="margin-top: var(--sp-4)" data-testid="project-automation">
            {(!can_edit).then(|| view! { <p class="alert tone-info"><Icon icon=Ic::Eye class="sm"/>"Du kannst Vorlagen und Auslöser ansehen. Anlegen können Bearbeiterinnen und Bearbeiter."</p> })}
            <section class="card settings-card" aria-labelledby="au-tpl">
                <header>
                    <span class="s-icon"><Icon icon=Ic::FileText/></span>
                    <div>
                        <h2 id="au-tpl" class="row">"Vorlagen" <HelpTip topic="Vorlagen" text=TEMPLATES_HELP align="align-start"/></h2>
                        <p>"Gespeicherte Aufträge, die Auslöser (und du im Auftragsformular) wiederverwenden."</p>
                    </div>
                </header>
                {move || match templates.get() {
                    None => view! { <p class="muted" role="status">"Lade Vorlagen …"</p> }.into_any(),
                    Some(list) if list.is_empty() => view! { <p class="muted">"Noch keine Vorlage."</p> }.into_any(),
                    Some(list) => view! {
                        <ul class="plain stack" aria-label="Vorlagen">
                            {list.into_iter().map(|t| view! { <TemplateRow t=t can_edit=can_edit on_change=reload/> }).collect_view()}
                        </ul>
                    }.into_any(),
                }}
                {move || (can_edit && !new_template.get()).then(|| view! {
                    <button type="button" class="btn" style="align-self:flex-start" data-testid="new-template" on:click=move |_| new_template.set(true)>
                        <Icon icon=Ic::Plus class="sm"/>"Neue Vorlage"
                    </button>
                })}
                {move || new_template.get().then(|| view! {
                    <TemplateForm pid=pid t=None on_done=Callback::new(move |saved: bool| {
                        new_template.set(false);
                        if saved { reload.run(()); }
                    })/>
                })}
            </section>
            <section class="card settings-card" aria-labelledby="au-trg">
                <header>
                    <span class="s-icon"><Icon icon=Ic::Zap/></span>
                    <div>
                        <h2 id="au-trg" class="row">"Auslöser" <HelpTip topic="Auslöser" text=TRIGGERS_HELP align="align-start"/></h2>
                        <p>"Starten Aufträge automatisch – nach Zeitplan, per Webhook, bei GitHub-Ereignissen oder per API."</p>
                    </div>
                </header>
                {move || match triggers.get() {
                    None => view! { <p class="muted" role="status">"Lade Auslöser …"</p> }.into_any(),
                    Some(list) if list.is_empty() => view! {
                        <Empty icon=Ic::Zap title="Noch keine Auslöser" text="Ein Auslöser startet Aufträge aus einer Vorlage, ohne dass du daran denken musst."/>
                    }.into_any(),
                    Some(list) => view! {
                        <ul class="plain stack" aria-label="Auslöser">
                            {list.into_iter().map(|t| view! {
                                <TriggerRow t=t templates=templates on_change=Callback::new(move |_| load_triggers()) on_secret=show_secret/>
                            }).collect_view()}
                        </ul>
                    }.into_any(),
                }}
                {move || {
                    let none = templates.with(|t| t.as_ref().is_some_and(Vec::is_empty));
                    (can_edit && !new_trigger.get()).then(|| view! {
                        {none.then(|| view! { <p class="small muted">"Lege zuerst eine Vorlage an – sie bestimmt, was der Auftrag tun soll."</p> })}
                        <button type="button" class="btn" style="align-self:flex-start" data-testid="new-trigger" disabled=none on:click=move |_| new_trigger.set(true)>
                            <Icon icon=Ic::Plus class="sm"/>"Neuer Auslöser"
                        </button>
                    })
                }}
                {move || new_trigger.get().then(|| view! {
                    <TriggerForm pid=pid t=None templates=templates on_done=Callback::new(move |r: Option<CreatedTrigger>| {
                        new_trigger.set(false);
                        match r {
                            Some(c) if c.secret.is_some() => show_secret.run(c),
                            Some(_) => load_triggers(),
                            None => {}
                        }
                    })/>
                })}
            </section>
            {move || secret.get().map(|c| view! { <SecretDialog c=c on_close=Callback::new(move |_| secret.set(None))/> })}
        </div>
    }
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[component]
fn TemplateRow(t: TemplateView, can_edit: bool, on_change: Callback<()>) -> impl IntoView {
    let store = use_store();
    let editing = RwSignal::new(false);
    let id = t.id;
    let name = t.name.clone();
    let used = t.used_by;
    let tv = t.clone();
    let del_title = if used > 0 {
        "Wird noch benutzt"
    } else {
        "Löschen"
    };
    let remove = move |_: leptos::ev::MouseEvent| {
        let name = name.clone();
        store.ask(Confirm {
            title: format!("Vorlage „{name}“ löschen?"),
            body: "Bereits gestartete Aufträge bleiben erhalten.".into(),
            confirm_label: "Löschen".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                store.run("Löschen fehlgeschlagen", async move {
                    api::delete(&format!("/templates/{id}")).await?;
                    store.toast("Vorlage gelöscht");
                    on_change.run(());
                    Ok(())
                });
            }),
        });
    };
    let first_line: String = t
        .prompt
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(160)
        .collect();
    view! {
        <li class="sub-card" data-testid="template-row">
            <div class="row wrap">
                <h3 class="grow">{t.name.clone()}</h3>
                {(used > 0).then(|| view! { <span class="tag">{format!("von {used} Auslöser(n) benutzt")}</span> })}
                {t.plan_mode.unwrap_or(false).then(|| view! { <span class="tag tone-info">"Erst planen"</span> })}
                {can_edit.then(|| view! {
                    <button type="button" class="btn sm" aria-expanded=move || editing.get().to_string() on:click=move |_| editing.update(|e| *e = !*e)>
                        <Icon icon=Ic::Pencil class="sm"/>"Bearbeiten"
                    </button>
                    <button type="button" class="btn sm icon-only danger" aria-label=format!("Vorlage {} löschen", t.name) title=del_title disabled={used > 0} on:click=remove.clone()>
                        <Icon icon=Ic::Trash/>
                    </button>
                })}
            </div>
            <p class="small"><strong>{t.title.clone()}</strong>" – "<span class="muted">{first_line}</span></p>
            {move || editing.get().then(|| view! {
                <TemplateForm pid=tv.project_id t=Some(tv.clone()) on_done=Callback::new(move |saved: bool| {
                    editing.set(false);
                    if saved { on_change.run(()); }
                })/>
            })}
        </li>
    }
}

#[component]
fn TemplateForm(pid: Uuid, t: Option<TemplateView>, on_done: Callback<bool>) -> impl IntoView {
    let store = use_store();
    let editing = t.as_ref().map(|t| t.id);
    let name = RwSignal::new(t.as_ref().map(|t| t.name.clone()).unwrap_or_default());
    let title = RwSignal::new(t.as_ref().map(|t| t.title.clone()).unwrap_or_default());
    let prompt = RwSignal::new(t.as_ref().map(|t| t.prompt.clone()).unwrap_or_default());
    let plan = RwSignal::new(match t.as_ref().and_then(|t| t.plan_mode) {
        None => "",
        Some(true) => "yes",
        Some(false) => "no",
    });
    let cost = RwSignal::new(
        t.as_ref()
            .and_then(|t| t.max_cost_usd)
            .map(|c| c.to_string())
            .unwrap_or_default(),
    );
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let ids = (
        FieldIds::new("tpl-name"),
        FieldIds::new("tpl-title"),
        FieldIds::new("tpl-prompt"),
        FieldIds::new("tpl-plan"),
        FieldIds::new("tpl-cost"),
    );
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        let cost_v = cost.get_untracked();
        let cost_v = cost_v.trim().replace(',', ".");
        let max_cost = if cost_v.is_empty() {
            None
        } else {
            match cost_v.parse::<f64>() {
                Ok(v) if v > 0.0 => Some(v),
                _ => {
                    error.set(Some(
                        "Kostenobergrenze: eine Zahl größer als 0 (USD) oder leer".into(),
                    ));
                    return;
                }
            }
        };
        let plan_mode = match plan.get_untracked() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        };
        let limits = TaskLimitFields {
            max_cost_usd: Some(max_cost),
            ..Default::default()
        };
        busy.set(true);
        spawn_local(async move {
            let r = match editing {
                None => {
                    api::post::<_, TemplateView>(
                        &format!("/projects/{pid}/templates"),
                        &NewTemplate {
                            name: name.get_untracked(),
                            title: title.get_untracked(),
                            prompt: prompt.get_untracked(),
                            provider: None,
                            model: None,
                            plan_mode,
                            limits,
                        },
                    )
                    .await
                }
                Some(id) => {
                    api::patch::<_, TemplateView>(
                        &format!("/templates/{id}"),
                        &UpdateTemplate {
                            name: Some(name.get_untracked()),
                            title: Some(title.get_untracked()),
                            prompt: Some(prompt.get_untracked()),
                            plan_mode: Some(plan_mode),
                            limits,
                            ..Default::default()
                        },
                    )
                    .await
                }
            };
            busy.set(false);
            match r {
                Ok(_) => {
                    store.toast("Vorlage gespeichert");
                    on_done.run(true);
                }
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    let (n, ti, pr, pl, co) = ids;
    view! {
        <form class="sub-card stack" on:submit=submit novalidate data-testid="template-form">
            <div class="grid-2">
                <Field ids=n.clone() label="Name der Vorlage" hint="Kurz, z. B. „Abhängigkeiten aktualisieren“">
                    <input id=n.id.clone() class="input" maxlength="80" aria-describedby=n.described_by() bind:value=name/>
                </Field>
                <Field ids=ti.clone() label="Titel des Auftrags">
                    <input id=ti.id.clone() class="input" maxlength="200" aria-describedby=ti.described_by() bind:value=title/>
                </Field>
            </div>
            <Field ids=pr.clone() label="Was soll der Agent tun?" hint="Genau wie im Auftragsformular.">
                <textarea id=pr.id.clone() class="textarea" rows="5" aria-describedby=pr.described_by() bind:value=prompt></textarea>
            </Field>
            <div class="grid-2">
                <Field ids=pl.clone() label="Erst planen?">
                    <select id=pl.id.clone() class="select" aria-describedby=pl.described_by()
                        on:change=move |ev| plan.set(match event_target_value(&ev).as_str() { "yes" => "yes", "no" => "no", _ => "" })>
                        <option value="" selected=move || plan.get().is_empty()>"Wie im Projekt eingestellt"</option>
                        <option value="yes" selected=move || plan.get() == "yes">"Ja – erst Plan, dann Freigabe"</option>
                        <option value="no" selected=move || plan.get() == "no">"Nein – direkt umsetzen"</option>
                    </select>
                </Field>
                <Field ids=co.clone() label="Kostenobergrenze (USD)" hint="leer = Projektvorgabe">
                    <input id=co.id.clone() class="input" inputmode="decimal" aria-describedby=co.described_by() bind:value=cost/>
                </Field>
            </div>
            {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            <div class="form-actions">
                <button type="submit" class="btn primary" disabled=move || busy.get()><Icon icon=Ic::Check class="sm"/>"Vorlage speichern"</button>
                <button type="button" class="btn ghost" on:click=move |_| on_done.run(false)>"Abbrechen"</button>
            </div>
        </form>
    }
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

fn kind_icon(k: TriggerKind) -> Ic {
    match k {
        TriggerKind::Schedule | TriggerKind::Once => Ic::Clock,
        TriggerKind::Api => Ic::Terminal,
        TriggerKind::Webhook => Ic::Zap,
        TriggerKind::Github => Ic::Branch,
    }
}

#[component]
fn TriggerRow(
    t: TriggerView,
    templates: RwSignal<Option<Vec<TemplateView>>>,
    on_change: Callback<()>,
    on_secret: Callback<CreatedTrigger>,
) -> impl IntoView {
    let store = use_store();
    let id = t.id;
    let editing = RwSignal::new(false);
    let enabled = t.enabled;
    let name = t.name.clone();
    let now = store.now.get_untracked();
    let summary = trigger_summary(&t, now, &tz());
    let toggle = move |ev: leptos::ev::Event| {
        let on = event_target_checked(&ev);
        store.run("Ändern fehlgeschlagen", async move {
            api::patch::<_, TriggerView>(
                &format!("/triggers/{id}"),
                &UpdateTrigger {
                    enabled: Some(on),
                    ..Default::default()
                },
            )
            .await?;
            store.toast(if on {
                "Auslöser eingeschaltet"
            } else {
                "Auslöser ausgeschaltet"
            });
            on_change.run(());
            Ok(())
        });
    };
    let fire = move |_| {
        store.run("Auslösen fehlgeschlagen", async move {
            let r: queued_api::triggers::FireResult =
                api::post(&format!("/triggers/{id}/fire"), &serde_json::json!({})).await?;
            store.toast(format!(
                "{}{}",
                r.outcome.label(),
                r.reason.map(|x| format!(": {x}")).unwrap_or_default()
            ));
            on_change.run(());
            Ok(())
        });
    };
    let renew = {
        let name = name.clone();
        move |_| {
            let name = name.clone();
            store.ask(Confirm {
                title: format!("Neues Secret für „{name}“?"),
                body: "Das alte Secret gilt sofort nicht mehr. Trage das neue danach beim Absender (z. B. GitHub) ein.".into(),
                confirm_label: "Neues Secret erzeugen".into(),
                danger: true,
                on_confirm: Callback::new(move |_| {
                    store.run("Erneuern fehlgeschlagen", async move {
                        let c: CreatedTrigger = api::post(&format!("/triggers/{id}/secret"), &serde_json::json!({})).await?;
                        on_secret.run(c);
                        Ok(())
                    });
                }),
            });
        }
    };
    let remove = {
        let name = name.clone();
        move |_| {
            let name = name.clone();
            store.ask(Confirm {
                title: format!("Auslöser „{name}“ löschen?"),
                body: "Er startet danach keine Aufträge mehr; sein Verlauf wird gelöscht. Gestartete Aufträge bleiben.".into(),
                confirm_label: "Löschen".into(),
                danger: true,
                on_confirm: Callback::new(move |_| {
                    store.run("Löschen fehlgeschlagen", async move {
                        api::delete(&format!("/triggers/{id}")).await?;
                        store.toast("Auslöser gelöscht");
                        on_change.run(());
                        Ok(())
                    });
                }),
            });
        }
    };
    let tv = t.clone();
    let last = t.last_run.clone();
    view! {
        <li class="sub-card" data-testid="trigger-row" data-kind=t.kind.as_str()>
            <div class="row wrap">
                <span class="tag"><Icon icon=kind_icon(t.kind) class="sm"/>{t.kind.label()}</span>
                <h3 class="grow">{t.name.clone()}</h3>
                {t.can_disable.then(|| view! {
                    <label class="switch">
                        <input type="checkbox" role="switch" prop:checked=enabled on:change=toggle data-testid="trigger-enabled"
                            aria-label=format!("Auslöser {} eingeschaltet", t.name)/>
                        <span class="check-text">{if enabled { "An" } else { "Aus" }}</span>
                    </label>
                })}
            </div>
            <p class="small" data-testid="trigger-summary">{summary}</p>
            <p class="small muted">{format!("Vorlage „{}“ · gehört {} · höchstens {} Aufträge pro Stunde", t.template_name, t.owner_name, t.max_per_hour)}</p>
            {t.disabled_reason.clone().map(|r| view! { <p class="alert tone-warning" role="status"><Icon icon=Ic::Alert class="sm"/>{r}</p> })}
            {t.hook_url.clone().map(|u| view! { <p class="small">"Adresse: "<span class="mono" style="overflow-wrap:anywhere">{u}</span></p> })}
            {last.map(|r| view! {
                <p class="small">
                    "Zuletzt: "<span class=format!("tag {}", outcome_tone(r.outcome).class())>{r.outcome.label()}</span>
                    {format!(" {} – {}", fmt_relative(r.created_at, now, &tz()), r.reason.clone().unwrap_or_else(|| r.source.label().to_string()))}
                </p>
            })}
            <div class="row wrap">
                {t.can_manage.then(|| view! {
                    <button type="button" class="btn sm" disabled=!enabled on:click=fire data-testid="trigger-fire"><Icon icon=Ic::Play class="sm"/>"Jetzt auslösen"</button>
                    <button type="button" class="btn sm" aria-expanded=move || editing.get().to_string() on:click=move |_| editing.update(|e| *e = !*e)><Icon icon=Ic::Pencil class="sm"/>"Bearbeiten"</button>
                })}
                {(t.can_manage && t.kind.has_secret()).then(|| view! {
                    <button type="button" class="btn sm" on:click=renew.clone()><Icon icon=Ic::Key class="sm"/>"Neues Secret"</button>
                })}
                {t.can_disable.then(|| view! {
                    <button type="button" class="btn sm icon-only danger" aria-label=format!("Auslöser {name} löschen") title="Löschen" on:click=remove.clone()><Icon icon=Ic::Trash/></button>
                })}
            </div>
            {move || editing.get().then(|| view! {
                <TriggerForm pid=tv.project_id t=Some(tv.clone()) templates=templates on_done=Callback::new(move |r: Option<CreatedTrigger>| {
                    editing.set(false);
                    if r.is_some() { on_change.run(()); }
                })/>
            })}
            <History id=id/>
        </li>
    }
}

#[component]
fn History(id: Uuid) -> impl IntoView {
    let store = use_store();
    let runs = RwSignal::new(None::<Vec<TriggerRunView>>);
    let open_task = use_open_task();
    let load = move || {
        spawn_local(async move {
            match api::get::<Vec<TriggerRunView>>(&format!("/triggers/{id}/runs")).await {
                Ok(r) => runs.set(Some(r)),
                Err(e) => store.toast_error("Verlauf konnte nicht geladen werden", &e),
            }
        })
    };
    view! {
        <details class="block" data-testid="trigger-history" on:toggle=move |ev| {
            use wasm_bindgen::JsCast;
            if ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlDetailsElement>().ok()).is_some_and(|d| d.open()) {
                load();
            }
        }>
            <summary>"Verlauf"</summary>
            {move || match runs.get() {
                None => view! { <p class="muted small" role="status">"Lade …"</p> }.into_any(),
                Some(r) if r.is_empty() => view! { <p class="muted small">"Noch nicht ausgelöst."</p> }.into_any(),
                Some(r) => {
                    let now = store.now.get_untracked();
                    let open_task = open_task.clone();
                    view! {
                        <ul class="plain stack" aria-label="Verlauf des Auslösers">
                            {r.into_iter().map(|x| {
                                let open_task = open_task.clone();
                                view! {
                                    <li class="row wrap small" data-testid="run-row" data-outcome=x.outcome.as_str()>
                                        <span class=format!("tag {}", outcome_tone(x.outcome).class())>{x.outcome.label()}</span>
                                        <span>{fmt_time(x.created_at, now, &tz())}</span>
                                        <span class="muted">{run_text(&x)}</span>
                                        {x.reason.clone().map(|r| view! { <span class="grow" style="overflow-wrap:anywhere">{r}</span> })}
                                        {x.task_id.map(|t| {
                                            let title = x.task_title.clone().unwrap_or_else(|| "Auftrag".into());
                                            view! { <button type="button" class="btn sm ghost" on:click=move |_| open_task(t)>{format!("Öffnen: {title}")}</button> }
                                        })}
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    }.into_any()
                }
            }}
        </details>
    }
}

#[component]
fn TriggerForm(
    pid: Uuid,
    t: Option<TriggerView>,
    templates: RwSignal<Option<Vec<TemplateView>>>,
    on_done: Callback<Option<CreatedTrigger>>,
) -> impl IntoView {
    let store = use_store();
    let editing = t.as_ref().map(|t| t.id);
    let kind = RwSignal::new(t.as_ref().map(|t| t.kind).unwrap_or(TriggerKind::Schedule));
    let name = RwSignal::new(t.as_ref().map(|t| t.name.clone()).unwrap_or_default());
    let template = RwSignal::new(
        t.as_ref()
            .map(|t| t.template_id.to_string())
            .or_else(|| {
                templates.with_untracked(|l| {
                    l.as_ref().and_then(|l| l.first()).map(|x| x.id.to_string())
                })
            })
            .unwrap_or_default(),
    );
    let cron = RwSignal::new(
        t.as_ref()
            .and_then(|t| t.cron.clone())
            .unwrap_or_else(|| CRON_PRESETS[0].0.to_string()),
    );
    let zone = RwSignal::new(
        t.as_ref()
            .and_then(|t| t.timezone.clone())
            .unwrap_or_else(browser_tz),
    );
    let run_at = RwSignal::new(
        t.as_ref()
            .and_then(|t| t.run_at)
            .map(|r| to_local_input(r, &tz()))
            .unwrap_or_default(),
    );
    let catch_up = RwSignal::new(t.as_ref().map(|t| t.catch_up).unwrap_or(true));
    let skip = RwSignal::new(t.as_ref().map(|t| t.skip_if_active).unwrap_or(true));
    let per_hour = RwSignal::new(t.as_ref().map(|t| t.max_per_hour).unwrap_or(10).to_string());
    let gh = t
        .as_ref()
        .and_then(|t| t.github.clone())
        .unwrap_or(GithubFilter {
            events: vec![GithubEvent::Push],
            ..Default::default()
        });
    let events = RwSignal::new(gh.events.clone());
    let branches = RwSignal::new(gh.branches.join(", "));
    let labels = RwSignal::new(gh.labels.join(", "));
    let repo = RwSignal::new(gh.repository.clone().unwrap_or_default());
    let bots = RwSignal::new(gh.include_bots);
    let drafts = RwSignal::new(gh.include_drafts);
    let zones = RwSignal::new(Vec::<String>::new());
    spawn_local(async move {
        if let Ok(z) = api::get::<Vec<String>>("/timezones").await {
            zones.set(z);
        }
    });
    let cron_text = Memo::new(move |_| cron_feedback(&cron.get()));
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        let k = kind.get_untracked();
        let Ok(template_id) = Uuid::parse_str(&template.get_untracked()) else {
            error.set(Some("Wähle eine Vorlage.".into()));
            return;
        };
        let max_per_hour = match per_hour.get_untracked().trim().parse::<i32>() {
            Ok(v) if (1..=MAX_PER_HOUR).contains(&v) => v,
            _ => {
                error.set(Some(format!("Aufträge pro Stunde: 1 bis {MAX_PER_HOUR}")));
                return;
            }
        };
        let run_at_v = if k == TriggerKind::Once {
            match parse_local(&run_at.get_untracked(), &tz()) {
                Some(v) => Some(v),
                None => {
                    error.set(Some("Wähle Datum und Uhrzeit.".into()));
                    return;
                }
            }
        } else {
            None
        };
        let github = (k == TriggerKind::Github).then(|| GithubFilter {
            events: events.get_untracked(),
            branches: parse_list(&branches.get_untracked()),
            labels: parse_list(&labels.get_untracked()),
            repository: Some(repo.get_untracked()).filter(|r| !r.trim().is_empty()),
            include_bots: bots.get_untracked(),
            include_drafts: drafts.get_untracked(),
        });
        let timed = k.is_timed();
        let cron_v = (k == TriggerKind::Schedule).then(|| cron.get_untracked());
        let zone_v = timed.then(|| zone.get_untracked());
        busy.set(true);
        spawn_local(async move {
            let r = match editing {
                None => {
                    api::post::<_, CreatedTrigger>(
                        &format!("/projects/{pid}/triggers"),
                        &NewTrigger {
                            template_id,
                            kind: k,
                            name: name.get_untracked(),
                            cron: cron_v,
                            timezone: zone_v,
                            run_at: run_at_v,
                            catch_up: catch_up.get_untracked(),
                            github,
                            max_per_hour,
                            skip_if_active: skip.get_untracked(),
                            enabled: true,
                        },
                    )
                    .await
                }
                Some(id) => api::patch::<_, TriggerView>(
                    &format!("/triggers/{id}"),
                    &UpdateTrigger {
                        template_id: Some(template_id),
                        name: Some(name.get_untracked()),
                        cron: cron_v,
                        timezone: zone_v,
                        run_at: run_at_v,
                        catch_up: Some(catch_up.get_untracked()),
                        github,
                        max_per_hour: Some(max_per_hour),
                        skip_if_active: Some(skip.get_untracked()),
                        enabled: None,
                    },
                )
                .await
                .map(|trigger| CreatedTrigger {
                    trigger,
                    secret: None,
                }),
            };
            busy.set(false);
            match r {
                Ok(c) => {
                    store.toast("Auslöser gespeichert");
                    on_done.run(Some(c));
                }
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    let name_ids = FieldIds::new("trg-name");
    let tpl_ids = FieldIds::new("trg-tpl");
    let cron_ids = FieldIds::new("trg-cron");
    let zone_ids = FieldIds::new("trg-zone");
    let at_ids = FieldIds::new("trg-at");
    let rate_ids = FieldIds::new("trg-rate");
    let br_ids = FieldIds::new("trg-branches");
    let lb_ids = FieldIds::new("trg-labels");
    let repo_ids = FieldIds::new("trg-repo");
    let kind_label = uid("trg-kind");
    let ev_label = uid("trg-events");
    let zone_list = uid("trg-zones");
    view! {
        <form class="sub-card stack" on:submit=submit novalidate data-testid="trigger-form">
            {editing.is_none().then(|| {
                let kind_label = kind_label.clone();
                let kind_for = kind_label.clone();
                view! {
                    <fieldset class="choice-list" role="radiogroup" aria-labelledby=kind_for>
                        <legend id=kind_label class="label">"Wann soll ein Auftrag starten?"</legend>
                        {TriggerKind::ALL.into_iter().map(|k| view! {
                            <label class="choice" data-testid=format!("kind-{}", k.as_str())>
                                <input type="radio" name="trigger-kind" prop:checked=move || kind.get() == k on:change=move |_| kind.set(k)/>
                                <span class="check-text">{k.label()}<small>{k.help()}</small></span>
                            </label>
                        }).collect_view()}
                    </fieldset>
                }
            })}
            <div class="grid-2">
                <Field ids=name_ids.clone() label="Name des Auslösers">
                    <input id=name_ids.id.clone() class="input" maxlength="80" aria-describedby=name_ids.described_by() bind:value=name/>
                </Field>
                <Field ids=tpl_ids.clone() label="Vorlage">
                    <select id=tpl_ids.id.clone() class="select" aria-describedby=tpl_ids.described_by() on:change=move |ev| template.set(event_target_value(&ev))>
                        {move || templates.get().unwrap_or_default().into_iter().map(|x| {
                            let v = x.id.to_string();
                            let sel = template.get_untracked() == v;
                            view! { <option value=v selected=sel>{x.name}</option> }
                        }).collect_view()}
                    </select>
                </Field>
            </div>
            {
                let cron_ids = cron_ids.clone();
                let zone_ids = zone_ids.clone();
                let zone_list = zone_list.clone();
                let at_ids = at_ids.clone();
                move || {
                    let k = kind.get();
                    let (cron_ids, at_ids, br_ids, lb_ids, repo_ids, ev_label) = (
                        cron_ids.clone(),
                        at_ids.clone(),
                        br_ids.clone(),
                        lb_ids.clone(),
                        repo_ids.clone(),
                        ev_label.clone(),
                    );
                    let zone_field = {
                        let zone_ids = zone_ids.clone();
                        let zone_list = zone_list.clone();
                        view! {
                            <Field ids=zone_ids.clone() label="Zeitzone" hint="z. B. Europe/Zurich">
                                <input id=zone_ids.id.clone() class="input" list=zone_list.clone() spellcheck="false" autocomplete="off" aria-describedby=zone_ids.described_by() bind:value=zone/>
                                <datalist id=zone_list>{move || zones.get().into_iter().map(|z| view! { <option value=z/> }).collect_view()}</datalist>
                            </Field>
                        }
                    };
                    match k {
                        TriggerKind::Schedule => view! {
                            <div class="stack">
                                <div class="row wrap" role="group" aria-label="Häufige Zeitpläne">
                                    {CRON_PRESETS.iter().map(|(c, l)| view! { <button type="button" class="btn sm ghost" on:click=move |_| cron.set(c.to_string())>{*l}</button> }).collect_view()}
                                </div>
                                <div class="grid-2">
                                    <Field ids=cron_ids.clone() label="Zeitplan (Cron)" hint="Minute Stunde Tag Monat Wochentag – höchstens stündlich">
                                        <input id=cron_ids.id.clone() class="input mono-input" spellcheck="false" autocomplete="off" aria-describedby=cron_ids.described_by() bind:value=cron data-testid="cron"/>
                                    </Field>
                                    {zone_field}
                                </div>
                                <p class="small" role="status" data-testid="cron-text">
                                    {move || match cron_text.get() {
                                        Ok(d) => view! { <span><Icon icon=Ic::Clock class="sm"/>" "{d}</span> }.into_any(),
                                        Err(e) => view! { <span class="err">{e}</span> }.into_any(),
                                    }}
                                </p>
                                <label class="switch">
                                    <input type="checkbox" role="switch" prop:checked=move || catch_up.get() on:change=move |ev| catch_up.set(event_target_checked(&ev))/>
                                    <span class="check-text">"Verpasste Läufe einmal nachholen"<small>{CATCH_UP_HELP}</small></span>
                                </label>
                            </div>
                        }.into_any(),
                        TriggerKind::Once => view! {
                            <div class="stack">
                                <div class="grid-2">
                                    <Field ids=at_ids.clone() label="Datum und Uhrzeit" hint="in deiner Ortszeit">
                                        <input id=at_ids.id.clone() class="input" type="datetime-local" aria-describedby=at_ids.described_by() bind:value=run_at data-testid="run-at"/>
                                    </Field>
                                    {zone_field}
                                </div>
                                <label class="switch">
                                    <input type="checkbox" role="switch" prop:checked=move || catch_up.get() on:change=move |ev| catch_up.set(event_target_checked(&ev))/>
                                    <span class="check-text">"Nachholen, falls Queue zu dem Zeitpunkt nicht lief"<small>{CATCH_UP_HELP}</small></span>
                                </label>
                            </div>
                        }.into_any(),
                        TriggerKind::Github => {
                            let ev_for = ev_label.clone();
                            view! {
                                <div class="stack">
                                    <fieldset class="choice-list" aria-labelledby=ev_for>
                                        <legend id=ev_label class="label">"Ereignisse"</legend>
                                        {GithubEvent::ALL.into_iter().map(|e| view! {
                                            <label class="check">
                                                <input type="checkbox" data-testid=format!("event-{}", e.as_str())
                                                    prop:checked=move || events.with(|v| v.contains(&e))
                                                    on:change=move |ev| {
                                                        let on = event_target_checked(&ev);
                                                        events.update(|v| { v.retain(|x| *x != e); if on { v.push(e); } });
                                                    }/>
                                                <span class="check-text">{e.label()}<small>{e.help()}</small></span>
                                            </label>
                                        }).collect_view()}
                                    </fieldset>
                                    <div class="grid-3">
                                        <Field ids=br_ids.clone() label="Branches" hint="leer = alle; z. B. main, release/*">
                                            <input id=br_ids.id.clone() class="input" spellcheck="false" aria-describedby=br_ids.described_by() bind:value=branches/>
                                        </Field>
                                        <Field ids=lb_ids.clone() label="Labels (Issues/PRs)" hint="leer = egal">
                                            <input id=lb_ids.id.clone() class="input" spellcheck="false" aria-describedby=lb_ids.described_by() bind:value=labels/>
                                        </Field>
                                        <Field ids=repo_ids.clone() label="Repository" hint="leer = das des Projekts">
                                            <input id=repo_ids.id.clone() class="input" spellcheck="false" placeholder="besitzer/repository" aria-describedby=repo_ids.described_by() bind:value=repo/>
                                        </Field>
                                    </div>
                                    <label class="check"><input type="checkbox" prop:checked=move || bots.get() on:change=move |ev| bots.set(event_target_checked(&ev))/><span class="check-text">"Auch Ereignisse von Bots"<small>"z. B. Dependabot. Checks zählen immer."</small></span></label>
                                    <label class="check"><input type="checkbox" prop:checked=move || drafts.get() on:change=move |ev| drafts.set(event_target_checked(&ev))/><span class="check-text">"Auch Entwürfe von Pull Requests"</span></label>
                                    <p class="small muted">{PAYLOAD_HELP}</p>
                                </div>
                            }.into_any()
                        }
                        TriggerKind::Webhook => view! { <p class="small muted">{PAYLOAD_HELP}" Adresse und Secret bekommst du nach dem Speichern."</p> }.into_any(),
                        TriggerKind::Api => view! { <p class="small muted">"Starte ihn mit einem API-Token mit der Berechtigung „Auslöser starten“: queuectl fire <ID> oder POST /api/triggers/<ID>/fire."</p> }.into_any(),
                    }
                }
            }
            <div class="grid-2">
                <label class="switch">
                    <input type="checkbox" role="switch" prop:checked=move || skip.get() on:change=move |ev| skip.set(event_target_checked(&ev))/>
                    <span class="check-text">"Nicht starten, solange der letzte Auftrag läuft"<small>{SKIP_ACTIVE_HELP}</small></span>
                </label>
                <Field ids=rate_ids.clone() label="Höchstens Aufträge pro Stunde" help=RATE_HELP>
                    <input id=rate_ids.id.clone() class="input" inputmode="numeric" aria-describedby=rate_ids.described_by() bind:value=per_hour/>
                </Field>
            </div>
            {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            <div class="form-actions">
                <button type="submit" class="btn primary" disabled=move || busy.get()><Icon icon=Ic::Check class="sm"/>"Auslöser speichern"</button>
                <button type="button" class="btn ghost" on:click=move |_| on_done.run(None)>"Abbrechen"</button>
            </div>
        </form>
    }
}

#[component]
fn SecretDialog(c: CreatedTrigger, on_close: Callback<()>) -> impl IntoView {
    let title = uid("sec-title");
    let desc = uid("sec-desc");
    let url = c.trigger.hook_url.clone().unwrap_or_default();
    let secret = c.secret.clone().unwrap_or_default();
    let github = c.trigger.kind == TriggerKind::Github;
    let steps = github_steps(&url, c.trigger.github.as_ref());
    let example = webhook_example(&url);
    view! {
        <Modal on_close=on_close labelledby=title.clone() describedby=desc.clone()>
            <span class="dialog-icon tone-success"><Icon icon=Ic::Key class="lg"/></span>
            <h2 id=title>{format!("Secret für „{}“", c.trigger.name)}</h2>
            <p id=desc class="alert tone-warning"><Icon icon=Ic::Alert class="sm"/>{SECRET_ONCE}</p>
            <SecretBox label="Adresse" value=url.clone() testid="hook-url"/>
            <SecretBox label="Secret" value=secret testid="hook-secret"/>
            {if github {
                view! {
                    <ol class="small stack" style="padding-left: 1.2em; margin: 0">
                        {steps.into_iter().map(|s| view! { <li style="overflow-wrap:anywhere">{s}</li> }).collect_view()}
                    </ol>
                }.into_any()
            } else {
                view! {
                    <details class="block">
                        <summary>"So signiert ein Absender (Beispiel)"</summary>
                        <p class="small muted">"Signatur = HMAC-SHA256 mit dem Secret über „Zeitstempel.Aufruf-ID.Inhalt“; der Zeitstempel darf höchstens 5 Minuten abweichen, jede Aufruf-ID zählt nur einmal."</p>
                        <pre class="code small" tabindex="0" aria-label="Beispiel-Aufruf">{example}</pre>
                    </details>
                }.into_any()
            }}
            <div class="dialog-actions">
                <button type="button" class="btn primary" data-autofocus on:click=move |_| on_close.run(())>"Fertig"</button>
            </div>
        </Modal>
    }
}
