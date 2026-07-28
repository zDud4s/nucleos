use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use std::path::Path;

pub async fn open(db_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(sqlx::Error::Io)?;
    }
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// A database on disk plus the directory holding it, for the tests that need a real file rather
/// than `sqlite::memory:` — WAL behaviour, migrations against a fresh file, a handler parked on a
/// pool while another connection watches it.
///
/// It exists because the obvious version leaks, silently and forever. `TempDir`'s drop removes the
/// directory and IGNORES the error when it cannot; on Windows it cannot, because SQLite still holds
/// the file open, so the directory simply outlives the run. Nothing fails, nothing is logged, and
/// they accumulate — there were roughly 780 of them before anyone counted.
///
/// So the pool has to be closed before the directory goes. That cannot be done in `Drop`: closing
/// is async, `#[tokio::test]` runs on a current-thread runtime, and dropping a pool schedules the
/// connection shutdown as a task ON that runtime. Blocking inside `Drop` to wait for it would
/// starve the very task being waited for. `close` is therefore explicit and consuming — the value
/// cannot be used afterwards, and `a_closed_temp_db_leaves_nothing_behind` is what notices if
/// someone drops the call.
#[cfg(test)]
pub(crate) struct TempDb {
    pub pool: SqlitePool,
    dir: tempfile::TempDir,
}

#[cfg(test)]
impl TempDb {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().expect("create database tempdir");
        let pool = open(&dir.path().join("nucleos.db"))
            .await
            .expect("open database");
        Self { pool, dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Closes the pool, then lets the directory go. Consuming, so the order cannot be got wrong.
    pub async fn close(self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard for the whole mechanism above. It asserts the absence that every other test here
    /// depends on and none of them would notice: a leaked directory breaks nothing, it just stays.
    #[tokio::test]
    async fn a_closed_temp_db_leaves_nothing_behind() {
        let db = TempDb::new().await;
        let path = db.path().to_path_buf();
        assert!(
            path.exists(),
            "the directory should exist while the db is open"
        );

        db.close().await;

        assert!(
            !path.exists(),
            "the temp directory outlived its pool: {}",
            path.display()
        );
    }

    #[tokio::test]
    async fn open_creates_db_file_and_schema_meta_table() {
        let db = TempDb::new().await;

        assert!(db.path().join("nucleos.db").exists());
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM schema_meta")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(row.0, 0);
        db.close().await;
    }

    #[tokio::test]
    async fn open_creates_runs_table() {
        let db = TempDb::new().await;
        let pool = &db.pool;

        // Insert a row exercising every Autopilot-support column (project_id/cwd/session_id/cost_usd)
        // and read them back — proves the columns exist with the right types.
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, cost_usd, created_at)
             VALUES (?, ?, ?, 'running', ?, ?, ?)",
        )
        .bind("proj-1")
        .bind("/tmp/proj-1")
        .bind("hello")
        .bind("sess-abc")
        .bind(0.42_f64)
        .bind("2026-07-17T00:00:00Z")
        .execute(pool)
        .await
        .unwrap();

        let row: (Option<String>, Option<String>, Option<String>, Option<f64>) = sqlx::query_as(
            "SELECT project_id, cwd, session_id, cost_usd FROM runs WHERE prompt = 'hello'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("proj-1"));
        assert_eq!(row.1.as_deref(), Some("/tmp/proj-1"));
        assert_eq!(row.2.as_deref(), Some("sess-abc"));
        assert_eq!(row.3, Some(0.42));
        db.close().await;
    }
}
