#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    // Task 7 adds `use std::time::Duration;` when it first needs it — adding it now would warn as
    // an unused import on every run from here to Task 6.

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn insert(pool: &sqlx::SqlitePool, project: &str, status: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at)
             VALUES ('merge', '{}', ?, 'C:/repo', 'human', ?, '2026-08-02T00:00:00Z')",
        )
        .bind(project)
        .bind(status)
        .execute(pool)
        .await
        .map(|_| ())
    }

    /// Exclusivity is the database's job, not a Mutex's: a Mutex does not survive a daemon restart
    /// and this index does. Asserted sequentially on purpose — the constraint is what is under
    /// test, and the pool helper is `max_connections(1)`, so a "concurrent" version would prove
    /// less and flake more.
    #[tokio::test]
    async fn only_one_request_may_run_per_repository() {
        let pool = test_pool().await;

        insert(&pool, "alpha", "running").await.expect("the first running request is allowed");

        let second = insert(&pool, "alpha", "running").await;
        assert!(second.is_err(), "a second running request for the same repository must be rejected");

        insert(&pool, "beta", "running")
            .await
            .expect("a different repository is not blocked by alpha's running request");

        for _ in 0..3 {
            insert(&pool, "alpha", "queued")
                .await
                .expect("queued requests are not limited — only running is");
        }
    }
}
