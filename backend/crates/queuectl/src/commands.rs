//! The subcommands of `queuectl` (output in German, `--json` for scripts).
use std::{io::Read, time::Duration};

use chrono::Local;
use futures::StreamExt;
use queued_api::{
    steer::{ContinueTask, PlanApprove, PlanRevise, SendMessage},
    tokens::WhoAmI,
    triggers::{FireRequest, FireResult, TemplateView},
    CreateTask, ProjectSummary, ServerEvent, Task, TaskDetail, TaskLimitFields, TaskStatus,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{exit, Api, Result, SseParser, EXIT_FAILED, EXIT_OK, EXIT_USAGE};

pub fn status_label(s: &str) -> &'static str {
    match TaskStatus::parse(s) {
        Some(TaskStatus::PendingApproval) => "wartet auf Freigabe",
        Some(TaskStatus::Queued) => "in der Warteschlange",
        Some(TaskStatus::Running) => "läuft",
        Some(TaskStatus::WaitingBatch) => "wartet auf Anbieter",
        Some(TaskStatus::WaitingInput) => "wartet auf Antwort",
        Some(TaskStatus::Held) => "Freigabe nötig",
        Some(TaskStatus::Merging) => "wird übernommen",
        Some(TaskStatus::Done) => "erledigt",
        Some(TaskStatus::Failed) => "fehlgeschlagen",
        Some(TaskStatus::Blocked) => "blockiert",
        Some(TaskStatus::Cancelled) => "abgebrochen",
        None => "unbekannt",
    }
}

fn short(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

fn print_json(v: &impl serde::Serialize) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// Text from the arguments, or from stdin with `-` (or no arguments).
pub fn text_arg(words: &[String], what: &str) -> Result<String> {
    let t = if words.is_empty() || (words.len() == 1 && words[0] == "-") {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| exit(EXIT_USAGE, format!("stdin: {e}")))?;
        s
    } else {
        words.join(" ")
    };
    if t.trim().is_empty() {
        return Err(exit(
            EXIT_USAGE,
            format!("{what} fehlt (als Argument oder über stdin mit „-“)"),
        ));
    }
    Ok(t.trim().to_string())
}

/// Project by id or (case-insensitive) name; without one the only project.
pub async fn resolve_project(api: &Api, p: Option<&str>) -> Result<ProjectSummary> {
    let all: Vec<ProjectSummary> = api.get("/projects").await?;
    match p {
        Some(p) => {
            let p = p.trim();
            all.iter()
                .find(|x| x.id.to_string() == p)
                .or_else(|| all.iter().find(|x| x.name.eq_ignore_ascii_case(p)))
                .cloned()
                .ok_or_else(|| exit(EXIT_USAGE, format!("Projekt „{p}“ nicht gefunden")))
        }
        None if all.len() == 1 => Ok(all[0].clone()),
        None => Err(exit(
            EXIT_USAGE,
            "Welches Projekt? --project NAME angeben oder „project = …“ in die Konfiguration schreiben",
        )),
    }
}

/// Task by full id or a unique prefix of the own tasks.
pub async fn resolve_task(api: &Api, s: &str) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(s.trim()) {
        return Ok(id);
    }
    let p = s.trim().to_ascii_lowercase();
    if p.len() < 4 || !p.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(exit(
            EXIT_USAGE,
            format!("„{s}“ ist keine Auftrags-ID (mindestens 4 Zeichen)"),
        ));
    }
    let tasks: Vec<Task> = api.get("/tasks").await?;
    let hits: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.id.to_string().starts_with(&p))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.id),
        [] => Err(exit(EXIT_FAILED, format!("Kein Auftrag beginnt mit „{s}“"))),
        _ => Err(exit(
            EXIT_USAGE,
            format!("„{s}“ ist mehrdeutig ({} Aufträge)", hits.len()),
        )),
    }
}

pub async fn whoami(api: &Api, json: bool) -> Result<i32> {
    let w: WhoAmI = api.get("/whoami").await?;
    if json {
        print_json(&w);
    } else {
        println!("Angemeldet als {} <{}>", w.name, w.email);
        println!(
            "Token: {} „{}“, gültig bis {}",
            w.token.prefix,
            w.token.name,
            w.token
                .expires_at
                .with_timezone(&Local)
                .format("%d.%m.%Y %H:%M")
        );
        let scopes: Vec<&str> = w.token.scopes.iter().map(|s| s.as_str()).collect();
        println!("Berechtigungen: {}", scopes.join(", "));
    }
    Ok(EXIT_OK)
}

pub async fn projects(api: &Api, json: bool) -> Result<i32> {
    let all: Vec<ProjectSummary> = api.get("/projects").await?;
    if json {
        print_json(&all);
    } else if all.is_empty() {
        println!("Keine Projekte.");
    } else {
        for p in all {
            println!(
                "{}  {:<10} {}",
                short(p.id),
                format!("{:?}", p.role).to_lowercase(),
                p.name
            );
        }
    }
    Ok(EXIT_OK)
}

pub struct AddArgs {
    pub project: Option<String>,
    pub title: Option<String>,
    pub template: Option<String>,
    pub plan: Option<bool>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_cost: Option<f64>,
    pub text: Vec<String>,
    pub json: bool,
}

pub async fn add(api: &Api, a: AddArgs) -> Result<Task> {
    let project = resolve_project(api, a.project.as_deref()).await?;
    let mut body = CreateTask {
        project_id: project.id,
        title: a.title.clone(),
        prompt: String::new(),
        context: String::new(),
        provider: a.provider.clone(),
        model: a.model.clone(),
        parent_id: None,
        connector_id: None,
        front: false,
        allow_test_changes: false,
        plan_mode: a.plan,
        limits: TaskLimitFields {
            max_cost_usd: a.max_cost.map(Some),
            ..Default::default()
        },
    };
    match &a.template {
        Some(name) => {
            let all: Vec<TemplateView> = api
                .get(&format!("/projects/{}/templates", project.id))
                .await?;
            let t = all
                .iter()
                .find(|t| t.id.to_string() == *name || t.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    exit(
                        EXIT_USAGE,
                        format!("Vorlage „{name}“ gibt es in „{}“ nicht", project.name),
                    )
                })?;
            // Extra words are appended to the template's description.
            let extra = if a.text.is_empty() {
                String::new()
            } else {
                text_arg(&a.text, "Text")?
            };
            body.prompt = if extra.is_empty() {
                t.prompt.clone()
            } else {
                format!("{}\n\n{extra}", t.prompt)
            };
            body.title = a.title.clone().or(Some(t.title.clone()));
            body.provider = a.provider.clone().or(t.provider.clone());
            body.model = a.model.clone().or(t.model.clone());
            body.plan_mode = a.plan.or(t.plan_mode);
            body.limits = TaskLimitFields {
                max_runtime_secs: t.max_runtime_secs.map(|v| Some(i64::from(v))),
                max_cost_usd: a.max_cost.or(t.max_cost_usd).map(Some),
                max_tokens: t.max_tokens.map(Some),
                max_turns: t.max_turns.map(|v| Some(i64::from(v))),
            };
        }
        None => body.prompt = text_arg(&a.text, "Beschreibung des Auftrags")?,
    }
    let t: Task = api.post("/tasks", &body).await?;
    if a.json {
        print_json(&t);
    } else {
        println!(
            "Auftrag angelegt: {} „{}“ in {} ({})",
            t.id,
            t.title,
            project.name,
            status_label(&t.status)
        );
    }
    Ok(t)
}

pub async fn list(
    api: &Api,
    project: Option<&str>,
    status: Option<&str>,
    json: bool,
) -> Result<i32> {
    let mut tasks: Vec<Task> = match project {
        Some(p) => {
            let p = resolve_project(api, Some(p)).await?;
            api.get(&format!("/projects/{}/tasks", p.id)).await?
        }
        None => api.get("/tasks").await?,
    };
    if let Some(s) = status {
        if TaskStatus::parse(s).is_none() {
            return Err(exit(
                EXIT_USAGE,
                format!("Unbekannter Status „{s}“ (z. B. running, queued, done, failed)"),
            ));
        }
        tasks.retain(|t| t.status == s);
    }
    if json {
        print_json(&tasks);
        return Ok(EXIT_OK);
    }
    if tasks.is_empty() {
        println!("Keine Aufträge.");
    }
    for t in tasks {
        println!(
            "{}  {:<20} {}",
            short(t.id),
            status_label(&t.status),
            t.title
        );
    }
    Ok(EXIT_OK)
}

fn exit_for(status: &str) -> Option<i32> {
    match TaskStatus::parse(status) {
        Some(TaskStatus::Done) => Some(EXIT_OK),
        Some(s) if s.is_final() => Some(EXIT_FAILED),
        _ => None,
    }
}

pub async fn status(api: &Api, id: Uuid, json: bool) -> Result<i32> {
    let d: TaskDetail = api.get(&format!("/tasks/{id}")).await?;
    if json {
        print_json(&d);
        return Ok(EXIT_OK);
    }
    let t = &d.task;
    println!("Auftrag   {}", t.id);
    println!("Titel     {}", t.title);
    println!("Status    {}", status_label(&t.status));
    println!(
        "Anbieter  {}{}",
        t.provider,
        t.model
            .as_deref()
            .map(|m| format!(" / {m}"))
            .unwrap_or_default()
    );
    println!(
        "Erstellt  {}",
        t.created_at.with_timezone(&Local).format("%d.%m.%Y %H:%M")
    );
    if let Some(f) = t.finished_at {
        println!(
            "Fertig    {}",
            f.with_timezone(&Local).format("%d.%m.%Y %H:%M")
        );
    }
    if let Some(s) = t.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        println!("Ergebnis  {}", s.trim().replace('\n', "\n          "));
    }
    if let Some(e) = t.error.as_deref().filter(|s| !s.trim().is_empty()) {
        println!("Fehler    {}", e.trim());
    }
    for q in t.questions_list() {
        println!("Frage     {q}");
    }
    if let Some(pr) = &d.pull_request {
        if let Some(u) = &pr.url {
            println!("Pull Request {u}");
        }
    }
    Ok(EXIT_OK)
}

fn print_event(kind: &str, content: &str, at: chrono::DateTime<chrono::Utc>) {
    let line: String = content
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(300)
        .collect();
    if !line.is_empty() {
        println!(
            "[{}] {kind}: {line}",
            at.with_timezone(&Local).format("%H:%M:%S")
        );
    }
}

fn print_status(t: &Task) {
    println!(
        "[{}] Status: {}",
        Local::now().format("%H:%M:%S"),
        status_label(&t.status)
    );
    if TaskStatus::parse(&t.status).is_some_and(|s| s.needs_user()) {
        println!("    Der Auftrag braucht dich (in Queue oder mit „queuectl plan/steer“).");
    }
}

/// Follows a task until it ends: SSE with reconnect, or polling.
pub async fn watch(
    api: &Api,
    id: Uuid,
    poll: bool,
    interval: u64,
    timeout: Option<u64>,
) -> Result<i32> {
    let run = async {
        let d: TaskDetail = api.get(&format!("/tasks/{id}")).await?;
        let mut last_event = d.events.last().map(|e| e.id).unwrap_or(0);
        let mut last_status = d.task.status.clone();
        println!("Auftrag {} „{}“", d.task.id, d.task.title);
        print_status(&d.task);
        if let Some(code) = exit_for(&last_status) {
            return Ok(code);
        }
        loop {
            if poll {
                tokio::time::sleep(Duration::from_secs(interval.max(1))).await;
            } else {
                // One SSE connection until it ends (the server closes token
                // streams after a while), then fall through to a refetch.
                if let Some(code) = follow(api, id, &mut last_event, &mut last_status).await? {
                    return Ok(code);
                }
                // Connection ended: short pause, then refetch and reconnect.
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let d: TaskDetail = api.get(&format!("/tasks/{id}")).await?;
            let from = last_event;
            for e in d.events.iter().filter(|e| e.id > from) {
                print_event(&e.kind, &e.content, e.ts);
                last_event = e.id;
            }
            if d.task.status != last_status {
                last_status = d.task.status.clone();
                print_status(&d.task);
            }
            if let Some(code) = exit_for(&last_status) {
                return Ok(code);
            }
        }
    };
    match timeout {
        Some(s) => tokio::time::timeout(Duration::from_secs(s), run)
            .await
            .unwrap_or_else(|_| Err(exit(EXIT_FAILED, format!("Zeitlimit von {s} s erreicht")))),
        None => run.await,
    }
}

async fn follow(
    api: &Api,
    id: Uuid,
    last_event: &mut i64,
    last_status: &mut String,
) -> Result<Option<i32>> {
    let resp = api.events().await?;
    // Changes between the first fetch and the connection: refetch once.
    let d: TaskDetail = api.get(&format!("/tasks/{id}")).await?;
    let from = *last_event;
    for e in d.events.iter().filter(|e| e.id > from) {
        print_event(&e.kind, &e.content, e.ts);
        *last_event = e.id;
    }
    if d.task.status != *last_status {
        *last_status = d.task.status.clone();
        print_status(&d.task);
    }
    if let Some(code) = exit_for(last_status) {
        return Ok(Some(code));
    }
    let mut parser = SseParser::default();
    let mut body = resp.bytes_stream();
    // The server sends a keep-alive at least every 15 s: silence for a
    // minute means a dead connection – reconnect.
    while let Ok(Some(chunk)) = tokio::time::timeout(Duration::from_secs(60), body.next()).await {
        let Ok(chunk) = chunk else { break };
        for data in parser.push(&chunk) {
            match serde_json::from_str::<ServerEvent>(&data) {
                Ok(ServerEvent::TaskEvent { task_id, event })
                    if task_id == id && event.id > *last_event =>
                {
                    print_event(&event.kind, &event.content, event.ts);
                    *last_event = event.id;
                }
                Ok(ServerEvent::Task { task }) if task.id == id && task.status != *last_status => {
                    *last_status = task.status.clone();
                    print_status(&task);
                    if let Some(code) = exit_for(last_status) {
                        return Ok(Some(code));
                    }
                }
                Ok(ServerEvent::Resync) => return Ok(None),
                _ => {}
            }
        }
    }
    Ok(None)
}

pub async fn cancel(api: &Api, id: Uuid) -> Result<i32> {
    let _: Value = api.post(&format!("/tasks/{id}/cancel"), &json!({})).await?;
    println!("Abbruch angefordert: {id}");
    Ok(EXIT_OK)
}

pub async fn steer(api: &Api, id: Uuid, text: String, now: bool) -> Result<i32> {
    let _: Value = api
        .post(
            &format!("/tasks/{id}/messages"),
            &SendMessage { body: text, now },
        )
        .await?;
    println!(
        "Nachricht eingereiht{} – sie lässt sich bis zur Zustellung in Queue zurückziehen.",
        if now { " (wird sofort zugestellt)" } else { "" }
    );
    Ok(EXIT_OK)
}

pub async fn continue_task(api: &Api, id: Uuid, text: String, plan: Option<bool>) -> Result<Task> {
    let t: Task = api
        .post(
            &format!("/tasks/{id}/continue"),
            &ContinueTask {
                message: text,
                plan_mode: plan,
            },
        )
        .await?;
    println!("Fortsetzung angelegt: {} „{}“", t.id, t.title);
    Ok(t)
}

pub enum PlanAction {
    Approve,
    Revise(String),
    Reject,
}

pub async fn plan(api: &Api, id: Uuid, a: PlanAction) -> Result<i32> {
    let msg = match a {
        PlanAction::Approve => {
            let _: Value = api
                .post(
                    &format!("/tasks/{id}/plan/approve"),
                    &PlanApprove { plan: None },
                )
                .await?;
            "Plan genehmigt – der Agent setzt ihn jetzt um."
        }
        PlanAction::Revise(note) => {
            let _: Value = api
                .post(&format!("/tasks/{id}/plan/revise"), &PlanRevise { note })
                .await?;
            "Neuer Plan angefordert."
        }
        PlanAction::Reject => {
            let _: Value = api
                .post(&format!("/tasks/{id}/plan/reject"), &json!({}))
                .await?;
            "Plan verworfen, Auftrag abgebrochen."
        }
    };
    println!("{msg}");
    Ok(EXIT_OK)
}

pub async fn fire(api: &Api, trigger: Uuid, text: Option<String>, json: bool) -> Result<i32> {
    let (status, v) = api
        .post_raw(&format!("/triggers/{trigger}/fire"), &FireRequest { text })
        .await?;
    let r: Option<FireResult> = serde_json::from_value(v.clone()).ok();
    if json {
        print_json(&v);
    } else {
        match &r {
            Some(r) => {
                println!(
                    "{}{}",
                    r.outcome.label(),
                    r.reason
                        .as_deref()
                        .map(|x| format!(": {x}"))
                        .unwrap_or_default()
                );
                if let Some(t) = r.task_id {
                    println!("Auftrag: {t}");
                }
            }
            None => println!(
                "Fehler (HTTP {status}): {}",
                v.get("error").and_then(Value::as_str).unwrap_or_default()
            ),
        }
    }
    Ok(if (200..300).contains(&status) {
        EXIT_OK
    } else {
        EXIT_FAILED
    })
}
