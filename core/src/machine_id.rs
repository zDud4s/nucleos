//! This install's stable machine id: a UUID generated at first start and kept in `nucleos.db`.
//!
//! Unlike the pairing alias (a local nickname that differs per paired machine), the id is the same
//! for every peer that asks, so a move history entry can name its destination unambiguously.

use sqlx::SqlitePool;

/// `schema_meta` key holding the id.
const KEY: &str = "machine.id";

/// The machine id, generated and stored on first use. Later calls (and restarts on the same db)
/// return the same value; two racing first calls agree because the insert keeps the first row.
pub async fn get_or_create(pool: &SqlitePool) -> sqlx::Result<String> {
    sqlx::query("INSERT OR IGNORE INTO schema_meta (key, value) VALUES (?, ?)")
        .bind(KEY)
        .bind(crate::auth::generate_uuid_v4())
        .execute(pool)
        .await?;
    sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = ?")
        .bind(KEY)
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_id_is_generated_once_and_stays_the_same() {
        let pool = crate::testdb::fresh_pool().await;
        let first = get_or_create(&pool).await.unwrap();
        let second = get_or_create(&pool).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(first.as_bytes()[14], b'4', "a version 4 UUID");
    }

    #[tokio::test]
    async fn the_id_survives_a_restart_of_the_same_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nucleos.db");
        let open = || async {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true);
            let pool = SqlitePool::connect_with(options).await.unwrap();
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool
        };
        let first_pool = open().await;
        let first = get_or_create(&first_pool).await.unwrap();
        first_pool.close().await;
        let second_pool = open().await;
        assert_eq!(get_or_create(&second_pool).await.unwrap(), first);
    }

    #[tokio::test]
    async fn two_installs_get_different_ids() {
        let a = get_or_create(&crate::testdb::fresh_pool().await)
            .await
            .unwrap();
        let b = get_or_create(&crate::testdb::fresh_pool().await)
            .await
            .unwrap();
        assert_ne!(a, b);
    }
}
