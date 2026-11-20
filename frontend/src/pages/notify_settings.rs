//! WP10: notification settings – which event goes to which channel, quiet
//! hours with time zone, daily summary and the signed webhook.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::notify::{
    fmt_minute, parse_minute, Channel, Event, NotificationSettings, NotificationSettingsUpdate,
    WebhookSecret, DIGEST_HELP, QUIET_HELP, WEBHOOK_HELP,
};

use crate::api;
use crate::store::use_store;
use crate::ui::{uid, Field, FieldIds, HelpTip, Ic, Icon, SecretBox};

#[component]
pub fn NotifySettingsCard() -> impl IntoView {
    let data = RwSignal::new(None::<Result<NotificationSettings, String>>);
    spawn_local(async move {
        data.set(Some(
            api::get::<NotificationSettings>("/notifications/settings")
                .await
                .map_err(|e| e.message),
        ));
    });
    view! {
        <super::settings::Card id="notify-rules" icon=Ic::Bell title="Was wohin gemeldet wird"
            text="Für jedes Ereignis einzeln: Postfach, Push aufs Handy, ntfy oder Webhook. Dazu Ruhezeiten und eine tägliche Zusammenfassung.">
            {move || match data.get() {
                None => view! { <p class="small muted" role="status">"Lade Einstellungen …"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="alert tone-warning" role="alert">{e}</p> }.into_any(),
                Some(Ok(s)) => view! { <NotifyForm s=s/> }.into_any(),
            }}
        </super::settings::Card>
    }
}

#[component]
fn NotifyForm(s: NotificationSettings) -> impl IntoView {
    let store = use_store();
    let matrix = RwSignal::new(s.matrix.clone());
    let tz = RwSignal::new(if s.timezone == "UTC" {
        // Not chosen yet: the browser's zone is the better default.
        super::automation::browser_tz()
    } else {
        s.timezone.clone()
    });
    let quiet = RwSignal::new(s.quiet_enabled);
    let q_start = RwSignal::new(fmt_minute(s.quiet_start));
    let q_end = RwSignal::new(fmt_minute(s.quiet_end));
    let critical = RwSignal::new(s.quiet_critical);
    let digest = RwSignal::new(s.digest_enabled);
    let d_time = RwSignal::new(fmt_minute(s.digest_minute));
    let webhook = RwSignal::new(s.webhook_url.clone().unwrap_or_default());
    let secret_set = RwSignal::new(s.webhook_secret_set);
    let secret = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);
    let zones = RwSignal::new(Vec::<String>::new());
    spawn_local(async move {
        if let Ok(z) = api::get::<Vec<String>>("/timezones").await {
            zones.set(z);
        }
    });
    let ntfy = s.ntfy_configured;
    let devices = s.push_devices;

    let save = move || {
        let time = |v: RwSignal<String>, what: &str| {
            parse_minute(&v.get_untracked())
                .ok_or_else(|| format!("{what}: bitte eine Uhrzeit wählen"))
        };
        let body = (|| -> Result<NotificationSettingsUpdate, String> {
            Ok(NotificationSettingsUpdate {
                timezone: tz.get_untracked().trim().to_string(),
                quiet_enabled: quiet.get_untracked(),
                quiet_start: time(q_start, "Ruhezeit von")?,
                quiet_end: time(q_end, "Ruhezeit bis")?,
                quiet_critical: critical.get_untracked(),
                digest_enabled: digest.get_untracked(),
                digest_minute: time(d_time, "Uhrzeit der Zusammenfassung")?,
                webhook_url: Some(webhook.get_untracked().trim().to_string())
                    .filter(|w| !w.is_empty()),
                matrix: matrix.get_untracked(),
            })
        })()
        .and_then(|b| queued_api::notify::validate(&b).map(|_| b));
        let body = match body {
            Ok(b) => b,
            Err(e) => {
                error.set(Some(e));
                return;
            }
        };
        error.set(None);
        spawn_local(async move {
            match api::put::<_, NotificationSettings>("/notifications/settings", &body).await {
                Ok(s) => {
                    matrix.set(s.matrix.clone());
                    secret_set.set(s.webhook_secret_set);
                    store.toast("Benachrichtigungen gespeichert");
                }
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    let new_secret = move |_| {
        spawn_local(async move {
            match api::post::<_, WebhookSecret>(
                "/notifications/webhook/secret",
                &serde_json::json!({}),
            )
            .await
            {
                Ok(s) => {
                    secret.set(Some(s.secret));
                    secret_set.set(true);
                }
                Err(e) => store.toast_error("Secret nicht erzeugt", &e),
            }
        });
    };
    let test_hook = move |_| {
        spawn_local(async move {
            match api::action("/notifications/webhook/test").await {
                Ok(()) => store.toast("Test an den Webhook geschickt"),
                Err(e) => store.toast_error("Test fehlgeschlagen", &e),
            }
        });
    };

    let tz_ids = FieldIds::new("nt-tz");
    let zone_list = uid("nt-zones");
    let qs_ids = FieldIds::new("nt-qs");
    let qe_ids = FieldIds::new("nt-qe");
    let dt_ids = FieldIds::new("nt-dt");
    let wh_ids = FieldIds::new("nt-wh");
    view! {
        <div class="stack" data-testid="notify-settings">
            <p class="small muted">
                {if ntfy { "ntfy ist eingerichtet." } else { "ntfy ist noch nicht eingerichtet (Adresse oben)." }}
                " "
                {match devices {
                    0 => "Push ist auf keinem Gerät aktiviert.".to_string(),
                    1 => "Push ist auf 1 Gerät aktiviert.".to_string(),
                    n => format!("Push ist auf {n} Geräten aktiviert."),
                }}
            </p>
            <div class="notify-matrix">
                {Event::ALL.into_iter().map(|e| view! {
                    <fieldset class="notify-event" data-event=e.as_str()>
                        <legend>
                            {e.label()}
                            {e.critical().then(|| view! { <span class="tag tone-warning crit">"wichtig"</span> })}
                        </legend>
                        <p class="small muted">{e.help()}</p>
                        <div class="notify-channels">
                            {Channel::ALL.into_iter().map(|c| view! {
                                <label class="check">
                                    <input type="checkbox" data-channel=c.as_str()
                                        prop:checked=move || matrix.with(|m| m.get(&e).and_then(|x| x.get(&c)).copied().unwrap_or_else(|| e.default_on(c)))
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            matrix.update(|m| { m.entry(e).or_default().insert(c, on); });
                                        }/>
                                    <span class="check-text">{c.label()}</span>
                                </label>
                            }).collect_view()}
                        </div>
                    </fieldset>
                }).collect_view()}
            </div>

            <hr class="divider"/>
            <h3 class="row">"Ruhezeit" <HelpTip topic="Ruhezeit" text=QUIET_HELP/></h3>
            <label class="switch">
                <input type="checkbox" role="switch" data-testid="quiet-enabled" prop:checked=move || quiet.get() on:change=move |ev| quiet.set(event_target_checked(&ev))/>
                <span class="check-text">"Nachts keine Push- und ntfy-Nachrichten"</span>
            </label>
            <div class="time-row">
                <Field ids=qs_ids.clone() label="Von">
                    <input id=qs_ids.id.clone() class="input" type="time" bind:value=q_start/>
                </Field>
                <Field ids=qe_ids.clone() label="Bis">
                    <input id=qe_ids.id.clone() class="input" type="time" bind:value=q_end/>
                </Field>
            </div>
            <label class="check">
                <input type="checkbox" prop:checked=move || critical.get() on:change=move |ev| critical.set(event_target_checked(&ev))/>
                <span class="check-text">"Wichtiges trotzdem melden"<small>"Rückfragen, Freigaben und blockierte Aufträge kommen auch in der Ruhezeit."</small></span>
            </label>
            <Field ids=tz_ids.clone() label="Zeitzone" hint="Gilt für Ruhezeit und Zusammenfassung">
                <input id=tz_ids.id.clone() class="input" list=zone_list.clone() autocomplete="off" aria-describedby=tz_ids.described_by() bind:value=tz/>
                <datalist id=zone_list>{move || zones.get().into_iter().map(|z| view! { <option value=z/> }).collect_view()}</datalist>
            </Field>

            <hr class="divider"/>
            <h3 class="row">"Tägliche Zusammenfassung" <HelpTip topic="Tägliche Zusammenfassung" text=DIGEST_HELP/></h3>
            <label class="switch">
                <input type="checkbox" role="switch" data-testid="digest-enabled" prop:checked=move || digest.get() on:change=move |ev| digest.set(event_target_checked(&ev))/>
                <span class="check-text">"Einmal am Tag zusammenfassen"</span>
            </label>
            <div class="time-row">
                <Field ids=dt_ids.clone() label="Uhrzeit">
                    <input id=dt_ids.id.clone() class="input" type="time" bind:value=d_time/>
                </Field>
            </div>

            <hr class="divider"/>
            <h3 class="row">"Webhook" <HelpTip topic="Webhook" text=WEBHOOK_HELP/></h3>
            <Field ids=wh_ids.clone() label="Adresse" hint="Leer lassen, wenn du keinen Webhook nutzt">
                <input id=wh_ids.id.clone() class="input" inputmode="url" placeholder="https://…" aria-describedby=wh_ids.described_by() bind:value=webhook/>
            </Field>
            {move || secret.get().map(|v| view! {
                <SecretBox label="Webhook-Secret (nur jetzt sichtbar)" value=v testid="webhook-secret"/>
            })}
            <div class="row wrap">
                <button type="button" class="btn" on:click=new_secret disabled=move || webhook.with(|w| w.trim().is_empty())>
                    <Icon icon=Ic::Key class="sm"/>{move || if secret_set.get() { "Neues Secret" } else { "Secret erzeugen" }}
                </button>
                <button type="button" class="btn" on:click=test_hook disabled=move || !secret_set.get()>
                    <Icon icon=Ic::Send class="sm"/>"Test senden"
                </button>
            </div>
            <p class="small muted">"Nach einer neuen Adresse zuerst speichern, dann ein Secret erzeugen."</p>

            <div class="form-actions">
                <button class="btn primary" type="button" data-testid="notify-save" on:click=move |_| save()><Icon icon=Ic::Check class="sm"/>"Speichern"</button>
                <span class="err small" role="alert">{move || error.get()}</span>
            </div>
        </div>
    }
}
