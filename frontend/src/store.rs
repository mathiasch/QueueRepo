//! Global client state: data from the API, live updates (SSE), toasts,
//! confirmations and the screen-reader live region.
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::{
    CredentialInfo, Meta, Notification, ProjectSummary, ProviderStatus, ServerEvent, Settings,
    Task, TaskDetail, TaskEvent, TaskStatus, User,
};
use uuid::Uuid;
use wasm_bindgen::{closure::Closure, JsCast};

use crate::api::{self, ApiError};
use crate::logic::labels::{notification_is_error, status_label};
use crate::logic::progress::{self, Phase};
use crate::ui::dialog::Confirm;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    Info,
    Success,
    Error,
}

#[derive(Clone)]
pub struct ToastAction {
    pub label: String,
    pub run: Callback<()>,
}

#[derive(Clone)]
pub struct Toast {
    pub id: u64,
    pub title: String,
    pub body: Option<String>,
    pub tone: ToastTone,
    pub task_id: Option<Uuid>,
    pub action: Option<ToastAction>,
}

#[derive(Clone, Copy)]
pub struct Store {
    pub me: RwSignal<User>,
    pub meta: RwSignal<Meta>,
    pub tasks: RwSignal<Vec<Task>>,
    pub projects: RwSignal<Vec<ProjectSummary>>,
    pub settings: RwSignal<Option<Settings>>,
    pub limits: RwSignal<Vec<ProviderStatus>>,
    pub notifications: RwSignal<Vec<Notification>>,
    pub credentials: RwSignal<Vec<CredentialInfo>>,
    /// Latest log line per task (shown in rows).
    pub last_event: RwSignal<HashMap<Uuid, TaskEvent>>,
    /// Progress step per running task.
    pub phases: RwSignal<HashMap<Uuid, Phase>>,
    pub toasts: RwSignal<Vec<Toast>>,
    /// Text of the polite live region.
    pub announcement: RwSignal<String>,
    pub confirm: RwSignal<Option<Confirm>>,
    pub show_shortcuts: RwSignal<bool>,
    /// Incremented to ask the composer to take the focus.
    pub focus_composer: RwSignal<u32>,
    /// Ticks every second (durations, retry hints).
    pub now: RwSignal<DateTime<Utc>>,
    /// First full load finished.
    pub loaded: RwSignal<bool>,
    /// Open task drawers receive log lines here (deduplicated by id).
    sinks: StoredValue<HashMap<Uuid, RwSignal<Vec<TaskEvent>>>>,
    next_toast: StoredValue<u64>,
}

pub fn use_store() -> Store {
    expect_context::<Store>()
}

/// Local time zone of the browser.
pub fn tz() -> Local {
    Local
}

impl Store {
    pub fn new(me: User, meta: Meta) -> Self {
        Self {
            me: RwSignal::new(me),
            meta: RwSignal::new(meta),
            tasks: RwSignal::new(vec![]),
            projects: RwSignal::new(vec![]),
            settings: RwSignal::new(None),
            limits: RwSignal::new(vec![]),
            notifications: RwSignal::new(vec![]),
            credentials: RwSignal::new(vec![]),
            last_event: RwSignal::new(HashMap::new()),
            phases: RwSignal::new(HashMap::new()),
            toasts: RwSignal::new(vec![]),
            announcement: RwSignal::new(String::new()),
            confirm: RwSignal::new(None),
            show_shortcuts: RwSignal::new(false),
            focus_composer: RwSignal::new(0),
            now: RwSignal::new(Utc::now()),
            loaded: RwSignal::new(false),
            sinks: StoredValue::new(HashMap::new()),
            next_toast: StoredValue::new(1),
        }
    }

    // ------------------------------------------------------------ loading

    pub fn reload(self) {
        spawn_local(async move {
            if let Ok(t) = api::get::<Vec<Task>>("/tasks").await {
                self.set_tasks(t);
            }
            if let Ok(n) = api::get::<Vec<Notification>>("/notifications").await {
                self.notifications.set(n);
            }
            self.reload_projects().await;
            self.reload_settings().await;
            self.reload_limits().await;
            self.reload_credentials().await;
            self.loaded.set(true);
        });
    }

    fn set_tasks(self, tasks: Vec<Task>) {
        let running: Vec<Uuid> = tasks
            .iter()
            .filter(|t| t.status_kind() == TaskStatus::Running)
            .map(|t| t.id)
            .collect();
        self.tasks.set(tasks);
        // Progress of tasks that were already running before this page loaded.
        for id in running {
            if self.phases.with_untracked(|p| p.contains_key(&id)) {
                continue;
            }
            spawn_local(async move {
                if let Ok(d) = api::get::<TaskDetail>(&format!("/tasks/{id}")).await {
                    let phase = progress::from_events(
                        d.events
                            .iter()
                            .map(|e| (e.kind.as_str(), e.content.as_str())),
                    );
                    self.phases.update(|p| {
                        p.entry(id).or_insert(phase);
                    });
                    if let Some(last) = d.events.last() {
                        self.last_event.update(|m| {
                            m.entry(id).or_insert_with(|| last.clone());
                        });
                    }
                }
            });
        }
    }

    pub async fn reload_projects(self) {
        if let Ok(p) = api::get::<Vec<ProjectSummary>>("/projects").await {
            self.projects.set(p);
        }
    }

    pub async fn reload_settings(self) {
        if let Ok(s) = api::get::<Settings>("/settings").await {
            self.settings.set(Some(s));
        }
    }

    pub async fn reload_limits(self) {
        if let Ok(l) = api::get::<Vec<ProviderStatus>>("/limits").await {
            self.limits.set(l);
        }
    }

    pub async fn reload_credentials(self) {
        if let Ok(c) = api::get::<Vec<CredentialInfo>>("/credentials").await {
            self.credentials.set(c);
        }
    }

    // ------------------------------------------------------------ live

    /// Applies one SSE message.
    pub fn apply(self, ev: ServerEvent) {
        match ev {
            ServerEvent::Task { task } => self.upsert_task(*task),
            ServerEvent::TaskEvent { task_id, event } => {
                self.phases.update(|p| {
                    let cur = p.get(&task_id).copied().unwrap_or(Phase::Prepare);
                    p.insert(task_id, progress::advance(cur, &event.kind, &event.content));
                });
                if let Some(sink) = self.sinks.with_value(|s| s.get(&task_id).copied()) {
                    sink.update(|evs| push_event(evs, event.clone()));
                }
                self.last_event.update(|m| {
                    m.insert(task_id, event);
                });
            }
            ServerEvent::Notification { notification: n } => {
                let tone = if notification_is_error(&n.kind) {
                    ToastTone::Error
                } else {
                    ToastTone::Info
                };
                self.toast_full(
                    n.title.clone(),
                    (!n.body.is_empty()).then(|| n.body.clone()),
                    tone,
                    n.task_id,
                    None,
                );
                self.notifications.update(|ns| {
                    ns.insert(
                        0,
                        Notification {
                            id: n.id,
                            kind: n.kind,
                            title: n.title,
                            body: n.body,
                            task_id: n.task_id,
                            read_at: None,
                            created_at: Utc::now(),
                        },
                    )
                });
            }
            ServerEvent::Resync => self.reload(),
        }
    }

    fn upsert_task(self, t: Task) {
        let status = t.status_kind();
        let previous = self
            .tasks
            .with_untracked(|ts| ts.iter().find(|x| x.id == t.id).map(|x| x.status_kind()));
        if previous != Some(status) {
            self.announce(format!("{}: {}", t.title, status_label(status)));
            if status == TaskStatus::Running {
                self.phases.update(|p| {
                    p.insert(t.id, Phase::Prepare);
                });
            } else if previous == Some(TaskStatus::Running) {
                self.phases.update(|p| {
                    p.remove(&t.id);
                });
            }
        }
        if status != TaskStatus::Running {
            spawn_local(async move { self.reload_limits().await });
        }
        self.tasks
            .update(|ts| match ts.iter_mut().find(|x| x.id == t.id) {
                Some(x) => *x = t,
                None => ts.push(t),
            });
    }

    /// Registers the event list of an open drawer for live log lines.
    pub fn attach_sink(self, task: Uuid, sink: RwSignal<Vec<TaskEvent>>) {
        self.sinks.update_value(|s| {
            s.insert(task, sink);
        });
    }

    pub fn detach_sink(self, task: Uuid) {
        self.sinks.update_value(|s| {
            s.remove(&task);
        });
    }

    // ------------------------------------------------------------ feedback

    pub fn announce(self, text: impl Into<String>) {
        let text = text.into();
        // Clear first so repeating the same text is announced again.
        self.announcement.set(String::new());
        set_timeout(
            move || self.announcement.set(text),
            Duration::from_millis(60),
        );
    }

    pub fn toast(self, title: impl Into<String>) {
        self.toast_full(title.into(), None, ToastTone::Success, None, None);
    }

    pub fn toast_error(self, title: impl Into<String>, e: &ApiError) {
        self.toast_full(
            title.into(),
            Some(e.message.clone()),
            ToastTone::Error,
            None,
            None,
        );
    }

    pub fn toast_action(self, title: impl Into<String>, action: ToastAction) {
        self.toast_full(title.into(), None, ToastTone::Info, None, Some(action));
    }

    pub fn toast_full(
        self,
        title: String,
        body: Option<String>,
        tone: ToastTone,
        task_id: Option<Uuid>,
        action: Option<ToastAction>,
    ) {
        let id = self.next_toast.get_value();
        self.next_toast.set_value(id + 1);
        self.toasts.update(|ts| {
            if ts.len() >= 4 {
                ts.remove(0);
            }
            ts.push(Toast {
                id,
                title,
                body,
                tone,
                task_id,
                action,
            });
        });
        let ms = if tone == ToastTone::Error {
            10_000
        } else {
            7_000
        };
        set_timeout(move || self.dismiss_toast(id), Duration::from_millis(ms));
    }

    pub fn dismiss_toast(self, id: u64) {
        self.toasts.update(|ts| ts.retain(|t| t.id != id));
    }

    pub fn ask(self, c: Confirm) {
        self.confirm.set(Some(c));
    }

    /// Runs an API call and shows an error toast on failure.
    pub fn run<F>(self, error_title: &'static str, fut: F)
    where
        F: std::future::Future<Output = Result<(), ApiError>> + 'static,
    {
        spawn_local(async move {
            if let Err(e) = fut.await {
                self.toast_error(error_title, &e);
            }
        });
    }

    // ------------------------------------------------------------ derived

    pub fn task(self, id: Uuid) -> Option<Task> {
        self.tasks
            .with(|ts| ts.iter().find(|t| t.id == id).cloned())
    }

    pub fn project_name(self, id: Uuid) -> String {
        self.projects.with(|ps| {
            ps.iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .unwrap_or_default()
        })
    }
}

/// Appends a log line unless it is already there (fetch and SSE overlap).
pub fn push_event(evs: &mut Vec<TaskEvent>, e: TaskEvent) {
    if evs.iter().rev().take(50).any(|x| x.id == e.id) {
        return;
    }
    if evs.last().map(|l| l.id < e.id).unwrap_or(true) {
        evs.push(e);
    } else {
        let pos = evs.partition_point(|x| x.id < e.id);
        evs.insert(pos, e);
    }
}

// ---------------------------------------------------------------- SSE

struct Live {
    source: web_sys::EventSource,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Connects `/api/events`; on error reconnects after 3 s and reloads everything.
pub fn connect_live(store: Store) {
    let Ok(source) = web_sys::EventSource::new("/api/events") else {
        return;
    };
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |m: web_sys::MessageEvent| {
            if let Some(text) = m.data().as_string() {
                match serde_json::from_str::<ServerEvent>(&text) {
                    Ok(ev) => store.apply(ev),
                    Err(e) => leptos::logging::warn!("unknown live event: {e}"),
                }
            }
        });
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        LIVE.with(|l| {
            if let Some(live) = l.borrow_mut().take() {
                live.source.close();
                // Closures must outlive this call: drop them after the reconnect delay.
                set_timeout(
                    move || {
                        drop(live);
                        store.reload();
                        connect_live(store);
                    },
                    Duration::from_secs(3),
                );
            }
        });
    });
    source.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    LIVE.with(|l| {
        *l.borrow_mut() = Some(Live {
            source,
            _on_message: on_message,
            _on_error: on_error,
        })
    });
}

/// Clock tick and periodic limit refresh.
pub fn start_timers(store: Store) {
    let _ = set_interval_with_handle(move || store.now.set(Utc::now()), Duration::from_secs(1));
    let _ = set_interval_with_handle(
        move || spawn_local(async move { store.reload_limits().await }),
        Duration::from_secs(60),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: i64) -> TaskEvent {
        TaskEvent {
            id,
            task_id: Uuid::nil(),
            ts: Utc::now(),
            kind: "tool".into(),
            content: format!("{id}"),
        }
    }

    #[test]
    fn events_are_deduplicated_and_ordered() {
        let mut v = vec![ev(1), ev(2)];
        push_event(&mut v, ev(2));
        push_event(&mut v, ev(4));
        push_event(&mut v, ev(3));
        let ids: Vec<i64> = v.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }
}
