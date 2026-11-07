//! Modal dialogs: focus trap, Escape, focus restore, scroll lock.
use std::cell::Cell;

use leptos::{html, prelude::*};
use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;

use super::{uid, Ic, Icon};

const FOCUSABLE: &str = "button:not([disabled]), [href], input:not([disabled]):not([type=hidden]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex='-1'])";

fn focusables(root: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    let Ok(list) = root.query_selector_all(FOCUSABLE) else {
        return vec![];
    };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|n| n.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|e| e.offset_parent().is_some() || e.get_attribute("data-autofocus").is_some())
        .collect()
}

fn active_element() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.active_element()
}

/// Keeps Tab / Shift+Tab inside `root`.
pub fn trap_tab(ev: &web_sys::KeyboardEvent, root: &web_sys::Element) {
    if ev.key() != "Tab" {
        return;
    }
    let items = focusables(root);
    let (Some(first), Some(last)) = (items.first(), items.last()) else {
        ev.prevent_default();
        return;
    };
    let active = active_element();
    let inside = active
        .as_ref()
        .map(|a| root.contains(Some(a)))
        .unwrap_or(false);
    let is = |e: &web_sys::HtmlElement| {
        active
            .as_ref()
            .map(|a| a == e.unchecked_ref::<web_sys::Element>())
            .unwrap_or(false)
    };
    if ev.shift_key() && (is(first) || !inside) {
        ev.prevent_default();
        let _ = last.focus();
    } else if !ev.shift_key() && (is(last) || !inside) {
        ev.prevent_default();
        let _ = first.focus();
    }
}

/// Focuses `[data-autofocus]` or the first focusable element inside `root`.
pub fn focus_initial(root: &web_sys::Element) {
    if let Ok(Some(el)) = root.query_selector("[data-autofocus]") {
        if let Ok(h) = el.dyn_into::<web_sys::HtmlElement>() {
            let _ = h.focus();
            return;
        }
    }
    if let Some(f) = focusables(root).first() {
        let _ = f.focus();
    }
}

thread_local! {
    static OPEN_MODALS: Cell<u32> = const { Cell::new(0) };
}

fn set_scroll_lock(delta: i32) {
    OPEN_MODALS.with(|c| {
        let n = (c.get() as i32 + delta).max(0) as u32;
        c.set(n);
        if let Some(root) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let _ = if n > 0 {
                root.class_list().add_1("modal-open")
            } else {
                root.class_list().remove_1("modal-open")
            };
        }
    });
}

/// Remembers the focused element and restores it (and the scroll lock) on cleanup.
pub fn modal_lifecycle() {
    let previous = SendWrapper::new(active_element());
    set_scroll_lock(1);
    on_cleanup(move || {
        set_scroll_lock(-1);
        if let Some(Ok(h)) = previous
            .take()
            .map(|e| e.dyn_into::<web_sys::HtmlElement>())
        {
            if h.is_connected() {
                let _ = h.focus();
            }
        }
    });
}

#[component]
pub fn Modal(
    on_close: Callback<()>,
    /// Id of the heading that labels the dialog.
    #[prop(into)]
    labelledby: String,
    #[prop(optional, into)] describedby: Option<String>,
    #[prop(optional)] alert: bool,
    children: Children,
) -> impl IntoView {
    let node: NodeRef<html::Div> = NodeRef::new();
    modal_lifecycle();
    Effect::new(move |_| {
        if let Some(el) = node.get() {
            focus_initial(&el);
        }
    });
    view! {
        <div
            class="dialog-backdrop"
            on:click=move |ev| {
                if ev.target() == ev.current_target() {
                    on_close.run(());
                }
            }
        >
            <div
                class="dialog"
                node_ref=node
                role=if alert { "alertdialog" } else { "dialog" }
                aria-modal="true"
                aria-labelledby=labelledby
                aria-describedby=describedby
                on:keydown=move |ev| {
                    if ev.key() == "Escape" {
                        ev.stop_propagation();
                        on_close.run(());
                    } else if let Some(el) = node.get_untracked() {
                        trap_tab(&ev, &el);
                    }
                }
            >
                {children()}
            </div>
        </div>
    }
}

/// A pending "are you sure?" question.
#[derive(Clone)]
pub struct Confirm {
    pub title: String,
    pub body: String,
    pub confirm_label: String,
    pub danger: bool,
    pub on_confirm: Callback<()>,
}

#[component]
pub fn ConfirmDialog(request: Confirm, on_close: Callback<()>) -> impl IntoView {
    let title_id = uid("confirm-title");
    let body_id = uid("confirm-body");
    let Confirm {
        title,
        body,
        confirm_label,
        danger,
        on_confirm,
    } = request;
    view! {
        <Modal on_close=on_close labelledby=title_id.clone() describedby=body_id.clone() alert=danger>
            <span class=if danger { "dialog-icon tone-danger" } else { "dialog-icon tone-info" }>
                <Icon icon=if danger { Ic::Alert } else { Ic::Info } class="lg"/>
            </span>
            <h2 id=title_id>{title}</h2>
            <p id=body_id class="muted pre">{body}</p>
            <div class="dialog-actions">
                <button type="button" class="btn" data-autofocus on:click=move |_| on_close.run(())>
                    "Abbrechen"
                </button>
                <button
                    type="button"
                    class=if danger { "btn danger solid" } else { "btn primary" }
                    on:click=move |_| {
                        on_close.run(());
                        on_confirm.run(());
                    }
                >
                    {confirm_label}
                </button>
            </div>
        </Modal>
    }
}
