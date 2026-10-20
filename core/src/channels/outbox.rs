//! The outbound queue with leases (flow F, Anexo D D.10). P0-2b: the adapter routes answer 501,
//! `release_all` is real, the rest lands with OL-a/OL-b/RB.

mod ack;
mod degrade;
mod lease;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::state::AppState;

/// Routes of D.10.2, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels/{channel}/outbox", get(not_implemented))
        .route("/channels/{channel}/outbox/{id}/ack", post(not_implemented))
        .route(
            "/channels/{channel}/outbox/{id}/renew",
            post(not_implemented),
        )
}

/// Placeholder handler: the real one replaces it in the chunk that owns the route.
async fn not_implemented() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// Per-channel long-poll wakers and the ephemeral set. Provisional: OL-a gives it fields.
#[derive(Default)]
pub struct Waiters;

/// `hello` (section 5.1): every `leased` row of the channel goes back to `pending`. `attempts`
/// is left alone: it counts deliveries and was already incremented when the row was leased.
pub async fn release_all(pool: &SqlitePool, channel: &str) -> sqlx::Result<u64> {
    let released = sqlx::query(
        "UPDATE channel_outbox SET state = 'pending', lease_until = NULL \
         WHERE channel = ? AND state = 'leased'",
    )
    .bind(channel)
    .execute(pool)
    .await?;
    Ok(released.rows_affected())
}

/// done/failed rows older than 7 days.
pub async fn prune(_pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<u64> {
    Ok(0) // P0 stub: real DELETE lands with its chunk
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn insert(pool: &SqlitePool, channel: &str, state: &str, attempts: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             state, due_at, lease_until, attempts, created_at) \
             VALUES (?, 'conv', 'ordered', 'send', '{}', 'turn', ?, 't', \
             CASE WHEN ? = 'leased' THEN 'later' END, ?, 't') RETURNING id",
        )
        .bind(channel)
        .bind(state)
        .bind(state)
        .bind(attempts)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn row(pool: &SqlitePool, id: i64) -> (String, Option<String>, i64) {
        sqlx::query_as("SELECT state, lease_until, attempts FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn release_all_returns_leased_rows_to_pending_without_touching_attempts() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let leased = insert(pool, "a", "leased", 2).await;
        let pending = insert(pool, "a", "pending", 1).await;
        let done = insert(pool, "a", "done", 3).await;
        let other_channel = insert(pool, "b", "leased", 1).await;

        assert_eq!(release_all(pool, "a").await.unwrap(), 1);

        assert_eq!(row(pool, leased).await, ("pending".to_string(), None, 2));
        assert_eq!(row(pool, pending).await, ("pending".to_string(), None, 1));
        assert_eq!(row(pool, done).await, ("done".to_string(), None, 3));
        let (st, lease, attempts) = row(pool, other_channel).await;
        assert_eq!((st.as_str(), attempts), ("leased", 1));
        assert!(lease.is_some());
    }

    #[tokio::test]
    async fn release_all_on_an_empty_queue_releases_nothing() {
        let state = crate::channels::testkit::state().await;
        assert_eq!(release_all(&state.pool, "a").await.unwrap(), 0);
    }
}
