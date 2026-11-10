//! Branch per task and the merge queue (WP04).
//!
//! Every task runs in its own working tree on branch `queue/<id>` (runner:
//! `tasks.rs`). A finished, checked task waits in the project's merge
//! queue (`status='merging'`); the scheduler merges one task per project at
//! a time: squash onto the target branch → gate re-check → verify again if
//! the target moved → fast-forward → gated push → done. Conflicts and
//! failed checks become a fix task that continues the task's changes
//! (`carry_from`).
//!
//! Child module of `runner` (shares its private helpers).
use queued_api::branch::MergeState;
use queued_common::{Carry, MergeFinish, MergeRequest, TaskWorkspaceRequest};

use super::gate::Pending;
use super::*;

/// Statuses after which a task never runs again by itself.
const TERMINAL: [&str; 4] = ["done", "failed", "blocked", "cancelled"];

/// Branches of failed/blocked tasks are kept this long (diff, retry, fix).
const KEEP_FAILED_DAYS: i32 = 7;

/// The task's branch; assigned on its first run (`queue/<12 hex>`, the full
/// id when the short name is taken in the project).
async fn branch_of(st: &AppState, task: &Task) -> Result<String, RunError> {
    if let Some(b) = sqlx::query_scalar::<_, Option<String>>("SELECT branch FROM tasks WHERE id=$1")
        .bind(task.id)
        .fetch_one(&st.db)
        .await?
    {
        return Ok(b);
    }
    let hex = task.id.simple().to_string();
    for long in [false, true] {
        let b = queued_common::task_branch(&hex, long);
        let r = sqlx::query("UPDATE tasks SET branch=$2 WHERE id=$1 AND branch IS NULL")
            .bind(task.id)
            .bind(&b)
            .execute(&st.db)
            .await;
        match r {
            Ok(_) => return Ok(b),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() && !long => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("the long branch name is unique")
}

/// Changes of the task this fix continues (unmerged work on its branch).
async fn carry_of(st: &AppState, task: &Task) -> Result<Option<(Carry, String)>, RunError> {
    let Some(pid) = task.carry_from else {
        return Ok(None);
    };
    let Some(parent) = queue::load_task(st, pid).await? else {
        return Ok(None);
    };
    let (Some(base), Some(tip)) = (parent.diff_base(), parent.snapshot_after.as_deref()) else {
        return Ok(None);
    };
    if parent.branch.is_none() || parent.on_target() || base == tip {
        return Ok(None);
    }
    Ok(Some((
        Carry {
            base: base.to_string(),
            tip: tip.to_string(),
            label: format!(
                "Übernommen aus „{}“\n\nQueue-Task: {}",
                cut(&parent.title, 120),
                parent.id
            ),
        },
        parent.title,
    )))
}

/// WP05: a pull-request follow-up starts on the request's branch as it is
/// on the host now (fetched by the runner from the project's remote URL).
async fn pr_start(
    st: &AppState,
    task: &Task,
    project: &Project,
) -> Result<Option<queued_common::GitRemote>, RunError> {
    let Some(id) = task.pr_id else {
        return Ok(None);
    };
    let row = crate::pr::load(st, id).await?.ok_or_else(|| {
        RunError::Config("Der Pull Request dieses Auftrags existiert nicht mehr.".into())
    })?;
    if !row.state().is_open() {
        return Err(RunError::Config(format!(
            "{} ist inzwischen {} – der Folge-Auftrag läuft nicht mehr.",
            row.label(),
            row.state().label(false).to_lowercase()
        )));
    }
    let remote = project_remote(st, project)
        .ok_or_else(|| RunError::Config("Das Projekt hat keine Remote-URL mehr.".into()))?;
    Ok(Some(queued_common::GitRemote {
        branch: row.head_branch,
        ..remote
    }))
}

/// Creates or continues the task's own working tree after the main
/// repository was prepared. Stores branch, merge base and gate base;
/// returns the gate base (`snapshot_before`).
pub(crate) async fn prepare(
    st: &AppState,
    task: &Task,
    project: &Project,
) -> Result<String, RunError> {
    let branch = branch_of(st, task).await?;
    let carry = carry_of(st, task).await?;
    let start = pr_start(st, task, project).await?;
    let merge_base: Option<String> = sqlx::query_scalar("SELECT merge_base FROM tasks WHERE id=$1")
        .bind(task.id)
        .fetch_one(&st.db)
        .await?;
    let r = st
        .runner
        .prepare_task(
            project.id,
            task.id,
            &TaskWorkspaceRequest {
                branch: branch.clone(),
                merge_base: merge_base.clone(),
                carry: carry.as_ref().map(|(c, _)| c.clone()),
                allow: project.secret_allowlist.clone(),
                start,
            },
        )
        .await?;
    for w in &r.warnings {
        event(st, task, "status", &format!("git: {w}")).await;
    }
    // Earlier work of this task (an existing working tree, or its branch):
    // the gate keeps comparing against the task's first base, so partial
    // commits of interrupted runs are checked before they can be merged.
    let (before, base) = match (r.resumed, task.snapshot_before.clone(), merge_base) {
        (true, Some(b), Some(m)) => (b, m),
        _ => (r.head.clone(), r.merge_base.clone()),
    };
    if r.created {
        let what = match (&r.carried, &carry) {
            (Some(_), Some((_, title))) if !r.conflicts.is_empty() => format!(
                "Eigener Branch {branch} – startet mit den Änderungen von „{title}“; Konflikte in: {}",
                r.conflicts.join(", ")
            ),
            (Some(_), Some((_, title))) => {
                format!("Eigener Branch {branch} – startet mit den Änderungen von „{title}“")
            }
            _ => format!("Eigener Branch {branch} ab {}", &base[..8.min(base.len())]),
        };
        event(st, task, "status", &what).await;
    }
    sqlx::query(
        "UPDATE tasks SET snapshot_before=$2, merge_base=$3, workspace_cleaned=FALSE WHERE id=$1",
    )
    .bind(task.id)
    .bind(&before)
    .bind(&base)
    .execute(&st.db)
    .await?;
    Ok(before)
}

/// Copies a task commit to its branch in the main repository.
pub(crate) async fn sync(
    st: &AppState,
    project: Uuid,
    task: Uuid,
    sha: &str,
) -> anyhow::Result<()> {
    let branch: Option<String> = sqlx::query_scalar("SELECT branch FROM tasks WHERE id=$1")
        .bind(task)
        .fetch_one(&st.db)
        .await?;
    if let Some(b) = branch {
        st.runner.sync_task(project, task, &b, sha).await?;
    }
    Ok(())
}

/// After a successful run (verified, reviewed): into the merge queue.
/// Returns `false` when there is nothing to merge (the caller finishes).
pub(crate) async fn enqueue(
    st: &AppState,
    task: &Task,
    p: &Pending,
    usage: &Usage,
) -> Result<bool, RunError> {
    let (branch, base, state, after): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT branch, merge_base, merge_state, snapshot_after FROM tasks WHERE id=$1",
    )
    .bind(task.id)
    .fetch_one(&st.db)
    .await?;
    if branch.is_none()
        || matches!(
            state.as_deref().and_then(MergeState::parse),
            Some(MergeState::Merged | MergeState::Published)
        )
    {
        return Ok(false);
    }
    let after = p.after.clone().or(after);
    if after.is_none() || after == base {
        return Ok(false);
    }
    let n = sqlx::query(
        "UPDATE tasks SET status='merging', merge_state='queued', merge_queued_at=now(), not_before=NULL,
                gate_pending=$2, usage=$3, summary=COALESCE(NULLIF($4, ''), summary),
                handoff=COALESCE($5, handoff), review=COALESCE($6, review), error=NULL, updated_at=now()
         WHERE id=$1 AND status IN ('running','waiting_batch')",
    )
    .bind(task.id)
    .bind(serde_json::to_value(p).unwrap_or_default())
    .bind(serde_json::to_value(usage).unwrap_or_default())
    .bind(secrets::redact(&p.summary))
    .bind(p.handoff.as_deref().map(secrets::redact))
    .bind(p.review.clone())
    .execute(&st.db)
    .await?
    .rows_affected();
    queue::emit_task(st, task.id).await;
    if n > 0 {
        event(
            st,
            task,
            "merge",
            "Fertig und geprüft – wartet auf die Übernahme in den Hauptstand",
        )
        .await;
        st.wake.notify_one();
    }
    Ok(true)
}

/// One merge of the project's merge queue (spawned by the scheduler with
/// `merge_state='merging'`).
pub async fn run_merge(st: AppState, task: Task, cancel: CancellationToken, gen: u64) {
    let id = task.id;
    let mut usage: Usage = task
        .usage
        .clone()
        .and_then(|u| serde_json::from_value(u).ok())
        .unwrap_or_default();
    match merge_inner(&st, &task, &cancel, &mut usage).await {
        Ok(()) => {}
        Err(_) if cancel.is_cancelled() => {
            let _ = finish_cancelled(&st, id, None, None, &usage).await;
            cleanup_task(&st, task.project_id, id, true).await;
        }
        Err(RunError::Config(msg)) => {
            event(&st, &task, "error", &msg).await;
            if block_running(&st, id, FailureClass::Config, &msg, &usage).await {
                notify_blocked(&st, &task, &msg).await;
            }
        }
        Err(RunError::Infra(e)) => {
            let msg = format!("{e:#}");
            tracing::error!("task {id}: merge failed: {msg}");
            event(
                &st,
                &task,
                "error",
                &format!("Übernahme unterbrochen: {msg}"),
            )
            .await;
            requeue_merge(&st, &task, &msg).await;
        }
    }
    st.untrack(id, gen).await;
    st.wake.notify_one();
}

/// Back into the merge queue with backoff (infrastructure problem); blocked
/// after too many attempts.
async fn requeue_merge(st: &AppState, task: &Task, msg: &str) {
    let n = task.transient_retries + 1;
    if n > st.cfg.transient_max_retries {
        let m = format!(
            "Übernahme nach {} Versuchen aufgegeben: {msg}",
            st.cfg.transient_max_retries
        );
        let usage = Usage::default();
        if block_running(st, task.id, FailureClass::Infra, &m, &usage).await {
            notify_blocked(st, task, &m).await;
        }
        return;
    }
    let delay = limits::backoff(n, st.cfg.backoff_base_secs, st.cfg.backoff_max_secs);
    let not_before = Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default();
    let _ = sqlx::query(
        "UPDATE tasks SET merge_state='queued', transient_retries=$2, not_before=$3, error=$4, updated_at=now()
         WHERE id=$1 AND status='merging'",
    )
    .bind(task.id)
    .bind(n)
    .bind(not_before)
    .bind(cut(msg, 2000))
    .execute(&st.db)
    .await;
    queue::emit_task(st, task.id).await;
    let wake = st.wake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        wake.notify_one();
    });
}

async fn merge_inner(
    st: &AppState,
    task: &Task,
    cancel: &CancellationToken,
    usage: &mut Usage,
) -> Result<(), RunError> {
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
    let mut p: Pending =
        sqlx::query_scalar::<_, Option<Value>>("SELECT gate_pending FROM tasks WHERE id=$1")
            .bind(task.id)
            .fetch_one(&st.db)
            .await?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
    let (Some(branch), Some(base), Some(tip)) = (
        task.branch.clone(),
        task.merge_base.clone(),
        task.snapshot_after.clone(),
    ) else {
        return Err(RunError::Config(
            "Auftrag hat keinen Branch zum Übernehmen".into(),
        ));
    };
    // WP05: output `branch`/`pr` (and every pull-request follow-up): push
    // the task branch instead of merging it.
    if task.pr_id.is_some() || project.output().pushes_branch() {
        return super::publish::publish(st, task, &project, &settings, p, usage).await;
    }
    let env = environment::load(&st.db, project.id).await?;
    let verify = project
        .verify_command
        .clone()
        .filter(|c| !c.trim().is_empty() && task.kind != "onempty");
    let test_task = task.allow_test_changes;
    event(
        st,
        task,
        "merge",
        &format!("Übernahme von {branch} beginnt"),
    )
    .await;
    for round in 0..3 {
        let m = st
            .runner
            .merge_task(
                project.id,
                task.id,
                &MergeRequest {
                    branch: branch.clone(),
                    base: base.clone(),
                    tip: tip.clone(),
                    message: format!(
                        "{}\n\nQueue-Task: {}\nProvider: {}\nBranch: {branch}",
                        task.title, task.id, task.provider
                    ),
                    author_name: author_name.clone(),
                    author_email: author_email.clone(),
                    protected: if test_task {
                        vec![]
                    } else {
                        project.protected()
                    },
                    test_guard: project.test_guard && !test_task,
                    allow: project.secret_allowlist.clone(),
                    checkout: verify.is_some(),
                },
            )
            .await?;
        if !m.conflicts.is_empty() {
            let files = m.conflicts.join(", ");
            event(
                st,
                task,
                "merge",
                &format!("Konflikt beim Übernehmen in: {files}"),
            )
            .await;
            let f = format!(
                "Konflikt beim Übernehmen in den Hauptstand: Inzwischen übernommene Aufträge haben dieselben Stellen geändert ({files}). \
                 Der Korrektur-Auftrag startet mit diesen Änderungen auf dem aktuellen Stand; in den genannten Dateien stehen Konfliktmarker \
                 (<<<<<<< … ======= … >>>>>>>). Beide Seiten sinnvoll zusammenführen und alle Marker entfernen."
            );
            return merge_failed(st, task, &settings, p, MergeState::Conflict, &f, usage).await;
        }
        if !m.markers.is_empty() {
            let f = format!(
                "Konfliktmarker (<<<<<<< / >>>>>>>) sind noch in: {}. Bitte alle Konflikte auflösen und die Marker entfernen.",
                m.markers.join(", ")
            );
            return merge_failed(st, task, &settings, p, MergeState::Failed, &f, usage).await;
        }
        if !m.new_findings.is_empty() {
            // Cannot happen for a clean 3-way merge of a gated change; the
            // re-check is fail-closed anyway.
            let list = m
                .new_findings
                .iter()
                .map(|f| format!("{} {}", queued_api::gate::finding_title(f.kind), f.path))
                .collect::<Vec<_>>()
                .join("; ");
            let msg = format!(
                "Übernahme gestoppt: Die Nachprüfung der zusammengeführten Änderungen hat neue Funde ergeben ({list}). Bitte den Auftrag prüfen und wiederholen."
            );
            event(st, task, "gate", &msg).await;
            let _ = set_merge_state(st, task.id, MergeState::Failed).await;
            if block_running(st, task.id, FailureClass::Gate, &msg, usage).await {
                notify_blocked(st, task, &msg).await;
            }
            cleanup_task(st, project.id, task.id, false).await;
            return Ok(());
        }
        let commit = m.commit.clone().unwrap_or_default();
        if m.moved {
            if let Some(cmd) = &verify {
                event(
                    st,
                    task,
                    "verify",
                    &format!("$ {cmd} (nach Aktualisierung auf den neuesten Hauptstand)"),
                )
                .await;
                let verdict = run_verify(st, task, cmd, cancel, &env).await?;
                if cancel.is_cancelled() {
                    finish_cancelled(st, task.id, None, None, usage).await?;
                    cleanup_task(st, project.id, task.id, true).await;
                    return Ok(());
                }
                if let Err(out) = verdict {
                    let f = format!(
                        "Verifikation `{cmd}` nach dem Aktualisieren auf den neuesten Hauptstand fehlgeschlagen (andere Aufträge wurden inzwischen übernommen):\n{out}"
                    );
                    return merge_failed(st, task, &settings, p, MergeState::Failed, &f, usage)
                        .await;
                }
            }
        }
        let fin = st
            .runner
            .merge_finish(
                project.id,
                task.id,
                &MergeFinish {
                    branch: branch.clone(),
                    commit: commit.clone(),
                    target: m.target.clone(),
                    allow: project.secret_allowlist.clone(),
                },
            )
            .await?;
        if !fin.merged {
            event(
                st,
                task,
                "merge",
                &format!(
                    "Der Hauptstand hat sich währenddessen geändert – neuer Versuch ({})",
                    round + 2
                ),
            )
            .await;
            continue;
        }
        sqlx::query(
            "UPDATE tasks SET merge_state='merged', merge_base=$2, snapshot_before=$2, snapshot_after=$3,
                    not_before=NULL, updated_at=now() WHERE id=$1",
        )
        .bind(task.id)
        .bind(&m.target)
        .bind(&commit)
        .execute(&st.db)
        .await?;
        event(
            st,
            task,
            "merge",
            &format!(
                "In den Hauptstand übernommen ({})",
                &commit[..8.min(commit.len())]
            ),
        )
        .await;
        // Changes of the tasks this one continued are merged with it.
        clean_carried(st, project.id, task).await;
        let fresh = queue::load_task(st, task.id)
            .await?
            .unwrap_or_else(|| task.clone());
        p.before = m.target.clone();
        p.after = Some(commit);
        let r = gate::finish_flow(st, &fresh, &project, &settings, p, None, usage).await;
        cleanup_task(st, project.id, task.id, true).await;
        return r;
    }
    Err(RunError::Infra(anyhow::anyhow!(
        "der Hauptstand ändert sich laufend"
    )))
}

async fn set_merge_state(st: &AppState, id: Uuid, m: MergeState) -> sqlx::Result<()> {
    sqlx::query("UPDATE tasks SET merge_state=$2, updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(m.as_str())
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Conflict or failed check after bringing the branch up to date: a real
/// task failure (fix task continuing the changes while attempts are left).
pub(super) async fn merge_failed(
    st: &AppState,
    task: &Task,
    settings: &Settings,
    p: Pending,
    state: MergeState,
    failure: &str,
    usage: &Usage,
) -> Result<(), RunError> {
    set_merge_state(st, task.id, state).await?;
    let end = End {
        summary: p.summary,
        handoff: p.handoff,
        review: p.review,
        usage,
    };
    finish_failed(st, task, settings, end, failure).await?;
    cleanup_task(st, task.project_id, task.id, false).await;
    Ok(())
}

/// Branches of the tasks whose changes `task` carried (now merged).
async fn clean_carried(st: &AppState, project: Uuid, task: &Task) {
    let mut next = task.carry_from;
    let mut guard = 0;
    while let Some(id) = next {
        guard += 1;
        if guard > 50 {
            break;
        }
        next = sqlx::query_scalar::<_, Option<Uuid>>("SELECT carry_from FROM tasks WHERE id=$1")
            .bind(id)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten()
            .flatten();
        // Its changes are merged: the branch goes even though `task` (still
        // finishing) carried it.
        remove(st, project, id, true).await;
    }
}

/// Removes a task's working tree; with `branch` also its branch in the main
/// repository (best effort – the hourly sweep retries).
pub(crate) async fn cleanup_task(st: &AppState, project: Uuid, id: Uuid, branch: bool) {
    let Ok(Some((b, active))) = sqlx::query_as::<_, (Option<String>, bool)>(
        "SELECT branch, EXISTS (SELECT 1 FROM tasks c WHERE c.carry_from = t.id AND NOT (c.status = ANY($2)))
         FROM tasks t WHERE id=$1",
    )
    .bind(id)
    .bind(TERMINAL.map(String::from).to_vec())
    .fetch_optional(&st.db)
    .await
    else {
        return;
    };
    if b.is_none() {
        return;
    }
    // A fix that has not started yet still needs the branch (carry).
    remove(st, project, id, branch && !active).await;
}

async fn remove(st: &AppState, project: Uuid, id: Uuid, drop_branch: bool) {
    let Ok(Some(Some(b))) =
        sqlx::query_scalar::<_, Option<String>>("SELECT branch FROM tasks WHERE id=$1")
            .bind(id)
            .fetch_optional(&st.db)
            .await
    else {
        return;
    };
    match st
        .runner
        .remove_task(project, id, drop_branch.then_some(b.as_str()))
        .await
    {
        Ok(()) if drop_branch => {
            let _ = sqlx::query("UPDATE tasks SET workspace_cleaned=TRUE WHERE id=$1")
                .bind(id)
                .execute(&st.db)
                .await;
        }
        Ok(()) => {}
        Err(e) => tracing::info!("task {id}: working tree not removed yet: {e:#}"),
    }
}

/// Hourly sweep: working trees of finished tasks; branches of merged,
/// cancelled and done tasks, and of failed/blocked tasks after
/// [`KEEP_FAILED_DAYS`] (unless a pending fix still continues them).
pub async fn sweep(st: &AppState) -> anyhow::Result<()> {
    let rows: Vec<(Uuid, Uuid, String, bool)> = sqlx::query_as(
        "SELECT id, project_id, status,
                (status IN ('done','cancelled') AND merge_state IS DISTINCT FROM 'published')
                OR merge_state = 'merged'
                OR finished_at < now() - make_interval(days => $2)
         FROM tasks
         WHERE branch IS NOT NULL AND NOT workspace_cleaned AND status = ANY($1)
         ORDER BY finished_at NULLS FIRST LIMIT 200",
    )
    .bind(TERMINAL.map(String::from).to_vec())
    .bind(KEEP_FAILED_DAYS)
    .fetch_all(&st.db)
    .await?;
    for (id, project, _, drop_branch) in rows {
        cleanup_task(st, project, id, drop_branch).await;
    }
    Ok(())
}

/// Tasks of a project whose working tree may still exist (before the project
/// is deleted; the working trees are removed afterwards with
/// [`remove_working_trees`]).
pub async fn working_trees(st: &AppState, project: Uuid) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM tasks WHERE project_id=$1 AND branch IS NOT NULL AND NOT workspace_cleaned",
    )
    .bind(project)
    .fetch_all(&st.db)
    .await
}

/// Removes the working trees of a deleted project's tasks (best effort).
pub async fn remove_working_trees(st: &AppState, project: Uuid, tasks: &[Uuid]) {
    for id in tasks {
        if let Err(e) = st.runner.remove_task(project, *id, None).await {
            tracing::warn!("working tree of task {id} not removed: {e:#}");
        }
    }
}
