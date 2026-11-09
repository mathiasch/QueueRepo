//! Eval set (WP13, basis for WP15): small, reproducible coding tasks with a
//! setup script, a task prompt and a verify script. A run creates one hidden
//! project per case (`projects.is_eval`), runs the setup through the runner,
//! commits it and queues the task on the chosen access/model. Success = the
//! task ends `done` (agent contract + verify command). No review, no fix tasks.
//!
//! Mock mode (provider/connector `mock`): the model name sets a simulated
//! strength (`mock-weak` = S, `mock-medium` = M, `mock-strong` = L); cases up
//! to that size get the reference solution handed to the mock agent
//! (`MOCK_SOLUTION_B64:`), the others fail their verify – deterministic,
//! so success rates and costs per "model" are reproducible.
use base64::{engine::general_purpose::STANDARD, Engine};
use queued_api::{
    add_money, EvalCaseInfo, EvalResultView, EvalRunView, EvalSummary, Money, StartEval, Usage,
};
use queued_common::{ExecFrame, ExecRequest, InitWorkspace};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    models::status,
    queue::{self, NewTask},
    routing,
    state::AppState,
};

const CASES_JSON: &str = include_str!("../evals/cases.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub title: String,
    pub category: String,
    pub size: String,
    pub prompt: String,
    pub setup: String,
    pub verify: String,
    pub solution: String,
}

#[derive(Deserialize)]
struct CaseFile {
    cases: Vec<Case>,
}

pub fn cases() -> Vec<Case> {
    serde_json::from_str::<CaseFile>(CASES_JSON)
        .expect("evals/cases.json is valid")
        .cases
}

pub fn infos() -> Vec<EvalCaseInfo> {
    cases()
        .into_iter()
        .map(|c| EvalCaseInfo {
            id: c.id,
            title: c.title,
            category: c.category,
            size: c.size,
        })
        .collect()
}

fn size_rank(s: &str) -> u8 {
    match s {
        "S" => 1,
        "M" => 2,
        _ => 3,
    }
}

/// Simulated strength of a mock "model" (largest size it solves).
pub fn mock_strength(model: Option<&str>) -> u8 {
    match model.unwrap_or("") {
        m if m.contains("weak") => 1,
        m if m.contains("strong") => 3,
        _ => 2,
    }
}

/// Context handed to the mock agent: the solution if the simulated model
/// is strong enough for the case.
pub fn mock_context(case: &Case, model: Option<&str>) -> String {
    if size_rank(&case.size) <= mock_strength(model) {
        format!(
            "MOCK_SOLUTION_B64:{}",
            STANDARD.encode(case.solution.as_bytes())
        )
    } else {
        String::new()
    }
}

/// Runs a shell script in a workspace through the runner (sandboxed).
async fn run_script(
    st: &AppState,
    project: Uuid,
    name: &str,
    script: &str,
) -> anyhow::Result<bool> {
    let req = ExecRequest {
        name: format!("eval-{name}-{}", Uuid::new_v4().simple()),
        workspace: project.to_string(),
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        env: vec![],
        home_files: vec![],
        read_back: vec![],
        stdin: None,
        timeout_secs: 60,
        idle_timeout_secs: None,
        queue_files: vec![],
        // Eval scripts need no network.
        egress: Default::default(),
        cache: None,
        secret_env: vec![],
        home_env: vec![],
    };
    let (tx, mut rx) = mpsc::channel::<ExecFrame>(64);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let last = st.runner.exec(&req, tx).await?;
    let _ = drain.await;
    Ok(matches!(last, ExecFrame::Exit { success: true, .. }))
}

pub async fn start(
    st: &AppState,
    user: Uuid,
    s: StartEval,
) -> anyhow::Result<Result<Uuid, String>> {
    let all = cases();
    let chosen: Vec<Case> = if s.cases.is_empty() {
        all
    } else {
        let c: Vec<Case> = all
            .into_iter()
            .filter(|c| s.cases.contains(&c.id))
            .collect();
        if c.len() != s.cases.len() {
            return Ok(Err("Unbekannter Eval-Fall".into()));
        }
        c
    };
    let settings = queue::load_settings(st, user).await?;
    // Access check once, against a throw-away view of a project without rules.
    if let Some(cid) = s.connector_id {
        if crate::connectors::load_for(st, cid, user, None)
            .await?
            .is_none()
        {
            return Ok(Err(
                "Zugang nicht gefunden (nur eigene, nicht projektgebundene Zugänge)".into(),
            ));
        }
    }
    let run = Uuid::new_v4();
    sqlx::query("INSERT INTO eval_runs (id, user_id, target, status) VALUES ($1,$2,$3,'running')")
        .bind(run)
        .bind(user)
        .bind(serde_json::to_value(&s)?)
        .execute(&st.db)
        .await?;
    for case in &chosen {
        let pid = Uuid::new_v4();
        let project: crate::models::Project = sqlx::query_as(
            "INSERT INTO projects (id, owner_id, name, description, verify_command, review_enabled, is_eval)
             VALUES ($1,$2,$3,$4,$5,FALSE,TRUE) RETURNING *",
        )
        .bind(pid)
        .bind(user)
        .bind(format!("Eval: {}", case.title))
        .bind(format!("Eval-Fall {} ({}, {})", case.id, case.category, case.size))
        .bind(&case.verify)
        .fetch_one(&st.db)
        .await?;
        sqlx::query(
            "INSERT INTO project_members (project_id, user_id, role) VALUES ($1,$2,'owner')",
        )
        .bind(pid)
        .bind(user)
        .execute(&st.db)
        .await?;
        st.runner
            .init(
                pid,
                &InitWorkspace {
                    remote: None,
                    branch: Some("main".into()),
                },
            )
            .await?;
        if !run_script(st, pid, "setup", &case.setup).await? {
            anyhow::bail!("setup of eval case {} failed", case.id);
        }
        let committed = st
            .runner
            .commit(
                pid,
                &format!("eval setup: {}", case.id),
                "queue-eval",
                "eval@localhost",
                &[],
            )
            .await?;
        anyhow::ensure!(
            committed.blocked.is_empty(),
            "eval case {} setup contains a possible secret",
            case.id
        );
        let exec = match routing::resolve_execute(
            st,
            user,
            &project,
            s.connector_id,
            s.provider.as_deref(),
            s.model.clone(),
            &settings.default_provider,
        )
        .await?
        {
            Ok(e) => e,
            Err(msg) => return Ok(Err(msg)),
        };
        let context = if exec.provider == "mock" {
            mock_context(case, exec.model.as_deref())
        } else {
            String::new()
        };
        let task = queue::insert_task(
            st,
            NewTask {
                user_id: user,
                project_id: pid,
                parent_id: None,
                kind: "task",
                title: format!("Eval: {}", case.title),
                prompt: case.prompt.clone(),
                context,
                provider: exec.provider,
                model: exec.model,
                status: status::QUEUED,
                attempt: 0,
                depth: 0,
                created_by_agent: false,
                front: false,
                limits: Default::default(),
                connector_id: exec.connector_id,
                escalation_level: 0,
                allow_test_changes: false,
                carry_from: None,
            },
        )
        .await?;
        sqlx::query(
            "INSERT INTO eval_results (run_id, case_id, task_id, project_id) VALUES ($1,$2,$3,$4)",
        )
        .bind(run)
        .bind(&case.id)
        .bind(task.id)
        .bind(pid)
        .execute(&st.db)
        .await?;
    }
    Ok(Ok(run))
}

type ResultRow = (
    String,
    Option<Uuid>,
    Option<String>,
    Option<Value>,
    Option<Value>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
);

pub fn summarize(results: &[EvalResultView]) -> EvalSummary {
    let finished: Vec<&EvalResultView> = results
        .iter()
        .filter(|r| queued_api::TaskStatus::parse(&r.status).is_some_and(|s| s.is_final()))
        .collect();
    let succeeded = finished.iter().filter(|r| r.success).count();
    let mut cost: Vec<Money> = vec![];
    for r in results {
        for m in &r.cost {
            add_money(&mut cost, &m.currency, m.amount);
        }
    }
    let cost_per_success = if succeeded > 0 {
        cost.iter()
            .map(|m| Money {
                currency: m.currency.clone(),
                amount: m.amount / succeeded as f64,
            })
            .collect()
    } else {
        vec![]
    };
    EvalSummary {
        cases: results.len(),
        finished: finished.len(),
        succeeded,
        success_rate: (!finished.is_empty()).then(|| succeeded as f64 / finished.len() as f64),
        cost,
        cost_per_success,
    }
}

pub async fn view(st: &AppState, user: Uuid, id: Uuid) -> anyhow::Result<Option<EvalRunView>> {
    let run: Option<(
        Value,
        String,
        chrono::DateTime<chrono::Utc>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> = sqlx::query_as(
        "SELECT target, status, created_at, finished_at FROM eval_runs WHERE id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&st.db)
    .await?;
    let Some((target, mut state, created_at, mut finished_at)) = run else {
        return Ok(None);
    };
    let rows: Vec<ResultRow> = sqlx::query_as(
        "SELECT r.case_id, r.task_id, t.status, t.usage, t.cost, t.started_at, t.finished_at
         FROM eval_results r LEFT JOIN tasks t ON t.id = r.task_id WHERE r.run_id=$1 ORDER BY r.case_id",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    let results: Vec<EvalResultView> = rows
        .into_iter()
        .map(
            |(case_id, task_id, status, usage, cost, started, finished)| {
                let status = status.unwrap_or_else(|| "cancelled".into());
                let cost = cost
                    .and_then(|c| serde_json::from_value::<queued_api::TaskCost>(c).ok())
                    .map(|c| c.totals)
                    .unwrap_or_default();
                EvalResultView {
                    case_id,
                    task_id,
                    success: status == "done",
                    status,
                    usage: usage
                        .and_then(|u| serde_json::from_value::<Usage>(u).ok())
                        .unwrap_or_default(),
                    cost,
                    duration_secs: started.zip(finished).map(|(a, b)| (b - a).num_seconds()),
                }
            },
        )
        .collect();
    let summary = summarize(&results);
    if state == "running" && summary.finished == summary.cases {
        let t: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
            "UPDATE eval_runs SET status='finished', finished_at=now() WHERE id=$1 RETURNING finished_at",
        )
        .bind(id)
        .fetch_one(&st.db)
        .await?;
        state = "finished".into();
        finished_at = Some(t);
    }
    let target: StartEval = serde_json::from_value(target).unwrap_or_default();
    let target_label = target_label(st, &target).await;
    Ok(Some(EvalRunView {
        id,
        target,
        target_label,
        status: state,
        created_at,
        finished_at,
        results,
        summary,
    }))
}

async fn target_label(st: &AppState, t: &StartEval) -> String {
    let access = match t.connector_id {
        Some(c) => crate::connectors::load_row(st, c)
            .await
            .ok()
            .flatten()
            .map(|r| r.4)
            .unwrap_or_else(|| "(gelöschter Zugang)".into()),
        None => t.provider.clone().unwrap_or_else(|| "Standard".into()),
    };
    match &t.model {
        Some(m) => format!("{m} über {access}"),
        None => access,
    }
}

pub async fn list(st: &AppState, user: Uuid) -> anyhow::Result<Vec<EvalRunView>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM eval_runs WHERE user_id=$1 ORDER BY created_at DESC LIMIT 20",
    )
    .bind(user)
    .fetch_all(&st.db)
    .await?;
    let mut out = vec![];
    for id in ids {
        if let Some(v) = view(st, user, id).await? {
            out.push(v);
        }
    }
    Ok(out)
}

/// Removes a run with its projects (tasks cascade) and workspaces.
pub async fn delete(st: &AppState, user: Uuid, id: Uuid) -> anyhow::Result<bool> {
    let projects: Vec<Uuid> = sqlx::query_scalar(
        "SELECT r.project_id FROM eval_results r JOIN eval_runs e ON e.id = r.run_id
         WHERE r.run_id=$1 AND e.user_id=$2 AND r.project_id IS NOT NULL",
    )
    .bind(id)
    .bind(user)
    .fetch_all(&st.db)
    .await?;
    let running: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tasks WHERE project_id = ANY($1) AND status IN ('running','waiting_batch'))")
        .bind(&projects)
        .fetch_one(&st.db)
        .await?;
    if running {
        anyhow::bail!("running");
    }
    let n = sqlx::query("DELETE FROM eval_runs WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(user)
        .execute(&st.db)
        .await?
        .rows_affected();
    for p in projects {
        let trees = crate::runner::merge::working_trees(st, p).await?;
        sqlx::query("DELETE FROM projects WHERE id=$1 AND is_eval")
            .bind(p)
            .execute(&st.db)
            .await?;
        crate::runner::merge::remove_working_trees(st, p, &trees).await;
        if let Err(e) = st.runner.delete(p).await {
            tracing::warn!("eval workspace {p} not deleted: {e:#}");
        }
    }
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases_are_complete_and_unique() {
        let c = cases();
        assert_eq!(c.len(), 8);
        let mut ids: Vec<&str> = c.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8);
        for x in &c {
            assert!(["S", "M", "L"].contains(&x.size.as_str()), "{}", x.id);
            assert!(!x.setup.is_empty() && !x.verify.is_empty() && !x.solution.is_empty());
        }
        let cats: std::collections::BTreeSet<&str> =
            c.iter().map(|c| c.category.as_str()).collect();
        assert!(cats.len() >= 5, "{cats:?}");
    }

    #[test]
    fn mock_strength_decides_who_gets_the_solution() {
        let c = cases();
        let s = c.iter().find(|c| c.size == "S").unwrap();
        let l = c.iter().find(|c| c.size == "L").unwrap();
        assert!(mock_context(s, Some("mock-weak")).starts_with("MOCK_SOLUTION_B64:"));
        assert_eq!(mock_context(l, Some("mock-weak")), "");
        assert_eq!(mock_context(l, Some("mock-medium")), "");
        assert!(!mock_context(l, Some("mock-strong")).is_empty());
        let b64 = mock_context(s, None)
            .trim_start_matches("MOCK_SOLUTION_B64:")
            .to_string();
        assert_eq!(STANDARD.decode(b64).unwrap(), s.solution.as_bytes());
    }

    #[test]
    fn summary_counts_only_finished_cases() {
        let r = |status: &str, ok: bool, usd: f64| EvalResultView {
            case_id: "c".into(),
            task_id: None,
            status: status.into(),
            success: ok,
            usage: Usage::default(),
            cost: vec![Money {
                currency: "USD".into(),
                amount: usd,
            }],
            duration_secs: None,
        };
        let s = summarize(&[
            r("done", true, 1.0),
            r("blocked", false, 0.5),
            r("running", false, 0.25),
        ]);
        assert_eq!((s.cases, s.finished, s.succeeded), (3, 2, 1));
        assert_eq!(s.success_rate, Some(0.5));
        assert_eq!(s.cost[0].amount, 1.75);
        assert_eq!(s.cost_per_success[0].amount, 1.75);
        assert!(summarize(&[]).success_rate.is_none());
    }
}
