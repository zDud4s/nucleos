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
/// What `changed_paths` gives git before giving up. A number of its own rather than
/// `DIFF_TIMEOUT_SECS`: this one runs on the job tick, not inside a request somebody is waiting on.
const CHANGED_PATHS_TIMEOUT_SECS: u64 = 20;
/// A ceiling on the output, in bytes. ~1 MiB of path names is tens of thousands of files; past
/// that is a repository this measurement does not serve.
const CHANGED_PATHS_CAP: usize = 1024 * 1024;
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

/// Every path this worktree has touched since it was born, committed or not.
///
/// The union of two questions, and **both** are required. `git diff` alone lies in exactly the case
/// that matters: `job.rs` commits a checkpoint at every green gate, so a diff of the working tree
/// comes back empty halfway through a job that has already rewritten a dozen files. `git status`
/// alone loses everything already committed.
///
/// `base` is a sha this daemon wrote into `worktrees.base_sha` — never a string from a caller — and
/// is still validated as hexadecimal. A revision argument that may begin with `-` is an option to
/// git, and the value travels through a database other code writes.
///
/// Error rather than truncation, unlike `diff`: a truncated list loses paths and reads as `clean`
/// for those files, which is the one outcome this measurement must not have.
pub fn changed_paths(root: &Path, base: &str) -> Result<Vec<String>, InspectError> {
    // 40 hexadecimal characters, which is what `git rev-parse HEAD` returns under SHA-1 — and the
    // repository's `core.abbrev` does not shorten it. A repository on `--object-format=sha256`
    // returns 64 and would be refused forever, in permanent `not measured` with no diagnosis; so
    // the pair is accepted rather than one length.
    let hexadecimal = base.chars().all(|character| character.is_ascii_hexdigit());
    if !matches!(base.len(), 40 | 64) || !hexadecimal {
        return Err(InspectError::UnsafePath);
    }

    let range = format!("{base}..HEAD");
    let committed = run_git(
        root,
        &[
            "-c",
            "diff.external=",
            "-c",
            "core.fsmonitor=",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "-z",
            &range,
            "--",
        ],
    )?;
    // `--no-optional-locks` because this runs on the job tick against a **live** worktree, where an
    // agent is running git at the same time: a normal `git status` refreshes and rewrites the
    // index, and takes `.git/index.lock` to do it. The flag removes the contention; the price is an
    // uncached walk, which is what this measurement was going to do anyway.
    let uncommitted = run_git(
        root,
        &[
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )?;

    let mut paths: std::collections::BTreeSet<String> = committed
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| String::from_utf8_lossy(record).into_owned())
        .collect();
    paths.extend(parse_status_z(&uncommitted));
    Ok(paths.into_iter().collect())
}

/// The parsing contract of `--porcelain=v1 -z`, written out in full because a mistake here produces
/// a false `clean`.
///
/// Records are NUL-terminated. Each is `XY <path>`: two status columns, a space, then the path,
/// taken by position. `-z` also **turns off** git's C-quoting, which would otherwise escape paths
/// with spaces or non-ASCII into something a naive reader splits in half.
///
/// A rename or copy (`R`/`C` in either column) emits a **second** NUL-terminated field right after
/// the record: the origin path. Both count — a worktree that renames a file another one is editing
/// has collided with it.
fn parse_status_z(output: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut records = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let (status, path) = record.split_at(3);
        paths.push(String::from_utf8_lossy(path).into_owned());
        let renamed = matches!(status[0], b'R' | b'C') || matches!(status[1], b'R' | b'C');
        if renamed && let Some(origin) = records.next() {
            paths.push(String::from_utf8_lossy(origin).into_owned());
        }
    }
    paths
}

/// A git with a deadline and a ceiling, whose output is returned whole or not returned.
///
/// The shape follows `diff` above, polling included — this code runs on a blocking thread, and a
/// git that ignores a closed pipe is exactly the case worth killing. The difference is at the end:
/// here, exceeding the deadline or the ceiling is an `Err`.
///
/// **Inherited limitation, said out loud:** `read_to_end` blocks until EOF, so the deadline only
/// applies once git has closed its output — a git hung *without writing anything* pins this thread.
/// It is the shape `diff` already has and is not fixed here. The mitigation is on the caller's
/// side: the tick does not wait for the measurement, and how many worktrees it measures per tick
/// has a ceiling.
fn run_git(root: &Path, args: &[&str]) -> Result<Vec<u8>, InspectError> {
    use std::io::Read;

    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(InspectError::Io)?;

    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut bytes = Vec::new();
    let read = stdout
        .by_ref()
        .take(CHANGED_PATHS_CAP as u64 + 1)
        .read_to_end(&mut bytes);

    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(CHANGED_PATHS_TIMEOUT_SECS);
    let refused;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                refused = !status.success();
                break;
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                refused = true;
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(error) => return Err(InspectError::Io(error)),
        }
    }
    read.map_err(InspectError::Io)?;

    if refused || bytes.len() > CHANGED_PATHS_CAP {
        return Err(InspectError::Io(std::io::Error::other(
            "git refused, timed out, or wrote past the ceiling",
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::tempdir;

    /// A git that has to succeed.
    ///
    /// `pub(crate)` along with `seeded_repo` below, because `collision.rs` reuses them — the same
    /// pattern `git_exec.rs` already uses to expose its `mod tests` to `http.rs`.
    pub(crate) fn git_in_repo(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repository with one commit, and that commit's sha.
    pub(crate) fn seeded_repo() -> (tempfile::TempDir, String) {
        let repo = tempdir().unwrap();
        git_in_repo(repo.path(), &["init"]);
        git_in_repo(repo.path(), &["config", "user.email", "test@x"]);
        git_in_repo(repo.path(), &["config", "user.name", "test"]);
        // Rename detection is a user setting (`status.renames`), so the rename test would otherwise
        // depend on the machine running it. Pinned in the test repository instead.
        git_in_repo(repo.path(), &["config", "status.renames", "true"]);
        std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
        git_in_repo(repo.path(), &["add", "-A"]);
        git_in_repo(repo.path(), &["commit", "-m", "seed"]);

        let base = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        (repo, base)
    }

    /// The case that makes the measurement lie if only one of the two questions is asked: `job.rs`
    /// commits a checkpoint at every green gate, so a plain `git diff` returns empty halfway
    /// through a job that has already rewritten half a dozen files.
    #[test]
    fn a_checkpointed_change_and_an_untracked_one_both_count() {
        let (repo, base) = seeded_repo();
        std::fs::write(repo.path().join("committed.rs"), "fn a() {}\n").unwrap();
        git_in_repo(repo.path(), &["add", "-A"]);
        git_in_repo(repo.path(), &["commit", "-m", "checkpoint"]);
        std::fs::write(repo.path().join("scratch.rs"), "fn b() {}\n").unwrap();

        let paths = changed_paths(repo.path(), &base).unwrap();

        assert!(paths.contains(&"committed.rs".to_string()), "{paths:?}");
        assert!(paths.contains(&"scratch.rs".to_string()), "{paths:?}");
    }

    /// A rename yields both paths. A worktree that renames a file another one is editing has
    /// collided with it, and naming only the destination would hide half of what happened.
    #[test]
    fn a_rename_yields_both_the_source_and_the_destination() {
        let (repo, base) = seeded_repo();
        git_in_repo(repo.path(), &["mv", "seed.txt", "renamed.txt"]);

        let paths = changed_paths(repo.path(), &base).unwrap();

        assert!(paths.contains(&"seed.txt".to_string()), "{paths:?}");
        assert!(paths.contains(&"renamed.txt".to_string()), "{paths:?}");
    }

    /// The input where a naive split loses half and produces a false `clean`. `-z` turns off git's
    /// C-quoting and NUL-terminates precisely for this.
    #[test]
    fn a_rename_whose_destination_has_a_space_yields_both_paths_whole() {
        let (repo, base) = seeded_repo();
        git_in_repo(repo.path(), &["mv", "seed.txt", "a name with spaces.txt"]);

        let paths = changed_paths(repo.path(), &base).unwrap();

        assert!(
            paths.contains(&"a name with spaces.txt".to_string()),
            "{paths:?}"
        );
        assert!(paths.contains(&"seed.txt".to_string()), "{paths:?}");
    }

    #[test]
    fn an_untracked_path_with_a_space_is_not_split() {
        let (repo, base) = seeded_repo();
        std::fs::write(repo.path().join("two words.rs"), "x\n").unwrap();

        let paths = changed_paths(repo.path(), &base).unwrap();

        assert_eq!(paths, vec!["two words.rs".to_string()]);
    }

    /// A base that is not a sha is refused, rather than silently handed to git as an argument. The
    /// value travels through a database other code writes, and an argument starting with `-` is an
    /// option.
    #[test]
    fn a_base_that_is_not_a_commit_sha_is_refused() {
        let (repo, _) = seeded_repo();

        for base in ["--output=/tmp/x", "", "HEAD", &"z".repeat(40)] {
            assert!(
                matches!(
                    changed_paths(repo.path(), base),
                    Err(InspectError::UnsafePath)
                ),
                "accepted `{base}`"
            );
        }
    }

    /// A git that refuses is an error, never an empty set. Empty reads as `clean`, and saying
    /// `clean` without having measured is the one way this screen can do active damage.
    #[test]
    fn a_base_git_does_not_know_is_an_error_and_not_an_empty_set() {
        let (repo, _) = seeded_repo();
        let absent = "0".repeat(40);

        assert!(changed_paths(repo.path(), &absent).is_err());
    }

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
