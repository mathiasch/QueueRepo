//! LiteLLM gateway (WP13): a virtual key per run with the task's remaining
//! cost limit as `max_budget`, revoked afterwards. The gateway then enforces
//! a hard cost cap even for CLIs without their own cost limit.
//!
//! Admin API (`POST /key/generate`, `POST /key/delete`, Bearer master key) is
//! prior knowledge, not verified against a real LiteLLM – tested against a
//! fake server only.
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::{agent::Credential, connectors};

/// Gateway base URL and master key of a connector with per-task keys enabled.
pub fn per_task(cred: &Credential) -> Option<(String, String)> {
    let c = cred.conn.as_ref()?;
    if c.kind != "litellm" || c.get("per_task_key") != Some("on") {
        return None;
    }
    let base = c.get("base_url")?.trim_end_matches('/').to_string();
    Some((base, cred.secret.clone()))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client")
}

/// Issues a virtual key for one run. `max_budget` in USD (`None` = no cap).
pub async fn issue(
    base: &str,
    master: &str,
    task: Uuid,
    max_budget: Option<f64>,
    guard: &crate::llm::Guard,
) -> anyhow::Result<String> {
    crate::llm::check_target(base, guard)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    let mut body = json!({
        "duration": "24h",
        "metadata": {"queue_task": task.to_string()},
        "key_alias": format!("queue-{task}-{}", Uuid::new_v4().simple()),
    });
    if let Some(b) = max_budget {
        body["max_budget"] = json!((b * 10_000.0).floor() / 10_000.0);
    }
    let resp = client()
        .post(format!("{base}/key/generate"))
        .bearer_auth(master)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    anyhow::ensure!(
        status.is_success(),
        "LiteLLM /key/generate: HTTP {}: {}",
        status.as_u16(),
        connectors::redact(&text.chars().take(300).collect::<String>(), Some(master))
    );
    let v: Value = serde_json::from_str(&text)?;
    v["key"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("LiteLLM /key/generate: Antwort ohne Schlüssel"))
}

/// Revokes a virtual key (best effort; it expires after 24 h anyway).
pub async fn revoke(base: &str, master: &str, key: &str, guard: &crate::llm::Guard) {
    if crate::llm::check_target(base, guard).await.is_err() {
        return;
    }
    let r = client()
        .post(format!("{base}/key/delete"))
        .bearer_auth(master)
        .json(&json!({"keys": [key]}))
        .send()
        .await;
    match r {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => tracing::warn!("LiteLLM key delete: HTTP {}", resp.status()),
        Err(e) => tracing::warn!("LiteLLM key delete: {e}"),
    }
}

/// The credential of a run with the virtual key instead of the master key.
pub fn with_key(cred: &Credential, key: String) -> Credential {
    Credential {
        kind: cred.kind.clone(),
        secret: key,
        conn: cred.conn.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ConnCfg;

    #[test]
    fn per_task_only_for_litellm_with_the_option() {
        let mk = |kind: &str, cfg: Value| Credential {
            kind: format!("connector:{kind}"),
            secret: "master".into(),
            conn: Some(ConnCfg {
                id: None,
                kind: kind.into(),
                label: "gw".into(),
                config: cfg,
                flex: false,
            }),
        };
        assert_eq!(
            per_task(&mk(
                "litellm",
                json!({"base_url": "https://gw.ch/", "per_task_key": "on"})
            )),
            Some(("https://gw.ch".into(), "master".into()))
        );
        assert!(per_task(&mk(
            "litellm",
            json!({"base_url": "https://gw.ch", "per_task_key": "off"})
        ))
        .is_none());
        assert!(per_task(&mk(
            "anthropic_gateway",
            json!({"base_url": "https://gw.ch", "per_task_key": "on"})
        ))
        .is_none());
        let c = with_key(&mk("litellm", json!({})), "sk-task".into());
        assert_eq!(c.secret, "sk-task");
        assert!(c.conn.is_some());
    }
}
