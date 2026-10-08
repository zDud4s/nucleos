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
pub fn resolved_within(root: &Path, rel: &str) -> Result<PathBuf, InspectError> {
    let joined = safe_join(root, rel)?;
    let resolved_root = std::fs::canonicalize(root).map_err(io_err)?;
    let resolved = std::fs::canonicalize(&joined).map_err(io_err)?;
    if !resolved.starts_with(&resolved_root) {
        return Err(InspectError::UnsafePath);
    }
    Ok(resolved)
}

/// Where a WRITE of `rel` should land, refused unless it lands inside `root`.
///
/// `root` is whatever directory the file belongs in: a project's folder, this machine's
/// `~/.nucleos/`, or a project's state directory under it. [`resolved_within`] cannot answer this,
/// and the reason is the whole point of a second function: it canonicalizes the target, and a
/// write's target routinely does not exist yet — a subdirectory on the way to it may not either.
///
/// So this resolves the deepest ancestor that DOES exist and requires that to be inside the root.
/// That is the same guarantee for the same threat — a directory symlink or a junction inside the
/// project has only `Normal` components and still lands outside it, and on Windows `mklink /J`
/// needs no elevation, so a run allowed to write inside its own worktree could build one — while
/// staying answerable for a path that is about to be created.
///
/// The root itself must exist; a project whose recorded root is gone from disk is a different
/// failure and gets [`InspectError::NotFound`] here rather than a write into a directory this
/// function would otherwise create.
pub fn safe_write_target(root: &Path, rel: &str) -> Result<PathBuf, InspectError> {
    let joined = safe_join(root, rel)?;
    // The root itself is not a file, and `safe_join` returns it for an empty path. Refusing here
    // rather than at the caller keeps every writer honest about it.
    if joined == root {
        return Err(InspectError::UnsafePath);
    }
    let resolved_root = std::fs::canonicalize(root).map_err(io_err)?;
    let mut existing = joined.as_path();
    let resolved = loop {
        match std::fs::canonicalize(existing) {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match existing.parent() {
                Some(parent) => existing = parent,
                // Walked past the root without finding anything that exists. Only reachable if the
                // root vanished between the check above and here.
                None => return Err(InspectError::NotFound),
            },
            Err(error) => return Err(io_err(error)),
        }
    };
    if !resolved.starts_with(&resolved_root) {
        return Err(InspectError::UnsafePath);
    }
    Ok(joined)
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
    diff_with(root, &[])
}

/// Everything one run changed since it branched, committed or not, optionally for a single file.
///
/// **`git diff <base>` and not `git diff`, and the difference is the whole review surface.** A plain
/// working-tree diff comes back empty halfway through a job, because `job.rs` commits a checkpoint
/// at every green gate — so the reviewer would be shown nothing at exactly the moment there is most
/// to see. Comparing against the branch point includes the checkpoints and the uncommitted work
/// together, which is what "what did this run do" means.
///
/// `base` is a sha this daemon wrote into `worktrees.base_sha` and is validated as hexadecimal all
/// the same, for the reason `changed_paths` gives: a revision argument that may begin with `-` is an
/// option to git, and the value travels through a database other code writes.
pub fn diff_since(root: &Path, base: &str, rel: &str) -> Result<String, InspectError> {
    let hexadecimal = base.chars().all(|character| character.is_ascii_hexdigit());
    if !matches!(base.len(), 40 | 64) || !hexadecimal {
        return Err(InspectError::UnsafePath);
    }
    let rel = rel.trim();
    if rel.is_empty() || rel == "." {
        return diff_with(root, &[base]);
    }
    // The same lexical refusal every other reader gives. Git takes this as a pathspec, and a
    // pathspec climbs out of a repository as happily as an open does.
    safe_join(root, rel)?;
    diff_with(root, &[base, "--", rel])
}

fn diff_with(root: &Path, extra: &[&str]) -> Result<String, InspectError> {
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
        .args(extra)
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

/* -------------------------------------------------------- history and branches -- */

/// The field separator inside one record of a git `--format`.
///
/// ASCII unit separator, which cannot appear in a commit subject, an author name or a branch name —
/// unlike a comma, a tab or a pipe, all of which can and one day will. The record separator is NUL,
/// through `-z`, for the same reason applied to newlines in a commit message.
const FIELD: char = '\u{1f}';

/// The most commits one read returns. A history panel is a panel, not an export.
const LOG_CAP: usize = 200;

/// The most branches one read returns, most recently committed first.
///
/// A ceiling because the ahead/behind figures cost one `git rev-list` **each** — the batched
/// `%(ahead-behind:…)` atom needs a newer git than this project is willing to require — so an
/// unbounded list would make a repository with three hundred stale branches spawn three hundred
/// processes for a panel nobody could read anyway.
const BRANCH_CAP: usize = 40;

/// One commit, as a panel needs it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Commit {
    pub sha: String,
    pub short_sha: String,
    pub author: String,
    /// Author date, strict ISO 8601 with an offset — `%aI`, never `%ad`, whose shape the
    /// repository's own config can change underneath us.
    pub at: String,
    pub subject: String,
}

/// One local branch, and how far it is from where work lands.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BranchRow {
    pub name: String,
    /// Commits this branch has that the integration branch does not.
    pub ahead: i64,
    /// Commits the integration branch has that this one does not.
    pub behind: i64,
    /// Whether the pair above was measured at all, which is NOT the same as measuring zero.
    ///
    /// `0/0` means *identical to where work lands*. A root on a detached HEAD, or a git that
    /// refused, means *nobody knows* — and a panel that drew the second as the first would report
    /// every branch as up to date the moment the measurement broke.
    pub measured: bool,
    pub last_commit_at: String,
    pub last_subject: String,
}

/// The branches of a project, and the one they are measured against.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Branches {
    /// The branch checked out at the project root.
    ///
    /// **A heuristic for this panel, and no longer `land.rs`'s own answer.** Before
    /// `core/src/land.rs` existed this WAS where a landing's target came from, and the two were the
    /// same read; the design at `.ai/specs/2026-08-27-dono-da-arvore-principal-design.md` killed
    /// that read precisely because a checkout parked on the wrong branch silently redirected every
    /// landing. `land::integration_branch` now answers from `autopilot_state.integration_branch`,
    /// declared once per project rather than read off a worktree. This function is synchronous and
    /// has no `project_id` to look one up with, so it keeps the old read for the panel alone — a
    /// project whose main checkout is parked away from its declared branch will see this measure
    /// against the wrong thing until this is threaded through to the real answer, which is follow-up
    /// work and not scene-setting for anything this struct's own callers rely on today.
    pub integration: Option<String>,
    pub branches: Vec<BranchRow>,
    /// Branches past the ceiling, which were not measured. Zero is the ordinary case.
    pub omitted: usize,
}

/// A project's recent commits, newest first.
///
/// `rel` narrows the history to one path and is validated through `safe_join` even though nothing
/// here opens it: git takes it as a pathspec, and a pathspec is not a file name — a leading `-` is
/// an option and a `..` reaches outside the repository. Refusing the same shapes the readers refuse
/// keeps one answer to "what may a caller name" rather than two.
pub fn log(root: &Path, rel: &str, limit: usize) -> Result<Vec<Commit>, InspectError> {
    // Validated for its shape and then discarded: what git is given is the caller's relative string,
    // because a pathspec is relative to the repository and `safe_join` returns an absolute path.
    safe_join(root, rel)?;
    let limit = limit.clamp(1, LOG_CAP).to_string();
    let format = format!("--format=%H{FIELD}%h{FIELD}%an{FIELD}%aI{FIELD}%s");

    let mut args = vec![
        "-c",
        "core.fsmonitor=",
        "log",
        "-z",
        "--no-color",
        &format,
        "-n",
        &limit,
    ];
    let rel = rel.trim();
    if !rel.is_empty() && rel != "." {
        // After `--`, so a path that looks like an option is a path.
        args.push("--");
        args.push(rel);
    }

    let output = run_git(root, &args)?;
    Ok(parse_log(&output))
}

/// PURE: git's `-z` log output as commits.
///
/// A record with too few fields is dropped rather than padded. Padding would invent an empty author
/// or an empty date for a row that is really a parse failure, and a panel would show it as a commit
/// by nobody.
pub fn parse_log(output: &[u8]) -> Vec<Commit> {
    String::from_utf8_lossy(output)
        .split('\0')
        .filter(|record| !record.trim().is_empty())
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split(FIELD);
            Some(Commit {
                sha: fields.next()?.to_owned(),
                short_sha: fields.next()?.to_owned(),
                author: fields.next()?.to_owned(),
                at: fields.next()?.to_owned(),
                // Last, and it takes the remainder: a subject cannot hold the separator, but a
                // parser that depends on that is one surprise away from splitting a record.
                subject: fields.collect::<Vec<_>>().join(&FIELD.to_string()),
            })
        })
        .collect()
}

/// The most lines one blame returns.
///
/// A blame is read beside a diff, one screen at a time. Past this the answer is not a panel, and
/// the cost is real: `--line-porcelain` repeats the whole commit header for every single line, so a
/// large file is megabytes of output for a question about twenty lines of it.
const BLAME_CAP: usize = 4_000;

/// One line of a file, and who last touched it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BlameLine {
    pub line: u64,
    pub sha: String,
    pub author: String,
    /// Epoch seconds, as git reports them. Formatted by whoever draws it.
    pub at: i64,
    pub summary: String,
    pub text: String,
    /// Whether this line is in the working tree and not in any commit.
    ///
    /// Git blames an uncommitted line to the all-zero sha, and the fields around it are placeholders
    /// — "Not Committed Yet" as the author, the current time as the date. Left as git reports them
    /// but flagged, because a panel that showed that row like any other would credit a person who
    /// never wrote it.
    pub uncommitted: bool,
}

/// Who last touched each line of a file.
///
/// `-w` so that a reindentation does not reassign a hundred lines to whoever ran the formatter —
/// blame is read to find out *why* a line is the way it is, and whitespace is never the reason.
pub fn blame(root: &Path, rel: &str) -> Result<Vec<BlameLine>, InspectError> {
    // The same resolution `cat` uses, which is the stronger of the two checks: it follows links and
    // refuses a path that lands outside the root. A blame reads a file, so it earns the same guard.
    resolved_within(root, rel)?;
    let output = run_git(
        root,
        &[
            "-c",
            "core.fsmonitor=",
            "blame",
            "-w",
            "--line-porcelain",
            "--",
            rel,
        ],
    )?;
    Ok(parse_blame(&output, BLAME_CAP))
}

/// PURE: `git blame --line-porcelain` as lines.
///
/// The format repeats a full header block per line: a `<sha> <orig> <final>` line, then `key value`
/// lines, then the content prefixed with a tab. A record is only emitted when the tab line arrives,
/// so a truncated tail cannot produce a half-built row.
pub fn parse_blame(output: &[u8], cap: usize) -> Vec<BlameLine> {
    let text = String::from_utf8_lossy(output);
    let mut lines = Vec::new();

    let mut sha = String::new();
    let mut line_number: u64 = 0;
    let mut author = String::new();
    let mut at: i64 = 0;
    let mut summary = String::new();

    for record in text.lines() {
        if let Some(content) = record.strip_prefix('\t') {
            if lines.len() >= cap {
                break;
            }
            lines.push(BlameLine {
                line: line_number,
                // All zeroes is git's way of saying "this line is not in any commit yet".
                uncommitted: sha.chars().all(|character| character == '0') && !sha.is_empty(),
                sha: std::mem::take(&mut sha),
                author: std::mem::take(&mut author),
                at,
                summary: std::mem::take(&mut summary),
                text: content.to_owned(),
            });
            continue;
        }

        if let Some(rest) = record.strip_prefix("author ") {
            author = rest.to_owned();
        } else if let Some(rest) = record.strip_prefix("author-time ") {
            at = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = record.strip_prefix("summary ") {
            summary = rest.to_owned();
        } else if let Some((candidate, rest)) = record.split_once(' ') {
            // A header line: 40 or 64 hexadecimal characters, then the original and final line
            // numbers. Anything else is one of the many `key value` lines this does not read.
            let hexadecimal = candidate
                .chars()
                .all(|character| character.is_ascii_hexdigit());
            if matches!(candidate.len(), 40 | 64) && hexadecimal {
                sha = candidate.to_owned();
                // The FINAL line number, which is the second field — the first is where the line
                // was in the commit that introduced it, and using that would number the panel by a
                // file that no longer exists.
                line_number = rest
                    .split_whitespace()
                    .nth(1)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
            }
        }
    }

    lines
}

/// What a run has changed, and how much of the tree it left alone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Changed {
    pub paths: Vec<String>,
    /// Files tracked in this tree. The subtraction is the point: the Code mode opens on what
    /// changed and says how many files nobody touched, which is what makes it a review surface
    /// rather than a file browser that happens to be read-only.
    pub tracked: usize,
}

/// The changed paths of a worktree, with the size of the tree they sit in.
pub fn changed(root: &Path, base: &str) -> Result<Changed, InspectError> {
    let paths = changed_paths(root, base)?;
    let listed = run_git(root, &["--no-optional-locks", "ls-files", "-z"])?;
    let tracked = listed
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .count();
    Ok(Changed { paths, tracked })
}

/// Every local branch, with its distance from where work lands.
pub fn branches(root: &Path) -> Result<Branches, InspectError> {
    let integration = current_branch(root)?;

    let format = format!(
        "--format=%(refname:short){FIELD}%(committerdate:iso-strict){FIELD}%(contents:subject)"
    );
    let listed = run_git(
        root,
        &[
            "-c",
            "core.fsmonitor=",
            "for-each-ref",
            "--sort=-committerdate",
            &format,
            "refs/heads/",
        ],
    )?;

    let all = parse_branch_list(&listed);
    let omitted = all.len().saturating_sub(BRANCH_CAP);

    let mut branches = Vec::new();
    for (name, at, subject) in all.into_iter().take(BRANCH_CAP) {
        // Names come from git's own ref listing, never from a caller, so there is nothing here to
        // validate that git did not already guarantee.
        let (ahead, behind, measured) = match integration.as_deref() {
            // The integration branch against itself is a real answer — zero and zero — and that is
            // a different fact from a branch that could not be measured.
            Some(target) if target == name => (0, 0, true),
            Some(target) => distance(root, target, &name)?,
            // No integration branch means a detached root. There is nothing to measure against, and
            // saying so beats measuring against a default nobody chose.
            None => (0, 0, false),
        };
        branches.push(BranchRow {
            name,
            ahead,
            behind,
            measured,
            last_commit_at: at,
            last_subject: subject,
        });
    }

    Ok(Branches {
        integration,
        branches,
        omitted,
    })
}

/// PURE: one line per branch, three fields each. Short records are dropped, never padded.
pub fn parse_branch_list(output: &[u8]) -> Vec<(String, String, String)> {
    String::from_utf8_lossy(output)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.split(FIELD);
            let name = fields.next()?.to_owned();
            let at = fields.next()?.to_owned();
            // A branch whose tip has an empty subject is possible; a branch with no third field at
            // all is a parse failure. `unwrap_or_default` would merge the two.
            let subject = fields.next()?.to_owned();
            (!name.is_empty()).then_some((name, at, subject))
        })
        .collect()
}

/// The branch checked out at `root`, or `None` on a detached HEAD.
fn current_branch(root: &Path) -> Result<Option<String>, InspectError> {
    // `--quiet` so a detached HEAD is exit 1 with no output rather than the word "HEAD", which is
    // also a legal branch name and would be indistinguishable from one.
    let output = match run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(output) => output,
        // A detached HEAD is an ordinary state, not a broken repository.
        Err(InspectError::Io(_)) => return Ok(None),
        Err(other) => return Err(other),
    };
    let name = String::from_utf8_lossy(&output).trim().to_owned();
    Ok((!name.is_empty()).then_some(name))
}

/// How far `branch` is from `target`, in commits each way.
///
/// `--count` on a symmetric difference.
///
/// **An unrelated history is counted, not refused** — checked against git rather than assumed, and
/// the assumption here was wrong first: `rev-list --left-right --count A...B` on two histories with
/// no common ancestor exits 0 and reports both sides in full. That is a true answer to the question
/// asked, and it is the one that reads correctly on the panel as well: a grafted branch shows every
/// commit on both sides, which is exactly the work a merge of it would involve.
///
/// The command that *does* refuse is `merge-base`, and calling it would cost one more process per
/// branch — forty more per read — to distinguish a case that is honest without it. Not paid.
fn distance(root: &Path, target: &str, branch: &str) -> Result<(i64, i64, bool), InspectError> {
    let range = format!("{target}...{branch}");
    let output = match run_git(root, &["rev-list", "--left-right", "--count", &range]) {
        Ok(output) => output,
        Err(InspectError::Io(_)) => return Ok((0, 0, false)),
        Err(other) => return Err(other),
    };
    Ok(parse_distance(&output))
}

/// PURE: git's `--left-right --count` pair, as (ahead, behind, measured).
///
/// Left is what the target has and the branch does not — *behind*. Right is the branch's own —
/// *ahead*. Getting that round the wrong way is the easiest mistake here and the hardest to catch
/// on a screen, since both numbers are usually small and both look plausible either way.
pub fn parse_distance(output: &[u8]) -> (i64, i64, bool) {
    let text = String::from_utf8_lossy(output);
    let mut fields = text.split_whitespace();
    match (
        fields.next().and_then(|value| value.parse::<i64>().ok()),
        fields.next().and_then(|value| value.parse::<i64>().ok()),
    ) {
        (Some(behind), Some(ahead)) => (ahead, behind, true),
        _ => (0, 0, false),
    }
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

/// Repository fixtures shared with the crates above (`collision.rs` uses them).
#[cfg(any(test, feature = "testkit"))]
pub mod test_support {
    use std::path::Path;
    use tempfile::tempdir;

    /// A git that has to succeed.
    ///
    /// Public along with `seeded_repo` below, behind `testkit`, because `collision.rs` in
    /// `nucleos-core` reuses them.
    pub fn git_in_repo(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repository with one commit, and that commit's sha.
    pub fn seeded_repo() -> (tempfile::TempDir, String) {
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
}

#[cfg(test)]
pub mod tests {
    use super::test_support::{git_in_repo, seeded_repo};
    use super::*;
    use tempfile::tempdir;

    /* --------------------------------------------------- the write target -- */

    /// A file that does not exist yet is a legitimate write target, and this is the case
    /// `resolved_within` cannot serve: it canonicalizes the target, and there is nothing to
    /// canonicalize. A directory on the way to a file nobody has written yet is often missing too,
    /// so the missing ancestor is the ordinary case rather than the edge one.
    #[test]
    fn a_file_that_does_not_exist_yet_is_a_target() {
        let root = tempdir().unwrap();
        let target = safe_write_target(root.path(), ".ai/autopilot.yaml")
            .expect("a path under the root is writable whether or not it is there yet");
        assert_eq!(target, root.path().join(".ai").join("autopilot.yaml"));
        assert!(!target.exists(), "the guard must not create anything");
    }

    /// Traversal is refused before anything else looks at the path, and with `UnsafePath` rather
    /// than `NotFound` — the caller turns those into different statuses, and "there is no such
    /// file" would send somebody looking for a file when the answer is that they may not ask.
    #[test]
    fn a_write_cannot_traverse_out_of_the_project() {
        let root = tempdir().unwrap();
        for rel in ["../escape.yaml", ".ai/../../escape.yaml", "/etc/passwd"] {
            assert!(
                matches!(
                    safe_write_target(root.path(), rel),
                    Err(InspectError::UnsafePath)
                ),
                "{rel} must be refused as unsafe"
            );
        }
    }

    /// The root is not a file. `safe_join` answers the root for an empty path — which is right for
    /// `ls` and catastrophic for a write, so the refusal lives here where every writer gets it
    /// rather than in whichever caller remembers.
    #[test]
    fn the_project_root_itself_is_not_a_write_target() {
        let root = tempdir().unwrap();
        for rel in ["", ".", "   "] {
            assert!(
                matches!(
                    safe_write_target(root.path(), rel),
                    Err(InspectError::UnsafePath)
                ),
                "{rel:?} names the root, which is a directory"
            );
        }
    }

    /// A link that leaves the project is refused even though every component of the path is
    /// `Normal` — the lexical filter cannot see it, which is the entire reason this function
    /// resolves anything at all.
    ///
    /// Skipped where the platform will not make the link without privileges: on Windows a symlink
    /// needs Developer Mode or elevation, and a test that failed on an ordinary developer's machine
    /// would be read as a broken repository. The threat it guards is real on both platforms (a
    /// junction needs no elevation at all), so the guard is unconditional and only the proof is
    /// conditional.
    #[test]
    fn a_link_out_of_the_project_is_not_a_write_target() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let link = root.path().join("elsewhere");

        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok();

        if !made {
            return;
        }
        assert!(
            matches!(
                safe_write_target(root.path(), "elsewhere/autopilot.yaml"),
                Err(InspectError::UnsafePath)
            ),
            "a directory link that leaves the project is not inside it"
        );
    }

    /// A project whose recorded root is gone from disk gets `NotFound`, not a write that silently
    /// recreates the folder somebody deleted.
    #[test]
    fn a_root_that_is_not_there_is_not_a_place_to_write() {
        let root = tempdir().unwrap();
        let gone = root.path().join("no-such-project");
        assert!(matches!(
            safe_write_target(&gone, ".ai/autopilot.yaml"),
            Err(InspectError::NotFound)
        ));
    }

    /* ------------------------------------------------ history and branches -- */

    /// The branch the root is standing on, read the way the tests need it rather than through the
    /// private helper under test.
    fn head_branch(repo: &Path) -> String {
        String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["symbolic-ref", "--short", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    fn commit_file(repo: &Path, name: &str, message: &str) {
        std::fs::write(repo.join(name), "x\n").unwrap();
        git_in_repo(repo, &["add", "-A"]);
        git_in_repo(repo, &["commit", "-m", message]);
    }

    #[test]
    fn the_log_reads_newest_first_with_every_field_present() {
        let (repo, _) = seeded_repo();
        commit_file(repo.path(), "second.txt", "the second commit");

        let commits = log(repo.path(), "", 10).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "the second commit");
        assert_eq!(commits[1].subject, "seed");
        assert_eq!(commits[0].author, "test");
        assert_eq!(commits[0].sha.len(), 40);
        assert!(!commits[0].short_sha.is_empty());
        // `%aI` and never `%ad`: the repository can redefine `%ad` in its own config, and a date
        // the shell cannot parse reads on screen as a commit from nowhen.
        assert!(commits[0].at.contains('T'), "not ISO: {}", commits[0].at);
    }

    #[test]
    fn a_path_narrows_the_log_to_the_commits_that_touched_it() {
        let (repo, _) = seeded_repo();
        commit_file(repo.path(), "only.txt", "touched only.txt");

        let commits = log(repo.path(), "only.txt", 10).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "touched only.txt");
    }

    /// The same refusal `cat` gives, for the same reason and through the same function.
    ///
    /// Nothing in `log` opens the path — git takes it as a pathspec — which is exactly why it is
    /// easy to forget that a pathspec climbs out of a repository just as happily as an open does.
    #[test]
    fn the_log_refuses_a_path_that_climbs_out_of_the_repository() {
        let (repo, _) = seeded_repo();
        assert!(matches!(
            log(repo.path(), "../secret", 10),
            Err(InspectError::UnsafePath)
        ));
        assert!(matches!(
            log(repo.path(), "sub/../../secret", 10),
            Err(InspectError::UnsafePath)
        ));
        #[cfg(windows)]
        assert!(matches!(
            log(repo.path(), "C:/Windows", 10),
            Err(InspectError::UnsafePath)
        ));
        #[cfg(unix)]
        assert!(matches!(
            log(repo.path(), "/etc", 10),
            Err(InspectError::UnsafePath)
        ));
    }

    #[test]
    fn the_log_is_bounded_however_much_is_asked_for() {
        let (repo, _) = seeded_repo();
        for index in 0..5 {
            commit_file(
                repo.path(),
                &format!("f{index}.txt"),
                &format!("commit {index}"),
            );
        }

        assert_eq!(log(repo.path(), "", 2).unwrap().len(), 2);
        // Zero is not a request for nothing; it is a request that cannot be honoured, and one
        // commit is the smallest honest answer to it.
        assert_eq!(log(repo.path(), "", 0).unwrap().len(), 1);
        assert_eq!(log(repo.path(), "", 10_000).unwrap().len(), 6);
    }

    #[test]
    fn the_branch_list_measures_every_branch_against_the_one_the_root_is_on() {
        let (repo, _) = seeded_repo();
        let integration = head_branch(repo.path());

        git_in_repo(repo.path(), &["checkout", "-b", "feature"]);
        commit_file(repo.path(), "a.txt", "ahead by one");
        git_in_repo(repo.path(), &["checkout", &integration]);

        let listed = branches(repo.path()).unwrap();
        assert_eq!(listed.integration.as_deref(), Some(integration.as_str()));

        let feature = listed
            .branches
            .iter()
            .find(|row| row.name == "feature")
            .expect("the feature branch should be listed");
        // One commit of its own, none of the target's missing. Reversing these two is the easiest
        // mistake in this file and the hardest to notice on a screen, since both look plausible.
        assert_eq!((feature.ahead, feature.behind), (1, 0));
        assert!(feature.measured);

        // The integration branch is in its own list at zero and zero — a measurement, not an
        // absence, and the two must not render alike.
        let itself = listed
            .branches
            .iter()
            .find(|row| row.name == integration)
            .expect("the integration branch should be listed too");
        assert_eq!((itself.ahead, itself.behind), (0, 0));
        assert!(itself.measured);
    }

    #[test]
    fn a_branch_behind_the_integration_branch_reads_as_behind_and_not_as_ahead() {
        let (repo, _) = seeded_repo();
        git_in_repo(repo.path(), &["branch", "stale"]);
        commit_file(repo.path(), "b.txt", "moved on");

        let listed = branches(repo.path()).unwrap();
        let stale = listed
            .branches
            .iter()
            .find(|row| row.name == "stale")
            .expect("the stale branch should be listed");
        assert_eq!((stale.ahead, stale.behind), (0, 1));
    }

    /// **This test exists because the code was written against a guess and the guess was wrong.**
    ///
    /// The assumption was that git refuses to count across histories with no common ancestor, so an
    /// orphan would come back unmeasured. It does not: `rev-list --left-right --count` exits 0 and
    /// reports both sides in full. `merge-base` is the command that refuses.
    ///
    /// The behaviour is kept rather than corrected, because the number it gives is true and reads
    /// correctly: every commit on both sides is exactly the work merging a grafted branch involves.
    /// What is corrected is the claim in the comment above it.
    #[test]
    fn an_unrelated_history_is_counted_in_full_rather_than_refused() {
        let (repo, _) = seeded_repo();
        let integration = head_branch(repo.path());
        git_in_repo(repo.path(), &["checkout", "--orphan", "grafted"]);
        git_in_repo(repo.path(), &["rm", "-rf", "."]);
        commit_file(repo.path(), "c.txt", "from nowhere");
        git_in_repo(repo.path(), &["checkout", &integration]);

        let listed = branches(repo.path()).unwrap();
        let grafted = listed
            .branches
            .iter()
            .find(|row| row.name == "grafted")
            .expect("the orphan branch should still be listed");
        assert!(grafted.measured);
        // One commit each, sharing nothing: the whole of both histories.
        assert_eq!((grafted.ahead, grafted.behind), (1, 1));
    }

    #[test]
    fn a_detached_root_names_no_integration_branch_instead_of_guessing_one() {
        let (repo, sha) = seeded_repo();
        git_in_repo(repo.path(), &["checkout", "--detach", &sha]);

        let listed = branches(repo.path()).unwrap();
        assert_eq!(listed.integration, None);
        // The branches are still listed — they exist — but nothing claims a distance to a target
        // that is not there.
        assert!(listed.branches.iter().all(|row| !row.measured));
    }

    /// **The case a plain working-tree diff gets wrong, and it is the common one.**
    ///
    /// `job.rs` commits a checkpoint at every green gate, so a run halfway through a job has its
    /// work in commits and a clean working tree. `git diff` shows nothing for it — the reviewer
    /// would be told there is nothing to review at exactly the moment there is most.
    #[test]
    fn a_checkpointed_change_is_in_the_diff_since_the_branch_point() {
        let (repo, base) = seeded_repo();
        std::fs::write(repo.path().join("seed.txt"), "seed\nand more\n").unwrap();
        git_in_repo(repo.path(), &["add", "-A"]);
        git_in_repo(repo.path(), &["commit", "-m", "a checkpoint"]);

        // The working tree is clean, so the old question answers "nothing".
        assert_eq!(diff(repo.path()).unwrap(), "");
        // The right question answers with the work.
        let since = diff_since(repo.path(), &base, "").unwrap();
        assert!(since.contains("and more"), "{since}");
    }

    #[test]
    fn the_diff_since_a_branch_point_can_be_narrowed_to_one_file() {
        let (repo, base) = seeded_repo();
        commit_file(repo.path(), "one.txt", "first");
        commit_file(repo.path(), "two.txt", "second");

        let one = diff_since(repo.path(), &base, "one.txt").unwrap();
        assert!(one.contains("one.txt"));
        assert!(!one.contains("two.txt"), "{one}");
    }

    /// The same refusals `changed_paths` gives, for the same reason: a revision that may begin with
    /// `-` is an option to git, and a pathspec climbs out of a repository as happily as an open.
    #[test]
    fn the_diff_since_refuses_a_base_that_is_not_a_sha_and_a_path_that_climbs_out() {
        let (repo, base) = seeded_repo();
        assert!(matches!(
            diff_since(repo.path(), "--output=/tmp/x", ""),
            Err(InspectError::UnsafePath)
        ));
        assert!(matches!(
            diff_since(repo.path(), &base, "../secret"),
            Err(InspectError::UnsafePath)
        ));
    }

    #[test]
    fn blame_names_who_last_touched_each_line() {
        let (repo, _) = seeded_repo();
        std::fs::write(repo.path().join("f.txt"), "one\ntwo\n").unwrap();
        git_in_repo(repo.path(), &["add", "-A"]);
        git_in_repo(repo.path(), &["commit", "-m", "wrote two lines"]);

        let lines = blame(repo.path(), "f.txt").unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line, 1);
        assert_eq!(lines[1].line, 2);
        assert_eq!(lines[0].text, "one");
        assert_eq!(lines[1].text, "two");
        assert_eq!(lines[0].author, "test");
        assert_eq!(lines[0].summary, "wrote two lines");
        assert!(lines[0].at > 0, "author-time should be an epoch");
        assert!(!lines[0].uncommitted);
    }

    /// Git blames a line nobody has committed to the all-zero sha and fills the rest of the header
    /// with placeholders — "Not Committed Yet" as the author, right now as the time. Left as git
    /// reports them and flagged, because a panel that drew that row like any other would credit a
    /// person who never wrote the line.
    #[test]
    fn an_uncommitted_line_is_flagged_rather_than_credited_to_somebody() {
        let (repo, _) = seeded_repo();
        std::fs::write(repo.path().join("f.txt"), "committed\n").unwrap();
        git_in_repo(repo.path(), &["add", "-A"]);
        git_in_repo(repo.path(), &["commit", "-m", "first"]);
        std::fs::write(repo.path().join("f.txt"), "committed\nnot yet\n").unwrap();

        let lines = blame(repo.path(), "f.txt").unwrap();
        assert_eq!(lines.len(), 2);
        assert!(!lines[0].uncommitted);
        assert!(lines[1].uncommitted, "the new line is in no commit");
    }

    /// The same refusal every other reader gives. A blame opens a file, so it earns the guard that
    /// follows links — not merely the lexical one.
    #[test]
    fn blame_refuses_a_path_outside_the_repository() {
        let (repo, _) = seeded_repo();
        assert!(matches!(
            blame(repo.path(), "../secret"),
            Err(InspectError::UnsafePath)
        ));
    }

    #[test]
    fn blame_is_bounded_however_long_the_file_is() {
        let porcelain = |sha: &str, line: usize| {
            format!(
                "{sha} {line} {line} 1\nauthor test\nauthor-time 1000\nsummary s\n\tline {line}\n"
            )
        };
        let sha = "a".repeat(40);
        let output: String = (1..=10).map(|line| porcelain(&sha, line)).collect();

        assert_eq!(parse_blame(output.as_bytes(), 3).len(), 3);
        assert_eq!(parse_blame(output.as_bytes(), 100).len(), 10);
    }

    /// The header carries two line numbers: where the line was in the commit that introduced it,
    /// and where it is now. Reading the first would number the panel by a file that no longer
    /// exists, and the two agree often enough that the mistake survives a casual test.
    #[test]
    fn parse_blame_numbers_lines_by_where_they_are_now() {
        let sha = "b".repeat(40);
        let output = format!("{sha} 7 42 1\nauthor test\nauthor-time 5\nsummary s\n\tthe line\n");
        let lines = parse_blame(output.as_bytes(), 10);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].line, 42);
    }

    /// A header with no content line behind it is a truncated tail, not a line of the file. Emitting
    /// on the header instead would put a row with no text into the panel.
    #[test]
    fn parse_blame_emits_nothing_for_a_header_with_no_content_line() {
        let sha = "c".repeat(40);
        let output = format!("{sha} 1 1 1\nauthor test\nauthor-time 5\nsummary s\n");
        assert!(parse_blame(output.as_bytes(), 10).is_empty());
    }

    #[test]
    fn changed_reports_the_size_of_the_tree_it_measured_in() {
        let (repo, base) = seeded_repo();
        commit_file(repo.path(), "a.txt", "one more");
        std::fs::write(repo.path().join("b.txt"), "untracked\n").unwrap();

        let changed = changed(repo.path(), &base).unwrap();
        // The committed change and the untracked file: `changed_paths` is the union of both
        // questions, and the Code mode opens on exactly this list.
        assert!(changed.paths.iter().any(|path| path == "a.txt"));
        assert!(changed.paths.iter().any(|path| path == "b.txt"));
        // `seed.txt` and `a.txt` are tracked; `b.txt` is not tracked yet. The count is what makes
        // "and N files nobody touched" possible.
        assert_eq!(changed.tracked, 2);
    }

    #[test]
    fn parse_distance_reads_left_as_behind_and_right_as_ahead() {
        assert_eq!(parse_distance(b"3\t5\n"), (5, 3, true));
        assert_eq!(parse_distance(b"0\t0\n"), (0, 0, true));
        // Anything that is not a pair of numbers is not a distance, and must not become zero.
        assert_eq!(parse_distance(b""), (0, 0, false));
        assert_eq!(parse_distance(b"fatal: no merge base\n"), (0, 0, false));
    }

    #[test]
    fn a_short_record_is_dropped_rather_than_padded_into_a_commit_by_nobody() {
        let good = format!("aaa{FIELD}aa{FIELD}me{FIELD}2026-08-23T00:00:00Z{FIELD}subject");
        let short = format!("bbb{FIELD}bb");
        let parsed = parse_log(format!("{good}\0{short}\0").as_bytes());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].subject, "subject");
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
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        if !made {
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
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&real, &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&real, &link).is_ok();
        if !made {
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

        for path in ["..", "../x", "a/../b", "/etc/passwd"] {
            assert!(matches!(
                safe_join(root, path),
                Err(InspectError::UnsafePath)
            ));
        }

        #[cfg(windows)]
        assert!(matches!(
            safe_join(root, "C:/x"),
            Err(InspectError::UnsafePath)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn safe_join_rejects_an_absolute_posix_path_on_unix() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        for path in ["/root/x", "/x"] {
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
        let pool = crate::testdb::fresh_pool().await;

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

/// What exists in this folder and nowhere else.
///
/// **Written for the one screen that deletes a folder, and shaped by what it has to say there.**
/// A file count would be the obvious thing to show somebody about to destroy a directory, and it is
/// the wrong thing: on a working repository it is dominated by `node_modules` and `target`, so the
/// big frightening number is mostly build output that would be regenerated in a minute. What cannot
/// be regenerated is work git has not been told to keep, and work git has been told to keep that no
/// remote has a copy of. Those two are the whole of what a deletion takes for ever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct OnlyHere {
    /// Files with changes no commit holds — modified, staged or untracked.
    pub uncommitted: i64,
    /// Commits on this repository's own branches that no remote has.
    ///
    /// **`None` is not zero and it is the more serious answer.** A repository with no remote
    /// configured has no elsewhere at all: every commit in it is only here, and reporting that as a
    /// count would be reporting the size of the history rather than the size of the loss. The
    /// screen says which of the two it is in words.
    pub unpushed: Option<i64>,
}

/// Read it, or `None` for a folder that is not a git repository.
///
/// `None` is the case that most deserves saying out loud: a folder git knows nothing about has no
/// commits, no remote and no way for anything in it to exist anywhere else — so the reassurance the
/// other branch can offer does not apply, and a screen that printed `0 uncommitted` over it would
/// be reassuring somebody about the most dangerous case there is.
pub fn only_here(root: &Path) -> Result<Option<OnlyHere>, InspectError> {
    if !root.join(".git").exists() {
        return Ok(None);
    }

    // The same flags `changed_paths` gives this command, and for the same reasons: no optional
    // locks, because an agent may be running git in this tree at the same moment, and no fsmonitor,
    // because that is a command string the target repository's own config chooses.
    let status = run_git(
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
    let uncommitted = parse_status_z(&status).len() as i64;

    // Asked before counting, because the count means something different without it. `--branches
    // --not --remotes` over a repository with no remote is every commit it has ever had, which is a
    // true answer to a question nobody asked.
    let remotes = run_git(root, &["--no-optional-locks", "remote"])?;
    if String::from_utf8_lossy(&remotes).trim().is_empty() {
        return Ok(Some(OnlyHere {
            uncommitted,
            unpushed: None,
        }));
    }

    let counted = run_git(
        root,
        &[
            "--no-optional-locks",
            "rev-list",
            "--count",
            "--branches",
            "--not",
            "--remotes",
        ],
    )?;
    let unpushed = String::from_utf8_lossy(&counted)
        .trim()
        .parse()
        .unwrap_or(0);
    Ok(Some(OnlyHere {
        uncommitted,
        unpushed: Some(unpushed),
    }))
}
