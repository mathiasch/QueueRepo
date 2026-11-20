//! WP10: daily summary per user at a chosen local time.
//!
//! Exactly once per user and local date, also with several app instances
//! and restarts: the row in `notification_digests` (primary key user +
//! local date), the in-app notification and the outbox rows are written in
//! one transaction; whoever inserts the row first sends, everyone else
//! gets `ON CONFLICT DO NOTHING`. A summary missed while the app was down
//! is sent later the same local day (at most 12 h late), otherwise the day
//! is skipped. Nothing happened → nothing is sent (the day still counts).
use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use queued_api::notify::Event;
use uuid::Uuid;

use crate::{
    notify::{self, Note},
    state::AppState,
};

/// Latest catch-up after the chosen time (same local day only).
pub const CATCH_UP_MINUTES: i32 = 12 * 60;

/// The local date whose summary is due at `now`, if any.
pub fn due_date(now: DateTime<Utc>, tz: chrono_tz::Tz, minute: i32) -> Option<NaiveDate> {
    let local = now.with_timezone(&tz);
    let m = (local.hour() * 60 + local.minute()) as i32;
    (m >= minute && m < minute + CATCH_UP_MINUTES).then(|| local.date_naive())
}

/// Numbers of one summary.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Summary {
    pub done: i64,
    pub failed: i64,
    pub blocked: i64,
    pub cancelled: i64,
    /// (currency, amount)
    pub costs: Vec<(String, f64)>,
    pub approvals: i64,
    pub questions: i64,
    pub held_back: i64,
    pub pause_minutes: i64,
}

impl Summary {
    pub fn is_empty(&self) -> bool {
        self.done
            + self.failed
            + self.blocked
            + self.cancelled
            + self.approvals
            + self.questions
            + self.held_back
            == 0
            && self.costs.iter().all(|(_, a)| *a <= 0.0)
            && self.pause_minutes == 0
    }

    /// Plain-language text of the summary (German UI).
    pub fn text(&self) -> String {
        let mut lines = vec![format!(
            "Erledigt: {} · Fehlgeschlagen: {} · Blockiert: {}{}",
            self.done,
            self.failed,
            self.blocked,
            if self.cancelled > 0 {
                format!(" · Abgebrochen: {}", self.cancelled)
            } else {
                String::new()
            }
        )];
        let costs: Vec<String> = self
            .costs
            .iter()
            .filter(|(_, a)| *a > 0.0)
            .map(|(c, a)| {
                // German decimal comma, like the app.
                let n = format!("{a:.2}").replace('.', ",");
                match c.as_str() {
                    "USD" => format!("{n} $"),
                    c => format!("{n} {c}"),
                }
            })
            .collect();
        if !costs.is_empty() {
            lines.push(format!("Kosten: {}", costs.join(" + ")));
        }
        if self.approvals + self.questions > 0 {
            lines.push(format!(
                "Wartet auf dich: {} Freigabe{}, {} Rückfrage{}",
                self.approvals,
                if self.approvals == 1 { "" } else { "n" },
                self.questions,
                if self.questions == 1 { "" } else { "n" },
            ));
        }
        if self.held_back > 0 {
            lines.push(format!(
                "In der Ruhezeit zurückgehalten: {} Meldung{}",
                self.held_back,
                if self.held_back == 1 { "" } else { "en" }
            ));
        }
        if self.pause_minutes > 0 {
            lines.push(format!(
                "Pausen durch Anbieter-Limits: {} min",
                self.pause_minutes
            ));
        }
        lines.join("\n")
    }
}

pub async fn summary(
    st: &AppState,
    user: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> sqlx::Result<Summary> {
    let (done, failed, blocked, cancelled, approvals, questions, held_back, pause_secs): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        f64,
    ) = sqlx::query_as(
        "SELECT
            COUNT(*) FILTER (WHERE t.status='done'),
            COUNT(*) FILTER (WHERE t.status='failed'),
            COUNT(*) FILTER (WHERE t.status='blocked'),
            COUNT(*) FILTER (WHERE t.status='cancelled'),
            (SELECT COUNT(*) FROM tasks WHERE user_id=$1 AND status IN ('pending_approval','held')),
            (SELECT COUNT(*) FROM tasks WHERE user_id=$1 AND status='waiting_input'),
            (SELECT COUNT(*) FROM notifications WHERE user_id=$1 AND quiet_suppressed AND created_at >= $2 AND created_at < $3),
            COALESCE((SELECT EXTRACT(EPOCH FROM SUM(upper(r) - lower(r))) FROM unnest((
                SELECT range_agg(tstzrange(GREATEST(started_at, $2), LEAST(COALESCE(ends_at, $3), $3)))
                  FROM provider_pauses
                 WHERE user_id=$1 AND started_at < $3 AND COALESCE(ends_at, $3) > $2)) r), 0)::DOUBLE PRECISION
           FROM tasks t
          WHERE t.user_id=$1 AND t.finished_at >= $2 AND t.finished_at < $3",
    )
    .bind(user)
    .bind(from)
    .bind(to)
    .fetch_one(&st.db)
    .await?;
    let costs: Vec<(String, f64)> = sqlx::query_as(
        "SELECT currency, SUM(cost_amount)::DOUBLE PRECISION FROM usage_log
          WHERE user_id=$1 AND ts >= $2 AND ts < $3 GROUP BY currency ORDER BY currency",
    )
    .bind(user)
    .bind(from)
    .bind(to)
    .fetch_all(&st.db)
    .await?;
    Ok(Summary {
        done,
        failed,
        blocked,
        cancelled,
        costs,
        approvals,
        questions,
        held_back,
        pause_minutes: (pause_secs / 60.0).round() as i64,
    })
}

/// Creates every summary that is due at `now` (called by `notify::tick`).
pub async fn run_due(st: &AppState, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let users: Vec<(Uuid, String, i16)> = sqlx::query_as(
        "SELECT s.user_id, s.timezone, s.digest_minute FROM notification_settings s
          WHERE s.digest_enabled",
    )
    .fetch_all(&st.db)
    .await?;
    let mut created = 0;
    for (user, tz, minute) in users {
        let Some(tz) = notify::parse_tz(&tz) else {
            continue;
        };
        let Some(date) = due_date(now, tz, minute as i32) else {
            continue;
        };
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM notification_digests WHERE user_id=$1 AND local_date=$2)",
        )
        .bind(user)
        .bind(date)
        .fetch_one(&st.db)
        .await?;
        if exists {
            continue;
        }
        match create(st, user, date, now).await {
            Ok(true) => created += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!("daily summary for a user failed: {e:#}"),
        }
    }
    Ok(created)
}

/// Writes the summary of `date` for `user`; `false` = another instance (or
/// an earlier run) already did.
pub async fn create(
    st: &AppState,
    user: Uuid,
    date: NaiveDate,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let prefs = notify::load_prefs(&st.db, user).await?;
    let prev: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT MAX(period_end) FROM notification_digests WHERE user_id=$1")
            .bind(user)
            .fetch_one(&st.db)
            .await?;
    // The last day at most (a summary after a long break is not a novel).
    let from = prev.map_or(now - Duration::hours(24), |p| {
        p.max(now - Duration::hours(24))
    });
    let s = summary(st, user, from, now).await?;
    let mut tx = st.db.begin().await?;
    let won: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO notification_digests (user_id, local_date, period_start, period_end)
         VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING RETURNING user_id",
    )
    .bind(user)
    .bind(date)
    .bind(from)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    if won.is_none() {
        tx.rollback().await?;
        return Ok(false);
    }
    st.metrics.digests.inc(&[]);
    if s.is_empty() {
        tx.commit().await?;
        return Ok(true);
    }
    let note = Note {
        kind: "digest",
        title: format!(
            "Tägliche Zusammenfassung ({})",
            now.with_timezone(&prefs.tz).format("%d.%m.")
        ),
        body: s.text(),
        task_id: None,
        push: true,
    };
    let c = notify::create(st, &mut tx, user, &prefs, Some(Event::Digest), &note, now).await?;
    if let Some(p) = &c.push {
        sqlx::query(
            "UPDATE notification_digests SET notification_id=$3 WHERE user_id=$1 AND local_date=$2",
        )
        .bind(user)
        .bind(date)
        .bind(p.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    notify::announce(st, user, c);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn due_from_the_chosen_minute_until_twelve_hours_later_same_day() {
        let berlin: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        // 07:59 Berlin (06:59 UTC in winter): not yet.
        let t = Utc.with_ymd_and_hms(2026, 11, 18, 6, 59, 0).unwrap();
        assert_eq!(due_date(t, berlin, 480), None);
        let t = Utc.with_ymd_and_hms(2026, 11, 18, 7, 0, 0).unwrap();
        assert_eq!(
            due_date(t, berlin, 480),
            NaiveDate::from_ymd_opt(2026, 11, 18)
        );
        // Catch-up until 19:59, then the day is skipped.
        let t = Utc.with_ymd_and_hms(2026, 11, 18, 18, 59, 0).unwrap();
        assert!(due_date(t, berlin, 480).is_some());
        let t = Utc.with_ymd_and_hms(2026, 11, 18, 19, 0, 0).unwrap();
        assert_eq!(due_date(t, berlin, 480), None);
        // The local date counts, not the UTC date: 23:30 UTC = 00:30 next day in Berlin.
        let t = Utc.with_ymd_and_hms(2026, 11, 18, 23, 30, 0).unwrap();
        assert_eq!(
            due_date(t, berlin, 15),
            NaiveDate::from_ymd_opt(2026, 11, 19)
        );
        // Daylight saving time: 08:00 Berlin in July is 06:00 UTC.
        let t = Utc.with_ymd_and_hms(2026, 7, 1, 6, 0, 0).unwrap();
        assert_eq!(
            due_date(t, berlin, 480),
            NaiveDate::from_ymd_opt(2026, 7, 1)
        );
        // Late evening time: the window ends at local midnight.
        let t = Utc.with_ymd_and_hms(2026, 11, 18, 23, 30, 0).unwrap();
        assert_eq!(due_date(t, berlin, 22 * 60), None, "00:30 next day < 22:00");
    }

    #[test]
    fn empty_and_text() {
        assert!(Summary::default().is_empty());
        let s = Summary {
            done: 3,
            failed: 1,
            costs: vec![("USD".into(), 1.234), ("CHF".into(), 0.5)],
            approvals: 1,
            questions: 2,
            held_back: 4,
            ..Default::default()
        };
        assert!(!s.is_empty());
        let t = s.text();
        assert!(
            t.contains("Erledigt: 3 · Fehlgeschlagen: 1 · Blockiert: 0"),
            "{t}"
        );
        assert!(t.contains("Kosten: 1,23 $ + 0,50 CHF"), "{t}");
        assert!(t.contains("1 Freigabe, 2 Rückfragen"), "{t}");
        assert!(t.contains("4 Meldungen"), "{t}");
    }
}
