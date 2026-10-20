//! Media bytes, transcription and turn input (flow B). P0 skeleton: the routes answer 501.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::state::AppState;

/// Routes of D.9, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels/{channel}/media", post(not_implemented))
        .route("/channels/{channel}/media/{media_id}", get(not_implemented))
}

/// Placeholder handler: the real one replaces it in the chunk that owns the route.
async fn not_implemented() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// Housekeeping hook called hourly by `run_housekeeping`.
pub async fn prune(_pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<u64> {
    Ok(0) // P0 stub: real DELETE lands with its chunk
}
