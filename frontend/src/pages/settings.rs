//! Settings: execution, automatic work, notifications/push, budgets,
//! provider access, appearance, account.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::{
    CredentialInfo, ProviderStatus, PushKey, PutCredential, Settings, SettingsUpdate,
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::api;
use crate::logic::b64;
use crate::logic::budget::{parse_budget, suggested_budget};
use crate::logic::format::{fmt_relative, fmt_time, fmt_tokens, fmt_usd};
use crate::logic::labels::{
    credential_help, credential_is_json, credential_kind_label, help, provider_label,
    ONEMPTY_ACTIONS,
};
use crate::nav::set_title;
use crate::store::{tz, use_store, Store};
use crate::theme::{self, Theme};
use crate::ui::dialog::Confirm;
use crate::ui::{Field, FieldIds, Ic, Icon, Meter};

#[component]
pub(crate) fn Card(
    id: &'static str,
    icon: Ic,
    title: &'static str,
    #[prop(into)] text: String,
    children: Children,
) -> impl IntoView {
    let hid = format!("{id}-title");
    let labelled = hid.clone();
    view! {
        <section class="card settings-card" id=id aria-labelledby=labelled>
            <header>
                <span class="s-icon"><Icon icon=icon/></span>
                <div>
                    <h2 id=hid>{title}</h2>
                    <p>{text}</p>
                </div>
            </header>
            {children()}
        </section>
    }
}

#[component]
pub fn SettingsPage() -> impl IntoView {
    set_title("Einstellungen");
    let store = use_store();
    let has_settings = Memo::new(move |_| store.settings.with(Option::is_some));
    // Jump to #budgets / #credentials after the content exists.
    Effect::new(move |_| {
        if store.settings.with(Option::is_some) {
            if let Some(w) = web_sys::window() {
                let hash = w.location().hash().unwrap_or_default();
                if let Some(el) = hash
                    .strip_prefix('#')
                    .and_then(|h| w.document()?.get_element_by_id(h))
                {
                    el.scroll_into_view();
                }
            }
        }
    });
    view! {
        <div class="page-head">
            <div>
                <h1>"Einstellungen"</h1>
                <p>"Wie die Queue arbeitet, wie sie dich benachrichtigt und mit welchen Zugängen."</p>
            </div>
        </div>
        <div class="settings-layout">
            // Only re-rendered when the settings first arrive: a background reload
            // (e.g. after an SSE reconnect) must not discard unsaved edits.
            {move || match has_settings.get().then(|| store.settings.get_untracked()).flatten() {
                None => view! { <p class="muted" role="status">"Lade Einstellungen …"</p> }.into_any(),
                Some(s) => view! { <GeneralSettings s=s/> }.into_any(),
            }}
            <Budgets/>
            <Credentials/>
            <super::tokens::TokensCard/>
            <super::providers::ConnectorsCard/>
            <super::providers::CostsCard/>
            <super::providers::PricesCard/>
            <super::providers::EvalCard/>
            <super::providers::CliStatusCard/>
            <Appearance/>
            <Account/>
        </div>
    }
}

#[component]
fn GeneralSettings(s: Settings) -> impl IntoView {
    let store = use_store();
    let parallelism = RwSignal::new(s.parallelism.to_string());
    let provider = RwSignal::new(s.default_provider.clone());
    let fixes = RwSignal::new(s.max_fix_attempts.to_string());
    let auto = RwSignal::new(s.max_auto_tasks.to_string());
    let onempty = RwSignal::new(s.onempty_enabled);
    let actions = RwSignal::new(s.onempty_actions.clone());
    let cooldown = RwSignal::new(s.onempty_cooldown_minutes.to_string());
    let ntfy = RwSignal::new(s.ntfy_url.clone().unwrap_or_default());
    let error = RwSignal::new(None::<String>);
    let last = s.last_onempty_at;

    let save = move |_| {
        let num = |v: RwSignal<String>, label: &str| -> Result<i32, String> {
            v.get_untracked()
                .trim()
                .parse::<i32>()
                .map_err(|_| format!("{label}: bitte eine ganze Zahl eingeben"))
        };
        let body = (|| -> Result<SettingsUpdate, String> {
            Ok(SettingsUpdate {
                parallelism: num(parallelism, "Parallele Agenten")?,
                default_provider: provider.get_untracked(),
                onempty_enabled: onempty.get_untracked(),
                onempty_actions: actions.get_untracked(),
                onempty_cooldown_minutes: num(cooldown, "Höchstens alle … Minuten")?,
                max_auto_tasks: num(auto, "Automatische Freigabe")?,
                max_fix_attempts: num(fixes, "Reparaturversuche")?,
                ntfy_url: Some(ntfy.get_untracked().trim().to_string()).filter(|u| !u.is_empty()),
            })
        })();
        let body = match body {
            Ok(b) => b,
            Err(e) => {
                error.set(Some(e));
                return;
            }
        };
        error.set(None);
        spawn_local(async move {
            match api::put::<_, Settings>("/settings", &body).await {
                Ok(s) => {
                    // The server clamps values: show what was stored.
                    parallelism.set(s.parallelism.to_string());
                    fixes.set(s.max_fix_attempts.to_string());
                    auto.set(s.max_auto_tasks.to_string());
                    cooldown.set(s.onempty_cooldown_minutes.to_string());
                    store.settings.set(Some(s));
                    store.toast("Einstellungen gespeichert");
                }
                Err(e) => store.toast_error("Speichern fehlgeschlagen", &e),
            }
        });
    };
    let number = move |sig: RwSignal<String>,
                       label: &'static str,
                       hint: &'static str,
                       help_text: &'static str,
                       min: &'static str,
                       max: &'static str| {
        let ids = FieldIds::new("set");
        let (fid, desc) = (ids.id.clone(), ids.described_by());
        view! {
            <Field ids=ids label=label hint=hint help=help_text>
                <input id=fid class="input" type="number" inputmode="numeric" min=min max=max aria-describedby=desc bind:value=sig/>
            </Field>
        }
    };
    let prov_ids = FieldIds::new("set-provider");
    let ntfy_ids = FieldIds::new("set-ntfy");
    let save_bar = move |label: &'static str| {
        view! {
            <div class="form-actions">
                <button class="btn primary" type="button" on:click=save><Icon icon=Ic::Check class="sm"/>{label}</button>
                <span class="err small" role="alert">{move || error.get()}</span>
            </div>
        }
    };
    view! {
        <Card id="execution" icon=Ic::Zap title="Ausführung" text="Wie viele Agenten arbeiten und wie die Queue mit Fehlern umgeht.">
            <div class="grid-2">
                {number(parallelism, "Parallele Agenten", "1 = einer nach dem anderen", help::PARALLEL, "1", "16")}
                <Field ids=prov_ids.clone() label="Standard-Anbieter" help=help::PROVIDER>
                    <select id=prov_ids.id.clone() class="select" prop:value=move || provider.get() on:change=move |ev| provider.set(event_target_value(&ev))>
                        {store.meta.with_untracked(|m| m.providers.iter().filter(|p| m.credential_kinds.get(*p).is_some_and(|k| !k.is_empty())).map(|p| view! { <option value=p.clone()>{provider_label(p)}</option> }).collect_view())}
                    </select>
                </Field>
                {number(fixes, "Max. Reparaturversuche", "Danach: gestoppt und Benachrichtigung", help::FIX, "0", "10")}
                {number(auto, "Automatische Freigabe bis", "neue Vorschläge von Agenten; darüber wirst du gefragt", help::AUTO_APPROVE, "0", "20")}
            </div>
            {save_bar("Speichern")}
        </Card>
        <Card id="onempty" icon=Ic::Sparkles title="Automatisch weiterarbeiten" text="Was passiert, wenn die Warteschlange leer ist (OnEmptyQueue).">
            <label class="switch">
                <input type="checkbox" role="switch" prop:checked=move || onempty.get() on:change=move |ev| onempty.set(event_target_checked(&ev))/>
                <span class="check-text">"Selbstständig nach sinnvoller Arbeit suchen"<small>{help::ONEMPTY}</small></span>
            </label>
            {move || onempty.get().then(|| view! {
                <fieldset class="stack" style="border:0; padding:0; margin:0">
                    <legend class="small" style="font-weight:600; margin-bottom: var(--sp-2)">"Was darf die Queue tun?"</legend>
                    {ONEMPTY_ACTIONS.iter().map(|(id, label, desc)| {
                        let id = id.to_string();
                        let id2 = id.clone();
                        view! {
                            <label class="check">
                                <input type="checkbox" prop:checked=move || actions.with(|a| a.contains(&id)) on:change=move |ev| {
                                    let on = event_target_checked(&ev);
                                    let id = id2.clone();
                                    actions.update(|a| { a.retain(|x| *x != id); if on { a.push(id); } });
                                }/>
                                <span class="check-text"><strong>{*label}</strong><small>{*desc}</small></span>
                            </label>
                        }
                    }).collect_view()}
                </fieldset>
                {number(cooldown, "Höchstens alle … Minuten", "300 = einmal pro 5-Stunden-Fenster. Läuft nur, wenn nichts auf dich wartet und das Budget reicht.", "", "5", "10080")}
                {last.map(|l| view! { <p class="small muted">{move || format!("Zuletzt: {}", fmt_relative(l, store.now.get(), &tz()))}</p> })}
            })}
            {save_bar("Speichern")}
        </Card>
        <Card id="notifications" icon=Ic::Bell title="Benachrichtigungen" text="Wann du Bescheid bekommst: bei Rückfragen, Freigaben und Problemen.">
            <Field ids=ntfy_ids.clone() label="ntfy-Adresse" hint="Optional, z. B. https://ntfy.sh/mein-geheimes-topic" help=help::NTFY>
                <input id=ntfy_ids.id.clone() class="input" inputmode="url" placeholder="https://ntfy.sh/…" aria-describedby=ntfy_ids.described_by() bind:value=ntfy/>
            </Field>
            {save_bar("Speichern")}
            <hr class="divider"/>
            <PushControls/>
        </Card>
    }
}

fn push_supported() -> bool {
    let Some(w) = web_sys::window() else {
        return false;
    };
    js_sys::Reflect::has(&w, &JsValue::from_str("PushManager")).unwrap_or(false)
        && js_sys::Reflect::has(&w.navigator(), &JsValue::from_str("serviceWorker"))
            .unwrap_or(false)
}

fn permission_label(p: web_sys::NotificationPermission) -> &'static str {
    match p {
        web_sys::NotificationPermission::Granted => "erlaubt",
        web_sys::NotificationPermission::Denied => "im Browser blockiert",
        _ => "noch nicht gefragt",
    }
}

async fn subscribe_push() -> Result<(), String> {
    let w = web_sys::window().ok_or("kein Fenster")?;
    let reg: web_sys::ServiceWorkerRegistration =
        JsFuture::from(w.navigator().service_worker().register("/sw.js"))
            .await
            .map_err(|e| format!("Service Worker: {e:?}"))?
            .unchecked_into();
    let perm =
        JsFuture::from(web_sys::Notification::request_permission().map_err(|e| format!("{e:?}"))?)
            .await
            .map_err(|e| format!("{e:?}"))?;
    if perm.as_string().as_deref() != Some("granted") {
        return Err("Benachrichtigungen wurden nicht erlaubt.".into());
    }
    let key = api::get::<PushKey>("/push/key")
        .await
        .map_err(|e| e.message)?;
    let bytes = b64::decode(&key.public_key).ok_or("ungültiger Server-Schlüssel")?;
    let opts = web_sys::PushSubscriptionOptionsInit::new();
    opts.set_user_visible_only(true);
    opts.set_application_server_key(&js_sys::Uint8Array::from(bytes.as_slice()));
    let pm = reg.push_manager().map_err(|e| format!("{e:?}"))?;
    let sub = JsFuture::from(
        pm.subscribe_with_options(&opts)
            .map_err(|e| format!("{e:?}"))?,
    )
    .await
    .map_err(|e| format!("{e:?}"))?;
    let json: String = js_sys::JSON::stringify(&sub)
        .map_err(|e| format!("{e:?}"))?
        .into();
    api::post_raw_json("/push/subscribe", json)
        .await
        .map_err(|e| e.message)
}

#[component]
fn PushControls() -> impl IntoView {
    let store = use_store();
    let supported = push_supported();
    let permission = RwSignal::new(if supported {
        web_sys::Notification::permission()
    } else {
        web_sys::NotificationPermission::Default
    });
    let enable = move |_| {
        spawn_local(async move {
            match subscribe_push().await {
                Ok(()) => store.toast("Push auf diesem Gerät aktiviert"),
                Err(e) => store.toast_full(
                    "Push fehlgeschlagen".into(),
                    Some(e),
                    crate::store::ToastTone::Error,
                    None,
                    None,
                ),
            }
            permission.set(web_sys::Notification::permission());
        });
    };
    let test = move |_| {
        store.run("Test fehlgeschlagen", async move {
            api::action("/push/test").await
        })
    };
    view! {
        <div class="stack">
            <h3>"Push-Benachrichtigungen auf diesem Gerät"</h3>
            {if supported {
                view! {
                    <p class="small muted">{move || format!("Status: {}", permission_label(permission.get()))}</p>
                    <div class="row wrap">
                        <button class="btn" type="button" on:click=enable>
                            <Icon icon=Ic::Bell class="sm"/>
                            {move || if permission.get() == web_sys::NotificationPermission::Granted { "Push erneuern" } else { "Push aktivieren" }}
                        </button>
                        <button class="btn ghost" type="button" on:click=test>"Test senden"</button>
                    </div>
                }.into_any()
            } else {
                view! {
                    <p class="small muted">"Dieser Browser unterstützt hier keine Push-Benachrichtigungen. Auf iPhone/iPad: Seite über „Teilen → Zum Home-Bildschirm“ als App installieren und dort aktivieren."</p>
                }.into_any()
            }}
        </div>
    }
}

#[component]
fn Budgets() -> impl IntoView {
    let store = use_store();
    view! {
        <section class="card settings-card" id="budgets" data-testid="budgets" aria-labelledby="budgets-title">
            <header>
                <span class="s-icon"><Icon icon=Ic::Gauge/></span>
                <div>
                    <h2 id="budgets-title">"Budgets"</h2>
                    <p>{help::BUDGET} " Gezählt werden Eingabe, Ausgabe und Cache-Schreibvorgänge (Cache-Lesen separat). Trifft ein echtes Abo-Limit, merkt sich die Queue den Verbrauch als „beobachtetes Limit“ – ein guter Startwert für dein 5h-Budget."</p>
                </div>
            </header>
            // Keyed by provider only: refreshing the numbers (every minute and after
            // each run) must not reset what the user is typing.
            <For
                each=move || store.limits.with(|ls| ls.iter().map(|l| l.provider.clone()).collect::<Vec<_>>())
                key=|p| p.clone()
                children=move |p| {
                    let pp = p.clone();
                    let live = Memo::new(move |_| store.limits.with(|ls| ls.iter().find(|l| l.provider == pp).cloned()));
                    let initial = live.get_untracked().unwrap_or_default();
                    view! { <BudgetRow l=initial live=live/> }
                }
            />
        </section>
    }
}

#[component]
fn BudgetRow(l: ProviderStatus, live: Memo<Option<ProviderStatus>>) -> impl IntoView {
    let store = use_store();
    let p = l.provider.clone();
    let b5 = RwSignal::new(
        l.budget
            .limit_5h_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    let b7 = RwSignal::new(
        l.budget
            .limit_7d_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    let usd = RwSignal::new(
        l.budget
            .limit_30d_usd
            .map(|v| v.to_string().replace('.', ","))
            .unwrap_or_default(),
    );
    let error = RwSignal::new(None::<String>);
    let name = provider_label(&p);
    let save = {
        let p = p.clone();
        let name = name.clone();
        move |_| {
            let body = match parse_budget(
                &b5.get_untracked(),
                &b7.get_untracked(),
                &usd.get_untracked(),
            ) {
                Ok(b) => b,
                Err(e) => {
                    error.set(Some(e));
                    return;
                }
            };
            error.set(None);
            let (p, name) = (p.clone(), name.clone());
            spawn_local(async move {
                match api::put::<_, ProviderStatus>(&format!("/budgets/{p}"), &body).await {
                    Ok(_) => {
                        store.reload_limits().await;
                        store.toast(format!("Budget für {name} gespeichert"));
                    }
                    Err(e) => store.toast_error("Speichern fehlgeschlagen", &e),
                }
            });
        }
    };
    let resume = {
        let p = p.clone();
        let name = name.clone();
        move |_| {
            let (p, name) = (p.clone(), name.clone());
            spawn_local(async move {
                match api::action(&format!("/limits/{p}/resume")).await {
                    Ok(()) => {
                        store.reload();
                        store.toast(format!("{name} läuft wieder"));
                    }
                    Err(e) => store.toast_error("Pause aufheben fehlgeschlagen", &e),
                }
            });
        }
    };
    let cli = store
        .meta
        .with_untracked(|m| m.cli_versions.get(&p).cloned());
    let (i5, i7, iu) = (
        FieldIds::new("b5"),
        FieldIds::new("b7"),
        FieldIds::new("busd"),
    );
    view! {
        <div class="sub-card" data-testid=format!("budget-{p}")>
            {
                let resume = resume.clone();
                let name = name.clone();
                move || live.get().map(|l| {
                    let now = store.now.get_untracked();
                    let w = l.window.clone();
                    let resume = resume.clone();
                    view! {
            <div class="row wrap">
                <h3 class="grow">{name.clone()}</h3>
                {l.paused_reason.clone().map(|r| view! { <span class="tag tone-danger">{format!("pausiert: {r}")}</span> })}
                {l.limited_until.map(|t| view! { <span class="tag tone-danger">{format!("Abo-Limit bis {}", fmt_time(t, now, &tz()))}</span> })}
                {l.budget_until.map(|t| view! { <span class="tag tone-danger">{format!("Budget erschöpft bis {}", fmt_time(t, now, &tz()))}</span> })}
                {(l.paused_reason.is_some() || l.limited_until.is_some()).then(|| {
                    let resume = resume.clone();
                    view! {
                        <button class="btn sm" type="button" on:click=resume><Icon icon=Ic::Play class="sm"/>"Pause aufheben"</button>
                    }
                })}
            </div>
            <Meter
                label=format!("5 Std.: {} Tokens · {} Läufe · Cache {}", fmt_tokens(w.used_5h), w.runs_5h, fmt_tokens(w.cache_read_5h))
                used=w.used_5h as f64
                limit=l.budget.limit_5h_tokens.map(|v| v as f64)
                value_text=format!("{} von {} Tokens", fmt_tokens(w.used_5h), l.budget.limit_5h_tokens.map(fmt_tokens).unwrap_or_else(|| "unbegrenzt".into()))
            />
            <Meter
                label=format!("7 Tage: {} Tokens", fmt_tokens(w.used_7d))
                used=w.used_7d as f64
                limit=l.budget.limit_7d_tokens.map(|v| v as f64)
                value_text=format!("{} von {} Tokens", fmt_tokens(w.used_7d), l.budget.limit_7d_tokens.map(fmt_tokens).unwrap_or_else(|| "unbegrenzt".into()))
            />
            <Meter
                label=format!("30 Tage: {}", fmt_usd(w.cost_30d, 2))
                used=w.cost_30d
                limit=l.budget.limit_30d_usd
                value_text=format!("{} von {}", fmt_usd(w.cost_30d, 2), l.budget.limit_30d_usd.map(|v| fmt_usd(v, 2)).unwrap_or_else(|| "unbegrenzt".into()))
            />
                    }
                })
            }
            {cli.map(|v| view! { <p class="xs muted">{format!("CLI-Version im Agent-Image: {v}")}</p> })}
            <div class="grid-3">
                <Field ids=i5.clone() label="5h-Budget (Tokens)"><input id=i5.id.clone() class="input" inputmode="numeric" placeholder="unbegrenzt" bind:value=b5/></Field>
                <Field ids=i7.clone() label="7-Tage-Budget (Tokens)"><input id=i7.id.clone() class="input" inputmode="numeric" placeholder="unbegrenzt" bind:value=b7/></Field>
                <Field ids=iu.clone() label="30-Tage-Budget (USD)"><input id=iu.id.clone() class="input" inputmode="decimal" placeholder="unbegrenzt" bind:value=usd/></Field>
            </div>
            <div class="form-actions">
                <button class="btn primary sm" type="button" on:click=save><Icon icon=Ic::Check class="sm"/>"Speichern"</button>
                {move || live.get().and_then(|l| l.observed_limit_tokens.map(|o| {
                    let at = l.observed_at.map(|t| fmt_time(t, store.now.get_untracked(), &tz())).unwrap_or_default();
                    view! {
                        <button class="btn ghost sm" type="button" on:click=move |_| b5.set(suggested_budget(o).to_string())>
                            {format!("Beobachtetes Limit {} ({at}) → 90 % übernehmen", fmt_tokens(o))}
                        </button>
                    }
                }))}
                <span class="err small" role="alert">{move || error.get()}</span>
            </div>
        </div>
    }
}

#[component]
fn Credentials() -> impl IntoView {
    let store = use_store();
    let providers = move || {
        store.meta.with(|m| {
            m.providers
                .iter()
                // opencode works only through connectors (no own credential).
                .filter(|p| {
                    p.as_str() != "mock"
                        && m.credential_kinds.get(*p).is_some_and(|k| !k.is_empty())
                })
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    view! {
        <Card id="credentials" icon=Ic::Key title="Zugänge zu den Anbietern" text="Jede Person nutzt ihr eigenes Abo oder ihren eigenen API-Key. Zugangsdaten werden verschlüsselt gespeichert (AES-256-GCM) und nie wieder angezeigt.">
            {move || providers().into_iter().map(|p| {
                let kinds = store.meta.with_untracked(|m| m.credential_kinds.get(&p).cloned().unwrap_or_default());
                let pp = p.clone();
                let current = Memo::new(move |_| store.credentials.with(|c| c.iter().find(|c| c.provider == pp).cloned()));
                view! { <CredentialRow provider=p kinds=kinds current=current/> }
            }).collect_view()}
        </Card>
    }
}

#[component]
fn CredentialRow(
    provider: String,
    kinds: Vec<String>,
    current: Memo<Option<CredentialInfo>>,
) -> impl IntoView {
    let store = use_store();
    let open = RwSignal::new(false);
    let kind = RwSignal::new(kinds.first().cloned().unwrap_or_default());
    let secret = RwSignal::new(String::new());
    let name = provider_label(&provider);
    let p_save = provider.clone();
    let save = move || {
        let body = PutCredential {
            kind: kind.get_untracked(),
            secret: secret.get_untracked(),
        };
        let p = p_save.clone();
        spawn_local(async move {
            match api::put_ok(&format!("/credentials/{p}"), &body).await {
                Ok(()) => {
                    secret.set(String::new());
                    open.set(false);
                    store.reload_credentials().await;
                    store.reload_limits().await;
                    store.toast(format!("{} verbunden", provider_label(&p)));
                }
                Err(e) => store.toast_error("Speichern fehlgeschlagen", &e),
            }
        });
    };
    let p_remove = provider.clone();
    let remove = move |_| confirm_remove(store, p_remove.clone());
    let secret_id = crate::ui::uid("secret");
    let pname = name.clone();
    view! {
        <div class="sub-card" data-testid=format!("credential-{provider}")>
            <div class="row wrap">
                <h3 class="grow">{name.clone()}</h3>
                {move || match current.get() {
                    Some(c) => view! {
                        <span class="tag tone-success">
                            <Icon icon=Ic::Check class="sm"/>
                            {format!("{} · {}", credential_kind_label(&c.kind), fmt_relative(c.updated_at, store.now.get(), &tz()))}
                        </span>
                    }.into_any(),
                    None => view! { <span class="tag">"nicht verbunden"</span> }.into_any(),
                }}
                <button class="btn sm" type="button" aria-expanded=move || open.get().to_string() on:click=move |_| open.update(|o| *o = !*o)>
                    {move || if current.with(Option::is_some) { "Ändern" } else { "Verbinden" }}
                </button>
                {
                    let remove = remove.clone();
                    let pname = pname.clone();
                    move || current.with(Option::is_some).then(|| {
                        let remove = remove.clone();
                        view! {
                            <button class="btn icon-only danger" type="button" aria-label=format!("Zugang zu {pname} entfernen") title="Entfernen" on:click=remove>
                                <Icon icon=Ic::Trash/>
                            </button>
                        }
                    })
                }
            </div>
            {
                let provider = provider.clone();
                let kinds = kinds.clone();
                let save = save.clone();
                let secret_id = secret_id.clone();
                move || open.get().then(|| {
                    let provider = provider.clone();
                    let provider2 = provider.clone();
                    let save = save.clone();
                    view! {
                        <div class="stack">
                            <div class="segmented" role="group" aria-label="Art des Zugangs">
                                {kinds.iter().map(|k| {
                                    let k = k.clone();
                                    let k2 = k.clone();
                                    view! {
                                        <button type="button" aria-pressed=move || (kind.get() == k).to_string() on:click=move |_| kind.set(k2.clone())>
                                            {credential_kind_label(&k)}
                                        </button>
                                    }
                                }).collect_view()}
                            </div>
                            <p class="small muted">{move || credential_help(&provider, &kind.get())}</p>
                            <label class="visually-hidden" for=secret_id.clone()>{move || if credential_is_json(&kind.get()) { format!("JSON-Inhalt für {}", provider_label(&provider2)) } else { format!("Token für {}", provider_label(&provider2)) }}</label>
                            <textarea
                                id=secret_id.clone()
                                class="textarea mono-input"
                                rows=move || if credential_is_json(&kind.get()) { "5" } else { "2" }
                                autocomplete="off"
                                spellcheck="false"
                                placeholder=move || if credential_is_json(&kind.get()) { "JSON-Inhalt einfügen" } else { "Token einfügen" }
                                bind:value=secret
                            ></textarea>
                            <div class="form-actions">
                                <button class="btn primary sm" type="button" disabled=move || secret.with(|s| s.trim().is_empty()) on:click=move |_| save()>
                                    <Icon icon=Ic::Check class="sm"/>"Speichern"
                                </button>
                                <button class="btn ghost sm" type="button" on:click=move |_| open.set(false)>"Abbrechen"</button>
                            </div>
                        </div>
                    }
                })
            }
        </div>
    }
}

fn confirm_remove(store: Store, provider: String) {
    let name = provider_label(&provider);
    store.ask(Confirm {
        title: format!("Zugang zu {name} entfernen?"),
        body: "Aufträge für diesen Anbieter können danach nicht mehr starten, bis du ihn wieder verbindest.".into(),
        confirm_label: "Entfernen".into(),
        danger: true,
        on_confirm: Callback::new(move |_| {
            let p = provider.clone();
            spawn_local(async move {
                match api::delete(&format!("/credentials/{p}")).await {
                    Ok(()) => {
                        store.reload_credentials().await;
                        store.toast("Zugang entfernt");
                    }
                    Err(e) => store.toast_error("Entfernen fehlgeschlagen", &e),
                }
            });
        }),
    });
}

#[component]
fn Appearance() -> impl IntoView {
    let store = use_store();
    let current = RwSignal::new(theme::stored());
    view! {
        <Card id="appearance" icon=Ic::Sun title="Darstellung" text="Hell, dunkel oder wie dein Gerät.">
            <div class="segmented" role="group" aria-label="Farbschema">
                {[(Theme::System, "Wie Gerät", Ic::Monitor), (Theme::Light, "Hell", Ic::Sun), (Theme::Dark, "Dunkel", Ic::Moon)].into_iter().map(|(t, label, icon)| view! {
                    <button type="button" aria-pressed=move || (current.get() == t).to_string() on:click=move |_| { theme::save(t); current.set(t); }>
                        <Icon icon=icon class="sm"/>{label}
                    </button>
                }).collect_view()}
            </div>
            <div>
                <button class="btn ghost" type="button" on:click=move |_| store.show_shortcuts.set(true)>
                    <Icon icon=Ic::Keyboard class="sm"/>"Tastaturkürzel anzeigen"
                </button>
            </div>
        </Card>
    }
}

#[component]
fn Account() -> impl IntoView {
    let store = use_store();
    let logout = move |_| {
        spawn_local(async move {
            let _ = api::action("/auth/logout").await;
            if let Some(w) = web_sys::window() {
                let _ = w.location().set_href("/");
            }
        });
    };
    view! {
        <Card id="account" icon=Ic::Users title="Konto" text="">
            <div class="row wrap">
                <div class="grow">
                    <p>"Angemeldet als " <strong>{move || store.me.with(|u| u.name.clone())}</strong></p>
                    <p class="small muted">{move || store.me.with(|u| u.email.clone())}</p>
                </div>
                <button class="btn" type="button" on:click=logout><Icon icon=Ic::LogOut class="sm"/>"Abmelden"</button>
            </div>
        </Card>
    }
}
