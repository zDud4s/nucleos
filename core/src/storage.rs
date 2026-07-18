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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_creates_db_file_and_schema_meta_table() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nucleos.db");

        let pool = open(&db_path).await.unwrap();

        assert!(db_path.exists());
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM schema_meta")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, 0);
    }

    #[tokio::test]
    async fn open_creates_runs_table() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("nucleos.db");

        let pool = open(&db_path).await.unwrap();

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
        .execute(&pool)
        .await
        .unwrap();

        let row: (Option<String>, Option<String>, Option<String>, Option<f64>) = sqlx::query_as(
            "SELECT project_id, cwd, session_id, cost_usd FROM runs WHERE prompt = 'hello'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("proj-1"));
        assert_eq!(row.1.as_deref(), Some("/tmp/proj-1"));
        assert_eq!(row.2.as_deref(), Some("sess-abc"));
        assert_eq!(row.3, Some(0.42));
    }
}
