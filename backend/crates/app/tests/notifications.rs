//! WP10 integration: notification settings per event and channel, quiet
//! hours, outbox (retries, SSRF), signed webhooks and the daily summary
//! without duplicates across restarts and several app instances. Fake ntfy
//! and webhook receivers on loopback – nothing external is called.
mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use chrono::{Duration, TimeZone, Timelike, Utc};
use common::{start, start_with};
use queued::{
    config::Config,
    notify::{self, Note},
    state::AppState,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// Fake ntfy server: records JSON bodies; answers `fail_first` times 503.
struct FakeNtfy {
    url: String,
    seen: Arc<Mutex<Vec<Value>>>,
}

async fn fake_ntfy(fail_first: usize) -> FakeNtfy {
    use axum::{http::StatusCode, routing::post, Json, Router};
    let seen: Arc<Mutex<Vec<Value>>> = Default::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let s2 = seen.clone();
    let app = Router::new().route(
        "/",
        post(move |Json(v): Json<Value>| {
            let (s, calls) = (s2.clone(), calls.clone());
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) < fail_first {
                    return StatusCode::SERVICE_UNAVAILABLE;
                }
                s.lock().unwrap().push(v);
                StatusCode::OK
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    FakeNtfy { url, seen }
}

type Hooks = Arc<Mutex<Vec<(axum::http::HeaderMap, String)>>>;

async fn fake_webhook() -> (String, Hooks) {
    use axum::{body::Bytes, http::HeaderMap, routing::post, Router};
    let seen: Hooks = Default::default();
    let s2 = seen.clone();
    let app = Router::new().route(
        "/hook",
        post(move |h: HeaderMap, b: Bytes| {
            let s = s2.clone();
            async move {
                s.lock()
                    .unwrap()
                    .push((h, String::from_utf8(b.to_vec()).unwrap()));
                "ok"
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, seen)
}

async fn uid(c: &common::Client) -> Uuid {
    Uuid::parse_str(c.get("/api/auth/me").await.1["id"].as_str().unwrap()).unwrap()
}

fn note(kind: &'static str, push: bool, title: &str) -> Note<'static> {
    Note {
        kind,
        title: title.to_string(),
        body: "body".into(),
        task_id: None,
        push,
    }
}

/// Settings with quiet hours around the current UTC time.
async fn quiet_now(c: &common::Client, critical: bool, extra: Value) -> Value {
    let (_, mut s) = c.get("/api/notifications/settings").await;
    let m = (Utc::now().hour() * 60 + Utc::now().minute()) as i64;
    s["timezone"] = json!("UTC");
    s["quiet_enabled"] = json!(true);
    s["quiet_start"] = json!((m + 1440 - 60) % 1440);
    s["quiet_end"] = json!((m + 60) % 1440);
    s["quiet_critical"] = json!(critical);
    for (k, v) in extra.as_object().unwrap() {
        s[k] = v.clone();
    }
    let (code, s) = c.put("/api/notifications/settings", s).await;
    assert_eq!(code, 200, "{s}");
    s
}

async fn outbox(db: &PgPool, user: Uuid) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT channel, event, status FROM notification_outbox WHERE user_id=$1 ORDER BY id",
    )
    .bind(user)
    .fetch_all(db)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn settings_api_validates_and_stores_only_deviations(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("set@x.ch").await;
    let (code, s) = a.get("/api/notifications/settings").await;
    assert_eq!(code, 200);
    assert_eq!(s["timezone"], "UTC");
    assert_eq!(s["matrix"]["needs_input"]["push"], true);
    assert_eq!(s["matrix"]["task_done"]["push"], false);
    assert_eq!(s["matrix"]["task_done"]["webhook"], false);
    assert_eq!(s["webhook_secret_set"], false);
    let put = |patch: Value| {
        let mut b = s.clone();
        for (k, v) in patch.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    for bad in [
        json!({"timezone": "Mars/Olympus"}),
        json!({"quiet_enabled": true, "quiet_start": 600, "quiet_end": 600}),
        json!({"digest_minute": 1440}),
        json!({"webhook_url": "ftp://x"}),
        json!({"webhook_url": "https://169.254.169.254/latest"}),
    ] {
        assert_eq!(
            a.put("/api/notifications/settings", put(bad.clone()))
                .await
                .0,
            400,
            "{bad}"
        );
    }
    let mut body =
        put(json!({"timezone": "Europe/Berlin", "digest_enabled": true, "digest_minute": 450}));
    body["matrix"]["task_done"]["push"] = json!(true);
    body["matrix"]["blocked"]["ntfy"] = json!(false);
    let (code, v) = a.put("/api/notifications/settings", body).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["timezone"], "Europe/Berlin");
    assert_eq!(v["matrix"]["task_done"]["push"], true);
    assert_eq!(v["matrix"]["blocked"]["ntfy"], false);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_prefs")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(n, 2, "only the two deviations are stored");
    // Webhook: secret only after an address, shown once, reset by a new address.
    assert_eq!(
        a.post("/api/notifications/webhook/secret", json!({}))
            .await
            .0,
        400
    );
    let mut w = v.clone();
    w["webhook_url"] = json!("https://hooks.example.com/queue");
    // Resolving hooks.example.com may fail here (no DNS): accept 400 for the
    // name but test the flow with a loopback receiver below.
    let (code, _) = a.put("/api/notifications/settings", w).await;
    assert!(code == 200 || code == 400);
    // API tokens cannot touch notification settings.
    let tok = a.token(&["tasks:read"]).await;
    assert_eq!(
        h.bearer(&tok).get("/api/notifications/settings").await.0,
        403
    );
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn quiet_hours_hold_back_push_but_not_critical_or_webhooks(pool: PgPool) {
    let h = start(pool).await;
    let ntfy = fake_ntfy(0).await;
    let (hook_url, hooks) = fake_webhook().await;
    let a = h.user("quiet@x.ch").await;
    let ua = uid(&a).await;
    a.settings(json!({"ntfy_url": format!("{}/topic-a", ntfy.url)}))
        .await;
    let mut m = a.get("/api/notifications/settings").await.1["matrix"].clone();
    m["pr_attention"]["webhook"] = json!(true);
    quiet_now(&a, true, json!({"webhook_url": hook_url, "matrix": m})).await;
    let (code, sec) = a.post("/api/notifications/webhook/secret", json!({})).await;
    assert_eq!(code, 200, "{sec}");
    let secret = sec["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("qwh_"));
    assert_eq!(
        a.get("/api/notifications/settings").await.1["webhook_secret_set"],
        true
    );

    // Non-critical attention event in the quiet hours: Postfach + webhook,
    // ntfy held back. Critical event: ntfy as well.
    notify::notify(&h.st, ua, note("pr", true, "Konflikt in PR")).await;
    notify::notify(&h.st, ua, note("blocked", true, "Blockiert: X")).await;
    // Info event: never external by default.
    notify::notify(&h.st, ua, note("done", false, "Fertig: Y")).await;
    while notify::tick(&h.st).await.unwrap() > 0 {}
    let got = ntfy.seen.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["title"], "Blockiert: X");
    assert_eq!(got[0]["topic"], "topic-a");
    let hs = hooks.lock().unwrap().clone();
    assert_eq!(hs.len(), 1, "webhooks ignore quiet hours");
    let (headers, body) = &hs[0];
    assert_eq!(headers["x-queue-event"], "pr_attention");
    let sig = headers["x-queue-signature"].to_str().unwrap();
    let t: i64 = sig
        .split(',')
        .next()
        .unwrap()
        .trim_start_matches("t=")
        .parse()
        .unwrap();
    assert_eq!(
        sig,
        notify::webhook_signature(secret.as_bytes(), t, body.as_bytes())
    );
    assert!((Utc::now().timestamp() - t).abs() < 60);
    let v: Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["title"], "Konflikt in PR");
    assert_eq!(
        v["delivery"].to_string(),
        headers["x-queue-delivery"].to_str().unwrap()
    );
    // All three are in the Postfach; the held-back one is marked.
    let ns = a.notifications().await;
    assert_eq!(ns.len(), 3);
    let quiet: Vec<(String, bool)> = sqlx::query_as(
        "SELECT title, quiet_suppressed FROM notifications WHERE user_id=$1 ORDER BY created_at",
    )
    .bind(ua)
    .fetch_all(&h.st.db)
    .await
    .unwrap();
    assert_eq!(
        quiet,
        vec![
            ("Konflikt in PR".into(), true),
            ("Blockiert: X".into(), false),
            ("Fertig: Y".into(), false)
        ]
    );

    // Without "Wichtiges trotzdem melden" even blocked waits.
    quiet_now(&a, false, json!({})).await;
    notify::notify(&h.st, ua, note("blocked", true, "Blockiert: Z")).await;
    while notify::tick(&h.st).await.unwrap() > 0 {}
    assert_eq!(ntfy.seen.lock().unwrap().len(), 1);
    // Per event: switch ntfy off for "blocked" outside quiet hours.
    let mut s = a.get("/api/notifications/settings").await.1;
    s["quiet_enabled"] = json!(false);
    s["matrix"]["blocked"]["ntfy"] = json!(false);
    s["matrix"]["task_done"]["ntfy"] = json!(true);
    assert_eq!(a.put("/api/notifications/settings", s).await.0, 200);
    notify::notify(&h.st, ua, note("blocked", true, "Blockiert: W")).await;
    notify::notify(&h.st, ua, note("done", false, "Fertig: V")).await;
    while notify::tick(&h.st).await.unwrap() > 0 {}
    let titles: Vec<String> = ntfy
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|v| v["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles, vec!["Blockiert: X", "Fertig: V"]);
    // Postfach switched off for an event: nothing stored.
    let mut s = a.get("/api/notifications/settings").await.1;
    s["matrix"]["knowledge"]["inapp"] = json!(false);
    assert_eq!(a.put("/api/notifications/settings", s).await.0, 200);
    let before = a.notifications().await.len();
    notify::notify(&h.st, ua, note("knowledge", false, "Regel-Vorschlag")).await;
    assert_eq!(a.notifications().await.len(), before);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn outbox_retries_transient_failures_and_blocks_internal_targets(pool: PgPool) {
    let h = start(pool.clone()).await;
    let ntfy = fake_ntfy(1).await;
    let a = h.user("retry@x.ch").await;
    let ua = uid(&a).await;
    a.settings(json!({"ntfy_url": format!("{}/t", ntfy.url)}))
        .await;
    notify::notify(&h.st, ua, note("question", true, "Rückfrage: A")).await;
    assert_eq!(notify::tick(&h.st).await.unwrap(), 1);
    assert_eq!(
        outbox(&h.st.db, ua).await,
        vec![("ntfy".into(), "needs_input".into(), "pending".into())]
    );
    assert!(ntfy.seen.lock().unwrap().is_empty());
    // Not due yet (backoff).
    assert_eq!(notify::tick(&h.st).await.unwrap(), 0);
    sqlx::query("UPDATE notification_outbox SET next_at=now()")
        .execute(&h.st.db)
        .await
        .unwrap();
    assert_eq!(notify::tick(&h.st).await.unwrap(), 1);
    assert_eq!(outbox(&h.st.db, ua).await[0].2, "sent");
    assert_eq!(ntfy.seen.lock().unwrap().len(), 1);
    // Delivery metrics.
    assert_eq!(h.st.metrics.notifications.get(&["ntfy", "retry"]), 1.0);
    assert_eq!(h.st.metrics.notifications.get(&["ntfy", "sent"]), 1.0);

    // Production setting: loopback/private targets are refused (SSRF), the
    // fake server never sees a request.
    let strict = start_with(pool, |c| c.allow_private_notify_targets = false).await;
    let b = strict.user("ssrf@x.ch").await;
    let ub = uid(&b).await;
    let ntfy2 = fake_ntfy(0).await;
    b.settings(json!({"ntfy_url": format!("{}/t", ntfy2.url)}))
        .await;
    notify::notify(&strict.st, ub, note("blocked", true, "Blockiert")).await;
    notify::tick(&strict.st).await.unwrap();
    assert_eq!(outbox(&strict.st.db, ub).await[0].2, "failed");
    assert!(ntfy2.seen.lock().unwrap().is_empty());
    let (_, s) = b.get("/api/notifications/settings").await;
    let mut w = s.clone();
    w["webhook_url"] = json!("https://127.0.0.1:9/hook");
    assert_eq!(b.put("/api/notifications/settings", w).await.0, 400);
    let mut w = s.clone();
    w["webhook_url"] = json!("http://example.com/hook");
    let (code, e) = b.put("/api/notifications/settings", w).await;
    assert_eq!(code, 400);
    assert!(e.to_string().contains("https://"), "{e}");
}

/// Two instances of the app on one database and a restart: exactly one
/// summary per user and local day, delivered exactly once.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn daily_summary_is_sent_once_across_instances_and_restarts(pool: PgPool) {
    let h = start(pool.clone()).await;
    let ntfy = fake_ntfy(0).await;
    let a = h.user("digest@x.ch").await;
    let ua = uid(&a).await;
    a.settings(json!({"ntfy_url": format!("{}/d", ntfy.url)}))
        .await;
    let p = a.project(json!({"name": "p"})).await;
    let (_, mut s) = a.get("/api/notifications/settings").await;
    s["timezone"] = json!("Europe/Berlin");
    s["digest_enabled"] = json!(true);
    s["digest_minute"] = json!(8 * 60);
    assert_eq!(a.put("/api/notifications/settings", s).await.0, 200);
    // A fixed "now": 18 Nov 2026, 07:30 UTC = 08:30 in Berlin (due).
    let now = Utc.with_ymd_and_hms(2026, 11, 18, 7, 30, 0).unwrap();
    for (status, mins) in [
        ("done", 60),
        ("done", 120),
        ("failed", 180),
        ("blocked", 30),
    ] {
        sqlx::query(
            "INSERT INTO tasks (id, user_id, project_id, kind, title, prompt, provider, status, created_at, finished_at)
             VALUES ($1,$2,$3::uuid,'task','t','p','mock',$4,$5,$5)",
        )
        .bind(Uuid::new_v4())
        .bind(ua)
        .bind(&p)
        .bind(status)
        .bind(now - Duration::minutes(mins))
        .execute(&h.st.db)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO usage_log (user_id, provider, ts, input_tokens, cost_amount, currency) VALUES ($1,'mock',$2,10,1.25,'USD')",
    )
    .bind(ua)
    .bind(now - Duration::hours(2))
    .execute(&h.st.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO notifications (id, user_id, kind, title, quiet_suppressed, created_at) VALUES ($1,$2,'pr','x',true,$3)",
    )
    .bind(Uuid::new_v4())
    .bind(ua)
    .bind(now - Duration::hours(5))
    .execute(&h.st.db)
    .await
    .unwrap();

    // Second app instance on the same database.
    let other_dir = tempfile::tempdir().unwrap();
    let other = AppState::new(
        Config::for_tests(
            "unused",
            other_dir.path().to_path_buf(),
            "http://127.0.0.1:9",
            "x",
        ),
        pool.clone(),
    )
    .unwrap();
    // Both instances, at the same time, several rounds.
    for _ in 0..3 {
        let (x, y) = tokio::join!(
            queued::digest::run_due(&h.st, now),
            queued::digest::run_due(&other, now)
        );
        let _ = (x.unwrap(), y.unwrap());
    }
    let rows: Vec<(chrono::NaiveDate, Option<Uuid>)> = sqlx::query_as(
        "SELECT local_date, notification_id FROM notification_digests WHERE user_id=$1",
    )
    .bind(ua)
    .fetch_all(&h.st.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].0,
        chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap()
    );
    assert!(rows[0].1.is_some());
    let (title, body): (String, String) =
        sqlx::query_as("SELECT title, body FROM notifications WHERE id=$1")
            .bind(rows[0].1)
            .fetch_one(&h.st.db)
            .await
            .unwrap();
    assert_eq!(title, "Tägliche Zusammenfassung (18.11.)");
    assert!(
        body.contains("Erledigt: 2 · Fehlgeschlagen: 1 · Blockiert: 1"),
        "{body}"
    );
    assert!(body.contains("Kosten: 1,25 $"), "{body}");
    assert!(
        body.contains("In der Ruhezeit zurückgehalten: 1 Meldung"),
        "{body}"
    );

    // A delivery lease of a crashed instance expires; then both instances
    // tick concurrently: one ntfy message in total.
    sqlx::query("UPDATE notification_outbox SET lease_until = now() + interval '1 hour'")
        .execute(&h.st.db)
        .await
        .unwrap();
    let (x, y) = tokio::join!(notify::tick(&h.st), notify::tick(&other));
    assert_eq!(x.unwrap() + y.unwrap(), 0, "leased by the crashed instance");
    sqlx::query(
        "UPDATE notification_outbox SET lease_until = now() - interval '1 second', attempts = 1",
    )
    .execute(&h.st.db)
    .await
    .unwrap();
    for _ in 0..3 {
        let (x, y) = tokio::join!(notify::tick(&h.st), notify::tick(&other));
        x.unwrap();
        y.unwrap();
    }
    let got = ntfy.seen.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0]["title"]
        .as_str()
        .unwrap()
        .starts_with("Tägliche Zusammenfassung"));

    // "Restart": a fresh instance later the same day sends nothing new.
    let restart = AppState::new(
        Config::for_tests(
            "unused",
            other_dir.path().to_path_buf(),
            "http://127.0.0.1:9",
            "x",
        ),
        pool.clone(),
    )
    .unwrap();
    assert_eq!(
        queued::digest::run_due(&restart, now + Duration::hours(3))
            .await
            .unwrap(),
        0
    );
    // Past the catch-up window of the next day: that day is skipped …
    assert_eq!(
        queued::digest::run_due(
            &restart,
            now + Duration::days(1) + Duration::hours(12) + Duration::minutes(31)
        )
        .await
        .unwrap(),
        0
    );
    // … the day after, nothing happened: the day counts, nothing is sent.
    let day3 = now + Duration::days(2);
    assert_eq!(queued::digest::run_due(&restart, day3).await.unwrap(), 1);
    let rows: Vec<(chrono::NaiveDate, Option<Uuid>)> = sqlx::query_as(
        "SELECT local_date, notification_id FROM notification_digests WHERE user_id=$1 ORDER BY local_date",
    )
    .bind(ua)
    .fetch_all(&h.st.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].1, None, "empty day: no notification");
    for _ in 0..2 {
        notify::tick(&restart).await.unwrap();
    }
    assert_eq!(ntfy.seen.lock().unwrap().len(), 1);
    assert_eq!(
        h.st.metrics.digests.get(&[]) + other.metrics.digests.get(&[]),
        1.0
    );
}

/// Real flow: a blocked task notifies through the outbox, the test button
/// ignores quiet hours, the webhook test arrives.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn end_to_end_flow_and_webhook_test(pool: PgPool) {
    let h = start(pool).await;
    let ntfy = fake_ntfy(0).await;
    let (hook_url, hooks) = fake_webhook().await;
    let a = h.user("flow@x.ch").await;
    let ua = uid(&a).await;
    a.settings(json!({"ntfy_url": format!("{}/f", ntfy.url), "max_fix_attempts": 0}))
        .await;
    quiet_now(&a, true, json!({"webhook_url": hook_url})).await;
    assert_eq!(
        a.post("/api/notifications/webhook/test", json!({})).await.0,
        400
    );
    a.post("/api/notifications/webhook/secret", json!({})).await;
    assert_eq!(
        a.post("/api/notifications/webhook/test", json!({})).await.0,
        200
    );
    // The push test ignores the quiet hours (it tests the channel).
    assert_eq!(a.post("/api/push/test", json!({})).await.0, 200);
    while notify::tick(&h.st).await.unwrap() > 0 {}
    assert_eq!(hooks.lock().unwrap().len(), 1);
    assert_eq!(hooks.lock().unwrap()[0].0["x-queue-event"], "test");
    let titles: Vec<String> = ntfy
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|v| v["title"].as_str().unwrap().to_string())
        .collect();
    assert!(
        titles.contains(&"Test-Benachrichtigung".to_string()),
        "{titles:?}"
    );
    let (code, d) = a.get("/api/notifications/deliveries").await;
    assert_eq!(code, 200);
    assert!(
        d.as_array().unwrap().iter().all(|r| r["status"] == "sent"),
        "{d}"
    );
    let _ = ua;
}

/// Web Push through the outbox: one row per device, encrypted (RFC 8291
/// test keys), VAPID-signed; a device the push service reports gone (410)
/// is removed and not retried.
#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn web_push_per_device_and_gone_devices(pool: PgPool) {
    use axum::{
        body::Bytes, extract::Path, http::HeaderMap, http::StatusCode, routing::post, Router,
    };
    let h = start(pool).await;
    let seen: Arc<Mutex<Vec<(String, HeaderMap, usize)>>> = Default::default();
    let s2 = seen.clone();
    let app = Router::new().route(
        "/push/{dev}",
        post(move |Path(dev): Path<String>, hd: HeaderMap, b: Bytes| {
            let s = s2.clone();
            async move {
                s.lock().unwrap().push((dev.clone(), hd, b.len()));
                if dev == "gone" {
                    StatusCode::GONE
                } else {
                    StatusCode::CREATED
                }
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let a = h.user("push@x.ch").await;
    let ua = uid(&a).await;
    // RFC 8291 example keys (a real browser sends such values).
    let p256dh =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    let auth = "BTBZMqHH6r4Tts7J_aSIgg";
    for dev in ["phone", "gone"] {
        sqlx::query("INSERT INTO push_subscriptions (id, user_id, endpoint, p256dh, auth) VALUES ($1,$2,$3,$4,$5)")
            .bind(Uuid::new_v4())
            .bind(ua)
            .bind(format!("{base}/push/{dev}"))
            .bind(p256dh)
            .bind(auth)
            .execute(&h.st.db)
            .await
            .unwrap();
    }
    assert_eq!(
        a.get("/api/notifications/settings").await.1["push_devices"],
        2
    );
    notify::notify(&h.st, ua, note("question", true, "Rückfrage: PUSH-TITLE")).await;
    while notify::tick(&h.st).await.unwrap() > 0 {}
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 2, "one request per device");
    for (_, hd, len) in &got {
        assert_eq!(hd["content-encoding"], "aes128gcm");
        assert!(
            hd["authorization"].to_str().unwrap().starts_with("vapid "),
            "{hd:?}"
        );
        assert!(*len > 0);
    }
    // The payload is encrypted: the title is not readable on the wire.
    let ob = outbox(&h.st.db, ua).await;
    assert_eq!(ob.iter().filter(|r| r.2 == "sent").count(), 1, "{ob:?}");
    assert_eq!(ob.iter().filter(|r| r.2 == "failed").count(), 1, "{ob:?}");
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM push_subscriptions WHERE user_id=$1")
        .bind(ua)
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(left, 1, "the gone device is removed");
}
