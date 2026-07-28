use sqlx::SqlitePool;
use std::path::{Component, Path, PathBuf};

const CAT_CAP: usize = 256 * 1024;
const GREP_CAP: usize = 200;
const GREP_LINE_CAP: usize = 400;
/// Files past this are not scanned at all. A grep walks the whole tree, so without a per-file
/// ceiling one build artifact decides how much memory the walk takes.
const GREP_FILE_CAP: usize = 2 * 1024 * 1024;
/// `git diff` is a subprocess over a repository the caller named, so it gets a deadline and its
/// output gets a ceiling — neither of which it had.
const DIFF_TIMEOUT_SECS: u64 = 20;
const DIFF_CAP: usize = 1024 * 1024;
const SKIP_DIRS: [&str; 4] = [".git", "node_modules", "target", "__pycache__"];

#[derive(Debug)]
pub enum InspectError {
    UnsafePath,
    NotFound,
    Io(std::io::Error),
}

#[derive(serde::Serialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(serde::Serialize)]
pub struct Match {
    pub path: String,
    pub line: u64,
    pub text: String,
}

/// Returns the project's stored root, or None when the row is absent OR the root is NULL (off mode).
pub async fn project_root(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<String>> {
    let root: Option<Option<String>> =
        sqlx::query_scalar("SELECT project_root FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    Ok(root.flatten())
}

/// Join a caller-supplied relative path onto `root`, rejecting absolute paths and ANY `..`/root/prefix
/// component (conservative: even a `..` that would stay inside is rejected). Empty or "." -> root itself.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, InspectError> {
    let rel = rel.trim();
    if rel.is_empty() || rel == "." {
        return Ok(root.to_path_buf());
    }
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(InspectError::UnsafePath);
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(InspectError::UnsafePath);
            }
        }
    }
    Ok(root.join(candidate))
}

/// `safe_join`, then the check the lexical filter cannot make: that the path RESOLVES inside the
/// root.
///
/// A directory symlink or junction inside the project has only `Normal` components and still lands
/// outside it, and on Windows `mklink /J` needs no elevation — so a run allowed to write inside its
/// own worktree could build one and then read anything the daemon can read. `grep` happened to be
/// safe by accident (`is_dir()` is false for a reparse point), which is exactly the kind of
/// inconsistency worth removing.
///
/// Resolving and comparing rather than refusing links outright: a project may legitimately contain
/// its own internal links, and rejecting those would be a different bug. Kept separate from
/// `safe_join` so that stays a pure path function — this one necessarily touches the filesystem,
/// and reports `NotFound` for a path that does not exist, which is what the callers want anyway.
fn resolved_within(root: &Path, rel: &str) -> Result<PathBuf, InspectError> {
    let joined = safe_join(root, rel)?;
    let resolved_root = std::fs::canonicalize(root).map_err(io_err)?;
    let resolved = std::fs::canonicalize(&joined).map_err(io_err)?;
    if !resolved.starts_with(&resolved_root) {
        return Err(InspectError::UnsafePath);
    }
    Ok(resolved)
}

fn io_err(e: std::io::Error) -> InspectError {
    if e.kind() == std::io::ErrorKind::NotFound {
        InspectError::NotFound
    } else {
        InspectError::Io(e)
    }
}

pub fn ls(root: &Path, rel: &str) -> Result<Vec<Entry>, InspectError> {
    let dir = resolved_within(root, rel)?;
    let read = std::fs::read_dir(&dir).map_err(io_err)?;
    let mut entries = Vec::new();
    for item in read {
        let item = item.map_err(InspectError::Io)?;
        let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push(Entry {
            name: item.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }
    // Directories first, then files; each group alphabetical.
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

pub fn cat(root: &Path, rel: &str) -> Result<String, InspectError> {
    use std::io::Read;

    let file = resolved_within(root, rel)?;
    // Reads one byte past the cap rather than the whole file: `CAT_CAP` used to truncate the
    // RESPONSE while `read()` had already allocated everything, so a large artifact in the tree was
    // a memory amplifier — and a daemon that dies takes the kill switch and the approval gate with
    // it. The extra byte is what distinguishes "exactly at the cap" from "there is more".
    let handle = std::fs::File::open(&file).map_err(io_err)?;
    let mut bytes = Vec::new();
    handle
        .take(CAT_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_err)?;
    let truncated = bytes.len() > CAT_CAP;
    let slice = if truncated {
        &bytes[..CAT_CAP]
    } else {
        &bytes[..]
    };
    let mut text = String::from_utf8_lossy(slice).into_owned();
    if truncated {
        text.push_str("\n…[truncated]");
    }
    Ok(text)
}

pub fn grep(root: &Path, query: &str, rel: &str) -> Result<Vec<Match>, InspectError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let base = resolved_within(root, rel)?;
    // Reported paths are relative to the root, and `base` is now canonical — so the prefix stripped
    // off has to be canonical too, or every match comes back as an absolute path.
    let strip_root = std::fs::canonicalize(root).map_err(io_err)?;
    let mut out = Vec::new();
    let mut stack = vec![base];
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for item in read.flatten() {
            let file_type = match item.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let path = item.path();
            if file_type.is_dir() {
                let name = item.file_name().to_string_lossy().into_owned();
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                // A grep walks the whole tree, so one oversized file is enough to make the walk
                // itself the problem: `read_to_string` had no cap at all. Skipping past the cap
                // costs nothing real — a file that large is a build artifact or a dataset, not
                // something a person is grepping for a line of code in.
                if item.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > GREP_FILE_CAP as u64 {
                    continue;
                }
                // read_to_string fails on binary/non-UTF8 files -> silently skipped.
                if let Ok(content) = std::fs::read_to_string(&path) {
                    for (index, line) in content.lines().enumerate() {
                        if line.contains(query) {
                            let rel_path = path
                                .strip_prefix(&strip_root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .replace('\\', "/");
                            out.push(Match {
                                path: rel_path,
                                line: index as u64 + 1,
                                text: line.chars().take(GREP_LINE_CAP).collect(),
                            });
                            if out.len() >= GREP_CAP {
                                return Ok(out);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

/// `git diff` over a project's own checkout.
///
/// Git reads configuration from the repository it is pointed at, and several of those settings are
/// COMMAND STRINGS: `diff.external`, a `textconv` filter named by the repo's own `.gitattributes`,
/// and `core.fsmonitor`. Running plain `git diff` inside a root the caller supplied therefore
/// executed whatever that root's `.git/config` asked for, as the daemon user. The flags below turn
/// every one of those off explicitly rather than trusting the target repository not to set them.
///
/// It also gets a deadline and an output ceiling, having had neither: the whole diff was buffered
/// into memory and into the response, and a hung git held the blocking thread indefinitely.
pub fn diff(root: &Path) -> Result<String, InspectError> {
    use std::io::Read;

    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        // `-c` beats anything the repository's own config sets.
        .arg("-c")
        .arg("diff.external=")
        .arg("-c")
        .arg("core.fsmonitor=")
        .arg("diff")
        .arg("--no-ext-diff")
        .arg("--no-textconv")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(InspectError::Io)?;

    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut bytes = Vec::new();
    let read = stdout
        .by_ref()
        .take(DIFF_CAP as u64 + 1)
        .read_to_end(&mut bytes);

    // The deadline is enforced by polling rather than by a timer thread: this already runs on a
    // blocking thread, and a git that ignores a closed pipe is exactly the case worth killing.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(DIFF_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(error) => return Err(InspectError::Io(error)),
        }
    }
    read.map_err(InspectError::Io)?;

    let truncated = bytes.len() > DIFF_CAP;
    if truncated {
        bytes.truncate(DIFF_CAP);
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str("\n…[truncated]");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// The lexical filter is right about `..`, drive prefixes and UNC, but it never checks the path
    /// it produced. A directory symlink or junction inside the project has only `Normal` components
    /// and still resolves outside it — and on Windows `mklink /J` needs no elevation, so a worktree
    /// run that may write in its own tree can make one and then read anything the daemon can read.
    #[test]
    fn a_link_out_of_the_project_is_rejected() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("project");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), "PRIVATE KEY").unwrap();

        // Skipped rather than silently passing where the OS will not let us build the fixture:
        // unprivileged symlink creation is off by default on some Windows configurations.
        let link = root.join("out");
        if std::os::windows::fs::symlink_dir(&outside, &link).is_err() {
            eprintln!("skipping: this machine cannot create a directory symlink unprivileged");
            return;
        }

        assert!(matches!(
            cat(&root, "out/id_rsa"),
            Err(InspectError::UnsafePath)
        ));
        assert!(matches!(ls(&root, "out"), Err(InspectError::UnsafePath)));
    }

    #[test]
    fn a_link_that_stays_inside_the_project_is_still_readable() {
        // The check has to be "resolves inside the root", not "is not a link" — a project may well
        // contain its own internal links, and refusing those would be a different bug.
        let temp = tempdir().unwrap();
        let root = temp.path().join("project");
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("note.txt"), "inside").unwrap();

        let link = root.join("alias");
        if std::os::windows::fs::symlink_dir(&real, &link).is_err() {
            eprintln!("skipping: this machine cannot create a directory symlink unprivileged");
            return;
        }

        assert_eq!(cat(&root, "alias/note.txt").unwrap(), "inside");
    }

    /// `CAT_CAP` truncated the RESPONSE, not the read: the whole file was allocated first. A few
    /// concurrent requests over a large artifact are enough to take the daemon down, and with it
    /// the kill switch and the approval gate.
    #[test]
    fn cat_does_not_read_more_than_it_will_return() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let big = vec![b'x'; CAT_CAP * 4];
        std::fs::write(root.join("big.bin"), &big).unwrap();

        let text = cat(root, "big.bin").unwrap();
        assert!(text.len() < CAT_CAP + 64, "returned {} bytes", text.len());
        assert!(text.ends_with("[truncated]"));
    }

    #[test]
    fn grep_skips_a_file_too_large_to_be_worth_scanning() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("small.txt"), "needle here").unwrap();
        let mut big = vec![b'x'; GREP_FILE_CAP + 1];
        big.extend_from_slice(b"\nneedle here");
        std::fs::write(root.join("big.txt"), &big).unwrap();

        let matches = grep(root, "needle", "").unwrap();
        assert_eq!(matches.len(), 1, "the oversized file must not be scanned");
        assert_eq!(matches[0].path, "small.txt");
    }

    #[test]
    fn safe_join_accepts_normal_relative_paths() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        assert_eq!(
            safe_join(root, "src/main.rs").unwrap(),
            root.join("src/main.rs")
        );
        assert_eq!(safe_join(root, "").unwrap(), root);
        assert_eq!(safe_join(root, ".").unwrap(), root);
    }

    #[test]
    fn safe_join_rejects_traversal_and_absolute() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        for path in ["..", "../x", "a/../b", "/etc/passwd", "C:/x"] {
            assert!(matches!(
                safe_join(root, path),
                Err(InspectError::UnsafePath)
            ));
        }
    }

    #[test]
    fn ls_lists_dirs_first() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("a.txt"), "file").unwrap();
        std::fs::create_dir(root.join("zsub")).unwrap();

        let entries = ls(root, "").unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "zsub");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "a.txt");
        assert!(!entries[1].is_dir);
    }

    #[test]
    fn cat_reads_and_reports_missing() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("hello.txt"), "hi").unwrap();

        assert_eq!(cat(root, "hello.txt").unwrap(), "hi");
        assert!(matches!(cat(root, "nope.txt"), Err(InspectError::NotFound)));
    }

    #[test]
    fn grep_finds_across_files_skips_git_and_caps() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("keep.txt"), "before\nneedle here\nafter").unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "needle hidden").unwrap();

        let matches = grep(root, "needle", "").unwrap();
        assert!(matches.iter().any(|item| item.path == "keep.txt"));
        assert!(!matches.iter().any(|item| item.path.starts_with(".git/")));

        std::fs::write(root.join("many.txt"), "needle\n".repeat(250)).unwrap();
        assert_eq!(grep(root, "needle", "").unwrap().len(), GREP_CAP);
    }

    #[tokio::test]
    async fn project_root_reads_autopilot_state() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('p', 'active', '/some/root')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('off1', 'off')")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            project_root(&pool, "p").await.unwrap(),
            Some("/some/root".to_string())
        );
        assert_eq!(project_root(&pool, "off1").await.unwrap(), None);
        assert_eq!(project_root(&pool, "unknown").await.unwrap(), None);
    }
}
