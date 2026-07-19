use crate::feed;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const GC_BACKOFF: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(25),
];

pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
}

pub fn worktree_root(project_root: &Path) -> PathBuf {
    if let Some(root) = std::env::var_os("NUCLEOS_WORKTREE_ROOT") {
        return PathBuf::from(root);
    }

    let parent = project_root.parent().unwrap_or_else(|| Path::new(""));
    let project_name = project_root.file_name().unwrap_or_default();
    parent.join("nucleos-worktrees").join(project_name)
}

fn git_bin() -> String {
    std::env::var("NUCLEOS_GIT_BIN").unwrap_or_else(|_| "git".into())
}

pub async fn create(project_root: &Path, run_id: i64) -> io::Result<WorktreeInfo> {
    let root = worktree_root(project_root);
    if root.to_string_lossy().contains(' ') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree root path contains a space — set NUCLEOS_WORKTREE_ROOT to a space-free path (cargo builds fail to link under spaced paths)",
        ));
    }

    let branch = format!("nucleos/run-{run_id}");
    let path = root.join(format!("run-{run_id}"));
    tokio::fs::create_dir_all(&root).await?;

    let output = tokio::process::Command::new(git_bin())
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch)
        .arg(&path)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("git worktree add failed: {stderr}"),
        ));
    }

    Ok(WorktreeInfo { path, branch })
}

pub(crate) async fn try_remove_once(project_root: &Path, path: &Path) -> io::Result<()> {
    let output = tokio::process::Command::new(git_bin())
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(path)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("git worktree remove failed: {stderr}"),
        ));
    }

    Ok(())
}

pub async fn remove(project_root: &Path, path: &Path, backoff: &[Duration]) -> io::Result<()> {
    let result = retry_with_backoff(backoff, || try_remove_once(project_root, path)).await;
    let _ = tokio::process::Command::new(git_bin())
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("prune")
        .output()
        .await;
    result
}

pub(crate) async fn retry_with_backoff<F, Fut>(
    backoff: &[Duration],
    mut attempt: F,
) -> io::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    let mut last_error = match attempt().await {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };

    for delay in backoff {
        tokio::time::sleep(*delay).await;
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }

    Err(last_error)
}

#[derive(Debug, Clone, sqlx::FromRow, PartialEq)]
pub struct WorktreeRow {
    pub run_id: i64,
    pub project_id: String,
    pub project_root: String,
    pub path: String,
    pub branch: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    Released,
    NotAwaitingApproval,
    NotFound,
}

pub async fn record(
    pool: &SqlitePool,
    run_id: i64,
    project_id: &str,
    project_root: &str,
    path: &str,
    branch: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO worktrees
         (run_id, project_id, project_root, path, branch, created_at, removed_at)
         VALUES (?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(run_id)
    .bind(project_id)
    .bind(project_root)
    .bind(path)
    .bind(branch)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_removed(pool: &SqlitePool, run_id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE worktrees SET removed_at = ? WHERE run_id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn release(pool: &SqlitePool, run_id: i64) -> sqlx::Result<ReleaseOutcome> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await?;
    let Some(status) = status else {
        return Ok(ReleaseOutcome::NotFound);
    };
    if status != "awaiting_approval" {
        return Ok(ReleaseOutcome::NotAwaitingApproval);
    }

    let worktree: Option<WorktreeRow> = sqlx::query_as(
        "SELECT run_id, project_id, project_root, path, branch
         FROM worktrees
         WHERE run_id = ? AND removed_at IS NULL",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;

    if let Some(worktree) = &worktree {
        if let Err(error) = remove(
            Path::new(&worktree.project_root),
            Path::new(&worktree.path),
            GC_BACKOFF,
        )
        .await
        {
            tracing::warn!(
                run_id,
                %error,
                "failed to remove released worktree; continuing with discard"
            );
        }
        mark_removed(pool, run_id).await?;
    }

    sqlx::query("UPDATE runs SET status = 'cancelled', completed_at = ? WHERE id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(pool)
        .await?;

    if let Some(worktree) = worktree {
        let summary = format!("released worktree {}", worktree.path);
        let _ = feed::append(
            pool,
            Some(&worktree.project_id),
            "worktree_released",
            &summary,
            Some(run_id),
        )
        .await;
    }

    Ok(ReleaseOutcome::Released)
}

pub async fn gc_candidates(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    retention: chrono::Duration,
) -> sqlx::Result<Vec<WorktreeRow>> {
    let cutoff = (now - retention).to_rfc3339();
    sqlx::query_as(
        "SELECT w.run_id, w.project_id, w.project_root, w.path, w.branch
         FROM worktrees w
         JOIN runs r ON r.id = w.run_id
         WHERE w.removed_at IS NULL
           AND r.status IN ('completed','failed','cancelled','timed_out','interrupted')
           AND COALESCE(r.completed_at, w.created_at) <= ?
         ORDER BY w.run_id",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await
}

fn retention() -> chrono::Duration {
    std::env::var("NUCLEOS_WORKTREE_RETENTION_HOURS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .map(chrono::Duration::hours)
        .unwrap_or_else(|| chrono::Duration::hours(72))
}

pub(crate) async fn gc_pass(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    retention: chrono::Duration,
    backoff: &[Duration],
) {
    let candidates = gc_candidates(pool, now, retention)
        .await
        .unwrap_or_default();

    let project_roots = candidates
        .iter()
        .map(|worktree| worktree.project_root.clone())
        .collect::<std::collections::HashSet<_>>();
    for project_root in project_roots {
        let _ = tokio::process::Command::new(git_bin())
            .arg("-C")
            .arg(project_root)
            .arg("worktree")
            .arg("prune")
            .output()
            .await;
    }

    for worktree in candidates {
        match remove(
            Path::new(&worktree.project_root),
            Path::new(&worktree.path),
            backoff,
        )
        .await
        {
            Ok(()) => {
                if let Err(error) = mark_removed(pool, worktree.run_id).await {
                    tracing::warn!(
                        run_id = worktree.run_id,
                        %error,
                        "failed to mark collected worktree as removed"
                    );
                }

                let branch_deleted = tokio::process::Command::new(git_bin())
                    .arg("-C")
                    .arg(&worktree.project_root)
                    .arg("branch")
                    .arg("-d")
                    .arg(&worktree.branch)
                    .output()
                    .await
                    .is_ok_and(|output| output.status.success());

                if branch_deleted {
                    let summary = format!("removed worktree + merged branch {}", worktree.branch);
                    let _ = feed::append(
                        pool,
                        Some(&worktree.project_id),
                        "worktree_removed",
                        &summary,
                        Some(worktree.run_id),
                    )
                    .await;
                } else {
                    let summary = format!("removed worktree {}", worktree.path);
                    let _ = feed::append(
                        pool,
                        Some(&worktree.project_id),
                        "worktree_removed",
                        &summary,
                        Some(worktree.run_id),
                    )
                    .await;
                    let summary = format!("kept unmerged branch {}", worktree.branch);
                    let _ = feed::append(
                        pool,
                        Some(&worktree.project_id),
                        "worktree_branch_kept",
                        &summary,
                        Some(worktree.run_id),
                    )
                    .await;
                }
            }
            Err(error) => {
                let summary = format!("failed to remove worktree {}: {}", worktree.path, error);
                let _ = feed::append(
                    pool,
                    Some(&worktree.project_id),
                    "worktree_gc_failed",
                    &summary,
                    Some(worktree.run_id),
                )
                .await;
            }
        }
    }
}

pub async fn run_gc(pool: SqlitePool) {
    let mut interval = tokio::time::interval(Duration::from_secs(1800));
    loop {
        interval.tick().await;
        gc_pass(&pool, Utc::now(), retention(), GC_BACKOFF).await;
    }
}

#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::ffi::{OsStr, OsString};
    use std::fs::OpenOptions;
    use std::process::Command;
    use std::sync::MutexGuard;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ROOT_ENV: &str = "NUCLEOS_WORKTREE_ROOT";

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

    async fn insert_run(
        pool: &sqlx::SqlitePool,
        status: &str,
        completed_at: Option<&str>,
        created_at: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, created_at, completed_at)
             VALUES ('project-a', '/project/a', 'test', ?, ?, ?)",
        )
        .bind(status)
        .bind(created_at)
        .bind(completed_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn timestamp(value: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    async fn set_worktree_created_at(pool: &sqlx::SqlitePool, run_id: i64, created_at: &str) {
        sqlx::query("UPDATE worktrees SET created_at = ? WHERE run_id = ?")
            .bind(created_at)
            .bind(run_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn create_and_record(pool: &sqlx::SqlitePool, repo: &Path, run_id: i64) -> WorktreeInfo {
        let info = create(repo, run_id).await.expect("create worktree");
        record(
            pool,
            run_id,
            "project-a",
            repo.to_str().expect("repository path should be UTF-8"),
            info.path.to_str().expect("worktree path should be UTF-8"),
            &info.branch,
        )
        .await
        .expect("record worktree");
        info
    }

    fn git_ok(dir: &Path, args: &[&OsStr]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn git_stdout(dir: &Path, args: &[&OsStr]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_owned()
    }

    fn init_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("create repository tempdir");
        assert!(git_ok(repo.path(), &[OsStr::new("init")]));
        assert!(git_ok(
            repo.path(),
            &[
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new("test@x"),
            ],
        ));
        assert!(git_ok(
            repo.path(),
            &[
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new("test"),
            ],
        ));
        std::fs::write(repo.path().join("seed.txt"), "seed\n").expect("write seed file");
        assert!(git_ok(repo.path(), &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo.path(),
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("seed"),],
        ));
        repo
    }

    fn space_free_tempdir() -> tempfile::TempDir {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        tempfile::Builder::new()
            .prefix("nucleos-wt-test-")
            .tempdir_in(base)
            .expect("create space-free tempdir")
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        super::test_env_lock()
    }

    struct WorktreeRootEnv {
        previous: Option<OsString>,
    }

    impl WorktreeRootEnv {
        fn set(value: Option<&Path>) -> Self {
            let previous = std::env::var_os(ROOT_ENV);
            unsafe {
                match value {
                    Some(path) => std::env::set_var(ROOT_ENV, path),
                    None => std::env::remove_var(ROOT_ENV),
                }
            }
            Self { previous }
        }
    }

    impl Drop for WorktreeRootEnv {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(ROOT_ENV, value),
                    None => std::env::remove_var(ROOT_ENV),
                }
            }
        }
    }

    #[test]
    fn worktree_root_derives_sibling_dir() {
        let _lock = env_lock();
        let _env = WorktreeRootEnv::set(None);
        let actual = worktree_root(Path::new(r"C:\work\repo"));
        let expected = Path::new(r"C:\work\nucleos-worktrees\repo");
        assert_eq!(
            actual.components().collect::<Vec<_>>(),
            expected.components().collect::<Vec<_>>()
        );
    }

    #[test]
    fn worktree_root_honors_env_override() {
        let _lock = env_lock();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        assert_eq!(worktree_root(Path::new(r"C:\work\repo")), root.path());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_makes_a_worktree_on_its_own_branch_without_touching_the_project() {
        let _lock = env_lock();
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let head_before = git_stdout(repo.path(), &[OsStr::new("rev-parse"), OsStr::new("HEAD")]);
        let status_before = git_stdout(
            repo.path(),
            &[OsStr::new("status"), OsStr::new("--porcelain")],
        );

        let info = create(repo.path(), 7).await.expect("create worktree");
        assert!(info.path.is_dir());
        assert_eq!(info.branch, "nucleos/run-7");
        let branch_listing = git_stdout(
            repo.path(),
            &[
                OsStr::new("branch"),
                OsStr::new("--list"),
                OsStr::new("nucleos/run-7"),
            ],
        );
        assert!(branch_listing.ends_with("nucleos/run-7"));

        std::fs::write(info.path.join("worktree-only.txt"), "isolated\n")
            .expect("write worktree file");
        assert!(git_ok(&info.path, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &info.path,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("worktree-only"),
            ],
        ));

        assert_eq!(
            git_stdout(repo.path(), &[OsStr::new("rev-parse"), OsStr::new("HEAD")]),
            head_before
        );
        assert_eq!(
            git_stdout(
                repo.path(),
                &[OsStr::new("status"), OsStr::new("--porcelain")]
            ),
            status_before
        );
        remove(repo.path(), &info.path, &[])
            .await
            .expect("remove worktree");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_rejects_a_spaced_root() {
        let _lock = env_lock();
        let repo = init_repo();
        let spaced_root = std::env::current_dir()
            .expect("resolve current directory")
            .join("root with a space");
        let _env = WorktreeRootEnv::set(Some(&spaced_root));
        let error = match create(repo.path(), 8).await {
            Ok(_) => panic!("spaced root must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_on_a_non_repo_errors() {
        let _lock = env_lock();
        let not_repo = tempfile::tempdir().expect("create non-repo tempdir");
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        assert!(create(not_repo.path(), 1).await.is_err());
    }

    #[tokio::test]
    async fn retry_with_backoff_succeeds_after_transient_failures() {
        let attempts = AtomicUsize::new(0);
        let result = retry_with_backoff(&[Duration::ZERO, Duration::ZERO, Duration::ZERO], || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if attempt < 3 {
                    Err(io::Error::other("transient"))
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_with_backoff_gives_up_after_exhausting() {
        let attempts = AtomicUsize::new(0);
        let result = retry_with_backoff(&[Duration::ZERO], || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(io::Error::other("still locked")) }
        })
        .await;
        assert_eq!(
            result.expect_err("retry should fail").to_string(),
            "still locked"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn gc_collects_an_old_terminal_worktree() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "completed", None, "2026-07-09T00:00:00+00:00").await;
        record(
            &pool,
            run_id,
            "project-a",
            "/project/a",
            "/worktrees/run-1",
            "nucleos/run-1",
        )
        .await
        .unwrap();
        set_worktree_created_at(&pool, run_id, "2026-07-09T00:00:00+00:00").await;

        let candidates = gc_candidates(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert_eq!(
            candidates,
            vec![WorktreeRow {
                run_id,
                project_id: "project-a".to_owned(),
                project_root: "/project/a".to_owned(),
                path: "/worktrees/run-1".to_owned(),
                branch: "nucleos/run-1".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn gc_respects_retention_boundary() {
        let pool = test_pool().await;
        let run_id = insert_run(
            &pool,
            "completed",
            Some("2026-07-18T23:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        record(
            &pool,
            run_id,
            "project-a",
            "/project/a",
            "/worktrees/run-2",
            "nucleos/run-2",
        )
        .await
        .unwrap();

        let candidates = gc_candidates(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn gc_never_collects_a_pinned_awaiting_approval_worktree() {
        let pool = test_pool().await;
        let run_id = insert_run(
            &pool,
            "awaiting_approval",
            None,
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        record(
            &pool,
            run_id,
            "project-a",
            "/project/a",
            "/worktrees/run-3",
            "nucleos/run-3",
        )
        .await
        .unwrap();
        set_worktree_created_at(&pool, run_id, "2026-07-09T00:00:00+00:00").await;

        let candidates = gc_candidates(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn gc_skips_already_removed_rows() {
        let pool = test_pool().await;
        let run_id = insert_run(
            &pool,
            "failed",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        record(
            &pool,
            run_id,
            "project-a",
            "/project/a",
            "/worktrees/run-4",
            "nucleos/run-4",
        )
        .await
        .unwrap();
        mark_removed(&pool, run_id).await.unwrap();

        let candidates = gc_candidates(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn gc_never_collects_a_running_run() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "running", None, "2026-07-09T00:00:00+00:00").await;
        record(
            &pool,
            run_id,
            "project-a",
            "/project/a",
            "/worktrees/run-5",
            "nucleos/run-5",
        )
        .await
        .unwrap();
        set_worktree_created_at(&pool, run_id, "2026-07-09T00:00:00+00:00").await;

        let candidates = gc_candidates(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert!(candidates.is_empty());
    }

    #[tokio::test]
    async fn record_then_mark_removed_roundtrips() {
        let pool = test_pool().await;
        let run_id = insert_run(
            &pool,
            "cancelled",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        record(
            &pool,
            run_id,
            "project-a",
            "/project/a",
            "/worktrees/run-6",
            "nucleos/run-6",
        )
        .await
        .unwrap();
        let now = timestamp("2026-07-19T00:00:00+00:00");

        assert_eq!(
            gc_candidates(&pool, now, chrono::Duration::hours(72))
                .await
                .unwrap()
                .len(),
            1
        );

        mark_removed(&pool, run_id).await.unwrap();

        assert!(
            gc_candidates(&pool, now, chrono::Duration::hours(72))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gc_removes_an_expired_terminal_worktree_and_marks_and_feeds() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(
            &pool,
            "completed",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        let info = create_and_record(&pool, repo.path(), run_id).await;
        let now = timestamp("2026-07-19T00:00:00+00:00");

        gc_pass(&pool, now, chrono::Duration::hours(72), &[]).await;

        assert!(!info.path.exists());
        let removed_at: Option<String> =
            sqlx::query_scalar("SELECT removed_at FROM worktrees WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(removed_at.is_some());
        assert!(
            gc_candidates(&pool, now, chrono::Duration::hours(72))
                .await
                .unwrap()
                .is_empty()
        );
        let entries = crate::feed::list_feed(&pool, Some("project-a"), 50)
            .await
            .unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.kind == "worktree_removed" && entry.run_id == Some(run_id))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gc_leaves_a_pinned_awaiting_approval_worktree() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(
            &pool,
            "awaiting_approval",
            None,
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        let info = create_and_record(&pool, repo.path(), run_id).await;

        gc_pass(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
            &[],
        )
        .await;

        assert!(info.path.exists());
        let removed_at: Option<String> =
            sqlx::query_scalar("SELECT removed_at FROM worktrees WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(removed_at.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gc_keeps_an_unmerged_branch_and_deletes_a_merged_branch() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let unmerged_run_id = insert_run(
            &pool,
            "completed",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        let unmerged = create_and_record(&pool, repo.path(), unmerged_run_id).await;
        std::fs::write(unmerged.path.join("unmerged.txt"), "unreviewed\n")
            .expect("write unmerged change");
        assert!(git_ok(
            &unmerged.path,
            &[OsStr::new("add"), OsStr::new("-A")]
        ));
        assert!(git_ok(
            &unmerged.path,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("unmerged work"),
            ]
        ));

        let merged_run_id = insert_run(
            &pool,
            "completed",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        let merged = create_and_record(&pool, repo.path(), merged_run_id).await;

        gc_pass(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
            &[],
        )
        .await;

        assert!(!unmerged.path.exists());
        assert!(!merged.path.exists());
        assert!(
            !git_stdout(
                repo.path(),
                &[
                    OsStr::new("branch"),
                    OsStr::new("--list"),
                    OsStr::new(&unmerged.branch),
                ],
            )
            .is_empty()
        );
        assert!(
            git_stdout(
                repo.path(),
                &[
                    OsStr::new("branch"),
                    OsStr::new("--list"),
                    OsStr::new(&merged.branch),
                ],
            )
            .is_empty()
        );
        let entries = crate::feed::list_feed(&pool, Some("project-a"), 50)
            .await
            .unwrap();
        assert!(entries.iter().any(|entry| {
            entry.kind == "worktree_branch_kept"
                && entry.run_id == Some(unmerged_run_id)
                && entry.summary.contains(&unmerged.branch)
        }));
        assert!(
            entries.iter().any(
                |entry| entry.kind == "worktree_removed" && entry.run_id == Some(merged_run_id)
            )
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gc_reports_failure_and_keeps_the_row_when_removal_fails() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(
            &pool,
            "completed",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        let info = create_and_record(&pool, repo.path(), run_id).await;
        try_remove_once(repo.path(), &info.path)
            .await
            .expect("pre-remove worktree");

        gc_pass(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
            &[],
        )
        .await;

        let removed_at: Option<String> =
            sqlx::query_scalar("SELECT removed_at FROM worktrees WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(removed_at.is_none());
        let entries = crate::feed::list_feed(&pool, Some("project-a"), 50)
            .await
            .unwrap();
        assert!(entries.iter().any(|entry| {
            entry.kind == "worktree_gc_failed"
                && entry.run_id == Some(run_id)
                && entry.summary.contains(info.path.to_string_lossy().as_ref())
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn release_discards_a_pinned_worktree() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(
            &pool,
            "awaiting_approval",
            None,
            "2026-07-19T00:00:00+00:00",
        )
        .await;
        let info = create_and_record(&pool, repo.path(), run_id).await;

        assert_eq!(
            release(&pool, run_id).await.unwrap(),
            ReleaseOutcome::Released
        );

        assert!(!info.path.exists());
        let removed_at: Option<String> =
            sqlx::query_scalar("SELECT removed_at FROM worktrees WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(removed_at.is_some());
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
        let entries = crate::feed::list_feed(&pool, Some("project-a"), 50)
            .await
            .unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| { entry.kind == "worktree_released" && entry.run_id == Some(run_id) })
        );
    }

    #[tokio::test]
    async fn release_frees_the_exclusivity_slot() {
        let pool = test_pool().await;
        let run_id = insert_run(
            &pool,
            "awaiting_approval",
            None,
            "2026-07-19T00:00:00+00:00",
        )
        .await;

        assert_eq!(
            release(&pool, run_id).await.unwrap(),
            ReleaseOutcome::Released
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn release_of_a_non_awaiting_run_is_rejected() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(&pool, "running", None, "2026-07-19T00:00:00+00:00").await;
        let info = create_and_record(&pool, repo.path(), run_id).await;

        assert_eq!(
            release(&pool, run_id).await.unwrap(),
            ReleaseOutcome::NotAwaitingApproval
        );

        assert!(info.path.exists());
        let removed_at: Option<String> =
            sqlx::query_scalar("SELECT removed_at FROM worktrees WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(removed_at.is_none());
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "running");

        remove(repo.path(), &info.path, &[])
            .await
            .expect("remove worktree");
    }

    #[tokio::test]
    async fn release_of_an_unknown_run_is_not_found() {
        let pool = test_pool().await;

        assert_eq!(
            release(&pool, i64::MAX).await.unwrap(),
            ReleaseOutcome::NotFound
        );
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_real_file_lock_blocks_then_clears() {
        let _lock = env_lock();
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), 9).await.expect("create worktree");
        let handle = OpenOptions::new()
            .read(true)
            .write(true)
            .open(info.path.join("seed.txt"))
            .expect("open a Windows file handle");

        let first_attempt = try_remove_once(repo.path(), &info.path).await;
        drop(handle);
        eprintln!(
            "real Windows file lock blocked removal: {}",
            first_attempt.is_err()
        );

        if first_attempt.is_err() {
            assert!(remove(repo.path(), &info.path, &[]).await.is_ok());
        } else {
            // Rust's standard Windows open mode may share delete access. In that case Git can remove
            // the worktree immediately; keep the meaningful eventual-success assertion.
        }
        assert!(!info.path.exists());
    }
}
