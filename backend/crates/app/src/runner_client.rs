//! HTTP client for queued-runner. The app never touches workspaces or
//! processes itself – everything privileged-ish goes through here.
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use queued_common::*;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

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

    fn url(&self, ws: Uuid, path: &str) -> String {
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

    pub async fn init(&self, ws: Uuid, req: &InitWorkspace) -> Result<String> {
        Ok(self.post::<_, Sha>(self.url(ws, ""), req).await?.sha)
    }

    pub async fn delete(&self, ws: Uuid) -> Result<()> {
        let r = self
            .http
            .delete(self.url(ws, ""))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Self::check(r).await?;
        Ok(())
    }

    pub async fn begin(&self, ws: Uuid, remote: Option<GitRemote>) -> Result<BeginResult> {
        self.post(self.url(ws, "/begin"), &BeginTask { remote })
            .await
    }

    pub async fn commit(
        &self,
        ws: Uuid,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<String> {
        let req = CommitRequest {
            message: message.into(),
            author_name: author_name.into(),
            author_email: author_email.into(),
        };
        Ok(self
            .post::<_, Sha>(self.url(ws, "/commit"), &req)
            .await?
            .sha)
    }

    pub async fn diff(
        &self,
        ws: Uuid,
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

    pub async fn revert(&self, ws: Uuid, from: &str, to: &str) -> Result<String> {
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

    pub async fn files(&self, ws: Uuid) -> Result<Vec<String>> {
        self.get(self.url(ws, "/files")).await
    }

    pub async fn file(&self, ws: Uuid, path: &str) -> Result<FileContent> {
        let q: String = url::form_urlencoded::byte_serialize(path.as_bytes()).collect();
        self.get(self.url(ws, &format!("/file?path={q}"))).await
    }

    pub async fn zip(&self, ws: Uuid) -> Result<Vec<u8>> {
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
        ws: Uuid,
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

    pub async fn push(&self, ws: Uuid, remote: GitRemote) -> Result<PushResult> {
        self.post(self.url(ws, "/push"), &PushRequest { remote })
            .await
    }

    pub async fn log(&self, ws: Uuid, limit: usize) -> Result<Vec<Commit>> {
        self.get(self.url(ws, &format!("/log?limit={limit}"))).await
    }

    pub async fn queue_file(&self, ws: Uuid, name: &str) -> Result<Option<String>> {
        Ok(self
            .get::<QueueFile>(self.url(ws, &format!("/queue/{name}")))
            .await?
            .content)
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
