//! Single-shot steps as jobs (WP13): plan, review and OnEmpty analysis,
//! either one synchronous call or a provider batch (−50 %, up to 24 h).
//!
//! * Jobs live in `batch_jobs` (survive restarts); the task waits in
//!   `waiting_batch`, which occupies no scheduler slot and locks no project.
//! * A separate poller loop submits, polls and completes jobs; it never
//!   blocks the scheduler.
//! * Batch wire formats (Anthropic Message Batches, OpenAI Batch, Mistral
//!   Batch) are prior knowledge and only tested against fake servers.
use std::time::Duration;

use chrono::{DateTime, Utc};
use queued_api::{StepMode, Usage};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    agent::ErrorCategory,
    budget,
    connectors::{self, BatchApi, Resolved},
    llm::{self, CallError, Endpoint, Request, Response},
    models::{status, Task},
    notify::{notify, Note},
    pricing::{self, Meter, PriceCtx, Tier},
    queue::{self, event},
    runner,
    state::AppState,
};

/// Transient failures (rate limit, overload, network) are retried this often.
pub const MAX_ATTEMPTS: i32 = 6;
/// Batches expire at the provider after 24 h; we give up a little later.
pub const EXPIRY_HOURS: i64 = 25;

pub struct JobSpec<'a> {
    pub task: &'a Task,
    pub step: &'static str,
    pub mode: StepMode,
    pub connector: &'a Resolved,
    pub request: Request,
    pub payload: Value,
}

fn api_of(mode: StepMode, c: &Resolved) -> Option<&'static str> {
    match mode {
        StepMode::Batch => c.batch_api().map(BatchApi::as_str),
        StepMode::Sync => match c.wire()?.0 {
            connectors::Wire::Anthropic => Some("anthropic"),
            connectors::Wire::OpenAiChat => Some("openai_chat"),
        },
        _ => None,
    }
}

fn request_json(r: &Request) -> Value {
    json!({"model": r.model, "system": r.system, "user": r.user, "max_tokens": r.max_tokens, "cache_long": r.cache_long})
}

fn request_of(v: &Value) -> Request {
    Request {
        model: v["model"].as_str().unwrap_or_default().into(),
        system: v["system"].as_str().unwrap_or_default().into(),
        user: v["user"].as_str().unwrap_or_default().into(),
        max_tokens: v["max_tokens"].as_u64().unwrap_or(4096).min(64_000) as u32,
        cache_long: v["cache_long"].as_bool().unwrap_or(false),
    }
}

/// Creates a job and moves the (running) task to `waiting_batch`.
pub async fn create(st: &AppState, s: JobSpec<'_>) -> anyhow::Result<Uuid> {
    let api = api_of(s.mode, s.connector)
        .ok_or_else(|| anyhow::anyhow!("{} unterstützt diesen Modus nicht", s.connector.label))?;
    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "INSERT INTO batch_jobs (id, task_id, user_id, step, mode, api, connector_id, model, status, request, payload)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'pending',$9,$10)",
    )
    .bind(id)
    .bind(s.task.id)
    .bind(s.task.user_id)
    .bind(s.step)
    .bind(s.mode.as_str())
    .bind(api)
    .bind(s.connector.id)
    .bind(&s.request.model)
    .bind(request_json(&s.request))
    .bind(&s.payload)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE tasks SET status='waiting_batch', started_at=NULL, updated_at=now() WHERE id=$1 AND status='running'",
    )
    .bind(s.task.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let how = if s.mode == StepMode::Batch {
        "Batch (−50 %, kann bis zu 24 h dauern)"
    } else {
        "direkter Aufruf"
    };
    event(
        st,
        s.task,
        "batch",
        &format!(
            "{}: {} über {} – {how}",
            step_label(s.step),
            s.request.model,
            s.connector.label
        ),
    )
    .await;
    queue::emit_task(st, s.task.id).await;
    st.batch_wake.notify_one();
    Ok(id)
}

pub fn step_label(step: &str) -> &'static str {
    match step {
        "plan" => "Planung",
        "review" => "Review",
        "analyze" => "Analyse",
        _ => "Schritt",
    }
}

/// Startup: leases of a previous process are void (single app instance).
pub async fn recover(st: &AppState) -> sqlx::Result<()> {
    sqlx::query("UPDATE batch_jobs SET next_poll_at=now() WHERE status IN ('pending','submitted')")
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Tasks left in `waiting_batch` without an open job – the process died
/// between finishing the job and continuing the task – are blocked visibly
/// instead of waiting forever (2 minutes grace after the last job ended).
pub async fn sweep_orphans(st: &AppState) -> sqlx::Result<()> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE tasks t SET status='blocked', failure_class='infra', finished_at=now(), updated_at=now(),
                error='Ein Planungs-/Prüf-/Analyseschritt ist verloren gegangen (Neustart?). Bitte den Auftrag erneut starten.'
         WHERE t.status='waiting_batch' AND t.updated_at < now() - interval '2 minutes'
           AND NOT EXISTS (SELECT 1 FROM batch_jobs j WHERE j.task_id=t.id
                           AND (j.status IN ('pending','submitted') OR j.finished_at > now() - interval '2 minutes'))
         RETURNING t.id",
    )
    .fetch_all(&st.db)
    .await?;
    for id in ids {
        tracing::warn!("task {id}: orphaned in waiting_batch, blocked");
        queue::emit_task(st, id).await;
    }
    Ok(())
}

pub async fn run(st: AppState) {
    loop {
        if let Err(e) = tick(&st).await {
            tracing::error!("batch poller tick failed: {e:#}");
        }
        tokio::select! {
            _ = st.batch_wake.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(st.cfg.batch_poll_secs.max(1))) => {}
        }
    }
}

#[derive(sqlx::FromRow, Clone)]
struct Job {
    id: Uuid,
    task_id: Uuid,
    step: String,
    mode: String,
    api: String,
    connector_id: Option<Uuid>,
    model: String,
    status: String,
    external_id: Option<String>,
    request: Value,
    payload: Value,
    attempts: i32,
    created_at: DateTime<Utc>,
}

async fn tick(st: &AppState) -> anyhow::Result<()> {
    sweep_orphans(st).await?;
    // Lease (15 min) so a slow call is not picked twice.
    let jobs: Vec<Job> = sqlx::query_as(
        "UPDATE batch_jobs SET next_poll_at = now() + interval '15 minutes'
         WHERE id IN (SELECT id FROM batch_jobs WHERE status IN ('pending','submitted') AND next_poll_at <= now()
                      ORDER BY next_poll_at LIMIT 20 FOR UPDATE SKIP LOCKED)
         RETURNING id, task_id, step, mode, api, connector_id, model, status, external_id, request, payload, attempts, created_at",
    )
    .fetch_all(&st.db)
    .await?;
    for j in jobs {
        let st = st.clone();
        tokio::spawn(async move {
            let id = j.id;
            if let Err(e) = process(&st, j).await {
                tracing::error!("batch job {id}: {e:#}");
                // Retry after the normal poll interval.
                let _ = sqlx::query("UPDATE batch_jobs SET next_poll_at = now() + make_interval(secs => $2) WHERE id=$1 AND status IN ('pending','submitted')")
                    .bind(id)
                    .bind(st.cfg.batch_poll_secs.max(1) as f64)
                    .execute(&st.db)
                    .await;
            }
        });
    }
    Ok(())
}

async fn endpoint(st: &AppState, j: &Job) -> anyhow::Result<Option<(Resolved, Endpoint)>> {
    let Some(cid) = j.connector_id else {
        return Ok(None);
    };
    let Some(row) = connectors::load_row(st, cid).await? else {
        return Ok(None);
    };
    let c = connectors::resolve(st, row)?;
    let Some((wire, base)) = c.wire() else {
        return Ok(None);
    };
    let ep = Endpoint {
        wire,
        base,
        kind: c.kind.clone(),
        secret: c.secret.clone(),
        guard: st.cfg.llm_guard(),
    };
    Ok(Some((c, ep)))
}

fn retryable(c: &ErrorCategory) -> bool {
    matches!(
        c,
        ErrorCategory::RateLimit(_)
            | ErrorCategory::Overloaded
            | ErrorCategory::ServerError
            | ErrorCategory::Network
            | ErrorCategory::Unknown(None)
    )
}

fn next_poll(st: &AppState, attempts: i32) -> f64 {
    let base = st.cfg.batch_poll_secs.max(1) as f64;
    (base * 2f64.powi(attempts.clamp(0, 6))).min(900.0)
}

async fn process(st: &AppState, j: Job) -> anyhow::Result<()> {
    let Some((conn, ep)) = endpoint(st, &j).await? else {
        return complete(
            st,
            &j,
            Err(CallError {
                category: ErrorCategory::InvalidRequest,
                message: "Zugang wurde gelöscht oder kann keine direkten Aufrufe".into(),
            }),
            None,
        )
        .await;
    };
    let http = llm::client(&st.cfg.llm_guard());
    let req = request_of(&j.request);
    match (j.mode.as_str(), j.status.as_str()) {
        ("sync", _) => {
            let r = llm::call(&http, &ep, &req).await;
            settle(st, &j, &conn, r).await
        }
        ("batch", "pending") => match submit(&http, &ep, &j, &req).await {
            Ok(ext) => {
                sqlx::query("UPDATE batch_jobs SET status='submitted', external_id=$2, attempts=0, next_poll_at = now() + make_interval(secs => $3) WHERE id=$1 AND status='pending'")
                    .bind(j.id)
                    .bind(&ext)
                    .bind(st.cfg.batch_poll_secs.max(1) as f64)
                    .execute(&st.db)
                    .await?;
                Ok(())
            }
            Err(e) => settle(st, &j, &conn, Err(e)).await,
        },
        ("batch", "submitted") => {
            let ext = j.external_id.clone().unwrap_or_default();
            if j.created_at < Utc::now() - chrono::Duration::hours(EXPIRY_HOURS) {
                let _ = cancel_remote(&http, &ep, &j.api, &ext).await;
                return complete(
                    st,
                    &j,
                    Err(CallError {
                        category: ErrorCategory::Unknown(None),
                        message: format!("Batch nach {EXPIRY_HOURS} h nicht fertig (abgelaufen)"),
                    }),
                    Some(&conn),
                )
                .await;
            }
            match poll(&http, &ep, &j.api, &ext, &j.id.to_string(), &j.model).await {
                Ok(None) => {
                    sqlx::query("UPDATE batch_jobs SET next_poll_at = now() + make_interval(secs => $2) WHERE id=$1")
                        .bind(j.id)
                        .bind(next_poll(st, j.attempts.min(4)))
                        .execute(&st.db)
                        .await?;
                    Ok(())
                }
                // A failed single request inside a finished batch is final.
                Ok(Some(r)) => complete(st, &j, r, Some(&conn)).await,
                Err(e) => settle(st, &j, &conn, Err(e)).await,
            }
        }
        _ => Ok(()),
    }
}

/// Retries transient failures, completes everything else.
async fn settle(
    st: &AppState,
    j: &Job,
    conn: &Resolved,
    r: Result<Response, CallError>,
) -> anyhow::Result<()> {
    if let Err(e) = &r {
        if retryable(&e.category) && j.attempts + 1 < MAX_ATTEMPTS {
            sqlx::query("UPDATE batch_jobs SET attempts=attempts+1, error=$2, next_poll_at = now() + make_interval(secs => $3) WHERE id=$1")
                .bind(j.id)
                .bind(&e.message)
                .bind(next_poll(st, j.attempts + 1))
                .execute(&st.db)
                .await?;
            return Ok(());
        }
    }
    complete(st, j, r, Some(conn)).await
}

/// Final state of a job; continues the task.
async fn complete(
    st: &AppState,
    j: &Job,
    r: Result<Response, CallError>,
    conn: Option<&Resolved>,
) -> anyhow::Result<()> {
    let (status, text, usage, err) = match &r {
        Ok(resp) => (
            "succeeded",
            Some(resp.text.clone()),
            Some(resp.usage.clone()),
            None,
        ),
        Err(e) => ("failed", None, None, Some(e.to_string())),
    };
    let claimed = sqlx::query(
        "UPDATE batch_jobs SET status=$2, result=$3, usage=$4, error=$5, finished_at=now() WHERE id=$1 AND status IN ('pending','submitted')",
    )
    .bind(j.id)
    .bind(status)
    .bind(&text)
    .bind(usage.as_ref().map(|u| serde_json::to_value(u).unwrap_or_default()))
    .bind(&err)
    .execute(&st.db)
    .await?
    .rows_affected();
    if claimed == 0 {
        return Ok(()); // cancelled meanwhile
    }
    let Some(task) = queue::load_task(st, j.task_id).await? else {
        return Ok(());
    };
    if task.status != status::WAITING_BATCH {
        return Ok(());
    }
    if let Err(e) = &r {
        event(st, &task, "error", &format!("{}: {e}", step_label(&j.step))).await;
        if let (Some(c), ErrorCategory::Auth | ErrorCategory::Billing) = (conn, &e.category) {
            pause_connector(st, &task, c, &e.message).await;
        }
    }
    // Money cost of the call.
    let mut total: Usage = task.usage_parsed().unwrap_or_default();
    if let (Ok(resp), Some(c)) = (&r, conn) {
        let cat = pricing::current(st).await;
        let ctx = PriceCtx {
            tier: if j.mode == "batch" {
                Tier::Batch
            } else {
                Tier::Standard
            },
            region_mult: c
                .region_rule()
                .and_then(|id| pricing::region(&cat, id))
                .map_or(1.0, |r| r.multiplier),
            long_ttl: j.mode == "batch",
        };
        let step = match j.step.as_str() {
            "plan" => "plan",
            "review" => "review",
            _ => "analyze",
        };
        let mut m = Meter::new(&cat, Some(&j.model), ctx, step, Some(c.label.clone()));
        m.on_delta(&resp.usage);
        let sc = m.finish(&resp.usage);
        runner::record_cost(st, &task, &c.harness, Some(c.id), &sc).await?;
        total.add(&sc.usage);
        sqlx::query("UPDATE tasks SET usage=$2 WHERE id=$1")
            .bind(task.id)
            .bind(serde_json::to_value(&total).unwrap_or_default())
            .execute(&st.db)
            .await?;
    }
    match j.step.as_str() {
        // WP06 plan mode: the plan waits for a human instead of running at once.
        "plan" if crate::plan::pending(&task) => {
            match &r {
                Ok(resp) if !resp.text.trim().is_empty() => {
                    let meta = queued_api::steer::PlanMeta {
                        source: queued_api::steer::PlanSource::SingleShot,
                        model: Some(j.model.clone()),
                        ..Default::default()
                    };
                    crate::plan::hold(st, &task, "waiting_batch", &resp.text, meta, &total).await?;
                }
                _ => {
                    // Plan mode: without a plan nothing is implemented.
                    let msg = "Planung per API ergab keinen Plan – der Auftrag wartet (Plan-Modus: ohne genehmigten Plan wird nichts umgesetzt). „Wiederholen“ versucht es erneut.";
                    event(st, &task, "plan", msg).await;
                    sqlx::query(
                        "UPDATE tasks SET status='blocked', failure_class='task', error=$2, plan_state=NULL,
                                finished_at=now(), updated_at=now() WHERE id=$1 AND status='waiting_batch'",
                    )
                    .bind(task.id)
                    .bind(msg)
                    .execute(&st.db)
                    .await?;
                    queue::emit_task(st, task.id).await;
                }
            }
            st.wake.notify_one();
        }
        "plan" => {
            let plan = match &r {
                Ok(resp) if !resp.text.trim().is_empty() => {
                    event(
                        st,
                        &task,
                        "plan",
                        &format!("Plan erstellt ({}):\n{}", j.model, cut(&resp.text, 3000)),
                    )
                    .await;
                    cut(resp.text.trim(), 12_000)
                }
                Ok(_) => {
                    event(
                        st,
                        &task,
                        "plan",
                        "Planung lieferte keinen Text – weiter ohne Plan",
                    )
                    .await;
                    String::new()
                }
                Err(_) => {
                    event(
                        st,
                        &task,
                        "plan",
                        "Planung übersprungen – der Agent arbeitet ohne Plan",
                    )
                    .await;
                    String::new()
                }
            };
            sqlx::query("UPDATE tasks SET plan=$2, status='queued', updated_at=now() WHERE id=$1 AND status='waiting_batch'")
                .bind(task.id)
                .bind(plan)
                .execute(&st.db)
                .await?;
            queue::emit_task(st, task.id).await;
            st.wake.notify_one();
        }
        "review" => {
            let outcome = match &r {
                Ok(resp) => llm::extract_json::<queued_api::ReviewResult>(&resp.text)
                    .ok_or_else(|| "Review-Antwort war kein gültiges JSON".to_string()),
                Err(_) => Err("der Aufruf ist fehlgeschlagen (Details im Protokoll)".to_string()),
            };
            runner::complete_review(st, &task, &j.payload, outcome, &total).await?;
        }
        _ => {
            let outcome = match &r {
                Ok(resp) => llm::extract_json::<crate::models::AgentResult>(&resp.text)
                    .filter(|c| c.has_valid_status())
                    .ok_or_else(|| "Analyse-Antwort war kein gültiges Ergebnis-JSON".to_string()),
                Err(e) => Err(e.to_string()),
            };
            runner::complete_analyze(st, &task, outcome, &total).await?;
        }
    }
    Ok(())
}

fn cut(s: &str, n: usize) -> String {
    let mut end = n.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

async fn pause_connector(st: &AppState, task: &Task, c: &Resolved, msg: &str) {
    let reason: String = msg.chars().take(300).collect();
    if let Err(e) = budget::set_paused(&st.db, task.user_id, &c.limit_key(), &reason).await {
        tracing::error!("pause connector failed: {e:#}");
        return;
    }
    notify(
        st,
        task.user_id,
        Note {
            kind: "provider_paused",
            title: format!("{} pausiert: Anmeldung/Abrechnung", c.label),
            body: format!("{reason}\nSchlüssel des Zugangs erneuern oder die Pause aufheben."),
            task_id: Some(task.id),
            push: true,
        },
    )
    .await;
}

/// Cancels the open jobs of a task (user cancel); provider cancel is best effort.
pub async fn cancel_for_task(st: &AppState, task_id: Uuid) -> anyhow::Result<()> {
    let jobs: Vec<Job> = sqlx::query_as(
        "UPDATE batch_jobs SET status='cancelled', finished_at=now() WHERE task_id=$1 AND status IN ('pending','submitted')
         RETURNING id, task_id, step, mode, api, connector_id, model, status, external_id, request, payload, attempts, created_at",
    )
    .bind(task_id)
    .fetch_all(&st.db)
    .await?;
    for j in jobs {
        let Some(ext) = j.external_id.clone() else {
            continue;
        };
        let st = st.clone();
        tokio::spawn(async move {
            if let Ok(Some((_, ep))) = endpoint(&st, &j).await {
                if let Err(e) =
                    cancel_remote(&llm::client(&st.cfg.llm_guard()), &ep, &j.api, &ext).await
                {
                    tracing::warn!("batch cancel {ext}: {e}");
                }
            }
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provider batch APIs
// ---------------------------------------------------------------------------

fn jsonl_line(api: &str, custom_id: &str, r: &Request) -> Value {
    match api {
        "openai" => {
            json!({"custom_id": custom_id, "method": "POST", "url": "/v1/chat/completions", "body": llm::chat_body(r, true)})
        }
        // Mistral: the model is set on the job.
        _ => json!({"custom_id": custom_id, "body": llm::chat_body(r, false)}),
    }
}

async fn upload(http: &reqwest::Client, ep: &Endpoint, line: &Value) -> Result<String, CallError> {
    let part = reqwest::multipart::Part::bytes(format!("{line}\n").into_bytes())
        .file_name("queue-batch.jsonl")
        .mime_str("application/jsonl")
        .map_err(|e| CallError {
            category: ErrorCategory::InvalidRequest,
            message: e.to_string(),
        })?;
    let form = reqwest::multipart::Form::new()
        .text("purpose", "batch")
        .part("file", part);
    let v: Value = llm::send_json(
        ep,
        ep.req(http, reqwest::Method::POST, &format!("{}/files", ep.base))
            .multipart(form),
    )
    .await?;
    id_of(&v)
}

fn id_of(v: &Value) -> Result<String, CallError> {
    v["id"].as_str().map(String::from).ok_or_else(|| CallError {
        category: ErrorCategory::Unknown(None),
        message: format!(
            "Antwort ohne id: {}",
            v.to_string().chars().take(200).collect::<String>()
        ),
    })
}

async fn submit(
    http: &reqwest::Client,
    ep: &Endpoint,
    j: &Job,
    r: &Request,
) -> Result<String, CallError> {
    let cid = j.id.to_string();
    match j.api.as_str() {
        "anthropic" => {
            let body = json!({"requests": [{"custom_id": cid, "params": llm::anthropic_body(r)}]});
            let v: Value = llm::send_json(
                ep,
                ep.req(
                    http,
                    reqwest::Method::POST,
                    &format!("{}/v1/messages/batches", ep.base),
                )
                .json(&body),
            )
            .await?;
            id_of(&v)
        }
        "openai" => {
            let file = upload(http, ep, &jsonl_line("openai", &cid, r)).await?;
            let body = json!({"input_file_id": file, "endpoint": "/v1/chat/completions", "completion_window": "24h"});
            let v: Value = llm::send_json(
                ep,
                ep.req(http, reqwest::Method::POST, &format!("{}/batches", ep.base))
                    .json(&body),
            )
            .await?;
            id_of(&v)
        }
        "mistral" => {
            let file = upload(http, ep, &jsonl_line("mistral", &cid, r)).await?;
            let body = json!({"input_files": [file], "endpoint": "/v1/chat/completions", "model": r.model});
            let v: Value = llm::send_json(
                ep,
                ep.req(
                    http,
                    reqwest::Method::POST,
                    &format!("{}/batch/jobs", ep.base),
                )
                .json(&body),
            )
            .await?;
            id_of(&v)
        }
        a => Err(CallError {
            category: ErrorCategory::InvalidRequest,
            message: format!("Batch-API {a} unbekannt"),
        }),
    }
}

fn final_error(msg: String) -> Result<Response, CallError> {
    Err(CallError {
        category: ErrorCategory::InvalidRequest,
        message: msg,
    })
}

/// `Ok(None)` = still running.
async fn poll(
    http: &reqwest::Client,
    ep: &Endpoint,
    api: &str,
    ext: &str,
    custom_id: &str,
    model: &str,
) -> Result<Option<Result<Response, CallError>>, CallError> {
    let get = |url: String| ep.req(http, reqwest::Method::GET, &url);
    let _ = model;
    match api {
        "anthropic" => {
            let v: Value =
                llm::send_json(ep, get(format!("{}/v1/messages/batches/{ext}", ep.base))).await?;
            match v["processing_status"].as_str() {
                Some("ended") => {}
                _ => return Ok(None),
            }
            // Only follow a results URL on the same endpoint (the API key is sent along).
            let url = match v["results_url"].as_str() {
                Some(u) if u.starts_with(&ep.base) => u.to_string(),
                _ => format!("{}/v1/messages/batches/{ext}/results", ep.base),
            };
            let text = fetch_text(ep, get(url)).await?;
            Ok(Some(match line_for(&text, custom_id) {
                Some(l) => match l["result"]["type"].as_str() {
                    Some("succeeded") => {
                        llm::parse_anthropic(&l["result"]["message"]).ok_or_else(|| CallError {
                            category: ErrorCategory::Unknown(None),
                            message: "Batch-Ergebnis ohne Nachricht".into(),
                        })
                    }
                    Some("errored") => final_error(format!(
                        "Batch-Anfrage fehlgeschlagen: {}",
                        l["result"]["error"]
                    )),
                    Some(other) => final_error(format!("Batch-Anfrage {other}")),
                    None => final_error("Batch-Ergebnis unlesbar".into()),
                },
                None => final_error("Batch-Ergebnis fehlt".into()),
            }))
        }
        "openai" | "mistral" => {
            let path = if api == "openai" {
                format!("{}/batches/{ext}", ep.base)
            } else {
                format!("{}/batch/jobs/{ext}", ep.base)
            };
            let v: Value = llm::send_json(ep, get(path)).await?;
            let st = v["status"].as_str().unwrap_or_default().to_lowercase();
            let output = if api == "openai" {
                v["output_file_id"].as_str()
            } else {
                v["output_file"].as_str()
            };
            match st.as_str() {
                "completed" | "success" => {}
                "failed" | "expired" | "cancelled" | "timeout_exceeded" => {
                    return Ok(Some(final_error(format!(
                        "Batch beendet mit Status „{st}“"
                    ))));
                }
                _ => return Ok(None),
            }
            let Some(file) = output else {
                return Ok(Some(final_error("Batch ohne Ergebnisdatei".into())));
            };
            let text = fetch_text(ep, get(format!("{}/files/{file}/content", ep.base))).await?;
            Ok(Some(match line_for(&text, custom_id) {
                Some(l) => {
                    let code = l["response"]["status_code"].as_u64().unwrap_or(0);
                    if (200..300).contains(&code) {
                        llm::parse_chat(&l["response"]["body"]).ok_or_else(|| CallError {
                            category: ErrorCategory::Unknown(None),
                            message: "Batch-Ergebnis ohne Text".into(),
                        })
                    } else {
                        final_error(format!(
                            "Batch-Anfrage fehlgeschlagen (HTTP {code}): {}",
                            l["error"]
                        ))
                    }
                }
                None => final_error("Batch-Ergebnis fehlt".into()),
            }))
        }
        a => Err(CallError {
            category: ErrorCategory::InvalidRequest,
            message: format!("Batch-API {a} unbekannt"),
        }),
    }
}

async fn fetch_text(ep: &Endpoint, rb: reqwest::RequestBuilder) -> Result<String, CallError> {
    let rb = llm::guarded(ep, rb).await?;
    let resp = rb.send().await.map_err(|e| llm::net_error(&e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| llm::net_error(&e))?;
    if !(200..300).contains(&status) {
        let mut c = llm::http_error(status, &text);
        c.message = ep.redact(&c.message);
        return Err(c);
    }
    Ok(text)
}

fn line_for(jsonl: &str, custom_id: &str) -> Option<Value> {
    jsonl
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["custom_id"].as_str() == Some(custom_id))
}

async fn cancel_remote(
    http: &reqwest::Client,
    ep: &Endpoint,
    api: &str,
    ext: &str,
) -> Result<(), CallError> {
    let url = match api {
        "anthropic" => format!("{}/v1/messages/batches/{ext}/cancel", ep.base),
        "openai" => format!("{}/batches/{ext}/cancel", ep.base),
        "mistral" => format!("{}/batch/jobs/{ext}/cancel", ep.base),
        _ => return Ok(()),
    };
    llm::send_json::<Value>(ep, ep.req(http, reqwest::Method::POST, &url))
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_lines_per_api() {
        let r = Request {
            model: "m".into(),
            system: "s".into(),
            user: "u".into(),
            max_tokens: 10,
            cache_long: true,
        };
        let o = jsonl_line("openai", "id1", &r);
        assert_eq!(
            (o["url"].as_str(), o["body"]["model"].as_str()),
            (Some("/v1/chat/completions"), Some("m"))
        );
        let m = jsonl_line("mistral", "id1", &r);
        assert!(m["body"].get("model").is_none() && m.get("url").is_none());
        let back = request_of(&request_json(&r));
        assert_eq!(back, r);
    }

    #[test]
    fn result_lines_are_found_by_custom_id() {
        let t = "{\"custom_id\":\"a\",\"x\":1}\nnot json\n{\"custom_id\":\"b\",\"x\":2}\n";
        assert_eq!(line_for(t, "b").unwrap()["x"], 2);
        assert!(line_for(t, "c").is_none());
        assert!(retryable(&ErrorCategory::RateLimit(None)) && !retryable(&ErrorCategory::Auth));
    }
}
