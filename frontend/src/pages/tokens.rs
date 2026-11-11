//! Settings card "API-Zugang" (WP07): personal API tokens – list, create
//! (shown exactly once), revoke, delete.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::tokens::{
    ApiTokenView, CreatedApiToken, NewApiToken, Scope, EXPIRY_CHOICES, SHOW_ONCE, TOKENS_HELP,
};

use crate::api;
use crate::logic::format::{fmt_relative, fmt_time};
use crate::logic::triggers::{queuectl_example, token_state, SCOPE_PRESETS};
use crate::store::{tz, use_store};
use crate::ui::dialog::{Confirm, Modal};
use crate::ui::{uid, Field, FieldIds, Ic, Icon, SecretBox};

use super::settings::Card;

#[component]
pub fn TokensCard() -> impl IntoView {
    let store = use_store();
    let tokens = RwSignal::new(None::<Vec<ApiTokenView>>);
    let created = RwSignal::new(None::<CreatedApiToken>);
    let reload = move || {
        spawn_local(async move {
            match api::get::<Vec<ApiTokenView>>("/tokens").await {
                Ok(t) => tokens.set(Some(t)),
                Err(e) => store.toast_error("Tokens konnten nicht geladen werden", &e),
            }
        })
    };
    reload();
    view! {
        <Card id="api-tokens" icon=Ic::Key title="API-Zugang" text=TOKENS_HELP>
            <div data-testid="api-tokens" class="stack">
                {move || match tokens.get() {
                    None => view! { <p class="muted" role="status">"Lade Tokens …"</p> }.into_any(),
                    Some(list) if list.is_empty() => view! { <p class="muted">"Noch keine Tokens."</p> }.into_any(),
                    Some(list) => view! {
                        <ul class="plain stack" aria-label="Deine API-Tokens">
                            {list.into_iter().map(|t| view! { <TokenRow t=t on_change=Callback::new(move |_| reload())/> }).collect_view()}
                        </ul>
                    }.into_any(),
                }}
                <NewTokenForm on_created=Callback::new(move |c: CreatedApiToken| {
                    created.set(Some(c));
                    reload();
                })/>
            </div>
            {move || created.get().map(|c| view! { <ShowOnce c=c on_close=Callback::new(move |_| created.set(None))/> })}
        </Card>
    }
}

#[component]
fn TokenRow(t: ApiTokenView, on_change: Callback<()>) -> impl IntoView {
    let store = use_store();
    let now = store.now.get_untracked();
    let (state, tone) = token_state(&t, now);
    let active = t.active_at(now);
    let id = t.id;
    let name = t.name.clone();
    let name2 = t.name.clone();
    let revoke = move |_| {
        let name = name.clone();
        store.ask(Confirm {
            title: format!("Token „{name}“ widerrufen?"),
            body: "Skripte und Programme, die dieses Token benutzen, werden sofort abgewiesen. Das lässt sich nicht rückgängig machen.".into(),
            confirm_label: "Widerrufen".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                store.run("Widerrufen fehlgeschlagen", async move {
                    api::action(&format!("/tokens/{id}/revoke")).await?;
                    store.toast("Token widerrufen");
                    on_change.run(());
                    Ok(())
                });
            }),
        });
    };
    let remove = move |_| {
        store.run("Löschen fehlgeschlagen", async move {
            api::delete(&format!("/tokens/{id}")).await?;
            store.toast("Token gelöscht");
            on_change.run(());
            Ok(())
        });
    };
    let scopes: Vec<Scope> = t.scopes.clone();
    view! {
        <li class="sub-card" data-testid="token-row">
            <div class="row wrap">
                <h3 class="grow">{t.name.clone()}</h3>
                <span class=format!("tag {}", tone.class())>{state}</span>
                {if active {
                    view! { <button type="button" class="btn sm danger" on:click=revoke aria-label=format!("Token {name2} widerrufen")><Icon icon=Ic::Ban class="sm"/>"Widerrufen"</button> }.into_any()
                } else {
                    view! { <button type="button" class="btn sm" on:click=remove aria-label=format!("Token {name2} löschen")><Icon icon=Ic::Trash class="sm"/>"Löschen"</button> }.into_any()
                }}
            </div>
            <p class="small muted">
                <span class="mono">{format!("{}…", t.prefix)}</span>
                {format!(" · gültig bis {} · zuletzt benutzt: {}",
                    fmt_time(t.expires_at, now, &tz()),
                    t.last_used_at.map(|l| fmt_relative(l, now, &tz())).unwrap_or_else(|| "nie".into()))}
            </p>
            <div class="row wrap" aria-label="Berechtigungen">
                {scopes.into_iter().map(|s| view! { <span class="tag" title=s.help()>{s.label()}</span> }).collect_view()}
            </div>
        </li>
    }
}

#[component]
fn NewTokenForm(on_created: Callback<CreatedApiToken>) -> impl IntoView {
    let store = use_store();
    let open = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let scopes = RwSignal::new(SCOPE_PRESETS[0].1.to_vec());
    let days = RwSignal::new(90);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let name_ids = FieldIds::new("tok-name");
    let days_ids = FieldIds::new("tok-days");
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        if name.with_untracked(|n| n.trim().is_empty()) {
            error.set(Some(
                "Gib dem Token einen Namen, z. B. „Laptop“ oder „CI“.".into(),
            ));
            return;
        }
        if scopes.with_untracked(Vec::is_empty) {
            error.set(Some("Wähle mindestens eine Berechtigung.".into()));
            return;
        }
        let body = NewApiToken {
            name: name.get_untracked(),
            scopes: scopes.get_untracked(),
            expires_in_days: days.get_untracked(),
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, CreatedApiToken>("/tokens", &body).await {
                Ok(c) => {
                    name.set(String::new());
                    open.set(false);
                    on_created.run(c);
                }
                Err(e) => {
                    error.set(Some(e.message.clone()));
                    store.toast_error("Token konnte nicht angelegt werden", &e);
                }
            }
            busy.set(false);
        });
    };
    let scopes_label = uid("tok-scopes");
    view! {
        {move || (!open.get()).then(|| view! {
            <button type="button" class="btn" style="align-self:flex-start" on:click=move |_| open.set(true) data-testid="new-token">
                <Icon icon=Ic::Plus class="sm"/>"Neues Token"
            </button>
        })}
        {
            let scopes_label = scopes_label.clone();
            let name_ids = name_ids.clone();
            let days_ids = days_ids.clone();
            move || open.get().then(|| {
                let scopes_label = scopes_label.clone();
                let scopes_for = scopes_label.clone();
                let name_ids = name_ids.clone();
                let days_ids = days_ids.clone();
                view! {
                    <form class="sub-card stack" on:submit=submit novalidate data-testid="token-form">
                        <Field ids=name_ids.clone() label="Name" hint="Wofür ist das Token? Hilft später beim Widerrufen.">
                            <input id=name_ids.id.clone() class="input" maxlength="80" autocomplete="off" aria-describedby=name_ids.described_by() bind:value=name/>
                        </Field>
                        <div class="row wrap" role="group" aria-label="Vorschläge für Berechtigungen">
                            {SCOPE_PRESETS.iter().map(|(label, set)| {
                                let set = set.to_vec();
                                view! { <button type="button" class="btn sm ghost" on:click=move |_| scopes.set(set.clone())>{*label}</button> }
                            }).collect_view()}
                        </div>
                        <fieldset class="choice-list" aria-labelledby=scopes_for>
                            <legend id=scopes_label class="label">"Berechtigungen"</legend>
                            {Scope::ALL.into_iter().map(|s| view! {
                                <label class="check">
                                    <input type="checkbox" data-testid=format!("scope-{}", s.as_str())
                                        prop:checked=move || scopes.with(|v| v.contains(&s))
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            scopes.update(|v| {
                                                v.retain(|x| *x != s);
                                                if on { v.push(s); }
                                            });
                                        }/>
                                    <span class="check-text">{s.label()}<small>{s.help()}</small></span>
                                </label>
                            }).collect_view()}
                        </fieldset>
                        <Field ids=days_ids.clone() label="Gültig für">
                            <select id=days_ids.id.clone() class="select" aria-describedby=days_ids.described_by()
                                on:change=move |ev| days.set(event_target_value(&ev).parse().unwrap_or(90))>
                                {EXPIRY_CHOICES.iter().map(|(d, l)| view! { <option value=d.to_string() selected=*d == 90>{*l}</option> }).collect_view()}
                            </select>
                        </Field>
                        {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
                        <div class="form-actions">
                            <button type="submit" class="btn primary" disabled=move || busy.get()><Icon icon=Ic::Key class="sm"/>"Token erstellen"</button>
                            <button type="button" class="btn ghost" on:click=move |_| open.set(false)>"Abbrechen"</button>
                        </div>
                    </form>
                }
            })
        }
    }
}

#[component]
fn ShowOnce(c: CreatedApiToken, on_close: Callback<()>) -> impl IntoView {
    let title = uid("tok-once");
    let desc = uid("tok-once-d");
    let origin = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();
    let example = queuectl_example(&origin, &c.token);
    view! {
        <Modal on_close=on_close labelledby=title.clone() describedby=desc.clone()>
            <span class="dialog-icon tone-success"><Icon icon=Ic::Key class="lg"/></span>
            <h2 id=title>{format!("Token „{}“ erstellt", c.info.name)}</h2>
            <p id=desc class="alert tone-warning"><Icon icon=Ic::Alert class="sm"/>{SHOW_ONCE}</p>
            <SecretBox label="API-Token" value=c.token.clone() testid="token-value"/>
            <details class="block">
                <summary>"So richtest du queuectl ein"</summary>
                <pre class="code small" tabindex="0" aria-label="Beispiel für die Einrichtung">{example}</pre>
            </details>
            <div class="dialog-actions">
                <button type="button" class="btn primary" data-autofocus on:click=move |_| on_close.run(())>"Ich habe es kopiert"</button>
            </div>
        </Modal>
    }
}
