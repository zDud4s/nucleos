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
