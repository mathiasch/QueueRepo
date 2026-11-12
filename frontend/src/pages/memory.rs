//! WP09 project tabs "Wissen" (managed project rules with versions,
//! proposals, import/export, what each CLI reads) and "Zustand" (the
//! structured project state), plus the dedupe explanation in the drawer.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::memory::{
    feature_status_label, knowledge_source_label, FeatureStatus, KnowledgeDecision,
    KnowledgeExportView, KnowledgeImport, KnowledgeVersion, KnowledgeView, ProjectState, PutState,
    SaveKnowledge, Screening, StateView, Verdict, CLI_INSTRUCTIONS, IMPORTABLE_FILES,
    KNOWLEDGE_HELP, STATE_HELP,
};
use queued_api::{ProjectView, Task};
use uuid::Uuid;

use crate::api;
use crate::logic::format::fmt_relative;
use crate::logic::memory::{
    add_feature, diff_counts, line_diff, set_status, status_counts, DiffLine,
};
use crate::nav::use_open_task;
use crate::store::{tz, use_store};
use crate::ui::dialog::Confirm;
use crate::ui::{uid, Field, FieldIds, HelpTip, Ic, Icon};

// ---------------------------------------------------------------------------
// Wissen
// ---------------------------------------------------------------------------

#[component]
pub fn ProjectKnowledge(p: ProjectView) -> impl IntoView {
    let data = RwSignal::new(None::<KnowledgeView>);
    let failed = RwSignal::new(None::<String>);
    let id = p.id;
    let load = move || {
        spawn_local(async move {
            match api::get::<KnowledgeView>(&format!("/projects/{id}/knowledge")).await {
                Ok(v) => data.set(Some(v)),
                Err(e) => failed.set(Some(e.message)),
            }
        });
    };
    load();
    // Rebuilt only on (re)load, so the editor keeps its text while typing.
    let generation = RwSignal::new(0u32);
    let loaded = Memo::new(move |_| data.with(|d| d.as_ref().map(|_| generation.get())));
    view! {
        <div class="stack" style="margin-top: var(--sp-4)" data-testid="project-knowledge">
            {move || failed.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            {move || loaded.get().map(|_| {
                let v = data.get_untracked().expect("loaded");
                view! {
                    <KnowledgeEditor v=v id=id on_saved=Callback::new(move |nv: KnowledgeView| {
                        data.set(Some(nv));
                        generation.update(|g| *g += 1);
                    })/>
                }
            })}
        </div>
    }
}

fn when(v: &KnowledgeVersion) -> String {
    let store = use_store();
    fmt_relative(
        v.decided_at.unwrap_or(v.created_at),
        store.now.get_untracked(),
        &tz(),
    )
}

#[component]
fn KnowledgeEditor(v: KnowledgeView, id: Uuid, on_saved: Callback<KnowledgeView>) -> impl IntoView {
    let store = use_store();
    let current = v.current.clone();
    let base_version = current.as_ref().and_then(|c| c.version);
    let current_text = current
        .as_ref()
        .map(|c| c.content.clone())
        .unwrap_or_default();
    let text = RwSignal::new(current_text.clone());
    let note = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let owner = v.can_approve;
    let ro = !v.can_propose;
    let editor_ids = FieldIds::new("rules-text");
    let note_ids = FieldIds::new("rules-note");

    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let body = SaveKnowledge {
            content: text.get_untracked(),
            note: note.get_untracked(),
            base_version,
        };
        if let Err(e) = queued_api::memory::validate_knowledge(&body.content, &body.note) {
            error.set(Some(e));
            return;
        }
        error.set(None);
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, KnowledgeView>(&format!("/projects/{id}/knowledge"), &body).await {
                Ok(nv) => {
                    store.toast(if owner {
                        "Regeln gespeichert und freigegeben"
                    } else {
                        "Vorschlag gesendet – der Eigentümer entscheidet"
                    });
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
    let decide = move |kid: Uuid, approve: bool| {
        spawn_local(async move {
            let what = if approve { "approve" } else { "reject" };
            match api::post::<_, KnowledgeView>(
                &format!("/projects/{id}/knowledge/{kid}/{what}"),
                &KnowledgeDecision::default(),
            )
            .await
            {
                Ok(nv) => {
                    store.toast(if approve {
                        "Vorschlag freigegeben – gilt ab dem nächsten Auftrag"
                    } else {
                        "Vorschlag abgelehnt"
                    });
                    on_saved.run(nv);
                }
                Err(e) => store.toast_error("Entscheidung fehlgeschlagen", &e),
            }
        });
    };
    let import_file = RwSignal::new("AGENTS.md".to_string());
    let import = move |_| {
        let file = import_file.get_untracked();
        spawn_local(async move {
            match api::post::<_, KnowledgeView>(
                &format!("/projects/{id}/knowledge/import"),
                &KnowledgeImport { file: file.clone() },
            )
            .await
            {
                Ok(nv) => {
                    store.toast(format!("{file} als Vorschlag übernommen"));
                    on_saved.run(nv);
                }
                Err(e) => store.toast_error("Übernehmen fehlgeschlagen", &e),
            }
        });
    };
    let export_result = RwSignal::new(None::<KnowledgeExportView>);
    let export = move |_| {
        store.ask(Confirm {
            title: "Regeln ins Repository schreiben?".into(),
            body: "Queue schreibt die freigegebene Fassung als AGENTS.md und legt CLAUDE.md und GEMINI.md als Verweis darauf an (nur, wenn es sie noch nicht gibt). Eine vorhandene AGENTS.md wird ersetzt; die alte Fassung bleibt im Git-Verlauf.".into(),
            confirm_label: "Schreiben".into(),
            danger: false,
            on_confirm: Callback::new(move |_| {
                spawn_local(async move {
                    match api::post::<_, KnowledgeExportView>(&format!("/projects/{id}/knowledge/export"), &serde_json::json!({})).await {
                        Ok(r) => {
                            store.toast("Regeln ins Repository geschrieben");
                            export_result.set(Some(r));
                        }
                        Err(e) => store.toast_error("Schreiben fehlgeschlagen", &e),
                    }
                });
            }),
        });
    };
    let import_ids = FieldIds::new("rules-import");
    let status = match &current {
        Some(c) => format!(
            "Version {} · freigegeben {}{}",
            c.version.unwrap_or(0),
            when(c),
            c.decided_by
                .as_ref()
                .map(|d| format!(" von {d}"))
                .unwrap_or_default()
        ),
        None => "Noch keine Regeln – Agenten bekommen nur den Auftrag.".into(),
    };
    let proposals = v.proposals.clone();
    let versions = v.versions.clone();
    let rejected = v.rejected.clone();
    let trust = v.trust_repo_config;
    view! {
        <form class="card settings-card" on:submit=save novalidate data-testid="rules-form">
            <section class="form-section" aria-labelledby="rules-title">
                <header>
                    <h2 id="rules-title" class="row"><Icon icon=Ic::Book/>"Projekt-Regeln" <HelpTip topic="Projekt-Regeln" text=KNOWLEDGE_HELP align="align-start"/></h2>
                    <p data-testid="rules-status">{status}</p>
                </header>
                {ro.then(|| view! { <p class="alert tone-info"><Icon icon=Ic::Eye class="sm"/>"Du kannst die Regeln lesen. Ändern oder vorschlagen dürfen Bearbeiter und der Eigentümer."</p> })}
                {(!ro && !owner).then(|| view! { <p class="alert tone-info"><Icon icon=Ic::Info class="sm"/>"Deine Änderung wird ein Vorschlag. Der Eigentümer gibt sie frei."</p> })}
                <Field ids=editor_ids.clone() label="Regeln (Markdown)" hint="Kurz und konkret, z. B. „Vor dem Abschluss immer cargo test laufen lassen“.">
                    <textarea id=editor_ids.id.clone() class="textarea mono-input" rows="12" spellcheck="true" disabled=ro data-testid="rules-text"
                        aria-describedby=editor_ids.described_by() bind:value=text></textarea>
                </Field>
                {(!ro).then(|| view! {
                    <Field ids=note_ids.clone() label="Was hat sich geändert?" hint="Optional – erscheint im Verlauf.">
                        <input id=note_ids.id.clone() class="input" aria-describedby=note_ids.described_by() bind:value=note/>
                    </Field>
                })}
                {move || error.get().map(|e| view! { <div class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/><span>{e}</span></div> })}
                {(!ro).then(|| view! {
                    <div class="form-actions">
                        <button class="btn primary" type="submit" data-testid="rules-save" disabled=move || busy.get()>
                            <Icon icon=Ic::Check class="sm"/>{if owner { "Speichern und freigeben" } else { "Als Vorschlag senden" }}
                        </button>
                    </div>
                })}
            </section>
        </form>

        <section class="card" aria-labelledby="proposals-title" data-testid="rules-proposals">
            <h2 id="proposals-title" class="row"><Icon icon=Ic::Inbox/>"Vorschläge" <span class="tag">{proposals.len()}</span></h2>
            {proposals.is_empty().then(|| view! { <p class="small muted">"Keine offenen Vorschläge. Agenten können Regeln vorschlagen, wenn ihnen etwas Wiederkehrendes auffällt – wirksam werden sie erst nach deiner Freigabe."</p> })}
            <ul class="proposal-list">
                {proposals.into_iter().map(|pr| {
                    let kid = pr.id;
                    let diff = line_diff(&current_text, &pr.content);
                    let (add, del) = diff_counts(&diff);
                    let source = knowledge_source_label(&pr.source);
                    let by = pr.task_title.clone().map(|t| format!("aus „{t}“")).or(pr.author.clone()).unwrap_or_default();
                    let created = when(&pr);
                    let open = use_open_task();
                    let task_link = pr.task_id.map(|tid| view! { <button type="button" class="link-btn small" on:click=move |_| open(tid)>"Auftrag öffnen"</button> });
                    let note = pr.note.clone();
                    let reject = move |_| {
                        store.ask(Confirm {
                            title: "Vorschlag ablehnen?".into(),
                            body: "Der Vorschlag wird nicht übernommen. Er bleibt im Verlauf sichtbar.".into(),
                            confirm_label: "Ablehnen".into(),
                            danger: true,
                            on_confirm: Callback::new(move |_| decide(kid, false)),
                        });
                    };
                    view! {
                        <li class="proposal" data-testid="rules-proposal">
                            <div class="row wrap">
                                <span class="tag">{source}</span>
                                <span class="small">{by}</span>
                                <span class="xs muted">{created}</span>
                                {task_link}
                            </div>
                            {(!note.is_empty()).then(|| view! { <p class="small">{note}</p> })}
                            <p class="xs muted">{format!("{add} Zeile(n) neu, {del} Zeile(n) entfernt gegenüber der aktuellen Fassung")}</p>
                            <pre class="rules-diff" tabindex="0" aria-label="Unterschied zur aktuellen Fassung">
                                {diff.into_iter().map(|l| match l {
                                    DiffLine::Added(t) => view! { <span class="d-add"><span class="visually-hidden">"neu: "</span>{format!("+ {t}\n")}</span> }.into_any(),
                                    DiffLine::Removed(t) => view! { <span class="d-del"><span class="visually-hidden">"entfernt: "</span>{format!("- {t}\n")}</span> }.into_any(),
                                    DiffLine::Same(t) => view! { <span>{format!("  {t}\n")}</span> }.into_any(),
                                }).collect_view()}
                            </pre>
                            {owner.then(|| view! {
                                <div class="row wrap">
                                    <button type="button" class="btn primary sm" data-testid="rules-approve" on:click=move |_| decide(kid, true)><Icon icon=Ic::Check class="sm"/>"Freigeben"</button>
                                    <button type="button" class="btn sm" data-testid="rules-reject" on:click=reject><Icon icon=Ic::X class="sm"/>"Ablehnen"</button>
                                </div>
                            })}
                        </li>
                    }
                }).collect_view()}
            </ul>
        </section>

        <section class="card" aria-labelledby="versions-title">
            <h2 id="versions-title" class="row"><Icon icon=Ic::Clock/>"Versionen"</h2>
            {versions.is_empty().then(|| view! { <p class="small muted">"Noch keine Versionen."</p> })}
            <ul class="version-list" data-testid="rules-versions">
                {versions.into_iter().map(|ver| {
                    let content = ver.content.clone();
                    let label = format!("Version {} · {} · {}", ver.version.unwrap_or(0), knowledge_source_label(&ver.source), when(&ver));
                    view! {
                        <li>
                            <details class="block">
                                <summary><Icon icon=Ic::ChevronRight class="sm chev"/>{label}</summary>
                                {(!ver.note.is_empty()).then(|| view! { <p class="small">{ver.note.clone()}</p> })}
                                <div class="box pre">{content.clone()}</div>
                                {owner.then(|| view! {
                                    <button type="button" class="btn sm" on:click=move |_| {
                                        text.set(content.clone());
                                        store.announce("Fassung in den Editor übernommen – zum Wiederherstellen speichern");
                                    }><Icon icon=Ic::Undo class="sm"/>"In den Editor übernehmen"</button>
                                })}
                            </details>
                        </li>
                    }
                }).collect_view()}
            </ul>
            {(!rejected.is_empty()).then(|| view! {
                <details class="block">
                    <summary><Icon icon=Ic::ChevronRight class="sm chev"/>{format!("Abgelehnte Vorschläge ({})", rejected.len())}</summary>
                    <ul class="plain-list">
                        {rejected.into_iter().map(|r| view! { <li class="small">{format!("{} · {}", knowledge_source_label(&r.source), when(&r))}{(!r.note.is_empty()).then(|| format!(" – {}", r.note))}</li> }).collect_view()}
                    </ul>
                </details>
            })}
        </section>

        <section class="card" aria-labelledby="repo-rules-title">
            <h2 id="repo-rules-title" class="row"><Icon icon=Ic::Branch/>"Repository"</h2>
            <p class="small">"AGENTS.md, CLAUDE.md und GEMINI.md im Repository werden nie ungeprüft zu Regeln. Übernehmen erzeugt einen Vorschlag; Schreiben legt die freigegebene Fassung als AGENTS.md ab (mit Verweisen für Claude und Gemini)."</p>
            {(!ro).then(|| view! {
                <div class="row wrap">
                    <label class="visually-hidden" for=import_ids.id.clone()>"Datei"</label>
                    <select id=import_ids.id.clone() class="select" style="max-width: 12rem" prop:value=move || import_file.get() on:change=move |ev| import_file.set(event_target_value(&ev))>
                        {IMPORTABLE_FILES.iter().map(|f| view! { <option value=*f>{*f}</option> }).collect_view()}
                    </select>
                    <button type="button" class="btn sm" data-testid="rules-import" on:click=import><Icon icon=Ic::Download class="sm"/>"Als Vorschlag übernehmen"</button>
                    {owner.then(|| view! {
                        <button type="button" class="btn sm" data-testid="rules-export" disabled=base_version.is_none() on:click=export><Icon icon=Ic::Upload class="sm"/>"Ins Repository schreiben"</button>
                    })}
                </div>
            })}
            {move || export_result.get().map(|r| view! {
                <p class="alert tone-success" role="status" data-testid="rules-export-result">
                    <Icon icon=Ic::CheckCircle class="sm"/>
                    <span>
                        {format!("Version {} geschrieben: {}.", r.version, if r.written.is_empty() { "nichts geändert".to_string() } else { r.written.join(", ") })}
                        {(!r.skipped.is_empty()).then(|| format!(" Nicht angefasst (eigene Datei vorhanden): {}.", r.skipped.join(", ")))}
                        {r.baseline_updated.then_some(" Die Freigabe für die Agent-Konfiguration ist aktualisiert.")}
                    </span>
                </p>
            })}
            <details class="block">
                <summary><Icon icon=Ic::ChevronRight class="sm chev"/>"Was liest welcher Agent?"</summary>
                <p class="small">{if trust {
                    "In diesem Projekt sind Einstellungen aus dem Repository erlaubt (Reiter Schutz): die Agenten lesen die Dateien zusätzlich, jede Änderung daran braucht eine Freigabe."
                } else {
                    "Standard in diesem Projekt: die Agenten lesen keine Anweisungsdateien aus dem Repository. Die freigegebenen Regeln bekommen sie von Queue im Auftrag."
                }}</p>
                <ul class="cli-support" data-testid="cli-instructions">
                    {CLI_INSTRUCTIONS.iter().map(|c| view! {
                        <li>
                            <strong>{c.cli}</strong>
                            <span class="small">{format!("Liest von sich aus: {}", c.reads)}</span>
                            <span class="small">{if trust { c.trusted } else { c.standard }}</span>
                            <span class="xs muted">{c.evidence}</span>
                        </li>
                    }).collect_view()}
                </ul>
            </details>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Zustand
// ---------------------------------------------------------------------------

#[component]
pub fn ProjectStatePanel(p: ProjectView) -> impl IntoView {
    let data = RwSignal::new(None::<StateView>);
    let failed = RwSignal::new(None::<String>);
    let id = p.id;
    spawn_local(async move {
        match api::get::<StateView>(&format!("/projects/{id}/state")).await {
            Ok(v) => data.set(Some(v)),
            Err(e) => failed.set(Some(e.message)),
        }
    });
    let generation = RwSignal::new(0u32);
    let loaded = Memo::new(move |_| data.with(|d| d.as_ref().map(|_| generation.get())));
    view! {
        <div class="stack" style="margin-top: var(--sp-4)" data-testid="project-state">
            {move || failed.get().map(|e| view! { <p class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/>{e}</p> })}
            {move || loaded.get().map(|_| {
                let v = data.get_untracked().expect("loaded");
                view! {
                    <StateEditor v=v id=id on_saved=Callback::new(move |nv: StateView| {
                        data.set(Some(nv));
                        generation.update(|g| *g += 1);
                    })/>
                }
            })}
        </div>
    }
}

fn status_tone(s: FeatureStatus) -> &'static str {
    match s {
        FeatureStatus::Done => "tone-success",
        FeatureStatus::InProgress => "tone-info",
        FeatureStatus::Blocked => "tone-danger",
        FeatureStatus::Todo => "tone-neutral",
    }
}

#[component]
fn StateEditor(v: StateView, id: Uuid, on_saved: Callback<StateView>) -> impl IntoView {
    let store = use_store();
    let state = RwSignal::new(v.state.clone());
    let dirty = RwSignal::new(false);
    let new_title = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let base_rev = v.rev;
    let can_edit = v.can_edit;
    let add_ids = FieldIds::new("state-add");
    let add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let t = new_title.get_untracked();
        let mut s = state.get_untracked();
        match add_feature(&mut s, &t) {
            Ok(()) => {
                state.set(s);
                new_title.set(String::new());
                dirty.set(true);
                error.set(None);
            }
            Err(e) => error.set(Some(e)),
        }
    };
    let save = move |_| {
        let body = PutState {
            state: state.get_untracked(),
            base_rev,
            note: "Im Projekt bearbeitet".into(),
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, StateView>(&format!("/projects/{id}/state"), &body).await {
                Ok(nv) => {
                    store.toast("Projektzustand gespeichert");
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
    let history = v.history.clone();
    let summary = move || {
        let s: ProjectState = state.get();
        if s.features.is_empty() {
            return "Noch keine Einträge.".to_string();
        }
        status_counts(&s)
            .into_iter()
            .map(|(st, n)| format!("{n} {}", feature_status_label(st)))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let open = use_open_task();
    view! {
        <section class="card" aria-labelledby="state-title">
            <h2 id="state-title" class="row"><Icon icon=Ic::ListChecks/>"Projektzustand" <HelpTip topic="Projektzustand" text=STATE_HELP align="align-start"/></h2>
            <p class="small" data-testid="state-summary">{summary}</p>
            <p class="xs muted">{if base_rev == 0 { "Noch kein Stand gespeichert.".to_string() } else { format!("Stand {base_rev}") }}</p>
            <ul class="feature-list" data-testid="state-features">
                <For each=move || state.get().features key=|f| (f.id.clone(), f.status.as_str(), f.passes) let:f>
                    {
                        let fid = f.id.clone();
                        let fid_rm = f.id.clone();
                        let sel_id = uid("feature-status");
                        let title = f.title.clone();
                        let title2 = f.title.clone();
                        view! {
                            <li class="feature" data-testid="state-feature">
                                <div class="feature-head">
                                    <strong class="grow">{f.title.clone()}</strong>
                                    {f.passes.then(|| view! { <span class="tag tone-success"><Icon icon=Ic::CheckCircle class="sm"/>"geprüft"</span> })}
                                    {if can_edit {
                                        view! {
                                            <label class="visually-hidden" for=sel_id.clone()>{format!("Status von „{title}“")}</label>
                                            <select id=sel_id class="select sm-select" data-testid="state-status"
                                                on:change=move |ev| {
                                                    let v = event_target_value(&ev);
                                                    if let Some(st) = FeatureStatus::ALL.into_iter().find(|s| s.as_str() == v) {
                                                        state.update(|s| set_status(s, &fid, st));
                                                        dirty.set(true);
                                                    }
                                                }>
                                                {FeatureStatus::ALL.into_iter().map(|st| view! {
                                                    <option value=st.as_str() selected=st == f.status>{feature_status_label(st)}</option>
                                                }).collect_view()}
                                            </select>
                                            <button type="button" class="btn ghost icon-only" aria-label=format!("„{title2}“ entfernen")
                                                on:click=move |_| { state.update(|s| s.features.retain(|x| x.id != fid_rm)); dirty.set(true); }>
                                                <Icon icon=Ic::Trash class="sm"/>
                                            </button>
                                        }.into_any()
                                    } else {
                                        view! { <span class=format!("pill {}", status_tone(f.status))><span class="dot" aria-hidden="true"></span>{feature_status_label(f.status)}</span> }.into_any()
                                    }}
                                </div>
                                <span class="xs muted mono">{f.id.clone()}</span>
                                {(!f.files.is_empty()).then(|| view! { <p class="xs mono">{f.files.join(", ")}</p> })}
                                {(!f.notes.is_empty()).then(|| view! { <p class="small">{f.notes.clone()}</p> })}
                                {(!f.steps.is_empty()).then(|| view! {
                                    <ul class="plain-list xs">{f.steps.iter().map(|s| view! { <li>{s.clone()}</li> }).collect_view()}</ul>
                                })}
                            </li>
                        }
                    }
                </For>
            </ul>
            {can_edit.then(|| view! {
                <form class="row wrap" on:submit=add novalidate>
                    <Field ids=add_ids.clone() label="Neuer Eintrag">
                        <input id=add_ids.id.clone() class="input" data-testid="state-new" placeholder="z. B. Login per Passkey" aria-describedby=add_ids.described_by() bind:value=new_title/>
                    </Field>
                    <button type="submit" class="btn sm" style="align-self:flex-end"><Icon icon=Ic::Plus class="sm"/>"Hinzufügen"</button>
                </form>
            })}
            {move || error.get().map(|e| view! { <div class="alert tone-danger" role="alert"><Icon icon=Ic::Alert class="sm"/><span>{e}</span></div> })}
            {can_edit.then(|| view! {
                <div class="form-actions">
                    <button type="button" class="btn primary" data-testid="state-save" disabled=move || busy.get() || !dirty.get() on:click=save>
                        <Icon icon=Ic::Check class="sm"/>"Änderungen speichern"
                    </button>
                </div>
            })}
        </section>
        <section class="card" aria-labelledby="state-log-title">
            <h2 id="state-log-title" class="row"><Icon icon=Ic::Clock/>"Fortschritt und Verlauf"</h2>
            {move || {
                let prog = state.with(|s| s.progress.clone());
                (!prog.is_empty()).then(|| view! {
                    <ol class="plain-list small" data-testid="state-progress">
                        {prog.into_iter().rev().take(10).map(|e| view! { <li>{e.text}<span class="xs muted">{format!(" · {}", e.at)}</span></li> }).collect_view()}
                    </ol>
                })
            }}
            <ul class="plain-list small" data-testid="state-history">
                {history.into_iter().map(|h| {
                    let open = open.clone();
                    let who = match (&h.task_title, &h.author) {
                        (Some(t), _) => format!("Agent im Auftrag „{t}“"),
                        (None, Some(a)) => a.clone(),
                        _ => if h.source == "agent" { "Agent".into() } else { "Mensch".into() },
                    };
                    let when = fmt_relative(h.created_at, store.now.get_untracked(), &tz());
                    view! {
                        <li>
                            {format!("Stand {} · {who} · {when}", h.rev)}
                            {(!h.note.is_empty()).then(|| view! { <span class="xs muted">{format!(" – {}", h.note)}</span> })}
                            {h.task_id.map(|t| view! { " " <button type="button" class="link-btn xs" on:click=move |_| open(t)>"öffnen"</button> })}
                        </li>
                    }
                }).collect_view()}
            </ul>
        </section>
    }
}

// ---------------------------------------------------------------------------
// Drawer: dedupe / critic explanation
// ---------------------------------------------------------------------------

fn verdict_tone(v: Verdict) -> &'static str {
    match v {
        Verdict::Accepted => "tone-success",
        Verdict::Review => "tone-warning",
        Verdict::Dropped => "tone-danger",
    }
}

/// "Warum dieser Auftrag?" (task created by OnEmpty) and "Geprüfte
/// Vorschläge" (the OnEmpty task itself).
#[component]
pub fn DedupeBlock(t: Task) -> impl IntoView {
    let own = t
        .dedupe
        .as_ref()
        .and_then(|v| serde_json::from_value::<Screening>(v.clone()).ok());
    let report: Vec<Screening> = t
        .proposals
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let open = use_open_task();
    let open2 = open.clone();
    view! {
        {own.map(|s| {
            let sim = s.similar.clone();
            view! {
                <section class="block" data-testid="dedupe-reason">
                    <h3 class="row">"Warum dieser Vorschlag?" <HelpTip topic="Prüfung der Vorschläge" text=queued_api::memory::DEDUPE_HELP align="align-start"/></h3>
                    <p><span class=format!("pill {}", verdict_tone(s.verdict))><span class="dot" aria-hidden="true"></span>{s.verdict.label()}</span>
                       <span class="xs muted">{if s.critic.contains("model") { " Geprüft nach Regeln und von einem Modell" } else { " Geprüft nach Regeln" }}</span></p>
                    <ul class="plain-list small">{s.reasons.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}</ul>
                    {sim.map(|x| {
                        let tid = x.task_id;
                        let open = open.clone();
                        view! { <p class="small">"Ähnlichster Auftrag: " <button type="button" class="link-btn" on:click=move |_| open(tid)>{x.title.clone()}</button>{format!(" ({} %)", (x.score * 100.0).round() as i32)}</p> }
                    })}
                </section>
            }
        })}
        {(!report.is_empty()).then(|| view! {
            <section class="block" data-testid="dedupe-report">
                <h3 class="row">"Geprüfte Vorschläge" <HelpTip topic="Prüfung der Vorschläge" text=queued_api::memory::DEDUPE_HELP align="align-start"/></h3>
                <ul class="proposal-list">
                    {report.into_iter().map(|s| {
                        let open = open2.clone();
                        view! {
                            <li class="proposal" data-testid="dedupe-item">
                                <div class="row wrap">
                                    <span class=format!("pill {}", verdict_tone(s.verdict))><span class="dot" aria-hidden="true"></span>{s.verdict.label()}</span>
                                    {match s.task_id {
                                        Some(tid) => view! { <button type="button" class="link-btn" on:click=move |_| open(tid)>{s.title.clone()}</button> }.into_any(),
                                        None => view! { <strong>{s.title.clone()}</strong> }.into_any(),
                                    }}
                                </div>
                                <ul class="plain-list small">{s.reasons.iter().map(|r| view! { <li>{r.clone()}</li> }).collect_view()}</ul>
                            </li>
                        }
                    }).collect_view()}
                </ul>
            </section>
        })}
    }
}
