//! Fake GitHub/Slack/Linear/Jira APIs for the end-to-end test stack (WP08):
//! the same implementation the integration tests use
//! (`tests/common/fake_intake.rs`). Never used in production.
//! `FAKE_INTAKE_BIND` (default 127.0.0.1:18096).
#[path = "../tests/common/fake_intake.rs"]
mod fake;

#[tokio::main]
async fn main() {
    let bind = std::env::var("FAKE_INTAKE_BIND").unwrap_or_else(|_| "127.0.0.1:18096".into());
    let l = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    eprintln!("fake intake APIs on {bind}");
    axum::serve(l, fake::router(fake::FakeIntake::default()))
        .await
        .expect("serve");
}
