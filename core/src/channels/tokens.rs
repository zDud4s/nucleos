//! Button action tokens (flow F). P0 skeleton: only the housekeeping stub.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

/// Rows whose proposal was decided more than 7 days ago; called by `run_housekeeping`.
pub async fn prune(_pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<u64> {
    Ok(0) // P0 stub: real DELETE lands with its chunk
}
