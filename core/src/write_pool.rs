//! The one write queue of the verify executor and the quota history (autonomy-trees spec A1).
//!
//! Their writes are small and frequent, and on the shared pool each held a connection (and then the
//! SQLite write lock) while runs and trees were reading through the same twelve: under load a gate
//! failed with `pool timed out` or `database is locked` and measured nothing. A single connection of
//! its own turns them into a queue: writers wait on it, readers never do, and a burst costs the
//! queued writers some latency instead of costing everybody else a connection. Until `main`
//! installs it (and in every test) [`for_writes`] answers the pool it was given, so nothing changes
//! there.

use std::sync::OnceLock;

use sqlx::SqlitePool;

static WRITES: OnceLock<SqlitePool> = OnceLock::new();

/// Installs the write queue's pool once; a second call keeps the first.
pub fn install(pool: SqlitePool) {
    let _ = WRITES.set(pool);
}

/// The pool the verify and quota writes go through: the installed write queue, else `fallback`.
pub fn for_writes(fallback: &SqlitePool) -> SqlitePool {
    WRITES.get().unwrap_or(fallback).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn without_an_installed_queue_the_given_pool_is_used() {
        // Tests never install the queue: the process-wide cell stays empty here.
        assert!(WRITES.get().is_none());
        let pool = crate::testdb::fresh_pool().await;
        let chosen = for_writes(&pool);
        assert_eq!(
            chosen.options().get_max_connections(),
            pool.options().get_max_connections()
        );
    }
}
