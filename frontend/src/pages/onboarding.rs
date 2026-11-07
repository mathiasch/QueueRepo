//! First start: create a project → connect a provider → write a task.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::{CreatedProject, NewProject};

use crate::api;
use crate::store::Store;
use crate::theme::{get_pref, set_pref};
use crate::ui::{FieldIds, Ic, Icon};

#[derive(Clone, Copy)]
pub struct OnboardingState {
    store: Store,
    hidden: RwSignal<bool>,
}

impl OnboardingState {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            hidden: RwSignal::new(get_pref("onboarding").as_deref() == Some("hidden")),
        }
    }

    pub fn has_project(self) -> bool {
        self.store.projects.with(|p| !p.is_empty())
    }

    pub fn has_provider(self) -> bool {
        self.store.credentials.with(|c| !c.is_empty())
            || self.store.settings.with(|s| {
                s.as_ref()
                    .map(|s| s.default_provider == "mock")
                    .unwrap_or(false)
            })
    }

    pub fn has_task(self) -> bool {
        self.store.tasks.with(|t| !t.is_empty())
    }

    pub fn done_count(self) -> usize {
        [self.has_project(), self.has_provider(), self.has_task()]
            .iter()
            .filter(|d| **d)
            .count()
    }

    /// Without a project there is nothing else to do, so it cannot be hidden.
    pub fn visible(self) -> bool {
        self.store.loaded.get()
            && (!self.has_project() || (!self.hidden.get() && self.done_count() < 3))
    }
}

#[component]
pub fn Onboarding(state: OnboardingState) -> impl IntoView {
    let store = state.store;
    let name = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let ids = FieldIds::new("ob-project");
    let create = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let n = name.get_untracked();
        if n.trim().is_empty() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, CreatedProject>(
                "/projects",
                &NewProject {
                    name: n.trim().into(),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(p) => {
                    store.reload_projects().await;
                    set_pref("project", &p.project.id.to_string());
                    store.toast(format!("Projekt „{}“ angelegt", p.project.name));
                }
                Err(e) => store.toast_error("Projekt konnte nicht angelegt werden", &e),
            }
            busy.set(false);
        });
    };
    let step_class = move |done: bool, current: bool| {
        if done {
            "ob-step done"
        } else if current {
            "ob-step current"
        } else {
            "ob-step"
        }
    };
    let s1 = move || state.has_project();
    let s2 = move || state.has_provider();
    let s3 = move || state.has_task();
    let label_id = ids.id.clone();
    view! {
        <section class="card onboarding" aria-labelledby="ob-title">
            <div class="row between wrap">
                <div class="stack" style="gap: 4px">
                    <h2 id="ob-title">"Los geht's in drei Schritten"</h2>
                    <p class="muted">"Queue lässt KI-Agenten deine Aufträge der Reihe nach erledigen. So richtest du alles ein:"</p>
                </div>
                {move || s1().then(|| view! {
                    <button class="btn ghost sm" type="button" on:click=move |_| {
                        set_pref("onboarding", "hidden");
                        state.hidden.set(true);
                    }>"Ausblenden"</button>
                })}
            </div>
            <div class="progress-line">
                <span>{move || format!("{} von 3 erledigt", state.done_count())}</span>
                <span class="track" aria-hidden="true">
                    <span class="fill" style=move || format!("display:block; width: {}%", state.done_count() * 100 / 3)></span>
                </span>
            </div>
            <ol>
                <li class=move || step_class(s1(), true)>
                    <span class="num" aria-hidden="true">{move || if s1() { view! { <Icon icon=Ic::Check/> }.into_any() } else { view! { "1" }.into_any() }}</span>
                    <div class="ob-body">
                        <h3>"Projekt anlegen" {move || s1().then_some(view! { <span class="visually-hidden">" (erledigt)"</span> })}</h3>
                        <p>"Lege zuerst ein Projekt an. Ein Projekt ist ein eigener Arbeitsbereich (ein Git-Repository), in dem die Agenten arbeiten – leer oder aus einem bestehenden Repository geklont."</p>
                        {move || (!s1()).then(|| {
                            let label_id = label_id.clone();
                            view! {
                                <form class="row wrap" on:submit=create>
                                    <label class="visually-hidden" for=label_id.clone()>"Projektname"</label>
                                    <input id=label_id class="input grow" style="max-width: 320px" placeholder="z. B. Meine Website" required bind:value=name/>
                                    <button class="btn primary" type="submit" disabled=move || busy.get()>
                                        <Icon icon=Ic::FolderPlus class="sm"/>"Projekt anlegen"
                                    </button>
                                    <a class="btn ghost" href="/projects">"Repository klonen …"</a>
                                </form>
                            }
                        })}
                    </div>
                </li>
                <li class=move || step_class(s2(), s1() && !s2())>
                    <span class="num" aria-hidden="true">{move || if s2() { view! { <Icon icon=Ic::Check/> }.into_any() } else { view! { "2" }.into_any() }}</span>
                    <div class="ob-body">
                        <h3>"Anbieter verbinden" {move || s2().then_some(view! { <span class="visually-hidden">" (erledigt)"</span> })}</h3>
                        <p>"Verbinde Claude, Codex oder Gemini – mit deinem Abo oder einem API-Key. Die Zugangsdaten werden verschlüsselt gespeichert und nie wieder angezeigt."</p>
                        {move || (!s2()).then(|| view! {
                            <div><a class="btn" href="/settings#credentials"><Icon icon=Ic::Key class="sm"/>"Zu den Zugängen"</a></div>
                        })}
                    </div>
                </li>
                <li class=move || step_class(s3(), s1() && s2() && !s3())>
                    <span class="num" aria-hidden="true">{move || if s3() { view! { <Icon icon=Ic::Check/> }.into_any() } else { view! { "3" }.into_any() }}</span>
                    <div class="ob-body">
                        <h3>"Ersten Auftrag schreiben" {move || s3().then_some(view! { <span class="visually-hidden">" (erledigt)"</span> })}</h3>
                        <p>"Beschreibe, was erledigt werden soll – zum Beispiel „Füge eine README mit Installationsanleitung hinzu“. Der Agent arbeitet ihn ab, prüft das Ergebnis und speichert es im Projekt."</p>
                        {move || (s1() && !s3()).then(|| view! {
                            <div>
                                <button class="btn" type="button" on:click=move |_| store.focus_composer.update(|n| *n += 1)>
                                    <Icon icon=Ic::Sparkles class="sm"/>"Zum Eingabefeld"
                                </button>
                            </div>
                        })}
                    </div>
                </li>
            </ol>
        </section>
    }
}
