//! Gate flow of a task run (WP03): checks before the commit, holds that wait
//! for a human (`held`), the result flow after the commit (verify → review →
//! push → finish) and its continuation after an approval.
//!
//! Child module of `runner` (shares its private helpers).
use chrono::Utc;
use queued_api::gate::{GateResume, GateStage, TaskGate};
use queued_common::{
    gate::{self as cgate, FindingKind, GateFinding},
    GitRemote, PushRequest,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;

/// What the flow after the agent run needs to continue later (`tasks.gate_pending`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct Pending {
    pub summary: String,
    pub handoff: Option<String>,
    pub follow_ups: Vec<FollowUp>,
    /// Real task failure of the run (fix task after the commit).
    pub failure: Option<String>,
    /// `needs_input` questions of the contract.
    pub questions: Vec<String>,
    pub before: String,
    pub after: Option<String>,
    /// Findings a human approved (context for the reviewer).
    pub approved: Vec<GateFinding>,
    /// Review verdict (kept while a push waits for its approval).
    pub review: Option<Value>,
}

/// Steps of the result flow after the commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Step {
    Verify,
    Review,
    Push,
}

/// Everything the result flow of one run needs.
pub(crate) struct Flow<'a> {
    pub st: &'a AppState,
    pub task: &'a Task,
    pub project: &'a Project,
    pub settings: &'a Settings,
    pub env: &'a Env,
    pub lim: &'a EffectiveLimits,
    pub routing: Option<&'a queued_api::Routing>,
    pub remote: Option<GitRemote>,
    pub cancel: &'a CancellationToken,
    pub commits: &'a Committer<'a>,
}

fn summary_of(findings: &[GateFinding]) -> String {
    findings
        .iter()
        .take(8)
        .map(|f| {
            let place = match (f.path.is_empty(), f.line) {
                (true, _) => String::new(),
                (false, Some(l)) => format!(" – {}:{l}", f.path),
                (false, None) => format!(" – {}", f.path),
            };
            format!(
                "- {}{place}{}",
                queued_api::gate::finding_title(f.kind),
                if f.detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", f.detail)
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Puts the task on hold: status `held`, gate and continuation state stored,
/// event + push notification. Works from a run (`running`) and from the
/// batch poller (`waiting_batch`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn hold(
    st: &AppState,
    task: &Task,
    stage: GateStage,
    findings: Vec<GateFinding>,
    pending: &Pending,
    head: Option<String>,
    has_diff: bool,
    usage: &Usage,
) -> Result<(), RunError> {
    let gate = TaskGate {
        stage,
        findings,
        held_at: Utc::now(),
        head,
        has_diff,
        resume: None,
    };
    let n = sqlx::query(
        "UPDATE tasks SET status='held', gate=$2, gate_pending=$3, usage=$4,
                summary=COALESCE(NULLIF($5, ''), summary), handoff=COALESCE($6, handoff),
                not_before=NULL, error=NULL, updated_at=now()
         WHERE id=$1 AND status IN ('running','waiting_batch','merging')",
    )
    .bind(task.id)
    .bind(serde_json::to_value(&gate).unwrap_or_default())
    .bind(serde_json::to_value(pending).unwrap_or_default())
    .bind(serde_json::to_value(usage).unwrap_or_default())
    .bind(queued_common::secrets::redact(&pending.summary))
    .bind(
        pending
            .handoff
            .as_deref()
            .map(queued_common::secrets::redact),
    )
    .execute(&st.db)
    .await?
    .rows_affected();
    queue::emit_task(st, task.id).await;
    if n == 0 {
        return Ok(());
    }
    let list = summary_of(&gate.findings);
    event(
        st,
        task,
        "gate",
        &format!("Freigabe nötig: {}\n{list}", stage.title()),
    )
    .await;
    notify(
        st,
        task.user_id,
        Note {
            kind: "gate",
            title: format!("Freigabe nötig: {}", task.title),
            body: format!("{}\n{}", stage.title(), cut(&list, 300)),
            task_id: Some(task.id),
            push: true,
        },
    )
    .await;
    Ok(())
}

/// Gate check after the agent run, before the commit (`changes` stage):
/// secrets (diff and agent log), protected paths, weakened tests. Returns
/// `true` when the task is now held.
pub(crate) async fn check_changes(
    f: &Flow<'_>,
    pending: &Pending,
    log_findings: &[GateFinding],
    usage: &Usage,
) -> Result<bool, RunError> {
    let (st, task, project) = (f.st, f.task, f.project);
    let test_task = task.allow_test_changes;
    let req = cgate::InspectRequest {
        base: pending.before.clone(),
        protected: if test_task {
            vec![]
        } else {
            project.protected()
        },
        test_guard: project.test_guard && !test_task,
        allow: project.secret_allowlist.clone(),
        // Agent configuration the next run (and this task's review) would
        // load: every change needs an approval, also a new file.
        watch: agent_config_paths(project, &task.provider),
    };
    let ws = crate::runner_client::task_ws(task.id);
    let res = st.runner.inspect(&ws, &req).await?;
    let mut findings = res.findings;
    findings.extend(log_findings.iter().cloned());
    if findings.is_empty() {
        if test_task && res.changed_files > 0 {
            event(
                st,
                task,
                "gate",
                "Test-Auftrag: Änderungen an Tests und geschützten Pfaden ohne Freigabe erlaubt",
            )
            .await;
        }
        return Ok(false);
    }
    // Park everything the run changed (also commits the agent made itself)
    // outside the branch; nothing reaches verify, review or a push.
    st.runner.hold(&ws, task.id, &pending.before).await?;
    hold(
        st,
        task,
        GateStage::Changes,
        findings,
        pending,
        None,
        true,
        usage,
    )
    .await?;
    Ok(true)
}

/// Commit of the run after a clean gate. `None` = the scanner blocked the
/// commit after all (the task is held).
pub(crate) async fn commit(
    f: &Flow<'_>,
    pending: &Pending,
    usage: &Usage,
) -> Result<Option<String>, RunError> {
    let out = f.commits.commit("").await?;
    if out.blocked.is_empty() {
        set_snapshot_after(f.st, f.task.id, &out.sha).await?;
        return Ok(Some(out.sha));
    }
    f.st.runner
        .hold(
            crate::runner_client::task_ws(f.task.id),
            f.task.id,
            &pending.before,
        )
        .await?;
    hold(
        f.st,
        f.task,
        GateStage::Changes,
        out.blocked,
        pending,
        None,
        true,
        usage,
    )
    .await?;
    Ok(None)
}

/// Repository files with agent configuration the CLI of `provider` loads.
fn agent_config_paths(project: &Project, provider: &str) -> Vec<String> {
    if project.trust_repo_config {
        cgate::AGENT_CONFIG_PATHS
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        agent::always_loaded_config(provider)
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
}

/// Before an agent run: repository configuration the CLI loads changed
/// since the last approval (`config` stage). Returns `true` when held.
pub(crate) async fn check_agent_config(
    st: &AppState,
    task: &Task,
    project: &Project,
    provider: &str,
    usage: &Usage,
) -> Result<bool, RunError> {
    let paths: Vec<String> = if project.trust_repo_config {
        cgate::AGENT_CONFIG_PATHS
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        agent::always_loaded_config(provider)
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    if paths.is_empty() {
        return Ok(false);
    }
    let res = st
        .runner
        .baseline_check(
            project.id,
            &cgate::BaselineCheck {
                name: "agent-config".into(),
                paths,
                task: Some(crate::runner_client::task_ws(task.id)),
            },
        )
        .await?;
    if res.changed.is_empty() {
        return Ok(false);
    }
    let findings = res
        .changed
        .iter()
        .map(|p| GateFinding {
            kind: FindingKind::AgentConfig,
            path: p.clone(),
            line: None,
            detail: String::new(),
            fingerprint: None,
        })
        .collect();
    hold(
        st,
        task,
        GateStage::Config,
        findings,
        &Pending::default(),
        Some(res.head),
        false,
        usage,
    )
    .await?;
    Ok(true)
}

/// Files the setup script executes (heuristic): lock files and manifests,
/// the cache files of the environment and repository paths named in the
/// script itself.
pub(crate) fn setup_paths(env: &Env) -> Vec<String> {
    const MANIFESTS: [&str; 16] = [
        "package.json",
        ".npmrc",
        ".yarnrc",
        ".yarnrc.yml",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements*.txt",
        "Pipfile",
        "Cargo.toml",
        "build.rs",
        "go.mod",
        "Gemfile",
        "Makefile",
        "composer.json",
        "build.gradle*",
    ];
    let mut v: Vec<String> = queued_api::env::DEFAULT_CACHE_FILES
        .iter()
        .chain(MANIFESTS.iter())
        .map(|s| s.to_string())
        .collect();
    v.extend(env.cache_files.iter().cloned());
    for tok in env
        .setup_script
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '"' | '\''))
    {
        let t = tok.trim_start_matches("./");
        if (t.contains('/') || t.contains('.'))
            && !t.starts_with('-')
            && !t.starts_with('$')
            && !t.contains("://")
            && queued_common::glob::invalid(t).is_none()
        {
            v.push(t.to_string());
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Before a setup with secrets (WP02-N1): setup files changed since the last
/// approved state (`setup` stage). Returns `true` when held.
pub(crate) async fn check_setup(
    st: &AppState,
    task: &Task,
    project: &Project,
    env: &Env,
    usage: &Usage,
) -> Result<bool, RunError> {
    let res = st
        .runner
        .baseline_check(
            project.id,
            &cgate::BaselineCheck {
                name: "setup".into(),
                paths: setup_paths(env),
                task: Some(crate::runner_client::task_ws(task.id)),
            },
        )
        .await?;
    if res.changed.is_empty() {
        return Ok(false);
    }
    let findings = res
        .changed
        .iter()
        .map(|p| GateFinding {
            kind: FindingKind::SetupFiles,
            path: p.clone(),
            line: None,
            detail: String::new(),
            fingerprint: None,
        })
        .collect();
    hold(
        st,
        task,
        GateStage::Setup,
        findings,
        &Pending::default(),
        Some(res.head),
        false,
        usage,
    )
    .await?;
    Ok(true)
}

/// The review could not run. Transient reasons are retried (only the review
/// phase, with backoff, without a fix attempt); afterwards – or right away
/// for permanent reasons – a human decides (WP01-N1: never `done` unreviewed).
async fn review_unavailable(
    st: &AppState,
    task: &Task,
    pending: &Pending,
    why: &str,
    retry: bool,
    usage: &Usage,
) -> Result<(), RunError> {
    let why = if why.is_empty() {
        "kein Zugang für den Review-Agenten verfügbar"
    } else {
        why
    };
    let finding = GateFinding {
        kind: FindingKind::ReviewUnavailable,
        path: String::new(),
        line: None,
        detail: why.to_string(),
        fingerprint: None,
    };
    let n = task.transient_retries + 1;
    if retry && n <= st.cfg.transient_max_retries {
        let delay = limits::backoff(n, st.cfg.backoff_base_secs, st.cfg.backoff_max_secs);
        let not_before = Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default();
        let gate = TaskGate {
            stage: GateStage::Review,
            findings: vec![finding],
            held_at: Utc::now(),
            head: None,
            has_diff: false,
            resume: Some(GateResume::Review),
        };
        sqlx::query("UPDATE tasks SET gate=$2, gate_pending=$3 WHERE id=$1")
            .bind(task.id)
            .bind(serde_json::to_value(&gate).unwrap_or_default())
            .bind(serde_json::to_value(pending).unwrap_or_default())
            .execute(&st.db)
            .await?;
        let requeued = queue::requeue(
            st,
            task.id,
            Requeue {
                class: FailureClass::ProviderTransient,
                transient_retries: n,
                not_before: Some(not_before),
                note: None,
                error: Some(format!("Review nicht möglich: {why}")),
                usage,
            },
        )
        .await?;
        if requeued {
            event(
                st,
                task,
                "review",
                &format!(
                    "Review nicht möglich ({why}) – nur das Review wird in {} s wiederholt ({n}/{})",
                    delay.as_secs(),
                    st.cfg.transient_max_retries
                ),
            )
            .await;
        }
        return Ok(());
    }
    hold(
        st,
        task,
        GateStage::Review,
        vec![finding],
        pending,
        None,
        false,
        usage,
    )
    .await
}

/// Push with the gate: the default branch of the remote and secrets in the
/// pushed commits need an approval. Returns `true` when the task is now held.
pub(crate) async fn push_gated(
    st: &AppState,
    task: &Task,
    project: &Project,
    remote: GitRemote,
    pending: &Pending,
    approved: bool,
    usage: &Usage,
) -> Result<bool, RunError> {
    let sha = pending.after.clone().unwrap_or_default();
    let req = PushRequest {
        remote,
        sha: (!sha.is_empty()).then(|| sha.clone()),
        allow_protected: approved,
        allow: project.secret_allowlist.clone(),
        allow_secrets: approved,
    };
    let p = st.runner.push(project.id, &req).await?;
    if !p.held.is_empty() {
        hold(
            st,
            task,
            GateStage::Push,
            p.held,
            pending,
            Some(sha),
            false,
            usage,
        )
        .await?;
        return Ok(true);
    }
    event(
        st,
        task,
        if p.ok { "status" } else { "error" },
        &format!(
            "git push{}: {}",
            if approved { " (freigegeben)" } else { "" },
            if p.ok { "ok" } else { &p.output }
        ),
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
    Ok(false)
}

/// Push (auto-push projects) and the final status.
pub(crate) async fn finish_flow(
    st: &AppState,
    task: &Task,
    project: &Project,
    settings: &Settings,
    p: Pending,
    push: Option<bool>,
    usage: &Usage,
) -> Result<(), RunError> {
    let review = p.review.clone();
    let before = p.before.clone();
    let after = p.after.clone().unwrap_or_default();
    match &p.failure {
        None => {
            // WP04: the task's branch goes through the merge queue first;
            // the push (below) happens after the merge.
            if super::merge::enqueue(st, task, &p, usage).await? {
                return Ok(());
            }
            // `push`: None = automatic (gated), Some(true) = approved, Some(false) = declined.
            if project.auto_push && before != after && push != Some(false) {
                if let Some(r) = project_remote(st, project) {
                    if push_gated(st, task, project, r, &p, push == Some(true), usage).await? {
                        return Ok(());
                    }
                }
            }
            let end = End {
                summary: p.summary,
                handoff: p.handoff,
                review,
                usage,
            };
            finish_done(st, task, settings, end, p.follow_ups).await
        }
        Some(f) => {
            let f = f.clone();
            let end = End {
                summary: p.summary,
                handoff: p.handoff,
                review,
                usage,
            };
            finish_failed(st, task, settings, end, &f).await
        }
    }
}

/// Result flow after the commit, starting at `from`.
pub(crate) async fn after_commit(
    f: &Flow<'_>,
    mut p: Pending,
    exec: Option<&RunAccess>,
    from: Step,
    usage: &mut Usage,
) -> Result<(), RunError> {
    let (st, task, project) = (f.st, f.task, f.project);
    let before = p.before.clone();
    let after = p.after.clone().unwrap_or_default();
    if from == Step::Verify {
        if p.failure.is_none() && !p.questions.is_empty() {
            finish(
                st,
                task.id,
                Finish {
                    status: status::WAITING_INPUT,
                    summary: Some(p.summary.clone()),
                    handoff: p.handoff.clone(),
                    error: None,
                    review: None,
                    questions: Some(json!(p.questions)),
                    usage,
                    failure_class: None,
                },
            )
            .await?;
            notify(
                st,
                task.user_id,
                Note {
                    kind: "question",
                    title: format!("Rückfrage: {}", task.title),
                    body: p.questions.join("\n"),
                    task_id: Some(task.id),
                    push: true,
                },
            )
            .await;
            return Ok(());
        }
        if p.failure.is_none() && task.kind != "onempty" {
            if let Some(cmd) = project
                .verify_command
                .as_deref()
                .filter(|c| !c.trim().is_empty())
            {
                event(st, task, "verify", &format!("$ {cmd}")).await;
                let verdict = run_verify(st, task, cmd, f.cancel, f.env).await?;
                if f.cancel.is_cancelled() {
                    finish_cancelled(st, task.id, Some(p.summary), p.handoff, usage).await?;
                    return Ok(());
                }
                if let Err(msg) = verdict {
                    p.failure = Some(format!("Verifikation `{cmd}` fehlgeschlagen:\n{msg}"));
                }
            }
        }
    }
    if from <= Step::Review
        && p.failure.is_none()
        && task.kind != "onempty"
        && project.review_enabled
        && before != after
    {
        let review_route = f.routing.map(|r| r.review.clone());
        if let Some(route) = review_route
            .as_ref()
            .filter(|r| matches!(r.mode, StepMode::Sync | StepMode::Batch))
        {
            let pending = PendingReview {
                summary: p.summary.clone(),
                handoff: p.handoff.clone(),
                follow_ups: p.follow_ups.clone(),
                before: before.clone(),
                after: after.clone(),
            };
            if defer_review(
                st,
                task,
                project,
                f.routing.expect("route"),
                route,
                &pending,
                usage,
            )
            .await?
            {
                return Ok(());
            }
        }
        let owned;
        let exec = match exec {
            Some(a) => a,
            None => match access_for(
                st,
                task.user_id,
                project,
                task.connector_id,
                &task.provider,
                false,
            )
            .await?
            {
                AccessResult::Ok(a) => {
                    owned = a;
                    &owned
                }
                AccessResult::Missing(m) | AccessResult::Denied(m) => {
                    return review_unavailable(st, task, &p, &m, false, usage).await;
                }
            },
        };
        match run_review(
            st,
            task,
            project,
            ReviewCtx {
                before: &before,
                after: &after,
                summary: &p.summary,
                route: review_route
                    .as_ref()
                    .filter(|r| r.mode == StepMode::Harness),
                exec,
                env: f.env,
                approved: &p.approved,
            },
            f.cancel,
            usage,
            f.lim,
        )
        .await?
        {
            ReviewOutcome::Reviewed(r) => {
                if !r.approved {
                    p.failure = Some(format!("Review abgelehnt:\n- {}", r.issues.join("\n- ")));
                }
                p.review = Some(json!(r));
            }
            ReviewOutcome::Unavailable { why, retry } => {
                return review_unavailable(st, task, &p, &why, retry, usage).await;
            }
            ReviewOutcome::Cancelled => {
                finish_cancelled(st, task.id, Some(p.summary), p.handoff, usage).await?;
                return Ok(());
            }
            ReviewOutcome::Blocked(class, reason) => {
                event(st, task, "guard", &reason).await;
                if block_running(st, task.id, class, &reason, usage).await {
                    notify_blocked(st, task, &reason).await;
                }
                return Ok(());
            }
        }
    }
    finish_flow(st, task, project, f.settings, p, None, usage).await
}

/// Continues a task after an approval (`gate.resume`).
pub(crate) async fn resume(f: &Flow<'_>, g: TaskGate, usage: &mut Usage) -> Result<(), RunError> {
    let (st, task, project) = (f.st, f.task, f.project);
    let mut p: Pending =
        sqlx::query_scalar::<_, Option<Value>>("SELECT gate_pending FROM tasks WHERE id=$1")
            .bind(task.id)
            .fetch_one(&st.db)
            .await?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
    if p.after.is_none() {
        p.after = task.snapshot_after.clone();
    }
    if p.before.is_empty() {
        p.before = task.snapshot_before.clone().unwrap_or_default();
    }
    let resume = g.resume.expect("resume set");
    event(
        st,
        task,
        "gate",
        match resume {
            GateResume::Commit => "Freigegeben – Änderungen werden übernommen",
            GateResume::Review => "Review wird wiederholt",
            GateResume::SkipReview => "Freigegeben – ohne Review übernommen",
            GateResume::Push => "Freigegeben – Hochladen",
            GateResume::NoPush => "Nicht hochladen – Auftrag wird abgeschlossen",
        },
    )
    .await;
    match resume {
        GateResume::Commit => {
            // The held changes live in the task's own working tree (moved
            // there from the shared workspace for holds from before WP04).
            let before = prepare_workspace(st, task, project, f.remote.clone()).await?;
            let out = st
                .runner
                .release(
                    crate::runner_client::task_ws(task.id),
                    &cgate::ReleaseRequest {
                        task: task.id.simple().to_string(),
                        message: f.commits.message(""),
                        author_name: f.commits.author_name.to_string(),
                        author_email: f.commits.author_email.to_string(),
                        allow: project.secret_allowlist.clone(),
                        // A human approved changes too large for a full scan.
                        allow_large: p.approved.iter().any(|f| f.kind == FindingKind::TooLarge),
                    },
                )
                .await?;
            if out.conflict {
                let msg = "Die freigegebenen Änderungen passen nicht mehr zum aktuellen Stand (andere Aufträge haben dieselben Stellen geändert). Bitte den Auftrag wiederholen.";
                event(st, task, "error", msg).await;
                if block_running(st, task.id, FailureClass::Gate, msg, usage).await {
                    notify_blocked(st, task, msg).await;
                }
                return Ok(());
            }
            if !out.blocked.is_empty() {
                p.before = before.clone();
                return hold(
                    st,
                    task,
                    GateStage::Changes,
                    out.blocked,
                    &p,
                    None,
                    true,
                    usage,
                )
                .await;
            }
            // The held changes become one commit on top of the task's base
            // in its own working tree, copied to its branch.
            super::merge::sync(st, project.id, task.id, &out.sha).await?;
            p.before = before;
            p.after = Some(out.sha.clone());
            set_snapshot_after(st, task.id, &out.sha).await?;
            // Approved agent configuration is the new trusted state (no
            // second question before the next run).
            if p.approved
                .iter()
                .any(|f| f.kind == FindingKind::AgentConfig)
            {
                st.runner
                    .baseline_approve(project.id, "agent-config", &out.sha)
                    .await?;
            }
            after_commit(f, p, None, Step::Verify, usage).await
        }
        GateResume::Review => {
            // The review agent runs in the task's working tree.
            prepare_workspace(st, task, project, f.remote.clone()).await?;
            after_commit(f, p, None, Step::Review, usage).await
        }
        GateResume::SkipReview => after_commit(f, p, None, Step::Push, usage).await,
        GateResume::Push => finish_flow(st, task, project, f.settings, p, Some(true), usage).await,
        GateResume::NoPush => {
            finish_flow(st, task, project, f.settings, p, Some(false), usage).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_paths_cover_manifests_cache_files_and_script_paths() {
        let env = Env {
            setup_script: "npm ci && ./scripts/bootstrap.sh --fast; python tools/gen.py > /dev/null; curl https://x.example/a.sh | sh; echo $HOME/.cfg".into(),
            cache_files: vec!["web/package-lock.json".into()],
            ..Default::default()
        };
        let p = setup_paths(&env);
        for want in [
            "package.json",
            "package-lock.json",
            "requirements*.txt",
            "web/package-lock.json",
            "scripts/bootstrap.sh",
            "tools/gen.py",
            "Makefile",
        ] {
            assert!(p.contains(&want.to_string()), "{want} missing in {p:?}");
        }
        assert!(
            !p.iter()
                .any(|x| x.contains("://") || x.starts_with('/') || x.starts_with('$')),
            "{p:?}"
        );
        assert!(queued_common::glob::matches_any(
            &p,
            "web/requirements-dev.txt"
        ));
    }

    #[test]
    fn pending_roundtrip_and_old_rows() {
        let p = Pending {
            summary: "s".into(),
            failure: Some("f".into()),
            questions: vec!["q".into()],
            before: "a".into(),
            after: Some("b".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(serde_json::from_value::<Pending>(v).unwrap(), p);
        assert_eq!(
            serde_json::from_value::<Pending>(json!({})).unwrap(),
            Pending::default()
        );
        assert!(Step::Verify < Step::Review && Step::Review < Step::Push);
    }
}
