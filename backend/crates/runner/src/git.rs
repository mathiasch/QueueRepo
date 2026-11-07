//! Project workspaces as real git repositories.
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use queued_common::{BeginResult, Commit, GitRemote, InitWorkspace};
use tokio::process::Command;

pub const QUEUE_DIR: &str = ".queue";

/// Contract files larger than this are ignored (agent-controlled content).
const MAX_QUEUE_FILE: u64 = 1024 * 1024;

/// Writes `<workspace>/.queue/<name>` without following symlinks the agent
/// may have planted (`.queue` → elsewhere, or `.queue/<name>` → a host file):
/// a symlinked/non-directory `.queue` is replaced by a real directory, an
/// existing entry is removed and the file is created exclusively.
pub async fn write_queue_file(workspace: &Path, name: &str, content: &str) -> Result<()> {
    anyhow::ensure!(
        queued_common::valid_queue_file_name(name),
        "invalid queue file name"
    );
    let dir = workspace.join(QUEUE_DIR);
    match tokio::fs::symlink_metadata(&dir).await {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => {
            tokio::fs::remove_file(&dir).await?;
            tokio::fs::create_dir(&dir).await?;
        }
        Err(_) => tokio::fs::create_dir(&dir).await?,
    }
    let path = dir.join(name);
    match tokio::fs::symlink_metadata(&path).await {
        Ok(m) if m.file_type().is_dir() => tokio::fs::remove_dir_all(&path).await?,
        Ok(_) => tokio::fs::remove_file(&path).await?,
        Err(_) => {}
    }
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
        .with_context(|| format!("create .queue/{name}"))?;
    use tokio::io::AsyncWriteExt;
    f.write_all(content.as_bytes()).await?;
    // tokio writes in the background: without flush the agent may start
    // before the content is on disk.
    f.flush().await?;
    Ok(())
}

const DEFAULT_GITIGNORE: &str = "node_modules/\ntarget/\n.venv/\nvenv/\n__pycache__/\ndist/\nbuild/\n.next/\n.DS_Store\n*.log\n.env\n";

pub struct Repo {
    pub dir: PathBuf,
}

/// `-c http.extraHeader=…` so tokens never end up in remote URLs or config.
fn auth_args(remote: &GitRemote) -> Vec<String> {
    match &remote.token {
        Some(t) if !t.is_empty() => {
            let basic = STANDARD.encode(format!("x-access-token:{t}"));
            vec![
                "-c".into(),
                format!("http.extraHeader=Authorization: Basic {basic}"),
            ]
        }
        _ => vec![],
    }
}

fn redact(s: &str, remote: Option<&GitRemote>) -> String {
    match remote
        .and_then(|r| r.token.as_deref())
        .filter(|t| !t.is_empty())
    {
        Some(t) => s.replace(t, "***"),
        None => s.to_string(),
    }
}

async fn run_git(
    cwd: &Path,
    pre: &[String],
    args: &[&str],
    remote: Option<&GitRemote>,
) -> Result<String> {
    let out = Command::new("git")
        .args([
            "-c",
            "safe.directory=*",
            "-c",
            "core.quotepath=off",
            "-c",
            "user.name=Queue",
            "-c",
            "user.email=queue@localhost",
        ])
        .args(pre)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .output()
        .await
        .context("git not available")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            redact(err.trim(), remote)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Repo {
    pub fn new(root: &Path, id: &str) -> Self {
        Self { dir: root.join(id) }
    }

    async fn git(&self, args: &[&str]) -> Result<String> {
        run_git(&self.dir, &[], args, None).await
    }

    pub fn exists(&self) -> bool {
        self.dir.join(".git").exists()
    }

    pub async fn head(&self) -> Result<String> {
        Ok(self.git(&["rev-parse", "HEAD"]).await?.trim().to_string())
    }

    pub async fn init(&self, req: &InitWorkspace) -> Result<String> {
        if !self.exists() {
            let parent = self.dir.parent().context("no parent")?;
            tokio::fs::create_dir_all(parent).await?;
            match &req.remote {
                Some(r) => {
                    let dir = self.dir.to_string_lossy().to_string();
                    let res = run_git(
                        parent,
                        &auth_args(r),
                        &["clone", "--quiet", "--", &r.url, &dir],
                        Some(r),
                    )
                    .await;
                    if let Err(e) = res {
                        let _ = tokio::fs::remove_dir_all(&self.dir).await;
                        return Err(e.context("clone failed"));
                    }
                    let branch = if r.branch.is_empty() {
                        "main"
                    } else {
                        &r.branch
                    };
                    if self
                        .git(&["rev-parse", "--verify", "-q", "HEAD"])
                        .await
                        .is_err()
                    {
                        // Empty remote repository.
                        self.git(&["checkout", "-q", "-b", branch]).await?;
                    } else if self
                        .git(&["rev-parse", "--abbrev-ref", "HEAD"])
                        .await?
                        .trim()
                        != branch
                    {
                        let remote_ref = format!("origin/{branch}");
                        if self
                            .git(&["rev-parse", "--verify", "-q", &remote_ref])
                            .await
                            .is_ok()
                        {
                            self.git(&["checkout", "-q", "-B", branch, &remote_ref])
                                .await?;
                        } else {
                            self.git(&["checkout", "-q", "-b", branch]).await?;
                        }
                    }
                }
                None => {
                    tokio::fs::create_dir_all(&self.dir).await?;
                    let branch = req
                        .branch
                        .clone()
                        .filter(|b| !b.is_empty())
                        .unwrap_or_else(|| "main".into());
                    self.git(&["init", "-q", "-b", &branch]).await?;
                }
            }
        }
        self.ensure_exclude().await?;
        if self
            .git(&["rev-parse", "--verify", "-q", "HEAD"])
            .await
            .is_err()
        {
            if !self.dir.join(".gitignore").exists() {
                tokio::fs::write(self.dir.join(".gitignore"), DEFAULT_GITIGNORE).await?;
            }
            self.git(&["add", "-A"]).await?;
            self.git(&["commit", "-q", "--allow-empty", "-m", "Initial commit"])
                .await?;
        }
        self.head().await
    }

    async fn ensure_exclude(&self) -> Result<()> {
        let p = self.dir.join(".git/info/exclude");
        let cur = tokio::fs::read_to_string(&p).await.unwrap_or_default();
        if !cur.lines().any(|l| l.trim() == ".queue/") {
            if let Some(d) = p.parent() {
                tokio::fs::create_dir_all(d).await?;
            }
            tokio::fs::write(&p, format!("{cur}\n.queue/\n")).await?;
        }
        Ok(())
    }

    async fn is_dirty(&self) -> Result<bool> {
        Ok(!self
            .git(&["status", "--porcelain"])
            .await?
            .trim()
            .is_empty())
    }

    /// Prepares a task: commits stray changes, optionally pulls, resets `.queue/`.
    pub async fn begin(&self, remote: Option<&GitRemote>) -> Result<BeginResult> {
        let mut warnings = vec![];
        if self.is_dirty().await? {
            self.git(&["add", "-A"]).await?;
            self.git(&[
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "WIP: uncommitted changes before task",
            ])
            .await?;
            warnings.push("uncommitted changes were committed as WIP".into());
        }
        if let Some(r) = remote.filter(|r| !r.url.is_empty()) {
            let branch = if r.branch.is_empty() {
                "main"
            } else {
                &r.branch
            };
            if let Err(e) = run_git(
                &self.dir,
                &auth_args(r),
                &["pull", "--ff-only", "--quiet", "origin", branch],
                Some(r),
            )
            .await
            {
                // A remote without this branch yet (first push pending) is fine.
                if !format!("{e:#}").contains("couldn't find remote ref") {
                    warnings.push(format!("pull skipped: {e:#}"));
                }
            }
        }
        self.reset_queue_dir().await?;
        Ok(BeginResult {
            head: self.head().await?,
            warnings,
        })
    }

    /// Commits everything the agent left uncommitted; returns HEAD.
    pub async fn commit(
        &self,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<String> {
        if self.is_dirty().await? {
            self.git(&["add", "-A"]).await?;
            let author = format!(
                "{} <{}>",
                author_name.replace(['<', '>'], ""),
                author_email.replace(['<', '>'], "")
            );
            self.git(&[
                "commit",
                "-q",
                "--no-verify",
                "--author",
                &author,
                "-m",
                message,
            ])
            .await?;
        }
        self.head().await
    }

    pub async fn diff(&self, from: &str, to: &str, max_bytes: usize) -> Result<(String, bool)> {
        check_rev(from)?;
        check_rev(to)?;
        let names = self.git(&["diff", "--name-only", from, to]).await?;
        let changed = !names.trim().is_empty();
        let mut d = self
            .git(&["diff", "--no-color", "--stat", "--patch", from, to])
            .await?;
        if d.len() > max_bytes {
            let stat = self
                .git(&["diff", "--no-color", "--stat", from, to])
                .await?;
            d = format!(
                "{stat}\n[diff truncated – {} bytes total; inspect the files directly]\n",
                d.len()
            );
        }
        Ok((d, changed))
    }

    /// Reverts all commits in from..to as new commits. Atomic: on conflict the
    /// revert is aborted and the working tree stays untouched.
    pub async fn revert(&self, from: &str, to: &str) -> Result<String> {
        check_rev(from)?;
        check_rev(to)?;
        if self
            .git(&["rev-list", &format!("{from}..{to}")])
            .await?
            .trim()
            .is_empty()
        {
            bail!("nothing to revert");
        }
        if self.is_dirty().await? {
            self.git(&["add", "-A"]).await?;
            self.git(&[
                "commit",
                "-q",
                "--no-verify",
                "-m",
                "WIP: uncommitted changes before revert",
            ])
            .await?;
        }
        if let Err(e) = self
            .git(&["revert", "--no-edit", &format!("{from}..{to}")])
            .await
        {
            let _ = self.git(&["revert", "--abort"]).await;
            return Err(e.context("revert conflicts with later changes"));
        }
        self.head().await
    }

    pub async fn files(&self) -> Result<Vec<String>> {
        let out = self
            .git(&["ls-files", "--cached", "--others", "--exclude-standard"])
            .await?;
        let mut v: Vec<String> = out.lines().map(String::from).collect();
        v.sort();
        v.dedup();
        Ok(v)
    }

    pub fn safe_path(&self, rel: &str) -> Result<PathBuf> {
        let p = Path::new(rel);
        if rel.is_empty()
            || p.components().any(|c| !matches!(c, Component::Normal(_)))
            || p.starts_with(".git")
        {
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

    pub async fn zip(&self) -> Result<Vec<u8>> {
        let out = Command::new("git")
            .args(["-c", "safe.directory=*", "archive", "--format=zip", "HEAD"])
            .current_dir(&self.dir)
            .output()
            .await?;
        if !out.status.success() {
            bail!(
                "git archive failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(out.stdout)
    }

    pub async fn import_zip(
        &self,
        data: Vec<u8>,
        author_name: &str,
        author_email: &str,
    ) -> Result<(usize, String)> {
        let dir = self.dir.clone();
        let n = tokio::task::spawn_blocking(move || extract_zip(&dir, &data)).await??;
        let sha = self.commit("Import ZIP", author_name, author_email).await?;
        Ok((n, sha))
    }

    pub async fn push(&self, r: &GitRemote) -> Result<String> {
        let branch = if r.branch.is_empty() {
            "main"
        } else {
            &r.branch
        };
        let target = format!("HEAD:refs/heads/{branch}");
        // Remote URL may have changed since clone.
        let _ = self.git(&["remote", "remove", "origin"]).await;
        self.git(&["remote", "add", "origin", &r.url]).await?;
        let out = run_git(
            &self.dir,
            &auth_args(r),
            &["push", "--porcelain", "origin", &target],
            Some(r),
        )
        .await?;
        Ok(redact(&out, Some(r)))
    }

    pub async fn log(&self, limit: usize) -> Result<Vec<Commit>> {
        let n = format!("-n{}", limit.clamp(1, 500));
        let out = self
            .git(&["log", &n, "--format=%H%x1f%an%x1f%aI%x1f%s"])
            .await?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let p: Vec<&str> = l.split('\x1f').collect();
                (p.len() == 4).then(|| Commit {
                    sha: p[0].into(),
                    author: p[1].into(),
                    date: p[2].into(),
                    subject: p[3].into(),
                })
            })
            .collect())
    }

    pub async fn reset_queue_dir(&self) -> Result<()> {
        let q = self.dir.join(QUEUE_DIR);
        let _ = tokio::fs::remove_dir_all(&q).await;
        tokio::fs::create_dir_all(&q).await?;
        Ok(())
    }

    /// Reads a contract file the agent wrote. The agent controls the
    /// workspace, so symlinks (`.queue` itself or the file) are never
    /// followed – otherwise any host file readable by the runner would be
    /// handed to the app – and oversized files are ignored.
    pub async fn queue_file(&self, name: &str) -> Result<Option<String>> {
        if !matches!(name, "result.json" | "review.json") {
            bail!("unknown queue file");
        }
        let dir = self.dir.join(QUEUE_DIR);
        let is_real_dir = tokio::fs::symlink_metadata(&dir)
            .await
            .is_ok_and(|m| m.file_type().is_dir());
        let path = dir.join(name);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(m) if is_real_dir && m.file_type().is_file() && m.len() <= MAX_QUEUE_FILE => {
                Ok(tokio::fs::read_to_string(&path).await.ok())
            }
            _ => Ok(None),
        }
    }

    pub async fn delete(&self) -> Result<()> {
        if self.dir.exists() {
            tokio::fs::remove_dir_all(&self.dir).await?;
        }
        Ok(())
    }
}

/// Only hex SHAs are accepted as revisions (no option injection).
fn check_rev(r: &str) -> Result<()> {
    if r.len() < 7 || r.len() > 64 || !r.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid revision");
    }
    Ok(())
}

fn extract_zip(dir: &Path, data: &[u8]) -> Result<usize> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data))?;
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| {
            zip.by_index(i)
                .ok()?
                .enclosed_name()
                .map(|p| p.to_string_lossy().to_string())
        })
        .collect();
    let prefix = common_prefix(&names);
    let mut count = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else {
            continue;
        };
        let rel = rel.strip_prefix(&prefix).unwrap_or(&rel).to_path_buf();
        if rel.as_os_str().is_empty() || rel.starts_with("__MACOSX") || rel.starts_with(".git") {
            continue;
        }
        let out = dir.join(&rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut buf = vec![];
            std::io::copy(&mut f, &mut buf)?;
            std::fs::File::create(&out)?.write_all(&buf)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Strip a single common top-level folder (typical for GitHub/macOS zips).
pub fn common_prefix(names: &[String]) -> PathBuf {
    let firsts: std::collections::HashSet<&str> =
        names.iter().filter_map(|n| n.split('/').next()).collect();
    if firsts.len() == 1 && names.iter().any(|n| n.contains('/')) {
        PathBuf::from(firsts.into_iter().next().unwrap())
    } else {
        PathBuf::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repo() -> (tempfile::TempDir, Repo) {
        let t = tempfile::tempdir().unwrap();
        let r = Repo::new(t.path(), "p1");
        r.init(&InitWorkspace::default()).await.unwrap();
        (t, r)
    }

    /// The agent controls the workspace: planted symlinks must never make the
    /// runner read or overwrite files outside it.
    #[cfg(unix)]
    #[tokio::test]
    async fn queue_files_never_follow_symlinks() {
        let (t, r) = repo().await;
        let outside = t.path().join("host-secret.json");
        std::fs::write(&outside, r#"{"status":"done","summary":"stolen"}"#).unwrap();
        let q = r.dir.join(QUEUE_DIR);
        std::fs::create_dir_all(&q).unwrap();

        // File symlink: not read, and writing replaces the link, not the target.
        std::os::unix::fs::symlink(&outside, q.join("result.json")).unwrap();
        assert_eq!(r.queue_file("result.json").await.unwrap(), None);
        std::os::unix::fs::symlink(&outside, q.join("review.json")).unwrap();
        write_queue_file(&r.dir, "review.json", "").await.unwrap();
        assert!(std::fs::read_to_string(&outside)
            .unwrap()
            .contains("stolen"));
        assert!(!std::fs::symlink_metadata(q.join("review.json"))
            .unwrap()
            .file_type()
            .is_symlink());

        // Directory symlink: `.queue` → elsewhere.
        let elsewhere = t.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("result.json"), r#"{"status":"done"}"#).unwrap();
        std::fs::remove_dir_all(&q).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &q).unwrap();
        assert_eq!(r.queue_file("result.json").await.unwrap(), None);
        write_queue_file(&r.dir, "result.schema.json", "{}")
            .await
            .unwrap();
        assert!(!elsewhere.join("result.schema.json").exists());
        assert!(std::fs::symlink_metadata(&q).unwrap().file_type().is_dir());
        assert_eq!(
            std::fs::read_to_string(q.join("result.schema.json")).unwrap(),
            "{}"
        );

        // Oversized contract files are ignored.
        std::fs::write(
            q.join("result.json"),
            " ".repeat(MAX_QUEUE_FILE as usize + 1),
        )
        .unwrap();
        assert_eq!(r.queue_file("result.json").await.unwrap(), None);
    }

    #[tokio::test]
    async fn init_creates_repo_with_initial_commit_and_gitignore() {
        let (_t, r) = repo().await;
        assert!(r.exists());
        assert_eq!(r.log(10).await.unwrap()[0].subject, "Initial commit");
        assert!(r.dir.join(".gitignore").exists());
        // idempotent
        let h = r.head().await.unwrap();
        assert_eq!(r.init(&InitWorkspace::default()).await.unwrap(), h);
    }

    #[tokio::test]
    async fn commit_diff_and_queue_dir_is_ignored() {
        let (_t, r) = repo().await;
        let before = r.begin(None).await.unwrap().head;
        tokio::fs::write(r.dir.join("a.txt"), "hello\n")
            .await
            .unwrap();
        tokio::fs::write(r.dir.join(".queue/result.json"), "{}")
            .await
            .unwrap();
        let after = r.commit("task: a", "Alice", "a@x.ch").await.unwrap();
        assert_ne!(before, after);
        let (d, changed) = r.diff(&before, &after, 100_000).await.unwrap();
        assert!(changed);
        assert!(d.contains("+hello"));
        assert!(!d.contains(".queue"), "queue dir must never be committed");
        assert_eq!(r.log(1).await.unwrap()[0].author, "Alice");
        assert_eq!(
            r.queue_file("result.json").await.unwrap().as_deref(),
            Some("{}")
        );
        assert!(r.queue_file("../x").await.is_err());
    }

    #[tokio::test]
    async fn commit_without_changes_returns_head() {
        let (_t, r) = repo().await;
        let h = r.head().await.unwrap();
        assert_eq!(r.commit("noop", "A", "a@x").await.unwrap(), h);
    }

    #[tokio::test]
    async fn agent_commits_are_part_of_range_and_revert_works() {
        let (_t, r) = repo().await;
        let before = r.head().await.unwrap();
        // Agent commits itself, then leaves another change uncommitted.
        tokio::fs::write(r.dir.join("x.txt"), "1\n").await.unwrap();
        r.git(&["add", "-A"]).await.unwrap();
        r.git(&["commit", "-q", "-m", "agent commit"])
            .await
            .unwrap();
        tokio::fs::write(r.dir.join("y.txt"), "2\n").await.unwrap();
        let after = r.commit("task", "A", "a@x").await.unwrap();
        r.revert(&before, &after).await.unwrap();
        assert!(!r.dir.join("x.txt").exists());
        assert!(!r.dir.join("y.txt").exists());
    }

    #[tokio::test]
    async fn conflicting_revert_is_atomic() {
        let (_t, r) = repo().await;
        let b1 = r.head().await.unwrap();
        tokio::fs::write(r.dir.join("f.txt"), "a\n").await.unwrap();
        let a1 = r.commit("t1", "A", "a@x").await.unwrap();
        tokio::fs::write(r.dir.join("f.txt"), "b\n").await.unwrap();
        r.commit("t2", "A", "a@x").await.unwrap();
        assert!(r.revert(&b1, &a1).await.is_err());
        assert_eq!(
            tokio::fs::read_to_string(r.dir.join("f.txt"))
                .await
                .unwrap(),
            "b\n"
        );
        assert!(!r.is_dirty().await.unwrap(), "no conflict leftovers");
    }

    #[tokio::test]
    async fn begin_commits_stray_changes_as_wip() {
        let (_t, r) = repo().await;
        tokio::fs::write(r.dir.join("stray.txt"), "x")
            .await
            .unwrap();
        let b = r.begin(None).await.unwrap();
        assert_eq!(b.warnings.len(), 1);
        assert_eq!(
            r.log(1).await.unwrap()[0].subject,
            "WIP: uncommitted changes before task"
        );
    }

    #[tokio::test]
    async fn rejects_unsafe_paths_and_revisions() {
        let (_t, r) = repo().await;
        assert!(r.safe_path("../etc/passwd").is_err());
        assert!(r.safe_path("/etc/passwd").is_err());
        assert!(r.safe_path(".git/config").is_err());
        assert!(r.safe_path("src/main.rs").is_ok());
        assert!(r.diff("--output=/tmp/x", "HEAD", 10).await.is_err());
    }

    #[tokio::test]
    async fn clone_push_and_pull_against_local_remote() {
        let t = tempfile::tempdir().unwrap();
        let bare = t.path().join("remote.git");
        run_git(
            t.path(),
            &[],
            &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
            None,
        )
        .await
        .unwrap();
        let remote = GitRemote {
            url: bare.to_string_lossy().into(),
            branch: "main".into(),
            token: None,
        };

        let a = Repo::new(t.path(), "a");
        a.init(&InitWorkspace {
            remote: Some(remote.clone()),
            branch: None,
        })
        .await
        .unwrap();
        tokio::fs::write(a.dir.join("hello.txt"), "hi")
            .await
            .unwrap();
        a.commit("add hello", "A", "a@x").await.unwrap();
        a.push(&remote).await.unwrap();

        let b = Repo::new(t.path(), "b");
        b.init(&InitWorkspace {
            remote: Some(remote.clone()),
            branch: None,
        })
        .await
        .unwrap();
        assert!(b.dir.join("hello.txt").exists());

        tokio::fs::write(a.dir.join("second.txt"), "2")
            .await
            .unwrap();
        a.commit("second", "A", "a@x").await.unwrap();
        a.push(&remote).await.unwrap();
        let res = b.begin(Some(&remote)).await.unwrap();
        assert!(res.warnings.is_empty(), "{:?}", res.warnings);
        assert!(b.dir.join("second.txt").exists());
    }

    #[tokio::test]
    async fn failed_clone_cleans_up_and_redacts_token() {
        let t = tempfile::tempdir().unwrap();
        let r = Repo::new(t.path(), "c");
        let remote = GitRemote {
            url: t.path().join("missing.git").to_string_lossy().into(),
            branch: "main".into(),
            token: Some("s3cret".into()),
        };
        let err = r
            .init(&InitWorkspace {
                remote: Some(remote),
                branch: None,
            })
            .await
            .unwrap_err();
        assert!(!format!("{err:#}").contains("s3cret"));
        assert!(!r.dir.exists());
    }

    #[tokio::test]
    async fn zip_roundtrip_strips_common_prefix() {
        let (_t, r) = repo().await;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o: zip::write::SimpleFileOptions = Default::default();
            z.start_file("proj-main/src/a.rs", o).unwrap();
            z.write_all(b"fn a() {}").unwrap();
            z.start_file("proj-main/README.md", o).unwrap();
            z.write_all(b"# hi").unwrap();
            z.finish().unwrap();
        }
        let (n, _) = r.import_zip(buf.into_inner(), "A", "a@x").await.unwrap();
        assert_eq!(n, 2);
        assert!(r.dir.join("src/a.rs").exists());
        let files = r.files().await.unwrap();
        assert!(files.contains(&"README.md".to_string()));
        let z = r.zip().await.unwrap();
        assert!(zip::ZipArchive::new(std::io::Cursor::new(z)).unwrap().len() >= 3);
    }

    #[test]
    fn common_prefix_detection() {
        assert_eq!(
            common_prefix(&["a/x".into(), "a/y".into(), "a".into()]),
            PathBuf::from("a")
        );
        assert_eq!(common_prefix(&["a/x".into(), "b/y".into()]), PathBuf::new());
        assert_eq!(common_prefix(&["README.md".into()]), PathBuf::new());
    }
}
