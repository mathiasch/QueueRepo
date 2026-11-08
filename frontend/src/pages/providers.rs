//! WP13 settings: additional accesses (connectors), costs, price table,
//! CLI status and the model test (eval set).
use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::{
    CliStatus, ConnectorCheck, ConnectorTypeInfo, ConnectorView, CostSummary, EvalCaseInfo,
    EvalRunView, NewConnector, PriceCatalog, PriceOverride, PriceOverrideVersion, ProjectRole,
    StartEval,
};

use crate::api;
use crate::logic::costs::{
    connector_label, fmt_money, fmt_money_list, group_label, parse_access, step_label, type_label,
    GROUPS,
};
use crate::logic::format::{fmt_relative, fmt_tokens};
use crate::logic::labels::{help, provider_label};
use crate::store::{tz, use_store, Store};
use crate::ui::dialog::Confirm;
use crate::ui::{Field, FieldIds, HelpTip, Ic, Icon};

use super::settings::Card;

// ---------------------------------------------------------------------------
// Connectors
// ---------------------------------------------------------------------------

#[component]
pub fn ConnectorsCard() -> impl IntoView {
    let store = use_store();
    let adding = RwSignal::new(false);
    view! {
        <Card id="connectors" icon=Ic::Zap title="Weitere Zugänge" text=help::CONNECTORS>
            <p class="small muted">"Für Abos reichen die Zugänge oben. Hier kommen API-Schlüssel (mit Batch-Rabatt), Claude über AWS Bedrock oder Google Vertex in der EU, Azure OpenAI, Mistral, Infomaniak (Schweiz), eigene Server, OpenRouter und LiteLLM dazu."</p>
            <ul class="connector-list" aria-label="Weitere Zugänge" data-testid="connector-list">
                {move || {
                    let list = store.connectors.get();
                    if list.is_empty() {
                        view! { <li class="small muted">"Noch keine weiteren Zugänge."</li> }.into_any()
                    } else {
                        list.into_iter().map(|c| view! { <ConnectorRow c=c/> }).collect_view().into_any()
                    }
                }}
            </ul>
            {move || if adding.get() {
                view! { <NewConnectorForm on_done=Callback::new(move |_| adding.set(false))/> }.into_any()
            } else {
                view! {
                    <div><button class="btn" type="button" on:click=move |_| adding.set(true)><Icon icon=Ic::Plus class="sm"/>"Zugang hinzufügen"</button></div>
                }.into_any()
            }}
        </Card>
    }
}

#[component]
fn ConnectorRow(c: ConnectorView) -> impl IntoView {
    let store = use_store();
    let types = store.meta.with_untracked(|m| m.connector_types.clone());
    let id = c.id;
    let name = c.label.clone();
    let paused = c.paused_reason.clone();
    let limited = c.limited_until;
    let check = move |_| {
        spawn_local(async move {
            match api::post::<_, ConnectorCheck>(
                &format!("/connectors/{id}/check"),
                &serde_json::json!({}),
            )
            .await
            {
                Ok(r) if r.ok => store.toast(r.message),
                Ok(r) => store.toast_error(
                    "Prüfung fehlgeschlagen",
                    &api::ApiError {
                        status: 0,
                        message: r.message,
                    },
                ),
                Err(e) => store.toast_error("Prüfung fehlgeschlagen", &e),
            }
        });
    };
    let resume = move |_| {
        store.run("Pause nicht aufgehoben", async move {
            api::action(&format!("/connectors/{id}/resume")).await?;
            store.reload_connectors().await;
            Ok(())
        })
    };
    let remove = {
        let name = name.clone();
        move |_| confirm_remove(store, id, name.clone())
    };
    view! {
        <li class="sub-card" data-testid=format!("connector-{}", c.label)>
            <div class="row wrap">
                <div class="grow">
                    <h3>{c.label.clone()}</h3>
                    <p class="small muted">{type_label(&types, &c.kind)}{c.project_id.is_some().then_some(" · nur für ein Projekt")}</p>
                </div>
                <span class="tag" title="Wo Prompts und Code verarbeitet werden">
                    {format!("Daten: {}{}", c.location.label(), if c.location_declared { " (selbst angegeben)" } else { "" })}
                </span>
                {c.batch.then(|| view! { <span class="tag tone-success">"Batch −50 %"</span> })}
                {paused.clone().map(|r| view! { <span class="tag tone-danger" title=r>"pausiert"</span> })}
                {limited.map(|t| view! { <span class="tag tone-warning">{format!("Limit bis {}", fmt_relative(t, store.now.get_untracked(), &tz()))}</span> })}
            </div>
            <div class="row wrap">
                {c.single_shot.then(|| view! { <button class="btn sm" type="button" on:click=check>"Verbindung prüfen"</button> })}
                {(paused.is_some() || limited.is_some()).then(|| view! { <button class="btn sm" type="button" on:click=resume>"Pause aufheben"</button> })}
                {c.can_manage.then(|| view! {
                    <button class="btn icon-only danger" type="button" aria-label=format!("Zugang {name} entfernen") title="Entfernen" on:click=remove>
                        <Icon icon=Ic::Trash/>
                    </button>
                })}
            </div>
        </li>
    }
}

fn confirm_remove(store: Store, id: uuid::Uuid, name: String) {
    store.ask(Confirm {
        title: format!("Zugang „{name}“ entfernen?"),
        body: "Der Schlüssel wird gelöscht. Offene Aufträge oder eine Modellwahl, die den Zugang nutzen, verhindern das Entfernen.".into(),
        confirm_label: "Entfernen".into(),
        danger: true,
        on_confirm: Callback::new(move |_| {
            store.run("Entfernen fehlgeschlagen", async move {
                api::delete(&format!("/connectors/{id}")).await?;
                store.reload_connectors().await;
                store.toast("Zugang entfernt");
                Ok(())
            })
        }),
    });
}

#[component]
fn NewConnectorForm(on_done: Callback<()>) -> impl IntoView {
    let store = use_store();
    let types = store.meta.with_untracked(|m| m.connector_types.clone());
    let kind = RwSignal::new(types.first().map(|t| t.kind.clone()).unwrap_or_default());
    let label = RwSignal::new(String::new());
    let values = RwSignal::new(HashMap::<String, String>::new());
    let secret = RwSignal::new(String::new());
    let scope = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let types_c = types.clone();
    let current = Memo::new(move |_| {
        let k = kind.get();
        types_c.iter().find(|t| t.kind == k).cloned()
    });
    // Defaults of the chosen type.
    Effect::new(move |_| {
        if let Some(t) = current.get() {
            values.set(
                t.fields
                    .iter()
                    .filter_map(|f| f.default.clone().map(|d| (f.key.clone(), d)))
                    .collect(),
            );
            if label.get_untracked().is_empty()
                || types.iter().any(|x| x.label == label.get_untracked())
            {
                label.set(t.label.clone());
            }
        }
    });
    let owned = Memo::new(move |_| {
        store.projects.with(|ps| {
            ps.iter()
                .filter(|p| p.role == ProjectRole::Owner)
                .map(|p| (p.id, p.name.clone()))
                .collect::<Vec<_>>()
        })
    });
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(t) = current.get_untracked() else {
            return;
        };
        let config: serde_json::Map<String, serde_json::Value> = values
            .get_untracked()
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        let body = NewConnector {
            kind: t.kind.clone(),
            label: label.get_untracked(),
            project_id: uuid::Uuid::parse_str(&scope.get_untracked()).ok(),
            config: serde_json::Value::Object(config),
            secret: Some(secret.get_untracked()).filter(|s| !s.trim().is_empty()),
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, ConnectorView>("/connectors", &body).await {
                Ok(c) => {
                    secret.set(String::new());
                    store.reload_connectors().await;
                    store.toast(format!("Zugang „{}“ gespeichert", c.label));
                    on_done.run(());
                }
                Err(e) => error.set(Some(e.message)),
            }
            busy.set(false);
        });
    };
    let type_ids = FieldIds::new("conn-type");
    let label_ids = FieldIds::new("conn-label");
    let scope_ids = FieldIds::new("conn-scope");
    let secret_ids = FieldIds::new("conn-secret");
    let all_types = store.meta.with_untracked(|m| m.connector_types.clone());
    view! {
        <form class="sub-card stack" on:submit=save data-testid="connector-form" aria-label="Neuer Zugang">
            <h3>"Neuer Zugang"</h3>
            <Field ids=type_ids.clone() label="Art des Zugangs">
                <select id=type_ids.id.clone() class="select" prop:value=move || kind.get() on:change=move |ev| { error.set(None); kind.set(event_target_value(&ev)); }>
                    {GROUPS.iter().map(|g| {
                        let in_group: Vec<ConnectorTypeInfo> = all_types.iter().filter(|t| t.group == *g).cloned().collect();
                        view! {
                            <optgroup label=group_label(g)>
                                {in_group.into_iter().map(|t| view! { <option value=t.kind.clone()>{t.label.clone()}</option> }).collect_view()}
                            </optgroup>
                        }
                    }).collect_view()}
                </select>
            </Field>
            {move || current.get().map(|t| view! {
                <div class="stack">
                    <p class="small">{t.description.clone()}</p>
                    <p class="small"><span class="tag">{format!("Daten: {}", t.location.label())}</span>
                        {t.batch.then(|| view! { " " <span class="tag tone-success">"Batch −50 %"</span> })}
                        {t.flex.then(|| view! { " " <span class="tag tone-success">"Flex −50 %"</span> })}</p>
                    {(!t.note.is_empty()).then(|| view! { <p class="alert tone-info small"><Icon icon=Ic::Info class="sm"/>{t.note.clone()}</p> })}
                    {(!t.unverified.is_empty()).then(|| view! {
                        <details class="small">
                            <summary>"Nicht verifiziert"</summary>
                            <ul>{t.unverified.iter().map(|u| view! { <li>{u.clone()}</li> }).collect_view()}</ul>
                        </details>
                    })}
                </div>
            })}
            <div class="grid-2">
                <Field ids=label_ids.clone() label="Bezeichnung" hint="Wie der Zugang in Listen heißt.">
                    <input id=label_ids.id.clone() class="input" maxlength="100" bind:value=label/>
                </Field>
                <Field ids=scope_ids.clone() label="Gilt für" hint="Projektzugänge dürfen alle Bearbeiter des Projekts nutzen.">
                    <select id=scope_ids.id.clone() class="select" prop:value=move || scope.get() on:change=move |ev| scope.set(event_target_value(&ev))>
                        <option value="">"Alle meine Aufträge"</option>
                        {move || owned.get().into_iter().map(|(id, n)| view! { <option value=id.to_string()>{format!("Nur Projekt „{n}“")}</option> }).collect_view()}
                    </select>
                </Field>
            </div>
            {move || current.get().map(|t| view! {
                <div class="grid-2">
                    {t.fields.iter().map(|f| {
                        let ids = FieldIds::new("conn-field");
                        let key = f.key.clone();
                        let key2 = key.clone();
                        let get = move || values.with(|v| v.get(&key).cloned().unwrap_or_default());
                        let set = move |v: String| values.update(|m| { m.insert(key2.clone(), v); });
                        let hint = if f.required { f.help.clone() } else { format!("Optional. {}", f.help) };
                        let input = if f.input == "select" {
                            let options = f.options.clone();
                            view! {
                                <select id=ids.id.clone() class="select" prop:value=get on:change=move |ev| set(event_target_value(&ev))>
                                    {options.into_iter().map(|o| view! { <option value=o.value>{o.label}</option> }).collect_view()}
                                </select>
                            }.into_any()
                        } else {
                            view! {
                                <input id=ids.id.clone() class="input" type=if f.input == "url" { "url" } else { "text" } autocomplete="off"
                                    required=f.required prop:value=get on:input=move |ev| set(event_target_value(&ev))/>
                            }.into_any()
                        };
                        view! { <Field ids=ids label=f.label.clone() hint=hint>{input}</Field> }
                    }).collect_view()}
                </div>
            })}
            {move || current.get().and_then(|t| t.secret_label.clone().map(|sl| {
                let ids = secret_ids.clone();
                let hint = if t.secret_optional { format!("Optional. {}", t.secret_help) } else { t.secret_help.clone() };
                view! {
                    <Field ids=ids.clone() label=sl hint=hint>
                        <textarea id=ids.id.clone() class="textarea mono-input" rows=if t.secret_json { "5" } else { "2" }
                            autocomplete="off" spellcheck="false" bind:value=secret></textarea>
                    </Field>
                }
            }))}
            {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert">{e}</p> })}
            <div class="form-actions">
                <button class="btn primary sm" type="submit" disabled=move || busy.get() || label.with(|l| l.trim().is_empty())>
                    <Icon icon=Ic::Check class="sm"/>"Zugang speichern"
                </button>
                <button class="btn ghost sm" type="button" on:click=move |_| on_done.run(())>"Abbrechen"</button>
            </div>
        </form>
    }
}

// ---------------------------------------------------------------------------
// Costs
// ---------------------------------------------------------------------------

#[component]
pub fn CostsCard() -> impl IntoView {
    let data = RwSignal::new(None::<Result<CostSummary, String>>);
    spawn_local(async move {
        data.set(Some(
            api::get::<CostSummary>("/costs?days=30")
                .await
                .map_err(|e| e.message),
        ));
    });
    view! {
        <Card id="costs" icon=Ic::Gauge title="Kosten" text="Berechnet aus deinen Tokens und der Preistabelle (Listenpreise), letzte 30 Tage.">
            {move || match data.get() {
                None => view! { <p class="small muted" role="status">"Lade Kosten …"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="alert tone-warning">{e}</p> }.into_any(),
                Some(Ok(c)) => view! {
                    <p class="cost-total" data-testid="costs-total"><strong>{fmt_money_list(&c.totals)}</strong>" in 30 Tagen"
                        {(c.unknown_runs > 0).then(|| view! { <span class="small muted">{format!(" · {} Läufe ohne bekannten Preis (nicht enthalten)", c.unknown_runs)}</span> })}</p>
                    <div class="grid-3">
                        <CostList title="Nach Projekt" rows=c.by_project.clone()/>
                        <CostList title="Nach Modell" rows=c.by_model.clone()/>
                        <CostList title="Nach Schritt" rows=c.by_step.clone()/>
                    </div>
                }.into_any(),
            }}
        </Card>
    }
}

#[component]
fn CostList(title: &'static str, rows: Vec<queued_api::CostRow>) -> impl IntoView {
    view! {
        <div>
            <h3 class="small">{title}</h3>
            {if rows.is_empty() {
                view! { <p class="small muted">"Noch nichts verbraucht."</p> }.into_any()
            } else {
                view! {
                    <ul class="small cost-rows">
                        {rows.into_iter().take(8).map(|r| view! {
                            <li><span class="grow truncate">{r.label}</span><strong>{fmt_money(r.amount, &r.currency)}</strong></li>
                        }).collect_view()}
                    </ul>
                }.into_any()
            }}
        </div>
    }
}

// ---------------------------------------------------------------------------
// Price table
// ---------------------------------------------------------------------------

fn price(v: Option<f64>, cur: &str) -> String {
    v.map(|x| fmt_money(x, cur)).unwrap_or_else(|| "–".into())
}

#[component]
pub fn PricesCard() -> impl IntoView {
    let store = use_store();
    let cat = RwSignal::new(None::<PriceCatalog>);
    let filter = RwSignal::new(String::new());
    spawn_local(async move {
        if let Ok(c) = api::get::<PriceCatalog>("/prices").await {
            cat.set(Some(c));
        }
    });
    let filter_ids = FieldIds::new("price-filter");
    let is_admin = move || store.me.with(|u| u.is_admin);
    view! {
        <Card id="prices" icon=Ic::FileText title="Preistabelle" text="Listenpreise je Million Tokens, mit Quelle und Belegstufe. Angekündigte Preisänderungen gelten ab ihrem Datum.">
            {move || cat.get().map(|c| {
                let filter_ids = filter_ids.clone();
                let version = c.version.clone();
                let ov = c.override_version;
                view! {
                    <p class="small muted">{format!("Stand {version}")}{ov.map(|v| format!(" · mit Anpassungen des Admins (Version {v})"))}</p>
                    <Field ids=filter_ids.clone() label="Modell suchen">
                        <input id=filter_ids.id class="input" type="search" placeholder="z. B. sonnet, mistral, gpt" bind:value=filter/>
                    </Field>
                    <table class="table compact" tabindex="0" data-testid="price-table">
                        <caption class="visually-hidden">"Preise je Million Tokens"</caption>
                        <thead><tr>
                            <th scope="col">"Modell"</th><th scope="col">"Eingabe"</th><th scope="col">"Ausgabe"</th>
                            <th scope="col">"Cache lesen"</th><th scope="col">"Batch"</th><th scope="col">"Gültig"</th><th scope="col">"Beleg"</th>
                        </tr></thead>
                        <tbody>
                            {move || {
                                let f = filter.get().to_lowercase();
                                c.entries.iter()
                                    .filter(|e| f.is_empty() || e.label.to_lowercase().contains(&f) || e.models.iter().any(|m| m.to_lowercase().contains(&f)))
                                    .map(|e| {
                                        let src = c.sources.iter().find(|s| s.id == e.source).cloned();
                                        let valid = match (e.effective_from, e.effective_until) {
                                            (Some(a), Some(b)) => format!("{} bis {}", a.format("%d.%m.%Y"), b.format("%d.%m.%Y")),
                                            (Some(a), None) => format!("ab {}", a.format("%d.%m.%Y")),
                                            (None, Some(b)) => format!("bis {}", b.format("%d.%m.%Y")),
                                            (None, None) => "–".into(),
                                        };
                                        view! {
                                            <tr>
                                                <td title=e.notes.clone()>{e.label.clone()}</td>
                                                <td>{fmt_money(e.input, &e.currency)}</td>
                                                <td>{fmt_money(e.output, &e.currency)}</td>
                                                <td>{price(e.cache_read, &e.currency)}</td>
                                                <td>{e.batch_factor.map(|b| format!("−{:.0} %", (1.0 - b) * 100.0)).unwrap_or_else(|| "–".into())}</td>
                                                <td>{valid}</td>
                                                <td>{match src {
                                                    Some(s) => view! { <a href=s.url target="_blank" rel="noopener noreferrer">{e.evidence.label()}</a> }.into_any(),
                                                    None => view! { <span>{e.evidence.label()}</span> }.into_any(),
                                                }}</td>
                                            </tr>
                                        }
                                    }).collect_view()
                            }}
                        </tbody>
                    </table>
                    <details class="small">
                        <summary>"Regionale Aufschläge"</summary>
                        <ul>{c.regions.iter().map(|r| view! { <li>{format!("{} – ×{} ({})", r.label, r.multiplier, r.evidence.label())}</li> }).collect_view()}</ul>
                    </details>
                }
            })}
            {move || is_admin().then(|| view! { <PriceAdmin cat=cat/> })}
        </Card>
    }
}

#[component]
fn PriceAdmin(cat: RwSignal<Option<PriceCatalog>>) -> impl IntoView {
    let store = use_store();
    let text = RwSignal::new(String::new());
    let history = RwSignal::new(Vec::<PriceOverrideVersion>::new());
    let error = RwSignal::new(None::<String>);
    let load = move || {
        spawn_local(async move {
            if let Ok(o) = api::get::<PriceOverride>("/admin/prices").await {
                text.set(serde_json::to_string_pretty(&o).unwrap_or_default());
            }
            if let Ok(h) = api::get::<Vec<PriceOverrideVersion>>("/admin/prices/history").await {
                history.set(h);
            }
        });
    };
    load();
    let save = move |_| {
        let body: PriceOverride = match serde_json::from_str(&text.get_untracked()) {
            Ok(b) => b,
            Err(e) => {
                error.set(Some(format!("Kein gültiges JSON: {e}")));
                return;
            }
        };
        error.set(None);
        spawn_local(async move {
            match api::put::<_, PriceCatalog>("/admin/prices", &body).await {
                Ok(c) => {
                    cat.set(Some(c));
                    store.toast("Preise gespeichert (neue Version)");
                    load();
                }
                Err(e) => error.set(Some(e.message)),
            }
        });
    };
    let ids = FieldIds::new("price-override");
    view! {
        <details class="sub-card">
            <summary>"Preise anpassen (Admin)"</summary>
            <p class="small muted">"Einträge mit der ID eines Katalogeintrags ersetzen ihn, neue IDs kommen dazu (z. B. für eigene Server). Jede Änderung wird als neue Version gespeichert."</p>
            <Field ids=ids.clone() label="Anpassungen (JSON)">
                <textarea id=ids.id.clone() class="textarea mono-input" rows="8" spellcheck="false" bind:value=text></textarea>
            </Field>
            {move || error.get().map(|e| view! { <p class="alert tone-danger" role="alert">{e}</p> })}
            <div class="form-actions"><button class="btn primary sm" type="button" on:click=save>"Speichern"</button></div>
            <ul class="small muted">
                {move || history.get().into_iter().map(|h| view! {
                    <li>{format!("Version {} · {} Einträge · {}{}", h.version, h.entries, h.created_at.format("%d.%m.%Y %H:%M"), if h.note.is_empty() { String::new() } else { format!(" · {}", h.note) })}</li>
                }).collect_view()}
            </ul>
        </details>
    }
}

// ---------------------------------------------------------------------------
// CLI status
// ---------------------------------------------------------------------------

fn cli_state_label(s: &str) -> &'static str {
    match s {
        "ok" => "Bereit",
        "missing" => "Fehlt",
        "version_mismatch" => "Andere Version",
        "frozen" => "Eingefroren",
        "replaced" => "Nachfolger gefunden",
        _ => "Unerwartet",
    }
}

#[component]
pub fn CliStatusCard() -> impl IntoView {
    let store = use_store();
    let result = RwSignal::new(None::<Result<Vec<CliStatus>, String>>);
    let busy = RwSignal::new(false);
    let check = move |_| {
        busy.set(true);
        spawn_local(async move {
            result.set(Some(
                api::get::<Vec<CliStatus>>("/cli-status")
                    .await
                    .map_err(|e| e.message),
            ));
            busy.set(false);
        });
    };
    view! {
        <Card id="cli-status" icon=Ic::Terminal title="Agenten-Programme" text="Welche Agenten-CLIs in der Sandbox installiert sind – und ob die Gemini CLI noch die richtige ist.">
            <ul class="small">
                {move || store.meta.with(|m| m.cli_notes.iter().map(|(cli, n)| view! {
                    <li><strong>{provider_label(cli)}</strong>{format!(" ({}): {}", if n.state == "frozen" { "eingefroren" } else { "aktiv" }, n.note)}</li>
                }).collect_view())}
            </ul>
            <div><button class="btn sm" type="button" disabled=move || busy.get() on:click=check>"Jetzt in der Sandbox prüfen"</button></div>
            {move || result.get().map(|r| match r {
                Err(e) => view! { <p class="alert tone-warning">{e}</p> }.into_any(),
                Ok(list) => view! {
                    <ul class="plain-list" data-testid="cli-status">
                        {list.into_iter().map(|s| view! {
                            <li class="row wrap">
                                <strong>{s.cli.clone()}</strong>
                                <span class=format!("tag {}", if s.state == "ok" { "tone-success" } else if s.state == "missing" || s.state == "replaced" { "tone-warning" } else { "tone-info" })>{cli_state_label(&s.state)}</span>
                                <span class="small muted">{s.installed.clone().unwrap_or_else(|| "nicht installiert".into())}</span>
                                <span class="small">{s.note.clone()}</span>
                            </li>
                        }).collect_view()}
                    </ul>
                }.into_any(),
            })}
        </Card>
    }
}

// ---------------------------------------------------------------------------
// Model test (eval set)
// ---------------------------------------------------------------------------

#[component]
pub fn EvalCard() -> impl IntoView {
    let store = use_store();
    let runs = RwSignal::new(Vec::<EvalRunView>::new());
    let cases = RwSignal::new(Vec::<EvalCaseInfo>::new());
    let access = RwSignal::new(String::new());
    let model = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            if let Ok(r) = api::get::<Vec<EvalRunView>>("/evals").await {
                runs.set(r);
            }
        });
    };
    spawn_local(async move {
        if let Ok(c) = api::get::<Vec<EvalCaseInfo>>("/evals/cases").await {
            cases.set(c);
        }
    });
    load();
    // Refresh while something runs.
    Effect::new(move |_| {
        store.now.track();
        if runs.with_untracked(|r| r.iter().any(|x| x.status == "running")) {
            load();
        }
    });
    let start = move |_| {
        let (connector_id, provider) = parse_access(&access.get_untracked());
        let body = StartEval {
            connector_id,
            provider,
            model: Some(model.get_untracked().trim().to_string()).filter(|m| !m.is_empty()),
            cases: vec![],
        };
        busy.set(true);
        spawn_local(async move {
            match api::post::<_, EvalRunView>("/evals", &body).await {
                Ok(_) => {
                    store.toast("Modell-Test gestartet");
                    load();
                }
                Err(e) => store.toast_error("Modell-Test nicht gestartet", &e),
            }
            busy.set(false);
        });
    };
    let acc_ids = FieldIds::new("eval-access");
    let model_ids = FieldIds::new("eval-model");
    let legacy = move || {
        store.meta.with(|m| {
            m.providers
                .iter()
                .filter(|p| m.credential_kinds.get(*p).is_some_and(|k| !k.is_empty()))
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    view! {
        <Card id="evals" icon=Ic::Bot title="Modelle testen" text="Ein kleiner, wiederholbarer Satz Programmieraufgaben mit automatischer Prüfung. Zeigt, wie oft ein Modell sie löst und was ein gelöster Auftrag kostet.">
            <p class="small muted">{move || format!("{} Aufgaben ({}). Läuft in versteckten Projekten, ohne Review und ohne Reparaturversuche.", cases.with(Vec::len), cases.with(|c| c.iter().map(|x| step_label_case(&x.category)).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>().join(", ")))}</p>
            <div class="grid-3">
                <Field ids=acc_ids.clone() label="Zugang">
                    <select id=acc_ids.id.clone() class="select" prop:value=move || access.get() on:change=move |ev| access.set(event_target_value(&ev))>
                        <option value="">"Standard"</option>
                        {move || legacy().into_iter().map(|p| view! { <option value=format!("p:{p}")>{provider_label(&p)}</option> }).collect_view()}
                        {move || store.connectors.get().into_iter().filter(|c| c.project_id.is_none()).map(|c| view! { <option value=format!("c:{}", c.id)>{connector_label(&c)}</option> }).collect_view()}
                    </select>
                </Field>
                <Field ids=model_ids.clone() label="Modell zum Testen" hint="Leer = Standardmodell. Beim Test-Anbieter: mock-weak, mock-medium oder mock-strong.">
                    <input id=model_ids.id.clone() class="input" autocomplete="off" bind:value=model/>
                </Field>
                <div class="field"><span class="label" aria-hidden="true">" "</span>
                    <button class="btn primary" type="button" disabled=move || busy.get() on:click=start><Icon icon=Ic::Play class="sm"/>"Test starten"</button>
                </div>
            </div>
            <ul class="plain-list" data-testid="eval-runs">
                {move || runs.get().into_iter().map(|r| {
                    let s = &r.summary;
                    let rate = s.success_rate.map(|x| format!("{} von {} gelöst ({:.0} %)", s.succeeded, s.finished, x * 100.0)).unwrap_or_else(|| "noch kein Ergebnis".into());
                    let tokens: i64 = r.results.iter().map(|x| x.usage.input_tokens + x.usage.output_tokens).sum();
                    view! {
                        <li class="sub-card">
                            <div class="row wrap">
                                <strong class="grow">{r.target_label.clone()}</strong>
                                <span class=format!("tag {}", if r.status == "finished" { "tone-success" } else { "tone-info" })>{if r.status == "finished" { "Fertig" } else { "Läuft" }}</span>
                            </div>
                            <p class="small">{format!("{rate} · Kosten {} · pro gelöster Aufgabe {} · {} Tokens", fmt_money_list(&s.cost), fmt_money_list(&s.cost_per_success), fmt_tokens(tokens))}</p>
                        </li>
                    }
                }).collect_view()}
            </ul>
            <p class="small muted"><HelpTip topic="Modelle testen" text="Die Erfolgsquoten der günstigen Modelle sind im Kostenbericht nur Annahmen. Erst eigene Messungen zeigen, ob ein billiges Modell pro gelöstem Auftrag wirklich günstiger ist." align="align-start"/>" Ergebnisse sind die Grundlage für die automatische Modellwahl."</p>
        </Card>
    }
}

fn step_label_case(category: &str) -> &'static str {
    match category {
        "bugfix" => "Fehler beheben",
        "feature" => "Funktion",
        "docs" => "Doku",
        "tests" => "Tests",
        "refactor" => "Umbau",
        "security" => "Sicherheit",
        _ => step_label(category),
    }
}
