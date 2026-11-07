//! "Neuer Auftrag": the most important action, always at the top.
use std::cell::Cell;

use leptos::task::spawn_local;
use leptos::{html, prelude::*};
use queued_api::{CreateTask, Task, TaskStatus};
use uuid::Uuid;
use wasm_bindgen::JsCast;

use crate::api;
use crate::logic::labels::{help, provider_label};
use crate::logic::limits::{LimitField, LimitForm};
use crate::logic::task::first_line;
use crate::store::use_store;
use crate::theme::{get_pref, set_pref};
use crate::ui::{uid, Field, FieldIds, HelpTip, Ic, Icon};

thread_local! {
    static FOCUS_HANDLED: Cell<u32> = const { Cell::new(0) };
}

/// Limit inputs shared by the composer and the drawer's edit form.
#[component]
pub fn TaskLimitInputs(
    form: RwSignal<LimitForm>,
    errors: RwSignal<Vec<(LimitField, String)>>,
) -> impl IntoView {
    let err = move |f: LimitField| {
        Signal::derive(move || {
            errors.with(|e| e.iter().find(|(x, _)| *x == f).map(|(_, m)| m.clone()))
        })
    };
    let input = move |f: LimitField,
                      label: &'static str,
                      hint: &'static str,
                      mode: &'static str,
                      name: &'static str| {
        let ids = FieldIds::new(name);
        let get = move || {
            form.with(|v| match f {
                LimitField::Runtime => v.runtime.clone(),
                LimitField::Stall => v.stall.clone(),
                LimitField::Cost => v.cost.clone(),
                LimitField::Tokens => v.tokens.clone(),
                LimitField::Turns => v.turns.clone(),
            })
        };
        let help_text = match f {
            LimitField::Turns => help::TURNS,
            LimitField::Tokens => help::TOKENS,
            _ => "",
        };
        let e = err(f);
        let (id, described) = (ids.id.clone(), ids.described_by());
        view! {
            <Field ids=ids label=label hint=hint help=help_text error=e>
                <input
                    id=id
                    name=name
                    class="input"
                    inputmode=mode
                    placeholder="Standard"
                    aria-describedby=described
                    aria-invalid=move || e.get().is_some().to_string()
                    prop:value=get
                    on:input=move |ev| {
                        let v = event_target_value(&ev);
                        form.update(|x| match f {
                            LimitField::Runtime => x.runtime = v,
                            LimitField::Stall => x.stall = v,
                            LimitField::Cost => x.cost = v,
                            LimitField::Tokens => x.tokens = v,
                            LimitField::Turns => x.turns = v,
                        });
                        errors.update(|es| es.retain(|(x, _)| *x != f));
                    }
                />
            </Field>
        }
    };
    view! {
        <div class="grid-3">
            {input(LimitField::Runtime, "Max. Laufzeit (Min.)", "je Lauf", "decimal", "task_max_runtime")}
            {input(LimitField::Cost, "Max. Kosten (USD)", "für den ganzen Auftrag", "decimal", "task_max_cost")}
            {input(LimitField::Tokens, "Max. Tokens", "für den ganzen Auftrag", "numeric", "task_max_tokens")}
            {input(LimitField::Turns, "Max. Turns", "je Lauf", "numeric", "task_max_turns")}
        </div>
    }
}

#[component]
pub fn Composer() -> impl IntoView {
    let store = use_store();
    let prompt = RwSignal::new(get_pref("draft").unwrap_or_default());
    let context = RwSignal::new(String::new());
    let provider = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let parent = RwSignal::new(String::new());
    let front = RwSignal::new(false);
    let advanced = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let limits = RwSignal::new(LimitForm::default());
    let limit_errors = RwSignal::new(vec![]);
    let chosen = RwSignal::new(get_pref("project").unwrap_or_default());
    let textarea: NodeRef<html::Textarea> = NodeRef::new();
    let section: NodeRef<html::Section> = NodeRef::new();

    let editable = Memo::new(move |_| {
        store.projects.with(|ps| {
            ps.iter()
                .filter(|p| p.role.can_edit())
                .map(|p| (p.id, p.name.clone()))
                .collect::<Vec<_>>()
        })
    });
    let project_id = Memo::new(move |_| {
        let c = chosen.get();
        editable.with(|ps| {
            ps.iter()
                .find(|(id, _)| id.to_string() == c)
                .or(ps.first())
                .map(|(id, _)| *id)
        })
    });
    let parents = Memo::new(move |_| {
        let Some(pid) = project_id.get() else {
            return vec![];
        };
        let mut done: Vec<Task> = store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.project_id == pid && t.status_kind() == TaskStatus::Done)
                .cloned()
                .collect()
        });
        done.sort_by_key(|t| std::cmp::Reverse(t.finished_at));
        done.into_iter()
            .take(20)
            .map(|t| (t.id, first_line(&t.title, 60)))
            .collect::<Vec<_>>()
    });

    Effect::new(move |_| {
        let n = store.focus_composer.get();
        let Some(el) = textarea.get() else { return };
        if n > FOCUS_HANDLED.with(Cell::get) {
            FOCUS_HANDLED.with(|c| c.set(n));
            let _ = el.focus();
            if let Some(s) = section.get_untracked() {
                let opts = web_sys::ScrollIntoViewOptions::new();
                opts.set_block(web_sys::ScrollLogicalPosition::Center);
                s.scroll_into_view_with_scroll_into_view_options(&opts);
            }
        }
    });

    let submit = move || {
        let text = prompt.get_untracked();
        let Some(pid) = project_id.get_untracked() else {
            return;
        };
        if text.trim().is_empty() || busy.get_untracked() {
            return;
        }
        let lim = match limits.get_untracked().parse() {
            Ok(l) => l,
            Err(errs) => {
                limit_errors.set(errs);
                advanced.set(true);
                store.announce("Bitte die markierten Limits korrigieren");
                return;
            }
        };
        busy.set(true);
        let body = CreateTask {
            project_id: pid,
            title: None,
            prompt: text,
            context: context.get_untracked(),
            provider: Some(provider.get_untracked()).filter(|p| !p.is_empty()),
            model: Some(model.get_untracked().trim().to_string()).filter(|m| !m.is_empty()),
            parent_id: Uuid::parse_str(&parent.get_untracked()).ok(),
            front: front.get_untracked(),
            limits: lim.create_fields(),
        };
        spawn_local(async move {
            match api::post::<_, Task>("/tasks", &body).await {
                Ok(t) => {
                    prompt.set(String::new());
                    set_pref("draft", "");
                    context.set(String::new());
                    parent.set(String::new());
                    front.set(false);
                    limits.set(LimitForm::default());
                    store.announce(format!("„{}“ eingereiht", first_line(&t.title, 60)));
                    if let Some(el) = textarea.get_untracked() {
                        let _ = el.focus();
                    }
                }
                Err(e) => store.toast_error("Auftrag konnte nicht eingereiht werden", &e),
            }
            busy.set(false);
        });
    };

    let title_id = uid("composer-title");
    let title_ref = title_id.clone();
    let prompt_id = uid("composer-prompt");
    let project_sel = uid("composer-project");
    let adv_id = uid("composer-advanced");
    let ctx_ids = FieldIds::new("ctx");
    let parent_ids = FieldIds::new("parent");
    let prov_ids = FieldIds::new("provider");
    let model_ids = FieldIds::new("model");
    let default_provider = move || {
        store.settings.with(|s| {
            s.as_ref()
                .map(|s| provider_label(&s.default_provider))
                .unwrap_or_default()
        })
    };

    view! {
        <section class="card composer" node_ref=section aria-labelledby=title_ref id="composer">
            <h2 class="composer-title" id=title_id>
                <Icon icon=Ic::Sparkles/>
                "Neuer Auftrag"
            </h2>
            <label class="visually-hidden" for=prompt_id.clone()>"Auftrag beschreiben"</label>
            <textarea
                id=prompt_id
                class="textarea prompt"
                rows="3"
                node_ref=textarea
                placeholder="Was soll erledigt werden? Beschreibe das Ziel so konkret wie möglich."
                aria-keyshortcuts="Control+Enter Meta+Enter"
                prop:value=move || prompt.get()
                on:input=move |ev| {
                    let v = event_target_value(&ev);
                    set_pref("draft", &v);
                    prompt.set(v);
                }
                on:keydown=move |ev| {
                    if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) {
                        ev.prevent_default();
                        submit();
                    }
                }
            ></textarea>
            {move || advanced.get().then(|| {
                let (ctx_ids, parent_ids, prov_ids, model_ids) = (ctx_ids.clone(), parent_ids.clone(), prov_ids.clone(), model_ids.clone());
                let (ctx_id, parent_id, prov_id, model_id) = (ctx_ids.id.clone(), parent_ids.id.clone(), prov_ids.id.clone(), model_ids.id.clone());
                view! {
                    <div class="advanced" id=adv_id.clone()>
                        <Field ids=ctx_ids label="Zusätzlicher Kontext" help=help::CONTEXT hint="Optional – wird dem Agenten mitgegeben.">
                            <textarea id=ctx_id class="textarea" rows="3" placeholder="Anforderungen, Links, Hinweise …" bind:value=context></textarea>
                        </Field>
                        <div class="grid-3">
                            <Field ids=parent_ids label="Fortsetzung von" help=help::HANDOFF>
                                <select id=parent_id class="select" prop:value=move || parent.get() on:change=move |ev| parent.set(event_target_value(&ev))>
                                    <option value="">"– keiner –"</option>
                                    {move || parents.get().into_iter().map(|(id, t)| view! { <option value=id.to_string()>{t}</option> }).collect_view()}
                                </select>
                            </Field>
                            <Field ids=prov_ids label="Anbieter" help=help::PROVIDER>
                                <select id=prov_id class="select" prop:value=move || provider.get() on:change=move |ev| provider.set(event_target_value(&ev))>
                                    <option value="">{move || format!("Standard ({})", default_provider())}</option>
                                    {move || store.meta.with(|m| m.providers.iter().map(|p| view! { <option value=p.clone()>{provider_label(p)}</option> }).collect_view())}
                                </select>
                            </Field>
                            <Field ids=model_ids label="Modell" help=help::MODEL>
                                <input id=model_id class="input" placeholder="Standard" bind:value=model/>
                            </Field>
                        </div>
                        <h4 class="row">"Limits dieses Auftrags" <HelpTip topic="Limits" text=help::LIMITS/></h4>
                        <TaskLimitInputs form=limits errors=limit_errors/>
                    </div>
                }
            })}
            <div class="composer-bar">
                <label class="visually-hidden" for=project_sel.clone()>"Projekt"</label>
                <select
                    id=project_sel
                    class="select"
                    prop:value=move || project_id.get().map(|p| p.to_string()).unwrap_or_default()
                    on:change=move |ev| {
                        let v = event_target_value(&ev);
                        set_pref("project", &v);
                        chosen.set(v);
                    }
                >
                    {move || editable.get().into_iter().map(|(id, name)| view! { <option value=id.to_string()>{name}</option> }).collect_view()}
                </select>
                <span class="front">
                    <label class="check">
                        <input type="checkbox" prop:checked=move || front.get() on:change=move |ev| front.set(event_target_checked(&ev))/>
                        <span>"Als Nächstes"</span>
                    </label>
                    <HelpTip topic="Als Nächstes" text=help::FRONT/>
                </span>
                <button
                    type="button"
                    class="btn ghost"
                    aria-expanded=move || advanced.get().to_string()
                    on:click=move |_| advanced.update(|a| *a = !*a)
                >
                    <Icon icon=Ic::Settings class="sm"/>
                    {move || if advanced.get() { "Weniger Optionen" } else { "Erweitert" }}
                </button>
                <span class="grow"></span>
                <span class="hint-keys" aria-hidden="true"><kbd>"Strg"</kbd>" + "<kbd>"Enter"</kbd></span>
                <button
                    type="button"
                    class="btn primary"
                    disabled=move || busy.get() || prompt.with(|p| p.trim().is_empty()) || project_id.get().is_none()
                    on:click=move |_| submit()
                >
                    <Icon icon=Ic::Send class="sm"/>
                    "Einreihen"
                </button>
            </div>
        </section>
    }
}

/// Moves the keyboard focus to an element by id (used by the onboarding).
pub fn focus_by_id(id: &str) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(id))
    {
        if let Ok(h) = el.dyn_into::<web_sys::HtmlElement>() {
            let _ = h.focus();
        }
    }
}
