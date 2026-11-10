//! Project tab "Ergebnis" (WP05): what a finished task delivers (commit,
//! branch, pull request) and the access to the git host. Everybody in the
//! project sees it; only the owner changes it (the server enforces it).
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::pr::{
    DeliveryView, GitHost, HostCheck, OutputMode, UpdateDelivery, AUTO_FIX_HELP, MAX_FOLLOWUPS,
    ROUNDS_HELP, TRIGGER_WORD,
};
use queued_api::{ProjectRole, ProjectView};
use uuid::Uuid;

use crate::api;
use crate::logic::labels::help;
use crate::logic::pr::{auto_label, labels_text, parse_labels, who_pushes};
use crate::store::use_store;
use crate::ui::{Field, FieldIds, HelpTip, Ic, Icon};

#[component]
pub fn ProjectDelivery(p: ProjectView) -> impl IntoView {
    let data = RwSignal::new(None::<DeliveryView>);
    let failed = RwSignal::new(None::<String>);
    let id = p.id;
    spawn_local(async move {
        match api::get::<DeliveryView>(&format!("/projects/{id}/delivery")).await {
            Ok(v) => data.set(Some(v)),
            Err(e) => failed.set(Some(e.message)),
        }
    });
    let generation = RwSignal::new(0u32);
    let loaded = Memo::new(move |_| data.with(|d| d.as_ref().map(|_| generation.get())));
    let owner = p.role == ProjectRole::Owner;
    view! {
        <div class="stack" style="margin-top: var(--sp-4)" data-testid="project-delivery">
            {move || failed.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            {move || loaded.get().map(|_| {
                let v = data.get_untracked().expect("loaded");
                view! {
                    <DeliveryForm v=v id=id owner=owner on_saved=Callback::new(move |nv: DeliveryView| {
                        data.set(Some(nv));
                        generation.update(|g| *g += 1);
                    })/>
                }
            })}
        </div>
    }
}

#[component]
fn DeliveryForm(
    v: DeliveryView,
    id: Uuid,
    owner: bool,
    on_saved: Callback<DeliveryView>,
) -> impl IntoView {
    let store = use_store();
    let ro = !owner;
    let mode = RwSignal::new(v.output_mode);
    let host = RwSignal::new(
        v.git_host
            .map(|h| h.as_str().to_string())
            .unwrap_or_default(),
    );
    let api_url = RwSignal::new(v.git_api_url.clone().unwrap_or_default());
    let repo = RwSignal::new(v.git_repo_path.clone().unwrap_or_default());
    let draft = RwSignal::new(v.pr_draft);
    let labels = RwSignal::new(labels_text(&v.pr_labels));
    let auto_fix = RwSignal::new(v.pr_auto_fix);
    let rounds = RwSignal::new(v.pr_max_followups);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let check = RwSignal::new(None::<HostCheck>);
    let problems = v.problems.clone();
    let has_remote = v.has_remote;
    let detected = v.detected_host;
    let effective_api = v.effective_api_url.clone();
    let effective_repo = v.effective_repo.clone();
    let token_host = Memo::new(move |_| GitHost::parse(&host.get()).or(detected));

    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        error.set(None);
        let body = UpdateDelivery {
            output_mode: Some(mode.get_untracked()),
            git_host: Some(host.get_untracked()),
            git_api_url: Some(api_url.get_untracked()),
            git_repo_path: Some(repo.get_untracked()),
            pr_draft: Some(draft.get_untracked()),
            pr_labels: Some(parse_labels(&labels.get_untracked())),
            pr_auto_fix: Some(auto_fix.get_untracked()),
            pr_max_followups: Some(rounds.get_untracked()),
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, DeliveryView>(&format!("/projects/{id}/delivery"), &body).await {
                Ok(nv) => {
                    store.toast("Ergebnis-Einstellungen gespeichert");
                    on_saved.run(nv);
                }
                Err(e) => {
                    error.set(Some(e.message.clone()));
                    store.toast_error("Speichern fehlgeschlagen", &e);
                }
            }
            busy.set(false);
        });
    };
    let run_check = move |_| {
        busy.set(true);
        check.set(None);
        spawn_local(async move {
            match api::post::<_, HostCheck>(
                &format!("/projects/{id}/delivery/check"),
                &serde_json::json!({}),
            )
            .await
            {
                Ok(c) => {
                    store.announce(&c.message);
                    check.set(Some(c));
                }
                Err(e) => store.toast_error("Prüfen fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };
    let host_ids = FieldIds::new("dl-host");
    let api_ids = FieldIds::new("dl-api");
    let repo_ids = FieldIds::new("dl-repo");
    let labels_ids = FieldIds::new("dl-labels");
    let rounds_ids = FieldIds::new("dl-rounds");
    view! {
        {ro.then(|| view! { <p class="alert tone-info"><Icon icon=Ic::Eye class="sm"/>"Nur die Eigentümerin bzw. der Eigentümer des Projekts kann das ändern."</p> })}
        {(!problems.is_empty()).then(|| view! {
            <div class="alert tone-warning" role="status" data-testid="delivery-problems">
                <Icon icon=Ic::Alert class="sm"/>
                <div><strong>"Noch nicht bereit:"</strong><ul class="plain">{problems.iter().map(|x| view! { <li>{x.clone()}</li> }).collect_view()}</ul></div>
            </div>
        })}
        <form class="card settings-card" on:submit=save novalidate data-testid="delivery-form">
            <section class="form-section" aria-labelledby="dl-mode">
                <header>
                    <h2 id="dl-mode" class="row">"Ergebnis abliefern als" <HelpTip topic="Ergebnis" text=help::DELIVERY align="align-start"/></h2>
                    <p>"Gilt für Aufträge, die ab jetzt fertig werden."</p>
                </header>
                <fieldset class="choice-list" role="radiogroup" aria-labelledby="dl-mode">
                    {OutputMode::ALL.into_iter().map(|m| {
                        let needs_remote = m.pushes_branch() && !has_remote;
                        view! {
                            <label class="choice" data-testid=format!("mode-{}", m.as_str())>
                                <input type="radio" name="output_mode" value=m.as_str()
                                    disabled=ro || needs_remote
                                    prop:checked=move || mode.get() == m
                                    on:change=move |_| mode.set(m)/>
                                <span class="check-text">{m.label()}
                                    <small>{m.help()}{needs_remote.then_some(" Braucht zuerst eine Repository-Adresse (Einstellungen → Git).")}</small>
                                </span>
                            </label>
                        }
                    }).collect_view()}
                </fieldset>
                <div class="box">
                    <p class="small"><strong>"Wer lädt wann hoch?"</strong></p>
                    <ul class="check-list small" data-testid="who-pushes">
                        {move || who_pushes(mode.get()).iter().map(|l| view! { <li><Icon icon=Ic::Check class="sm"/><span>{*l}</span></li> }).collect_view()}
                    </ul>
                </div>
            </section>
            <section class="form-section" aria-labelledby="dl-host-h">
                <header>
                    <h2 id="dl-host-h" class="row">"Zugang zum Git-Server" <HelpTip topic="Git-Server" text=help::HOST align="align-start"/></h2>
                    <p>"Für Pull Requests. Das Zugangstoken steht unter Einstellungen → Git; es wird verschlüsselt gespeichert und nie angezeigt."</p>
                </header>
                <div class="grid-3">
                    <Field ids=host_ids.clone() label="Git-Server">
                        <select id=host_ids.id.clone() class="select" disabled=ro aria-describedby=host_ids.described_by()
                            prop:value=move || host.get() on:change=move |ev| host.set(event_target_value(&ev))>
                            <option value="">{auto_label(detected)}</option>
                            {GitHost::ALL.into_iter().map(|h| view! { <option value=h.as_str()>{h.label()}</option> }).collect_view()}
                        </select>
                    </Field>
                    <Field ids=api_ids.clone() label="API-Adresse" hint=effective_api.clone().map(|a| format!("leer = {a}")).unwrap_or_else(|| "nur bei eigenen Servern nötig".into())>
                        <input id=api_ids.id.clone() class="input" inputmode="url" spellcheck="false" disabled=ro
                            aria-describedby=api_ids.described_by() placeholder="https://git.example.org/api/v1" bind:value=api_url/>
                    </Field>
                    <Field ids=repo_ids.clone() label="Repository" hint=effective_repo.clone().map(|r| format!("leer = {r}")).unwrap_or_else(|| "besitzer/repository".into())>
                        <input id=repo_ids.id.clone() class="input" spellcheck="false" disabled=ro
                            aria-describedby=repo_ids.described_by() placeholder="besitzer/repository" bind:value=repo/>
                    </Field>
                </div>
                <p class="small muted" data-testid="token-rights">
                    {move || match token_host.get() {
                        Some(h) => format!("Nötige Token-Rechte bei {}: {}", h.label(), h.token_help()),
                        None => "Wähle den Git-Server, um die nötigen Token-Rechte zu sehen.".into(),
                    }}
                </p>
                {owner.then(|| view! {
                    <button type="button" class="btn sm" style="align-self:flex-start" disabled=move || busy.get() on:click=run_check data-testid="host-check">
                        <Icon icon=Ic::Key class="sm"/>"Verbindung prüfen"
                    </button>
                })}
                {move || check.get().map(|c| view! {
                    <p class=format!("alert {}", if c.ok { "tone-success" } else { "tone-danger" }) role="status" data-testid="host-check-result">
                        <Icon icon=if c.ok { Ic::CheckCircle } else { Ic::Alert } class="sm"/>{c.message}
                    </p>
                })}
            </section>
            <section class="form-section" aria-labelledby="dl-pr">
                <header>
                    <h2 id="dl-pr" class="row">"Pull Requests" <HelpTip topic="Auto-Fix" text=AUTO_FIX_HELP align="align-start"/></h2>
                    <p>{format!("Allgemeine Kommentare zählen nur mit {TRIGGER_WORD}. Kommentare am Code und „Änderungen anfordern“ zählen immer – aber nur von Personen mit Schreibrecht.")}</p>
                </header>
                <label class="switch">
                    <input type="checkbox" role="switch" disabled=ro prop:checked=move || draft.get() on:change=move |ev| draft.set(event_target_checked(&ev))/>
                    <span class="check-text">"Als Entwurf anlegen"<small>"Der Pull Request ist erst bereit, wenn ihr ihn freigebt."</small></span>
                </label>
                <label class="switch">
                    <input type="checkbox" role="switch" data-testid="auto-fix" disabled=ro prop:checked=move || auto_fix.get() on:change=move |ev| auto_fix.set(event_target_checked(&ev))/>
                    <span class="check-text">"Auto-Fix"<small>"Rote CI und Review-Kommentare starten Folge-Aufträge auf demselben Branch."</small></span>
                </label>
                <div class="grid-2">
                    <Field ids=labels_ids.clone() label="Labels" hint="Durch Komma getrennt, z. B. queue, ki">
                        <input id=labels_ids.id.clone() class="input" disabled=ro aria-describedby=labels_ids.described_by() bind:value=labels/>
                    </Field>
                    <Field ids=rounds_ids.clone() label="Folge-Runden pro Pull Request" help=ROUNDS_HELP>
                        <select id=rounds_ids.id.clone() class="select" disabled=ro aria-describedby=rounds_ids.described_by()
                            prop:value=move || rounds.get().to_string()
                            on:change=move |ev| rounds.set(event_target_value(&ev).parse().unwrap_or(3))>
                            {(0..=MAX_FOLLOWUPS).map(|n| view! {
                                <option value=n.to_string() selected=move || rounds.get() == n>
                                    {if n == 0 { "0 – keine automatischen Folge-Aufträge".to_string() } else { n.to_string() }}
                                </option>
                            }).collect_view()}
                        </select>
                    </Field>
                </div>
            </section>
            {move || error.get().map(|e| view! {
                <div class="alert tone-danger" role="alert" data-testid="delivery-errors"><Icon icon=Ic::Alert class="sm"/><span>{e}</span></div>
            })}
            {(!ro).then(|| view! {
                <div class="form-actions">
                    <button class="btn primary lg" type="submit" disabled=move || busy.get()><Icon icon=Ic::Check class="sm"/>"Speichern"</button>
                </div>
            })}
        </form>
    }
}
