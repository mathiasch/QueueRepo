//! WP06 plan run: the agent explores and writes a plan; it must not change
//! anything. Three layers, none of which relies on the others:
//!
//! 1. the CLI's own plan/read-only switch (where it exists, see
//!    `queued_api::steer::CLI_SESSION_SUPPORT`);
//! 2. the runner: `read_only` mounts the workspace read-only in containers
//!    and compares the whole tree before/after on every engine;
//! 3. a throwaway working tree (own id, same start as the task, incl.
//!    `carry_from` and the pull-request start) that is always deleted – no
//!    change can reach the task's working tree, its branch or the main
//!    repository, whatever the CLI does.
//!
//! Child module of `runner` (shares its private helpers).
use queued_api::steer::{Delivery, PlanMeta, PlanSource};
use queued_common::TaskWorkspaceRequest;

use super::*;
use crate::models::PlanResult;

const PLAN_XOR: u128 = 0x5157_4555_5f50_4c41_4e5f_5741_5944_5321;

/// Id of a task's throwaway plan working tree (deterministic, never a task).
pub(super) fn plan_ws_id(task: Uuid) -> Uuid {
    Uuid::from_u128(task.as_u128() ^ PLAN_XOR)
}

/// The plan of a finished plan run: structured output (Claude/Codex
/// schema, mock) or the final text (Gemini, opencode).
pub(crate) fn extract_plan(structured: Option<&Value>, final_text: &str) -> Option<PlanResult> {
    if let Some(r) = structured
        .and_then(|v| serde_json::from_value::<PlanResult>(v.clone()).ok())
        .filter(|r| !r.plan.trim().is_empty())
    {
        return Some(PlanResult {
            plan: r.plan.trim().to_string(),
            questions: r
                .questions
                .into_iter()
                .map(|q| q.trim().chars().take(500).collect::<String>())
                .filter(|q| !q.is_empty())
                .take(20)
                .collect(),
        });
    }
    let t = final_text.trim();
    (!t.is_empty()).then(|| PlanResult {
        plan: t.to_string(),
        questions: vec![],
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    st: &AppState,
    task: &Task,
    project: &Project,
    env: &Env,
    lim: &EffectiveLimits,
    acc: &RunAccess,
    remote: Option<GitRemote>,
    commits: &Committer<'_>,
    cancel: &CancellationToken,
    usage: &mut Usage,
) -> Result<(), RunError> {
    crate::plan::mark_planning(st, task.id).await?;
    event(
        st,
        task,
        "plan",
        &if task.plan_revision > 0 {
            format!(
                "Neuer Plan wird erstellt (Überarbeitung {}) – der Agent liest nur, in einer schreibgeschützten Wegwerf-Kopie",
                task.plan_revision
            )
        } else {
            "Plan wird erstellt – der Agent liest nur, in einer schreibgeschützten Wegwerf-Kopie".into()
        },
    )
    .await;
    main_ready(st, task, project, remote).await?;
    let pid = plan_ws_id(task.id);
    // Never reuse what an earlier (crashed) plan run left behind.
    if let Err(e) = st.runner.remove_task(project.id, pid, None).await {
        tracing::debug!("task {}: no old plan working tree: {e:#}", task.id);
    }
    let carry = merge::carry_of(st, task).await?.map(|(c, _)| c);
    let start = merge::pr_start(st, task, project).await?;
    st.runner
        .prepare_task(
            project.id,
            pid,
            &TaskWorkspaceRequest {
                branch: queued_common::task_branch(&pid.simple().to_string(), true),
                merge_base: None,
                carry,
                allow: project.secret_allowlist.clone(),
                start,
            },
        )
        .await?;
    let res = inner(
        st, task, project, env, lim, acc, pid, commits, cancel, usage,
    )
    .await;
    // Always: the throwaway copy goes, whatever happened in it – before the
    // plan is shown to anyone.
    let mut removed = false;
    for _ in 0..5 {
        match st.runner.remove_task(project.id, pid, None).await {
            Ok(()) => {
                removed = true;
                break;
            }
            Err(e) => {
                tracing::info!("task {}: plan working tree not removed yet: {e:#}", task.id);
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
        }
    }
    if !removed {
        tracing::warn!("task {}: plan working tree {} left behind", task.id, pid);
    }
    if let Some((plan, meta)) = res? {
        crate::plan::hold(st, task, status::RUNNING, &plan, meta, usage).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn inner(
    st: &AppState,
    task: &Task,
    project: &Project,
    env: &Env,
    lim: &EffectiveLimits,
    acc: &RunAccess,
    pid: Uuid,
    commits: &Committer<'_>,
    cancel: &CancellationToken,
    usage: &mut Usage,
) -> Result<Option<(String, PlanMeta)>, RunError> {
    // Messages that waited are delivered with the plan run (and stay in
    // every later prompt of the task).
    let claimed = crate::steer::claim(st, task.id, Delivery::Prompt).await?;
    if !claimed.is_empty() {
        crate::steer::log(
            st,
            task,
            &format!(
                "{} Nachricht(en) zugestellt (beim Start der Planung)",
                claimed.len()
            ),
        )
        .await;
    }
    let delivered = crate::steer::delivered(st, task.id).await?;
    let items = crate::steer::items(st, &delivered).await?;
    let nonce = prompt::nonce();
    let parent = parent_of(st, task).await?;
    let revision = task
        .plan_feedback
        .as_deref()
        .filter(|n| !n.trim().is_empty());
    let prompt = prompt::plan_mode(
        task,
        project,
        parent.as_ref(),
        revision.and(task.plan.as_deref()),
        revision,
        &prompt::messages_section(&items, &nonce),
        &nonce,
    );
    let meter = meter_for(st, acc, task.model.as_deref(), "plan").await;
    let out = agent_run(
        st,
        task,
        RunSpec {
            provider: &acc.provider,
            model: task.model.as_deref(),
            cred: &acc.cred,
            prompt,
            kind: RunKind::Plan,
            label: "plan",
            limits: lim,
            used_before: usage,
            meter,
            env,
            trust_repo_config: project.trust_repo_config,
            session: agent::Session::None,
            store: None,
            read_only: true,
            workspace: Some(crate::runner_client::task_ws(pid)),
            steer: None,
            clear_result: false,
        },
        cancel,
    )
    .await?;
    record_run_usage(st, task, acc, &out, usage).await?;
    if out.changed_total > 0 {
        event(
            st,
            task,
            "guard",
            &format!(
                "Der Plan-Lauf hat {} Datei(en) verändert, obwohl er nur lesen darf – alles verworfen (Wegwerf-Kopie): {}",
                out.changed_total,
                out.changed.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
            ),
        )
        .await;
    }
    let user_cancel = cancel.is_cancelled();
    let caps = agent::capabilities_for(&acc.provider, acc.cred.conn.as_ref());
    let (disposition, pause) =
        classify_run(&out, user_cancel, &acc.provider, caps, None, lim, usage);
    match pause {
        Some(Pause::RateLimit(reset)) => {
            pause_rate_limited(st, task, &acc.key, reset).await;
        }
        Some(Pause::Auth(msg)) => pause_auth(st, task, &acc.key, &acc.label, &msg).await,
        None => {}
    }
    let failure = match disposition {
        // Nothing to evaluate: the result is the plan (no contract).
        Disposition::Contract(_) | Disposition::ExitFallback { .. } => None,
        Disposition::TaskFailure(f) => Some(f),
        other => {
            // Cancel, limits, re-queue, provider pause: as for every run
            // (nothing to commit – the plan run changed nothing).
            interrupted(commits, acc, other, lim, usage).await?;
            return Ok(None);
        }
    };
    match extract_plan(out.structured.as_ref(), &out.final_text) {
        Some(p) => {
            let meta = PlanMeta {
                source: PlanSource::Agent,
                questions: p.questions,
                attempted_changes: out.changed.clone(),
                attempted_total: out.changed_total,
                model: task.model.clone(),
                ..Default::default()
            };
            Ok(Some((p.plan, meta)))
        }
        None => {
            let mut msg =
                "Der Agent hat keinen Plan geliefert. „Wiederholen“ startet die Planung erneut."
                    .to_string();
            if let Some(f) = failure {
                msg.push_str(&format!("\n\n{}", cut(&f, 2000)));
            }
            if !out.stderr_tail.trim().is_empty() {
                msg.push_str(&format!("\n\nstderr (Ende):\n{}", out.stderr_tail));
            }
            event(st, task, "error", &msg).await;
            if block_running(st, task.id, FailureClass::Task, &msg, usage).await {
                notify_blocked(st, task, &msg).await;
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_from_structured_output_or_final_text() {
        let v = json!({"plan": " 1. a\n2. b ", "questions": ["  Postgres?  ", ""]});
        let p = extract_plan(Some(&v), "ignored").unwrap();
        assert_eq!(p.plan, "1. a\n2. b");
        assert_eq!(p.questions, ["Postgres?"]);
        // Invalid or empty structured output: the final text counts.
        let p = extract_plan(Some(&json!({"plan": ""})), "Plan: x").unwrap();
        assert_eq!((p.plan.as_str(), p.questions.len()), ("Plan: x", 0));
        assert!(extract_plan(None, "  ").is_none());
        assert!(extract_plan(Some(&json!({"status": "done"})), "").is_none());
    }

    #[test]
    fn plan_workspace_is_never_the_task_itself() {
        let t = Uuid::new_v4();
        assert_ne!(plan_ws_id(t), t);
        assert_eq!(plan_ws_id(plan_ws_id(t)), t);
        assert!(queued_common::is_task_workspace(
            &crate::runner_client::task_ws(plan_ws_id(t))
        ));
    }

    #[test]
    fn recorded_claude_plan_run_yields_a_plan_and_no_change() {
        // Real Claude Code 2.1.322 run in plan mode (tests/fixtures/cli/claude-2.1.322).
        let raw = include_str!("../tests/fixtures/cli/claude-2.1.322/plan_mode_structured.jsonl");
        let mut ps = ParseState::default();
        let mut structured = None;
        let mut session = None;
        let mut edits = vec![];
        for line in raw.lines() {
            for ev in agent::parse_line("claude", line, &mut ps) {
                match ev {
                    AgentEvent::Structured(v) => structured = Some(v),
                    AgentEvent::Session(s) => session = Some(s),
                    AgentEvent::Tool(t) if t.starts_with("Write") || t.starts_with("Edit") => {
                        edits.push(t)
                    }
                    _ => {}
                }
            }
        }
        let p = extract_plan(structured.as_ref(), "").expect("plan");
        assert!(p.plan.contains("calc.py"), "{}", p.plan);
        assert_eq!(
            session.as_deref(),
            Some("11111111-2222-4333-8444-555555555556")
        );
        // The only write of the run went to Claude's own plans folder in HOME.
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert!(edits[0].contains("/home/.claude/plans/"), "{edits:?}");
    }
}
