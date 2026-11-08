//! Project tab "Umgebung" (WP02): setup script, variables, secrets, network
//! per phase and dependency cache. Changes are owner-only (the server
//! enforces the same); everybody in the project sees the configuration.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::env::{
    EnvVar, EnvironmentView, NetMode, NetworkPolicy, SecretInfo, SetSecret, DEFAULT_CACHE_FILES,
    PRESETS,
};
use queued_api::{ProjectRole, ProjectView};
use uuid::Uuid;

use crate::api::{self, enc};
use crate::logic::env::{mode_help, mode_label, sandbox_status, EnvForm, NetForm, Phase, MODES};
use crate::logic::format::fmt_relative;
use crate::store::{tz, use_store};
use crate::ui::dialog::Confirm;
use crate::ui::{uid, Field, FieldIds, HelpTip, Ic, Icon};

const HELP_SETUP: &str = "Läuft vor jedem Auftrag in einem eigenen Container im Projektordner, zum Beispiel „npm ci“ oder „pip install -r requirements.txt“. Nur das Setup bekommt die Geheimnisse. Es bricht beim ersten fehlerhaften Befehl ab; dann startet der Agent nicht.";
const HELP_NET: &str = "Container haben keinen eigenen Internetzugang. Jede Verbindung läuft über einen Prüf-Proxy der Queue, der nur erlaubte Adressen durchlässt und alles protokolliert. Interne Netze, der Server selbst und Cloud-Metadaten sind immer gesperrt.";
const HELP_READ_ONLY: &str = "Erlaubt zu den freigegebenen Adressen nur Abrufen (GET, HEAD, OPTIONS), kein Hochladen. Für HTTPS prüft die Queue dazu die Anfragen mit einem eigenen, nur für diesen Lauf gültigen Zertifikat. Programme mit fest eingebauten Zertifikaten funktionieren dann nicht.";
const HELP_CACHE: &str = "Heruntergeladene Pakete werden pro Projekt zwischengespeichert, damit das Setup schneller läuft. Der Cache wird neu angelegt, wenn sich Setup-Skript, Variablen, Setup-Netz oder eine Lock-Datei ändern. Der Agent selbst kann den Cache nicht verändern.";

struct VarRow {
    key: u64,
    name: RwSignal<String>,
    value: RwSignal<String>,
}

struct NetSignals {
    mode: RwSignal<NetMode>,
    presets: RwSignal<Vec<String>>,
    domains: RwSignal<String>,
    read_only: RwSignal<bool>,
}

impl NetSignals {
    fn new(p: &NetworkPolicy) -> Self {
        let f = NetForm::from(p);
        Self {
            mode: RwSignal::new(f.mode),
            presets: RwSignal::new(f.presets),
            domains: RwSignal::new(f.domains),
            read_only: RwSignal::new(f.read_only),
        }
    }
    fn form(&self) -> NetForm {
        NetForm {
            mode: self.mode.get_untracked(),
            presets: self.presets.get_untracked(),
            domains: self.domains.get_untracked(),
            read_only: self.read_only.get_untracked(),
        }
    }
}

#[component]
pub fn ProjectEnvironment(p: ProjectView) -> impl IntoView {
    let data = RwSignal::new(None::<EnvironmentView>);
    let failed = RwSignal::new(None::<String>);
    let id = p.id;
    let load = move || {
        spawn_local(async move {
            match api::get::<EnvironmentView>(&format!("/projects/{id}/environment")).await {
                Ok(v) => data.set(Some(v)),
                Err(e) => failed.set(Some(e.message)),
            }
        });
    };
    load();
    let is_owner = p.role == ProjectRole::Owner;
    let can_edit = p.role.can_edit();
    // Built once per load: inputs keep their state while typing.
    let loaded = Memo::new(move |_| data.with(|d| d.as_ref().map(|v| v.updated_at)));
    view! {
        <div class="stack" style="margin-top: var(--sp-4)" data-testid="project-environment">
            {move || failed.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            {move || loaded.get().map(|_| {
                let v = data.get_untracked().expect("loaded");
                view! {
                    <SandboxBanner v=v.clone()/>
                    {(!is_owner).then(|| view! { <p class="alert tone-info"><Icon icon=Ic::Eye class="sm"/>"Nur die Eigentümerin bzw. der Eigentümer des Projekts kann die Umgebung ändern."</p> })}
                    <EnvEditor v=v.clone() id=id is_owner=is_owner can_edit=can_edit on_saved=Callback::new(move |nv: EnvironmentView| data.set(Some(nv)))/>
                    <Secrets id=id is_owner=is_owner initial=v.secrets.clone()/>
                }
            })}
        </div>
    }
}

#[component]
fn SandboxBanner(v: EnvironmentView) -> impl IntoView {
    let (tone, title, text) = sandbox_status(&v.sandbox);
    let icon = if v.sandbox.egress_enforced {
        Ic::Shield
    } else {
        Ic::Alert
    };
    view! {
        <div class=format!("alert {tone} env-banner") data-testid="sandbox-status" role="status">
            <Icon icon=icon/>
            <div>
                <strong>{title}</strong>
                <p class="small">{text}</p>
            </div>
        </div>
    }
}

#[component]
fn EnvEditor(
    v: EnvironmentView,
    id: Uuid,
    is_owner: bool,
    can_edit: bool,
    on_saved: Callback<EnvironmentView>,
) -> impl IntoView {
    let store = use_store();
    let script = RwSignal::new(v.setup_script.clone());
    let timeout = RwSignal::new(v.setup_timeout_secs.to_string());
    let next_key = StoredValue::new(0u64);
    let mk_row = move |e: &EnvVar| {
        let key = next_key.get_value();
        next_key.set_value(key + 1);
        VarRow {
            key,
            name: RwSignal::new(e.name.clone()),
            value: RwSignal::new(e.value.clone()),
        }
    };
    let vars = RwSignal::new(v.env_vars.iter().map(mk_row).collect::<Vec<_>>());
    let setup = NetSignals::new(&v.setup_network);
    let agent = NetSignals::new(&v.agent_network);
    let cache_enabled = RwSignal::new(v.cache_enabled);
    let cache_files = RwSignal::new(v.cache_files.join("\n"));
    let errors = RwSignal::new(Vec::<String>::new());
    let busy = RwSignal::new(false);
    let ro = !is_owner;

    let collect = {
        let (s_mode, s_presets, s_domains, s_ro) =
            (setup.mode, setup.presets, setup.domains, setup.read_only);
        let (a_mode, a_presets, a_domains, a_ro) =
            (agent.mode, agent.presets, agent.domains, agent.read_only);
        move || EnvForm {
            script: script.get_untracked(),
            timeout: timeout.get_untracked(),
            vars: vars.with_untracked(|rs| {
                rs.iter()
                    .map(|r| EnvVar {
                        name: r.name.get_untracked(),
                        value: r.value.get_untracked(),
                    })
                    .collect()
            }),
            setup: NetSignals {
                mode: s_mode,
                presets: s_presets,
                domains: s_domains,
                read_only: s_ro,
            }
            .form(),
            agent: NetSignals {
                mode: a_mode,
                presets: a_presets,
                domains: a_domains,
                read_only: a_ro,
            }
            .form(),
            cache_enabled: cache_enabled.get_untracked(),
            cache_files: cache_files.get_untracked(),
        }
    };
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let body = match collect().to_update() {
            Ok(b) => b,
            Err(es) => {
                store.announce("Bitte die markierten Angaben korrigieren");
                errors.set(es);
                return;
            }
        };
        errors.set(vec![]);
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, EnvironmentView>(&format!("/projects/{id}/environment"), &body)
                .await
            {
                Ok(nv) => {
                    store.toast("Umgebung gespeichert");
                    on_saved.run(nv);
                }
                Err(e) => {
                    errors.set(vec![e.message.clone()]);
                    store.toast_error("Speichern fehlgeschlagen", &e);
                }
            }
            busy.set(false);
        });
    };
    let clear_cache = move |_| {
        spawn_local(async move {
            match api::action(&format!("/projects/{id}/environment/cache/clear")).await {
                Ok(()) => store.toast("Cache geleert – das nächste Setup lädt alles neu"),
                Err(e) => store.toast_error("Cache leeren fehlgeschlagen", &e),
            }
        });
    };

    let script_ids = FieldIds::new("env-script");
    let timeout_ids = FieldIds::new("env-timeout");
    let cache_ids = FieldIds::new("env-cache-files");
    let provider_hosts = v.provider_hosts.clone();
    view! {
        <form class="card settings-card" on:submit=save novalidate data-testid="env-form">
            <section class="form-section">
                <header>
                    <h2 class="row">"Setup vor jedem Auftrag" <HelpTip topic="Setup" text=HELP_SETUP align="align-start"/></h2>
                    <p>"Installiert, was der Agent braucht. Leer = kein Setup."</p>
                </header>
                <Field ids=script_ids.clone() label="Setup-Skript" hint="Shell-Befehle, eine Zeile pro Befehl. Läuft mit „sh -e“ im Projektordner.">
                    <textarea id=script_ids.id.clone() class="textarea mono-input" rows="5" spellcheck="false" disabled=ro
                        aria-describedby=script_ids.described_by() placeholder="npm ci" bind:value=script></textarea>
                </Field>
                <div class="grid-3">
                    <Field ids=timeout_ids.clone() label="Zeitlimit (Sekunden)" hint="10 bis 3600">
                        <input id=timeout_ids.id.clone() class="input" inputmode="numeric" disabled=ro aria-describedby=timeout_ids.described_by() bind:value=timeout/>
                    </Field>
                </div>
            </section>
            <section class="form-section" aria-labelledby="env-vars-title">
                <header>
                    <h2 id="env-vars-title">"Umgebungsvariablen"</h2>
                    <p>"Sichtbar für Setup, Agent und Prüfbefehl – und für alle im Projekt. Nichts Geheimes eintragen; dafür gibt es unten die Geheimnisse. Variablen, die KI-Zugänge steuern (etwa ANTHROPIC_… oder OPENAI_…), sind gesperrt."</p>
                </header>
                <div class="env-vars" data-testid="env-vars">
                    <For
                        each=move || vars.with(|rs| rs.iter().map(|r| (r.key, r.name, r.value)).collect::<Vec<_>>())
                        key=|(k, _, _)| *k
                        children=move |(k, name, value)| {
                            let nid = uid("env-var-name");
                            let vid = uid("env-var-value");
                            view! {
                                <div class="env-var-row">
                                    <label class="visually-hidden" for=nid.clone()>"Name"</label>
                                    <input id=nid class="input mono-input" placeholder="NAME" autocomplete="off" spellcheck="false" disabled=ro bind:value=name/>
                                    <label class="visually-hidden" for=vid.clone()>{move || format!("Wert von {}", name.get())}</label>
                                    <input id=vid class="input mono-input" placeholder="Wert" autocomplete="off" spellcheck="false" disabled=ro bind:value=value/>
                                    {(!ro).then(|| view! {
                                        <button type="button" class="btn ghost icon-only" aria-label=move || format!("Variable {} entfernen", name.get())
                                            on:click=move |_| vars.update(|rs| rs.retain(|r| r.key != k))>
                                            <Icon icon=Ic::Trash class="sm"/>
                                        </button>
                                    })}
                                </div>
                            }
                        }
                    />
                    {move || vars.with(|rs| rs.is_empty()).then(|| view! { <p class="small muted">"Keine Variablen."</p> })}
                </div>
                {(!ro).then(|| view! {
                    <button type="button" class="btn sm" style="align-self:flex-start" on:click=move |_| {
                        let row = mk_row(&EnvVar { name: String::new(), value: String::new() });
                        vars.update(|rs| rs.push(row));
                    }><Icon icon=Ic::Plus class="sm"/>"Variable hinzufügen"</button>
                })}
            </section>
            <NetCard phase=Phase::Setup sig=setup ro=ro provider_hosts=vec![]/>
            <NetCard phase=Phase::Agent sig=agent ro=ro provider_hosts=provider_hosts/>
            <section class="form-section">
                <header>
                    <h2 class="row">"Cache" <HelpTip topic="Cache" text=HELP_CACHE align="align-start"/></h2>
                    <p>{format!("Lock-Dateien, die den Cache bestimmen: {} …", DEFAULT_CACHE_FILES[..5].join(", "))}</p>
                </header>
                <label class="switch">
                    <input type="checkbox" role="switch" disabled=ro prop:checked=move || cache_enabled.get() on:change=move |ev| cache_enabled.set(event_target_checked(&ev))/>
                    <span class="check-text">"Abhängigkeiten zwischenspeichern"<small>"Nur das Setup schreibt in den Cache."</small></span>
                </label>
                <Field ids=cache_ids.clone() label="Weitere Dateien für den Cache-Schlüssel" hint="Relativer Pfad, eine Datei pro Zeile, z. B. web/package-lock.json">
                    <textarea id=cache_ids.id.clone() class="textarea mono-input" rows="2" spellcheck="false" disabled=ro aria-describedby=cache_ids.described_by() bind:value=cache_files></textarea>
                </Field>
                {can_edit.then(|| view! {
                    <button type="button" class="btn sm" style="align-self:flex-start" on:click=clear_cache data-testid="env-clear-cache"><Icon icon=Ic::Trash class="sm"/>"Cache leeren"</button>
                })}
            </section>
            {move || (!errors.with(Vec::is_empty)).then(|| view! {
                <div class="alert tone-danger" role="alert" data-testid="env-errors">
                    <Icon icon=Ic::Alert class="sm"/>
                    <ul>{errors.get().into_iter().map(|e| view! { <li>{e}</li> }).collect_view()}</ul>
                </div>
            })}
            {is_owner.then(|| view! {
                <div class="form-actions">
                    <button class="btn primary lg" type="submit" disabled=move || busy.get()><Icon icon=Ic::Check class="sm"/>"Umgebung speichern"</button>
                </div>
            })}
        </form>
    }
}

#[component]
fn NetCard(phase: Phase, sig: NetSignals, ro: bool, provider_hosts: Vec<String>) -> impl IntoView {
    let (title, lead, key) = match phase {
        Phase::Setup => (
            "Netz im Setup",
            "Was das Setup-Skript herunterladen darf.",
            "setup",
        ),
        Phase::Agent => (
            "Netz für den Agenten",
            "Was der Agent, der Prüfbefehl und das Review erreichen dürfen.",
            "agent",
        ),
    };
    let group = uid(&format!("net-{key}"));
    let domain_ids = FieldIds::new(&format!("net-{key}-domains"));
    let NetSignals {
        mode,
        presets,
        domains,
        read_only,
    } = sig;
    let help_id = uid(&format!("net-{key}-help"));
    let help_ref = help_id.clone();
    view! {
        <section class="form-section net-card" data-testid=format!("net-{key}")>
            <header>
                <h2 class="row">{title} <HelpTip topic=title text=HELP_NET align="align-start"/></h2>
                <p>{lead}</p>
            </header>
            <div class="segmented" role="radiogroup" aria-label=title aria-describedby=help_ref>
                {MODES.into_iter().map(|m| {
                    let name = group.clone();
                    view! {
                        <label class="seg-radio">
                            <input type="radio" name=name value=m.as_str() disabled=ro prop:checked=move || mode.get() == m on:change=move |_| mode.set(m)/>
                            <span>{mode_label(phase, m)}</span>
                        </label>
                    }
                }).collect_view()}
            </div>
            <p class="small" id=help_id>{move || mode_help(phase, mode.get())}</p>
            {(phase == Phase::Agent && !provider_hosts.is_empty()).then(|| view! {
                <p class="small muted" data-testid="provider-hosts">
                    "Immer erreichbar (Schnittstelle des KI-Modells): "
                    <span class="mono">{provider_hosts.join(", ")}</span>
                </p>
            })}
            {move || (mode.get() == NetMode::Allowlist).then(|| {
                let domain_ids = domain_ids.clone();
                view! {
                    <div class="preset-grid" role="group" aria-label=format!("{title}: Paketquellen")>
                        {PRESETS.iter().map(|p| {
                            let pid = p.id;
                            view! {
                                <label class="check" title=p.domains.join(", ")>
                                    <input type="checkbox" disabled=ro
                                        prop:checked=move || presets.with(|ps| ps.iter().any(|x| x == pid))
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            presets.update(|ps| {
                                                ps.retain(|x| x != pid);
                                                if on { ps.push(pid.to_string()); }
                                            });
                                        }/>
                                    <span class="check-text">{p.label}<small class="mono">{p.domains[0]}{(p.domains.len() > 1).then(|| format!(" +{}", p.domains.len() - 1))}</small></span>
                                </label>
                            }
                        }).collect_view()}
                    </div>
                    <Field ids=domain_ids.clone() label="Weitere Adressen" hint="Eine pro Zeile: example.com, *.example.com (alle Unterdomains) oder host:port">
                        <textarea id=domain_ids.id.clone() class="textarea mono-input" rows="3" spellcheck="false" disabled=ro
                            aria-describedby=domain_ids.described_by() placeholder="docs.rs" bind:value=domains></textarea>
                    </Field>
                }
            })}
            {move || (mode.get() != NetMode::Off).then(|| view! {
                <div class="row">
                    <label class="switch">
                        <input type="checkbox" role="switch" disabled=ro prop:checked=move || read_only.get() on:change=move |ev| read_only.set(event_target_checked(&ev))/>
                        <span class="check-text">"Nur lesen"<small>"Nur Abrufen, kein Hochladen (GET, HEAD, OPTIONS)."</small></span>
                    </label>
                    <HelpTip topic="Nur lesen" text=HELP_READ_ONLY/>
                </div>
            })}
        </section>
    }
}

#[component]
fn Secrets(id: Uuid, is_owner: bool, initial: Vec<SecretInfo>) -> impl IntoView {
    let store = use_store();
    let list = RwSignal::new(initial);
    let name = RwSignal::new(String::new());
    let value = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let reload = move || {
        spawn_local(async move {
            if let Ok(v) = api::get::<EnvironmentView>(&format!("/projects/{id}/environment")).await
            {
                list.set(v.secrets);
            }
        });
    };
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let n = name.get_untracked().trim().to_string();
        let val = value.get_untracked();
        if let Err(e) = queued_api::env::check_name(&n, true)
            .and_then(|_| queued_api::env::check_secret_value(&val))
        {
            error.set(Some(e));
            return;
        }
        error.set(None);
        busy.set(true);
        spawn_local(async move {
            match api::put_ok(
                &format!("/projects/{id}/secrets/{}", enc(&n)),
                &SetSecret { value: val },
            )
            .await
            {
                Ok(()) => {
                    name.set(String::new());
                    value.set(String::new());
                    store.toast(format!("Geheimnis {n} gespeichert"));
                    reload();
                }
                Err(e) => error.set(Some(e.message)),
            }
            busy.set(false);
        });
    };
    let remove = move |n: String| {
        store.ask(Confirm {
            title: format!("Geheimnis {n} entfernen?"),
            body: "Das Setup bekommt den Wert ab dem nächsten Auftrag nicht mehr. Der Wert lässt sich nicht wiederherstellen.".into(),
            confirm_label: "Entfernen".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                let n = n.clone();
                spawn_local(async move {
                    match api::delete(&format!("/projects/{id}/secrets/{}", enc(&n))).await {
                        Ok(()) => {
                            store.toast(format!("Geheimnis {n} entfernt"));
                            reload();
                        }
                        Err(e) => store.toast_error("Entfernen fehlgeschlagen", &e),
                    }
                });
            }),
        });
    };
    let name_ids = FieldIds::new("secret-name");
    let value_ids = FieldIds::new("secret-value");
    let err = Signal::derive(move || error.get());
    view! {
        <section class="card settings-card" aria-labelledby="secrets-title" data-testid="env-secrets">
            <header>
                <span class="s-icon"><Icon icon=Ic::Key/></span>
                <div>
                    <h2 id="secrets-title">"Geheimnisse"</h2>
                    <p>"Zum Beispiel ein Token für eine private Paketquelle. Verschlüsselt gespeichert, nie wieder angezeigt und nur im Setup als Umgebungsvariable verfügbar – nie für den Agenten."</p>
                </div>
            </header>
            <p class="alert tone-warning small"><Icon icon=Ic::Alert class="sm"/>
                "Skripte aus dem Projekt, die das Setup startet (etwa beim Installieren von Paketen), sehen die Geheimnisse auch – einschließlich Änderungen, die ein früherer Agent gemacht hat. Nur Zugänge mit minimalen Rechten verwenden, am besten nur lesend."
            </p>
            <ul class="secret-list" data-testid="secret-list">
                {move || {
                    let now = chrono::Utc::now();
                    let items = list.get();
                    if items.is_empty() {
                        return view! { <li class="small muted">"Noch keine Geheimnisse."</li> }.into_any();
                    }
                    items.into_iter().map(|s| {
                        let n = s.name.clone();
                        view! {
                            <li class="secret-row">
                                <Icon icon=Ic::Key class="sm"/>
                                <span class="mono grow">{s.name.clone()}</span>
                                <span class="xs muted">{format!("geändert {}", fmt_relative(s.updated_at, now, &tz()))}</span>
                                {is_owner.then(|| {
                                    let n = n.clone();
                                    view! {
                                        <button type="button" class="btn ghost sm" aria-label=format!("Geheimnis {n} entfernen") on:click=move |_| remove(n.clone())>
                                            <Icon icon=Ic::Trash class="sm"/>"Entfernen"
                                        </button>
                                    }
                                })}
                            </li>
                        }
                    }).collect_view().into_any()
                }}
            </ul>
            {is_owner.then(|| view! {
                <form class="grid-3 secret-form" on:submit=save novalidate>
                    <Field ids=name_ids.clone() label="Name" hint="GROSSBUCHSTABEN, z. B. NPM_TOKEN. Gleicher Name ersetzt den Wert.">
                        <input id=name_ids.id.clone() class="input mono-input" autocomplete="off" spellcheck="false" aria-describedby=name_ids.described_by() bind:value=name/>
                    </Field>
                    <Field ids=value_ids.clone() label="Wert" hint="Mindestens 8 Zeichen" error=err>
                        <input id=value_ids.id.clone() class="input" type="password" autocomplete="new-password" aria-describedby=value_ids.described_by()
                            aria-invalid=move || error.get().is_some().to_string() bind:value=value/>
                    </Field>
                    <div class="field secret-submit">
                        <button class="btn primary" type="submit" disabled=move || busy.get()><Icon icon=Ic::Key class="sm"/>"Geheimnis speichern"</button>
                    </div>
                </form>
            })}
        </section>
    }
}
