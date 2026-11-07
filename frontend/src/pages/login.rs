//! Sign-in and registration.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::{LoginRequest, Meta, RegisterRequest};

use crate::api;
use crate::nav::{query_param, set_title};
use crate::ui::{FieldIds, Ic, Icon};

#[component]
pub fn Login(meta: Meta, on_login: Callback<()>) -> impl IntoView {
    set_title("Anmelden");
    let register = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(query_param("login_error").unwrap_or_default());
    let busy = RwSignal::new(false);
    let allow_registration = meta.allow_registration;

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let res = if register.get_untracked() {
                api::post_ok(
                    "/auth/register",
                    &RegisterRequest {
                        email: email.get_untracked(),
                        password: password.get_untracked(),
                        name: name.get_untracked(),
                    },
                )
                .await
            } else {
                api::post_ok(
                    "/auth/login",
                    &LoginRequest {
                        email: email.get_untracked(),
                        password: password.get_untracked(),
                    },
                )
                .await
            };
            busy.set(false);
            match res {
                Ok(()) => on_login.run(()),
                Err(e) => error.set(e.message),
            }
        });
    };

    let (n_ids, e_ids, p_ids) = (
        FieldIds::new("name"),
        FieldIds::new("email"),
        FieldIds::new("password"),
    );
    let oidc = meta.oidc.clone();
    view! {
        <main class="center">
            <form class="card login" on:submit=submit novalidate=false>
                <h1 class="brand">
                    <span class="logo"><Icon icon=Ic::Logo/></span>
                    "Queue"
                </h1>
                <p class="lead">
                    {move || if register.get() {
                        "Erstelle dein Konto. Danach legst du ein Projekt an und schreibst deinen ersten Auftrag."
                    } else {
                        "Deine KI-Agenten erledigen Aufträge der Reihe nach – du behältst den Überblick."
                    }}
                </p>
                {(!oidc.is_empty()).then(|| view! {
                    <div class="stack">
                        {oidc.iter().map(|p| view! {
                            <a class="btn block lg" href=format!("/api/auth/oidc/{}/start", p.id)>
                                {format!("Mit {} anmelden", p.name)}
                            </a>
                        }).collect_view()}
                        <div class="or">"oder mit E-Mail"</div>
                    </div>
                })}
                {move || register.get().then(|| {
                    let ids = n_ids.clone();
                    view! {
                        <div class="field">
                            <div class="label"><label for=ids.id.clone()>"Name"</label></div>
                            <input id=ids.id.clone() class="input" autocomplete="name" bind:value=name/>
                        </div>
                    }
                })}
                <div class="field">
                    <div class="label"><label for=e_ids.id.clone()>"E-Mail"</label></div>
                    <input id=e_ids.id.clone() class="input" type="email" autocomplete="email" required bind:value=email/>
                </div>
                <div class="field">
                    <div class="label"><label for=p_ids.id.clone()>"Passwort"</label></div>
                    <input
                        id=p_ids.id.clone()
                        class="input"
                        type="password"
                        required
                        aria-describedby=p_ids.hint.clone()
                        autocomplete=move || if register.get() { "new-password" } else { "current-password" }
                        minlength=move || register.get().then_some("8")
                        bind:value=password
                    />
                    <span class="hint" id=p_ids.hint.clone()>
                        {move || register.get().then_some("Mindestens 8 Zeichen.")}
                    </span>
                </div>
                <div role="alert">
                    {move || (!error.get().is_empty()).then(|| view! {
                        <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>{error.get()}</div>
                    })}
                </div>
                <button class="btn primary lg block" type="submit" disabled=move || busy.get()>
                    {move || match (busy.get(), register.get()) {
                        (true, _) => "Einen Moment …",
                        (false, true) => "Konto erstellen",
                        (false, false) => "Anmelden",
                    }}
                </button>
                {allow_registration.then(|| view! {
                    <button
                        type="button"
                        class="btn ghost block"
                        on:click=move |_| {
                            register.update(|r| *r = !*r);
                            error.set(String::new());
                        }
                    >
                        {move || if register.get() { "Ich habe schon ein Konto" } else { "Neues Konto erstellen" }}
                    </button>
                })}
            </form>
        </main>
    }
}
