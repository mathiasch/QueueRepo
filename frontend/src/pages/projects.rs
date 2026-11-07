//! Project list and "Neues Projekt".
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::{hooks::use_navigate, NavigateOptions};
use queued_api::{CreatedProject, NewProject, ProjectRole};

use crate::api;
use crate::nav::set_title;
use crate::store::use_store;
use crate::ui::{Empty, FieldIds, HelpTip, Ic, Icon};

pub fn role_label(r: ProjectRole) -> &'static str {
    match r {
        ProjectRole::Owner => "Inhaber",
        ProjectRole::Editor => "Bearbeiten",
        ProjectRole::Viewer => "Nur ansehen",
    }
}

const GOAL_HELP: &str = "Das Ziel wird jedem Agenten mitgegeben. Ist die Warteschlange leer, orientieren sich automatische Vorschläge daran.";
const CLONE_HELP: &str = "Ohne Repository-Adresse wird ein neues, leeres Git-Repository angelegt. Mit Adresse wird das bestehende Repository geklont.";
const TOKEN_HELP: &str = "Für private Repositories und zum Hochladen (Push), z. B. ein GitHub Fine-grained Personal Access Token. Wird verschlüsselt gespeichert.";

#[component]
pub fn ProjectsPage() -> impl IntoView {
    set_title("Projekte");
    let store = use_store();
    let navigate = use_navigate();
    let show_form = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let goal = RwSignal::new(String::new());
    let clone_repo = RwSignal::new(false);
    let remote = RwSignal::new(String::new());
    let branch = RwSignal::new("main".to_string());
    let token = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let form_visible =
        move || show_form.get() || (store.loaded.get() && store.projects.with(Vec::is_empty));

    let create = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if name.with_untracked(|n| n.trim().is_empty()) {
            return;
        }
        busy.set(true);
        let cloning = clone_repo.get_untracked();
        let body = NewProject {
            name: name.get_untracked().trim().into(),
            description: String::new(),
            goal: goal.get_untracked(),
            git_remote_url: Some(remote.get_untracked().trim().to_string())
                .filter(|r| cloning && !r.is_empty()),
            git_branch: Some(branch.get_untracked().trim().to_string()).filter(|b| !b.is_empty()),
            git_token: Some(token.get_untracked().trim().to_string())
                .filter(|t| cloning && !t.is_empty()),
        };
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::post::<_, CreatedProject>("/projects", &body).await {
                Ok(p) => {
                    name.set(String::new());
                    goal.set(String::new());
                    remote.set(String::new());
                    token.set(String::new());
                    show_form.set(false);
                    store.reload_projects().await;
                    store.toast(format!("Projekt „{}“ angelegt", p.project.name));
                    navigate(
                        &format!("/projects/{}", p.project.id),
                        NavigateOptions::default(),
                    );
                }
                Err(e) => store.toast_error("Projekt konnte nicht angelegt werden", &e),
            }
            busy.set(false);
        });
    };

    let (n_ids, g_ids, r_ids, b_ids, t_ids) = (
        FieldIds::new("p-name"),
        FieldIds::new("p-goal"),
        FieldIds::new("p-remote"),
        FieldIds::new("p-branch"),
        FieldIds::new("p-token"),
    );

    view! {
        <div class="page-head">
            <div>
                <h1>"Projekte"</h1>
                <p>"Jedes Projekt ist ein eigenes Git-Repository, in dem die Agenten arbeiten."</p>
            </div>
            {move || (!form_visible()).then(|| view! {
                <button class="btn primary" type="button" on:click=move |_| show_form.set(true)>
                    <Icon icon=Ic::FolderPlus class="sm"/>"Neues Projekt"
                </button>
            })}
        </div>
        {move || form_visible().then(|| {
            let create = create.clone();
            let (n_ids, g_ids, r_ids, b_ids, t_ids) = (n_ids.clone(), g_ids.clone(), r_ids.clone(), b_ids.clone(), t_ids.clone());
            view! {
                <form class="card settings-card" on:submit=create aria-labelledby="new-project-title">
                    <header>
                        <span class="s-icon"><Icon icon=Ic::FolderPlus/></span>
                        <div>
                            <h2 id="new-project-title">"Neues Projekt"</h2>
                            <p>"Leer starten oder ein bestehendes Repository klonen."</p>
                        </div>
                    </header>
                    <div class="field">
                        <div class="label"><label for=n_ids.id.clone()>"Projektname"</label></div>
                        <input id=n_ids.id.clone() class="input" required bind:value=name data-autofocus placeholder="z. B. Meine Website"/>
                    </div>
                    <div class="field">
                        <div class="label"><label for=g_ids.id.clone()>"Ziel des Projekts"</label><HelpTip topic="Ziel des Projekts" text=GOAL_HELP/></div>
                        <textarea id=g_ids.id.clone() class="textarea" rows="2" bind:value=goal placeholder="Optional – z. B. „Eine schnelle, barrierefreie Website für unseren Verein“"></textarea>
                    </div>
                    <label class="switch">
                        <input type="checkbox" role="switch" prop:checked=move || clone_repo.get() on:change=move |ev| clone_repo.set(event_target_checked(&ev))/>
                        <span class="check-text">"Bestehendes Repository klonen"<small>"Statt eines leeren Repositorys"</small></span>
                    </label>
                    <HelpTip topic="Repository klonen" text=CLONE_HELP align="align-start"/>
                    {move || clone_repo.get().then(|| {
                        let (r_ids, b_ids, t_ids) = (r_ids.clone(), b_ids.clone(), t_ids.clone());
                        view! {
                            <div class="grid-3">
                                <div class="field">
                                    <div class="label"><label for=r_ids.id.clone()>"Repository-Adresse"</label></div>
                                    <input id=r_ids.id.clone() class="input" inputmode="url" placeholder="https://github.com/org/repo.git" bind:value=remote/>
                                </div>
                                <div class="field">
                                    <div class="label"><label for=b_ids.id.clone()>"Branch"</label></div>
                                    <input id=b_ids.id.clone() class="input" bind:value=branch/>
                                </div>
                                <div class="field">
                                    <div class="label"><label for=t_ids.id.clone()>"Zugangstoken"</label><HelpTip topic="Zugangstoken" text=TOKEN_HELP align="align-end"/></div>
                                    <input id=t_ids.id.clone() class="input" type="password" autocomplete="off" placeholder="optional" bind:value=token/>
                                </div>
                            </div>
                        }
                    })}
                    <div class="form-actions">
                        <button class="btn primary" type="submit" disabled=move || busy.get()>
                            {move || match (busy.get(), clone_repo.get()) {
                                (true, true) => "Klone …",
                                (true, false) => "Lege an …",
                                _ => "Anlegen",
                            }}
                        </button>
                        {move || store.projects.with(|p| !p.is_empty()).then(|| view! {
                            <button class="btn ghost" type="button" on:click=move |_| show_form.set(false)>"Abbrechen"</button>
                        })}
                    </div>
                </form>
            }
        })}
        {move || (store.loaded.get() && store.projects.with(Vec::is_empty)).then(|| view! {
            <div class="card">
                <Empty icon=Ic::Folder title="Noch keine Projekte" text="Lege oben dein erstes Projekt an. Danach kannst du Aufträge dafür schreiben."/>
            </div>
        })}
        <div class="project-grid">
            <For
                each=move || store.projects.get()
                key=|p| format!("{}-{}-{}", p.id, p.name, p.role.as_str())
                children=move |p| {
                    let navigate = use_navigate();
                    let id = p.id;
                    view! {
                        <article class="card project-card" data-project=p.name.clone()>
                            <div class="row between">
                                <span class="p-icon"><Icon icon=Ic::Folder/></span>
                                <span class="tag">{role_label(p.role)}</span>
                            </div>
                            <h2 style="font-size: var(--fs-lg)">
                                <button class="open" type="button" on:click=move |_| navigate(&format!("/projects/{id}"), NavigateOptions::default())>
                                    {p.name.clone()}
                                </button>
                            </h2>
                            <p class="small muted">{if p.description.is_empty() { format!("von {}", p.owner_name) } else { p.description.clone() }}</p>
                            <p class="xs muted row"><Icon icon=Ic::Branch class="sm"/><span class="truncate mono">{p.git_remote_url.clone().unwrap_or_else(|| "Lokales Repository".into())}</span></p>
                        </article>
                    }
                }
            />
        </div>
    }
}
