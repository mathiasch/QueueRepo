//! WP06 steering inside a task's agent phase: CLI sessions that survive
//! between runs, the first run of a "Weiter mit …" task (resume of the
//! origin's session), and the steering rounds after an agent run when user
//! messages wait (resume where the CLI can, else a new run in the same
//! working tree with a handoff).
//!
//! Child module of `runner` (shares its private helpers).
use queued_api::steer::{Delivery, MAX_STEER_ROUNDS};
use queued_common::SessionSpec;

use super::*;
use crate::steer::{self as msgs, MsgRow};

/// Session files of the task's runs (`None` = the CLI keeps none we know).
pub(super) fn store_spec(task: &Task, project: &Project, provider: &str) -> Option<SessionSpec> {
    let (paths, exclude) = agent::session_files(provider)?;
    Some(SessionSpec {
        scope: project.id.to_string(),
        key: crate::runner_client::task_ws(task.id),
        seed: task.session_from.map(crate::runner_client::task_ws),
        paths,
        exclude,
    })
}

fn resumable(provider: &str) -> bool {
    queued_api::steer::cli_session_support(provider).is_some_and(|c| c.resume)
        && agent::session_files(provider).is_some()
}

/// A new session for a fresh run (the CLI's own id where it takes one).
fn fresh_session(provider: &str) -> agent::Session {
    if agent::session_id_settable(provider) {
        agent::Session::Start(Uuid::new_v4().to_string())
    } else {
        agent::Session::None
    }
}

fn started_id(s: &agent::Session) -> Option<String> {
    match s {
        agent::Session::Start(id) | agent::Session::Resume(id) => Some(id.clone()),
        agent::Session::None => None,
    }
}

/// Full prompt of a fresh run: the task, an approved plan and every user
/// message delivered so far (`claim`: deliver the waiting ones now).
pub(super) async fn fresh_prompt(
    st: &AppState,
    task: &Task,
    project: &Project,
    claim: Option<Delivery>,
    progress: Option<String>,
) -> sqlx::Result<String> {
    if let Some(how) = claim {
        let c = msgs::claim(st, task.id, how).await?;
        if !c.is_empty() {
            msgs::log(
                st,
                task,
                &format!("{} Nachricht(en) zugestellt ({})", c.len(), how.label()),
            )
            .await;
        }
    }
    let nonce = prompt::nonce();
    let mut extra = String::new();
    if task.plan_state.as_deref() == Some("approved") {
        if let Some(p) = task.plan.as_deref().filter(|p| !p.trim().is_empty()) {
            let edited = task.plan_meta_parsed().is_some_and(|m| m.edited);
            extra.push_str(&prompt::approved_plan_section(p, edited, &nonce));
        }
    }
    let delivered = msgs::delivered(st, task.id).await?;
    extra.push_str(&prompt::messages_section(
        &msgs::items(st, &delivered).await?,
        &nonce,
    ));
    if let Some(p) = progress {
        extra.push_str(&p);
    }
    let parent = parent_of(st, task).await?;
    Ok(prompt::task_with(task, project, parent.as_ref(), &extra))
}

/// The first agent run of the task.
pub(super) struct First {
    pub session: agent::Session,
    pub prompt: String,
    pub resumed: bool,
    /// Messages delivered into this run (relabelled if a resume fails).
    pub claimed: Vec<Uuid>,
}

pub(super) async fn first_run(
    st: &AppState,
    task: &Task,
    project: &Project,
    provider: &str,
) -> sqlx::Result<First> {
    // "Weiter mit …": the origin's session, same agent, first run only.
    if let (Some(origin), true, None) =
        (task.session_from, resumable(provider), &task.agent_session)
    {
        let s = queue::load_task(st, origin)
            .await?
            .and_then(|o| o.agent_session_parsed())
            .filter(|s| s.provider == provider && agent::valid_session_id(&s.id));
        if let Some(s) = s {
            let claimed = msgs::claim(st, task.id, Delivery::Resume).await?;
            let nonce = prompt::nonce();
            let section = prompt::messages_section(&msgs::items(st, &claimed).await?, &nonce);
            msgs::log(
                st,
                task,
                "Setzt die Sitzung des vorherigen Auftrags fort (gleicher Agent, gleiche Unterhaltung)",
            )
            .await;
            return Ok(First {
                session: agent::Session::Resume(s.id),
                prompt: prompt::resume(task, prompt::RESUME_CONTINUE, &section),
                resumed: true,
                claimed: claimed.iter().map(|m| m.id).collect(),
            });
        }
    }
    Ok(First {
        session: fresh_session(provider),
        prompt: fresh_prompt(st, task, project, Some(Delivery::Prompt), None).await?,
        resumed: false,
        claimed: vec![],
    })
}

/// Remembers the CLI session of a run (for steering and "Weiter mit …").
pub(super) async fn remember(
    st: &AppState,
    task: &Task,
    provider: &str,
    session: &agent::Session,
    out: &RunOutcome,
) {
    let Some(id) = out.session_id.clone().or_else(|| started_id(session)) else {
        return;
    };
    if agent::session_files(provider).is_none() || !agent::valid_session_id(&id) {
        return;
    }
    let v = serde_json::to_value(queued_api::steer::AgentSession {
        provider: provider.to_string(),
        id,
    })
    .unwrap_or_default();
    if let Err(e) = sqlx::query("UPDATE tasks SET agent_session=$2 WHERE id=$1")
        .bind(task.id)
        .bind(v)
        .execute(&st.db)
        .await
    {
        tracing::warn!("task {}: session not stored: {e:#}", task.id);
    }
}

/// A resume that did nothing (no usage, no tool): the session was unknown
/// to the CLI (Claude: "No conversation found with session ID", recorded).
pub(super) fn resume_failed(out: &RunOutcome) -> bool {
    !out.success
        && !out.cancelled
        && out.tools == 0
        && budget::counted(&out.usage()) == 0
        && out.structured.is_none()
}

/// What the next steering round runs.
pub(super) struct Round {
    pub session: agent::Session,
    pub prompt: String,
    pub resumed: bool,
    pub claimed: Vec<Uuid>,
}

/// After an agent run: the next steering round, if user messages wait (or
/// "Sofort zustellen" interrupted the run). `None` = the normal result flow
/// continues (cancel, limits, provider trouble, nothing to deliver).
#[allow(clippy::too_many_arguments)]
pub(super) async fn next_round(
    st: &AppState,
    task: &Task,
    project: &Project,
    provider: &str,
    out: &RunOutcome,
    current: &agent::Session,
    cancel: &CancellationToken,
    lim: &EffectiveLimits,
    usage: &Usage,
) -> Result<Option<Round>, RunError> {
    if cancel.is_cancelled() || early(out, false, provider, lim, usage).is_some() {
        return Ok(None);
    }
    if let Some((c, _)) = &out.failure {
        if matches!(
            c.class(),
            FailureClass::RateLimit
                | FailureClass::ProviderAuth
                | FailureClass::ProviderTransient
                | FailureClass::Config
        ) {
            return Ok(None);
        }
    }
    if !out.steered && !msgs::has_queued(st, task.id).await? {
        return Ok(None);
    }
    let rounds: i32 = sqlx::query_scalar("SELECT steer_rounds FROM tasks WHERE id=$1")
        .bind(task.id)
        .fetch_one(&st.db)
        .await?;
    if rounds >= MAX_STEER_ROUNDS {
        msgs::log(
            st,
            task,
            &format!(
                "Höchstens {MAX_STEER_ROUNDS} Steuerrunden pro Auftrag – weitere Nachrichten werden nach dem Auftrag als Fortsetzung zugestellt"
            ),
        )
        .await;
        return Ok(None);
    }
    let session_id = out
        .session_id
        .clone()
        .or_else(|| started_id(current))
        .filter(|id| agent::valid_session_id(id));
    let resume = resumable(provider).then_some(session_id).flatten();
    let how = if resume.is_some() {
        Delivery::Resume
    } else {
        Delivery::Handoff
    };
    let claimed: Vec<MsgRow> = msgs::claim(st, task.id, how).await?;
    if claimed.is_empty() && !out.steered {
        // Withdrawn in the meantime.
        return Ok(None);
    }
    sqlx::query("UPDATE tasks SET steer_rounds=steer_rounds+1 WHERE id=$1")
        .bind(task.id)
        .execute(&st.db)
        .await?;
    msgs::log(
        st,
        task,
        &if claimed.is_empty() {
            "Agent unterbrochen – die Nachricht wurde vorher zurückgezogen; er setzt die Arbeit fort".to_string()
        } else {
            format!("{} Nachricht(en) zugestellt ({})", claimed.len(), how.label())
        },
    )
    .await;
    Ok(Some(match resume {
        Some(id) => {
            let nonce = prompt::nonce();
            let section = prompt::messages_section(&msgs::items(st, &claimed).await?, &nonce);
            Round {
                session: agent::Session::Resume(id),
                prompt: prompt::resume(
                    task,
                    if out.steered {
                        prompt::RESUME_INTERRUPTED
                    } else {
                        prompt::RESUME_STEER
                    },
                    &section,
                ),
                resumed: true,
                claimed: claimed.iter().map(|m| m.id).collect(),
            }
        }
        None => {
            // No resume: a new run with everything plus what the last run did.
            let file = st
                .runner
                .queue_file(crate::runner_client::task_ws(task.id), "result.json")
                .await
                .ok()
                .flatten();
            let (c, _) = load_contract(out.structured.as_ref(), file.as_deref());
            let summary = c
                .as_ref()
                .and_then(|c| c.summary.clone())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| cut(&out.final_text, 1500));
            let handoff = c.and_then(|c| c.handoff);
            Round {
                session: fresh_session(provider),
                prompt: fresh_prompt(
                    st,
                    task,
                    project,
                    None,
                    Some(prompt::progress_section(&summary, handoff.as_deref())),
                )
                .await?,
                resumed: false,
                claimed: claimed.iter().map(|m| m.id).collect(),
            }
        }
    }))
}

/// Fresh run after a failed resume (once): everything with a handoff.
pub(super) async fn fallback(
    st: &AppState,
    task: &Task,
    project: &Project,
    provider: &str,
    claimed: &[Uuid],
) -> sqlx::Result<Round> {
    // They reach the agent in the new run's prompt, not in the session.
    sqlx::query("UPDATE task_messages SET delivery='handoff' WHERE id = ANY($1)")
        .bind(claimed)
        .execute(&st.db)
        .await?;
    msgs::log(
        st,
        task,
        "Die Sitzung des Agenten ließ sich nicht fortsetzen – neuer Lauf mit Übergabe und allen Nachrichten",
    )
    .await;
    // Messages delivered for the failed resume are part of every fresh prompt.
    Ok(Round {
        session: fresh_session(provider),
        prompt: fresh_prompt(st, task, project, Some(Delivery::Prompt), None).await?,
        resumed: false,
        claimed: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(raw: &str, success: bool) -> RunOutcome {
        let mut ps = ParseState::default();
        let mut o = RunOutcome {
            success,
            ..Default::default()
        };
        for line in raw.lines() {
            for ev in agent::parse_line("claude", line, &mut ps) {
                match ev {
                    AgentEvent::Tool(_) => o.tools += 1,
                    AgentEvent::UsageDelta(u) => o.usage_delta.add(&u),
                    AgentEvent::UsageTotal(u) => o.usage_total = Some(u),
                    AgentEvent::Session(s) => o.session_id = Some(s),
                    AgentEvent::Structured(v) => o.structured = Some(v),
                    _ => {}
                }
            }
        }
        o
    }

    #[test]
    fn recorded_unknown_session_counts_as_failed_resume() {
        // Claude Code 2.1.322, `--resume` with an unknown id (exit 1).
        let raw = include_str!("../tests/fixtures/cli/claude-2.1.322/resume_not_found.jsonl");
        assert!(resume_failed(&outcome(raw, false)));
        // A real resume did work (tokens, tools): never a failed resume.
        let raw = include_str!("../tests/fixtures/cli/claude-2.1.322/resume_after_plan.jsonl");
        let o = outcome(raw, true);
        assert!(!resume_failed(&o));
        assert!(o.tools > 0 && budget::counted(&o.usage()) > 0);
        assert_eq!(
            o.session_id.as_deref(),
            Some("11111111-2222-4333-8444-555555555556")
        );
        // Interrupted runs are never "failed resumes" (the steer path decides).
        let mut c = outcome("", false);
        c.cancelled = true;
        assert!(!resume_failed(&c));
    }
}
