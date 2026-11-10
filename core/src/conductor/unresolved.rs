//! The ticks a launch key stays reserved without a run, counted so the silence ends.
//!
//! A handoff continuation whose launch answers `NodeRunError::Unresolved` (the key is reserved but
//! holds no run id) used to be skipped with no trace, tick after tick, for as long as the daemon ran:
//! only a boot cleared such a reservation. Now each such tick is counted per node in
//! `node_watchdog.unresolved_ticks`, and at [`UNRESOLVED_CAP`] the reservation is cleared, as boot
//! recovery would, and a feed line says so. The count restarts from zero after a clear or a launch.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::tree_job::store::clear_spent_launch;

/// Consecutive unresolved ticks after which the reservation is cleared. One tick a minute, so a call
/// that is genuinely creating its run (seconds) is long done by then.
pub(crate) const UNRESOLVED_CAP: i64 = 3;

/// The feed kind of the line written when a stuck reservation is cleared.
pub(crate) const UNRESOLVED_CLEARED: &str = "node_launch_unresolved_cleared";

/// Counts one more unresolved tick of `node_id`'s launch `key`. At the cap clears the reservation,
/// resets the count and writes a feed line; answers whether it cleared.
pub(crate) async fn note(pool: &SqlitePool, node_id: &str, key: &str) -> sqlx::Result<bool> {
    let now = Utc::now().to_rfc3339();
    let ticks: i64 = sqlx::query_scalar(
        "INSERT INTO node_watchdog (node_id, unresolved_ticks, updated_at) VALUES (?, 1, ?) \
         ON CONFLICT (node_id) DO UPDATE SET unresolved_ticks = unresolved_ticks + 1, \
                                             updated_at = excluded.updated_at \
         RETURNING unresolved_ticks",
    )
    .bind(node_id)
    .bind(&now)
    .fetch_one(pool)
    .await?;
    if ticks < UNRESOLVED_CAP {
        return Ok(false);
    }
    let cleared = clear_spent_launch(pool, node_id, key).await.unwrap_or(false);
    reset(pool, node_id).await?;
    if cleared {
        sqlx::query(
            "INSERT INTO feed (kind, summary, run_id, subject, created_at) VALUES (?, ?, NULL, ?, ?)",
        )
        .bind(UNRESOLVED_CLEARED)
        .bind(format!(
            "Launch key {key} held no run for {UNRESOLVED_CAP} ticks in a row: the reservation was cleared and the launch will be tried again."
        ))
        .bind(format!("node:{node_id}"))
        .bind(&now)
        .execute(pool)
        .await?;
        nucleos_base::wake::bump(nucleos_base::wake::Topic::Feed);
    }
    Ok(cleared)
}

/// Forgets the count of `node_id`: its launch resolved.
pub(crate) async fn reset(pool: &SqlitePool, node_id: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE node_watchdog SET unresolved_ticks = 0 WHERE node_id = ? AND unresolved_ticks <> 0")
        .bind(node_id)
        .execute(pool)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn reserve(pool: &SqlitePool, node_id: &str, key: &str) {
        sqlx::query(
            "INSERT INTO tree_transitions (node_id, transition, key, result, created_at) VALUES (?, 'launch', ?, '', ?)",
        )
        .bind(node_id)
        .bind(key)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn reservations(pool: &SqlitePool, node_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions WHERE node_id = ? AND transition = 'launch'")
            .bind(node_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn feed_lines(pool: &SqlitePool, node_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ? AND subject = ?")
            .bind(UNRESOLVED_CLEARED)
            .bind(format!("node:{node_id}"))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn ticks_below_the_cap_keep_the_reservation_and_stay_silent() {
        let pool = crate::testdb::fresh_pool().await;
        reserve(&pool, "n1", "continue-a").await;
        for _ in 1..UNRESOLVED_CAP {
            assert!(!note(&pool, "n1", "continue-a").await.unwrap());
        }
        assert_eq!(reservations(&pool, "n1").await, 1);
        assert_eq!(feed_lines(&pool, "n1").await, 0);
    }

    #[tokio::test]
    async fn the_capth_tick_clears_the_reservation_and_writes_a_feed_line() {
        let pool = crate::testdb::fresh_pool().await;
        reserve(&pool, "n1", "continue-a").await;
        for _ in 1..UNRESOLVED_CAP {
            note(&pool, "n1", "continue-a").await.unwrap();
        }
        assert!(note(&pool, "n1", "continue-a").await.unwrap());
        assert_eq!(reservations(&pool, "n1").await, 0);
        assert_eq!(feed_lines(&pool, "n1").await, 1);
    }

    #[tokio::test]
    async fn the_count_restarts_after_a_clear() {
        let pool = crate::testdb::fresh_pool().await;
        reserve(&pool, "n1", "continue-a").await;
        for _ in 0..UNRESOLVED_CAP {
            note(&pool, "n1", "continue-a").await.unwrap();
        }
        reserve(&pool, "n1", "continue-a").await;
        assert!(!note(&pool, "n1", "continue-a").await.unwrap());
        assert_eq!(reservations(&pool, "n1").await, 1);
    }

    #[tokio::test]
    async fn a_resolved_launch_resets_the_count() {
        let pool = crate::testdb::fresh_pool().await;
        reserve(&pool, "n1", "continue-a").await;
        for _ in 1..UNRESOLVED_CAP {
            note(&pool, "n1", "continue-a").await.unwrap();
        }
        reset(&pool, "n1").await.unwrap();
        assert!(!note(&pool, "n1", "continue-a").await.unwrap());
        assert_eq!(reservations(&pool, "n1").await, 1);
    }

    #[tokio::test]
    async fn a_reservation_holding_a_run_id_is_never_cleared() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO tree_transitions (node_id, transition, key, result, created_at) VALUES ('n1', 'launch', 'continue-a', '42', ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
        for _ in 0..UNRESOLVED_CAP {
            note(&pool, "n1", "continue-a").await.unwrap();
        }
        assert_eq!(reservations(&pool, "n1").await, 1);
        assert_eq!(feed_lines(&pool, "n1").await, 0);
    }
}
