//! Output modes `branch` and `pr` (WP05): the merge-queue step that pushes
//! a finished, checked task branch instead of merging it locally.
//!
//! Who pushes when:
//! * only this step (scheduled by the merge queue, one per project at a
//!   time) pushes task branches; it pushes **exactly** the gated commit
//!   (`snapshot_after`: gate → commit → verify → review), from the main
//!   repository, to the project's remote URL (`push_checked`: secret scan,
//!   default-branch protection, never forced);
//! * a new task pushes `queue/<id>` (must not exist on the remote yet);
//! * a pull-request follow-up (and its fixes) pushes to the request's head
//!   branch, fast-forward from the state it started on; if someone pushed
//!   there meanwhile, nothing is pushed and a fix task continues the
//!   changes on the new state (`carry_from`, like a merge conflict);
//! * in mode `pr` the poller (`pr.rs`) then creates the request on the host.
//!
//! Child module of `runner` (shares its private helpers).
use queued_api::{branch::MergeState, gate::GateStage};
use queued_common::{GitRemote, PushRequest};

use super::gate::Pending;
use super::*;

pub(crate) async fn publish(
    st: &AppState,
    task: &Task,
    project: &Project,
    settings: &Settings,
    p: Pending,
    usage: &mut Usage,
) -> Result<(), RunError> {
    let mode = project.output();
    let (Some(branch), Some(tip)) = (task.branch.clone(), task.snapshot_after.clone()) else {
        return Err(RunError::Config(
            "Auftrag hat keinen Branch zum Hochladen".into(),
        ));
    };
    let Some(remote) = project_remote(st, project) else {
        return Err(RunError::Config(
            "Zum Hochladen braucht das Projekt eine Remote-URL (Projekt → Einstellungen).".into(),
        ));
    };
    let pr_row = match task.pr_id {
        Some(id) => Some(crate::pr::load(st, id).await?.ok_or_else(|| {
            RunError::Config("Der Pull Request dieses Auftrags existiert nicht mehr.".into())
        })?),
        None => None,
    };
    if let Some(row) = &pr_row {
        if !row.state().is_open() {
            return Err(RunError::Config(format!(
                "{} ist inzwischen {} – die Änderungen wurden nicht hochgeladen.",
                row.label(),
                row.state().label(false).to_lowercase()
            )));
        }
    } else if mode == queued_api::pr::OutputMode::Pr {
        // Fail before pushing when the pull request could not be created.
        if let Err(msg) = crate::pr::client(st, project) {
            return Err(RunError::Config(format!(
                "Pull Request nicht möglich: {msg} (Projekt → Einstellungen → Ergebnis)."
            )));
        }
    }
    // Target branch on the remote and its expected state.
    let (head, expect) = match &pr_row {
        Some(row) => (
            row.head_branch.clone(),
            task.merge_base.clone().unwrap_or_default(),
        ),
        None => (branch.clone(), String::new()),
    };
    event(
        st,
        task,
        "merge",
        &format!("Hochladen von {} nach {head}", &tip[..8.min(tip.len())]),
    )
    .await;
    let r = st
        .runner
        .push(
            project.id,
            &PushRequest {
                remote: GitRemote {
                    branch: head.clone(),
                    ..remote
                },
                sha: Some(tip.clone()),
                allow_protected: p.push_approved,
                allow: project.secret_allowlist.clone(),
                allow_secrets: p.push_approved,
                scan_base: Some(project.git_branch.clone()),
                expect: Some(expect),
            },
        )
        .await?;
    if !r.held.is_empty() {
        sqlx::query("UPDATE tasks SET merge_state='queued' WHERE id=$1")
            .bind(task.id)
            .execute(&st.db)
            .await?;
        return gate::hold(
            st,
            task,
            GateStage::Push,
            r.held,
            &p,
            Some(tip),
            false,
            usage,
        )
        .await;
    }
    if r.moved {
        let f = format!(
            "Der Branch {head} wurde auf dem Git-Server inzwischen geändert ({}). Queue überschreibt fremde Arbeit nie. \
             Der Korrektur-Auftrag startet auf dem neuen Stand mit diesen Änderungen; bei Konflikten stehen Marker \
             (<<<<<<< … >>>>>>>) in den Dateien – beide Seiten sinnvoll zusammenführen.",
            secrets::redact(&r.output)
        );
        event(st, task, "merge", &f).await;
        return merge::merge_failed(st, task, settings, p, MergeState::Conflict, &f, usage).await;
    }
    if !r.ok {
        return Err(RunError::Infra(anyhow::anyhow!(
            "git push: {}",
            secrets::redact(&r.output)
        )));
    }
    sqlx::query(
        "UPDATE tasks SET merge_state='published', not_before=NULL, updated_at=now() WHERE id=$1",
    )
    .bind(task.id)
    .execute(&st.db)
    .await?;
    event(
        st,
        task,
        "merge",
        &format!("Branch {head} hochgeladen ({})", &tip[..8.min(tip.len())]),
    )
    .await;
    match &pr_row {
        Some(row) => {
            crate::pr::pushed(st, row.id, &tip).await?;
            event(
                st,
                task,
                "pr",
                &format!("Neuer Stand für {} hochgeladen", row.label()),
            )
            .await;
        }
        None if mode == queued_api::pr::OutputMode::Pr => {
            crate::pr::open_for(st, project, task, &head, &tip).await?;
            event(st, task, "pr", "Pull Request wird angelegt").await;
        }
        None => {}
    }
    let end = End {
        summary: p.summary,
        handoff: p.handoff,
        review: p.review,
        usage,
    };
    finish_done(st, task, settings, end, p.follow_ups).await?;
    // The branch stays (diff; the request may still get follow-ups).
    merge::cleanup_task(st, project.id, task.id, false).await;
    Ok(())
}
