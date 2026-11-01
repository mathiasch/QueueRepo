//! Notifications: in-app (SSE) always; ntfy + Web Push for things that need attention.
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

pub struct Note<'a> {
    pub kind: &'a str,
    pub title: String,
    pub body: String,
    pub task_id: Option<Uuid>,
    /// Push to phone (ntfy / Web Push). Use only when user attention is needed.
    pub push: bool,
}

pub async fn notify(st: &AppState, user_id: Uuid, n: Note<'_>) {
    if let Err(e) = notify_inner(st, user_id, &n).await {
        tracing::warn!("notification failed: {e:#}");
    }
}

async fn notify_inner(st: &AppState, user_id: Uuid, n: &Note<'_>) -> anyhow::Result<()> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO notifications (id, user_id, kind, title, body, task_id) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(id)
        .bind(user_id)
        .bind(n.kind)
        .bind(&n.title)
        .bind(&n.body)
        .bind(n.task_id)
        .execute(&st.db)
        .await?;
    st.emit(
        user_id,
        json!({"type": "notification", "notification": {
            "id": id, "kind": n.kind, "title": n.title, "body": n.body, "task_id": n.task_id, "push": n.push
        }}),
    );
    if !n.push {
        return Ok(());
    }

    let click = match n.task_id {
        Some(t) => format!("{}/?task={t}", st.cfg.public_url),
        None => st.cfg.public_url.clone(),
    };

    // ntfy (https://ntfy.sh/<topic> or self-hosted)
    let ntfy: Option<String> =
        sqlx::query_scalar("SELECT ntfy_url FROM user_settings WHERE user_id=$1")
            .bind(user_id)
            .fetch_optional(&st.db)
            .await?
            .flatten();
    if let Some(url) = ntfy {
        if let Err(e) = send_ntfy(st, &url, &n.title, &n.body, &click).await {
            tracing::warn!("ntfy failed: {e:#}");
        }
    }

    // Web Push
    let subs: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, endpoint, p256dh, auth FROM push_subscriptions WHERE user_id=$1",
    )
    .bind(user_id)
    .fetch_all(&st.db)
    .await?;
    let payload = json!({"title": n.title, "body": n.body, "url": click}).to_string();
    for (sid, endpoint, p256dh, auth) in subs {
        match crate::push::send(st, &endpoint, &p256dh, &auth, payload.as_bytes()).await {
            Ok(true) => {}
            Ok(false) => {
                sqlx::query("DELETE FROM push_subscriptions WHERE id=$1")
                    .bind(sid)
                    .execute(&st.db)
                    .await?;
            }
            Err(e) => tracing::warn!("web push failed: {e:#}"),
        }
    }
    Ok(())
}

async fn send_ntfy(
    st: &AppState,
    url: &str,
    title: &str,
    body: &str,
    click: &str,
) -> anyhow::Result<()> {
    let u = url::Url::parse(url)?;
    let topic = u.path().trim_matches('/').to_string();
    anyhow::ensure!(!topic.is_empty(), "ntfy url needs a topic path");
    let mut server = u.clone();
    server.set_path("/");
    let mut req = st.http.post(server.as_str()).json(&json!({
        "topic": topic, "title": title, "message": body, "click": click, "tags": ["robot"]
    }));
    if !u.username().is_empty() {
        req = req.basic_auth(u.username(), u.password());
    }
    req.send().await?.error_for_status()?;
    Ok(())
}
