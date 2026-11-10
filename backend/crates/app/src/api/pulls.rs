//! WP05 endpoints: delivery settings of a project (output mode, git host
//! access) and its pull requests.
//!
//! Access follows the project: members read, editors steer a pull request
//! (check now, auto-fix, one more round), only the owner changes how and
//! where results are delivered (a security boundary like the environment:
//! it decides where the owner's token is sent). Strangers get 404.
use axum::{
    extract::{Path, State},
    Json,
};
use queued_api::pr::{
    self, DeliveryView, GitHost, HostCheck, OutputMode, PullRequestDetail, PullRequestView,
    SetAutoFix, UpdateDelivery,
};
use uuid::Uuid;

use super::projects::{load, require, Role};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    githost::{self, HostError},
    models::Project,
    pr as prs,
    state::AppState,
};

fn delivery_view(st: &AppState, p: &Project) -> DeliveryView {
    let remote = p.git_remote_url.clone().filter(|u| !u.trim().is_empty());
    let eff = githost::effective(p, st.cfg.allow_private_git_hosts);
    let mode = p.output();
    let mut problems = vec![];
    if mode.pushes_branch() {
        if remote.is_none() {
            problems.push("Remote-URL fehlt (Projekt → Einstellungen → Git)".to_string());
        } else if p.git_token_enc.is_none()
            && remote
                .as_deref()
                .is_some_and(|u| u.starts_with("https://") || u.starts_with("http://"))
        {
            problems.push("Zugangs-Token fehlt".to_string());
        }
    }
    if mode == OutputMode::Pr {
        if let Err(e) = &eff {
            if remote.is_some() {
                problems.push(e.clone());
            }
        }
        if p.git_token_enc.is_none() && !problems.iter().any(|x| x.contains("Token")) {
            problems.push("Zugangs-Token fehlt".to_string());
        }
    }
    let eff = eff.ok();
    DeliveryView {
        output_mode: mode,
        git_host: p.git_host.as_deref().and_then(GitHost::parse),
        detected_host: remote.as_deref().and_then(pr::detect_host),
        git_api_url: p.git_api_url.clone(),
        git_repo_path: p.git_repo_path.clone(),
        effective_host: p.host(),
        effective_api_url: eff.as_ref().map(|e| e.1.clone()),
        effective_repo: eff.as_ref().map(|e| e.2.clone()),
        has_remote: remote.is_some(),
        has_token: p.git_token_enc.is_some(),
        pr_draft: p.pr_draft,
        pr_labels: p.pr_labels.clone(),
        pr_auto_fix: p.pr_auto_fix,
        pr_max_followups: p.pr_max_followups,
        problems,
    }
}

pub async fn get_delivery(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DeliveryView>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let p = load(&st, id).await?;
    Ok(Json(delivery_view(&st, &p)))
}

pub async fn put_delivery(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(d): Json<UpdateDelivery>,
) -> ApiResult<Json<DeliveryView>> {
    require(&st, id, u.id, Role::Owner).await?;
    let cur = load(&st, id).await?;
    let host = match d.git_host.as_deref().map(str::trim) {
        None => None,
        Some("") => Some(None),
        Some(h) => Some(Some(
            GitHost::parse(h)
                .ok_or_else(|| AppError::bad_request("Unbekannter Git-Host"))?
                .as_str()
                .to_string(),
        )),
    };
    let api_url = d.git_api_url.as_deref().map(str::trim).map(|u| {
        let u = u.trim_end_matches('/');
        (!u.is_empty()).then(|| u.to_string())
    });
    if let Some(Some(u)) = &api_url {
        pr::validate_api_url(u).map_err(AppError::bad_request)?;
        if u.starts_with("http://") && !st.cfg.allow_private_git_hosts {
            return Err(AppError::bad_request(
                "API-Adresse ohne https:// ist nur mit ALLOW_PRIVATE_GIT_HOSTS erlaubt",
            ));
        }
    }
    let repo = d.git_repo_path.as_deref().map(str::trim).map(|r| {
        let r = r.trim_matches('/');
        (!r.is_empty()).then(|| r.to_string())
    });
    if let Some(Some(r)) = &repo {
        if !pr::valid_repo_path(r) {
            return Err(AppError::bad_request(
                "Repository-Pfad: bitte in der Form besitzer/repository",
            ));
        }
    }
    let labels = d.pr_labels.map(|l| {
        l.into_iter()
            .map(|x| x.trim().to_string())
            .collect::<Vec<_>>()
    });
    if let Some(l) = &labels {
        pr::validate_labels(l).map_err(AppError::bad_request)?;
    }
    if let Some(n) = d.pr_max_followups {
        pr::validate_max_followups(n).map_err(AppError::bad_request)?;
    }
    if let Some(m) = d.output_mode {
        if m.pushes_branch()
            && cur
                .git_remote_url
                .as_deref()
                .is_none_or(|u| u.trim().is_empty())
        {
            return Err(AppError::bad_request(
                "Für „Branch hochladen“ und „Pull Request“ braucht das Projekt zuerst eine Remote-URL.",
            ));
        }
    }
    let p: Project = sqlx::query_as(
        "UPDATE projects SET
            output_mode = COALESCE($2, output_mode),
            git_host = CASE WHEN $3 THEN $4 ELSE git_host END,
            git_api_url = CASE WHEN $5 THEN $6 ELSE git_api_url END,
            git_repo_path = CASE WHEN $7 THEN $8 ELSE git_repo_path END,
            pr_draft = COALESCE($9, pr_draft),
            pr_labels = COALESCE($10, pr_labels),
            pr_auto_fix = COALESCE($11, pr_auto_fix),
            pr_max_followups = COALESCE($12, pr_max_followups)
         WHERE id=$1 RETURNING *",
    )
    .bind(id)
    .bind(d.output_mode.map(|m| m.as_str()))
    .bind(host.is_some())
    .bind(host.flatten())
    .bind(api_url.is_some())
    .bind(api_url.flatten())
    .bind(repo.is_some())
    .bind(repo.flatten())
    .bind(d.pr_draft)
    .bind(labels)
    .bind(d.pr_auto_fix)
    .bind(d.pr_max_followups)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(delivery_view(&st, &p)))
}

/// "Verbindung prüfen": who the token is and whether it may write. Never
/// returns the token; host errors are redacted by the adapter.
pub async fn check_delivery(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HostCheck>> {
    require(&st, id, u.id, Role::Owner).await?;
    let p = load(&st, id).await?;
    let host = match prs::client(&st, &p) {
        Ok(h) => h,
        Err(msg) => {
            return Ok(Json(HostCheck {
                ok: false,
                login: None,
                can_write: false,
                message: msg,
            }))
        }
    };
    let res = async {
        let (login, uid) = host.whoami().await?;
        let can = host.can_write(&login, uid).await?;
        Ok::<_, HostError>((login, can))
    }
    .await;
    Ok(Json(match res {
        Ok((login, true)) => HostCheck {
            message: format!(
                "Verbunden mit {} als {login} – darf hochladen und {}s anlegen.",
                host.kind.label(),
                host.kind.request_name()
            ),
            ok: true,
            login: Some(login),
            can_write: true,
        },
        Ok((login, false)) => HostCheck {
            message: format!(
                "Verbunden als {login}, aber ohne Schreibrecht auf {}. {}",
                host.repo,
                host.kind.token_help()
            ),
            ok: false,
            login: Some(login),
            can_write: false,
        },
        Err(e) => HostCheck {
            ok: false,
            login: None,
            can_write: false,
            message: e.to_string(),
        },
    }))
}

pub async fn list(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<PullRequestView>>> {
    require(&st, id, u.id, Role::Viewer).await?;
    let rows: Vec<prs::PrRow> = sqlx::query_as(
        "SELECT * FROM pull_requests WHERE project_id=$1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(prs::view_of(&st, r).await?);
    }
    Ok(Json(out))
}

/// The pull request, if the caller may see it (`min` role in its project).
async fn visible(st: &AppState, id: Uuid, user: Uuid, min: Role) -> ApiResult<prs::PrRow> {
    let row = prs::load(st, id).await?.ok_or_else(AppError::not_found)?;
    require(st, row.project_id, user, min).await?;
    Ok(row)
}

async fn detail(st: &AppState, row: &prs::PrRow) -> ApiResult<Json<PullRequestDetail>> {
    Ok(Json(PullRequestDetail {
        pr: prs::view_of(st, row).await?,
        feedback: prs::feedback_of(st, row.id).await?,
    }))
}

pub async fn get(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PullRequestDetail>> {
    let row = visible(&st, id, u.id, Role::Viewer).await?;
    detail(&st, &row).await
}

pub async fn poll_now(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PullRequestDetail>> {
    let row = visible(&st, id, u.id, Role::Editor).await?;
    if !prs::poke(&st, id).await? {
        return Err(AppError::conflict(
            "Der Pull Request ist geschlossen – es gibt nichts mehr abzufragen.",
        ));
    }
    let row = prs::load(&st, id).await?.unwrap_or(row);
    detail(&st, &row).await
}

pub async fn auto_fix(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SetAutoFix>,
) -> ApiResult<Json<PullRequestDetail>> {
    visible(&st, id, u.id, Role::Editor).await?;
    prs::set_auto_fix(&st, id, b.enabled).await?;
    let row = prs::load(&st, id).await?.ok_or_else(AppError::not_found)?;
    detail(&st, &row).await
}

pub async fn extra_round(
    State(st): State<AppState>,
    AuthUser(u): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PullRequestDetail>> {
    let row = visible(&st, id, u.id, Role::Editor).await?;
    if !row.state().is_open() {
        return Err(AppError::conflict("Der Pull Request ist geschlossen."));
    }
    prs::extra_round(&st, id).await?;
    let row = prs::load(&st, id).await?.ok_or_else(AppError::not_found)?;
    detail(&st, &row).await
}

/// Pull request of a task (opened for it, or the one it works on).
pub async fn of_task(
    st: &AppState,
    task: &crate::models::Task,
) -> sqlx::Result<Option<PullRequestView>> {
    let row: Option<prs::PrRow> = sqlx::query_as(
        "SELECT * FROM pull_requests WHERE task_id=$1 OR id=$2 ORDER BY created_at LIMIT 1",
    )
    .bind(task.id)
    .bind(task.pr_id)
    .fetch_optional(&st.db)
    .await?;
    match row {
        Some(r) => Ok(Some(prs::view_of(st, &r).await?)),
        None => Ok(None),
    }
}
