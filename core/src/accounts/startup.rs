//! Daemon start and the fan-out after any account change.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::SqlitePool;

use super::pool::PoolState;
use super::{store, usage, watch};
use crate::quota::client::QuotaClient;

/// Loads the accounts into the daemon's pool, then starts the usage feed and the auth watch.
/// With no account rows the pool stays empty and every launch is today's.
pub fn start(db: SqlitePool, quota: Option<QuotaClient>, _data_dir: PathBuf) {
    let pool = PoolState::global();
    DAEMON_STARTED.store(true, Ordering::SeqCst);
    tokio::spawn(async move {
        reload(&db, &pool).await;
        usage::start(db.clone(), quota, pool.clone());
        watch::start(db, pool);
    });
}

/// Set once by `start`, which only the daemon's `main` calls. The global pool is the daemon's
/// own: a test database must never load its accounts into it, or runs and chat turns of
/// unrelated tests would pick them.
static DAEMON_STARTED: AtomicBool = AtomicBool::new(false);

/// After any insert, rename, state change or removal: reload the pool and tell the feeds.
/// Outside a started daemon (tests) the global pool is left alone.
pub async fn accounts_changed(db: &SqlitePool) {
    if !DAEMON_STARTED.load(Ordering::SeqCst) {
        return;
    }
    let pool = PoolState::global();
    reload(db, &pool).await;
    usage::on_accounts_changed(db, &pool).await;
    watch::on_accounts_changed(db, &pool).await;
}

async fn reload(db: &SqlitePool, pool: &std::sync::Arc<PoolState>) {
    match store::list(db).await {
        Ok(accounts) => pool.set_accounts(accounts),
        Err(error) => tracing::warn!(%error, "could not load the account pool"),
    }
}
