//! Daemon start and the fan-out after any account change.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::SqlitePool;

use super::pool::PoolState;
use super::provider::LimitHit;
use super::{launch_chat, store, usage, watch};
use crate::quota::client::QuotaClient;

/// Loads the accounts into the daemon's pool, then starts the usage feed and the auth watch.
/// With no account rows the pool stays empty and every launch is today's.
pub fn start(db: SqlitePool, quota: Option<QuotaClient>, _data_dir: PathBuf) {
    let pool = PoolState::global();
    DAEMON_STARTED.store(true, Ordering::SeqCst);
    tokio::spawn(async move {
        reload(&db, &pool).await;
        restore_exhaustions(&db, &pool, chrono::Utc::now()).await;
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

/// Puts the exhaustion marks stored before a restart back into `pool` and into the notice dedup,
/// after pruning the ones that no longer hold. Runs after `reload`: a mark only lands on an
/// account the pool already lists.
async fn restore_exhaustions(
    db: &SqlitePool,
    pool: &PoolState,
    now: chrono::DateTime<chrono::Utc>,
) {
    if let Err(error) = store::prune_exhaustions(db, now).await {
        tracing::warn!(%error, "could not prune the expired account exhaustion marks");
    }
    let marks = match store::live_exhaustions(db, now).await {
        Ok(marks) => marks,
        Err(error) => {
            tracing::warn!(%error, "could not load the account exhaustion marks");
            return;
        }
    };
    for mark in &marks {
        // A mark with no known reset holds until its stored `until`, not forever.
        let hit = LimitHit {
            window: Some(mark.window.clone()),
            resets_at: mark.resets_at.or(Some(mark.until)),
        };
        pool.mark_exhausted(&mark.account_id, &hit, now);
    }
    launch_chat::seed_announced(&marks, now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountId;
    use crate::accounts::choose::test_support::account;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn db() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    fn mark(id: &str, until: chrono::DateTime<chrono::Utc>) -> store::Exhaustion {
        store::Exhaustion {
            account_id: AccountId(id.into()),
            window: "5h".into(),
            resets_at: Some(until),
            until,
            announced_at: until - chrono::Duration::hours(5),
        }
    }

    #[tokio::test]
    async fn a_stored_mark_survives_a_restart_and_is_not_announced_again() {
        let db = db().await;
        let now = chrono::Utc::now();
        let id = "restore-test:live";
        let until = now + chrono::Duration::hours(1);
        store::upsert_exhaustion(&db, &mark(id, until))
            .await
            .unwrap();

        // A fresh pool stands in for the daemon after a restart.
        let pool = PoolState::new();
        pool.set_accounts(vec![account(id)]);
        restore_exhaustions(&db, &pool, now).await;

        let view = pool.view();
        let exhausted = &view.candidates[0].exhausted;
        assert_eq!(exhausted.len(), 1);
        assert_eq!(exhausted[0].0, "5h");
        assert!(exhausted[0].1.is_some_and(|at| at > now));

        let hit = LimitHit {
            window: Some("5h".into()),
            resets_at: Some(until),
        };
        assert!(
            !launch_chat::limit_notice_due(&AccountId(id.into()), &hit, now),
            "the notice was written before the restart"
        );
    }

    #[tokio::test]
    async fn an_expired_mark_is_not_restored() {
        let db = db().await;
        let now = chrono::Utc::now();
        let id = "restore-test:expired";
        store::upsert_exhaustion(&db, &mark(id, now - chrono::Duration::hours(1)))
            .await
            .unwrap();

        let pool = PoolState::new();
        pool.set_accounts(vec![account(id)]);
        restore_exhaustions(&db, &pool, now).await;

        assert!(pool.view().candidates[0].exhausted.is_empty());
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_exhaustions")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(left, 0, "the expired row is pruned");
        let hit = LimitHit {
            window: Some("5h".into()),
            resets_at: Some(now + chrono::Duration::hours(1)),
        };
        assert!(launch_chat::limit_notice_due(
            &AccountId(id.into()),
            &hit,
            now
        ));
    }
}
