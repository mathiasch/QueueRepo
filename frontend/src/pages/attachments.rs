//! WP09 attachments: picker (composer and drawer – file or camera, iPhone
//! friendly) and the task's attachment list in the drawer.
use leptos::task::spawn_local;
use leptos::{html, prelude::*};
use queued_api::memory::{
    attachment_kind_label, file_size, AttachmentView, ATTACHMENT_ACCEPT, ATTACHMENT_HELP,
};
use uuid::Uuid;

use crate::api::{self, enc};
use crate::logic::memory::precheck;
use crate::store::use_store;
use crate::ui::dialog::Confirm;
use crate::ui::{uid, HelpTip, Ic, Icon};

/// Uploads the chosen files one after the other (drafts, or straight to
/// `task`). Files that fail the pre-check or the server's checks are named
/// in a toast; the others are added to `items`.
fn upload_files(
    input: web_sys::HtmlInputElement,
    project: Uuid,
    task: Option<Uuid>,
    items: RwSignal<Vec<AttachmentView>>,
    busy: RwSignal<Option<String>>,
) {
    let store = use_store();
    let Some(list) = input.files() else {
        return;
    };
    let files: Vec<web_sys::File> = (0..list.length()).filter_map(|i| list.get(i)).collect();
    // Reset so the same file can be chosen again (and the camera re-opens).
    input.set_value("");
    if files.is_empty() {
        return;
    }
    spawn_local(async move {
        let total = files.len();
        let mut ok = 0;
        for (i, f) in files.into_iter().enumerate() {
            let name = f.name();
            if let Err(e) = precheck(&name, f.size(), items.with_untracked(Vec::len)) {
                store.toast_error_text("Anhang nicht möglich", &e);
                continue;
            }
            busy.set(Some(format!("Lade „{name}“ hoch ({}/{total}) …", i + 1)));
            let mut path = format!("/projects/{project}/attachments?name={}", enc(&name));
            if let Some(t) = task {
                path.push_str(&format!("&task={t}"));
            }
            match api::upload::<AttachmentView>(&path, &f).await {
                Ok(a) => {
                    ok += 1;
                    items.update(|v| v.push(a));
                }
                Err(e) => store.toast_error(format!("„{name}“ nicht angehängt"), &e),
            }
        }
        busy.set(None);
        if ok > 0 {
            store.announce(if ok == 1 {
                "1 Anhang hinzugefügt".to_string()
            } else {
                format!("{ok} Anhänge hinzugefügt")
            });
        }
    });
}

/// "Datei anhängen" + "Foto aufnehmen" (camera on phones). Both are real
/// buttons that open a hidden file input, so keyboard and screen readers
/// work and the touch target is large enough.
#[component]
pub fn AttachmentPicker(
    #[prop(into)] project: Signal<Option<Uuid>>,
    items: RwSignal<Vec<AttachmentView>>,
    #[prop(optional)] task: Option<Uuid>,
) -> impl IntoView {
    let file_ref: NodeRef<html::Input> = NodeRef::new();
    let cam_ref: NodeRef<html::Input> = NodeRef::new();
    let busy = RwSignal::new(None::<String>);
    let status_id = uid("att-status");
    let on_change = move |input: Option<web_sys::HtmlInputElement>| {
        if let (Some(i), Some(p)) = (input, project.get_untracked()) {
            upload_files(i, p, task, items, busy);
        }
    };
    view! {
        <div class="attach-picker" data-testid="attach-picker">
            <input
                type="file"
                multiple
                accept=ATTACHMENT_ACCEPT
                class="visually-hidden"
                tabindex="-1"
                aria-hidden="true"
                data-testid="attach-file-input"
                node_ref=file_ref
                on:change=move |_| on_change(file_ref.get_untracked())
            />
            <input
                type="file"
                accept="image/*"
                capture="environment"
                class="visually-hidden"
                tabindex="-1"
                aria-hidden="true"
                data-testid="attach-camera-input"
                node_ref=cam_ref
                on:change=move |_| on_change(cam_ref.get_untracked())
            />
            <button type="button" class="btn sm" data-testid="attach-file"
                disabled=move || busy.with(Option::is_some) || project.get().is_none()
                aria-describedby=status_id.clone()
                on:click=move |_| if let Some(i) = file_ref.get_untracked() { i.click() }>
                <Icon icon=Ic::Paperclip class="sm"/>"Datei anhängen"
            </button>
            <button type="button" class="btn sm" data-testid="attach-camera"
                disabled=move || busy.with(Option::is_some) || project.get().is_none()
                on:click=move |_| if let Some(i) = cam_ref.get_untracked() { i.click() }>
                <Icon icon=Ic::Camera class="sm"/>"Foto aufnehmen"
            </button>
            <HelpTip topic="Anhänge" text=ATTACHMENT_HELP/>
            <span id=status_id class="small muted" role="status" aria-live="polite">{move || busy.get()}</span>
        </div>
    }
}

/// Chips of attachments with preview, download and remove.
#[component]
pub fn AttachmentChips(
    items: RwSignal<Vec<AttachmentView>>,
    /// Ask before deleting (attachments of an existing task).
    #[prop(optional)]
    confirm: bool,
) -> impl IntoView {
    let store = use_store();
    let remove = move |a: AttachmentView| {
        let run = move || {
            let id = a.id;
            spawn_local(async move {
                match api::delete(&format!("/attachments/{id}")).await {
                    Ok(()) => {
                        items.update(|v| v.retain(|x| x.id != id));
                        store.announce("Anhang entfernt");
                    }
                    Err(e) => store.toast_error("Anhang nicht entfernt", &e),
                }
            });
        };
        if confirm {
            store.ask(Confirm {
                title: format!("„{}“ löschen?", a.display_name),
                body: "Der Anhang wird gelöscht und beim nächsten Lauf nicht mehr bereitgestellt."
                    .into(),
                confirm_label: "Löschen".into(),
                danger: true,
                on_confirm: Callback::new(move |_| run()),
            });
        } else {
            run();
        }
    };
    view! {
        <ul class="attach-list" data-testid="attach-list" aria-label="Anhänge">
            <For each=move || items.get() key=|a| a.id let:a>
                {
                    let a2 = a.clone();
                    let href = format!("/api/attachments/{}", a.id);
                    let dims = match (a.width, a.height) {
                        (Some(w), Some(h)) => format!(" · {w}×{h}"),
                        _ => String::new(),
                    };
                    let meta = format!("{} · {}{dims}", attachment_kind_label(&a.mime), file_size(a.size));
                    view! {
                        <li class="attach-item" data-testid="attach-item">
                            {if a.is_image() {
                                view! { <img class="attach-thumb" src=href.clone() alt="" loading="lazy" width="44" height="44"/> }.into_any()
                            } else {
                                view! { <span class="attach-thumb icon" aria-hidden="true"><Icon icon=Ic::FileText/></span> }.into_any()
                            }}
                            <span class="attach-text">
                                <a class="attach-name truncate" href=href download=a.display_name.clone()>{a.display_name.clone()}</a>
                                <span class="xs muted">{meta}</span>
                            </span>
                            {a.can_delete.then(|| {
                                let a3 = a2.clone();
                                view! {
                                    <button type="button" class="btn ghost icon-only" data-testid="attach-remove"
                                        aria-label=format!("Anhang „{}“ entfernen", a2.display_name)
                                        on:click=move |_| remove(a3.clone())>
                                        <Icon icon=Ic::X class="sm"/>
                                    </button>
                                }
                            })}
                        </li>
                    }
                }
            </For>
        </ul>
    }
}

/// Drawer block: the task's attachments (inherited ones of a fix task
/// included) and, while it does not run, adding more.
#[component]
pub fn TaskAttachments(
    project: Uuid,
    task: Uuid,
    items: RwSignal<Vec<AttachmentView>>,
    can_add: Signal<bool>,
) -> impl IntoView {
    view! {
        <Show when=move || can_add.get() || !items.with(Vec::is_empty)>
            <section class="block" data-testid="task-attachments" aria-labelledby="task-att-title">
                <h3 id="task-att-title" class="row"><Icon icon=Ic::Paperclip class="sm"/>"Anhänge"</h3>
                <p class="small muted">"Der Agent findet sie unter .queue/attachments/ – sie landen nie in einem Commit."</p>
                <AttachmentChips items=items confirm=true/>
                {move || can_add.get().then(|| view! {
                    <AttachmentPicker project=Signal::derive(move || Some(project)) items=items task=task/>
                })}
            </section>
        </Show>
    }
}
