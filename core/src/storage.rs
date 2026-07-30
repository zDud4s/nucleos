use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use std::path::Path;

/// A write that has been made durable but is not yet visible at its destination.
#[must_use = "a staged write does nothing until it is committed"]
pub struct Staged {
    path: std::path::PathBuf,
}

static STAGED_WRITE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `bytes` somewhere durable that is NOT `dest`, leaving `dest` as it was.
pub fn stage(dest: &std::path::Path, bytes: &[u8]) -> std::io::Result<Staged> {
    use std::io::Write;

    let parent = dest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let sequence = STAGED_WRITE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut file_name = dest
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("staged"))
        .to_os_string();
    file_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
    let path = parent.join(file_name);

    let mut file = std::fs::File::create(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;

    Ok(Staged { path })
}

/// Makes a staged write visible at its destination, replacing whatever was there.
pub fn commit(staged: Staged, dest: &std::path::Path) -> std::io::Result<()> {
    match std::fs::rename(&staged.path, dest) {
        Ok(()) => Ok(()),
        Err(error) => {
            // A failed commit must not accumulate anonymous staged files in repeated callers.
            let _ = std::fs::remove_file(&staged.path);
            Err(error)
        }
    }
}

/// Writes a file all-or-nothing: readers see the old bytes or the new ones, never a prefix.
///
/// `std::fs::write` truncates and then fills, so a process that dies in between leaves a file
/// that exists, is readable, and is wrong. Every caller here hands its file to something that
/// reads it immediately afterwards — an agent CLI, `schtasks` — so a truncated file is not a
/// state anyone notices before it is used.
pub fn write_atomic(dest: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let staged = stage(dest, bytes)?;
    commit(staged, dest)
}

pub async fn open(db_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(sqlx::Error::Io)?;
    }
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        // Stated rather than inherited. Every fail-closed gate in this daemon turns a database
        // error into a refusal, so how long a writer waits before becoming one is a decision worth
        // making here instead of depending on whichever default sqlx happens to ship.
        .busy_timeout(std::time::Duration::from_secs(10))
        // SQLite disables foreign keys per connection unless asked. The schema declares them
        // (`worktrees.run_id`, `action_grants.proposal_id`, ...), so without this they were
        // documentation: an orphaned row was free to exist and nothing said so.
        .foreign_keys(true);
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

    #[test]
    fn uma_escrita_por_confirmar_nao_toca_no_destino() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("estado");
        std::fs::write(&path, b"antigo").unwrap();

        let staged = stage(&path, b"novo").unwrap();

        // Staging without committing is the only way to observe atomicity without killing this
        // process. A test that merely writes and reads back would pass both before and after GREEN.
        assert_eq!(std::fs::read(&path).unwrap(), b"antigo");

        commit(staged, &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"novo");
    }

    #[test]
    fn uma_escrita_atomica_substitui_o_ficheiro_por_inteiro() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("estado");

        write_atomic(&path, b"conteudo antigo deliberadamente muito comprido").unwrap();
        write_atomic(&path, b"curto").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"curto");
    }

    #[test]
    fn uma_escrita_atomica_cria_o_que_ainda_nao_existe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("novo");

        write_atomic(&path, b"primeiro conteudo").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"primeiro conteudo");
    }

    #[test]
    fn uma_escrita_atomica_nao_deixa_temporarios() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("estado");

        write_atomic(&path, b"conteudo").unwrap();

        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

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
