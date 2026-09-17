//! Deferred notification delivery: the one module that knows a feed row IS a notification.
//!
//! The Telegram sidecar forwards every new `feed` entry without inspecting its kind, which is why
//! `triage.rs` has always filtered on the writing side rather than the reading side. That single
//! fact decides this module's whole shape: suppressing a notification cannot mean intercepting a
//! message somewhere downstream, because there is no downstream — it means **not writing the feed
//! row yet**. So this is not a second notification channel sitting beside the feed. It is the
//! feed's waiting room, and the sidecar needs no change at all.
//!
//! `feed.rs` stays purely observational and knows nothing about any of this; `calendar.rs` is asked
//! whether the person is busy and knows nothing about notifications. This module is the only place
//! the two meet.
//!
//! **What is NOT deferred, and why.** Proposals are not: a pending proposal is autonomous work
//! blocked on a human, and holding it back stops the agent rather than protecting the quiet. Kill
//! switch and budget alerts are not: those are governance, and governance is immediate by
//! definition. The choice is made per call site rather than by a list of kinds here, so that
//! "does this wait?" is answered where the notification is written and is visible when reading it.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::Serialize;

use crate::state::AppState;

/// How often the waiting room is checked. The cost of the interval is how late a held notification
/// can be once the calendar opens, which is a far cheaper error than the one this whole mechanism
/// exists to prevent.
const FLUSH_INTERVAL_SECONDS: u64 = 30;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PendingNotification {
    pub id: i64,
    pub kind: String,
    pub summary: String,
    pub queued_at: String,
    pub delivered_at: Option<String>,
}

/// Writes the feed row now, or parks it until the calendar says the person is free.
///
/// Returns `true` when the notification went out immediately.
pub async fn deliver_or_defer(
    pool: &sqlx::SqlitePool,
    kind: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    // `busy_at` fails OPEN: an unreadable calendar answers "free", and the notification goes out.
    // See calendar.rs — this is the one place in the tree where failing open is the correct
    // direction, because the failure being guarded against is silence.
    if !crate::calendar::busy_at(pool, Utc::now()).await {
        crate::feed::append(pool, None, kind, summary, None, None).await?;
        return Ok(true);
    }

    sqlx::query("INSERT INTO pending_notifications (kind, summary, queued_at) VALUES (?, ?, ?)")
        .bind(kind)
        .bind(summary)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await?;

    tracing::info!(
        kind,
        "notification held: the calendar says this is not a good moment"
    );
    Ok(false)
}

/// Delivers everything waiting, oldest first, if the calendar has opened.
///
/// Returns how many went out. Called on the tick and once at startup, because a daemon that was off
/// when the meeting ended must not leave the queue sitting there until the next meeting.
pub async fn flush_due(pool: &sqlx::SqlitePool) -> sqlx::Result<usize> {
    if crate::calendar::busy_at(pool, Utc::now()).await {
        return Ok(0);
    }

    let waiting: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, kind, summary FROM pending_notifications
          WHERE delivered_at IS NULL
          ORDER BY queued_at ASC, id ASC",
    )
    .fetch_all(pool)
    .await?;

    let mut delivered = 0;
    for (id, kind, summary) in waiting {
        // The feed row is written BEFORE the row is marked delivered, so a crash in between
        // re-delivers rather than loses. Between an unwanted second ping and a message that never
        // arrives, this pillar has already chosen: the whole reason the queue is durable is that
        // notifications must not evaporate.
        crate::feed::append(pool, None, &kind, &summary, None, None).await?;
        sqlx::query("UPDATE pending_notifications SET delivered_at = ? WHERE id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(pool)
            .await?;
        delivered += 1;
    }

    if delivered > 0 {
        tracing::info!(
            delivered,
            "the calendar opened; held notifications delivered"
        );
    }
    Ok(delivered)
}

/// The 30-second tick that empties the waiting room. Sibling of the scheduler and the worktree GC.
pub async fn run_flush_loop(state: AppState) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(FLUSH_INTERVAL_SECONDS));
    loop {
        tick.tick().await;
        if let Err(error) = flush_due(&state.pool).await {
            tracing::warn!(%error, "notify: flushing held notifications failed; will retry");
        }
    }
}

/// `GET /notifications/pending` — what is being held, and what was held and later delivered.
///
/// Delivered rows are kept and shown on purpose. The fear this feature earns is "did the calendar
/// swallow something?", and a queue that only lists what has not arrived yet cannot answer it.
pub async fn list_pending(State(state): State<AppState>) -> impl IntoResponse {
    let rows: Result<Vec<PendingNotification>, _> = sqlx::query_as(
        "SELECT id, kind, summary, queued_at, delivered_at FROM pending_notifications
          ORDER BY queued_at DESC, id DESC LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await;

    match rows {
        Ok(rows) => axum::Json(rows).into_response(),
        Err(error) => {
            tracing::warn!(%error, "notify: reading held notifications failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDateTime};

    const LISBON: chrono_tz::Tz = chrono_tz::Europe::Lisbon;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .expect("an in-memory database");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations to apply");
        pool
    }

    /// Books an event covering `now`, so the calendar reports busy for this test's duration.
    async fn book_around_now(pool: &sqlx::SqlitePool) {
        let local: NaiveDateTime = Utc::now()
            .with_timezone(&LISBON)
            .naive_local()
            .checked_sub_signed(Duration::minutes(30))
            .expect("half an hour ago");
        crate::calendar::insert_event(
            pool,
            "in a meeting",
            local,
            120,
            LISBON,
            "human",
            None,
            None,
        )
        .await
        .expect("the event to insert");
    }

    async fn feed_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed")
            .fetch_one(pool)
            .await
            .expect("the feed to be countable")
    }

    async fn undelivered_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM pending_notifications WHERE delivered_at IS NULL")
            .fetch_one(pool)
            .await
            .expect("the queue to be countable")
    }

    #[tokio::test]
    async fn a_free_calendar_writes_the_feed_row_immediately() {
        let pool = test_pool().await;

        let delivered = deliver_or_defer(&pool, "email_urgent", "the roof is on fire")
            .await
            .expect("the write to succeed");

        assert!(delivered);
        assert_eq!(feed_count(&pool).await, 1);
        assert_eq!(undelivered_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn a_busy_calendar_holds_the_notification_instead_of_writing_it() {
        let pool = test_pool().await;
        book_around_now(&pool).await;

        let delivered = deliver_or_defer(&pool, "email_urgent", "the roof is on fire")
            .await
            .expect("the write to succeed");

        assert!(!delivered);
        assert_eq!(
            feed_count(&pool).await,
            0,
            "nothing reaches the sidecar yet"
        );
        assert_eq!(undelivered_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn a_flush_while_still_busy_delivers_nothing() {
        let pool = test_pool().await;
        book_around_now(&pool).await;
        deliver_or_defer(&pool, "email_urgent", "held")
            .await
            .expect("the write to succeed");

        assert_eq!(flush_due(&pool).await.expect("the flush to run"), 0);
        assert_eq!(feed_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn the_queue_empties_in_arrival_order_once_the_calendar_opens() {
        let pool = test_pool().await;
        book_around_now(&pool).await;
        deliver_or_defer(&pool, "email_urgent", "first")
            .await
            .expect("the write to succeed");
        deliver_or_defer(&pool, "email_action", "second")
            .await
            .expect("the write to succeed");

        // The meeting is over: with no events at all, the calendar is open.
        sqlx::query("DELETE FROM calendar_events")
            .execute(&pool)
            .await
            .expect("the meeting to end");

        assert_eq!(flush_due(&pool).await.expect("the flush to run"), 2);

        let summaries: Vec<String> = sqlx::query_scalar("SELECT summary FROM feed ORDER BY id ASC")
            .fetch_all(&pool)
            .await
            .expect("the feed to be readable");
        assert_eq!(summaries, vec!["first", "second"]);
        assert_eq!(undelivered_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn a_second_flush_does_not_deliver_the_same_notification_twice() {
        let pool = test_pool().await;
        book_around_now(&pool).await;
        deliver_or_defer(&pool, "email_urgent", "once")
            .await
            .expect("the write to succeed");
        sqlx::query("DELETE FROM calendar_events")
            .execute(&pool)
            .await
            .expect("the meeting to end");

        assert_eq!(flush_due(&pool).await.expect("the first flush"), 1);
        assert_eq!(flush_due(&pool).await.expect("the second flush"), 0);
        assert_eq!(feed_count(&pool).await, 1);
    }

    /// A delivered row stays, because the question this table has to be able to answer is "did the
    /// calendar hold anything back, and did it arrive".
    #[tokio::test]
    async fn a_delivered_notification_is_kept_rather_than_deleted() {
        let pool = test_pool().await;
        book_around_now(&pool).await;
        deliver_or_defer(&pool, "email_urgent", "held then sent")
            .await
            .expect("the write to succeed");
        sqlx::query("DELETE FROM calendar_events")
            .execute(&pool)
            .await
            .expect("the meeting to end");
        flush_due(&pool).await.expect("the flush to run");

        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_notifications")
            .fetch_one(&pool)
            .await
            .expect("the queue to be countable");
        assert_eq!(total, 1);

        let delivered_at: Option<String> =
            sqlx::query_scalar("SELECT delivered_at FROM pending_notifications")
                .fetch_one(&pool)
                .await
                .expect("the row to be readable");
        assert!(delivered_at.is_some(), "and it records when it went out");
    }
}
