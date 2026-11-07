//! App frame: navigation (sidebar / bottom tab bar), routes, drawer,
//! dialogs, toasts, live region and keyboard shortcuts.
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::{
    components::{Route, Router, Routes},
    hooks::{use_location, use_navigate, use_query_map},
    path, NavigateOptions,
};
use queued_api::{Meta, TaskStatus, User};
use uuid::Uuid;

use crate::api;
use crate::logic::budget::{chip_visible, limit_chip};
use crate::logic::shortcuts::{self, Action, Keys};
use crate::nav::use_open_task;
use crate::store::{connect_live, start_timers, tz, Store, ToastTone};
use crate::ui::dialog::{ConfirmDialog, Modal};
use crate::ui::{uid, Ic, Icon};

use super::{
    drawer::TaskDrawer, inbox::InboxPage, project::ProjectPage, projects::ProjectsPage,
    queue::QueuePage, settings::SettingsPage,
};

#[component]
pub fn Shell(meta: Meta, user: User) -> impl IntoView {
    let store = Store::new(user, meta);
    provide_context(store);
    store.reload();
    connect_live(store);
    start_timers(store);

    view! {
        <Router>
            <a class="skip-link" href="#main" on:click=skip_to_main>"Zum Inhalt springen"</a>
            <div class="app">
                <AppBar/>
                <main id="main" class="main" tabindex="-1">
                    <Routes fallback=|| view! { <NotFound/> }>
                        <Route path=path!("/") view=QueuePage/>
                        <Route path=path!("/inbox") view=InboxPage/>
                        <Route path=path!("/projects") view=ProjectsPage/>
                        <Route path=path!("/projects/:id") view=ProjectPage/>
                        <Route path=path!("/settings") view=SettingsPage/>
                    </Routes>
                </main>
            </div>
            <DrawerHost/>
            <ConfirmHost/>
            <ShortcutsHost/>
            <Toasts/>
            <div class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
                {move || store.announcement.get()}
            </div>
            <KeyboardShortcuts/>
        </Router>
    }
}

/// The router would treat `#main` as a navigation; move the focus directly.
fn skip_to_main(ev: leptos::ev::MouseEvent) {
    use wasm_bindgen::JsCast;
    ev.prevent_default();
    if let Some(main) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("main"))
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = main.focus();
        main.scroll_into_view();
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <div class="card">
            <crate::ui::Empty icon=Ic::Search title="Seite nicht gefunden" text="Diese Adresse gibt es nicht (mehr).">
                <a class="btn primary" href="/">"Zu den Aufträgen"</a>
            </crate::ui::Empty>
        </div>
    }
}

fn initials(name: &str, email: &str) -> String {
    let src = if name.trim().is_empty() { email } else { name };
    let mut it = src.split_whitespace().filter_map(|w| w.chars().next());
    let a = it.next().unwrap_or('?');
    match it.next() {
        Some(b) => format!("{a}{b}").to_uppercase(),
        None => a.to_uppercase().collect(),
    }
}

#[component]
fn AppBar() -> impl IntoView {
    let store = crate::store::use_store();
    let location = use_location();
    let navigate = use_navigate();
    let path = location.pathname;

    let running = Memo::new(move |_| {
        store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.status_kind() == TaskStatus::Running)
                .count()
        })
    });
    let active = Memo::new(move |_| {
        store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| matches!(t.status_kind(), TaskStatus::Running | TaskStatus::Queued))
                .count()
        })
    });
    let attention = Memo::new(move |_| {
        store
            .tasks
            .with(|ts| ts.iter().filter(|t| t.status_kind().needs_user()).count())
    });
    let unread = Memo::new(move |_| {
        store
            .notifications
            .with(|ns| ns.iter().filter(|n| n.read_at.is_none()).count())
    });

    let new_task = {
        let navigate = navigate.clone();
        move |_| {
            if path.get_untracked() != "/" {
                navigate("/", NavigateOptions::default());
            }
            store.focus_composer.update(|n| *n += 1);
        }
    };
    let new_task2 = new_task.clone();

    let tab = move |href: &'static str,
                    label: &'static str,
                    icon: Ic,
                    badge: Signal<(usize, bool, &'static str)>| {
        let current = move || {
            let p = path.get();
            let on = if href == "/" {
                p == "/"
            } else {
                p == href || p.starts_with(&format!("{href}/"))
            };
            on.then_some("page")
        };
        view! {
            <a class="tab" href=href aria-current=current>
                <Icon icon=icon/>
                <span class="tab-label">{label}</span>
                {move || {
                    let (n, warn, what) = badge.get();
                    (n > 0).then(|| view! {
                        <span class=if warn { "badge attention" } else { "badge" } aria-hidden="true">{n}</span>
                        <span class="visually-hidden">{format!(", {n} {what}")}</span>
                    })
                }}
            </a>
        }
    };

    let logout = move |_| {
        spawn_local(async move {
            let _ = api::action("/auth/logout").await;
            if let Some(w) = web_sys::window() {
                let _ = w.location().set_href("/");
            }
        });
    };

    view! {
        <header class="appbar">
            <a class="brand" href="/" aria-label="Queue – zu den Aufträgen">
                <span class="logo"><Icon icon=Ic::Logo/></span>
                "Queue"
                {move || (running.get() > 0).then(|| view! {
                    <span class="live" title=format!("{} läuft", running.get())></span>
                })}
            </a>
            <button class="btn primary lg new-task" type="button" on:click=new_task aria-keyshortcuts="n">
                <Icon icon=Ic::Plus/>
                "Neuer Auftrag"
            </button>
            <button class="btn primary icon-only new-task-mobile" type="button" aria-label="Neuer Auftrag" on:click=new_task2>
                <Icon icon=Ic::Plus/>
            </button>
            <nav class="tabs" aria-label="Hauptnavigation">
                {tab("/", "Aufträge", Ic::Queue, Signal::derive(move || (active.get(), false, "aktiv")))}
                {tab("/inbox", "Postfach", Ic::Inbox, Signal::derive(move || {
                    let a = attention.get();
                    (a + unread.get(), a > 0, "neu")
                }))}
                {tab("/projects", "Projekte", Ic::Folder, Signal::derive(|| (0, false, "")))}
                {tab("/settings", "Einstellungen", Ic::Settings, Signal::derive(|| (0, false, "")))}
            </nav>
            <LimitChips/>
            <div class="spacer"></div>
            <div class="account">
                <span class="avatar" aria-hidden="true">
                    {move || store.me.with(|u| initials(&u.name, &u.email))}
                </span>
                <span class="who">
                    <strong class="truncate">{move || store.me.with(|u| u.name.clone())}</strong>
                    <span class="truncate">{move || store.me.with(|u| u.email.clone())}</span>
                </span>
                <button class="btn icon-only" type="button" aria-label="Abmelden" title="Abmelden" on:click=logout>
                    <Icon icon=Ic::LogOut/>
                </button>
            </div>
        </header>
    }
}

#[component]
fn LimitChips() -> impl IntoView {
    let store = crate::store::use_store();
    let chips = Memo::new(move |_| {
        let now = store.now.get();
        store.limits.with(|ls| {
            ls.iter()
                .filter(|l| chip_visible(l))
                .map(|l| (l.provider.clone(), limit_chip(l, now, &tz())))
                .collect::<Vec<_>>()
        })
    });
    view! {
        <div class="limits" aria-label="Status der Anbieter">
            {move || {
                let list = chips.get();
                (!list.is_empty()).then(|| view! {
                    <span class="limits-title" aria-hidden="true">"Anbieter"</span>
                    {list.into_iter().map(|(p, c)| view! {
                        <a class=format!("chip {}", c.tone.class()) href="/settings#budgets" title=c.detail.clone() data-provider=p>
                            <Icon icon=Ic::Gauge class="sm"/>
                            {c.text}
                            <span class="visually-hidden">{format!(" – {}", c.detail)}</span>
                        </a>
                    }).collect_view()}
                })
            }}
        </div>
    }
}

#[component]
fn DrawerHost() -> impl IntoView {
    let query = use_query_map();
    let id =
        Memo::new(move |_| query.with(|q| q.get("task").and_then(|v| Uuid::parse_str(&v).ok())));
    move || id.get().map(|id| view! { <TaskDrawer id=id/> })
}

#[component]
fn ConfirmHost() -> impl IntoView {
    let store = crate::store::use_store();
    let close = Callback::new(move |_| store.confirm.set(None));
    move || {
        store
            .confirm
            .get()
            .map(|c| view! { <ConfirmDialog request=c on_close=close/> })
    }
}

#[component]
fn ShortcutsHost() -> impl IntoView {
    let store = crate::store::use_store();
    let close = Callback::new(move |_| store.show_shortcuts.set(false));
    move || {
        store.show_shortcuts.get().then(|| {
            let title = uid("shortcuts");
            view! {
                <Modal on_close=close labelledby=title.clone()>
                    <span class="dialog-icon tone-info"><Icon icon=Ic::Keyboard class="lg"/></span>
                    <h2 id=title>"Tastaturkürzel"</h2>
                    <dl class="shortcut-list">
                        {shortcuts::LIST.iter().map(|(k, what)| view! {
                            <dt>{k.split(' ').map(|p| view! { <kbd>{p.to_string()}</kbd>" " }).collect_view()}</dt>
                            <dd>{*what}</dd>
                        }).collect_view()}
                    </dl>
                    <div class="dialog-actions">
                        <button class="btn primary" type="button" data-autofocus on:click=move |_| close.run(())>"Verstanden"</button>
                    </div>
                </Modal>
            }
        })
    }
}

#[component]
fn Toasts() -> impl IntoView {
    let store = crate::store::use_store();
    let open = use_open_task();
    view! {
        <section class="toasts" aria-label="Meldungen" aria-live="polite">
            <For
                each=move || store.toasts.get()
                key=|t| t.id
                children=move |t| {
                    let open = open.clone();
                    let (icon, class) = match t.tone {
                        ToastTone::Error => (Ic::Alert, "toast error"),
                        ToastTone::Success => (Ic::CheckCircle, "toast success"),
                        ToastTone::Info => (Ic::Bell, "toast"),
                    };
                    let id = t.id;
                    let task = t.task_id;
                    let action = t.action.clone();
                    view! {
                        <div class=class role=(t.tone == ToastTone::Error).then_some("alert")>
                            <span class="toast-icon"><Icon icon=icon/></span>
                            <div class="toast-body">
                                <strong>{t.title.clone()}</strong>
                                {t.body.clone().map(|b| view! { <span class="text">{b}</span> })}
                            </div>
                            <div class="toast-actions">
                                {action.map(|a| view! {
                                    <button class="btn sm" type="button" on:click=move |_| {
                                        a.run.run(());
                                        store.dismiss_toast(id);
                                    }>{a.label.clone()}</button>
                                })}
                                {task.map(|tid| view! {
                                    <button class="btn sm" type="button" on:click=move |_| {
                                        open(tid);
                                        store.dismiss_toast(id);
                                    }>"Öffnen"</button>
                                })}
                                <button class="btn icon-only" type="button" aria-label="Meldung schließen" on:click=move |_| store.dismiss_toast(id)>
                                    <Icon icon=Ic::X class="sm"/>
                                </button>
                            </div>
                        </div>
                    }
                }
            />
        </section>
    }
}

fn typing_target(ev: &web_sys::KeyboardEvent) -> bool {
    use wasm_bindgen::JsCast;
    let Some(el) = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return false;
    };
    let tag = el.tag_name().to_ascii_lowercase();
    matches!(tag.as_str(), "input" | "textarea" | "select")
        || el.get_attribute("contenteditable").is_some()
}

#[component]
fn KeyboardShortcuts() -> impl IntoView {
    let store = crate::store::use_store();
    let navigate = use_navigate();
    let query = use_query_map();
    let keys = StoredValue::new(Keys::default());
    let handle = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.ctrl_key()
            || ev.meta_key()
            || ev.alt_key()
            || ev.default_prevented()
            || typing_target(&ev)
        {
            return;
        }
        // Modal dialogs and the drawer own the keyboard.
        if store.confirm.get_untracked().is_some()
            || store.show_shortcuts.get_untracked()
            || query.with_untracked(|q| q.get("task").is_some())
        {
            return;
        }
        let now = js_sys::Date::now();
        let action = keys.try_update_value(|k| k.press(&ev.key(), now)).flatten();
        let Some(action) = action else { return };
        ev.prevent_default();
        let go = |to: &str| navigate(to, NavigateOptions::default());
        match action {
            Action::NewTask => {
                go("/");
                store.focus_composer.update(|n| *n += 1);
            }
            Action::GoQueue => go("/"),
            Action::GoInbox => go("/inbox"),
            Action::GoProjects => go("/projects"),
            Action::GoSettings => go("/settings"),
            Action::Help => store.show_shortcuts.set(true),
        }
    });
    on_cleanup(move || handle.remove());
}

#[cfg(test)]
mod tests {
    use super::initials;

    #[test]
    fn avatar_initials() {
        assert_eq!(initials("Ada Lovelace", "a@b"), "AL");
        assert_eq!(initials("ada", "a@b"), "A");
        assert_eq!(initials("  ", "zoe@b"), "Z");
    }
}
