//! The account hook of a chat turn. W0 returns today's launch; the chat-wiring branch decides.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::pool::PoolState;
use super::{AccountId, AccountPick};

pub async fn pick_for_chat_turn(
    _db: &SqlitePool,
    _pool: &Arc<PoolState>,
    _chat_id: &str,
    _engine: &str,
    _resuming: Option<&str>,
    _now: DateTime<Utc>,
) -> AccountPick {
    AccountPick::default_launch()
}

/// Records the turn's cache usage and classifies its output for a limit hit.
#[allow(clippy::too_many_arguments)]
pub async fn after_chat_turn(
    _db: &SqlitePool,
    _pool: &Arc<PoolState>,
    _chat_id: &str,
    _account: &AccountId,
    _engine: &str,
    _cache_creation_tokens: i64,
    _cache_read_tokens: i64,
    _stdout: &str,
    _stderr: &str,
    _now: DateTime<Utc>,
) {
}
