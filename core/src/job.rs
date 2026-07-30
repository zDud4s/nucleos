//! The job state machine: a sequence of runs over one shared worktree.
//!
//! A run is one `claude -p` subprocess and therefore one context window, which caps how large a
//! piece of autonomous work can be. A job lifts that cap by running several nodes in sequence —
//! `plan → implement×N → gate → review` — each with a fresh window, sharing state through the
//! worktree on disk rather than through a transcript.

use sqlx::SqlitePool;

/// Starts a job and returns its id.
///
/// Fails when the project already has a live one. That refusal is the unique index
/// `one_live_job_per_project` (migration 0036) rather than a check here, deliberately: with the
/// constraint in the storage layer the INSERT itself is the lock, so a scheduler tick and a manual
/// request racing for the same project cannot both pass a check and then both proceed. It mirrors
/// what `one_open_worktree_run_per_project` already does for runs.
pub async fn insert_job(
    pool: &SqlitePool,
    project_id: &str,
    project_root: &str,
    status: &str,
    max_items: i64,
) -> sqlx::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(project_root)
    .bind(status)
    .bind(max_items)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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

    #[tokio::test]
    async fn a_second_live_job_for_a_project_is_rejected_by_the_index() {
        let pool = test_pool().await;
        insert_job(&pool, "project-a", "/project/a", "planning", 5)
            .await
            .expect("the first job starts");

        let second = insert_job(&pool, "project-a", "/project/a", "planning", 5).await;

        // Rejected by the unique index rather than by a check in this module: the INSERT is the
        // lock, so the scheduler tick and a manual POST racing for the same project cannot both win.
        assert!(second.is_err(), "a project may have only one live job");
    }

    #[tokio::test]
    async fn a_finished_job_does_not_hold_the_project_slot() {
        let pool = test_pool().await;
        insert_job(&pool, "project-a", "/project/a", "completed", 5)
            .await
            .expect("a job that has finished");

        // The partial index covers live statuses only. Without that, a project would be wedged
        // forever by its own first job — a worse failure than the race the index exists to stop,
        // because nothing would ever clear it.
        insert_job(&pool, "project-a", "/project/a", "planning", 5)
            .await
            .expect("a new job may start once the last one is done");
    }
}
