//! Project workspaces as real git repositories.
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use queued_common::{
    gate::{self, FindingKind, GateFinding},
    BeginResult, Commit, CommitOutcome, GitRemote, InitWorkspace,
};
use tokio::process::Command;

pub const QUEUE_DIR: &str = ".queue";

/// Set when a discard could not prune right away (inside `.git`, where the
/// agent's own `git` never looks).
const PRUNE_MARKER: &str = ".git/queue-prune-pending";

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

/// Command-line configuration of every git call (WP03). It overrides the
/// repository's `.git/config`, which the agent controls: no hooks (a
/// `post-commit`/`post-merge` hook would run on the host, outside the
/// sandbox), no fsmonitor command, no credential helper, no `ext::`
/// transport, no signing program, no automatic gc/maintenance.
pub const HARDEN: [&str; 26] = [
    "-c",
    "safe.directory=*",
    "-c",
    "core.quotepath=off",
    "-c",
    "user.name=Queue",
    "-c",
    "user.email=queue@localhost",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "credential.helper=",
    "-c",
    "protocol.ext.allow=never",
    "-c",
    "commit.gpgSign=false",
    "-c",
    "tag.gpgSign=false",
    "-c",
    "core.sshCommand=ssh",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
];

/// Diff options for scanning: every file as text (a `binary` attribute in
/// `.gitattributes` must not hide a secret), no external diff drivers or
/// textconv programs, no rename detection, zero context.
const SCAN_DIFF: [&str; 8] = [
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--no-renames",
    "-a",
    "-U0",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

/// Larger diffs are not scanned completely: the caller treats that as a
/// finding (fail-closed).
pub const MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;

async fn run_git(
    cwd: &Path,
    pre: &[String],
    args: &[&str],
    remote: Option<&GitRemote>,
) -> Result<String> {
    run_git_env(cwd, pre, args, remote, &[]).await
}

async fn run_git_env(
    cwd: &Path,
    pre: &[String],
    args: &[&str],
    remote: Option<&GitRemote>,
    envs: &[(&str, &str)],
) -> Result<String> {
    let out = Command::new("git")
        .args(HARDEN)
        .args(pre)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(envs.iter().copied())
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
        self.harden().await?;
        run_git(&self.dir, &[], args, None).await
    }

    /// Like [`Repo::git`] with extra environment (temporary index).
    async fn git_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<String> {
        self.harden().await?;
        run_git_env(&self.dir, &[], args, None, envs).await
    }

    /// Makes the agent-controlled `.git` safe for host-side git calls:
    /// `.git` and its top-level entries must be real directories/files
    /// (a symlinked `.git`/`objects`/`refs` would make the runner work on
    /// another workspace), alternates/worktree links are removed and
    /// `.git/config` is rewritten from an allowlist (filters, diff/merge
    /// drivers, `url.*.insteadOf`, `http.*`, `include*`, `core.*` commands …
    /// are dropped). Cheap when nothing changed (content cache).
    pub async fn harden(&self) -> Result<()> {
        let git_dir = self.dir.join(".git");
        let meta = tokio::fs::symlink_metadata(&git_dir)
            .await
            .context("workspace has no .git")?;
        if !meta.file_type().is_dir() {
            bail!("workspace .git is not a directory");
        }
        for sub in ["objects", "refs", "info", "hooks", "logs"] {
            if let Ok(m) = tokio::fs::symlink_metadata(git_dir.join(sub)).await {
                if m.file_type().is_symlink() {
                    tokio::fs::remove_file(git_dir.join(sub)).await?;
                    if sub == "objects" || sub == "refs" {
                        bail!("workspace .git/{sub} was a symlink (removed)");
                    }
                }
            }
        }
        for f in [
            "objects/info/alternates",
            "objects/info/http-alternates",
            "commondir",
            "config.worktree",
            "info/grafts",
        ] {
            let p = git_dir.join(f);
            if tokio::fs::symlink_metadata(&p).await.is_ok() {
                let _ = tokio::fs::remove_file(&p).await;
            }
        }
        let cfg = git_dir.join("config");
        let is_link = tokio::fs::symlink_metadata(&cfg)
            .await
            .is_ok_and(|m| !m.file_type().is_file());
        let current = if is_link {
            None
        } else {
            tokio::fs::read(&cfg).await.ok()
        };
        if let Some(c) = &current {
            if config_cache().lock().expect("cache").get(&cfg) == Some(c) {
                return Ok(());
            }
        }
        // Read the file outside of any repository: repository discovery would
        // parse `.git/config` itself, including its `include` directives.
        let parent = self.dir.parent().context("no parent")?;
        let cfg_s = cfg.to_string_lossy().to_string();
        let parent_s = parent.to_string_lossy().to_string();
        let listed = match &current {
            Some(_) => run_git_env(
                parent,
                &[],
                &[
                    "config",
                    "--file",
                    &cfg_s,
                    "--no-includes",
                    "--null",
                    "--list",
                ],
                None,
                &[
                    ("GIT_CEILING_DIRECTORIES", &parent_s),
                    ("GIT_DIR", "/nonexistent"),
                ],
            )
            .await
            .unwrap_or_default(),
            None => String::new(),
        };
        let canonical = canonical_config(&listed);
        if current.as_deref() != Some(canonical.as_bytes()) {
            if is_link {
                let _ = tokio::fs::remove_file(&cfg).await;
            }
            let tmp = git_dir.join("config.queue-tmp");
            let _ = tokio::fs::remove_file(&tmp).await;
            tokio::fs::write(&tmp, &canonical).await?;
            tokio::fs::rename(&tmp, &cfg).await?;
        }
        config_cache()
            .lock()
            .expect("cache")
            .insert(cfg, canonical.into_bytes());
        Ok(())
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
                    run_git(&self.dir, &[], &["init", "-q", "-b", &branch], None).await?;
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

    /// Prepares a task: commits stray changes (or quarantines them when
    /// they contain an unapproved secret), optionally pulls, resets `.queue/`.
    pub async fn begin(
        &self,
        remote: Option<&GitRemote>,
        key: &[u8],
        allow: &[String],
    ) -> Result<BeginResult> {
        let mut warnings = vec![];
        if tokio::fs::symlink_metadata(self.dir.join(PRUNE_MARKER))
            .await
            .is_ok()
        {
            self.prune().await?;
        }
        if self.is_dirty().await? {
            self.git(&["add", "-A"]).await?;
            let findings = self.staged_secrets(key, allow).await?;
            if findings.is_empty() {
                self.git(&[
                    "commit",
                    "-q",
                    "--no-verify",
                    "-m",
                    "WIP: uncommitted changes before task",
                ])
                .await?;
                warnings.push("uncommitted changes were committed as WIP".into());
            } else {
                let r = self.park("quarantine", &now_id(), None).await?;
                warnings.push(format!(
                    "Reste eines früheren Laufs enthielten mögliche Geheimnisse und wurden nicht committet (zurückgestellt unter {r}): {}",
                    describe(&findings)
                ));
            }
        }
        if let Some(r) = remote.filter(|r| !r.url.is_empty()) {
            let branch = if r.branch.is_empty() {
                "main"
            } else {
                &r.branch
            };
            // Pull from the URL of the app, never from `origin`: the agent
            // controls `.git/config` and could point it (with our token
            // header) at another host.
            if let Err(e) = run_git(
                &self.dir,
                &auth_args(r),
                &[
                    "pull",
                    "--ff-only",
                    "--quiet",
                    "--no-rebase",
                    "--",
                    &r.url,
                    branch,
                ],
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

    /// Commits everything the agent left uncommitted unless it contains a
    /// secret outside `allow` (then nothing is committed and the index is
    /// reset; the working tree stays as it is).
    pub async fn commit_checked(
        &self,
        message: &str,
        author_name: &str,
        author_email: &str,
        key: &[u8],
        allow: &[String],
    ) -> Result<CommitOutcome> {
        self.commit_scanned(message, author_name, author_email, key, allow, false)
            .await
    }

    /// [`Repo::commit_checked`]; `allow_large`: a human approved changes too
    /// large for a complete scan.
    pub async fn commit_scanned(
        &self,
        message: &str,
        author_name: &str,
        author_email: &str,
        key: &[u8],
        allow: &[String],
        allow_large: bool,
    ) -> Result<CommitOutcome> {
        if self.is_dirty().await? {
            self.git(&["add", "-A"]).await?;
            let mut blocked = self.staged_secrets(key, allow).await?;
            if allow_large {
                blocked.retain(|f| f.kind != FindingKind::TooLarge);
            }
            if !blocked.is_empty() {
                self.git(&["reset", "-q"]).await?;
                return Ok(CommitOutcome {
                    sha: self.head().await?,
                    blocked,
                    conflict: false,
                });
            }
        }
        Ok(CommitOutcome {
            sha: self.commit(message, author_name, author_email).await?,
            ..Default::default()
        })
    }

    /// Commits everything left uncommitted without a scan (human imports,
    /// tests); returns HEAD.
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
            .git(&[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--stat",
                "--patch",
                from,
                to,
            ])
            .await?;
        if d.len() > max_bytes {
            let stat = self
                .git(&["diff", "--no-color", "--no-ext-diff", "--stat", from, to])
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
        self.harden().await?;
        let out = Command::new("git")
            .args(HARDEN)
            .args(["archive", "--format=zip", "HEAD"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
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

    /// Pushes `sha` (default HEAD) to the branch of `r`, by URL (never via
    /// the agent-controlled `origin`) and never forced. Returns the findings
    /// instead of pushing when the target is the remote's default branch
    /// without `allow_protected`, or the pushed commits contain a secret
    /// outside `allow` (unless `allow_secrets`).
    pub async fn push_checked(&self, r: &GitRemote, opt: &PushOptions<'_>) -> Result<PushOutcome> {
        let branch = if r.branch.is_empty() {
            "main"
        } else {
            &r.branch
        };
        check_branch(branch)?;
        let sha = match opt.sha {
            Some(s) => {
                check_rev(s)?;
                self.git(&["rev-parse", "--verify", "-q", &format!("{s}^{{commit}}")])
                    .await
                    .context("commit to push not found")?
                    .trim()
                    .to_string()
            }
            None => self.head().await?,
        };
        let (default, remote_sha) = self.remote_heads(r, branch).await;
        let mut held = vec![];
        let protected = match &default {
            Some(d) => d == branch,
            None => matches!(branch, "main" | "master"),
        };
        if protected && !opt.allow_protected {
            held.push(GateFinding {
                kind: FindingKind::PushProtected,
                path: String::new(),
                line: None,
                detail: format!(
                    "Ziel ist der Standard-Branch „{branch}“{}",
                    if default.is_none() {
                        " (Standard-Branch des Remotes nicht ermittelbar)"
                    } else {
                        ""
                    }
                ),
                fingerprint: None,
            });
        }
        if !opt.allow_secrets {
            let known = match &remote_sha {
                Some(rs) if check_rev(rs).is_ok() => self
                    .git(&["cat-file", "-e", &format!("{rs}^{{commit}}")])
                    .await
                    .is_ok(),
                _ => false,
            };
            let range = match (&remote_sha, known) {
                (Some(rs), true) => format!("{rs}..{sha}"),
                _ => sha.clone(),
            };
            let mut args = vec!["log", "-p", "--format=commit %H"];
            args.extend(SCAN_DIFF);
            args.push(&range);
            held.extend(self.scan_output(&args, opt.key, opt.allow).await?);
        }
        if !held.is_empty() {
            return Ok(PushOutcome {
                output: String::new(),
                held,
            });
        }
        let target = format!("{sha}:refs/heads/{branch}");
        let out = run_git(
            &self.dir,
            &auth_args(r),
            &["push", "--porcelain", "--", &r.url, &target],
            Some(r),
        )
        .await?;
        Ok(PushOutcome {
            output: redact(&out, Some(r)),
            held: vec![],
        })
    }

    /// Default branch of the remote (`ls-remote --symref`) and the current
    /// commit of `branch` there (`None` = unknown / not yet created).
    async fn remote_heads(&self, r: &GitRemote, branch: &str) -> (Option<String>, Option<String>) {
        let head_ref = format!("refs/heads/{branch}");
        let Ok(out) = run_git(
            &self.dir,
            &auth_args(r),
            &["ls-remote", "--symref", "--", &r.url, "HEAD", &head_ref],
            Some(r),
        )
        .await
        else {
            return (None, None);
        };
        parse_ls_remote(&out, &head_ref)
    }

    /// Secrets in the staged changes (against HEAD).
    async fn staged_secrets(&self, key: &[u8], allow: &[String]) -> Result<Vec<GateFinding>> {
        let mut args = vec!["diff", "--cached"];
        args.extend(SCAN_DIFF);
        self.scan_output(&args, key, allow).await
    }

    /// Runs a diff/log command and scans its added lines for secrets.
    async fn scan_output(
        &self,
        args: &[&str],
        key: &[u8],
        allow: &[String],
    ) -> Result<Vec<GateFinding>> {
        let out = self.git(args).await?;
        if out.len() > MAX_SCAN_BYTES {
            return Ok(vec![too_large(out.len())]);
        }
        Ok(gate::secret_findings(&gate::parse_diff(&out), key, allow))
    }

    /// Tree of the current working tree (including untracked, not ignored
    /// files) via a temporary index; the real index stays untouched.
    async fn worktree_tree(&self) -> Result<String> {
        let idx = self
            .dir
            .join(".git")
            .join(format!("queue-index-{}", now_id()));
        let idx_s = idx.to_string_lossy().to_string();
        let env = [("GIT_INDEX_FILE", idx_s.as_str())];
        let res = async {
            self.git_env(&["read-tree", "HEAD"], &env).await?;
            self.git_env(&["add", "-A"], &env).await?;
            Ok::<String, anyhow::Error>(
                self.git_env(&["write-tree"], &env)
                    .await?
                    .trim()
                    .to_string(),
            )
        }
        .await;
        let _ = tokio::fs::remove_file(&idx).await;
        res
    }

    /// Gate check of the working tree (WP03): secrets against HEAD,
    /// protected paths and weakened tests against the task base.
    pub async fn inspect(
        &self,
        req: &gate::InspectRequest,
        key: &[u8],
    ) -> Result<gate::InspectResult> {
        check_rev(&req.base)?;
        let tree = self.worktree_tree().await?;
        let names = self
            .git(&["diff", "--name-only", "--no-renames", "HEAD", &tree])
            .await?;
        let changed_files = names.lines().filter(|l| !l.trim().is_empty()).count();
        let mut findings = vec![];
        if changed_files > 0 {
            let mut args = vec!["diff"];
            args.extend(SCAN_DIFF);
            args.extend(["HEAD", tree.as_str()]);
            findings.extend(self.scan_output(&args, key, &req.allow).await?);
        }
        // Commits the agent made itself during the run were not scanned.
        let head = self.head().await?;
        if !head.starts_with(&req.base) {
            let range = format!("{}..{head}", req.base);
            let mut args = vec!["log", "-p", "--format=commit %H"];
            args.extend(SCAN_DIFF);
            args.push(&range);
            for f in self.scan_output(&args, key, &req.allow).await? {
                if !findings.contains(&f) {
                    findings.push(f);
                }
            }
        }
        let mut args = vec!["diff"];
        args.extend(SCAN_DIFF);
        args.extend([req.base.as_str(), tree.as_str()]);
        let out = self.git(&args).await?;
        if out.len() > MAX_SCAN_BYTES {
            if !findings.iter().any(|f| f.kind == FindingKind::TooLarge) {
                findings.push(too_large(out.len()));
            }
        } else {
            let files = gate::parse_diff(&out);
            findings.extend(gate::change_findings(
                &files,
                &req.protected,
                req.test_guard,
            ));
            findings.extend(gate::watched_findings(&files, &req.watch));
            if req.test_guard {
                for f in files.iter().filter(|f| {
                    !f.added && !f.deleted && f.path.rsplit('/').next() == Some("package.json")
                }) {
                    let old = self.show(&req.base, &f.path).await.unwrap_or_default();
                    let new = self.show(&tree, &f.path).await.unwrap_or_default();
                    if gate::package_scripts_changed(&old, &new) {
                        findings.push(GateFinding {
                            kind: FindingKind::VerifierConfig,
                            path: f.path.clone(),
                            line: None,
                            detail: "Skripte („scripts“) geändert".into(),
                            fingerprint: None,
                        });
                    }
                }
            }
        }
        Ok(gate::InspectResult {
            findings,
            changed_files,
        })
    }

    async fn show(&self, rev: &str, path: &str) -> Result<String> {
        self.git(&["cat-file", "-p", &format!("{rev}:{path}")])
            .await
    }

    /// Saves the working tree as a commit under `refs/queue/<kind>/<id>`
    /// with parent `base` (default HEAD) and resets branch and working tree
    /// to `base`. Returns the ref.
    async fn park(&self, kind: &str, id: &str, base: Option<&str>) -> Result<String> {
        let tree = self.worktree_tree().await?;
        let base = match base {
            Some(b) => {
                check_rev(b)?;
                self.git(&["rev-parse", "--verify", "-q", &format!("{b}^{{commit}}")])
                    .await
                    .context("unknown base commit")?
                    .trim()
                    .to_string()
            }
            None => self.head().await?,
        };
        let msg = format!("queue {kind} {id}");
        let c = self
            .git(&["commit-tree", &tree, "-p", &base, "-m", &msg])
            .await?
            .trim()
            .to_string();
        let r = format!("refs/queue/{kind}/{id}");
        self.git(&["update-ref", &r, &c]).await?;
        self.git(&["reset", "-q", "--hard", &base]).await?;
        self.git(&["clean", "-fdq"]).await?;
        Ok(r)
    }

    /// Parks everything the task changed since `base` – uncommitted changes
    /// and commits the agent made itself – outside the branch (gate hold).
    pub async fn hold(&self, task: &str, base: &str) -> Result<String> {
        anyhow::ensure!(gate::valid_task_ref(task), "invalid task id");
        self.park("held", task, Some(base)).await
    }

    fn held_ref(task: &str) -> Result<String> {
        anyhow::ensure!(gate::valid_task_ref(task), "invalid task id");
        Ok(format!("refs/queue/held/{task}"))
    }

    /// Applies the held changes on top of HEAD (3-way) and commits them.
    pub async fn release(&self, req: &gate::ReleaseRequest, key: &[u8]) -> Result<CommitOutcome> {
        let r = Self::held_ref(&req.task)?;
        let held = self
            .git(&["rev-parse", "--verify", "-q", &r])
            .await
            .context("no held changes for this task")?
            .trim()
            .to_string();
        if self.is_dirty().await? {
            bail!("working tree is not clean");
        }
        if self
            .git(&["cherry-pick", "--no-commit", &held])
            .await
            .is_err()
        {
            let _ = self.git(&["cherry-pick", "--abort"]).await;
            let _ = self.git(&["reset", "-q", "--hard", "HEAD"]).await;
            let _ = self.git(&["clean", "-fdq"]).await;
            return Ok(CommitOutcome {
                sha: self.head().await?,
                blocked: vec![],
                conflict: true,
            });
        }
        let out = self
            .commit_checked(
                &req.message,
                &req.author_name,
                &req.author_email,
                key,
                &req.allow,
            )
            .await?;
        if out.blocked.is_empty() {
            self.drop_ref(&r).await?;
        } else {
            self.git(&["reset", "-q", "--hard", "HEAD"]).await?;
            self.git(&["clean", "-fdq"]).await?;
        }
        Ok(out)
    }

    /// Discards held changes and prunes their objects. With `busy` (a
    /// process runs in the workspace and may be writing objects that are not
    /// referenced yet) the prune waits for the next `begin`.
    pub async fn discard(&self, task: &str, busy: bool) -> Result<()> {
        let r = Self::held_ref(task)?;
        if self.git(&["rev-parse", "--verify", "-q", &r]).await.is_ok() {
            self.git(&["update-ref", "-d", &r]).await?;
        }
        if busy {
            tokio::fs::write(self.dir.join(PRUNE_MARKER), b"").await?;
            return Ok(());
        }
        self.prune().await
    }

    async fn drop_ref(&self, r: &str) -> Result<()> {
        if self.git(&["rev-parse", "--verify", "-q", r]).await.is_ok() {
            self.git(&["update-ref", "-d", r]).await?;
        }
        self.prune().await
    }

    /// Removes unreachable objects at once (discarded held secrets).
    async fn prune(&self) -> Result<()> {
        let _ = self
            .git(&["reflog", "expire", "--expire-unreachable=now", "--all"])
            .await;
        let _ = self.git(&["prune", "--expire=now"]).await;
        let _ = tokio::fs::remove_file(self.dir.join(PRUNE_MARKER)).await;
        Ok(())
    }

    /// Diff of the held changes against the commit they were based on.
    pub async fn held_diff(&self, task: &str, max_bytes: usize) -> Result<(String, bool)> {
        let r = Self::held_ref(task)?;
        let held = self
            .git(&["rev-parse", "--verify", "-q", &r])
            .await
            .context("no held changes for this task")?
            .trim()
            .to_string();
        let parent = self
            .git(&["rev-parse", &format!("{held}^")])
            .await?
            .trim()
            .to_string();
        self.diff(&parent, &held, max_bytes).await
    }

    /// Files under `paths` changed since the approved baseline
    /// (first use: the baseline is set to HEAD).
    pub async fn baseline_check(&self, req: &gate::BaselineCheck) -> Result<gate::BaselineResult> {
        anyhow::ensure!(gate::valid_baseline(&req.name), "invalid baseline");
        let r = format!("refs/queue/baseline/{}", req.name);
        let head = self.head().await?;
        let base = match self.git(&["rev-parse", "--verify", "-q", &r]).await {
            Ok(b) => b.trim().to_string(),
            Err(_) => {
                self.git(&["update-ref", &r, &head]).await?;
                head.clone()
            }
        };
        let names = self
            .git(&["diff", "--name-only", "--no-renames", &base, &head])
            .await?;
        let changed = names
            .lines()
            .filter(|p| !p.is_empty() && queued_common::glob::matches_any(&req.paths, p))
            .map(String::from)
            .collect();
        Ok(gate::BaselineResult {
            baseline: base,
            head,
            changed,
        })
    }

    pub async fn baseline_approve(&self, req: &gate::BaselineApprove) -> Result<()> {
        anyhow::ensure!(gate::valid_baseline(&req.name), "invalid baseline");
        check_rev(&req.sha)?;
        let c = self
            .git(&[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{}^{{commit}}", req.sha),
            ])
            .await
            .context("unknown commit")?;
        self.git(&[
            "update-ref",
            &format!("refs/queue/baseline/{}", req.name),
            c.trim(),
        ])
        .await?;
        Ok(())
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

/// Options of [`Repo::push_checked`].
pub struct PushOptions<'a> {
    pub sha: Option<&'a str>,
    pub allow_protected: bool,
    pub allow: &'a [String],
    pub allow_secrets: bool,
    pub key: &'a [u8],
}

pub struct PushOutcome {
    pub output: String,
    pub held: Vec<GateFinding>,
}

fn too_large(bytes: usize) -> GateFinding {
    GateFinding {
        kind: FindingKind::TooLarge,
        path: String::new(),
        line: None,
        detail: format!(
            "Änderung zu groß für die vollständige Prüfung ({} MiB)",
            bytes / (1024 * 1024)
        ),
        fingerprint: None,
    }
}

/// Short masked summary of findings for messages.
pub fn describe(findings: &[GateFinding]) -> String {
    findings
        .iter()
        .take(5)
        .map(|f| match f.line {
            Some(l) => format!("{} ({}:{l})", f.detail, f.path),
            None if f.path.is_empty() => f.detail.clone(),
            None => format!("{} ({})", f.detail, f.path),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn now_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "{t}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn config_cache() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, Vec<u8>>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Keys of `.git/config` that survive [`Repo::harden`]. Everything else can
/// run programs or redirect network traffic (filters, drivers, `core.*`
/// commands, `url.*`, `http.*`, `include*`, `remote.*.uploadpack` …).
fn allowed_config_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "core.repositoryformatversion"
            | "core.filemode"
            | "core.logallrefupdates"
            | "core.ignorecase"
            | "core.precomposeunicode"
            | "core.symlinks"
            | "core.autocrlf"
            | "extensions.objectformat"
            | "extensions.refstorage"
            | "remote.origin.url"
            | "remote.origin.fetch"
    ) || (lower.starts_with("branch.") && (lower.ends_with(".remote") || lower.ends_with(".merge")))
}

fn quote_value(v: &str) -> String {
    let mut s = String::from("\"");
    for c in v.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            c if c.is_control() => {}
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// Canonical `.git/config` from `git config --null --list` output.
fn canonical_config(listed: &str) -> String {
    let mut sections: Vec<(String, Vec<(String, String)>)> = vec![];
    let mut push = |section: String, name: String, value: String| match sections
        .iter_mut()
        .find(|(s, _)| *s == section)
    {
        Some((_, v)) => v.push((name, value)),
        None => sections.push((section, vec![(name, value)])),
    };
    push(
        "[core]".into(),
        "repositoryformatversion".into(),
        String::new(),
    );
    push("[core]".into(), "bare".into(), "false".into());
    let mut version = "0".to_string();
    for entry in listed.split('\0').filter(|e| !e.is_empty()) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        if !allowed_config_key(key) {
            continue;
        }
        let Some((sec, name)) = key.split_once('.') else {
            continue;
        };
        let (section, name) = match name.rsplit_once('.') {
            Some((sub, n)) => (
                format!(
                    "[{sec} \"{}\"]",
                    sub.replace('\\', "\\\\").replace('"', "\\\"")
                ),
                n.to_string(),
            ),
            None => (format!("[{sec}]"), name.to_string()),
        };
        if key.eq_ignore_ascii_case("core.repositoryformatversion") {
            if value == "1" {
                version = "1".into();
            }
            continue;
        }
        push(section, name, value.to_string());
    }
    let mut out = String::new();
    for (section, entries) in &sections {
        out.push_str(section);
        out.push('\n');
        for (n, v) in entries {
            let v = if n == "repositoryformatversion" {
                &version
            } else {
                v
            };
            out.push_str(&format!("\t{n} = {}\n", quote_value(v)));
        }
    }
    out
}

fn parse_ls_remote(out: &str, head_ref: &str) -> (Option<String>, Option<String>) {
    let mut default = None;
    let mut sha = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
            if let Some((b, name)) = rest.split_once('\t') {
                if name == "HEAD" {
                    default = Some(b.to_string());
                }
            }
        } else if let Some((s, name)) = line.split_once('\t') {
            if name == head_ref {
                sha = Some(s.to_string());
            }
        }
    }
    (default, sha)
}

/// Branch names from the app (no options, no ref tricks).
fn check_branch(b: &str) -> Result<()> {
    if b.is_empty()
        || b.len() > 200
        || b.starts_with('-')
        || b.contains("..")
        || b.contains(':')
        || b.chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^?*[\\".contains(c))
    {
        bail!("invalid branch name");
    }
    Ok(())
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
    use queued_common::secrets::testdata;

    const KEY: &[u8] = b"test-key";

    async fn push_all(r: &Repo, remote: &GitRemote) {
        let o = r
            .push_checked(
                remote,
                &PushOptions {
                    sha: None,
                    allow_protected: true,
                    allow: &[],
                    allow_secrets: false,
                    key: KEY,
                },
            )
            .await
            .unwrap();
        assert!(o.held.is_empty(), "{:?}", o.held);
    }

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
        let before = r.begin(None, KEY, &[]).await.unwrap().head;
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
        let b = r.begin(None, KEY, &[]).await.unwrap();
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
        push_all(&a, &remote).await;

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
        push_all(&a, &remote).await;
        let res = b.begin(Some(&remote), KEY, &[]).await.unwrap();
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

    async fn bare_remote(t: &std::path::Path, default_branch: &str) -> GitRemote {
        let bare = t.join("remote.git");
        run_git(
            t,
            &[],
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                default_branch,
                bare.to_str().unwrap(),
            ],
            None,
        )
        .await
        .unwrap();
        GitRemote {
            url: bare.to_string_lossy().into(),
            branch: default_branch.into(),
            token: None,
        }
    }

    /// The agent controls `.git`: nothing it configures there may run on
    /// the host when the runner calls git (WP03).
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_git_config_and_hooks_never_run_on_the_host() {
        let (t, r) = repo().await;
        let marker = t.path().join("pwned");
        let m = marker.display().to_string();
        let cmd = format!("sh -c 'echo x >> {m}'");
        let hook = r.dir.join(".git/hooks/post-commit");
        std::fs::write(&hook, format!("#!/bin/sh\necho hook >> {m}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = r.dir.join(".git/config");
        let mut c = std::fs::read_to_string(&cfg).unwrap();
        c.push_str(&format!(
            "[core]\n\tfsmonitor = \"{cmd}\"\n\thooksPath = .git/hooks\n\tpager = {cmd}\n[filter \"x\"]\n\tclean = {cmd}\n\tsmudge = {cmd}\n\trequired = true\n[diff \"x\"]\n\ttextconv = {cmd}\n[diff]\n\texternal = {cmd}\n[commit]\n\tgpgSign = true\n[gpg]\n\tprogram = {cmd}\n[url \"https://evil.invalid/\"]\n\tinsteadOf = https://github.com/\n[include]\n\tpath = /etc/passwd\n[branch \"main\"]\n\tremote = origin\n"
        ));
        std::fs::write(&cfg, c).unwrap();
        std::fs::write(r.dir.join(".gitattributes"), "*.txt filter=x diff=x\n").unwrap();
        std::fs::create_dir_all(r.dir.join(".git/objects/info")).unwrap();
        std::fs::write(
            r.dir.join(".git/objects/info/alternates"),
            "/elsewhere/objects\n",
        )
        .unwrap();
        std::fs::write(r.dir.join("a.txt"), "hello\n").unwrap();

        let before = r.head().await.unwrap();
        let out = r.commit_checked("t", "A", "a@x", KEY, &[]).await.unwrap();
        assert!(out.blocked.is_empty());
        r.diff(&before, &out.sha, 100_000).await.unwrap();
        r.files().await.unwrap();
        r.zip().await.unwrap();
        r.begin(None, KEY, &[]).await.unwrap();
        assert!(
            !marker.exists(),
            "agent-controlled git config ran on the host"
        );

        let cleaned = std::fs::read_to_string(&cfg).unwrap();
        for bad in [
            "fsmonitor",
            "filter",
            "textconv",
            "external",
            "gpg",
            "insteadOf",
            "include",
            "hooksPath",
            "pager",
        ] {
            assert!(!cleaned.contains(bad), "{bad} survived:\n{cleaned}");
        }
        assert!(cleaned.contains("[branch \"main\"]"), "{cleaned}");
        assert!(!r.dir.join(".git/objects/info/alternates").exists());
        // The commit contains the file (no clean filter applied).
        let shown = r.show(&out.sha, "a.txt").await.unwrap();
        assert_eq!(shown, "hello\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_git_dirs_are_refused() {
        let (t, r) = repo().await;
        let other = Repo::new(t.path(), "other");
        other.init(&InitWorkspace::default()).await.unwrap();
        // `.git/objects` → another workspace.
        let objects = r.dir.join(".git/objects");
        std::fs::rename(&objects, t.path().join("moved-objects")).unwrap();
        std::os::unix::fs::symlink(other.dir.join(".git/objects"), &objects).unwrap();
        let e = r.head().await.unwrap_err();
        assert!(format!("{e:#}").contains("symlink"), "{e:#}");
        assert!(!std::fs::symlink_metadata(&objects).is_ok_and(|m| m.file_type().is_symlink()));
        // `.git` itself → another workspace.
        let (t2, r2) = repo().await;
        let o2 = Repo::new(t2.path(), "o2");
        o2.init(&InitWorkspace::default()).await.unwrap();
        std::fs::remove_dir_all(r2.dir.join(".git")).unwrap();
        std::os::unix::fs::symlink(o2.dir.join(".git"), r2.dir.join(".git")).unwrap();
        assert!(r2.commit_checked("x", "A", "a@x", KEY, &[]).await.is_err());
    }

    #[tokio::test]
    async fn secrets_block_commits_unless_approved() {
        let (_t, r) = repo().await;
        let token = testdata::github(21);
        std::fs::write(r.dir.join("cfg.py"), format!("TOKEN = \"{token}\"\n")).unwrap();
        let head = r.head().await.unwrap();
        let out = r.commit_checked("t", "A", "a@x", KEY, &[]).await.unwrap();
        assert_eq!(out.sha, head, "nothing committed");
        assert_eq!(out.blocked.len(), 1);
        let f = &out.blocked[0];
        assert_eq!(
            (f.kind, f.path.as_str(), f.line),
            (FindingKind::Secret, "cfg.py", Some(1))
        );
        assert!(!serde_json::to_string(&out).unwrap().contains(&token));
        assert!(r.dir.join("cfg.py").exists(), "working tree untouched");
        assert!(r
            .git(&["diff", "--cached", "--name-only"])
            .await
            .unwrap()
            .trim()
            .is_empty());
        // Approved fingerprint: committed.
        let fp = f.fingerprint.clone().unwrap();
        let ok = r.commit_checked("t", "A", "a@x", KEY, &[fp]).await.unwrap();
        assert!(ok.blocked.is_empty());
        assert_ne!(ok.sha, head);
        // A `binary` attribute does not hide a secret from the scanner.
        std::fs::write(r.dir.join(".gitattributes"), "*.py binary\n").unwrap();
        std::fs::write(
            r.dir.join("hidden.py"),
            format!("K = \"{}\"\n", testdata::aws(3)),
        )
        .unwrap();
        let out = r.commit_checked("t", "A", "a@x", KEY, &[]).await.unwrap();
        assert_eq!(out.blocked.len(), 1, "{:?}", out.blocked);
    }

    #[tokio::test]
    async fn begin_quarantines_leftovers_with_secrets() {
        let (_t, r) = repo().await;
        let head = r.head().await.unwrap();
        std::fs::write(
            r.dir.join(".env.local"),
            format!("X={}\n", testdata::anthropic(1)),
        )
        .unwrap();
        let b = r.begin(None, KEY, &[]).await.unwrap();
        assert_eq!(b.head, head, "no WIP commit");
        assert_eq!(b.warnings.len(), 1);
        assert!(
            b.warnings[0].contains("refs/queue/quarantine/"),
            "{}",
            b.warnings[0]
        );
        assert!(b.warnings[0].contains("Anthropic-API-Schlüssel"));
        assert!(!b.warnings[0].contains(&testdata::anthropic(1)));
        assert!(!r.dir.join(".env.local").exists());
        assert!(!r.is_dirty().await.unwrap());
    }

    #[tokio::test]
    async fn inspect_reports_secrets_protected_paths_and_tests() {
        let (_t, r) = repo().await;
        std::fs::create_dir_all(r.dir.join("tests")).unwrap();
        std::fs::write(
            r.dir.join("tests/test_a.py"),
            "def test_a():\n    assert f() == 1\n",
        )
        .unwrap();
        std::fs::write(r.dir.join("package.json"), r#"{"scripts":{"test":"jest"}}"#).unwrap();
        let base = r.commit("seed", "A", "a@x").await.unwrap();
        // Agent: weakens the test, changes the test script, adds a secret.
        std::fs::write(
            r.dir.join("tests/test_a.py"),
            "import pytest\n@pytest.mark.skip\ndef test_a():\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            r.dir.join("package.json"),
            r#"{"scripts":{"test":"exit 0"}}"#,
        )
        .unwrap();
        std::fs::write(
            r.dir.join("new.txt"),
            format!("token: {}\n", testdata::github(8)),
        )
        .unwrap();
        std::fs::write(
            r.dir.join("tests/test_new.py"),
            "def test_b():\n    assert True\n",
        )
        .unwrap();
        let req = gate::InspectRequest {
            base: base.clone(),
            protected: gate::DEFAULT_PROTECTED
                .iter()
                .map(|s| s.to_string())
                .collect(),
            test_guard: true,
            allow: vec![],
            watch: vec![".mcp.json".into()],
        };
        let res = r.inspect(&req, KEY).await.unwrap();
        assert_eq!(res.changed_files, 4);
        let mut kinds: Vec<_> = res
            .findings
            .iter()
            .map(|f| (f.kind, f.path.clone()))
            .collect();
        kinds.sort_by_key(|(k, p)| (format!("{k:?}"), p.clone()));
        assert_eq!(
            kinds,
            vec![
                (
                    FindingKind::AssertionsRemoved,
                    "tests/test_a.py".to_string()
                ),
                (FindingKind::ProtectedChanged, "tests/test_a.py".to_string()),
                (FindingKind::Secret, "new.txt".to_string()),
                (FindingKind::TestSkipped, "tests/test_a.py".to_string()),
                (FindingKind::VerifierConfig, "package.json".to_string()),
            ]
        );
        // The working tree and the real index are untouched.
        assert!(r.dir.join("new.txt").exists());
        assert!(r
            .git(&["diff", "--cached", "--name-only"])
            .await
            .unwrap()
            .trim()
            .is_empty());
        // Without test guard and protected paths only the secret remains.
        let res = r
            .inspect(
                &gate::InspectRequest {
                    base,
                    ..Default::default()
                },
                KEY,
            )
            .await
            .unwrap();
        assert_eq!(res.findings.len(), 1);
        assert_eq!(res.findings[0].kind, FindingKind::Secret);
    }

    #[tokio::test]
    async fn hold_release_and_discard() {
        let (_t, r) = repo().await;
        let task = "0123456789abcdef0123456789abcdef";
        std::fs::write(r.dir.join("a.txt"), "agent\n").unwrap();
        let head = r.head().await.unwrap();
        r.hold(task, &r.head().await.unwrap()).await.unwrap();
        assert!(!r.dir.join("a.txt").exists());
        assert!(!r.is_dirty().await.unwrap());
        let (d, changed) = r.held_diff(task, 100_000).await.unwrap();
        assert!(changed && d.contains("+agent"));
        // Another task commits meanwhile.
        std::fs::write(r.dir.join("b.txt"), "other\n").unwrap();
        let other = r.commit("other", "A", "a@x").await.unwrap();
        assert_ne!(other, head);
        let rel = gate::ReleaseRequest {
            task: task.into(),
            message: "task".into(),
            author_name: "A".into(),
            author_email: "a@x".into(),
            allow: vec![],
            allow_large: false,
        };
        let out = r.release(&rel, KEY).await.unwrap();
        assert!(!out.conflict && out.blocked.is_empty());
        assert_ne!(out.sha, other);
        assert!(r.dir.join("a.txt").exists() && r.dir.join("b.txt").exists());
        assert!(r.held_diff(task, 10).await.is_err(), "ref removed");

        // Conflict: held change and a later commit touch the same line.
        std::fs::write(r.dir.join("a.txt"), "held\n").unwrap();
        r.hold(task, &r.head().await.unwrap()).await.unwrap();
        std::fs::write(r.dir.join("a.txt"), "later\n").unwrap();
        r.commit("later", "A", "a@x").await.unwrap();
        let out = r.release(&rel, KEY).await.unwrap();
        assert!(out.conflict);
        assert!(!r.is_dirty().await.unwrap(), "no conflict leftovers");
        assert_eq!(
            std::fs::read_to_string(r.dir.join("a.txt")).unwrap(),
            "later\n"
        );

        // Discard prunes the held objects (a held secret leaves no trace).
        let secret = testdata::github(30);
        std::fs::write(r.dir.join("s.txt"), format!("{secret}\n")).unwrap();
        let t2 = "fedcba9876543210fedcba9876543210";
        r.hold(t2, &r.head().await.unwrap()).await.unwrap();
        let held = r
            .git(&["rev-parse", &format!("refs/queue/held/{t2}")])
            .await
            .unwrap();
        let blob = r
            .git(&["rev-parse", &format!("{}:s.txt", held.trim())])
            .await
            .unwrap();
        r.discard(t2, false).await.unwrap();
        assert!(
            r.git(&["cat-file", "-e", blob.trim()]).await.is_err(),
            "blob pruned"
        );
        // While a process runs in the workspace the prune waits for `begin`.
        std::fs::write(r.dir.join("s.txt"), format!("{}\n", testdata::github(31))).unwrap();
        r.hold(t2, &r.head().await.unwrap()).await.unwrap();
        let held = r
            .git(&["rev-parse", &format!("refs/queue/held/{t2}")])
            .await
            .unwrap();
        let blob = r
            .git(&["rev-parse", &format!("{}:s.txt", held.trim())])
            .await
            .unwrap();
        r.discard(t2, true).await.unwrap();
        assert!(
            r.git(&["cat-file", "-e", blob.trim()]).await.is_ok(),
            "not pruned while busy"
        );
        assert!(r.dir.join(PRUNE_MARKER).exists());
        r.begin(None, KEY, &[]).await.unwrap();
        assert!(
            r.git(&["cat-file", "-e", blob.trim()]).await.is_err(),
            "pruned by begin"
        );
        assert!(!r.dir.join(PRUNE_MARKER).exists());
        assert!(r.hold("../x", &r.head().await.unwrap()).await.is_err());
    }

    #[tokio::test]
    async fn release_blocks_unapproved_secret_and_keeps_the_hold() {
        let (_t, r) = repo().await;
        let task = "0123456789abcdef0123456789abcdee";
        std::fs::write(r.dir.join("k.txt"), format!("{}\n", testdata::github(2))).unwrap();
        r.hold(task, &r.head().await.unwrap()).await.unwrap();
        let mut rel = gate::ReleaseRequest {
            task: task.into(),
            message: "task".into(),
            author_name: "A".into(),
            author_email: "a@x".into(),
            allow: vec![],
            allow_large: false,
        };
        let out = r.release(&rel, KEY).await.unwrap();
        assert_eq!(out.blocked.len(), 1);
        assert!(!r.is_dirty().await.unwrap());
        rel.allow = vec![out.blocked[0].fingerprint.clone().unwrap()];
        let ok = r.release(&rel, KEY).await.unwrap();
        assert!(ok.blocked.is_empty() && !ok.conflict);
        assert!(r.dir.join("k.txt").exists());
    }

    #[tokio::test]
    async fn oversized_changes_block_until_a_human_allows_them() {
        let (_t, r) = repo().await;
        let line = "abcdefghij".repeat(10) + "\n";
        std::fs::write(
            r.dir.join("big.txt"),
            line.repeat(MAX_SCAN_BYTES / line.len() + 10),
        )
        .unwrap();
        let head = r.head().await.unwrap();
        let out = r.commit_checked("t", "A", "a@x", KEY, &[]).await.unwrap();
        assert_eq!(out.sha, head);
        assert_eq!(out.blocked.len(), 1);
        assert_eq!(out.blocked[0].kind, FindingKind::TooLarge);
        let ok = r
            .commit_scanned("t", "A", "a@x", KEY, &[], true)
            .await
            .unwrap();
        assert!(ok.blocked.is_empty());
        assert_ne!(ok.sha, head);
    }

    #[tokio::test]
    async fn baselines_detect_changes_since_approval() {
        let (_t, r) = repo().await;
        let check = gate::BaselineCheck {
            name: "setup".into(),
            paths: vec!["package.json".into(), "scripts/".into()],
        };
        let first = r.baseline_check(&check).await.unwrap();
        assert!(first.changed.is_empty(), "trust on first use");
        std::fs::create_dir_all(r.dir.join("scripts")).unwrap();
        std::fs::write(r.dir.join("scripts/setup.sh"), "curl evil\n").unwrap();
        std::fs::write(r.dir.join("README.md"), "x\n").unwrap();
        let sha = r.commit("agent", "A", "a@x").await.unwrap();
        let res = r.baseline_check(&check).await.unwrap();
        assert_eq!(res.changed, vec!["scripts/setup.sh"]);
        assert_eq!(res.head, sha);
        r.baseline_approve(&gate::BaselineApprove {
            name: "setup".into(),
            sha: sha.clone(),
        })
        .await
        .unwrap();
        assert!(r.baseline_check(&check).await.unwrap().changed.is_empty());
        assert!(r
            .baseline_approve(&gate::BaselineApprove {
                name: "x".into(),
                sha
            })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn push_to_default_branch_and_secrets_need_approval() {
        let t = tempfile::tempdir().unwrap();
        let remote = bare_remote(t.path(), "main").await;
        let a = Repo::new(t.path(), "a");
        a.init(&InitWorkspace {
            remote: Some(remote.clone()),
            branch: None,
        })
        .await
        .unwrap();
        std::fs::write(a.dir.join("x.txt"), "1\n").unwrap();
        let first = a.commit("first", "A", "a@x").await.unwrap();
        let opt = |sha: Option<&'static str>| PushOptions {
            sha,
            allow_protected: false,
            allow: &[],
            allow_secrets: false,
            key: KEY,
        };
        // Empty remote: default branch unknown, `main` counts as protected.
        let o = a.push_checked(&remote, &opt(None)).await.unwrap();
        assert_eq!(o.held.len(), 1);
        assert_eq!(o.held[0].kind, FindingKind::PushProtected);
        push_all(&a, &remote).await;
        // Now the remote reports its default branch.
        std::fs::write(a.dir.join("y.txt"), "2\n").unwrap();
        let second = a.commit("second", "A", "a@x").await.unwrap();
        let o = a.push_checked(&remote, &opt(None)).await.unwrap();
        assert!(
            o.held[0].detail.contains("„main“") && !o.held[0].detail.contains("nicht ermittelbar")
        );
        // A feature branch is not protected; exact commit is pushed.
        let feature = GitRemote {
            branch: "queue/x".into(),
            ..remote.clone()
        };
        let o = a.push_checked(&feature, &opt(None)).await.unwrap();
        assert!(o.held.is_empty(), "{:?}", o.held);
        let remote_sha = run_git(
            t.path(),
            &[],
            &["--git-dir", &remote.url, "rev-parse", "refs/heads/queue/x"],
            None,
        )
        .await
        .unwrap();
        assert_eq!(remote_sha.trim(), second);
        // Secret in a commit of the pushed range (even if removed later).
        std::fs::write(a.dir.join("s.txt"), format!("{}\n", testdata::github(77))).unwrap();
        a.commit("leak", "A", "a@x").await.unwrap();
        std::fs::remove_file(a.dir.join("s.txt")).unwrap();
        a.commit("remove", "A", "a@x").await.unwrap();
        let o = a.push_checked(&feature, &opt(None)).await.unwrap();
        assert_eq!(o.held.len(), 1);
        assert_eq!(o.held[0].kind, FindingKind::Secret);
        let fp = o.held[0].fingerprint.clone().unwrap();
        let allow = [fp];
        let o = a
            .push_checked(
                &feature,
                &PushOptions {
                    allow: &allow,
                    ..opt(None)
                },
            )
            .await
            .unwrap();
        assert!(o.held.is_empty());
        // The agent re-points `origin`: the push still goes to the app's URL.
        a.git(&["remote", "set-url", "origin", "https://evil.invalid/x.git"])
            .await
            .unwrap();
        let o = a
            .push_checked(
                &remote,
                &PushOptions {
                    allow_protected: true,
                    allow: &allow,
                    ..opt(None)
                },
            )
            .await
            .unwrap();
        assert!(o.held.is_empty(), "{:?}", o.held);
        let _ = first;
        assert!(a
            .push_checked(
                &GitRemote {
                    branch: "-x".into(),
                    ..remote.clone()
                },
                &opt(None)
            )
            .await
            .is_err());
        assert!(a
            .push_checked(&remote, &opt(Some("--force")))
            .await
            .is_err());
    }

    #[test]
    fn ls_remote_and_config_helpers() {
        let out = "ref: refs/heads/trunk\tHEAD\nabc\tHEAD\ndef\trefs/heads/feat\n";
        assert_eq!(
            parse_ls_remote(out, "refs/heads/feat"),
            (Some("trunk".into()), Some("def".into()))
        );
        assert_eq!(parse_ls_remote("", "refs/heads/x"), (None, None));
        let listed = "core.repositoryformatversion\n1\0core.fsmonitor\nevil\0extensions.objectformat\nsha256\0branch.we\"ird.merge\nrefs/heads/x\0url.https://e/.insteadof\nhttps://g/\0";
        let c = canonical_config(listed);
        assert!(c.contains("repositoryformatversion = \"1\""), "{c}");
        assert!(c.contains("objectformat = \"sha256\""));
        assert!(c.contains("[branch \"we\\\"ird\"]"), "{c}");
        assert!(!c.contains("fsmonitor") && !c.contains("insteadof"));
        assert!(check_branch("feature/a-b").is_ok());
        for bad in ["", "-f", "a..b", "a b", "a:b", "x~1"] {
            assert!(check_branch(bad).is_err(), "{bad}");
        }
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
