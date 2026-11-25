//! WP12 in the task drawer: Best-of-N comparison and selection, the
//! routing explanation ("Warum dieses Modell?"), screenshots and the live
//! preview in a sandboxed frame.
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::bon::{attempt_status_label, AttemptView, Comparison, SelectAttempt};
use queued_api::preview::{PreviewConfig, PreviewStatus, PreviewView, ScreenshotView, Viewport};
use queued_api::Task;
use uuid::Uuid;

use crate::api;
use crate::logic::bon::{
    changes_label, comparison_status, cost_label, decision_text, review_label, secs_label,
    verify_label,
};
use crate::logic::format::fmt_time;
use crate::nav::use_open_task;
use crate::store::{tz, use_store};
use crate::ui::dialog::Confirm;
use crate::ui::{HelpTip, Ic, Icon};

const HELP_BON: &str = "Mehrere Agenten lösen denselben Auftrag gleichzeitig, jeder in seinem eigenen, abgeschotteten Arbeitsbereich. Du vergleichst die Ergebnisse und übernimmst das beste – die anderen werden verworfen und aufgeräumt.";
const HELP_PREVIEW: &str = "Startet die App dieses Auftrags (Dev-Server) in seiner abgeschotteten Umgebung. Sie ist nur für Projektmitglieder erreichbar, läuft auf einer eigenen Adresse und hat keinen Zugriff auf Queue oder deine Anmeldung. Sie endet automatisch.";

/// Comparison of a Best-of-N task (`group`): one card per attempt, a table
/// as text alternative, "Diesen übernehmen".
#[component]
pub fn ComparisonBlock(group: Uuid) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let data = RwSignal::new(None::<Result<Comparison, String>>);
    let busy = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            let r = api::get::<Comparison>(&format!("/tasks/{group}/attempts"))
                .await
                .map_err(|e| e.message);
            data.set(Some(r));
        });
    };
    // Refresh when the group or one of its attempts changes (live events).
    let version = Memo::new(move |_| {
        store.tasks.with(|ts| {
            ts.iter()
                .filter(|t| t.id == group || t.attempt_of == Some(group))
                .map(|t| format!("{}:{}", t.id, t.status))
                .collect::<Vec<_>>()
                .join(",")
        })
    });
    Effect::new(move |_| {
        version.track();
        load();
    });
    let select = move |a: AttemptView| {
        store.ask(Confirm {
            title: format!("Versuch {} übernehmen?", a.attempt_no),
            body: "Dieser Versuch geht in die Übernahme (bzw. als Pull Request raus, je nach Projekt). Alle anderen Versuche werden verworfen: laufende werden gestoppt, ihre Arbeitsbereiche und Branches gelöscht.".into(),
            confirm_label: "Übernehmen".into(),
            danger: false,
            on_confirm: Callback::new(move |_| {
                busy.set(true);
                spawn_local(async move {
                    let body = SelectAttempt { attempt_id: a.id };
                    match api::post_ok(&format!("/tasks/{group}/select"), &body).await {
                        Ok(()) => {
                            store.toast(format!("Versuch {} wird übernommen", a.attempt_no));
                            load();
                        }
                        Err(e) => store.toast_error("Übernehmen fehlgeschlagen", &e),
                    }
                    busy.set(false);
                });
            }),
        });
    };
    view! {
        <section class="block" data-testid="comparison" aria-labelledby="cmp-title">
            <h3 id="cmp-title" class="row">"Versuche vergleichen" <HelpTip topic="Best-of-N" text=HELP_BON align="align-start"/></h3>
            {move || match data.get() {
                None => view! { <p class="small muted" role="status">"Lade Versuche …"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="alert tone-warning">{e}</p> }.into_any(),
                Some(Ok(c)) => {
                    let status = comparison_status(&c);
                    let can = c.can_select;
                    let total = cost_label(&c.total);
                    let rows = c.attempts.clone();
                    let selector = c.selector.clone().map(|s| (s, c.selector_reason.clone().unwrap_or_default()));
                    let err = c.select_error.clone();
                    let open = open.clone();
                    view! {
                        <p class="box" data-testid="comparison-status" aria-live="polite">{status}</p>
                        {selector.map(|(s, why)| view! {
                            <p class="small muted">{format!("Prüf-Agent für die automatische Auswahl: {s}")}{(!why.is_empty()).then(|| format!(" – {why}"))}</p>
                        })}
                        {err.map(|e| view! { <p class="alert tone-warning" role="note">{format!("Automatische Auswahl nicht möglich: {e}")}</p> })}
                        <div class="attempt-grid">
                            {c.attempts.into_iter().map(|a| {
                                let open = open.clone();
                                let (aid, no) = (a.id, a.attempt_no);
                                let selectable = can && a.status == "candidate";
                                let a2 = a.clone();
                                let label = attempt_status_label(&a.status, a.selected);
                                let tone = match a.status.as_str() {
                                    "candidate" => "tone-warning",
                                    "done" | "merging" if a.selected => "tone-success",
                                    "failed" | "blocked" => "tone-danger",
                                    "running" | "queued" => "tone-run",
                                    _ => "tone-neutral",
                                };
                                view! {
                                    <article class="attempt-card" class:selected=a.selected data-testid=format!("attempt-{no}") aria-labelledby=format!("att-{no}")>
                                        <header class="row wrap">
                                            <h4 id=format!("att-{no}")>{format!("Versuch {no}")}</h4>
                                            <span class=format!("pill {tone}")><span class="dot" aria-hidden="true"></span>{label}</span>
                                        </header>
                                        <p class="small muted truncate">{format!("{}{}", a.access, a.model.as_ref().map(|m| format!(" · {m}")).unwrap_or_default())}</p>
                                        <dl class="kv">
                                            <dt>"Änderungen"</dt><dd>{changes_label(&a)}</dd>
                                            <dt>"Prüfkommando"</dt><dd>{verify_label(a.verify)}</dd>
                                            <dt>"Review"</dt><dd>{review_label(a.review.as_ref())}</dd>
                                            <dt>"Kosten"</dt><dd>{cost_label(&a.cost)}</dd>
                                            <dt>"Dauer"</dt><dd>{secs_label(a.duration_secs)}</dd>
                                        </dl>
                                        {a.summary.clone().filter(|s| !s.trim().is_empty()).map(|s| view! { <p class="small clamp-3">{s}</p> })}
                                        {a.error.clone().filter(|_| a.status != "candidate").map(|e| view! { <p class="small tone-danger-text clamp-3">{e}</p> })}
                                        {(!a.screenshots.is_empty()).then(|| view! { <Screenshots shots=a.screenshots.clone() compact=true/> })}
                                        <div class="row wrap attempt-actions">
                                            <button class="btn sm" type="button" on:click=move |_| open(aid)>
                                                <Icon icon=Ic::Eye class="sm"/>"Details & Änderungen"
                                            </button>
                                            {selectable.then(|| view! {
                                                <button class="btn primary sm" type="button" disabled=move || busy.get() on:click=move |_| select(a2.clone())>
                                                    <Icon icon=Ic::Check class="sm"/>"Diesen übernehmen"
                                                </button>
                                            })}
                                        </div>
                                    </article>
                                }
                            }).collect_view()}
                        </div>
                        <details class="block">
                            <summary><Icon icon=Ic::ChevronRight class="sm chev"/>"Als Tabelle"</summary>
                            <div class="table-wrap" tabindex="0" role="region" aria-label="Vergleich der Versuche als Tabelle">
                                <table class="table compact">
                                    <caption class="visually-hidden">"Vergleich der Versuche"</caption>
                                    <thead><tr>
                                        <th scope="col">"Versuch"</th><th scope="col">"Status"</th><th scope="col">"Modell"</th>
                                        <th scope="col">"Änderungen"</th><th scope="col">"Prüfkommando"</th><th scope="col">"Review"</th>
                                        <th scope="col">"Kosten"</th><th scope="col">"Dauer"</th>
                                    </tr></thead>
                                    <tbody>
                                        {rows.iter().map(|a| view! {
                                            <tr>
                                                <th scope="row">{a.attempt_no}</th>
                                                <td>{attempt_status_label(&a.status, a.selected)}</td>
                                                <td>{a.model.clone().unwrap_or_else(|| a.access.clone())}</td>
                                                <td>{changes_label(a)}</td>
                                                <td>{verify_label(a.verify)}</td>
                                                <td>{review_label(a.review.as_ref())}</td>
                                                <td>{cost_label(&a.cost)}</td>
                                                <td>{secs_label(a.duration_secs)}</td>
                                            </tr>
                                        }).collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        </details>
                        <p class="small muted" data-testid="comparison-total">{format!("Kosten aller Versuche: {total}")}</p>
                    }.into_any()
                }
            }}
        </section>
    }
}

/// An attempt points back to its comparison.
#[component]
pub fn AttemptOf(t: Task) -> impl IntoView {
    let open = use_open_task();
    let group = t.attempt_of;
    let no = t.attempt_no.unwrap_or(0);
    group.map(|g| {
        view! {
            <section class="block">
                <p class="row wrap">
                    <span class="tag">{format!("Versuch {no}")}</span>
                    "Teil eines Vergleichs mehrerer Versuche."
                    <button class="btn sm" type="button" on:click=move |_| open(g)>
                        <Icon icon=Ic::Layers class="sm"/>"Zum Vergleich"
                    </button>
                </p>
            </section>
        }
    })
}

/// "Warum dieses Modell?"
#[component]
pub fn DecisionLine(t: Task) -> impl IntoView {
    t.routing_decision_parsed().map(|d| {
        view! {
            <section class="block" data-testid="routing-decision">
                <h3 class="row"><Icon icon=Ic::Route class="sm"/>"Warum dieses Modell?"</h3>
                <p class="box small">{decision_text(&d)}</p>
            </section>
        }
    })
}

/// Screenshots (desktop and mobile), opened full size in a new tab.
#[component]
pub fn Screenshots(shots: Vec<ScreenshotView>, #[prop(optional)] compact: bool) -> impl IntoView {
    view! {
        <div class=if compact { "shots compact" } else { "shots" } data-testid="screenshots">
            {shots.into_iter().map(|s| {
                let alt = format!("Screenshot {} von {}", s.viewport.label(), s.path);
                let open_label = format!("{alt} in voller Größe öffnen");
                let cls = match s.viewport { Viewport::Desktop => "shot desktop", Viewport::Mobile => "shot mobile" };
                view! {
                    <figure class=cls>
                        <a href=s.url.clone() target="_blank" rel="noopener noreferrer" aria-label=open_label>
                            <img src=s.url.clone() alt=alt loading="lazy" width=s.width height=s.height/>
                        </a>
                        <figcaption class="xs muted">{format!("{} · {}", s.viewport.label(), s.path)}</figcaption>
                    </figure>
                }
            }).collect_view()}
        </div>
    }
}

/// Live preview of a task whose working tree exists (sandboxed frame on an
/// own origin; members only, ends by itself).
#[component]
pub fn PreviewBlock(t: Task) -> impl IntoView {
    let store = use_store();
    let id = t.id;
    let pid = t.project_id;
    let possible = matches!(t.status.as_str(), "candidate" | "held" | "waiting_input");
    let can_edit = store
        .projects
        .with_untracked(|ps| ps.iter().any(|p| p.id == pid && p.role.can_edit()));
    let cfg = RwSignal::new(None::<PreviewConfig>);
    let current = RwSignal::new(None::<PreviewView>);
    let busy = RwSignal::new(false);
    spawn_local(async move {
        if let Ok(c) = api::get::<PreviewConfig>(&format!("/projects/{pid}/preview")).await {
            cfg.set(Some(c));
        }
        if let Ok(v) = api::get::<Option<PreviewView>>(&format!("/tasks/{id}/preview")).await {
            current.set(v);
        }
    });
    let start = move |_| {
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, PreviewView>(
                &format!("/tasks/{id}/preview"),
                &serde_json::json!({}),
            )
            .await
            {
                Ok(v) => {
                    if v.status == PreviewStatus::Failed {
                        store.toast_error_text(
                            "Vorschau nicht gestartet",
                            v.error.as_deref().unwrap_or("unbekannt"),
                        );
                    }
                    current.set(Some(v));
                }
                Err(e) => store.toast_error("Vorschau nicht gestartet", &e),
            }
            busy.set(false);
        });
    };
    let stop = move |_| {
        spawn_local(async move {
            match api::delete(&format!("/tasks/{id}/preview")).await {
                Ok(()) => current.set(None),
                Err(e) => store.toast_error("Vorschau nicht beendet", &e),
            }
        });
    };
    view! {
        {move || cfg.get().filter(|c| c.settings.enabled && (possible || current.with(Option::is_some))).map(|c| view! {
            <section class="block" data-testid="preview" aria-labelledby="preview-title">
                <h3 id="preview-title" class="row">"Vorschau" <HelpTip topic="Vorschau" text=HELP_PREVIEW align="align-start"/></h3>
                {(!c.live_available).then(|| view! { <p class="small muted">"Live-Vorschau ist auf diesem Server nicht eingerichtet. Screenshots erscheinen trotzdem nach UI-Änderungen."</p> })}
                {move || match current.get().filter(|v| v.status == PreviewStatus::Ready) {
                    Some(v) => {
                        let url = v.url.clone().unwrap_or_default();
                        let until = fmt_time(v.expires_at, store.now.get_untracked(), &tz());
                        view! {
                            <div class="row wrap">
                                <a class="btn sm" href=url.clone() target="_blank" rel="noopener noreferrer"><Icon icon=Ic::ExternalLink class="sm"/>"In neuem Tab öffnen"</a>
                                {can_edit.then(|| view! { <button class="btn danger sm" type="button" on:click=stop><Icon icon=Ic::X class="sm"/>"Vorschau beenden"</button> })}
                                <span class="small muted">{format!("läuft bis {until}")}</span>
                            </div>
                            <iframe class="preview-frame" src=url title="Vorschau der App dieses Auftrags" sandbox="allow-scripts allow-forms" referrerpolicy="no-referrer"></iframe>
                        }.into_any()
                    }
                    None => {
                        let err = current.get().and_then(|v| v.error);
                        view! {
                            {err.map(|e| view! { <p class="box pre small tone-danger-text">{e}</p> })}
                            {(c.live_available && can_edit && possible).then(|| view! {
                                <button class="btn" type="button" disabled=move || busy.get() on:click=start>
                                    <Icon icon=Ic::Play class="sm"/>{move || if busy.get() { "Startet …" } else { "Vorschau starten" }}
                                </button>
                            })}
                        }.into_any()
                    }
                }}
            </section>
        })}
    }
}

const HELP_RULES: &str = "Regeln wählen das Modell der Umsetzung nach Merkmalen des Auftrags – z. B. ein günstiges Modell für kleine Aufträge. Die erste passende Regel gilt; passt keine, gilt die Modellwahl oben. Am Auftrag steht später, welche Regel gegriffen hat.";

/// WP12: routing rules on top of the project's model choice (preset or
/// hand-made). Saved together with the current routing.
#[component]
pub fn RoutingRulesCard(p: queued_api::ProjectView, on_saved: Callback<()>) -> impl IntoView {
    use queued_api::{RoutingRule, RoutingUpdate, RuleWhen, Target};
    let store = use_store();
    let id = p.id;
    let can_edit = p.role.can_edit();
    let residency = p.data_residency;
    let routing = p.routing.clone();
    let rules = RwSignal::new(
        routing
            .as_ref()
            .map(|r| r.rules.clone())
            .unwrap_or_default(),
    );
    let (name, max_chars, keywords, access, model, fixes) = (
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(false),
    );
    let busy = RwSignal::new(false);
    let routing_sv = StoredValue::new(routing.clone());
    let save = move |list: Vec<RoutingRule>| {
        let Some(mut r) = routing_sv.get_value() else {
            return;
        };
        r.rules = list;
        busy.set(true);
        spawn_local(async move {
            let body = RoutingUpdate {
                routing: Some(r),
                data_residency: residency,
            };
            match api::put::<_, queued_api::ProjectView>(&format!("/projects/{id}/routing"), &body)
                .await
            {
                Ok(v) => {
                    let saved = v
                        .routing
                        .as_ref()
                        .map(|r| r.rules.clone())
                        .unwrap_or_default();
                    routing_sv.set_value(v.routing.clone());
                    rules.set(saved);
                    store.toast("Regeln gespeichert");
                    on_saved.run(());
                }
                Err(e) => store.toast_error("Regeln nicht gespeichert", &e),
            }
            busy.set(false);
        });
    };
    let add = move |_| {
        let (c, pr) = crate::logic::costs::parse_access(&access.get_untracked());
        let kw: Vec<String> = keywords
            .get_untracked()
            .split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        let rule = RoutingRule {
            name: name.get_untracked().trim().to_string(),
            when: RuleWhen {
                kinds: if fixes.get_untracked() {
                    vec!["fix".into()]
                } else {
                    vec![]
                },
                max_chars: max_chars.get_untracked().trim().parse().ok(),
                min_chars: None,
                keywords: kw,
                has_attachments: None,
            },
            target: Target {
                connector_id: c,
                provider: pr,
                model: Some(model.get_untracked().trim().to_string()).filter(|m| !m.is_empty()),
            },
        };
        let mut list = rules.get_untracked();
        list.push(rule);
        save(list);
        name.set(String::new());
        max_chars.set(String::new());
        keywords.set(String::new());
        model.set(String::new());
    };
    let legacy = Memo::new(move |_| {
        store.meta.with(|m| {
            m.providers
                .iter()
                .filter(|p| m.credential_kinds.get(*p).is_some_and(|k| !k.is_empty()))
                .cloned()
                .collect::<Vec<_>>()
        })
    });
    let ids = (
        crate::ui::uid("rule-name"),
        crate::ui::uid("rule-max"),
        crate::ui::uid("rule-kw"),
        crate::ui::uid("rule-access"),
        crate::ui::uid("rule-model"),
    );
    view! {
        <section class="card settings-card" style="margin-top: var(--sp-4)" aria-labelledby="rules-title" data-testid="routing-rules">
            <header>
                <span class="s-icon"><Icon icon=Ic::Route/></span>
                <div>
                    <h2 id="rules-title" class="row">"Regeln für die Modellwahl" <HelpTip topic="Regeln" text=HELP_RULES align="align-start"/></h2>
                    <p>"Zum Beispiel: kleine Aufträge mit einem günstigen Modell. Nach einem Fehlschlag eskaliert der Fix-Auftrag wie in der Modellwahl festgelegt."</p>
                </div>
            </header>
            {match routing {
                None => view! { <p class="small muted">"Regeln ergänzen eine Modellwahl – bitte oben zuerst eine Vorlage übernehmen."</p> }.into_any(),
                Some(_) => view! {
                    {move || if rules.with(Vec::is_empty) {
                        view! { <p class="small muted" data-testid="rules-empty">"Noch keine Regeln – es gilt immer die Modellwahl oben."</p> }.into_any()
                    } else {
                        let cs = store.connectors.get();
                        view! {
                            <ol class="rules">
                                {rules.get().into_iter().enumerate().map(|(i, r)| {
                                    let target = crate::logic::costs::target_text(Some(&r.target), &cs, crate::logic::labels::provider_label, "Standard");
                                    let cond = rule_condition(&r.when);
                                    view! {
                                        <li class="row wrap">
                                            <strong>{r.name.clone()}</strong>
                                            <span class="small">{format!("{cond} → {target}")}</span>
                                            <span class="grow"></span>
                                            {can_edit.then(|| view! {
                                                <button class="btn ghost sm" type="button" aria-label=format!("Regel „{}“ löschen", r.name) disabled=move || busy.get()
                                                    on:click=move |_| { let mut l = rules.get_untracked(); if i < l.len() { l.remove(i); } save(l); }>
                                                    <Icon icon=Ic::Trash class="sm"/>
                                                </button>
                                            })}
                                        </li>
                                    }
                                }).collect_view()}
                            </ol>
                        }.into_any()
                    }}
                    {can_edit.then(|| view! {
                        <div class="form-section">
                            <h3>"Neue Regel"</h3>
                            <div class="grid-3">
                                <div class="field"><label class="label" for=ids.0.clone()>"Name"</label>
                                    <input id=ids.0.clone() class="input" placeholder="Kleine Aufträge" bind:value=name/></div>
                                <div class="field"><label class="label" for=ids.1.clone()>"Höchstens Zeichen"</label>
                                    <input id=ids.1.clone() class="input" inputmode="numeric" placeholder="z. B. 400" bind:value=max_chars/></div>
                                <div class="field"><label class="label" for=ids.2.clone()>"Stichworte (Komma)"</label>
                                    <input id=ids.2.clone() class="input" placeholder="README, Doku" bind:value=keywords/></div>
                                <div class="field"><label class="label" for=ids.3.clone()>"Zugang"</label>
                                    <select id=ids.3.clone() class="select" prop:value=move || access.get() on:change=move |ev| access.set(event_target_value(&ev))>
                                        <option value="">"– wählen –"</option>
                                        {move || legacy.get().into_iter().map(|p| view! { <option value=format!("p:{p}")>{crate::logic::labels::provider_label(&p)}</option> }).collect_view()}
                                        {move || store.connectors.get().into_iter().map(|c| view! { <option value=format!("c:{}", c.id)>{crate::logic::costs::connector_label(&c)}</option> }).collect_view()}
                                    </select></div>
                                <div class="field"><label class="label" for=ids.4.clone()>"Modell"</label>
                                    <input id=ids.4.clone() class="input" placeholder="claude-haiku-5-5" bind:value=model/></div>
                            </div>
                            <label class="check">
                                <input type="checkbox" prop:checked=move || fixes.get() on:change=move |ev| fixes.set(event_target_checked(&ev))/>
                                <span>"Nur für Fix-Aufträge (wenn keine Eskalation greift)"</span>
                            </label>
                            <button class="btn" type="button" data-testid="rule-add"
                                disabled=move || busy.get() || name.with(|n| n.trim().is_empty()) || access.with(String::is_empty)
                                on:click=add>
                                <Icon icon=Ic::Plus class="sm"/>"Regel hinzufügen"
                            </button>
                        </div>
                    })}
                }.into_any(),
            }}
        </section>
    }
}

fn rule_condition(w: &queued_api::RuleWhen) -> String {
    let mut p = vec![];
    if w.kinds.iter().any(|k| k == "fix") {
        p.push("Fix-Aufträge".to_string());
    }
    if let Some(m) = w.max_chars {
        p.push(format!("≤ {m} Zeichen"));
    }
    if let Some(m) = w.min_chars {
        p.push(format!("≥ {m} Zeichen"));
    }
    if !w.keywords.is_empty() {
        p.push(format!("„{}“", w.keywords.join("“, „")));
    }
    if p.is_empty() {
        "alle neuen Aufträge".into()
    } else {
        p.join(", ")
    }
}

const HELP_PREVIEW_SETTINGS: &str = "Queue kann die App eines Auftrags starten (Dev-Server) – ohne Netzwerk, in derselben abgeschotteten Umgebung wie der Agent. Nach Änderungen an Oberflächen-Dateien entstehen automatisch Screenshots für Desktop und Handy. Abhängigkeiten installiert die Einrichtung (Umgebung), nicht das Startkommando.";

/// WP12: preview settings of a project.
#[component]
pub fn ProjectPreviewCard(p: queued_api::ProjectView) -> impl IntoView {
    use queued_api::preview::PreviewSettings;
    let store = use_store();
    let id = p.id;
    let can_edit = p.role.can_edit();
    let cfg = RwSignal::new(None::<PreviewConfig>);
    let (enabled, command, port, ready, timeout, lifetime, shots, paths, globs) = (
        RwSignal::new(false),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
        RwSignal::new(true),
        RwSignal::new(String::new()),
        RwSignal::new(String::new()),
    );
    let fill = move |s: &PreviewSettings| {
        enabled.set(s.enabled);
        command.set(s.command.clone());
        port.set(s.port.to_string());
        ready.set(s.ready_path.clone());
        timeout.set(s.ready_timeout_secs.to_string());
        lifetime.set((s.max_lifetime_secs / 60).to_string());
        shots.set(s.screenshots);
        paths.set(s.screenshot_paths.join(", "));
        globs.set(s.ui_globs.join(", "));
    };
    spawn_local(async move {
        if let Ok(c) = api::get::<PreviewConfig>(&format!("/projects/{id}/preview")).await {
            fill(&c.settings);
            cfg.set(Some(c));
        }
    });
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let save = move |_| {
        let list = |s: String| -> Vec<String> {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        };
        let s = PreviewSettings {
            enabled: enabled.get_untracked(),
            command: command.get_untracked(),
            port: port.get_untracked().trim().parse().unwrap_or(0),
            ready_path: ready.get_untracked().trim().to_string(),
            ready_timeout_secs: timeout.get_untracked().trim().parse().unwrap_or(0),
            max_lifetime_secs: lifetime
                .get_untracked()
                .trim()
                .parse::<u32>()
                .unwrap_or(0)
                .saturating_mul(60),
            screenshots: shots.get_untracked(),
            screenshot_paths: list(paths.get_untracked()),
            ui_globs: list(globs.get_untracked()),
        };
        if let Err(e) = s.validate() {
            error.set(Some(e));
            return;
        }
        error.set(None);
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, PreviewConfig>(&format!("/projects/{id}/preview"), &s).await {
                Ok(c) => {
                    fill(&c.settings);
                    cfg.set(Some(c));
                    store.toast("Vorschau-Einstellungen gespeichert");
                }
                Err(e) => error.set(Some(e.message)),
            }
            busy.set(false);
        });
    };
    let f = |label: &'static str,
             hint: &'static str,
             sig: RwSignal<String>,
             mode: &'static str,
             test: &'static str| {
        let fid = crate::ui::uid("pv");
        let hid = format!("{fid}-hint");
        view! {
            <div class="field">
                <label class="label" for=fid.clone()>{label}</label>
                <input id=fid class="input" inputmode=mode data-testid=test aria-describedby=hid.clone() bind:value=sig/>
                <span class="hint" id=hid>{hint}</span>
            </div>
        }
    };
    view! {
        <section class="card settings-card" style="margin-top: var(--sp-4)" aria-labelledby="pv-title" data-testid="project-preview">
            <header>
                <span class="s-icon"><Icon icon=Ic::Eye/></span>
                <div>
                    <h2 id="pv-title" class="row">"Vorschau & Screenshots" <HelpTip topic="Vorschau" text=HELP_PREVIEW_SETTINGS align="align-start"/></h2>
                    <p>"Die App eines Auftrags ansehen, bevor sie übernommen wird."</p>
                </div>
            </header>
            {move || cfg.get().map(|c| view! {
                {(!c.live_available).then(|| view! { <p class="small muted">"Hinweis: Live-Vorschau ist auf diesem Server nicht eingerichtet (eigene Vorschau-Domain fehlt). Screenshots funktionieren."</p> })}
                <fieldset class="form-section" disabled=!can_edit>
                    <label class="check">
                        <input type="checkbox" data-testid="pv-enabled" prop:checked=move || enabled.get() on:change=move |ev| enabled.set(event_target_checked(&ev))/>
                        <span>"Vorschau einschalten"</span>
                    </label>
                    {f("Startkommando", "Startet den Dev-Server, z. B. npm run dev -- --port $PORT", command, "text", "pv-command")}
                    <div class="grid-3">
                        {f("Port", "Auf diesem Port lauscht der Server ($PORT).", port, "numeric", "pv-port")}
                        {f("Bereit-Pfad", "Antwortet, sobald der Server läuft.", ready, "url", "pv-ready")}
                        {f("Startzeit (Sekunden)", "Höchstens so lange wird gewartet.", timeout, "numeric", "pv-timeout")}
                        {f("Laufzeit (Minuten)", "Danach wird die Vorschau beendet.", lifetime, "numeric", "pv-lifetime")}
                    </div>
                    <label class="check">
                        <input type="checkbox" data-testid="pv-shots" prop:checked=move || shots.get() on:change=move |ev| shots.set(event_target_checked(&ev))/>
                        <span>"Screenshots nach Änderungen an Oberflächen-Dateien"</span>
                    </label>
                    <div class="grid-2">
                        {f("Seiten für Screenshots (Komma)", "z. B. /, /login", paths, "text", "pv-paths")}
                        {f("Oberflächen-Dateien (Muster, Komma)", "z. B. **/*.css, **/*.tsx", globs, "text", "pv-globs")}
                    </div>
                    {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert">{e}</p> })}
                    {can_edit.then(|| view! {
                        <button class="btn primary" type="button" data-testid="pv-save" disabled=move || busy.get() on:click=save>"Speichern"</button>
                    })}
                </fieldset>
            })}
        </section>
    }
}
