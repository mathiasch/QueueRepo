//! Task details: progress, result, review, changes (diff), revert, edit, log.
use leptos::task::spawn_local;
use leptos::{html, prelude::*};
use queued_api::gate::{
    finding_help, finding_title, FindingKind, GateAction, GateDecision, GateStage, TaskGate,
};
use queued_api::{Answer, DiffView, Task, TaskCost, TaskDetail, TaskEvent, TaskStatus, UpdateTask};
use uuid::Uuid;

use crate::api;
use crate::logic::costs::{fmt_money, fmt_money_list, source_label, step_label};
use crate::logic::diff::{classify, file_stats};
use crate::logic::format::{duration, fmt_relative, fmt_time, fmt_tokens};
use crate::logic::labels::{
    event_kind_label, failure_help, failure_label, help, provider_label, status_help, status_label,
    task_kind_label,
};
use crate::logic::limits::{task_limits, LimitField, LimitForm};
use crate::logic::progress::Phase;
use crate::logic::task::{first_line, shows_abort_reason};
use crate::nav::{use_close_task, use_open_task};
use crate::store::{push_event, tz, use_store};
use crate::ui::dialog::{focus_initial, modal_lifecycle, trap_tab, Confirm};
use crate::ui::{uid, HelpTip, Ic, Icon, StatusPill};

use super::composer::TaskLimitInputs;
use super::queue::{confirm_cancel_running, discard, Steps};

#[derive(Clone)]
struct EditState {
    title: RwSignal<String>,
    prompt: RwSignal<String>,
    context: RwSignal<String>,
    limits: RwSignal<LimitForm>,
    errors: RwSignal<Vec<(LimitField, String)>>,
}

#[component]
pub fn TaskDrawer(id: Uuid) -> impl IntoView {
    let store = use_store();
    let close = use_close_task();
    let fetched = RwSignal::new(None::<Task>);
    let events = RwSignal::new(Vec::<TaskEvent>::new());
    let loaded = RwSignal::new(false);
    let diff = RwSignal::new(None::<String>);
    let editing = RwSignal::new(None::<EditState>);
    let node: NodeRef<html::Div> = NodeRef::new();
    let title_id = uid("drawer-title");

    store.attach_sink(id, events);
    on_cleanup(move || store.detach_sink(id));
    modal_lifecycle();
    {
        let close = close.clone();
        spawn_local(async move {
            match api::get::<TaskDetail>(&format!("/tasks/{id}")).await {
                Ok(d) => {
                    fetched.set(Some(d.task));
                    events.update(|evs| {
                        let live = std::mem::take(evs);
                        *evs = d.events;
                        for e in live {
                            push_event(evs, e);
                        }
                    });
                    loaded.set(true);
                }
                Err(e) => {
                    store.toast_error("Auftrag konnte nicht geladen werden", &e);
                    close();
                }
            }
        });
    }
    Effect::new(move |done: Option<bool>| {
        // Focus the dialog once its content exists.
        if done == Some(true) {
            return true;
        }
        match node.get() {
            Some(el) if loaded.get() || store.task(id).is_some() => {
                focus_initial(&el);
                true
            }
            _ => false,
        }
    });

    // Escape also works when the focus fell back to <body> (e.g. after saving an edit).
    {
        let close = close.clone();
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if ev.key() != "Escape"
                || ev.default_prevented()
                || store.confirm.get_untracked().is_some()
                || store.show_shortcuts.get_untracked()
            {
                return;
            }
            if editing.get_untracked().is_some() {
                editing.set(None);
            } else {
                close();
            }
        });
        on_cleanup(move || handle.remove());
    }
    // After leaving the edit form the focus returns to the dialog.
    Effect::new(move |was_editing: Option<bool>| {
        let now = editing.with(Option::is_some);
        if was_editing == Some(true) && !now {
            if let Some(el) = node.get_untracked() {
                let _ = el.focus();
            }
        }
        now
    });

    // Live version from the store, otherwise the fetched one (older tasks).
    let task = Memo::new(move |_| store.task(id).or_else(|| fetched.get()));

    let close_btn = close.clone();
    let close_backdrop = close.clone();
    let close_key = close.clone();
    let tid = title_id.clone();
    let title_id = StoredValue::new(title_id);
    view! {
        <div class="backdrop" on:click=move |ev| if ev.target() == ev.current_target() { close_backdrop() }>
            <div
                class="drawer"
                role="dialog"
                aria-modal="true"
                aria-labelledby=tid
                tabindex="-1"
                node_ref=node
                on:keydown=move |ev| {
                    if ev.key() == "Escape" {
                        if editing.get_untracked().is_some() {
                            editing.set(None);
                        } else {
                            close_key();
                        }
                        ev.stop_propagation();
                    } else if let Some(el) = node.get_untracked() {
                        trap_tab(&ev, &el);
                    }
                }
            >
                <Show
                    when=move || task.with(Option::is_some)
                    fallback={
                        let title_id = title_id.get_value();
                        move || view! {
                            <div class="drawer-head">
                                <h2 id=title_id.clone() class="grow">"Lade Auftrag …"</h2>
                            </div>
                        }
                    }
                >
                    {
                        let close_btn = close_btn.clone();
                        let title_id = title_id.get_value();
                        move || task.get().map(|t| {
                            let close_btn = close_btn.clone();
                            view! { <DrawerHead t=t title_id=title_id.clone() on_close=Callback::new(move |_| close_btn())/> }
                        })
                    }
                    {move || task.get().map(|t| view! { <DrawerActions t=t diff=diff editing=editing/> })}
                    <div class="drawer-body">
                        {move || match editing.get() {
                            Some(ed) => Some(view! { <EditForm id=id ed=ed editing=editing/> }.into_any()),
                            None => task.get().map(|t| view! { <Details t=t/> }.into_any()),
                        }}
                        {move || diff.get().map(|d| view! { <DiffBlock diff=d/> })}
                        <LogBlock t=task events=events/>
                    </div>
                </Show>
            </div>
        </div>
    }
}

#[component]
fn DrawerHead(t: Task, title_id: String, on_close: Callback<()>) -> impl IntoView {
    let store = use_store();
    let status = t.status_kind();
    let pid = t.project_id;
    let provider =
        crate::logic::costs::access_text(&t, &store.connectors.get_untracked(), provider_label);
    let (started, finished, created) = (t.started_at, t.finished_at, t.created_at);
    view! {
        <div class="drawer-head">
            <div class="grow stack" style="gap: 6px">
                <div class="row wrap">
                    <StatusPill status=status/>
                    <span class="tag">{task_kind_label(&t.kind)}</span>
                    {(t.attempt > 0).then(|| view! { <span class="tag">{format!("Versuch {}", t.attempt + 1)}</span> })}
                </div>
                <h2 id=title_id>{t.title.clone()}</h2>
                <p class="small muted">{status_help(status)}</p>
                <div class="sub">
                    {move || {
                        let project = store.project_name(pid);
                        (!project.is_empty()).then(|| view! { <span>{project}</span><span aria-hidden="true">"·"</span> })
                    }}
                    <span>{provider}</span>
                    <span aria-hidden="true">"·"</span>
                    <span>{move || format!("erstellt {}", fmt_relative(created, store.now.get(), &tz()))}</span>
                    {started.map(|_| view! {
                        <span aria-hidden="true">"·"</span>
                        <span>{move || format!("Dauer {}", duration(started, finished, store.now.get()))}</span>
                    })}
                </div>
            </div>
            <button class="btn icon-only drawer-close" type="button" aria-label="Schließen" title="Schließen (Esc)" on:click=move |_| on_close.run(())>
                <Icon icon=Ic::X class="lg"/>
            </button>
        </div>
    }
}

#[component]
fn DrawerActions(
    t: Task,
    diff: RwSignal<Option<String>>,
    editing: RwSignal<Option<EditState>>,
) -> impl IntoView {
    let store = use_store();
    let status = t.status_kind();
    let id = t.id;
    let has_changes = t.has_changes();
    let edit_t = t.clone();
    let start_edit = move |_| {
        editing.set(Some(EditState {
            title: RwSignal::new(edit_t.title.clone()),
            prompt: RwSignal::new(edit_t.prompt.clone()),
            context: RwSignal::new(edit_t.context.clone()),
            limits: RwSignal::new(LimitForm::from_task(&edit_t)),
            errors: RwSignal::new(vec![]),
        }))
    };
    let toggle_diff = move |_| {
        if diff.get_untracked().is_some() {
            diff.set(None);
            return;
        }
        spawn_local(async move {
            match api::get::<DiffView>(&format!("/tasks/{id}/diff")).await {
                Ok(d) => diff.set(Some(d.diff)),
                Err(e) => store.toast_error("Änderungen konnten nicht geladen werden", &e),
            }
        });
    };
    let revert_title = t.title.clone();
    let revert = move |_| {
        store.ask(Confirm {
            title: "Änderungen rückgängig machen?".into(),
            body: format!(
                "Alle Änderungen von „{}“ werden im Projekt zurückgenommen (als neuer Commit, der Verlauf bleibt erhalten).",
                first_line(&revert_title, 80)
            ),
            confirm_label: "Rückgängig machen".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                spawn_local(async move {
                    match api::action(&format!("/tasks/{id}/revert")).await {
                        Ok(()) => store.toast("Änderungen zurückgesetzt"),
                        Err(e) => store.toast_error("Rückgängig machen fehlgeschlagen", &e),
                    }
                });
            }),
        });
    };
    let (tc, td) = (t.clone(), t.clone());
    view! {
        <div class="drawer-actions">
            {(status.is_editable()).then(|| view! {
                <button class="btn" type="button" on:click=start_edit disabled=move || editing.get().is_some()>
                    <Icon icon=Ic::Pencil class="sm"/>"Bearbeiten"
                </button>
            })}
            {(status == TaskStatus::PendingApproval).then(|| view! {
                <button class="btn primary" type="button" on:click=move |_| {
                    store.run("Freigabe fehlgeschlagen", async move { api::action(&format!("/tasks/{id}/approve")).await })
                }>
                    <Icon icon=Ic::Check class="sm"/>"Freigeben"
                </button>
            })}
            {(status == TaskStatus::Running).then(|| view! {
                <button class="btn danger" type="button" on:click=move |_| confirm_cancel_running(store, &tc)>
                    <Icon icon=Ic::X class="sm"/>"Abbrechen"
                </button>
            })}
            {(status.can_cancel() && !matches!(status, TaskStatus::Running | TaskStatus::Held)).then(|| view! {
                <button class="btn danger" type="button" on:click=move |_| discard(store, &td)>
                    <Icon icon=Ic::Trash class="sm"/>"Verwerfen"
                </button>
            })}
            {status.can_retry().then(|| view! {
                <button class="btn" type="button" on:click=move |_| {
                    spawn_local(async move {
                        match api::action(&format!("/tasks/{id}/retry")).await {
                            Ok(()) => store.toast("Erneut eingereiht"),
                            Err(e) => store.toast_error("Erneut einreihen fehlgeschlagen", &e),
                        }
                    })
                }>
                    <Icon icon=Ic::Retry class="sm"/>"Erneut versuchen"
                </button>
            })}
            {has_changes.then(|| view! {
                <button class="btn" type="button" aria-expanded=move || diff.get().is_some().to_string() on:click=toggle_diff>
                    <Icon icon=Ic::FileText class="sm"/>
                    {move || if diff.get().is_some() { "Änderungen ausblenden" } else { "Änderungen anzeigen" }}
                </button>
            })}
            {(has_changes && status != TaskStatus::Running).then(|| view! {
                <button class="btn danger" type="button" on:click=revert>
                    <Icon icon=Ic::Undo class="sm"/>"Rückgängig machen"
                </button>
            })}
        </div>
    }
}

#[component]
fn Block(
    #[prop(into)] title: String,
    #[prop(optional)] tone: &'static str,
    #[prop(optional)] help: &'static str,
    children: Children,
) -> impl IntoView {
    let topic = title.clone();
    view! {
        <section class=format!("block {tone}")>
            <h3>{title}{(!help.is_empty()).then(|| view! { <HelpTip topic=topic text=help align="align-start"/> })}</h3>
            {children()}
        </section>
    }
}

#[component]
fn Collapsible(
    #[prop(into)] title: String,
    text: String,
    #[prop(optional)] open: bool,
    #[prop(optional)] help: &'static str,
) -> impl IntoView {
    view! {
        <details class="block" open=open>
            <summary>
                <Icon icon=Ic::ChevronRight class="sm chev"/>
                {title}
            </summary>
            {(!help.is_empty()).then(|| view! { <p class="small muted"><Icon icon=Ic::Info class="sm"/>" "{help}</p> })}
            <div class="box pre">{text}</div>
        </details>
    }
}

#[component]
fn Details(t: Task) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let status = t.status_kind();
    let id = t.id;
    let parent_id = t.parent_id;
    // Live: repair and follow-up tasks appear while the details are open.
    let chain = Memo::new(move |_| {
        let parent = parent_id.and_then(|p| store.task(p));
        let children: Vec<Task> = store.tasks.with(|ts| {
            ts.iter()
                .filter(|c| c.parent_id == Some(id))
                .cloned()
                .collect()
        });
        (parent, children)
    });
    let limits = task_limits(&t);
    let review = t.review_result();
    let usage = t.usage_parsed();
    let cost = t.cost_parsed().filter(|c| !c.steps.is_empty());
    let plan = t.plan.clone().filter(|p| !p.trim().is_empty());
    let questions = t.questions_list();
    let started = t.started_at.is_some();
    let phase = Memo::new(move |_| {
        store.phases.with(|p| {
            p.get(&id)
                .copied()
                .unwrap_or(if started { Phase::Work } else { Phase::Prepare })
        })
    });
    let activity = Memo::new(move |_| {
        store
            .last_event
            .with(|m| m.get(&id).map(|e| first_line(&e.content, 200)))
    });

    let abort = t.failure().filter(|_| shows_abort_reason(&t)).map(|c| {
        let mut lines = vec![failure_label(c).to_string(), failure_help(c).to_string()];
        if t.transient_retries > 0 {
            lines.push(format!("Wiederholungen: {}", t.transient_retries));
        }
        if let (Some(nb), TaskStatus::Queued) = (t.not_before, status) {
            lines.push(format!(
                "Nächster Versuch: {}",
                fmt_time(nb, store.now.get_untracked(), &tz())
            ));
        }
        lines.push("Kein Fix-Versuch verbraucht.".into());
        (
            lines.join("\n"),
            if status == TaskStatus::Blocked {
                "tone-danger"
            } else {
                "tone-warning"
            },
        )
    });

    let prompt_long = t.prompt.chars().count() > 400 && status != TaskStatus::Queued;
    view! {
        {(status == TaskStatus::Running).then(|| view! {
            <Block title="Fortschritt">
                <Steps phase=phase.into()/>
                {move || activity.get().map(|a| view! { <p class="box small live">{a}</p> })}
            </Block>
        })}
        {t.gate_parsed().filter(|_| status == TaskStatus::Held).map(|g| view! { <GatePanel id=id gate=g/> })}
        {(!questions.is_empty()).then(|| view! { <DrawerQuestions id=id questions=questions.clone() waiting=status == TaskStatus::WaitingInput/> })}
        {abort.map(|(text, tone)| view! {
            <div data-testid="abort-reason">
                <Block title="Abbruchgrund" tone=tone><div class="box pre">{text}</div></Block>
            </div>
        })}
        {(!limits.is_empty()).then(|| view! {
            <Block title="Limits dieses Auftrags" help=help::LIMITS><div class="box">{limits.join(" · ")}</div></Block>
        })}
        {t.error.clone().map(|e| view! { <Block title="Fehler" tone="tone-danger"><div class="box pre">{e}</div></Block> })}
        {t.summary.clone().map(|s| view! { <Block title="Ergebnis"><div class="box pre">{s}</div></Block> })}
        {review.map(|r| {
            let tone = if r.approved { "tone-success" } else { "tone-danger" };
            view! {
                <Block title="Review durch zweiten Agenten" tone=tone help=help::REVIEW>
                    <div class="box">
                        <strong class="row">
                            <Icon icon=if r.approved { Ic::CheckCircle } else { Ic::XCircle } class="sm"/>
                            {if r.approved { "Bestanden" } else { "Abgelehnt" }}
                        </strong>
                        {if r.issues.is_empty() {
                            view! { <p>"Keine Beanstandungen."</p> }.into_any()
                        } else {
                            view! { <ul class="questions">{r.issues.into_iter().map(|i| view! { <li>{i}</li> }).collect_view()}</ul> }.into_any()
                        }}
                    </div>
                </Block>
            }
        })}
        {t.handoff.clone().filter(|h| !h.trim().is_empty()).map(|h| view! {
            <Collapsible title="Übergabe an Folgeaufträge" text=h.clone() open={h.len() < 400} help=help::HANDOFF/>
        })}
        <Collapsible
            title="Auftrag"
            text=if t.prompt.is_empty() { "(automatisch erzeugt)".to_string() } else { t.prompt.clone() }
            open=!prompt_long
        />
        {(!t.context.is_empty()).then(|| view! { <Collapsible title="Kontext" text=t.context.clone() open={t.context.len() < 400}/> })}
        {move || {
            let (parent, children) = chain.get();
            let open = open.clone();
            (parent.is_some() || !children.is_empty()).then(|| {
            let open_p = open.clone();
            view! {
                <Block title="Zusammenhang">
                    <div class="stack chain" style="gap: 6px">
                        {parent.map(|p| {
                            let pid = p.id;
                            view! {
                                <button class="link-btn" type="button" on:click=move |_| open_p(pid)>
                                    <Icon icon=Ic::ChevronLeft class="sm"/>{format!("Vorgänger: {}", p.title)}
                                </button>
                            }
                        })}
                        {children.into_iter().map(|c| {
                            let open = open.clone();
                            let cid = c.id;
                            view! {
                                <button class="link-btn" type="button" on:click=move |_| open(cid)>
                                    <Icon icon=Ic::ChevronRight class="sm"/>
                                    {format!("{} ({})", c.title, status_label(c.status_kind()))}
                                </button>
                            }
                        }).collect_view()}
                    </div>
                </Block>
            }
        })
        }}
        {plan.map(|p| view! { <Collapsible title="Plan" text=p help=help::PLAN/> })}
        {usage.map(|u| view! {
            <Block title="Verbrauch" help=help::TOKENS>
                <div class="usage">
                    <span><strong>{fmt_tokens(u.input_tokens)}</strong>" Eingabe"</span>
                    <span><strong>{fmt_tokens(u.output_tokens)}</strong>" Ausgabe"</span>
                    <span><strong>{fmt_tokens(u.cache_read_tokens)}</strong>" aus dem Cache"</span>
                </div>
            </Block>
        })}
        {cost.map(|c| view! { <CostBlock c=c/> })}
    }
}

/// Money cost per step (catalog prices; CLI dollars only as marked fallback).
#[component]
fn CostBlock(c: TaskCost) -> impl IntoView {
    view! {
        <Block title="Kosten" help=help::COSTS>
            <p class="cost-total" data-testid="task-cost"><strong>{fmt_money_list(&c.totals)}</strong>
                {c.incomplete.then(|| view! { <span class="small muted">" – unvollständig, nicht für jeden Schritt ist ein Preis bekannt"</span> })}
            </p>
            <table class="table compact" tabindex="0">
                <caption class="visually-hidden">"Kosten je Schritt"</caption>
                <thead><tr><th scope="col">"Schritt"</th><th scope="col">"Modell"</th><th scope="col">"Tokens"</th><th scope="col">"Kosten"</th></tr></thead>
                <tbody>
                    {c.steps.iter().map(|s| {
                        let tokens = s.usage.input_tokens + s.usage.output_tokens + s.usage.cache_read_tokens + s.usage.cache_write_tokens;
                        let model = match (&s.model, &s.connector) {
                            (Some(m), Some(a)) => format!("{m} ({a})"),
                            (Some(m), None) => m.clone(),
                            (None, Some(a)) => a.clone(),
                            (None, None) => "Standard".into(),
                        };
                        view! {
                            <tr>
                                <td>{step_label(&s.step)}{(s.runs > 1).then(|| format!(" ×{}", s.runs))}</td>
                                <td>{model}</td>
                                <td>{fmt_tokens(tokens)}</td>
                                <td title=source_label(s.source)>{fmt_money(s.amount, &s.currency)}<span class="small muted">{format!(" · {}", source_label(s.source))}</span></td>
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
            {c.steps.iter().flat_map(|s| s.notes.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().map(|n| view! { <p class="small muted">{n}</p> }).collect_view()}
            <p class="small muted">{format!("Preistabelle vom {}", c.price_version)}</p>
        </Block>
    }
}

/// A gate holds the task (WP03): what was found, why it matters, the held
/// changes (secrets redacted by the server) and the decision.
#[component]
fn GatePanel(id: Uuid, gate: TaskGate) -> impl IntoView {
    let store = use_store();
    let stage = gate.stage;
    let diff = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let has_secret = gate.findings.iter().any(|f| f.kind == FindingKind::Secret);
    let decide = move |path: &'static str, action: Option<GateAction>, done: &'static str| {
        busy.set(true);
        spawn_local(async move {
            let url = format!("/tasks/{id}/gate/{path}");
            let res = match action {
                Some(a) => api::post_ok(&url, &GateDecision { action: a }).await,
                None => api::action(&url).await,
            };
            busy.set(false);
            match res {
                Ok(()) => store.announce(done),
                Err(e) => store.toast_error("Entscheidung fehlgeschlagen", &e),
            }
        });
    };
    let approve = move |_| {
        if has_secret {
            store.ask(Confirm {
                title: "Mögliches Geheimnis freigeben?".into(),
                body: "Der Wert wird im Projekt gespeichert und kann hochgeladen werden. Gib ihn nur frei, wenn er kein echtes Passwort, Token oder Schlüssel ist (z. B. Testdaten). Queue merkt sich die Freigabe für dieses Projekt.".into(),
                confirm_label: "Trotzdem freigeben".into(),
                danger: true,
                on_confirm: Callback::new(move |_| decide("approve", Some(GateAction::Approve), "Freigegeben, der Auftrag geht weiter")),
            });
        } else {
            decide(
                "approve",
                Some(GateAction::Approve),
                "Freigegeben, der Auftrag geht weiter",
            );
        }
    };
    let reject = move |_| {
        if stage == GateStage::Changes {
            store.ask(Confirm {
                title: "Änderungen verwerfen?".into(),
                body: "Die Änderungen des Agenten werden gelöscht. Der Auftrag wird angehalten; du kannst ihn später erneut starten.".into(),
                confirm_label: "Verwerfen".into(),
                danger: true,
                on_confirm: Callback::new(move |_| decide("reject", None, "Änderungen verworfen")),
            });
        } else {
            decide("reject", None, "Entschieden");
        }
    };
    let toggle_diff = move |_| {
        if diff.get_untracked().is_some() {
            diff.set(None);
            return;
        }
        spawn_local(async move {
            match api::get::<DiffView>(&format!("/tasks/{id}/gate/diff")).await {
                Ok(d) => diff.set(Some(d.diff)),
                Err(e) => store.toast_error("Änderungen konnten nicht geladen werden", &e),
            }
        });
    };
    let findings = gate.findings.clone();
    view! {
        <section class="block tone-warning gate-panel" data-testid="gate-panel" aria-labelledby="gate-title">
            <h3 id="gate-title"><Icon icon=Ic::Shield class="sm"/>" "{stage.title()}<HelpTip topic="Schutzprüfung" text=help::PROTECTION align="align-start"/></h3>
            <p class="small">{stage.help()}</p>
            <ul class="gate-findings">
                {findings.into_iter().map(|f| {
                    let place = match (f.path.is_empty(), f.line) {
                        (true, _) => String::new(),
                        (false, Some(l)) => format!("{}, Zeile {l}", f.path),
                        (false, None) => f.path.clone(),
                    };
                    view! {
                        <li class="gate-finding" data-kind=format!("{:?}", f.kind)>
                            <strong>{finding_title(f.kind)}</strong>
                            {(!place.is_empty()).then(|| view! { <span class="mono small truncate">{place}</span> })}
                            {(!f.detail.is_empty()).then(|| view! { <span class="small">{f.detail.clone()}</span> })}
                            <span class="small muted">{finding_help(f.kind)}</span>
                        </li>
                    }
                }).collect_view()}
            </ul>
            <div class="row wrap gate-actions">
                {(stage == GateStage::Review).then(|| view! {
                    <button class="btn primary" type="button" disabled=move || busy.get()
                        on:click=move |_| decide("approve", Some(GateAction::RetryReview), "Review wird wiederholt")>
                        <Icon icon=Ic::Retry class="sm"/>"Review wiederholen"
                    </button>
                })}
                <button class=if stage == GateStage::Review { "btn" } else { "btn primary" } type="button" disabled=move || busy.get() on:click=approve>
                    <Icon icon=if stage == GateStage::Push { Ic::CloudUpload } else { Ic::Check } class="sm"/>{stage.approve_label()}
                </button>
                <button class="btn danger" type="button" disabled=move || busy.get() on:click=reject>
                    <Icon icon=if stage == GateStage::Changes { Ic::Trash } else { Ic::X } class="sm"/>{stage.reject_label()}
                </button>
                {gate.has_diff.then(|| view! {
                    <button class="btn" type="button" aria-expanded=move || diff.get().is_some().to_string() on:click=toggle_diff>
                        <Icon icon=Ic::Eye class="sm"/>
                        {move || if diff.get().is_some() { "Zurückgehaltene Änderungen ausblenden" } else { "Zurückgehaltene Änderungen ansehen" }}
                    </button>
                })}
            </div>
            {move || diff.get().map(|d| view! { <DiffBlock diff=d/> })}
        </section>
    }
}

#[component]
fn DrawerQuestions(id: Uuid, questions: Vec<String>, waiting: bool) -> impl IntoView {
    let store = use_store();
    let answer = RwSignal::new(String::new());
    let input_id = uid("drawer-answer");
    let send = move || {
        let text = answer.get_untracked();
        if text.trim().is_empty() {
            return;
        }
        spawn_local(async move {
            match api::post_ok(&format!("/tasks/{id}/answer"), &Answer { answer: text }).await {
                Ok(()) => {
                    answer.set(String::new());
                    store.announce("Antwort gesendet, der Auftrag geht weiter");
                }
                Err(e) => store.toast_error("Antwort konnte nicht gesendet werden", &e),
            }
        });
    };
    view! {
        <Block title="Rückfragen des Agenten" tone=if waiting { "tone-warning" } else { "" }>
            <div class="box">
                <ul class="questions">{questions.into_iter().map(|q| view! { <li>{q}</li> }).collect_view()}</ul>
            </div>
            {waiting.then(|| view! {
                <label class="visually-hidden" for=input_id.clone()>"Deine Antwort"</label>
                <textarea
                    id=input_id
                    class="textarea"
                    rows="3"
                    placeholder="Deine Antwort … (Strg + Enter zum Senden)"
                    bind:value=answer
                    on:keydown=move |ev| if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) { ev.prevent_default(); send(); }
                ></textarea>
                <div><button class="btn primary" type="button" disabled=move || answer.with(|a| a.trim().is_empty()) on:click=move |_| send()>
                    <Icon icon=Ic::Send class="sm"/>"Antwort senden"
                </button></div>
            })}
        </Block>
    }
}

#[component]
fn EditForm(id: Uuid, ed: EditState, editing: RwSignal<Option<EditState>>) -> impl IntoView {
    let store = use_store();
    let busy = RwSignal::new(false);
    let EditState {
        title,
        prompt,
        context,
        limits,
        errors,
    } = ed;
    let save = move || {
        let lim = match limits.get_untracked().parse() {
            Ok(l) => l,
            Err(e) => {
                errors.set(e);
                return;
            }
        };
        busy.set(true);
        let body = UpdateTask {
            title: Some(title.get_untracked()),
            prompt: Some(prompt.get_untracked()),
            context: Some(context.get_untracked()),
            limits: lim.patch_fields(),
            ..Default::default()
        };
        spawn_local(async move {
            match api::patch_ok(&format!("/tasks/{id}"), &body).await {
                Ok(()) => {
                    editing.set(None);
                    store.toast("Auftrag gespeichert");
                }
                Err(e) => store.toast_error("Speichern fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };
    let (t_id, p_id, c_id) = (uid("edit-title"), uid("edit-prompt"), uid("edit-context"));
    let title_input: NodeRef<html::Input> = NodeRef::new();
    Effect::new(move |_| {
        if let Some(el) = title_input.get() {
            let _ = el.focus();
        }
    });
    let on_key = move |ev: leptos::ev::KeyboardEvent| {
        if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) {
            ev.prevent_default();
            save();
        }
    };
    view! {
        <form class="stack" on:submit=move |ev| { ev.prevent_default(); save(); }>
            <h3>"Auftrag bearbeiten"</h3>
            <div class="field">
                <div class="label"><label for=t_id.clone()>"Titel"</label></div>
                <input id=t_id class="input" node_ref=title_input bind:value=title on:keydown=on_key/>
            </div>
            <div class="field">
                <div class="label"><label for=p_id.clone()>"Auftrag"</label></div>
                <textarea id=p_id class="textarea" rows="8" bind:value=prompt on:keydown=on_key></textarea>
            </div>
            <div class="field">
                <div class="label"><label for=c_id.clone()>"Kontext"</label><HelpTip topic="Kontext" text=help::CONTEXT/></div>
                <textarea id=c_id class="textarea" rows="4" bind:value=context on:keydown=on_key></textarea>
            </div>
            <h4 class="row">"Limits dieses Auftrags" <HelpTip topic="Limits" text=help::LIMITS/></h4>
            <TaskLimitInputs form=limits errors=errors/>
            <div class="form-actions">
                <button class="btn primary" type="submit" disabled=move || busy.get()><Icon icon=Ic::Check class="sm"/>"Speichern"</button>
                <button class="btn ghost" type="button" on:click=move |_| editing.set(None)>"Verwerfen"</button>
            </div>
        </form>
    }
}

#[component]
fn DiffBlock(diff: String) -> impl IntoView {
    if diff.trim().is_empty() {
        return view! { <Block title="Änderungen"><p class="box muted">"Keine Änderungen."</p></Block> }.into_any();
    }
    let stats = file_stats(&diff);
    view! {
        <Block title="Änderungen">
            <ul class="diff-files" style="list-style:none; margin:0; padding:0">
                {stats.into_iter().map(|f| view! {
                    <li>
                        <Icon icon=Ic::File class="sm"/>
                        <span class="mono truncate">{f.path}</span>
                        <span class="add">{format!("+{}", f.added)}</span>
                        <span class="del">{format!("−{}", f.removed)}</span>
                    </li>
                }).collect_view()}
            </ul>
            <pre class="diff" tabindex="0" aria-label="Unterschiede im Detail">
                {diff.lines().map(|l| {
                    let class = classify(l).class();
                    view! { <div class=class>{if l.is_empty() { " ".to_string() } else { l.to_string() }}</div> }
                }).collect_view()}
            </pre>
        </Block>
    }
    .into_any()
}

#[component]
fn LogBlock(t: Memo<Option<Task>>, events: RwSignal<Vec<TaskEvent>>) -> impl IntoView {
    let store = use_store();
    let log: NodeRef<html::Div> = NodeRef::new();
    let open = RwSignal::new(false);
    Effect::new(move |_| {
        let _ = events.with(Vec::len);
        if let Some(el) = log.get() {
            if el.scroll_height() - el.scroll_top() - el.client_height() < 160 {
                el.set_scroll_top(el.scroll_height());
            }
        }
    });
    let running = move || {
        t.with(|t| {
            t.as_ref()
                .map(|t| t.status_kind() == TaskStatus::Running)
                .unwrap_or(false)
        })
    };
    view! {
        <details class="block" on:toggle=move |ev| {
            use wasm_bindgen::JsCast;
            if let Some(d) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlDetailsElement>().ok()) {
                open.set(d.open());
            }
        }>
            <summary>
                <Icon icon=Ic::ChevronRight class="sm chev"/>
                {move || if open.get() { "Protokoll ausblenden" } else { "Protokoll anzeigen" }}
                <span class="tag">{move || events.with(Vec::len)}</span>
                {move || running().then(|| view! { <span class="pill tone-run"><span class="dot"></span>"live"</span> })}
            </summary>
            <div class="log" node_ref=log tabindex="0" aria-label="Protokoll">
                {move || events.with(Vec::is_empty).then(|| view! { <p class="muted">"Noch keine Einträge."</p> })}
                <For
                    each=move || events.get()
                    key=|e| e.id
                    children=move |e| view! {
                        <div class=format!("ev ev-{}", e.kind)>
                            <span class="ts">{fmt_time(e.ts, store.now.get_untracked(), &tz())}</span>
                            <span class="kind">{event_kind_label(&e.kind)}</span>
                            <span class="content">{e.content}</span>
                        </div>
                    }
                />
            </div>
        </details>
    }
}
