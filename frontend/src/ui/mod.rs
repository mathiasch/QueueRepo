//! Design-system components.
pub mod dialog;
pub mod icons;

use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::prelude::*;
use queued_api::TaskStatus;

pub use icons::{Ic, Icon};

use crate::logic::labels::{status_help, status_label, status_tone, Tone};

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

/// Unique DOM id for label/description wiring.
pub fn uid(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

pub fn status_icon(s: TaskStatus) -> Ic {
    match s {
        TaskStatus::PendingApproval => Ic::Shield,
        TaskStatus::Queued => Ic::CircleDashed,
        TaskStatus::Running => Ic::Loader,
        TaskStatus::WaitingBatch => Ic::Clock,
        TaskStatus::WaitingInput => Ic::Message,
        TaskStatus::Held => Ic::Shield,
        TaskStatus::Merging => Ic::Branch,
        TaskStatus::Done => Ic::CheckCircle,
        TaskStatus::Failed => Ic::XCircle,
        TaskStatus::Blocked => Ic::Octagon,
        TaskStatus::Cancelled => Ic::Ban,
    }
}

/// Status as text + colour + dot (never colour alone).
#[component]
pub fn StatusPill(status: TaskStatus) -> impl IntoView {
    view! {
        <span class=format!("pill {}", status_tone(status).class()) title=status_help(status)>
            <span class="dot" aria-hidden="true"></span>
            {status_label(status)}
        </span>
    }
}

/// Square status icon at the start of list rows.
#[component]
pub fn StatusIcon(status: TaskStatus) -> impl IntoView {
    view! {
        <span class=format!("status-icon {}", status_tone(status).class()) title=status_label(status)>
            <Icon icon=status_icon(status) class="sm"/>
        </span>
    }
}

#[component]
pub fn Tag(
    #[prop(into)] text: String,
    #[prop(optional)] tone: Option<Tone>,
    #[prop(optional)] test_id: Option<&'static str>,
) -> impl IntoView {
    let class = format!("tag {}", tone.map(Tone::class).unwrap_or(""));
    view! { <span class=class data-testid=test_id>{text}</span> }
}

/// Toggletip: "?" button that shows an explanation (keyboard + touch friendly).
#[component]
pub fn HelpTip(
    /// What the tip explains, used in the button label ("Hilfe: …").
    #[prop(into)]
    topic: String,
    text: &'static str,
    #[prop(optional)] align: &'static str,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let id = uid("help");
    let class = format!("help {}", if align.is_empty() { "" } else { align });
    let bid = id.clone();
    view! {
        <span class=class>
            <button
                type="button"
                aria-label=format!("Hilfe: {topic}")
                aria-expanded=move || open.get().to_string()
                aria-controls=bid
                on:click=move |ev| {
                    ev.prevent_default();
                    ev.stop_propagation();
                    open.update(|o| *o = !*o)
                }
                on:blur=move |_| open.set(false)
                on:keydown=move |ev| {
                    if ev.key() == "Escape" && open.get_untracked() {
                        ev.stop_propagation();
                        open.set(false);
                    }
                }
            >
                "?"
            </button>
            <span id=id role="status" class="help-live">
                {move || open.get().then(|| view! { <span class="bubble">{text}</span> })}
            </span>
        </span>
    }
}

/// Labelled form field. The control gets `id` and `aria-describedby` from the caller
/// via [`FieldIds`].
#[derive(Clone)]
pub struct FieldIds {
    pub id: String,
    pub hint: String,
    pub err: String,
}

impl FieldIds {
    pub fn new(prefix: &str) -> Self {
        let id = uid(prefix);
        Self {
            hint: format!("{id}-hint"),
            err: format!("{id}-err"),
            id,
        }
    }

    pub fn described_by(&self) -> String {
        format!("{} {}", self.hint, self.err)
    }
}

#[component]
pub fn Field(
    ids: FieldIds,
    #[prop(into)] label: String,
    #[prop(optional, into)] hint: Option<String>,
    /// Explanation behind a "?" button (empty = none).
    #[prop(optional)]
    help: &'static str,
    #[prop(optional, into)] error: Option<Signal<Option<String>>>,
    #[prop(optional)] class: &'static str,
    children: Children,
) -> impl IntoView {
    let topic = label.clone();
    view! {
        <div class=format!("field {class}")>
            <div class="label">
                <label for=ids.id.clone()>{label}</label>
                {(!help.is_empty()).then(|| view! { <HelpTip topic=topic text=help align="align-start"/> })}
            </div>
            {children()}
            <span class="hint" id=ids.hint.clone()>{hint}</span>
            <span class="err" id=ids.err.clone() aria-live="polite">
                {move || error.and_then(|e| e.get())}
            </span>
        </div>
    }
}

#[component]
pub fn Empty(
    icon: Ic,
    #[prop(into)] title: String,
    #[prop(into)] text: String,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="empty">
            <span class="empty-icon"><Icon icon=icon class="lg"/></span>
            <h3>{title}</h3>
            <p>{text}</p>
            {children.map(|c| c())}
        </div>
    }
}

/// Budget bar with an accessible value.
#[component]
pub fn Meter(
    #[prop(into)] label: String,
    used: f64,
    limit: Option<f64>,
    #[prop(into)] value_text: String,
) -> impl IntoView {
    let (pct, tone) = crate::logic::budget::bar(used, limit);
    let right = if limit.is_some() {
        format!("{} %", pct.round() as i64)
    } else {
        "kein Limit".to_string()
    };
    view! {
        <div class="meter">
            <div class="meter-head">
                <span>{label.clone()}</span>
                <span class="muted">{right}</span>
            </div>
            <div
                class="track"
                role="progressbar"
                aria-label=label
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow=pct.round().to_string()
                aria-valuetext=value_text
            >
                <div class=format!("fill bar-fill {}", tone.class()) style=format!("width: {pct}%")></div>
            </div>
        </div>
    }
}

/// Copies `text` to the clipboard (needs a secure context; the caller shows
/// the text selectable as fallback).
pub async fn copy_text(text: &str) -> bool {
    let Some(w) = web_sys::window() else {
        return false;
    };
    let p = w.navigator().clipboard().write_text(text);
    wasm_bindgen_futures::JsFuture::from(p).await.is_ok()
}

/// A secret shown once: read-only, selectable, with a copy button.
#[component]
pub fn SecretBox(
    #[prop(into)] label: String,
    #[prop(into)] value: String,
    #[prop(into)] testid: String,
) -> impl IntoView {
    let store = crate::store::use_store();
    let id = uid("secret-box");
    let v = value.clone();
    view! {
        <div class="field">
            <div class="label"><label for=id.clone()>{label.clone()}</label></div>
            <div class="row">
                <input id=id class="input mono-input grow" readonly value=value data-testid=testid
                    on:focus=move |ev| {
                        use wasm_bindgen::JsCast;
                        if let Some(i) = ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()) {
                            i.select();
                        }
                    }/>
                <button type="button" class="btn" aria-label=format!("{label} kopieren") on:click=move |_| {
                    let v = v.clone();
                    leptos::task::spawn_local(async move {
                        if copy_text(&v).await {
                            store.toast("Kopiert");
                        } else {
                            store.announce("Kopieren nicht möglich – bitte markieren und selbst kopieren");
                        }
                    });
                }><Icon icon=Ic::Copy class="sm"/>"Kopieren"</button>
            </div>
        </div>
    }
}
