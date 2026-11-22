//! WP11: account security – second factor at sign-in, the settings card
//! "Sicherheit" (password, authenticator app with QR code, passkeys,
//! recovery codes, signed-in devices) and the setup page that the MFA
//! policy enforces.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::security::{
    describe_auth_method, describe_user_agent, format_recovery_code, normalize_totp,
    validate_password, ChangePassword, FactorAdded, LoginOutcome, MfaCode, MfaMethod,
    RecoveryCodes, SecurityOverview, SessionInfo, StartPasskey, StepUp, TotpConfirm, TotpSetup,
    WebauthnChallenge, WebauthnResponse,
};
use serde_json::json;

use crate::api;
use crate::logic::format::fmt_time;

fn when(ts: chrono::DateTime<chrono::Utc>) -> String {
    fmt_time(ts, chrono::Utc::now(), &tz())
}
use crate::logic::security::{ago, group_secret, qr_path};
use crate::store::{tz, use_store};
use crate::ui::dialog::{Confirm, Modal};
use crate::ui::{uid, FieldIds, Ic, Icon};
use crate::webauthn;

use super::settings::Card;

// ---------------------------------------------------------------------------
// Second login step
// ---------------------------------------------------------------------------

/// "Bestätige, dass du es bist": code from the app, passkey or recovery code.
#[component]
pub fn MfaStep(
    outcome: LoginOutcome,
    on_done: Callback<()>,
    on_cancel: Callback<()>,
) -> impl IntoView {
    let has = |m: MfaMethod| outcome.methods.contains(&m);
    let (totp, passkey, recovery) = (
        has(MfaMethod::Totp),
        has(MfaMethod::Webauthn),
        has(MfaMethod::Recovery),
    );
    let use_recovery = RwSignal::new(!totp && !passkey);
    let code = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let ids = FieldIds::new("mfa-code");

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let path = if use_recovery.get_untracked() {
                "/auth/mfa/recovery"
            } else {
                "/auth/mfa/totp"
            };
            let r = api::post_ok(
                path,
                &MfaCode {
                    code: code.get_untracked(),
                },
            )
            .await;
            busy.set(false);
            match r {
                Ok(()) => on_done.run(()),
                Err(e) => {
                    code.set(String::new());
                    error.set(e.message);
                }
            }
        });
    };
    let with_passkey = move |_| {
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let r: Result<(), String> = async {
                let ch: WebauthnChallenge = api::post("/auth/mfa/webauthn/start", &json!({}))
                    .await
                    .map_err(|e| e.message)?;
                let cred = webauthn::get_assertion(&ch.options).await?;
                api::post_ok(
                    "/auth/mfa/webauthn/finish",
                    &WebauthnResponse {
                        credential: cred,
                        name: String::new(),
                    },
                )
                .await
                .map_err(|e| e.message)
            }
            .await;
            busy.set(false);
            match r {
                Ok(()) => on_done.run(()),
                Err(e) => error.set(e),
            }
        });
    };

    view! {
        <form class="card login" on:submit=submit data-testid="mfa-step">
            <h1 class="brand">
                <span class="logo"><Icon icon=Ic::Shield/></span>
                "Bestätige, dass du es bist"
            </h1>
            <p class="lead">
                {move || if use_recovery.get() {
                    "Gib einen deiner Wiederherstellungscodes ein. Jeder Code funktioniert nur einmal."
                } else if totp {
                    "Öffne deine Authenticator-App und gib den 6-stelligen Code ein."
                } else {
                    "Melde dich mit deinem Passkey an (z. B. Fingerabdruck oder Gesichtserkennung)."
                }}
            </p>
            {passkey.then(|| view! {
                <button type="button" class="btn primary lg block" on:click=with_passkey disabled=move || busy.get()>
                    <Icon icon=Ic::Key/>"Mit Passkey bestätigen"
                </button>
            })}
            {move || (totp || use_recovery.get()).then(|| {
                let ids = ids.clone();
                view! {
                    <div class="field">
                        <div class="label">
                            <label for=ids.id.clone()>
                                {move || if use_recovery.get() { "Wiederherstellungscode" } else { "Code aus der App" }}
                            </label>
                        </div>
                        <input
                            id=ids.id.clone()
                            class="input mono-input"
                            autocomplete="one-time-code"
                            inputmode=move || if use_recovery.get() { "text" } else { "numeric" }
                            autocapitalize="characters"
                            spellcheck="false"
                            required
                            data-testid="mfa-code"
                            bind:value=code
                        />
                    </div>
                    <button class="btn primary lg block" type="submit" disabled=move || busy.get()>
                        {move || if busy.get() { "Einen Moment …" } else { "Bestätigen" }}
                    </button>
                }
            })}
            <div role="alert">
                {move || (!error.get().is_empty()).then(|| view! {
                    <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>{error.get()}</div>
                })}
            </div>
            {recovery.then(|| view! {
                <button type="button" class="btn ghost block" on:click=move |_| {
                    use_recovery.update(|r| *r = !*r);
                    code.set(String::new());
                    error.set(String::new());
                }>
                    {move || if use_recovery.get() { "Doch mit App oder Passkey bestätigen" } else { "Kein Zugriff? Wiederherstellungscode verwenden" }}
                </button>
            })}
            <button type="button" class="btn ghost block" on:click=move |_| on_cancel.run(())>"Abbrechen"</button>
        </form>
    }
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

/// Re-authentication for sensitive changes: the password, or (accounts
/// without one) a code from the authenticator app.
#[component]
fn StepUpField(password_set: bool, value: RwSignal<String>) -> impl IntoView {
    let ids = FieldIds::new("step-up");
    view! {
        <div class="field">
            <div class="label">
                <label for=ids.id.clone()>
                    {if password_set { "Dein aktuelles Passwort" } else { "Code aus deiner Authenticator-App" }}
                </label>
            </div>
            <input
                id=ids.id.clone()
                class="input"
                type=if password_set { "password" } else { "text" }
                autocomplete=if password_set { "current-password" } else { "one-time-code" }
                aria-describedby=ids.hint.clone()
                data-testid="step-up"
                bind:value=value
            />
            <span class="hint" id=ids.hint.clone()>"Zur Sicherheit fragen wir bei Änderungen nach."</span>
        </div>
    }
}

fn step_up(password_set: bool, v: String) -> StepUp {
    if password_set {
        StepUp {
            password: Some(v),
            code: None,
        }
    } else {
        StepUp {
            password: None,
            code: Some(v),
        }
    }
}

/// Recovery codes shown exactly once, with copy and download.
#[component]
fn RecoveryList(codes: Vec<String>) -> impl IntoView {
    let store = use_store();
    let text = codes
        .iter()
        .map(|c| format_recovery_code(c))
        .collect::<Vec<_>>()
        .join("\n");
    let copy_text = text.clone();
    let href = format!(
        "data:text/plain;charset=utf-8,{}",
        js_sys::encode_uri_component(&format!(
            "Queue – Wiederherstellungscodes\nJeder Code funktioniert genau einmal.\n\n{text}\n"
        ))
    );
    view! {
        <div class="stack" data-testid="recovery-codes">
            <div class="alert tone-warning">
                <Icon icon=Ic::Alert class="sm"/>
                "Speichere diese Codes jetzt an einem sicheren Ort (z. B. Passwort-Manager). Sie werden nur dieses eine Mal angezeigt."
            </div>
            <ul class="plain recovery-grid" aria-label="Wiederherstellungscodes">
                {codes.iter().map(|c| view! { <li class="mono">{format_recovery_code(c)}</li> }).collect_view()}
            </ul>
            <div class="row wrap">
                <button type="button" class="btn" on:click=move |_| {
                    let t = copy_text.clone();
                    spawn_local(async move {
                        if crate::ui::copy_text(&t).await {
                            store.toast("Codes kopiert");
                        } else {
                            store.announce("Kopieren nicht möglich – bitte die Codes markieren und selbst kopieren");
                        }
                    });
                }><Icon icon=Ic::Copy class="sm"/>"Kopieren"</button>
                <a class="btn" href=href download="queue-wiederherstellungscodes.txt">
                    <Icon icon=Ic::Download class="sm"/>"Als Datei speichern"
                </a>
            </div>
        </div>
    }
}

/// QR code of an `otpauth://` URI (SVG, scales with the card).
#[component]
pub fn QrCode(#[prop(into)] text: String) -> impl IntoView {
    match qr_path(&text) {
        Some((size, d)) => view! {
            <svg class="qr" viewBox=format!("0 0 {size} {size}") role="img"
                aria-label="QR-Code für die Authenticator-App" shape-rendering="crispEdges" data-testid="totp-qr">
                <rect width=size height=size fill="#fff"/>
                <path d=d fill="#000"/>
            </svg>
        }
        .into_any(),
        None => view! { <p class="muted">"QR-Code nicht verfügbar – bitte den Schlüssel von Hand eingeben."</p> }.into_any(),
    }
}

/// Setup of the authenticator app: QR code + key, then the first code.
/// `on_done` gets the result (recovery codes if new).
#[component]
pub fn TotpSetupFlow(
    password_set: bool,
    /// Needs re-authentication (a password or factor exists).
    needs_step_up: bool,
    on_done: Callback<FactorAdded>,
    on_cancel: Callback<()>,
) -> impl IntoView {
    let setup = RwSignal::new(None::<TotpSetup>);
    let secret_in = RwSignal::new(String::new());
    let code = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let code_ids = FieldIds::new("totp-code");

    let start = move || {
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let body = if needs_step_up {
                step_up(password_set, secret_in.get_untracked())
            } else {
                StepUp::default()
            };
            match api::post::<_, TotpSetup>("/account/mfa/totp/setup", &body).await {
                Ok(s) => setup.set(Some(s)),
                Err(e) => error.set(e.message),
            }
            busy.set(false);
        });
    };
    if !needs_step_up {
        start();
    }
    let confirm = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(c) = normalize_totp(&code.get_untracked()) else {
            error.set("Bitte die 6 Ziffern aus der App eingeben.".into());
            return;
        };
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            match api::post::<_, FactorAdded>("/account/mfa/totp/confirm", &TotpConfirm { code: c })
                .await
            {
                Ok(r) => on_done.run(r),
                Err(e) => {
                    code.set(String::new());
                    error.set(e.message);
                }
            }
            busy.set(false);
        });
    };

    view! {
        <div class="stack" data-testid="totp-setup">
            {move || match setup.get() {
                None => (needs_step_up).then(|| view! {
                    <form class="stack" on:submit=move |ev| { ev.prevent_default(); start(); }>
                        <StepUpField password_set=password_set value=secret_in/>
                        <div class="row wrap">
                            <button class="btn primary" type="submit" disabled=move || busy.get()>"Weiter"</button>
                            <button class="btn ghost" type="button" on:click=move |_| on_cancel.run(())>"Abbrechen"</button>
                        </div>
                    </form>
                }).into_any(),
                Some(s) => {
                    let ids = code_ids.clone();
                    view! {
                        <ol class="setup-steps">
                            <li>"Installiere eine Authenticator-App, falls du noch keine hast (z. B. die Passwort-App deines iPhones, Google Authenticator, Aegis oder 1Password)."</li>
                            <li>"Scanne diesen QR-Code mit der App:"</li>
                        </ol>
                        <div class="qr-wrap"><QrCode text=s.uri.clone()/></div>
                        <details class="manual-key">
                            <summary>"QR-Code geht nicht? Schlüssel von Hand eingeben"</summary>
                            <p class="mono selectable" data-testid="totp-secret">{group_secret(&s.secret)}</p>
                            <p class="small muted">"Typ: zeitbasiert (TOTP), 6 Ziffern, alle 30 Sekunden."</p>
                        </details>
                        <form class="stack" on:submit=confirm>
                            <div class="field">
                                <div class="label"><label for=ids.id.clone()>"3. Gib den Code ein, den die App jetzt anzeigt"</label></div>
                                <input id=ids.id.clone() class="input mono-input" inputmode="numeric" autocomplete="one-time-code"
                                    maxlength="7" required data-testid="totp-confirm-code" bind:value=code/>
                            </div>
                            <div class="row wrap">
                                <button class="btn primary" type="submit" disabled=move || busy.get()>"Einrichten"</button>
                                <button class="btn ghost" type="button" on:click=move |_| on_cancel.run(())>"Abbrechen"</button>
                            </div>
                        </form>
                    }.into_any()
                }
            }}
            <div role="alert">
                {move || (!error.get().is_empty()).then(|| view! {
                    <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>{error.get()}</div>
                })}
            </div>
        </div>
    }
}

/// Adds a passkey (re-authentication, name, browser ceremony).
async fn add_passkey(step: StepUp, name: String) -> Result<FactorAdded, String> {
    let ch: WebauthnChallenge = api::post(
        "/account/webauthn/register/start",
        &StartPasskey { step_up: step },
    )
    .await
    .map_err(|e| e.message)?;
    let cred = webauthn::create(&ch.options).await?;
    api::post(
        "/account/webauthn/register/finish",
        &WebauthnResponse {
            credential: cred,
            name,
        },
    )
    .await
    .map_err(|e| e.message)
}

// ---------------------------------------------------------------------------
// Settings card
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Dialog {
    None,
    Totp,
    Passkey,
    Password,
    Recovery,
    RemoveTotp,
}

#[component]
pub fn SecurityCard() -> impl IntoView {
    let store = use_store();
    let overview = RwSignal::new(None::<SecurityOverview>);
    let dialog = RwSignal::new(Dialog::None);
    let new_codes = RwSignal::new(None::<Vec<String>>);
    let reload = move || {
        spawn_local(async move {
            match api::get::<SecurityOverview>("/account/security").await {
                Ok(o) => overview.set(Some(o)),
                Err(e) => store.toast_error("Sicherheit konnte nicht geladen werden", &e),
            }
        })
    };
    reload();
    let factor_added = Callback::new(move |r: FactorAdded| {
        dialog.set(Dialog::None);
        if let Some(c) = r.recovery_codes {
            new_codes.set(Some(c));
        }
        store.toast(if r.revoked_sessions > 0 {
            format!(
                "Eingerichtet. {} andere Geräte wurden abgemeldet.",
                r.revoked_sessions
            )
        } else {
            "Eingerichtet".to_string()
        });
        reload();
    });
    let close = Callback::new(move |_| dialog.set(Dialog::None));

    view! {
        <Card id="security" icon=Ic::Shield title="Sicherheit"
            text="Passwort, Zwei-Faktor-Anmeldung und die Geräte, auf denen du angemeldet bist.">
            {move || match overview.get() {
                None => view! { <p class="muted" role="status">"Lade …"</p> }.into_any(),
                Some(o) => {
                    let o2 = o.clone();
                    let enabled = o.mfa_enabled();
                    view! {
                        <div class="stack" data-testid="security">
                            <div class=if enabled { "alert tone-success" } else if o.mfa_required { "alert tone-danger" } else { "alert tone-warning" }>
                                <Icon icon=if enabled { Ic::CheckCircle } else { Ic::Alert } class="sm"/>
                                {if enabled {
                                    "Zwei-Faktor-Anmeldung ist aktiv. Wer nur dein Passwort kennt, kommt nicht hinein."
                                } else if o.mfa_required {
                                    "Zwei-Faktor-Anmeldung ist auf dieser Instanz Pflicht. Bitte richte sie jetzt ein."
                                } else {
                                    "Zwei-Faktor-Anmeldung ist aus. Wir empfehlen sie: Ein gestohlenes Passwort allein reicht dann nicht."
                                }}
                            </div>
                            <h3 class="h4">"Authenticator-App"</h3>
                            <div class="row wrap between">
                                <p class="grow">{if o.totp_enabled { "Eingerichtet." } else { "Erzeugt alle 30 Sekunden einen neuen 6-stelligen Code." }}</p>
                                {if o.totp_enabled {
                                    view! { <button class="btn" type="button" on:click=move |_| dialog.set(Dialog::RemoveTotp)>"Entfernen"</button> }.into_any()
                                } else {
                                    view! { <button class="btn primary" type="button" data-testid="totp-start" on:click=move |_| dialog.set(Dialog::Totp)>
                                        <Icon icon=Ic::Shield class="sm"/>"App einrichten"
                                    </button> }.into_any()
                                }}
                            </div>
                            <h3 class="h4">"Passkeys"</h3>
                            <p class="small muted">"Anmelden mit Fingerabdruck, Gesichtserkennung oder Sicherheitsschlüssel – ohne Code abzutippen."</p>
                            <ul class="plain stack" aria-label="Deine Passkeys" data-testid="passkeys">
                                {o.passkeys.iter().map(|p| {
                                    let (id, name) = (p.id, p.name.clone());
                                    let pw = o.password_set;
                                    view! {
                                        <li class="row wrap between list-row">
                                            <span class="grow"><Icon icon=Ic::Key class="sm"/>" "<strong>{p.name.clone()}</strong>
                                                <span class="small muted">{format!(" · seit {}", when(p.created_at))}</span></span>
                                            <PasskeyRemove id=id name=name password_set=pw on_done=Callback::new(move |_| reload())/>
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                            <div>
                                <button class="btn" type="button" data-testid="passkey-add" on:click=move |_| dialog.set(Dialog::Passkey)
                                    disabled=!webauthn::supported()>
                                    <Icon icon=Ic::Plus class="sm"/>"Passkey hinzufügen"
                                </button>
                            </div>
                            {enabled.then(|| view! {
                                <h3 class="h4">"Wiederherstellungscodes"</h3>
                                <div class="row wrap between">
                                    <p class="grow" data-testid="recovery-left">{format!("Noch {} von 10 Codes übrig – für den Fall, dass dein Handy weg ist.", o.recovery_left)}</p>
                                    <button class="btn" type="button" on:click=move |_| dialog.set(Dialog::Recovery)>"Neue Codes erzeugen"</button>
                                </div>
                            })}
                            {move || new_codes.get().map(|c| view! {
                                <div class="stack">
                                    <RecoveryList codes=c/>
                                    <div><button class="btn primary" type="button" on:click=move |_| new_codes.set(None)>"Ich habe die Codes gespeichert"</button></div>
                                </div>
                            })}
                            <h3 class="h4">"Passwort"</h3>
                            <div class="row wrap between">
                                <p class="grow">{if o.password_set { "Beim Ändern werden alle anderen Geräte abgemeldet." } else { "Du meldest dich über einen Anbieter an. Du kannst zusätzlich ein Passwort festlegen." }}</p>
                                <button class="btn" type="button" data-testid="password-change" on:click=move |_| dialog.set(Dialog::Password)>
                                    {if o.password_set { "Passwort ändern" } else { "Passwort festlegen" }}
                                </button>
                            </div>
                            <SessionsList/>
                            {move || {
                                let o = o2.clone();
                                match dialog.get() {
                                    Dialog::None => ().into_any(),
                                    Dialog::Totp => {
                                        let t = uid("totp-title");
                                        view! {
                                            <Modal on_close=close labelledby=t.clone()>
                                                <h2 id=t>"Authenticator-App einrichten"</h2>
                                                <TotpSetupFlow password_set=o.password_set needs_step_up=o.password_set || o.mfa_enabled()
                                                    on_done=factor_added on_cancel=close/>
                                            </Modal>
                                        }.into_any()
                                    }
                                    Dialog::Passkey => view! { <PasskeyDialog password_set=o.password_set needs_step_up=o.password_set || o.mfa_enabled() on_done=factor_added on_close=close/> }.into_any(),
                                    Dialog::Password => view! { <PasswordDialog password_set=o.password_set has_mfa=o.mfa_enabled() on_close=close/> }.into_any(),
                                    Dialog::Recovery => view! { <StepUpDialog title="Neue Wiederherstellungscodes" text="Die alten Codes werden sofort ungültig."
                                        password_set=o.password_set confirm_label="Neue Codes erzeugen" danger=false
                                        on_close=close on_confirm=Callback::new(move |s: StepUp| {
                                            store.run("Codes konnten nicht erzeugt werden", async move {
                                                let r: RecoveryCodes = api::post("/account/mfa/recovery", &s).await?;
                                                dialog.set(Dialog::None);
                                                new_codes.set(Some(r.codes));
                                                reload();
                                                Ok(())
                                            });
                                        })/> }.into_any(),
                                    Dialog::RemoveTotp => view! { <StepUpDialog title="Authenticator-App entfernen?" text="Andere Geräte werden abgemeldet. Ohne zweiten Faktor reicht wieder das Passwort allein."
                                        password_set=o.password_set confirm_label="Entfernen" danger=true
                                        on_close=close on_confirm=Callback::new(move |s: StepUp| {
                                            store.run("Entfernen fehlgeschlagen", async move {
                                                api::post_ok("/account/mfa/totp/disable", &s).await?;
                                                dialog.set(Dialog::None);
                                                store.toast("Authenticator-App entfernt");
                                                reload();
                                                Ok(())
                                            });
                                        })/> }.into_any(),
                                }
                            }}
                        </div>
                    }.into_any()
                }
            }}
        </Card>
    }
}

/// Generic confirmation with re-authentication.
#[component]
fn StepUpDialog(
    title: &'static str,
    text: &'static str,
    password_set: bool,
    confirm_label: &'static str,
    danger: bool,
    on_close: Callback<()>,
    on_confirm: Callback<StepUp>,
) -> impl IntoView {
    let t = uid("stepup-title");
    let v = RwSignal::new(String::new());
    view! {
        <Modal on_close=on_close labelledby=t.clone() alert=danger>
            <h2 id=t>{title}</h2>
            <p class="muted">{text}</p>
            <form class="stack" on:submit=move |ev| { ev.prevent_default(); on_confirm.run(step_up(password_set, v.get_untracked())); }>
                <StepUpField password_set=password_set value=v/>
                <div class="dialog-actions">
                    <button class="btn" type="button" on:click=move |_| on_close.run(())>"Abbrechen"</button>
                    <button class=if danger { "btn danger solid" } else { "btn primary" } type="submit">{confirm_label}</button>
                </div>
            </form>
        </Modal>
    }
}

#[component]
fn PasskeyRemove(
    id: uuid::Uuid,
    name: String,
    password_set: bool,
    on_done: Callback<()>,
) -> impl IntoView {
    let store = use_store();
    let open = RwSignal::new(false);
    view! {
        <button class="btn sm" type="button" aria-label=format!("Passkey {name} entfernen") on:click=move |_| open.set(true)>
            <Icon icon=Ic::Trash class="sm"/>"Entfernen"
        </button>
        {move || open.get().then(|| view! {
            <StepUpDialog title="Passkey entfernen?" text="Mit diesem Passkey kannst du dich danach nicht mehr anmelden."
                password_set=password_set confirm_label="Entfernen" danger=true
                on_close=Callback::new(move |_| open.set(false))
                on_confirm=Callback::new(move |s: StepUp| {
                    store.run("Entfernen fehlgeschlagen", async move {
                        api::post_ok(&format!("/account/webauthn/{id}/remove"), &s).await?;
                        open.set(false);
                        store.toast("Passkey entfernt");
                        on_done.run(());
                        Ok(())
                    });
                })/>
        })}
    }
}

#[component]
fn PasskeyDialog(
    password_set: bool,
    needs_step_up: bool,
    on_done: Callback<FactorAdded>,
    on_close: Callback<()>,
) -> impl IntoView {
    let t = uid("passkey-title");
    let v = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let ids = FieldIds::new("passkey-name");
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        busy.set(true);
        error.set(String::new());
        spawn_local(async move {
            let s = if needs_step_up {
                step_up(password_set, v.get_untracked())
            } else {
                StepUp::default()
            };
            match add_passkey(s, name.get_untracked()).await {
                Ok(r) => on_done.run(r),
                Err(e) => error.set(e),
            }
            busy.set(false);
        });
    };
    view! {
        <Modal on_close=on_close labelledby=t.clone()>
            <h2 id=t>"Passkey hinzufügen"</h2>
            <p class="muted">"Dein Gerät fragt gleich nach Fingerabdruck, Gesicht, PIN oder deinem Sicherheitsschlüssel."</p>
            <form class="stack" on:submit=submit>
                <div class="field">
                    <div class="label"><label for=ids.id.clone()>"Name des Passkeys"</label></div>
                    <input id=ids.id.clone() class="input" maxlength="60" placeholder="z. B. iPhone oder YubiKey" bind:value=name data-testid="passkey-name"/>
                </div>
                {needs_step_up.then(|| view! { <StepUpField password_set=password_set value=v/> })}
                <div role="alert">
                    {move || (!error.get().is_empty()).then(|| view! {
                        <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>{error.get()}</div>
                    })}
                </div>
                <div class="dialog-actions">
                    <button class="btn" type="button" on:click=move |_| on_close.run(())>"Abbrechen"</button>
                    <button class="btn primary" type="submit" disabled=move || busy.get() data-testid="passkey-create">"Passkey erstellen"</button>
                </div>
            </form>
        </Modal>
    }
}

#[component]
fn PasswordDialog(password_set: bool, has_mfa: bool, on_close: Callback<()>) -> impl IntoView {
    let store = use_store();
    let t = uid("pw-title");
    let current = RwSignal::new(String::new());
    let new_pw = RwSignal::new(String::new());
    let repeat = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let (n_ids, r_ids) = (FieldIds::new("new-pw"), FieldIds::new("new-pw2"));
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let pw = new_pw.get_untracked();
        if let Err(e) = validate_password(&pw) {
            error.set(e);
            return;
        }
        if pw != repeat.get_untracked() {
            error.set("Die beiden Passwörter stimmen nicht überein.".into());
            return;
        }
        error.set(String::new());
        let s = if password_set || has_mfa {
            step_up(password_set, current.get_untracked())
        } else {
            StepUp::default()
        };
        spawn_local(async move {
            match api::post::<_, serde_json::Value>(
                "/account/password",
                &ChangePassword {
                    step_up: s,
                    new_password: pw,
                },
            )
            .await
            {
                Ok(v) => {
                    on_close.run(());
                    let n = v["revoked_sessions"].as_i64().unwrap_or(0);
                    store.toast(if n > 0 {
                        format!("Passwort geändert. {n} andere Geräte wurden abgemeldet.")
                    } else {
                        "Passwort geändert".into()
                    });
                }
                Err(e) => error.set(e.message),
            }
        });
    };
    view! {
        <Modal on_close=on_close labelledby=t.clone()>
            <h2 id=t>{if password_set { "Passwort ändern" } else { "Passwort festlegen" }}</h2>
            <form class="stack" on:submit=submit>
                {(password_set || has_mfa).then(|| view! { <StepUpField password_set=password_set value=current/> })}
                <div class="field">
                    <div class="label"><label for=n_ids.id.clone()>"Neues Passwort"</label></div>
                    <input id=n_ids.id.clone() class="input" type="password" autocomplete="new-password" minlength="8" required bind:value=new_pw data-testid="new-password"/>
                    <span class="hint">"Mindestens 8 Zeichen. Ein langer Satz ist sicherer als ein kurzes kompliziertes Wort."</span>
                </div>
                <div class="field">
                    <div class="label"><label for=r_ids.id.clone()>"Neues Passwort wiederholen"</label></div>
                    <input id=r_ids.id.clone() class="input" type="password" autocomplete="new-password" required bind:value=repeat data-testid="new-password-repeat"/>
                </div>
                <div role="alert">
                    {move || (!error.get().is_empty()).then(|| view! {
                        <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>{error.get()}</div>
                    })}
                </div>
                <div class="dialog-actions">
                    <button class="btn" type="button" on:click=move |_| on_close.run(())>"Abbrechen"</button>
                    <button class="btn primary" type="submit">"Speichern"</button>
                </div>
            </form>
        </Modal>
    }
}

/// Signed-in devices with sign-out (one / all others).
#[component]
fn SessionsList() -> impl IntoView {
    let store = use_store();
    let sessions = RwSignal::new(None::<Vec<SessionInfo>>);
    let reload = move || {
        spawn_local(async move {
            if let Ok(s) = api::get::<Vec<SessionInfo>>("/account/sessions").await {
                sessions.set(Some(s));
            }
        })
    };
    reload();
    let revoke_others = move |_| {
        store.ask(Confirm {
            title: "Alle anderen Geräte abmelden?".into(),
            body: "Überall außer hier musst du dich danach neu anmelden.".into(),
            confirm_label: "Abmelden".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                store.run("Abmelden fehlgeschlagen", async move {
                    api::action("/account/sessions/revoke-others").await?;
                    store.toast("Andere Geräte abgemeldet");
                    reload();
                    Ok(())
                });
            }),
        });
    };
    view! {
        <h3 class="h4">"Angemeldete Geräte"</h3>
        {move || match sessions.get() {
            None => view! { <p class="muted" role="status">"Lade …"</p> }.into_any(),
            Some(list) => {
                let others = list.iter().filter(|s| !s.current).count();
                let now = store.now.get_untracked();
                view! {
                    <ul class="plain stack" aria-label="Angemeldete Geräte" data-testid="sessions">
                        {list.into_iter().map(|s| {
                            let id = s.id;
                            let seen = (now - s.last_seen_at).num_seconds().max(0);
                            view! {
                                <li class="row wrap between list-row">
                                    <span class="grow">
                                        <strong>{describe_user_agent(&s.user_agent)}</strong>
                                        {s.current.then(|| view! { " " <span class="tag tone-success">"Dieses Gerät"</span> })}
                                        <br/>
                                        <span class="small muted">{format!("{} · zuletzt aktiv {} · angemeldet {}",
                                            describe_auth_method(&s.auth_method), ago(seen), when(s.created_at))}</span>
                                    </span>
                                    {(!s.current).then(|| view! {
                                        <button class="btn sm" type="button" aria-label="Dieses Gerät abmelden" on:click=move |_| {
                                            store.run("Abmelden fehlgeschlagen", async move {
                                                api::delete(&format!("/account/sessions/{id}")).await?;
                                                store.toast("Gerät abgemeldet");
                                                reload();
                                                Ok(())
                                            });
                                        }>"Abmelden"</button>
                                    })}
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                    {(others > 0).then(|| view! {
                        <div><button class="btn" type="button" data-testid="sessions-revoke-others" on:click=revoke_others>"Alle anderen Geräte abmelden"</button></div>
                    })}
                }.into_any()
            }
        }}
    }
}

// ---------------------------------------------------------------------------
// Enforced setup (MFA policy)
// ---------------------------------------------------------------------------

/// Shown instead of the app while the instance requires a second factor
/// that this person has not set up yet.
#[component]
pub fn MfaSetupGate(on_done: Callback<()>) -> impl IntoView {
    let codes = RwSignal::new(None::<Vec<String>>);
    let mode = RwSignal::new(0u8); // 0 choose, 1 app, 2 passkey
    let error = RwSignal::new(String::new());
    let overview = RwSignal::new(None::<SecurityOverview>);
    spawn_local(async move {
        if let Ok(o) = api::get::<SecurityOverview>("/account/security").await {
            overview.set(Some(o));
        }
    });
    let finished = Callback::new(move |r: FactorAdded| match r.recovery_codes {
        Some(c) => codes.set(Some(c)),
        None => on_done.run(()),
    });
    let logout = move |_| {
        spawn_local(async move {
            let _ = api::action("/auth/logout").await;
            if let Some(w) = web_sys::window() {
                let _ = w.location().set_href("/");
            }
        });
    };
    view! {
        <main class="center">
            <div class="card login stack" data-testid="mfa-gate">
                <h1 class="brand"><span class="logo"><Icon icon=Ic::Shield/></span>"Zwei-Faktor-Anmeldung einrichten"</h1>
                {move || match (codes.get(), overview.get()) {
                    (Some(c), _) => view! {
                        <p class="lead">"Geschafft! Zum Schluss deine Wiederherstellungscodes:"</p>
                        <RecoveryListPlain codes=c/>
                        <button class="btn primary lg block" type="button" on:click=move |_| on_done.run(())>"Ich habe die Codes gespeichert – weiter"</button>
                    }.into_any(),
                    (None, None) => view! { <p class="muted" role="status">"Lade …"</p> }.into_any(),
                    (None, Some(o)) => match mode.get() {
                        1 => view! { <TotpSetupFlow password_set=o.password_set needs_step_up=o.password_set
                            on_done=finished on_cancel=Callback::new(move |_| mode.set(0))/> }.into_any(),
                        _ => {
                            let pw = o.password_set;
                            view! {
                                <p class="lead">"Auf dieser Instanz ist ein zweiter Faktor Pflicht. So schützt du dein Konto, selbst wenn jemand dein Passwort kennt."</p>
                                <button class="btn primary lg block" type="button" data-testid="gate-totp" on:click=move |_| mode.set(1)>
                                    <Icon icon=Ic::Shield/>"Authenticator-App einrichten"
                                </button>
                                {webauthn::supported().then(|| view! {
                                    <button class="btn lg block" type="button" on:click=move |_| mode.set(2)>
                                        <Icon icon=Ic::Key/>"Passkey verwenden"
                                    </button>
                                })}
                                {move || (mode.get() == 2).then(|| view! {
                                    <PasskeyDialog password_set=pw needs_step_up=pw on_done=finished
                                        on_close=Callback::new(move |_| mode.set(0))/>
                                })}
                                <div role="alert">
                                    {move || (!error.get().is_empty()).then(|| view! {
                                        <div class="alert tone-danger"><Icon icon=Ic::Alert class="sm"/>{error.get()}</div>
                                    })}
                                </div>
                                <button class="btn ghost block" type="button" on:click=logout>"Abmelden"</button>
                            }.into_any()
                        }
                    },
                }}
            </div>
        </main>
    }
}

/// Recovery codes outside the app frame (no toasts available there).
#[component]
fn RecoveryListPlain(codes: Vec<String>) -> impl IntoView {
    let text = codes
        .iter()
        .map(|c| format_recovery_code(c))
        .collect::<Vec<_>>()
        .join("\n");
    let href = format!(
        "data:text/plain;charset=utf-8,{}",
        js_sys::encode_uri_component(&text)
    );
    view! {
        <div class="stack" data-testid="recovery-codes">
            <div class="alert tone-warning"><Icon icon=Ic::Alert class="sm"/>"Speichere diese Codes sicher. Sie werden nur jetzt angezeigt."</div>
            <ul class="plain recovery-grid" aria-label="Wiederherstellungscodes">
                {codes.iter().map(|c| view! { <li class="mono">{format_recovery_code(c)}</li> }).collect_view()}
            </ul>
            <a class="btn" href=href download="queue-wiederherstellungscodes.txt"><Icon icon=Ic::Download class="sm"/>"Als Datei speichern"</a>
        </div>
    }
}
