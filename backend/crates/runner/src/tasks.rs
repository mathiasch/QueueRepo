//! Task working trees and the merge queue (WP04).
//!
//! Every task works in its own repository `workspaces/t<task>` on branch
//! `queue/<id>`; only that directory is mounted into its containers. It is
//! **not** a `git worktree`: a linked worktree shares objects, refs, hooks
//! and config with the main repository through `commondir`, so an agent
//! would either have no working git (main `.git` not mounted) or could
//! rewrite other tasks' branches, held secrets and baselines (main `.git`
//! mounted). The task repository is filled by `git fetch` from the main
//! repository (upload-pack sends only what the start commit reaches – never
//! quarantined, held or other tasks' objects) and has no link back.
//!
//! The main repository (`workspaces/<project>`) is runner-only: it holds the
//! target branch, the task branches (`refs/heads/queue/*`), baselines and
//! quarantined leftovers. Objects only move from a task repository into it
//! by exact SHA, with `transfer.fsckObjects`, after `harden()`.
use anyhow::{bail, Context, Result};
use queued_common::{
    gate, is_task_workspace, valid_task_branch, MergeFinish, MergeFinishResult, MergeRequest,
    MergeResult, TaskWorkspaceRequest, TaskWorkspaceResult,
};

use crate::git::{check_rev, run_git, run_git_code, too_large, Repo, MAX_SCAN_BYTES, SCAN_DIFF};

/// Fetch options for objects that come from (or go to) a task repository.
const FSCK: [&str; 4] = [
    "-c",
    "transfer.fsckObjects=true",
    "-c",
    "fetch.fsckObjects=true",
];

fn check_names(task_ws: &str, branch: Option<&str>) -> Result<()> {
    anyhow::ensure!(is_task_workspace(task_ws), "invalid task workspace");
    if let Some(b) = branch {
        anyhow::ensure!(valid_task_branch(b), "invalid task branch");
    }
    Ok(())
}

/// Task id (32 hex) of a task workspace id.
fn task_hex(task_ws: &str) -> &str {
    &task_ws[1..]
}

fn s(p: &std::path::Path) -> String {
    p.to_string_lossy().to_string()
}

impl Repo {
    async fn commit_of(&self, rev: &str) -> Result<String> {
        Ok(self
            .git(&["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])
            .await
            .with_context(|| format!("unknown commit {rev}"))?
            .trim()
            .to_string())
    }

    async fn has_ref(&self, r: &str) -> bool {
        self.git(&["rev-parse", "--verify", "-q", r]).await.is_ok()
    }

    async fn delete_ref(&self, r: &str) -> Result<()> {
        if self.has_ref(r).await {
            self.git(&["update-ref", "-d", r]).await?;
        }
        Ok(())
    }

    async fn tree_of(&self, rev: &str) -> Result<String> {
        Ok(self
            .git(&["rev-parse", &format!("{rev}^{{tree}}")])
            .await?
            .trim()
            .to_string())
    }

    /// `git merge-tree --write-tree`: the merged tree, or the conflicting
    /// files (exit code 1). Never touches a working tree or the index.
    async fn merge_tree(&self, base: &str, ours: &str, theirs: &str) -> Result<MergeTree> {
        self.harden().await?;
        let mb = format!("--merge-base={base}");
        let (code, out, err) = run_git_code(
            &self.dir,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                &mb,
                ours,
                theirs,
            ],
            &[],
        )
        .await?;
        let mut lines = out.lines();
        let tree = lines.next().unwrap_or_default().trim().to_string();
        match code {
            0 => Ok(MergeTree {
                tree,
                conflicts: vec![],
            }),
            1 if check_rev(&tree).is_ok() => {
                let mut conflicts: Vec<String> = lines
                    .take_while(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                conflicts.dedup();
                Ok(MergeTree { tree, conflicts })
            }
            _ => bail!("git merge-tree: {}", err.trim()),
        }
    }

    /// Commit object `tree` with parent `parent` (author from the task).
    async fn commit_tree(
        &self,
        tree: &str,
        parent: &str,
        message: &str,
        author: Option<(&str, &str)>,
    ) -> Result<String> {
        let (name, email) = author.unwrap_or(("Queue", "queue@localhost"));
        let name = name.replace(['<', '>', '\n'], "");
        let email = email.replace(['<', '>', '\n'], "");
        Ok(self
            .git_env(
                &["commit-tree", tree, "-p", parent, "-m", message],
                &[
                    ("GIT_AUTHOR_NAME", name.as_str()),
                    ("GIT_AUTHOR_EMAIL", email.as_str()),
                ],
            )
            .await?
            .trim()
            .to_string())
    }

    /// Copies `sha` from the task repository `task` into this (main)
    /// repository; the object stays reachable from `keep`.
    async fn fetch_from_task(&self, task: &Repo, sha: &str, keep: &str) -> Result<()> {
        check_rev(sha)?;
        // The task repository is agent-controlled: its config is rewritten
        // from the allowlist before its upload-pack reads it.
        task.harden().await?;
        self.harden().await?;
        run_git(
            &self.dir,
            &FSCK.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &[
                "fetch",
                "-q",
                "--no-tags",
                "--no-write-fetch-head",
                "--",
                &s(&task.dir),
                sha,
            ],
            None,
        )
        .await
        .context("copy from the task workspace")?;
        let c = self.commit_of(sha).await?;
        self.git(&["update-ref", keep, &c]).await?;
        Ok(())
    }

    /// Puts `commit` of this (main) repository on `branch` of the task
    /// repository `task` and checks it out (working tree reset, untracked
    /// files removed, ignored files such as installed dependencies kept).
    async fn export_to_task(
        &self,
        task: &Repo,
        task_ws: &str,
        commit: &str,
        branch: &str,
    ) -> Result<()> {
        if task.dir.join(".git").exists() {
            // An existing task repository is agent-controlled: its config
            // (`url.*.insteadOf` could point this fetch elsewhere) is
            // rewritten from the allowlist first.
            task.harden().await?;
        }
        let export = format!("refs/queue/export/{task_ws}");
        self.git(&["update-ref", &export, commit]).await?;
        let spec = format!("+{export}:refs/heads/{branch}");
        let res = run_git(
            &task.dir,
            &FSCK.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &[
                "fetch",
                "-q",
                "--no-tags",
                "--update-head-ok",
                "--no-write-fetch-head",
                "--",
                &s(&self.dir),
                &spec,
            ],
            None,
        )
        .await;
        self.delete_ref(&export).await?;
        res.context("copy into the task workspace")?;
        task.git(&["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")])
            .await?;
        task.git(&["reset", "-q", "--hard", commit]).await?;
        task.git(&["clean", "-fdq"]).await?;
        Ok(())
    }

    /// Creates or continues the working tree of a task (`self` = main repo).
    pub async fn prepare_task(
        &self,
        task: &Repo,
        task_ws: &str,
        req: &TaskWorkspaceRequest,
        key: &[u8],
    ) -> Result<TaskWorkspaceResult> {
        check_names(task_ws, Some(&req.branch))?;
        if let Some(b) = &req.merge_base {
            check_rev(b)?;
        }
        let branch_ref = format!("refs/heads/{}", req.branch);
        let mut warnings = vec![];
        if task.exists() && task.harden().await.is_err() {
            // `.git` replaced by a file/symlink, symlinked objects/refs …:
            // start again from the task's last synced commit.
            task.delete().await?;
            warnings.push("Arbeitsverzeichnis war beschädigt und wurde neu angelegt".to_string());
        }
        if task.exists() {
            // Continuation (re-queued, answered, approved): leftovers of the
            // last run are committed as WIP or quarantined, as in `begin`.
            let begin = task.begin(None, key, &req.allow).await?;
            warnings.extend(begin.warnings);
            let current = task
                .git(&["symbolic-ref", "-q", "HEAD"])
                .await
                .unwrap_or_default();
            if current.trim() != branch_ref {
                // The agent switched branches: continue from where it is,
                // the gate compares against the task's base anyway.
                task.git(&["checkout", "-q", "-B", &req.branch]).await?;
                warnings.push(format!("Branch wiederhergestellt: {}", req.branch));
            }
            let head = task.head().await?;
            return Ok(TaskWorkspaceResult {
                merge_base: req.merge_base.clone().unwrap_or_else(|| head.clone()),
                head,
                created: false,
                resumed: true,
                carried: None,
                conflicts: vec![],
                warnings,
            });
        }
        let target = self.head().await?;
        let mut res = TaskWorkspaceResult {
            created: true,
            warnings,
            ..Default::default()
        };
        let start = if self.has_ref(&branch_ref).await {
            // The working tree was removed (task ended, then retried): the
            // branch in the main repository has the task's last commits.
            let tip = self.commit_of(&branch_ref).await?;
            res.resumed = true;
            res.merge_base = match &req.merge_base {
                Some(b) => b.clone(),
                None => self
                    .git(&["merge-base", &target, &tip])
                    .await
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| target.clone()),
            };
            tip
        } else if let Some(c) = &req.carry {
            check_rev(&c.base)?;
            check_rev(&c.tip)?;
            let base = self.commit_of(&c.base).await?;
            let tip = self.commit_of(&c.tip).await?;
            res.merge_base = target.clone();
            let m = self.merge_tree(&base, &target, &tip).await?;
            if m.tree == self.tree_of(&target).await? {
                target.clone()
            } else {
                let mut msg = c.label.replace('\0', "");
                if !m.conflicts.is_empty() {
                    msg.push_str(&format!(
                        "\n\nKonflikte (Marker im Arbeitsbaum): {}",
                        m.conflicts.join(", ")
                    ));
                }
                let k = self.commit_tree(&m.tree, &target, &msg, None).await?;
                res.carried = Some(k.clone());
                res.conflicts = m.conflicts;
                k
            }
        } else {
            res.merge_base = target.clone();
            target.clone()
        };
        let parent = task.dir.parent().context("no parent")?;
        tokio::fs::create_dir_all(parent).await?;
        let made = async {
            run_git(
                parent,
                &[],
                &["init", "-q", "-b", &req.branch, "--", &s(&task.dir)],
                None,
            )
            .await?;
            self.export_to_task(task, task_ws, &start, &req.branch)
                .await?;
            task.ensure_exclude().await?;
            // Changes a WP03 gate held in the shared workspace before WP04
            // move into the task's own repository (and out of the shared one).
            let held = format!("refs/queue/held/{}", task_hex(task_ws));
            if self.has_ref(&held).await {
                let h = self.commit_of(&held).await?;
                let spec = format!("+{held}:{held}");
                run_git(
                    &task.dir,
                    &FSCK.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    &[
                        "fetch",
                        "-q",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--",
                        &s(&self.dir),
                        &spec,
                    ],
                    None,
                )
                .await?;
                anyhow::ensure!(task.commit_of(&held).await? == h, "held changes not copied");
                self.delete_ref(&held).await?;
                self.prune().await?;
            }
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = made {
            let _ = tokio::fs::remove_dir_all(&task.dir).await;
            return Err(e.context("create task workspace"));
        }
        res.head = task.head().await?;
        Ok(res)
    }

    /// Copies exactly `sha` of the task's working tree to its branch in the
    /// main repository (`self`).
    pub async fn sync_task(
        &self,
        task: &Repo,
        task_ws: &str,
        branch: &str,
        sha: &str,
    ) -> Result<()> {
        check_names(task_ws, Some(branch))?;
        self.fetch_from_task(task, sha, &format!("refs/heads/{branch}"))
            .await
    }

    /// HEAD of the task's working tree, copied into the main repository
    /// (baseline checks of WP03 compare against it there).
    pub async fn fetch_task_head(&self, task: &Repo, task_ws: &str) -> Result<String> {
        check_names(task_ws, None)?;
        let head = task.head().await?;
        self.fetch_from_task(task, &head, &format!("refs/queue/tasks/{task_ws}"))
            .await?;
        Ok(head)
    }

    /// Merge queue step 1: squash `base..tip` onto the target branch as one
    /// commit (3-way, `merge-tree`, no working tree, no hooks) and re-check
    /// the merged change (`self` = main repo).
    pub async fn merge_prepare(
        &self,
        task: &Repo,
        task_ws: &str,
        req: &MergeRequest,
        key: &[u8],
    ) -> Result<MergeResult> {
        check_names(task_ws, Some(&req.branch))?;
        check_rev(&req.base)?;
        check_rev(&req.tip)?;
        if self.commit_of(&req.tip).await.is_err() {
            self.fetch_from_task(task, &req.tip, &format!("refs/heads/{}", req.branch))
                .await?;
        }
        let base = self.commit_of(&req.base).await?;
        let tip = self.commit_of(&req.tip).await?;
        let target = self.head().await?;
        let mut res = MergeResult {
            target: target.clone(),
            moved: target != base,
            ..Default::default()
        };
        let tree = if res.moved {
            let m = self.merge_tree(&base, &target, &tip).await?;
            if !m.conflicts.is_empty() {
                res.conflicts = m.conflicts;
                return Ok(res);
            }
            m.tree
        } else {
            self.tree_of(&tip).await?
        };
        if tree == self.tree_of(&target).await? {
            // Nothing left to bring in (e.g. merged before a crash of the
            // app): no empty commit on the target branch.
            res.empty = true;
            return Ok(res);
        }
        let commit = self
            .commit_tree(
                &tree,
                &target,
                &req.message,
                Some((&req.author_name, &req.author_email)),
            )
            .await?;
        // Keeps the commit alive while verify runs (a discard elsewhere
        // prunes unreachable objects at once).
        self.git(&[
            "update-ref",
            &format!("refs/queue/merge/{task_ws}"),
            &commit,
        ])
        .await?;
        let merged = self.diff_files(&target, &commit).await?;
        let own = self.diff_files(&base, &tip).await?;
        match (merged, own) {
            (None, _) => res.new_findings.push(too_large(MAX_SCAN_BYTES + 1)),
            (Some(merged), own) => {
                res.markers = conflict_markers(&merged);
                res.new_findings
                    .extend(gate::secret_findings(&merged, key, &req.allow));
                let known = own
                    .map(|o| gate::change_findings(&o, &req.protected, req.test_guard))
                    .unwrap_or_default();
                res.new_findings.extend(
                    gate::change_findings(&merged, &req.protected, req.test_guard)
                        .into_iter()
                        .filter(|f| !known.iter().any(|k| k.kind == f.kind && k.path == f.path)),
                );
            }
        }
        if res.markers.is_empty() && res.new_findings.is_empty() && res.moved && req.checkout {
            self.export_to_task(task, task_ws, &commit, &req.branch)
                .await?;
        }
        res.commit = Some(commit);
        Ok(res)
    }

    /// Parsed scan diff `from..to` (`None` = too large for a full check).
    async fn diff_files(&self, from: &str, to: &str) -> Result<Option<Vec<gate::FileDiff>>> {
        let mut args = vec!["diff"];
        args.extend(SCAN_DIFF);
        args.extend([from, to]);
        let out = self.git(&args).await?;
        if out.len() > MAX_SCAN_BYTES {
            return Ok(None);
        }
        Ok(Some(gate::parse_diff(&out)))
    }

    /// Merge queue step 2: fast-forward of the target branch to the merged
    /// commit, only if nothing else moved it in between.
    pub async fn merge_finish(
        &self,
        task_ws: &str,
        req: &MergeFinish,
        key: &[u8],
    ) -> Result<MergeFinishResult> {
        check_names(task_ws, Some(&req.branch))?;
        check_rev(&req.commit)?;
        check_rev(&req.target)?;
        let pinned = format!("refs/queue/merge/{task_ws}");
        let commit = self
            .commit_of(&pinned)
            .await
            .context("nothing prepared to merge")?;
        anyhow::ensure!(commit.starts_with(&req.commit), "prepared commit differs");
        if self.is_dirty().await? {
            // Leftovers in the runner-only main tree (from before WP04):
            // committed as WIP or quarantined, which moves the target.
            self.begin(None, key, &req.allow).await?;
        }
        let head = self.head().await?;
        let parent = self.commit_of(&format!("{commit}^")).await?;
        if head != parent || head != req.target {
            return Ok(MergeFinishResult {
                merged: false,
                head,
            });
        }
        self.git(&["merge", "-q", "--ff-only", "--no-edit", &commit])
            .await?;
        for r in [
            pinned,
            format!("refs/heads/{}", req.branch),
            format!("refs/queue/tasks/{task_ws}"),
        ] {
            self.delete_ref(&r).await?;
        }
        Ok(MergeFinishResult {
            merged: true,
            head: self.head().await?,
        })
    }

    /// Removes the task's working tree and its helper refs; with `branch`
    /// also the task branch in the main repository (`self`).
    pub async fn remove_task(
        &self,
        task: &Repo,
        task_ws: &str,
        branch: Option<&str>,
    ) -> Result<()> {
        check_names(task_ws, branch)?;
        task.delete().await?;
        if self.exists() {
            let mut refs = vec![
                format!("refs/queue/merge/{task_ws}"),
                format!("refs/queue/tasks/{task_ws}"),
                format!("refs/queue/export/{task_ws}"),
            ];
            if let Some(b) = branch {
                refs.push(format!("refs/heads/{b}"));
            }
            for r in refs {
                self.delete_ref(&r).await?;
            }
        }
        Ok(())
    }
}

struct MergeTree {
    tree: String,
    conflicts: Vec<String>,
}

/// Files whose added lines contain git conflict markers.
fn conflict_markers(files: &[gate::FileDiff]) -> Vec<String> {
    files
        .iter()
        .filter(|f| {
            f.plus.iter().any(|(_, lines)| {
                lines.iter().any(|l| {
                    l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "<<<<<<<"
                })
            })
        })
        .map(|f| f.path.clone())
        .collect()
}

#[cfg(test)]
mod tests;
