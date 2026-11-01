//! Executes one task: fresh agent → result contract → verify → review →
//! git commit → done | waiting_input | failed (+ fix task) | blocked.
//! All file/process work happens in queued-runner.
use anyhow::Result;
use chrono::{DateTime, Utc};
use queued_common::{ExecFrame, ExecRequest, GitRemote};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    agent::{self, AgentEvent, Credential, ParseState},
    budget, crypto,
    models::{status, AgentResult, FollowUp, Project, ReviewResult, Settings, Task, Usage},
    notify::{notify, Note},
    prompt::{self, Parent},
    queue::{self, event, finish, Finish, NewTask},
    state::AppState,
};

/// Agent-generated tasks deeper than this always need approval (loop guard).
const MAX_AUTO_DEPTH: i32 = 3;

pub async fn run_task(st: AppState, task: Task, cancel: CancellationToken) {
    let id = task.id;
    if let Err(e) = run_inner(&st, &task, &cancel).await {
        let msg = format!("{e:#}");
        tracing::error!("task {id} crashed: {msg}");
        event(&st, &task, "error", &msg).await;
        let _ = sqlx::query("UPDATE tasks SET status='failed', error=$2, finished_at=now(), updated_at=now() WHERE id=$1 AND status='running'")
            .bind(id)
            .bind(&msg)
            .execute(&st.db)
            .await;
        queue::emit_task(&st, id).await;
        notify(
            &st,
            task.user_id,
            Note {
                kind: "task_failed",
                title: format!("Interner Fehler: {}", task.title),
                body: msg,
                task_id: Some(id),
                push: true,
            },
        )
        .await;
    }
    st.running.lock().await.remove(&id);
    st.wake.notify_one();
}

#[derive(Default)]
struct RunOutcome {
    success: bool,
    timed_out: bool,
    cancelled: bool,
    usage: Usage,
    final_text: String,
    error: Option<String>,
    rate_limited: Option<Option<DateTime<Utc>>>,
    stderr_tail: String,
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn tail(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

pub fn project_remote(st: &AppState, p: &Project) -> Option<GitRemote> {
    let url = p.git_remote_url.clone().filter(|u| !u.trim().is_empty())?;
    let token = p
        .git_token_enc
        .as_deref()
        .and_then(|e| crypto::decrypt(&st.cfg.master_key, e).ok())
        .and_then(|b| String::from_utf8(b).ok());
    Some(GitRemote {
        url,
        branch: p.git_branch.clone(),
        token,
    })
}

fn run_name(task: Uuid, label: &str) -> String {
    format!("q-{}-{label}", &task.simple().to_string()[..12])
}

async fn run_inner(st: &AppState, task: &Task, cancel: &CancellationToken) -> Result<()> {
    let project: Project = sqlx::query_as("SELECT * FROM projects WHERE id=$1")
        .bind(task.project_id)
        .fetch_one(&st.db)
        .await?;
    let settings = queue::load_settings(st, task.user_id).await?;
    let (author_name, author_email): (String, String) =
        sqlx::query_as("SELECT name, email FROM users WHERE id=$1")
            .bind(task.user_id)
            .fetch_one(&st.db)
            .await?;
    event(
        st,
        task,
        "status",
        &format!("Start #{} via {}", task.attempt, task.provider),
    )
    .await;

    let Some(cred) = queue::load_credential(st, task.user_id, &task.provider).await? else {
        let msg = format!(
            "Keine Zugangsdaten für '{}' hinterlegt (Einstellungen → Provider).",
            task.provider
        );
        finish(
            st,
            task.id,
            Finish {
                status: status::BLOCKED,
                summary: None,
                handoff: None,
                error: Some(msg.clone()),
                review: None,
                questions: None,
                usage: &Usage::default(),
            },
        )
        .await?;
        notify(
            st,
            task.user_id,
            Note {
                kind: "blocked",
                title: format!("Blockiert: {}", task.title),
                body: msg,
                task_id: Some(task.id),
                push: true,
            },
        )
        .await;
        return Ok(());
    };

    let remote = project_remote(st, &project);
    st.runner
        .init(
            project.id,
            &queued_common::InitWorkspace {
                remote: remote.clone(),
                branch: Some(project.git_branch.clone()),
            },
        )
        .await?;
    let begin = st.runner.begin(project.id, remote.clone()).await?;
    for w in &begin.warnings {
        event(st, task, "status", &format!("git: {w}")).await;
    }
    let before = begin.head;
    sqlx::query("UPDATE tasks SET snapshot_before=$2 WHERE id=$1")
        .bind(task.id)
        .bind(&before)
        .execute(&st.db)
        .await?;

    let prompt = if task.kind == "onempty" {
        let history: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT title, summary FROM tasks WHERE project_id=$1 AND status='done' AND kind<>'onempty' ORDER BY finished_at DESC LIMIT 8",
        )
        .bind(project.id)
        .fetch_all(&st.db)
        .await?;
        prompt::onempty(&project, &settings.onempty_actions, &history)
    } else {
        let parent = match task.parent_id {
            Some(pid) => sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                "SELECT title, handoff, summary FROM tasks WHERE id=$1",
            )
            .bind(pid)
            .fetch_optional(&st.db)
            .await?
            .map(|(title, handoff, summary)| Parent {
                title,
                handoff,
                summary,
            }),
            None => None,
        };
        prompt::task(task, &project, parent.as_ref())
    };

    let mut usage = Usage::default();
    let out = agent_run(
        st,
        task,
        &task.provider,
        task.model.as_deref(),
        &cred,
        prompt,
        cancel,
        "agent",
    )
    .await?;
    usage.add(&out.usage);
    budget::record_usage(
        &st.db,
        task.user_id,
        &task.provider,
        Some(task.id),
        &out.usage,
    )
    .await?;
    check_budget(st, task.user_id, &task.provider).await;

    let commit_msg = |suffix: &str| {
        format!(
            "{}{suffix}\n\nQueue-Task: {}\nProvider: {}",
            task.title, task.id, task.provider
        )
    };

    if out.cancelled {
        st.runner
            .commit(
                project.id,
                &commit_msg(" (abgebrochen)"),
                &author_name,
                &author_email,
            )
            .await?;
        finish(
            st,
            task.id,
            Finish {
                status: status::CANCELLED,
                summary: None,
                handoff: None,
                error: Some("abgebrochen".into()),
                review: None,
                questions: None,
                usage: &usage,
            },
        )
        .await?;
        return Ok(());
    }

    if let Some(reset) = out.rate_limited {
        let until = match reset {
            Some(t) if t > Utc::now() => t,
            _ => {
                budget::fallback_reset(
                    &st.db,
                    task.user_id,
                    &task.provider,
                    st.cfg.limit_fallback_min,
                )
                .await
            }
        };
        budget::set_limited(&st.db, task.user_id, &task.provider, until).await?;
        st.runner
            .commit(
                project.id,
                &commit_msg(" (unterbrochen: Limit)"),
                &author_name,
                &author_email,
            )
            .await?;
        sqlx::query(
            "UPDATE tasks SET status='queued', started_at=NULL, updated_at=now(), usage=$2,
                    context = context || E'\\n\\n(Note: a previous run was interrupted by a usage limit; the repository may contain partial changes from it – see git log.)'
             WHERE id=$1",
        )
        .bind(task.id)
        .bind(serde_json::to_value(&usage)?)
        .execute(&st.db)
        .await?;
        event(
            st,
            task,
            "limit",
            &format!(
                "Limit erreicht – {} pausiert bis {}",
                task.provider,
                until.to_rfc3339()
            ),
        )
        .await;
        queue::emit_task(st, task.id).await;
        notify(
            st,
            task.user_id,
            Note {
                kind: "limit",
                title: format!("{}-Limit erreicht", task.provider),
                body: format!(
                    "Queue für {} pausiert bis {} UTC, danach geht es automatisch weiter.",
                    task.provider,
                    until.format("%d.%m. %H:%M")
                ),
                task_id: Some(task.id),
                push: true,
            },
        )
        .await;
        return Ok(());
    }

    let result: AgentResult = st
        .runner
        .queue_file(project.id, "result.json")
        .await?
        .and_then(|s| {
            serde_json::from_str(&s)
                .map_err(|e| tracing::warn!("invalid result.json: {e}"))
                .ok()
        })
        .unwrap_or_default();
    let summary = result
        .summary
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| cut(&out.final_text, 1500));
    let handoff = result.handoff.clone().map(|h| cut(&h, prompt::MAX_HANDOFF));
    let agent_status = result.status.clone().unwrap_or_else(|| {
        if out.success && out.error.is_none() {
            "done".into()
        } else {
            "failed".into()
        }
    });

    let after = st
        .runner
        .commit(project.id, &commit_msg(""), &author_name, &author_email)
        .await?;
    sqlx::query("UPDATE tasks SET snapshot_after=$2 WHERE id=$1")
        .bind(task.id)
        .bind(&after)
        .execute(&st.db)
        .await?;

    if agent_status == "needs_input" && !result.questions.is_empty() {
        finish(
            st,
            task.id,
            Finish {
                status: status::WAITING_INPUT,
                summary: Some(summary),
                handoff,
                error: None,
                review: None,
                questions: Some(json!(result.questions)),
                usage: &usage,
            },
        )
        .await?;
        notify(
            st,
            task.user_id,
            Note {
                kind: "question",
                title: format!("Rückfrage: {}", task.title),
                body: result.questions.join("\n"),
                task_id: Some(task.id),
                push: true,
            },
        )
        .await;
        return Ok(());
    }

    let mut failure: Option<String> = None;
    if out.timed_out {
        failure = Some(format!("Timeout nach {} Minuten.", st.cfg.task_timeout_min));
    } else if agent_status == "failed" || (!out.success && result.status.is_none()) {
        let mut f = summary.clone();
        if let Some(e) = &out.error {
            f.push_str(&format!("\n\nAgent error: {e}"));
        }
        if !out.stderr_tail.trim().is_empty() {
            f.push_str(&format!("\n\nstderr (tail):\n{}", out.stderr_tail));
        }
        failure = Some(f);
    }

    let mut review_json = None;
    if failure.is_none() && task.kind != "onempty" {
        if let Some(cmd) = project
            .verify_command
            .as_deref()
            .filter(|c| !c.trim().is_empty())
        {
            event(st, task, "verify", &format!("$ {cmd}")).await;
            if let Err(msg) = run_verify(st, task, cmd, cancel).await? {
                failure = Some(format!("Verifikation `{cmd}` fehlgeschlagen:\n{msg}"));
            }
        }
        let changed = before != after;
        if failure.is_none() && project.review_enabled && changed {
            match run_review(
                st, task, &project, &before, &after, &summary, cancel, &mut usage,
            )
            .await?
            {
                Some(r) => {
                    if !r.approved {
                        failure = Some(format!("Review abgelehnt:\n- {}", r.issues.join("\n- ")));
                    }
                    review_json = Some(json!(r));
                }
                None => event(st, task, "review", "Review übersprungen").await,
            }
        }
    }

    match failure {
        None => {
            if project.auto_push && before != after {
                if let Some(r) = remote {
                    let p = st.runner.push(project.id, r).await?;
                    event(
                        st,
                        task,
                        if p.ok { "status" } else { "error" },
                        &format!("git push: {}", if p.ok { "ok" } else { &p.output }),
                    )
                    .await;
                    if !p.ok {
                        notify(
                            st,
                            task.user_id,
                            Note {
                                kind: "push_failed",
                                title: format!("Push fehlgeschlagen: {}", project.name),
                                body: cut(&p.output, 400),
                                task_id: Some(task.id),
                                push: false,
                            },
                        )
                        .await;
                    }
                }
            }
            finish(
                st,
                task.id,
                Finish {
                    status: status::DONE,
                    summary: Some(summary.clone()),
                    handoff,
                    error: None,
                    review: review_json,
                    questions: None,
                    usage: &usage,
                },
            )
            .await?;
            event(st, task, "status", "Erledigt").await;
            let created = create_followups(st, task, &settings, result.follow_up_tasks).await?;
            if created == 0 && task.kind != "onempty" {
                notify(
                    st,
                    task.user_id,
                    Note {
                        kind: "done",
                        title: format!("Erledigt: {}", task.title),
                        body: cut(&summary, 300),
                        task_id: Some(task.id),
                        push: false,
                    },
                )
                .await;
            }
        }
        Some(f) => {
            event(st, task, "error", &f).await;
            let can_fix = task.kind != "onempty" && task.attempt < settings.max_fix_attempts;
            let st_final = if can_fix {
                status::FAILED
            } else {
                status::BLOCKED
            };
            finish(
                st,
                task.id,
                Finish {
                    status: st_final,
                    summary: Some(summary),
                    handoff,
                    error: Some(cut(&f, 8000)),
                    review: review_json,
                    questions: None,
                    usage: &usage,
                },
            )
            .await?;
            if can_fix {
                create_fix(st, task, &f).await?;
            } else {
                notify(
                    st,
                    task.user_id,
                    Note {
                        kind: "blocked",
                        title: format!("Blockiert: {}", task.title),
                        body: format!(
                            "Nach {} Fix-Versuchen nicht gelöst:\n{}",
                            task.attempt,
                            cut(&f, 400)
                        ),
                        task_id: Some(task.id),
                        push: true,
                    },
                )
                .await;
            }
        }
    }
    Ok(())
}

async fn check_budget(st: &AppState, user: Uuid, provider: &str) {
    if let Ok(Some((reason, until))) =
        budget::take_budget_notification(&st.db, user, provider).await
    {
        notify(
            st,
            user,
            Note {
                kind: "budget",
                title: format!("{provider}: Token-Budget erschöpft"),
                body: format!(
                    "{reason}. Neue Aufträge für {provider} warten bis ca. {} UTC.",
                    until.format("%d.%m. %H:%M")
                ),
                task_id: None,
                push: true,
            },
        )
        .await;
    }
}

/// Runs a process through the runner; cancellation is forwarded.
async fn exec(
    st: &AppState,
    req: ExecRequest,
    cancel: &CancellationToken,
    tx: mpsc::Sender<ExecFrame>,
) -> Result<ExecFrame> {
    let watcher = {
        let (c, client, name) = (cancel.clone(), st.runner.clone(), req.name.clone());
        tokio::spawn(async move {
            c.cancelled().await;
            let _ = client.cancel(&name).await;
        })
    };
    let res = st.runner.exec(&req, tx).await;
    watcher.abort();
    res
}

#[allow(clippy::too_many_arguments)]
async fn agent_run(
    st: &AppState,
    task: &Task,
    provider: &str,
    model: Option<&str>,
    cred: &Credential,
    prompt: String,
    cancel: &CancellationToken,
    label: &str,
) -> Result<RunOutcome> {
    let inv = agent::invocation(provider, model, cred)?;
    let req = ExecRequest {
        name: run_name(task.id, label),
        workspace: task.project_id.to_string(),
        program: inv.program.clone(),
        args: inv.args.clone(),
        env: inv.env.clone(),
        home_files: inv.home_files.clone(),
        read_back: inv.writeback.iter().cloned().collect(),
        stdin: Some(prompt),
        timeout_secs: st.cfg.task_timeout_min * 60,
    };

    let (tx, mut rx) = mpsc::channel::<ExecFrame>(512);
    let consumer = async {
        let mut ps = ParseState::default();
        let mut o = RunOutcome::default();
        let mut stderr_events = 0;
        while let Some(frame) = rx.recv().await {
            match frame {
                ExecFrame::Out { line } => {
                    for ev in agent::parse_line(provider, &line, &mut ps) {
                        match ev {
                            AgentEvent::Text(t) if !t.trim().is_empty() => {
                                event(st, task, "text", &t).await
                            }
                            AgentEvent::Text(_) => {}
                            AgentEvent::Tool(t) => event(st, task, "tool", &t).await,
                            AgentEvent::Usage(u) => o.usage.add(&u),
                            AgentEvent::Final(t) => o.final_text = t,
                            AgentEvent::Error(e) => {
                                event(st, task, "error", &e).await;
                                o.error = Some(e);
                            }
                            AgentEvent::RateLimited(t) => o.rate_limited = Some(t),
                        }
                    }
                }
                ExecFrame::Err { line } => {
                    o.stderr_tail.push_str(&line);
                    o.stderr_tail.push('\n');
                    if o.stderr_tail.len() > 8000 {
                        o.stderr_tail = tail(&o.stderr_tail, 4000);
                    }
                    if stderr_events < 100 && !line.trim().is_empty() {
                        stderr_events += 1;
                        event(st, task, "stderr", &line).await;
                    }
                }
                _ => {}
            }
        }
        o
    };
    let (last, mut o) = tokio::join!(exec(st, req, cancel, tx), consumer);
    match last? {
        ExecFrame::Exit {
            success,
            timed_out,
            cancelled,
            files,
            ..
        } => {
            o.success = success;
            o.timed_out = timed_out;
            o.cancelled = cancelled;
            // Persist refreshed OAuth credentials (codex/gemini rotate tokens).
            if let Some((_, new)) = files.into_iter().next() {
                if new.trim() != cred.secret.trim()
                    && serde_json::from_str::<serde_json::Value>(&new).is_ok()
                {
                    queue::store_credential(st, task.user_id, provider, &cred.kind, &new).await?;
                }
            }
        }
        ExecFrame::Fail { error } => anyhow::bail!("runner: {error}"),
        _ => {}
    }
    o.stderr_tail = tail(&o.stderr_tail, 3000);
    if !o.success
        && o.rate_limited.is_none()
        && !o.cancelled
        && agent::is_rate_limit_text(&o.stderr_tail)
    {
        o.rate_limited = Some(agent::parse_reset(&o.stderr_tail));
    }
    Ok(o)
}

/// Ok(Ok(())) = passed, Ok(Err(output tail)) = failed.
async fn run_verify(
    st: &AppState,
    task: &Task,
    cmd: &str,
    cancel: &CancellationToken,
) -> Result<std::result::Result<(), String>> {
    let req = ExecRequest {
        name: run_name(task.id, "verify"),
        workspace: task.project_id.to_string(),
        program: "sh".into(),
        args: vec!["-c".into(), cmd.into()],
        env: vec![],
        home_files: vec![],
        read_back: vec![],
        stdin: None,
        timeout_secs: st.cfg.verify_timeout_min * 60,
    };
    let (tx, mut rx) = mpsc::channel::<ExecFrame>(512);
    let collect = async {
        let mut buf = String::new();
        while let Some(f) = rx.recv().await {
            if let ExecFrame::Out { line } | ExecFrame::Err { line } = f {
                buf.push_str(&line);
                buf.push('\n');
                if buf.len() > 16000 {
                    buf = tail(&buf, 8000);
                }
            }
        }
        buf
    };
    let (last, output) = tokio::join!(exec(st, req, cancel, tx), collect);
    let output = tail(&output, 4000);
    let (ok, timed_out, code) = match last? {
        ExecFrame::Exit {
            success,
            timed_out,
            code,
            ..
        } => (success, timed_out, code),
        ExecFrame::Fail { error } => {
            tracing::warn!("verify failed to start: {error}");
            (false, false, Some(-1))
        }
        _ => (false, false, None),
    };
    event(st, task, "verify", &format!("exit={code:?}\n{output}")).await;
    if ok {
        Ok(Ok(()))
    } else if timed_out {
        Ok(Err(format!(
            "Timeout nach {} Minuten\n{output}",
            st.cfg.verify_timeout_min
        )))
    } else {
        Ok(Err(output))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_review(
    st: &AppState,
    task: &Task,
    project: &Project,
    before: &str,
    after: &str,
    summary: &str,
    cancel: &CancellationToken,
    usage: &mut Usage,
) -> Result<Option<ReviewResult>> {
    let provider = project
        .review_provider
        .clone()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| task.provider.clone());
    let model = if project.review_provider.is_some() {
        project.review_model.clone()
    } else {
        project.review_model.clone().or(task.model.clone())
    };
    let Some(cred) = queue::load_credential(st, task.user_id, &provider).await? else {
        return Ok(None);
    };
    if budget::is_blocked(&st.db, task.user_id, &provider).await? {
        return Ok(None);
    }
    event(st, task, "review", &format!("Review via {provider}")).await;
    let diff = st
        .runner
        .diff(project.id, before, after, prompt::MAX_DIFF)
        .await?
        .diff;
    let out = agent_run(
        st,
        task,
        &provider,
        model.as_deref(),
        &cred,
        prompt::review(task, summary, &diff),
        cancel,
        "review",
    )
    .await?;
    usage.add(&out.usage);
    budget::record_usage(&st.db, task.user_id, &provider, Some(task.id), &out.usage).await?;
    if out.rate_limited.is_some() || out.cancelled {
        return Ok(None);
    }
    let r: Option<ReviewResult> = st
        .runner
        .queue_file(project.id, "review.json")
        .await?
        .and_then(|s| serde_json::from_str(&s).ok());
    if let Some(r) = &r {
        event(
            st,
            task,
            "review",
            &if r.approved {
                "Review: OK".to_string()
            } else {
                format!("Review: abgelehnt\n- {}", r.issues.join("\n- "))
            },
        )
        .await;
    }
    Ok(r)
}

async fn create_followups(
    st: &AppState,
    parent: &Task,
    settings: &Settings,
    items: Vec<FollowUp>,
) -> Result<usize> {
    let items: Vec<FollowUp> = items
        .into_iter()
        .filter(|f| !f.prompt.trim().is_empty())
        .take(10)
        .collect();
    if items.is_empty() {
        return Ok(0);
    }
    let depth = parent.depth + 1;
    let needs_approval = items.len() as i32 > settings.max_auto_tasks || depth > MAX_AUTO_DEPTH;
    let provider = if parent.kind == "onempty" {
        settings.default_provider.clone()
    } else {
        parent.provider.clone()
    };
    for f in &items {
        let title = if f.title.trim().is_empty() {
            f.prompt
                .lines()
                .next()
                .unwrap_or("Folgeauftrag")
                .to_string()
        } else {
            f.title.clone()
        };
        queue::insert_task(
            st,
            NewTask {
                user_id: parent.user_id,
                project_id: parent.project_id,
                parent_id: Some(parent.id),
                kind: "task",
                title,
                prompt: f.prompt.clone(),
                context: String::new(),
                provider: provider.clone(),
                model: if parent.kind == "onempty" {
                    None
                } else {
                    parent.model.clone()
                },
                status: if needs_approval {
                    status::PENDING_APPROVAL
                } else {
                    status::QUEUED
                },
                attempt: 0,
                depth,
                created_by_agent: true,
                front: false,
            },
        )
        .await?;
    }
    let n = items.len();
    let list: String = items
        .iter()
        .map(|f| {
            format!(
                "• {}",
                if f.title.is_empty() {
                    &f.prompt
                } else {
                    &f.title
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if needs_approval {
        notify(
            st,
            parent.user_id,
            Note {
                kind: "approval",
                title: format!("{n} neue Aufträge – Freigabe nötig"),
                body: list,
                task_id: Some(parent.id),
                push: true,
            },
        )
        .await;
    } else {
        notify(
            st,
            parent.user_id,
            Note {
                kind: "followups",
                title: format!("{n} Folgeaufträge eingeplant"),
                body: list,
                task_id: Some(parent.id),
                push: false,
            },
        )
        .await;
    }
    Ok(n)
}

async fn create_fix(st: &AppState, failed: &Task, failure: &str) -> Result<()> {
    let mut root = failed.clone();
    while root.kind == "fix" {
        match root.parent_id {
            Some(pid) => match queue::load_task(st, pid).await? {
                Some(p) => root = p,
                None => break,
            },
            None => break,
        }
    }
    let fix = queue::insert_task(
        st,
        NewTask {
            user_id: failed.user_id,
            project_id: failed.project_id,
            parent_id: Some(failed.id),
            kind: "fix",
            title: format!("Fix #{}: {}", failed.attempt + 1, root.title),
            prompt: prompt::fix_prompt(&root.title, &root.prompt, failure),
            context: root.context.clone(),
            provider: failed.provider.clone(),
            model: failed.model.clone(),
            status: status::QUEUED,
            attempt: failed.attempt + 1,
            depth: failed.depth,
            created_by_agent: true,
            front: true,
        },
    )
    .await?;
    event(
        st,
        failed,
        "status",
        &format!("Fix-Auftrag erstellt: {}", fix.title),
    )
    .await;
    Ok(())
}
