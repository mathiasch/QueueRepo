//! Project tab "Schutz" (WP03): what Queue checks before changes are saved
//! or uploaded, and the settings the owner may change. Everybody in the
//! project sees them; only the owner changes them (the server enforces it).
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::gate::{default_protected, ProtectionView, UpdateProtection, REPO_CONFIG_SUPPORT};
use queued_api::ProjectView;
use uuid::Uuid;

use crate::api;
use crate::logic::labels::help;
use crate::logic::protection::{is_default, parse_paths, paths_text};
use crate::store::use_store;
use crate::ui::dialog::Confirm;
use crate::ui::{Field, FieldIds, HelpTip, Ic, Icon};

#[component]
pub fn ProjectProtection(p: ProjectView) -> impl IntoView {
    let data = RwSignal::new(None::<ProtectionView>);
    let failed = RwSignal::new(None::<String>);
    let id = p.id;
    spawn_local(async move {
        match api::get::<ProtectionView>(&format!("/projects/{id}/protection")).await {
            Ok(v) => data.set(Some(v)),
            Err(e) => failed.set(Some(e.message)),
        }
    });
    // Rebuilt only on (re)load, so inputs keep their state while typing.
    let generation = RwSignal::new(0u32);
    let loaded = Memo::new(move |_| data.with(|d| d.as_ref().map(|_| generation.get())));
    view! {
        <div class="stack" style="margin-top: var(--sp-4)" data-testid="project-protection">
            {move || failed.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            {move || loaded.get().map(|_| {
                let v = data.get_untracked().expect("loaded");
                view! {
                    <ProtectionForm
                        v=v
                        id=id
                        on_saved=Callback::new(move |nv: ProtectionView| {
                            data.set(Some(nv));
                            generation.update(|g| *g += 1);
                        })
                    />
                }
            })}
        </div>
    }
}

#[component]
fn ProtectionForm(
    v: ProtectionView,
    id: Uuid,
    on_saved: Callback<ProtectionView>,
) -> impl IntoView {
    let store = use_store();
    let ro = !v.can_edit;
    let trust = RwSignal::new(v.trust_repo_config);
    let test_guard = RwSignal::new(v.test_guard);
    let setup_guard = RwSignal::new(v.setup_guard);
    let paths = RwSignal::new(paths_text(&v.protected_paths));
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let approved = v.approved_secrets;

    let send = move |body: UpdateProtection, done: &'static str| {
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, ProtectionView>(&format!("/projects/{id}/protection"), &body).await
            {
                Ok(nv) => {
                    store.toast(done);
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
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let text = paths.get_untracked();
        let list = match parse_paths(&text) {
            Ok(l) => l,
            Err(e) => {
                store.announce("Bitte die geschützten Dateien korrigieren");
                error.set(Some(e));
                return;
            }
        };
        error.set(None);
        let body = UpdateProtection {
            trust_repo_config: Some(trust.get_untracked()),
            protected_paths: Some(if is_default(&text) { None } else { Some(list) }),
            test_guard: Some(test_guard.get_untracked()),
            setup_guard: Some(setup_guard.get_untracked()),
            clear_approved_secrets: false,
        };
        if trust.get_untracked() && !v.trust_repo_config {
            store.ask(Confirm {
                title: "Einstellungen aus dem Repository erlauben?".into(),
                body: "Agenten laden dann Hooks, MCP-Server und Einstellungen aus dem Repository. Diese können eigene Programme starten oder Zugänge umleiten. Nur für Repositories, deren Inhalt du kontrollierst. Der aktuelle Stand gilt beim ersten Lauf als freigegeben; ändern Agenten diese Dateien danach, fragt Queue nach.".into(),
                confirm_label: "Erlauben und speichern".into(),
                danger: true,
                on_confirm: Callback::new(move |_| send(body.clone(), "Schutz gespeichert")),
            });
        } else {
            send(body, "Schutz gespeichert");
        }
    };
    let forget = move |_| {
        store.ask(Confirm {
            title: "Freigegebene Treffer vergessen?".into(),
            body:
                "Queue fragt dann wieder nach, wenn ein früher freigegebener Wert erneut auftaucht."
                    .into(),
            confirm_label: "Vergessen".into(),
            danger: false,
            on_confirm: Callback::new(move |_| {
                send(
                    UpdateProtection {
                        clear_approved_secrets: true,
                        ..Default::default()
                    },
                    "Freigaben vergessen",
                )
            }),
        });
    };
    let paths_ids = FieldIds::new("protected-paths");
    let default_text = paths_text(&default_protected());
    view! {
        {ro.then(|| view! { <p class="alert tone-info"><Icon icon=Ic::Eye class="sm"/>"Nur die Eigentümerin bzw. der Eigentümer des Projekts kann den Schutz ändern."</p> })}
        <form class="card settings-card" on:submit=save novalidate data-testid="protection-form">
            <section class="form-section" aria-labelledby="prot-always">
                <header>
                    <h2 id="prot-always" class="row">"Immer aktiv" <HelpTip topic="Schutzprüfung" text=help::PROTECTION align="align-start"/></h2>
                    <p>"Diese Prüfungen lassen sich nicht abschalten."</p>
                </header>
                <ul class="check-list">
                    <li><Icon icon=Ic::Key class="sm"/><span><strong>"Geheimnisse"</strong>" – Passwörter, Tokens und Schlüssel werden vor dem Speichern und Hochladen gesucht. Im Protokoll erscheinen sie nur geschwärzt."</span></li>
                    <li><Icon icon=Ic::CloudUpload class="sm"/><span><strong>"Hochladen"</strong>" – auf den Standard-Branch (z. B. main) lädt Queue nur mit deiner Freigabe hoch."</span></li>
                    <li><Icon icon=Ic::Shield class="sm"/><span><strong>"Review"</strong>" – fällt der Review-Agent aus, wird nichts ungeprüft als „Fertig“ übernommen."</span></li>
                    <li><Icon icon=Ic::Terminal class="sm"/><span><strong>"Git"</strong>" – Hooks und Programme aus der Git-Konfiguration des Repositories laufen nie auf dem Server."</span></li>
                </ul>
                <p class="small muted" data-testid="approved-secrets">
                    {if approved == 0 { "Keine freigegebenen Treffer.".to_string() } else { format!("{approved} freigegebene(r) Treffer – Queue merkt sich nur einen Fingerabdruck, nie den Wert.") }}
                </p>
                {(!ro && approved > 0).then(|| view! {
                    <button type="button" class="btn sm" style="align-self:flex-start" on:click=forget disabled=move || busy.get()>
                        <Icon icon=Ic::Trash class="sm"/>"Freigaben vergessen"
                    </button>
                })}
            </section>
            <section class="form-section" aria-labelledby="prot-repo">
                <header>
                    <h2 id="prot-repo" class="row">"Einstellungen aus dem Repository" <HelpTip topic="Repository-Einstellungen" text=help::TRUST_REPO align="align-start"/></h2>
                    <p>"Standard: aus. Agenten laden dann keine Hooks, MCP-Server oder Einstellungen aus dem Projekt."</p>
                </header>
                <label class="switch">
                    <input type="checkbox" role="switch" data-testid="trust-repo" disabled=ro prop:checked=move || trust.get() on:change=move |ev| trust.set(event_target_checked(&ev))/>
                    <span class="check-text">"Einstellungen aus dem Repository erlauben"<small>"Änderungen daran brauchen vor dem nächsten Lauf deine Freigabe."</small></span>
                </label>
                <details class="block">
                    <summary><Icon icon=Ic::ChevronRight class="sm chev"/>"So wird es je Agent abgeschaltet"</summary>
                    <ul class="cli-support">
                        {REPO_CONFIG_SUPPORT.iter().map(|c| view! {
                            <li><strong>{c.cli}</strong><span class="small">{c.off}</span><span class="xs muted">{c.evidence}</span></li>
                        }).collect_view()}
                    </ul>
                </details>
            </section>
            <section class="form-section" aria-labelledby="prot-tests">
                <header>
                    <h2 id="prot-tests" class="row">"Tests und geschützte Dateien" <HelpTip topic="Geschützte Dateien" text=help::PROTECTED_PATHS align="align-start"/></h2>
                    <p>"Verhindert, dass ein Agent die Prüfung austrickst, statt die Aufgabe zu lösen."</p>
                </header>
                <Field ids=paths_ids.clone() label="Geschützte Dateien und Ordner" hint="Ein Muster pro Zeile, z. B. tests/ oder *.spec.ts. Leer = keine.">
                    <textarea id=paths_ids.id.clone() class="textarea mono-input" rows="7" spellcheck="false" disabled=ro
                        aria-describedby=paths_ids.described_by() bind:value=paths></textarea>
                </Field>
                {(!ro).then(|| view! {
                    <button type="button" class="btn sm" style="align-self:flex-start" on:click=move |_| paths.set(default_text.clone())>
                        <Icon icon=Ic::Undo class="sm"/>"Standardliste einsetzen"
                    </button>
                })}
                <div class="row">
                    <label class="switch">
                        <input type="checkbox" role="switch" data-testid="test-guard" disabled=ro prop:checked=move || test_guard.get() on:change=move |ev| test_guard.set(event_target_checked(&ev))/>
                        <span class="check-text">"Geschwächte Tests erkennen"<small>"Übersprungene oder gelöschte Tests, entfernte Prüfungen"</small></span>
                    </label>
                    <HelpTip topic="Geschwächte Tests" text=help::TEST_GUARD/>
                </div>
            </section>
            <section class="form-section" aria-labelledby="prot-setup">
                <header>
                    <h2 id="prot-setup" class="row">"Vorbereitung mit Geheimnissen" <HelpTip topic="Vorbereitung" text=help::SETUP_GUARD align="align-start"/></h2>
                    <p>"Gilt nur, wenn die Umgebung des Projekts Geheimnisse hat."</p>
                </header>
                <label class="switch">
                    <input type="checkbox" role="switch" data-testid="setup-guard" disabled=ro prop:checked=move || setup_guard.get() on:change=move |ev| setup_guard.set(event_target_checked(&ev))/>
                    <span class="check-text">"Vor dem Setup nachfragen, wenn sich Setup-Dateien geändert haben"<small>"z. B. package.json, Lock-Dateien oder Skripte, die das Setup aufruft"</small></span>
                </label>
            </section>
            {move || error.get().map(|e| view! {
                <div class="alert tone-danger" role="alert" data-testid="protection-errors"><Icon icon=Ic::Alert class="sm"/><span>{e}</span></div>
            })}
            {(!ro).then(|| view! {
                <div class="form-actions">
                    <button class="btn primary lg" type="submit" disabled=move || busy.get()><Icon icon=Ic::Check class="sm"/>"Schutz speichern"</button>
                </div>
            })}
        </form>
    }
}
