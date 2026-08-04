//! Running git for the queue: argv, a deadline, and what it printed.
//!
//! Separate from `vcs.rs` because this is process transport and that is a queue —
//! `gate.rs` and `transcribe.rs` are each their own module for the same reason. Nothing here knows
//! what a request is or when it may run; it takes a repository, an argv and a deadline.

// Task 2 lands the process boundary before Tasks 3-7 build compute, publish and the worker loop on
// top of it, so every item here has a test caller and no production one — and `scripts/gates.sh`
// runs `cargo clippy --all-targets -- -D warnings` over this crate, which compiles the bin target
// without `cfg(test)` and so turns each of those into an error.
//
// **Delete this attribute in the same commit that gives these a production caller**, then fix what
// the compiler reports rather than putting it back: anything still dead once a caller exists is dead
// for a reason worth reading. If one item genuinely has no caller yet, narrow it to an
// `#[allow(dead_code)]` on that item carrying the reason — do not keep the blanket. `vcs.rs:17-30`
// is this same instruction, and says at length why the descriptive version of it rots.
#![cfg_attr(not(test), allow(dead_code))]

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

/// The budget for one whole queued operation — every git command it runs, added together.
///
/// **Per operation, not per command**, which spec §7 requires and which is not a detail: one merge
/// runs the better part of a dozen git invocations — worktree add, or abort + reset + clean; a
/// checkout; two rev-parses, three on the fast-forward route; the merge itself, plus an abort if it
/// conflicts; a worktree list; then either update-ref, or a fast-forward and possibly a status. A per-command ceiling of this size
/// would let a pathological operation hold the repository's only slot for the better part of an
/// hour, which is precisely the failure that spec row exists to prevent. Task 3 threads it as a
/// deadline.
///
/// Checked against `state.rs`'s `DEFAULT_PROGRESS_TIMEOUT` (300s) rather than against how long a
/// merge "should" take: a run waiting on this streams no events, and the progress timeout is what
/// kills a run that has gone quiet. An operation that outlives 300s has already outlived the caller
/// waiting on it, so waiting longer buys nobody anything and holds the slot while it does. Generous
/// for a merge, and finite, which is the property that matters — the same justification `gate.rs`'s
/// deadline has.
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(300);

/// How much of a command's output is kept for the row.
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// `Debug` because the deadline test's `expect_err` needs the `Ok` side printable, and every
/// caller of `run_git` is a `Result` whose failure a test will want to read.
#[derive(Debug)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    /// Standard output only, untruncated — callers parse object ids and porcelain out of this.
    pub stdout: String,
    /// stderr then stdout, truncated to the last `OUTPUT_TAIL_BYTES`. What the row records.
    pub output_tail: String,
}

impl CommandResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Runs one git command in `repo` and returns how it ended.
///
/// `Err` is reserved for "we could not find out": git would not start, or the deadline passed. A
/// non-zero exit is `Ok` — it is git's answer, and a conflicted merge arrives that way.
///
/// Output is buffered whole and truncated afterwards, unlike `gate.rs`, which streams into a
/// `TailBuffer`. That is not an oversight: a gate runs a test suite, which can print without bound
/// for as long as it likes, while a git command's output is bounded by the size of the change. The
/// two are different shapes, so this is a second implementation rather than a duplicate.
///
/// On a deadline the command future is dropped, and `kill_on_drop` takes the direct child down with
/// it. There is deliberately no third copy of `TreeKiller` (`gate.rs:328`, `runner.rs:422`, both
/// private to their modules): git spawns children — hooks, credential helpers, ssh — but nothing
/// here hands it a shell, and a third copy of a subtle process-lifetime guard is a worse bet than
/// the fourth copy problem it would solve. If a hook-spawned grandchild ever does hold this past its
/// deadline, the fix is to extract the shared one, not to paste it again.
pub async fn run_git(
    repo: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<CommandResult, String> {
    let mut command = crate::worktree::git();
    command.arg("-C").arg(repo).args(args).kill_on_drop(true);

    let output = match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(format!("could not run git: {error}")),
        Err(_) => {
            return Err(format!(
                "git {} timed out after {timeout:?}",
                rendered(args)
            ));
        }
    };

    Ok(CommandResult {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        output_tail: tail(&output.stderr, &output.stdout),
    })
}

/// The argv as a human reads it, for a message about a command that produced no output because it
/// never finished.
fn rendered(args: &[&OsStr]) -> String {
    args.iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// stderr first: git says what went wrong there, and a truncation that ate the diagnostic to keep
/// the progress output would be the wrong half.
fn tail(stderr: &[u8], stdout: &[u8]) -> String {
    let mut combined = String::from_utf8_lossy(stderr).into_owned();
    if !combined.is_empty() && !stdout.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&String::from_utf8_lossy(stdout));
    if combined.len() <= OUTPUT_TAIL_BYTES {
        return combined;
    }
    // Slicing a `String` off a byte offset panics mid-character, and lossy conversion guarantees
    // valid UTF-8 but says nothing about where the boundaries fall.
    let wanted = combined.len() - OUTPUT_TAIL_BYTES;
    let start = (wanted..combined.len())
        .find(|index| combined.is_char_boundary(*index))
        .unwrap_or(combined.len());
    combined[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    // `Path` explicitly, not via `use super::*`: at Step 1 this file contains nothing BUT this
    // module, so the glob imports nothing and the helpers below would not resolve it.
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn git_ok(dir: &Path, args: &[&OsStr]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    /// Cargo cannot link under a path containing a space, and the daemon refuses such a worktree
    /// root for the same reason (`worktree.rs:107-111`) — so a tempdir under the checkout, not the
    /// system temp directory, which on Windows is routinely under `C:\Users\Some Name\`.
    fn space_free_tempdir(prefix: &str) -> tempfile::TempDir {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(base)
            .expect("create space-free tempdir")
    }

    fn initialize_repo(repo: &Path) {
        std::fs::create_dir_all(repo).expect("create repository directory");
        assert!(git_ok(repo, &[OsStr::new("init")]));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new("test@x")
            ]
        ));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new("test")
            ]
        ));
        // Not optional, and not present in the copies this was taken from. Task 5 asserts a
        // checked-out file's bytes, and `core.autocrlf` is `true` from the system config on a
        // default Windows install — so git materialises `from the branch\n` as `...\r\n` and a
        // byte comparison fails on a correct implementation. The four existing `initialize_repo`
        // copies get away without it only because none of them reads a file git rewrote.
        assert!(git_ok(
            repo,
            &[
                OsStr::new("config"),
                OsStr::new("core.autocrlf"),
                OsStr::new("false")
            ]
        ));
        std::fs::write(repo.join("seed.txt"), "seed\n").expect("write seed file");
        assert!(git_ok(repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("seed")]
        ));
    }

    fn init_contained_repo(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let container = space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        (container, repo)
    }

    #[tokio::test]
    async fn a_git_command_reports_what_it_printed_and_how_it_exited() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-ok-");

        let result = run_git(
            &repo,
            &[OsStr::new("rev-parse"), OsStr::new("HEAD")],
            OPERATION_TIMEOUT,
        )
        .await
        .expect("git should run");

        assert!(result.succeeded());
        assert_eq!(result.stdout.trim().len(), 40, "a full object id");
    }

    /// A git command that exits non-zero is an answer, not an error of this call — the same
    /// distinction `VcsExecutor::execute` is documented around. An `Err` here would make a
    /// conflicted merge indistinguishable from git failing to start.
    #[tokio::test]
    async fn a_failing_git_command_is_a_result_rather_than_an_error() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-fail-");

        let result = run_git(
            &repo,
            &[OsStr::new("rev-parse"), OsStr::new("no-such-ref")],
            OPERATION_TIMEOUT,
        )
        .await
        .expect("git ran; it merely refused");

        assert!(!result.succeeded());
        assert!(
            !result.output_tail.is_empty(),
            "what git said is the only diagnostic the row will carry"
        );
    }

    /// `Duration::ZERO` takes the deadline branch without needing a slow git command to exist.
    ///
    /// The mechanism, stated precisely because "it is already elapsed" is not quite it: `timeout`
    /// polls the inner future first and the timer second, and tokio rounds a sleep deadline up to
    /// the next 1ms tick. So what this relies on is that a Windows `git` process cannot be spawned,
    /// executed and reaped inside a millisecond — a margin of one to two orders of magnitude, not a
    /// coin flip.
    #[tokio::test]
    async fn a_git_command_that_outlives_its_deadline_is_reported_rather_than_awaited() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-deadline-");

        let error = run_git(
            &repo,
            &[OsStr::new("rev-parse"), OsStr::new("HEAD")],
            std::time::Duration::ZERO,
        )
        .await
        .expect_err("a deadline that has already passed must not be waited through");

        assert!(error.contains("timed out"), "got: {error}");
    }
}
