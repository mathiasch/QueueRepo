//! Branch per task and merge queue (WP04): the branch block of the task
//! drawer and the project's merge queue.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::branch::{MergeQueueEntry, MergeState};
use queued_api::{Task, TaskStatus};
use uuid::Uuid;

use crate::api;
use crate::logic::branch::{branch_lines, merge_tone, position_text};
use crate::logic::format::fmt_relative;
use crate::logic::labels::help;
use crate::nav::use_open_task;
use crate::store::{tz, use_store};
use crate::ui::{HelpTip, Ic, Icon, Tag};

/// Loads the merge queue of `project` again whenever a task of the project
/// enters, moves in or leaves it (SSE task updates).
fn merge_queue(project: Uuid) -> RwSignal<Option<Vec<MergeQueueEntry>>> {
    let store = use_store();
    let entries = RwSignal::new(None::<Vec<MergeQueueEntry>>);
    let watched = Memo::new(move |_| {
        store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.project_id == project && t.status_kind() == TaskStatus::Merging)
                .map(|t| format!("{}:{:?}", t.id, t.merge_state))
                .collect::<Vec<_>>()
        })
    });
    Effect::new(move |_| {
        watched.track();
        spawn_local(async move {
            if let Ok(e) =
                api::get::<Vec<MergeQueueEntry>>(&format!("/projects/{project}/merge-queue")).await
            {
                entries.set(Some(e));
            }
        });
    });
    entries
}

/// Drawer: branch, fork point, merge state and place in the merge queue.
#[component]
pub fn BranchBlock(t: Task) -> impl IntoView {
    let Some(lines) = branch_lines(&t) else {
        return ().into_any();
    };
    let state = t.merge_state_kind();
    let merging = t.status_kind() == TaskStatus::Merging;
    let id = t.id;
    let queue = merging.then(|| merge_queue(t.project_id));
    let place = move || {
        queue.and_then(|q| {
            q.with(|e| {
                e.as_ref()
                    .and_then(|v| v.iter().find(|x| x.task_id == id))
                    .map(|x| position_text(x.state, x.position))
            })
        })
    };
    let open = use_open_task();
    let carry = t.carry_from;
    view! {
        <section class="block" data-testid="branch-block">
            <h3>"Branch und Übernahme"<HelpTip topic="Branch und Übernahme" text=help::BRANCH align="align-start"/></h3>
            <div class="box">
                {state.map(|m| view! {
                    <p class="row wrap">
                        <Tag text=m.label() tone=merge_tone(m) test_id="merge-state"/>
                        {move || place().map(|p| view! { <span class="small" data-testid="merge-position">{p}</span> })}
                    </p>
                    <p class="small muted">{m.help()}</p>
                })}
                <ul class="plain small">
                    {lines.into_iter().map(|l| view! { <li class="mono-wrap">{l}</li> }).collect_view()}
                </ul>
                {carry.map(|c| view! {
                    <button class="btn sm" type="button" on:click=move |_| open(c)>
                        <Icon icon=Ic::Branch class="sm"/>"Ursprünglichen Auftrag öffnen"
                    </button>
                })}
            </div>
        </section>
    }
    .into_any()
}

/// Project → Verlauf: tasks waiting for (or in) their merge, in order.
#[component]
pub fn MergeQueueCard(project: Uuid) -> impl IntoView {
    let store = use_store();
    let entries = merge_queue(project);
    let open = use_open_task();
    view! {
        <section class="card stack" aria-labelledby="mq-title" data-testid="merge-queue">
            <div class="row between wrap">
                <h2 id="mq-title" class="row" style="font-size: var(--fs-lg)">
                    <Icon icon=Ic::Branch class="sm"/>"Übernahme-Warteschlange"
                    <HelpTip topic="Übernahme-Warteschlange" text=help::MERGE_QUEUE align="align-start"/>
                </h2>
            </div>
            {move || match entries.get() {
                None => view! { <div class="list"><p class="empty-line">"Lade …"</p></div> }.into_any(),
                Some(e) if e.is_empty() => view! {
                    <div class="list">
                        <p class="empty-line">"Nichts wartet auf die Übernahme. Fertige Aufträge erscheinen hier kurz, bevor sie im Hauptstand landen."</p>
                    </div>
                }.into_any(),
                Some(e) => view! {
                    <ol class="list">
                        {e.into_iter().map(|x| {
                            let open = open.clone();
                            let task = x.task_id;
                            let when = x.queued_at;
                            view! {
                                <li class="item">
                                    <span class="commit-sha" aria-hidden="true">{x.position}</span>
                                    <div class="item-body">
                                        <button class="open" type="button" on:click=move |_| open(task)>{x.title.clone()}</button>
                                        <p class="row wrap small">
                                            <Tag text=position_text(x.state, x.position) tone=merge_tone(if x.state == MergeState::Merging { MergeState::Merging } else { MergeState::Queued })/>
                                            <span class="mono-wrap">{x.branch.clone()}</span>
                                            <span class="muted">{x.user.clone()}</span>
                                            {when.map(|w| view! { <span class="muted">{move || fmt_relative(w, store.now.get(), &tz())}</span> })}
                                        </p>
                                    </div>
                                </li>
                            }
                        }).collect_view()}
                    </ol>
                }.into_any(),
            }}
        </section>
    }
}
