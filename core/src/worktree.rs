use crate::feed;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const GC_BACKOFF: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(25),
];

/// Bounds the amount of unseen work autonomous cleanup will commit at once.
/// 256 MiB accommodates normal source trees and generated assets without silently preserving an
/// unexpectedly large build output; larger worktrees are left in place for explicit inspection.
const DEFAULT_PRESERVATION_BYTE_CEILING: u64 = 256 * 1024 * 1024;

pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    /// The commit this tree was born on.
    ///
    /// `Option`, and a failure to read it does **not** fail provisioning: the worktree exists and
    /// the work can carry on. What is lost is the base collision measures against, and the reader
    /// sees `not measured` instead of a `clean` nobody computed.
    pub base_sha: Option<String>,
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

/// Every git invocation in this module goes through here.
///
/// `core.fsmonitor` is a COMMAND STRING read from the repository git is pointed at, and these
/// commands run inside project roots the daemon does not control — so a target repository can name
/// a program that the daemon then executes as itself. Measured on git 2.50.1: `worktree add` runs
/// it exactly once; `worktree prune`, `worktree remove` and `branch -d` do not, today. The flag
/// goes on all of them anyway, because which commands refresh the index is a git implementation
/// detail a later version may widen, and a guarantee that has to be re-derived per command is one
/// that quietly stops holding.
///
/// The diff-side command strings `inspect.rs` disables (`diff.external`, a `textconv` filter) are
/// deliberately absent: nothing here produces a diff, and carrying flags that cannot apply would
/// advertise a protection that was never at issue in this module.
pub(crate) fn git() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(git_bin());
    command.arg("-c").arg("core.fsmonitor=");
    command
}

/// Who a worktree belongs to.
///
/// A run owns its own tree; a job owns one that outlives each of its nodes, so several runs in
/// sequence can build on what the previous one left. Modelled as an enum rather than a `&str` so a
/// caller cannot invent a third kind that the migration's CHECK constraint would then reject at
/// runtime, in a write nobody is watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Run(i64),
    Job(i64),
}

impl Owner {
    pub fn kind(self) -> &'static str {
        match self {
            Owner::Run(_) => "run",
            Owner::Job(_) => "job",
        }
    }

    pub fn id(self) -> i64 {
        match self {
            Owner::Run(id) | Owner::Job(id) => id,
        }
    }

    /// The directory name for this owner's worktree.
    ///
    /// For a run this is byte-identical to what `create` produced before this type existed, which is
    /// the point: no worktree already on disk becomes unrecognisable to `orphaned_worktrees`, which
    /// would leave it uncollectable forever.
    pub fn dir_name(self) -> String {
        format!("{}-{}", self.kind(), self.id())
    }

    /// The run a feed row about this worktree should be attributed to, if any.
    ///
    /// A job-owned worktree has no single run to blame, and writing a job id into `feed.run_id`
    /// would mislabel it as one — the ids come from different sequences and would collide silently.
    pub fn feed_run_id(self) -> Option<i64> {
        match self {
            Owner::Run(id) => Some(id),
            Owner::Job(_) => None,
        }
    }

    /// The branch `create` opens for this owner's worktree.
    pub fn branch_name(self) -> String {
        format!("{BRANCH_PREFIX}{}", self.dir_name())
    }
}

/// The prefix every worktree branch this daemon creates carries. One constant because two callers
/// read the name apart: `create_at` writes it, and `run_behind_branch` takes it back off.
const BRANCH_PREFIX: &str = "nucleos/";

/// The run a worktree branch was opened for, if this daemon opened it.
///
/// **The branch name outlives the row that records who owns the tree, and that is why this exists
/// rather than a join.** A run that pauses for approval and resumes hands its worktree to a NEW run
/// id, and the `worktrees` row is rewritten to the successor — so anything that identified the tree
/// by its current owner loses the link to the run that created it. The branch is opened once, is
/// never renamed, and names that run for as long as the branch is around. Measured the hard way: a
/// conflict resolution whose inspection command was held for approval landed unmarked, so nothing
/// verified it before it was published.
///
/// A name an agent could write by hand is accepted, and the direction that fails in is the safe one:
/// claiming to be a resolution buys stricter checking, never less.
pub fn run_behind_branch(branch: &str) -> Option<i64> {
    branch
        .strip_prefix(BRANCH_PREFIX)?
        .strip_prefix("run-")?
        .parse()
        .ok()
}

pub async fn create(project_root: &Path, owner: Owner) -> io::Result<WorktreeInfo> {
    create_at(project_root, owner, None).await
}

/// `create`, on a named starting point instead of wherever the project's checkout happens to stand.
///
/// **One caller needs this and the reason is not convenience.** A conflict resolver's tree has to be
/// born on the merge's TARGET, and two things follow from that which follow from nothing else: the
/// conflict staged in it is exactly the one the queue met — a tree born on some other commit would
/// present a different conflict, or none — and the branch the resolver produces has the target's tip
/// as an ancestor, so landing it does not reopen the question it was made to settle.
///
/// `None` keeps git's own default, which is the project checkout's HEAD, and that is what every
/// other run wants: work starts from where the project is.
pub async fn create_at(
    project_root: &Path,
    owner: Owner,
    base: Option<&str>,
) -> io::Result<WorktreeInfo> {
    let root = worktree_root(project_root);
    if root.to_string_lossy().contains(' ') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree root path contains a space — set NUCLEOS_WORKTREE_ROOT to a space-free path (cargo builds fail to link under spaced paths)",
        ));
    }

    // A relative root is resolved twice, against two different directories: `git -C project_root`
    // places the worktree under the project, while every later `tokio::fs` call on the stored path
    // resolves against the daemon's own working directory. Removal refuses a path it cannot place,
    // so allowing one here would create a worktree that can never be cleaned up.
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "worktree root path is relative ({}) — set NUCLEOS_WORKTREE_ROOT to an absolute path",
                root.display()
            ),
        ));
    }

    let name = owner.dir_name();
    let branch = owner.branch_name();
    let path = root.join(&name);
    tokio::fs::create_dir_all(&root).await?;

    let mut command = git();
    command
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch)
        .arg(&path);
    // Last, because that is where `worktree add` takes its commit-ish, and only when one was asked
    // for — an empty argument here is not "the default", it is a ref that does not resolve.
    if let Some(base) = base {
        command.arg(base);
    }
    let output = command.output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "git worktree add failed: {stderr}"
        )));
    }

    // Asked here rather than passed in from outside: this is the only place that knows where the
    // tree branched from. Handing the sha to `record` as a parameter would make both callers find
    // it their own way — and for jobs one already exists (`create_job` computes
    // `current_branch_sha`) while for runs there is nothing, which would bring back at the moment
    // of writing exactly the asymmetry the base exists to remove.
    //
    // `.ok()` and not `?`: the worktree exists and the work can carry on without a base. What is
    // lost is the yardstick collision measures against, and the reader sees `not measured` rather
    // than a `clean` nobody computed.
    let base_sha = head_sha(&path).await.ok();
    Ok(WorktreeInfo {
        path,
        branch,
        base_sha,
    })
}

/// Leaves `source` merged half-way into this worktree: markers in the files, MERGE_HEAD set.
///
/// **This is the inversion the resolver design turns on: the daemon stages the conflict, the agent
/// only resolves it.** The obvious shape — hand the agent the two branches and let it merge — cannot
/// work here, because any `git merge` an agent runs goes to the queue, and the queue is what just
/// refused that merge for conflicting. It would circle, and no wording in a prompt gets it out,
/// because the refusal is structural and correct. With the merge already staged, the agent does only
/// what any agent does: edits files and commits. No exception to the gate, and therefore no exception
/// for anybody to abuse.
///
/// It also buys the resolution's two-parent tip for free — a plain `git commit` on top of a staged
/// merge carries both parents, so the resolver cannot flatten the merge by accident, and
/// `git_exec::verify_resolution` refuses the deliberate ones.
///
/// **A clean merge is an error here, not a success.** It means the conflict was gone by the time the
/// resolver looked — other work landed in between — and there is nothing for an agent to resolve.
/// Launching one anyway would spend a session to produce a merge the queue can compute by itself.
///
/// Conflicted is told apart from broken by `ls-files --unmerged` rather than by the exit code, which
/// is 1 for both. A merge refused before it started — an unknown ref, a dirty tree — leaves no
/// unmerged paths, and staging nothing while reporting a staged conflict would put an agent in a
/// worktree with no work in it and no way to tell.
pub async fn stage_conflict(worktree: &Path, source: &str) -> io::Result<()> {
    let merged = git()
        .arg("-C")
        .arg(worktree)
        .arg("merge")
        // Never an editor: this runs with no terminal, and on the clean-merge path git would
        // otherwise wait for one that is never coming.
        .arg("--no-edit")
        .arg(source)
        .output()
        .await?;
    if merged.status.success() {
        return Err(io::Error::other(format!(
            "{source} merged cleanly, so there is no conflict left to resolve — \
             the repository moved between the escalation and now"
        )));
    }

    let unmerged = git()
        .arg("-C")
        .arg(worktree)
        .arg("ls-files")
        .arg("--unmerged")
        .output()
        .await?;
    if !unmerged.status.success() || unmerged.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&merged.stderr);
        return Err(io::Error::other(format!(
            "merging {source} left no conflicted paths to resolve: {stderr}"
        )));
    }
    Ok(())
}

/// Where a job's nodes hand work to each other, relative to the worktree they share.
///
/// Inside the worktree, not beside it. `classifier.rs` denies any write outside the run's cwd — the
/// class is `outside-workspace`, it is the classifier's first rule, and it is a hard `deny` with no
/// single-use grant to fall back on — and a node's cwd *is* the worktree. A sibling directory would
/// make the plan node's only output unwritable, so every job would fail at its first node, always.
pub const ARTIFACTS_DIR: &str = ".nucleos";

/// Creates a job's artifacts directory and hides it from git, returning its absolute path.
///
/// The exclusion goes in `info/exclude`, not the project's `.gitignore`: that file belongs to the
/// user and the entry would turn up in their diff.
///
/// Excluded rather than merely untracked, because `preserve_uncommitted` runs `git add -A`. Left
/// visible, the handoff files would ride into the preservation commit — and count against
/// `DEFAULT_PRESERVATION_BYTE_CEILING`, where going over does not truncate the commit, it blocks
/// removal of the worktree. A node that commits its own work with `git add -A` would sweep them up
/// the same way.
///
/// It lands in the repository's **common** git directory, which is the one place git reads
/// `info/exclude` from. A linked worktree has a private git directory of its own, and writing there
/// looks right and excludes nothing — `info/` is on git's common list, so the per-worktree copy is
/// never consulted. The design called for "the worktree's `info/exclude`"; this is where that file
/// actually is. The cost is that the entry is visible to every worktree of the repository including
/// the user's own checkout, and it is accepted: `.nucleos` is this daemon's own directory name, the
/// file is per-clone and unversioned, and the alternative is a node committing handoff files onto
/// the user's branch.
///
/// The entry is anchored (`/.nucleos/`) so a directory of the same name deeper in the tree keeps
/// showing up in the user's status.
pub async fn prepare_artifacts(worktree: &Path) -> io::Result<PathBuf> {
    let artifacts = worktree.join(ARTIFACTS_DIR);
    tokio::fs::create_dir_all(&artifacts).await?;

    // Asked of git rather than assumed: in a linked worktree `.git` is a file, not a directory, so
    // `<worktree>/.git/info/exclude` cannot be written to at all.
    let output = git()
        .arg("-C")
        .arg(worktree)
        .arg("rev-parse")
        .arg("--git-common-dir")
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "could not resolve the repository's common git directory: {stderr}"
        )));
    }
    // Older git answers relatively, and relative to the `-C` directory above.
    let reported = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let git_dir = if reported.is_absolute() {
        reported
    } else {
        worktree.join(reported)
    };

    let info = git_dir.join("info");
    tokio::fs::create_dir_all(&info).await?;
    let exclude = info.join("exclude");
    let entry = format!("/{ARTIFACTS_DIR}/");
    let mut contents = tokio::fs::read_to_string(&exclude)
        .await
        .unwrap_or_default();
    if !contents.lines().any(|line| line.trim() == entry) {
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(&entry);
        contents.push('\n');
        tokio::fs::write(&exclude, contents).await?;
    }

    Ok(artifacts)
}

pub(crate) async fn try_remove_once(project_root: &Path, path: &Path) -> io::Result<()> {
    let output = git()
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
        return Err(io::Error::other(format!(
            "git worktree remove failed: {stderr}"
        )));
    }

    Ok(())
}

fn component_eq(left: &std::path::Component<'_>, right: &std::path::Component<'_>) -> bool {
    if cfg!(windows) {
        left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
    } else {
        left == right
    }
}

/// Component-wise path equality.
///
/// String comparison gets this wrong in both directions here: git prints `C:/x/y` where we hold
/// `C:\x\y`, and Windows paths differ in case without differing. Components normalise the
/// separator; `component_eq` handles the case rule.
fn paths_equal(left: &Path, right: &Path) -> bool {
    let left: Vec<_> = left.components().collect();
    let right: Vec<_> = right.components().collect();
    left.len() == right.len() && left.iter().zip(&right).all(|(a, b)| component_eq(a, b))
}

/// True when `inner` is `outer` or sits beneath it.
///
/// Compared by component so a sibling named `repo-backup` is not mistaken for a child of `repo`,
/// which a string prefix would do.
fn path_contains(outer: &Path, inner: &Path) -> bool {
    let outer: Vec<_> = outer.components().collect();
    let inner: Vec<_> = inner.components().collect();
    if outer.is_empty() || inner.len() < outer.len() {
        return false;
    }
    outer.iter().zip(&inner).all(|(a, b)| component_eq(a, b))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
}

/// Paths a removal must refuse, whatever the caller believes about them.
///
/// The orphan sweep already demands a `run-<id>` name under `worktree_root`, but that root comes
/// from `NUCLEOS_WORKTREE_ROOT` — an environment variable — so the tree being walked is settable
/// from outside the daemon. This is the backstop for that: a judgement about the path itself,
/// independent of how it was reached.
fn is_dangerous_removal_path(path: &Path, project_root: &Path) -> bool {
    if path.to_string_lossy().trim().is_empty() {
        return true;
    }
    // Every worktree this module creates is absolute. A relative one means we have lost track of
    // what it is relative to, and would resolve against the daemon's working directory.
    if !path.is_absolute() || path.parent().is_none() {
        return true;
    }
    // Catches the path being the project itself, and the path being an ancestor of it.
    if path_contains(path, project_root) {
        return true;
    }
    if home_dir().is_some_and(|home| path_contains(path, &home)) {
        return true;
    }
    // Recursing into a symlink would follow it straight out of the worktree tree.
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// What git's own bookkeeping says about a worktree path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registration {
    Absent,
    Present,
}

/// Asks git whether it still holds a row for `path`.
///
/// This exists so the filesystem fallback never classifies a failure by reading its message.
/// `git worktree remove`'s stderr is localised and version-dependent, and the distinction that
/// actually matters — did git refuse, or did it start and not finish — lives in its worktree list,
/// not in its prose. A failure to ask is an error and never `Absent`: "we could not find out" must
/// not be allowed to read as "git has nothing".
async fn registration(project_root: &Path, path: &Path) -> io::Result<Registration> {
    let output = git()
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("list")
        .arg("--porcelain")
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "git worktree list failed: {stderr}"
        )));
    }

    let mut unreadable = false;
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(listed) = line.strip_prefix(b"worktree ") else {
            continue;
        };
        match path_from_git_bytes(listed) {
            Ok(listed) if paths_equal(&listed, path) => return Ok(Registration::Present),
            Ok(_) => {}
            // A path elsewhere in the list that cannot be read says nothing about this one, so keep
            // looking — but remember it, because it could have been the match we were after.
            Err(_) => unreadable = true,
        }
    }

    if unreadable {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "git listed a worktree path that could not be read, so absence cannot be proven",
        ));
    }
    Ok(Registration::Absent)
}

/// Clears git's row for a directory that is already gone, and proves it cleared.
///
/// Order is the whole point. `prune` only drops rows whose directory is missing, so running it
/// while the directory still exists — which is what `remove` does — accomplishes nothing. The row
/// then outlives the directory, and since `orphaned_worktrees` only ever sees directories, nothing
/// lists it again and nothing prunes it: it leaks for the life of the repository.
async fn prune_and_verify_registration_gone(project_root: &Path, path: &Path) -> io::Result<()> {
    let output = git()
        .arg("-C")
        .arg(project_root)
        .arg("worktree")
        .arg("prune")
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "git worktree prune failed: {stderr}"
        )));
    }

    match registration(project_root, path).await? {
        Registration::Absent => Ok(()),
        Registration::Present => Err(io::Error::other(format!(
            "git still registers {} after its directory was removed",
            path.display()
        ))),
    }
}

#[derive(Debug)]
struct PreservationFailure(io::Error);

impl std::fmt::Display for PreservationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "failed to preserve uncommitted work: {}", self.0)
    }
}

impl std::error::Error for PreservationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn preservation_failure(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), PreservationFailure(error))
}

fn is_preservation_failure(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<PreservationFailure>())
        .is_some()
}

async fn directory_is_empty(path: &Path) -> io::Result<bool> {
    let mut entries = tokio::fs::read_dir(path).await?;
    Ok(entries.next_entry().await?.is_none())
}

fn path_from_git_bytes(path: &[u8]) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
    }

    #[cfg(not(unix))]
    {
        String::from_utf8(path.to_vec())
            .map(PathBuf::from)
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("git returned a non-UTF-8 worktree path: {error}"),
                )
            })
    }
}

async fn measure_status_paths(
    worktree_path: &Path,
    status: &[u8],
    byte_ceiling: u64,
) -> io::Result<()> {
    let mut cursor = 0;
    let mut measured = 0_u64;

    while cursor < status.len() {
        if status.len() - cursor < 4 || status[cursor + 2] != b' ' {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "git status returned malformed porcelain output",
            ));
        }

        let x = status[cursor];
        let y = status[cursor + 1];
        let path_start = cursor + 3;
        let path_end = status[path_start..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| path_start + offset)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "git status returned an unterminated path",
                )
            })?;
        let relative_path = path_from_git_bytes(&status[path_start..path_end])?;
        cursor = path_end + 1;

        // Porcelain v1 `-z` emits a second NUL-terminated source path for renames and copies. The
        // first path is the destination whose current contents `git add -A` would stage.
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            let source_end = status[cursor..]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| cursor + offset)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "git status returned an unterminated rename source",
                    )
                })?;
            cursor = source_end + 1;
        }

        let bytes = match tokio::fs::symlink_metadata(worktree_path.join(relative_path)).await {
            Ok(metadata) if metadata.file_type().is_dir() => 0,
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        measured = measured.checked_add(bytes).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "uncommitted work exceeds the preservation byte ceiling",
            )
        })?;
        if measured > byte_ceiling {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "uncommitted work is {measured} bytes, above the {byte_ceiling}-byte preservation ceiling"
                ),
            ));
        }
    }

    Ok(())
}

/// Whether `worktree_path` is the root of a worktree, rather than an ordinary directory that merely
/// happens to sit inside somebody's repository.
///
/// `git status`, `add` and `commit` all discover the *enclosing* repository when handed a plain
/// directory. A stray `run-<id>` under a worktree root that itself lives inside a repo — nested
/// projects, or a `NUCLEOS_WORKTREE_ROOT` pointed somewhere inside one — would therefore make
/// preservation stage and commit that repository's unrelated work under the daemon's name.
///
/// Answers `false` only on positive proof: a toplevel that exists and sits strictly above this
/// path. Every uncertainty — git not run, path unreadable, forms that will not compare — answers
/// `true`, because the cost of a wrong `false` is deleting a real worktree without preserving it,
/// while the cost of a wrong `true` is the status quo.
async fn is_worktree_root(worktree_path: &Path) -> bool {
    let Ok(output) = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .await
    else {
        return true;
    };
    // Not inside a repository at all: there is no enclosing work to commit by mistake, and the
    // ordinary `git status` failure below already handles it.
    if !output.status.success() {
        return true;
    }
    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if toplevel.is_empty() {
        return true;
    }

    // Canonicalise both sides before comparing. Windows hands out 8.3 short names (`PC MULTI~1`)
    // where git prints the long form, and a spurious mismatch here would read as "not a worktree".
    let (Ok(toplevel), Ok(here)) = (
        std::fs::canonicalize(&toplevel),
        std::fs::canonicalize(worktree_path),
    ) else {
        return true;
    };

    !(path_contains(&toplevel, &here) && !paths_equal(&toplevel, &here))
}

pub(crate) async fn preserve_uncommitted(
    worktree_path: &Path,
    byte_ceiling: u64,
) -> io::Result<bool> {
    if !is_worktree_root(worktree_path).await {
        // A plain directory inside someone else's repository. Nothing here belongs to a run, and
        // staging it would commit that repository's work — so preserve nothing and let the ordinary
        // removal failure carry it to the filesystem fallback, which proves provenance separately.
        return Ok(false);
    }

    let status = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("status")
        .arg("--porcelain=v1")
        .arg("-z")
        .arg("--untracked-files=all")
        .output()
        .await?;
    if !status.status.success() {
        // Startup may find an empty `run-*` directory that git never registered. It contains
        // nothing to preserve, so let the ordinary removal failure reach the filesystem fallback.
        if directory_is_empty(worktree_path).await.unwrap_or(false) {
            return Ok(false);
        }
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(io::Error::other(format!("git status failed: {stderr}")));
    }
    if status.stdout.is_empty() {
        return Ok(false);
    }

    measure_status_paths(worktree_path, &status.stdout, byte_ceiling).await?;

    let add = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("add")
        .arg("-A")
        .output()
        .await?;
    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        return Err(io::Error::other(format!("git add failed: {stderr}")));
    }

    let commit = git()
        .arg("-c")
        .arg("user.name=nucleos")
        .arg("-c")
        .arg("user.email=nucleos@localhost")
        .arg("-C")
        .arg(worktree_path)
        .arg("commit")
        .arg("--no-verify")
        .arg("-m")
        .arg("Preserve uncommitted work before worktree removal")
        .output()
        .await?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        return Err(io::Error::other(format!("git commit failed: {stderr}")));
    }

    Ok(true)
}

/// Commits whatever a job's item left behind, and answers with the commit it can be rolled back to.
///
/// The footing a job stands on. Until this existed there was no point to revert TO: an item that
/// broke the tree left it broken, and the next item built on the wreckage — visible in the job that
/// ran on 2026-08-07, where a red gate simply ended the night and left the partial on the branch.
///
/// Shares `preserve_uncommitted`'s road deliberately — the same [`git()`] wrapper carrying
/// `-c core.fsmonitor=`, the same `add -A`, the same identity, the same `--no-verify` — and shares
/// none of its body. They answer different questions and one of them is about to change: this runs
/// on the happy path, per green gate, dozens of times a night.
///
/// **A clean tree is not an error, and does not get a commit.** An item that measured green without
/// writing anything is already standing on `HEAD`, and `--allow-empty` would mint a distinct SHA per
/// item that distinguishes nothing — a history where every revert target looks different and none of
/// them mean anything.
///
/// **No byte ceiling, where `preserve_uncommitted` has one, and the asymmetry is the point.** That
/// function runs while a worktree is being destroyed, so refusing to preserve 4 GB costs the work
/// and saves the disk. Here, refusing costs the NEXT item its footing, which is the exact state this
/// whole change exists to abolish. If a ceiling is ever wanted here it has to come with an answer to
/// "and then what does the next item revert to", and today there is none.
pub(crate) async fn checkpoint(worktree_path: &Path) -> io::Result<String> {
    if !is_worktree_root(worktree_path).await {
        // Same door `preserve_uncommitted` keeps shut: a plain directory inside somebody else's
        // repository would have its `add -A` land on THAT repository's work.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to checkpoint a path that is not a worktree root: {}",
                worktree_path.display()
            ),
        ));
    }

    let status = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("status")
        .arg("--porcelain=v1")
        .arg("-z")
        .arg("--untracked-files=all")
        .output()
        .await?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(io::Error::other(format!("git status failed: {stderr}")));
    }

    if !status.stdout.is_empty() {
        let add = git()
            .arg("-C")
            .arg(worktree_path)
            .arg("add")
            .arg("-A")
            .output()
            .await?;
        if !add.status.success() {
            let stderr = String::from_utf8_lossy(&add.stderr);
            return Err(io::Error::other(format!("git add failed: {stderr}")));
        }

        let commit = git()
            .arg("-c")
            .arg("user.name=nucleos")
            .arg("-c")
            .arg("user.email=nucleos@localhost")
            .arg("-C")
            .arg(worktree_path)
            .arg("commit")
            .arg("--no-verify")
            .arg("-m")
            .arg("nucleos: checkpoint a gate agreed with")
            .output()
            .await?;
        if !commit.status.success() {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            return Err(io::Error::other(format!("git commit failed: {stderr}")));
        }
    }

    head_sha(worktree_path).await
}

/// Puts the tree back where `sha` left it, and takes the untracked files with it.
///
/// **`clean` is not tidiness, it is half the revert.** `reset --hard` restores tracked files and has
/// nothing to say about a file the item CREATED — so without it the next item starts on top of the
/// previous one's new modules, its scratch scripts and its `__pycache__`, in a tree that looks
/// reverted. That is the defect this change is here to remove, wearing a disguise.
///
/// `-fd` and deliberately not `-fdx`: `-x` reaches what `.gitignore` covers, which in a job's
/// worktree is `target/`. Throwing away a whole build per skipped item is a cost nobody would
/// connect back to the commit that introduced it.
///
/// `sha` NEVER comes from a request. Every caller passes `job_items.checkpoint_sha` or
/// `jobs.head_sha`, both written by this daemon. A `reset --hard` taking a caller-supplied ref is an
/// arbitrary-write primitive pointed at the user's own repository.
pub(crate) async fn revert_to(worktree_path: &Path, sha: &str) -> io::Result<()> {
    let reset = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("reset")
        .arg("--hard")
        .arg(sha)
        .output()
        .await?;
    if !reset.status.success() {
        let stderr = String::from_utf8_lossy(&reset.stderr);
        return Err(io::Error::other(format!("git reset failed: {stderr}")));
    }

    let clean = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("clean")
        .arg("-fd")
        .output()
        .await?;
    if !clean.status.success() {
        let stderr = String::from_utf8_lossy(&clean.stderr);
        return Err(io::Error::other(format!("git clean failed: {stderr}")));
    }
    Ok(())
}

async fn head_sha(worktree_path: &Path) -> io::Result<String> {
    let output = git()
        .arg("-C")
        .arg(worktree_path)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!("git rev-parse failed: {stderr}")));
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if sha.is_empty() {
        return Err(io::Error::other("git rev-parse printed no revision"));
    }
    Ok(sha)
}

pub async fn remove(project_root: &Path, path: &Path, backoff: &[Duration]) -> io::Result<()> {
    if is_dangerous_removal_path(path, project_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to remove a worktree at a dangerous path: {}",
                path.display()
            ),
        ));
    }
    preserve_uncommitted(path, DEFAULT_PRESERVATION_BYTE_CEILING)
        .await
        .map_err(preservation_failure)?;
    let result = retry_with_backoff(backoff, || try_remove_once(project_root, path)).await;
    let _ = git()
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
    pub owner_kind: String,
    pub owner_id: i64,
    pub project_id: String,
    pub project_root: String,
    pub path: String,
    pub branch: String,
}

impl WorktreeRow {
    /// The typed owner behind the two stored columns.
    ///
    /// `None` for a kind the CHECK constraint should have made impossible. Callers treat that as
    /// "not mine to touch" rather than guessing — the operations downstream of this delete
    /// directories, so an unrecognised row is left alone instead of being collected on a guess.
    pub fn owner(&self) -> Option<Owner> {
        match self.owner_kind.as_str() {
            "run" => Some(Owner::Run(self.owner_id)),
            "job" => Some(Owner::Job(self.owner_id)),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    Released,
    NotAwaitingApproval,
    NotFound,
}

pub async fn record(
    pool: &SqlitePool,
    owner: Owner,
    project_id: &str,
    project_root: &str,
    path: &str,
    branch: &str,
    base_sha: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO worktrees
         (owner_kind, owner_id, project_id, project_root, path, branch, base_sha,
          created_at, removed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(owner.kind())
    .bind(owner.id())
    .bind(project_id)
    .bind(project_root)
    .bind(path)
    .bind(branch)
    .bind(base_sha)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_removed(pool: &SqlitePool, owner: Owner) -> sqlx::Result<()> {
    sqlx::query("UPDATE worktrees SET removed_at = ? WHERE owner_kind = ? AND owner_id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(owner.kind())
        .bind(owner.id())
        .execute(pool)
        .await?;
    // The collision measurement describes a tree that is no longer on disk. Best-effort: a row left
    // behind is one warning too many, and failing the worktree's removal on account of it would
    // trade a small problem for a large one.
    if let Err(error) = crate::collision::forget(pool, owner.kind(), owner.id()).await {
        tracing::warn!(?owner, %error, "could not clear the worktree's collision measurement");
    }
    Ok(())
}

pub async fn release(pool: &SqlitePool, run_id: i64) -> sqlx::Result<ReleaseOutcome> {
    // Claim the run first, with the check inside the write. Read-then-write left a gap that this
    // call then spent tens of seconds inside — `git worktree remove` retries on a backoff — and
    // anything finalising the run in that gap (a cancel, or an approval that supersedes it and hands
    // its worktree to a resume) would find `cancelled` stamped over its status and its worktree
    // deleted. Claiming before touching anything means only the winner does the destructive part.
    //
    // The order also fails in the better direction: a crash between the claim and the removal leaves
    // a `cancelled` run with a live worktree row, which the GC collects; the old order could leave a
    // removed worktree pinned by an `awaiting_approval` run, which the GC never touches.
    let claimed = sqlx::query(
        "UPDATE runs SET status = 'cancelled', completed_at = ?
         WHERE id = ? AND status = 'awaiting_approval'",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(pool)
    .await?;
    if claimed.rows_affected() == 0 {
        // Losing the claim is not an error; it only decides which of the two honest answers this is.
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
        return Ok(match exists {
            Some(_) => ReleaseOutcome::NotAwaitingApproval,
            None => ReleaseOutcome::NotFound,
        });
    }

    // Release stays a *run* concept: it un-pins a run paused for approval, and a run still owns its
    // own worktree. The `owner_kind` filter keeps it from ever matching a job whose id collides.
    let worktree: Option<WorktreeRow> = sqlx::query_as(
        "SELECT owner_kind, owner_id, project_id, project_root, path, branch
         FROM worktrees
         WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
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
            if is_preservation_failure(&error) {
                tracing::warn!(
                    run_id,
                    %error,
                    "failed to preserve released worktree; deferring removal"
                );
                return Ok(ReleaseOutcome::Released);
            }
            tracing::warn!(
                run_id,
                %error,
                "failed to remove released worktree; continuing with discard"
            );
        }
        mark_removed(pool, Owner::Run(run_id)).await?;
    }

    if let Some(worktree) = worktree {
        // A discarded run's branch was left behind until now, so every rejected proposal leaked one.
        // `-d` keeps anything with unmerged commits, so this only collects the branches that carry
        // no work — which is exactly the case for a run blocked at its first high-risk action.
        let branch_deleted =
            delete_branch_if_merged(&worktree.project_root, &worktree.branch).await;
        let summary = if branch_deleted {
            format!(
                "released worktree {} + branch {}",
                worktree.path, worktree.branch
            )
        } else {
            format!("released worktree {}", worktree.path)
        };
        let _ = feed::append(
            pool,
            Some(&worktree.project_id),
            "worktree_released",
            &summary,
            Some(run_id),
        )
        .await;
        feed_branch_outcome(pool, &worktree, branch_deleted).await;
    }

    Ok(ReleaseOutcome::Released)
}

/// Best-effort `git branch -d`, reporting whether the branch went away.
///
/// Cleanup first commits every non-ignored, uncommitted path that fits the preservation ceiling;
/// overflow or any preservation failure leaves the worktree in place. Always `-d`, never `-D`, then
/// keeps that preservation commit (and any other unmerged work) on its branch. Together those two
/// refusals ensure autonomous cleanup cannot destroy work the human has not seen. A discarded run
/// with no commits remains the common branch this safely collects.
async fn delete_branch_if_merged(project_root: &str, branch: &str) -> bool {
    git()
        .arg("-C")
        .arg(project_root)
        .arg("branch")
        .arg("-d")
        .arg(branch)
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

/// Records what became of a branch after its worktree was collected. Kept branches are announced
/// too — an unmerged branch left behind is a thing the human may want to look at, not a silent leak.
async fn feed_branch_outcome(pool: &SqlitePool, worktree: &WorktreeRow, deleted: bool) {
    if deleted {
        return;
    }
    let summary = format!("kept unmerged branch {}", worktree.branch);
    let _ = feed::append(
        pool,
        Some(&worktree.project_id),
        "worktree_branch_kept",
        &summary,
        worktree.owner().and_then(Owner::feed_run_id),
    )
    .await;
}

// Two arms rather than one join, because a worktree's owner decides which table says whether it is
// finished. `owner_kind` is load-bearing in both: the joins match on the bare id, and run ids and
// job ids come from different sequences, so without the filters a job whose id happened to equal a
// terminal run's would have its worktree collected mid-use. There is a test on exactly that
// (`a_job_worktree_is_not_collected_by_a_run_of_the_same_id`).
//
// Both arms list terminal statuses explicitly rather than excluding live ones. A status added later
// then defaults to *not collected* — a stale directory — instead of to deleting the worktree of
// something still using it. That is the safe direction and still a leak, so each arm is held
// against the module that writes those statuses:
// `job::every_ending_a_job_can_have_is_an_ending_the_gc_collects` for the job arm, and
// `runs::every_ending_a_run_can_have_is_an_ending_the_gc_collects` for the run arm. The run arm had
// no such guard until `superseded` was found missing from it.
pub(crate) const GC_CANDIDATES_SQL: &str =
    "SELECT w.owner_kind, w.owner_id, w.project_id, w.project_root, w.path, w.branch
         FROM worktrees w
         JOIN runs r ON r.id = w.owner_id
         WHERE w.owner_kind = 'run'
           AND w.removed_at IS NULL
           AND r.status IN ('completed','failed','cancelled','timed_out','interrupted','superseded')
           AND COALESCE(r.completed_at, w.created_at) <= ?
         UNION ALL
         SELECT w.owner_kind, w.owner_id, w.project_id, w.project_root, w.path, w.branch
         FROM worktrees w
         JOIN jobs j ON j.id = w.owner_id
         WHERE w.owner_kind = 'job'
           AND w.removed_at IS NULL
           AND j.status IN ('completed','failed','gate_failed','gate_errored','expired','stopped',
                            'cancelled','interrupted')
           AND COALESCE(j.completed_at, w.created_at) <= ?
         ORDER BY 1, 2";

pub async fn gc_candidates(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    retention: chrono::Duration,
) -> sqlx::Result<Vec<WorktreeRow>> {
    let cutoff = (now - retention).to_rfc3339();
    sqlx::query_as(GC_CANDIDATES_SQL)
        .bind(&cutoff)
        .bind(&cutoff)
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
        let _ = git()
            .arg("-C")
            .arg(project_root)
            .arg("worktree")
            .arg("prune")
            .output()
            .await;
    }

    for worktree in candidates {
        // A row whose kind this build does not recognise is not ours to delete. `gc_candidates`
        // filters to `owner_kind = 'run'`, so this cannot fire today; it exists so that a future
        // kind added to the CHECK constraint but not to `Owner` skips collection rather than
        // falling through to a branch that removes directories.
        let Some(owner) = worktree.owner() else {
            tracing::warn!(
                owner_kind = %worktree.owner_kind,
                owner_id = worktree.owner_id,
                "skipping a worktree row whose owner kind is unrecognised"
            );
            continue;
        };
        let feed_run_id = owner.feed_run_id();

        match remove(
            Path::new(&worktree.project_root),
            Path::new(&worktree.path),
            backoff,
        )
        .await
        {
            Ok(()) => {
                if let Err(error) = mark_removed(pool, owner).await {
                    tracing::warn!(
                        owner_id = owner.id(),
                        %error,
                        "failed to mark collected worktree as removed"
                    );
                }

                let branch_deleted =
                    delete_branch_if_merged(&worktree.project_root, &worktree.branch).await;
                let summary = if branch_deleted {
                    format!("removed worktree + merged branch {}", worktree.branch)
                } else {
                    format!("removed worktree {}", worktree.path)
                };
                let _ = feed::append(
                    pool,
                    Some(&worktree.project_id),
                    "worktree_removed",
                    &summary,
                    feed_run_id,
                )
                .await;
                feed_branch_outcome(pool, &worktree, branch_deleted).await;
            }
            // A directory that is no longer there is the outcome this pass wanted, however it got
            // that way — a hand-deleted stale worktree, a `git worktree remove` that ran elsewhere,
            // a volume that came back empty. `git worktree remove` still fails on a path git has
            // forgotten, so without this the row stayed a candidate: same backoff, same failure,
            // another feed row, every half hour forever.
            Err(_) if !Path::new(&worktree.path).exists() => {
                if let Err(error) = mark_removed(pool, owner).await {
                    tracing::warn!(
                        owner_id = owner.id(),
                        %error,
                        "failed to retire a worktree row whose directory had already gone"
                    );
                }
                tracing::info!(
                    owner_id = owner.id(),
                    path = %worktree.path,
                    "worktree directory was already gone; retiring its row"
                );
            }
            Err(error) => {
                let summary = format!("failed to remove worktree {}: {}", worktree.path, error);
                let _ = feed::append(
                    pool,
                    Some(&worktree.project_id),
                    "worktree_gc_failed",
                    &summary,
                    feed_run_id,
                )
                .await;
            }
        }
    }
}

/// How old an unaccounted-for directory must be before startup will collect it. Generous on purpose:
/// the cost of waiting is a stale directory for an hour, the cost of being wrong is deleting a
/// worktree a live run is mid-way through creating.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(3600);

/// Worktree directories on disk that no LIVE `worktrees` row accounts for.
///
/// Two ways one appears, and the normal GC pass can reach neither, because it only ever walks rows
/// that exist: a crash between `git worktree add` and the row INSERT leaves a directory nothing in
/// the database knows about, and a removal that set `removed_at` but whose files never actually went
/// away leaves a directory the GC now believes is gone. Both leak forever without this.
///
/// Fail-safe: a directory whose age cannot be determined is left alone, and anything not named
/// `run-<id>` or `job-<id>` is not ours to touch.
/// The owner a worktree directory name denotes, or `None` when the name is not ours to touch.
///
/// Fail-safe by construction: an unparseable name yields `None` and the sweeper leaves the directory
/// alone. Nothing here may split on `.` — the operations downstream delete directories, and a looser
/// parse would let a sibling like `job-5.artifacts` be read as job 5 and taken with it.
fn owner_from_dir_name(name: &str) -> Option<Owner> {
    if let Some(rest) = name.strip_prefix("run-") {
        return rest.parse::<i64>().ok().map(Owner::Run);
    }
    if let Some(rest) = name.strip_prefix("job-") {
        return rest.parse::<i64>().ok().map(Owner::Job);
    }
    None
}

pub async fn orphaned_worktrees(
    pool: &SqlitePool,
    project_root: &Path,
    min_age: Duration,
) -> sqlx::Result<Vec<PathBuf>> {
    // Keyed by the full owner pair, not by id alone: run ids and job ids come from different
    // sequences, so a live `run-7` row would otherwise account for a `job-7` directory and leave it
    // uncollectable forever.
    let live: std::collections::HashSet<(String, i64)> =
        sqlx::query_as("SELECT owner_kind, owner_id FROM worktrees WHERE removed_at IS NULL")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();

    let root = worktree_root(project_root);
    let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
        return Ok(Vec::new());
    };

    let mut orphans = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = entry.file_name();
        let Some(owner) = owner_from_dir_name(&file_name.to_string_lossy()) else {
            continue;
        };
        if live.contains(&(owner.kind().to_owned(), owner.id())) {
            continue;
        }

        let old_enough = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if !old_enough {
            continue;
        }

        orphans.push(entry.path());
    }
    Ok(orphans)
}

/// The last resort for an orphan `git worktree remove` would not take: delete the directory here.
///
/// Reached only when git no longer holds a row for the path, and that single condition is what
/// makes the delete defensible. Git refuses removal for reasons that must be respected — a
/// worktree someone locked needs `--force` twice, which `try_remove_once` deliberately does not
/// pass — and every refusal leaves the row in place. So the row is the signal: finishing a removal
/// git already began is repair, while deleting past one it declined is data loss. The failure's
/// own message is not consulted, because it is localised and version-dependent.
async fn collect_by_hand(project_root: &Path, orphan: &Path, cause: &io::Error) -> bool {
    if is_dangerous_removal_path(orphan, project_root) {
        tracing::warn!(
            path = %orphan.display(),
            %cause,
            "refusing to recursively delete an orphan at a dangerous path"
        );
        return false;
    }

    match registration(project_root, orphan).await {
        Ok(Registration::Absent) => {}
        Ok(Registration::Present) => {
            tracing::warn!(
                path = %orphan.display(),
                %cause,
                "git still registers this worktree, so its removal was refused rather than \
                 interrupted; leaving it for the next startup"
            );
            return false;
        }
        Err(error) => {
            tracing::warn!(
                path = %orphan.display(),
                %cause,
                %error,
                "could not ask git whether this worktree is still registered; leaving it alone"
            );
            return false;
        }
    }

    if let Err(error) = tokio::fs::remove_dir_all(orphan).await {
        tracing::warn!(
            path = %orphan.display(),
            %cause,
            %error,
            "failed to remove an orphaned worktree directory"
        );
        return false;
    }

    // The directory is gone, so the orphan is collected however the bookkeeping went. Reporting
    // otherwise would promise a retry that cannot happen — the sweep only ever finds directories,
    // and this one no longer exists. A surviving row is still worth saying out loud: it is exactly
    // the leak this step exists to close.
    if let Err(error) = prune_and_verify_registration_gone(project_root, orphan).await {
        tracing::warn!(
            path = %orphan.display(),
            %error,
            "removed the orphan directory but git's registration did not clear; \
             run `git worktree prune` in the project to drop it"
        );
    }

    true
}

/// Sweeps orphaned worktree directories for every project the daemon knows a root for, returning how
/// many it collected. Runs once at startup, where `reconcile_orphaned_runs` has already established
/// that nothing from a previous life is still running.
pub async fn reconcile_orphaned_worktrees(
    pool: &SqlitePool,
    min_age: Duration,
    backoff: &[Duration],
) -> sqlx::Result<usize> {
    // Both sources matter: a project switched off still owns whatever its runs left behind.
    let roots: Vec<String> = sqlx::query_scalar(
        "SELECT project_root FROM autopilot_state WHERE project_root IS NOT NULL
         UNION
         SELECT project_root FROM worktrees",
    )
    .fetch_all(pool)
    .await?;

    let mut collected = 0;
    for root in roots {
        let project_root = Path::new(&root);
        for orphan in orphaned_worktrees(pool, project_root, min_age).await? {
            // `git worktree remove` first, so git's own bookkeeping is updated when it still knows
            // about the worktree; a bare directory it never registered falls through to the fs.
            let removed = match remove(project_root, &orphan, backoff).await {
                Ok(()) => true,
                Err(error) if is_preservation_failure(&error) => false,
                Err(error) => collect_by_hand(project_root, &orphan, &error).await,
            };
            if removed {
                collected += 1;
                tracing::warn!(
                    path = %orphan.display(),
                    "collected an orphaned worktree directory left by a previous daemon"
                );
            } else {
                tracing::warn!(
                    path = %orphan.display(),
                    "could not collect an orphaned worktree directory; will retry next startup"
                );
            }
        }
    }
    Ok(collected)
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
    // `env_lock()` returns a process-wide MutexGuard deliberately held across the
    // awaits of these `current_thread` async tests, serialising mutation of the
    // shared `WORKTREE_ROOT` env override. It is a `std::sync::Mutex` (shared with
    // sync `#[test]`s, so it cannot become a `tokio::Mutex`) and there is no
    // multi-thread runtime to starve here — `await_holding_lock` is a false
    // positive for this intentional test-serialisation guard.
    #![allow(clippy::await_holding_lock)]

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
        set_worktree_created_at_for(pool, Owner::Run(run_id), created_at).await;
    }

    async fn set_worktree_created_at_for(pool: &sqlx::SqlitePool, owner: Owner, created_at: &str) {
        sqlx::query("UPDATE worktrees SET created_at = ? WHERE owner_kind = ? AND owner_id = ?")
            .bind(created_at)
            .bind(owner.kind())
            .bind(owner.id())
            .execute(pool)
            .await
            .unwrap();
    }

    /// Keeps its `run_id: i64` signature deliberately: every caller is exercising a *run's*
    /// worktree, and widening it to `Owner` would churn ten call sites without testing anything new.
    async fn create_and_record(pool: &sqlx::SqlitePool, repo: &Path, run_id: i64) -> WorktreeInfo {
        let owner = Owner::Run(run_id);
        let info = create(repo, owner).await.expect("create worktree");
        record(
            pool,
            owner,
            "project-a",
            repo.to_str().expect("repository path should be UTF-8"),
            info.path.to_str().expect("worktree path should be UTF-8"),
            &info.branch,
            info.base_sha.as_deref(),
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

    fn init_space_free_repo() -> tempfile::TempDir {
        let repo = space_free_tempdir();
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

    fn commit_count(dir: &Path, revision: &str) -> usize {
        git_stdout(
            dir,
            &[
                OsStr::new("rev-list"),
                OsStr::new("--count"),
                OsStr::new(revision),
            ],
        )
        .parse()
        .expect("commit count should be a number")
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

    /// `core.fsmonitor` is a command string, and git runs it whenever it refreshes the index —
    /// measured on git 2.50.1, a `worktree add` fires it exactly once. The project root is a
    /// repository the daemon does not control, so without `-c core.fsmonitor=` provisioning a
    /// worktree executes whatever that repository named, as the daemon user. Run against the
    /// unhardened command first, where the canary file appears.
    #[tokio::test(flavor = "current_thread")]
    async fn create_does_not_run_a_command_the_target_repository_names() {
        let _lock = env_lock();
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));

        // Forward slashes both ways: git hands the value to a shell, which reads a Windows
        // backslash as an escape rather than a separator.
        let canary = root.path().join("fsmonitor-canary.txt");
        let canary_arg = canary.to_string_lossy().replace('\\', "/");
        let hook = root.path().join("fsmonitor-probe.sh");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\necho fired >> '{canary_arg}'\nexit 0\n"),
        )
        .expect("write fsmonitor probe");
        let hook_arg = hook.to_string_lossy().replace('\\', "/");
        assert!(git_ok(
            repo.path(),
            &[
                OsStr::new("config"),
                OsStr::new("core.fsmonitor"),
                OsStr::new(hook_arg.as_str()),
            ],
        ));

        create(repo.path(), Owner::Run(4242))
            .await
            .expect("create worktree");

        assert!(
            !canary.exists(),
            "the target repository's core.fsmonitor command was executed"
        );
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

        let info = create(repo.path(), Owner::Run(7))
            .await
            .expect("create worktree");
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

    /// Where the tree was born, asked of git while git is the only thing that knows.
    #[tokio::test(flavor = "current_thread")]
    async fn a_new_worktree_knows_the_commit_it_branched_from() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let head = git_stdout(repo.path(), &[OsStr::new("rev-parse"), OsStr::new("HEAD")]);

        let info = create(repo.path(), Owner::Run(1))
            .await
            .expect("create worktree");

        assert_eq!(info.base_sha.as_deref(), Some(head.as_str()));
    }

    /// Recorded for both owners alike. The asymmetry this removes is exactly the dangerous one: if
    /// only jobs had a base, one live worktree run would put its whole project into *not measured*
    /// — the warning would switch itself off precisely while somebody was working.
    #[tokio::test(flavor = "current_thread")]
    async fn the_base_is_recorded_for_a_run_and_for_a_job_alike() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));

        for owner in [Owner::Run(1), Owner::Job(1)] {
            let info = create(repo.path(), owner).await.expect("create worktree");
            record(
                &pool,
                owner,
                "project-a",
                repo.path().to_str().unwrap(),
                info.path.to_str().unwrap(),
                &info.branch,
                info.base_sha.as_deref(),
            )
            .await
            .expect("record worktree");

            let stored: Option<String> = sqlx::query_scalar(
                "SELECT base_sha FROM worktrees WHERE owner_kind = ? AND owner_id = ?",
            )
            .bind(owner.kind())
            .bind(owner.id())
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(stored, info.base_sha, "{owner:?} was left without a base");
        }

        pool.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_rejects_a_spaced_root() {
        let _lock = env_lock();
        let repo = init_repo();
        let spaced_root = std::env::current_dir()
            .expect("resolve current directory")
            .join("root with a space");
        let _env = WorktreeRootEnv::set(Some(&spaced_root));
        let error = match create(repo.path(), Owner::Run(8)).await {
            Ok(_) => panic!("spaced root must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// Paired with `remove`'s refusal of relative paths: if `create` accepted one, the pair would
    /// produce a worktree nothing could ever collect.
    #[tokio::test(flavor = "current_thread")]
    async fn create_rejects_a_relative_root() {
        let _lock = env_lock();
        let repo = init_repo();
        let _env = WorktreeRootEnv::set(Some(Path::new("relative-worktree-root")));
        let error = match create(repo.path(), Owner::Run(9)).await {
            Ok(_) => panic!("relative root must be rejected"),
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
        assert!(create(not_repo.path(), Owner::Run(1)).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserve_commits_uncommitted_work_before_removal() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Run(101))
            .await
            .expect("create worktree");
        let commits_before = commit_count(repo.path(), &info.branch);

        std::fs::write(info.path.join("seed.txt"), "modified\n").expect("modify tracked file");
        std::fs::write(info.path.join("staged.txt"), "staged\n").expect("write staged file");
        assert!(git_ok(
            &info.path,
            &[OsStr::new("add"), OsStr::new("staged.txt")]
        ));
        std::fs::write(info.path.join("untracked.txt"), "untracked\n")
            .expect("write untracked file");

        // Preservation commits must bypass project hooks: a repository-controlled hook must not
        // get to turn cleanup into data loss.
        std::fs::write(
            repo.path().join(".git").join("hooks").join("pre-commit"),
            "#!/bin/sh\nexit 1\n",
        )
        .expect("write rejecting pre-commit hook");

        remove(repo.path(), &info.path, &[])
            .await
            .expect("preserve and remove worktree");

        assert!(!info.path.exists());
        assert_eq!(
            commit_count(repo.path(), &info.branch),
            commits_before + 1,
            "removal must leave exactly one preservation commit on the worktree branch"
        );
        let identity = git_stdout(
            repo.path(),
            &[
                OsStr::new("show"),
                OsStr::new("-s"),
                OsStr::new("--format=%an <%ae>"),
                OsStr::new(&info.branch),
            ],
        );
        assert_eq!(identity, "nucleos <nucleos@localhost>");
        for (path, expected) in [
            ("seed.txt", "modified"),
            ("staged.txt", "staged"),
            ("untracked.txt", "untracked"),
        ] {
            let object = format!("{}:{path}", info.branch);
            assert_eq!(
                git_stdout(
                    repo.path(),
                    &[OsStr::new("show"), OsStr::new(object.as_str())],
                ),
                expected,
                "{path} must survive in the preservation commit"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reconcile_preserves_before_collecting_an_orphan() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root)
             VALUES ('project-a', 'active', ?)",
        )
        .bind(repo.path().to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .unwrap();
        let orphan = create(repo.path(), Owner::Run(102))
            .await
            .expect("create orphan");
        let commits_before = commit_count(repo.path(), &orphan.branch);
        std::fs::write(orphan.path.join("crash-recovery.txt"), "survived startup\n")
            .expect("write orphaned work");

        let collected = reconcile_orphaned_worktrees(&pool, Duration::ZERO, &[])
            .await
            .expect("reconcile orphaned worktrees");

        assert_eq!(collected, 1);
        assert!(!orphan.path.exists());
        assert_eq!(
            commit_count(repo.path(), &orphan.branch),
            commits_before + 1
        );
        let object = format!("{}:crash-recovery.txt", orphan.branch);
        assert_eq!(
            git_stdout(
                repo.path(),
                &[OsStr::new("show"), OsStr::new(object.as_str())],
            ),
            "survived startup"
        );
        pool.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_clean_worktree_is_removed_without_an_empty_commit() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Run(103))
            .await
            .expect("create worktree");
        let commits_before = commit_count(repo.path(), &info.branch);

        remove(repo.path(), &info.path, &[])
            .await
            .expect("remove clean worktree");

        assert!(!info.path.exists());
        assert_eq!(
            commit_count(repo.path(), &info.branch),
            commits_before,
            "a clean worktree must not gain an empty preservation commit"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_preserve_leaves_the_worktree_alone() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root)
             VALUES ('project-a', 'active', ?)",
        )
        .bind(repo.path().to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .unwrap();
        let info = create(repo.path(), Owner::Run(104))
            .await
            .expect("create worktree");
        let commits_before = commit_count(repo.path(), &info.branch);
        std::fs::write(info.path.join("failure.txt"), "must survive\n")
            .expect("write uncommitted work");

        // `--no-verify` does not bypass commit signing. Pointing signing at an absent binary makes
        // the preservation commit fail while leaving `git worktree remove` itself fully usable.
        assert!(git_ok(
            &info.path,
            &[
                OsStr::new("config"),
                OsStr::new("commit.gpgSign"),
                OsStr::new("true"),
            ],
        ));
        let missing_signer = root.path().join("missing-gpg.exe");
        assert!(git_ok(
            &info.path,
            &[
                OsStr::new("config"),
                OsStr::new("gpg.program"),
                missing_signer.as_os_str(),
            ],
        ));

        let collected = reconcile_orphaned_worktrees(&pool, Duration::ZERO, &[])
            .await
            .expect("reconcile should defer a failed preservation");

        assert_eq!(
            collected, 0,
            "the filesystem fallback must not count a failed preservation as collected"
        );
        assert!(info.path.is_dir(), "the worktree directory must survive");
        assert_eq!(commit_count(repo.path(), &info.branch), commits_before);
        assert_eq!(
            std::fs::read_to_string(info.path.join("failure.txt")).expect("read surviving work"),
            "must survive\n"
        );
        assert!(
            git_stdout(
                &info.path,
                &[OsStr::new("status"), OsStr::new("--porcelain")],
            )
            .contains("failure.txt"),
            "the uncommitted file must remain visible to git"
        );
        pool.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn release_defers_removal_on_a_failed_preserve() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(
            &pool,
            "awaiting_approval",
            None,
            "2026-07-29T00:00:00+00:00",
        )
        .await;
        let info = create_and_record(&pool, repo.path(), run_id).await;
        std::fs::write(info.path.join("failure.txt"), "must survive release\n")
            .expect("write uncommitted work");

        // `--no-verify` does not bypass commit signing. Pointing signing at an absent binary makes
        // preservation fail before release can remove the worktree.
        assert!(git_ok(
            &info.path,
            &[
                OsStr::new("config"),
                OsStr::new("commit.gpgSign"),
                OsStr::new("true"),
            ],
        ));
        let missing_signer = root.path().join("missing-gpg.exe");
        assert!(git_ok(
            &info.path,
            &[
                OsStr::new("config"),
                OsStr::new("gpg.program"),
                missing_signer.as_os_str(),
            ],
        ));

        assert_eq!(
            release(&pool, run_id).await.unwrap(),
            ReleaseOutcome::Released
        );

        assert!(info.path.is_dir(), "the worktree directory must survive");
        assert_eq!(
            std::fs::read_to_string(info.path.join("failure.txt")).expect("read surviving work"),
            "must survive release\n"
        );
        pool.close().await;
    }

    /// The handoff between a job's nodes lives in the worktree, which is the only place the
    /// classifier lets a node write — and `preserve_uncommitted` runs `git add -A`, so left visible
    /// it would ride into the preservation commit and count against the byte ceiling, where going
    /// over blocks removal of the worktree rather than trimming the commit.
    #[tokio::test(flavor = "current_thread")]
    async fn a_jobs_handoff_files_stay_out_of_the_preservation_commit() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Job(41))
            .await
            .expect("create the job's worktree");

        let artifacts = prepare_artifacts(&info.path)
            .await
            .expect("prepare the job's artifacts directory");
        assert!(artifacts.is_dir());
        std::fs::write(artifacts.join("plan.json"), r#"{"items":[]}"#).expect("write a plan");
        std::fs::write(info.path.join("seed.txt"), "an item's work\n")
            .expect("modify tracked file");

        assert!(
            preserve_uncommitted(&info.path, 1_000_000)
                .await
                .expect("preserve the item's work"),
            "the tracked change alone should still produce a commit"
        );

        let handoff_object = format!("{}:{ARTIFACTS_DIR}/plan.json", info.branch);
        assert!(
            !git_ok(
                repo.path(),
                &[
                    OsStr::new("cat-file"),
                    OsStr::new("-e"),
                    OsStr::new(handoff_object.as_str()),
                ],
            ),
            "the handoff must not enter the preservation commit"
        );
        // Still on disk: the next node reads it. Excluded means invisible to git, not deleted.
        assert!(artifacts.join("plan.json").exists());
        assert_eq!(
            git_stdout(
                &info.path,
                &[OsStr::new("status"), OsStr::new("--porcelain")],
            ),
            "",
            "an excluded handoff leaves the worktree looking clean"
        );
    }

    /// The footing itself: an item's work becomes a commit, and the caller is told which one.
    #[tokio::test(flavor = "current_thread")]
    async fn a_checkpoint_commits_what_an_item_left_and_answers_with_its_sha() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Job(60))
            .await
            .expect("create the job's worktree");

        let before = head_sha(&info.path).await.expect("read HEAD before");
        std::fs::write(info.path.join("seed.txt"), "an item's work\n").expect("modify a file");
        std::fs::write(info.path.join("new_module.txt"), "and a new one\n").expect("add a file");

        let sha = checkpoint(&info.path).await.expect("checkpoint the item");

        assert_ne!(sha, before, "work that changed the tree must move HEAD");
        assert_eq!(
            sha,
            head_sha(&info.path).await.expect("read HEAD after"),
            "the answer has to be the commit that was just made, not some other revision"
        );
        assert_eq!(
            git_stdout(
                &info.path,
                &[OsStr::new("status"), OsStr::new("--porcelain")],
            ),
            "",
            "nothing may be left uncommitted, or the next item inherits it"
        );
    }

    /// An item that measured green without writing anything is already standing on `HEAD`.
    ///
    /// `--allow-empty` would be the lazy spelling and would mint a distinct SHA per item that
    /// distinguishes nothing — every revert target looking different and none of them meaning
    /// anything. The property worth having is that the answer is always a commit the tree IS at.
    #[tokio::test(flavor = "current_thread")]
    async fn a_checkpoint_on_a_clean_tree_answers_head_without_committing() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Job(61))
            .await
            .expect("create the job's worktree");

        let before = head_sha(&info.path).await.expect("read HEAD before");
        let sha = checkpoint(&info.path)
            .await
            .expect("checkpoint a clean tree");

        assert_eq!(
            sha, before,
            "a clean tree must not earn a commit of its own"
        );
    }

    /// The same door `preserve_uncommitted` keeps shut, and for the sharper reason: this one runs
    /// `add -A` on the happy path, dozens of times a night. Pointed at a plain directory inside
    /// somebody else's repository it would commit THEIR work, on a schedule.
    #[tokio::test(flavor = "current_thread")]
    async fn a_checkpoint_refuses_a_path_that_is_not_a_worktree_root() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let inside = repo.path().join("just-a-directory");
        std::fs::create_dir(&inside).expect("create a plain directory inside the repository");
        std::fs::write(inside.join("theirs.txt"), "somebody else's work\n").expect("write");

        let error = checkpoint(&inside)
            .await
            .expect_err("a plain directory must be refused");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            git_stdout(
                repo.path(),
                &[OsStr::new("status"), OsStr::new("--porcelain")],
            )
            .trim(),
            "?? just-a-directory/",
            "the enclosing repository's work must be exactly as untouched as it was"
        );
    }

    /// The half of the revert that `reset --hard` cannot do.
    ///
    /// A skipped item that only CREATED files has nothing for `reset` to undo, so without the clean
    /// the next item starts on top of its modules, its scratch scripts and its `__pycache__` — in a
    /// tree that looks reverted. Which is the defect this change exists to remove, in disguise.
    #[tokio::test(flavor = "current_thread")]
    async fn reverting_takes_back_created_files_and_not_only_modified_ones() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Job(62))
            .await
            .expect("create the job's worktree");

        let footing = checkpoint(&info.path).await.expect("take a footing");

        std::fs::write(info.path.join("seed.txt"), "the item edited this\n").expect("modify");
        std::fs::write(info.path.join("invented.txt"), "and invented this\n").expect("create");
        std::fs::create_dir(info.path.join("scratch")).expect("create a directory");
        std::fs::write(info.path.join("scratch/notes.txt"), "and this\n").expect("write");

        revert_to(&info.path, &footing).await.expect("revert");

        // Trimmed, because git's own end-of-line filter rewrites the checkout on this platform and
        // the property under test is the CONTENT, not which bytes git chose to terminate it with.
        assert_eq!(
            std::fs::read_to_string(info.path.join("seed.txt"))
                .expect("read the tracked file")
                .trim(),
            "seed",
            "a tracked edit has to be taken back"
        );
        assert!(
            !info.path.join("invented.txt").exists(),
            "a created file has to be taken back too — this is the half reset cannot do"
        );
        assert!(
            !info.path.join("scratch").exists(),
            "and a created directory with it"
        );
        assert_eq!(
            head_sha(&info.path).await.expect("read HEAD"),
            footing,
            "the tree must be standing on the footing it was given"
        );
    }

    /// A revert that cannot name its target has to say so rather than half-happen. The caller's
    /// answer to this error is to stop the job, and it can only make that choice if the tree is in
    /// a state it recognises.
    #[tokio::test(flavor = "current_thread")]
    async fn reverting_to_a_revision_that_does_not_exist_fails_and_leaves_the_work_alone() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Job(63))
            .await
            .expect("create the job's worktree");
        std::fs::write(info.path.join("seed.txt"), "work in progress\n").expect("modify");

        revert_to(&info.path, "0000000000000000000000000000000000000000")
            .await
            .expect_err("a revision that does not exist must be an error");

        assert_eq!(
            std::fs::read_to_string(info.path.join("seed.txt")).expect("read"),
            "work in progress\n",
            "a failed revert must not have thrown the work away on its way to failing"
        );
    }

    /// Called once per job today, but a resumed job would call it again on a tree that already has
    /// the entry. Appending it a second time is harmless to git and a lie to anyone reading the
    /// file, and the same loop that produced two would produce two hundred.
    #[tokio::test(flavor = "current_thread")]
    async fn preparing_the_handoff_twice_writes_one_exclusion() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Job(42))
            .await
            .expect("create the job's worktree");

        prepare_artifacts(&info.path).await.expect("first call");
        prepare_artifacts(&info.path).await.expect("second call");

        // Read from the main repository's `.git`, deliberately: that is the common directory, and
        // the only `info/exclude` git ever reads. A per-worktree copy would be inert.
        let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude"))
            .expect("read the repository's exclude file");
        let entry = format!("/{ARTIFACTS_DIR}/");
        assert_eq!(
            exclude.lines().filter(|line| line.trim() == entry).count(),
            1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserve_skips_ignored_paths_and_respects_the_ceiling() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        std::fs::write(repo.path().join(".gitignore"), "ignored/\n")
            .expect("write project ignore rule");
        assert!(git_ok(repo.path(), &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo.path(),
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("add ignore rule"),
            ],
        ));
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Run(105))
            .await
            .expect("create worktree");
        let ceiling = 64_u64;
        let commits_before = commit_count(repo.path(), &info.branch);

        std::fs::create_dir_all(info.path.join("ignored")).expect("create ignored directory");
        std::fs::write(
            info.path.join("ignored").join("cache.bin"),
            vec![b'i'; 1024],
        )
        .expect("write ignored file above ceiling");
        std::fs::write(info.path.join("seed.txt"), "small change\n").expect("modify tracked file");
        std::fs::write(info.path.join("keep.txt"), "keep\n").expect("write small untracked file");

        assert!(
            preserve_uncommitted(&info.path, ceiling)
                .await
                .expect("preserve non-ignored changes"),
            "the helper should report that it created a commit"
        );
        assert_eq!(commit_count(repo.path(), &info.branch), commits_before + 1);
        assert_eq!(
            git_stdout(
                &info.path,
                &[OsStr::new("status"), OsStr::new("--porcelain")],
            ),
            "",
            "all non-ignored changes should be committed"
        );
        assert!(info.path.join("ignored").join("cache.bin").exists());
        let ignored_object = format!("{}:ignored/cache.bin", info.branch);
        assert!(
            !git_ok(
                repo.path(),
                &[
                    OsStr::new("cat-file"),
                    OsStr::new("-e"),
                    OsStr::new(ignored_object.as_str()),
                ],
            ),
            "a project-ignored path must not enter the preservation commit"
        );

        let commits_before_overflow = commit_count(repo.path(), &info.branch);
        std::fs::write(
            info.path.join("too-large.bin"),
            vec![b'x'; ceiling as usize + 1],
        )
        .expect("write file above ceiling");

        let error = preserve_uncommitted(&info.path, ceiling)
            .await
            .expect_err("ceiling overflow must fail before committing");

        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidData,
            "ceiling overflow must be distinguishable from a clean worktree"
        );
        assert_eq!(
            commit_count(repo.path(), &info.branch),
            commits_before_overflow,
            "overflow must not create a partial preservation commit"
        );
        assert!(info.path.is_dir());
        assert!(
            git_stdout(
                &info.path,
                &[OsStr::new("status"), OsStr::new("--porcelain")],
            )
            .contains("too-large.bin")
        );
        let overflow_object = format!("{}:too-large.bin", info.branch);
        assert!(
            !git_ok(
                repo.path(),
                &[
                    OsStr::new("cat-file"),
                    OsStr::new("-e"),
                    OsStr::new(overflow_object.as_str()),
                ],
            ),
            "overflowing content must not be partially committed"
        );
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
            Owner::Run(run_id),
            "project-a",
            "/project/a",
            "/worktrees/run-1",
            "nucleos/run-1",
            None,
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
                owner_kind: "run".to_owned(),
                owner_id: run_id,
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
            Owner::Run(run_id),
            "project-a",
            "/project/a",
            "/worktrees/run-2",
            "nucleos/run-2",
            None,
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
            Owner::Run(run_id),
            "project-a",
            "/project/a",
            "/worktrees/run-3",
            "nucleos/run-3",
            None,
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
            Owner::Run(run_id),
            "project-a",
            "/project/a",
            "/worktrees/run-4",
            "nucleos/run-4",
            None,
        )
        .await
        .unwrap();
        mark_removed(&pool, Owner::Run(run_id)).await.unwrap();

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
            Owner::Run(run_id),
            "project-a",
            "/project/a",
            "/worktrees/run-5",
            "nucleos/run-5",
            None,
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
            Owner::Run(run_id),
            "project-a",
            "/project/a",
            "/worktrees/run-6",
            "nucleos/run-6",
            None,
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

        mark_removed(&pool, Owner::Run(run_id)).await.unwrap();

        assert!(
            gc_candidates(&pool, now, chrono::Duration::hours(72))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_job_and_a_run_can_each_own_a_worktree_with_the_same_id() {
        let pool = test_pool().await;

        record(
            &pool,
            Owner::Run(1),
            "project-a",
            "/project/a",
            "/worktrees/run-1",
            "nucleos/run-1",
            None,
        )
        .await
        .expect("record run-owned worktree");
        record(
            &pool,
            Owner::Job(1),
            "project-a",
            "/project/a",
            "/worktrees/job-1",
            "nucleos/job-1",
            None,
        )
        .await
        .expect("record job-owned worktree");

        // Same numeric id, different kind. Keying on `owner_id` alone would make the second insert a
        // constraint violation and the `expect` above would panic, so this count tests the composite
        // key rather than restating it.
        let live: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM worktrees WHERE removed_at IS NULL")
                .fetch_one(&pool)
                .await
                .expect("count live worktrees");
        assert_eq!(live, 2);
    }

    #[tokio::test]
    async fn a_job_worktree_is_not_collected_by_a_run_of_the_same_id() {
        let pool = test_pool().await;
        // A terminal run, old enough to be collectable, whose id the job then reuses. Run ids and
        // job ids come from different sequences, so this collision is ordinary, not contrived — and
        // the GC joins on the bare id, so only `owner_kind = 'run'` keeps the two apart.
        let run_id = insert_run(
            &pool,
            "completed",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        record(
            &pool,
            Owner::Job(run_id),
            "project-a",
            "/project/a",
            "/worktrees/job-1",
            "nucleos/job-1",
            None,
        )
        .await
        .unwrap();
        set_worktree_created_at_for(&pool, Owner::Job(run_id), "2026-07-09T00:00:00+00:00").await;

        let candidates = gc_candidates(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert!(
            candidates.is_empty(),
            "a job's worktree must not be collected because a run happens to share its id"
        );
    }

    async fn insert_job(
        pool: &sqlx::SqlitePool,
        status: &str,
        completed_at: Option<&str>,
        created_at: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at, completed_at)
             VALUES ('project-a', '/project/a', ?, 5, ?, ?)",
        )
        .bind(status)
        .bind(created_at)
        .bind(completed_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn gc_collects_a_finished_jobs_worktree() {
        let pool = test_pool().await;
        let job_id = insert_job(
            &pool,
            "completed",
            Some("2026-07-09T00:00:00+00:00"),
            "2026-07-09T00:00:00+00:00",
        )
        .await;
        record(
            &pool,
            Owner::Job(job_id),
            "project-a",
            "/project/a",
            "/worktrees/job-9",
            "nucleos/job-9",
            None,
        )
        .await
        .unwrap();
        set_worktree_created_at_for(&pool, Owner::Job(job_id), "2026-07-09T00:00:00+00:00").await;

        let candidates = gc_candidates(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].owner_kind, "job");
        assert_eq!(candidates[0].owner_id, job_id);
    }

    #[tokio::test]
    async fn gc_never_collects_a_live_jobs_worktree_however_old() {
        let pool = test_pool().await;
        // `waiting` is the sharpest case: an hourly budget limit can hold a job there for a long
        // time, so age alone must never make its worktree collectable. The job is coming back to
        // it, and deleting it would destroy work a gate had already measured as good.
        let job_id = insert_job(&pool, "waiting", None, "2020-01-01T00:00:00+00:00").await;
        record(
            &pool,
            Owner::Job(job_id),
            "project-a",
            "/project/a",
            "/worktrees/job-10",
            "nucleos/job-10",
            None,
        )
        .await
        .unwrap();
        set_worktree_created_at_for(&pool, Owner::Job(job_id), "2020-01-01T00:00:00+00:00").await;

        let candidates = gc_candidates(
            &pool,
            timestamp("2030-01-01T00:00:00+00:00"),
            chrono::Duration::hours(72),
        )
        .await
        .unwrap();

        assert!(candidates.is_empty(), "a live job keeps its worktree");
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
        let removed_at: Option<String> = sqlx::query_scalar(
            "SELECT removed_at FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?",
        )
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
        let removed_at: Option<String> = sqlx::query_scalar(
            "SELECT removed_at FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?",
        )
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

    /// A worktree whose directory is already gone is the outcome the GC is trying to reach, so it
    /// has to converge on it. `git worktree remove` fails on a path git no longer knows about, and
    /// the failure arm never marked the row — so the same candidate came back every half hour,
    /// ran the full backoff, failed again and appended another feed row, forever. Someone deleting
    /// a stale directory by hand was enough to start it.
    #[tokio::test(flavor = "current_thread")]
    async fn gc_converges_when_the_directory_is_already_gone() {
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
        assert!(!info.path.exists(), "the fixture must leave no directory");

        gc_pass(
            &pool,
            timestamp("2026-07-19T00:00:00+00:00"),
            chrono::Duration::hours(72),
            &[],
        )
        .await;

        let removed_at: Option<String> = sqlx::query_scalar(
            "SELECT removed_at FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            removed_at.is_some(),
            "a vanished directory must retire its row, not be retried forever"
        );

        // A second pass must find nothing left to do, which is the property "converges" means.
        let before = crate::feed::list_feed(&pool, Some("project-a"), 50)
            .await
            .unwrap()
            .len();
        gc_pass(
            &pool,
            timestamp("2026-07-19T01:00:00+00:00"),
            chrono::Duration::hours(72),
            &[],
        )
        .await;
        let after = crate::feed::list_feed(&pool, Some("project-a"), 50)
            .await
            .unwrap();
        assert_eq!(after.len(), before, "the second pass must be a no-op");
        assert!(
            !after.iter().any(|entry| entry.kind == "worktree_gc_failed"),
            "a directory that is already gone is not a failure"
        );
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
        let removed_at: Option<String> = sqlx::query_scalar(
            "SELECT removed_at FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?",
        )
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
        let removed_at: Option<String> = sqlx::query_scalar(
            "SELECT removed_at FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?",
        )
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

    /// Release is not a quick write: `git worktree remove` retries on a backoff that can run for
    /// half a minute, and everything the run's status meant when the call started can change inside
    /// that window — a cancel, or an approval that supersedes the run and hands its worktree to a
    /// resume. Checking the status and then writing it are therefore two decisions about two
    /// different moments unless the check lives inside the write.
    #[tokio::test(flavor = "current_thread")]
    async fn release_never_overwrites_a_status_written_under_it() {
        use std::future::Future;

        let _lock = env_lock();
        // A file-backed pool with room for a second connection: the test writes to the same rows
        // while the release future is mid-flight, which a single-connection pool would deadlock on.
        let db = crate::storage::TempDb::new().await;
        let pool = db.pool.clone();
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(
            &pool,
            "awaiting_approval",
            None,
            "2026-07-28T00:00:00+00:00",
        )
        .await;
        let info = create_and_record(&pool, repo.path(), run_id).await;

        // Driven by hand so the status can be moved at a chosen point rather than a hoped-for one:
        // once the worktree is gone from disk, which is past the status check and, unguarded, still
        // short of the `cancelled` write.
        let mut releasing = Box::pin(release(&pool, run_id));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut removed = false;
        for _ in 0..10_000 {
            assert!(
                releasing.as_mut().poll(&mut context).is_pending(),
                "release finished before the status could move under it"
            );
            if !info.path.exists() {
                removed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(removed, "the worktree was never removed");

        // Whatever finalises the run next — here, an approval superseding it.
        sqlx::query("UPDATE runs SET status = 'superseded' WHERE id = ?")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();

        let outcome = loop {
            if let std::task::Poll::Ready(outcome) = releasing.as_mut().poll(&mut context) {
                break outcome.expect("release should not fail");
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };

        // Honest either way: the claim that authorised this release happened before the status
        // moved, so the worktree really was this call's to discard.
        assert_eq!(outcome, ReleaseOutcome::Released);
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            status, "superseded",
            "release must not stamp `cancelled` over a status it did not read"
        );
        db.close().await;
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
        let info = create(repo.path(), Owner::Run(9))
            .await
            .expect("create worktree");
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

    #[tokio::test(flavor = "current_thread")]
    async fn orphan_sweep_ignores_a_directory_a_live_row_accounts_for() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(&pool, "running", None, "2026-07-27T00:00:00+00:00").await;
        create_and_record(&pool, repo.path(), run_id).await;

        // Old enough to collect, but a live row explains it — this is the pinned-worktree case, and
        // collecting it would delete work an approval is still waiting on.
        assert!(
            orphaned_worktrees(&pool, repo.path(), Duration::ZERO)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn orphan_sweep_finds_a_directory_no_live_row_explains() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));

        // A crash between `git worktree add` and the row INSERT: on disk, unknown to the database.
        let unrecorded = create(repo.path(), Owner::Run(41))
            .await
            .expect("create worktree");
        // A removal that set `removed_at` but whose files never went away.
        let leaked_run = insert_run(&pool, "completed", None, "2026-07-27T00:00:00+00:00").await;
        let leaked = create_and_record(&pool, repo.path(), leaked_run).await;
        mark_removed(&pool, Owner::Run(leaked_run)).await.unwrap();
        // Not ours, whatever its age.
        let stranger = root.path().join("not-a-run");
        std::fs::create_dir_all(&stranger).expect("create unrelated directory");

        let mut orphans = orphaned_worktrees(&pool, repo.path(), Duration::ZERO)
            .await
            .unwrap();
        orphans.sort();
        let mut expected = vec![unrecorded.path.clone(), leaked.path.clone()];
        expected.sort();
        assert_eq!(orphans, expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unaccounted_job_directory_is_sweepable() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));

        // Nothing creates a `job-*` worktree yet. The sweeper has to recognise the name *before*
        // the first one exists: a directory it does not recognise is one it never collects, so a
        // name taught later leaves every job worktree made in the meantime leaking forever, with
        // nothing to report it.
        let orphan = create(repo.path(), Owner::Job(3))
            .await
            .expect("create job worktree");

        let orphans = orphaned_worktrees(&pool, repo.path(), Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(orphans, vec![orphan.path.clone()]);
    }

    #[test]
    fn an_artifacts_directory_is_not_a_worktree() {
        // The sweeper deletes what this returns, so a looser parse — splitting on `.`, say — would
        // read a sibling artifacts directory as its job and take it too.
        assert_eq!(owner_from_dir_name("job-5.artifacts"), None);
        assert_eq!(owner_from_dir_name("job-5"), Some(Owner::Job(5)));
        assert_eq!(owner_from_dir_name("run-5"), Some(Owner::Run(5)));
        assert_eq!(owner_from_dir_name("not-a-run"), None);
    }

    /// The integration worktree is the daemon's own, and nothing in `vcs.rs` protects it from the
    /// sweeper — only this module's naming rule does. The `run-99` directory is the control: without
    /// it a sweeper that returned nothing at all would pass this test while collecting everything.
    #[tokio::test]
    async fn the_integration_worktree_is_not_an_orphan() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let container = space_free_tempdir();
        let project_root = container.path().join("repo");
        let roots = container.path().join("worktrees");
        let _env = WorktreeRootEnv::set(Some(roots.as_path()));

        let integration = crate::git_exec::integration_worktree(&project_root);
        std::fs::create_dir_all(&integration).expect("create the integration worktree directory");
        std::fs::create_dir_all(roots.join("run-99")).expect("create the control directory");

        let orphans = orphaned_worktrees(&pool, &project_root, Duration::ZERO)
            .await
            .expect("sweep");

        assert!(
            orphans.contains(&roots.join("run-99")),
            "the control must be collected, or this test proves nothing"
        );
        assert!(
            !orphans.contains(&integration),
            "the daemon's own worktree must never be swept"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn orphan_sweep_leaves_a_young_directory_alone() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        create(repo.path(), Owner::Run(42))
            .await
            .expect("create worktree");

        // Freshly made and unrecorded looks exactly like a run mid-way through starting up, so the
        // age gate is what stops the sweep from deleting a worktree out from under a live run.
        assert!(
            orphaned_worktrees(&pool, repo.path(), ORPHAN_MIN_AGE)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reconcile_collects_orphans_for_every_known_root() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('project-a', 'active', ?)")
            .bind(repo.path().to_string_lossy().as_ref())
            .execute(&pool)
            .await
            .unwrap();
        let orphan = create(repo.path(), Owner::Run(43))
            .await
            .expect("create worktree");

        let collected = reconcile_orphaned_worktrees(&pool, Duration::ZERO, &[])
            .await
            .unwrap();

        assert_eq!(collected, 1);
        assert!(!orphan.path.exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn release_deletes_a_branch_that_carries_no_work() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let run_id = insert_run(
            &pool,
            "awaiting_approval",
            None,
            "2026-07-27T00:00:00+00:00",
        )
        .await;
        let info = create_and_record(&pool, repo.path(), run_id).await;

        assert_eq!(
            release(&pool, run_id).await.unwrap(),
            ReleaseOutcome::Released
        );

        // A run blocked at its first high-risk action leaves a commitless branch; discarding the
        // worktree without it is how every rejected proposal used to leak one.
        let branches = git_stdout(repo.path(), &[OsStr::new("branch"), OsStr::new("--list")]);
        assert!(
            !branches.contains(&info.branch),
            "branch should be gone, got: {branches}"
        );
    }

    /// `git status`/`add`/`commit` discover the enclosing repository when pointed at a plain
    /// directory, so preserving a stray `run-<id>` that sits inside one would commit that
    /// repository's unrelated work under the daemon's name.
    #[tokio::test(flavor = "current_thread")]
    async fn preserve_refuses_a_plain_directory_inside_another_repository() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let stray = repo.path().join("run-77");
        std::fs::create_dir_all(&stray).expect("create stray directory");
        std::fs::write(stray.join("scratch.txt"), "unrelated\n").expect("write stray file");
        let before = commit_count(repo.path(), "HEAD");

        let preserved = preserve_uncommitted(&stray, DEFAULT_PRESERVATION_BYTE_CEILING)
            .await
            .expect("a plain directory is not an error");

        assert!(!preserved, "nothing belonging to a run was preserved");
        assert_eq!(
            commit_count(repo.path(), "HEAD"),
            before,
            "the enclosing repository must not gain a commit"
        );
        let status = git_stdout(
            repo.path(),
            &[OsStr::new("status"), OsStr::new("--porcelain")],
        );
        assert!(
            status.contains("run-77"),
            "the stray directory must be left untracked, got: {status}"
        );
    }

    /// The counterpart: a real worktree still gets its work preserved. The guard above must not
    /// answer "not a worktree" for the case preservation exists to serve.
    #[tokio::test(flavor = "current_thread")]
    async fn preserve_still_commits_inside_a_real_worktree() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Run(78))
            .await
            .expect("create worktree");
        std::fs::write(info.path.join("work.txt"), "real work\n").expect("write work file");
        let before = commit_count(repo.path(), &info.branch);

        let preserved = preserve_uncommitted(&info.path, DEFAULT_PRESERVATION_BYTE_CEILING)
            .await
            .expect("preserving a real worktree should succeed");

        assert!(
            preserved,
            "uncommitted work in a real worktree is preserved"
        );
        assert_eq!(commit_count(repo.path(), &info.branch), before + 1);
    }

    #[test]
    fn path_contains_compares_components_not_string_prefixes() {
        let base = space_free_tempdir();
        let repo = base.path().join("repo");
        let backup = base.path().join("repo-backup");

        assert!(path_contains(base.path(), &repo));
        assert!(path_contains(&repo, &repo));
        assert!(!path_contains(&repo, base.path()));
        // `repo-backup` is not inside `repo`, though its string starts the same way.
        assert!(!path_contains(&repo, &backup));
    }

    /// git prints `C:/x/y` where the daemon holds `C:\x\y`, and the registration lookup compares
    /// the two — so separator and case must not make the same path look like two.
    #[cfg(windows)]
    #[test]
    fn paths_equal_ignores_separator_and_case_on_windows() {
        assert!(paths_equal(
            Path::new("C:/work/repo"),
            Path::new(r"C:\work\repo")
        ));
        assert!(paths_equal(
            Path::new(r"c:\work\REPO"),
            Path::new(r"C:\Work\repo")
        ));
        assert!(!paths_equal(
            Path::new(r"C:\work\repo"),
            Path::new(r"C:\work\repo-backup")
        ));
    }

    #[test]
    fn is_dangerous_removal_path_rejects_repo_root_filesystem_root_and_home() {
        let _lock = env_lock();
        let root = space_free_tempdir();
        let repo = root.path().join("repo");

        assert!(is_dangerous_removal_path(Path::new(""), &repo));
        assert!(is_dangerous_removal_path(Path::new("   "), &repo));
        // Relative: it would resolve against the daemon's working directory, not this root.
        assert!(is_dangerous_removal_path(Path::new("run-1"), &repo));
        // A filesystem root has no parent.
        let filesystem_root = root
            .path()
            .ancestors()
            .last()
            .expect("every path has a root ancestor");
        assert!(is_dangerous_removal_path(filesystem_root, &repo));
        // The project itself, and any ancestor of it.
        assert!(is_dangerous_removal_path(&repo, &repo));
        assert!(is_dangerous_removal_path(root.path(), &repo));
        // The home directory, and any ancestor of it.
        let home = home_dir().expect("a home directory should be set");
        assert!(is_dangerous_removal_path(&home, &repo));
        assert!(is_dangerous_removal_path(
            home.parent().expect("home has a parent"),
            &repo
        ));
    }

    #[test]
    fn is_dangerous_removal_path_allows_an_ordinary_run_directory() {
        let _lock = env_lock();
        let root = space_free_tempdir();
        let repo = root.path().join("repo");
        let ordinary = root.path().join("run-1");
        std::fs::create_dir_all(&ordinary).expect("create run directory");

        assert!(!is_dangerous_removal_path(&ordinary, &repo));
    }

    /// A junction rather than a symlink, because `CreateSymbolicLink` needs elevation or Developer
    /// Mode while `mklink /J` needs neither — so the junction is both the reachable case on a
    /// normal Windows account and the one this test can actually build. Rust reports it through
    /// `FileType::is_symlink`, which is what the guard reads.
    #[test]
    fn is_dangerous_removal_path_rejects_a_linked_worktree_path() {
        let _lock = env_lock();
        let root = space_free_tempdir();
        let repo = root.path().join("repo");
        let target = root.path().join("real");
        std::fs::create_dir(&target).expect("create link target");
        let link = root.path().join("run-1");

        #[cfg(windows)]
        {
            let status = Command::new("cmd")
                .arg("/c")
                .arg("mklink")
                .arg("/J")
                .arg(&link)
                .arg(&target)
                .status()
                .expect("mklink should start");
            assert!(status.success(), "mklink /J should not need elevation");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat the link")
                .file_type()
                .is_symlink(),
            "the fixture must actually be a link, or this test proves nothing"
        );
        assert!(is_dangerous_removal_path(&link, &repo));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remove_refuses_a_dangerous_path() {
        let _lock = env_lock();
        let repo = init_space_free_repo();

        // The project root itself. A caller that got this wrong would delete the repository, and
        // `git worktree remove --force` is quite willing to be pointed at it.
        let error = remove(repo.path(), repo.path(), &[])
            .await
            .expect_err("removing the project root must be refused");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(
            repo.path().join("seed.txt").exists(),
            "the repository must survive"
        );
    }

    /// The case that used to lose data. `git worktree remove --force` refuses a *locked* worktree
    /// — removing one takes `--force` twice, which `try_remove_once` deliberately withholds — and
    /// the old `Err(_)` arm answered that refusal by deleting the directory anyway.
    #[tokio::test(flavor = "current_thread")]
    async fn orphan_collection_refuses_to_recurse_while_git_still_registers_the_worktree() {
        let _lock = env_lock();
        let pool = test_pool().await;
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('project-a', 'active', ?)")
            .bind(repo.path().to_string_lossy().as_ref())
            .execute(&pool)
            .await
            .unwrap();
        let orphan = create(repo.path(), Owner::Run(44))
            .await
            .expect("create worktree");
        assert!(git_ok(
            repo.path(),
            &[
                OsStr::new("worktree"),
                OsStr::new("lock"),
                orphan.path.as_os_str(),
            ],
        ));

        let collected = reconcile_orphaned_worktrees(&pool, Duration::ZERO, &[])
            .await
            .unwrap();

        assert_eq!(collected, 0);
        assert!(
            orphan.path.exists(),
            "a worktree git refused to remove must survive the sweep"
        );
        assert_eq!(
            registration(repo.path(), &orphan.path).await.unwrap(),
            Registration::Present
        );
    }

    /// Prune only drops rows whose directory is already missing, so running it before the
    /// filesystem fallback — which is what `remove` does — cannot clear anything. Left unpruned
    /// afterwards, the row outlives the directory and no later sweep ever sees it again.
    #[tokio::test(flavor = "current_thread")]
    async fn prune_after_a_filesystem_removal_clears_the_registration() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let root = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(root.path()));
        let info = create(repo.path(), Owner::Run(46))
            .await
            .expect("create worktree");
        assert_eq!(
            registration(repo.path(), &info.path).await.unwrap(),
            Registration::Present
        );

        tokio::fs::remove_dir_all(&info.path)
            .await
            .expect("remove the worktree directory");
        assert_eq!(
            registration(repo.path(), &info.path).await.unwrap(),
            Registration::Present
        );

        prune_and_verify_registration_gone(repo.path(), &info.path)
            .await
            .expect("prune should clear the row once the directory is gone");

        assert_eq!(
            registration(repo.path(), &info.path).await.unwrap(),
            Registration::Absent
        );
    }

    /// A repository standing on `master`, where `feat/x` has changed the same line. Merging either
    /// way conflicts.
    fn repo_with_a_conflict() -> tempfile::TempDir {
        let repo = init_space_free_repo();
        let path = repo.path();
        assert!(git_ok(
            path,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        std::fs::write(path.join("seed.txt"), "theirs\n").expect("write their side");
        assert!(git_ok(
            path,
            &[
                OsStr::new("commit"),
                OsStr::new("-am"),
                OsStr::new("theirs")
            ]
        ));
        assert!(git_ok(
            path,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        std::fs::write(path.join("seed.txt"), "ours\n").expect("write our side");
        assert!(git_ok(
            path,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("ours")]
        ));
        repo
    }

    /// The base is the whole reason `create_at` exists: a resolution's tree has to be born on the
    /// branch the merge was going INTO, and the project is by definition standing somewhere else —
    /// it is standing on whatever the person using it is working on.
    #[tokio::test]
    async fn a_worktree_can_be_born_on_a_branch_the_project_is_not_standing_on() {
        let _lock = env_lock();
        let repo = repo_with_a_conflict();
        let roots = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(roots.path()));

        let info = create_at(repo.path(), Owner::Run(41), Some("feat/x"))
            .await
            .expect("create the worktree on a named base");

        assert_eq!(
            git_stdout(&info.path, &[OsStr::new("rev-parse"), OsStr::new("HEAD")]),
            git_stdout(
                repo.path(),
                &[OsStr::new("rev-parse"), OsStr::new("feat/x")]
            ),
            "the tree has to be born where it was told, not where the project happens to stand"
        );
    }

    /// **The claim `git_exec::verify_resolution` is built on**: a resolver that does nothing but edit
    /// the conflicted files and commit produces a two-parent merge commit, with no step of its own to
    /// earn it. That is what makes "one parent means the merge was thrown away" a safe thing to
    /// refuse on — if the resolver had to do something special for the second parent, refusing would
    /// punish forgetting rather than catch discarding.
    #[tokio::test]
    async fn a_staged_conflict_commits_with_both_parents_and_the_resolver_does_nothing_to_earn_it()
    {
        let _lock = env_lock();
        let repo = repo_with_a_conflict();
        let roots = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(roots.path()));

        let info = create_at(repo.path(), Owner::Run(42), Some("master"))
            .await
            .expect("create the worktree on the target");
        stage_conflict(&info.path, "feat/x")
            .await
            .expect("a conflicting merge is what this stages");

        let conflicted = std::fs::read_to_string(info.path.join("seed.txt")).expect("read");
        assert!(
            conflicted.contains("<<<<<<<") && conflicted.contains(">>>>>>>"),
            "both sides have to be in front of whoever resolves them: {conflicted}"
        );
        assert_eq!(
            git_stdout(
                &info.path,
                &[OsStr::new("rev-parse"), OsStr::new("MERGE_HEAD")]
            ),
            git_stdout(
                repo.path(),
                &[OsStr::new("rev-parse"), OsStr::new("feat/x")]
            ),
            "the half-finished merge has to name the branch that was being brought in"
        );

        // Exactly what a resolver does and nothing more: edit the file, add, commit.
        std::fs::write(info.path.join("seed.txt"), "ours and theirs\n").expect("resolve");
        assert!(git_ok(&info.path, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &info.path,
            &[OsStr::new("commit"), OsStr::new("--no-edit")]
        ));

        let parents = git_stdout(
            &info.path,
            &[
                OsStr::new("rev-list"),
                OsStr::new("--parents"),
                OsStr::new("-1"),
                OsStr::new("HEAD"),
            ],
        );
        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "the commit itself plus two parents: {parents}"
        );
    }

    /// A conflict that has evaporated is not work, and an agent started against one would be asked to
    /// resolve an empty worktree. The queue can compute this merge by itself; whoever wants it
    /// published asks again.
    #[tokio::test]
    async fn a_merge_that_comes_out_clean_is_refused_rather_than_handed_to_an_agent() {
        let _lock = env_lock();
        let repo = init_space_free_repo();
        let path = repo.path();
        assert!(git_ok(
            path,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/elsewhere")
            ]
        ));
        std::fs::write(path.join("theirs.txt"), "theirs\n").expect("write their file");
        assert!(git_ok(path, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            path,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("theirs")]
        ));
        assert!(git_ok(
            path,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        std::fs::write(path.join("ours.txt"), "ours\n").expect("write our file");
        assert!(git_ok(path, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            path,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("ours")]
        ));

        let roots = space_free_tempdir();
        let _env = WorktreeRootEnv::set(Some(roots.path()));
        let info = create_at(path, Owner::Run(43), Some("master"))
            .await
            .expect("create the worktree on the target");

        let refused = stage_conflict(&info.path, "feat/elsewhere")
            .await
            .expect_err("a clean merge is not a conflict to resolve");
        assert!(
            refused.to_string().contains("merged cleanly"),
            "the refusal has to say the conflict was gone, not that staging broke: {refused}"
        );
    }
}
