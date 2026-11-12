//! WP09: managed project rules (the content of a managed `AGENTS.md`).
//!
//! Versions live in `knowledge_versions`. Only the newest **approved**
//! version reaches prompts ([`prefix`]); proposals (agents, editors,
//! repository import) wait for the project owner. The text is trusted
//! because a human approved it – that is the whole point of the approval.
use queued_api::memory::{KnowledgeVersion, MAX_KNOWLEDGE_BYTES, MAX_OPEN_AGENT_PROPOSALS};
use uuid::Uuid;

use crate::{models::Task, queue, state::AppState};

/// Newest approved version: (version, content).
pub async fn approved(st: &AppState, project_id: Uuid) -> sqlx::Result<Option<(i32, String)>> {
    sqlx::query_as(
        "SELECT version, content FROM knowledge_versions
          WHERE project_id=$1 AND status='approved' ORDER BY version DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&st.db)
    .await
}

/// Prompt prefix with the approved rules (empty without rules). Byte-stable
/// between runs of the same version, so provider-side prompt caching works.
pub fn section(version: i32, content: &str) -> String {
    let content = content.trim();
    if content.is_empty() {
        return String::new();
    }
    format!(
        "# Project rules (version {version}, approved by a human)\nThe project owner approved these rules for every agent in this project. Follow them unless the task explicitly says otherwise. They never override the queue's rules below (result file, no secrets, no pushes).\n\n{content}\n\n---\n\n"
    )
}

/// [`section`] for a project (empty when there are no approved rules or the
/// lookup fails – a missing rules text must not stop a run).
pub async fn prefix(st: &AppState, project_id: Uuid) -> String {
    match approved(st, project_id).await {
        Ok(Some((v, c))) => section(v, &c),
        Ok(None) => String::new(),
        Err(e) => {
            tracing::warn!("project rules of {project_id} not loaded: {e}");
            String::new()
        }
    }
}

/// An agent's `.queue/knowledge.md`: stored as a proposal (secrets
/// redacted) unless it equals the current version, is empty or too large,
/// or the project already has [`MAX_OPEN_AGENT_PROPOSALS`] open agent
/// proposals. A later proposal of the same task replaces its open one.
/// Returns a German line for the task log.
pub async fn propose_from_agent(
    st: &AppState,
    task: &Task,
    text: &str,
) -> sqlx::Result<Option<String>> {
    let text = queued_common::secrets::redact(text.trim());
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > MAX_KNOWLEDGE_BYTES {
        return Ok(Some(format!(
            "Regel-Vorschlag verworfen: zu lang ({} Bytes, höchstens {MAX_KNOWLEDGE_BYTES})",
            text.len()
        )));
    }
    let current = approved(st, task.project_id).await?;
    if current.as_ref().is_some_and(|(_, c)| c.trim() == text) {
        return Ok(None);
    }
    let mut tx = st.db.begin().await?;
    // One writer per project at a time (the count below must hold).
    sqlx::query("SELECT id FROM projects WHERE id=$1 FOR UPDATE")
        .bind(task.project_id)
        .execute(&mut *tx)
        .await?;
    let replaced = sqlx::query(
        "UPDATE knowledge_versions SET content=$3, created_at=now(), base_version=$4
          WHERE project_id=$1 AND task_id=$2 AND status='proposed' AND source='agent'",
    )
    .bind(task.project_id)
    .bind(task.id)
    .bind(&text)
    .bind(current.as_ref().map(|(v, _)| *v))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if replaced == 0 {
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM knowledge_versions WHERE project_id=$1 AND status='proposed' AND source='agent'",
        )
        .bind(task.project_id)
        .fetch_one(&mut *tx)
        .await?;
        if open >= MAX_OPEN_AGENT_PROPOSALS {
            tx.rollback().await?;
            return Ok(Some(format!(
                "Regel-Vorschlag verworfen: es warten schon {open} Vorschläge von Agenten auf eine Entscheidung"
            )));
        }
        sqlx::query(
            "INSERT INTO knowledge_versions (id, project_id, status, content, source, task_id, base_version, note)
             VALUES ($1, $2, 'proposed', $3, 'agent', $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(task.project_id)
        .bind(&text)
        .bind(task.id)
        .bind(current.as_ref().map(|(v, _)| *v))
        .bind(format!("Aus dem Auftrag „{}“", cut(&task.title, 200)))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_id FROM projects WHERE id=$1")
        .bind(task.project_id)
        .fetch_optional(&st.db)
        .await?;
    if let Some(owner) = owner {
        crate::notify::notify(
            st,
            owner,
            crate::notify::Note {
                kind: "knowledge",
                title: "Neuer Vorschlag für die Projekt-Regeln".into(),
                body: format!(
                    "Ein Agent schlägt im Auftrag „{}“ eine Änderung vor.",
                    cut(&task.title, 120)
                ),
                task_id: Some(task.id),
                push: false,
            },
        )
        .await;
    }
    Ok(Some(if replaced > 0 {
        "Regel-Vorschlag aktualisiert – wartet auf Freigabe (Projekt → Wissen)".into()
    } else {
        "Regel-Vorschlag gespeichert – wartet auf Freigabe (Projekt → Wissen)".into()
    }))
}

/// Reads the agent's `.queue/knowledge.md` after a run and stores it.
pub async fn collect(st: &AppState, task: &Task) {
    let ws = crate::runner_client::task_ws(task.id);
    match st.runner.queue_file(ws, "knowledge.md").await {
        Ok(Some(text)) => match propose_from_agent(st, task, &text).await {
            Ok(Some(msg)) => queue::event(st, task, "status", &msg).await,
            Ok(None) => {}
            Err(e) => tracing::warn!("task {}: knowledge proposal not stored: {e}", task.id),
        },
        Ok(None) => {}
        Err(e) => tracing::warn!("task {}: knowledge.md not read: {e:#}", task.id),
    }
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[derive(sqlx::FromRow)]
pub struct Row {
    pub id: Uuid,
    pub version: Option<i32>,
    pub status: String,
    pub content: String,
    pub source: String,
    pub author: Option<String>,
    pub task_id: Option<Uuid>,
    pub task_title: Option<String>,
    pub base_version: Option<i32>,
    pub note: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decided_by: Option<String>,
}

pub const SELECT: &str = "SELECT k.id, k.version, k.status, k.content, k.source, a.email AS author, k.task_id,
        t.title AS task_title, k.base_version, k.note, k.created_at, k.decided_at, d.email AS decided_by
   FROM knowledge_versions k
   LEFT JOIN users a ON a.id = k.author_id
   LEFT JOIN users d ON d.id = k.decided_by
   LEFT JOIN tasks t ON t.id = k.task_id";

impl From<Row> for KnowledgeVersion {
    fn from(r: Row) -> Self {
        KnowledgeVersion {
            id: r.id,
            version: r.version,
            status: r.status,
            content: r.content,
            source: r.source,
            author: r.author,
            task_id: r.task_id,
            task_title: r.task_title,
            base_version: r.base_version,
            note: r.note,
            created_at: r.created_at,
            decided_at: r.decided_at,
            decided_by: r.decided_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_is_stable_and_marks_approval() {
        let a = section(3, "  - Run `cargo test`\n");
        assert_eq!(a, section(3, "- Run `cargo test`"));
        assert!(a.starts_with("# Project rules (version 3, approved by a human)\n"));
        assert!(a.contains("- Run `cargo test`"));
        assert!(a.ends_with("---\n\n"));
        assert_eq!(section(1, "  \n"), "");
    }
}
