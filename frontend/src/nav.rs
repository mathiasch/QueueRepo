//! Navigation helpers: routes and the task drawer (`?task=<id>`).
use std::cell::Cell;

use leptos_router::{hooks::use_navigate, NavigateOptions};
use uuid::Uuid;

thread_local! {
    /// The drawer was opened inside the app (so "close" can go back in history).
    static OPENED_IN_APP: Cell<bool> = const { Cell::new(false) };
}

fn location() -> (String, String) {
    let loc = web_sys::window().map(|w| w.location());
    let path = loc
        .as_ref()
        .and_then(|l| l.pathname().ok())
        .unwrap_or_else(|| "/".into());
    let search = loc
        .as_ref()
        .and_then(|l| l.search().ok())
        .unwrap_or_default();
    (path, search)
}

/// Current URL with one query parameter set or removed.
pub fn with_query(key: &str, value: Option<&str>) -> String {
    let (path, search) = location();
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok();
    if let Some(p) = &params {
        match value {
            Some(v) => p.set(key, v),
            None => p.delete(key),
        }
    }
    let q = params
        .map(|p| String::from(p.to_string()))
        .unwrap_or_default();
    if q.is_empty() {
        path
    } else {
        format!("{path}?{q}")
    }
}

pub fn query_param(key: &str) -> Option<String> {
    let (_, search) = location();
    web_sys::UrlSearchParams::new_with_str(&search)
        .ok()?
        .get(key)
}

/// `open(task_id)` opens the drawer for a task.
pub fn use_open_task() -> impl Fn(Uuid) + Clone + 'static {
    let navigate = use_navigate();
    move |id: Uuid| {
        let already_open = query_param("task").is_some();
        let url = with_query("task", Some(&id.to_string()));
        if !already_open {
            OPENED_IN_APP.with(|c| c.set(true));
        }
        navigate(
            &url,
            NavigateOptions {
                replace: already_open,
                scroll: false,
                ..Default::default()
            },
        );
    }
}

/// `close()` closes the drawer (back in history if the app opened it).
pub fn use_close_task() -> impl Fn() + Clone + 'static {
    let navigate = use_navigate();
    move || {
        if OPENED_IN_APP.with(|c| c.replace(false)) {
            if let Some(h) = web_sys::window().and_then(|w| w.history().ok()) {
                if h.back().is_ok() {
                    return;
                }
            }
        }
        let url = with_query("task", None);
        navigate(
            &url,
            NavigateOptions {
                replace: true,
                scroll: false,
                ..Default::default()
            },
        );
    }
}

pub fn set_title(page: &str) {
    if let Some(d) = web_sys::window().and_then(|w| w.document()) {
        d.set_title(&format!("{page} · Queue"));
    }
}
