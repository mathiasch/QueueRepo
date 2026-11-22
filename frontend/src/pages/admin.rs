//! WP11: admin console – people, invitations, sign-up and MFA rules, the
//! audit log and the runner fleet. Only for admins (the server checks).
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::security::{
    audit_label, AdminUser, AuditPage, AuditVerify, CreatedInvitation, CreatedRunner,
    InstanceSettings, Invitation, InviteStatus, MfaPolicy, MoveProject, MoveResult, NewInvitation,
    NewRunner, Quota, RegistrationMode, RunnerProject, RunnerStatus, RunnerView,
    UpdateInstanceSettings, UpdateRunner, UpdateUser, AUDIT_GROUPS, INVITE_DAYS,
};
use serde_json::json;
use uuid::Uuid;

use crate::api;
use crate::logic::format::fmt_time;

fn when(ts: chrono::DateTime<chrono::Utc>) -> String {
    fmt_time(ts, chrono::Utc::now(), &tz())
}
use crate::logic::security::{ago, audit_details, parse_limit, runner_env, usd};
use crate::nav::set_title;
use crate::store::{tz, use_store};
use crate::ui::dialog::{Confirm, Modal};
use crate::ui::{uid, Empty, FieldIds, Ic, Icon, SecretBox};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    People,
    Invites,
    Rules,
    Audit,
    Runners,
}

const TABS: [(Tab, &str, Ic); 5] = [
    (Tab::People, "Personen", Ic::Users),
    (Tab::Invites, "Einladungen", Ic::Send),
    (Tab::Rules, "Regeln", Ic::Shield),
    (Tab::Audit, "Protokoll", Ic::ListChecks),
    (Tab::Runners, "Runner", Ic::Terminal),
];

#[component]
pub fn AdminPage() -> impl IntoView {
    set_title("Verwaltung");
    let store = use_store();
    let tab = RwSignal::new(Tab::People);
    if !store.me.get_untracked().is_admin {
        return view! {
            <div class="card">
                <Empty icon=Ic::Ban title="Nur für Admins" text="Diese Seite sehen nur Personen mit Admin-Rolle."/>
            </div>
        }
        .into_any();
    }
    view! {
        <div class="page-head">
            <div>
                <h1>"Verwaltung"</h1>
                <p>"Wer Zugang hat, welche Regeln gelten, was passiert ist – und wo die Agenten laufen."</p>
            </div>
        </div>
        <div class="segmented admin-tabs" role="tablist" aria-label="Bereiche der Verwaltung">
            {TABS.into_iter().map(|(t, label, icon)| view! {
                <button type="button" role="tab" id=format!("admin-tab-{label}")
                    aria-selected=move || (tab.get() == t).to_string()
                    aria-controls="admin-panel"
                    on:click=move |_| tab.set(t)>
                    <Icon icon=icon class="sm"/>{label}
                </button>
            }).collect_view()}
        </div>
        <div id="admin-panel" role="tabpanel" class="stack" aria-labelledby=move || {
            TABS.iter().find(|(t, _, _)| *t == tab.get()).map(|(_, l, _)| format!("admin-tab-{l}")).unwrap_or_default()
        }>
            {move || match tab.get() {
                Tab::People => view! { <People/> }.into_any(),
                Tab::Invites => view! { <Invites/> }.into_any(),
                Tab::Rules => view! { <Rules/> }.into_any(),
                Tab::Audit => view! { <Audit/> }.into_any(),
                Tab::Runners => view! { <Runners/> }.into_any(),
            }}
        </div>
    }
    .into_any()
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

#[component]
fn People() -> impl IntoView {
    let store = use_store();
    let q = RwSignal::new(String::new());
    let users = RwSignal::new(None::<Vec<AdminUser>>);
    let reload = move || {
        spawn_local(async move {
            match api::get::<Vec<AdminUser>>(&format!(
                "/admin/users?q={}",
                api::enc(&q.get_untracked())
            ))
            .await
            {
                Ok(u) => users.set(Some(u)),
                Err(e) => store.toast_error("Personen konnten nicht geladen werden", &e),
            }
        })
    };
    reload();
    let ids = FieldIds::new("user-search");
    view! {
        <section class="card pad stack" aria-labelledby="people-title">
            <h2 id="people-title" class="h3">"Personen"</h2>
            <form class="row wrap" on:submit=move |ev| { ev.prevent_default(); reload(); } role="search">
                <label class="visually-hidden" for=ids.id.clone()>"Personen suchen"</label>
                <input id=ids.id.clone() class="input grow" type="search" placeholder="Name oder E-Mail" bind:value=q/>
                <button class="btn" type="submit"><Icon icon=Ic::Search class="sm"/>"Suchen"</button>
            </form>
            {move || match users.get() {
                None => view! { <p class="muted" role="status">"Lade …"</p> }.into_any(),
                Some(list) if list.is_empty() => view! { <p class="muted">"Niemand gefunden."</p> }.into_any(),
                Some(list) => view! {
                    <ul class="plain stack" aria-label="Personen" data-testid="admin-users">
                        {list.into_iter().map(|u| view! { <PersonRow u=u on_change=Callback::new(move |_| reload())/> }).collect_view()}
                    </ul>
                }.into_any(),
            }}
        </section>
    }
}

#[component]
fn PersonRow(u: AdminUser, on_change: Callback<()>) -> impl IntoView {
    let store = use_store();
    let me = store.me.get_untracked().id;
    let id = u.id;
    let quota_open = RwSignal::new(false);
    let lock_open = RwSignal::new(false);
    let disabled = u.disabled_at.is_some();
    let is_admin = u.is_admin;
    let email = u.email.clone();
    let now = store.now.get_untracked();
    let seen = u
        .last_seen_at
        .map(|t| ago((now - t).num_seconds().max(0)))
        .unwrap_or_else(|| "nie".into());
    let set_admin = move |make: bool| {
        let email = email.clone();
        store.ask(Confirm {
            title: if make {
                format!("{email} zur Admin-Person machen?")
            } else {
                format!("{email} die Admin-Rolle entziehen?")
            },
            body: if make {
                "Admins sehen die Verwaltung, das Protokoll und können Personen sperren.".into()
            } else {
                "Die Person verliert den Zugang zur Verwaltung.".into()
            },
            confirm_label: if make {
                "Admin machen".into()
            } else {
                "Rolle entziehen".into()
            },
            danger: !make,
            on_confirm: Callback::new(move |_| {
                store.run("Rolle konnte nicht geändert werden", async move {
                    api::patch_ok(
                        &format!("/admin/users/{id}"),
                        &UpdateUser {
                            is_admin: Some(make),
                            ..Default::default()
                        },
                    )
                    .await?;
                    store.toast("Rolle geändert");
                    on_change.run(());
                    Ok(())
                });
            }),
        });
    };
    let set_admin2 = set_admin.clone();
    let unlock = move |_| {
        store.run("Entsperren fehlgeschlagen", async move {
            api::patch_ok(
                &format!("/admin/users/{id}"),
                &UpdateUser {
                    disabled: Some(false),
                    ..Default::default()
                },
            )
            .await?;
            store.toast("Person entsperrt");
            on_change.run(());
            Ok(())
        });
    };
    let quota_text = if u.quota.is_unlimited() {
        "Kein Kontingent".to_string()
    } else {
        let mut parts = vec![];
        if let Some(c) = u.quota.max_cost_30d_usd {
            parts.push(format!("{} von {} (30 Tage)", usd(u.cost_30d_usd), usd(c)));
        }
        if let Some(p) = u.quota.max_parallel {
            parts.push(format!("{p} gleichzeitig"));
        }
        if let Some(p) = u.quota.max_projects {
            parts.push(format!("max. {p} Projekte"));
        }
        parts.join(" · ")
    };
    let u2 = u.clone();
    let email3 = u.email.clone();
    view! {
        <li class="list-row stack-sm" data-testid="admin-user" data-email=u.email.clone()>
            <div class="row wrap between">
                <div class="grow">
                    <strong>{u.name.clone()}</strong>" "
                    {is_admin.then(|| view! { <span class="tag tone-info">"Admin"</span> })}
                    {disabled.then(|| view! { <span class="tag tone-danger">"Gesperrt"</span> })}
                    {u.mfa_enabled.then(|| view! { <span class="tag tone-success">"2FA"</span> })}
                    <br/>
                    <span class="small muted">{format!("{} · zuletzt aktiv: {} · {} Geräte · {} Tokens · {} Projekte",
                        u.email, seen, u.sessions, u.active_tokens, u.projects)}</span>
                    <br/>
                    <span class="small muted">{format!("Kosten 30 Tage: {} · {}", usd(u.cost_30d_usd), quota_text)}</span>
                    {disabled.then(|| (!u.disabled_reason.is_empty()).then(|| view! {
                        <br/><span class="small">{format!("Grund: {}", u.disabled_reason)}</span>
                    }))}
                </div>
                <div class="row wrap">
                    <button class="btn sm" type="button" on:click=move |_| quota_open.set(true)>"Kontingent"</button>
                    {if is_admin {
                        view! { <button class="btn sm" type="button" on:click=move |_| set_admin(false)>"Admin entziehen"</button> }.into_any()
                    } else {
                        view! { <button class="btn sm" type="button" on:click=move |_| set_admin2(true)>"Admin machen"</button> }.into_any()
                    }}
                    {if disabled {
                        view! { <button class="btn sm" type="button" on:click=unlock>"Entsperren"</button> }.into_any()
                    } else if id == me {
                        ().into_any()
                    } else {
                        view! { <button class="btn sm danger" type="button" data-testid="lock-user" on:click=move |_| lock_open.set(true)>"Sperren"</button> }.into_any()
                    }}
                </div>
            </div>
            {move || quota_open.get().then(|| view! { <QuotaDialog u=u2.clone() on_close=Callback::new(move |_| quota_open.set(false)) on_saved=on_change/> })}
            {move || lock_open.get().then(|| view! { <LockDialog id=id email=email3.clone() on_close=Callback::new(move |_| lock_open.set(false)) on_saved=on_change/> })}
        </li>
    }
}

#[component]
fn LockDialog(
    id: Uuid,
    email: String,
    on_close: Callback<()>,
    on_saved: Callback<()>,
) -> impl IntoView {
    let store = use_store();
    let t = uid("lock-title");
    let reason = RwSignal::new(String::new());
    let ids = FieldIds::new("lock-reason");
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        store.run("Sperren fehlgeschlagen", async move {
            api::patch_ok(
                &format!("/admin/users/{id}"),
                &UpdateUser {
                    disabled: Some(true),
                    reason: Some(reason.get_untracked()),
                    ..Default::default()
                },
            )
            .await?;
            store.toast("Person gesperrt – alle Geräte und Tokens sind abgemeldet");
            on_close.run(());
            on_saved.run(());
            Ok(())
        });
    };
    view! {
        <Modal on_close=on_close labelledby=t.clone() alert=true>
            <span class="dialog-icon tone-danger"><Icon icon=Ic::Ban class="lg"/></span>
            <h2 id=t>{format!("{email} sperren?")}</h2>
            <p class="muted">"Die Person wird sofort überall abgemeldet, ihre API-Tokens werden widerrufen und ihre Aufträge starten nicht mehr. Entsperren ist jederzeit möglich (Tokens bleiben widerrufen)."</p>
            <form class="stack" on:submit=submit>
                <div class="field">
                    <div class="label"><label for=ids.id.clone()>"Grund (optional, für das Protokoll)"</label></div>
                    <input id=ids.id.clone() class="input" maxlength="500" bind:value=reason/>
                </div>
                <div class="dialog-actions">
                    <button class="btn" type="button" on:click=move |_| on_close.run(())>"Abbrechen"</button>
                    <button class="btn danger solid" type="submit" data-testid="lock-confirm">"Sperren"</button>
                </div>
            </form>
        </Modal>
    }
}

#[component]
fn QuotaDialog(u: AdminUser, on_close: Callback<()>, on_saved: Callback<()>) -> impl IntoView {
    let store = use_store();
    let t = uid("quota-title");
    let cost = RwSignal::new(
        u.quota
            .max_cost_30d_usd
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    let par = RwSignal::new(
        u.quota
            .max_parallel
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    let proj = RwSignal::new(
        u.quota
            .max_projects
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    let error = RwSignal::new(String::new());
    let id = u.id;
    let (c_ids, p_ids, j_ids) = (
        FieldIds::new("q-cost"),
        FieldIds::new("q-par"),
        FieldIds::new("q-proj"),
    );
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let q = match (
            parse_limit::<f64>(&cost.get_untracked()),
            parse_limit::<i32>(&par.get_untracked()),
            parse_limit::<i32>(&proj.get_untracked()),
        ) {
            (Ok(c), Ok(p), Ok(n)) => Quota {
                max_cost_30d_usd: c,
                max_parallel: p,
                max_projects: n,
            },
            _ => {
                error.set("Bitte nur Zahlen eingeben (leer = unbegrenzt).".into());
                return;
            }
        };
        if let Err(e) = q.validate() {
            error.set(e);
            return;
        }
        store.run("Kontingent konnte nicht gespeichert werden", async move {
            api::put_ok(&format!("/admin/users/{id}/quota"), &q).await?;
            store.toast("Kontingent gespeichert");
            on_close.run(());
            on_saved.run(());
            Ok(())
        });
    };
    view! {
        <Modal on_close=on_close labelledby=t.clone()>
            <h2 id=t>{format!("Kontingent für {}", u.email)}</h2>
            <p class="muted">{format!("Leer lassen = unbegrenzt. Die Kosten kommen aus der Kostenrechnung (letzte 30 Tage, derzeit {}). Ist die Grenze erreicht, warten neue Aufträge.", usd(u.cost_30d_usd))}</p>
            <form class="stack" on:submit=submit>
                <div class="field">
                    <div class="label"><label for=c_ids.id.clone()>"Höchstkosten in 30 Tagen (USD)"</label></div>
                    <input id=c_ids.id.clone() class="input" inputmode="decimal" bind:value=cost data-testid="quota-cost"/>
                </div>
                <div class="field">
                    <div class="label"><label for=p_ids.id.clone()>"Gleichzeitige Aufträge (höchstens)"</label></div>
                    <input id=p_ids.id.clone() class="input" inputmode="numeric" bind:value=par/>
                </div>
                <div class="field">
                    <div class="label"><label for=j_ids.id.clone()>"Eigene Projekte (höchstens)"</label></div>
                    <input id=j_ids.id.clone() class="input" inputmode="numeric" bind:value=proj/>
                </div>
                <div role="alert">{move || (!error.get().is_empty()).then(|| view! { <div class="alert tone-danger">{error.get()}</div> })}</div>
                <div class="dialog-actions">
                    <button class="btn" type="button" on:click=move |_| on_close.run(())>"Abbrechen"</button>
                    <button class="btn primary" type="submit" data-testid="quota-save">"Speichern"</button>
                </div>
            </form>
        </Modal>
    }
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

#[component]
fn Invites() -> impl IntoView {
    let store = use_store();
    let list = RwSignal::new(None::<Vec<Invitation>>);
    let created = RwSignal::new(None::<CreatedInvitation>);
    let email = RwSignal::new(String::new());
    let note = RwSignal::new(String::new());
    let days = RwSignal::new("7".to_string());
    let admin = RwSignal::new(false);
    let reload = move || {
        spawn_local(async move {
            match api::get::<Vec<Invitation>>("/admin/invitations").await {
                Ok(l) => list.set(Some(l)),
                Err(e) => store.toast_error("Einladungen konnten nicht geladen werden", &e),
            }
        })
    };
    reload();
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let inv = NewInvitation {
            email: Some(email.get_untracked()).filter(|e| !e.trim().is_empty()),
            make_admin: admin.get_untracked(),
            note: note.get_untracked(),
            days: days.get_untracked().parse().unwrap_or(7),
        };
        if let Err(e) = inv.validate() {
            store.toast_error_text("Einladung nicht möglich", &e);
            return;
        }
        store.run("Einladung konnte nicht erstellt werden", async move {
            let c: CreatedInvitation = api::post("/admin/invitations", &inv).await?;
            created.set(Some(c));
            email.set(String::new());
            note.set(String::new());
            reload();
            Ok(())
        });
    };
    let (e_ids, n_ids, d_ids) = (
        FieldIds::new("inv-email"),
        FieldIds::new("inv-note"),
        FieldIds::new("inv-days"),
    );
    view! {
        <section class="card pad stack" aria-labelledby="invites-title">
            <h2 id="invites-title" class="h3">"Einladungen"</h2>
            <p class="muted">"Ein Einladungslink funktioniert genau einmal und läuft ab. Gib optional die E-Mail-Adresse an, dann kann nur diese Person ihn benutzen."</p>
            <form class="stack" on:submit=submit>
                <div class="grid-2">
                    <div class="field">
                        <div class="label"><label for=e_ids.id.clone()>"E-Mail-Adresse (optional)"</label></div>
                        <input id=e_ids.id.clone() class="input" type="email" autocomplete="off" bind:value=email data-testid="invite-email"/>
                    </div>
                    <div class="field">
                        <div class="label"><label for=d_ids.id.clone()>"Gültig für"</label></div>
                        <select id=d_ids.id.clone() class="input" on:change=move |ev| days.set(event_target_value(&ev))>
                            {INVITE_DAYS.into_iter().map(|(d, l)| view! { <option value=d.to_string() selected=d == 7>{l}</option> }).collect_view()}
                        </select>
                    </div>
                </div>
                <div class="field">
                    <div class="label"><label for=n_ids.id.clone()>"Notiz (optional)"</label></div>
                    <input id=n_ids.id.clone() class="input" maxlength="200" bind:value=note/>
                </div>
                <label class="check"><input type="checkbox" bind:checked=admin/>" Als Admin einladen"</label>
                <div><button class="btn primary" type="submit" data-testid="invite-create"><Icon icon=Ic::Send class="sm"/>"Einladung erstellen"</button></div>
            </form>
            {move || created.get().map(|c| view! {
                <div class="stack" data-testid="invite-created">
                    <SecretBox label="Einladungslink (wird nur jetzt angezeigt)" value=c.link.clone() testid="invite-link"/>
                    <div><button class="btn" type="button" on:click=move |_| created.set(None)>"Fertig"</button></div>
                </div>
            })}
            {move || match list.get() {
                None => view! { <p class="muted" role="status">"Lade …"</p> }.into_any(),
                Some(l) if l.is_empty() => view! { <p class="muted">"Noch keine Einladungen."</p> }.into_any(),
                Some(l) => view! {
                    <ul class="plain stack" aria-label="Einladungen" data-testid="invites">
                        {l.into_iter().map(|i| {
                            let id = i.id;
                            let tone = match i.status { InviteStatus::Open => "tone-info", InviteStatus::Used => "tone-success", _ => "tone-neutral" };
                            view! {
                                <li class="row wrap between list-row">
                                    <span class="grow">
                                        <strong>{i.email.clone().unwrap_or_else(|| "Beliebige Adresse".into())}</strong>" "
                                        <span class=format!("tag {tone}")>{i.status.label()}</span>
                                        {i.make_admin.then(|| view! { " " <span class="tag tone-info">"Admin"</span> })}
                                        <br/>
                                        <span class="small muted">{format!("erstellt {} · läuft ab {}{}",
                                            when(i.created_at), when(i.expires_at),
                                            i.used_by.as_ref().map(|u| format!(" · angenommen von {u}")).unwrap_or_default())}</span>
                                    </span>
                                    {(i.status == InviteStatus::Open).then(|| view! {
                                        <button class="btn sm" type="button" on:click=move |_| {
                                            store.run("Widerrufen fehlgeschlagen", async move {
                                                api::action(&format!("/admin/invitations/{id}/revoke")).await?;
                                                store.toast("Einladung widerrufen");
                                                reload();
                                                Ok(())
                                            });
                                        }>"Widerrufen"</button>
                                    })}
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any(),
            }}
        </section>
    }
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

#[component]
fn Rules() -> impl IntoView {
    let store = use_store();
    let s = RwSignal::new(None::<InstanceSettings>);
    spawn_local(async move {
        match api::get::<InstanceSettings>("/admin/settings").await {
            Ok(v) => s.set(Some(v)),
            Err(e) => store.toast_error("Regeln konnten nicht geladen werden", &e),
        }
    });
    let save = move |u: UpdateInstanceSettings| {
        store.run("Speichern fehlgeschlagen", async move {
            let v: InstanceSettings = api::put("/admin/settings", &u).await?;
            s.set(Some(v));
            store.toast("Gespeichert");
            Ok(())
        });
    };
    view! {
        <section class="card pad stack" aria-labelledby="rules-title">
            <h2 id="rules-title" class="h3">"Regeln"</h2>
            {move || s.get().map(|v| view! {
                <fieldset class="stack">
                    <legend class="h4">"Wer darf sich registrieren?"</legend>
                    {RegistrationMode::ALL.into_iter().map(|m| view! {
                        <label class="radio-card">
                            <input type="radio" name="reg-mode" checked=v.registration_mode == m
                                on:change=move |_| save(UpdateInstanceSettings { registration_mode: Some(m), ..Default::default() })/>
                            <span><strong>{m.label()}</strong><br/><span class="small muted">{m.help()}</span></span>
                        </label>
                    }).collect_view()}
                    {v.registration_from_env.then(|| view! { <p class="small muted">"Noch nicht gewählt: gilt derzeit aus der Server-Konfiguration (ALLOW_REGISTRATION)."</p> })}
                </fieldset>
                <fieldset class="stack">
                    <legend class="h4">"Zwei-Faktor-Anmeldung"</legend>
                    {MfaPolicy::ALL.into_iter().map(|p| view! {
                        <label class="radio-card">
                            <input type="radio" name="mfa-policy" checked=v.mfa_policy == p
                                on:change=move |_| save(UpdateInstanceSettings { mfa_policy: Some(p), ..Default::default() })/>
                            <span><strong>{p.label()}</strong><br/><span class="small muted">{match p {
                                MfaPolicy::Off => "Jede Person entscheidet selbst.",
                                MfaPolicy::Admins => "Admins müssen einen zweiten Faktor einrichten, bevor sie weiterarbeiten können.",
                                MfaPolicy::All => "Alle müssen einen zweiten Faktor einrichten. Auch API-Tokens funktionieren erst danach.",
                            }}</span></span>
                        </label>
                    }).collect_view()}
                </fieldset>
            })}
        </section>
    }
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

#[component]
fn Audit() -> impl IntoView {
    let store = use_store();
    let group = RwSignal::new(String::new());
    let actor = RwSignal::new(String::new());
    let entries = RwSignal::new(Vec::new());
    let next = RwSignal::new(None::<i64>);
    let loaded = RwSignal::new(false);
    let verify = RwSignal::new(None::<AuditVerify>);
    let query = move |before: Option<i64>| {
        let mut q = format!(
            "/admin/audit?action={}&actor={}",
            api::enc(&group.get_untracked()),
            api::enc(&actor.get_untracked())
        );
        if let Some(b) = before {
            q.push_str(&format!("&before={b}"));
        }
        q
    };
    let load = move |more: bool| {
        let before = if more { next.get_untracked() } else { None };
        spawn_local(async move {
            match api::get::<AuditPage>(&query(before)).await {
                Ok(p) => {
                    if more {
                        entries.update(|e| e.extend(p.entries));
                    } else {
                        entries.set(p.entries);
                    }
                    next.set(p.next_before);
                    loaded.set(true);
                }
                Err(e) => store.toast_error("Protokoll konnte nicht geladen werden", &e),
            }
        })
    };
    load(false);
    let (g_ids, a_ids) = (FieldIds::new("audit-group"), FieldIds::new("audit-actor"));
    let export_href = move || {
        format!(
            "/api{}",
            query(None).replacen("/admin/audit?", "/admin/audit/export?", 1)
        )
    };
    view! {
        <section class="card pad stack" aria-labelledby="audit-title">
            <h2 id="audit-title" class="h3">"Protokoll"</h2>
            <p class="muted">{format!("Jede sicherheitsrelevante Aktion wird hier festgehalten. Einträge lassen sich nicht ändern oder löschen; sie bleiben mindestens {} Tage.", queued_api::security::AUDIT_MIN_RETENTION_DAYS)}</p>
            <form class="grid-2" on:submit=move |ev| { ev.prevent_default(); load(false); }>
                <div class="field">
                    <div class="label"><label for=g_ids.id.clone()>"Bereich"</label></div>
                    <select id=g_ids.id.clone() class="input" data-testid="audit-group" on:change=move |ev| { group.set(event_target_value(&ev)); load(false); }>
                        {AUDIT_GROUPS.into_iter().map(|(v, l)| view! { <option value=v>{l}</option> }).collect_view()}
                    </select>
                </div>
                <div class="field">
                    <div class="label"><label for=a_ids.id.clone()>"Person (E-Mail enthält)"</label></div>
                    <div class="row">
                        <input id=a_ids.id.clone() class="input grow" type="search" bind:value=actor data-testid="audit-actor"/>
                        <button class="btn" type="submit">"Filtern"</button>
                    </div>
                </div>
            </form>
            <div class="row wrap">
                <button class="btn" type="button" data-testid="audit-verify" on:click=move |_| {
                    spawn_local(async move {
                        match api::get::<AuditVerify>("/admin/audit/verify").await {
                            Ok(v) => verify.set(Some(v)),
                            Err(e) => store.toast_error("Prüfung fehlgeschlagen", &e),
                        }
                    })
                }><Icon icon=Ic::Shield class="sm"/>"Unverändertheit prüfen"</button>
                <a class="btn" href=export_href download="audit-log.jsonl"><Icon icon=Ic::Download class="sm"/>"Exportieren"</a>
            </div>
            <div role="status" aria-live="polite">
                {move || verify.get().map(|v| if v.intact {
                    view! { <div class="alert tone-success" data-testid="audit-intact"><Icon icon=Ic::CheckCircle class="sm"/>{format!("Alles in Ordnung: {} Einträge geprüft, keiner wurde verändert.", v.checked)}</div> }.into_any()
                } else {
                    view! { <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>{format!("Achtung: Eintrag Nr. {} wurde nachträglich verändert oder gelöscht.", v.first_broken.unwrap_or_default())}</div> }.into_any()
                })}
            </div>
            {move || {
                let list = entries.get();
                if loaded.get() && list.is_empty() {
                    return view! { <p class="muted">"Keine Einträge für diesen Filter."</p> }.into_any();
                }
                view! {
                    <ol class="plain stack audit-list" aria-label="Protokolleinträge" data-testid="audit-entries">
                        {list.into_iter().map(|e| {
                            let details = audit_details(&e.details);
                            view! {
                                <li class="list-row" data-action=e.action.clone()>
                                    <div class="row wrap between">
                                        <strong>{audit_label(&e.action)}</strong>
                                        <span class="small muted">{format!("Nr. {} · {}", e.seq, when(e.at))}</span>
                                    </div>
                                    <span class="small">{format!("{} · {}", e.actor_label, e.action)}</span>
                                    {(!details.is_empty()).then(|| view! { <br/><span class="small muted break">{details}</span> })}
                                </li>
                            }
                        }).collect_view()}
                    </ol>
                }.into_any()
            }}
            {move || next.get().map(|_| view! {
                <div><button class="btn" type="button" on:click=move |_| load(true)>"Ältere Einträge laden"</button></div>
            })}
        </section>
    }
}

// ---------------------------------------------------------------------------
// Runner fleet
// ---------------------------------------------------------------------------

#[component]
fn Runners() -> impl IntoView {
    let store = use_store();
    let runners = RwSignal::new(None::<Vec<RunnerView>>);
    let projects = RwSignal::new(Vec::<RunnerProject>::new());
    let created = RwSignal::new(None::<CreatedRunner>);
    let add_open = RwSignal::new(false);
    let reload = move || {
        spawn_local(async move {
            match api::get::<Vec<RunnerView>>("/admin/runners").await {
                Ok(r) => runners.set(Some(r)),
                Err(e) => store.toast_error("Runner konnten nicht geladen werden", &e),
            }
            if let Ok(p) = api::get::<Vec<RunnerProject>>("/admin/projects").await {
                projects.set(p);
            }
        })
    };
    reload();
    view! {
        <section class="card pad stack" aria-labelledby="runners-title">
            <div class="row wrap between">
                <h2 id="runners-title" class="h3">"Runner"</h2>
                <button class="btn primary" type="button" data-testid="runner-add" on:click=move |_| add_open.set(true)><Icon icon=Ic::Plus class="sm"/>"Runner anmelden"</button>
            </div>
            <p class="muted">"Runner sind die Rechner, auf denen die Agenten arbeiten. Jedes Projekt liegt auf genau einem Runner; neue Projekte gehen an den Runner mit den wenigsten Projekten."</p>
            {move || created.get().map(|c| view! {
                <div class="stack" data-testid="runner-created">
                    <div class="alert tone-warning"><Icon icon=Ic::Key class="sm"/>"Trage diese Zeilen in die Konfiguration des neuen Runners ein (runner.env) und starte ihn neu. Das Token wird nur jetzt angezeigt."</div>
                    <SecretBox label="Runner-Token" value=c.token.clone() testid="runner-token"/>
                    <pre class="code small">{runner_env(&c.token)}</pre>
                    <div><button class="btn" type="button" on:click=move |_| created.set(None)>"Fertig"</button></div>
                </div>
            })}
            {move || match runners.get() {
                None => view! { <p class="muted" role="status">"Lade …"</p> }.into_any(),
                Some(list) => view! {
                    <ul class="plain stack" aria-label="Runner" data-testid="runners">
                        {list.into_iter().map(|r| view! { <RunnerRow r=r projects=projects on_change=Callback::new(move |_| reload()) on_token=Callback::new(move |c| created.set(Some(c)))/> }).collect_view()}
                    </ul>
                }.into_any(),
            }}
            {move || add_open.get().then(|| view! {
                <AddRunner on_close=Callback::new(move |_| add_open.set(false)) on_created=Callback::new(move |c: CreatedRunner| {
                    add_open.set(false);
                    created.set(Some(c));
                    reload();
                })/>
            })}
        </section>
    }
}

#[component]
fn RunnerRow(
    r: RunnerView,
    projects: RwSignal<Vec<RunnerProject>>,
    on_change: Callback<()>,
    on_token: Callback<CreatedRunner>,
) -> impl IntoView {
    let store = use_store();
    let id = r.id;
    let move_open = RwSignal::new(false);
    let now = store.now.get_untracked();
    let seen = r
        .last_seen_at
        .map(|t| ago((now - t).num_seconds().max(0)))
        .unwrap_or_else(|| "noch nie".into());
    let set_status = move |s: RunnerStatus| {
        store.run("Ändern fehlgeschlagen", async move {
            api::patch_ok(
                &format!("/admin/runners/{id}"),
                &UpdateRunner {
                    status: Some(s),
                    ..Default::default()
                },
            )
            .await?;
            store.toast(if s == RunnerStatus::Draining {
                "Runner nimmt keine neuen Aufträge mehr an"
            } else {
                "Runner ist wieder bereit"
            });
            on_change.run(());
            Ok(())
        });
    };
    let name = r.name.clone();
    let revoke = move |_| {
        let name = name.clone();
        store.ask(Confirm {
            title: format!("Runner „{name}“ widerrufen?"),
            body: "Sein Token wird sofort ungültig. Er bekommt nie wieder Arbeit oder Zugangsdaten. Laufende Aufträge werden abgebrochen und neu eingereiht. Seine Projekte warten, bis sie aus einem Backup auf einen anderen Runner gebracht werden.".into(),
            confirm_label: "Widerrufen".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                store.run("Widerrufen fehlgeschlagen", async move {
                    api::action(&format!("/admin/runners/{id}/revoke")).await?;
                    store.toast("Runner widerrufen");
                    on_change.run(());
                    Ok(())
                });
            }),
        });
    };
    let rotate = move |_| {
        store.run("Neues Token fehlgeschlagen", async move {
            let c: CreatedRunner =
                api::post(&format!("/admin/runners/{id}/token"), &json!({})).await?;
            on_token.run(c);
            on_change.run(());
            Ok(())
        });
    };
    let remove = move |_| {
        store.run("Löschen fehlgeschlagen", async move {
            api::delete(&format!("/admin/runners/{id}")).await?;
            store.toast("Runner gelöscht");
            on_change.run(());
            Ok(())
        });
    };
    let cap = r
        .capacity
        .map(|c| format!("{} von {c} Plätzen belegt", r.running_tasks))
        .unwrap_or_else(|| format!("{} Aufträge laufen (ohne Obergrenze)", r.running_tasks));
    let r2 = r.clone();
    view! {
        <li class="list-row stack-sm" data-testid="runner" data-name=r.name.clone()>
            <div class="row wrap between">
                <div class="grow">
                    <strong>{r.name.clone()}</strong>" "
                    <span class=format!("tag tone-{}", r.state_tone()) data-testid="runner-state">{r.state_label()}</span>
                    {r.builtin.then(|| view! { " " <span class="tag tone-neutral">"aus der Konfiguration"</span> })}
                    <br/>
                    <span class="small muted break">{format!("{} · {} · {} Projekte · letzter Kontakt {}",
                        r.url.clone().unwrap_or_default(), cap, r.projects, seen)}</span>
                    <br/>
                    <span class="small muted">{format!("Sandbox: {}{}{}",
                        if r.engine.is_empty() { "unbekannt".to_string() } else { r.engine.clone() },
                        if r.egress_enforced { " (Netzregeln durchgesetzt)" } else { " (Netzregeln nicht durchgesetzt)" },
                        if r.version.is_empty() { String::new() } else { format!(" · Version {}", r.version) })}</span>
                </div>
                {(r.status != RunnerStatus::Revoked).then(|| view! {
                    <div class="row wrap">
                        {if r.status == RunnerStatus::Active {
                            view! { <button class="btn sm" type="button" data-testid="runner-drain" on:click=move |_| set_status(RunnerStatus::Draining)>"Leeren (keine neuen Aufträge)"</button> }.into_any()
                        } else {
                            view! { <button class="btn sm" type="button" on:click=move |_| set_status(RunnerStatus::Active)>"Wieder freigeben"</button> }.into_any()
                        }}
                        <button class="btn sm" type="button" on:click=move |_| move_open.set(true)>"Projekte umziehen"</button>
                        {(!r.builtin).then(|| view! {
                            <button class="btn sm" type="button" on:click=rotate>"Neues Token"</button>
                            <button class="btn sm danger" type="button" on:click=revoke>"Widerrufen"</button>
                        })}
                    </div>
                })}
                {(r.status == RunnerStatus::Revoked && r.projects == 0).then(|| view! {
                    <button class="btn sm" type="button" on:click=remove>"Löschen"</button>
                })}
            </div>
            {move || move_open.get().then(|| view! { <MoveDialog from=r2.clone() projects=projects on_close=Callback::new(move |_| move_open.set(false)) on_moved=on_change/> })}
        </li>
    }
}

#[component]
fn AddRunner(on_close: Callback<()>, on_created: Callback<CreatedRunner>) -> impl IntoView {
    let store = use_store();
    let t = uid("runner-title");
    let name = RwSignal::new(String::new());
    let url = RwSignal::new(String::new());
    let cap = RwSignal::new("2".to_string());
    let error = RwSignal::new(String::new());
    let (n_ids, u_ids, c_ids) = (
        FieldIds::new("r-name"),
        FieldIds::new("r-url"),
        FieldIds::new("r-cap"),
    );
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let capacity = match parse_limit::<i32>(&cap.get_untracked()) {
            Ok(c) => c,
            Err(_) => {
                error.set("Kapazität: eine Zahl (leer = unbegrenzt).".into());
                return;
            }
        };
        let r = NewRunner {
            name: name.get_untracked(),
            url: url.get_untracked(),
            capacity,
        };
        if let Err(e) = r.validate() {
            error.set(e);
            return;
        }
        store.run("Runner konnte nicht angemeldet werden", async move {
            let c: CreatedRunner = api::post("/admin/runners", &r).await?;
            on_created.run(c);
            Ok(())
        });
    };
    view! {
        <Modal on_close=on_close labelledby=t.clone()>
            <h2 id=t>"Runner anmelden"</h2>
            <p class="muted">"Der Runner läuft auf einem eigenen Rechner (ohne Root-Rechte) und muss von hier aus erreichbar sein. Du bekommst gleich ein Token, das nur dieser Runner kennt."</p>
            <form class="stack" on:submit=submit>
                <div class="field">
                    <div class="label"><label for=n_ids.id.clone()>"Name"</label></div>
                    <input id=n_ids.id.clone() class="input" maxlength="60" required placeholder="z. B. Werkstatt-PC" bind:value=name data-testid="runner-name"/>
                </div>
                <div class="field">
                    <div class="label"><label for=u_ids.id.clone()>"Adresse"</label></div>
                    <input id=u_ids.id.clone() class="input" type="url" required placeholder="http://10.0.0.5:8090" bind:value=url data-testid="runner-url"/>
                </div>
                <div class="field">
                    <div class="label"><label for=c_ids.id.clone()>"Kapazität (gleichzeitige Aufträge)"</label></div>
                    <input id=c_ids.id.clone() class="input" inputmode="numeric" bind:value=cap/>
                </div>
                <div role="alert">{move || (!error.get().is_empty()).then(|| view! { <div class="alert tone-danger">{error.get()}</div> })}</div>
                <div class="dialog-actions">
                    <button class="btn" type="button" on:click=move |_| on_close.run(())>"Abbrechen"</button>
                    <button class="btn primary" type="submit" data-testid="runner-create">"Anmelden"</button>
                </div>
            </form>
        </Modal>
    }
}

#[component]
fn MoveDialog(
    from: RunnerView,
    projects: RwSignal<Vec<RunnerProject>>,
    on_close: Callback<()>,
    on_moved: Callback<()>,
) -> impl IntoView {
    let store = use_store();
    let t = uid("move-title");
    let targets = RwSignal::new(Vec::<RunnerView>::new());
    let target = RwSignal::new(String::new());
    spawn_local(async move {
        if let Ok(l) = api::get::<Vec<RunnerView>>("/admin/runners").await {
            let ok: Vec<RunnerView> = l
                .into_iter()
                .filter(|r| r.id != from.id && r.status == RunnerStatus::Active && r.online)
                .collect();
            if let Some(f) = ok.first() {
                target.set(f.id.to_string());
            }
            targets.set(ok);
        }
    });
    let mine = move || {
        projects
            .get()
            .into_iter()
            .filter(|p| p.runner_id == from.id)
            .collect::<Vec<_>>()
    };
    let ids = FieldIds::new("move-target");
    view! {
        <Modal on_close=on_close labelledby=t.clone()>
            <h2 id=t>{format!("Projekte von „{}“ umziehen", from.name)}</h2>
            <p class="muted">"Das Repository samt Branches und offenen Arbeitsständen wird kopiert, danach hier gelöscht. Projekte, in denen gerade etwas läuft, warten."</p>
            {move || (targets.get().is_empty()).then(|| view! { <p class="alert tone-warning">"Kein anderer bereiter Runner vorhanden."</p> })}
            <div class="field">
                <div class="label"><label for=ids.id.clone()>"Ziel-Runner"</label></div>
                <select id=ids.id.clone() class="input" on:change=move |ev| target.set(event_target_value(&ev))>
                    {move || targets.get().into_iter().map(|r| view! { <option value=r.id.to_string()>{r.name.clone()}</option> }).collect_view()}
                </select>
            </div>
            <ul class="plain stack">
                {move || mine().into_iter().map(|p| {
                    let pid = p.id;
                    view! {
                        <li class="row wrap between list-row">
                            <span class="grow"><strong>{p.name.clone()}</strong><br/><span class="small muted">{p.owner.clone()}</span></span>
                            <button class="btn sm" type="button" disabled=p.busy || targets.get().is_empty() on:click=move |_| {
                                let Ok(rid) = Uuid::parse_str(&target.get_untracked()) else { return };
                                store.run("Umzug fehlgeschlagen", async move {
                                    let r: MoveResult = api::post(&format!("/admin/projects/{pid}/move"), &MoveProject { runner_id: rid }).await?;
                                    store.toast(format!("Umgezogen ({} Arbeitsstände)", r.workspaces));
                                    on_moved.run(());
                                    on_close.run(());
                                    Ok(())
                                });
                            }>{if p.busy { "läuft gerade" } else { "Umziehen" }}</button>
                        </li>
                    }
                }).collect_view()}
            </ul>
            {move || mine().is_empty().then(|| view! { <p class="muted">"Auf diesem Runner liegen keine Projekte."</p> })}
            <div class="dialog-actions">
                <button class="btn" type="button" on:click=move |_| on_close.run(())>"Schließen"</button>
            </div>
        </Modal>
    }
}
