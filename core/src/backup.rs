use chrono::{NaiveDateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_RETENTION: usize = 10;

#[derive(Debug, Clone, Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub migration_version: Option<i64>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StagedRestore {
    pub name: String,
    pub migration_version: i64,
    pub applies: &'static str,
}

#[derive(Debug, Clone)]
pub struct AppliedRestore {
    pub restored_from: String,
    pub safety_backup: PathBuf,
}

#[derive(Debug)]
pub enum BackupError {
    Database(sqlx::Error),
    ExistingTarget(PathBuf),
    Io(std::io::Error),
    InvalidName,
    InvalidRetention,
    NotFound,
    PendingRestoreExists,
    Verification(String),
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "database error: {error}"),
            Self::ExistingTarget(path) => {
                write!(
                    formatter,
                    "backup target already exists: {}",
                    path.display()
                )
            }
            Self::Io(error) => write!(formatter, "backup file error: {error}"),
            Self::InvalidName => formatter.write_str("invalid backup name"),
            Self::InvalidRetention => formatter.write_str("backup retention must be at least one"),
            Self::NotFound => formatter.write_str("backup not found"),
            Self::PendingRestoreExists => formatter.write_str("a restore is already pending"),
            Self::Verification(reason) => write!(formatter, "backup verification failed: {reason}"),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<sqlx::Error> for BackupError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<std::io::Error> for BackupError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Takes a transactionally consistent snapshot of the live database.
///
/// This must remain `VACUUM INTO`, not `fs::copy`. The live pool uses WAL mode, so the newest
/// committed pages can exist only in `nucleos.db-wal`; copying `nucleos.db` alone creates a
/// plausible SQLite file that silently lacks those commits. `VACUUM INTO` asks SQLite itself for
/// one consistent, standalone snapshot while the database is live. SQLite also requires the
/// destination not to exist, which is a safety property here: backups are never overwritten.
pub async fn take_backup(pool: &SqlitePool, retention: usize) -> Result<BackupInfo, BackupError> {
    if retention == 0 {
        return Err(BackupError::InvalidRetention);
    }
    let db_path = database_path(pool).await?;
    let (info, _) = snapshot(pool, &db_path).await?;
    prune_backups(&db_path, retention, &info.name)?;
    Ok(info)
}

pub async fn list_backups(pool: &SqlitePool) -> Result<Vec<BackupInfo>, BackupError> {
    let db_path = database_path(pool).await?;
    list_backup_files(&db_path)
}

pub async fn stage_restore(pool: &SqlitePool, name: &str) -> Result<StagedRestore, BackupError> {
    validate_name(name)?;
    let db_path = database_path(pool).await?;
    let source = resolve_backup(&db_path, name)?;
    let version = verify_backup(pool, &source).await?;
    let marker = pending_marker_path(&db_path);
    if marker.exists() {
        return Err(BackupError::PendingRestoreExists);
    }

    // A stale safety reference can only remain after a restore whose primary marker was
    // successfully cleared. It no longer belongs to a pending operation.
    remove_if_exists(&safety_marker_path(&db_path))?;
    atomic_write_new(&marker, source.to_string_lossy().as_bytes())?;
    Ok(StagedRestore {
        name: name.to_owned(),
        migration_version: version,
        applies: "on next daemon start",
    })
}

pub async fn apply_pending_restore(db_path: &Path) -> Result<Option<AppliedRestore>, BackupError> {
    let marker = pending_marker_path(db_path);
    if !marker.exists() {
        return Ok(None);
    }

    let source = read_recorded_backup(db_path, &marker)?;
    let safety_marker = safety_marker_path(db_path);
    let safety_backup = if safety_marker.exists() {
        read_recorded_backup(db_path, &safety_marker)?
    } else {
        if !db_path.exists() {
            return Err(BackupError::Verification(
                "current database is missing before its safety snapshot was recorded".to_owned(),
            ));
        }

        let live_pool = open_existing(db_path).await?;
        let safety_result = async {
            verify_backup(&live_pool, &source).await?;
            snapshot(&live_pool, db_path).await
        }
        .await;
        live_pool.close().await;
        let (_, safety_path) = safety_result?;
        atomic_write_new(&safety_marker, safety_path.to_string_lossy().as_bytes())?;
        safety_path
    };

    let expected_version = verify_pair(&safety_backup, &source).await?;
    let replacement = replacement_path(db_path);
    remove_if_exists(&replacement)?;
    copy_new(&source, &replacement)?;
    let replacement_version = verified_version(&replacement).await?;
    if replacement_version != expected_version {
        remove_if_exists(&replacement)?;
        return Err(BackupError::Verification(format!(
            "prepared restore has migration version {replacement_version}, expected {expected_version}"
        )));
    }

    // The safety snapshot and its durable reference exist before any live file is displaced.
    // From here on, every failure retains the primary marker, so startup retries the operation.
    for sidecar in sqlite_sidecars(db_path) {
        remove_if_exists(&sidecar)?;
    }
    remove_if_exists(db_path)?;
    fs::rename(&replacement, db_path)?;

    let applied_version = verified_version(db_path).await?;
    if applied_version != expected_version {
        return Err(BackupError::Verification(format!(
            "restored database has migration version {applied_version}, expected {expected_version}"
        )));
    }

    // This is the pending marker. It is cleared only after the replacement is present and verified.
    // If removing it fails, the safety reference remains and the next startup retries safely.
    fs::remove_file(&marker)?;
    let _ = remove_if_exists(&safety_marker);

    Ok(Some(AppliedRestore {
        restored_from: source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        safety_backup,
    }))
}

async fn verify_backup(live_pool: &SqlitePool, backup_path: &Path) -> Result<i64, BackupError> {
    let live_version = latest_migration(live_pool).await?;
    let backup_version = verified_version(backup_path).await?;
    if backup_version != live_version {
        return Err(BackupError::Verification(format!(
            "migration version {backup_version} does not match live version {live_version}"
        )));
    }
    Ok(backup_version)
}

fn pending_marker_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("pending-restore")
}

fn safety_marker_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("pending-restore-safety")
}

fn replacement_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("restore-new")
}

fn backup_dir(db_path: &Path) -> Result<PathBuf, BackupError> {
    let parent = db_path.parent().ok_or_else(|| {
        BackupError::Verification("database path has no parent directory".to_owned())
    })?;
    Ok(parent.join("backups"))
}

async fn database_path(pool: &SqlitePool) -> Result<PathBuf, BackupError> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as("PRAGMA database_list")
        .fetch_all(pool)
        .await?;
    let path = rows
        .into_iter()
        .find_map(|(_, name, file)| (name == "main" && !file.is_empty()).then_some(file))
        .ok_or_else(|| {
            BackupError::Verification("the live database is not file-backed".to_owned())
        })?;
    Ok(PathBuf::from(path))
}

fn validate_name(name: &str) -> Result<(), BackupError> {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(BackupError::InvalidName);
    }
    let stem = name
        .strip_prefix("nucleos-")
        .and_then(|value| value.strip_suffix(".db"))
        .ok_or(BackupError::InvalidName)?;
    let (timestamp, sequence) = stem.rsplit_once('-').ok_or(BackupError::InvalidName)?;
    if sequence.len() != 4 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(BackupError::InvalidName);
    }
    NaiveDateTime::parse_from_str(timestamp, "%Y%m%dT%H%M%S%.9fZ")
        .map_err(|_| BackupError::InvalidName)?;
    Ok(())
}

fn resolve_backup(db_path: &Path, name: &str) -> Result<PathBuf, BackupError> {
    validate_name(name)?;
    let root = backup_dir(db_path)?;
    let canonical_root = fs::canonicalize(&root).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            BackupError::NotFound
        } else {
            BackupError::Io(error)
        }
    })?;
    let joined = root.join(name);
    let resolved = fs::canonicalize(joined).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            BackupError::NotFound
        } else {
            BackupError::Io(error)
        }
    })?;
    if !resolved.starts_with(&canonical_root) {
        return Err(BackupError::InvalidName);
    }
    Ok(resolved)
}

fn read_recorded_backup(db_path: &Path, marker: &Path) -> Result<PathBuf, BackupError> {
    let recorded = fs::read_to_string(marker)?;
    let recorded = PathBuf::from(recorded);
    let name = recorded
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(BackupError::InvalidName)?;
    let resolved = resolve_backup(db_path, name)?;
    let canonical_recorded = fs::canonicalize(recorded).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            BackupError::NotFound
        } else {
            BackupError::Io(error)
        }
    })?;
    if canonical_recorded != resolved {
        return Err(BackupError::InvalidName);
    }
    Ok(resolved)
}

fn next_backup_path(db_path: &Path) -> Result<(String, PathBuf), BackupError> {
    let directory = backup_dir(db_path)?;
    fs::create_dir_all(&directory)?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    for sequence in 0..=9999 {
        let name = format!("nucleos-{timestamp}-{sequence:04}.db");
        let path = directory.join(&name);
        if !path.exists() {
            return Ok((name, path));
        }
    }
    Err(BackupError::ExistingTarget(directory))
}

async fn snapshot(pool: &SqlitePool, db_path: &Path) -> Result<(BackupInfo, PathBuf), BackupError> {
    let (name, path) = next_backup_path(db_path)?;
    if path.exists() {
        return Err(BackupError::ExistingTarget(path));
    }
    let destination = path.to_string_lossy().into_owned();
    if let Err(error) = sqlx::query("VACUUM INTO ?")
        .bind(destination)
        .execute(pool)
        .await
    {
        let _ = remove_if_exists(&path);
        return Err(BackupError::Database(error));
    }

    let version = match verify_backup(pool, &path).await {
        Ok(version) => version,
        Err(error) => {
            let _ = remove_if_exists(&path);
            return Err(error);
        }
    };
    let size_bytes = fs::metadata(&path)?.len();
    Ok((
        BackupInfo {
            name,
            migration_version: Some(version),
            size_bytes,
        },
        path,
    ))
}

fn list_backup_files(db_path: &Path) -> Result<Vec<BackupInfo>, BackupError> {
    let directory = backup_dir(db_path)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut backups = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if validate_name(&name).is_err() {
            continue;
        }
        backups.push(BackupInfo {
            name,
            migration_version: None,
            size_bytes: entry.metadata()?.len(),
        });
    }
    backups.sort_by(|left, right| right.name.cmp(&left.name));
    Ok(backups)
}

fn prune_backups(db_path: &Path, retention: usize, just_taken: &str) -> Result<(), BackupError> {
    if retention == 0 {
        return Err(BackupError::InvalidRetention);
    }
    let backups = list_backup_files(db_path)?;
    let mut keep = HashSet::from([just_taken.to_owned()]);

    // A restore staged before another backup request must not have its source pruned away.
    let marker = pending_marker_path(db_path);
    if marker.exists()
        && let Ok(source) = fs::read_to_string(&marker)
        && let Some(name) = Path::new(&source)
            .file_name()
            .and_then(|name| name.to_str())
    {
        keep.insert(name.to_owned());
    }

    for backup in &backups {
        if keep.len() >= retention {
            break;
        }
        keep.insert(backup.name.clone());
    }
    let directory = backup_dir(db_path)?;
    for backup in backups {
        if !keep.contains(&backup.name) {
            fs::remove_file(directory.join(backup.name))?;
        }
    }
    Ok(())
}

async fn open_existing(path: &Path) -> Result<SqlitePool, BackupError> {
    Ok(SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(10)),
        )
        .await?)
}

async fn open_read_only_database(path: &Path) -> Result<SqlitePool, BackupError> {
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .read_only(true),
        )
        .await?)
}

async fn latest_migration(pool: &SqlitePool) -> Result<i64, BackupError> {
    let version = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(version) FROM _sqlx_migrations WHERE success = TRUE",
    )
    .fetch_one(pool)
    .await?;
    version.ok_or_else(|| {
        BackupError::Verification("database has no successfully applied migration".to_owned())
    })
}

async fn verified_version(path: &Path) -> Result<i64, BackupError> {
    let pool = open_read_only_database(path).await?;
    let result = async {
        let checks = sqlx::query_scalar::<_, String>("PRAGMA quick_check")
            .fetch_all(&pool)
            .await?;
        if checks.as_slice() != ["ok"] {
            return Err(BackupError::Verification(format!(
                "SQLite quick_check returned {}",
                checks.join("; ")
            )));
        }
        latest_migration(&pool).await
    }
    .await;
    pool.close().await;
    result
}

async fn verify_pair(left: &Path, right: &Path) -> Result<i64, BackupError> {
    let left_version = verified_version(left).await?;
    let right_version = verified_version(right).await?;
    if left_version != right_version {
        return Err(BackupError::Verification(format!(
            "migration version {right_version} does not match safety version {left_version}"
        )));
    }
    Ok(right_version)
}

fn atomic_write_new(path: &Path, contents: &[u8]) -> Result<(), BackupError> {
    if path.exists() {
        return Err(BackupError::PendingRestoreExists);
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let result = (|| -> Result<(), BackupError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_if_exists(&temp);
    }
    result
}

fn copy_new(source: &Path, destination: &Path) -> Result<(), BackupError> {
    let mut source_file = OpenOptions::new().read(true).open(source)?;
    let mut destination_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    std::io::copy(&mut source_file, &mut destination_file)?;
    destination_file.sync_all()?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<(), BackupError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(BackupError::Io(error)),
    }
}

fn sqlite_sidecars(db_path: &Path) -> [PathBuf; 2] {
    [
        PathBuf::from(format!("{}-wal", db_path.display())),
        PathBuf::from(format!("{}-shm", db_path.display())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::TempDb;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::fs::OpenOptions;

    async fn open_read_only(path: &Path) -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .expect("open SQLite database read-only")
    }

    async fn create_probe(pool: &SqlitePool, value: &str) {
        sqlx::query("CREATE TABLE IF NOT EXISTS backup_probe (value TEXT NOT NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO backup_probe (value) VALUES (?)")
            .bind(value)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn probe_values(path: &Path) -> Vec<String> {
        let pool = open_read_only(path).await;
        let values =
            sqlx::query_scalar::<_, String>("SELECT value FROM backup_probe ORDER BY rowid")
                .fetch_all(&pool)
                .await
                .unwrap();
        pool.close().await;
        values
    }

    #[tokio::test]
    async fn vacuum_backup_contains_uncheckpointed_wal_content() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        sqlx::query("CREATE TABLE backup_probe (value TEXT NOT NULL)")
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&db.pool)
            .await
            .unwrap();

        let mut connection = db.pool.acquire().await.unwrap();
        sqlx::query("PRAGMA wal_autocheckpoint = 0")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query("INSERT INTO backup_probe (value) VALUES ('wal-only')")
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);

        let wal_path = db_path.with_extension("db-wal");
        assert!(
            wal_path.metadata().unwrap().len() > 0,
            "the test must leave committed content in the WAL"
        );

        let plain_copy = db.path().join("plain-copy.db");
        std::fs::copy(&db_path, &plain_copy).unwrap();
        assert_eq!(
            probe_values(&plain_copy).await,
            Vec::<String>::new(),
            "the control proves copying only nucleos.db loses the WAL commit"
        );

        let backup = take_backup(&db.pool, 5).await.unwrap();
        let backup_path = db.path().join("backups").join(backup.name);
        assert_eq!(probe_values(&backup_path).await, vec!["wal-only"]);
        db.close().await;
    }

    #[tokio::test]
    async fn verification_rejects_corrupt_truncated_and_non_sqlite_files() {
        let db = TempDb::new().await;
        let backup = take_backup(&db.pool, 5).await.unwrap();
        let backup_path = db.path().join("backups").join(backup.name);

        let truncated = db.path().join("truncated.db");
        std::fs::copy(&backup_path, &truncated).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&truncated)
            .unwrap()
            .set_len(64)
            .unwrap();
        assert!(verify_backup(&db.pool, &truncated).await.is_err());

        let corrupt = db.path().join("corrupt.db");
        let mut bytes = std::fs::read(&backup_path).unwrap();
        bytes[0] ^= 0xff;
        std::fs::write(&corrupt, bytes).unwrap();
        assert!(verify_backup(&db.pool, &corrupt).await.is_err());

        let not_sqlite = db.path().join("not-sqlite.db");
        std::fs::write(&not_sqlite, b"definitely not sqlite").unwrap();
        assert!(verify_backup(&db.pool, &not_sqlite).await.is_err());
        db.close().await;
    }

    #[tokio::test]
    async fn staging_an_invalid_backup_leaves_no_pending_marker() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        let backups = db.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        let name = "nucleos-20260729T010203.000000000Z-0000.db";
        std::fs::write(backups.join(name), b"not sqlite").unwrap();

        assert!(stage_restore(&db.pool, name).await.is_err());
        assert!(!pending_marker_path(&db_path).exists());
        db.close().await;
    }

    #[tokio::test]
    async fn applying_restore_preserves_the_replaced_database_in_a_safety_backup() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        create_probe(&db.pool, "restored").await;
        let backup = take_backup(&db.pool, 5).await.unwrap();
        stage_restore(&db.pool, &backup.name).await.unwrap();
        create_probe(&db.pool, "current").await;
        db.pool.close().await;

        let applied = apply_pending_restore(&db_path)
            .await
            .unwrap()
            .expect("a restore was pending");

        assert_eq!(probe_values(&db_path).await, vec!["restored"]);
        assert_eq!(
            probe_values(&applied.safety_backup).await,
            vec!["restored", "current"]
        );
        assert!(!pending_marker_path(&db_path).exists());
    }

    #[tokio::test]
    async fn an_apply_failure_leaves_the_pending_marker_for_retry() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        let backup = take_backup(&db.pool, 5).await.unwrap();
        stage_restore(&db.pool, &backup.name).await.unwrap();
        db.pool.close().await;
        std::fs::remove_file(db.path().join("backups").join(backup.name)).unwrap();

        assert!(apply_pending_restore(&db_path).await.is_err());
        assert!(pending_marker_path(&db_path).exists());
    }

    #[tokio::test]
    async fn retention_keeps_the_newest_count_and_never_removes_the_new_backup() {
        let db = TempDb::new().await;

        let first = take_backup(&db.pool, 2).await.unwrap();
        let second = take_backup(&db.pool, 2).await.unwrap();
        let newest = take_backup(&db.pool, 2).await.unwrap();
        let listed = list_backups(&db.pool).await.unwrap();
        let names: Vec<&str> = listed.iter().map(|item| item.name.as_str()).collect();

        assert_eq!(listed.len(), 2);
        assert_eq!(names[0], newest.name);
        assert!(names.contains(&second.name.as_str()));
        assert!(!names.contains(&first.name.as_str()));
        assert!(take_backup(&db.pool, 0).await.is_err());
        assert_eq!(list_backups(&db.pool).await.unwrap().len(), 2);
        db.close().await;
    }
}
