use std::path::Path;

use queued_common::{
    gate::{BaselineApprove, BaselineCheck},
    secrets::testdata,
    Carry, InitWorkspace, MergeFinish, MergeRequest, TaskWorkspaceRequest,
};

use super::*;

const KEY: &[u8] = b"test-key";
const A: &str = "taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "tbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BRANCH_A: &str = "queue/aaaaaaaaaaaa";
const BRANCH_B: &str = "queue/bbbbbbbbbbbb";

/// Plain git in `dir` (a stand-in for the agent's own git in the container).
fn agent_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=Agent", "-c", "user.email=a@x"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

async fn main_repo() -> (tempfile::TempDir, Repo) {
    let t = tempfile::tempdir().unwrap();
    let r = Repo::new(t.path(), "11111111-2222-4333-8444-555555555555");
    r.init(&InitWorkspace::default()).await.unwrap();
    std::fs::write(r.dir.join("app.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::create_dir_all(r.dir.join("tests")).unwrap();
    std::fs::write(r.dir.join("tests/app_test.py"), "assert one()\n").unwrap();
    r.commit("base", "Me", "me@x").await.unwrap();
    (t, r)
}

fn req(branch: &str) -> TaskWorkspaceRequest {
    TaskWorkspaceRequest {
        branch: branch.into(),
        merge_base: None,
        carry: None,
        allow: vec![],
        start: None,
    }
}

fn merge_req(branch: &str, base: &str, tip: &str) -> MergeRequest {
    MergeRequest {
        branch: branch.into(),
        base: base.into(),
        tip: tip.into(),
        message: format!("{branch}\n\nQueue-Task: x"),
        author_name: "Alice".into(),
        author_email: "alice@x".into(),
        protected: vec!["tests/".into()],
        test_guard: true,
        allow: vec![],
        checkout: true,
    }
}

/// Change in a task working tree, committed through the runner.
async fn work(task: &Repo, file: &str, content: &str) -> String {
    std::fs::write(task.dir.join(file), content).unwrap();
    let o = task
        .commit_checked("task", "Alice", "alice@x", KEY, &[])
        .await
        .unwrap();
    assert!(o.blocked.is_empty(), "{:?}", o.blocked);
    o.sha
}

#[tokio::test]
async fn task_workspace_is_an_isolated_repository() {
    let (t, main) = main_repo().await;
    // Objects that must never reach an agent: a quarantined secret, changes
    // another task's gate holds, and another task's branch.
    let secret_file = main.dir.join("leak.env");
    std::fs::write(&secret_file, format!("T={}\n", testdata::github(3))).unwrap();
    main.begin(None, KEY, &[]).await.unwrap();
    let quarantine = main
        .git(&[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/queue/quarantine",
        ])
        .await
        .unwrap();
    assert!(!quarantine.trim().is_empty(), "leftover quarantined");
    let target = main.head().await.unwrap();
    std::fs::write(main.dir.join("held.txt"), "held by another task").unwrap();
    main.hold(&"c".repeat(32), &target).await.unwrap();
    let held = main
        .git(&["rev-parse", &format!("refs/queue/held/{}", "c".repeat(32))])
        .await
        .unwrap();

    let a = Repo::new(t.path(), A);
    let r = main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    assert!(r.created && !r.resumed && r.carried.is_none() && r.conflicts.is_empty());
    assert_eq!(r.head, target);
    assert_eq!(r.merge_base, target);
    let git_dir = a.dir.join(".git");
    assert!(std::fs::symlink_metadata(&git_dir)
        .unwrap()
        .file_type()
        .is_dir());
    for f in ["commondir", "objects/info/alternates", "gitdir"] {
        assert!(!git_dir.join(f).exists(), "{f}");
    }
    assert_eq!(
        agent_git(&a.dir, &["symbolic-ref", "HEAD"]),
        format!("refs/heads/{BRANCH_A}")
    );
    assert_eq!(
        std::fs::read_to_string(a.dir.join("app.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    // Only the target history was copied.
    for obj in [quarantine.trim(), held.trim()] {
        let ok = std::process::Command::new("git")
            .args(["cat-file", "-e", obj])
            .current_dir(&a.dir)
            .status()
            .unwrap()
            .success();
        assert!(!ok, "object {obj} must not be in the task repository");
    }
    assert!(agent_git(&a.dir, &["for-each-ref"])
        .lines()
        .all(|l| l.ends_with(&format!("refs/heads/{BRANCH_A}"))));
    // `.queue/` is excluded in the task repository too.
    std::fs::create_dir_all(a.dir.join(".queue")).unwrap();
    std::fs::write(a.dir.join(".queue/result.json"), "{}").unwrap();
    assert!(!a.is_dirty().await.unwrap());

    // Continuation: same tree, leftovers committed as WIP.
    std::fs::write(a.dir.join("wip.txt"), "x").unwrap();
    let again = main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    assert!(!again.created && again.resumed);
    assert_ne!(again.head, target);
    assert!(again.warnings.iter().any(|w| w.contains("WIP")));
    // Bad names never reach git.
    let bad = Repo::new(t.path(), "11111111-2222-4333-8444-555555555556");
    assert!(main
        .prepare_task(
            &bad,
            "11111111-2222-4333-8444-555555555556",
            &req(BRANCH_A),
            KEY
        )
        .await
        .is_err());
    assert!(main.prepare_task(&a, A, &req("main"), KEY).await.is_err());
}

/// What an agent does with its own `.git` stays there: hooks/config never
/// run on the host when the runner syncs, forged baselines and refs have no
/// effect on the main repository or on other tasks.
#[cfg(unix)]
#[tokio::test]
async fn agent_cannot_reach_main_repo_or_other_tasks() {
    let (t, main) = main_repo().await;
    let a = Repo::new(t.path(), A);
    let b = Repo::new(t.path(), B);
    main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    main.prepare_task(&b, B, &req(BRANCH_B), KEY).await.unwrap();
    let base = main.head().await.unwrap();
    // Baseline approved on the base state.
    let check = BaselineCheck {
        name: "setup".into(),
        paths: vec!["app.txt".into()],
        task: None,
    };
    main.baseline_check(&check, None).await.unwrap();

    // The agent in A: changes a setup file, commits, forges the baseline in
    // its own repository, plants hooks and config that would run programs.
    let marker = t.path().join("pwned");
    std::fs::write(a.dir.join("app.txt"), "evil\n").unwrap();
    agent_git(&a.dir, &["commit", "-qam", "agent"]);
    agent_git(&a.dir, &["update-ref", "refs/queue/baseline/setup", "HEAD"]);
    let hook = format!("#!/bin/sh\ntouch {}\n", marker.display());
    for h in [
        "post-commit",
        "pre-push",
        "post-checkout",
        "reference-transaction",
        "post-merge",
    ] {
        let p = a.dir.join(".git/hooks").join(h);
        std::fs::write(&p, &hook).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cfg_cmd = format!("touch {}", marker.display());
    agent_git(&a.dir, &["config", "core.fsmonitor", &cfg_cmd]);
    agent_git(&a.dir, &["config", "uploadpack.packObjectsHook", &cfg_cmd]);
    agent_git(&a.dir, &["config", "core.alternateRefsCommand", &cfg_cmd]);
    let tip = agent_git(&a.dir, &["rev-parse", "HEAD"]);

    // The runner copies the task head into the main repository for the
    // baseline check: nothing runs, and the forged baseline is ignored.
    let checked = BaselineCheck {
        task: Some(A.into()),
        ..check.clone()
    };
    let head = main.fetch_task_head(&a, A).await.unwrap();
    assert_eq!(head, tip);
    let res = main.baseline_check(&checked, Some(&head)).await.unwrap();
    assert_eq!(res.baseline, base, "baseline from the main repository");
    assert_eq!(res.changed, vec!["app.txt".to_string()]);
    main.sync_task(&a, A, BRANCH_A, &tip).await.unwrap();
    assert!(
        !marker.exists(),
        "no hook or config command ran on the host"
    );
    let cfg = std::fs::read_to_string(a.dir.join(".git/config")).unwrap();
    assert!(
        !cfg.contains("fsmonitor")
            && !cfg.contains("packObjectsHook")
            && !cfg.contains("alternateRefsCommand"),
        "task config rewritten before its upload-pack ran: {cfg}"
    );
    assert_eq!(
        main.git(&["rev-parse", &format!("refs/heads/{BRANCH_A}")])
            .await
            .unwrap()
            .trim(),
        tip
    );
    // B sees nothing of A, and the main target branch did not move.
    assert!(!std::process::Command::new("git")
        .args(["cat-file", "-e", &tip])
        .current_dir(&b.dir)
        .status()
        .unwrap()
        .success());
    assert_eq!(main.head().await.unwrap(), base);
    // Approving the baseline needs a commit the main repository knows.
    main.baseline_approve(&BaselineApprove {
        name: "setup".into(),
        sha: tip.clone(),
    })
    .await
    .unwrap();
    assert!(main
        .baseline_check(&checked, Some(&tip))
        .await
        .unwrap()
        .changed
        .is_empty());
    assert!(!marker.exists());
}

#[tokio::test]
async fn sync_copies_exactly_the_given_commit_and_checks_objects() {
    let (t, main) = main_repo().await;
    let a = Repo::new(t.path(), A);
    main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    let gated = work(&a, "app.txt", "one\nTWO\nthree\n").await;
    // Later, a verify/review process moves the branch: the gated commit is
    // what gets synced, not the branch head.
    std::fs::write(a.dir.join("later.txt"), "x").unwrap();
    agent_git(&a.dir, &["add", "-A"]);
    agent_git(&a.dir, &["commit", "-qm", "later"]);
    main.sync_task(&a, A, BRANCH_A, &gated).await.unwrap();
    assert_eq!(
        main.git(&["rev-parse", &format!("refs/heads/{BRANCH_A}")])
            .await
            .unwrap()
            .trim(),
        gated
    );
    // A malformed tree (an entry named `.git`) is refused by fsck.
    let blob = {
        let out = std::process::Command::new("sh")
            .args(["-c", "printf x | git hash-object -w --stdin"])
            .current_dir(&a.dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let tree = {
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                &format!("printf '100644 blob {blob}\\t.git\\n' | git mktree"),
            ])
            .current_dir(&a.dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let bad = agent_git(&a.dir, &["commit-tree", &tree, "-m", "bad"]);
    assert!(main.sync_task(&a, A, BRANCH_A, &bad).await.is_err());
    assert_eq!(
        main.git(&["rev-parse", &format!("refs/heads/{BRANCH_A}")])
            .await
            .unwrap()
            .trim(),
        gated
    );
    assert!(main.sync_task(&a, A, "queue/x", &gated).await.is_err());
    assert!(main.sync_task(&a, A, BRANCH_A, "HEAD").await.is_err());
}

#[tokio::test]
async fn merge_queue_squashes_rebases_and_fast_forwards() {
    let (t, main) = main_repo().await;
    let base = main.head().await.unwrap();
    let a = Repo::new(t.path(), A);
    let b = Repo::new(t.path(), B);
    main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    main.prepare_task(&b, B, &req(BRANCH_B), KEY).await.unwrap();
    work(&a, "a.txt", "from a\n").await;
    let tip_a = work(&a, "a.txt", "from a, second commit\n").await;
    let tip_b = work(&b, "app.txt", "one\ntwo\nthree\nfour\n").await;
    main.sync_task(&a, A, BRANCH_A, &tip_a).await.unwrap();
    main.sync_task(&b, B, BRANCH_B, &tip_b).await.unwrap();

    // A: target did not move → one squash commit with exactly A's tree.
    let ra = main
        .merge_prepare(&a, A, &merge_req(BRANCH_A, &base, &tip_a), KEY)
        .await
        .unwrap();
    assert!(
        !ra.moved && ra.conflicts.is_empty() && ra.markers.is_empty() && ra.new_findings.is_empty()
    );
    let ca = ra.commit.clone().unwrap();
    assert_eq!(
        main.tree_of(&ca).await.unwrap(),
        main.tree_of(&tip_a).await.unwrap()
    );
    let fa = main
        .merge_finish(
            A,
            &MergeFinish {
                branch: BRANCH_A.into(),
                commit: ca.clone(),
                target: base.clone(),
                allow: vec![],
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(fa.merged && fa.head == ca);
    assert!(
        !main.has_ref(&format!("refs/heads/{BRANCH_A}")).await,
        "branch cleaned up"
    );

    // B: target moved → 3-way squash on top of A, checked out in B for verify.
    let rb = main
        .merge_prepare(&b, B, &merge_req(BRANCH_B, &base, &tip_b), KEY)
        .await
        .unwrap();
    assert!(rb.moved && rb.conflicts.is_empty() && rb.new_findings.is_empty());
    let cb = rb.commit.clone().unwrap();
    assert_eq!(
        b.head().await.unwrap(),
        cb,
        "rebased commit checked out in the task workspace"
    );
    assert_eq!(
        std::fs::read_to_string(b.dir.join("a.txt")).unwrap(),
        "from a, second commit\n"
    );
    // A stale target is refused (something else moved the branch).
    let stale = main
        .merge_finish(
            B,
            &MergeFinish {
                branch: BRANCH_B.into(),
                commit: cb.clone(),
                target: base.clone(),
                allow: vec![],
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(!stale.merged);
    let fb = main
        .merge_finish(
            B,
            &MergeFinish {
                branch: BRANCH_B.into(),
                commit: cb.clone(),
                target: ca.clone(),
                allow: vec![],
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(fb.merged);
    // Linear history, one commit per task, authored by the task's author.
    let log = main.log(10).await.unwrap();
    assert_eq!(log[0].sha, cb);
    assert_eq!(log[1].sha, ca);
    assert_eq!(log[2].sha, base);
    assert_eq!(log[0].author, "Alice");
    assert_eq!(
        std::fs::read_to_string(main.dir.join("app.txt")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    // Merging again what is already on the target (app crashed after the
    // fast-forward): nothing to merge, no empty commit.
    let again = main
        .merge_prepare(&b, B, &merge_req(BRANCH_B, &base, &tip_b), KEY)
        .await
        .unwrap();
    assert!(again.empty && again.commit.is_none(), "{again:?}");
    assert_eq!(main.head().await.unwrap(), cb);
    // Diff and revert against the merge base work on the target branch.
    let (d, changed) = main.diff(&ca, &cb, 100_000).await.unwrap();
    assert!(changed && d.contains("+four") && !d.contains("from a"));
    main.revert(&ca, &cb).await.unwrap();
    assert!(!std::fs::read_to_string(main.dir.join("app.txt"))
        .unwrap()
        .contains("four"));
    // Removing the task workspaces.
    main.remove_task(&b, B, Some(BRANCH_B)).await.unwrap();
    assert!(!b.dir.exists());
    assert!(main.remove_task(&b, "x", None).await.is_err());
}

#[tokio::test]
async fn conflicts_become_a_fix_workspace_with_markers() {
    let (t, main) = main_repo().await;
    let base = main.head().await.unwrap();
    let a = Repo::new(t.path(), A);
    let b = Repo::new(t.path(), B);
    main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    main.prepare_task(&b, B, &req(BRANCH_B), KEY).await.unwrap();
    let tip_a = work(&a, "app.txt", "one\nTWO from a\nthree\n").await;
    let tip_b = work(&b, "app.txt", "one\nTWO from b\nthree\n").await;
    main.sync_task(&a, A, BRANCH_A, &tip_a).await.unwrap();
    main.sync_task(&b, B, BRANCH_B, &tip_b).await.unwrap();
    let ra = main
        .merge_prepare(&a, A, &merge_req(BRANCH_A, &base, &tip_a), KEY)
        .await
        .unwrap();
    let ca = ra.commit.unwrap();
    assert!(
        main.merge_finish(
            A,
            &MergeFinish {
                branch: BRANCH_A.into(),
                commit: ca.clone(),
                target: base.clone(),
                allow: vec![]
            },
            KEY
        )
        .await
        .unwrap()
        .merged
    );
    let rb = main
        .merge_prepare(&b, B, &merge_req(BRANCH_B, &base, &tip_b), KEY)
        .await
        .unwrap();
    assert!(rb.commit.is_none());
    assert_eq!(rb.conflicts, vec!["app.txt".to_string()]);
    assert_eq!(main.head().await.unwrap(), ca, "nothing merged");
    assert_eq!(b.head().await.unwrap(), tip_b, "task workspace untouched");

    // Fix task: carries B's changes onto the current target, with markers.
    const F: &str = "tffffffffffffffffffffffffffffffff";
    const BRANCH_F: &str = "queue/ffffffffffff";
    let f = Repo::new(t.path(), F);
    let rf = main
        .prepare_task(
            &f,
            F,
            &TaskWorkspaceRequest {
                carry: Some(Carry {
                    base: base.clone(),
                    tip: tip_b.clone(),
                    label: "Übernommen aus B".into(),
                }),
                ..req(BRANCH_F)
            },
            KEY,
        )
        .await
        .unwrap();
    assert_eq!(rf.merge_base, ca);
    assert_eq!(rf.conflicts, vec!["app.txt".to_string()]);
    let carried = rf.carried.clone().unwrap();
    assert_eq!(rf.head, carried);
    let text = std::fs::read_to_string(f.dir.join("app.txt")).unwrap();
    assert!(
        text.contains("<<<<<<<") && text.contains("TWO from a") && text.contains("TWO from b"),
        "{text}"
    );
    // Unresolved markers stop the merge.
    let rm = main
        .merge_prepare(&f, F, &merge_req(BRANCH_F, &ca, &carried), KEY)
        .await
        .unwrap();
    assert_eq!(rm.markers, vec!["app.txt".to_string()]);
    // Resolved by the fix agent → merges on top of A.
    let fixed = work(&f, "app.txt", "one\nTWO from a and b\nthree\n").await;
    main.sync_task(&f, F, BRANCH_F, &fixed).await.unwrap();
    let rf2 = main
        .merge_prepare(&f, F, &merge_req(BRANCH_F, &ca, &fixed), KEY)
        .await
        .unwrap();
    assert!(
        !rf2.moved && rf2.markers.is_empty() && rf2.new_findings.is_empty(),
        "{rf2:?}"
    );
    let cf = rf2.commit.unwrap();
    assert!(
        main.merge_finish(
            F,
            &MergeFinish {
                branch: BRANCH_F.into(),
                commit: cf.clone(),
                target: ca.clone(),
                allow: vec![]
            },
            KEY
        )
        .await
        .unwrap()
        .merged
    );
    let (d, _) = main.diff(&ca, &cf, 100_000).await.unwrap();
    assert!(d.contains("+TWO from a and b") && !d.contains("<<<<<<<"));
    // Carrying an empty change starts on the target as is.
    const E: &str = "teeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    let e = Repo::new(t.path(), E);
    let re = main
        .prepare_task(
            &e,
            E,
            &TaskWorkspaceRequest {
                carry: Some(Carry {
                    base: base.clone(),
                    tip: base.clone(),
                    label: "x".into(),
                }),
                ..req("queue/eeeeeeeeeeee")
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(re.carried.is_none() && re.head == cf);
}

/// The merge re-checks the merged change with the gate's rules (fail-closed):
/// what the task's own change did not have is reported.
#[tokio::test]
async fn merge_rechecks_the_merged_change() {
    let (t, main) = main_repo().await;
    let base = main.head().await.unwrap();
    let a = Repo::new(t.path(), A);
    main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    // A secret committed without the scanner (e.g. approved earlier).
    let token = testdata::github(5);
    std::fs::write(a.dir.join("cfg.txt"), format!("token = {token}\n")).unwrap();
    let tip = a.commit("approved", "Alice", "a@x").await.unwrap();
    let r = main
        .merge_prepare(&a, A, &merge_req(BRANCH_A, &base, &tip), KEY)
        .await
        .unwrap();
    assert_eq!(r.new_findings.len(), 1);
    assert_eq!(r.new_findings[0].path, "cfg.txt");
    assert!(!format!("{:?}", r.new_findings).contains(&token));
    let fp = r.new_findings[0].fingerprint.clone().unwrap();
    let r = main
        .merge_prepare(
            &a,
            A,
            &MergeRequest {
                allow: vec![fp.clone()],
                ..merge_req(BRANCH_A, &base, &tip)
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(r.new_findings.is_empty(), "{:?}", r.new_findings);
    // A protected test file the task itself changed is a known finding of
    // its own change (approved at its gate), not a new one.
    std::fs::write(a.dir.join("tests/app_test.py"), "assert True\n").unwrap();
    let tip2 = a.commit("tests", "Alice", "a@x").await.unwrap();
    let r = main
        .merge_prepare(
            &a,
            A,
            &MergeRequest {
                allow: vec![fp],
                ..merge_req(BRANCH_A, &base, &tip2)
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(r.new_findings.is_empty(), "{:?}", r.new_findings);
}

/// Changes a WP03 gate held in the shared workspace before the upgrade move
/// into the task's own repository and out of the shared one.
#[tokio::test]
async fn legacy_held_changes_move_into_the_task_workspace() {
    let (t, main) = main_repo().await;
    let base = main.head().await.unwrap();
    std::fs::write(main.dir.join("held.txt"), "held\n").unwrap();
    main.hold(&"a".repeat(32), &base).await.unwrap();
    let a = Repo::new(t.path(), A);
    main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    assert!(
        !main
            .has_ref(&format!("refs/queue/held/{}", "a".repeat(32)))
            .await
    );
    let (d, changed) = a.held_diff(&"a".repeat(32), 100_000).await.unwrap();
    assert!(changed && d.contains("+held"));
    let out = a
        .release(
            &queued_common::gate::ReleaseRequest {
                task: "a".repeat(32),
                message: "m".into(),
                author_name: "A".into(),
                author_email: "a@x".into(),
                allow: vec![],
                allow_large: false,
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(!out.conflict && out.blocked.is_empty());
    assert_eq!(
        std::fs::read_to_string(a.dir.join("held.txt")).unwrap(),
        "held\n"
    );
}

/// A broken task repository (`.git` replaced by a symlink) is rebuilt from
/// the task's branch in the main repository instead of being used.
#[cfg(unix)]
#[tokio::test]
async fn broken_task_workspace_is_rebuilt_from_its_branch() {
    let (t, main) = main_repo().await;
    let a = Repo::new(t.path(), A);
    main.prepare_task(&a, A, &req(BRANCH_A), KEY).await.unwrap();
    let base = main.head().await.unwrap();
    let tip = work(&a, "a.txt", "a\n").await;
    main.sync_task(&a, A, BRANCH_A, &tip).await.unwrap();
    std::fs::remove_dir_all(a.dir.join(".git")).unwrap();
    std::os::unix::fs::symlink(main.dir.join(".git"), a.dir.join(".git")).unwrap();
    let r = main
        .prepare_task(
            &a,
            A,
            &TaskWorkspaceRequest {
                merge_base: Some(base.clone()),
                ..req(BRANCH_A)
            },
            KEY,
        )
        .await
        .unwrap();
    assert!(r.created && r.resumed && r.head == tip && r.merge_base == base);
    assert!(std::fs::symlink_metadata(a.dir.join(".git"))
        .unwrap()
        .file_type()
        .is_dir());
    assert_eq!(
        main.head().await.unwrap(),
        base,
        "main repository untouched"
    );
}

/// WP05: a pull-request follow-up starts on the state of the pull request's
/// branch on the host (including commits people pushed there), and a carry
/// is applied on top of that state, not on the target branch.
#[tokio::test]
async fn pull_request_follow_up_starts_on_the_remote_branch() {
    let (t, main) = main_repo().await;
    let bare = t.path().join("remote.git");
    run_git(
        t.path(),
        &[],
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
        None,
    )
    .await
    .unwrap();
    let url = bare.to_string_lossy().to_string();
    // The pull request branch on the host: base + a human's commit.
    let base = main.head().await.unwrap();
    main.git(&["push", "-q", "--", &url, &format!("{base}:refs/heads/main")])
        .await
        .unwrap();
    let human = t.path().join("human");
    agent_git(t.path(), &["clone", "-q", &url, human.to_str().unwrap()]);
    agent_git(&human, &["checkout", "-q", "-b", BRANCH_A]);
    std::fs::write(human.join("human.txt"), "pushed on the host\n").unwrap();
    agent_git(&human, &["add", "-A"]);
    agent_git(&human, &["commit", "-q", "-m", "human"]);
    agent_git(&human, &["push", "-q", "origin", BRANCH_A]);
    let pr_head = agent_git(&human, &["rev-parse", "HEAD"]);
    // Meanwhile the target branch of the main repository moved on.
    std::fs::write(main.dir.join("other.txt"), "x\n").unwrap();
    main.commit("other", "Me", "me@x").await.unwrap();

    let b = Repo::new(t.path(), B);
    let mut rq = req(BRANCH_B);
    rq.start = Some(GitRemote {
        url: url.clone(),
        branch: BRANCH_A.into(),
        token: None,
    });
    let r = main.prepare_task(&b, B, &rq, KEY).await.unwrap();
    assert_eq!(r.head, pr_head, "starts on the pull request's head");
    assert_eq!(r.merge_base, pr_head);
    assert!(b.dir.join("human.txt").exists());
    assert!(
        !b.dir.join("other.txt").exists(),
        "not on the target branch"
    );
    // The start ref is cleaned up with the working tree.
    main.remove_task(&b, B, None).await.unwrap();
    assert!(!main.has_ref(&format!("refs/queue/start/{B}")).await);

    // A carry (fix of a follow-up) lands on the pull request's state.
    let a = Repo::new(t.path(), A);
    let mut ra = req(BRANCH_A);
    ra.start = rq.start.clone();
    main.prepare_task(&a, A, &ra, KEY).await.unwrap();
    let tip = work(&a, "fix.txt", "fix\n").await;
    main.sync_task(&a, A, BRANCH_A, &tip).await.unwrap();
    let mut rc = req(BRANCH_B);
    rc.start = rq.start.clone();
    rc.carry = Some(Carry {
        base: pr_head.clone(),
        tip: tip.clone(),
        label: "carried".into(),
    });
    let r = main.prepare_task(&b, B, &rc, KEY).await.unwrap();
    assert_eq!(r.merge_base, pr_head);
    assert!(r.carried.is_some());
    assert!(b.dir.join("fix.txt").exists() && b.dir.join("human.txt").exists());
    assert!(!b.dir.join("other.txt").exists());
    // A missing branch is an error, not a silent fallback to the target.
    let c = Repo::new(t.path(), &format!("t{}", "c".repeat(32)));
    let mut rm = req(&format!("queue/{}", "c".repeat(12)));
    rm.start = Some(GitRemote {
        url,
        branch: "queue/doesnotexist".into(),
        token: None,
    });
    let err = main
        .prepare_task(&c, &format!("t{}", "c".repeat(32)), &rm, KEY)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("nicht abrufbar"), "{err:#}");
}
