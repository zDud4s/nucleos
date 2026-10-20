//! Outbox degradation of unsupported content. Filled by the outbox chunk.

use sqlx::SqliteConnection;

use crate::channels::contract::{Capabilities, OutAction};

/// Rewrites `action` so it only uses what `caps` supports.
// P0 stub: OL-b implements the degradation
#[allow(dead_code, unused_variables)]
pub(super) async fn degrade(
    conn: &mut SqliteConnection,
    channel: &str,
    caps: &Capabilities,
    action: OutAction,
) -> sqlx::Result<OutAction> {
    Ok(action)
}
