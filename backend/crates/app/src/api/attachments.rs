//! WP09 attachments API.
//!
//! * `POST /api/projects/{id}/attachments?name=…[&task=…]` – raw body
//!   (the `Content-Type` of the request is ignored), editor or owner;
//!   with `task` the file is added to a task that is not running (creator
//!   or owner), otherwise it is a draft of the caller for the composer.
//! * `GET /api/attachments/{id}` – download with protective headers,
//!   project members (drafts: only the uploader).
//! * `DELETE /api/attachments/{id}` – uploader or project owner.
//! * `GET /api/tasks/{id}/attachments`, `GET /api/attachments/quota`.
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use queued_api::memory::{
    AttachmentQuota, AttachmentView, MAX_ATTACHMENTS_PER_TASK, MAX_ATTACHMENT_BYTES_PER_TASK,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::projects::{require, role, Role};
use crate::{
    attachments::{self as att, Row},
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::Task,
    queue,
    state::AppState,
};

#[derive(Deserialize)]
pub struct UploadQ {
    pub name: String,
    #[serde(default)]
    pub task: Option<Uuid>,
}

/// Statuses in which a task's attachments may change (not while a run,
/// review or merge uses them).
const EDITABLE: [&str; 6] = [
    "queued",
    "pending_approval",
    "waiting_input",
    "held",
    "failed",
    "blocked",
];

pub async fn upload(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(project_id): Path<Uuid>,
    Query(q): Query<UploadQ>,
    body: Bytes,
) -> ApiResult<Json<AttachmentView>> {
    require(&st, project_id, u.id, Role::Editor).await?;
    if let Some(tid) = q.task {
        let t = task_in(&st, tid, project_id).await?;
        let owner = role(&st, project_id, u.id).await? == Some(Role::Owner);
        if t.user_id != u.id && !owner {
            return Err(AppError::forbidden());
        }
        if !EDITABLE.contains(&t.status.as_str()) {
            return Err(AppError::conflict(
                "Anhänge lassen sich nur ändern, solange der Auftrag nicht läuft.",
            ));
        }
    }
    let display = att::display_name(&q.name);
    let raw = body.to_vec();
    let name_for_sniff = display.clone();
    let processed = tokio::task::spawn_blocking(move || att::process(&raw, &name_for_sniff))
        .await
        .map_err(|e| AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")))?
        .map_err(|m| AppError(StatusCode::UNPROCESSABLE_ENTITY, m))?;
    let id = Uuid::new_v4();
    let name = att::workspace_name(&display, id, processed.ext);
    let size = processed.data.len() as i64;
    let mut tx = st.db.begin().await?;
    // Quota and per-task limits under a lock of the uploader (one upload
    // of the same user at a time; concurrent uploads cannot both pass).
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(u.id)
        .execute(&mut *tx)
        .await?;
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(size), 0)::BIGINT FROM attachments WHERE user_id=$1",
    )
    .bind(u.id)
    .fetch_one(&mut *tx)
    .await?;
    let limit = st.cfg.attachment_quota_bytes;
    if used + size > limit {
        return Err(AppError(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Speicherplatz für Anhänge erschöpft ({} von {} belegt). Bitte alte Anhänge oder Aufträge löschen.",
                queued_api::memory::file_size(used),
                queued_api::memory::file_size(limit)
            ),
        ));
    }
    // Drafts of the caller in this project, or the task's attachments.
    let (count, bytes): (i64, i64) =
        match q.task {
            Some(t) => sqlx::query_as(
                "SELECT count(*), COALESCE(sum(size), 0)::BIGINT FROM attachments WHERE task_id=$1",
            )
            .bind(t)
            .fetch_one(&mut *tx)
            .await?,
            None => {
                sqlx::query_as(
                    "SELECT count(*), COALESCE(sum(size), 0)::BIGINT FROM attachments
              WHERE task_id IS NULL AND user_id=$1 AND project_id=$2",
                )
                .bind(u.id)
                .bind(project_id)
                .fetch_one(&mut *tx)
                .await?
            }
        };
    if count as usize >= MAX_ATTACHMENTS_PER_TASK {
        return Err(AppError::bad_request(format!(
            "Höchstens {MAX_ATTACHMENTS_PER_TASK} Anhänge pro Auftrag."
        )));
    }
    if (bytes + size) as usize > MAX_ATTACHMENT_BYTES_PER_TASK {
        return Err(AppError(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Alle Anhänge eines Auftrags zusammen höchstens {}.",
                queued_api::memory::file_size(MAX_ATTACHMENT_BYTES_PER_TASK as i64)
            ),
        ));
    }
    let row: Row = sqlx::query_as(&format!(
        "INSERT INTO attachments (id, project_id, task_id, user_id, name, display_name, mime, size, sha256, width, height, data)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING {}",
        att::COLUMNS
    ))
    .bind(id)
    .bind(project_id)
    .bind(q.task)
    .bind(u.id)
    .bind(&name)
    .bind(&display)
    .bind(processed.mime)
    .bind(size as i32)
    .bind(att::sha256_hex(&processed.data))
    .bind(processed.width)
    .bind(processed.height)
    .bind(&processed.data)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    if let Some(tid) = q.task {
        if let Some(t) = queue::load_task(&st, tid).await? {
            queue::event(&st, &t, "status", &format!("Anhang hinzugefügt: {display}")).await;
            queue::emit_task(&st, tid).await;
        }
    }
    Ok(Json(row.view(true)))
}

async fn task_in(st: &AppState, task: Uuid, project: Uuid) -> ApiResult<Task> {
    let t = queue::load_task(st, task)
        .await?
        .ok_or_else(AppError::not_found)?;
    if t.project_id != project {
        return Err(AppError::not_found());
    }
    Ok(t)
}

async fn load(st: &AppState, id: Uuid) -> ApiResult<Row> {
    sqlx::query_as::<_, Row>(&format!(
        "SELECT {} FROM attachments WHERE id=$1",
        att::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(AppError::not_found)
}

/// Read access: drafts only for the uploader; attached files for every
/// member of the project. Unknown and foreign files are both 404.
async fn readable(st: &AppState, r: &Row, user: Uuid) -> ApiResult<Option<Role>> {
    let role = role(st, r.project_id, user).await?;
    match (r.task_id, role) {
        (_, None) => Err(AppError::not_found()),
        (None, Some(_)) if r.user_id != user => Err(AppError::not_found()),
        (_, role) => Ok(role),
    }
}

pub async fn download(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let r = load(&st, id).await?;
    readable(&st, &r, u.id).await?;
    let data: Vec<u8> = sqlx::query_scalar("SELECT data FROM attachments WHERE id=$1")
        .bind(id)
        .fetch_one(&st.db)
        .await?;
    Ok(file_response(&r, data))
}

/// The protective headers: always a download, never sniffed, never
/// executed (no script, no plugin, sandboxed), not cached by shared caches.
pub fn file_response(r: &Row, data: Vec<u8>) -> Response {
    let ctype = if r.mime.starts_with("image/") {
        r.mime.as_str()
    } else {
        "application/octet-stream"
    };
    let ascii: String = r
        .display_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let disposition = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii.trim(),
        percent(&r.display_name)
    );
    let mut resp = Response::new(Body::from(data));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(static_type(ctype)),
    );
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    } else {
        h.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    resp
}

fn static_type(t: &str) -> &'static str {
    match t {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

/// RFC 5987 percent-encoding of a file name.
fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub async fn remove(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let r = load(&st, id).await?;
    let role = readable(&st, &r, u.id).await?;
    if r.user_id != u.id && role != Some(Role::Owner) {
        return Err(AppError::forbidden());
    }
    if let Some(tid) = r.task_id {
        let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id=$1")
            .bind(tid)
            .fetch_one(&st.db)
            .await?;
        if matches!(status.as_str(), "running" | "merging") {
            return Err(AppError::conflict(
                "Der Auftrag läuft gerade – den Anhang bitte danach löschen.",
            ));
        }
    }
    sqlx::query("DELETE FROM attachments WHERE id=$1")
        .bind(id)
        .execute(&st.db)
        .await?;
    if let Some(tid) = r.task_id {
        if let Some(t) = queue::load_task(&st, tid).await? {
            queue::event(
                &st,
                &t,
                "status",
                &format!("Anhang gelöscht: {}", r.display_name),
            )
            .await;
            queue::emit_task(&st, tid).await;
        }
    }
    Ok(Json(json!({"ok": true})))
}

/// The caller's drafts in a project (the composer shows them again after
/// a reload, so they are attached to the next task or removed).
pub async fn drafts(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(project_id): Path<Uuid>,
) -> ApiResult<Json<Vec<AttachmentView>>> {
    require(&st, project_id, u.id, Role::Editor).await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {} FROM attachments WHERE task_id IS NULL AND user_id=$1 AND project_id=$2 ORDER BY created_at, id",
        att::COLUMNS
    ))
    .bind(u.id)
    .bind(project_id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(rows.iter().map(|r| r.view(true)).collect()))
}

pub async fn quota(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
) -> ApiResult<Json<AttachmentQuota>> {
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(size), 0)::BIGINT FROM attachments WHERE user_id=$1",
    )
    .bind(u.id)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(AttachmentQuota {
        used,
        limit: st.cfg.attachment_quota_bytes,
    }))
}

/// Attachments of a task as the caller sees them.
pub async fn views_for_task(st: &AppState, t: &Task, user: Uuid) -> ApiResult<Vec<AttachmentView>> {
    let owner = role(st, t.project_id, user).await? == Some(Role::Owner);
    Ok(att::for_task(st, t.id)
        .await?
        .iter()
        .map(|r| r.view(r.task_id == Some(t.id) && (r.user_id == user || owner)))
        .collect())
}

pub async fn of_task(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<AttachmentView>>> {
    let t = queue::load_task(&st, id)
        .await?
        .ok_or_else(AppError::not_found)?;
    require(&st, t.project_id, u.id, Role::Viewer).await?;
    Ok(Json(views_for_task(&st, &t, u.id).await?))
}

/// Drafts of the caller → the new task (in the task's transaction).
pub async fn attach_drafts(
    conn: &mut sqlx::PgConnection,
    ids: &[Uuid],
    task: Uuid,
    project: Uuid,
    user: Uuid,
) -> ApiResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut uniq = ids.to_vec();
    uniq.sort();
    uniq.dedup();
    if uniq.len() > MAX_ATTACHMENTS_PER_TASK {
        return Err(AppError::bad_request(format!(
            "Höchstens {MAX_ATTACHMENTS_PER_TASK} Anhänge pro Auftrag."
        )));
    }
    let (n, bytes): (i64, i64) = sqlx::query_as(
        "SELECT count(*), COALESCE(sum(size), 0)::BIGINT FROM attachments
          WHERE id = ANY($1) AND user_id=$2 AND project_id=$3 AND task_id IS NULL",
    )
    .bind(&uniq)
    .bind(user)
    .bind(project)
    .fetch_one(&mut *conn)
    .await?;
    if n as usize != uniq.len() {
        return Err(AppError::bad_request(
            "Anhang nicht gefunden (abgelaufen oder aus einem anderen Projekt). Bitte erneut hochladen.",
        ));
    }
    if bytes as usize > MAX_ATTACHMENT_BYTES_PER_TASK {
        return Err(AppError::bad_request("Die Anhänge sind zusammen zu groß."));
    }
    sqlx::query("UPDATE attachments SET task_id=$1 WHERE id = ANY($2) AND task_id IS NULL")
        .bind(task)
        .bind(&uniq)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
