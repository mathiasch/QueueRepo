//! Project tab "Anbindungen" (WP08): connect GitHub Issues, Linear, Slack
//! or Jira, map external accounts to people in the project, see what came
//! in. Everybody in the project sees it; only the project owner connects
//! and changes; members map (and confirm) their own accounts. The server
//! enforces all rights; secrets and tokens are never shown again.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::intake::{
    CreatedIntegration, Identities, IdentityState, IdentityView, IntakeConfig, IntakeEventView,
    IntakeKind, IntegrationView, NewIdentity, UpdateIntegration,
};
use queued_api::ProjectView;
use uuid::Uuid;

use crate::api;
use crate::logic::format::fmt_time;
use crate::logic::intake::{
    defaults, feedback_summary, identity_tone, new_integration, outcome_tone, trigger_summary,
    who_summary, ConnectForm,
};
use crate::nav::use_open_task;
use crate::store::{tz, use_store};
use crate::ui::dialog::{Confirm, Modal};
use crate::ui::{uid, Empty, Field, FieldIds, Ic, Icon, SecretBox};

pub fn kind_icon(k: IntakeKind) -> Ic {
    match k {
        IntakeKind::Github => Ic::Branch,
        IntakeKind::Linear => Ic::Zap,
        IntakeKind::Slack => Ic::Message,
        IntakeKind::Jira => Ic::FileText,
    }
}

#[component]
pub fn ProjectIntegrations(p: ProjectView) -> impl IntoView {
    let store = use_store();
    let pid = p.id;
    let owner = p.role == queued_api::ProjectRole::Owner;
    let list = RwSignal::new(None::<Vec<IntegrationView>>);
    let created = RwSignal::new(None::<CreatedIntegration>);
    let connecting = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            match api::get::<Vec<IntegrationView>>(&format!("/projects/{pid}/integrations")).await {
                Ok(l) => list.set(Some(l)),
                Err(e) => store.toast_error("Anbindungen konnten nicht geladen werden", &e),
            }
        })
    };
    load();
    let reload = Callback::new(move |_| load());
    let show = Callback::new(move |c: CreatedIntegration| {
        created.set(Some(c));
        load();
    });
    view! {
        <div class="stack" style="margin-top: var(--sp-4)" data-testid="project-integrations">
            <section class="card settings-card" aria-labelledby="in-head">
                <header>
                    <span class="s-icon"><Icon icon=Ic::Link/></span>
                    <div>
                        <h2 id="in-head">"Anbindungen"</h2>
                        <p>"Aufträge direkt aus GitHub Issues, Linear, Slack oder Jira – Queue meldet dort den Stand und nimmt Antworten auf Rückfragen entgegen."</p>
                    </div>
                </header>
                {(!owner).then(|| view! {
                    <p class="alert tone-info"><Icon icon=Ic::Eye class="sm"/>"Verbinden kann nur die Projekt-Eigentümerin. Dein eigenes Konto ordnest du unten bei „Personen“ zu."</p>
                })}
                <p class="small muted">"Sicher voreingestellt: Nur Personen, die hier zugeordnet und bestätigt sind, können Aufträge anlegen oder Rückfragen beantworten. Texte von außen gehen nur als gekennzeichnete Daten an den Agenten."</p>
                {move || match list.get() {
                    None => view! { <p class="muted" role="status">"Lade Anbindungen …"</p> }.into_any(),
                    Some(l) if l.is_empty() => view! {
                        <Empty icon=Ic::Link title="Noch keine Anbindung" text="Verbinde ein Werkzeug, in dem dein Team schon arbeitet. Aufträge entstehen dann dort, wo die Arbeit beschrieben wird."/>
                    }.into_any(),
                    Some(l) => view! {
                        <ul class="plain stack" aria-label="Anbindungen">
                            {l.into_iter().map(|i| view! { <IntegrationCard i=i on_change=reload on_secret=show/> }).collect_view()}
                        </ul>
                    }.into_any(),
                }}
                {move || (owner && !connecting.get()).then(|| view! {
                    <button type="button" class="btn primary" style="align-self:flex-start" data-testid="connect-integration" on:click=move |_| connecting.set(true)>
                        <Icon icon=Ic::Plus class="sm"/>"Anbindung verbinden"
                    </button>
                })}
                {move || connecting.get().then(|| view! {
                    <ConnectForm pid=pid on_done=Callback::new(move |r: Option<CreatedIntegration>| {
                        connecting.set(false);
                        if let Some(c) = r { show.run(c); }
                    })/>
                })}
            </section>
            {move || created.get().map(|c| view! { <CreatedDialog c=c on_close=Callback::new(move |_| created.set(None))/> })}
        </div>
    }
}

#[component]
fn ConnectForm(pid: Uuid, on_done: Callback<Option<CreatedIntegration>>) -> impl IntoView {
    let kind = RwSignal::new(IntakeKind::Github);
    let form = RwSignal::new(defaults(IntakeKind::Github));
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let set = move |f: fn(&mut ConnectForm, String)| {
        move |ev: leptos::ev::Event| {
            let v = event_target_value(&ev);
            form.update(|x| f(x, v));
        }
    };
    let ids = [
        FieldIds::new("in-name"),
        FieldIds::new("in-tenant"),
        FieldIds::new("in-secret"),
        FieldIds::new("in-token"),
        FieldIds::new("in-account"),
        FieldIds::new("in-label"),
        FieldIds::new("in-mention"),
        FieldIds::new("in-notes"),
        FieldIds::new("in-rate"),
    ];
    let [n, te, se, to, ac, la, me, no, ra] = ids;
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        let req = match new_integration(kind.get_untracked(), &form.get_untracked()) {
            Ok(r) => r,
            Err(e) => {
                error.set(Some(e));
                return;
            }
        };
        busy.set(true);
        spawn_local(async move {
            let r =
                api::post::<_, CreatedIntegration>(&format!("/projects/{pid}/integrations"), &req)
                    .await;
            busy.set(false);
            match r {
                Ok(c) => on_done.run(Some(c)),
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    view! {
        <form class="sub-card stack" on:submit=submit novalidate data-testid="connect-form">
            <fieldset class="choice-list" role="radiogroup" aria-label="Womit verbinden?">
                <legend class="small"><strong>"Womit verbinden?"</strong></legend>
                {IntakeKind::ALL.into_iter().map(|k| view! {
                    <label class="choice" data-testid=format!("kind-{}", k.as_str())>
                        <input type="radio" name="intake_kind" value=k.as_str()
                            prop:checked=move || kind.get() == k
                            on:change=move |_| { kind.set(k); form.set(defaults(k)); error.set(None); }/>
                        <span class="check-text"><span class="row"><Icon icon=kind_icon(k) class="sm"/>{k.label()}</span><small>{k.help()}</small></span>
                    </label>
                }).collect_view()}
            </fieldset>
            {move || {
                let k = kind.get();
                let (n, te, se, to, ac, la, me) = (n.clone(), te.clone(), se.clone(), to.clone(), ac.clone(), la.clone(), me.clone());
                view! {
                    <div class="grid-2">
                        <Field ids=n.clone() label="Name" hint="Wie die Anbindung in Queue heißt">
                            <input id=n.id.clone() class="input" maxlength="80" aria-describedby=n.described_by()
                                prop:value=move || form.with(|f| f.name.clone()) on:input=set(|f, v| f.name = v)/>
                        </Field>
                        <Field ids=te.clone() label=k.tenant_label() hint=k.tenant_help()>
                            <input id=te.id.clone() class="input" autocomplete="off" placeholder=k.tenant_placeholder() aria-describedby=te.described_by()
                                prop:value=move || form.with(|f| f.tenant.clone()) on:input=set(|f, v| f.tenant = v) data-testid="tenant"/>
                        </Field>
                    </div>
                    {(!k.queue_makes_secret()).then(|| view! {
                        <Field ids=se.clone() label=k.secret_label() hint="Wird verschlüsselt gespeichert und nie wieder angezeigt.">
                            <input id=se.id.clone() class="input" type="password" autocomplete="off" aria-describedby=se.described_by()
                                prop:value=move || form.with(|f| f.secret.clone()) on:input=set(|f, v| f.secret = v) data-testid="secret"/>
                        </Field>
                    })}
                    {matches!(k, IntakeKind::Github | IntakeKind::Jira).then(|| view! {
                        <div class="grid-2">
                            <Field ids=la.clone() label="Label" hint="Issues mit diesem Label werden zu Aufträgen">
                                <input id=la.id.clone() class="input" maxlength="50" aria-describedby=la.described_by()
                                    prop:value=move || form.with(|f| f.label.clone()) on:input=set(|f, v| f.label = v)/>
                            </Field>
                            {(k == IntakeKind::Github).then(|| view! {
                                <Field ids=me.clone() label="Erwähnung" hint="Steht das im Issue oder einem Kommentar, wird es ebenfalls ein Auftrag (leer = nur Label)">
                                    <input id=me.id.clone() class="input" maxlength="40" aria-describedby=me.described_by()
                                        prop:value=move || form.with(|f| f.mention.clone()) on:input=set(|f, v| f.mention = v)/>
                                </Field>
                            })}
                        </div>
                    })}
                    <Field ids=to.clone() label=format!("{} (für Rückmeldungen)", k.token_label()) hint=k.token_help()>
                        <input id=to.id.clone() class="input" type="password" autocomplete="off" aria-describedby=to.described_by()
                            prop:value=move || form.with(|f| f.token.clone()) on:input=set(|f, v| f.token = v)/>
                    </Field>
                    {k.needs_account().then(|| view! {
                        <Field ids=ac.clone() label="E-Mail des Jira-Kontos" hint="Zu diesem Konto gehört das API-Token">
                            <input id=ac.id.clone() class="input" type="email" autocomplete="off" aria-describedby=ac.described_by()
                                prop:value=move || form.with(|f| f.account.clone()) on:input=set(|f, v| f.account = v)/>
                        </Field>
                    })}
                }
            }}
            <details class="block">
                <summary>"Erweitert"</summary>
                <div class="stack" style="margin-top: var(--sp-3)">
                    <Field ids=no.clone() label="Hinweise für jeden Auftrag" hint="Optional, z. B. „Schreibe immer Tests.“ – stammt von dir und gilt als vertrauenswürdig.">
                        <textarea id=no.id.clone() class="textarea" rows="3" maxlength="2000" aria-describedby=no.described_by()
                            prop:value=move || form.with(|f| f.instructions.clone()) on:input=set(|f, v| f.instructions = v)></textarea>
                    </Field>
                    <Field ids=ra.clone() label="Höchstens Aufträge pro Stunde" hint="1–100; schützt vor Fluten und Kosten">
                        <input id=ra.id.clone() class="input" inputmode="numeric" aria-describedby=ra.described_by()
                            prop:value=move || form.with(|f| f.per_hour.clone()) on:input=set(|f, v| f.per_hour = v)/>
                    </Field>
                    <label class="check">
                        <input type="checkbox" prop:checked=move || form.with(|f| f.allow_unmapped)
                            on:change=move |ev| { let on = event_target_checked(&ev); form.update(|f| f.allow_unmapped = on); }/>
                        <span class="check-text">"Auch nicht zugeordnete Personen dürfen Aufträge anlegen"
                            <small>"Sie handeln dann als du – mit deinem Zugang und auf deine Kosten. Bei GitHub zusätzlich nur mit Schreibrechten im Repository. Standard: aus."</small>
                        </span>
                    </label>
                </div>
            </details>
            {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            <div class="form-actions">
                <button type="submit" class="btn primary" disabled=move || busy.get() data-testid="connect-submit"><Icon icon=Ic::Check class="sm"/>"Verbinden"</button>
                <button type="button" class="btn ghost" on:click=move |_| on_done.run(None)>"Abbrechen"</button>
            </div>
        </form>
    }
}

#[component]
fn CreatedDialog(c: CreatedIntegration, on_close: Callback<()>) -> impl IntoView {
    let title = uid("in-created");
    let desc = uid("in-created-desc");
    let i = c.integration.clone();
    view! {
        <Modal on_close=on_close labelledby=title.clone() describedby=desc.clone()>
            <span class="dialog-icon tone-success"><Icon icon=Ic::Link class="lg"/></span>
            <h2 id=title>{format!("„{}“ einrichten", i.name)}</h2>
            <p id=desc class="alert tone-warning"><Icon icon=Ic::Alert class="sm"/>{if c.secret.is_some() {
                "Das Secret wird nur jetzt angezeigt. Kopiere es in die Einstellungen des anderen Werkzeugs."
            } else {
                "Trage diese Adresse beim anderen Werkzeug ein."
            }}</p>
            <SecretBox label="Adresse" value=i.hook_url.clone() testid="intake-url"/>
            {c.secret.clone().map(|s| view! { <SecretBox label="Secret" value=s testid="intake-secret"/> })}
            <ol class="small stack" style="padding-left: 1.2em; margin: 0" data-testid="intake-steps">
                {c.steps.into_iter().map(|s| view! { <li style="overflow-wrap:anywhere">{s}</li> }).collect_view()}
            </ol>
            <div class="dialog-actions">
                <button type="button" class="btn primary" data-autofocus on:click=move |_| on_close.run(())>"Fertig"</button>
            </div>
        </Modal>
    }
}

#[component]
fn IntegrationCard(
    i: IntegrationView,
    on_change: Callback<()>,
    on_secret: Callback<CreatedIntegration>,
) -> impl IntoView {
    let store = use_store();
    let id = i.id;
    let name = i.name.clone();
    let editing = RwSignal::new(false);
    let (fb_text, fb_tone) = feedback_summary(&i);
    let toggle = move |ev: leptos::ev::Event| {
        let on = event_target_checked(&ev);
        store.run("Ändern fehlgeschlagen", async move {
            api::patch::<_, IntegrationView>(
                &format!("/integrations/{id}"),
                &UpdateIntegration {
                    enabled: Some(on),
                    ..Default::default()
                },
            )
            .await?;
            store.toast(if on {
                "Anbindung eingeschaltet"
            } else {
                "Anbindung ausgeschaltet"
            });
            on_change.run(());
            Ok(())
        });
    };
    let renew = {
        let name = name.clone();
        move |_| {
            let name = name.clone();
            store.ask(Confirm {
                title: format!("Neues Secret für „{name}“?"),
                body: "Das alte Secret gilt sofort nicht mehr. Trage das neue danach im anderen Werkzeug ein.".into(),
                confirm_label: "Neues Secret erzeugen".into(),
                danger: true,
                on_confirm: Callback::new(move |_| {
                    store.run("Erneuern fehlgeschlagen", async move {
                        let c: CreatedIntegration = api::post(&format!("/integrations/{id}/secret"), &serde_json::json!({})).await?;
                        on_secret.run(c);
                        Ok(())
                    });
                }),
            });
        }
    };
    let remove = {
        let name = name.clone();
        move |_| {
            let name = name.clone();
            store.ask(Confirm {
                title: format!("Anbindung „{name}“ löschen?"),
                body: "Queue nimmt danach nichts mehr an und meldet nichts mehr zurück. Bereits angelegte Aufträge bleiben.".into(),
                confirm_label: "Löschen".into(),
                danger: true,
                on_confirm: Callback::new(move |_| {
                    store.run("Löschen fehlgeschlagen", async move {
                        api::delete(&format!("/integrations/{id}")).await?;
                        store.toast("Anbindung gelöscht");
                        on_change.run(());
                        Ok(())
                    });
                }),
            });
        }
    };
    let iv = i.clone();
    // Who may act changes with the people below: refresh this card only
    // (a reload of the whole list would rebuild it and close the section).
    let who = RwSignal::new(who_summary(&i));
    let refresh = Callback::new(move |_| {
        spawn_local(async move {
            if let Ok(v) = api::get::<IntegrationView>(&format!("/integrations/{id}")).await {
                who.set(who_summary(&v));
            }
        })
    });
    let now = store.now.get_untracked();
    view! {
        <li class="sub-card" data-testid="integration-row" data-kind=i.kind.as_str()>
            <div class="row wrap">
                <span class="tag"><Icon icon=kind_icon(i.kind) class="sm"/>{i.kind.label()}</span>
                <h3 class="grow">{i.name.clone()}</h3>
                {i.can_manage.then(|| view! {
                    <label class="switch">
                        <input type="checkbox" role="switch" prop:checked=i.enabled on:change=toggle
                            aria-label=format!("Anbindung {} eingeschaltet", i.name)/>
                        <span class="check-text">{if i.enabled { "An" } else { "Aus" }}</span>
                    </label>
                })}
            </div>
            <p class="small">{format!("{}: ", i.kind.tenant_label())}<span class="mono" style="overflow-wrap:anywhere">{i.tenant.clone()}</span></p>
            <p class="small" data-testid="integration-trigger">{trigger_summary(&i)}</p>
            <p class="small" data-testid="integration-who">{move || who.get()}</p>
            <p class="small"><span class=format!("tag {}", fb_tone.class())>{fb_text}</span></p>
            {i.disabled_reason.clone().map(|r| view! { <p class="alert tone-warning" role="status"><Icon icon=Ic::Alert class="sm"/>{r}</p> })}
            <p class="small">"Adresse: "<span class="mono" style="overflow-wrap:anywhere">{i.hook_url.clone()}</span></p>
            {i.last_event.clone().map(|e| view! {
                <p class="small">"Zuletzt: "<span class=format!("tag {}", outcome_tone(e.outcome).class())>{e.outcome.label()}</span>
                    {format!(" {}{}", fmt_time(e.created_at, now, &tz()), e.reason.map(|r| format!(" – {r}")).unwrap_or_default())}</p>
            })}
            {i.can_manage.then(|| view! {
                <div class="row wrap">
                    <button type="button" class="btn sm" aria-expanded=move || editing.get().to_string() on:click=move |_| editing.update(|e| *e = !*e)><Icon icon=Ic::Pencil class="sm"/>"Bearbeiten"</button>
                    {i.kind.queue_makes_secret().then(|| view! {
                        <button type="button" class="btn sm" on:click=renew.clone()><Icon icon=Ic::Key class="sm"/>"Neues Secret"</button>
                    })}
                    <button type="button" class="btn sm icon-only danger" aria-label=format!("Anbindung {} löschen", i.name) title="Löschen" on:click=remove.clone()><Icon icon=Ic::Trash/></button>
                </div>
            })}
            {move || editing.get().then(|| view! {
                <EditForm i=iv.clone() on_done=Callback::new(move |saved: bool| {
                    editing.set(false);
                    if saved { on_change.run(()); }
                })/>
            })}
            <People i=i.clone() on_change=refresh/>
            <History id=id/>
        </li>
    }
}

#[component]
fn EditForm(i: IntegrationView, on_done: Callback<bool>) -> impl IntoView {
    let store = use_store();
    let id = i.id;
    let kind = i.kind;
    let name = RwSignal::new(i.name.clone());
    let label = RwSignal::new(i.config.label.clone().unwrap_or_default());
    let mention = RwSignal::new(i.config.mention.clone().unwrap_or_default());
    let notes = RwSignal::new(i.config.instructions.clone().unwrap_or_default());
    let open = RwSignal::new(i.config.allow_unmapped);
    let token = RwSignal::new(String::new());
    let account = RwSignal::new(i.account.clone().unwrap_or_default());
    let secret = RwSignal::new(String::new());
    let per_hour = RwSignal::new(i.max_per_hour.to_string());
    let error = RwSignal::new(None::<String>);
    let ids = [
        FieldIds::new("ie-name"),
        FieldIds::new("ie-label"),
        FieldIds::new("ie-mention"),
        FieldIds::new("ie-token"),
        FieldIds::new("ie-account"),
        FieldIds::new("ie-secret"),
        FieldIds::new("ie-notes"),
        FieldIds::new("ie-rate"),
    ];
    let [n, la, me, to, ac, se, no, ra] = ids;
    let has_token = i.has_token;
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        let max = match per_hour.get_untracked().trim().parse::<i32>() {
            Ok(v) if (1..=100).contains(&v) => v,
            _ => {
                error.set(Some("Aufträge pro Stunde: eine Zahl von 1 bis 100".into()));
                return;
            }
        };
        let opt = |s: String| {
            let t = s.trim().to_string();
            (!t.is_empty()).then_some(t)
        };
        let patch = UpdateIntegration {
            name: Some(name.get_untracked()),
            config: Some(IntakeConfig {
                label: opt(label.get_untracked()),
                mention: opt(mention.get_untracked()),
                allow_unmapped: open.get_untracked(),
                instructions: opt(notes.get_untracked()),
            }),
            max_per_hour: Some(max),
            token: opt(token.get_untracked()),
            account: kind.needs_account().then(|| account.get_untracked()),
            secret: opt(secret.get_untracked()),
            enabled: None,
        };
        spawn_local(async move {
            match api::patch::<_, IntegrationView>(&format!("/integrations/{id}"), &patch).await {
                Ok(_) => {
                    store.toast("Anbindung gespeichert");
                    on_done.run(true);
                }
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    let drop_token = move |_| {
        store.run("Ändern fehlgeschlagen", async move {
            api::patch::<_, IntegrationView>(
                &format!("/integrations/{id}"),
                &UpdateIntegration {
                    token: Some(String::new()),
                    ..Default::default()
                },
            )
            .await?;
            store.toast("Token entfernt – Rückmeldungen sind aus");
            on_done.run(true);
            Ok(())
        });
    };
    view! {
        <form class="sub-card stack" on:submit=submit novalidate data-testid="integration-edit">
            <Field ids=n.clone() label="Name">
                <input id=n.id.clone() class="input" maxlength="80" aria-describedby=n.described_by() bind:value=name/>
            </Field>
            {matches!(kind, IntakeKind::Github | IntakeKind::Jira).then(|| view! {
                <div class="grid-2">
                    <Field ids=la.clone() label="Label">
                        <input id=la.id.clone() class="input" maxlength="50" aria-describedby=la.described_by() bind:value=label/>
                    </Field>
                    {(kind == IntakeKind::Github).then(|| view! {
                        <Field ids=me.clone() label="Erwähnung">
                            <input id=me.id.clone() class="input" maxlength="40" aria-describedby=me.described_by() bind:value=mention/>
                        </Field>
                    })}
                </div>
            })}
            <Field ids=to.clone() label=if has_token { "Neues Token (leer = unverändert)".to_string() } else { kind.token_label().to_string() } hint=kind.token_help()>
                <input id=to.id.clone() class="input" type="password" autocomplete="off" aria-describedby=to.described_by() bind:value=token/>
            </Field>
            {kind.needs_account().then(|| view! {
                <Field ids=ac.clone() label="E-Mail des Jira-Kontos">
                    <input id=ac.id.clone() class="input" type="email" aria-describedby=ac.described_by() bind:value=account/>
                </Field>
            })}
            {(!kind.queue_makes_secret()).then(|| view! {
                <Field ids=se.clone() label=format!("Neues {} (leer = unverändert)", kind.secret_label())>
                    <input id=se.id.clone() class="input" type="password" autocomplete="off" aria-describedby=se.described_by() bind:value=secret/>
                </Field>
            })}
            <Field ids=no.clone() label="Hinweise für jeden Auftrag">
                <textarea id=no.id.clone() class="textarea" rows="3" maxlength="2000" aria-describedby=no.described_by() bind:value=notes></textarea>
            </Field>
            <Field ids=ra.clone() label="Höchstens Aufträge pro Stunde">
                <input id=ra.id.clone() class="input" inputmode="numeric" aria-describedby=ra.described_by() bind:value=per_hour/>
            </Field>
            <label class="check">
                <input type="checkbox" prop:checked=move || open.get() on:change=move |ev| open.set(event_target_checked(&ev))/>
                <span class="check-text">"Auch nicht zugeordnete Personen dürfen Aufträge anlegen"<small>"Sie handeln dann als Eigentümerin der Anbindung (deren Zugang, deren Kosten)."</small></span>
            </label>
            {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            <div class="form-actions">
                <button type="submit" class="btn primary"><Icon icon=Ic::Check class="sm"/>"Speichern"</button>
                {has_token.then(|| view! { <button type="button" class="btn" on:click=drop_token>"Token entfernen"</button> })}
                <button type="button" class="btn ghost" on:click=move |_| on_done.run(false)>"Abbrechen"</button>
            </div>
        </form>
    }
}

#[component]
fn People(i: IntegrationView, on_change: Callback<()>) -> impl IntoView {
    let store = use_store();
    let id = i.id;
    let kind = i.kind;
    let data = RwSignal::new(None::<Identities>);
    let xid = RwSignal::new(String::new());
    let xname = RwSignal::new(String::new());
    let who = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let load = move || {
        spawn_local(async move {
            match api::get::<Identities>(&format!("/integrations/{id}/identities")).await {
                Ok(d) => {
                    if who.get_untracked().is_empty() {
                        if let Some(m) = d.mappable.first() {
                            who.set(m.user_id.to_string());
                        }
                    }
                    data.set(Some(d));
                }
                Err(e) => store.toast_error("Personen konnten nicht geladen werden", &e),
            }
        })
    };
    let changed = move |list: Vec<IdentityView>| {
        data.update(|d| {
            if let Some(d) = d {
                d.identities = list;
            }
        });
        on_change.run(());
    };
    let add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        let Ok(user_id) = Uuid::parse_str(&who.get_untracked()) else {
            error.set(Some("Bitte eine Person auswählen".into()));
            return;
        };
        let body = NewIdentity {
            external_id: xid.get_untracked(),
            external_name: xname.get_untracked(),
            user_id,
        };
        spawn_local(async move {
            match api::post::<_, Vec<IdentityView>>(
                &format!("/integrations/{id}/identities"),
                &body,
            )
            .await
            {
                Ok(l) => {
                    xid.set(String::new());
                    xname.set(String::new());
                    store.toast("Zuordnung gespeichert");
                    changed(l);
                }
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    let act = move |path: String, delete: bool| {
        spawn_local(async move {
            let r = if delete {
                match api::delete(&path).await {
                    Ok(()) => api::get::<Identities>(&format!("/integrations/{id}/identities"))
                        .await
                        .map(|d| d.identities),
                    Err(e) => Err(e),
                }
            } else {
                api::post::<_, Vec<IdentityView>>(&path, &serde_json::json!({})).await
            };
            match r {
                Ok(l) => changed(l),
                Err(e) => store.toast_error("Ändern fehlgeschlagen", &e),
            }
        })
    };
    let (fx, fn_, fw) = (
        FieldIds::new("ip-id"),
        FieldIds::new("ip-name"),
        FieldIds::new("ip-who"),
    );
    view! {
        <details class="block" data-testid="integration-people" on:toggle=move |ev| {
            use wasm_bindgen::JsCast;
            if ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlDetailsElement>().ok()).is_some_and(|d| d.open()) {
                load();
            }
        }>
            <summary>"Personen"</summary>
            <p class="small muted">{format!("Welches {}-Konto gehört zu wem im Projekt? Eine Zuordnung gilt erst, wenn die Person selbst und die Projekt-Eigentümerin zugestimmt haben – denn Aufträge laufen mit dem Zugang der Person.", kind.short())}</p>
            {move || data.get().map(|d| {
                let rows = d.identities.clone();
                let mappable = d.mappable.clone();
                view! {
                    {if rows.is_empty() {
                        view! { <p class="small muted">"Noch niemand zugeordnet."</p> }.into_any()
                    } else {
                        view! {
                            <ul class="plain stack" aria-label="Zugeordnete Konten">
                                {rows.into_iter().map(|r| {
                                    let iid = r.id;
                                    let label = if r.external_name.is_empty() { r.external_id.clone() } else { format!("{} ({})", r.external_name, r.external_id) };
                                    view! {
                                        <li class="row wrap small" data-testid="identity-row" data-state=match r.state { IdentityState::Active => "active", IdentityState::WaitingUser => "waiting_user", IdentityState::WaitingOwner => "waiting_owner" }>
                                            <span class="mono" style="overflow-wrap:anywhere">{label}</span>
                                            <span aria-hidden="true">"→"</span>
                                            <strong class="grow">{r.user_name.clone()}</strong>
                                            <span class=format!("tag {}", identity_tone(r.state).class())>{r.state.label()}</span>
                                            {r.can_confirm.then(|| view! {
                                                <button type="button" class="btn sm" on:click=move |_| act(format!("/integrations/{id}/identities/{iid}/confirm"), false)>
                                                    <Icon icon=Ic::Check class="sm"/>"Bestätigen"
                                                </button>
                                            })}
                                            {r.can_remove.then(|| view! {
                                                <button type="button" class="btn sm icon-only danger" aria-label=format!("Zuordnung {} entfernen", r.external_id) title="Entfernen"
                                                    on:click=move |_| act(format!("/integrations/{id}/identities/{iid}"), true)>
                                                    <Icon icon=Ic::Trash/>
                                                </button>
                                            })}
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                        }.into_any()
                    }}
                    {(!mappable.is_empty()).then(|| {
                        let (fx, fn_, fw) = (fx.clone(), fn_.clone(), fw.clone());
                        view! {
                            <form class="stack" on:submit=add novalidate data-testid="identity-form">
                                <div class="grid-3">
                                    <Field ids=fx.clone() label=kind.person_id_label() hint=kind.person_id_help()>
                                        <input id=fx.id.clone() class="input" autocomplete="off" aria-describedby=fx.described_by() bind:value=xid/>
                                    </Field>
                                    <Field ids=fn_.clone() label="Name dort (optional)">
                                        <input id=fn_.id.clone() class="input" maxlength="200" aria-describedby=fn_.described_by() bind:value=xname/>
                                    </Field>
                                    <Field ids=fw.clone() label="Person im Projekt">
                                        <select id=fw.id.clone() class="select" aria-describedby=fw.described_by() on:change=move |ev| who.set(event_target_value(&ev))>
                                            {mappable.into_iter().map(|m| {
                                                let v = m.user_id.to_string();
                                                let sel = v.clone();
                                                view! { <option value=v selected=move || who.get() == sel>{m.name}</option> }
                                            }).collect_view()}
                                        </select>
                                    </Field>
                                </div>
                                {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
                                <button type="submit" class="btn sm" style="align-self:flex-start"><Icon icon=Ic::Plus class="sm"/>"Zuordnen"</button>
                            </form>
                        }
                    })}
                }
            })}
        </details>
    }
}

#[component]
fn History(id: Uuid) -> impl IntoView {
    let store = use_store();
    let rows = RwSignal::new(None::<Vec<IntakeEventView>>);
    let open_task = use_open_task();
    let load = move || {
        spawn_local(async move {
            match api::get::<Vec<IntakeEventView>>(&format!("/integrations/{id}/events")).await {
                Ok(r) => rows.set(Some(r)),
                Err(e) => store.toast_error("Verlauf konnte nicht geladen werden", &e),
            }
        })
    };
    view! {
        <details class="block" data-testid="integration-history" on:toggle=move |ev| {
            use wasm_bindgen::JsCast;
            if ev.target().and_then(|t| t.dyn_into::<web_sys::HtmlDetailsElement>().ok()).is_some_and(|d| d.open()) {
                load();
            }
        }>
            <summary>"Verlauf"</summary>
            {move || match rows.get() {
                None => view! { <p class="muted small" role="status">"Lade …"</p> }.into_any(),
                Some(r) if r.is_empty() => view! { <p class="muted small">"Noch nichts angekommen."</p> }.into_any(),
                Some(r) => {
                    let now = store.now.get_untracked();
                    let open_task = open_task.clone();
                    view! {
                        <ul class="plain stack" aria-label="Verlauf der Anbindung">
                            {r.into_iter().map(|x| {
                                let open_task = open_task.clone();
                                view! {
                                    <li class="row wrap small" data-testid="intake-event" data-outcome=x.outcome.as_str()>
                                        <span class=format!("tag {}", outcome_tone(x.outcome).class())>{x.outcome.label()}</span>
                                        <span>{fmt_time(x.created_at, now, &tz())}</span>
                                        {x.event.clone().map(|e| view! { <span class="muted mono">{e}</span> })}
                                        {x.external_user.clone().map(|u| view! { <span class="muted">{format!("von {u}")}</span> })}
                                        {x.reason.clone().map(|r| view! { <span class="grow" style="overflow-wrap:anywhere">{r}</span> })}
                                        {x.task_id.map(|t| view! { <button type="button" class="btn sm ghost" on:click=move |_| open_task(t)>"Auftrag öffnen"</button> })}
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    }.into_any()
                }
            }}
        </details>
    }
}

/// Task drawer: where the task came from, with a link.
#[component]
pub fn IntakeBlock(link: queued_api::intake::IntakeLinkView) -> impl IntoView {
    view! {
        <section class="block intake-link" aria-label="Herkunft des Auftrags" data-testid="intake-link">
            <p class="row wrap small">
                <span class="tag tone-info"><Icon icon=kind_icon(link.kind) class="sm"/>{format!("Aus {}", link.label)}</span>
                {match link.url.clone() {
                    Some(u) => view! {
                        <a class="btn sm" href=u target="_blank" rel="noopener noreferrer">
                            <Icon icon=Ic::ExternalLink class="sm"/>{format!("In {} öffnen", link.kind.short())}
                        </a>
                    }.into_any(),
                    None => ().into_any(),
                }}
            </p>
            {link.title.clone().map(|t| view! { <p class="small"><strong>{t}</strong></p> })}
            <p class="small muted">{if link.updates {
                format!("Über die Anbindung „{}“. Queue meldet den Stand dorthin zurück{}.", link.integration_name, link.last_update.clone().map(|u| format!(" – zuletzt: {u}")).unwrap_or_default())
            } else {
                format!("Über die Anbindung „{}“. Rückmeldungen dorthin sind aus (kein Token eingetragen).", link.integration_name)
            }}</p>
        </section>
    }
}
