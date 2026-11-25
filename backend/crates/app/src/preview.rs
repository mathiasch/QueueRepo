//! WP12 Preview: settings, live previews through the authenticated proxy on
//! an own origin, and screenshots after UI changes.
//!
//! * The dev server runs in the runner's sandbox (no network, no port
//!   binding). The app reaches it only through the runner
//!   (`/v1/previews/{name}/http`).
//! * Live previews have an own origin: `<capability>.<PREVIEW_ORIGIN
//!   suffix>`. Requests with such a `Host` never reach the API or the SPA.
//!   The capability (128 bit, base32) is handed out only to project members
//!   through the authenticated API; it is looked up by its SHA-256.
//! * Every proxied response gets `Content-Security-Policy: sandbox …`
//!   (opaque origin even when opened directly) – credentials of the app
//!   (`Cookie`, `Authorization`) never reach the dev server, its
//!   `Set-Cookie` never reaches the browser.
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use queued_api::preview::{PreviewSettings, PreviewStatus, PreviewView, ScreenshotView, Viewport};
use queued_common::{
    preview_request_header_ok, preview_response_header_ok, PreviewStart, ShotSpec, ShotViewport,
    MAX_PREVIEW_REQUEST, MAX_SHOTS,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{config::PreviewOrigin, models::Task, queue, state::AppState};

/// Statuses in which a task's working tree exists and no agent works in it.
pub const PREVIEWABLE: [&str; 3] = ["candidate", "held", "waiting_input"];
/// Live previews per project at the same time.
const MAX_PER_PROJECT: i64 = 2;

/// Screenshots of a task (metadata; the PNG is served by
/// `GET /api/tasks/{id}/screenshots/{sid}`).
pub async fn screenshots_of(st: &AppState, task: Uuid) -> sqlx::Result<Vec<ScreenshotView>> {
    let rows: Vec<(Uuid, String, String, i32, i32)> = sqlx::query_as(
        "SELECT id, viewport, path, width, height FROM task_screenshots WHERE task_id=$1 ORDER BY created_at, path, viewport",
    )
    .bind(task)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, vp, path, w, h)| {
            Some(ScreenshotView {
                id,
                viewport: Viewport::parse(&vp)?,
                path,
                width: u32::try_from(w).unwrap_or(0),
                height: u32::try_from(h).unwrap_or(0),
                url: format!("/api/tasks/{task}/screenshots/{id}"),
            })
        })
        .collect())
}

type SettingsRow = (
    bool,
    String,
    i32,
    String,
    i32,
    i32,
    bool,
    Vec<String>,
    Vec<String>,
);

/// Preview settings of a project (defaults when none are stored).
pub async fn settings(st: &AppState, project: Uuid) -> sqlx::Result<PreviewSettings> {
    let r: Option<SettingsRow> = sqlx::query_as(
        "SELECT enabled, command, port, ready_path, ready_timeout_secs, max_lifetime_secs, screenshots, screenshot_paths, ui_globs
         FROM preview_settings WHERE project_id=$1",
    )
    .bind(project)
    .fetch_optional(&st.db)
    .await?;
    Ok(match r {
        Some((enabled, command, port, ready_path, rt, lt, screenshots, paths, globs)) => {
            PreviewSettings {
                enabled,
                command,
                port: u16::try_from(port).unwrap_or(3000),
                ready_path,
                ready_timeout_secs: u32::try_from(rt).unwrap_or(60),
                max_lifetime_secs: u32::try_from(lt).unwrap_or(900),
                screenshots,
                screenshot_paths: paths,
                ui_globs: globs,
            }
        }
        None => PreviewSettings::default(),
    })
}

pub async fn save_settings(st: &AppState, project: Uuid, s: &PreviewSettings) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO preview_settings (project_id, enabled, command, port, ready_path, ready_timeout_secs, max_lifetime_secs,
                                       screenshots, screenshot_paths, ui_globs, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,now())
         ON CONFLICT (project_id) DO UPDATE SET enabled=$2, command=$3, port=$4, ready_path=$5, ready_timeout_secs=$6,
                max_lifetime_secs=$7, screenshots=$8, screenshot_paths=$9, ui_globs=$10, updated_at=now()",
    )
    .bind(project)
    .bind(s.enabled)
    .bind(s.command.trim())
    .bind(i32::from(s.port))
    .bind(&s.ready_path)
    .bind(s.ready_timeout_secs as i32)
    .bind(s.max_lifetime_secs as i32)
    .bind(s.screenshots)
    .bind(&s.screenshot_paths)
    .bind(
        s.ui_globs
            .iter()
            .map(|g| g.trim().to_string())
            .collect::<Vec<_>>(),
    )
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Project variables for the dev server (no secrets: it has no network,
/// but its output reaches the UI).
async fn plain_env(st: &AppState, project: Uuid) -> anyhow::Result<Vec<(String, String)>> {
    let env = crate::environment::load(&st.db, project).await?;
    Ok(env
        .vars()
        .into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "PORT" | "HOST" | "HOME" | "PATH"))
        .collect())
}

fn new_cap() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    base32(&b)
}

/// RFC 4648 base32, lowercase, no padding (DNS-label safe).
pub fn base32(data: &[u8]) -> String {
    const A: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn cap_hash(cap: &str) -> Vec<u8> {
    Sha256::digest(cap.as_bytes()).to_vec()
}

type PreviewRow = (
    Uuid,
    Uuid,
    String,
    String,
    Option<String>,
    DateTime<Utc>,
    Vec<u8>,
);

async fn active_of(st: &AppState, task: Uuid) -> sqlx::Result<Option<PreviewRow>> {
    sqlx::query_as(
        "SELECT id, task_id, runner_name, status, error, expires_at, cap_enc FROM previews
         WHERE task_id=$1 AND status IN ('starting','ready') AND expires_at > now()
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(task)
    .fetch_optional(&st.db)
    .await
}

fn view_of(st: &AppState, r: &PreviewRow) -> PreviewView {
    let status = PreviewStatus::parse(&r.3).unwrap_or(PreviewStatus::Failed);
    let url = match (&st.cfg.preview_origin, status) {
        (Some(o), PreviewStatus::Ready) => crate::crypto::decrypt(&st.cfg.master_key, &r.6)
            .ok()
            .and_then(|c| String::from_utf8(c).ok())
            .map(|c| o.url(&c)),
        _ => None,
    };
    PreviewView {
        id: r.0,
        task_id: r.1,
        status,
        url,
        expires_at: r.5,
        error: r.4.clone(),
    }
}

/// The task's running preview (members only – checked by the caller).
pub async fn current(st: &AppState, task: Uuid) -> sqlx::Result<Option<PreviewView>> {
    Ok(active_of(st, task).await?.map(|r| view_of(st, &r)))
}

/// Starts a live preview of a task. `Err` = German message.
pub async fn start(
    st: &AppState,
    task: &Task,
    user: Uuid,
) -> anyhow::Result<Result<PreviewView, String>> {
    if st.cfg.preview_origin.is_none() {
        return Ok(Err(
            "Live-Vorschau ist auf diesem Server nicht eingerichtet (PREVIEW_ORIGIN fehlt). Screenshots funktionieren trotzdem.".into(),
        ));
    }
    let s = settings(st, task.project_id).await?;
    if !s.enabled {
        return Ok(Err(
            "Die Vorschau ist im Projekt nicht eingeschaltet.".into()
        ));
    }
    let cleaned: bool = sqlx::query_scalar("SELECT workspace_cleaned FROM tasks WHERE id=$1")
        .bind(task.id)
        .fetch_one(&st.db)
        .await?;
    if task.branch.is_none() || cleaned || !PREVIEWABLE.contains(&task.status.as_str()) {
        return Ok(Err(
            "Eine Vorschau geht nur, solange der Arbeitsbaum des Auftrags besteht und kein Agent darin arbeitet (z. B. bei einem Versuch, der auf die Auswahl wartet, oder bei einer Rückfrage).".into(),
        ));
    }
    stop_for_task(st, task.id).await;
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM previews WHERE project_id=$1 AND status IN ('starting','ready') AND expires_at > now()",
    )
    .bind(task.project_id)
    .fetch_one(&st.db)
    .await?;
    if n >= MAX_PER_PROJECT {
        return Ok(Err(format!(
            "Höchstens {MAX_PER_PROJECT} Vorschauen pro Projekt gleichzeitig – bitte eine andere beenden."
        )));
    }
    let id = Uuid::new_v4();
    let cap = new_cap();
    let name = format!("pv-{}", &id.simple().to_string()[..20]);
    let expires = Utc::now() + chrono::Duration::seconds(i64::from(s.max_lifetime_secs) + 30);
    let runner_id: Option<Uuid> = sqlx::query_scalar("SELECT runner_id FROM projects WHERE id=$1")
        .bind(task.project_id)
        .fetch_one(&st.db)
        .await?;
    sqlx::query(
        "INSERT INTO previews (id, project_id, task_id, started_by, cap_hash, cap_enc, runner_name, runner_id, status, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'starting',$9)",
    )
    .bind(id)
    .bind(task.project_id)
    .bind(task.id)
    .bind(user)
    .bind(cap_hash(&cap))
    .bind(crate::crypto::encrypt(&st.cfg.master_key, cap.as_bytes())?)
    .bind(&name)
    .bind(runner_id)
    .bind(expires)
    .execute(&st.db)
    .await?;
    let req = PreviewStart {
        name: name.clone(),
        command: s.command.clone(),
        port: s.port,
        ready_path: s.ready_path.clone(),
        ready_timeout_secs: u64::from(s.ready_timeout_secs),
        max_lifetime_secs: u64::from(s.max_lifetime_secs),
        env: plain_env(st, task.project_id).await?,
        shots: vec![],
        keep: true,
    };
    let res = st
        .runner(task.project_id)
        .await
        .preview_start(task.project_id, task.id, &req)
        .await;
    let (status, error) = match &res {
        Ok(r) if r.running => (PreviewStatus::Ready, None),
        Ok(r) => (
            PreviewStatus::Failed,
            Some(format!(
                "{}{}",
                r.error.clone().unwrap_or_else(|| "nicht bereit".into()),
                tail_lines(&r.log_tail)
            )),
        ),
        Err(e) => (PreviewStatus::Failed, Some(format!("Runner: {e}"))),
    };
    st.metrics.preview(if status == PreviewStatus::Ready {
        "started"
    } else {
        "failed"
    });
    let error = error.map(|e| queued_common::secrets::redact(&e));
    sqlx::query("UPDATE previews SET status=$2, error=$3, stopped_at = CASE WHEN $2='failed' THEN now() END WHERE id=$1")
        .bind(id)
        .bind(status.as_str())
        .bind(&error)
        .execute(&st.db)
        .await?;
    queue::event(
        st,
        task,
        "status",
        &match &error {
            None => "Vorschau gestartet".to_string(),
            Some(e) => format!(
                "Vorschau nicht gestartet: {}",
                e.lines().next().unwrap_or_default()
            ),
        },
    )
    .await;
    let r: PreviewRow = sqlx::query_as(
        "SELECT id, task_id, runner_name, status, error, expires_at, cap_enc FROM previews WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&st.db)
    .await?;
    Ok(Ok(view_of(st, &r)))
}

fn tail_lines(log: &str) -> String {
    let lines: Vec<&str> = log.lines().rev().take(8).collect();
    if lines.is_empty() {
        return String::new();
    }
    let mut v: Vec<&str> = lines.into_iter().rev().collect();
    v.retain(|l| !l.trim().is_empty());
    format!("\nLetzte Ausgabe:\n{}", v.join("\n"))
}

/// Stops the live previews of a task (its working tree goes away, an agent
/// starts working in it, or the user stops it).
pub async fn stop_for_task(st: &AppState, task: Uuid) {
    let rows: Vec<(Uuid, Uuid, String)> = match sqlx::query_as(
        "UPDATE previews SET status='stopped', stopped_at=now() WHERE task_id=$1 AND status IN ('starting','ready')
         RETURNING id, project_id, runner_name",
    )
    .bind(task)
    .fetch_all(&st.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("task {task}: previews not stopped: {e:#}");
            return;
        }
    };
    for (_, project, name) in rows {
        if let Err(e) = st.runner(project).await.preview_stop(&name).await {
            tracing::info!(
                "preview {name}: stop at the runner failed (its lifetime ends it): {e:#}"
            );
        }
    }
}

/// Scheduler: expired previews are marked stopped (the runner's reaper
/// ends the process at the same deadline).
pub async fn sweep(st: &AppState) -> sqlx::Result<()> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "UPDATE previews SET status='stopped', stopped_at=now()
         WHERE status IN ('starting','ready') AND expires_at <= now() RETURNING project_id, runner_name",
    )
    .fetch_all(&st.db)
    .await?;
    for (project, name) in rows {
        let _ = st.runner(project).await.preview_stop(&name).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Screenshots after a run
// ---------------------------------------------------------------------------

/// Paths of a unified diff (new side; deletions count with their old path).
pub fn changed_paths(diff: &str) -> Vec<String> {
    let mut out = vec![];
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("diff --git a/") {
            if let Some((a, b)) = rest.split_once(" b/") {
                let p = if b.is_empty() { a } else { b };
                if !out.iter().any(|x: &String| x == p) {
                    out.push(p.to_string());
                }
            }
        }
    }
    out
}

/// Width and height from a PNG's IHDR chunk.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") || &png[12..16] != b"IHDR" {
        return None;
    }
    Some((
        u32::from_be_bytes(png[16..20].try_into().ok()?),
        u32::from_be_bytes(png[20..24].try_into().ok()?),
    ))
}

/// After a verified run: screenshots of the dev server (desktop and mobile
/// per configured page) when the run changed UI files. Never fails the
/// task; problems become an event.
pub async fn screenshots_after_run(st: &AppState, task: &Task, before: &str, after: &str) {
    if let Err(e) = screenshots_inner(st, task, before, after).await {
        queue::event(
            st,
            task,
            "status",
            &format!("Screenshots nicht möglich: {e:#}"),
        )
        .await;
    }
}

async fn screenshots_inner(
    st: &AppState,
    task: &Task,
    before: &str,
    after: &str,
) -> anyhow::Result<()> {
    let s = settings(st, task.project_id).await?;
    if !s.enabled || !s.screenshots || s.screenshot_paths.is_empty() || before == after {
        return Ok(());
    }
    let diff = st
        .runner(task.project_id)
        .await
        .diff(task.project_id, before, after, 4_000_000)
        .await?;
    let ui: Vec<String> = changed_paths(&diff.diff)
        .into_iter()
        .filter(|p| queued_common::glob::matches_any(&s.ui_globs, p))
        .collect();
    if ui.is_empty() {
        return Ok(());
    }
    let mut shots = vec![];
    for p in s.screenshot_paths.iter().take(MAX_SHOTS / 2) {
        for vp in [ShotViewport::Desktop, ShotViewport::Mobile] {
            shots.push(ShotSpec {
                path: p.clone(),
                viewport: vp,
            });
        }
    }
    queue::event(
        st,
        task,
        "status",
        &format!(
            "UI-Dateien geändert ({}) – Screenshots werden aufgenommen",
            ui.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        ),
    )
    .await;
    let name = format!("q-{}-shot", &task.id.simple().to_string()[..12]);
    let out = st
        .runner(task.project_id)
        .await
        .preview_start(
            task.project_id,
            task.id,
            &PreviewStart {
                name,
                command: s.command.clone(),
                port: s.port,
                ready_path: s.ready_path.clone(),
                ready_timeout_secs: u64::from(s.ready_timeout_secs),
                max_lifetime_secs: u64::from(s.max_lifetime_secs),
                env: plain_env(st, task.project_id).await?,
                shots,
                keep: false,
            },
        )
        .await?;
    if !out.ready {
        anyhow::bail!(
            "{}{}",
            out.error
                .unwrap_or_else(|| "Dev-Server nicht bereit".into()),
            queued_common::secrets::redact(&tail_lines(&out.log_tail))
        );
    }
    let mut tx = st.db.begin().await?;
    // Each run replaces the screenshots of the previous one.
    sqlx::query("DELETE FROM task_screenshots WHERE task_id=$1")
        .bind(task.id)
        .execute(&mut *tx)
        .await?;
    let (mut ok, mut failed) = (0, vec![]);
    for shot in out.shots {
        if let Some(e) = shot.error {
            failed.push(format!("{} ({}): {e}", shot.path, vp_label(shot.viewport)));
            continue;
        }
        let Ok(png) = base64::engine::general_purpose::STANDARD.decode(&shot.png_b64) else {
            continue;
        };
        let Some((w, h)) = png_size(&png) else {
            failed.push(format!("{}: kein gültiges PNG", shot.path));
            continue;
        };
        sqlx::query(
            "INSERT INTO task_screenshots (id, task_id, project_id, viewport, path, png, width, height) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(Uuid::new_v4())
        .bind(task.id)
        .bind(task.project_id)
        .bind(match shot.viewport {
            ShotViewport::Desktop => "desktop",
            ShotViewport::Mobile => "mobile",
        })
        .bind(&shot.path)
        .bind(&png)
        .bind(w as i32)
        .bind(h as i32)
        .execute(&mut *tx)
        .await?;
        ok += 1;
    }
    tx.commit().await?;
    for _ in 0..ok {
        st.metrics.preview("screenshot");
    }
    queue::event(
        st,
        task,
        "status",
        &if failed.is_empty() {
            format!("{ok} Screenshots aufgenommen")
        } else {
            format!(
                "{ok} Screenshots aufgenommen, {} fehlgeschlagen: {}",
                failed.len(),
                failed.join("; ")
            )
        },
    )
    .await;
    queue::emit_task(st, task.id).await;
    Ok(())
}

fn vp_label(v: ShotViewport) -> &'static str {
    match v {
        ShotViewport::Desktop => "Desktop",
        ShotViewport::Mobile => "Mobil",
    }
}

// ---------------------------------------------------------------------------
// Proxy on the preview origin
// ---------------------------------------------------------------------------

/// Security policy of every preview response: an opaque origin (scripts
/// and forms run, but nothing same-origin with anything), framed only by
/// the app.
pub fn csp(o: &PreviewOrigin) -> String {
    format!(
        "sandbox allow-scripts allow-forms allow-popups allow-modals; frame-ancestors {}",
        o.app_origin
    )
}

fn secure(o: &PreviewOrigin, mut r: Response) -> Response {
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&csp(o)) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    h.remove(header::SET_COOKIE);
    r
}

fn page(o: &PreviewOrigin, status: StatusCode, text: &str) -> Response {
    secure(
        o,
        (
            status,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            text.to_string(),
        )
            .into_response(),
    )
}

/// Middleware in front of the whole app: a request for a preview host is
/// answered by the preview proxy, never by the API or the SPA.
pub async fn layer(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let Some(o) = st.cfg.preview_origin.clone() else {
        return next.run(req).await;
    };
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if host.is_empty() {
        return next.run(req).await;
    }
    let Some(cap) = o.cap_of(&host) else {
        // Any other host of the preview domain (wrong length, a typo) is
        // never the app either.
        let bare = host
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if bare.ends_with(&format!(".{}", o.suffix)) {
            return page(&o, StatusCode::NOT_FOUND, "Unbekannte Vorschau.");
        }
        return next.run(req).await;
    };
    serve(&st, &o, &cap, req).await
}

async fn serve(st: &AppState, o: &PreviewOrigin, cap: &str, req: Request) -> Response {
    let row: Option<(Uuid, String)> = match sqlx::query_as(
        "SELECT p.project_id, p.runner_name FROM previews p JOIN projects pr ON pr.id = p.project_id
         WHERE p.cap_hash=$1 AND p.status='ready' AND p.expires_at > now()",
    )
    .bind(cap_hash(cap))
    .fetch_optional(&st.db)
    .await
    {
        Ok(r) => r,
        Err(_) => return page(o, StatusCode::SERVICE_UNAVAILABLE, "Vorschau gerade nicht erreichbar."),
    };
    let Some((project, name)) = row else {
        return page(
            o,
            StatusCode::NOT_FOUND,
            "Diese Vorschau ist beendet oder unbekannt.",
        );
    };
    let (parts, body) = req.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let headers: Vec<(String, Vec<u8>)> = parts
        .headers
        .iter()
        .filter(|(k, _)| preview_request_header_ok(k.as_str()))
        .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
        .collect();
    let body = match axum::body::to_bytes(body, MAX_PREVIEW_REQUEST).await {
        Ok(b) => b,
        Err(_) => return page(o, StatusCode::PAYLOAD_TOO_LARGE, "Anfrage zu groß."),
    };
    let res = st
        .runner(project)
        .await
        .preview_http(&name, parts.method.as_str(), &path, headers, body)
        .await;
    let (status, hs, body) = match res {
        Ok(x) => x,
        Err(_) => {
            return page(
                o,
                StatusCode::BAD_GATEWAY,
                "Der Dev-Server antwortet nicht.",
            )
        }
    };
    let mut b =
        Response::builder().status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY));
    for (k, v) in hs {
        if !preview_response_header_ok(&k)
            || k.eq_ignore_ascii_case(queued_common::PREVIEW_STATUS_HEADER)
        {
            continue;
        }
        if k.eq_ignore_ascii_case("location") {
            // Absolute redirects to the dev server's own address stay on the
            // preview origin.
            let loc = String::from_utf8_lossy(&v).to_string();
            b = b.header(k, rewrite_location(&loc));
            continue;
        }
        b = b.header(k, v);
    }
    match b.body(Body::from(body)) {
        Ok(r) => secure(o, r),
        Err(_) => page(
            o,
            StatusCode::BAD_GATEWAY,
            "Ungültige Antwort des Dev-Servers.",
        ),
    }
}

/// `http://127.0.0.1:<port>/x` / `http://localhost:<port>/x` → `/x`.
pub fn rewrite_location(loc: &str) -> String {
    for p in ["http://127.0.0.1", "http://localhost", "http://0.0.0.0"] {
        if let Some(rest) = loc.strip_prefix(p) {
            let rest = rest.trim_start_matches(|c: char| c == ':' || c.is_ascii_digit());
            return if rest.starts_with('/') {
                rest.to_string()
            } else {
                format!("/{rest}")
            };
        }
    }
    loc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_and_caps() {
        assert_eq!(base32(b"f"), "my");
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
        let c = new_cap();
        assert_eq!(c.len(), crate::config::PREVIEW_CAP_LEN);
        assert_ne!(c, new_cap());
        assert_eq!(cap_hash(&c).len(), 32);
    }

    #[test]
    fn origin_parsing_and_host_matching() {
        let o = PreviewOrigin::parse("https://*.preview.example.com", "https://queue.example.com")
            .unwrap();
        let cap = "abcdefghijklmnopqrstuvwxyz";
        assert_eq!(
            o.cap_of(&format!("{cap}.preview.example.com")),
            Some(cap.into())
        );
        assert_eq!(
            o.cap_of(&format!("{}.PREVIEW.example.com", cap.to_uppercase())),
            Some(cap.into())
        );
        assert_eq!(o.cap_of("queue.example.com"), None);
        assert_eq!(
            o.cap_of(&format!("{cap}.preview.example.com:8443")),
            None,
            "port must match"
        );
        assert_eq!(o.cap_of(&format!("x{cap}.preview.example.com")), None);
        assert_eq!(
            o.cap_of("abcdefghijklmnopqrstuvwxy1.preview.example.com"),
            None,
            "base32 only"
        );
        assert_eq!(o.url(cap), format!("https://{cap}.preview.example.com/"));
        assert_eq!(o.app_origin, "https://queue.example.com");
        let l = PreviewOrigin::parse("http://*.localhost:18080", "http://localhost:18080").unwrap();
        assert_eq!(
            l.cap_of(&format!("{cap}.localhost:18080")),
            Some(cap.into())
        );
        assert_eq!(
            l.cap_of("localhost:18080"),
            None,
            "the app's host is no preview"
        );
        assert!(PreviewOrigin::parse("https://preview.example.com", "https://a.b").is_err());
        assert!(PreviewOrigin::parse("ftp://*.x.y", "https://a.b").is_err());
        assert!(PreviewOrigin::parse("https://*.x.y/path", "https://a.b").is_err());
        assert!(
            PreviewOrigin::parse(
                "https://*.example.com",
                &format!("https://{cap}.example.com")
            )
            .is_err(),
            "the app must not live on a preview host"
        );
    }

    #[test]
    fn csp_is_a_sandbox_framed_only_by_the_app() {
        let o = PreviewOrigin::parse("https://*.p.example.com", "https://q.example.com").unwrap();
        let c = csp(&o);
        assert!(c.starts_with("sandbox "));
        assert!(!c.contains("allow-same-origin") && !c.contains("allow-top-navigation"));
        assert!(c.contains("frame-ancestors https://q.example.com"));
    }

    #[test]
    fn paths_png_and_redirects() {
        let d = "diff --git a/web/app.css b/web/app.css\n+x\ndiff --git a/src/x.rs b/src/x.rs\n";
        assert_eq!(changed_paths(d), vec!["web/app.css", "src/x.rs"]);
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(1280u32.to_be_bytes());
        png.extend(800u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((1280, 800)));
        assert_eq!(png_size(b"GIF89a"), None);
        assert_eq!(
            rewrite_location("http://127.0.0.1:5173/login?x=1"),
            "/login?x=1"
        );
        assert_eq!(rewrite_location("http://localhost:3000"), "/");
        assert_eq!(rewrite_location("/rel"), "/rel");
        assert_eq!(
            rewrite_location("https://example.com/x"),
            "https://example.com/x"
        );
    }
}
