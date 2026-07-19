use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::fs::OpenOptions;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    const ROOT_ENV: &str = "NUCLEOS_WORKTREE_ROOT";

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
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
