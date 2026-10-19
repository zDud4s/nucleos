use chrono::{DateTime, NaiveDateTime, Utc};
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

/// Automatic copies kept. Two weekly snapshots of a database that is hundreds of megabytes is the
/// ceiling the owner chose (2026-10-19); the manual copies are never counted against it.
pub const AUTO_RETENTION: usize = 2;

/// How long the newest automatic copy stays good before the maintenance loop takes another.
pub const AUTO_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Marks a copy the daemon took on its own, so pruning and listing can tell it from a manual one.
const AUTO_PREFIX: &str = "auto-";

/// Who took a snapshot: a person pressing the button, or the maintenance loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupKind {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub kind: BackupKind,
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
    /// `VACUUM` succeeded but the compacted file did not pass the checks afterwards. The safety
    /// copy taken before it is the way back, so it is kept and named.
    CompactionUnverified {
        reason: String,
        safety_backup: PathBuf,
    },
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
            Self::CompactionUnverified {
                reason,
                safety_backup,
            } => write!(
                formatter,
                "compacted file failed verification ({reason}); safety copy kept at {}",
                safety_backup.display()
            ),
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
    let (info, _) = snapshot(pool, &db_path, BackupKind::Manual).await?;
    prune_backups(&db_path, retention, &info.name, BackupKind::Manual)?;
    Ok(info)
}

/// The same snapshot as [`take_backup`], named `auto-nucleos-...` and pruned against its own
/// retention: only `auto-` files are ever removed here, and a manual backup never counts toward
/// (or is removed by) the automatic ones.
pub async fn take_auto_backup(
    pool: &SqlitePool,
    retention: usize,
) -> Result<BackupInfo, BackupError> {
    if retention == 0 {
        return Err(BackupError::InvalidRetention);
    }
    let db_path = database_path(pool).await?;
    let (info, _) = snapshot(pool, &db_path, BackupKind::Auto).await?;
    prune_backups(&db_path, retention, &info.name, BackupKind::Auto)?;
    Ok(info)
}

/// Whether the newest automatic copy (taken at `last`, `None` when there is none) is old enough
/// that another is due. A `last` in the future (a clock that went backwards) is not due.
pub fn auto_backup_due(last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match last {
        None => true,
        Some(last) => now
            .signed_duration_since(last)
            .to_std()
            .is_ok_and(|age| age >= AUTO_INTERVAL),
    }
}

/// When the newest automatic copy was taken, read from its file name; `None` when there is none.
pub async fn newest_auto_backup(pool: &SqlitePool) -> Result<Option<DateTime<Utc>>, BackupError> {
    let db_path = database_path(pool).await?;
    newest_auto_backup_at(&db_path)
}

fn newest_auto_backup_at(db_path: &Path) -> Result<Option<DateTime<Utc>>, BackupError> {
    Ok(list_backup_files(db_path)?
        .into_iter()
        .filter(|backup| backup.kind == BackupKind::Auto)
        .filter_map(|backup| split_name(&backup.name).ok().map(|parts| parts.timestamp))
        .max()
        .map(|taken| taken.and_utc()))
}

/// Takes an automatic copy when [`auto_backup_due`] says so, and returns it; `None` means the
/// newest one is still fresh. The age comes from the file name, so it survives a daemon restart.
pub async fn take_auto_backup_if_due(
    pool: &SqlitePool,
    retention: usize,
    now: DateTime<Utc>,
) -> Result<Option<BackupInfo>, BackupError> {
    let db_path = database_path(pool).await?;
    let last = newest_auto_backup_at(&db_path)?;
    if !auto_backup_due(last, now) {
        return Ok(None);
    }
    take_auto_backup(pool, retention).await.map(Some)
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
            snapshot(&live_pool, db_path, BackupKind::Manual).await
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

/// What a compaction did, for the startup log.
#[derive(Debug, Clone)]
pub struct Compacted {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub safety_backup: PathBuf,
}

/// How many times the database (plus its WAL) must fit in the free space before a compaction is
/// tried. The database is in WAL mode, so `VACUUM` writes the rebuilt database through the `-wal`
/// file (up to about one more database) and builds it in a temp database in SQLite's temp
/// directory. The factor covers, on the database's own volume, the database itself, the
/// `pre-compact` copy and that WAL. The owner's 440 MB database wants ~1.3 GB free, which is what
/// keeps a full disk from being the thing a "reclaim space" button causes.
const COMPACTION_ROOM_FACTOR: u64 = 3;

/// Safety copies taken by [`apply_pending_compaction`]. The name deliberately does not parse as a
/// backup name (`split_name` wants `nucleos-`): the copy is neither listed, restorable by the
/// Backups tab, nor reached by either retention. Only the newest one is kept.
const PRE_COMPACT_PREFIX: &str = "pre-compact-";

/// Schedules a compaction for the next start. Idempotent, and refused while a restore is pending:
/// the restore replaces the file, so compacting around it would be compacting the wrong database.
pub async fn stage_compaction(pool: &SqlitePool) -> Result<(), BackupError> {
    let db_path = database_path(pool).await?;
    if pending_marker_path(&db_path).exists() {
        return Err(BackupError::PendingRestoreExists);
    }
    fs::write(compact_marker_path(&db_path), b"")?;
    Ok(())
}

/// Whether a compaction is scheduled for the next start.
pub fn compaction_pending(db_path: &Path) -> bool {
    compact_marker_path(db_path).exists()
}

/// Compacts the database file when a compaction was scheduled, before the pool opens. The caller
/// runs it AFTER [`apply_pending_restore`] and BEFORE `storage::open`, on the primary daemon only.
///
/// `available_space` answers how many bytes the volume holding the database directory has free;
/// it is a parameter so the test can say "no room" without filling a disk.
///
/// A failure before `VACUUM` has finished leaves the marker in place (so the next start retries),
/// the database untouched (`VACUUM` is atomic) and no safety copy of this run behind, so failed
/// retries never pile up full-size copies. Once `VACUUM` has succeeded the marker is always
/// removed; if the compacted file then fails its checks the error is
/// [`BackupError::CompactionUnverified`], this run's safety copy is kept and the older ones are not
/// pruned. Not having room is not a failure: it forgets the request, because retrying a disk that
/// is still full on every start would only make every start slow.
pub async fn apply_pending_compaction(
    db_path: &Path,
    available_space: impl FnOnce(&Path) -> std::io::Result<u64>,
) -> Result<Option<Compacted>, BackupError> {
    apply_compaction(db_path, available_space, "VACUUM").await
}

/// [`apply_pending_compaction`] with the statement that compacts the file as a parameter, so a test
/// can make the compaction fail, or leave a file that fails its checks, without a second process.
async fn apply_compaction(
    db_path: &Path,
    available_space: impl FnOnce(&Path) -> std::io::Result<u64>,
    vacuum: &str,
) -> Result<Option<Compacted>, BackupError> {
    let marker = compact_marker_path(db_path);
    if !marker.exists() {
        return Ok(None);
    }
    if pending_marker_path(db_path).exists() {
        return Err(BackupError::PendingRestoreExists);
    }
    if !db_path.exists() {
        // Nothing to compact; `storage::open` will create a fresh one.
        remove_if_exists(&marker)?;
        return Ok(None);
    }
    let directory = db_path.parent().ok_or_else(|| {
        BackupError::Verification("database path has no parent directory".to_owned())
    })?;

    let size = file_len(db_path) + file_len(&sqlite_sidecars(db_path)[0]);
    let needed = size.saturating_mul(COMPACTION_ROOM_FACTOR);
    let free = available_space(directory)?;
    if free < needed {
        tracing::warn!(
            free_bytes = free,
            needed_bytes = needed,
            "database compaction skipped for lack of disk space; the request is forgotten"
        );
        remove_if_exists(&marker)?;
        return Ok(None);
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(db_path)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(10)),
        )
        .await?;
    let result = compact_database(&pool, db_path, vacuum).await;
    // Closing the last connection folds the WAL away, so the sizes below are the whole story.
    pool.close().await;
    let (before_bytes, safety_backup) = result?;

    // `VACUUM` has run: the request is answered whatever the checks below say, because retrying a
    // compaction of a file that is already compact (or that failed its checks) helps nobody.
    let marker_removed = remove_if_exists(&marker);
    if let Err(reason) = verify_compacted(db_path, &safety_backup).await {
        if let Err(error) = &marker_removed {
            tracing::warn!(%error, "could not remove the compaction request");
        }
        return Err(BackupError::CompactionUnverified {
            reason: reason.to_string(),
            safety_backup,
        });
    }
    let after_bytes = file_len(db_path);

    // Best effort from here: the compaction is done and verified, only housekeeping is left. Older
    // safety copies go only now, once this run's copy has proved to be the one to keep.
    if let Err(error) = prune_pre_compact_copies(directory, &safety_backup) {
        tracing::warn!(%error, "could not remove older pre-compact copies");
    }
    // If the marker cannot be removed the next start compacts an already compact file, which is
    // slow but harmless.
    marker_removed?;
    Ok(Some(Compacted {
        before_bytes,
        after_bytes,
        safety_backup,
    }))
}

/// The three steps on one connection: fold the WAL in, copy to a verified safety file, `VACUUM`.
/// Returns the size of the file before `VACUUM` and where the safety copy is.
async fn compact_database(
    pool: &SqlitePool,
    db_path: &Path,
    vacuum: &str,
) -> Result<(u64, PathBuf), BackupError> {
    let (busy, _, _): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await?;
    if busy != 0 {
        return Err(BackupError::Verification(
            "the database is in use by another process".to_owned(),
        ));
    }
    let before_bytes = file_len(db_path);

    let directory = backup_dir(db_path)?;
    fs::create_dir_all(&directory)?;
    let safety_path = directory.join(format!(
        "{PRE_COMPACT_PREFIX}{}.db",
        Utc::now().format("%Y%m%dT%H%M%S%.9fZ")
    ));
    if safety_path.exists() {
        return Err(BackupError::ExistingTarget(safety_path));
    }
    if let Err(error) = sqlx::query("VACUUM INTO ?")
        .bind(safety_path.to_string_lossy().into_owned())
        .execute(pool)
        .await
    {
        let _ = remove_if_exists(&safety_path);
        return Err(BackupError::Database(error));
    }
    if let Err(error) = verify_backup(pool, &safety_path).await {
        let _ = remove_if_exists(&safety_path);
        return Err(error);
    }

    // `VACUUM` is atomic, so a failure leaves the database as it was and this run's copy is only
    // dead weight: remove it, or every failed retry would leave another full-size file behind.
    if let Err(error) = sqlx::query(vacuum).execute(pool).await {
        let _ = remove_if_exists(&safety_path);
        return Err(BackupError::Database(error));
    }
    Ok((before_bytes, safety_path))
}

/// The compacted file passes `quick_check` and carries the migration version of the safety copy
/// taken just before it.
async fn verify_compacted(db_path: &Path, safety_backup: &Path) -> Result<(), BackupError> {
    let version = verified_version(db_path).await?;
    let safety_version = verified_version(safety_backup).await?;
    if version != safety_version {
        return Err(BackupError::Verification(format!(
            "compacted database has migration version {version}, its safety copy {safety_version}"
        )));
    }
    Ok(())
}

/// Removes every `pre-compact-*.db` in `backups/` except `keep`.
fn prune_pre_compact_copies(db_directory: &Path, keep: &Path) -> Result<(), BackupError> {
    let directory = db_directory.join("backups");
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path != keep
            && entry.file_type()?.is_file()
            && name.starts_with(PRE_COMPACT_PREFIX)
            && name.ends_with(".db")
        {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |meta| meta.len())
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

/// `nucleos.db.compact-pending`: appended to the whole file name, so it cannot collide with the
/// `with_extension` markers above, and `storage::loose_files_bytes` counts it (an empty file).
fn compact_marker_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(".compact-pending");
    PathBuf::from(name)
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

/// The parts of a backup file name: `[auto-]nucleos-<timestamp>-<sequence>.db`.
struct NameParts<'a> {
    kind: BackupKind,
    timestamp: NaiveDateTime,
    /// `<timestamp>-<sequence>`: fixed width, so it orders chronologically whatever the kind.
    stem: &'a str,
}

fn split_name(name: &str) -> Result<NameParts<'_>, BackupError> {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(BackupError::InvalidName);
    }
    let (kind, rest) = match name.strip_prefix(AUTO_PREFIX) {
        Some(rest) => (BackupKind::Auto, rest),
        None => (BackupKind::Manual, name),
    };
    let stem = rest
        .strip_prefix("nucleos-")
        .and_then(|value| value.strip_suffix(".db"))
        .ok_or(BackupError::InvalidName)?;
    let (timestamp, sequence) = stem.rsplit_once('-').ok_or(BackupError::InvalidName)?;
    if sequence.len() != 4 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(BackupError::InvalidName);
    }
    let timestamp = NaiveDateTime::parse_from_str(timestamp, "%Y%m%dT%H%M%S%.9fZ")
        .map_err(|_| BackupError::InvalidName)?;
    Ok(NameParts {
        kind,
        timestamp,
        stem,
    })
}

fn validate_name(name: &str) -> Result<(), BackupError> {
    split_name(name).map(|_| ())
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

fn next_backup_path(db_path: &Path, kind: BackupKind) -> Result<(String, PathBuf), BackupError> {
    let directory = backup_dir(db_path)?;
    fs::create_dir_all(&directory)?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    let prefix = match kind {
        BackupKind::Manual => "",
        BackupKind::Auto => AUTO_PREFIX,
    };
    for sequence in 0..=9999 {
        let name = format!("{prefix}nucleos-{timestamp}-{sequence:04}.db");
        let path = directory.join(&name);
        if !path.exists() {
            return Ok((name, path));
        }
    }
    Err(BackupError::ExistingTarget(directory))
}

async fn snapshot(
    pool: &SqlitePool,
    db_path: &Path,
    kind: BackupKind,
) -> Result<(BackupInfo, PathBuf), BackupError> {
    let (name, path) = next_backup_path(db_path, kind)?;
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
            kind,
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
        let Ok(parts) = split_name(&name) else {
            continue;
        };
        let kind = parts.kind;
        let stem = parts.stem.to_owned();
        backups.push((
            stem,
            BackupInfo {
                name,
                kind,
                migration_version: None,
                size_bytes: entry.metadata()?.len(),
            },
        ));
    }
    // Newest first by the timestamp inside the name, not the whole name: the `auto-` prefix would
    // otherwise sort every automatic copy ahead of every manual one.
    backups.sort_by(|left, right| right.0.cmp(&left.0));
    Ok(backups.into_iter().map(|(_, info)| info).collect())
}

/// Prunes one kind only: `retention` counts the copies of `kind`, and the other kind is neither
/// counted nor touched.
fn prune_backups(
    db_path: &Path,
    retention: usize,
    just_taken: &str,
    kind: BackupKind,
) -> Result<(), BackupError> {
    if retention == 0 {
        return Err(BackupError::InvalidRetention);
    }
    let backups: Vec<BackupInfo> = list_backup_files(db_path)?
        .into_iter()
        .filter(|backup| backup.kind == kind)
        .collect();
    let mut keep = HashSet::from([just_taken.to_owned()]);

    // A restore staged before another backup request must not have its source pruned away.
    let marker = pending_marker_path(db_path);
    if marker.exists()
        && let Ok(source) = fs::read_to_string(&marker)
        && let Some(name) = Path::new(&source)
            .file_name()
            .and_then(|name| name.to_str())
        && split_name(name).is_ok_and(|parts| parts.kind == kind)
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

    async fn listed_names(pool: &SqlitePool) -> Vec<String> {
        list_backups(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|item| item.name)
            .collect()
    }

    #[tokio::test]
    async fn an_auto_backup_prunes_only_auto_backups() {
        let db = TempDb::new().await;
        let manual_first = take_backup(&db.pool, 10).await.unwrap();
        let manual_second = take_backup(&db.pool, 10).await.unwrap();

        let auto_first = take_auto_backup(&db.pool, 2).await.unwrap();
        let auto_second = take_auto_backup(&db.pool, 2).await.unwrap();
        let auto_newest = take_auto_backup(&db.pool, 2).await.unwrap();
        let names = listed_names(&db.pool).await;

        assert!(
            !names.contains(&auto_first.name),
            "the oldest automatic copy is pruned"
        );
        assert!(names.contains(&auto_second.name));
        assert!(names.contains(&auto_newest.name));
        assert!(
            names.contains(&manual_first.name) && names.contains(&manual_second.name),
            "an automatic copy never removes a manual one"
        );

        // And the other way round: a manual copy with a tiny retention leaves the automatic ones.
        let manual_newest = take_backup(&db.pool, 1).await.unwrap();
        let names = listed_names(&db.pool).await;
        assert!(names.contains(&manual_newest.name));
        assert!(!names.contains(&manual_second.name));
        assert!(names.contains(&auto_second.name) && names.contains(&auto_newest.name));
        assert!(take_auto_backup(&db.pool, 0).await.is_err());
        db.close().await;
    }

    #[tokio::test]
    async fn list_backups_says_which_are_auto() {
        let db = TempDb::new().await;
        let manual = take_backup(&db.pool, 5).await.unwrap();
        let auto = take_auto_backup(&db.pool, 5).await.unwrap();
        assert!(auto.name.starts_with("auto-nucleos-"));
        assert!(manual.name.starts_with("nucleos-"));

        let listed = list_backups(&db.pool).await.unwrap();
        let kind_of = |name: &str| listed.iter().find(|item| item.name == name).unwrap().kind;
        assert_eq!(kind_of(&manual.name), BackupKind::Manual);
        assert_eq!(kind_of(&auto.name), BackupKind::Auto);
        assert_eq!(
            listed[0].name, auto.name,
            "newest first by the timestamp, whatever the prefix"
        );
        assert_eq!(serde_json::to_value(&listed[0]).unwrap()["kind"], "auto");
        assert_eq!(serde_json::to_value(&listed[1]).unwrap()["kind"], "manual");
        db.close().await;
    }

    #[test]
    fn an_auto_backup_is_due_only_after_a_week() {
        let now = Utc::now();
        let days = chrono::Duration::days;
        assert!(auto_backup_due(None, now), "no automatic copy yet");
        assert!(!auto_backup_due(Some(now - days(1)), now));
        assert!(!auto_backup_due(Some(now - days(6)), now));
        assert!(auto_backup_due(Some(now - days(7)), now));
        assert!(auto_backup_due(Some(now - days(30)), now));
        assert!(
            !auto_backup_due(Some(now + days(1)), now),
            "a clock that went backwards does not trigger a copy"
        );
    }

    #[tokio::test]
    async fn an_auto_backup_is_taken_once_per_interval() {
        let db = TempDb::new().await;
        let now = Utc::now();

        let first = take_auto_backup_if_due(&db.pool, AUTO_RETENTION, now)
            .await
            .unwrap();
        assert!(first.is_some(), "the first one is due at once");
        let again = take_auto_backup_if_due(&db.pool, AUTO_RETENTION, now)
            .await
            .unwrap();
        assert!(again.is_none(), "a fresh automatic copy is not repeated");
        let next =
            take_auto_backup_if_due(&db.pool, AUTO_RETENTION, now + chrono::Duration::days(8))
                .await
                .unwrap();
        assert!(next.is_some(), "a week later the next one is due");
        db.close().await;
    }

    #[tokio::test]
    async fn a_manual_backup_does_not_postpone_the_automatic_one() {
        let db = TempDb::new().await;
        take_backup(&db.pool, 5).await.unwrap();
        let taken = take_auto_backup_if_due(&db.pool, AUTO_RETENTION, Utc::now())
            .await
            .unwrap();
        assert!(taken.is_some());
        db.close().await;
    }

    #[tokio::test]
    async fn an_auto_backup_can_be_staged_and_restored() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        create_probe(&db.pool, "kept").await;
        let auto = take_auto_backup(&db.pool, 2).await.unwrap();

        let staged = stage_restore(&db.pool, &auto.name).await.unwrap();
        assert_eq!(staged.name, auto.name);
        create_probe(&db.pool, "lost").await;
        db.pool.close().await;

        let applied = apply_pending_restore(&db_path)
            .await
            .unwrap()
            .expect("a restore was pending");
        assert_eq!(applied.restored_from, auto.name);
        assert_eq!(probe_values(&db_path).await, vec!["kept"]);
    }

    /// A scratch table with `kept` rows left and many more deleted, so the file holds free pages a
    /// `VACUUM` can give back. Returns the rows that remain.
    async fn seed_free_pages(pool: &SqlitePool, kept: i64) -> Vec<(i64, String)> {
        sqlx::query("CREATE TABLE scratch (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c WHERE i < 400)
             INSERT INTO scratch SELECT i, hex(randomblob(2048)) FROM c",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM scratch WHERE id > ?")
            .bind(kept)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query_as("SELECT id, payload FROM scratch ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn scratch_rows(path: &Path) -> Vec<(i64, String)> {
        let pool = open_read_only(path).await;
        let rows = sqlx::query_as("SELECT id, payload FROM scratch ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        pool.close().await;
        rows
    }

    fn pre_compact_files(db_path: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(db_path.parent().unwrap().join("backups")) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(PRE_COMPACT_PREFIX))
            })
            .collect()
    }

    #[tokio::test]
    async fn a_pending_compaction_shrinks_the_file_and_keeps_every_row() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        let rows = seed_free_pages(&db.pool, 40).await;
        assert_eq!(rows.len(), 40);
        stage_compaction(&db.pool).await.unwrap();
        assert!(compaction_pending(&db_path));
        db.pool.close().await;

        let compacted = apply_pending_compaction(&db_path, |_| Ok(u64::MAX))
            .await
            .unwrap()
            .expect("a compaction was pending");

        assert!(
            compacted.after_bytes < compacted.before_bytes,
            "{compacted:?}: the deleted rows' pages are given back"
        );
        assert_eq!(compacted.after_bytes, file_len(&db_path));
        assert!(!compaction_pending(&db_path), "the marker is consumed");
        assert_eq!(scratch_rows(&db_path).await, rows, "every row survives");
        assert_eq!(
            scratch_rows(&compacted.safety_backup).await,
            rows,
            "the safety copy holds the same rows"
        );
        assert_eq!(pre_compact_files(&db_path), vec![compacted.safety_backup]);
        assert!(
            list_backup_files(&db_path).unwrap().is_empty(),
            "a pre-compact copy is not a listed backup, so no retention can reach it"
        );
        assert!(
            apply_pending_compaction(&db_path, |_| Ok(u64::MAX))
                .await
                .unwrap()
                .is_none(),
            "with no marker there is nothing to do"
        );
    }

    #[tokio::test]
    async fn a_second_compaction_keeps_only_the_newest_safety_copy() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        seed_free_pages(&db.pool, 40).await;
        db.pool.close().await;
        for _ in 0..2 {
            fs::write(compact_marker_path(&db_path), b"").unwrap();
            apply_pending_compaction(&db_path, |_| Ok(u64::MAX))
                .await
                .unwrap()
                .expect("a compaction was pending");
        }
        assert_eq!(pre_compact_files(&db_path).len(), 1);
    }

    #[tokio::test]
    async fn a_compaction_without_room_on_disk_is_skipped_and_forgotten() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        let rows = seed_free_pages(&db.pool, 40).await;
        stage_compaction(&db.pool).await.unwrap();
        db.pool.close().await;
        let size_before = file_len(&db_path);

        let result = apply_pending_compaction(&db_path, |_| Ok(size_before))
            .await
            .unwrap();

        assert!(result.is_none(), "no room, nothing done");
        assert!(
            !compaction_pending(&db_path),
            "and the request is forgotten"
        );
        assert_eq!(file_len(&db_path), size_before, "the file is untouched");
        assert!(pre_compact_files(&db_path).is_empty());
        assert_eq!(scratch_rows(&db_path).await, rows);
    }

    #[tokio::test]
    async fn a_failing_space_probe_keeps_the_request_and_the_file() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        stage_compaction(&db.pool).await.unwrap();
        db.pool.close().await;

        let result = apply_pending_compaction(&db_path, |_| {
            Err(std::io::Error::other("the probe is unavailable"))
        })
        .await;

        assert!(result.is_err());
        assert!(compaction_pending(&db_path), "the next start retries");
    }

    /// An older safety copy, as a previous compaction would have left it.
    fn plant_old_pre_compact_copy(db_path: &Path) -> PathBuf {
        let directory = db_path.parent().unwrap().join("backups");
        fs::create_dir_all(&directory).unwrap();
        let old = directory.join(format!("{PRE_COMPACT_PREFIX}19700101T000000.000000000Z.db"));
        fs::write(&old, b"old").unwrap();
        old
    }

    #[tokio::test]
    async fn a_failed_vacuum_removes_its_own_safety_copy_and_keeps_the_request() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        let rows = seed_free_pages(&db.pool, 40).await;
        let old = plant_old_pre_compact_copy(&db_path);
        stage_compaction(&db.pool).await.unwrap();
        db.pool.close().await;

        // `VACUUM` of a schema that does not exist is an error, after the safety copy was taken.
        let result = apply_compaction(&db_path, |_| Ok(u64::MAX), "VACUUM nosuchschema").await;

        assert!(
            matches!(result, Err(BackupError::Database(_))),
            "{result:?}"
        );
        assert!(compaction_pending(&db_path), "the next start retries");
        assert_eq!(
            pre_compact_files(&db_path),
            vec![old],
            "this run's copy is gone, the older one is not touched"
        );
        assert_eq!(scratch_rows(&db_path).await, rows);
    }

    #[tokio::test]
    async fn a_compacted_file_that_fails_verification_keeps_its_copies_and_loses_the_request() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        seed_free_pages(&db.pool, 40).await;
        let old = plant_old_pre_compact_copy(&db_path);
        stage_compaction(&db.pool).await.unwrap();
        db.pool.close().await;

        // Compacts, then drops the newest migration row: the file no longer matches its copy.
        let result = apply_compaction(
            &db_path,
            |_| Ok(u64::MAX),
            "VACUUM; DELETE FROM _sqlx_migrations \
             WHERE version = (SELECT MAX(version) FROM _sqlx_migrations)",
        )
        .await;

        let Err(BackupError::CompactionUnverified { safety_backup, .. }) = &result else {
            panic!("expected CompactionUnverified, got {result:?}");
        };
        assert!(
            result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains(&safety_backup.display().to_string()),
            "the message names the copy"
        );
        assert!(safety_backup.exists());
        assert!(old.exists(), "older copies are not pruned after a bad run");
        assert!(
            !compaction_pending(&db_path),
            "VACUUM ran, the request is answered"
        );
    }

    #[tokio::test]
    async fn a_compaction_is_refused_while_a_restore_is_pending() {
        let db = TempDb::new().await;
        let db_path = db.path().join("nucleos.db");
        let backup = take_backup(&db.pool, 5).await.unwrap();
        stage_restore(&db.pool, &backup.name).await.unwrap();

        assert!(matches!(
            stage_compaction(&db.pool).await,
            Err(BackupError::PendingRestoreExists)
        ));
        assert!(!compaction_pending(&db_path), "no marker was written");

        // A marker that got there anyway is not honoured until the restore has been applied.
        fs::write(compact_marker_path(&db_path), b"").unwrap();
        db.pool.close().await;
        assert!(matches!(
            apply_pending_compaction(&db_path, |_| Ok(u64::MAX)).await,
            Err(BackupError::PendingRestoreExists)
        ));
        assert!(compaction_pending(&db_path));
        assert!(pre_compact_files(&db_path).is_empty());
    }

    #[test]
    fn the_auto_prefix_does_not_loosen_name_validation() {
        let ok = "auto-nucleos-20260729T010203.000000000Z-0000.db";
        assert!(validate_name(ok).is_ok());
        for bad in [
            "auto-auto-nucleos-20260729T010203.000000000Z-0000.db",
            "auto-nucleos-20260729T010203.000000000Z-0000.txt",
            "auto-nucleos-20260729T010203.000000000Z-00.db",
            "auto-../nucleos-20260729T010203.000000000Z-0000.db",
            "auto-nucleos-notatime-0000.db",
        ] {
            assert!(validate_name(bad).is_err(), "{bad} must be refused");
        }
    }
}
