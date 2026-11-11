//! WP06 in the task drawer: the plan view (genehmigen / ändern / verwerfen),
//! messages to the agent (with withdraw and "Sofort zustellen"), "Weiter
//! mit …" and inline comments in the diff (also by touch).
use leptos::prelude::*;
use leptos::task::spawn_local;
use queued_api::diff::{self, Kind};
use queued_api::steer::{
    CommentLocation, CommentView, ContinueTask, NewComment, PlanApprove, PlanRevise, SendComments,
    SendMessage, SentFollowUp, Side, TaskComments, TaskMessageView, TaskMessages, COMMENTS_HELP,
    CONTINUE_HELP, MAX_COMMENT_CHARS, MAX_MESSAGE_CHARS, MAX_NOTE_CHARS, MAX_PLAN_CHARS,
    MESSAGE_HELP, PLAN_MODE_HELP,
};
use queued_api::{gate::GateStage, ProjectRole, Task, TaskEvent};
use uuid::Uuid;

use crate::api;
use crate::logic::comments::{click, count_label, placement, Selection};
use crate::logic::format::fmt_time;
use crate::nav::use_open_task;
use crate::store::{tz, use_store, Store};
use crate::ui::dialog::Confirm;
use crate::ui::{uid, HelpTip, Ic, Icon};

/// The caller may decide/steer this task: its creator (editor or owner of
/// the project) or the project owner (the server checks it again).
fn may_steer(store: Store, t: &Task) -> bool {
    let role = store
        .projects
        .with(|ps| ps.iter().find(|p| p.id == t.project_id).map(|p| p.role));
    match role {
        Some(ProjectRole::Owner) => true,
        Some(r) => r.can_edit() && store.me.with(|m| m.id == t.user_id),
        None => false,
    }
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Remaining characters as a small hint (only near the limit).
fn rest_hint(text: RwSignal<String>, max: usize) -> impl IntoView {
    move || {
        let n = text.with(|t| chars(t));
        (n > max * 8 / 10).then(|| {
            let class = if n > max {
                "small tone-warning-text"
            } else {
                "small muted"
            };
            view! { <span class=class aria-live="polite">{format!("{n} / {max} Zeichen")}</span> }
        })
    }
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlanEdit {
    None,
    Own,
    Revise,
}

/// The plan waits for a decision (task held at gate stage `plan`).
#[component]
pub fn PlanPanel(t: Task) -> impl IntoView {
    let store = use_store();
    let id = t.id;
    let can = may_steer(store, &t);
    let plan = t.plan.clone().unwrap_or_default();
    let meta = t.plan_meta_parsed().unwrap_or_default();
    let mode = RwSignal::new(PlanEdit::None);
    let text = RwSignal::new(plan.clone());
    let note = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let (edit_id, note_id) = (uid("plan-edit"), uid("plan-note"));
    let post = move |path: &'static str, body: serde_json::Value, done: &'static str| {
        busy.set(true);
        spawn_local(async move {
            let res = api::post_ok(&format!("/tasks/{id}/plan/{path}"), &body).await;
            busy.set(false);
            match res {
                Ok(()) => {
                    mode.set(PlanEdit::None);
                    store.announce(done);
                }
                Err(e) => store.toast_error("Entscheidung fehlgeschlagen", &e),
            }
        });
    };
    let approve = move |_| {
        post(
            "approve",
            serde_json::to_value(PlanApprove::default()).unwrap_or_default(),
            "Plan genehmigt – der Agent setzt ihn jetzt um",
        )
    };
    let approve_edited = move |_| {
        let p = text.get_untracked();
        post(
            "approve",
            serde_json::to_value(PlanApprove { plan: Some(p) }).unwrap_or_default(),
            "Geänderter Plan genehmigt – der Agent setzt ihn jetzt um",
        )
    };
    let revise = move |_| {
        let n = note.get_untracked();
        post(
            "revise",
            serde_json::to_value(PlanRevise { note: n }).unwrap_or_default(),
            "Neuer Plan angefordert",
        )
    };
    let reject = move |_| {
        store.ask(Confirm {
            title: "Plan verwerfen?".into(),
            body: "Der Auftrag wird beendet, ohne dass etwas geändert wurde. Du kannst ihn später mit „Erneut versuchen“ neu planen lassen.".into(),
            confirm_label: "Plan verwerfen".into(),
            danger: true,
            on_confirm: Callback::new(move |_| {
                post("reject", serde_json::json!({}), "Plan verworfen");
            }),
        });
    };
    let source = match meta.source {
        queued_api::steer::PlanSource::Agent => "Vom Agenten erstellt, der nur lesen durfte",
        queued_api::steer::PlanSource::SingleShot => {
            "Von einem Modell in einem einzelnen Schritt erstellt"
        }
    };
    let attempted = meta.attempted_changes.clone();
    let attempted_total = meta.attempted_total;
    let questions = meta.questions.clone();
    let auto_at = meta.auto_approve_at;
    let revision = t.plan_revision;
    view! {
        <section class="block plan-panel" data-testid="plan-panel" aria-labelledby="plan-title">
            <h3 id="plan-title">
                <Icon icon=Ic::FileText class="sm"/>" "{GateStage::Plan.title()}
                <HelpTip topic="Plan-Modus" text=PLAN_MODE_HELP align="align-start"/>
            </h3>
            <p class="small">{GateStage::Plan.help()}</p>
            <p class="small muted">
                {source}
                {(revision > 0).then(|| format!(" · Überarbeitung {revision}"))}
            </p>
            {(attempted_total > 0).then(|| view! {
                <div class="box tone-warning" role="note" data-testid="plan-attempted">
                    <strong>{format!("Der Agent hat beim Planen versucht, {attempted_total} Datei(en) zu ändern.")}</strong>
                    " Queue hat ihn in einer schreibgeschützten Wegwerf-Kopie arbeiten lassen – diese Änderungen wurden verworfen, am Projekt hat sich nichts geändert. Schau dir den Plan deshalb besonders genau an."
                    <ul class="mono small">{attempted.into_iter().take(10).map(|p| view! { <li>{p}</li> }).collect_view()}</ul>
                </div>
            })}
            <div class="box plan-text" data-testid="plan-text" tabindex="0" aria-label="Plan des Agenten">{plan}</div>
            {(!questions.is_empty()).then(|| view! {
                <div class="box tone-warning">
                    <strong>"Offene Fragen des Agenten"</strong>
                    <ul class="questions">{questions.into_iter().map(|q| view! { <li>{q}</li> }).collect_view()}</ul>
                    <p class="small">"Antworte über „Ändern → Neuen Plan anfordern“ oder passe den Plan selbst an."</p>
                </div>
            })}
            {auto_at.map(|at| view! {
                <p class="small muted" data-testid="plan-auto">
                    <Icon icon=Ic::Clock class="sm"/>
                    {format!(" Wird {} automatisch genehmigt, wenn niemand entscheidet.", fmt_time(at, store.now.get_untracked(), &tz()))}
                </p>
            })}
            {(!can).then(|| view! { <p class="small muted">"Entscheiden können die Person, die den Auftrag erstellt hat, und die Eigentümerin oder der Eigentümer des Projekts."</p> })}
            {can.then(|| view! {
                <div class="row wrap gate-actions">
                    <button class="btn primary" type="button" disabled=move || busy.get() on:click=approve>
                        <Icon icon=Ic::Check class="sm"/>"Plan genehmigen"
                    </button>
                    <button class="btn" type="button" aria-expanded=move || (mode.get() != PlanEdit::None).to_string()
                        on:click=move |_| mode.update(|m| *m = if *m == PlanEdit::None { PlanEdit::Own } else { PlanEdit::None })>
                        <Icon icon=Ic::Pencil class="sm"/>"Ändern"
                    </button>
                    <button class="btn danger" type="button" disabled=move || busy.get() on:click=reject>
                        <Icon icon=Ic::X class="sm"/>"Verwerfen"
                    </button>
                </div>
            })}
            {move || (mode.get() != PlanEdit::None).then(|| {
                let (edit_id, note_id) = (edit_id.clone(), note_id.clone());
                view! {
                    <div class="plan-edit stack">
                        <div class="segmented" role="group" aria-label="Wie ändern?">
                            <button type="button" aria-pressed=move || (mode.get() == PlanEdit::Own).to_string() on:click=move |_| mode.set(PlanEdit::Own)>"Selbst bearbeiten"</button>
                            <button type="button" aria-pressed=move || (mode.get() == PlanEdit::Revise).to_string() on:click=move |_| mode.set(PlanEdit::Revise)>"Neuen Plan anfordern"</button>
                        </div>
                        {move || match mode.get() {
                            PlanEdit::Own => view! {
                                <div class="field">
                                    <div class="label"><label for=edit_id.clone()>"Plan"</label></div>
                                    <textarea id=edit_id.clone() class="textarea mono-text" rows="12" bind:value=text></textarea>
                                    {rest_hint(text, MAX_PLAN_CHARS)}
                                    <p class="small muted">"Der Agent setzt genau diesen Text um."</p>
                                </div>
                                <div class="row wrap">
                                    <button class="btn primary" type="button"
                                        disabled=move || busy.get() || text.with(|t| t.trim().is_empty() || chars(t) > MAX_PLAN_CHARS)
                                        on:click=approve_edited>
                                        <Icon icon=Ic::Check class="sm"/>"Geänderten Plan genehmigen"
                                    </button>
                                    <button class="btn ghost" type="button" on:click=move |_| mode.set(PlanEdit::None)>"Abbrechen"</button>
                                </div>
                            }.into_any(),
                            _ => view! {
                                <div class="field">
                                    <div class="label"><label for=note_id.clone()>"Was soll anders werden?"</label></div>
                                    <textarea id=note_id.clone() class="textarea" rows="4" placeholder="z. B. „Bitte zuerst Tests schreiben“ oder die Antwort auf eine Frage" bind:value=note></textarea>
                                    {rest_hint(note, MAX_NOTE_CHARS)}
                                    <p class="small muted">"Der Agent schreibt einen neuen Plan – wieder nur lesend. Du entscheidest danach erneut."</p>
                                </div>
                                <div class="row wrap">
                                    <button class="btn primary" type="button"
                                        disabled=move || busy.get() || note.with(|t| t.trim().is_empty() || chars(t) > MAX_NOTE_CHARS)
                                        on:click=revise>
                                        <Icon icon=Ic::Retry class="sm"/>"Neuen Plan anfordern"
                                    </button>
                                    <button class="btn ghost" type="button" on:click=move |_| mode.set(PlanEdit::None)>"Abbrechen"</button>
                                </div>
                            }.into_any(),
                        }}
                    </div>
                }
            })}
        </section>
    }
}

// ---------------------------------------------------------------------------
// Messages and "Weiter mit …"
// ---------------------------------------------------------------------------

fn state_tone(m: &TaskMessageView) -> &'static str {
    match m.state {
        queued_api::steer::MessageState::Queued => "tone-warning",
        queued_api::steer::MessageState::Delivered => "tone-success",
        _ => "tone-neutral",
    }
}

/// Messages to the agent (active tasks) and "Weiter mit …" (finished ones).
#[component]
pub fn MessagesBlock(
    id: Uuid,
    task: Memo<Option<Task>>,
    events: RwSignal<Vec<TaskEvent>>,
) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let data = RwSignal::new(None::<TaskMessages>);
    let text = RwSignal::new(String::new());
    let next = RwSignal::new(String::new());
    let plan_first = RwSignal::new(None::<bool>);
    let busy = RwSignal::new(false);
    let (msg_id, next_id) = (uid("steer-msg"), uid("steer-next"));
    // Reload on every steering/plan event and status change of the task.
    let trigger = Memo::new(move |_| {
        let n = events.with(|e| {
            e.iter()
                .filter(|e| matches!(e.kind.as_str(), "steer" | "plan"))
                .count()
        });
        let s = task.with(|t| t.as_ref().map(|t| t.status.clone()).unwrap_or_default());
        (n, s)
    });
    let load = move || {
        spawn_local(async move {
            if let Ok(d) = api::get::<TaskMessages>(&format!("/tasks/{id}/messages")).await {
                data.set(Some(d));
            }
        });
    };
    Effect::new(move |_| {
        let _ = trigger.get();
        load();
    });
    let send = move |now: bool| {
        let body = text.get_untracked();
        if body.trim().is_empty() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let res = api::post::<_, TaskMessageView>(
                &format!("/tasks/{id}/messages"),
                &SendMessage { body, now },
            )
            .await;
            busy.set(false);
            match res {
                Ok(_) => {
                    text.set(String::new());
                    store.announce(if now {
                        "Nachricht gesendet – der Agent wird unterbrochen"
                    } else {
                        "Nachricht gesendet – sie wartet auf die nächste Pause des Agenten"
                    });
                    load();
                }
                Err(e) => store.toast_error("Nachricht konnte nicht gesendet werden", &e),
            }
        });
    };
    let withdraw = move |mid: Uuid| {
        spawn_local(async move {
            match api::delete(&format!("/tasks/{id}/messages/{mid}")).await {
                Ok(()) => {
                    store.announce("Nachricht zurückgezogen");
                    load();
                }
                Err(e) => {
                    store.toast_error("Zurückziehen nicht möglich", &e);
                    load();
                }
            }
        });
    };
    // Default of "Erst einen Plan zeigen": the project's plan mode (the
    // server applies the same default).
    let project_plan = Memo::new(move |_| {
        let pid = task.with(|t| t.as_ref().map(|t| t.project_id));
        store
            .projects
            .with(|ps| ps.iter().any(|p| Some(p.id) == pid && p.plan_mode))
    });
    let open2 = open.clone();
    let cont = move || {
        let message = next.get_untracked();
        if message.trim().is_empty() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        let open = open2.clone();
        spawn_local(async move {
            let body = ContinueTask {
                message,
                plan_mode: plan_first.get_untracked(),
            };
            let res = api::post::<_, Task>(&format!("/tasks/{id}/continue"), &body).await;
            busy.set(false);
            match res {
                Ok(t) => {
                    next.set(String::new());
                    store.announce(format!("Fortsetzung „{}“ eingereiht", t.title));
                    open(t.id);
                }
                Err(e) => store.toast_error("Fortsetzung konnte nicht gestartet werden", &e),
            }
        });
    };
    view! {
        {move || data.get().filter(|d| d.can_send || d.can_continue || !d.messages.is_empty()).map(|d| {
            let (msg_id, next_id) = (msg_id.clone(), next_id.clone());
            let cont = cont.clone();
            let open = open.clone();
            let running = d.running;
            view! {
                <section class="block steer" data-testid="messages" aria-labelledby="steer-title">
                    <h3 id="steer-title">
                        <Icon icon=Ic::Message class="sm"/>
                        {if d.can_continue && !d.can_send { " Weiter mit diesem Auftrag" } else { " Nachricht an den Agenten" }}
                        <HelpTip topic="Nachrichten" text=if d.can_continue && !d.can_send { CONTINUE_HELP } else { MESSAGE_HELP } align="align-start"/>
                    </h3>
                    {(!d.messages.is_empty()).then(|| view! {
                        <ul class="messages" aria-label="Nachrichten">
                            {d.messages.iter().map(|m| {
                                let mid = m.id;
                                let when = fmt_time(m.created_at, store.now.get_untracked(), &tz());
                                let state = match (m.state, m.delivery) {
                                    (queued_api::steer::MessageState::Delivered, Some(dl)) => format!("Zugestellt {}", dl.label()),
                                    (s, _) => s.label().to_string(),
                                };
                                let open = open.clone();
                                view! {
                                    <li class="message" data-state=m.state.as_str()>
                                        <div class="row wrap small">
                                            <strong>{m.author.clone()}</strong>
                                            <span class="muted">{when}</span>
                                            {(m.source == queued_api::steer::MessageSource::Comments).then(|| view! { <span class="tag">"Kommentare"</span> })}
                                            {m.urgent.then(|| view! { <span class="tag">"sofort"</span> })}
                                            <span class=format!("tag {}", state_tone(m))>{state}</span>
                                        </div>
                                        <p class="message-body">{m.body.clone()}</p>
                                        <div class="row wrap">
                                            {m.can_withdraw.then(|| view! {
                                                <button class="btn sm" type="button" on:click=move |_| withdraw(mid)>
                                                    <Icon icon=Ic::Undo class="sm"/>"Zurückziehen"
                                                </button>
                                            })}
                                            {m.continued_as.map(|c| view! {
                                                <button class="btn sm" type="button" on:click=move |_| open(c)>
                                                    <Icon icon=Ic::ChevronRight class="sm"/>"Zur Fortsetzung"
                                                </button>
                                            })}
                                        </div>
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    })}
                    {d.can_send.then(|| view! {
                        <div class="field">
                            <div class="label"><label for=msg_id.clone()>"Deine Nachricht"</label></div>
                            <textarea id=msg_id.clone() class="textarea" rows="3" data-testid="message-input"
                                placeholder="z. B. „Bitte auch die Doku anpassen“ (Strg + Enter zum Senden)"
                                bind:value=text
                                on:keydown=move |ev| if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) { ev.prevent_default(); send(false); }
                            ></textarea>
                            {rest_hint(text, MAX_MESSAGE_CHARS)}
                        </div>
                        <div class="row wrap">
                            <button class="btn primary" type="button"
                                disabled=move || busy.get() || text.with(|t| t.trim().is_empty() || chars(t) > MAX_MESSAGE_CHARS)
                                on:click=move |_| send(false)>
                                <Icon icon=Ic::Send class="sm"/>"Senden"
                            </button>
                            {running.then(|| view! {
                                <button class="btn" type="button"
                                    disabled=move || busy.get() || text.with(|t| t.trim().is_empty() || chars(t) > MAX_MESSAGE_CHARS)
                                    on:click=move |_| send(true)>
                                    <Icon icon=Ic::Zap class="sm"/>"Sofort zustellen"
                                </button>
                            })}
                        </div>
                        {running.then(|| view! { <p class="small muted">"„Sofort zustellen“ unterbricht den Agenten kurz; er macht danach mit deiner Nachricht weiter."</p> })}
                    })}
                    {d.can_continue.then(|| {
                        let cont = cont.clone();
                        let cont2 = cont.clone();
                        view! {
                            <div class="field">
                                <div class="label"><label for=next_id.clone()>"Wie soll es weitergehen?"</label></div>
                                <textarea id=next_id.clone() class="textarea" rows="3" data-testid="continue-input"
                                    placeholder="z. B. „Jetzt noch die Tests ergänzen“"
                                    bind:value=next
                                    on:keydown=move |ev| if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) { ev.prevent_default(); cont(); }
                                ></textarea>
                                {rest_hint(next, MAX_MESSAGE_CHARS)}
                            </div>
                            <label class="check">
                                <input type="checkbox" prop:checked=move || plan_first.get().unwrap_or(project_plan.get())
                                    on:change=move |ev| plan_first.set(Some(event_target_checked(&ev)))/>
                                <span class="check-text">"Erst einen Plan zeigen"<small>"Der Agent plant zuerst, du gibst den Plan frei."</small></span>
                            </label>
                            <div class="row wrap">
                                <button class="btn primary" type="button"
                                    disabled=move || busy.get() || next.with(|t| t.trim().is_empty() || chars(t) > MAX_MESSAGE_CHARS)
                                    on:click=move |_| cont2()>
                                    <Icon icon=Ic::ChevronRight class="sm"/>"Weiter mit diesem Auftrag"
                                </button>
                            </div>
                        }
                    })}
                </section>
            }
        })}
    }
}

// ---------------------------------------------------------------------------
// Diff with inline comments
// ---------------------------------------------------------------------------

fn location_chip(c: &CommentView) -> impl IntoView {
    let (tone, label) = match c.location {
        CommentLocation::Current => ("tone-success", "aktuell"),
        CommentLocation::Moved { .. } => ("tone-info", "verschoben"),
        CommentLocation::Stale => ("tone-warning", "veraltet"),
    };
    view! { <span class=format!("tag {tone}") title=c.location.help()>{label}</span> }
}

/// The task's changes; with "Kommentieren" every line can be selected (tap
/// or click, a second line in the same section makes a range) and commented.
#[component]
pub fn CommentDiff(id: Uuid, diff: String) -> impl IntoView {
    let store = use_store();
    let open = use_open_task();
    let files = StoredValue::new(diff::parse(&diff));
    let comments = RwSignal::new(None::<TaskComments>);
    let commenting = RwSignal::new(false);
    let selection = RwSignal::new(None::<Selection>);
    let writing = RwSignal::new(false);
    let body = RwSignal::new(String::new());
    let note = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let (body_id, note_id) = (uid("comment-body"), uid("comment-note"));
    let load = move || {
        spawn_local(async move {
            if let Ok(c) = api::get::<TaskComments>(&format!("/tasks/{id}/comments")).await {
                comments.set(Some(c));
            }
        });
    };
    load();
    let tap = move |path: String, side: Side, n: i32| {
        if !commenting.get_untracked() {
            return;
        }
        let cur = selection.get_untracked();
        match files.with_value(|f| click(cur.as_ref(), f, &path, side, n)) {
            Ok(s) => {
                store.announce(format!("{} ausgewählt", s.label()));
                selection.set(Some(s));
                writing.set(false);
            }
            Err(e) => store.announce(e),
        }
    };
    let save = move || {
        let Some(s) = selection.get_untracked() else {
            return;
        };
        let text = body.get_untracked();
        if text.trim().is_empty() || busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            let req = NewComment {
                path: s.path.clone(),
                side: s.side,
                line_start: s.start,
                line_end: s.end,
                body: text,
            };
            let res = api::post::<_, CommentView>(&format!("/tasks/{id}/comments"), &req).await;
            busy.set(false);
            match res {
                Ok(_) => {
                    body.set(String::new());
                    selection.set(None);
                    writing.set(false);
                    store.announce(
                        "Kommentar gespeichert – er wird mit den anderen zusammen gesendet",
                    );
                    load();
                }
                Err(e) => store.toast_error("Kommentar konnte nicht gespeichert werden", &e),
            }
        });
    };
    let remove = move |cid: Uuid| {
        spawn_local(async move {
            match api::delete(&format!("/tasks/{id}/comments/{cid}")).await {
                Ok(()) => {
                    store.announce("Kommentar gelöscht");
                    load();
                }
                Err(e) => store.toast_error("Löschen fehlgeschlagen", &e),
            }
        });
    };
    let send_all = move || {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let open = open.clone();
        spawn_local(async move {
            let m = note.get_untracked();
            let req = SendComments {
                message: (!m.trim().is_empty()).then_some(m),
            };
            let res =
                api::post::<_, SentFollowUp>(&format!("/tasks/{id}/comments/send"), &req).await;
            busy.set(false);
            match res {
                Ok(r) => {
                    note.set(String::new());
                    commenting.set(false);
                    load();
                    if r.delivered_as == "continuation" {
                        store.announce(format!(
                            "{} als neuer Auftrag gesendet",
                            count_label(r.comments)
                        ));
                        open(r.task_id);
                    } else {
                        store.announce(format!(
                            "{} an den Agenten gesendet",
                            count_label(r.comments)
                        ));
                    }
                }
                Err(e) => store.toast_error("Senden fehlgeschlagen", &e),
            }
        });
    };
    let drafts = Memo::new(move |_| {
        comments.with(|c| {
            c.as_ref()
                .map(|c| c.comments.iter().filter(|x| !x.sent && x.mine).count())
                .unwrap_or(0)
        })
    });
    let can_comment =
        Memo::new(move |_| comments.with(|c| c.as_ref().is_some_and(|c| c.can_comment)));
    let stats: Vec<(String, usize, usize)> = files.with_value(|f| {
        f.iter()
            .map(|x| (x.path.clone(), x.added(), x.removed()))
            .collect()
    });
    if stats.is_empty() {
        return view! { <section class="block"><h3>"Änderungen"</h3><p class="box muted">"Keine Änderungen."</p></section> }.into_any();
    }
    let comment_card = move |c: CommentView| {
        let cid = c.id;
        let anchor_note =
            (c.location == CommentLocation::Stale && !c.anchor.is_empty()).then(|| {
                c.anchor
                    .iter()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        view! {
            <div class="comment" data-testid="comment" data-location=match c.location { CommentLocation::Current => "current", CommentLocation::Moved { .. } => "moved", CommentLocation::Stale => "stale" }>
                <div class="row wrap small">
                    <strong>{c.author.clone()}</strong>
                    <span class="muted">{if c.line_start == c.line_end { format!("Zeile {}", c.line_start) } else { format!("Zeilen {}–{}", c.line_start, c.line_end) }}</span>
                    {location_chip(&c)}
                    <span class="tag">{if c.sent { "gesendet" } else { "Entwurf" }}</span>
                </div>
                {anchor_note.map(|a| view! { <pre class="comment-anchor" aria-label="Ursprünglich kommentierte Zeilen">{a}</pre> })}
                <p class="message-body">{c.body.clone()}</p>
                {(c.mine && !c.sent).then(|| view! {
                    <div><button class="btn sm" type="button" on:click=move |_| remove(cid)><Icon icon=Ic::Trash class="sm"/>"Löschen"</button></div>
                })}
            </div>
        }
    };
    view! {
        <section class="block comment-diff" data-testid="diff">
            <h3>"Änderungen"</h3>
            <ul class="diff-files" style="list-style:none; margin:0; padding:0">
                {stats.into_iter().map(|(p, a, r)| view! {
                    <li>
                        <Icon icon=Ic::File class="sm"/>
                        <span class="mono truncate">{p}</span>
                        <span class="add">{format!("+{a}")}</span>
                        <span class="del">{format!("−{r}")}</span>
                    </li>
                }).collect_view()}
            </ul>
            {move || can_comment.get().then(|| view! {
                <div class="row wrap">
                    <button class="btn" type="button" aria-pressed=move || commenting.get().to_string() data-testid="comment-mode"
                        on:click=move |_| { commenting.update(|c| *c = !*c); selection.set(None); writing.set(false); }>
                        <Icon icon=Ic::Message class="sm"/>
                        {move || if commenting.get() { "Kommentieren beenden" } else { "Kommentieren" }}
                    </button>
                    <HelpTip topic="Kommentieren" text=COMMENTS_HELP/>
                </div>
            })}
            {move || commenting.get().then(|| view! {
                <p class="small muted">"Tippe auf eine Zeile, um sie auszuwählen – auf eine zweite für einen Bereich."</p>
            })}
            <div class="diff" tabindex="0" aria-label="Unterschiede im Detail" class:commenting=move || commenting.get()>
                {move || {
                    let placed = comments.with(|c| c.as_ref().map(|c| placement(&c.comments)));
                    let all = comments.with(|c| c.as_ref().map(|c| c.comments.clone()).unwrap_or_default());
                    let active = commenting.get();
                    files.with_value(|fs| fs.iter().map(|f| {
                        let path = f.path.clone();
                        let mut rows = vec![view! { <div class="d-file">{f.path.clone()}</div> }.into_any()];
                        let mut hunk = usize::MAX;
                        for l in &f.lines {
                            if l.hunk != hunk {
                                hunk = l.hunk;
                                let h = f.hunks.get(hunk).map(|h| h.header.clone()).unwrap_or_default();
                                rows.push(view! { <div class="d-hunk">{h}</div> }.into_any());
                            }
                            let side = l.side();
                            let n = l.number(side).unwrap_or(0);
                            let class = match l.kind { Kind::Add => "d-add", Kind::Del => "d-del", Kind::Context => "d-ctx" };
                            let sign = match l.kind { Kind::Add => '+', Kind::Del => '-', Kind::Context => ' ' };
                            let nums = format!("{:>4} {:>4}", l.old.map(|x| x.to_string()).unwrap_or_default(), l.new.map(|x| x.to_string()).unwrap_or_default());
                            let text = format!("{sign}{}", l.text);
                            if active {
                                let (p2, p3) = (path.clone(), path.clone());
                                let label = format!("{} Zeile {n} von {}", match side { Side::New => "Neue", Side::Old => "Entfernte" }, path);
                                rows.push(view! {
                                    <button type="button" class=format!("d-line {class}")
                                        aria-pressed=move || selection.with(|s| s.as_ref().is_some_and(|s| s.contains(&p2, side, n))).to_string()
                                        on:click=move |_| tap(p3.clone(), side, n)>
                                        <span class="d-num" aria-hidden="true">{nums}</span>
                                        // The visible text stays part of the name (WCAG 2.5.3).
                                        <span class="visually-hidden">{format!("{label}: ")}</span>
                                        <span class="d-text">{text}</span>
                                    </button>
                                }.into_any());
                            } else {
                                rows.push(view! {
                                    <div class=format!("d-row {class}")><span class="d-num" aria-hidden="true">{nums}</span><span class="d-text">{text}</span></div>
                                }.into_any());
                            }
                            if let Some((at, _)) = &placed {
                                if let Some(ix) = at.get(&(path.clone(), side, n)) {
                                    for i in ix {
                                        rows.push(view! { <div class="d-comments">{comment_card(all[*i].clone())}</div> }.into_any());
                                    }
                                }
                            }
                        }
                        rows.collect_view()
                    }).collect_view())
                }}
            </div>
            {move || {
                let stale: Vec<CommentView> = comments.with(|c| c.as_ref().map(|c| {
                    let (_, st) = placement(&c.comments);
                    st.into_iter().map(|i| c.comments[i].clone()).collect()
                }).unwrap_or_default());
                (!stale.is_empty()).then(|| view! {
                    <div class="stack" data-testid="stale-comments">
                        <h4 class="row">"Veraltete Kommentare"<HelpTip topic="Veraltet" text=CommentLocation::Stale.help()/></h4>
                        {stale.into_iter().map(|c| view! { <div class="d-comments">{comment_card(c)}</div> }).collect_view()}
                    </div>
                })
            }}
            // Compact and sticky above the home indicator: what is selected.
            {move || selection.get().filter(|_| commenting.get() && !writing.get()).map(|s| view! {
                <div class="comment-bar" role="region" aria-label="Ausgewählte Zeilen" data-testid="comment-bar">
                    <p class="small"><strong>{s.label()}</strong>{(s.start == s.end).then_some(" – tippe eine weitere Zeile für einen Bereich")}</p>
                    <div class="row wrap">
                        <button class="btn primary" type="button" on:click=move |_| writing.set(true)>
                            <Icon icon=Ic::Pencil class="sm"/>"Kommentar schreiben"
                        </button>
                        <button class="btn ghost" type="button" on:click=move |_| selection.set(None)>"Auswahl aufheben"</button>
                    </div>
                </div>
            })}
            // The comment itself in the flow (a tall sticky box would not fit on a phone).
            {move || selection.get().filter(|_| commenting.get() && writing.get()).map(|s| {
                let body_id = body_id.clone();
                let input: NodeRef<leptos::html::Textarea> = NodeRef::new();
                Effect::new(move |_| {
                    if let Some(el) = input.get() {
                        let _ = el.focus();
                    }
                });
                view! {
                    <div class="comment-form" role="region" aria-label="Kommentar zu den ausgewählten Zeilen" data-testid="comment-form">
                        <p class="small"><strong>{s.label()}</strong></p>
                        <div class="field">
                            <div class="label"><label for=body_id.clone()>"Dein Kommentar"</label></div>
                            <textarea id=body_id class="textarea" rows="3" node_ref=input bind:value=body data-testid="comment-input"
                                on:keydown=move |ev| if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) { ev.prevent_default(); save(); }></textarea>
                            {rest_hint(body, MAX_COMMENT_CHARS)}
                        </div>
                        <div class="row wrap">
                            <button class="btn primary" type="button" disabled=move || busy.get() || body.with(|b| b.trim().is_empty() || chars(b) > MAX_COMMENT_CHARS) on:click=move |_| save()>
                                <Icon icon=Ic::Check class="sm"/>"Kommentar speichern"
                            </button>
                            <button class="btn ghost" type="button" on:click=move |_| { writing.set(false); selection.set(None); }>"Abbrechen"</button>
                        </div>
                    </div>
                }
            })}
            {move || (drafts.get() > 0).then(|| {
                let note_id = note_id.clone();
                let send_all = send_all.clone();
                view! {
                    <div class="comment-send" data-testid="comment-send">
                        <p class="small"><strong>{format!("{} gesammelt", count_label(drafts.get()))}</strong>" – sie gehen zusammen als eine Nachricht an den Agenten."</p>
                        <div class="field">
                            <div class="label"><label for=note_id.clone()>"Dazu schreiben (optional)"</label></div>
                            <textarea id=note_id class="textarea" rows="2" bind:value=note></textarea>
                        </div>
                        <div class="row wrap">
                            <button class="btn primary" type="button" disabled=move || busy.get() on:click=move |_| send_all()>
                                <Icon icon=Ic::Send class="sm"/>{move || format!("{} senden", count_label(drafts.get()))}
                            </button>
                        </div>
                    </div>
                }
            })}
        </section>
    }
    .into_any()
}
