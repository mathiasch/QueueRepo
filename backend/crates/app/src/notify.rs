//! Notifications (WP10): in-app (SSE + Postfach), Web Push, ntfy and a
//! signed webhook – per event and channel configurable, with quiet hours
//! and a daily summary (`digest.rs`).
//!
//! In-app notifications are stored at once. Everything that leaves the
//! server goes through `notification_outbox` (lease, retries, restart- and
//! multi-instance-safe) and the SSRF guard of `llm.rs`
//! (`ALLOW_PRIVATE_NOTIFY_TARGETS` for targets in the LAN).
use std::time::Duration;

use chrono::{DateTime, Timelike, Utc};
use queued_api::notify::{in_quiet, Channel, Event, Matrix};
use serde_json::{json, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::state::AppState;

pub struct Note<'a> {
    pub kind: &'a str,
    pub title: String,
    pub body: String,
    pub task_id: Option<Uuid>,
    /// Needs the user's attention (the call site's importance; the user's
    /// settings decide per event and channel what is delivered where).
    pub push: bool,
}

/// Attempts of an external delivery before it is given up.
pub const MAX_ATTEMPTS: i32 = 5;
/// Rows claimed per pass and their lease: a pass takes at most
/// `BATCH × 15 s` (request timeout), well below the lease – another
/// instance never re-claims a row this one still works on.
pub const BATCH: i64 = 10;
const LEASE_SECS: i64 = 300;

/// A user's notification settings (defaults when nothing is stored).
#[derive(Debug, Clone)]
pub struct Prefs {
    pub tz: chrono_tz::Tz,
    pub quiet_enabled: bool,
    pub quiet_start: i32,
    pub quiet_end: i32,
    pub quiet_critical: bool,
    pub digest_enabled: bool,
    pub digest_minute: i32,
    pub webhook_url: Option<String>,
    pub webhook_secret_enc: Option<Vec<u8>>,
    pub matrix: Matrix,
}

impl Prefs {
    pub fn on(&self, e: Event, c: Channel) -> bool {
        self.matrix
            .get(&e)
            .and_then(|m| m.get(&c))
            .copied()
            .unwrap_or_else(|| e.default_on(c))
    }

    /// Quiet hours hold back push/ntfy for `e` at `now`.
    pub fn quiet_at(&self, e: Event, now: DateTime<Utc>) -> bool {
        if !self.quiet_enabled || (e.critical() && self.quiet_critical) || e == Event::Digest {
            return false;
        }
        let local = now.with_timezone(&self.tz);
        in_quiet(
            self.quiet_start,
            self.quiet_end,
            (local.hour() * 60 + local.minute()) as i32,
        )
    }
}

pub fn parse_tz(s: &str) -> Option<chrono_tz::Tz> {
    s.parse::<chrono_tz::Tz>().ok()
}

pub async fn load_prefs(db: &sqlx::PgPool, user: Uuid) -> sqlx::Result<Prefs> {
    let row: Option<(String, bool, i16, i16, bool, bool, i16, Option<String>, Option<Vec<u8>>)> =
        sqlx::query_as(
            "SELECT timezone, quiet_enabled, quiet_start, quiet_end, quiet_critical, digest_enabled, digest_minute,
                    webhook_url, webhook_secret_enc
               FROM notification_settings WHERE user_id=$1",
        )
        .bind(user)
        .fetch_optional(db)
        .await?;
    let overrides: Vec<(String, String, bool)> =
        sqlx::query_as("SELECT event, channel, enabled FROM notification_prefs WHERE user_id=$1")
            .bind(user)
            .fetch_all(db)
            .await?;
    let mut matrix = queued_api::notify::default_matrix();
    for (e, c, on) in overrides {
        if let (Some(e), Some(c)) = (Event::parse(&e), Channel::parse(&c)) {
            matrix.entry(e).or_default().insert(c, on);
        }
    }
    let (tz, qe, qs, qend, qc, de, dm, wu, ws) =
        row.unwrap_or(("UTC".into(), false, 1320, 420, true, false, 480, None, None));
    Ok(Prefs {
        tz: parse_tz(&tz).unwrap_or(chrono_tz::UTC),
        quiet_enabled: qe,
        quiet_start: qs as i32,
        quiet_end: qend as i32,
        quiet_critical: qc,
        digest_enabled: de,
        digest_minute: dm as i32,
        webhook_url: wu,
        webhook_secret_enc: ws,
        matrix,
    })
}

pub async fn notify(st: &AppState, user_id: Uuid, n: Note<'_>) {
    if let Err(e) = notify_inner(st, user_id, &n).await {
        tracing::warn!("notification failed: {e:#}");
    }
}

/// Click target of a notification.
pub fn click_url(st: &AppState, task: Option<Uuid>) -> String {
    match task {
        Some(t) => format!("{}/?task={t}", st.cfg.public_url),
        None => st.cfg.public_url.clone(),
    }
}

/// What [`create`] stored: the in-app notification to announce.
pub struct Created {
    pub push: Option<queued_api::NotificationPush>,
    pub outbox: usize,
}

/// Stores a notification for `user` in `conn` (a transaction of the caller
/// or a plain connection): the in-app row if enabled, and one outbox row per
/// external channel that is on, configured and not held back by quiet
/// hours. `event == None` is the push test (push + ntfy, no settings).
#[allow(clippy::too_many_arguments)]
pub async fn create(
    st: &AppState,
    conn: &mut PgConnection,
    user: Uuid,
    prefs: &Prefs,
    event: Option<Event>,
    n: &Note<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<Created> {
    // Notifications leave the server: never with a token.
    let title = queued_common::secrets::redact(&n.title);
    let body = queued_common::secrets::redact(&n.body);
    let quiet = event.is_some_and(|e| prefs.quiet_at(e, now));
    let wants = |c: Channel| match event {
        None => matches!(c, Channel::Inapp | Channel::Push | Channel::Ntfy),
        Some(e) => prefs.on(e, c),
    };
    let mut targets: Vec<(Channel, Option<Uuid>)> = vec![];
    let mut held_back = false;
    if wants(Channel::Push) {
        let subs: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM push_subscriptions WHERE user_id=$1 ORDER BY created_at",
        )
        .bind(user)
        .fetch_all(&mut *conn)
        .await?;
        if quiet && !subs.is_empty() {
            held_back = true;
        } else {
            targets.extend(subs.into_iter().map(|s| (Channel::Push, Some(s))));
        }
    }
    if wants(Channel::Ntfy) {
        let ntfy: Option<String> =
            sqlx::query_scalar("SELECT ntfy_url FROM user_settings WHERE user_id=$1")
                .bind(user)
                .fetch_optional(&mut *conn)
                .await?
                .flatten();
        if ntfy.is_some() {
            if quiet {
                held_back = true;
            } else {
                targets.push((Channel::Ntfy, None));
            }
        }
    }
    // Webhooks are machines: quiet hours do not apply.
    if event.is_some() && wants(Channel::Webhook) && prefs.webhook_url.is_some() {
        targets.push((Channel::Webhook, None));
    }
    let event_str = event.map(Event::as_str).unwrap_or("test");
    let mut push = None;
    let mut nid = None;
    if wants(Channel::Inapp) {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO notifications (id, user_id, kind, title, body, task_id, event, quiet_suppressed)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(id)
        .bind(user)
        .bind(n.kind)
        .bind(&title)
        .bind(&body)
        .bind(n.task_id)
        .bind(event_str)
        .bind(held_back)
        .execute(&mut *conn)
        .await?;
        nid = Some(id);
        push = Some(queued_api::NotificationPush {
            id,
            kind: n.kind.to_string(),
            title: title.clone(),
            body: body.clone(),
            task_id: n.task_id,
            push: n.push && !quiet,
        });
    }
    let payload = json!({
        "kind": n.kind,
        "event": event_str,
        "title": title,
        "body": body,
        "task_id": n.task_id,
        "url": click_url(st, n.task_id),
        "created_at": now,
    });
    for (c, target) in &targets {
        sqlx::query(
            "INSERT INTO notification_outbox (user_id, notification_id, channel, target_id, event, payload)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(user)
        .bind(nid)
        .bind(c.as_str())
        .bind(target)
        .bind(event_str)
        .bind(&payload)
        .execute(&mut *conn)
        .await?;
    }
    Ok(Created {
        push,
        outbox: targets.len(),
    })
}

/// Announces what [`create`] stored (after the commit).
pub fn announce(st: &AppState, user: Uuid, c: Created) {
    if let Some(p) = c.push {
        st.emit(
            user,
            queued_api::ServerEvent::Notification { notification: p },
        );
    }
    if c.outbox > 0 {
        st.notify_wake.notify_one();
    }
}

async fn notify_inner(st: &AppState, user_id: Uuid, n: &Note<'_>) -> anyhow::Result<()> {
    let event = Event::classify(n.kind, n.push);
    let prefs = load_prefs(&st.db, user_id).await?;
    let mut conn = st.db.acquire().await?;
    let c = create(st, &mut conn, user_id, &prefs, event, n, Utc::now()).await?;
    drop(conn);
    announce(st, user_id, c);
    Ok(())
}

/// One claimed outbox row.
#[derive(sqlx::FromRow)]
struct Outbox {
    id: i64,
    user_id: Uuid,
    channel: String,
    target_id: Option<Uuid>,
    event: String,
    payload: Value,
    attempts: i32,
}

/// Result of one delivery attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// Try again later.
    Retry(String),
    /// Never deliverable (target removed, refused address, …).
    Gone(String),
}

/// Backoff before attempt `n + 1` (30 s, 1 min, 2 min, 4 min …).
pub fn backoff(attempts: i32) -> chrono::Duration {
    chrono::Duration::seconds(30 * 2i64.pow(attempts.clamp(1, 6) as u32 - 1))
}

/// One pass of the notification loop: due daily summaries, then due
/// outbox rows. Safe with several app instances (row leases, unique keys).
pub async fn tick(st: &AppState) -> anyhow::Result<usize> {
    if let Err(e) = crate::digest::run_due(st, Utc::now()).await {
        tracing::warn!("daily summaries: {e:#}");
    }
    let rows: Vec<Outbox> = sqlx::query_as(
        "UPDATE notification_outbox SET lease_until = now() + make_interval(secs => $1), attempts = attempts + 1
          WHERE id IN (SELECT id FROM notification_outbox
                        WHERE status='pending' AND next_at <= now() AND (lease_until IS NULL OR lease_until < now())
                        ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED)
          RETURNING id, user_id, channel, target_id, event, payload, attempts",
    )
    .bind(LEASE_SECS as f64)
    .bind(BATCH)
    .fetch_all(&st.db)
    .await?;
    let n = rows.len();
    for r in rows {
        let res = deliver(st, &r).await;
        let channel = Channel::parse(&r.channel)
            .map(Channel::as_str)
            .unwrap_or("other");
        let (status, next, err, label) = match &res {
            Delivery::Sent => ("sent", None, None, "sent"),
            Delivery::Gone(e) => ("failed", None, Some(e.clone()), "failed"),
            Delivery::Retry(e) if r.attempts >= MAX_ATTEMPTS => {
                ("failed", None, Some(e.clone()), "failed")
            }
            Delivery::Retry(e) => (
                "pending",
                Some(Utc::now() + backoff(r.attempts)),
                Some(e.clone()),
                "retry",
            ),
        };
        st.metrics.notifications.inc(&[channel, label]);
        sqlx::query(
            "UPDATE notification_outbox SET status=$2, next_at=COALESCE($3, next_at), last_error=$4, lease_until=NULL,
                    sent_at = CASE WHEN $2='sent' THEN now() ELSE sent_at END
              WHERE id=$1",
        )
        .bind(r.id)
        .bind(status)
        .bind(next)
        .bind(err.map(|e| e.chars().take(300).collect::<String>()))
        .execute(&st.db)
        .await?;
    }
    Ok(n)
}

async fn deliver(st: &AppState, r: &Outbox) -> Delivery {
    let title = r.payload["title"].as_str().unwrap_or_default();
    let body = r.payload["body"].as_str().unwrap_or_default();
    let url = r.payload["url"].as_str().unwrap_or_default();
    let guard = st.cfg.notify_guard();
    match Channel::parse(&r.channel) {
        Some(Channel::Ntfy) => {
            let ntfy: Option<String> =
                match sqlx::query_scalar("SELECT ntfy_url FROM user_settings WHERE user_id=$1")
                    .bind(r.user_id)
                    .fetch_optional(&st.db)
                    .await
                {
                    Ok(v) => v.flatten(),
                    Err(e) => return Delivery::Retry(format!("db: {e}")),
                };
            let Some(u) = ntfy else {
                return Delivery::Gone("ntfy-Adresse entfernt".into());
            };
            send_ntfy(&guard, &u, title, body, url).await
        }
        Some(Channel::Push) => {
            let sub: Option<(String, String, String)> = match sqlx::query_as(
                "SELECT endpoint, p256dh, auth FROM push_subscriptions WHERE id=$1 AND user_id=$2",
            )
            .bind(r.target_id)
            .bind(r.user_id)
            .fetch_optional(&st.db)
            .await
            {
                Ok(v) => v,
                Err(e) => return Delivery::Retry(format!("db: {e}")),
            };
            let Some((endpoint, p256dh, auth)) = sub else {
                return Delivery::Gone("Gerät abgemeldet".into());
            };
            let payload = json!({"title": title, "body": body, "url": url}).to_string();
            if let Err(e) = crate::llm::check_target(&endpoint, &guard).await {
                return Delivery::Gone(e.message);
            }
            match crate::push::send(
                st,
                &crate::llm::client(&guard),
                &endpoint,
                &p256dh,
                &auth,
                payload.as_bytes(),
            )
            .await
            {
                Ok(true) => Delivery::Sent,
                Ok(false) => {
                    let _ = sqlx::query("DELETE FROM push_subscriptions WHERE id=$1")
                        .bind(r.target_id)
                        .execute(&st.db)
                        .await;
                    Delivery::Gone("Gerät nicht mehr registriert".into())
                }
                Err(e) => Delivery::Retry(format!("{e:#}")),
            }
        }
        Some(Channel::Webhook) => {
            let row: Option<(Option<String>, Option<Vec<u8>>)> = match sqlx::query_as(
                "SELECT webhook_url, webhook_secret_enc FROM notification_settings WHERE user_id=$1",
            )
            .bind(r.user_id)
            .fetch_optional(&st.db)
            .await
            {
                Ok(v) => v,
                Err(e) => return Delivery::Retry(format!("db: {e}")),
            };
            let (Some(u), Some(enc)) = row.unwrap_or((None, None)) else {
                return Delivery::Gone("Webhook entfernt".into());
            };
            let Ok(secret) = crate::crypto::decrypt(&st.cfg.master_key, &enc) else {
                return Delivery::Gone("Webhook-Secret nicht lesbar".into());
            };
            send_webhook(&guard, &u, &secret, r.id, &r.event, &r.payload).await
        }
        _ => Delivery::Gone("unbekannter Kanal".into()),
    }
}

fn classify_http(res: Result<reqwest::Response, reqwest::Error>) -> Delivery {
    match res {
        Ok(r) if r.status().is_success() => Delivery::Sent,
        Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => {
            Delivery::Retry(format!("HTTP {}", r.status().as_u16()))
        }
        Ok(r) if r.status().as_u16() == 408 => Delivery::Retry("HTTP 408".into()),
        Ok(r) => Delivery::Gone(format!("HTTP {}", r.status().as_u16())),
        Err(e) if e.is_timeout() => Delivery::Retry("Zeitüberschreitung".into()),
        Err(e) => Delivery::Retry(format!("{e}")),
    }
}

async fn send_ntfy(
    guard: &crate::llm::Guard,
    url: &str,
    title: &str,
    body: &str,
    click: &str,
) -> Delivery {
    let Ok(u) = url::Url::parse(url) else {
        return Delivery::Gone("ntfy-Adresse ungültig".into());
    };
    let topic = u.path().trim_matches('/').to_string();
    if topic.is_empty() {
        return Delivery::Gone("ntfy-Adresse ohne Topic".into());
    }
    let mut server = u.clone();
    server.set_path("/");
    let _ = server.set_username("");
    let _ = server.set_password(None);
    if let Err(e) = crate::llm::check_target(server.as_str(), guard).await {
        return Delivery::Gone(e.message);
    }
    let mut req = crate::llm::client(guard)
        .post(server.as_str())
        .timeout(Duration::from_secs(15))
        .json(&json!({
            "topic": topic, "title": title, "message": body, "click": click, "tags": ["robot"]
        }));
    if !u.username().is_empty() {
        req = req.basic_auth(u.username(), u.password());
    }
    classify_http(req.send().await)
}

/// `t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`.
pub fn webhook_signature(secret: &[u8], t: i64, body: &[u8]) -> String {
    let sig = crate::webhooks::signature(secret, &[t.to_string().as_bytes(), b".", body]);
    format!("t={t},v1={}", sig.trim_start_matches("sha256="))
}

async fn send_webhook(
    guard: &crate::llm::Guard,
    url: &str,
    secret: &[u8],
    delivery: i64,
    event: &str,
    payload: &Value,
) -> Delivery {
    if let Err(e) = crate::llm::check_target(url, guard).await {
        return Delivery::Gone(e.message);
    }
    let mut body = payload.clone();
    body["delivery"] = json!(delivery);
    let body = body.to_string();
    let t = Utc::now().timestamp();
    let res = crate::llm::client(guard)
        .post(url)
        .timeout(Duration::from_secs(15))
        .header("content-type", "application/json")
        .header("x-queue-event", event)
        .header("x-queue-delivery", delivery.to_string())
        .header(
            "x-queue-signature",
            webhook_signature(secret, t, body.as_bytes()),
        )
        .body(body)
        .send()
        .await;
    classify_http(res)
}

/// Removes delivered/failed outbox rows after 30 days.
pub async fn prune(st: &AppState) -> sqlx::Result<()> {
    sqlx::query(
        "DELETE FROM notification_outbox WHERE status <> 'pending' AND created_at < now() - interval '30 days'",
    )
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Background loop (`NOTIFY_TICK_SECS`; woken by new outbox rows).
pub async fn run(st: AppState) {
    let every = Duration::from_secs(st.cfg.notify_tick_secs.max(1));
    let mut last_prune = std::time::Instant::now() - Duration::from_secs(7200);
    loop {
        match tick(&st).await {
            // A full batch: there may be more right away.
            Ok(n) if n as i64 >= BATCH => continue,
            Ok(_) => {}
            Err(e) => tracing::warn!("notification loop: {e:#}"),
        }
        if last_prune.elapsed() > Duration::from_secs(3600) {
            last_prune = std::time::Instant::now();
            if let Err(e) = prune(&st).await {
                tracing::warn!("notification prune: {e:#}");
            }
        }
        tokio::select! {
            _ = st.notify_wake.notified() => {}
            _ = tokio::time::sleep(every) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn prefs(tz: &str) -> Prefs {
        Prefs {
            tz: parse_tz(tz).unwrap(),
            quiet_enabled: true,
            quiet_start: 22 * 60,
            quiet_end: 7 * 60,
            quiet_critical: true,
            digest_enabled: false,
            digest_minute: 480,
            webhook_url: None,
            webhook_secret_enc: None,
            matrix: queued_api::notify::default_matrix(),
        }
    }

    #[test]
    fn quiet_hours_use_the_users_time_zone_and_let_critical_through() {
        let p = prefs("Europe/Berlin");
        // 22:30 UTC = 23:30 in Berlin (winter time): quiet.
        let late = Utc.with_ymd_and_hms(2026, 11, 18, 22, 30, 0).unwrap();
        assert!(p.quiet_at(Event::TaskDone, late));
        assert!(
            !p.quiet_at(Event::Approval, late),
            "critical breaks through"
        );
        assert!(!p.quiet_at(Event::Digest, late));
        let mut p2 = p.clone();
        p2.quiet_critical = false;
        assert!(p2.quiet_at(Event::Blocked, late));
        // 06:30 UTC = 07:30 Berlin: awake again; but 05:30 UTC = 06:30 quiet.
        assert!(!p.quiet_at(
            Event::TaskDone,
            Utc.with_ymd_and_hms(2026, 11, 18, 6, 30, 0).unwrap()
        ));
        assert!(p.quiet_at(
            Event::TaskDone,
            Utc.with_ymd_and_hms(2026, 11, 18, 5, 30, 0).unwrap()
        ));
        // Summer time: 05:30 UTC = 07:30 in Berlin – awake.
        assert!(!p.quiet_at(
            Event::TaskDone,
            Utc.with_ymd_and_hms(2026, 7, 1, 5, 30, 0).unwrap()
        ));
        // Another zone, same instant.
        let ny = prefs("America/New_York");
        assert!(!ny.quiet_at(Event::TaskDone, late), "17:30 in New York");
        let mut off = p.clone();
        off.quiet_enabled = false;
        assert!(!off.quiet_at(Event::TaskDone, late));
    }

    #[test]
    fn webhook_signature_is_hmac_over_time_and_body() {
        // Reference: python3 -c 'import hmac,hashlib;print(hmac.new(b"k",b"1700000000.{}",hashlib.sha256).hexdigest())'
        assert_eq!(
            webhook_signature(b"k", 1_700_000_000, b"{}"),
            "t=1700000000,v1=c6ef4734033cee2367414a0939385eca0397754aa7a59716da7463a9298b5146"
        );
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff(1).num_seconds(), 30);
        assert_eq!(backoff(2).num_seconds(), 60);
        assert_eq!(backoff(4).num_seconds(), 240);
        assert_eq!(backoff(50).num_seconds(), 30 * 32);
    }
}
