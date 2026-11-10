//! Fake GitHub/GitLab/Gitea for the end-to-end test stack (WP05): the same
//! implementation the integration tests use (`tests/common/fake_git_host.rs`).
//! Never used in production. `FAKE_GIT_HOST_BIND` (default 127.0.0.1:18095).
#[path = "../tests/common/fake_git_host.rs"]
mod fake;

#[tokio::main]
async fn main() {
    let bind = std::env::var("FAKE_GIT_HOST_BIND").unwrap_or_else(|_| "127.0.0.1:18095".into());
    let l = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    eprintln!("fake git host on {bind}");
    let fake = fake::FakeHost::default();
    axum::serve(l, fake::router(fake)).await.expect("serve");
}
