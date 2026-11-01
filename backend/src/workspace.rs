//! Project workspace on disk + invisible snapshot history.
//!
//! The snapshot repo lives *outside* the workspace (`data/snapshots/<id>.git`,
//! git --git-dir/--work-tree), so agents never see or touch it and the user
//! never deals with git. A user's own `.git` inside the workspace is ignored.
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use tokio::process::Command;
use uuid::Uuid;

use crate::config::Config;

const EXCLUDES: &str = ".git/\n.queue/\nnode_modules/\ntarget/\n.venv/\nvenv/\n__pycache__/\ndist/\nbuild/\n.next/\n.DS_Store\n";
pub const QUEUE_DIR: &str = ".queue";

pub struct Workspace {
    pub dir: PathBuf,
    git_dir: PathBuf,
}

impl Workspace {
    pub fn new(cfg: &Config, project_id: Uuid) -> Self {
        Self {
            dir: cfg.data_dir.join("workspaces").join(project_id.to_string()),
            git_dir: cfg.data_dir.join("snapshots").join(format!("{project_id}.git")),
        }
    }

    async fn git(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .arg(format!("--git-dir={}", self.git_dir.display()))
            .arg(format!("--work-tree={}", self.dir.display()))
            .args(["-c", "safe.directory=*", "-c", "user.name=queue", "-c", "user.email=queue@localhost", "-c", "core.quotepath=off"])
            .args(args)
            .current_dir(&self.dir)
            .output()
            .await
            .context("git not available")?;
        if !out.status.success() {
            bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn ensure(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        if !self.git_dir.join("HEAD").exists() {
            tokio::fs::create_dir_all(&self.git_dir).await?;
            let out = Command::new("git").args(["init", "-q", "--bare"]).arg(&self.git_dir).output().await?;
            if !out.status.success() {
                bail!("git init failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            tokio::fs::create_dir_all(self.git_dir.join("info")).await?;
            tokio::fs::write(self.git_dir.join("info/exclude"), EXCLUDES).await?;
            self.git(&["config", "core.bare", "false"]).await?;
            self.snapshot("init").await?;
        }
        Ok(())
    }

    /// Commits the full current state; returns the commit id.
    pub async fn snapshot(&self, msg: &str) -> Result<String> {
        self.git(&["add", "-A"]).await?;
        if self.git(&["status", "--porcelain"]).await?.trim().is_empty() {
            if let Ok(head) = self.git(&["rev-parse", "--verify", "-q", "HEAD"]).await {
                return Ok(head.trim().to_string());
            }
        }
        self.git(&["commit", "-q", "--allow-empty", "--no-verify", "-m", msg]).await?;
        Ok(self.git(&["rev-parse", "HEAD"]).await?.trim().to_string())
    }

    pub async fn diff(&self, from: &str, to: &str, max_bytes: usize) -> Result<String> {
        let mut d = self.git(&["diff", "--no-color", "--stat", "--patch", from, to]).await?;
        if d.len() > max_bytes {
            let stat = self.git(&["diff", "--no-color", "--stat", from, to]).await?;
            d = format!("{stat}\n[diff truncated – {} bytes total; inspect the files directly]\n", d.len());
        }
        Ok(d)
    }

    pub async fn has_changes(&self, from: &str, to: &str) -> Result<bool> {
        Ok(!self.git(&["diff", "--name-only", from, to]).await?.trim().is_empty())
    }

    /// Reverts the changes of one snapshot range (from..to) on top of the current state.
    pub async fn revert(&self, from: &str, to: &str, label: &str) -> Result<String> {
        self.snapshot("pre-revert").await?;
        let patch = self.git(&["diff", "--binary", from, to]).await?;
        if patch.trim().is_empty() {
            bail!("nothing to revert");
        }
        let tmp = self.git_dir.join("revert.patch");
        tokio::fs::write(&tmp, patch).await?;
        let tmp_s = tmp.to_string_lossy().to_string();
        // Plain `git apply` is atomic: on conflict nothing in the workspace is touched.
        let res = self.git(&["apply", "-R", "--whitespace=nowarn", &tmp_s]).await;
        let _ = tokio::fs::remove_file(&tmp).await;
        res.context("revert conflicts with later changes")?;
        self.snapshot(&format!("revert {label}")).await
    }

    pub async fn list_files(&self) -> Result<Vec<String>> {
        self.snapshot("browse").await?;
        Ok(self.git(&["ls-tree", "-r", "--name-only", "HEAD"]).await?.lines().map(String::from).collect())
    }

    pub async fn zip(&self) -> Result<Vec<u8>> {
        self.snapshot("export").await?;
        let out = Command::new("git")
            .arg(format!("--git-dir={}", self.git_dir.display()))
            .args(["-c", "safe.directory=*", "archive", "--format=zip", "HEAD"])
            .output()
            .await?;
        if !out.status.success() {
            bail!("git archive failed");
        }
        Ok(out.stdout)
    }

    pub fn safe_path(&self, rel: &str) -> Result<PathBuf> {
        let p = Path::new(rel);
        if p.components().any(|c| !matches!(c, Component::Normal(_))) {
            bail!("invalid path");
        }
        Ok(self.dir.join(p))
    }

    pub async fn read_file(&self, rel: &str, max: u64) -> Result<String> {
        let path = self.safe_path(rel)?;
        let meta = tokio::fs::metadata(&path).await?;
        if meta.len() > max {
            bail!("file too large ({} bytes)", meta.len());
        }
        let bytes = tokio::fs::read(&path).await?;
        Ok(String::from_utf8(bytes).unwrap_or_else(|_| "[binary file]".into()))
    }

    pub async fn import_zip(&self, data: Vec<u8>) -> Result<usize> {
        let dir = self.dir.clone();
        let n = tokio::task::spawn_blocking(move || -> Result<usize> {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data))?;
            // Strip a single common top-level folder (typical for GitHub/macOS zips).
            let names: Vec<String> = (0..zip.len()).filter_map(|i| zip.by_index(i).ok()?.enclosed_name().map(|p| p.to_string_lossy().to_string())).collect();
            let prefix = common_prefix(&names);
            let mut count = 0;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i)?;
                let Some(rel) = f.enclosed_name() else { continue };
                let rel = rel.strip_prefix(&prefix).unwrap_or(&rel).to_path_buf();
                if rel.as_os_str().is_empty() || rel.starts_with("__MACOSX") {
                    continue;
                }
                let out = dir.join(&rel);
                if f.is_dir() {
                    std::fs::create_dir_all(&out)?;
                } else {
                    if let Some(p) = out.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    std::io::copy(&mut f, &mut std::fs::File::create(&out)?)?;
                    count += 1;
                }
            }
            Ok(count)
        })
        .await??;
        self.snapshot("import zip").await?;
        Ok(n)
    }

    pub async fn reset_queue_dir(&self) -> Result<()> {
        let q = self.dir.join(QUEUE_DIR);
        let _ = tokio::fs::remove_dir_all(&q).await;
        tokio::fs::create_dir_all(&q).await?;
        Ok(())
    }

    pub async fn read_queue_json<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        let s = tokio::fs::read_to_string(self.dir.join(QUEUE_DIR).join(name)).await.ok()?;
        match serde_json::from_str(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("invalid {name}: {e}");
                None
            }
        }
    }

    pub async fn delete(&self) {
        let _ = tokio::fs::remove_dir_all(&self.dir).await;
        let _ = tokio::fs::remove_dir_all(&self.git_dir).await;
    }
}

fn common_prefix(names: &[String]) -> PathBuf {
    let firsts: std::collections::HashSet<&str> = names.iter().filter_map(|n| n.split('/').next()).collect();
    if firsts.len() == 1 && names.iter().any(|n| n.contains('/')) {
        PathBuf::from(firsts.into_iter().next().unwrap())
    } else {
        PathBuf::new()
    }
}
