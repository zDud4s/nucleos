//! Database fixtures the test suite shares.
//!
//! It exists because of one gap: `sqlx::migrate!().run()` applies the whole chain against empty
//! tables, so every `UPDATE` in every migration is unreachable by a suite that only ever builds a
//! fresh database — delete one and nothing goes red. Stopping the chain part-way and putting rows
//! in the gap is all it takes to reach them, and that is what these two functions are for.
//!
//! They were written in `vcs.rs` for migration `0049`, with a note asking that the second module
//! to need them move them somewhere neutral rather than copy them. `0129`'s backfill is the second
//! module; this is that somewhere.

use sqlx::sqlite::{SqliteConnectOptions, SqliteOwnedBuf, SqlitePoolOptions};
use tokio::sync::OnceCell;

static MIGRATED_SCHEMA: OnceCell<Vec<u8>> = OnceCell::const_new();

/// A fresh, isolated in-memory database with the current schema.
///
/// The migrator runs once per machine and migration set, not once per test: the migrated image is
/// kept in the temp directory (`schema_cache_path`) and every later caller, in this process or any
/// other, deserializes a copy of it. Each caller receives a separate writable SQLite allocation,
/// including its own copy of `_sqlx_migrations`.
///
/// Measured 2026-10-07 (nextest, full suite on master): a test that migrated its own database paid
/// ~1.8 s before its first assertion — the median test took 1.93 s, one with no database 0.03-0.1 s,
/// and the ~2,500 tests that migrate held ~40% of the suite's test time. The disk copy is what
/// makes this pay under nextest, which runs every test in a process of its own.
pub async fn fresh_pool() -> sqlx::SqlitePool {
    let schema = MIGRATED_SCHEMA.get_or_init(cached_schema).await;

    let pool = empty_memory_pool().await;
    let mut connection = pool.acquire().await.unwrap();
    connection
        .deserialize(
            None,
            SqliteOwnedBuf::try_from(schema.as_slice()).unwrap(),
            false,
        )
        .await
        .unwrap();
    drop(connection);
    pool
}

/// A fresh database with the current schema that several connections share, for a test whose
/// code holds one connection while it asks the pool for another (the `AppState` the HTTP handlers
/// take is built with five).
///
/// `fresh_pool` cannot serve those: a deserialized database is private to the connection that
/// received it. So the same cached image is written to a file of its own under the temp directory
/// and opened like `storage::open` opens the real one — WAL, foreign keys on. The file is left
/// behind when the test ends; files over an hour old are swept by the next process that takes one.
pub async fn fresh_shared_pool() -> sqlx::SqlitePool {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    static SWEEP: std::sync::Once = std::sync::Once::new();

    let schema = MIGRATED_SCHEMA.get_or_init(cached_schema).await;
    let dir = std::env::temp_dir().join("nucleos-testdb-pools");
    std::fs::create_dir_all(&dir).unwrap();
    SWEEP.call_once(|| sweep_older_than(&dir, std::time::Duration::from_secs(3600)));
    // The pid alone is reused across a long run; the clock keeps a stale `-wal` beside a reused
    // name from being replayed into a new database.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let path = dir.join(format!(
        "{}-{nanos}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, schema).unwrap();
    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(10)),
        )
        .await
        .unwrap()
}

fn sweep_older_than(dir: &std::path::Path, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed > age);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `sweep_older_than` for directories: removes every child directory of `dir` last modified more
/// than `age` ago. Best effort — one still held open by a live process just stays for next time.
pub(crate) fn sweep_dirs_older_than(dir: &std::path::Path, age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_dir())
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed > age);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The migrated database as bytes, for a fixture that needs it as a file of its own
/// (`storage::TempDb`).
pub async fn migrated_image() -> &'static [u8] {
    MIGRATED_SCHEMA.get_or_init(cached_schema).await
}

/// The migrated image from the temp directory, or a fresh migration that is then left there.
///
/// Keyed by every migration's version and checksum, so a branch that adds or edits one reads its
/// own file and never another branch's schema. Written to a process-unique name and renamed into
/// place, so a concurrent reader sees a whole image or none; one that does not start like a SQLite
/// file is ignored and rebuilt.
async fn cached_schema() -> Vec<u8> {
    let path = schema_cache_path();
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.starts_with(b"SQLite format 3\0")
    {
        return bytes;
    }
    let bytes = migrated_schema().await;
    let staging = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&staging, &bytes).is_ok() && std::fs::rename(&staging, &path).is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    bytes
}

fn schema_cache_path() -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    // `DefaultHasher::new()` uses fixed keys, so every process computes the same key.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for migration in crate::storage::MIGRATOR.iter() {
        migration.version.hash(&mut hasher);
        migration.checksum.hash(&mut hasher);
    }
    std::env::temp_dir().join(format!("nucleos-testdb-{:016x}.sqlite", hasher.finish()))
}

async fn migrated_schema() -> Vec<u8> {
    let pool = empty_memory_pool().await;
    crate::storage::MIGRATOR.run(&pool).await.unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let bytes = connection.serialize(None).await.unwrap().to_vec();
    drop(connection);
    pool.close().await;
    bytes
}

async fn empty_memory_pool() -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .create_if_missing(true),
        )
        .await
        .unwrap()
}

#[cfg(test)]
mod fresh_pool_tests {
    #[tokio::test]
    async fn a_shared_pool_sees_one_database_from_every_connection() {
        let pool = super::fresh_shared_pool().await;
        let mut held = pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO runs (prompt, status, created_at) VALUES ('shared', 'running', 'now')",
        )
        .execute(&mut *held)
        .await
        .unwrap();
        let seen: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE prompt = 'shared'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(seen, 1);
        let migrations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(migrations as usize, crate::storage::MIGRATOR.iter().count());
        drop(held);
        pool.close().await;
    }

    #[test]
    fn a_dir_sweep_removes_stale_directories_and_leaves_files_alone() {
        let parent = tempfile::tempdir().unwrap();
        let stale = parent.path().join("stale");
        std::fs::create_dir_all(stale.join("inner")).unwrap();
        std::fs::write(stale.join("inner").join("nucleos.db"), b"x").unwrap();
        let file = parent.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        super::sweep_dirs_older_than(parent.path(), std::time::Duration::from_secs(3600));
        assert!(stale.exists(), "a fresh directory is not stale");

        super::sweep_dirs_older_than(parent.path(), std::time::Duration::ZERO);
        assert!(!stale.exists(), "a directory older than the age is removed");
        assert!(file.exists(), "files are not this sweep's business");
    }

    #[tokio::test]
    async fn fresh_pools_have_the_full_schema_and_do_not_share_rows() {
        let first = super::fresh_pool().await;
        let migrations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&first)
            .await
            .unwrap();
        assert_eq!(migrations as usize, crate::storage::MIGRATOR.iter().count());
        sqlx::query(
            "INSERT INTO runs (prompt, status, created_at) VALUES ('only-first', 'running', 'now')",
        )
        .execute(&first)
        .await
        .unwrap();

        let second = super::fresh_pool().await;
        let copied: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE prompt = 'only-first'")
                .fetch_one(&second)
                .await
                .unwrap();
        assert_eq!(copied, 0);
        first.close().await;
        second.close().await;
    }
}

/// A database with every migration up to and including `version` applied, and none after.
///
/// The migrator's own list is walked rather than the files read directly, so this cannot drift
/// from what ships: the SQL is the SQL that will run on the real database, in the order it will
/// run there. Nothing is written to `_sqlx_migrations` — the bookkeeping is not what is under
/// test, and a caller finishes the chain with `apply_migrations_after`.
pub async fn pool_migrated_through(version: i64) -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .create_if_missing(true),
        )
        .await
        .unwrap();
    apply_migrations(&pool, |candidate| candidate <= version).await;
    pool
}

/// Finishes the chain a `pool_migrated_through` stopped, running everything above `version`.
pub async fn apply_migrations_after(pool: &sqlx::SqlitePool, version: i64) {
    apply_migrations(pool, |candidate| candidate > version).await;
}

/// Runs exactly one migration, for a test whose claim is about that file alone and must not see
/// what later migrations add on top of it.
pub async fn apply_migration(pool: &sqlx::SqlitePool, version: i64) {
    apply_migrations(pool, |candidate| candidate == version).await;
}

async fn apply_migrations(pool: &sqlx::SqlitePool, wanted: impl Fn(i64) -> bool) {
    for migration in crate::storage::MIGRATOR.iter() {
        if !wanted(migration.version) {
            continue;
        }
        // `raw_sql` rather than `query`: a migration is many statements, and `query` runs the
        // first and silently drops the rest — which would have made this harness quietly test
        // a fraction of each file.
        sqlx::raw_sql(migration.sql.clone())
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("migration {} failed: {error}", migration.version));
    }
}

/// Two migrations with one version number break every database, fresh or not. sqlx keys a
/// migration by its number alone: on an empty database the second insert into `_sqlx_migrations`
/// hits the primary key and the run fails; on one that already applied either file, the other's
/// checksum disagrees and startup stops with `VersionMismatch`. Git sees nothing wrong — the file
/// names differ — so two branches cut from the same tip can each take "the next number" and both
/// land. That is how two `0159_*` files reached master on 2026-10-04.
#[test]
fn no_two_migrations_share_a_version() {
    let mut seen = std::collections::BTreeMap::new();
    for migration in crate::storage::MIGRATOR.iter() {
        if let Some(first) = seen.insert(migration.version, migration.description.clone()) {
            panic!(
                "migrations '{first}' and '{}' both carry version {} — renumber the one the \
                 live database has not applied yet",
                migration.description, migration.version
            );
        }
    }
}
