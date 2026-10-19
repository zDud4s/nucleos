//! Turn cycle and queue notices (flow G, Anexo D D.11). P0 skeleton: A2 owns the `todo!()` bodies.
#![allow(dead_code, unused_variables)] // P0 skeleton: A2 removes this with the real body

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::channels::bus::{Cadence, ChannelsBus};
use crate::state::AppState;

/// The ping deadlines, in memory: `(created_at + ping_after, run_id)` of every open channel turn
/// without `pinged_at`, ordered.
#[derive(Default)]
pub struct Pings(std::collections::BTreeMap<(tokio::time::Instant, i64), ()>);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Runs found by `discover` that had no `channel_turn_replies` row.
    pub discovered: u32,
    pub acked: u32,
    pub pinged: u32,
    pub replied: u32,
}

/// Registers the turn and enqueues the "working" ack. A2.
pub async fn turn_started(state: &AppState, run_id: i64, chat_id: &str) {
    todo!()
}

/// Spawned once from `main.rs` when a channel instance exists. A2.
pub async fn run_replies_loop(state: AppState, bus: ChannelsBus, cadence: Cadence) {
    todo!()
}

/// Finds turns that have no `channel_turn_replies` row. A2.
pub async fn discover(state: &AppState, now: DateTime<Utc>) -> sqlx::Result<SweepReport> {
    todo!()
}

/// Answers terminal turns and refreshes ping deadlines. A2.
pub async fn open_pass(
    state: &AppState,
    now: DateTime<Utc>,
    cadence: &Cadence,
    pings: &mut Pings,
) -> sqlx::Result<SweepReport> {
    todo!()
}

/// A ping that fired. A2.
pub async fn fire_ping(state: &AppState, run_id: i64, now: DateTime<Utc>) -> sqlx::Result<bool> {
    todo!()
}

/// `discover` + `open_pass`. A2.
pub async fn sweep_once(
    state: &AppState,
    now: DateTime<Utc>,
    cadence: &Cadence,
    pings: &mut Pings,
) -> sqlx::Result<SweepReport> {
    todo!()
}

/// `channel_turn_replies` rows more than 7 days past `replied_at`.
pub async fn prune(_pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<u64> {
    Ok(0) // P0 stub: real DELETE lands with its chunk
}

/// Posts a system notice and mirrors `queue` notices to the channel. A2.
pub async fn post_notice(
    state: &AppState,
    chat_id: &str,
    kind: &str,
    from_run_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    todo!()
}

/// "Queue paused (N messages) - write to continue", or `None` when nothing waits. A2.
pub async fn queue_paused_text(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    todo!()
}

/// Announces "queue paused (N)" once per pause. A2.
pub async fn announce_queue_paused(state: &AppState, chat_id: &str) -> sqlx::Result<bool> {
    todo!()
}

/// Clears the pause mark when the owner writes again. Real no-op placeholder that A2 replaces.
pub async fn clear_queue_pause(_pool: &SqlitePool, _chat_id: &str) -> sqlx::Result<()> {
    Ok(())
}
