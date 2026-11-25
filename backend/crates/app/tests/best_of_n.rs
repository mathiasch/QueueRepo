//! WP12 integration tests: Best-of-N (isolated attempts, comparison,
//! selection → merge queue, discard → cleanup, group budget over all
//! attempts, automatic selection by a fake review model, failures, quotas,
//! cost preview) and routing rules with their explanation at the task.
//! App + real runner (engine `none`) + Postgres; no real model calls.
mod common;

use std::{path::Path, sync::atomic::Ordering, time::Duration};

use common::{fake_llm::start_fake, start, Client, Harness};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
    )
}

fn hex(id: &str) -> String {
    id.replace('-', "")
}

fn tws(h: &Harness, id: &str) -> std::path::PathBuf {
    h.workspace(&format!("t{}", hex(id)))
}

async fn cmp(a: &Client, g: &str) -> Value {
    let (code, v) = a.get(&format!("/api/tasks/{g}/attempts")).await;
    assert_eq!(code, 200, "{v}");
    v
}

/// Polls the comparison until `f` holds.
async fn wait_cmp<F: Fn(&Value) -> bool>(a: &Client, g: &str, what: &str, f: F) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let v = cmp(a, g).await;
        if f(&v) {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timeout: {what}: {v:#}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn statuses(v: &Value) -> Vec<String> {
    v["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["status"].as_str().unwrap().to_string())
        .collect()
}

fn all(v: &Value, s: &str) -> bool {
    let st = statuses(v);
    !st.is_empty() && st.iter().all(|x| x == s)
}

fn attempt(v: &Value, no: i64) -> Value {
    v["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["attempt_no"] == no)
        .unwrap()
        .clone()
}

async fn setup(h: &Harness, email: &str, parallelism: i64) -> (Client, String) {
    let a = h.user(email).await;
    a.settings(json!({"parallelism": parallelism})).await;
    let p = a.project(json!({"name": "bon"})).await;
    let (code, v) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"review_enabled": false}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    (a, p)
}

/// N attempts, each in its own working tree and repository: none sees the
/// other's files or commits. The user picks one; it goes through the merge
/// queue into the target branch, the others are discarded and removed
/// (working tree and branch). A second selection is refused.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn attempts_are_isolated_selection_merges_and_losers_are_cleaned_up(pool: PgPool) {
    let h = start(pool).await;
    let (a, p) = setup(&h, "bon@x.ch", 3).await;
    let g = a
        .task_with(
            &p,
            "Eine Zeile ergänzen MOCK_FILE:shared.md",
            json!({"attempts": 3}),
        )
        .await;
    let t = a.task_json(&g).await;
    assert_eq!(t["task"]["attempts"], 3);
    let v = wait_cmp(&a, &g, "all candidates", |v| all(v, "candidate")).await;
    assert_eq!(a.task_json(&g).await["task"]["status"], "comparing");
    let ids: Vec<String> = (1..=3)
        .map(|n| attempt(&v, n)["id"].as_str().unwrap().to_string())
        .collect();
    // Every attempt: own repository, own branch, own change.
    let mut lines = vec![];
    let mut tips = vec![];
    for id in &ids {
        let d = tws(&h, id);
        assert!(d.join(".git").is_dir(), "own repository {d:?}");
        let content = std::fs::read_to_string(d.join("shared.md")).unwrap();
        lines.push(content);
        let at = a.task_json(id).await["task"].clone();
        assert_eq!(at["kind"], "attempt");
        assert_eq!(at["attempt_of"], g.as_str());
        assert_eq!(at["routing_decision"]["source"], "attempt");
        tips.push(at["snapshot_after"].as_str().unwrap().to_string());
        let (_, alt) = git(&d, &["config", "--get-regexp", "remote"]);
        assert!(alt.is_empty(), "no link to another repository: {alt}");
        assert!(!d.join(".git/objects/info/alternates").exists());
    }
    for i in 0..3 {
        for j in 0..3 {
            if i == j {
                continue;
            }
            // No file of another attempt in this working tree …
            for l in lines[j].lines().filter(|l| !l.trim().is_empty()) {
                assert!(
                    !lines[i].contains(l),
                    "attempt {i} sees a line of attempt {j}"
                );
            }
            // … and none of its commits in this repository.
            let (exists, _) = git(
                &tws(&h, &ids[i]),
                &["cat-file", "-e", &format!("{}^{{commit}}", tips[j])],
            );
            assert!(!exists, "attempt {i} has the commit of attempt {j}");
        }
    }
    assert_eq!(v["can_select"], true);
    assert!(attempt(&v, 1)["files_changed"].as_u64().unwrap() >= 1);
    assert_eq!(attempt(&v, 1)["verify"], "none");

    // Not a candidate's own group / wrong attempt: refused.
    let (code, _) = a
        .post(
            &format!("/api/tasks/{}/select", ids[0]),
            json!({"attempt_id": ids[1]}),
        )
        .await;
    assert_eq!(code, 404, "select only on the group");
    let other = h.user("other@x.ch").await;
    let (code, _) = other
        .post(
            &format!("/api/tasks/{g}/select"),
            json!({"attempt_id": ids[1]}),
        )
        .await;
    assert_eq!(code, 404, "only the owner picks");

    let (code, r) = a
        .post(
            &format!("/api/tasks/{g}/select"),
            json!({"attempt_id": ids[1]}),
        )
        .await;
    assert_eq!(code, 200, "{r}");
    let (code, _) = a
        .post(
            &format!("/api/tasks/{g}/select"),
            json!({"attempt_id": ids[2]}),
        )
        .await;
    assert_eq!(code, 409, "exactly one selection");
    let w = a.wait_status(&ids[1], &["done"]).await;
    assert_eq!(w["task"]["merge_state"], "merged");
    let gt = a.wait_status(&g, &["done"]).await;
    assert_eq!(gt["task"]["bon"]["choice"]["attempt_no"], 2);
    assert_eq!(gt["task"]["bon"]["choice"]["by"], "user");
    // Target branch: the winner's line only.
    let main = h.workspace(&p);
    let (_, head_file) = git(&main, &["show", "HEAD:shared.md"]);
    assert!(head_file.contains(lines[1].trim()), "{head_file}");
    assert!(!head_file.contains(lines[0].trim()) && !head_file.contains(lines[2].trim()));
    // Losers: discarded, working tree and branch gone.
    for i in [0, 2] {
        let l = a.task_json(&ids[i]).await;
        assert_eq!(l["task"]["status"], "cancelled");
        assert!(l["task"]["error"]
            .as_str()
            .unwrap()
            .starts_with("Verworfen"));
        assert!(!tws(&h, &ids[i]).exists(), "working tree removed");
        let b = l["task"]["branch"].as_str().unwrap().to_string();
        let (exists, _) = git(
            &main,
            &["rev-parse", "--verify", "-q", &format!("refs/heads/{b}")],
        );
        assert!(!exists, "branch {b} removed");
    }
    // Analytics: one task, three attempts (two not counted as tasks).
    let (_, an) = a.get("/api/analytics?days=7").await;
    assert_eq!(an["outcomes"]["finished"], 1, "{an}");
    assert_eq!(an["best_of_n"]["groups"], 1);
    assert_eq!(an["best_of_n"]["attempts"], 3);
    assert_eq!(an["best_of_n"]["successful_attempts"], 3);
    assert_eq!(an["best_of_n"]["selected_by_user"], 1);
    // Audit entry of the selection.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action='approval.attempt_selected'",
    )
    .fetch_one(&h.st.db)
    .await
    .unwrap();
    assert_eq!(n, 1);
}

/// Two selections at the same moment (two tabs, or user and review agent):
/// exactly one wins.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn concurrent_selections_pick_exactly_one(pool: PgPool) {
    let h = start(pool).await;
    let (a, p) = setup(&h, "race@x.ch", 2).await;
    let g = a
        .task_with(&p, "Zeile MOCK_FILE:r.md", json!({"attempts": 2}))
        .await;
    let v = wait_cmp(&a, &g, "candidates", |v| all(v, "candidate")).await;
    let (i1, i2) = (
        attempt(&v, 1)["id"].as_str().unwrap().to_string(),
        attempt(&v, 2)["id"].as_str().unwrap().to_string(),
    );
    let gid = Uuid::parse_str(&g).unwrap();
    let (x, y) = tokio::join!(
        queued::bon::select(
            &h.st,
            gid,
            Uuid::parse_str(&i1).unwrap(),
            queued_api::bon::SelectedBy::User,
            "a",
            None
        ),
        queued::bon::select(
            &h.st,
            gid,
            Uuid::parse_str(&i2).unwrap(),
            queued_api::bon::SelectedBy::Auto,
            "b",
            None
        )
    );
    let ok = [x.unwrap(), y.unwrap()]
        .iter()
        .filter(|r| r.is_ok())
        .count();
    assert_eq!(ok, 1, "exactly one selection");
    let v = wait_cmp(&a, &g, "settled", |v| {
        statuses(v).iter().filter(|s| *s == "done").count() == 1
            && statuses(v).iter().filter(|s| *s == "cancelled").count() == 1
    })
    .await;
    assert_eq!(
        v["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["selected"] == true)
            .count(),
        1
    );
}

/// The budget is one for all attempts: each attempt alone stays below it,
/// together they exceed it – the live ledger stops every attempt at once;
/// they end blocked (limit), the group too, nothing is selectable.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn group_budget_stops_all_attempts(pool: PgPool) {
    let h = start(pool).await;
    let (a, p) = setup(&h, "budget@x.ch", 2).await;
    // One attempt ≈ 3 deltas à ≈ 0.0079 $ (Opus prices) ≈ 0.024 $ < 0.03 $;
    // two attempts in parallel pass 0.03 $ after their second delta.
    let g = a
        .task_with(
            &p,
            "Teuer MOCK_TOKENS",
            json!({
                "attempts": 2,
                "attempt_targets": [
                    {"provider": "mock", "model": "claude-opus-5-5"},
                    {"provider": "mock", "model": "claude-opus-5-5"}
                ],
                "max_cost_usd": 0.03
            }),
        )
        .await;
    let gt = a
        .wait_status(&g, &["blocked", "failed", "comparing", "done"])
        .await;
    let v = wait_cmp(&a, &g, "both stopped", |v| all(v, "blocked")).await;
    let gt2 = a.wait_status(&g, &["blocked"]).await;
    assert_eq!(gt2["task"]["failure_class"], "limit_cost", "{gt} {gt2}");
    assert!(gt2["task"]["bon"]["aborted"]
        .as_str()
        .unwrap()
        .contains("Budget aller Versuche"));
    for n in [1, 2] {
        let at = attempt(&v, n);
        let t = a.task_json(at["id"].as_str().unwrap()).await;
        assert_eq!(t["task"]["failure_class"], "limit_cost", "{t}");
    }
    assert_eq!(v["can_select"], false);
    // Booked cost of the group: at most one delta per attempt past the limit.
    let gid = Uuid::parse_str(&g).unwrap();
    let spent = queued::bon::spent(&h.st, gid).await.unwrap();
    assert!(spent.cost_usd > 0.03 && spent.cost_usd < 0.05, "{spent:?}");
    let (_, an) = a.get("/api/analytics?days=7").await;
    assert_eq!(an["best_of_n"]["budget_stops"], 1);
}

/// Automatic selection: a review model (fake server) picks the winner; it is
/// at least as strong as the attempts; its cost is booked as `select`.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn automatic_selection_by_a_fake_review_model(pool: PgPool) {
    let fake = start_fake().await;
    let h = start(pool).await;
    let (a, p) = setup(&h, "auto@x.ch", 2).await;
    // Without an access for direct calls, automatic selection is refused.
    let (code, v) = a
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "x", "attempts": 2, "auto_select": true}),
        )
        .await;
    assert_eq!(code, 400, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("nie schwächer"),
        "{v}"
    );
    let (code, c) = a
        .post(
            "/api/connectors",
            json!({"kind": "mock", "label": "Prüfer", "config": {"base_url": fake.url, "api": "anthropic"}}),
        )
        .await;
    assert_eq!(code, 200, "{c}");
    let g = a
        .task_with(
            &p,
            "Zeile MOCK_FILE:auto.md",
            json!({
                "attempts": 2,
                "auto_select": true,
                "attempt_targets": [
                    {"provider": "mock", "model": "claude-sonnet-5-5"},
                    {"provider": "mock", "model": "claude-haiku-5-5"}
                ]
            }),
        )
        .await;
    let t = a.task_json(&g).await;
    let sel = t["task"]["bon"]["selector"].clone();
    assert_eq!(sel["connector_id"], c["id"]);
    let tier = queued::routing::tier_of(queued::pricing::builtin(), sel["model"].as_str().unwrap())
        .unwrap();
    assert!(
        tier >= queued_api::ModelTier::Mid,
        "never weaker than Sonnet: {sel}"
    );
    let gt = a.wait_status(&g, &["done"]).await;
    let choice = &gt["task"]["bon"]["choice"];
    assert_eq!(choice["by"], "auto", "{gt}");
    assert_eq!(
        choice["attempt_no"], 2,
        "the fake reviewer picks the highest number"
    );
    assert!(choice["reason"].as_str().unwrap().contains("Fake reviewer"));
    assert_eq!(fake.state.selections.load(Ordering::SeqCst), 1);
    let req = fake
        .requests()
        .into_iter()
        .find(|r| r.body.contains("senior reviewer"))
        .unwrap();
    assert!(
        req.body.contains("<<<USER_CANDIDATES_"),
        "candidates are fenced data"
    );
    let steps: Vec<String> = sqlx::query_scalar("SELECT step FROM usage_log WHERE task_id=$1")
        .bind(Uuid::parse_str(&g).unwrap())
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    assert_eq!(steps, vec!["select".to_string()]);
    let w = attempt(&cmp(&a, &g).await, 2)["id"]
        .as_str()
        .unwrap()
        .to_string();
    a.wait_status(&w, &["done"]).await;
}

/// Failed attempts get no fix task; no candidate ⇒ the group fails; a retry
/// starts a new round (the comparison shows only that round).
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn failed_attempts_get_no_fix_and_a_retry_starts_a_new_round(pool: PgPool) {
    let h = start(pool).await;
    let (a, p) = setup(&h, "fail@x.ch", 2).await;
    let g = a.task_with(&p, "MOCK_FAIL", json!({"attempts": 2})).await;
    let gt = a.wait_status(&g, &["failed"]).await;
    assert!(gt["task"]["error"]
        .as_str()
        .unwrap()
        .contains("Kein Versuch"));
    let v = cmp(&a, &g).await;
    assert!(all(&v, "failed"), "{v}");
    let fixes: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE kind='fix'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(fixes, 0, "no fix tasks for attempts");
    // An attempt of a finished group cannot be retried on its own.
    let first = attempt(&v, 1)["id"].as_str().unwrap().to_string();
    let (code, _) = a
        .post(&format!("/api/tasks/{first}/retry"), json!({}))
        .await;
    assert_eq!(code, 409);
    let (code, _) = a.post(&format!("/api/tasks/{g}/retry"), json!({})).await;
    assert_eq!(code, 200);
    a.wait_status(&g, &["failed"]).await;
    let v2 = cmp(&a, &g).await;
    assert_eq!(
        v2["attempts"].as_array().unwrap().len(),
        2,
        "latest round only"
    );
    assert_ne!(attempt(&v2, 1)["id"], attempt(&v, 1)["id"]);
}

/// Cancelling the group stops running attempts and removes their trees.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cancelling_the_group_stops_and_cleans_every_attempt(pool: PgPool) {
    let h = start(pool).await;
    let (a, p) = setup(&h, "cancel@x.ch", 2).await;
    let g = a.task_with(&p, "MOCK_SLOW", json!({"attempts": 2})).await;
    wait_cmp(&a, &g, "both running", |v| all(v, "running")).await;
    let (code, _) = a.post(&format!("/api/tasks/{g}/cancel"), json!({})).await;
    assert_eq!(code, 200);
    let v = wait_cmp(&a, &g, "cancelled", |v| all(v, "cancelled")).await;
    assert_eq!(a.task_json(&g).await["task"]["status"], "cancelled");
    for n in [1, 2] {
        let id = attempt(&v, n)["id"].as_str().unwrap().to_string();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while tws(&h, &id).exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "tree of {id} not removed"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Bounds: 2–8 attempts, admin quota, one access per attempt; the cost
/// preview is N × the estimate of one attempt.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn bounds_quota_and_cost_preview(pool: PgPool) {
    let h = start(pool).await;
    let (a, p) = setup(&h, "bounds@x.ch", 1).await;
    for (body, needle) in [
        (json!({"attempts": 9}), "2 bis 8"),
        (
            json!({"attempts": 2, "attempt_targets": [{"provider": "mock"}]}),
            "braucht es 2",
        ),
        (
            json!({"attempts": 2, "attempt_targets": [{"provider": "mock"}, {"provider": "nope"}]}),
            "Versuch 2",
        ),
    ] {
        let mut b = json!({"project_id": p, "prompt": "x"});
        for (k, v) in body.as_object().unwrap() {
            b[k] = v.clone();
        }
        let (code, v) = a.post("/api/tasks", b).await;
        assert_eq!(code, 400, "{v}");
        assert!(v["error"].as_str().unwrap().contains(needle), "{v}");
    }
    let uid: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email='bounds@x.ch'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_quotas (user_id, max_parallel) VALUES ($1, 2)")
        .bind(uid)
        .execute(&h.st.db)
        .await
        .unwrap();
    let (code, v) = a
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "x", "attempts": 3}),
        )
        .await;
    assert_eq!(code, 400);
    assert!(v["error"].as_str().unwrap().contains("Kontingent"), "{v}");

    let t = json!({"provider": "mock", "model": "claude-sonnet-5-5"});
    let (code, one) = a
        .post(
            "/api/estimate",
            json!({"project_id": p, "provider": "mock", "model": "claude-sonnet-5-5"}),
        )
        .await;
    assert_eq!(code, 200, "{one}");
    let (code, three) = a
        .post(
            "/api/estimate",
            json!({"project_id": p, "provider": "mock", "model": "claude-sonnet-5-5", "attempts": [t, t, t]}),
        )
        .await;
    assert_eq!(code, 200, "{three}");
    assert_eq!(three["attempts"].as_array().unwrap().len(), 3);
    let mid = |v: &Value| v["mid"][0]["amount"].as_f64().unwrap();
    assert!(
        (mid(&three) - 3.0 * mid(&one)).abs() < 1e-9,
        "{} vs {}",
        mid(&three),
        mid(&one)
    );
}

/// Routing rules: a small task runs on the cheap model of the rule, a large
/// one on the project route, an explicit choice wins; a failed task
/// escalates. Every decision is explained at the task.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn routing_rules_escalation_and_explanations(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("route@x.ch").await;
    a.settings(json!({"max_fix_attempts": 1})).await;
    let p = a.project(json!({"name": "route"})).await;
    let routing = json!({
        "execute": {"target": {"provider": "mock", "model": "claude-sonnet-5-5"}, "escalation": ["claude-opus-5-5"]},
        "review": {"mode": "harness"},
        "rules": [
            {"name": "Kleine Aufträge", "when": {"max_chars": 40}, "target": {"provider": "mock", "model": "claude-haiku-5-5"}},
            {"name": "Doku", "when": {"keywords": ["README"]}, "target": {"provider": "mock", "model": "claude-sonnet-5"}}
        ]
    });
    // A rule with an unknown access is refused.
    let mut bad = routing.clone();
    bad["rules"][0]["target"] = json!({"connector_id": Uuid::new_v4()});
    let (code, v) = a
        .put(
            &format!("/api/projects/{p}/routing"),
            json!({"routing": bad, "data_residency": "any"}),
        )
        .await;
    assert_eq!(code, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("Regel 1"), "{v}");
    let (code, v) = a
        .put(
            &format!("/api/projects/{p}/routing"),
            json!({"routing": routing, "data_residency": "any"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");

    let small = a.task(&p, "Tippfehler korrigieren").await;
    let t = a.task_json(&small).await["task"].clone();
    assert_eq!(t["model"], "claude-haiku-5-5");
    assert_eq!(t["routing_decision"]["source"], "rule");
    assert_eq!(t["routing_decision"]["rule"], "Kleine Aufträge");
    assert!(t["routing_decision"]["reason"]
        .as_str()
        .unwrap()
        .contains("höchstens 40 Zeichen"));
    let big = a
        .task(&p, "Eine ausführliche Beschreibung einer größeren Änderung mit vielen Details zur Umsetzung")
        .await;
    let t = a.task_json(&big).await["task"].clone();
    assert_eq!(t["model"], "claude-sonnet-5-5");
    assert_eq!(t["routing_decision"]["source"], "route");
    let doc = a
        .task(&p, "Bitte das README um einen langen Abschnitt zur Installation und Konfiguration ergänzen")
        .await;
    assert_eq!(a.task_json(&doc).await["task"]["model"], "claude-sonnet-5");
    let explicit = a
        .task_with(
            &p,
            "kurz",
            json!({"provider": "mock", "model": "claude-opus-5-5"}),
        )
        .await;
    let t = a.task_json(&explicit).await["task"].clone();
    assert_eq!(t["model"], "claude-opus-5-5");
    assert_eq!(t["routing_decision"]["source"], "explicit");

    // Escalation after a failure: the fix task runs on the next model.
    let failing = a.task(&p, "MOCK_FAIL hier").await;
    let ts = a
        .wait_until("fix task", |ts| ts.iter().any(|t| t["kind"] == "fix"))
        .await;
    let fix = ts.iter().find(|t| t["kind"] == "fix").unwrap();
    assert_eq!(fix["parent_id"], failing.as_str());
    assert_eq!(fix["model"], "claude-opus-5-5");
    assert_eq!(fix["routing_decision"]["source"], "escalation");
    assert!(fix["routing_decision"]["reason"]
        .as_str()
        .unwrap()
        .contains("claude-haiku-5-5 → claude-opus-5-5"));
}
