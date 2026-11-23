//! HTTP client for queued-runner. The app never touches workspaces or
//! processes itself – everything privileged-ish goes through here.
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use queued_common::{gate, *};
use serde::{de::DeserializeOwned, Serialize};
use std::{collections::HashSet, sync::Arc};

use tokio::sync::{mpsc, watch};
use uuid::Uuid;

/// Runner workspace id of a task's own working tree (WP04).
pub fn task_ws(task: Uuid) -> String {
    task_workspace_id(&task.simple().to_string())
}

#[derive(Clone)]
pub struct RunnerClient {
    base: String,
    token: String,
    http: reqwest::Client,
    stream: reqwest::Client,
    /// WP11: why this client cannot be used (runner revoked, offline,
    /// project unknown); every call fails with this text.
    fault: Option<Arc<str>>,
    /// WP11: lease of runs started through this client (`None` = no lease).
    lease_secs: Option<u64>,
    /// WP11: names of the runs streaming from this runner right now (sent
    /// with every heartbeat to renew their leases).
    runs: Arc<std::sync::Mutex<HashSet<String>>>,
    /// WP11: `true` while the fleet monitor considers the runner lost; open
    /// exec streams end at once (instead of hanging on a dead host).
    lost: Arc<watch::Sender<bool>>,
}

/// Removes a run name from the heartbeat set when its stream ends.
struct RunGuard {
    runs: Arc<std::sync::Mutex<HashSet<String>>>,
    name: String,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.name);
    }
}

/// Error returned by the runner, with its HTTP status.
#[derive(Debug)]
pub struct RunnerError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for RunnerError {}

impl RunnerClient {
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base: format!("{}{}", base.trim_end_matches('/'), API_PREFIX),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("http client"),
            stream: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
            fault: None,
            lease_secs: None,
            runs: Default::default(),
            lost: Arc::new(watch::channel(false).0),
        }
    }

    /// WP11: a client whose every call fails with `why`.
    pub fn faulty(why: &str) -> Self {
        let mut c = Self::new("http://invalid.invalid", "-");
        c.fault = Some(why.into());
        c
    }

    /// WP11: runs started through this client carry a lease.
    pub fn with_lease(mut self, secs: u64) -> Self {
        self.lease_secs = Some(secs);
        self
    }

    /// WP11: the same runner under a new address/token; open runs, the
    /// heartbeat set and the lost signal are kept.
    pub fn renewed(&self, base: &str, token: &str) -> Self {
        let mut c = Self::new(base, token);
        c.lease_secs = self.lease_secs;
        c.runs = self.runs.clone();
        c.lost = self.lost.clone();
        c
    }

    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// WP11: marks the runner lost (open streams end) or back.
    pub fn set_lost(&self, lost: bool) {
        self.lost.send_if_modified(|v| {
            let changed = *v != lost;
            *v = lost;
            changed
        });
    }

    pub fn is_lost(&self) -> bool {
        *self.lost.borrow()
    }

    /// Runs streaming from this runner right now.
    pub fn active_runs(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// The HTTP client, unless the runner cannot be used (fault) or the
    /// fleet monitor declared it lost (fail fast instead of a timeout).
    fn h(&self) -> Result<&reqwest::Client> {
        match &self.fault {
            Some(f) => Err(anyhow!("{f}")),
            None if self.is_lost() => Err(anyhow!("runner unreachable (no heartbeat)")),
            None => Ok(&self.http),
        }
    }

    /// Sends a request; ends with an error as soon as the runner is
    /// declared lost (a hung host never answers, not even with a reset).
    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let mut lost = self.lost.subscribe();
        if *lost.borrow_and_update() {
            return Err(anyhow!("runner unreachable (no heartbeat)"));
        }
        let gone = async {
            while lost.changed().await.is_ok() {
                if *lost.borrow_and_update() {
                    return;
                }
            }
            futures::future::pending::<()>().await
        };
        tokio::select! {
            r = rb.send() => Ok(r.context("runner unreachable")?),
            _ = gone => Err(anyhow!("runner unreachable (no heartbeat)")),
        }
    }

    /// WP11: renews the leases of the open runs; the runner's state.
    pub async fn heartbeat(&self, lease_secs: u64, timeout: Duration) -> Result<HeartbeatResponse> {
        if let Some(f) = &self.fault {
            return Err(anyhow!("{f}"));
        }
        let r = self
            .http
            .post(format!("{}/heartbeat", self.base))
            .bearer_auth(&self.token)
            .timeout(timeout)
            .json(&HeartbeatRequest {
                lease_secs,
                runs: self.active_runs(),
            })
            .send()
            .await
            .context("runner unreachable")?;
        Ok(Self::check(r).await?.json().await?)
    }

    /// WP11: workspace ids on the runner.
    pub async fn workspaces(&self) -> Result<Vec<String>> {
        Ok(self
            .get::<WorkspaceList>(format!("{}/workspaces", self.base))
            .await?
            .workspaces)
    }

    /// WP11: `git bundle` of a workspace as a byte stream plus its HEAD ref.
    pub async fn bundle(&self, ws: impl std::fmt::Display) -> Result<(String, reqwest::Response)> {
        self.h()?;
        let r = self
            .stream
            .get(self.url(ws, "/bundle"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("runner unreachable")?;
        let r = Self::check(r).await?;
        let head = r
            .headers()
            .get(BUNDLE_HEAD_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        Ok((head, r))
    }

    /// WP11: creates a workspace from a bundle (must not exist yet).
    pub async fn restore(
        &self,
        ws: impl std::fmt::Display,
        head: &str,
        body: reqwest::Body,
    ) -> Result<String> {
        self.h()?;
        let r = self
            .stream
            .post(self.url(ws, "/restore"))
            .bearer_auth(&self.token)
            .header(BUNDLE_HEAD_HEADER, head)
            .timeout(Duration::from_secs(3600))
            .body(body)
            .send()
            .await
            .context("runner unreachable")?;
        Ok(Self::check(r).await?.json::<Sha>().await?.sha)
    }

    fn url(&self, ws: impl std::fmt::Display, path: &str) -> String {
        format!("{}/workspaces/{ws}{path}", self.base)
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let message = resp
            .json::<ErrorBody>()
            .await
            .map(|e| e.error)
            .unwrap_or_else(|_| format!("runner returned {status}"));
        Err(RunnerError { status, message }.into())
    }

    async fn post<B: Serialize, T: DeserializeOwned>(&self, url: String, body: &B) -> Result<T> {
        let r = self
            .send(self.h()?.post(url).bearer_auth(&self.token).json(body))
            .await
            .context("runner unreachable")?;
        Ok(Self::check(r).await?.json().await?)
    }

    async fn get<T: DeserializeOwned>(&self, url: String) -> Result<T> {
        let r = self
            .send(self.h()?.get(url).bearer_auth(&self.token))
            .await
            .context("runner unreachable")?;
        Ok(Self::check(r).await?.json().await?)
    }

    pub async fn init(&self, ws: impl std::fmt::Display, req: &InitWorkspace) -> Result<String> {
        Ok(self.post::<_, Sha>(self.url(ws, ""), req).await?.sha)
    }

    pub async fn delete(&self, ws: impl std::fmt::Display) -> Result<()> {
        let r = self
            .send(self.h()?.delete(self.url(ws, "")).bearer_auth(&self.token))
            .await?;
        Self::check(r).await?;
        Ok(())
    }

    pub async fn begin(
        &self,
        ws: impl std::fmt::Display,
        remote: Option<GitRemote>,
        allow: &[String],
    ) -> Result<BeginResult> {
        self.post(
            self.url(ws, "/begin"),
            &BeginTask {
                remote,
                allow: allow.to_vec(),
            },
        )
        .await
    }

    /// Commits unless the changes contain a secret outside `allow`.
    pub async fn commit(
        &self,
        ws: impl std::fmt::Display,
        message: &str,
        author_name: &str,
        author_email: &str,
        allow: &[String],
        squash_base: Option<&str>,
    ) -> Result<CommitOutcome> {
        let req = CommitRequest {
            message: message.into(),
            author_name: author_name.into(),
            author_email: author_email.into(),
            allow: allow.to_vec(),
            allow_large: false,
            squash_base: squash_base.map(String::from),
        };
        self.post(self.url(ws, "/commit"), &req).await
    }

    /// Gate check of the working tree (WP03).
    pub async fn inspect(
        &self,
        ws: impl std::fmt::Display,
        req: &gate::InspectRequest,
    ) -> Result<gate::InspectResult> {
        self.post(self.url(ws, "/inspect"), req).await
    }

    /// Parks the working tree under `refs/queue/held/<task>`.
    pub async fn hold(&self, ws: impl std::fmt::Display, task: Uuid, base: &str) -> Result<()> {
        let _: serde_json::Value = self
            .post(
                self.url(ws, "/hold"),
                &gate::HoldRequest {
                    task: task.simple().to_string(),
                    base: base.to_string(),
                },
            )
            .await?;
        Ok(())
    }

    pub async fn release(
        &self,
        ws: impl std::fmt::Display,
        req: &gate::ReleaseRequest,
    ) -> Result<CommitOutcome> {
        self.post(self.url(ws, "/release"), req).await
    }

    pub async fn held_diff(&self, ws: impl std::fmt::Display, task: Uuid) -> Result<DiffResult> {
        self.get(self.url(ws, &format!("/held/{}", task.simple())))
            .await
    }

    pub async fn discard_held(&self, ws: impl std::fmt::Display, task: Uuid) -> Result<()> {
        let r = self
            .send(
                self.h()?
                    .delete(self.url(ws, &format!("/held/{}", task.simple())))
                    .bearer_auth(&self.token),
            )
            .await
            .context("runner unreachable")?;
        Self::check(r).await?;
        Ok(())
    }

    pub async fn baseline_check(
        &self,
        ws: impl std::fmt::Display,
        req: &gate::BaselineCheck,
    ) -> Result<gate::BaselineResult> {
        self.post(self.url(ws, "/baseline/check"), req).await
    }

    pub async fn baseline_approve(
        &self,
        ws: impl std::fmt::Display,
        name: &str,
        sha: &str,
    ) -> Result<()> {
        let _: serde_json::Value = self
            .post(
                self.url(ws, "/baseline/approve"),
                &gate::BaselineApprove {
                    name: name.into(),
                    sha: sha.into(),
                },
            )
            .await?;
        Ok(())
    }

    pub async fn diff(
        &self,
        ws: impl std::fmt::Display,
        from: &str,
        to: &str,
        max_bytes: usize,
    ) -> Result<DiffResult> {
        self.post(
            self.url(ws, "/diff"),
            &serde_json::json!({"from": from, "to": to, "max_bytes": max_bytes}),
        )
        .await
    }

    pub async fn revert(&self, ws: impl std::fmt::Display, from: &str, to: &str) -> Result<String> {
        Ok(self
            .post::<_, Sha>(
                self.url(ws, "/revert"),
                &RangeRequest {
                    from: from.into(),
                    to: to.into(),
                },
            )
            .await?
            .sha)
    }

    /// Creates or continues the task's own working tree (WP04).
    pub async fn prepare_task(
        &self,
        main: Uuid,
        task: Uuid,
        req: &TaskWorkspaceRequest,
    ) -> Result<TaskWorkspaceResult> {
        self.post(self.url(main, &format!("/tasks/{}", task_ws(task))), req)
            .await
    }

    /// Copies the task commit `sha` to its branch in the main repository.
    pub async fn sync_task(&self, main: Uuid, task: Uuid, branch: &str, sha: &str) -> Result<()> {
        let _: serde_json::Value = self
            .post(
                self.url(main, &format!("/tasks/{}/sync", task_ws(task))),
                &SyncRequest {
                    branch: branch.into(),
                    sha: sha.into(),
                },
            )
            .await?;
        Ok(())
    }

    /// Merge queue: squash onto the target branch and re-check.
    pub async fn merge_task(
        &self,
        main: Uuid,
        task: Uuid,
        req: &MergeRequest,
    ) -> Result<MergeResult> {
        self.post(
            self.url(main, &format!("/tasks/{}/merge", task_ws(task))),
            req,
        )
        .await
    }

    /// Merge queue: fast-forward of the target branch.
    pub async fn merge_finish(
        &self,
        main: Uuid,
        task: Uuid,
        req: &MergeFinish,
    ) -> Result<MergeFinishResult> {
        self.post(
            self.url(main, &format!("/tasks/{}/merge/finish", task_ws(task))),
            req,
        )
        .await
    }

    /// Removes the task's working tree (and with `branch` its branch).
    pub async fn remove_task(&self, main: Uuid, task: Uuid, branch: Option<&str>) -> Result<()> {
        let mut url = self.url(main, &format!("/tasks/{}", task_ws(task)));
        if let Some(b) = branch {
            url.push_str(&format!("?branch={b}"));
        }
        let r = self
            .send(self.h()?.delete(url).bearer_auth(&self.token))
            .await
            .context("runner unreachable")?;
        Self::check(r).await?;
        Ok(())
    }

    /// WP12: starts the dev server of a task's working tree (screenshots,
    /// live preview). Takes as long as the ready timeout plus screenshots.
    pub async fn preview_start(
        &self,
        main: Uuid,
        task: Uuid,
        req: &queued_common::PreviewStart,
    ) -> Result<queued_common::PreviewStarted> {
        let secs = req.ready_timeout_secs + 70 * req.shots.len() as u64 + 30;
        let r = self
            .send(
                self.h()?
                    .post(self.url(main, &format!("/tasks/{}/preview", task_ws(task))))
                    .bearer_auth(&self.token)
                    .timeout(Duration::from_secs(secs))
                    .json(req),
            )
            .await
            .context("runner unreachable")?;
        Ok(Self::check(r).await?.json().await?)
    }

    /// WP12: stops a live preview (`false` = it was not running).
    pub async fn preview_stop(&self, name: &str) -> Result<bool> {
        let v: serde_json::Value = self
            .post(format!("{}/previews/{name}/stop", self.base), &serde_json::json!({}))
            .await?;
        Ok(v["stopped"].as_bool().unwrap_or(false))
    }

    /// WP12: running previews of the runner.
    pub async fn previews(&self) -> Result<queued_common::PreviewList> {
        self.get(format!("{}/previews", self.base)).await
    }

    /// WP12: one request to a live preview. Returns the dev server's status,
    /// its (runner-filtered) headers and the body.
    pub async fn preview_http(
        &self,
        name: &str,
        method: &str,
        path: &str,
        headers: Vec<(String, Vec<u8>)>,
        body: bytes::Bytes,
    ) -> Result<(u16, Vec<(String, Vec<u8>)>, bytes::Bytes)> {
        let mut rb = self
            .h()?
            .post(format!("{}/previews/{name}/http", self.base))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(60))
            .header("x-queue-preview-method", method)
            .header("x-queue-preview-path", path);
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        let r = self
            .send(rb.body(body))
            .await
            .context("runner unreachable")?;
        let r = Self::check(r).await?;
        let status = r
            .headers()
            .get(queued_common::PREVIEW_STATUS_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(502);
        let hs = r
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
            .collect();
        Ok((status, hs, r.bytes().await?))
    }

    /// Engine and whether egress rules are enforced.
    pub async fn info(&self) -> Result<RunnerInfo> {
        self.get(format!("{}/info", self.base)).await
    }

    /// Drops the dependency caches of a project.
    pub async fn clear_cache(&self, ws: impl std::fmt::Display) -> Result<()> {
        let r = self
            .send(
                self.h()?
                    .delete(self.url(ws, "/cache"))
                    .bearer_auth(&self.token),
            )
            .await
            .context("runner unreachable")?;
        Self::check(r).await?;
        Ok(())
    }

    /// Agent CLIs found in the sandbox (`<cli> --version`).
    pub async fn cli_status(&self) -> Result<Vec<queued_common::CliProbe>> {
        self.get(format!("{}/cli-status", self.base)).await
    }

    pub async fn files(&self, ws: impl std::fmt::Display) -> Result<Vec<String>> {
        self.get(self.url(ws, "/files")).await
    }

    pub async fn file(&self, ws: impl std::fmt::Display, path: &str) -> Result<FileContent> {
        let q: String = url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
        self.get(self.url(ws, &format!("/file?path={q}"))).await
    }

    pub async fn zip(&self, ws: impl std::fmt::Display) -> Result<Vec<u8>> {
        let r = self
            .send(self.h()?.get(self.url(ws, "/zip")).bearer_auth(&self.token))
            .await?;
        Ok(Self::check(r).await?.bytes().await?.to_vec())
    }

    pub async fn import(
        &self,
        ws: impl std::fmt::Display,
        data: Vec<u8>,
        author_name: &str,
        author_email: &str,
    ) -> Result<ImportResult> {
        let r = self
            .send(
                self.h()?
                    .post(self.url(ws, "/import"))
                    .query(&[("author_name", author_name), ("author_email", author_email)])
                    .bearer_auth(&self.token)
                    .body(data),
            )
            .await?;
        Ok(Self::check(r).await?.json().await?)
    }

    pub async fn push(&self, ws: impl std::fmt::Display, req: &PushRequest) -> Result<PushResult> {
        self.post(self.url(ws, "/push"), req).await
    }

    pub async fn log(&self, ws: impl std::fmt::Display, limit: usize) -> Result<Vec<Commit>> {
        self.get(self.url(ws, &format!("/log?limit={limit}"))).await
    }

    pub async fn queue_file(
        &self,
        ws: impl std::fmt::Display,
        name: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .get::<QueueFile>(self.url(ws, &format!("/queue/{name}")))
            .await?
            .content)
    }

    /// WP09: replaces `.queue/attachments/` of a workspace.
    pub async fn put_attachments(
        &self,
        ws: impl std::fmt::Display,
        files: &[(String, Vec<u8>)],
    ) -> Result<()> {
        use base64::Engine as _;
        let req = queued_common::ProvideAttachments {
            files: files
                .iter()
                .map(|(n, d)| queued_common::AttachmentFile {
                    name: n.clone(),
                    data: base64::engine::general_purpose::STANDARD.encode(d),
                })
                .collect(),
        };
        let r = self
            .send(
                self.h()?
                    .put(self.url(ws, "/attachments"))
                    .bearer_auth(&self.token)
                    .json(&req),
            )
            .await
            .context("runner unreachable")?;
        Self::check(r).await?;
        Ok(())
    }

    /// WP09: check of the agent's `.queue/state.json`.
    pub async fn state_check(
        &self,
        ws: impl std::fmt::Display,
        req: &queued_common::StateCheckRequest,
    ) -> Result<queued_common::StateCheckResult> {
        self.post(self.url(ws, "/state/check"), req).await
    }

    /// WP09: managed `AGENTS.md` + bridges into the main repository.
    pub async fn knowledge_export(
        &self,
        ws: impl std::fmt::Display,
        req: &queued_common::KnowledgeExport,
    ) -> Result<queued_common::KnowledgeExportResult> {
        self.post(self.url(ws, "/knowledge/export"), req).await
    }

    /// Streams frames into `tx` until the run ends. Returns the final frame.
    /// WP11: the run carries this client's lease and ends with an error as
    /// soon as the fleet monitor declares the runner lost.
    pub async fn exec(&self, req: &ExecRequest, tx: mpsc::Sender<ExecFrame>) -> Result<ExecFrame> {
        self.h()?;
        let mut lost = self.lost.subscribe();
        if *lost.borrow_and_update() {
            return Err(anyhow!("runner unreachable (no heartbeat)"));
        }
        let mut req = req.clone();
        req.lease_secs = self.lease_secs;
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(req.name.clone());
        let _guard = RunGuard {
            runs: self.runs.clone(),
            name: req.name.clone(),
        };
        let run = async {
            let r = self
                .stream
                .post(format!("{}/exec", self.base))
                .bearer_auth(&self.token)
                // Runtime + SIGINT grace (runner side, ≤ 60 s) + slack for setup/cleanup.
                .timeout(Duration::from_secs(req.timeout_secs + 180))
                .json(&req)
                .send()
                .await
                .context("runner unreachable")?;
            let r = Self::check(r).await?;
            let mut body = r.bytes_stream();
            let mut buf: Vec<u8> = vec![];
            let mut last: Option<ExecFrame> = None;
            while let Some(chunk) = body.next().await {
                buf.extend_from_slice(&chunk.context("runner stream interrupted")?);
                while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let frame: ExecFrame = serde_json::from_slice(&line[..line.len() - 1])
                        .context("bad frame from runner")?;
                    match frame {
                        ExecFrame::Exit { .. } | ExecFrame::Fail { .. } => last = Some(frame),
                        f => {
                            let _ = tx.send(f).await;
                        }
                    }
                }
            }
            last.ok_or_else(|| anyhow!("runner stream ended without exit frame"))
        };
        let gone = async {
            while lost.changed().await.is_ok() {
                if *lost.borrow_and_update() {
                    return;
                }
            }
            futures::future::pending::<()>().await
        };
        tokio::select! {
            r = run => r,
            _ = gone => Err(anyhow!(
                "runner unreachable (no heartbeat): the run was abandoned; the runner stops it when its lease expires"
            )),
        }
    }

    pub async fn cancel(&self, name: &str) -> Result<()> {
        let r = self
            .send(
                self.h()?
                    .post(format!("{}/exec/{name}/cancel", self.base))
                    .bearer_auth(&self.token),
            )
            .await?;
        Self::check(r).await?;
        Ok(())
    }
}

/// Maps a runner error status to something meaningful for API callers.
pub fn runner_status(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<RunnerError>().map(|r| r.status)
}
