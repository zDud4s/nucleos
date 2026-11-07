//! The per-account usage feed: polls the quota sidecar and fills the pool. W0 is a no-op, so the
//! pool stays empty of usage and every pick is today's launch.

use std::sync::Arc;

use sqlx::SqlitePool;

use super::pool::PoolState;
use crate::quota::client::QuotaClient;

pub fn start(_db: SqlitePool, _client: Option<QuotaClient>, _pool: Arc<PoolState>) {}

pub async fn on_accounts_changed(_db: &SqlitePool, _pool: &Arc<PoolState>) {}
