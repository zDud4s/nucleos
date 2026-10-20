//! Access policy, pairing, primary channel and management routes (flow C). P0 skeleton: the
//! routes answer 501.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{delete, get, put};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::state::AppState;

/// Routes of D.7 and E, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels", get(not_implemented))
        .route("/channels/primary", put(not_implemented))
        .route(
            "/channels/{channel}/pairing",
            get(not_implemented)
                .post(not_implemented)
                .delete(not_implemented),
        )
        .route("/channels/{channel}/identities", get(not_implemented))
        .route(
            "/channels/{channel}/identities/{sender_ref}",
            delete(not_implemented),
        )
        .route(
            "/channels/{channel}/identities/{sender_ref}/home",
            put(not_implemented),
        )
}

/// Placeholder handler: the real one replaces it in the chunk that owns the route.
async fn not_implemented() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// Housekeeping hook called hourly by `run_housekeeping`.
pub async fn prune(_pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<u64> {
    Ok(0) // P0 stub: real DELETE lands with its chunk
}
