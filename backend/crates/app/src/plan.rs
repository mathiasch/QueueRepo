//! WP06 plan mode: holding a plan for approval and the decisions on it
//! (genehmigen – optionally edited –, überarbeiten lassen, verwerfen,
//! automatic approval). The plan run itself is in `runner_plan.rs`.
//!
//! A plan waits as a WP03 hold: `status='held'` with gate stage `plan`, so
//! the inbox, the "Freigabe nötig" section, notifications and cancelling
//! work as for every other decision.
use chrono::{Duration, Utc};
use queued_api::{
    gate::{GateStage, TaskGate},
    steer::{PlanMeta, PlanState, MAX_PLAN_REVISIONS},
};
use uuid::Uuid;

use crate::{
    models::{Task, Usage},
    notify::{notify, Note},
    queue,
    state::AppState,
};

/// The task must plan (again) before it may change anything.
pub fn pending(t: &Task) -> bool {
    t.plan_mode
        && matches!(t.kind.as_str(), "task" | "continue")
        && t.plan_state_kind() != Some(PlanState::Approved)
}

pub async fn mark_planning(st: &AppState, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE tasks SET plan_state='planning' WHERE id=$1")
        .bind(id)
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Holds `plan` for a human (from the plan run or the single-shot step).
/// `from` is the status the task is in now (`running` / `waiting_batch`).
pub async fn hold(
    st: &AppState,
    task: &Task,
    from: &str,
    plan: &str,
    mut meta: PlanMeta,
    usage: &Usage,
) -> sqlx::Result<bool> {
    let auto: Option<i32> =
        sqlx::query_scalar("SELECT plan_auto_approve_mins FROM projects WHERE id=$1")
            .bind(task.project_id)
            .fetch_one(&st.db)
            .await?;
    let now = Utc::now();
    meta.created_at = Some(now);
    meta.auto_approve_at = auto
        .filter(|_| meta.may_auto_approve())
        .map(|m| now + Duration::minutes(i64::from(m)));
    let gate = TaskGate {
        stage: GateStage::Plan,
        findings: vec![],
        held_at: now,
        head: None,
        has_diff: false,
        resume: None,
    };
    let plan: String = queued_common::secrets::redact(plan.trim())
        .chars()
        .take(queued_api::steer::MAX_PLAN_CHARS)
        .collect();
    let n = sqlx::query(
        "UPDATE tasks SET status='held', plan=$3, plan_state='ready', plan_meta=$4, gate=$5, usage=$6,
                failure_class=NULL, error=NULL, updated_at=now()
         WHERE id=$1 AND status=$2",
    )
    .bind(task.id)
    .bind(from)
    .bind(&plan)
    .bind(serde_json::to_value(&meta).unwrap_or_default())
    .bind(serde_json::to_value(&gate).unwrap_or_default())
    .bind(serde_json::to_value(usage).unwrap_or_default())
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(false);
    }
    let mut what = String::from("Plan erstellt – wartet auf deine Freigabe");
    if meta.attempted_total > 0 {
        what.push_str(&format!(
            ". Achtung: Der Plan-Lauf hat versucht, {} Datei(en) zu ändern ({}); die Änderungen wurden verworfen.",
            meta.attempted_total,
            meta.attempted_changes.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(at) = meta.auto_approve_at {
        what.push_str(&format!(
            ". Wird am {} UTC automatisch genehmigt, wenn niemand entscheidet.",
            at.format("%d.%m. %H:%M")
        ));
    }
    queue::event(st, task, "plan", &what).await;
    queue::emit_task(st, task.id).await;
    notify(
        st,
        task.user_id,
        Note {
            kind: "held",
            title: format!("Plan wartet auf Freigabe: {}", task.title),
            body: plan.chars().take(300).collect(),
            task_id: Some(task.id),
            push: true,
        },
    )
    .await;
    Ok(true)
}

/// Result of a decision.
#[derive(Debug, PartialEq, Eq)]
pub enum Decided {
    Ok,
    /// The task does not wait for a plan decision (any more).
    NotHeld,
}

const HELD_PLAN: &str = "status='held' AND gate->>'stage'='plan'";

/// Genehmigen (optionally with an edited plan); `by` = name or "automatisch".
pub async fn approve(
    st: &AppState,
    t: &Task,
    by: &str,
    edited: Option<String>,
    auto: bool,
) -> sqlx::Result<Decided> {
    let mut meta = t.plan_meta_parsed().unwrap_or_default();
    meta.edited = edited.is_some();
    meta.approved_by = Some(by.to_string());
    meta.auto_approved = auto;
    meta.auto_approve_at = None;
    let edited = edited.map(|p| queued_common::secrets::redact(&p));
    let n = sqlx::query(&format!(
        "UPDATE tasks SET status='queued', gate=NULL, plan_state='approved', plan=COALESCE($2, plan),
                plan_meta=$3, plan_feedback=NULL, transient_retries=0, not_before=NULL, updated_at=now(),
                position = (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id=$4 AND status='queued')
         WHERE id=$1 AND {HELD_PLAN}"
    ))
    .bind(t.id)
    .bind(&edited)
    .bind(serde_json::to_value(&meta).unwrap_or_default())
    .bind(t.user_id)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(Decided::NotHeld);
    }
    let what = match (auto, edited.is_some()) {
        (true, _) => {
            "Plan automatisch genehmigt (keine Entscheidung in der eingestellten Zeit)".to_string()
        }
        (false, true) => format!("Plan geändert und genehmigt (von {by}) – der Agent setzt ihn um"),
        (false, false) => format!("Plan genehmigt (von {by}) – der Agent setzt ihn um"),
    };
    queue::event(st, t, "plan", &what).await;
    queue::emit_task(st, t.id).await;
    st.wake.notify_one();
    Ok(Decided::Ok)
}

/// Überarbeiten lassen: a new plan run with the note and the old plan.
/// `Err` = too many revisions.
pub async fn revise(
    st: &AppState,
    t: &Task,
    by: &str,
    note: &str,
) -> sqlx::Result<Result<Decided, String>> {
    if t.plan_revision >= MAX_PLAN_REVISIONS {
        return Ok(Err(format!(
            "Höchstens {MAX_PLAN_REVISIONS} Überarbeitungen – genehmige, bearbeite oder verwirf den Plan"
        )));
    }
    let n = sqlx::query(&format!(
        "UPDATE tasks SET status='queued', gate=NULL, plan_state=NULL, plan_feedback=$2,
                plan_revision=plan_revision+1, updated_at=now(),
                position = (SELECT COALESCE(MIN(position), 0) - 1 FROM tasks WHERE user_id=$3 AND status='queued')
         WHERE id=$1 AND {HELD_PLAN}"
    ))
    .bind(t.id)
    .bind(note)
    .bind(t.user_id)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(Ok(Decided::NotHeld));
    }
    queue::event(
        st,
        t,
        "plan",
        &format!(
            "Neuer Plan angefordert (von {by}): {}",
            note.chars().take(500).collect::<String>()
        ),
    )
    .await;
    queue::emit_task(st, t.id).await;
    st.wake.notify_one();
    Ok(Ok(Decided::Ok))
}

/// Verwerfen: the task ends without changing anything.
pub async fn reject(st: &AppState, t: &Task, by: &str) -> anyhow::Result<Decided> {
    let n = sqlx::query(&format!(
        "UPDATE tasks SET status='cancelled', failure_class='cancelled', error='Plan verworfen – nichts wurde geändert',
                gate=NULL, plan_state='rejected', finished_at=now(), updated_at=now()
         WHERE id=$1 AND {HELD_PLAN}"
    ))
    .bind(t.id)
    .execute(&st.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Ok(Decided::NotHeld);
    }
    queue::event(
        st,
        t,
        "plan",
        &format!("Plan verworfen (von {by}) – der Auftrag ist beendet, nichts wurde geändert"),
    )
    .await;
    queue::emit_task(st, t.id).await;
    crate::steer::settle(st, t.id).await?;
    crate::runner::merge::cleanup_task(st, t.project_id, t.id, true).await;
    st.wake.notify_one();
    Ok(Decided::Ok)
}

/// Scheduler tick: plans whose automatic approval is due (project setting,
/// never with questions or attempted changes – `auto_approve_at` is only
/// set then, and re-checked here against the current project setting).
pub async fn auto_approve_due(st: &AppState) -> anyhow::Result<usize> {
    let due: Vec<Task> = sqlx::query_as(
        "SELECT t.* FROM tasks t JOIN projects p ON p.id = t.project_id
         WHERE t.status='held' AND t.gate->>'stage'='plan' AND p.plan_auto_approve_mins IS NOT NULL
           AND (t.plan_meta->>'auto_approve_at')::timestamptz <= now()
           AND (t.gate->>'held_at')::timestamptz + make_interval(mins => p.plan_auto_approve_mins) <= now()
         LIMIT 50",
    )
    .fetch_all(&st.db)
    .await?;
    let mut n = 0;
    for t in due {
        if !t.plan_meta_parsed().is_some_and(|m| m.may_auto_approve()) {
            continue;
        }
        if approve(st, &t, "automatisch", None, true).await? == Decided::Ok {
            n += 1;
        }
    }
    Ok(n)
}
