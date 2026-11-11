//! WP07 integration tests: personal API tokens – scopes per route (every
//! route of the token table, with and without its scope), default deny for
//! account/settings/token management, expired/revoked/malformed tokens,
//! token beats cookie, `last_used_at`, tenant separation and roles.
mod common;

use common::start;
use queued_api::tokens::{Scope, ROUTES};
use reqwest::Method;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const ALL: [&str; 7] = [
    "projects:read",
    "tasks:read",
    "tasks:write",
    "tasks:steer",
    "triggers:read",
    "triggers:write",
    "triggers:fire",
];

fn path(route: &str, project: &str) -> String {
    let route = route.replace("/api/projects/{id}", &format!("/api/projects/{project}"));
    route
        .replace("{id}", &Uuid::new_v4().to_string())
        .replace("{mid}", &Uuid::new_v4().to_string())
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn every_token_route_needs_its_scope(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("tok-a@example.com").await;
    let p = a.project(json!({"name": "Tok"})).await;
    // One token with only the scope and one with all others, per scope.
    let mut with = std::collections::HashMap::new();
    let mut without = std::collections::HashMap::new();
    for s in Scope::ALL {
        let others: Vec<&str> = ALL.iter().copied().filter(|x| *x != s.as_str()).collect();
        with.insert(s, h.bearer(&a.token(&[s.as_str()]).await));
        without.insert(s, h.bearer(&a.token(&others).await));
    }
    for (method, route, scope) in ROUTES {
        let Some(scope) = scope else { continue };
        let m = Method::from_bytes(method.as_bytes()).unwrap();
        let (t_with, t_without) = (&with[scope], &without[scope]);
        let url = path(route, &p);
        let body = (*method != "GET" && *method != "DELETE").then(|| json!({}));
        let code = t_without.status_of(m.clone(), &url, body.clone()).await;
        assert_eq!(code, 403, "{method} {route} without {}", scope.as_str());
        let code = t_with.status_of(m, &url, body).await;
        assert!(
            code != 401 && code != 403,
            "{method} {route} with {} got {code}",
            scope.as_str()
        );
    }
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn account_settings_and_token_management_are_never_reachable_by_token(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("tok-b@example.com").await;
    let p = a.project(json!({"name": "Tok"})).await;
    let tid = a.task(&p, "MOCK_SLOW hello").await;
    let all = h.bearer(&a.token(&ALL).await);
    let (_, list) = a.get("/api/tokens").await;
    let some_token = list[0]["id"].as_str().unwrap().to_string();
    for (m, url, body) in [
        (Method::GET, "/api/tokens".to_string(), None),
        (
            Method::POST,
            "/api/tokens".into(),
            Some(json!({"name": "x", "scopes": ["tasks:read"], "expires_in_days": 1})),
        ),
        (
            Method::POST,
            format!("/api/tokens/{some_token}/revoke"),
            Some(json!({})),
        ),
        (Method::DELETE, format!("/api/tokens/{some_token}"), None),
        (Method::GET, "/api/auth/me".into(), None),
        (Method::GET, "/api/settings".into(), None),
        (Method::PUT, "/api/settings".into(), Some(json!({}))),
        (Method::GET, "/api/credentials".into(), None),
        (Method::PUT, "/api/credentials/mock".into(), Some(json!({}))),
        (Method::GET, "/api/connectors".into(), None),
        (Method::PUT, "/api/admin/prices".into(), Some(json!({}))),
        (Method::GET, "/api/notifications".into(), None),
        (Method::POST, "/api/push/subscribe".into(), Some(json!({}))),
        (
            Method::PATCH,
            format!("/api/projects/{p}"),
            Some(json!({"name": "x"})),
        ),
        (Method::DELETE, format!("/api/projects/{p}"), None),
        (
            Method::POST,
            format!("/api/projects/{p}/members"),
            Some(json!({"email": "x@y", "role": "editor"})),
        ),
        (
            Method::PUT,
            format!("/api/projects/{p}/secrets/X"),
            Some(json!({"value": "y"})),
        ),
        (
            Method::PUT,
            format!("/api/projects/{p}/delivery"),
            Some(json!({})),
        ),
        (
            Method::PUT,
            format!("/api/projects/{p}/protection"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/tasks/{tid}/gate/approve"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/tasks/{tid}/move"),
            Some(json!({"direction": "top"})),
        ),
        (Method::GET, format!("/api/projects/{p}/zip"), None),
        (Method::HEAD, "/api/tasks".into(), None),
    ] {
        let code = all.status_of(m.clone(), &url, body).await;
        assert_eq!(code, 403, "{m} {url}");
    }
    // Nothing changed: the token list is unchanged, the project still there.
    let (_, after) = a.get("/api/tokens").await;
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(a.get(&format!("/api/projects/{p}")).await.0, 200);
    a.post(&format!("/api/tasks/{tid}/cancel"), json!({})).await;
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn expired_revoked_and_malformed_tokens_are_refused(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("tok-c@example.com").await;
    let raw = a.token(&["tasks:read"]).await;
    let t = h.bearer(&raw);
    assert_eq!(t.get("/api/tasks").await.0, 200);
    // whoami: token info, never the token or its hash.
    let (code, me) = t.get("/api/whoami").await;
    assert_eq!(code, 200, "{me}");
    assert_eq!(me["email"], "tok-c@example.com");
    assert_eq!(me["token"]["scopes"], json!(["tasks:read"]));
    assert!(!me.to_string().contains(&raw[10..]));
    assert_eq!(
        a.get("/api/whoami").await.0,
        400,
        "cookie: no token to describe"
    );
    // The list shows the prefix and last use, never the secret.
    let (_, list) = a.get("/api/tokens").await;
    let row = &list[0];
    assert_eq!(row["prefix"].as_str().unwrap(), &raw[..9]);
    assert!(row["last_used_at"].is_string(), "{row}");
    assert!(!list.to_string().contains(&raw[10..]));
    let hash: String = sqlx::query_scalar("SELECT token_hash FROM api_tokens")
        .fetch_one(&h.st.db)
        .await
        .unwrap();
    assert_eq!(
        hash,
        queued::crypto::sha256_hex(&raw),
        "only the SHA-256 is stored"
    );

    // Revoked.
    let id = row["id"].as_str().unwrap();
    let (code, v) = a.post(&format!("/api/tokens/{id}/revoke"), json!({})).await;
    assert_eq!(code, 200, "{v}");
    let (code, v) = t.get("/api/tasks").await;
    assert_eq!((code, v["error"].as_str()), (401, Some("token revoked")));

    // Expired.
    let raw2 = a.token(&["tasks:read"]).await;
    sqlx::query(
        "UPDATE api_tokens SET expires_at = now() - interval '1 second' WHERE token_hash=$1",
    )
    .bind(queued::crypto::sha256_hex(&raw2))
    .execute(&h.st.db)
    .await
    .unwrap();
    let (code, v) = h.bearer(&raw2).get("/api/tasks").await;
    assert_eq!((code, v["error"].as_str()), (401, Some("token expired")));

    // Malformed, wrong checksum, unknown but well-formed, wrong scheme.
    let mut typo = raw2.clone().into_bytes();
    typo[5] = if typo[5] == b'A' { b'B' } else { b'A' };
    let unknown = queued_api::tokens::compose("qk_", &"Z".repeat(43));
    for bad in [
        "nope".to_string(),
        String::from_utf8(typo).unwrap(),
        unknown,
    ] {
        let (code, v) = h.bearer(&bad).get("/api/tasks").await;
        assert_eq!(
            (code, v["error"].as_str()),
            (401, Some("invalid token")),
            "{bad}"
        );
    }
    let basic = common::Client {
        base: h.base.clone(),
        http: reqwest::Client::builder()
            .default_headers(
                [(
                    reqwest::header::AUTHORIZATION,
                    "Basic eDp5".parse().unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .build()
            .unwrap(),
    };
    assert_eq!(basic.get("/api/tasks").await.0, 401);

    // A live stream of a token ends when the token expires (revocation
    // takes effect at the latest after 10 minutes the same way).
    let raw3 = a.token(&["tasks:read"]).await;
    sqlx::query(
        "UPDATE api_tokens SET expires_at = now() + interval '2 seconds' WHERE token_hash=$1",
    )
    .bind(queued::crypto::sha256_hex(&raw3))
    .execute(&h.st.db)
    .await
    .unwrap();
    let resp = reqwest::Client::new()
        .get(format!("{}/api/events", h.base))
        .bearer_auth(&raw3)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let started = std::time::Instant::now();
    let body = tokio::time::timeout(std::time::Duration::from_secs(15), resp.text())
        .await
        .expect("the stream must end with the token");
    assert!(body.is_ok());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));

    // Validation of new tokens.
    for bad in [
        json!({"name": "", "scopes": ["tasks:read"], "expires_in_days": 30}),
        json!({"name": "x", "scopes": [], "expires_in_days": 30}),
        json!({"name": "x", "scopes": ["tasks:read"], "expires_in_days": 0}),
        json!({"name": "x", "scopes": ["tasks:read"], "expires_in_days": 400}),
    ] {
        assert_eq!(a.post("/api/tokens", bad.clone()).await.0, 400, "{bad}");
    }
    assert_eq!(
        a.post(
            "/api/tokens",
            json!({"name": "x", "scopes": ["admin"], "expires_in_days": 3})
        )
        .await
        .0,
        422,
        "unknown scope"
    );
    // Another user can neither see nor revoke it.
    let b = h.user("tok-d@example.com").await;
    assert_eq!(
        b.post(&format!("/api/tokens/{id}/revoke"), json!({}))
            .await
            .0,
        404
    );
    assert_eq!(b.delete(&format!("/api/tokens/{id}")).await.0, 404);
    assert_eq!(b.get("/api/tokens").await.1, json!([]));
    assert_eq!(a.delete(&format!("/api/tokens/{id}")).await.0, 200);
}

#[sqlx::test(migrator = "queued::MIGRATOR")]
async fn the_token_wins_over_the_cookie_and_roles_still_apply(pool: PgPool) {
    let h = start(pool).await;
    let a = h.user("tok-e@example.com").await;
    let p = a.project(json!({"name": "Shared"})).await;
    let tid = a.task(&p, "MOCK_SLOW work").await;
    // A cookie session that also sends a read-only token: the token decides.
    let read_only = a.token(&["tasks:read"]).await;
    let both = common::Client {
        base: h.base.clone(),
        http: reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(
                [(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {read_only}").parse().unwrap(),
                )]
                .into_iter()
                .collect(),
            )
            .build()
            .unwrap(),
    };
    let (code, _) = both
        .post(
            "/api/auth/login",
            json!({"email": "tok-e@example.com", "password": "password123"}),
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(both.get("/api/settings").await.0, 403, "cookie ignored");
    assert_eq!(
        both.post("/api/tasks", json!({"project_id": p, "prompt": "x"}))
            .await
            .0,
        403
    );

    // Tenant separation: B's token sees nothing of A.
    let b = h.user("tok-f@example.com").await;
    let tb = h.bearer(&b.token(&ALL).await);
    assert_eq!(tb.get(&format!("/api/tasks/{tid}")).await.0, 404);
    assert_eq!(tb.get(&format!("/api/projects/{p}")).await.0, 404);
    assert_eq!(
        tb.post(&format!("/api/tasks/{tid}/cancel"), json!({}))
            .await
            .0,
        404
    );
    assert_eq!(
        tb.post("/api/tasks", json!({"project_id": p, "prompt": "x"}))
            .await
            .0,
        404
    );
    // As viewer: may read, may not create.
    let (code, v) = a
        .post(
            &format!("/api/projects/{p}/members"),
            json!({"email": "tok-f@example.com", "role": "viewer"}),
        )
        .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(tb.get(&format!("/api/tasks/{tid}")).await.0, 200);
    assert_eq!(
        tb.post("/api/tasks", json!({"project_id": p, "prompt": "x"}))
            .await
            .0,
        403
    );
    // A's own token works for the full task cycle.
    let ta = h.bearer(&a.token(&["tasks:read", "tasks:write", "tasks:steer"]).await);
    let (code, t) = ta
        .post(
            "/api/tasks",
            json!({"project_id": p, "prompt": "via token"}),
        )
        .await;
    assert_eq!(code, 200, "{t}");
    let id = t["id"].as_str().unwrap();
    assert_eq!(t["user_id"], a.get("/api/auth/me").await.1["id"]);
    let (code, m) = ta
        .post(
            &format!("/api/tasks/{id}/messages"),
            json!({"body": "also add tests"}),
        )
        .await;
    assert!(code == 200 || code == 409, "{code} {m}");
    assert_eq!(
        ta.post(&format!("/api/tasks/{tid}/cancel"), json!({}))
            .await
            .0,
        200
    );
    a.wait_status(&tid, &["cancelled"]).await;
}
