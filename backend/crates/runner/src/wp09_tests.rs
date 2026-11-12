//! WP09 runner tests: attachments in `.queue/attachments/`, `.queue/` never
//! in a commit, the project state check against the working tree and the
//! knowledge export.
use std::path::Path;

use queued_common::{
    state::{Feature, FeatureStatus, ProjectState},
    InitWorkspace, KnowledgeExport, StateCheckRequest, BRIDGE_CONTENT, MANAGED_MARKER,
};

use crate::git::{provide_attachments, Repo, QUEUE_DIR};

const KEY: &[u8] = b"test-key";

async fn repo() -> (tempfile::TempDir, Repo) {
    let t = tempfile::tempdir().unwrap();
    let r = Repo::new(t.path(), "p1");
    r.init(&InitWorkspace::default()).await.unwrap();
    (t, r)
}

fn git(dir: &Path, args: &[&str]) -> String {
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

fn files(n: usize) -> Vec<(String, Vec<u8>)> {
    (0..n)
        .map(|i| {
            (
                format!("{:02}-file.txt", i + 1),
                format!("content {i}").into_bytes(),
            )
        })
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn attachments_are_provided_without_following_symlinks() {
    let (t, r) = repo().await;
    let outside = t.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "host file").unwrap();
    let q = r.dir.join(QUEUE_DIR);
    std::fs::create_dir_all(&q).unwrap();

    // `.queue/attachments` → a host directory: the link is replaced, the
    // host directory is neither emptied nor written.
    std::os::unix::fs::symlink(&outside, q.join("attachments")).unwrap();
    provide_attachments(&r.dir, &files(2)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "host file"
    );
    assert!(!outside.join("01-file.txt").exists());
    let dir = q.join("attachments");
    assert!(std::fs::symlink_metadata(&dir)
        .unwrap()
        .file_type()
        .is_dir());
    assert_eq!(
        std::fs::read_to_string(dir.join("02-file.txt")).unwrap(),
        "content 1"
    );

    // A symlink inside the directory (planted by the agent) and an extra
    // file disappear: the directory is rebuilt, nothing written through.
    std::os::unix::fs::symlink(outside.join("keep.txt"), dir.join("01-file.txt.tmp")).unwrap();
    std::fs::remove_file(dir.join("01-file.txt")).unwrap();
    std::os::unix::fs::symlink(outside.join("keep.txt"), dir.join("01-file.txt")).unwrap();
    provide_attachments(&r.dir, &files(1)).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "host file"
    );
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["01-file.txt"]);
    assert!(std::fs::symlink_metadata(dir.join("01-file.txt"))
        .unwrap()
        .file_type()
        .is_file());

    // `.queue` itself → elsewhere.
    std::fs::remove_dir_all(&q).unwrap();
    std::os::unix::fs::symlink(&outside, &q).unwrap();
    provide_attachments(&r.dir, &files(1)).await.unwrap();
    assert!(!outside.join("attachments").exists());
    assert!(std::fs::symlink_metadata(&q).unwrap().file_type().is_dir());

    // Names: no traversal, no upper case, no hidden files, no duplicates;
    // count and size limits.
    for bad in ["../x.txt", "a/b.txt", "A.txt", ".hidden", "x..txt", ""] {
        let e = provide_attachments(&r.dir, &[(bad.to_string(), b"x".to_vec())]).await;
        assert!(e.is_err(), "{bad}");
    }
    let dup = vec![
        ("a.txt".to_string(), vec![1]),
        ("a.txt".to_string(), vec![2]),
    ];
    assert!(provide_attachments(&r.dir, &dup).await.is_err());
    assert!(
        provide_attachments(&r.dir, &files(queued_common::MAX_ATTACHMENTS_PER_TASK + 1))
            .await
            .is_err()
    );
    let big = vec![(
        "big.bin".to_string(),
        vec![0u8; queued_common::MAX_ATTACHMENT_BYTES_PER_TASK + 1],
    )];
    assert!(provide_attachments(&r.dir, &big).await.is_err());
    // A failed call leaves the previous attachments alone (checks first).
    assert!(q.join("attachments/01-file.txt").exists());
    // Empty list removes the directory.
    provide_attachments(&r.dir, &[]).await.unwrap();
    assert!(!q.join("attachments").exists());
}

/// Attachments never reach a commit: not through a `.gitignore` negation,
/// not with the exclude line removed by the agent, not through the
/// temporary index of the gate.
#[tokio::test]
async fn queue_dir_is_never_staged() {
    let (_t, r) = repo().await;
    let head = r.head().await.unwrap();
    provide_attachments(&r.dir, &files(1)).await.unwrap();
    std::fs::write(r.dir.join(".git/info/exclude"), "").unwrap();
    std::fs::write(r.dir.join(".gitignore"), "!.queue/\n!.queue/**\n").unwrap();
    // Only `.gitignore` changed for real: it is committed, `.queue/` is not.
    let o = r.commit_checked("t", "A", "a@x", KEY, &[]).await.unwrap();
    assert!(o.blocked.is_empty());
    assert_ne!(o.sha, head);
    let tree = git(&r.dir, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(!tree.contains(".queue"), "{tree}");
    assert!(tree.contains(".gitignore"));
    // Nothing but `.queue/` dirty: no commit at all (and no error).
    std::fs::write(r.dir.join(".queue/attachments/01-file.txt"), "changed").unwrap();
    let again = r.commit_checked("t", "A", "a@x", KEY, &[]).await.unwrap();
    assert_eq!(again.sha, o.sha);
    // The exclude line is back.
    assert!(std::fs::read_to_string(r.dir.join(".git/info/exclude"))
        .unwrap()
        .contains(".queue/"));
}

/// Commits the agent made itself with `.queue/` content are squashed into
/// the queue's commit without it; the branch history holds no attachment.
#[tokio::test]
async fn agent_commits_with_queue_files_are_squashed() {
    let (_t, r) = repo().await;
    let base = r.head().await.unwrap();
    // No agent commit: nothing to do.
    assert!(!r.squash_queue_commits(&base).await.unwrap());
    provide_attachments(&r.dir, &files(1)).await.unwrap();
    std::fs::write(r.dir.join("src.txt"), "work").unwrap();
    git(&r.dir, &["add", "-f", "-A"]);
    git(&r.dir, &["commit", "-qm", "agent 1 (with attachment)"]);
    git(&r.dir, &["rm", "-q", "-r", "--cached", ".queue"]);
    git(&r.dir, &["commit", "-qm", "agent 2 (removes it again)"]);
    std::fs::write(r.dir.join("more.txt"), "more").unwrap();
    assert!(r.squash_queue_commits(&base).await.unwrap());
    let o = r
        .commit_checked("queue", "A", "a@x", KEY, &[])
        .await
        .unwrap();
    assert!(o.blocked.is_empty());
    assert_eq!(
        git(&r.dir, &["rev-parse", "HEAD~1"]),
        base,
        "one commit on the base"
    );
    assert_eq!(
        git(
            &r.dir,
            &[
                "log",
                "--format=%H",
                &format!("{base}..HEAD"),
                "--",
                ".queue"
            ]
        ),
        ""
    );
    let tree = git(&r.dir, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(tree.contains("src.txt") && tree.contains("more.txt"));
    assert!(!tree.contains(".queue"));
    // Agent commits without `.queue/` keep their history.
    let base2 = r.head().await.unwrap();
    std::fs::write(r.dir.join("x.txt"), "x").unwrap();
    git(&r.dir, &["add", "x.txt"]);
    git(&r.dir, &["commit", "-qm", "agent 3"]);
    assert!(!r.squash_queue_commits(&base2).await.unwrap());
    assert!(r.squash_queue_commits("not a rev").await.is_err());
}

fn feature(id: &str, status: FeatureStatus, files: &[&str]) -> Feature {
    Feature {
        id: id.into(),
        title: id.to_uppercase(),
        status,
        passes: false,
        files: files.iter().map(|s| s.to_string()).collect(),
        steps: vec![],
        notes: String::new(),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn state_is_checked_against_the_working_tree() {
    let (t, r) = repo().await;
    let req = |prev: ProjectState, ok: bool| StateCheckRequest {
        previous: prev,
        allow_done: ok,
        now: "2026-11-12T10:00:00Z".into(),
        task: "1234abcd".into(),
    };
    // No file: nothing to do.
    let res = r
        .check_state(&req(ProjectState::default(), true))
        .await
        .unwrap();
    assert!(!res.present && res.state.is_none());

    std::fs::create_dir_all(r.dir.join("src")).unwrap();
    std::fs::write(r.dir.join("src/login.rs"), "fn login() {}").unwrap();
    std::fs::create_dir_all(t.path().join("host")).unwrap();
    std::fs::write(t.path().join("host/secret.rs"), "x").unwrap();
    std::os::unix::fs::symlink(t.path().join("host"), r.dir.join("linked")).unwrap();
    let q = r.dir.join(QUEUE_DIR);
    std::fs::create_dir_all(&q).unwrap();
    let prev = ProjectState {
        version: 1,
        features: vec![
            feature("login", FeatureStatus::InProgress, &[]),
            feature("export", FeatureStatus::Todo, &[]),
        ],
        progress: vec![],
    };
    let next = serde_json::json!({
        "version": 1,
        "features": [
            {"id": "login", "title": "LOGIN", "status": "done", "passes": true,
             "files": ["src/login.rs", "src/missing.rs", "linked/secret.rs", "../outside.rs", ".git/config"]},
            {"id": "search", "title": "Suche", "status": "todo"}
        ],
        "progress": [{"text": "Login fertig, Tests grün", "at": "made up"}]
    });
    std::fs::write(q.join("state.json"), next.to_string()).unwrap();
    let res = r.check_state(&req(prev.clone(), true)).await.unwrap();
    assert!(res.present && res.rejected.is_none() && res.changed);
    let s = res.state.unwrap();
    let login = s.feature("login").unwrap();
    assert_eq!(login.status, FeatureStatus::Done);
    assert_eq!(
        login.files,
        vec!["src/login.rs"],
        "missing, symlinked and traversing refs removed"
    );
    assert!(s.feature("export").is_some(), "deletion repaired");
    assert!(s.feature("search").is_some());
    assert_eq!(s.progress[0].at, "2026-11-12T10:00:00Z");
    assert_eq!(s.progress[0].task, "1234abcd");
    let repairs = res.repairs.join("\n");
    assert!(
        repairs.contains("src/missing.rs") && repairs.contains("linked/secret.rs"),
        "{repairs}"
    );
    assert!(repairs.contains("export"), "{repairs}");

    // The run failed: "done" is not accepted.
    let res = r.check_state(&req(prev.clone(), false)).await.unwrap();
    assert_eq!(
        res.state.unwrap().feature("login").unwrap().status,
        FeatureStatus::InProgress
    );

    // Junk and schema violations are rejected as a whole.
    std::fs::write(q.join("state.json"), "{ not json").unwrap();
    let res = r.check_state(&req(prev.clone(), true)).await.unwrap();
    assert!(res.present && res.state.is_none());
    assert!(res.rejected.unwrap().contains("kein gültiges JSON"));
    std::fs::write(
        q.join("state.json"),
        r#"{"version":7,"features":[],"progress":[]}"#,
    )
    .unwrap();
    let res = r.check_state(&req(prev.clone(), true)).await.unwrap();
    assert!(res.rejected.unwrap().contains("Schema-Version 7"));
    std::fs::write(
        q.join("state.json"),
        " ".repeat(queued_common::state::MAX_STATE_BYTES + 1),
    )
    .unwrap();
    let res = r.check_state(&req(prev.clone(), true)).await.unwrap();
    assert!(res.rejected.unwrap().contains("zu groß"));
    // A symlinked state file is never read.
    std::fs::remove_file(q.join("state.json")).unwrap();
    std::fs::write(t.path().join("host/state.json"), next.to_string()).unwrap();
    std::os::unix::fs::symlink(t.path().join("host/state.json"), q.join("state.json")).unwrap();
    let res = r.check_state(&req(prev, true)).await.unwrap();
    assert!(!res.present && res.state.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn knowledge_export_writes_agents_md_and_bridges_only() {
    let (t, r) = repo().await;
    let req = |v: i32, content: &str| KnowledgeExport {
        content: content.into(),
        version: v,
        author_name: "Owner".into(),
        author_email: "o@x".into(),
        allow: vec![],
    };
    let out = r
        .export_knowledge(&req(1, "# Regeln\n- Tests laufen lassen\n"), KEY)
        .await
        .unwrap();
    assert_eq!(out.written, vec!["AGENTS.md", "CLAUDE.md", "GEMINI.md"]);
    let agents = std::fs::read_to_string(r.dir.join("AGENTS.md")).unwrap();
    assert!(agents.starts_with(MANAGED_MARKER) && agents.contains("Version 1"));
    assert!(agents.contains("- Tests laufen lassen"));
    assert_eq!(
        std::fs::read_to_string(r.dir.join("CLAUDE.md")).unwrap(),
        BRIDGE_CONTENT
    );
    assert!(git(&r.dir, &["log", "-1", "--format=%B"]).contains("Queue-Knowledge: v1"));
    assert_eq!(git(&r.dir, &["log", "-1", "--format=%an"]), "Owner");

    // A hand-written CLAUDE.md and a symlinked GEMINI.md stay untouched; a
    // symlinked AGENTS.md is replaced, never written through.
    std::fs::write(r.dir.join("CLAUDE.md"), "# eigene Notizen\n").unwrap();
    std::fs::remove_file(r.dir.join("GEMINI.md")).unwrap();
    std::os::unix::fs::symlink("AGENTS.md", r.dir.join("GEMINI.md")).unwrap();
    let host = t.path().join("host.md");
    std::fs::write(&host, "host").unwrap();
    std::fs::remove_file(r.dir.join("AGENTS.md")).unwrap();
    std::os::unix::fs::symlink(&host, r.dir.join("AGENTS.md")).unwrap();
    git(&r.dir, &["add", "-A"]);
    git(&r.dir, &["commit", "-qm", "human edits"]);
    let out = r.export_knowledge(&req(2, "neu"), KEY).await.unwrap();
    assert_eq!(out.skipped, vec!["CLAUDE.md", "GEMINI.md"]);
    assert_eq!(out.written, vec!["AGENTS.md"]);
    assert_eq!(std::fs::read_to_string(&host).unwrap(), "host");
    assert!(std::fs::symlink_metadata(r.dir.join("AGENTS.md"))
        .unwrap()
        .file_type()
        .is_file());
    assert_eq!(
        std::fs::read_to_string(r.dir.join("CLAUDE.md")).unwrap(),
        "# eigene Notizen\n"
    );

    // A secret blocks the export; the files are restored.
    let head = r.head().await.unwrap();
    let token = queued_common::secrets::testdata::github(7);
    let out = r
        .export_knowledge(&req(3, &format!("token = {token}")), KEY)
        .await
        .unwrap();
    assert_eq!(out.blocked.len(), 1);
    assert_eq!(out.sha, head);
    assert!(!std::fs::read_to_string(r.dir.join("AGENTS.md"))
        .unwrap()
        .contains(&token));
    assert!(!r.is_dirty().await.unwrap());
}
