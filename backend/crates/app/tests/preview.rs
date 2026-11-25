//! WP12 integration tests: live preview through the app's proxy on an own
//! origin (members only, CSP sandbox, no app credentials to the dev server,
//! no cookies from it, never the API on a preview host, stop and expiry) and
//! screenshots after UI changes (real headless Chromium, engine `none`).
mod common;

use std::path::PathBuf;

use common::{start_full, Client, Harness};
use queued::config::PreviewOrigin;
use serde_json::{json, Value};
use sqlx::PgPool;

/// Dev server: answers every request with its method, URL and headers and
/// tries to set a cookie and its own frame/CSP policy.
const ECHO: &str = "exec node -e \"require('http').createServer((q,s)=>{s.setHeader('Set-Cookie','qsession=evil; Path=/');s.setHeader('X-Frame-Options','DENY');s.setHeader('Content-Security-Policy','default-src *');s.setHeader('Content-Type','application/json');s.end(JSON.stringify({url:q.url,method:q.method,h:q.headers}))}).listen(process.env.PORT,'127.0.0.1')\"";

fn chromium() -> PathBuf {
    if let Ok(p) = std::env::var("RUNNER_CHROMIUM") {
        return PathBuf::from(p);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir("/opt/pw-browsers")
        .expect("Chromium under /opt/pw-browsers (or RUNNER_CHROMIUM)")
        .filter_map(|e| e.ok())
        .map(|e| {
            e.path()
                .join("chrome-headless-shell-linux64/chrome-headless-shell")
        })
        .filter(|p| p.exists())
        .collect();
    found.sort();
    found.pop().expect("chrome-headless-shell")
}

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
}

async fn boot(pool: PgPool) -> Harness {
    start_full(
        pool,
        |c| {
            c.preview_origin =
                Some(PreviewOrigin::parse("http://*.preview.test", "http://localhost").unwrap())
        },
        |r| {
            r.chromium = Some(chromium());
            r.chromium_no_sandbox = is_root();
        },
    )
    .await
}

async fn project(a: &Client, preview: Value) -> String {
    let p = a.project(json!({"name": "pv"})).await;
    a.patch(
        &format!("/api/projects/{p}"),
        json!({"review_enabled": false}),
    )
    .await;
    let mut s = a.get(&format!("/api/projects/{p}/preview")).await.1["settings"].clone();
    for (k, v) in preview.as_object().unwrap() {
        s[k] = v.clone();
    }
    let (code, v) = a.put(&format!("/api/projects/{p}/preview"), s).await;
    assert_eq!(code, 200, "{v}");
    p
}

/// A candidate of a Best-of-N task (its working tree stays).
async fn candidate(a: &Client, p: &str) -> String {
    let g = a
        .task_with(p, "Zeile MOCK_FILE:index.html", json!({"attempts": 2}))
        .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let (_, v) = a.get(&format!("/api/tasks/{g}/attempts")).await;
        if let Some(c) = v["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["status"] == "candidate")
        {
            return c["id"].as_str().unwrap().to_string();
        }
        assert!(tokio::time::Instant::now() < deadline, "no candidate: {v}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn via_host(h: &Harness, host: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", h.base))
        .header("host", host)
        .header("cookie", "qsession=app-session-secret")
        .header("authorization", "Bearer app-token-secret")
        .send()
        .await
        .unwrap()
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn live_preview_is_member_only_sandboxed_and_never_sees_app_credentials(pool: PgPool) {
    let h = boot(pool).await;
    let a = h.user("owner@x.ch").await;
    let b = h.user("viewer@x.ch").await;
    let c = h.user("stranger@x.ch").await;
    a.settings(json!({"parallelism": 2})).await;
    let p = project(&a, json!({"enabled": true, "command": ECHO, "port": 4321})).await;
    assert_eq!(
        a.post(
            &format!("/api/projects/{p}/members"),
            json!({"email": "viewer@x.ch", "role": "viewer"})
        )
        .await
        .0,
        200
    );
    let t = candidate(&a, &p).await;
    // Only editors start it; non-members do not even see the task.
    assert_eq!(
        c.post(&format!("/api/tasks/{t}/preview"), json!({}))
            .await
            .0,
        404
    );
    assert_eq!(
        b.post(&format!("/api/tasks/{t}/preview"), json!({}))
            .await
            .0,
        403
    );
    let (code, v) = a.post(&format!("/api/tasks/{t}/preview"), json!({})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["status"], "ready", "{v}");
    let url = v["url"].as_str().unwrap().to_string();
    let host = url
        .strip_prefix("http://")
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    assert!(host.ends_with(".preview.test"), "{url}");
    let cap = host.split('.').next().unwrap().to_string();
    assert_eq!(cap.len(), 26);
    // Members see the link, others nothing; the capability is never stored in clear.
    assert_eq!(
        b.get(&format!("/api/tasks/{t}/preview")).await.1["url"],
        url.as_str()
    );
    assert_eq!(c.get(&format!("/api/tasks/{t}/preview")).await.0, 404);
    let raw: Vec<Vec<u8>> = sqlx::query_scalar("SELECT cap_enc FROM previews")
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    assert!(raw
        .iter()
        .all(|r| !String::from_utf8_lossy(r).contains(&cap)));

    // Through the proxy: the dev server's answer, without the app's
    // credentials, without its cookie, inside a CSP sandbox.
    let r = via_host(&h, &host, "/seite?x=1").await;
    assert_eq!(r.status(), 200);
    let hd = r.headers().clone();
    let csp = hd["content-security-policy"].to_str().unwrap().to_string();
    assert!(csp.starts_with("sandbox allow-scripts"), "{csp}");
    assert!(!csp.contains("allow-same-origin"));
    assert!(csp.contains("frame-ancestors http://localhost"), "{csp}");
    assert!(
        hd.get("set-cookie").is_none(),
        "no cookies from the preview"
    );
    assert!(hd.get("x-frame-options").is_none());
    assert_eq!(hd["x-content-type-options"], "nosniff");
    assert_eq!(hd["referrer-policy"], "no-referrer");
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["url"], "/seite?x=1");
    let seen = body["h"].as_object().unwrap();
    assert!(
        !seen.contains_key("cookie") && !seen.contains_key("authorization"),
        "{seen:?}"
    );
    // A preview host never reaches the API or the SPA.
    let r = via_host(&h, &host, "/api/auth/me").await;
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["url"], "/api/auth/me", "answered by the dev server");
    // Unknown capability on the preview domain: 404, still sandboxed.
    let r = via_host(&h, "abcdefghijklmnopqrstuvwxyz.preview.test", "/").await;
    assert_eq!(r.status(), 404);
    assert!(r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .starts_with("sandbox"));
    let r = via_host(&h, "nope.preview.test", "/api/auth/me").await;
    assert_eq!(r.status(), 404);
    // The app itself is unaffected.
    assert_eq!(a.get("/api/auth/me").await.0, 200);

    // Stop: the link is dead at once.
    assert_eq!(b.delete(&format!("/api/tasks/{t}/preview")).await.0, 403);
    assert_eq!(a.delete(&format!("/api/tasks/{t}/preview")).await.0, 200);
    assert_eq!(via_host(&h, &host, "/").await.status(), 404);
    assert!(a.get(&format!("/api/tasks/{t}/preview")).await.1.is_null());
    let names =
        h.st.runner(uuid::Uuid::parse_str(&p).unwrap())
            .await
            .previews()
            .await
            .unwrap();
    assert!(names.names.is_empty(), "process stopped at the runner");

    // Expiry: a preview past its lifetime answers 404 and is stopped.
    let (_, v) = a.post(&format!("/api/tasks/{t}/preview"), json!({})).await;
    let host2 = v["url"]
        .as_str()
        .unwrap()
        .strip_prefix("http://")
        .unwrap()
        .trim_end_matches('/')
        .to_string();
    assert_eq!(via_host(&h, &host2, "/").await.status(), 200);
    sqlx::query(
        "UPDATE previews SET expires_at = now() - interval '1 second' WHERE status='ready'",
    )
    .execute(&h.st.db)
    .await
    .unwrap();
    assert_eq!(via_host(&h, &host2, "/").await.status(), 404);
    queued::preview::sweep(&h.st).await.unwrap();
    let names =
        h.st.runner(uuid::Uuid::parse_str(&p).unwrap())
            .await
            .previews()
            .await
            .unwrap();
    assert!(names.names.is_empty());

    // Not while an agent works or after the tree is gone.
    let done = a.task(&p, "normal").await;
    a.wait_status(&done, &["done"]).await;
    let (code, v) = a
        .post(&format!("/api/tasks/{done}/preview"), json!({}))
        .await;
    assert_eq!(code, 400, "{v}");
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn screenshots_after_ui_changes_are_real_pngs_for_members_only(pool: PgPool) {
    let h = boot(pool).await;
    let a = h.user("shots@x.ch").await;
    let c = h.user("nobody@x.ch").await;
    let p = project(
        &a,
        json!({
            "enabled": true,
            "command": "exec python3 -m http.server \"$PORT\" --bind 127.0.0.1",
            "port": 4322,
            "ui_globs": ["**/*.html"],
            "screenshot_paths": ["/"]
        }),
    )
    .await;
    // A change that is no UI change: no screenshots.
    let t0 = a.task(&p, "Zeile MOCK_FILE:notes.md").await;
    let d0 = a.wait_status(&t0, &["done"]).await;
    assert!(d0["screenshots"].as_array().unwrap().is_empty());
    // A UI change: desktop and mobile.
    let t = a.task(&p, "Zeile MOCK_FILE:index.html").await;
    let d = a.wait_status(&t, &["done"]).await;
    let shots = d["screenshots"].as_array().unwrap().clone();
    assert_eq!(shots.len(), 2, "{d}");
    let mut seen = vec![];
    for s in &shots {
        let url = s["url"].as_str().unwrap();
        let r = a.http.get(format!("{}{url}", a.base)).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "image/png");
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
        let png = r.bytes().await.unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
        assert_eq!(w as u64, s["width"].as_u64().unwrap());
        seen.push((s["viewport"].as_str().unwrap().to_string(), w));
        assert_eq!(
            c.http
                .get(format!("{}{url}", c.base))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    seen.sort();
    assert_eq!(
        seen,
        vec![("desktop".to_string(), 1280), ("mobile".to_string(), 780)]
    );
    let ev = a.events_of(&t).await;
    assert!(
        ev.iter()
            .any(|(_, c)| c.contains("2 Screenshots aufgenommen")),
        "{ev:?}"
    );
}
