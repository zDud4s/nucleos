//! The auth watch per account: notices a login that went missing or was rejected. W0 is a no-op.

use std::sync::Arc;

use sqlx::SqlitePool;

use super::pool::PoolState;

pub fn start(_db: SqlitePool, _pool: Arc<PoolState>) {}

pub async fn on_accounts_changed(_db: &SqlitePool, _pool: &Arc<PoolState>) {}
