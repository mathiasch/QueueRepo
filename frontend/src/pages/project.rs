//! Project details: files, git history, tasks, settings, environment, members.
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::{
    hooks::{use_navigate, use_params_map, use_query_map},
    NavigateOptions,
};
use queued_api::{
    AddMember, Commit, CostSummary, FileView, ImportResult, Member, PresetInfo, ProjectRole,
    ProjectView, PushResult, Residency, Routing, RoutingUpdate, StepMode, Target, Task,
    UpdateProject,
};
use uuid::Uuid;
use wasm_bindgen::JsCast;

use crate::api::{self, enc};
use crate::logic::costs::{fmt_money_list, step_mode_label, target_text, RESIDENCIES};
use crate::logic::format::{fmt_relative, parse_rfc3339, plural};
use crate::logic::labels::{help, provider_label};
use crate::logic::limits::{LimitField, LimitForm};
use crate::nav::{set_title, with_query};
use crate::store::{tz, use_store};
use crate::ui::dialog::Confirm;
use crate::ui::{uid, Empty, Field, FieldIds, HelpTip, Ic, Icon};

use super::projects::role_label;
use super::queue::TaskRow;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Files,
    Git,
    Tasks,
    Settings,
    Delivery,
    Environment,
    Protection,
    Members,
}

impl Tab {
    const ALL: [Tab; 8] = [
        Tab::Files,
        Tab::Git,
        Tab::Tasks,
        Tab::Settings,
        Tab::Delivery,
        Tab::Environment,
        Tab::Protection,
        Tab::Members,
    ];
    fn key(self) -> &'static str {
        match self {
            Tab::Files => "files",
            Tab::Git => "git",
            Tab::Tasks => "tasks",
            Tab::Settings => "settings",
            Tab::Delivery => "delivery",
            Tab::Environment => "environment",
            Tab::Protection => "protection",
            Tab::Members => "members",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Tab::Files => "Dateien",
            Tab::Git => "Verlauf",
            Tab::Tasks => "Aufträge",
            Tab::Settings => "Einstellungen",
            Tab::Delivery => "Ergebnis",
            Tab::Environment => "Umgebung",
            Tab::Protection => "Schutz",
            Tab::Members => "Mitglieder",
        }
    }
    fn icon(self) -> Ic {
        match self {
            Tab::Files => Ic::File,
            Tab::Git => Ic::Commit,
            Tab::Tasks => Ic::Queue,
            Tab::Settings => Ic::Settings,
            Tab::Delivery => Ic::CloudUpload,
            Tab::Environment => Ic::Terminal,
            Tab::Protection => Ic::Shield,
            Tab::Members => Ic::Users,
        }
    }
    fn parse(s: &str) -> Tab {
        Tab::ALL
            .into_iter()
            .find(|t| t.key() == s)
            .unwrap_or(Tab::Files)
    }
}

#[component]
pub fn ProjectPage() -> impl IntoView {
    let store = use_store();
    let params = use_params_map();
    let query = use_query_map();
    let navigate = use_navigate();
    let id =
        Memo::new(move |_| params.with(|p| p.get("id").and_then(|v| Uuid::parse_str(&v).ok())));
    let tab = Memo::new(move |_| query.with(|q| Tab::parse(&q.get("tab").unwrap_or_default())));
    let project = RwSignal::new(None::<ProjectView>);
    let failed = RwSignal::new(None::<String>);
    let reload = move || {
        let Some(pid) = id.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::get::<ProjectView>(&format!("/projects/{pid}")).await {
                Ok(p) => {
                    set_title(&p.name);
                    project.set(Some(p));
                }
                Err(e) => failed.set(Some(e.message)),
            }
        });
    };
    Effect::new(move |_| {
        id.track();
        project.set(None);
        reload();
    });

    let select = {
        let navigate = navigate.clone();
        move |t: Tab| {
            let url = with_query("tab", Some(t.key()));
            navigate(
                &url,
                NavigateOptions {
                    replace: true,
                    scroll: false,
                    ..Default::default()
                },
            );
        }
    };

    view! {
        <a class="btn ghost sm" href="/projects" style="align-self: flex-start"><Icon icon=Ic::ChevronLeft class="sm"/>"Alle Projekte"</a>
        {move || failed.get().map(|e| view! {
            <div class="card"><Empty icon=Ic::Alert title="Projekt nicht verfügbar" text=e/></div>
        })}
        {move || project.get().map(|p| {
            let select = select.clone();
            let pid = p.id;
            let can_edit = p.role.can_edit();
            view! {
                <div class="page-head">
                    <div class="project-head">
                        <span class="p-icon" style="display:grid;place-items:center;width:44px;height:44px;border-radius:14px;background:var(--primary-soft);color:var(--primary-text)"><Icon icon=Ic::Folder class="lg"/></span>
                        <div>
                            <h1>{p.name.clone()}</h1>
                            <p class="row wrap">
                                <span class="tag">{role_label(p.role)}</span>
                                <span class="xs mono truncate">{p.git_remote_url.clone().unwrap_or_else(|| "Lokales Repository".into())}" · "{p.git_branch.clone()}</span>
                            </p>
                        </div>
                    </div>
                    <a class="btn" href=format!("/api/projects/{pid}/zip") download><Icon icon=Ic::Download class="sm"/>"ZIP herunterladen"</a>
                </div>
                <div class="tablist" role="tablist" aria-label="Projektbereiche">
                    {Tab::ALL.into_iter().map(|t| {
                        let select = select.clone();
                        let select_key = select.clone();
                        view! {
                            <button
                                type="button"
                                role="tab"
                                id=format!("tab-{}", t.key())
                                aria-controls="project-panel"
                                aria-selected=move || (tab.get() == t).to_string()
                                tabindex=move || if tab.get() == t { "0" } else { "-1" }
                                on:click=move |_| select(t)
                                on:keydown=move |ev| {
                                    let i = Tab::ALL.iter().position(|x| *x == t).unwrap_or(0);
                                    let next = match ev.key().as_str() {
                                        "ArrowRight" => Some(Tab::ALL[(i + 1) % Tab::ALL.len()]),
                                        "ArrowLeft" => Some(Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()]),
                                        "Home" => Some(Tab::ALL[0]),
                                        "End" => Some(Tab::ALL[Tab::ALL.len() - 1]),
                                        _ => None,
                                    };
                                    if let Some(n) = next {
                                        ev.prevent_default();
                                        select_key(n);
                                        if let Some(el) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(&format!("tab-{}", n.key()))) {
                                            if let Ok(h) = el.dyn_into::<web_sys::HtmlElement>() {
                                                let _ = h.focus();
                                            }
                                        }
                                    }
                                }
                            >
                                <Icon icon=t.icon() class="sm"/>{t.label()}
                            </button>
                        }
                    }).collect_view()}
                </div>
                <div id="project-panel" role="tabpanel" aria-labelledby=move || format!("tab-{}", tab.get().key())>
                    {move || {
                        // Briefly None while switching projects: render nothing instead of panicking.
                        let Some(p) = project.get() else { return ().into_any() };
                        match tab.get() {
                            Tab::Files => view! { <Files id=pid can_edit=can_edit/> }.into_any(),
                            Tab::Git => view! { <GitLog p=p can_edit=can_edit/> }.into_any(),
                            Tab::Tasks => view! { <ProjectTasks id=pid/> }.into_any(),
                            Tab::Settings => view! {
                                <ProjectRouting p=p.clone() on_saved=Callback::new(move |_| reload())/>
                                <ProjectSettings p=p on_saved=Callback::new(move |_| reload())/>
                            }.into_any(),
                            Tab::Delivery => view! { <super::delivery::ProjectDelivery p=p/> }.into_any(),
                            Tab::Environment => view! { <super::environment::ProjectEnvironment p=p/> }.into_any(),
                            Tab::Protection => view! { <super::protection::ProjectProtection p=p/> }.into_any(),
                            Tab::Members => view! { <Members id=pid is_owner=p.role == ProjectRole::Owner me=store.me.get_untracked().id/> }.into_any(),
                        }
                    }}
                </div>
            }
        })}
    }
}

#[component]
fn Files(id: Uuid, can_edit: bool) -> impl IntoView {
    let store = use_store();
    let files = RwSignal::new(None::<Vec<String>>);
    let filter = RwSignal::new(String::new());
    let open = RwSignal::new(None::<FileView>);
    let busy = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            match api::get::<Vec<String>>(&format!("/projects/{id}/files")).await {
                Ok(f) => files.set(Some(f)),
                Err(e) => store.toast_error("Dateien konnten nicht geladen werden", &e),
            }
        })
    };
    load();
    let shown = Memo::new(move |_| {
        let f = filter.get().to_lowercase();
        files.with(|fs| {
            fs.as_ref()
                .map(|fs| {
                    fs.iter()
                        .filter(|x| x.to_lowercase().contains(&f))
                        .take(500)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    let show = move |path: String| {
        spawn_local(async move {
            match api::get::<FileView>(&format!("/projects/{id}/file?path={}", enc(&path))).await {
                Ok(f) => open.set(Some(f)),
                Err(e) => store.toast_error("Datei konnte nicht geöffnet werden", &e),
            }
        })
    };
    let upload = move |ev: leptos::ev::Event| {
        let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(file) = input.files().and_then(|f| f.get(0)) else {
            return;
        };
        input.set_value("");
        busy.set(true);
        spawn_local(async move {
            match api::upload::<ImportResult>(&format!("/projects/{id}/import"), &file).await {
                Ok(r) => {
                    store.toast(format!(
                        "{} importiert",
                        plural(r.files, "Datei", "Dateien")
                    ));
                    load();
                }
                Err(e) => store.toast_error("Import fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };
    let filter_id = crate::ui::uid("file-filter");
    view! {
        <div class="stack" style="padding-top: var(--sp-4)">
            <div class="row wrap">
                <label class="visually-hidden" for=filter_id.clone()>"Dateien filtern"</label>
                <input
                    id=filter_id
                    class="input grow"
                    type="search"
                    placeholder=move || format!("{} durchsuchen …", plural(files.with(|f| f.as_ref().map(Vec::len).unwrap_or(0)), "Datei", "Dateien"))
                    bind:value=filter
                />
                {can_edit.then(|| view! {
                    <label class="btn">
                        <Icon icon=Ic::Upload class="sm"/>
                        {move || if busy.get() { "Importiere …" } else { "ZIP importieren" }}
                        <input type="file" accept=".zip,application/zip" class="visually-hidden" on:change=upload/>
                    </label>
                })}
            </div>
            <div class=move || if open.with(Option::is_some) { "files-split" } else { "files-split single" }>
                <div class="card file-list" role="list" aria-label="Dateien">
                    {move || match files.get() {
                        None => view! { <p class="muted" style="padding: var(--sp-3)">"Lade …"</p> }.into_any(),
                        Some(f) if f.is_empty() => view! {
                            <Empty icon=Ic::File title="Noch keine Dateien" text="Importiere ein ZIP oder schreib einen Auftrag – der Agent legt die Dateien an."/>
                        }.into_any(),
                        Some(_) => view! {
                            <For each=move || shown.get() key=|f| f.clone() children=move |f| {
                                let path = f.clone();
                                let current = f.clone();
                                view! {
                                    <div role="listitem">
                                        <button class="file" type="button" aria-current=move || open.with(|o| o.as_ref().map(|o| o.path == current).unwrap_or(false)).to_string() on:click=move |_| show(path.clone())>
                                            <Icon icon=Ic::File class="sm"/><span>{f}</span>
                                        </button>
                                    </div>
                                }
                            }/>
                        }.into_any(),
                    }}
                </div>
                {move || open.get().map(|f| view! {
                    <div class="card file-view">
                        <div class="row">
                            <strong class="grow mono small truncate">{f.path.clone()}</strong>
                            <button class="btn icon-only" type="button" aria-label="Datei schließen" on:click=move |_| open.set(None)><Icon icon=Ic::X/></button>
                        </div>
                        <pre class="code" tabindex="0" aria-label=format!("Inhalt von {}", f.path)>{f.content}</pre>
                    </div>
                })}
            </div>
        </div>
    }
}

#[component]
fn GitLog(p: ProjectView, can_edit: bool) -> impl IntoView {
    let store = use_store();
    let commits = RwSignal::new(None::<Vec<Commit>>);
    let busy = RwSignal::new(false);
    let id = p.id;
    spawn_local(async move {
        match api::get::<Vec<Commit>>(&format!("/projects/{id}/commits")).await {
            Ok(c) => commits.set(Some(c)),
            Err(e) => store.toast_error("Verlauf konnte nicht geladen werden", &e),
        }
    });
    let branch = p.git_branch.clone();
    let push = move |_| {
        busy.set(true);
        let branch = branch.clone();
        spawn_local(async move {
            match api::post::<_, PushResult>(
                &format!("/projects/{id}/push"),
                &serde_json::json!({}),
            )
            .await
            {
                Ok(_) => store.toast(format!("Nach {branch} hochgeladen")),
                Err(e) => store.toast_error("Hochladen fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };
    view! {
        <div class="stack" style="padding-top: var(--sp-4)">
            <super::branch::MergeQueueCard project=id/>
            <super::pr::PullRequestsCard project=id/>
            <h2 style="font-size: var(--fs-lg)">"Hauptstand"</h2>
            <div class="row wrap between">
                <p class="small muted row"><Icon icon=Ic::Branch class="sm"/>
                    <span class="mono">{p.git_remote_url.clone().unwrap_or_else(|| "Lokales Repository".into())}" · "{p.git_branch.clone()}</span>
                </p>
                {(can_edit && p.git_remote_url.is_some()).then(|| view! {
                    <button class="btn" type="button" disabled=move || busy.get() on:click=push>
                        <Icon icon=Ic::CloudUpload class="sm"/>{move || if busy.get() { "Lade hoch …" } else { "Jetzt hochladen (Push)" }}
                    </button>
                })}
            </div>
            <div class="list">
                {move || match commits.get() {
                    None => view! { <p class="empty-line">"Lade …"</p> }.into_any(),
                    Some(c) if c.is_empty() => view! { <p class="empty-line">"Noch keine Commits."</p> }.into_any(),
                    Some(c) => c.into_iter().map(|c| {
                        let when = parse_rfc3339(&c.date);
                        view! {
                            <article class="item">
                                <span class="commit-sha" title=c.sha.clone()>{c.sha.chars().take(8).collect::<String>()}</span>
                                <div class="item-body">
                                    <p class="truncate" style="font-weight: 600">{c.subject}</p>
                                    <p class="meta">{c.author}" · "{move || when.map(|w| fmt_relative(w, store.now.get(), &tz())).unwrap_or_else(|| c.date.clone())}</p>
                                </div>
                            </article>
                        }
                    }).collect_view().into_any(),
                }}
            </div>
        </div>
    }
}

#[component]
fn ProjectTasks(id: Uuid) -> impl IntoView {
    let store = use_store();
    let tasks = RwSignal::new(None::<Vec<Task>>);
    spawn_local(async move {
        match api::get::<Vec<Task>>(&format!("/projects/{id}/tasks")).await {
            Ok(t) => tasks.set(Some(t)),
            Err(e) => store.toast_error("Aufträge konnten nicht geladen werden", &e),
        }
    });
    view! {
        <div class="list" style="margin-top: var(--sp-4)">
            {move || match tasks.get() {
                None => view! { <p class="empty-line">"Lade …"</p> }.into_any(),
                Some(t) if t.is_empty() => view! { <p class="empty-line">"Noch keine Aufträge in diesem Projekt."</p> }.into_any(),
                Some(t) => t.into_iter().map(|t| {
                    // Prefer the live version for tasks the store knows.
                    let live = store.task(t.id).unwrap_or(t);
                    view! { <TaskRow t=live/> }
                }).collect_view().into_any(),
            }}
        </div>
    }
}

/// Model choice per step (presets) and the data-residency rule; costs of the project.
#[component]
fn ProjectRouting(p: ProjectView, on_saved: Callback<()>) -> impl IntoView {
    let store = use_store();
    let id = p.id;
    let can_edit = p.role.can_edit();
    let is_owner = p.role == ProjectRole::Owner;
    let residency = RwSignal::new(p.data_residency);
    let current = RwSignal::new(p.routing.clone());
    let presets = RwSignal::new(None::<Result<Vec<PresetInfo>, String>>);
    let costs = RwSignal::new(None::<CostSummary>);
    let busy = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            if can_edit {
                let r = api::get::<Vec<PresetInfo>>(&format!("/projects/{id}/presets"))
                    .await
                    .map_err(|e| e.message);
                presets.set(Some(r));
            }
            if let Ok(c) = api::get::<CostSummary>(&format!("/costs?days=30&project_id={id}")).await
            {
                costs.set(Some(c));
            }
        });
    };
    load();
    let save = move |routing: Option<Routing>, res: Residency| {
        busy.set(true);
        spawn_local(async move {
            let body = RoutingUpdate {
                routing,
                data_residency: res,
            };
            match api::put::<_, ProjectView>(&format!("/projects/{id}/routing"), &body).await {
                Ok(v) => {
                    current.set(v.routing.clone());
                    residency.set(v.data_residency);
                    store.toast("Modellwahl gespeichert");
                    on_saved.run(());
                }
                Err(e) => store.toast_error("Modellwahl nicht gespeichert", &e),
            }
            busy.set(false);
        });
    };
    let res_name = uid("residency");
    view! {
        <section class="card settings-card" style="margin-top: var(--sp-4)" aria-labelledby="routing-title" data-testid="project-routing">
            <header>
                <span class="s-icon"><Icon icon=Ic::Gauge/></span>
                <div>
                    <h2 id="routing-title" class="row">"Modellwahl & Datenstandort" <HelpTip topic="Modellwahl" text=help::ROUTING align="align-start"/></h2>
                    <p>"Welches Modell plant, umsetzt und prüft – und wo deine Daten verarbeitet werden dürfen."</p>
                </div>
            </header>
            <fieldset class="form-section" disabled=!can_edit>
                <legend class="row">"Datenstandort" <HelpTip topic="Datenstandort" text=help::RESIDENCY align="align-start"/></legend>
                <div class="segmented" role="radiogroup" aria-label="Datenstandort">
                    {RESIDENCIES.into_iter().map(|r| {
                        let name = res_name.clone();
                        view! {
                            <label class="seg-radio">
                                <input type="radio" name=name value=r.as_str() prop:checked=move || residency.get() == r
                                    // Lowering the rule is an owner decision (also enforced by the server).
                                    disabled=move || r == Residency::Any && !is_owner && residency.get() != Residency::Any
                                    on:change=move |_| save(current.get_untracked(), r)/>
                                <span>{r.label()}</span>
                            </label>
                        }
                    }).collect_view()}
                </div>
            </fieldset>
            <div class="form-section">
                <h3>"So arbeitet dieses Projekt"</h3>
                {move || match current.get() {
                    None => view! { <p class="small muted" data-testid="routing-current">"Standard: Der Agent des Auftrags (bzw. dein Standard-Anbieter) setzt um und prüft. Keine Planung, kein Batch."</p> }.into_any(),
                    Some(r) => {
                        let cs = store.connectors.get();
                        let exec = target_text(r.execute.target.as_ref(), &cs, provider_label, "Standard");
                        let row = |label: &'static str, mode: Option<StepMode>, t: Option<&Target>| {
                            let what = match mode {
                                Some(StepMode::Off) => "aus".to_string(),
                                _ => format!("{} · {}", target_text(t, &cs, provider_label, "wie Umsetzung"), mode.map(step_mode_label).unwrap_or("Agent")),
                            };
                            view! { <li><strong>{label}</strong>" "{what}</li> }
                        };
                        view! {
                            <ul class="routing-steps" data-testid="routing-current">
                                {row("Planung:", Some(r.plan.mode), r.plan.target.as_ref())}
                                <li><strong>"Umsetzung:"</strong>" "{exec}{r.execute.flex.then_some(" · Flex (−50 %)")}
                                    {(!r.execute.escalation.is_empty()).then(|| format!(" · bei Fehlschlag: {}", r.execute.escalation.join(" → ")))}</li>
                                {row("Prüfung:", Some(r.review.mode), r.review.target.as_ref())}
                                {row("Analyse bei leerer Warteschlange:", Some(r.analyze.mode), r.analyze.target.as_ref())}
                            </ul>
                        }.into_any()
                    }
                }}
            </div>
            {can_edit.then(|| view! {
                <div class="form-section">
                    <h3>"Vorlagen"</h3>
                    {move || match presets.get() {
                        None => view! { <p class="small muted" role="status">"Lade Vorlagen …"</p> }.into_any(),
                        Some(Err(e)) => view! { <p class="alert tone-warning">{e}</p> }.into_any(),
                        Some(Ok(list)) => view! {
                            <div class="preset-grid">
                                {list.into_iter().map(|pr| {
                                    let active = Memo::new({ let pid = pr.id.clone(); move |_| current.with(|c| c.as_ref().and_then(|r| r.preset.clone()) == Some(pid.clone())) });
                                    let routing = pr.routing.clone();
                                    let res = if pr.residency == Residency::Any { None } else { Some(pr.residency) };
                                    let available = pr.available;
                                    view! {
                                        <article class="preset" class:active=move || active.get() data-testid=format!("preset-{}", pr.id)>
                                            <h4>{pr.label.clone()}</h4>
                                            <p class="small">{pr.description.clone()}</p>
                                            {(!pr.estimate.is_empty()).then(|| view! { <p class="small"><strong>{format!("ca. {} pro Versuch", fmt_money_list(&pr.estimate))}</strong>" (mittlerer Auftrag, Annahme)"</p> })}
                                            {pr.missing.clone().filter(|_| !available).map(|m| view! { <p class="small muted">{m}</p> })}
                                            <button class="btn sm" type="button" disabled=move || !available || busy.get() || active.get()
                                                on:click=move |_| save(routing.clone(), res.unwrap_or_else(|| residency.get_untracked()))>
                                                {move || if active.get() { "Aktiv" } else { "Übernehmen" }}
                                            </button>
                                        </article>
                                    }
                                }).collect_view()}
                            </div>
                            <button class="btn ghost sm" type="button" disabled=move || current.with(Option::is_none) || busy.get()
                                on:click=move |_| save(None, residency.get_untracked())>"Zurück zum Standard"</button>
                        }.into_any(),
                    }}
                </div>
            })}
            <div class="form-section">
                <h3>"Kosten der letzten 30 Tage"</h3>
                {move || costs.get().map(|c| view! {
                    <p data-testid="project-cost"><strong>{fmt_money_list(&c.totals)}</strong>
                        {(c.unknown_runs > 0).then(|| format!(" · {} Läufe ohne bekannten Preis", c.unknown_runs))}</p>
                    {(!c.by_model.is_empty()).then(|| view! {
                        <ul class="small muted">
                            {c.by_model.iter().take(5).map(|r| view! { <li>{format!("{}: {}", r.label, crate::logic::costs::fmt_money(r.amount, &r.currency))}</li> }).collect_view()}
                        </ul>
                    })}
                })}
            </div>
        </section>
    }
}

#[component]
fn ProjectSettings(p: ProjectView, on_saved: Callback<()>) -> impl IntoView {
    let store = use_store();
    let can_edit = p.role.can_edit();
    let name = RwSignal::new(p.name.clone());
    let description = RwSignal::new(p.description.clone());
    let goal = RwSignal::new(p.goal.clone());
    let verify = RwSignal::new(p.verify_command.clone().unwrap_or_default());
    let review = RwSignal::new(p.review_enabled);
    let review_provider = RwSignal::new(p.review_provider.clone().unwrap_or_default());
    let review_model = RwSignal::new(p.review_model.clone().unwrap_or_default());
    let remote = RwSignal::new(p.git_remote_url.clone().unwrap_or_default());
    let branch = RwSignal::new(p.git_branch.clone());
    let token = RwSignal::new(None::<String>);
    let auto_push = RwSignal::new(p.auto_push);
    let is_owner = p.role == ProjectRole::Owner;
    let parallelism = RwSignal::new(p.parallelism);
    let plan_mode = RwSignal::new(p.plan_mode);
    let plan_auto = RwSignal::new(p.plan_auto_approve_mins);
    let limits = RwSignal::new(LimitForm::from_project(&p));
    let errors = RwSignal::new(Vec::<(LimitField, String)>::new());
    let busy = RwSignal::new(false);
    let id = p.id;
    let has_token = p.has_git_token;

    let not_enforced = move |cap: fn(&queued_api::ProviderCapabilities) -> bool| {
        store.meta.with(|m| {
            m.provider_capabilities
                .iter()
                .filter(|(n, c)| n.as_str() != "mock" && !cap(c))
                .map(|(n, _)| provider_label(n))
                .collect::<Vec<_>>()
                .join(", ")
        })
    };
    // Live cost guard: the CLI's own limit or live usage priced from the catalog.
    let no_cost = not_enforced(|c| c.max_cost || c.live_usage);
    let no_turns = not_enforced(|c| c.max_turns);

    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let lim = match limits.get_untracked().parse() {
            Ok(l) => l,
            Err(e) => {
                errors.set(e);
                store.announce("Bitte die markierten Limits korrigieren");
                return;
            }
        };
        let mut body = UpdateProject {
            name: Some(name.get_untracked()),
            description: Some(description.get_untracked()),
            goal: Some(goal.get_untracked()),
            verify_command: Some(verify.get_untracked()),
            review_enabled: Some(review.get_untracked()),
            review_provider: Some(review_provider.get_untracked()),
            review_model: Some(review_model.get_untracked()),
            // Git access is the owner's (the server refuses it from editors:
            // it decides where the owner's token is sent).
            git_remote_url: is_owner.then(|| remote.get_untracked()),
            git_branch: is_owner.then(|| branch.get_untracked()),
            git_token: if is_owner {
                token.get_untracked()
            } else {
                None
            },
            auto_push: is_owner.then(|| auto_push.get_untracked()),
            // Owner only (the server refuses it from editors).
            parallelism: is_owner.then(|| parallelism.get_untracked()),
            // WP06, owner only as well.
            plan_mode: is_owner.then(|| plan_mode.get_untracked()),
            plan_auto_approve_mins: is_owner.then(|| plan_auto.get_untracked()),
            ..Default::default()
        };
        lim.apply_to_project(&mut body);
        busy.set(true);
        spawn_local(async move {
            match api::patch::<_, ProjectView>(&format!("/projects/{id}"), &body).await {
                Ok(_) => {
                    token.set(None);
                    store.toast("Projekt-Einstellungen gespeichert");
                    on_saved.run(());
                    store.reload_projects().await;
                }
                Err(e) => store.toast_error("Speichern fehlgeschlagen", &e),
            }
            busy.set(false);
        });
    };

    let navigate = use_navigate();
    let pname = p.name.clone();
    let remove = move |_| {
        let navigate = navigate.clone();
        store.ask(Confirm {
            title: format!("Projekt „{pname}“ löschen?"),
            body: "Das Projekt wird mit Workspace, allen Dateien und allen Aufträgen endgültig gelöscht. Das lässt sich nicht rückgängig machen.".into(),
            confirm_label: "Endgültig löschen".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                let navigate = navigate.clone();
                spawn_local(async move {
                    match api::delete(&format!("/projects/{id}")).await {
                        Ok(()) => {
                            store.reload_projects().await;
                            store.toast("Projekt gelöscht");
                            navigate("/projects", NavigateOptions::default());
                        }
                        Err(e) => store.toast_error("Löschen fehlgeschlagen", &e),
                    }
                });
            }),
        });
    };

    let text = move |sig: RwSignal<String>,
                     label: &'static str,
                     hint: &'static str,
                     help_text: &'static str,
                     placeholder: &'static str| {
        let ids = FieldIds::new("ps");
        let (fid, desc) = (ids.id.clone(), ids.described_by());
        view! {
            <Field ids=ids label=label hint=hint help=help_text>
                <input id=fid class="input" disabled=!can_edit aria-describedby=desc placeholder=placeholder bind:value=sig/>
            </Field>
        }
    };
    let git_text = move |sig: RwSignal<String>,
                         label: &'static str,
                         hint: &'static str,
                         placeholder: &'static str| {
        let ids = FieldIds::new("ps-git");
        let (fid, desc) = (ids.id.clone(), ids.described_by());
        view! {
            <Field ids=ids label=label hint=hint>
                <input id=fid class="input" disabled=!is_owner aria-describedby=desc placeholder=placeholder bind:value=sig/>
            </Field>
        }
    };
    let err = move |f: LimitField| {
        Signal::derive(move || {
            errors.with(|e| e.iter().find(|(x, _)| *x == f).map(|(_, m)| m.clone()))
        })
    };
    let limit = move |f: LimitField,
                      label: &'static str,
                      name_attr: &'static str,
                      mode: &'static str,
                      placeholder: &'static str,
                      help_text: &'static str| {
        let ids = FieldIds::new(name_attr);
        let (fid, desc) = (ids.id.clone(), ids.described_by());
        let e = err(f);
        let get = move || {
            limits.with(|v| match f {
                LimitField::Runtime => v.runtime.clone(),
                LimitField::Stall => v.stall.clone(),
                LimitField::Cost => v.cost.clone(),
                LimitField::Tokens => v.tokens.clone(),
                LimitField::Turns => v.turns.clone(),
            })
        };
        view! {
            <Field ids=ids label=label help=help_text error=e>
                <input
                    id=fid
                    class="input"
                    name=name_attr
                    inputmode=mode
                    placeholder=placeholder
                    disabled=!can_edit
                    aria-describedby=desc
                    aria-invalid=move || e.get().is_some().to_string()
                    prop:value=get
                    on:input=move |ev| {
                        let v = event_target_value(&ev);
                        limits.update(|x| match f {
                            LimitField::Runtime => x.runtime = v,
                            LimitField::Stall => x.stall = v,
                            LimitField::Cost => x.cost = v,
                            LimitField::Tokens => x.tokens = v,
                            LimitField::Turns => x.turns = v,
                        });
                        errors.update(|es| es.retain(|(x, _)| *x != f));
                    }
                />
            </Field>
        }
    };
    let goal_ids = FieldIds::new("ps-goal");
    let par_ids = FieldIds::new("ps-par");
    let auto_ids = FieldIds::new("ps-plan-auto");
    let token_ids = FieldIds::new("ps-token");
    let rp_ids = FieldIds::new("ps-rprov");
    view! {
        <form class="card settings-card" style="margin-top: var(--sp-4)" on:submit=save>
            {(!can_edit).then(|| view! { <p class="alert tone-info"><Icon icon=Ic::Eye class="sm"/>"Du kannst dieses Projekt nur ansehen."</p> })}
            <section class="form-section">
                <header><h2>"Allgemein"</h2></header>
                <div class="grid-2">
                    {text(name, "Name", "", "", "")}
                    {text(description, "Beschreibung", "Erscheint in der Projektliste.", "", "Optional")}
                </div>
                <Field ids=goal_ids.clone() label="Projektziel" help="Wird jedem Agenten mitgegeben und steuert die automatischen Vorschläge bei leerer Warteschlange.">
                    <textarea id=goal_ids.id.clone() class="textarea" rows="3" disabled=!can_edit bind:value=goal></textarea>
                </Field>
            </section>
            <section class="form-section">
                <header>
                    <h2 class="row">"Gleichzeitig arbeiten" <HelpTip topic="Gleichzeitig arbeiten" text=queued_api::branch::PARALLELISM_HELP align="align-start"/></h2>
                    <p>"Wie viele Aufträge dieses Projekts gleichzeitig laufen dürfen – jeder auf seinem eigenen Branch."</p>
                </header>
                <Field ids=par_ids.clone() label="Gleichzeitige Aufträge" hint=if is_owner { "1 = nacheinander wie bisher. Fertige Aufträge werden der Reihe nach übernommen." } else { "Nur die Eigentümerin oder der Eigentümer des Projekts kann das ändern." }>
                    <select
                        id=par_ids.id.clone()
                        class="select"
                        name="parallelism"
                        disabled=!can_edit || !is_owner
                        aria-describedby=par_ids.described_by()
                        prop:value=move || parallelism.get().to_string()
                        on:change=move |ev| parallelism.set(event_target_value(&ev).parse().unwrap_or(1))
                    >
                        {(1..=queued_api::branch::MAX_PARALLELISM).map(|n| view! {
                            <option value=n.to_string() selected=move || parallelism.get() == n>
                                {if n == 1 { "1 – nacheinander".to_string() } else { format!("{n} gleichzeitig") }}
                            </option>
                        }).collect_view()}
                    </select>
                </Field>
            </section>
            <section class="form-section" data-testid="plan-settings">
                <header>
                    <h2 class="row">"Erst planen, dann umsetzen" <HelpTip topic="Plan-Modus" text=queued_api::steer::PLAN_MODE_HELP align="align-start"/></h2>
                    <p>"Der Agent zeigt dir zuerst seinen Plan. Er ändert nichts, bevor du ihn freigibst."</p>
                </header>
                <label class="switch">
                    <input type="checkbox" role="switch" name="plan_mode" disabled=!can_edit || !is_owner prop:checked=move || plan_mode.get() on:change=move |ev| plan_mode.set(event_target_checked(&ev))/>
                    <span class="check-text">"Plan-Modus für neue Aufträge"<small>{if is_owner { "Gilt als Voreinstellung; im Auftrag unter „Erweitert“ änderbar." } else { "Ändern kann nur, wem das Projekt gehört." }}</small></span>
                </label>
                <Field ids=auto_ids.clone() label="Automatisch genehmigen" help=queued_api::steer::AUTO_APPROVE_HELP hint="Standard: nie. Ein Plan mit offenen Fragen wartet immer auf einen Menschen.">
                    <select
                        id=auto_ids.id.clone()
                        class="select"
                        name="plan_auto_approve"
                        disabled=!can_edit || !is_owner
                        aria-describedby=auto_ids.described_by()
                        prop:value=move || plan_auto.get().map(|m| m.to_string()).unwrap_or_default()
                        on:change=move |ev| plan_auto.set(event_target_value(&ev).parse().ok())
                    >
                        {queued_api::steer::AUTO_APPROVE_CHOICES.iter().map(|(v, label)| {
                            let v = *v;
                            view! { <option value=v.map(|m| m.to_string()).unwrap_or_default() selected=move || plan_auto.get() == v>{*label}</option> }
                        }).collect_view()}
                        {move || plan_auto.get().filter(|m| !queued_api::steer::AUTO_APPROVE_CHOICES.iter().any(|(v, _)| *v == Some(*m))).map(|m| view! {
                            <option value=m.to_string() selected=true>{queued_api::steer::auto_approve_label(Some(m))}</option>
                        })}
                    </select>
                </Field>
            </section>
            <section class="form-section">
                <header>
                    <h2>"Qualität sichern"</h2>
                    <p>"Was nach jedem Auftrag geprüft wird, bevor er als fertig gilt."</p>
                </header>
                {text(verify, "Prüfbefehl (Verify)", "Läuft nach jedem Auftrag in der Sandbox und muss erfolgreich enden.", help::VERIFY, "z. B. npm ci && npm test · cargo test · pytest -q")}
                <label class="switch">
                    <input type="checkbox" role="switch" disabled=!can_edit prop:checked=move || review.get() on:change=move |ev| review.set(event_target_checked(&ev))/>
                    <span class="check-text">"Review durch zweiten Agenten"<small>"Frischer Blick: sieht nur Auftrag und Änderungen."</small></span>
                </label>
                {move || review.get().then(|| {
                    let rp_ids = rp_ids.clone();
                    view! {
                        <div class="grid-2">
                            <Field ids=rp_ids.clone() label="Review-Anbieter" help=help::PROVIDER>
                                <select id=rp_ids.id.clone() class="select" disabled=!can_edit prop:value=move || review_provider.get() on:change=move |ev| review_provider.set(event_target_value(&ev))>
                                    <option value="">"wie beim Auftrag"</option>
                                    {store.meta.with_untracked(|m| m.providers.iter().filter(|x| m.credential_kinds.get(*x).is_some_and(|k| !k.is_empty())).map(|x| view! { <option value=x.clone()>{provider_label(x)}</option> }).collect_view())}
                                </select>
                            </Field>
                            {text(review_model, "Review-Modell", "z. B. ein günstigeres Modell", help::MODEL, "Standard")}
                        </div>
                    }
                })}
            </section>
            <section class="form-section">
                <header>
                    <h2 class="row">"Limits pro Auftrag" <HelpTip topic="Limits pro Auftrag" text=help::LIMITS align="align-start"/></h2>
                    <p>"Leer = Standard. Aufträge können eigene Limits haben. Laufzeit und Stillstand gelten je Lauf, Kosten und Tokens für den ganzen Auftrag."</p>
                </header>
                <div class="grid-3" data-testid="project-limits">
                    {limit(LimitField::Runtime, "Max. Laufzeit (Min.)", "max_runtime", "decimal", "Standard", "")}
                    {limit(LimitField::Stall, "Stillstand nach (Min.)", "stall_timeout", "decimal", "Standard", "Stoppt einen Lauf, der so lange nichts mehr ausgegeben hat. 0 = aus.")}
                    {limit(LimitField::Cost, "Max. Kosten (USD)", "max_cost", "decimal", "unbegrenzt", "")}
                    {limit(LimitField::Tokens, "Max. Tokens", "max_tokens", "numeric", "unbegrenzt", help::TOKENS)}
                    {limit(LimitField::Turns, "Max. Turns", "max_turns", "numeric", "unbegrenzt", help::TURNS)}
                </div>
                {(!no_cost.is_empty() || !no_turns.is_empty()).then(|| view! {
                    <p class="small muted">
                        {(!no_cost.is_empty()).then(|| format!("Das Kostenlimit wird bei {no_cost} erst nach dem Lauf geprüft (keine Verbrauchsdaten während des Laufs). "))}
                        {(!no_turns.is_empty()).then(|| format!("Das Turn-Limit wird von {no_turns} nicht unterstützt. "))}
                        "Tokens dieser Anbieter werden erst nach dem Lauf geprüft – eine Überschreitung blockiert den Auftrag dann nachträglich. Kosten werden aus den Tokens und der Preistabelle berechnet."
                    </p>
                })}
            </section>
            <section class="form-section">
                <header>
                    <h2>"Git"</h2>
                    <p>{if is_owner { "Wohin die Änderungen hochgeladen werden. Leer = nur lokal. Ob als Commit, Branch oder Pull Request, steht unter „Ergebnis“." } else { "Den Git-Zugang (Adresse, Branch, Token) ändert nur, wem das Projekt gehört – das Token gehört der Projekt-Eigentümerin bzw. dem Projekt-Eigentümer." }}</p>
                </header>
                <div class="grid-3">
                    {git_text(remote, "Repository-Adresse", "leer = nur lokal. Ein anderer Server löscht das gespeicherte Token.", "https://github.com/org/repo.git")}
                    {git_text(branch, "Branch", "", "")}
                    <Field ids=token_ids.clone() label="Zugangstoken" hint=if has_token { "Ein Token ist gespeichert. Neues eingeben ersetzt es, leeren entfernt es." } else { "Optional, für private Repositories und Push." }>
                        <input
                            id=token_ids.id.clone()
                            class="input"
                            type="password"
                            autocomplete="off"
                            disabled=!is_owner
                            aria-describedby=token_ids.described_by()
                            placeholder=if has_token { "•••••••• (gespeichert)" } else { "optional" }
                            prop:value=move || token.get().unwrap_or_default()
                            on:input=move |ev| token.set(Some(event_target_value(&ev)))
                        />
                    </Field>
                </div>
                <label class="switch">
                    <input type="checkbox" role="switch" disabled=move || !is_owner || remote.with(|r| r.trim().is_empty()) prop:checked=move || auto_push.get() on:change=move |ev| auto_push.set(event_target_checked(&ev))/>
                    <span class="check-text">"Automatisch hochladen"<small>"Nach jedem erfolgreichen Auftrag pushen (braucht eine Repository-Adresse)."</small></span>
                </label>
            </section>
            {can_edit.then(|| view! {
                <div class="form-actions">
                    <button class="btn primary lg" type="submit" disabled=move || busy.get()><Icon icon=Ic::Check class="sm"/>"Speichern"</button>
                </div>
            })}
        </form>
        {(p.role == ProjectRole::Owner).then(|| view! {
            <div class="danger-zone" style="margin-top: var(--sp-5)">
                <div class="grow">
                    <h2 style="font-size: var(--fs-lg)">"Projekt löschen"</h2>
                    <p class="small muted">"Entfernt Workspace, Dateien und alle Aufträge endgültig."</p>
                </div>
                <button class="btn danger" type="button" on:click=remove><Icon icon=Ic::Trash class="sm"/>"Projekt löschen"</button>
            </div>
        })}
    }
}

#[component]
fn Members(id: Uuid, is_owner: bool, me: Uuid) -> impl IntoView {
    let store = use_store();
    let members = RwSignal::new(None::<Vec<Member>>);
    let email = RwSignal::new(String::new());
    let role = RwSignal::new("editor".to_string());
    let load = move || {
        spawn_local(async move {
            match api::get::<Vec<Member>>(&format!("/projects/{id}/members")).await {
                Ok(m) => members.set(Some(m)),
                Err(e) => store.toast_error("Mitglieder konnten nicht geladen werden", &e),
            }
        })
    };
    load();
    let add = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let body = AddMember {
            email: email.get_untracked().trim().into(),
            role: role.get_untracked(),
        };
        spawn_local(async move {
            match api::post_ok(&format!("/projects/{id}/members"), &body).await {
                Ok(()) => {
                    email.set(String::new());
                    store.toast("Mitglied hinzugefügt");
                    load();
                }
                Err(e) => store.toast_error("Hinzufügen fehlgeschlagen", &e),
            }
        });
    };
    let remove = move |m: Member| {
        let uid = m.user_id;
        let me_leaving = uid == me;
        store.ask(Confirm {
            title: if me_leaving {
                "Projekt verlassen?".into()
            } else {
                format!("{} entfernen?", m.name)
            },
            body: if me_leaving {
                "Du verlierst den Zugriff auf dieses Projekt.".into()
            } else {
                format!(
                    "{} ({}) verliert den Zugriff auf dieses Projekt.",
                    m.name, m.email
                )
            },
            confirm_label: if me_leaving {
                "Verlassen".into()
            } else {
                "Entfernen".into()
            },
            danger: true,
            on_confirm: Callback::new(move |_| {
                spawn_local(async move {
                    match api::delete(&format!("/projects/{id}/members/{uid}")).await {
                        Ok(()) => load(),
                        Err(e) => store.toast_error("Entfernen fehlgeschlagen", &e),
                    }
                })
            }),
        });
    };
    let (e_id, r_id) = (crate::ui::uid("m-email"), crate::ui::uid("m-role"));
    view! {
        <div class="stack lg" style="padding-top: var(--sp-4)">
            <p class="small muted">"Jede Person behält ihre eigene Warteschlange und ihr eigenes Abo – geteilt wird nur der Workspace. Pro Projekt läuft immer nur ein Auftrag gleichzeitig."</p>
            <div class="list">
                {move || members.get().map(|ms| ms.into_iter().map(|m| {
                    let can_remove = m.role != ProjectRole::Owner && (is_owner || m.user_id == me);
                    let mm = m.clone();
                    view! {
                        <div class="member">
                            <span class="avatar" aria-hidden="true">{m.name.chars().next().unwrap_or('?').to_uppercase().collect::<String>()}</span>
                            <div class="grow">
                                <strong>{m.name.clone()}</strong>
                                <p class="xs muted truncate">{m.email.clone()}</p>
                            </div>
                            <span class="tag">{role_label(m.role)}</span>
                            {can_remove.then(|| view! {
                                <button class="btn icon-only danger" type="button" aria-label=format!("{} entfernen", mm.name) title="Entfernen" on:click=move |_| remove(mm.clone())>
                                    <Icon icon=Ic::Trash/>
                                </button>
                            })}
                        </div>
                    }
                }).collect_view())}
            </div>
            {is_owner.then(|| view! {
                <form class="card pad stack" on:submit=add>
                    <h2 style="font-size: var(--fs-lg)">"Person einladen"</h2>
                    <div class="row wrap top">
                        <div class="field grow" style="min-width: 220px">
                            <div class="label"><label for=e_id.clone()>"E-Mail-Adresse"</label></div>
                            <input id=e_id.clone() class="input" type="email" required placeholder="Die Person muss sich schon einmal angemeldet haben" bind:value=email/>
                        </div>
                        <div class="field">
                            <div class="label"><label for=r_id.clone()>"Rolle"</label></div>
                            <select id=r_id.clone() class="select" prop:value=move || role.get() on:change=move |ev| role.set(event_target_value(&ev))>
                                <option value="editor">"Bearbeiten"</option>
                                <option value="viewer">"Nur ansehen"</option>
                            </select>
                        </div>
                    </div>
                    <div><button class="btn primary" type="submit"><Icon icon=Ic::Plus class="sm"/>"Hinzufügen"</button></div>
                </form>
            })}
        </div>
    }
}
