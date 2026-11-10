//! WP04 integration tests: branch/working tree per task, parallelism per
//! project, the merge queue (rebase → verify again → fast-forward), conflicts
//! as fix tasks, diff/revert against the merge base, race-free claims.
//! App + real runner (engine `none`) + Postgres; agents are the mock provider.
mod common;

use std::time::Duration;

use common::{start, Client, Harness};
use queued::worker::{claim_merge, claim_next, Blocked};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

async fn project(a: &Client, parallelism: i32, extra: Value) -> String {
    let p = a.project(json!({"name": "Par"})).await;
    let mut body = json!({"review_enabled": false, "parallelism": parallelism});
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        b.extend(e.clone());
    }
    let (code, v) = a.patch(&format!("/api/projects/{p}"), body).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["parallelism"], parallelism);
    p
}

fn git(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

fn task_ws(h: &Harness, task: &str) -> std::path::PathBuf {
    h.workspace(&format!("t{}", task.replace('-', "")))
}

/// Polls until `pred` holds for the task list (or fails after 30 s).
async fn wait_until(a: &Client, what: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let ts = a.tasks().await;
        if pred(&ts) {
            return ts;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: {ts:#?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn status_of<'a>(ts: &'a [Value], id: &str) -> &'a str {
    ts.iter()
        .find(|t| t["id"] == id)
        .and_then(|t| t["status"].as_str())
        .unwrap_or_default()
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn parallel_tasks_run_in_their_own_working_trees_and_merge_serially(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"parallelism": 4})).await;
    let p = project(&a, 2, json!({})).await;
    let t1 = a.task(&p, "MOCK_QUIET MOCK_FILE:one.md first").await;
    let t2 = a.task(&p, "MOCK_QUIET MOCK_FILE:two.md second").await;
    let t3 = a.task(&p, "MOCK_FILE:three.md third").await;
    // Two run at the same time, the third waits for a free slot.
    let ts = wait_until(&a, "two running", |ts| {
        status_of(ts, &t1) == "running" && status_of(ts, &t2) == "running"
    })
    .await;
    assert_eq!(status_of(&ts, &t3), "queued", "project parallelism 2");
    for t in [&t1, &t2] {
        let mut d = a.task_json(t).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        // `.git` appears before the runner finished preparing it (init,
        // fetch, config rewrite): wait until git answers in it.
        while d["task"]["branch"].is_null() || !git(&task_ws(&h, t), &["symbolic-ref", "HEAD"]).0 {
            assert!(tokio::time::Instant::now() < deadline, "{d}");
            tokio::time::sleep(Duration::from_millis(50)).await;
            d = a.task_json(t).await;
        }
        let branch = d["task"]["branch"].as_str().unwrap_or_default().to_string();
        assert_eq!(branch, format!("queue/{}", &t.replace('-', "")[..12]));
        // Own repository with a real `.git` directory on its branch.
        let ws = task_ws(&h, t);
        assert!(std::fs::symlink_metadata(ws.join(".git")).unwrap().is_dir());
        assert_eq!(
            git(&ws, &["symbolic-ref", "HEAD"]).1,
            format!("refs/heads/{branch}")
        );
    }
    for t in [&t1, &t2, &t3] {
        a.wait_status(t, &["done"]).await;
    }
    let main = h.workspace(&p);
    for f in ["one.md", "two.md", "three.md"] {
        assert!(main.join(f).exists(), "{f} merged");
    }
    // One commit per task on the target branch, linear (fast-forward only).
    let log = git(&main, &["log", "--format=%P|%B%x1e"]).1;
    for t in [&t1, &t2, &t3] {
        assert_eq!(log.matches(&format!("Queue-Task: {t}")).count(), 1, "{log}");
    }
    assert!(
        log.split('\x1e')
            .all(|c| !c.trim().split('|').next().unwrap_or("").contains(' ')),
        "no merge commits: {log}"
    );
    // Cleaned up: working trees and task branches.
    for t in [&t1, &t2, &t3] {
        assert!(!task_ws(&h, t).exists(), "working tree of {t} removed");
        let d = a.task_json(t).await;
        assert_eq!(d["task"]["merge_state"], "merged");
        let branch = d["task"]["branch"].as_str().unwrap();
        assert!(
            !git(
                &main,
                &[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("refs/heads/{branch}")
                ]
            )
            .0
        );
        let ev = a.events_of(t).await;
        assert!(
            ev.iter()
                .any(|(k, c)| k == "merge" && c.contains("übernommen")),
            "{ev:?}"
        );
    }
    // The agent ran in its own working tree, not in the project's.
    let where_ = a
        .task(&p, "MOCK_SOLUTION_B64:cHdkID4gd2hlcmUudHh0Cg== where")
        .await;
    a.wait_status(&where_, &["done"]).await;
    let pwd = std::fs::read_to_string(main.join("where.txt")).unwrap();
    assert!(
        pwd.trim()
            .ends_with(&format!("/workspaces/t{}", where_.replace('-', ""))),
        "{pwd}"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn default_parallelism_keeps_a_project_serial_including_its_merge(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"parallelism": 4})).await;
    let p = project(&a, 1, json!({"verify_command": "sleep 2"})).await;
    let t1 = a.task(&p, "first").await;
    let t2 = a.task(&p, "second").await;
    // While t1 runs or waits for its merge, t2 does not start: with the
    // default every task starts on the state its predecessor left.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let ts = a.tasks().await;
        let (s1, s2) = (status_of(&ts, &t1), status_of(&ts, &t2));
        assert!(
            !(s2 == "running" && matches!(s1, "running" | "merging")),
            "{s1} {s2}"
        );
        if s1 == "done" {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let d2 = a.wait_status(&t2, &["done"]).await;
    let d1 = a.task_json(&t1).await;
    assert_eq!(
        d2["task"]["merge_base"], d1["task"]["snapshot_after"],
        "the second task is based on the first one's merged commit"
    );
    // No conflict although both changed MOCK_LOG.md.
    assert_eq!(d2["task"]["merge_state"], "merged");
    let ev = a.events_of(&t2).await;
    assert!(
        !ev.iter().any(|(_, c)| c.contains("nach Aktualisierung")),
        "no re-verify needed: {ev:?}"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn merge_conflict_becomes_a_fix_task_that_carries_the_changes(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"parallelism": 4})).await;
    let p = project(&a, 2, json!({})).await;
    let t1 = a.task(&p, "MOCK_RESOLVE MOCK_FILE:shared.md left").await;
    let t2 = a.task(&p, "MOCK_RESOLVE MOCK_FILE:shared.md right").await;
    // Both change the same line; the one merged second conflicts.
    let ts = wait_until(&a, "one done, one failed with a fix", |ts| {
        let s = [status_of(ts, &t1), status_of(ts, &t2)];
        s.contains(&"done") && s.contains(&"failed") && ts.iter().any(|t| t["kind"] == "fix")
    })
    .await;
    let (winner, loser) = if status_of(&ts, &t1) == "done" {
        (&t1, &t2)
    } else {
        (&t2, &t1)
    };
    let l = a.task_json(loser).await;
    assert_eq!(l["task"]["merge_state"], "conflict", "{l}");
    assert_eq!(l["task"]["failure_class"], "task");
    assert!(
        l["task"]["error"].as_str().unwrap().contains("shared.md"),
        "{l}"
    );
    let fix = ts
        .iter()
        .find(|t| t["parent_id"] == loser.as_str() && t["kind"] == "fix")
        .expect("fix task")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let f = a.wait_status(&fix, &["done"]).await;
    assert_eq!(f["task"]["carry_from"], loser.as_str());
    assert!(
        f["task"]["prompt"]
            .as_str()
            .unwrap()
            .contains("Konfliktmarker"),
        "conflict context for the agent: {f}"
    );
    let ev = a.events_of(&fix).await;
    assert!(
        ev.iter()
            .any(|(_, c)| c.contains("startet mit den Änderungen")
                && c.contains("Konflikte in: shared.md")),
        "{ev:?}"
    );
    assert!(
        ev.iter()
            .any(|(k, c)| k == "tool" && c.contains("resolved")),
        "{ev:?}"
    );
    // Both sides are in the target branch, no marker left.
    let main = h.workspace(&p);
    let shared = std::fs::read_to_string(main.join("shared.md")).unwrap();
    assert!(
        !shared.contains("<<<<<<<") && !shared.contains(">>>>>>>"),
        "{shared}"
    );
    assert!(
        shared.lines().filter(|l| l.starts_with("run ")).count() >= 2,
        "{shared}"
    );
    // The fix's diff (against its merge base) contains the carried change;
    // the branches of the conflicting task and the fix are gone.
    let w = a.task_json(winner).await;
    assert_eq!(f["task"]["merge_base"], w["task"]["snapshot_after"]);
    for t in [loser.as_str(), fix.as_str()] {
        let b = a.task_json(t).await["task"]["branch"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            !git(
                &main,
                &["rev-parse", "--verify", "-q", &format!("refs/heads/{b}")]
            )
            .0,
            "{b}"
        );
        assert!(!task_ws(&h, t).exists());
    }
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn merge_verifies_again_when_the_target_moved(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"parallelism": 4})).await;
    // Each task alone passes; together they fail (an integration bug).
    let p = project(
        &a,
        2,
        json!({"verify_command": "test ! -f a.md || test ! -f b.md"}),
    )
    .await;
    let t1 = a.task(&p, "MOCK_QUIET MOCK_FILE:a.md first").await;
    let t2 = a.task(&p, "MOCK_QUIET MOCK_FILE:b.md second").await;
    let ts = wait_until(&a, "one done, one failed", |ts| {
        let s = [status_of(ts, &t1), status_of(ts, &t2)];
        s.contains(&"done") && (s.contains(&"failed") || s.contains(&"blocked"))
    })
    .await;
    let loser = if status_of(&ts, &t1) == "done" {
        &t2
    } else {
        &t1
    };
    let l = a.task_json(loser).await;
    assert_eq!(l["task"]["merge_state"], "failed", "{l}");
    assert!(
        l["task"]["error"]
            .as_str()
            .unwrap()
            .contains("nach dem Aktualisieren"),
        "{l}"
    );
    let ev = a.events_of(loser).await;
    assert!(
        ev.iter()
            .any(|(k, c)| k == "verify" && c.contains("nach Aktualisierung")),
        "{ev:?}"
    );
    // Nothing of the failed task reached the target branch.
    let main = h.workspace(&p);
    assert!(!(main.join("a.md").exists() && main.join("b.md").exists()));
    // Diff of the unmerged task against its merge base; no revert.
    let d = a.get(&format!("/api/tasks/{loser}/diff")).await.1;
    let file = if loser == &t1 { "a.md" } else { "b.md" };
    assert!(d["diff"].as_str().unwrap().contains(file), "{d}");
    let (code, v) = a
        .post(&format!("/api/tasks/{loser}/revert"), json!({}))
        .await;
    assert_eq!(code, 409, "{v}");
    for t in a.tasks().await {
        if t["kind"] == "fix" {
            a.post(
                &format!("/api/tasks/{}/cancel", t["id"].as_str().unwrap()),
                json!({}),
            )
            .await;
        }
    }
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn diff_and_revert_against_the_merge_base(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    a.settings(json!({"parallelism": 4})).await;
    let p = project(&a, 2, json!({})).await;
    let t1 = a.task(&p, "MOCK_FILE:x.md first").await;
    a.wait_status(&t1, &["done"]).await;
    let t2 = a.task(&p, "MOCK_FILE:y.md second").await;
    let d2 = a.wait_status(&t2, &["done"]).await;
    let d1 = a.task_json(&t1).await;
    assert_eq!(d2["task"]["merge_base"], d1["task"]["snapshot_after"]);
    let diff = a.get(&format!("/api/tasks/{t2}/diff")).await.1;
    let text = diff["diff"].as_str().unwrap();
    assert!(text.contains("y.md") && !text.contains("x.md"), "{text}");
    // Revert of the first task leaves the second one's change alone.
    let (code, v) = a.post(&format!("/api/tasks/{t1}/revert"), json!({})).await;
    assert_eq!(code, 200, "{v}");
    let main = h.workspace(&p);
    assert!(!main.join("x.md").exists() && main.join("y.md").exists());
    let commits = a.get(&format!("/api/projects/{p}/commits")).await.1;
    assert!(
        commits[0]["subject"]
            .as_str()
            .unwrap()
            .starts_with("Revert"),
        "{commits}"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn parallelism_setting_and_merge_queue_api(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let b = h.user("b@x.ch").await;
    let p = a.project(json!({"name": "P"})).await;
    assert_eq!(
        a.get(&format!("/api/projects/{p}")).await.1["parallelism"],
        1
    );
    for bad in [0, 9, -1] {
        let (code, v) = a
            .patch(&format!("/api/projects/{p}"), json!({"parallelism": bad}))
            .await;
        assert_eq!(code, 400, "{bad}: {v}");
        assert!(v.to_string().contains("1 bis 8"), "{v}");
    }
    let (code, _) = a
        .post(
            &format!("/api/projects/{p}/members"),
            json!({"email": "b@x.ch", "role": "editor"}),
        )
        .await;
    assert_eq!(code, 200);
    let (code, _) = b
        .patch(&format!("/api/projects/{p}"), json!({"parallelism": 3}))
        .await;
    assert_eq!(code, 403, "only the owner");
    let (code, _) = b
        .patch(&format!("/api/projects/{p}"), json!({"goal": "x"}))
        .await;
    assert_eq!(code, 200, "editors still edit the rest");
    assert_eq!(
        b.get(&format!("/api/projects/{p}/merge-queue")).await.1,
        json!([])
    );
    let c = h.user("c@x.ch").await;
    assert_eq!(
        c.get(&format!("/api/projects/{p}/merge-queue")).await.0,
        404
    );
    // Entries in merge order: the one being merged first, then the queue.
    let uid: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email='a@x.ch'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    let pid: Uuid = p.parse().unwrap();
    let mut ids = vec![];
    for (state, title) in [
        ("queued", "second"),
        ("merging", "first"),
        ("queued", "third"),
    ] {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO tasks (id, user_id, project_id, title, prompt, provider, status, branch, merge_state, merge_queued_at)
             VALUES ($1,$2,$3,$4,'p','mock','merging',$5,$6, now() + make_interval(secs => $7))",
        )
        .bind(id)
        .bind(uid)
        .bind(pid)
        .bind(title)
        .bind(format!("queue/{}", &id.simple().to_string()[..12]))
        .bind(state)
        .bind(ids.len() as f64)
        .execute(&h.st.db)
        .await
        .unwrap();
        ids.push(id);
    }
    let q = b.get(&format!("/api/projects/{p}/merge-queue")).await.1;
    let titles: Vec<&str> = q
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["first", "second", "third"], "{q}");
    assert_eq!(q[0]["state"], "merging");
    assert_eq!(q[0]["position"], 1);
    assert_eq!(q[2]["position"], 3);
    assert_eq!(q[1]["user"], "a@x.ch");
    assert!(q[1]["branch"].as_str().unwrap().starts_with("queue/"));
    // A waiting entry can be cancelled; it leaves the queue.
    let (code, v) = a
        .post(&format!("/api/tasks/{}/cancel", ids[2]), json!({}))
        .await;
    assert_eq!(code, 200, "{v}");
    let q = b.get(&format!("/api/projects/{p}/merge-queue")).await.1;
    assert_eq!(q.as_array().unwrap().len(), 2);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cancelling_a_task_removes_its_working_tree_and_branch(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a, 1, json!({})).await;
    let t = a.task(&p, "MOCK_SLOW long").await;
    a.wait_status(&t, &["running"]).await;
    let ws = task_ws(&h, &t);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ws.join(".git").exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await;
    let d = a.wait_status(&t, &["cancelled"]).await;
    let branch = d["task"]["branch"].as_str().unwrap().to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while ws.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "working tree still there"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The runner removes the working tree first, then the branch ref.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while git(
        &h.workspace(&p),
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}"),
        ],
    )
    .0 && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !git(
            &h.workspace(&p),
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}")
            ]
        )
        .0,
        "{:?} {:?}",
        git(&h.workspace(&p), &["for-each-ref"]),
        a.events_of(&t).await
    );
    // Retry starts afresh on the target branch.
    let (code, _) = a.post(&format!("/api/tasks/{t}/retry"), json!({})).await;
    assert_eq!(code, 200);
    a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await;
    a.wait_status(&t, &["cancelled"]).await;
    // Deleting the project removes every working tree of its tasks.
    let t2 = a.task(&p, "MOCK_ASK question").await;
    a.wait_status(&t2, &["waiting_input"]).await;
    let ws2 = task_ws(&h, &t2);
    assert!(
        ws2.join(".git").exists(),
        "a task waiting for an answer keeps its working tree"
    );
    let (code, v) = a.delete(&format!("/api/projects/{p}")).await;
    assert_eq!(code, 200, "{v}");
    assert!(!ws2.exists() && !h.workspace(&p).exists());
}

// ---------------------------------------------------------------------------
// Scheduler concurrency: many claimers at the same time (several app
// instances, or a tick racing an API wake-up) never exceed the project's
// parallelism, and never claim a task twice.

async fn seed(pool: &PgPool, parallelism: &[i32], tasks_per_project: usize) -> (Uuid, Vec<Uuid>) {
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
        .bind(user)
        .bind(format!("{user}@x"))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_settings (user_id, parallelism) VALUES ($1, 100)")
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    let mut projects = vec![];
    for par in parallelism {
        let p = Uuid::new_v4();
        sqlx::query("INSERT INTO projects (id, owner_id, name, parallelism) VALUES ($1,$2,'p',$3)")
            .bind(p)
            .bind(user)
            .bind(par)
            .execute(pool)
            .await
            .unwrap();
        for _ in 0..tasks_per_project {
            sqlx::query(
                "INSERT INTO tasks (id, user_id, project_id, title, prompt, provider, status)
                 VALUES ($1,$2,$3,'t','p','mock','queued')",
            )
            .bind(Uuid::new_v4())
            .bind(user)
            .bind(p)
            .execute(pool)
            .await
            .unwrap();
        }
        projects.push(p);
    }
    (user, projects)
}

async fn running_per_project(pool: &PgPool) -> Vec<(Uuid, i64)> {
    sqlx::query_as(
        "SELECT project_id, count(*) FROM tasks WHERE status IN ('running','merging') GROUP BY project_id ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn concurrent_claims_respect_project_parallelism(pool: PgPool) {
    let pars = [1, 2, 3];
    let (user, projects) = seed(&pool, &pars, 6).await;
    // One project already has a task in its merge queue: it uses a slot.
    sqlx::query(
        "UPDATE tasks SET status='merging', merge_state='queued', merge_queued_at=now()
         WHERE id = (SELECT id FROM tasks WHERE project_id=$1 LIMIT 1)",
    )
    .bind(projects[2])
    .execute(&pool)
    .await
    .unwrap();
    for round in 0..3 {
        let claims = (0..24).map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move { claim_next(&pool, user, &Blocked::default()).await.unwrap() })
        });
        let got: Vec<Option<queued::models::Task>> = futures::future::join_all(claims)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        let ids: Vec<Uuid> = got.iter().flatten().map(|t| t.id).collect();
        let mut dedup = ids.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), ids.len(), "a task was claimed twice");
        let counts = running_per_project(&pool).await;
        for (p, n) in &counts {
            let i = projects.iter().position(|x| x == p).unwrap();
            assert!(
                *n <= pars[i] as i64,
                "round {round}: project {i} has {n} > {}",
                pars[i]
            );
        }
        if round == 0 {
            // 1 + 2 + (3 - 1 merging) = 5 claimed in the first round.
            assert_eq!(ids.len(), 5, "{counts:?}");
        } else {
            assert!(ids.is_empty(), "everything is full: {ids:?}");
        }
    }
    // A finished task frees exactly one slot of its project.
    sqlx::query("UPDATE tasks SET status='done' WHERE id = (SELECT id FROM tasks WHERE project_id=$1 AND status='running' LIMIT 1)")
        .bind(projects[1])
        .execute(&pool)
        .await
        .unwrap();
    let claims = (0..8).map(|_| {
        let pool = pool.clone();
        tokio::spawn(async move { claim_next(&pool, user, &Blocked::default()).await.unwrap() })
    });
    let got: Vec<_> = futures::future::join_all(claims)
        .await
        .into_iter()
        .filter_map(|r| r.unwrap())
        .collect();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].project_id, projects[1]);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn concurrent_merge_claims_merge_one_task_per_project_in_order(pool: PgPool) {
    let (_, projects) = seed(&pool, &[3, 3], 4).await;
    for p in &projects {
        sqlx::query(
            "UPDATE tasks SET status='merging', merge_state='queued',
                    merge_queued_at = now() + make_interval(secs => position::float8)
             WHERE project_id=$1",
        )
        .bind(p)
        .execute(&pool)
        .await
        .unwrap();
    }
    let claims = (0..16).map(|_| {
        let pool = pool.clone();
        tokio::spawn(async move { claim_merge(&pool).await.unwrap() })
    });
    let got: Vec<queued::models::Task> = futures::future::join_all(claims)
        .await
        .into_iter()
        .filter_map(|r| r.unwrap())
        .collect();
    assert_eq!(got.len(), 2, "one merge per project");
    for p in &projects {
        let first: Uuid = sqlx::query_scalar(
            "SELECT id FROM tasks WHERE project_id=$1 ORDER BY merge_queued_at LIMIT 1",
        )
        .bind(p)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(got.iter().any(|t| t.id == first), "oldest first");
    }
    // While one merges, nobody else gets one of that project.
    assert!(claim_merge(&pool).await.unwrap().is_none());
    sqlx::query("UPDATE tasks SET status='done', merge_state='merged' WHERE id=$1")
        .bind(got[0].id)
        .execute(&pool)
        .await
        .unwrap();
    let next = claim_merge(&pool).await.unwrap().unwrap();
    assert_eq!(next.project_id, got[0].project_id);
}
