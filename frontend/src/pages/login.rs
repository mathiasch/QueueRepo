//! Sign-in and registration.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::security::{InvitePreview, LoginOutcome, RegistrationMode};
use queued_api::{LoginRequest, Meta};
use serde_json::json;

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
    let error = RwSignal::new(query_param("login_error").unwrap_or_else(|| {
        if query_param("signed_out").is_some() {
            "Du wurdest abgemeldet – zum Beispiel auf einem anderen Gerät oder durch die Verwaltung. Bitte melde dich erneut an.".into()
        } else {
            String::new()
        }
    }));
    let busy = RwSignal::new(false);
    // WP11: second step after password or OIDC (`?mfa=1`), invitation links.
    let mfa = RwSignal::new(None::<LoginOutcome>);
    let invite = query_param("invite").filter(|i| !i.trim().is_empty());
    let invite_info = RwSignal::new(None::<InvitePreview>);
    let allow_registration = meta.allow_registration || invite.is_some();
    if let Some(i) = invite.clone() {
        register.set(true);
        spawn_local(async move {
            if let Ok(p) =
                api::get::<InvitePreview>(&format!("/auth/invite/{}", api::enc(&i))).await
            {
                if let Some(e) = &p.email {
                    email.set(e.clone());
                }
                invite_info.set(Some(p));
            }
        });
    }
    if query_param("mfa").is_some() {
        spawn_local(async move {
            match api::get::<LoginOutcome>("/auth/mfa").await {
                Ok(o) => mfa.set(Some(o)),
                Err(e) => error.set(e.message),
            }
        });
    }
    let closed = meta.registration_mode != RegistrationMode::Open && invite.is_none();
    let invite2 = invite.clone();

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        error.set(String::new());
        let invite = invite2.clone();
        spawn_local(async move {
            let res = if register.get_untracked() {
                api::post::<_, LoginOutcome>(
                    "/auth/register",
                    &json!({
                        "email": email.get_untracked(),
                        "password": password.get_untracked(),
                        "name": name.get_untracked(),
                        "invite": invite,
                    }),
                )
                .await
            } else {
                api::post::<_, LoginOutcome>(
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
                Ok(o) if o.mfa_required => {
                    password.set(String::new());
                    mfa.set(Some(o));
                }
                Ok(_) => on_login.run(()),
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
    let invite3 = invite.clone();
    let done = Callback::new(move |_| {
        // Drop `?mfa=1` / `?invite=` from the address.
        if let Some(w) = web_sys::window() {
            if let Ok(h) = w.history() {
                let _ = h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/"));
            }
        }
        on_login.run(());
    });
    let cancel = Callback::new(move |_| {
        mfa.set(None);
        spawn_local(async move {
            let _ = api::action("/auth/logout").await;
        });
    });
    view! {
        <main class="center">
            {move || mfa.get().map(|o| view! { <super::security::MfaStep outcome=o on_done=done on_cancel=cancel/> })}
            <form class="card login" on:submit=submit novalidate=false hidden=move || mfa.get().is_some()>
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
                {move || invite_info.get().map(|p| if p.valid {
                    view! { <div class="alert tone-info"><Icon icon=Ic::Send class="sm"/>"Du wurdest eingeladen. Leg jetzt dein Konto an."</div> }.into_any()
                } else {
                    view! { <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>"Diese Einladung ist abgelaufen, schon benutzt oder widerrufen. Bitte frag nach einer neuen."</div> }.into_any()
                })}
                {(!oidc.is_empty()).then(|| view! {
                    <div class="stack">
                        {oidc.iter().map(|p| view! {
                            <a class="btn block lg" href=match &invite3 {
                                Some(i) => format!("/api/auth/oidc/{}/start?invite={}", p.id, api::enc(i)),
                                None => format!("/api/auth/oidc/{}/start", p.id),
                            }>
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
                {closed.then(|| view! { <p class="small muted center-text">"Neue Konten gibt es hier nur auf Einladung der Verwaltung."</p> })}
                {(allow_registration && !closed).then(|| view! {
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
