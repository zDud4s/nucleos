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
/// received it. So the cached template file is copied (`fs::copy`: one in-kernel copy, a block
/// clone where the filesystem has one, and the 2 MB never passes through this process's heap) to a
/// file of its own under the temp directory and opened like `storage::open` opens the real one —
/// WAL, foreign keys on.
///
/// Cost per call, before: a listing of the whole pool directory (every test is its own process
/// under nextest, so the once-per-process sweep ran for each one), a 2 MB read of the template
/// into memory, a 2 MB `fs::write`, and a file left behind for good (2866 of them, ~6 GB). After:
/// one `stat` on a sweep stamp, a 16-byte read to vet the template, one `fs::copy`, and the
/// database (+ `-wal`/`-shm`) removed when the pool is dropped. A leftover (a crash, or a Windows
/// handle still closing) is swept by a later process once the stamp is an hour old, and that sweep
/// looks at a bounded number of entries.
pub async fn fresh_shared_pool() -> sqlx::SqlitePool {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    let template = template_file().await;
    let dir = std::env::temp_dir().join("nucleos-testdb-pools");
    std::fs::create_dir_all(&dir).unwrap();
    if sweep_due(&dir.join(SWEEP_STAMP), SWEEP_INTERVAL) {
        sweep_older_than(&dir, SWEEP_AGE, SWEEP_MAX_ENTRIES);
    }
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
    let copied = template.is_some_and(|template| std::fs::copy(template, &path).is_ok());
    if !copied {
        std::fs::write(&path, migrated_image().await).unwrap();
    }
    // The guard rides in the pool's own `after_connect` closure, which the pool owns, so the files
    // go when the last clone of the pool is dropped.
    let guard = std::sync::Arc::new(PoolFiles(path.clone()));
    SqlitePoolOptions::new()
        .max_connections(5)
        .after_connect(move |_connection, _meta| {
            let _keep = &guard;
            Box::pin(async move { Ok(()) })
        })
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(10)),
        )
        .await
        .unwrap()
}

const SWEEP_STAMP: &str = ".sweep-stamp";
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);
const SWEEP_AGE: std::time::Duration = std::time::Duration::from_secs(3600);
/// A sweep looks at no more entries than this, so a directory that grew large costs a bounded
/// amount per sweep and is worked down over several.
const SWEEP_MAX_ENTRIES: usize = 512;
/// A template image of another migration hash is kept this long before it is removed.
const TEMPLATE_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
const TEMPLATE_MAX_ENTRIES: usize = 20_000;

/// Removes a shared test database and its `-wal`/`-shm` when dropped. Best effort.
struct PoolFiles(std::path::PathBuf);

impl Drop for PoolFiles {
    fn drop(&mut self) {
        remove_db_files(&self.0);
    }
}

fn remove_db_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(name));
    }
}

/// True when the sweep stamp is missing or older than `interval`; the stamp is then renewed, so
/// the check is one `stat` per call and at most one sweep per interval across all processes.
fn sweep_due(stamp: &std::path::Path, interval: std::time::Duration) -> bool {
    let fresh = std::fs::metadata(stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|elapsed| elapsed <= interval);
    if fresh {
        return false;
    }
    let _ = std::fs::write(stamp, b"");
    true
}

/// Removes files in `dir` last modified more than `age` ago, looking at no more than
/// `max_entries` entries.
fn sweep_older_than(dir: &std::path::Path, age: std::time::Duration, max_entries: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(max_entries) {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed > age);
        if stale && entry.file_name() != SWEEP_STAMP {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Removes template images (and half-written staging files) of other migration hashes from `dir`,
/// keeping `keep` and anything newer than `age`.
fn remove_stale_templates(
    dir: &std::path::Path,
    keep: &std::path::Path,
    age: std::time::Duration,
    max_entries: usize,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(max_entries) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_template = name.starts_with("nucleos-testdb-")
            && (name.ends_with(".sqlite") || name.ends_with(".tmp"));
        if !is_template || entry.path() == keep {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed > age);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn has_sqlite_header(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok()
        && head == *b"SQLite format 3\0"
}

/// The template image as a file, built (and left in the temp directory) if no process has yet.
/// `None` when it cannot be written; the caller then writes the in-memory image instead.
async fn template_file() -> Option<&'static std::path::Path> {
    static TEMPLATE: OnceCell<Option<std::path::PathBuf>> = OnceCell::const_new();
    TEMPLATE
        .get_or_init(|| async {
            let path = schema_cache_path();
            if !has_sqlite_header(&path) {
                migrated_image().await;
            }
            has_sqlite_header(&path).then_some(path)
        })
        .await
        .as_deref()
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
    if let Some(dir) = path.parent() {
        remove_stale_templates(dir, &path, TEMPLATE_AGE, TEMPLATE_MAX_ENTRIES);
    }
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

/// How long a test is willing to wait for something that moves through real processes (git, a
/// spawned unit): bounded by silence, not by the wall clock.
///
/// A fixed deadline measures the machine as much as the code: under load the same gate takes three
/// times as long and a 30 s budget fails with nothing wrong. Here the clock restarts whenever the
/// progress marker the caller reads (a status column, a step) changes, so only a wait that stopped
/// moving for `idle` fails, and `cap` bounds a wait that moves for ever.
pub struct Patience {
    idle: std::time::Duration,
    cap: std::time::Duration,
    started: std::time::Instant,
    last_change: std::time::Instant,
    last: Option<String>,
}

impl Patience {
    pub fn new(idle: std::time::Duration, cap: std::time::Duration) -> Self {
        Self::starting_at(idle, cap, std::time::Instant::now())
    }

    fn starting_at(
        idle: std::time::Duration,
        cap: std::time::Duration,
        now: std::time::Instant,
    ) -> Self {
        Self {
            idle,
            cap,
            started: now,
            last_change: now,
            last: None,
        }
    }

    /// Whether the wait must give up, given the progress marker as of now.
    pub fn expired(&mut self, progress: &str) -> bool {
        self.expired_at(std::time::Instant::now(), progress)
    }

    fn expired_at(&mut self, now: std::time::Instant, progress: &str) -> bool {
        if self.last.as_deref() != Some(progress) {
            self.last = Some(progress.to_owned());
            self.last_change = now;
        }
        now.duration_since(self.started) >= self.cap
            || now.duration_since(self.last_change) >= self.idle
    }

    /// The last marker seen, for the failure message.
    pub fn last(&self) -> &str {
        self.last.as_deref().unwrap_or("")
    }
}

#[cfg(test)]
mod patience_tests {
    use super::Patience;
    use std::time::{Duration, Instant};

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn a_wait_that_keeps_moving_outlives_the_idle_budget() {
        let t0 = Instant::now();
        let mut p = Patience::starting_at(S(30), S(600), t0);
        assert!(!p.expired_at(t0 + S(25), "queued"));
        assert!(!p.expired_at(t0 + S(50), "running"));
        assert!(!p.expired_at(t0 + S(75), "done"));
    }

    #[test]
    fn a_wait_that_stops_moving_expires_after_the_idle_budget() {
        let t0 = Instant::now();
        let mut p = Patience::starting_at(S(30), S(600), t0);
        assert!(!p.expired_at(t0 + S(10), "queued"));
        assert!(!p.expired_at(t0 + S(39), "queued"));
        assert!(p.expired_at(t0 + S(40), "queued"));
        assert_eq!(p.last(), "queued");
    }

    #[test]
    fn the_cap_bounds_a_wait_that_never_stops_moving() {
        let t0 = Instant::now();
        let mut p = Patience::starting_at(S(30), S(100), t0);
        assert!(!p.expired_at(t0 + S(90), "a"));
        assert!(p.expired_at(t0 + S(100), "b"));
    }
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
    fn dropping_the_files_guard_removes_the_database_and_its_wal() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["x.sqlite", "x.sqlite-wal", "x.sqlite-shm", "other"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        drop(super::PoolFiles(dir.path().join("x.sqlite")));
        assert!(!dir.path().join("x.sqlite").exists());
        assert!(!dir.path().join("x.sqlite-wal").exists());
        assert!(!dir.path().join("x.sqlite-shm").exists());
        assert!(dir.path().join("other").exists());
    }

    #[test]
    fn the_sweep_stamp_gates_the_sweep_to_once_per_interval() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join(super::SWEEP_STAMP);
        let hour = std::time::Duration::from_secs(3600);
        assert!(super::sweep_due(&stamp, hour), "no stamp: due");
        assert!(!super::sweep_due(&stamp, hour), "fresh stamp: not due");
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(super::sweep_due(&stamp, std::time::Duration::ZERO));
    }

    #[test]
    fn a_file_sweep_looks_at_a_bounded_number_of_entries_and_keeps_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..6 {
            std::fs::write(dir.path().join(format!("f{index}")), b"x").unwrap();
        }
        std::fs::write(dir.path().join(super::SWEEP_STAMP), b"").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        super::sweep_older_than(dir.path(), std::time::Duration::ZERO, 3);
        let left = std::fs::read_dir(dir.path()).unwrap().count();
        assert!(left >= 4, "at most 3 of 7 entries examined, left {left}");
        super::sweep_older_than(dir.path(), std::time::Duration::ZERO, 100);
        assert!(dir.path().join(super::SWEEP_STAMP).exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn stale_templates_of_other_hashes_go_and_the_current_one_stays() {
        let dir = tempfile::tempdir().unwrap();
        let keep = dir.path().join("nucleos-testdb-aaaa.sqlite");
        let old = dir.path().join("nucleos-testdb-bbbb.sqlite");
        let staged = dir.path().join("nucleos-testdb-bbbb.123.tmp");
        let other = dir.path().join("unrelated.sqlite");
        for file in [&keep, &old, &staged, &other] {
            std::fs::write(file, b"x").unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        super::remove_stale_templates(dir.path(), &keep, std::time::Duration::ZERO, 100);
        assert!(keep.exists() && other.exists());
        assert!(!old.exists() && !staged.exists());
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
