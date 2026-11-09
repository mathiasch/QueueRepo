//! "Postfach": everything that waits for the user plus notifications.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::{Notification, TaskStatus};
use uuid::Uuid;

use crate::api;
use crate::logic::format::fmt_relative;
use crate::logic::labels::notification_is_error;
use crate::nav::{set_title, use_open_task};
use crate::store::{tz, use_store};
use crate::ui::{Empty, Ic, Icon};

use super::queue::{QuestionRow, TaskRow};

#[component]
pub fn InboxPage() -> impl IntoView {
    set_title("Postfach");
    let store = use_store();
    let open = use_open_task();
    let attention = Memo::new(move |_| {
        store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.status_kind().needs_user())
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let unread = Memo::new(move |_| {
        store
            .notifications
            .with(|ns| ns.iter().filter(|n| n.read_at.is_none()).count())
    });

    let mark_all = move |_| {
        spawn_local(async move {
            match api::action("/notifications/read-all").await {
                Ok(()) => {
                    let now = chrono::Utc::now();
                    store.notifications.update(|ns| {
                        ns.iter_mut().for_each(|n| {
                            n.read_at.get_or_insert(now);
                        })
                    });
                    store.announce("Alle Benachrichtigungen als gelesen markiert");
                }
                Err(e) => store.toast_error("Konnte nicht als gelesen markieren", &e),
            }
        });
    };
    let open_notification = move |n: Notification| {
        let id: Uuid = n.id;
        if n.read_at.is_none() {
            store.notifications.update(|ns| {
                if let Some(x) = ns.iter_mut().find(|x| x.id == id) {
                    x.read_at = Some(chrono::Utc::now());
                }
            });
            spawn_local(async move {
                let _ = api::action(&format!("/notifications/{id}/read")).await;
            });
        }
        if let Some(t) = n.task_id {
            open(t);
        }
    };

    view! {
        <div class="page-head">
            <div>
                <h1>"Postfach"</h1>
                <p>"Rückfragen, Freigaben und Neuigkeiten an einem Ort."</p>
            </div>
        </div>
        {move || (!attention.with(Vec::is_empty)).then(|| view! {
            <section class="section" aria-labelledby="inbox-attention">
                <div class="section-head">
                    <h2 id="inbox-attention">"Wartet auf dich" <span class="count">{move || attention.with(Vec::len)}</span></h2>
                </div>
                <div class="list">
                    <For
                        each=move || attention.get()
                        key=|t| format!("{}-{}", t.id, t.updated_at.timestamp_micros())
                        children=move |t| if t.status_kind() == TaskStatus::WaitingInput {
                            view! { <QuestionRow t=t/> }.into_any()
                        } else {
                            view! { <TaskRow t=t/> }.into_any()
                        }
                    />
                </div>
            </section>
        })}
        <section class="section" aria-labelledby="inbox-notes">
            <div class="section-head">
                <h2 id="inbox-notes">"Benachrichtigungen" {move || (unread.get() > 0).then(|| view! { <span class="count">{format!("{} neu", unread.get())}</span> })}</h2>
                {move || (unread.get() > 0).then(|| view! {
                    <button class="btn sm" type="button" on:click=mark_all><Icon icon=Ic::Check class="sm"/>"Alle als gelesen markieren"</button>
                })}
            </div>
            {move || if store.notifications.with(Vec::is_empty) {
                view! {
                    <div class="card">
                        <Empty icon=Ic::Bell title="Noch keine Benachrichtigungen" text="Hier erfährst du, wenn ein Auftrag fertig ist, eine Rückfrage hat oder Hilfe braucht. Auf Wunsch auch per Push aufs Handy (Einstellungen)."/>
                    </div>
                }.into_any()
            } else {
                let open_notification = open_notification.clone();
                view! {
                    <div class="list" data-testid="notifications">
                        <For
                            each=move || store.notifications.get()
                            key=|n| format!("{}-{}", n.id, n.read_at.is_some())
                            children=move |n| {
                                let open_notification = open_notification.clone();
                                let unread = n.read_at.is_none();
                                let error = notification_is_error(&n.kind);
                                let icon = if error { Ic::Alert } else if n.kind == "done" { Ic::CheckCircle } else if n.kind == "question" { Ic::Message } else if n.kind == "gate" { Ic::Shield } else { Ic::Bell };
                                let tone = if error { "tone-danger" } else if n.kind == "done" { "tone-success" } else if n.kind == "gate" { "tone-warning" } else if unread { "tone-info" } else { "tone-neutral" };
                                let created = n.created_at;
                                let (title, body, kind) = (n.title.clone(), n.body.clone(), n.kind.clone());
                                view! {
                                    <article class="item notification" data-unread=unread.to_string() data-kind=kind>
                                        <span class=format!("status-icon {tone}")><Icon icon=icon class="sm"/></span>
                                        <div class="item-body">
                                            <div class="item-title">
                                                <button class="open" type="button" on:click=move |_| open_notification(n.clone())>
                                                    {unread.then(|| view! { <span class="visually-hidden">"Neu: "</span> })}
                                                    {title}
                                                </button>
                                                {unread.then(|| view! { <span class="badge" aria-hidden="true">"neu"</span> })}
                                            </div>
                                            {(!body.is_empty()).then(|| view! { <p class="summary pre">{body}</p> })}
                                        </div>
                                        <span class="xs muted" style="white-space:nowrap">{move || fmt_relative(created, store.now.get(), &tz())}</span>
                                    </article>
                                }
                            }
                        />
                    </div>
                }.into_any()
            }}
        </section>
    }
}
