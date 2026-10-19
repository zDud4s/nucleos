use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use std::path::Path;

/// The daemon's schema, embedded once. Every caller — `open` below, `testdb` and the tests that
/// migrate a pool of their own — goes through this one static instead of expanding
/// `sqlx::migrate!()` where it stands: each expansion embeds every migration again, and a bare
/// `migrate!()` resolves against whichever crate it is expanded in, which a crate split would move.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

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

/// How much of the database each connection maps into memory. Mapped pages live in the OS file
/// cache, shared by every connection, so this costs address space rather than five copies of RAM —
/// and a scan of an already-read table stops being thousands of `ReadFile` calls.
const MMAP_SIZE_BYTES: i64 = 256 * 1024 * 1024;

/// Each connection's private page cache. SQLite's default is 2 MiB, which no hot table here fits.
/// Kept modest because it is per connection, times `max_connections`; the map above does the
/// heavy lifting.
const CACHE_SIZE_KIB: i64 = 16 * 1024;

/// After a checkpoint SQLite truncates the `-wal` file down to this size. Without it the file keeps
/// the size of the largest burst it ever held (47 MB on the owner's machine) for as long as the
/// daemon runs.
pub const WAL_SIZE_LIMIT_BYTES: i64 = 64 * 1024 * 1024;

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
        .foreign_keys(true)
        .pragma("mmap_size", MMAP_SIZE_BYTES.to_string())
        .pragma("cache_size", (-CACHE_SIZE_KIB).to_string())
        .pragma("journal_size_limit", WAL_SIZE_LIMIT_BYTES.to_string());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;
    crate::storage::MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// Folds the write-ahead log back into the database file and truncates it to nothing. Returns
/// whether it could: a reader that is mid-query makes SQLite give up with `busy` set rather than
/// fail, and the next pass simply tries again, so `false` is not an error.
pub async fn truncate_wal(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let (busy, _, _): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await?;
    Ok(busy == 0)
}

/// What the database file itself occupies, for the "Space" panel. Everything except the page
/// counts comes from the file system, so a database that is not file-backed reports sizes of zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DatabaseStats {
    pub db_bytes: u64,
    pub wal_bytes: u64,
    pub page_size: i64,
    pub page_count: i64,
    pub freelist_count: i64,
    /// Pages a `VACUUM` would give back: the freelist, which the file keeps until then.
    pub reclaimable_bytes: u64,
}

pub async fn database_stats(pool: &SqlitePool) -> Result<DatabaseStats, sqlx::Error> {
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size").fetch_one(pool).await?;
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count").fetch_one(pool).await?;
    let freelist_count: i64 = sqlx::query_scalar("PRAGMA freelist_count")
        .fetch_one(pool)
        .await?;
    let (db_bytes, wal_bytes) = match database_file(pool).await? {
        Some(path) => (
            file_len(&path),
            file_len(&sibling_with_suffix(&path, "-wal")),
        ),
        None => (0, 0),
    };
    Ok(DatabaseStats {
        db_bytes,
        wal_bytes,
        page_size,
        page_count,
        freelist_count,
        reclaimable_bytes: u64::try_from(freelist_count.saturating_mul(page_size)).unwrap_or(0),
    })
}

/// The path of the live database file, or `None` for an in-memory one.
pub async fn database_file(pool: &SqlitePool) -> Result<Option<std::path::PathBuf>, sqlx::Error> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as("PRAGMA database_list")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .find_map(|(_, name, file)| (name == "main" && !file.is_empty()).then_some(file))
        .map(std::path::PathBuf::from))
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    std::path::PathBuf::from(name)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |meta| meta.len())
}

/// Total size of every file under `path`; zero when it does not exist. Symlinks are not followed,
/// so a link out of the folder cannot make the number describe somebody else's disk.
pub fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |meta| meta.len()),
            _ => 0,
        })
        .sum()
}

/// What `nucleos.db.*` files left beside the database (`nucleos.db.before-116-revert`, ...) occupy:
/// everything named `<database>.<something>`. The WAL and shared-memory files are `<database>-wal`
/// and `<database>-shm`, so the dot keeps them out.
pub fn loose_files_bytes(db_path: &Path) -> u64 {
    let (Some(directory), Some(name)) = (
        db_path.parent(),
        db_path.file_name().and_then(|name| name.to_str()),
    ) else {
        return 0;
    };
    let prefix = format!("{name}.");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            file_name.starts_with(&prefix)
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .map(|entry| entry.metadata().map_or(0, |meta| meta.len()))
        .sum()
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
#[cfg(any(test, feature = "testkit"))]
pub struct TempDb {
    pub pool: SqlitePool,
    dir: tempfile::TempDir,
}

#[cfg(any(test, feature = "testkit"))]
impl TempDb {
    pub async fn new() -> Self {
        // Under a parent of their own, swept of directories over an hour old by the first `new` of
        // every process. A test that panics before `close` (or never calls it) leaves its directory
        // behind — on Windows the open `nucleos.db` makes the TempDir's own removal fail silently —
        // and unswept those piled up in %TEMP%: 2,312 directories, 2.6 GB, measured 2026-10-17.
        static SWEEP: std::sync::Once = std::sync::Once::new();
        let parent = std::env::temp_dir().join("nucleos-testdb-dirs");
        std::fs::create_dir_all(&parent).expect("create database tempdir parent");
        SWEEP.call_once(|| {
            crate::testdb::sweep_dirs_older_than(&parent, std::time::Duration::from_secs(3600))
        });
        let dir = tempfile::tempdir_in(&parent).expect("create database tempdir");
        let path = dir.path().join("nucleos.db");
        // Seeded with the migrated image `testdb` caches, so `open` finds every migration applied
        // and only checks them instead of running the chain again.
        std::fs::write(&path, crate::testdb::migrated_image().await).expect("seed database");
        let pool = open(&path).await.expect("open database");
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

    /// Two branches, two migrations, one number, and a merge that says nothing.
    ///
    /// `_sqlx_migrations.version` is the PRIMARY KEY, so a repeated number is neither a merge
    /// conflict nor a compile error: it is a panic inside `open` on the next start, and only after
    /// the first file of the pair has already been applied and committed. That is the expensive
    /// part. The loser rolls back, but the winner is now recorded with its checksum, so the loser
    /// is the only one of the two that can still be renumbered — rename the winner and every
    /// database that ran it refuses to open at all.
    ///
    /// The migrator's own list is walked rather than the directory, so this cannot drift from what
    /// ships: it is the same embedded list `open` runs.
    #[test]
    fn no_two_migrations_claim_the_same_version() {
        use std::collections::BTreeMap;

        let mut claimed: BTreeMap<i64, String> = BTreeMap::new();
        for migration in crate::storage::MIGRATOR.iter() {
            let description = migration.description.to_string();
            if let Some(first) = claimed.insert(migration.version, description.clone()) {
                panic!(
                    "migration {} is claimed twice, by '{first}' and by '{description}' -- \
                     renumber the one no database has applied yet",
                    migration.version
                );
            }
        }
    }

    /// Every column of `runs` as `(name, declared type, not null, default)`, in table order.
    async fn runs_columns(pool: &SqlitePool) -> Vec<(String, String, bool, Option<String>)> {
        sqlx::query_as(
            "SELECT name, type, \"notnull\", dflt_value FROM pragma_table_info('runs') ORDER BY cid",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Every stored value of `runs`, quoted, keyed by row and column — a byte-level snapshot.
    async fn runs_values(pool: &SqlitePool) -> Vec<(i64, String, String)> {
        let mut values = Vec::new();
        for (column, ..) in runs_columns(pool).await {
            let rows: Vec<(i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT id, quote(\"{column}\") FROM runs ORDER BY id"
            )))
            .fetch_all(pool)
            .await
            .unwrap();
            values.extend(rows.into_iter().map(|(id, v)| (id, column.clone(), v)));
        }
        values.sort();
        values
    }

    async fn schema_of(pool: &SqlitePool, sql: &str) -> Vec<String> {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// Migration `0154` rebuilds `runs` with its large text columns last.
    ///
    /// SQLite keeps a value too large for its page in a chain of overflow pages, and reading any
    /// column declared AFTER that value means walking the chain. `stdout` was the seventh column
    /// of 56, so `budget`'s scan of `mode`/`cost_usd`/`created_at` read every run's whole output:
    /// 118 ms per call on the owner's 229 MB database, several calls every three seconds. The
    /// rebuild may move columns and nothing else — every row, value, default, index, foreign key
    /// and the AUTOINCREMENT high-water mark has to come out the other side unchanged.
    #[tokio::test]
    async fn migration_0154_moves_the_large_runs_columns_last_and_keeps_everything_else() {
        let pool = crate::testdb::pool_migrated_through(153).await;
        // The real pool runs with foreign keys on; the rebuild has to survive that.
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();

        let big = "x".repeat(20_000);
        for (id, successor) in [(1_i64, None), (2, Some(1_i64)), (3, None)] {
            sqlx::query(
                "INSERT INTO runs (id, project_id, prompt, status, stdout, stderr, gate_output, \
                 created_at, mode, cost_usd, successor_run_id, tools_used, thought) \
                 VALUES (?, 'p', 'ask', 'completed', ?, 'err', 'gate', '2026-10-01T00:00:00Z', \
                 'shadow', 0.25, ?, '[\"Read\"]', 'hm')",
            )
            .bind(id)
            .bind(&big)
            .bind(successor)
            .execute(&pool)
            .await
            .unwrap();
        }
        // The high-water mark sits above the highest surviving id; a rebuild that re-derived it
        // from the rows would hand id 3 out again.
        sqlx::query("DELETE FROM runs WHERE id = 3")
            .execute(&pool)
            .await
            .unwrap();

        let columns_before = runs_columns(&pool).await;
        let values_before = runs_values(&pool).await;
        let indexes_before = schema_of(
            &pool,
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'runs' \
             AND sql IS NOT NULL ORDER BY name",
        )
        .await;
        let children_before = schema_of(
            &pool,
            "SELECT m.name || '.' || f.\"from\" FROM sqlite_master m, pragma_foreign_key_list(m.name) f \
             WHERE m.type = 'table' AND f.\"table\" = 'runs' ORDER BY 1",
        )
        .await;

        // 0154 alone: later migrations may append columns with `ADD COLUMN`, which is not this
        // rebuild moving anything.
        crate::testdb::apply_migration(&pool, 154).await;

        let columns_after = runs_columns(&pool).await;
        let tail: Vec<&str> = columns_after[columns_after.len() - 7..]
            .iter()
            .map(|(name, ..)| name.as_str())
            .collect();
        assert_eq!(
            tail,
            [
                "tools_used",
                "prompt_images",
                "thought",
                "prompt",
                "gate_output",
                "stderr",
                "stdout"
            ]
        );
        let mut sorted_before = columns_before.clone();
        let mut sorted_after = columns_after.clone();
        sorted_before.sort();
        sorted_after.sort();
        assert_eq!(
            sorted_after, sorted_before,
            "a column changed beyond its position"
        );
        assert_eq!(runs_values(&pool).await, values_before);
        assert_eq!(
            schema_of(
                &pool,
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'runs' \
                 AND sql IS NOT NULL ORDER BY name",
            )
            .await,
            indexes_before
        );
        assert_eq!(
            schema_of(
                &pool,
                "SELECT m.name || '.' || f.\"from\" FROM sqlite_master m, pragma_foreign_key_list(m.name) f \
                 WHERE m.type = 'table' AND f.\"table\" = 'runs' ORDER BY 1",
            )
            .await,
            children_before
        );
        let violations: Vec<String> =
            sqlx::query_scalar("SELECT \"table\" FROM pragma_foreign_key_check")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(violations.is_empty(), "{violations:?}");
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            foreign_keys, 1,
            "the migration must hand the connection back with foreign keys on"
        );

        let next: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, created_at) VALUES ('q', 'running', 'now') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(next, 4);
    }

    /// Migrations `0198` and `0202` index the `runs` lookups that were full scans of a 229 MB table.
    #[tokio::test]
    async fn the_runs_lookups_by_job_status_and_project_use_an_index() {
        let pool = crate::testdb::fresh_pool().await;
        for (sql, index) in [
            (
                "SELECT id, stdout FROM runs WHERE job_id = 1 AND stage = 'review' ORDER BY id",
                "idx_runs_job_id",
            ),
            (
                "SELECT id FROM runs WHERE status = 'awaiting_approval'",
                "runs_by_status_created",
            ),
            (
                "SELECT 1 FROM runs WHERE project_id = 'p' AND status = 'running'",
                "runs_by_project_status",
            ),
        ] {
            // `EXPLAIN QUERY PLAN` has four columns; the detail is the last.
            let detail: Vec<(i64, i64, i64, String)> =
                sqlx::query_as(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {sql}")))
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert!(
                detail.iter().any(|row| row.3.contains(index)),
                "{sql} does not use {index}: {detail:?}"
            );
        }
    }

    /// The pool reads the database through a memory map and a page cache sized for it, rather than
    /// SQLite's 2 MiB default, so a repeated scan is served from memory instead of `ReadFile`.
    #[tokio::test]
    async fn every_connection_maps_the_database_and_keeps_a_real_page_cache() {
        let db = TempDb::new().await;
        let mmap: i64 = sqlx::query_scalar("PRAGMA mmap_size")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let cache: i64 = sqlx::query_scalar("PRAGMA cache_size")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        db.close().await;
        assert_eq!(mmap, MMAP_SIZE_BYTES);
        assert_eq!(cache, -CACHE_SIZE_KIB);
    }

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

    /// Fills a scratch table with `rows` rows of about 4 KiB each, in one write.
    async fn fill_scratch(pool: &SqlitePool, rows: i64) {
        sqlx::query("CREATE TABLE IF NOT EXISTS scratch (payload TEXT NOT NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c WHERE i < ?)              INSERT INTO scratch SELECT hex(randomblob(2048)) FROM c",
        )
        .bind(rows)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The limit is a per-connection pragma, so it has to be on every connection the pool opens,
    /// and the checkpoint the retention loop runs has to leave the `-wal` empty rather than at
    /// whatever size the last burst grew it to.
    #[tokio::test]
    async fn the_wal_is_capped_after_a_checkpoint() {
        let db = TempDb::new().await;
        let pool = &db.pool;

        let limit: i64 = sqlx::query_scalar("PRAGMA journal_size_limit")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(limit, WAL_SIZE_LIMIT_BYTES);

        fill_scratch(pool, 200).await;
        let before = database_stats(pool).await.unwrap();
        assert!(before.wal_bytes > 0, "the writes sit in the WAL until a checkpoint");

        assert!(truncate_wal(pool).await.unwrap(), "nothing else is reading");
        let after = database_stats(pool).await.unwrap();
        assert_eq!(after.wal_bytes, 0);
        assert!(after.db_bytes >= before.page_size as u64 * 200 / 2);
        db.close().await;
    }

    #[tokio::test]
    async fn the_stats_count_the_pages_a_delete_leaves_behind() {
        let db = TempDb::new().await;
        let pool = &db.pool;
        fill_scratch(pool, 200).await;
        let full = database_stats(pool).await.unwrap();
        assert_eq!(full.reclaimable_bytes, 0);

        sqlx::query("DELETE FROM scratch").execute(pool).await.unwrap();
        let emptied = database_stats(pool).await.unwrap();
        assert!(emptied.freelist_count > 0);
        assert_eq!(
            emptied.reclaimable_bytes,
            emptied.freelist_count as u64 * emptied.page_size as u64
        );
        db.close().await;
    }

    #[test]
    fn folder_sizes_sum_nested_files_and_loose_copies_skip_the_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("nucleos.db");
        for (name, len) in [
            ("nucleos.db", 10),
            ("nucleos.db-wal", 20),
            ("nucleos.db-shm", 30),
            ("nucleos.db.before-116-revert", 400),
            ("nucleos.db.old", 500),
            ("unrelated.db", 7000),
        ] {
            std::fs::write(dir.path().join(name), vec![0_u8; len]).unwrap();
        }
        assert_eq!(loose_files_bytes(&db), 900);

        let nested = dir.path().join("backups").join("deeper");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("a"), vec![0_u8; 3]).unwrap();
        std::fs::write(dir.path().join("backups").join("b"), vec![0_u8; 4]).unwrap();
        assert_eq!(directory_size(&dir.path().join("backups")), 7);
        assert_eq!(directory_size(&dir.path().join("missing")), 0);
    }
}
