//! WP11 integration tests: MFA (TOTP, recovery codes, passkeys), sessions,
//! brute-force throttle, account lock, last admin, invitations and
//! registration modes, quotas, audit log (append-only, hash chain, no
//! secrets). App + runner in-process, fresh Postgres per test.
mod common;

use std::{sync::Arc, time::Duration};

use common::{Client, Harness};
use futures::StreamExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const PW: &str = "password123";

fn totp_now(secret_b32: &str, offset_steps: i64) -> String {
    let secret = queued::mfa::base32_decode(secret_b32).unwrap();
    let step = queued::mfa::step_at(chrono::Utc::now().timestamp()) + offset_steps;
    format!("{:06}", queued::mfa::hotp(&secret, step as u64, 6))
}

/// Client with an inspectable cookie jar.
fn jar_client(h: &Harness) -> (Client, Arc<reqwest::cookie::Jar>) {
    let jar = Arc::new(reqwest::cookie::Jar::default());
    (
        Client {
            base: h.base.clone(),
            http: reqwest::Client::builder()
                .cookie_provider(jar.clone())
                .build()
                .unwrap(),
        },
        jar,
    )
}

fn cookie(h: &Harness, jar: &reqwest::cookie::Jar, name: &str) -> Option<String> {
    use reqwest::cookie::CookieStore;
    let url: url::Url = format!("{}/api/auth/me", h.base).parse().unwrap();
    let hv = jar.cookies(&url)?;
    hv.to_str()
        .unwrap()
        .split("; ")
        .find_map(|c| c.strip_prefix(&format!("{name}=")).map(String::from))
}

async fn login(c: &Client, email: &str, pw: &str) -> (u16, Value) {
    c.post("/api/auth/login", json!({"email": email, "password": pw}))
        .await
}

/// Sets up the authenticator app; returns (secret, recovery codes, code used).
async fn enable_totp(c: &Client) -> (String, Vec<String>) {
    let (code, setup) = c
        .post("/api/account/mfa/totp/setup", json!({"password": PW}))
        .await;
    assert_eq!(code, 200, "{setup}");
    let secret = setup["secret"].as_str().unwrap().to_string();
    assert!(setup["uri"]
        .as_str()
        .unwrap()
        .starts_with("otpauth://totp/"));
    let (code, done) = c
        .post(
            "/api/account/mfa/totp/confirm",
            json!({"code": totp_now(&secret, 0)}),
        )
        .await;
    assert_eq!(code, 200, "{done}");
    let codes: Vec<String> = done["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes.len(), 10);
    (secret, codes)
}

/// TOTP login, replay protection (same and older time steps), throttles
/// for the second factor and for passwords; no session before the second
/// factor; the second step is single use.
#[sqlx::test]
async fn totp_login_replay_and_lockout(pool: PgPool) {
    let h = common::start(pool).await;
    let a = h.user("admin@x.org").await;
    let u = h.user("tom@x.org").await;
    let (secret, _codes) = enable_totp(&u).await;
    let used = totp_now(&secret, 0);

    let (c, jar) = jar_client(&h);
    let (code, v) = login(&c, "tom@x.org", PW).await;
    assert_eq!(code, 200);
    assert_eq!(v["ok"], false);
    assert_eq!(v["mfa_required"], true);
    assert_eq!(v["methods"], json!(["totp", "recovery"]));
    assert!(
        cookie(&h, &jar, "qsid").is_none(),
        "no session before the second factor"
    );
    assert_eq!(c.get("/api/tasks").await.0, 401);
    assert_eq!(c.get("/api/auth/mfa").await.0, 200);

    // Replay: the code of the confirmation is used up.
    let (code, v) = c.post("/api/auth/mfa/totp", json!({"code": used})).await;
    assert_eq!(code, 401, "replayed code accepted: {v}");
    // An older step is not accepted either.
    let (code, _) = c
        .post("/api/auth/mfa/totp", json!({"code": totp_now(&secret, -1)}))
        .await;
    assert_eq!(code, 401);
    // The next step is (window ±1).
    let next = totp_now(&secret, 1);
    let (code, v) = c.post("/api/auth/mfa/totp", json!({"code": next})).await;
    assert_eq!(code, 200, "{v}");
    assert!(cookie(&h, &jar, "qsid").is_some());
    assert_eq!(c.get("/api/auth/me").await.1["email"], "tom@x.org");
    // The flow is gone (single use).
    assert_eq!(
        c.post("/api/auth/mfa/totp", json!({"code": next})).await.0,
        401
    );

    // Same code in a second login: refused (replay).
    let c2 = h.anon();
    assert_eq!(login(&c2, "tom@x.org", PW).await.1["mfa_required"], true);
    assert_eq!(
        c2.post("/api/auth/mfa/totp", json!({"code": next})).await.0,
        401
    );

    // Five wrong codes in a row (1 replay + 3 + 1 below): the second factor
    // is locked, also for a right code in a new login.
    for _ in 0..3 {
        let (code, _) = c2
            .post("/api/auth/mfa/totp", json!({"code": "000000"}))
            .await;
        assert_eq!(code, 401);
    }
    let c3 = h.anon();
    assert_eq!(login(&c3, "tom@x.org", PW).await.0, 200);
    let (code, v) = c3
        .post("/api/auth/mfa/totp", json!({"code": "000001"}))
        .await;
    assert_eq!(code, 401, "{v}");
    let (code, v) = c3
        .post("/api/auth/mfa/totp", json!({"code": totp_now(&secret, 1)}))
        .await;
    assert_eq!(code, 429, "locked: {v}");
    assert!(v["error"].as_str().unwrap().contains("Minute"));

    // Passwords: 10 failures lock the account name (also for the right
    // password), unknown accounts behave the same.
    for i in 0..10 {
        let (code, _) = login(&h.anon(), "admin@x.org", "wrong-password").await;
        assert_eq!(code, 401, "attempt {i}");
    }
    let (code, v) = login(&h.anon(), "admin@x.org", PW).await;
    assert_eq!(code, 429, "{v}");
    for _ in 0..10 {
        login(&h.anon(), "nobody@x.org", "wrong-password").await;
    }
    assert_eq!(login(&h.anon(), "nobody@x.org", "x").await.0, 429);
    // Unknown addresses never reach the (append-only) audit log.
    let anon_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE actor_id IS NULL AND action LIKE 'auth.%'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(anon_rows, 0);
    // The existing admin session still works.
    assert_eq!(a.get("/api/auth/me").await.0, 200);

    // Audited, without codes or passwords.
    let log: String = sqlx::query_scalar(
        "SELECT string_agg(action || ' ' || details::text, E'\\n' ORDER BY seq) FROM audit_log",
    )
    .fetch_one(&h.st.db)
    .await
    .unwrap();
    for a in [
        "auth.login",
        "auth.mfa_failed",
        "auth.locked",
        "auth.login_failed",
        "account.totp_enabled",
    ] {
        assert!(log.contains(a), "{a} missing:\n{log}");
    }
    for s in [PW, "wrong-password", next.as_str(), secret.as_str()] {
        assert!(!log.contains(s), "secret {s} in the audit log");
    }
    // The seed is stored encrypted.
    let enc: Vec<u8> = sqlx::query_scalar("SELECT secret_enc FROM mfa_totp")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    let raw = queued::mfa::base32_decode(&secret).unwrap();
    assert!(!enc.windows(raw.len()).any(|w| w == raw.as_slice()));
}

/// Recovery codes: stored hashed only, each works exactly once, a new set
/// needs re-authentication and replaces the old one.
#[sqlx::test]
async fn recovery_codes_are_single_use(pool: PgPool) {
    let h = common::start(pool).await;
    let u = h.user("rita@x.org").await;
    let (_secret, codes) = enable_totp(&u).await;
    let stored: Vec<String> = sqlx::query_scalar("SELECT code_hash FROM recovery_codes")
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    assert_eq!(stored.len(), 10);
    for c in &codes {
        assert!(
            !stored.iter().any(|s| s.contains(c.as_str())),
            "plaintext code stored"
        );
    }

    let c = h.anon();
    login(&c, "rita@x.org", PW).await;
    let shown = queued_api::security::format_recovery_code(&codes[0]).to_lowercase();
    let (code, v) = c
        .post("/api/auth/mfa/recovery", json!({"code": shown}))
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(c.get("/api/account/security").await.1["recovery_left"], 9);
    let notes = c.notifications().await;
    assert!(notes.iter().any(|n| n["title"]
        .as_str()
        .unwrap()
        .contains("Wiederherstellungscode")));

    // Second use of the same code: refused.
    let c2 = h.anon();
    login(&c2, "rita@x.org", PW).await;
    assert_eq!(
        c2.post("/api/auth/mfa/recovery", json!({"code": codes[0]}))
            .await
            .0,
        401
    );
    assert_eq!(
        c2.post("/api/auth/mfa/recovery", json!({"code": codes[1]}))
            .await
            .0,
        200
    );

    // Parallel use of one code: exactly one wins.
    let (p1, p2) = (h.anon(), h.anon());
    login(&p1, "rita@x.org", PW).await;
    login(&p2, "rita@x.org", PW).await;
    let body = json!({"code": codes[2]});
    let (r1, r2) = tokio::join!(
        p1.post("/api/auth/mfa/recovery", body.clone()),
        p2.post("/api/auth/mfa/recovery", body.clone())
    );
    assert_eq!(
        [r1.0, r2.0].iter().filter(|c| **c == 200).count(),
        1,
        "{r1:?} {r2:?}"
    );

    // New set: re-authentication required, old codes invalid.
    assert_eq!(c.post("/api/account/mfa/recovery", json!({})).await.0, 403);
    assert_eq!(
        c.post(
            "/api/account/mfa/recovery",
            json!({"password": "nope-nope"})
        )
        .await
        .0,
        403
    );
    let (code, fresh) = c
        .post("/api/account/mfa/recovery", json!({"password": PW}))
        .await;
    assert_eq!(code, 200);
    assert_eq!(fresh["codes"].as_array().unwrap().len(), 10);
    let c3 = h.anon();
    login(&c3, "rita@x.org", PW).await;
    assert_eq!(
        c3.post("/api/auth/mfa/recovery", json!({"code": codes[3]}))
            .await
            .0,
        401
    );
    let new_code = fresh["codes"][0].as_str().unwrap();
    assert_eq!(
        c3.post("/api/auth/mfa/recovery", json!({"code": new_code}))
            .await
            .0,
        200
    );
}

/// Passkey registration and login with a software authenticator
/// (webauthn-authenticator-rs); a wrong origin fails; the signature
/// counter is stored.
#[sqlx::test]
async fn passkey_registration_and_login(pool: PgPool) {
    use webauthn_authenticator_rs::{softpasskey::SoftPasskey, WebauthnAuthenticator};
    let h = common::start_with(pool, |c| c.public_url = "http://localhost".into()).await;
    let u = h.user("paula@x.org").await;
    let origin: url::Url = "http://localhost".parse().unwrap();
    let mut key = SoftPasskey::new(true);

    let (code, start) = u
        .post(
            "/api/account/webauthn/register/start",
            json!({"step_up": {"password": PW}}),
        )
        .await;
    assert_eq!(code, 200, "{start}");
    let ccr = serde_json::from_value(start["options"].clone()).unwrap();
    let reg = key.do_registration(origin.clone(), ccr).unwrap();
    let (code, done) = u
        .post(
            "/api/account/webauthn/register/finish",
            json!({"credential": serde_json::to_value(&reg).unwrap(), "name": "Laptop"}),
        )
        .await;
    assert_eq!(code, 200, "{done}");
    assert_eq!(done["recovery_codes"].as_array().unwrap().len(), 10);
    let sec = u.get("/api/account/security").await.1;
    assert_eq!(sec["passkeys"][0]["name"], "Laptop");
    // The ceremony state is single use.
    assert_eq!(
        u.post(
            "/api/account/webauthn/register/finish",
            json!({"credential": serde_json::to_value(&reg).unwrap()}),
        )
        .await
        .0,
        400
    );

    let c = h.anon();
    let (_, v) = login(&c, "paula@x.org", PW).await;
    assert_eq!(v["methods"], json!(["webauthn", "recovery"]));
    // Wrong origin (same RP id, other port – the authenticator signs it,
    // the server must refuse it).
    let (_, ch) = c.post("/api/auth/mfa/webauthn/start", json!({})).await;
    let rcr = serde_json::from_value(ch["options"].clone()).unwrap();
    let evil = key
        .do_authentication("http://localhost:9999".parse().unwrap(), rcr)
        .unwrap();
    let (code, _) = c
        .post(
            "/api/auth/mfa/webauthn/finish",
            json!({"credential": serde_json::to_value(&evil).unwrap()}),
        )
        .await;
    assert_eq!(code, 401);
    assert_eq!(c.get("/api/tasks").await.0, 401);
    // Right origin.
    let (_, ch) = c.post("/api/auth/mfa/webauthn/start", json!({})).await;
    let rcr = serde_json::from_value(ch["options"].clone()).unwrap();
    let auth = key.do_authentication(origin.clone(), rcr).unwrap();
    let (code, v) = c
        .post(
            "/api/auth/mfa/webauthn/finish",
            json!({"credential": serde_json::to_value(&auth).unwrap()}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(c.get("/api/auth/me").await.1["email"], "paula@x.org");
    let used: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_used_at FROM webauthn_credentials")
            .fetch_one(&h.st.db)
            .await
            .unwrap();
    assert!(used.is_some());
    // Removing the passkey needs the password; then no factor is left.
    let id = sec["passkeys"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        c.post(&format!("/api/account/webauthn/{id}/remove"), json!({}))
            .await
            .0,
        403
    );
    assert_eq!(
        c.post(
            &format!("/api/account/webauthn/{id}/remove"),
            json!({"password": PW})
        )
        .await
        .0,
        200
    );
    let sec = c.get("/api/account/security").await.1;
    assert_eq!(sec["passkeys"], json!([]));
    assert_eq!(sec["recovery_left"], 0, "codes go with the last factor");
}

/// A new session token at every login (no fixation), sessions listed and
/// revocable one by one or all others; password and MFA changes sign the
/// other sessions out; a revoked session's live stream ends.
#[sqlx::test]
async fn sessions_are_listed_revoked_and_never_fixed(pool: PgPool) {
    let h = common::start(pool).await;
    let _admin = h.user("admin@x.org").await;
    let victim = h.user("vic@x.org").await;
    // Planted cookie: a valid session of another person.
    let (planted_client, planted_jar) = jar_client(&h);
    assert_eq!(login(&planted_client, "admin@x.org", PW).await.0, 200);
    let planted = cookie(&h, &planted_jar, "qsid").unwrap();
    let (c, jar) = jar_client(&h);
    let url: url::Url = h.base.parse().unwrap();
    jar.add_cookie_str(&format!("qsid={planted}; Path=/"), &url);
    assert_eq!(c.get("/api/auth/me").await.1["email"], "admin@x.org");
    assert_eq!(login(&c, "vic@x.org", PW).await.0, 200);
    let fresh = cookie(&h, &jar, "qsid").unwrap();
    assert_ne!(fresh, planted, "login issues a new session token");
    assert_eq!(c.get("/api/auth/me").await.1["email"], "vic@x.org");
    // The planted session is gone with the login.
    assert_eq!(planted_client.get("/api/auth/me").await.0, 401);

    // List: three sessions of vic (register, c, d); the current one marked.
    let d = h.anon();
    login(&d, "vic@x.org", PW).await;
    let list = c.get("/api/account/sessions").await.1;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 3, "{list:?}");
    assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);

    // A live stream of `d` ends when its session is revoked.
    let resp = d
        .http
        .get(format!("{}/api/events", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.bytes_stream();
    let d_id = d
        .get("/api/account/sessions")
        .await
        .1
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["current"] == true)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        c.delete(&format!("/api/account/sessions/{d_id}")).await.0,
        200
    );
    assert_eq!(d.get("/api/auth/me").await.0, 401);
    let ended = tokio::time::timeout(Duration::from_secs(12), async {
        while let Some(Ok(_)) = stream.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "SSE stream of a revoked session must end");
    // Someone else's session: not found.
    assert_eq!(
        victim
            .delete(&format!("/api/account/sessions/{}", Uuid::new_v4()))
            .await
            .0,
        404
    );

    // All others: only `c` remains.
    let (code, v) = c
        .post("/api/account/sessions/revoke-others", json!({}))
        .await;
    assert_eq!(code, 200);
    assert_eq!(v["revoked_sessions"], 1, "victim's registration session");
    assert_eq!(victim.get("/api/auth/me").await.0, 401);

    // Password change: other sessions out, own session continues with a new token.
    let e = h.anon();
    login(&e, "vic@x.org", PW).await;
    assert_eq!(
        c.post(
            "/api/account/password",
            json!({"step_up": {"password": "wrong-one"}, "new_password": "new-password-1"})
        )
        .await
        .0,
        403
    );
    assert_eq!(
        c.post(
            "/api/account/password",
            json!({"step_up": {"password": PW}, "new_password": "short"})
        )
        .await
        .0,
        400
    );
    let (code, v) = c
        .post(
            "/api/account/password",
            json!({"step_up": {"password": PW}, "new_password": "new-password-1"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_ne!(cookie(&h, &jar, "qsid").unwrap(), fresh);
    assert_eq!(c.get("/api/auth/me").await.0, 200);
    assert_eq!(e.get("/api/auth/me").await.0, 401);
    assert_eq!(login(&h.anon(), "vic@x.org", PW).await.0, 401);
    assert_eq!(login(&h.anon(), "vic@x.org", "new-password-1").await.0, 200);

    // MFA enabled: other sessions out as well.
    let f = h.anon();
    login(&f, "vic@x.org", "new-password-1").await;
    let (code, setup) = c
        .post(
            "/api/account/mfa/totp/setup",
            json!({"password": "new-password-1"}),
        )
        .await;
    assert_eq!(code, 200);
    let (code, v) = c
        .post(
            "/api/account/mfa/totp/confirm",
            json!({"code": totp_now(setup["secret"].as_str().unwrap(), 0)}),
        )
        .await;
    assert_eq!(code, 200);
    assert!(v["revoked_sessions"].as_i64().unwrap() >= 2);
    assert_eq!(f.get("/api/auth/me").await.0, 401);
    assert_eq!(c.get("/api/auth/me").await.0, 200);
}

/// No way around the second factor: API tokens never reach account, MFA,
/// token or admin routes; the MFA policy blocks sessions and tokens of
/// people without a factor except for the setup.
#[sqlx::test]
async fn mfa_cannot_be_bypassed(pool: PgPool) {
    let h = common::start(pool).await;
    let admin = h.user("admin@x.org").await;
    let u = h.user("ben@x.org").await;
    let token = u
        .token(&["tasks:read", "tasks:write", "projects:read"])
        .await;
    let t = h.bearer(&token);
    let admin_token = admin.token(&["tasks:read"]).await;
    let at = h.bearer(&admin_token);
    for (m, path) in [
        (reqwest::Method::GET, "/api/account/security"),
        (reqwest::Method::POST, "/api/account/password"),
        (reqwest::Method::POST, "/api/account/mfa/totp/setup"),
        (reqwest::Method::POST, "/api/account/mfa/totp/disable"),
        (reqwest::Method::POST, "/api/account/mfa/recovery"),
        (
            reqwest::Method::POST,
            "/api/account/webauthn/register/start",
        ),
        (reqwest::Method::GET, "/api/account/sessions"),
        (reqwest::Method::POST, "/api/account/sessions/revoke-others"),
        (reqwest::Method::POST, "/api/tokens"),
        (reqwest::Method::GET, "/api/admin/users"),
        (reqwest::Method::GET, "/api/admin/audit"),
        (reqwest::Method::GET, "/api/admin/runners"),
        (reqwest::Method::PUT, "/api/admin/settings"),
    ] {
        assert_eq!(
            t.status_of(m.clone(), path, Some(json!({}))).await,
            403,
            "{m} {path}"
        );
        assert_eq!(
            at.status_of(m.clone(), path, Some(json!({}))).await,
            403,
            "admin token {m} {path}"
        );
    }
    // Non-admin sessions do not reach the console.
    assert_eq!(u.get("/api/admin/users").await.0, 403);

    // Policy "all": ben has no factor.
    let (code, _) = admin
        .put("/api/admin/settings", json!({"mfa_policy": "all"}))
        .await;
    assert_eq!(code, 200);
    // The admin has none either – but the policy keeps the console reachable?
    // No: the admin must set up a factor as well (only setup routes).
    assert_eq!(admin.get("/api/admin/users").await.0, 403);
    let me = u.get("/api/auth/me").await;
    assert_eq!(me.0, 200);
    assert_eq!(me.1["mfa_setup_required"], true);
    let (code, v) = u.get("/api/projects").await;
    assert_eq!(
        (code, v["error"].as_str()),
        (403, Some("mfa_setup_required"))
    );
    assert_eq!(
        t.get("/api/tasks").await.0,
        403,
        "tokens are held back as well"
    );
    assert_eq!(u.get("/api/account/security").await.0, 200);
    enable_totp(&u).await;
    assert_eq!(u.get("/api/projects").await.0, 200);
    assert_eq!(t.get("/api/tasks").await.0, 200);
    // The last factor cannot be removed while the policy requires one.
    let (code, _) = u
        .post("/api/account/mfa/totp/disable", json!({"password": PW}))
        .await;
    assert_eq!(code, 409);
    // Admin sets up a factor and lifts the policy.
    enable_totp(&admin).await;
    assert_eq!(
        admin
            .put("/api/admin/settings", json!({"mfa_policy": "off"}))
            .await
            .0,
        200
    );
    assert_eq!(
        u.post("/api/account/mfa/totp/disable", json!({"password": PW}))
            .await
            .0,
        200
    );
}

/// A lock signs the person out everywhere at once (sessions, tokens, live
/// streams), refuses logins and starts none of their tasks; unlocking does
/// not bring back revoked tokens.
#[sqlx::test]
async fn lock_revokes_sessions_and_tokens(pool: PgPool) {
    let h = common::start(pool).await;
    let admin = h.user("admin@x.org").await;
    let u = h.user("lisa@x.org").await;
    let token = u.token(&["tasks:read", "tasks:write"]).await;
    let t = h.bearer(&token);
    assert_eq!(t.get("/api/tasks").await.0, 200);
    let p = u.project(json!({"name": "p"})).await;
    let id: String = sqlx::query_scalar("SELECT id::text FROM users WHERE email='lisa@x.org'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    let resp = t
        .http
        .get(format!("{}/api/events", h.base))
        .send()
        .await
        .unwrap();
    let mut stream = resp.bytes_stream();

    let (code, v) = admin
        .patch(
            &format!("/api/admin/users/{id}"),
            json!({"disabled": true, "reason": "Test"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert!(v["disabled_at"].is_string());
    assert_eq!(v["sessions"], 0);
    assert_eq!(v["active_tokens"], 0);
    assert_eq!(u.get("/api/auth/me").await.0, 401);
    assert_eq!(t.get("/api/tasks").await.0, 401);
    let ended = tokio::time::timeout(Duration::from_secs(12), async {
        while let Some(Ok(_)) = stream.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "token stream of a locked person ends");
    let (code, v) = login(&h.anon(), "lisa@x.org", PW).await;
    assert_eq!(code, 403, "{v}");
    // Queued work of the person does not start.
    sqlx::query("INSERT INTO tasks (id, user_id, project_id, title, prompt, provider, status, position)
                 SELECT gen_random_uuid(), u.id, $1::uuid, 'locked', 'x', 'mock', 'queued', nextval('task_position_seq')
                 FROM users u WHERE u.email='lisa@x.org'")
        .bind(&p)
        .execute(&h.st.db)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let s: String = sqlx::query_scalar("SELECT status FROM tasks WHERE title='locked'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(s, "queued");

    // Unlock: login works again, the token stays revoked.
    let (code, _) = admin
        .patch(
            &format!("/api/admin/users/{id}"),
            json!({"disabled": false}),
        )
        .await;
    assert_eq!(code, 200);
    let c = h.anon();
    assert_eq!(login(&c, "lisa@x.org", PW).await.0, 200);
    assert_eq!(
        t.get("/api/tasks").await.0,
        401,
        "revoked tokens stay revoked"
    );
    c.wait_status(
        &sqlx::query_scalar::<_, Uuid>("SELECT id FROM tasks WHERE title='locked'")
            .fetch_one(&h.st.db)
            .await
            .unwrap()
            .to_string(),
        &["done", "failed", "blocked"],
    )
    .await;
    let log: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY seq")
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    assert!(log.contains(&"admin.user_disabled".to_string()));
    assert!(log.contains(&"admin.user_enabled".to_string()));
}

/// There is always an active admin: no self-lock, no demotion of the last
/// one, and two admins demoting each other at the same time cannot both win.
#[sqlx::test]
async fn last_admin_cannot_lock_out(pool: PgPool) {
    let h = common::start(pool).await;
    let a = h.user("a@x.org").await;
    let b = h.user("b@x.org").await;
    let id = |e: &'static str| {
        let db = h.st.db.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE email=$1")
                .bind(e)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    let (ia, ib) = (id("a@x.org").await, id("b@x.org").await);
    assert_eq!(
        a.patch(
            &format!("/api/admin/users/{ia}"),
            json!({"is_admin": false})
        )
        .await
        .0,
        409
    );
    assert_eq!(
        a.patch(&format!("/api/admin/users/{ia}"), json!({"disabled": true}))
            .await
            .0,
        409
    );
    assert_eq!(b.get("/api/admin/users").await.0, 403);
    assert_eq!(
        a.patch(&format!("/api/admin/users/{ib}"), json!({"is_admin": true}))
            .await
            .0,
        200
    );
    for _ in 0..5 {
        // Both demote the other at the same time: exactly one succeeds.
        let (pa, pb) = (
            format!("/api/admin/users/{ib}"),
            format!("/api/admin/users/{ia}"),
        );
        let (ra, rb) = tokio::join!(
            a.patch(&pa, json!({"is_admin": false})),
            b.patch(&pb, json!({"is_admin": false}))
        );
        let admins: i64 =
            sqlx::query_scalar("SELECT count(*) FROM users WHERE is_admin AND disabled_at IS NULL")
                .fetch_one(&h.st.db)
                .await
                .unwrap();
        assert_eq!(admins, 1, "{ra:?} {rb:?}");
        assert_eq!(
            [ra.0, rb.0].iter().filter(|c| **c == 200).count(),
            1,
            "{ra:?} {rb:?}"
        );
        // Restore two admins for the next round.
        let winner = if ra.0 == 200 { &a } else { &b };
        let loser = if ra.0 == 200 { ib } else { ia };
        assert_eq!(
            winner
                .patch(
                    &format!("/api/admin/users/{loser}"),
                    json!({"is_admin": true})
                )
                .await
                .0,
            200
        );
    }
    // Locking the other admin while one remains: fine; the remaining one
    // cannot demote itself.
    assert_eq!(
        a.patch(&format!("/api/admin/users/{ib}"), json!({"disabled": true}))
            .await
            .0,
        200
    );
    assert_eq!(
        a.patch(
            &format!("/api/admin/users/{ia}"),
            json!({"is_admin": false})
        )
        .await
        .0,
        409
    );
}

/// Registration modes and single-use invitations with expiry and e-mail binding.
#[sqlx::test]
async fn invitations_and_registration_modes(pool: PgPool) {
    let h = common::start(pool).await;
    let admin = h.user("admin@x.org").await;
    let reg = |email: &str, invite: Option<&str>| {
        let c = h.anon();
        let mut b = json!({"email": email, "password": PW});
        if let Some(i) = invite {
            b["invite"] = json!(i);
        }
        async move { c.post("/api/auth/register", b).await }
    };
    assert_eq!(
        h.anon().get("/api/meta").await.1["registration_mode"],
        "open"
    );
    assert_eq!(
        admin
            .put(
                "/api/admin/settings",
                json!({"registration_mode": "closed"})
            )
            .await
            .0,
        200
    );
    assert_eq!(reg("c1@x.org", None).await.0, 403);
    assert_eq!(
        h.anon().get("/api/meta").await.1["allow_registration"],
        false
    );
    assert_eq!(
        admin
            .put(
                "/api/admin/settings",
                json!({"registration_mode": "invite"})
            )
            .await
            .0,
        200
    );
    assert_eq!(reg("c1@x.org", None).await.0, 403);
    let (code, inv) = admin
        .post(
            "/api/admin/invitations",
            json!({"email": "Bound@x.org", "days": 7}),
        )
        .await;
    assert_eq!(code, 200, "{inv}");
    let link = inv["link"].as_str().unwrap();
    let token = link.split("invite=").nth(1).unwrap().to_string();
    assert_eq!(
        h.anon().get(&format!("/api/auth/invite/{token}")).await.1["email"],
        "bound@x.org"
    );
    assert_eq!(
        reg("other@x.org", Some(&token)).await.0,
        400,
        "bound to an address"
    );
    assert_eq!(reg("bound@x.org", Some(&token)).await.0, 200);
    assert_eq!(reg("bound2@x.org", Some(&token)).await.0, 400, "single use");
    assert_eq!(
        h.anon().get(&format!("/api/auth/invite/{token}")).await.1["valid"],
        false
    );
    // Admin invitation, any address.
    let (_, inv) = admin
        .post(
            "/api/admin/invitations",
            json!({"make_admin": true, "days": 1}),
        )
        .await;
    let t2 = inv["link"]
        .as_str()
        .unwrap()
        .split("invite=")
        .nth(1)
        .unwrap()
        .to_string();
    assert_eq!(reg("boss@x.org", Some(&t2)).await.0, 200);
    let is_admin: bool = sqlx::query_scalar("SELECT is_admin FROM users WHERE email='boss@x.org'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert!(is_admin);
    // Expired and revoked invitations.
    let (_, inv) = admin
        .post("/api/admin/invitations", json!({"days": 1}))
        .await;
    let t3 = inv["link"]
        .as_str()
        .unwrap()
        .split("invite=")
        .nth(1)
        .unwrap()
        .to_string();
    sqlx::query(
        "UPDATE invitations SET expires_at = now() - interval '1 minute' WHERE id=$1::uuid",
    )
    .bind(inv["invitation"]["id"].as_str().unwrap())
    .execute(&h.st.db)
    .await
    .unwrap();
    assert_eq!(reg("late@x.org", Some(&t3)).await.0, 400);
    let (_, inv) = admin
        .post("/api/admin/invitations", json!({"days": 1}))
        .await;
    let t4 = inv["link"]
        .as_str()
        .unwrap()
        .split("invite=")
        .nth(1)
        .unwrap()
        .to_string();
    let iid = inv["invitation"]["id"].as_str().unwrap();
    assert_eq!(
        admin
            .post(&format!("/api/admin/invitations/{iid}/revoke"), json!({}))
            .await
            .0,
        200
    );
    assert_eq!(reg("rev@x.org", Some(&t4)).await.0, 400);
    let list = admin.get("/api/admin/invitations").await.1;
    let states: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    for s in ["used", "expired", "revoked"] {
        assert!(states.contains(&s), "{states:?}");
    }
    assert_eq!(
        admin
            .post("/api/admin/invitations", json!({"days": 99}))
            .await
            .0,
        400
    );
    // Tokens are only stored hashed.
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM invitations WHERE token_hash LIKE 'qinv_%'")
            .fetch_one(&h.st.db)
            .await
            .unwrap();
    assert_eq!(n, 0);
}

/// Quotas of the admin bind the scheduler (cost of the last 30 days from
/// the WP13 accounting, parallelism) and project creation.
#[sqlx::test]
async fn quotas_bind_scheduler_and_projects(pool: PgPool) {
    let h = common::start(pool).await;
    let admin = h.user("admin@x.org").await;
    let u = h.user("q@x.org").await;
    let uid: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email='q@x.org'")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    let p = u.project(json!({"name": "p1"})).await;
    sqlx::query("INSERT INTO usage_log (user_id, provider, cost_usd) VALUES ($1, 'mock', 3.5)")
        .bind(uid)
        .execute(&h.st.db)
        .await
        .unwrap();
    let (code, v) = admin
        .put(
            &format!("/api/admin/users/{uid}/quota"),
            json!({"max_cost_30d_usd": 3.0, "max_projects": 1}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert!((v["cost_30d_usd"].as_f64().unwrap() - 3.5).abs() < 1e-9);
    let q = u.get("/api/account/quota").await.1;
    assert_eq!(q["exhausted"], true);
    let t = u.task(&p, "blocked by quota").await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(u.task_json(&t).await["task"]["status"], "queued");
    let (code, v) = u.post("/api/projects", json!({"name": "p2"})).await;
    assert_eq!(code, 403, "{v}");
    assert!(v["error"].as_str().unwrap().contains("höchstens 1"));
    // Raised: the task runs.
    assert_eq!(
        admin
            .put(
                &format!("/api/admin/users/{uid}/quota"),
                json!({"max_cost_30d_usd": 10.0})
            )
            .await
            .0,
        200
    );
    u.wait_status(&t, &["done"]).await;
    assert_eq!(u.post("/api/projects", json!({"name": "p2"})).await.0, 200);
    assert_eq!(
        admin
            .put(
                &format!("/api/admin/users/{uid}/quota"),
                json!({"max_parallel": 0})
            )
            .await
            .0,
        400
    );
    assert_eq!(
        u.put(&format!("/api/admin/users/{uid}/quota"), json!({}))
            .await
            .0,
        403
    );
}

/// The audit log: written for the actions, append-only in the database
/// (UPDATE/DELETE/TRUNCATE refused, also directly in SQL), chained (a
/// change behind the triggers' back is found), filterable, without secrets.
#[sqlx::test]
async fn audit_log_is_append_only_chained_and_secret_free(pool: PgPool) {
    let h = common::start(pool).await;
    let admin = h.user("admin@x.org").await;
    let u = h.user("sam@x.org").await;
    let p = u.project(json!({"name": "audited"})).await;
    let marker = "sk-ant-api03-SECRETMARKER0123456789";
    assert_eq!(
        u.put(
            "/api/credentials/claude",
            json!({"kind": "api_key", "secret": marker})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        u.put(
            &format!("/api/projects/{p}/secrets/DEPLOY_TOKEN"),
            json!({"value": "SECRETMARKER-2"})
        )
        .await
        .0,
        200
    );
    let tok = u.token(&["tasks:read"]).await;
    assert_eq!(
        u.post(
            &format!("/api/projects/{p}/members"),
            json!({"email": "admin@x.org", "role": "viewer"})
        )
        .await
        .0,
        200
    );
    let all: String =
        sqlx::query_scalar("SELECT string_agg(row_to_json(a)::text, E'\\n') FROM audit_log a")
            .fetch_one(&h.st.db)
            .await
            .unwrap();
    for s in [marker, "SECRETMARKER-2", tok.as_str(), PW] {
        assert!(!all.contains(s), "secret {s} in the audit log");
    }
    for a in [
        "auth.register",
        "project.created",
        "credential.saved",
        "credential.secret_saved",
        "token.created",
        "project.member_added",
    ] {
        assert!(all.contains(a), "{a} missing");
    }

    // Append-only, even with direct SQL of the app's own role.
    for sql in [
        "UPDATE audit_log SET action='auth.login' WHERE seq=1",
        "DELETE FROM audit_log WHERE seq=1",
        "TRUNCATE audit_log",
    ] {
        let e = sqlx::query(sql).execute(&h.st.db).await.unwrap_err();
        assert!(e.to_string().contains("append-only"), "{sql}: {e}");
    }
    // Old entries (>365 days) may go – oldest first only.
    let v = admin.get("/api/admin/audit/verify").await.1;
    assert_eq!(v["intact"], true, "{v}");
    let n = v["checked"].as_i64().unwrap();
    assert!(n >= 7, "{v}");

    // Filter and paging.
    let page = admin
        .get("/api/admin/audit?action=credential.&limit=1")
        .await
        .1;
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    let before = page["next_before"].as_i64().unwrap();
    let page2 = admin
        .get(&format!(
            "/api/admin/audit?action=credential.&limit=1&before={before}"
        ))
        .await
        .1;
    assert!(page2["entries"][0]["seq"].as_i64().unwrap() < before);
    let by_actor = admin.get("/api/admin/audit?actor=sam@").await.1;
    assert!(by_actor["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["actor_label"] == "sam@x.org"));
    let by_project = admin.get(&format!("/api/admin/audit?project={p}")).await.1;
    assert!(!by_project["entries"].as_array().unwrap().is_empty());
    assert_eq!(
        admin.get("/api/admin/audit?action=DROP%20TABLE").await.0,
        400
    );
    let export = admin
        .http
        .get(format!("{}/api/admin/audit/export", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(export.status(), 200);
    let lines = export.text().await.unwrap();
    assert!(lines.lines().count() as i64 >= n);
    assert_eq!(u.get("/api/admin/audit").await.0, 403);

    // Tampering behind the triggers' back (table owner disables them) is
    // found by the chain check.
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_update")
        .execute(&h.st.db)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET details = '{\"forged\": true}' WHERE seq = 3")
        .execute(&h.st.db)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_update")
        .execute(&h.st.db)
        .await
        .unwrap();
    let v = admin.get("/api/admin/audit/verify").await.1;
    assert_eq!(v["intact"], false);
    assert_eq!(v["first_broken"], 3);
}

/// Retention: only entries older than 365 days, from the oldest end.
#[sqlx::test]
async fn audit_retention_only_deletes_old_entries_in_order(pool: PgPool) {
    let h = common::start(pool).await;
    let _a = h.user("admin@x.org").await;
    // Entries "from two years ago": inserted with the trigger, then moved
    // back in time with the update trigger off (only the test can do that).
    for _ in 0..3 {
        queued::audit::system(&h.st, "admin.runner_online", "runner", "x", None, json!({})).await;
    }
    let max: i64 = sqlx::query_scalar("SELECT max(seq) FROM audit_log")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_update")
        .execute(&h.st.db)
        .await
        .unwrap();
    sqlx::query("UPDATE audit_log SET at = now() - interval '800 days' WHERE seq <= $1 - 1")
        .bind(max)
        .execute(&h.st.db)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_update")
        .execute(&h.st.db)
        .await
        .unwrap();
    // A young entry can never go; an old one behind a young one neither.
    assert!(sqlx::query("DELETE FROM audit_log WHERE seq=$1")
        .bind(max)
        .execute(&h.st.db)
        .await
        .is_err());
    let deleted = queued::audit::prune(&h.st.db, 365).await.unwrap();
    assert_eq!(deleted as i64, max - 1);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert!(left >= 1);
}

/// A fake OpenID provider: `/token` answers with an id_token for the nonce
/// and e-mail the test put into `next` (the id_token comes straight from the
/// token endpoint, so the app checks iss/aud/exp/nonce, not a signature).
async fn fake_idp() -> (String, Arc<std::sync::Mutex<(String, String)>>) {
    use base64::Engine;
    let next = Arc::new(std::sync::Mutex::new((String::new(), String::new())));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let (n2, iss) = (next.clone(), url.clone());
    let app = axum::Router::new().route(
        "/token",
        axum::routing::post(move |_form: String| {
            let (nonce, email) = n2.lock().unwrap().clone();
            let claims = json!({
                "iss": iss, "aud": "cid", "exp": chrono::Utc::now().timestamp() + 300,
                "nonce": nonce, "sub": format!("sub-{email}"), "email": email,
                "email_verified": true, "name": "Per OIDC",
            });
            let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            let jwt = format!("{}.{}.sig", b64.encode(br#"{"alg":"none"}"#), b64.encode(claims.to_string()));
            async move { axum::Json(json!({"id_token": jwt, "access_token": "at", "token_type": "Bearer"})) }
        }),
    );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, next)
}

/// Runs the OIDC redirect dance for `email`; returns (status, Location).
async fn oidc_login(
    h: &Harness,
    c: &reqwest::Client,
    next: &std::sync::Mutex<(String, String)>,
    email: &str,
    invite: Option<&str>,
) -> (u16, String) {
    let start = match invite {
        Some(i) => format!("{}/api/auth/oidc/fake/start?invite={i}", h.base),
        None => format!("{}/api/auth/oidc/fake/start", h.base),
    };
    let r = c.get(start).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 303);
    let loc: url::Url = r.headers()["location"].to_str().unwrap().parse().unwrap();
    let q: std::collections::HashMap<String, String> = loc.query_pairs().into_owned().collect();
    *next.lock().unwrap() = (q["nonce"].clone(), email.to_string());
    let r = c
        .get(format!("{}/api/auth/oidc/fake/callback?code=c0de&state={}", h.base, q["state"]))
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.headers()["location"].to_str().unwrap().to_string())
}

/// OIDC is no way around a local second factor: the callback ends in the
/// same second step as the password (no session before the code); people
/// without a factor get their session; in invitation mode new OIDC
/// accounts need an invitation.
#[sqlx::test]
async fn oidc_login_still_needs_the_second_factor(pool: PgPool) {
    let (idp, next) = fake_idp().await;
    let issuer = idp.clone();
    let h = common::start_with(pool, move |c| {
        c.oidc = vec![queued::oidc::OidcProvider {
            id: "fake".into(),
            name: "Fake".into(),
            issuer: Some(issuer.clone()),
            auth_url: format!("{issuer}/authorize"),
            token_url: format!("{issuer}/token"),
            userinfo_url: None,
            client_id: "cid".into(),
            client_secret: Some("secret".into()),
            scopes: "openid email".into(),
            apple: None,
            pkce: true,
            form_post: false,
        }]
    })
    .await;
    let admin = h.user("admin@x.org").await;
    let u = h.user("olga@x.org").await;
    let (secret, _) = enable_totp(&u).await;
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let c = reqwest::Client::builder()
        .cookie_provider(jar.clone())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let (code, to) = oidc_login(&h, &c, &next, "olga@x.org", None).await;
    assert_eq!((code, to.as_str()), (303, "/?mfa=1"), "OIDC must not skip the second factor");
    assert!(cookie(&h, &jar, "qsid").is_none(), "no session after OIDC alone");
    let client = Client { base: h.base.clone(), http: c.clone() };
    assert_eq!(client.get("/api/tasks").await.0, 401);
    let pending = client.get("/api/auth/mfa").await.1;
    assert_eq!(pending["methods"], json!(["totp", "recovery"]));
    let (code, v) = client.post("/api/auth/mfa/totp", json!({"code": totp_now(&secret, 1)})).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(client.get("/api/auth/me").await.1["email"], "olga@x.org");
    let methods: Vec<String> = sqlx::query_scalar("SELECT auth_method FROM sessions s JOIN users u ON u.id=s.user_id WHERE u.email='olga@x.org'")
        .fetch_all(&h.st.db)
        .await
        .unwrap();
    assert!(methods.contains(&"oidc:Fake+totp".to_string()), "{methods:?}");

    // Without a factor: straight in (new account, open registration).
    let c2 = reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap();
    assert_eq!(oidc_login(&h, &c2, &next, "pia@x.org", None).await, (303, "/".to_string()));
    assert_eq!(Client { base: h.base.clone(), http: c2 }.get("/api/auth/me").await.1["email"], "pia@x.org");

    // Invitation mode: new OIDC accounts only with an invitation.
    assert_eq!(admin.put("/api/admin/settings", json!({"registration_mode": "invite"})).await.0, 200);
    let c3 = reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let (_, to) = oidc_login(&h, &c3, &next, "neu@x.org", None).await;
    assert!(to.starts_with("/?login_error="), "{to}");
    let (_, inv) = admin.post("/api/admin/invitations", json!({"email": "neu@x.org", "days": 1})).await;
    let token = inv["link"].as_str().unwrap().split("invite=").nth(1).unwrap().to_string();
    assert_eq!(oidc_login(&h, &c3, &next, "neu@x.org", Some(&token)).await, (303, "/".to_string()));
    let used: bool = sqlx::query_scalar("SELECT used_at IS NOT NULL FROM invitations")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert!(used);
    // A locked account cannot come in through OIDC either.
    let olga: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email='olga@x.org'").fetch_one(&h.st.db).await.unwrap();
    assert_eq!(admin.patch(&format!("/api/admin/users/{olga}"), json!({"disabled": true})).await.0, 200);
    let c4 = reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let (_, to) = oidc_login(&h, &c4, &next, "olga@x.org", None).await;
    assert!(to.starts_with("/?login_error="), "{to}");
}
