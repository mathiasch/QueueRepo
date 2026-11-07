//! Queue web frontend (Leptos CSR → WebAssembly, built with Trunk).
pub mod api;
pub mod logic;
pub mod nav;
pub mod pages;
pub mod store;
pub mod theme;
pub mod ui;

use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::{Meta, User};

/// Entry point called from `main`.
pub fn run() {
    console_error_panic_hook::set_once();
    theme::apply(theme::stored());
    register_service_worker();
    if let Some(boot) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("boot"))
    {
        boot.remove();
    }
    leptos::mount::mount_to_body(App);
}

fn register_service_worker() {
    if let Some(w) = web_sys::window() {
        let sw = w.navigator().service_worker();
        // Web Push only; failures (e.g. http://) are not fatal.
        let _ = sw.register("/sw.js");
    }
}

/// Session state before the main UI exists.
#[derive(Clone)]
enum Session {
    Loading,
    LoggedOut(Meta),
    LoggedIn(Meta, User),
    Unreachable,
}

#[component]
fn App() -> impl IntoView {
    let session = RwSignal::new(Session::Loading);
    let check = move || {
        spawn_local(async move {
            let meta = match api::get::<Meta>("/meta").await {
                Ok(m) => m,
                Err(_) => {
                    session.set(Session::Unreachable);
                    return;
                }
            };
            match api::get::<User>("/auth/me").await {
                Ok(u) => session.set(Session::LoggedIn(meta, u)),
                Err(_) => session.set(Session::LoggedOut(meta)),
            }
        });
    };
    check();
    let on_login = Callback::new(move |_| check());
    move || {
        match session.get() {
        Session::Loading => view! { <div class="boot" role="status">"Queue lädt …"</div> }.into_any(),
        Session::Unreachable => view! {
            <main class="center">
                <div class="card pad stack" role="alert">
                    <h1>"Server nicht erreichbar"</h1>
                    <p class="muted">{logic::errors::NETWORK}</p>
                    <button class="btn primary" on:click=move |_| check()>"Erneut versuchen"</button>
                </div>
            </main>
        }
        .into_any(),
        Session::LoggedOut(meta) => view! { <pages::login::Login meta=meta on_login=on_login/> }.into_any(),
        Session::LoggedIn(meta, user) => view! { <pages::shell::Shell meta=meta user=user/> }.into_any(),
    }
    }
}
