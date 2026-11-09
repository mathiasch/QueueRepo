//! "Aufträge": onboarding, composer, running work, questions/approvals,
//! queue and history.
use leptos::prelude::*;
use queued_api::{Answer, MoveTask, Task, TaskStatus, MOVE_DOWN, MOVE_TOP, MOVE_UP};

use crate::api;
use crate::logic::format::{duration, fmt_relative, plural};
use crate::logic::labels::{provider_label, status_help, status_label, Tone};
use crate::logic::progress::Phase;
use crate::logic::task::{failure_tag, finished_key, first_line, retry_hint};
use crate::nav::{set_title, use_open_task};
use crate::store::{tz, use_store, Store, ToastAction};
use crate::ui::dialog::Confirm;
use crate::ui::{HelpTip, Ic, Icon, StatusIcon, StatusPill, Tag};

use super::composer::Composer;
use super::onboarding::{Onboarding, OnboardingState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoryFilter {
    All,
    Done,
    Problems,
}

/// Key that changes whenever a row must be re-rendered.
fn row_key(t: &Task) -> String {
    format!(
        "{}-{}-{}-{}-{:?}",
        t.id,
        t.status,
        t.updated_at.timestamp_micros(),
        t.position,
        t.merge_state
    )
}

#[component]
pub fn QueuePage() -> impl IntoView {
    set_title("Aufträge");
    let store = use_store();
    let filter = RwSignal::new(HistoryFilter::All);
    let by = move |pred: fn(TaskStatus) -> bool| {
        Memo::new(move |_| {
            store.tasks.with(|ts| {
                ts.iter()
                    .filter(|t| pred(t.status_kind()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    };
    let running = by(|s| s == TaskStatus::Running);
    let waiting_batch = by(|s| s == TaskStatus::WaitingBatch);
    let merging = Memo::new(move |_| {
        let mut m: Vec<Task> = store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.status_kind() == TaskStatus::Merging)
                .cloned()
                .collect()
        });
        m.sort_by_key(|t| t.merge_queued_at);
        m
    });
    let attention = by(TaskStatus::needs_user);
    let queued = Memo::new(move |_| {
        let mut q: Vec<Task> = store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.status_kind() == TaskStatus::Queued)
                .cloned()
                .collect()
        });
        q.sort_by_key(|t| t.position);
        q
    });
    let history = Memo::new(move |_| {
        let f = filter.get();
        let mut h: Vec<Task> = store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| {
                    let s = t.status_kind();
                    s.is_final()
                        && match f {
                            HistoryFilter::All => true,
                            HistoryFilter::Done => s == TaskStatus::Done,
                            HistoryFilter::Problems => s != TaskStatus::Done,
                        }
                })
                .cloned()
                .collect()
        });
        h.sort_by_key(|t| std::cmp::Reverse(finished_key(t)));
        h.truncate(100);
        h
    });
    let onboarding = OnboardingState::new(store);
    let has_editable = Memo::new(move |_| {
        store
            .projects
            .with(|ps| ps.iter().any(|p| p.role.can_edit()))
    });

    let summary = move || {
        let (r, q, a) = (
            running.with(Vec::len),
            queued.with(Vec::len),
            attention.with(Vec::len),
        );
        if r + q + a == 0 {
            return "Gerade ist nichts zu tun. Schreib einfach den nächsten Auftrag.".to_string();
        }
        let mut parts = vec![];
        if r > 0 {
            parts.push(format!("{r} {}", if r == 1 { "läuft" } else { "laufen" }));
        }
        if q > 0 {
            parts.push(format!("{q} {}", if q == 1 { "wartet" } else { "warten" }));
        }
        if a > 0 {
            parts.push(format!(
                "{a} {}",
                if a == 1 {
                    "braucht dich"
                } else {
                    "brauchen dich"
                }
            ));
        }
        parts.join(" · ")
    };

    view! {
        <div class="page-head">
            <div>
                <h1>"Aufträge"</h1>
                <p>{summary}</p>
            </div>
        </div>
        {move || (!store.loaded.get()).then(|| view! { <p class="muted" role="status">"Lade Aufträge …"</p> })}
        {move || onboarding.visible().then(|| view! { <Onboarding state=onboarding/> })}
        {move || has_editable.get().then(|| view! { <Composer/> })}

        {move || (!running.with(Vec::is_empty)).then(|| view! {
            <section class="section" aria-labelledby="sec-running">
                <div class="section-head">
                    <h2 id="sec-running">"Läuft gerade"</h2>
                </div>
                <For each=move || running.get() key=row_key children=move |t| view! { <RunningCard t=t/> }/>
            </section>
        })}

        {move || (!merging.with(Vec::is_empty)).then(|| view! {
            <section class="section" aria-labelledby="sec-merging">
                <div class="section-head">
                    <h2 id="sec-merging">"Wird übernommen" <span class="count">{move || merging.with(Vec::len)}</span></h2>
                    <HelpTip topic="Wird übernommen" text=status_help(TaskStatus::Merging)/>
                </div>
                <div class="list" data-testid="merging-list">
                    <For each=move || merging.get() key=row_key children=move |t| view! { <TaskRow t=t/> }/>
                </div>
            </section>
        })}

        {move || (!waiting_batch.with(Vec::is_empty)).then(|| view! {
            <section class="section" aria-labelledby="sec-batch">
                <div class="section-head">
                    <h2 id="sec-batch">"Beim Anbieter"</h2>
                    <HelpTip topic="Beim Anbieter" text=status_help(TaskStatus::WaitingBatch)/>
                </div>
                <For each=move || waiting_batch.get() key=row_key children=move |t| view! { <BatchCard t=t/> }/>
            </section>
        })}

        {move || (!attention.with(Vec::is_empty)).then(|| view! { <AttentionSection tasks=attention/> })}

        {move || store.loaded.get().then(|| view! {
            <section class="section" aria-labelledby="sec-queue">
                <div class="section-head">
                    <h2 id="sec-queue">"Warteschlange" <span class="count">{move || queued.with(Vec::len)}</span></h2>
                </div>
                <div class="list" data-testid="queue-list">
                    {move || queued.with(Vec::is_empty).then(|| view! {
                        <p class="empty-line">
                            "Keine wartenden Aufträge. Ist die Warteschlange leer, kann die Queue selbst Vorschläge machen (Einstellungen → Automatisch weiterarbeiten)."
                        </p>
                    })}
                    <For
                        each=move || { let q = queued.get(); let n = q.len(); q.into_iter().enumerate().map(move |(i, t)| (i, n, t)).collect::<Vec<_>>() }
                        key=|(i, n, t)| format!("{}-{i}-{n}", row_key(t))
                        children=move |(i, n, t)| view! { <TaskRow t=t index=i count=n/> }
                    />
                </div>
            </section>
            <section class="section" aria-labelledby="sec-history">
                <div class="section-head">
                    <h2 id="sec-history">"Verlauf"</h2>
                    <div class="segmented" role="group" aria-label="Verlauf filtern">
                        {[(HistoryFilter::All, "Alle"), (HistoryFilter::Done, "Erledigt"), (HistoryFilter::Problems, "Probleme")]
                            .into_iter()
                            .map(|(f, label)| view! {
                                <button type="button" aria-pressed=move || (filter.get() == f).to_string() on:click=move |_| filter.set(f)>
                                    {label}
                                </button>
                            })
                            .collect_view()}
                    </div>
                </div>
                <div class="list" data-testid="history-list">
                    {move || history.with(Vec::is_empty).then(|| view! {
                        <p class="empty-line">
                            {move || match filter.get() {
                                HistoryFilter::Problems => "Keine Probleme – alles lief glatt.",
                                _ => "Noch nichts erledigt. Fertige Aufträge erscheinen hier.",
                            }}
                        </p>
                    })}
                    <For each=move || history.get() key=row_key children=move |t| view! { <TaskRow t=t/> }/>
                </div>
            </section>
        })}
    }
}

fn kind_tags(t: &Task) -> impl IntoView {
    let mut tags = vec![];
    match t.kind.as_str() {
        "fix" => tags.push(view! { <Tag text="Reparatur" tone=Tone::Danger/> }.into_any()),
        "onempty" => tags.push(view! { <Tag text="Automatisch" tone=Tone::Info/> }.into_any()),
        _ if t.created_by_agent => tags.push(
            view! { <span class="tag tone-warning agent-tag" title="Von einem Agenten vorgeschlagen">"Vorschlag"</span> }.into_any(),
        ),
        _ => {}
    }
    if let Some((text, tone)) = failure_tag(t) {
        tags.push(view! { <Tag text=text tone=tone test_id="failure-tag"/> }.into_any());
    }
    tags
}

fn post(store: Store, t: &Task, path: &'static str, err: &'static str) {
    let id = t.id;
    store.run(err, async move {
        api::action(&format!("/tasks/{id}/{path}")).await
    });
}

fn move_task(store: Store, t: &Task, dir: &'static str) {
    let id = t.id;
    store.run("Verschieben fehlgeschlagen", async move {
        api::post_ok(
            &format!("/tasks/{id}/move"),
            &MoveTask {
                direction: dir.into(),
            },
        )
        .await
    });
}

/// Discard a waiting task, with "Wiederherstellen" instead of a dialog.
pub fn discard(store: Store, t: &Task) {
    let id = t.id;
    let title = t.title.clone();
    leptos::task::spawn_local(async move {
        match api::action(&format!("/tasks/{id}/cancel")).await {
            Ok(()) => store.toast_action(
                format!("„{}“ verworfen", first_line(&title, 60)),
                ToastAction {
                    label: "Wiederherstellen".into(),
                    run: Callback::new(move |_| {
                        store.run("Wiederherstellen fehlgeschlagen", async move {
                            api::action(&format!("/tasks/{id}/retry")).await
                        });
                        store.announce("Auftrag wieder am Ende der Warteschlange");
                    }),
                },
            ),
            Err(e) => store.toast_error("Verwerfen fehlgeschlagen", &e),
        }
    });
}

/// Cancel a running task after confirmation.
pub fn confirm_cancel_running(store: Store, t: &Task) {
    let id = t.id;
    store.ask(Confirm {
        title: "Laufenden Auftrag abbrechen?".into(),
        body: format!(
            "„{}“ wird sofort gestoppt. Bereits gemachte Änderungen bleiben als Zwischenstand erhalten, der Auftrag gilt als abgebrochen. Du kannst ihn später erneut starten.",
            first_line(&t.title, 80)
        ),
        confirm_label: "Auftrag abbrechen".into(),
        danger: true,
        on_confirm: Callback::new(move |_| {
            store.run("Abbrechen fehlgeschlagen", async move { api::action(&format!("/tasks/{id}/cancel")).await })
        }),
    });
}

#[component]
fn RowMeta(t: Task) -> impl IntoView {
    let store = use_store();
    let pid = t.project_id;
    let project = move || store.project_name(pid);
    let provider =
        crate::logic::costs::access_text(&t, &store.connectors.get_untracked(), provider_label);
    let tt = t.clone();
    view! {
        <div class="meta">
            {move || {
                let p = project();
                (!p.is_empty()).then(|| view! { <span>{p}</span><span class="sep" aria-hidden="true">"·"</span> })
            }}
            <span>{provider}</span>
            {move || {
                let now = store.now.get();
                let mut extra = vec![];
                if let Some(f) = tt.finished_at {
                    extra.push(fmt_relative(f, now, &tz()));
                }
                if let Some(h) = retry_hint(&tt, now, &tz()) {
                    extra.push(h);
                }
                if tt.status_kind() == TaskStatus::Merging {
                    if let Some(m) = tt.merge_state_kind() {
                        extra.push(m.label().to_string());
                    }
                }
                extra.into_iter().map(|e| view! { <span class="sep" aria-hidden="true">"·"</span><span>{e}</span> }).collect_view()
            }}
        </div>
    }
}

#[component]
pub fn TaskRow(
    t: Task,
    #[prop(optional)] index: Option<usize>,
    #[prop(optional)] count: Option<usize>,
) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let status = t.status_kind();
    let id = t.id;
    let summary = (status.is_final())
        .then(|| {
            t.summary
                .as_deref()
                .or(t.error.as_deref())
                .map(|s| first_line(s, 140))
        })
        .flatten();
    let class = if status.needs_user() {
        "item attention"
    } else {
        "item"
    };
    let actions = {
        let t = t.clone();
        let open_held = open.clone();
        move || {
            let mut out: Vec<AnyView> = vec![];
            if status == TaskStatus::Queued {
                if let (Some(i), Some(n)) = (index, count) {
                    let (a, b, c) = (t.clone(), t.clone(), t.clone());
                    out.push(view! {
                        <button class="btn icon-only" type="button" title="Ganz nach oben" aria-label="Ganz nach oben" disabled={i == 0} on:click=move |_| move_task(store, &a, MOVE_TOP)>
                            <Icon icon=Ic::ArrowTop/>
                        </button>
                        <button class="btn icon-only" type="button" title="Nach oben" aria-label="Nach oben" disabled={i == 0} on:click=move |_| move_task(store, &b, MOVE_UP)>
                            <Icon icon=Ic::ArrowUp/>
                        </button>
                        <button class="btn icon-only" type="button" title="Nach unten" aria-label="Nach unten" disabled={i + 1 >= n} on:click=move |_| move_task(store, &c, MOVE_DOWN)>
                            <Icon icon=Ic::ArrowDown/>
                        </button>
                    }.into_any());
                }
            }
            if status == TaskStatus::PendingApproval {
                let a = t.clone();
                out.push(view! {
                    <button class="btn sm primary" type="button" on:click=move |_| post(store, &a, "approve", "Freigabe fehlgeschlagen")>
                        <Icon icon=Ic::Check class="sm"/>"Freigeben"
                    </button>
                }.into_any());
            }
            if status == TaskStatus::Held {
                let open = open_held.clone();
                out.push(
                    view! {
                        <button class="btn sm primary" type="button" on:click=move |_| open(id)>
                            <Icon icon=Ic::Shield class="sm"/>"Entscheiden"
                        </button>
                    }
                    .into_any(),
                );
            }
            if matches!(status, TaskStatus::Queued | TaskStatus::PendingApproval) {
                let a = t.clone();
                out.push(view! {
                    <button class="btn icon-only danger" type="button" title="Verwerfen" aria-label="Verwerfen" on:click=move |_| discard(store, &a)>
                        <Icon icon=Ic::Trash/>
                    </button>
                }.into_any());
            }
            if status.can_retry() {
                let a = t.clone();
                out.push(view! {
                    <button class="btn icon-only" type="button" title="Erneut versuchen" aria-label="Erneut versuchen" on:click=move |_| post(store, &a, "retry", "Erneut einreihen fehlgeschlagen")>
                        <Icon icon=Ic::Retry/>
                    </button>
                }.into_any());
            }
            out
        }
    };
    let title = t.title.clone();
    view! {
        <article class=class data-status=status.as_str() data-task=id.to_string()>
            <StatusIcon status=status/>
            <div class="item-body">
                <div class="item-title">
                    <button class="open" type="button" on:click=move |_| open(id) title=status_help(status)>
                        {title}
                    </button>
                </div>
                <div class="row wrap">
                    <span class="visually-hidden">{format!("Status: {}.", status_label(status))}</span>
                    {kind_tags(&t)}
                </div>
                <RowMeta t=t.clone()/>
                {summary.map(|s| view! { <p class="summary">{s}</p> })}
            </div>
            <div class="actions">{actions}</div>
        </article>
    }
}

#[component]
fn RunningCard(t: Task) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let id = t.id;
    let started = t.started_at;
    let phase = Memo::new(move |_| {
        store.phases.with(|p| {
            p.get(&id).copied().unwrap_or(if started.is_some() {
                Phase::Work
            } else {
                Phase::Prepare
            })
        })
    });
    let activity = Memo::new(move |_| {
        store
            .last_event
            .with(|m| m.get(&id).map(|e| first_line(&e.content, 160)))
    });
    let tc = t.clone();
    view! {
        <article class="card running-card" data-status="running" data-task=id.to_string()>
            <div class="head">
                <StatusIcon status=TaskStatus::Running/>
                <div class="item-body grow">
                    <button class="open" type="button" on:click=move |_| open(id)>{t.title.clone()}</button>
                    <div class="row wrap">{kind_tags(&t)}</div>
                    <RowMeta t=t.clone()/>
                </div>
                <span class="pill tone-run" aria-label="Laufzeit">
                    <Icon icon=Ic::Clock class="sm"/>
                    {move || duration(started, None, store.now.get())}
                </span>
                <button class="btn icon-only danger" type="button" title="Abbrechen" aria-label="Abbrechen" on:click=move |_| confirm_cancel_running(store, &tc)>
                    <Icon icon=Ic::X/>
                </button>
            </div>
            <Steps phase=phase.into()/>
            {move || activity.get().map(|a| view! {
                <p class="activity live"><Icon icon=Ic::Terminal class="sm"/><span>{a}</span></p>
            })}
        </article>
    }
}

/// A task whose plan/review/analysis step waits for the provider.
#[component]
fn BatchCard(t: Task) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let id = t.id;
    let since = t.updated_at;
    let activity = Memo::new(move |_| {
        store
            .last_event
            .with(|m| m.get(&id).map(|e| first_line(&e.content, 160)))
    });
    let tc = t.clone();
    view! {
        <article class="card running-card" data-status="waiting_batch" data-task=id.to_string()>
            <div class="head">
                <StatusIcon status=TaskStatus::WaitingBatch/>
                <div class="item-body grow">
                    <button class="open" type="button" on:click=move |_| open(id)>{t.title.clone()}</button>
                    <div class="row wrap">{kind_tags(&t)}<StatusPill status=TaskStatus::WaitingBatch/></div>
                    <RowMeta t=t.clone()/>
                </div>
                <span class="pill tone-info" aria-label="Wartezeit">
                    <Icon icon=Ic::Clock class="sm"/>
                    {move || duration(Some(since), None, store.now.get())}
                </span>
                <button class="btn icon-only danger" type="button" title="Abbrechen" aria-label="Abbrechen" on:click=move |_| confirm_cancel_running(store, &tc)>
                    <Icon icon=Ic::X/>
                </button>
            </div>
            {move || activity.get().map(|a| view! {
                <p class="activity"><Icon icon=Ic::Info class="sm"/><span>{a}</span></p>
            })}
        </article>
    }
}

/// Progress steps of a running task.
#[component]
pub fn Steps(phase: Signal<Phase>) -> impl IntoView {
    view! {
        <ol class="steps" aria-label="Fortschritt">
            {Phase::ALL.into_iter().map(|p| {
                let state = move || {
                    let cur = phase.get();
                    if p < cur { "done" } else if p == cur { "current" } else { "" }
                };
                view! {
                    <li class=state aria-current=move || (phase.get() == p).then_some("step") title=p.help()>
                        <span class="bar" aria-hidden="true"></span>
                        <span>
                            {p.label()}
                            <span class="visually-hidden">
                                {move || match state() { "done" => " (erledigt)", "current" => " (aktuell)", _ => "" }}
                            </span>
                        </span>
                    </li>
                }
            }).collect_view()}
        </ol>
    }
}

#[component]
fn AttentionSection(tasks: Memo<Vec<Task>>) -> impl IntoView {
    let store = use_store();
    let pending = Memo::new(move |_| {
        tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.status_kind() == TaskStatus::PendingApproval)
                .count()
        })
    });
    let approve_all = move |_| {
        store.run("Freigabe fehlgeschlagen", async move {
            api::action("/tasks/approve-all").await
        });
    };
    view! {
        <section class="section" aria-labelledby="sec-attention">
            <div class="section-head">
                <h2 id="sec-attention">"Wartet auf dich" <span class="count">{move || tasks.with(Vec::len)}</span></h2>
                {move || (pending.get() > 1).then(|| view! {
                    <button class="btn primary sm" type="button" on:click=approve_all>
                        <Icon icon=Ic::Check class="sm"/>
                        {format!("Alle {} freigeben", pending.get())}
                    </button>
                })}
            </div>
            <div class="list">
                <For
                    each=move || tasks.get()
                    key=row_key
                    children=move |t| {
                        if t.status_kind() == TaskStatus::WaitingInput {
                            view! { <QuestionRow t=t/> }.into_any()
                        } else {
                            view! { <TaskRow t=t/> }.into_any()
                        }
                    }
                />
            </div>
        </section>
    }
}

#[component]
pub fn QuestionRow(t: Task) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let answer = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let id = t.id;
    let input_id = crate::ui::uid("answer");
    let send = move || {
        let text = answer.get_untracked();
        if text.trim().is_empty() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            match api::post_ok(&format!("/tasks/{id}/answer"), &Answer { answer: text }).await {
                Ok(()) => {
                    answer.set(String::new());
                    store.announce("Antwort gesendet, der Auftrag geht weiter");
                }
                Err(e) => store.toast_error("Antwort konnte nicht gesendet werden", &e),
            }
            busy.set(false);
        });
    };
    view! {
        <article class="item attention" data-status="waiting_input" data-task=id.to_string()>
            <StatusIcon status=TaskStatus::WaitingInput/>
            <div class="item-body">
                <div class="item-title">
                    <button class="open" type="button" on:click=move |_| open(id)>{t.title.clone()}</button>
                </div>
                <p class="small muted">{plural(t.questions_list().len(), "Rückfrage", "Rückfragen")} " des Agenten:"</p>
                <ul class="questions">
                    {t.questions_list().into_iter().map(|q| view! { <li>{q}</li> }).collect_view()}
                </ul>
                <div class="answer">
                    <label class="visually-hidden" for=input_id.clone()>{format!("Antwort auf: {}", t.title)}</label>
                    <input
                        id=input_id
                        class="input"
                        placeholder="Deine Antwort …"
                        bind:value=answer
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" {
                                ev.prevent_default();
                                send();
                            }
                        }
                    />
                    <button class="btn primary" type="button" disabled=move || answer.with(|a| a.trim().is_empty()) || busy.get() on:click=move |_| send()>
                        <Icon icon=Ic::Send class="sm"/>"Antworten"
                    </button>
                </div>
            </div>
        </article>
    }
}
