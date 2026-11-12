//! HTTP client for queued-runner. The app never touches workspaces or
//! processes itself – everything privileged-ish goes through here.
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use queued_common::{gate, *};
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::mpsc;
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
        }
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
            .http
            .post(url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .context("runner unreachable")?;
        Ok(Self::check(r).await?.json().await?)
    }

    async fn get<T: DeserializeOwned>(&self, url: String) -> Result<T> {
        let r = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("runner unreachable")?;
        Ok(Self::check(r).await?.json().await?)
    }

    pub async fn init(&self, ws: impl std::fmt::Display, req: &InitWorkspace) -> Result<String> {
        Ok(self.post::<_, Sha>(self.url(ws, ""), req).await?.sha)
    }

    pub async fn delete(&self, ws: impl std::fmt::Display) -> Result<()> {
        let r = self
            .http
            .delete(self.url(ws, ""))
            .bearer_auth(&self.token)
            .send()
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
            .http
            .delete(self.url(ws, &format!("/held/{}", task.simple())))
            .bearer_auth(&self.token)
            .send()
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
            .http
            .delete(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("runner unreachable")?;
        Self::check(r).await?;
        Ok(())
    }

    /// Engine and whether egress rules are enforced.
    pub async fn info(&self) -> Result<RunnerInfo> {
        self.get(format!("{}/info", self.base)).await
    }

    /// Drops the dependency caches of a project.
    pub async fn clear_cache(&self, ws: impl std::fmt::Display) -> Result<()> {
        let r = self
            .http
            .delete(self.url(ws, "/cache"))
            .bearer_auth(&self.token)
            .send()
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
            .http
            .get(self.url(ws, "/zip"))
            .bearer_auth(&self.token)
            .send()
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
            .http
            .post(self.url(ws, "/import"))
            .query(&[("author_name", author_name), ("author_email", author_email)])
            .bearer_auth(&self.token)
            .body(data)
            .send()
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
            .http
            .put(self.url(ws, "/attachments"))
            .bearer_auth(&self.token)
            .json(&req)
            .send()
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
    pub async fn exec(&self, req: &ExecRequest, tx: mpsc::Sender<ExecFrame>) -> Result<ExecFrame> {
        let r = self
            .stream
            .post(format!("{}/exec", self.base))
            .bearer_auth(&self.token)
            // Runtime + SIGINT grace (runner side, ≤ 60 s) + slack for setup/cleanup.
            .timeout(Duration::from_secs(req.timeout_secs + 180))
            .json(req)
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
    }

    pub async fn cancel(&self, name: &str) -> Result<()> {
        let r = self
            .http
            .post(format!("{}/exec/{name}/cancel", self.base))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Self::check(r).await?;
        Ok(())
    }
}

/// Maps a runner error status to something meaningful for API callers.
pub fn runner_status(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<RunnerError>().map(|r| r.status)
}
