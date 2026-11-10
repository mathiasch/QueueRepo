//! queued-runner: the only component that touches files and processes.
//! Runs as an unprivileged user; agents run in rootless containers.
pub mod egress;
pub mod exec;
pub mod git;
pub mod setup;
pub mod tasks;

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use exec::Engine;
use futures::StreamExt;
use queued_common::{gate, *};
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::git::Repo;

#[derive(Clone, Debug)]
pub struct RunnerConfig {
    pub data_dir: PathBuf,
    pub token: String,
    pub engine: Engine,
    pub image: String,
    pub memory: String,
    pub cpus: String,
    /// `--user` for the docker engine (defaults to the runner's uid:gid).
    pub docker_user: Option<String>,
    /// Extra bind-mount options, e.g. `Z` for SELinux hosts.
    pub volume_opts: String,
    /// Seconds a process gets after SIGINT (cancel/timeout/stall) to finish
    /// its turn and flush output before it is SIGKILLed.
    pub kill_grace_secs: u64,
    /// Operator settings of the egress proxy (`RUNNER_EGRESS_*`).
    pub egress: Arc<egress::EgressConfig>,
}

/// Longest data dir path: `<data_dir>/e/<16 hex>/p.sock` must stay below the
/// 108-byte limit of Unix socket paths.
pub const MAX_DATA_DIR_LEN: usize = 80;

impl RunnerConfig {
    pub fn workspaces(&self) -> PathBuf {
        self.data_dir.join("workspaces")
    }

    pub fn for_tests(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            token: "test-token".into(),
            engine: Engine::None,
            image: "queued-agent:latest".into(),
            memory: "1g".into(),
            cpus: "1".into(),
            docker_user: None,
            volume_opts: String::new(),
            kill_grace_secs: 2,
            egress: Arc::new(egress::EgressConfig {
                // Tests use local fake upstream servers.
                private_hosts: vec![queued_common::net::HostRule::parse("127.0.0.1").expect("rule")],
                ..Default::default()
            }),
        }
    }

    pub fn info(&self) -> RunnerInfo {
        RunnerInfo {
            engine: match self.engine {
                Engine::Podman => "podman",
                Engine::Docker => "docker",
                Engine::None => "none",
            }
            .into(),
            egress_enforced: self.engine != Engine::None,
        }
    }
}

#[derive(Clone)]
pub struct RunnerState {
    pub cfg: Arc<RunnerConfig>,
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    running: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Workspace of each active run (run name → workspace id).
    active: Arc<Mutex<HashMap<String, String>>>,
}

impl RunnerState {
    pub fn new(cfg: RunnerConfig) -> Self {
        Self {
            cfg: Arc::new(cfg),
            locks: Default::default(),
            running: Default::default(),
            active: Default::default(),
        }
    }

    async fn lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .await
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    /// A process runs in this workspace right now.
    pub async fn workspace_busy(&self, id: &str) -> bool {
        self.active.lock().await.values().any(|w| w == id)
    }

    /// Key of the secret fingerprints (stable as long as the token is).
    pub fn scan_key(&self) -> &[u8] {
        self.cfg.token.as_bytes()
    }

    fn repo(&self, id: &str) -> Result<Repo, ApiErr> {
        if !valid_name(id) {
            return Err(ApiErr(
                StatusCode::BAD_REQUEST,
                "invalid workspace id".into(),
            ));
        }
        Ok(Repo::new(&self.cfg.workspaces(), id))
    }
}

pub struct ApiErr(StatusCode, String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

impl From<anyhow::Error> for ApiErr {
    fn from(e: anyhow::Error) -> Self {
        ApiErr(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}"))
    }
}

type R<T> = Result<T, ApiErr>;

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(st): State<RunnerState>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| constant_time_eq(t.as_bytes(), st.cfg.token.as_bytes()))
        .unwrap_or(false);
    if !ok {
        return ApiErr(StatusCode::UNAUTHORIZED, "unauthorized".into()).into_response();
    }
    next.run(req).await
}

pub fn router(st: RunnerState) -> Router {
    let api = Router::new()
        .route("/workspaces/{id}", post(init).delete(delete_ws))
        .route(
            "/workspaces/{id}/cache",
            axum::routing::delete(delete_cache),
        )
        .route("/workspaces/{id}/begin", post(begin))
        .route("/workspaces/{id}/commit", post(commit))
        .route("/workspaces/{id}/diff", post(diff))
        .route("/workspaces/{id}/revert", post(revert))
        .route("/workspaces/{id}/files", get(files))
        .route("/workspaces/{id}/file", get(file))
        .route("/workspaces/{id}/zip", get(zip))
        .route(
            "/workspaces/{id}/import",
            post(import).layer(DefaultBodyLimit::max(512 * 1024 * 1024)),
        )
        .route("/workspaces/{id}/push", post(push))
        .route("/workspaces/{id}/inspect", post(inspect))
        .route("/workspaces/{id}/hold", post(hold))
        .route("/workspaces/{id}/release", post(release))
        .route(
            "/workspaces/{id}/held/{task}",
            get(held_diff).delete(discard_held),
        )
        .route("/workspaces/{id}/baseline/check", post(baseline_check))
        .route("/workspaces/{id}/baseline/approve", post(baseline_approve))
        .route(
            "/workspaces/{id}/tasks/{task}",
            post(task_prepare).delete(task_remove),
        )
        .route("/workspaces/{id}/tasks/{task}/sync", post(task_sync))
        .route("/workspaces/{id}/tasks/{task}/merge", post(task_merge))
        .route(
            "/workspaces/{id}/tasks/{task}/merge/finish",
            post(task_merge_finish),
        )
        .route("/workspaces/{id}/log", get(log))
        .route("/workspaces/{id}/queue/{name}", get(queue_file))
        .route("/exec", post(exec_start))
        .route("/exec/{name}/cancel", post(exec_cancel))
        .route("/cli-status", get(cli_status))
        .route("/info", get(info))
        .layer(middleware::from_fn_with_state(st.clone(), auth));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest(API_PREFIX, api)
        .with_state(st)
}

async fn init(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<InitWorkspace>,
) -> R<Json<Sha>> {
    let repo = st.repo(&id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    Ok(Json(Sha {
        sha: repo.init(&req).await?,
    }))
}

async fn delete_ws(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
) -> R<Json<serde_json::Value>> {
    let repo = st.repo(&id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    repo.delete().await?;
    remove_caches(&st, &id).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn remove_caches(st: &RunnerState, id: &str) -> R<()> {
    let dir = setup::caches_root(&st.cfg, id);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::Error::from(e).into()),
    }
}

/// Drops the dependency caches of a project (next setup starts cold).
async fn delete_cache(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
) -> R<Json<serde_json::Value>> {
    st.repo(&id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    remove_caches(&st, &id).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

/// Engine and whether egress rules are enforced.
async fn info(State(st): State<RunnerState>) -> Json<RunnerInfo> {
    Json(st.cfg.info())
}

fn existing(st: &RunnerState, id: &str) -> R<Repo> {
    let r = st.repo(id)?;
    if !r.exists() {
        return Err(ApiErr(StatusCode::NOT_FOUND, "workspace not found".into()));
    }
    Ok(r)
}

async fn begin(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<BeginTask>,
) -> R<Json<BeginResult>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    Ok(Json(
        repo.begin(req.remote.as_ref(), st.scan_key(), &req.allow)
            .await?,
    ))
}

/// Commits unless the changes contain a secret outside `allow` (WP03).
async fn commit(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<CommitRequest>,
) -> R<Json<CommitOutcome>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    Ok(Json(
        repo.commit_scanned(
            &req.message,
            &req.author_name,
            &req.author_email,
            st.scan_key(),
            &req.allow,
            req.allow_large,
        )
        .await?,
    ))
}

async fn inspect(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<gate::InspectRequest>,
) -> R<Json<gate::InspectResult>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    Ok(Json(repo.inspect(&req, st.scan_key()).await?))
}

async fn hold(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<gate::HoldRequest>,
) -> R<Json<serde_json::Value>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let r = repo.hold(&req.task, &req.base).await?;
    Ok(Json(serde_json::json!({ "ref": r })))
}

async fn release(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<gate::ReleaseRequest>,
) -> R<Json<CommitOutcome>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    Ok(Json(repo.release(&req, st.scan_key()).await?))
}

async fn held_diff(
    State(st): State<RunnerState>,
    Path((id, task)): Path<(String, String)>,
) -> R<Json<DiffResult>> {
    let repo = existing(&st, &id)?;
    let (diff, changed) = repo
        .held_diff(&task, 2_000_000)
        .await
        .map_err(|e| ApiErr(StatusCode::NOT_FOUND, format!("{e:#}")))?;
    Ok(Json(DiffResult { diff, changed }))
}

async fn discard_held(
    State(st): State<RunnerState>,
    Path((id, task)): Path<(String, String)>,
) -> R<Json<serde_json::Value>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let busy = st.workspace_busy(&id).await;
    repo.discard(&task, busy).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn baseline_check(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<gate::BaselineCheck>,
) -> R<Json<gate::BaselineResult>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let head = match &req.task {
        // WP04: the baseline lives in the main repository; the task's HEAD
        // is copied there first.
        Some(t) => {
            let task = task_repo(&st, &id, t)?;
            let tl = st.lock(t).await;
            let _tg = tl.lock().await;
            Some(repo.fetch_task_head(&task, t).await?)
        }
        None => None,
    };
    Ok(Json(repo.baseline_check(&req, head.as_deref()).await?))
}

/// Main repository `id` (a project workspace) and task workspace `task`.
fn task_repo(st: &RunnerState, id: &str, task: &str) -> R<Repo> {
    if is_task_workspace(id) || !is_task_workspace(task) {
        return Err(ApiErr(
            StatusCode::BAD_REQUEST,
            "invalid task workspace".into(),
        ));
    }
    st.repo(task)
}

async fn task_prepare(
    State(st): State<RunnerState>,
    Path((id, t)): Path<(String, String)>,
    Json(req): Json<TaskWorkspaceRequest>,
) -> R<Json<TaskWorkspaceResult>> {
    let task = task_repo(&st, &id, &t)?;
    let main = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let tl = st.lock(&t).await;
    let _tg = tl.lock().await;
    Ok(Json(
        main.prepare_task(&task, &t, &req, st.scan_key()).await?,
    ))
}

#[derive(Deserialize)]
struct BranchQ {
    #[serde(default)]
    branch: Option<String>,
}

async fn task_remove(
    State(st): State<RunnerState>,
    Path((id, t)): Path<(String, String)>,
    Query(q): Query<BranchQ>,
) -> R<Json<serde_json::Value>> {
    let task = task_repo(&st, &id, &t)?;
    let main = st.repo(&id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let tl = st.lock(&t).await;
    let _tg = tl.lock().await;
    if st.workspace_busy(&t).await {
        return Err(ApiErr(
            StatusCode::CONFLICT,
            "a process runs in this task workspace".into(),
        ));
    }
    main.remove_task(&task, &t, q.branch.as_deref()).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn task_sync(
    State(st): State<RunnerState>,
    Path((id, t)): Path<(String, String)>,
    Json(req): Json<SyncRequest>,
) -> R<Json<serde_json::Value>> {
    let task = task_repo(&st, &id, &t)?;
    let main = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let tl = st.lock(&t).await;
    let _tg = tl.lock().await;
    if st.workspace_busy(&t).await {
        return Err(ApiErr(
            StatusCode::CONFLICT,
            "a process runs in this task workspace".into(),
        ));
    }
    main.sync_task(&task, &t, &req.branch, &req.sha).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

async fn task_merge(
    State(st): State<RunnerState>,
    Path((id, t)): Path<(String, String)>,
    Json(req): Json<MergeRequest>,
) -> R<Json<MergeResult>> {
    let task = task_repo(&st, &id, &t)?;
    let main = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let tl = st.lock(&t).await;
    let _tg = tl.lock().await;
    if req.checkout && st.workspace_busy(&t).await {
        return Err(ApiErr(
            StatusCode::CONFLICT,
            "a process runs in this task workspace".into(),
        ));
    }
    Ok(Json(
        main.merge_prepare(&task, &t, &req, st.scan_key()).await?,
    ))
}

async fn task_merge_finish(
    State(st): State<RunnerState>,
    Path((id, t)): Path<(String, String)>,
    Json(req): Json<MergeFinish>,
) -> R<Json<MergeFinishResult>> {
    task_repo(&st, &id, &t)?;
    let main = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    Ok(Json(main.merge_finish(&t, &req, st.scan_key()).await?))
}

async fn baseline_approve(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<gate::BaselineApprove>,
) -> R<Json<serde_json::Value>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    repo.baseline_approve(&req).await?;
    Ok(Json(serde_json::json!({"ok": true})))
}

#[derive(Deserialize)]
struct DiffReq {
    from: String,
    to: String,
    #[serde(default = "default_max")]
    max_bytes: usize,
}
fn default_max() -> usize {
    2_000_000
}

async fn diff(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<DiffReq>,
) -> R<Json<DiffResult>> {
    let repo = existing(&st, &id)?;
    let (diff, changed) = repo.diff(&req.from, &req.to, req.max_bytes).await?;
    Ok(Json(DiffResult { diff, changed }))
}

async fn revert(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<RangeRequest>,
) -> R<Json<Sha>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    repo.revert(&req.from, &req.to)
        .await
        .map(|sha| Json(Sha { sha }))
        .map_err(|e| ApiErr(StatusCode::CONFLICT, format!("{e:#}")))
}

async fn files(State(st): State<RunnerState>, Path(id): Path<String>) -> R<Json<Vec<String>>> {
    Ok(Json(existing(&st, &id)?.files().await?))
}

#[derive(Deserialize)]
struct PathQ {
    path: String,
}

async fn file(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Query(q): Query<PathQ>,
) -> R<Json<FileContent>> {
    let content = existing(&st, &id)?.read_file(&q.path, 1024 * 1024).await?;
    Ok(Json(FileContent {
        path: q.path,
        content,
    }))
}

async fn zip(State(st): State<RunnerState>, Path(id): Path<String>) -> R<Response> {
    let data = existing(&st, &id)?.zip().await?;
    Ok(([(header::CONTENT_TYPE, "application/zip")], data).into_response())
}

#[derive(Deserialize)]
struct AuthorQ {
    #[serde(default = "default_author")]
    author_name: String,
    #[serde(default = "default_email")]
    author_email: String,
}
fn default_author() -> String {
    "Queue".into()
}
fn default_email() -> String {
    "queue@localhost".into()
}

async fn import(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Query(a): Query<AuthorQ>,
    body: Bytes,
) -> R<Json<ImportResult>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let (files, sha) = repo
        .import_zip(body.to_vec(), &a.author_name, &a.author_email)
        .await?;
    Ok(Json(ImportResult { files, sha }))
}

async fn push(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Json(req): Json<PushRequest>,
) -> R<Json<PushResult>> {
    let repo = existing(&st, &id)?;
    let l = st.lock(&id).await;
    let _g = l.lock().await;
    let opt = git::PushOptions {
        sha: req.sha.as_deref(),
        allow_protected: req.allow_protected,
        allow: &req.allow,
        allow_secrets: req.allow_secrets,
        key: st.scan_key(),
        scan_base: req.scan_base.as_deref(),
        expect: req.expect.as_deref(),
    };
    Ok(Json(match repo.push_checked(&req.remote, &opt).await {
        Ok(o) => PushResult {
            ok: o.held.is_empty() && !o.moved,
            output: o.output,
            held: o.held,
            moved: o.moved,
        },
        Err(e) => PushResult {
            ok: false,
            output: format!("{e:#}"),
            held: vec![],
            moved: false,
        },
    }))
}

#[derive(Deserialize)]
struct LogQ {
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    50
}

async fn log(
    State(st): State<RunnerState>,
    Path(id): Path<String>,
    Query(q): Query<LogQ>,
) -> R<Json<Vec<Commit>>> {
    Ok(Json(existing(&st, &id)?.log(q.limit).await?))
}

async fn queue_file(
    State(st): State<RunnerState>,
    Path((id, name)): Path<(String, String)>,
) -> R<Json<QueueFile>> {
    Ok(Json(QueueFile {
        content: existing(&st, &id)?.queue_file(&name).await?,
    }))
}

/// Streams NDJSON frames. If the client disconnects, the run is cancelled.
async fn exec_start(State(st): State<RunnerState>, Json(req): Json<ExecRequest>) -> R<Response> {
    if !valid_name(&req.name) || !valid_name(&req.workspace) {
        return Err(ApiErr(StatusCode::BAD_REQUEST, "invalid name".into()));
    }
    let cancel = CancellationToken::new();
    {
        let mut running = st.running.lock().await;
        if running.contains_key(&req.name) {
            return Err(ApiErr(
                StatusCode::CONFLICT,
                "run with this name is active".into(),
            ));
        }
        running.insert(req.name.clone(), cancel.clone());
        st.active
            .lock()
            .await
            .insert(req.name.clone(), req.workspace.clone());
    }
    let (tx, mut rx) = mpsc::channel::<ExecFrame>(256);
    let (body_tx, body_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(256);
    let name = req.name.clone();
    let st2 = st.clone();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        let cfg = st2.cfg.clone();
        let runner = tokio::spawn(async move { exec::run(&cfg, req, c2, tx).await });
        let mut ended = false;
        while let Some(frame) = rx.recv().await {
            if matches!(frame, ExecFrame::Exit { .. } | ExecFrame::Fail { .. }) {
                ended = true;
                // The process is gone: the workspace is free before the
                // client sees the end (it may remove the working tree next).
                st2.running.lock().await.remove(&name);
                st2.active.lock().await.remove(&name);
            }
            let mut line = serde_json::to_vec(&frame).unwrap_or_default();
            line.push(b'\n');
            if body_tx.send(Ok(Bytes::from(line))).await.is_err() {
                cancel.cancel(); // client went away
            }
        }
        let _ = runner.await;
        if !ended {
            // (A run with the same name may already have started otherwise.)
            st2.running.lock().await.remove(&name);
            st2.active.lock().await.remove(&name);
        }
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(body_rx).map(|r| r);
    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response())
}

/// Which agent CLIs exist in the sandbox, and in which version.
async fn cli_status(State(st): State<RunnerState>) -> R<Json<Vec<CliProbe>>> {
    Ok(Json(exec::probe(&st.cfg).await?))
}

async fn exec_cancel(
    State(st): State<RunnerState>,
    Path(name): Path<String>,
) -> R<Json<serde_json::Value>> {
    let found = match st.running.lock().await.get(&name) {
        Some(t) => {
            t.cancel();
            true
        }
        None => false,
    };
    Ok(Json(serde_json::json!({"cancelled": found})))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve() -> (tempfile::TempDir, String) {
        let t = tempfile::tempdir().unwrap();
        let st = RunnerState::new(RunnerConfig::for_tests(t.path().to_path_buf()));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, router(st)).await.unwrap() });
        (t, addr)
    }

    #[tokio::test]
    async fn requires_token() {
        let (_t, a) = serve().await;
        let c = reqwest::Client::new();
        assert_eq!(
            c.get(format!("{a}/healthz")).send().await.unwrap().status(),
            200
        );
        assert_eq!(
            c.get(format!("{a}/v1/workspaces/x/files"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            c.get(format!("{a}/v1/workspaces/x/files"))
                .bearer_auth("wrong")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            c.get(format!("{a}/v1/workspaces/x/files"))
                .bearer_auth("test-token")
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            c.get(format!("{a}/v1/workspaces/..%2Fetc/files"))
                .bearer_auth("test-token")
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }

    #[tokio::test]
    async fn info_and_cache_removal() {
        let (t, a) = serve().await;
        let c = reqwest::Client::new();
        let info: RunnerInfo = c
            .get(format!("{a}/v1/info"))
            .bearer_auth("test-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            info,
            RunnerInfo {
                engine: "none".into(),
                egress_enforced: false
            }
        );
        assert_eq!(
            c.get(format!("{a}/v1/info")).send().await.unwrap().status(),
            401
        );
        c.post(format!("{a}/v1/workspaces/w1"))
            .bearer_auth("test-token")
            .json(&InitWorkspace::default())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let cache = t.path().join("caches/w1/abc");
        std::fs::create_dir_all(&cache).unwrap();
        let del = |path: &str| {
            c.delete(format!("{a}/v1/workspaces/{path}"))
                .bearer_auth("test-token")
                .send()
        };
        assert_eq!(del("w1/cache").await.unwrap().status(), 200);
        assert!(!cache.exists());
        // Idempotent, and deleting the workspace drops its caches too.
        assert_eq!(del("w1/cache").await.unwrap().status(), 200);
        std::fs::create_dir_all(&cache).unwrap();
        assert_eq!(del("w1").await.unwrap().status(), 200);
        assert!(!t.path().join("caches/w1").exists());
        assert_eq!(del("..%2Fx/cache").await.unwrap().status(), 400);
    }

    /// Task endpoints only accept a project workspace as main repository and
    /// a task workspace (`t<32 hex>`) as task: never a project's main
    /// repository in place of a task (deleting it, exporting into it …).
    #[tokio::test]
    async fn task_endpoints_validate_workspace_kinds() {
        let (t, a) = serve().await;
        let c = reqwest::Client::new();
        let main = "11111111-2222-4333-8444-555555555555";
        let other = "11111111-2222-4333-8444-666666666666";
        let task = "t0123456789abcdef0123456789abcdef";
        for ws in [main, other] {
            c.post(format!("{a}/v1/workspaces/{ws}"))
                .bearer_auth("test-token")
                .json(&InitWorkspace::default())
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        let body = TaskWorkspaceRequest {
            branch: "queue/0123456789ab".into(),
            merge_base: None,
            carry: None,
            allow: vec![],
            start: None,
        };
        let post = |path: String| {
            c.post(format!("{a}/v1/workspaces/{path}"))
                .bearer_auth("test-token")
                .json(&body)
                .send()
        };
        assert_eq!(
            post(format!("{main}/tasks/{other}"))
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(
            post(format!("{task}/tasks/{task}")).await.unwrap().status(),
            400
        );
        assert_eq!(
            c.delete(format!("{a}/v1/workspaces/{main}/tasks/{other}"))
                .bearer_auth("test-token")
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert!(t
            .path()
            .join("workspaces")
            .join(other)
            .join(".git")
            .is_dir());
        let r: TaskWorkspaceResult = post(format!("{main}/tasks/{task}"))
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(r.created);
        assert!(t.path().join("workspaces").join(task).join(".git").is_dir());
        assert_eq!(
            c.delete(format!(
                "{a}/v1/workspaces/{main}/tasks/{task}?branch=queue/0123456789ab"
            ))
            .bearer_auth("test-token")
            .send()
            .await
            .unwrap()
            .status(),
            200
        );
        assert!(!t.path().join("workspaces").join(task).exists());
    }

    #[tokio::test]
    async fn exec_streams_ndjson_over_http() {
        let (_t, a) = serve().await;
        let c = reqwest::Client::new();
        c.post(format!("{a}/v1/workspaces/w1"))
            .bearer_auth("test-token")
            .json(&InitWorkspace::default())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let req = ExecRequest {
            name: "run-1".into(),
            workspace: "w1".into(),
            program: "sh".into(),
            args: vec![
                "-c".into(),
                "echo a; echo b >&2; git log --oneline | wc -l".into(),
            ],
            env: vec![],
            home_files: vec![],
            read_back: vec![],
            stdin: None,
            timeout_secs: 10,
            idle_timeout_secs: None,
            queue_files: vec![("result.schema.json".into(), "{\"type\":\"object\"}".into())],
            egress: Default::default(),
            cache: None,
            secret_env: vec![],
            home_env: vec![],
        };
        let body = c
            .post(format!("{a}/v1/exec"))
            .bearer_auth("test-token")
            .json(&req)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let frames: Vec<ExecFrame> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(frames.contains(&ExecFrame::Out { line: "a".into() }));
        assert!(frames.contains(&ExecFrame::Err { line: "b".into() }));
        assert!(matches!(
            frames.last().unwrap(),
            ExecFrame::Exit {
                success: true,
                stalled: false,
                killed: false,
                ..
            }
        ));
        assert_eq!(
            std::fs::read_to_string(_t.path().join("workspaces/w1/.queue/result.schema.json"))
                .unwrap(),
            "{\"type\":\"object\"}",
            "queue files are written into the workspace before the start"
        );
    }
}
