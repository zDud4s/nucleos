//! §spec fila-vcs
//!
//! Running git for the queue: argv, a deadline, and what it printed.
//!
//! Separate from `vcs.rs` because this is process transport and that is a queue —
//! `gate.rs` and `transcribe.rs` are each their own module for the same reason. Nothing here knows
//! what a request is or when it may run; it takes a repository, an argv and a deadline.

// `Outcome` comes from `vcs.rs`, and that is the right direction of dependency: `git_exec` produces
// outcomes for the queue to record, and knows nothing about rows, claims or ordering.
use crate::execution::vcs::Outcome;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod changed;
mod compute;
mod executor;
mod ops;
mod publish;
mod repo;
mod resolution;
mod transport;
mod worktrees;

pub use self::changed::*;
pub use self::compute::*;
pub use self::executor::*;
use self::ops::*;
pub use self::publish::*;
pub use self::repo::*;
pub use self::resolution::*;
pub use self::transport::*;
pub use self::worktrees::*;

// `pub` on the TEST module only, so nothing about this module's production surface widens.
// `vcs.rs`'s end-to-end test needs a real repository with a branch to merge, and the alternative is a
// fifth copy of `init_contained_repo` — four already exist in this crate. Sharing the four helpers
// `vcs.rs` reaches for (`repo_with_a_branch_to_merge`, `space_free_tempdir`, `WorktreeRootEnv`,
// `sha_of`) is the smaller cost, and it keeps one definition of what a test repository looks like.
/// Fixtures the test module below uses and the binary's tests (`http.rs`) reach for too —
/// lifted out of `mod tests` by the lib/bin split so the `testkit` feature can expose them.
#[cfg(any(test, feature = "testkit"))]
#[allow(unused_imports)]
pub mod testkit {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub fn git_ok(dir: &Path, args: &[&OsStr]) -> bool {
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
    pub fn space_free_tempdir(prefix: &str) -> tempfile::TempDir {
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

    /// `pub` for `runs.rs`, which needs a real repository to test the approval path against —
    /// the same reason `space_free_tempdir` above is.
    pub fn initialize_repo(repo: &Path) {
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

    pub fn init_contained_repo(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let container = space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        (container, repo)
    }

    /// `master` with a `feat/x` that touched one file, and back on `master`. Every test in this task
    /// starts here.
    pub fn repo_with_a_branch_to_merge(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = init_contained_repo(prefix);
        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("-M"), OsStr::new("master")]
        ));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        std::fs::write(repo.join("feature.txt"), "from the branch\n").expect("write");
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("feature")
            ]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        (container, repo)
    }
}

#[cfg(test)]
pub mod tests {
    // A process-wide guard held across awaits on purpose: it serialises mutation of the shared
    // NUCLEOS_WORKTREE_ROOT override, and there is no multi-thread runtime here to starve.
    #![allow(clippy::await_holding_lock)]

    pub use super::testkit::*;
    use super::*;
    use std::ffi::{OsStr, OsString};
    // `Path` explicitly, not via `use super::*`: at Step 1 this file contains nothing BUT this
    // module, so the glob imports nothing and the helpers below would not resolve it.
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub struct WorktreeRootEnv {
        previous: Option<OsString>,
    }

    impl WorktreeRootEnv {
        // `pub` on the associated function as well as the struct: the struct alone gives
        // `error[E0624]: associated function 'set' is private` at `vcs.rs`'s call site.
        pub fn set(path: &Path) -> Self {
            let previous = std::env::var_os("NUCLEOS_WORKTREE_ROOT");
            unsafe {
                std::env::set_var("NUCLEOS_WORKTREE_ROOT", path);
            }
            Self { previous }
        }
    }

    impl Drop for WorktreeRootEnv {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("NUCLEOS_WORKTREE_ROOT", value),
                    None => std::env::remove_var("NUCLEOS_WORKTREE_ROOT"),
                }
            }
        }
    }

    pub fn sha_of(repo: &Path, revision: &str) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", revision])
            .output()
            .expect("git should start");
        assert!(output.status.success(), "rev-parse {revision} failed");
        String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_owned()
    }

    /// A fresh operation budget. The functions under test take a deadline rather than a duration —
    /// the budget is for the whole operation, not for each git command inside it.
    fn deadline() -> std::time::Instant {
        std::time::Instant::now() + OPERATION_TIMEOUT
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
        // Pins the WIRING, not just `tail`'s own contract: the streams have to reach it in the
        // order that keeps the diagnostic when a noisier command overruns the ceiling. Swapping the
        // two arguments at the call site restores the original defect and leaves every other
        // assertion in this module green — measured, not assumed.
        // `rev-parse` prints the unresolved argument on stdout and `fatal:` on stderr, so comparing
        // first occurrences is enough to tell the two orderings apart.
        let printed = result
            .output_tail
            .find("no-such-ref")
            .expect("what git printed belongs in the tail");
        let diagnostic = result
            .output_tail
            .find("fatal:")
            .expect("git's diagnostic belongs in the tail");
        assert!(
            printed < diagnostic,
            "stderr goes last, so a truncation drops stdout first; got: {}",
            result.output_tail
        );
    }

    /// A git killed at its deadline never deletes its `index.lock`, and the next `reset --hard` in
    /// that checkout then fails for ever. The lock planted here stands in for the one the killed
    /// process left; with nothing else of ours running there, the timeout takes it away.
    #[tokio::test]
    async fn a_timed_out_git_does_not_leave_its_index_lock_behind() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-lock-");
        let lock = repo.join(".git").join("index.lock");
        std::fs::write(&lock, "").expect("plant the stale lock");

        let error = run_git(
            &repo,
            &[
                OsStr::new("-c"),
                OsStr::new("alias.nap=!sleep 30"),
                OsStr::new("nap"),
            ],
            std::time::Duration::ZERO,
        )
        .await
        .expect_err("the deadline has already passed");

        assert!(error.contains("timed out"), "got: {error}");
        assert!(
            !lock.exists(),
            "the timed-out git's index.lock was left behind"
        );
    }

    /// `NUCLEOS_WORKTREE_ROOT` makes every project share one parent, so two projects whose leaf
    /// directory is `app` must still get different integration directories.
    #[test]
    fn two_projects_with_the_same_leaf_name_get_distinct_integration_worktrees() {
        let first = integration_worktree(Path::new("/work/one/app"));
        let second = integration_worktree(Path::new("/work/two/app"));
        assert_ne!(first.file_name(), second.file_name());
        assert_eq!(first, integration_worktree(Path::new("/work/one/app")));
    }

    /// A fetch or push never takes the index lock, so a lock present after its timeout is somebody
    /// else's and stays; an index-touching command's timeout still clears it.
    #[test]
    fn a_network_timeout_is_not_taken_for_a_killed_index_writer() {
        let argv = |parts: &[&str]| parts.iter().map(OsString::from).collect::<Vec<_>>();
        let held = |parts: &[&str]| {
            let owned = argv(parts);
            let refs: Vec<&OsStr> = owned.iter().map(|a| a.as_os_str()).collect();
            super::transport::may_have_held_index_lock(&refs)
        };
        assert!(!held(&["fetch", "origin"]));
        assert!(!held(&["-c", "http.x=1", "push", "origin", "main"]));
        assert!(!held(&["ls-remote", "origin"]));
        assert!(held(&["reset", "--hard"]));
        assert!(held(&["-c", "alias.nap=!sleep 30", "nap"]));
        assert!(held(&["merge", "fetch"]));
    }

    /// The control: a lock is only stale when nothing of ours is still running in that checkout.
    #[tokio::test]
    async fn a_timeout_leaves_the_index_lock_of_a_git_still_running_there() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-lock-live-");
        let lock = repo.join(".git").join("index.lock");
        std::fs::write(&lock, "").expect("plant the live lock");
        let _sibling = super::in_flight::Guard::enter(&repo);

        let error = run_git(
            &repo,
            &[
                OsStr::new("-c"),
                OsStr::new("alias.nap=!sleep 30"),
                OsStr::new("nap"),
            ],
            std::time::Duration::ZERO,
        )
        .await
        .expect_err("the deadline has already passed");

        assert!(error.contains("timed out"), "got: {error}");
        assert!(lock.exists(), "a running sibling's index.lock was removed");
    }

    /// A git command still running when its deadline passes is killed and reported, never awaited.
    ///
    /// The command is a shell alias that sleeps, so git cannot finish first on any platform. It used
    /// to be `rev-parse HEAD` under `Duration::ZERO`, which relied on git not being spawned, run and
    /// reaped inside tokio's first 1ms timer tick: true on Windows, false on Linux, where git won and
    /// the test failed (measured in WSL2 Ubuntu 24.04, 2026-09-12). Production is not exposed to it:
    /// every caller refuses a spent budget before spawning (`remaining`, and the
    /// `saturating_duration_since` check beside each direct `run_git` call), so `Duration::ZERO`
    /// reaches this function only from here.
    #[tokio::test]
    async fn a_git_command_that_outlives_its_deadline_is_reported_rather_than_awaited() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-deadline-");

        let error = run_git(
            &repo,
            &[
                OsStr::new("-c"),
                OsStr::new("alias.nap=!sleep 30"),
                OsStr::new("nap"),
            ],
            std::time::Duration::ZERO,
        )
        .await
        .expect_err("a deadline that has already passed must not be waited through");

        assert!(error.contains("timed out"), "got: {error}");
    }

    /// **The test above is about the transport; this one is about the budget, and only this one can
    /// see the difference.** `run_git` takes a `Duration` and is told how long to wait; everything
    /// production runs goes through `git` or `add_worktree`, which take a *deadline* and compute what
    /// is left of it here. Replacing this function's body with `let budget = OPERATION_TIMEOUT;` —
    /// handing every command a fresh full 300s — left the suite byte-identical at 1064 passed before
    /// this test existed, and what it ships is precisely the failure `OPERATION_TIMEOUT` is written
    /// to prevent: a stalled merge (a `post-merge` hook in a repository the daemon does not control,
    /// an antivirus scan, an auto-`gc`) holding the repository's only slot for 300s *per command*
    /// across the dozen this operation runs, which is the better part of an hour.
    ///
    /// Five seconds rather than a tight bound: the question is which quantity is being returned, not
    /// scheduler jitter, and any ceiling below `OPERATION_TIMEOUT` tells the two apart.
    #[test]
    fn a_command_is_given_what_is_left_of_the_operation_s_budget_and_never_a_fresh_one() {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);

        let budget = remaining(deadline, "merge feat/x").expect("a live deadline still has budget");

        assert!(
            budget <= Duration::from_secs(5),
            "the budget is what is left of the operation, not a fresh {OPERATION_TIMEOUT:?} for each command; got {budget:?}"
        );
    }

    /// The other half of the same property, and the one `git`'s doc comment is about: a spent budget
    /// is refused *before* a child is spawned, because `Command::output()` spawns eagerly and a
    /// `merge` or an `update-ref` killed mid-flight is a worse thing to do to a repository than
    /// declining to start.
    ///
    /// `Instant::now()` as the deadline rather than `now() - 1s`: the clock is monotonic, so by the
    /// time `remaining` reads it again the budget is exactly zero — and subtracting from an `Instant`
    /// is what panics on a platform whose epoch is boot.
    #[test]
    fn a_spent_operation_budget_refuses_the_next_command_rather_than_starting_it() {
        let spent = std::time::Instant::now();

        let outcome = remaining(spent, "merge --no-ff feat/x")
            .expect_err("a budget that is gone must not start another command");

        match outcome {
            Outcome::Failed {
                reason,
                exit_code,
                output_tail,
            } => {
                assert!(reason.contains("ran out of time before"), "got: {reason}");
                assert!(
                    reason.contains("merge --no-ff feat/x"),
                    "the reason names the command that never started: {reason}"
                );
                assert_eq!(exit_code, None, "nothing ran, so nothing exited");
                assert!(
                    output_tail.is_empty(),
                    "no command printed anything: {output_tail}"
                );
            }
            other => panic!("a spent budget reports a Failed, got {other:?}"),
        }
    }

    /// And the same property through a caller, because a call site that *bypasses* `remaining` is
    /// invisible to both unit tests above — the function they exercise is still correct. Measured
    /// with the one call site that can hide it: replacing `add_worktree`'s `remaining(deadline,
    /// "worktree add")?` with `OPERATION_TIMEOUT` leaves the whole suite green except this test,
    /// which then reports the same `ran out of time before` (the *next* command refuses) while a
    /// worktree has already been added. So it is the second assertion, not the reason, that carries
    /// this one.
    ///
    /// The repository is real and mergeable on purpose — what this pins is that an operation whose
    /// budget is already gone stops before its FIRST command rather than starting work nobody is
    /// waiting for any more.
    #[tokio::test]
    async fn an_operation_whose_budget_is_gone_computes_nothing() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-spent-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let outcome = compute_merge(&repo, "feat/x", "master", std::time::Instant::now())
            .await
            .expect_err("an operation out of time must not start a merge");

        match outcome {
            Outcome::Failed { reason, .. } => {
                assert!(reason.contains("ran out of time before"), "got: {reason}")
            }
            other => panic!("a spent budget reports a Failed, got {other:?}"),
        }
        assert!(
            !integration_worktree(&repo).exists(),
            "the first command never ran, so no worktree was added"
        );
    }

    /// The case the ordering exists for, and the one a hand-measurement caught rather than a test:
    /// a diagnostic one line long behind an stdout large enough to force the truncation. `clean -fd`
    /// prints a line per file and a large merge a diffstat, so the size is reachable from argv this
    /// chunk plans, in repositories the daemon does not control.
    #[test]
    fn a_diagnostic_survives_an_stdout_large_enough_to_truncate_it() {
        let noisy = "Removing some/long/path/to/a/file.txt\n".repeat(1024);
        assert!(noisy.len() > OUTPUT_TAIL_BYTES, "must force a truncation");

        let kept = tail(
            noisy.as_bytes(),
            b"CONFLICT (content): Merge conflict in seed.txt\n",
        );

        assert!(
            kept.contains("CONFLICT"),
            "the only diagnostic the row carries must not be the first thing dropped"
        );
        assert!(
            kept.starts_with("…[output truncated; showing tail]\n"),
            "a bare tail cannot be told apart from a complete output; got: {kept:.80}"
        );
    }

    /// Nothing is marked when nothing was dropped — the marker has to mean something.
    #[test]
    fn output_that_fits_is_returned_whole_and_unmarked() {
        let kept = tail(b"stdout line\n", b"stderr line\n");

        assert_eq!(kept, "stdout line\n\nstderr line\n");
    }

    #[tokio::test]
    async fn a_merge_is_computed_where_the_user_is_not_standing() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-compute-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let before = sha_of(&repo, "master");
        let computed = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("the merge should compute");

        assert_eq!(computed.old, before);
        assert_ne!(computed.new, before, "a merge commit was created");

        // The two halves of the promise: the object exists, and the branch has not moved.
        assert_eq!(sha_of(&repo, "master"), before, "compute must not publish");
        assert_eq!(
            sha_of(&repo, &format!("{}^1", computed.new)),
            before,
            "the merge commit's first parent is the old target — this is what makes publishing a fast-forward"
        );
        assert!(
            !repo.join("feature.txt").exists(),
            "the user's working copy never saw the merge"
        );
    }

    /// Two branches that changed the same line, and a `feat/x` that will be asked to land as a
    /// resolution. What each test does to `feat/x` before asking is what it is testing.
    fn repo_with_a_conflict(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = init_contained_repo(prefix);
        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("-M"), OsStr::new("master")]
        ));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        std::fs::write(repo.join("seed.txt"), "theirs\n").expect("write");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-am"),
                OsStr::new("theirs")
            ]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        std::fs::write(repo.join("seed.txt"), "ours\n").expect("write");
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("ours")]
        ));
        (container, repo)
    }

    async fn land_as_resolution(repo: &Path) -> Outcome {
        use crate::execution::vcs::VcsExecutor;
        GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: true,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await
    }

    /// **The failure the whole verification exists for.** A resolver that keeps one side and throws
    /// the other away produces a branch that compiles, passes every test, and is indistinguishable
    /// from a good resolution by any measure the suite has. What it cannot fake is the shape: the
    /// daemon left a merge half-finished, so a real resolution commits on top of it and carries two
    /// parents for free. One parent means the merge was discarded and something else put in its
    /// place, and that is refused **without reading the content**, because the content is exactly
    /// what a flattened resolution gets right.
    #[tokio::test]
    async fn a_resolution_that_flattened_the_merge_is_refused_without_reading_it() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_conflict("nucleos-gitexec-flat-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // A single-parent commit on `feat/x` holding content that looks perfectly resolved.
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("feat/x")]
        ));
        std::fs::write(repo.join("seed.txt"), "ours\ntheirs\n").expect("write");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-am"),
                OsStr::new("looks resolved, is not a merge")
            ]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        let before = sha_of(&repo, "master");

        match land_as_resolution(&repo).await {
            Outcome::Escalated { reason, .. } => assert!(
                reason.contains("fewer than two parents"),
                "the reason has to say what was wrong with the SHAPE: {reason}"
            ),
            other => panic!("a flattened resolution must not merge, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "master"),
            before,
            "nothing may be published when the resolution is refused"
        );
    }

    /// An agent that stopped halfway commits the markers without noticing. The file then holds both
    /// sides, the suite may well pass — markers landing inside a comment or a string break nothing —
    /// and the merge would publish something no human wrote.
    #[tokio::test]
    async fn a_resolution_that_committed_its_conflict_markers_is_refused() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_conflict("nucleos-gitexec-markers-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // A real merge, left conflicted, then committed as-is: two parents AND markers.
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("feat/x")]
        ));
        assert!(!git_ok(&repo, &[OsStr::new("merge"), OsStr::new("master")],));
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("--no-edit")]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        let before = sha_of(&repo, "master");

        match land_as_resolution(&repo).await {
            Outcome::Escalated { reason, .. } => assert!(
                reason.contains("conflict markers"),
                "the reason has to name what is still in the tree: {reason}"
            ),
            other => panic!("committed markers must not merge, got {other:?}"),
        }
        assert_eq!(sha_of(&repo, "master"), before);
    }

    /// And the check applies to resolutions ONLY. An ordinary landing has one parent on its tip as a
    /// matter of course, so applying this to every branch anybody asked to land would refuse almost
    /// all of them — which is why `from_resolution` travels with the claim at all.
    #[tokio::test]
    async fn an_ordinary_landing_is_not_held_to_the_resolution_shape() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_conflict("nucleos-gitexec-ordinary-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // `feat/x` as any session leaves it: one parent, no merge in it. Its content conflicts with
        // master, so the expected answer is the ordinary escalation for a conflict — reached by
        // computing the merge, NOT by the shape check refusing it first.
        use crate::execution::vcs::VcsExecutor;
        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match outcome {
            Outcome::Escalated { reason, .. } => assert!(
                !reason.contains("fewer than two parents"),
                "an ordinary landing must never be judged on the resolution shape: {reason}"
            ),
            other => panic!("a conflict is an Escalated, got {other:?}"),
        }
    }

    /// **A merge the project's gate refuses is not published.**
    ///
    /// The load-bearing assertion is the last one: `master` still stands where it stood. A version
    /// of this that read only the `Outcome` would pass with the merge published and the row simply
    /// lying about it — which is the failure mode of every "it was refused" claim that never looks
    /// at the thing the refusal was supposed to prevent.
    ///
    /// And nothing is reverted anywhere, because nothing was ever published: the measurement happens
    /// on the computed merge while the target has not moved.
    #[tokio::test]
    async fn um_merge_que_o_gate_recusa_nao_e_publicado() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-red-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(
            container.path(),
            "gate_before_publish: true\ngate_command: git rev-parse --verify nao-existe\n",
        );
        let before = sha_of(&repo, "master");

        match land_ordinarily(&repo, container.path()).await {
            Outcome::Failed {
                reason,
                output_tail,
                ..
            } => {
                assert!(reason.contains("gate"), "{reason}");
                assert!(
                    !output_tail.is_empty(),
                    "the gate's own words are what make it fixable; without them the row says only \
                     that something was refused"
                );
            }
            other => panic!("a red gate must refuse the merge, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "master"),
            before,
            "master moved even though the gate refused it"
        );
    }

    /// The green half. Without it the test above passes just as well for a repository where merging
    /// never worked at all.
    #[tokio::test]
    async fn um_merge_que_o_gate_aceita_e_publicado() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-green-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        // `git --version` is the gate that always agrees, and it needs no shell to run.
        write_autopilot_rules(
            container.path(),
            "gate_before_publish: true\ngate_command: git --version\n",
        );
        let before = sha_of(&repo, "master");

        let published = match land_ordinarily(&repo, container.path()).await {
            Outcome::Succeeded { sha, .. } => sha.expect("a merge names the commit it published"),
            other => panic!("a green gate must let the merge through, got {other:?}"),
        };
        assert_ne!(sha_of(&repo, "master"), before, "nothing was published");
        assert_eq!(sha_of(&repo, "master"), published);
    }

    /// A gated merge leaves its measurement in `verify_runs`, under the request that asked for it.
    #[tokio::test]
    async fn o_gate_de_um_merge_fica_em_verify_runs() {
        use crate::execution::vcs::VcsExecutor;

        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-rec-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(
            container.path(),
            "gate_before_publish: true
gate_command: git --version
",
        );
        let pool = crate::testdb::fresh_pool().await;
        let outcome = GitExecutor {
            machine_root: Some(container.path().to_path_buf()),
            pool: Some(pool.clone()),
            ..GitExecutor::default()
        }
        .execute(&crate::execution::vcs::ClaimedRequest {
            id: 41,
            op: crate::execution::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            project_id: "alpha".to_owned(),
            project_root: repo.to_string_lossy().into_owned(),
            from_resolution: false,
            run_id: None,
            approved_map_blob: None,
            integration_branch: None,
        })
        .await;
        assert!(matches!(outcome, Outcome::Succeeded { .. }), "{outcome:?}");

        let (origin, origin_id, status, project_id, sha): (
            String,
            Option<i64>,
            String,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as("SELECT origin, origin_id, status, project_id, sha FROM verify_runs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(origin, crate::verify::runs::ORIGIN_MERGE);
        assert_eq!(origin_id, Some(41));
        assert_eq!(status, crate::verify::runs::STATUS_PASSED);
        assert_eq!(project_id.as_deref(), Some("alpha"));
        assert!(sha.is_some(), "the merge commit that was measured");
    }

    /// **The key is off by default, and that is what makes this free for every project that never
    /// asked for it.**
    ///
    /// The gate configured here is one that always refuses. Without `gate_before_publish` it is
    /// never run at all, and the merge lands exactly as it landed before this existed — which is a
    /// stronger statement than "the default is false", because it is measured through the same door
    /// the two tests above go through.
    #[tokio::test]
    async fn sem_a_chave_o_gate_nem_sequer_corre() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-off-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(
            container.path(),
            "gate_command: git rev-parse --verify nao-existe\n",
        );
        let before = sha_of(&repo, "master");

        match land_ordinarily(&repo, container.path()).await {
            Outcome::Succeeded { .. } => {}
            other => panic!("an ungated merge must land untouched, got {other:?}"),
        }
        assert_ne!(sha_of(&repo, "master"), before, "nothing was published");
    }

    /// Asking for a measurement and naming nothing to measure with is refused, not ignored.
    ///
    /// Ignoring it is the tempting arm and the wrong one: the queue would go on publishing
    /// unmeasured while the file says it does not, and a brake that reads as engaged and is not is
    /// worse than no brake — it is the one nobody thinks to check.
    #[tokio::test]
    async fn pedir_medida_sem_gate_command_e_recusado() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-nocmd-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(container.path(), "gate_before_publish: true\n");
        let before = sha_of(&repo, "master");

        match land_ordinarily(&repo, container.path()).await {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("gate_command"),
                "the refusal must name the key that is missing: {reason}"
            ),
            other => panic!("a gate asked for and not named must refuse, got {other:?}"),
        }
        assert_eq!(sha_of(&repo, "master"), before, "master moved");
    }

    /// Writes project `alpha`'s rules where the queue reads them: `<machine_root>/projects/alpha/`,
    /// with `machine_root` a temporary directory standing in for `~/.nucleos`.
    fn write_autopilot_rules(machine_root: &Path, contents: &str) {
        let path = crate::project::state::file(
            Some(machine_root),
            "alpha",
            crate::project::state::AUTOPILOT_FILE,
        )
        .expect("alpha names a directory");
        std::fs::create_dir_all(path.parent().expect("the rules file sits in a folder"))
            .expect("create the rules folder");
        std::fs::write(path, contents).expect("write the rules");
    }

    /// An ordinary landing of `feat/x` into `master`, through the real executor.
    async fn land_ordinarily(repo: &Path, machine_root: &Path) -> Outcome {
        use crate::execution::vcs::VcsExecutor;
        GitExecutor {
            machine_root: Some(machine_root.to_path_buf()),
            ..GitExecutor::default()
        }
        .execute(&crate::execution::vcs::ClaimedRequest {
            id: 1,
            op: crate::execution::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            project_id: "alpha".to_owned(),
            project_root: repo.to_string_lossy().into_owned(),
            from_resolution: false,
            run_id: None,
            approved_map_blob: None,
            integration_branch: None,
        })
        .await
    }

    // ---- F3-6: `gate_before_publish` combined with `gate_after_land` ----------------------------
    //
    // Every fixture pairs a RED `gate_command` with a GREEN map group (or the reverse), so the
    // verdict itself says which gate ran: the red `gate_command` only fails when the full gate
    // ran, and the map group only runs under `verify scope`.

    const RED_GATE: &str = "git rev-parse --verify nao-existe";
    const GREEN_GATE: &str = "git --version";
    const BOTH_SWITCHES: &str = "gate_before_publish: true\ngate_after_land: true\n";

    /// Commits a `nucleos.tests.yaml` on `master` whose single group watches `feature.txt`, the
    /// file `feat/x` adds, so `map_change` stays `None` and the scope covers the merge's diff.
    fn commit_map_on_master(repo: &Path, group_command: &str) {
        std::fs::write(
            repo.join(crate::tests_map::MAP_FILE),
            format!(
                "version: 1\ntests:\n  groups:\n    feature:\n      paths: [feature.txt]\n      \
                 check: {group_command}\n      command: {group_command}\n"
            ),
        )
        .expect("write the map");
        assert!(git_ok(repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("add the map")
            ]
        ));
    }

    fn verify_executor(
        pool: &sqlx::SqlitePool,
        machine_root: &Path,
    ) -> std::sync::Arc<crate::verify::exec::Executor> {
        let executor = crate::verify::exec::Executor::new(
            pool.clone(),
            crate::config::VerifyConfig::default(),
            Some(machine_root.to_path_buf()),
        );
        tokio::spawn(crate::verify::exec::run_executor(executor.clone()));
        executor
    }

    async fn roster(pool: &sqlx::SqlitePool, repo: &Path) {
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) \
             VALUES ('alpha', 'active', ?)",
        )
        .bind(repo.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .expect("roster alpha");
    }

    async fn land_with(
        repo: &Path,
        machine_root: &Path,
        pool: Option<sqlx::SqlitePool>,
        verify: Option<std::sync::Arc<crate::verify::exec::Executor>>,
        integration_branch: Option<&str>,
        id: i64,
    ) -> Outcome {
        use crate::execution::vcs::VcsExecutor;
        GitExecutor {
            machine_root: Some(machine_root.to_path_buf()),
            pool,
            verify,
            ..GitExecutor::default()
        }
        .execute(&crate::execution::vcs::ClaimedRequest {
            id,
            op: crate::execution::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            project_id: "alpha".to_owned(),
            project_root: repo.to_string_lossy().into_owned(),
            from_resolution: false,
            run_id: None,
            approved_map_blob: None,
            integration_branch: integration_branch.map(crate::execution::vcs::Branch::from),
        })
        .await
    }

    async fn request_callers(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT caller FROM verify_requests ORDER BY id")
            .fetch_all(pool)
            .await
            .expect("read verify_requests")
    }

    async fn merge_gate_rows(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM verify_runs WHERE origin = ?")
            .bind(crate::verify::runs::ORIGIN_MERGE)
            .fetch_one(pool)
            .await
            .expect("count verify_runs")
    }

    /// With `gate_after_land` off (the default) nothing about the merge gate changes, even with a
    /// valid map and an executor installed: the full gate runs and the verify queue is not asked.
    #[tokio::test]
    async fn with_gate_after_land_off_the_merge_takes_the_full_gate_even_with_a_map() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-off-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, GREEN_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("gate_before_publish: true\ngate_command: {RED_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());
        let before = sha_of(&repo, "master");

        let outcome = land_with(
            &repo,
            container.path(),
            Some(pool.clone()),
            Some(executor),
            Some("master"),
            7,
        )
        .await;

        assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
        assert!(request_callers(&pool).await.is_empty(), "no verify request");
        assert_eq!(sha_of(&repo, "master"), before);
        assert!(
            crate::verify::postgate::load(&pool, "alpha")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Both switches and a valid map at the merge: the gate is a `verify scope` named
    /// `merge:<id>`, and the (red) `gate_command` never runs.
    #[tokio::test]
    async fn with_both_switches_and_a_map_the_merge_is_gated_by_scope() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-scope-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, GREEN_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {RED_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());

        let outcome = land_with(
            &repo,
            container.path(),
            Some(pool.clone()),
            Some(executor),
            Some("master"),
            7,
        )
        .await;

        assert!(matches!(outcome, Outcome::Succeeded { .. }), "{outcome:?}");
        assert_eq!(request_callers(&pool).await, vec!["merge:7".to_owned()]);
        assert_eq!(merge_gate_rows(&pool).await, 0, "the full gate did not run");
        assert!(
            crate::verify::postgate::load(&pool, "alpha")
                .await
                .unwrap()
                .is_none(),
            "a scope covers nothing for the post-merge gate"
        );
    }

    /// A red `scope` refuses the merge, and the target does not move.
    #[tokio::test]
    async fn a_red_scope_gate_refuses_the_merge() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-red-scope-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, RED_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {GREEN_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());
        let before = sha_of(&repo, "master");

        let outcome = land_with(
            &repo,
            container.path(),
            Some(pool.clone()),
            Some(executor),
            Some("master"),
            7,
        )
        .await;

        assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
        assert_eq!(request_callers(&pool).await, vec!["merge:7".to_owned()]);
        assert_eq!(sha_of(&repo, "master"), before, "master moved");
    }

    /// Both switches, no map: the full gate runs once, and after the publish the published SHA is
    /// recorded as covered, against the right target.
    #[tokio::test]
    async fn with_both_switches_and_no_map_the_full_gate_covers_the_postgate() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-nomap-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {GREEN_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());

        let outcome = land_with(
            &repo,
            container.path(),
            Some(pool.clone()),
            Some(executor),
            Some("master"),
            7,
        )
        .await;

        let published = match outcome {
            Outcome::Succeeded { sha, .. } => sha.expect("a merge names the commit it published"),
            other => panic!("a green full gate must publish, got {other:?}"),
        };
        assert_eq!(merge_gate_rows(&pool).await, 1);
        let state = crate::verify::postgate::load(&pool, "alpha")
            .await
            .unwrap()
            .expect("the published merge is recorded as covered");
        assert_eq!(state.last_green_sha.as_deref(), Some(published.as_str()));
        assert_eq!(state.target, "master");
    }

    /// A red full gate refuses the merge and marks nothing covered.
    #[tokio::test]
    async fn a_red_full_gate_marks_nothing_covered() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-redfull-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {RED_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());
        let before = sha_of(&repo, "master");

        let outcome = land_with(
            &repo,
            container.path(),
            Some(pool.clone()),
            Some(executor),
            Some("master"),
            7,
        )
        .await;

        assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
        assert_eq!(sha_of(&repo, "master"), before);
        assert!(
            crate::verify::postgate::load(&pool, "alpha")
                .await
                .unwrap()
                .is_none(),
            "a refused merge covers nothing"
        );
    }

    /// A `scope` that cannot be submitted (here: the project is not rostered, so `submit` refuses)
    /// falls back to the full gate, and what ran is the full gate, so it is covered.
    #[tokio::test]
    async fn an_unsubmittable_scope_falls_back_to_the_full_gate_and_covers_it() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-unavail-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, GREEN_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {GREEN_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        // No roster row on purpose.
        let executor = verify_executor(&pool, container.path());

        let outcome = land_with(
            &repo,
            container.path(),
            Some(pool.clone()),
            Some(executor),
            Some("master"),
            7,
        )
        .await;

        let published = match outcome {
            Outcome::Succeeded { sha, .. } => sha.expect("a merge names the commit it published"),
            other => panic!("the fallback to the full gate must publish, got {other:?}"),
        };
        assert_eq!(merge_gate_rows(&pool).await, 1, "the full gate ran");
        let state = crate::verify::postgate::load(&pool, "alpha")
            .await
            .unwrap()
            .expect("the full gate covered the published merge");
        assert_eq!(state.last_green_sha.as_deref(), Some(published.as_str()));
    }

    /// F3-9: a `scope` that is submitted but reaches no verdict in time (nothing drains the verify
    /// queue here, so its units stay `queued`) is not a reason to refuse the merge: the full gate
    /// runs instead, and what ran is the full gate, so it is covered.
    #[tokio::test]
    async fn a_scope_that_reaches_no_verdict_falls_back_to_the_full_gate_and_covers_it() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-slow-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, GREEN_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {GREEN_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        // No `run_executor`: the scope's units are accepted and never run.
        let executor = crate::verify::exec::Executor::new(
            pool.clone(),
            crate::config::VerifyConfig::default(),
            Some(container.path().to_path_buf()),
        );
        compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute the merge the gate measures");

        let gate = gate_the_merge(
            Some(container.path()),
            Some(&pool),
            7,
            "alpha",
            &repo,
            &integration_worktree(&repo),
            Duration::from_secs(120),
            Duration::from_millis(300),
            Some("master"),
            Some(&executor),
        )
        .await;

        assert!(matches!(gate, Ok(MergeGate::FullCovering)), "{gate:?}");
        assert_eq!(request_callers(&pool).await, vec!["merge:7".to_owned()]);
        assert_eq!(merge_gate_rows(&pool).await, 1, "the full gate ran");
    }

    /// F3-10: when the scope reaches no verdict and the full gate measures the merge instead, the
    /// scope's units still `queued` are cancelled, so they never run beside the full gate.
    #[tokio::test]
    async fn a_scope_that_reaches_no_verdict_cancels_its_queued_units() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-cancel-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, GREEN_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {GREEN_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        // No `run_executor`: the scope's units are accepted and never run.
        let executor = crate::verify::exec::Executor::new(
            pool.clone(),
            crate::config::VerifyConfig::default(),
            Some(container.path().to_path_buf()),
        );
        compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute the merge the gate measures");

        let gate = gate_the_merge(
            Some(container.path()),
            Some(&pool),
            7,
            "alpha",
            &repo,
            &integration_worktree(&repo),
            Duration::from_secs(120),
            Duration::from_millis(300),
            Some("master"),
            Some(&executor),
        )
        .await;

        assert!(matches!(gate, Ok(MergeGate::FullCovering)), "{gate:?}");
        let still_queued: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM verify_runs WHERE origin = ? AND status = 'queued'",
        )
        .bind(crate::verify::runs::ORIGIN_VERIFY)
        .fetch_one(&pool)
        .await
        .expect("count queued verify units");
        assert_eq!(
            still_queued, 0,
            "no scope unit may stay queued beside the full gate"
        );
        let cancelled: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT status, output_tail FROM verify_runs WHERE origin = ?")
                .bind(crate::verify::runs::ORIGIN_VERIFY)
                .fetch_all(&pool)
                .await
                .expect("read the scope's units");
        assert!(!cancelled.is_empty(), "the scope queued at least one unit");
        for (status, tail) in cancelled {
            assert_eq!(status, "errored");
            assert!(
                tail.as_deref().is_some_and(|t| t.contains("full gate")),
                "the reason names the full gate: {tail:?}"
            );
        }
    }

    /// F3-10: a map the merge rewrote (it differs from the project root's copy) is not trusted, so
    /// the merge takes the full gate and the verify queue is not asked.
    #[tokio::test]
    async fn a_merge_whose_map_differs_from_the_root_takes_the_full_gate() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-mapdiff-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, GREEN_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {GREEN_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());
        compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute the merge the gate measures");
        let integration = integration_worktree(&repo);
        let map = integration.join(crate::tests_map::MAP_FILE);
        let mut text = std::fs::read_to_string(&map).expect("the integration tree carries the map");
        text.push_str("# changed in the integration worktree\n");
        std::fs::write(&map, text).expect("rewrite the map");

        let gate = gate_the_merge(
            Some(container.path()),
            Some(&pool),
            7,
            "alpha",
            &repo,
            &integration,
            Duration::from_secs(120),
            Duration::from_secs(30),
            Some("master"),
            Some(&executor),
        )
        .await;

        assert!(matches!(gate, Ok(MergeGate::FullCovering)), "{gate:?}");
        assert!(
            request_callers(&pool).await.is_empty(),
            "an untrusted map must not reach the verify queue"
        );
        assert_eq!(merge_gate_rows(&pool).await, 1, "the full gate ran");
    }

    /// F3-10: a script the map's groups run that the merge changed (it differs from the root's
    /// copy) is not trusted either: the merge takes the full gate.
    #[tokio::test]
    async fn a_merge_that_changes_a_script_the_map_runs_takes_the_full_gate() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-script-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        std::fs::write(repo.join("probe.txt"), "original\n").expect("write the probe");
        commit_map_on_master(&repo, "git hash-object probe.txt");
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {GREEN_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());
        compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute the merge the gate measures");
        let integration = integration_worktree(&repo);
        std::fs::write(integration.join("probe.txt"), "changed by the merge\n")
            .expect("rewrite the probe in the integration tree");

        let gate = gate_the_merge(
            Some(container.path()),
            Some(&pool),
            7,
            "alpha",
            &repo,
            &integration,
            Duration::from_secs(120),
            Duration::from_secs(30),
            Some("master"),
            Some(&executor),
        )
        .await;

        assert!(matches!(gate, Ok(MergeGate::FullCovering)), "{gate:?}");
        assert!(
            request_callers(&pool).await.is_empty(),
            "a changed script must not reach the verify queue"
        );
        assert_eq!(merge_gate_rows(&pool).await, 1, "the full gate ran");
    }

    /// A merge that is not into the land target (`integration_branch` names another branch) keeps
    /// today's full gate, asks the verify queue nothing and marks nothing covered.
    #[tokio::test]
    async fn a_merge_off_the_land_target_keeps_the_full_gate() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-combine-offtarget-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        commit_map_on_master(&repo, GREEN_GATE);
        write_autopilot_rules(
            container.path(),
            &format!("{BOTH_SWITCHES}gate_command: {RED_GATE}\n"),
        );
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        let executor = verify_executor(&pool, container.path());

        let outcome = land_with(
            &repo,
            container.path(),
            Some(pool.clone()),
            Some(executor),
            Some("main"),
            7,
        )
        .await;

        assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
        assert!(request_callers(&pool).await.is_empty(), "no verify request");
        assert!(
            crate::verify::postgate::load(&pool, "alpha")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Spec §7, first row. A conflict is not a problem this module solves — and the point of
    /// computing on the side is that the user's copy is not where it happens.
    ///
    /// **It is `Escalated` and no longer `Failed`.** The distinction is who is left holding it:
    /// `failed` says the operation did not happen and that is the end, which leaves the conflict
    /// with whoever asked — and for a landing that is the agent which had just finished its work,
    /// the one actor this queue exists to spare from other sessions' integration.
    #[tokio::test]
    async fn a_conflicted_merge_escalates_without_touching_the_user_s_copy() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = init_contained_repo("nucleos-gitexec-conflict-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // Two branches that change the same line of the same file, in different ways.
        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("-M"), OsStr::new("master")]
        ));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        std::fs::write(repo.join("seed.txt"), "theirs\n").expect("write");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-am"),
                OsStr::new("theirs")
            ]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        std::fs::write(repo.join("seed.txt"), "ours\n").expect("write");
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("ours")]
        ));

        let before = sha_of(&repo, "master");
        let outcome = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect_err("a conflict must not produce a merge commit");

        match outcome {
            Outcome::Escalated {
                output_tail,
                reason,
                ..
            } => {
                assert!(
                    output_tail.contains("CONFLICT"),
                    "the row must carry what git said: {output_tail}"
                );
                // The asker's instinct on reading "merging failed" is to go and fix it, and that is
                // the one thing here that is not theirs: the merge ran in an integration worktree
                // they do not have and was aborted, so there is no conflicted state anywhere to
                // resolve — only the temptation to manufacture one. The reason has to say so, and
                // has to say what IS theirs instead.
                assert!(
                    reason.contains("not yours to fix"),
                    "the reason must name the owner: {reason}"
                );
                assert!(
                    reason.contains("no copy was left conflicted"),
                    "the reason must say there is nothing to resolve: {reason}"
                );
                assert!(
                    reason.contains("a resolution is minted"),
                    "a refusal that does not say what happens next sends the asker looking: {reason}"
                );
                // The alternative this used to name was unactionable in BOTH directions — the
                // reverse merge is the same conflict, and a session's `git merge` is computed here
                // and aborted here just the same. Naming it cost a session two hours and three
                // escalated rows. Pinned as an absence, because the sentence read as helpful.
                assert!(
                    !reason.contains("in your own worktree"),
                    "the reason must not prescribe a merge the queue computes the same way: {reason}"
                );
            }
            other => panic!("a conflict is an Escalated, got {other:?}"),
        }

        assert_eq!(sha_of(&repo, "master"), before);
        assert_eq!(
            std::fs::read_to_string(repo.join("seed.txt")).expect("read"),
            "ours\n",
            "the user's file is byte-identical"
        );
    }

    /// **A merge git refused to start is not a conflict**, and the two used to arrive the same way.
    ///
    /// Escalating is a promise that a resolution can be minted, so the promise was made for a
    /// branch name that does not exist. Measured: request 91 on this repository, `merge feat into
    /// feat/calendario`, printed `merge: feat - not something we can merge`, escalated, was given
    /// resolution run 900436 within the minute, and that run failed three seconds later with
    /// nothing staged. The row is now permanently asking a person to look at a conflict that never
    /// existed.
    #[tokio::test]
    async fn a_merge_git_refused_to_start_is_a_failure_and_not_a_conflict() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-nosuch-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let outcome = compute_merge(&repo, "feat", "master", deadline())
            .await
            .expect_err("a name no branch has cannot produce a merge");

        match outcome {
            Outcome::Failed {
                reason,
                output_tail,
                ..
            } => {
                assert!(
                    output_tail.contains("not something we can merge"),
                    "the row must carry what git said: {output_tail}"
                );
                assert!(
                    reason.contains("did not happen at all"),
                    "the reason must not read as a conflict: {reason}"
                );
                assert!(
                    reason.contains("the request that is wrong"),
                    "the reason must send the asker at the request rather than at the branches: \
                     {reason}"
                );
            }
            // The specific wrong answer this test exists for. `Escalated` here mints an agent for a
            // worktree that will have nothing staged in it.
            other => panic!("a merge that never started must not escalate, got {other:?}"),
        }
    }

    /// A repository whose two sides of one file disagree about line endings, which is what makes a
    /// three-line change conflict from the first line to the last.
    ///
    /// `nul` puts a NUL byte in every version of the file, which is the measured shape rather than a
    /// contrived one: a NUL makes git classify the file as binary, no end-of-line filter ever runs
    /// on it, and its blob keeps whatever endings it was written with while every sibling in the
    /// checkout is normalised.
    fn repo_with_endings_that_disagree(prefix: &str, nul: bool) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = init_contained_repo(prefix);
        let byte = if nul { "\0" } else { "" };
        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("-M"), OsStr::new("master")]
        ));
        std::fs::write(repo.join("f.ts"), format!("one\ntwo{byte}\nthree\nfour\n")).expect("write");
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("base")]
        ));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        // The branch changes one line and keeps the endings it was given.
        std::fs::write(
            repo.join("f.ts"),
            format!("one\ntwo{byte} CHANGED\nthree\nfour\n"),
        )
        .expect("write");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-am"),
                OsStr::new("branch")
            ]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        // The target's copy was rewritten with CRLF at some point — a Windows editor, a tool, a
        // checkout that filtered what it should not have. Every line now differs from the base's,
        // so it overlaps whatever the branch did.
        std::fs::write(
            repo.join("f.ts"),
            format!("one\r\ntwo{byte}\r\nthree\r\nfour CHANGED\r\n"),
        )
        .expect("write");
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("crlf")]
        ));
        (container, repo)
    }

    /// **The diagnostic that cost a day.** A conflict covering a whole file says nothing about why,
    /// and the reason is not in the markers: it is in the two blobs, which the queue is holding and
    /// nobody downstream is. So it says so where both readers look — the row, and the tail the
    /// resolution prompt quotes as git's own account.
    #[tokio::test]
    async fn a_conflict_whose_sides_disagree_about_line_endings_says_so() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_endings_that_disagree("nucleos-gitexec-eol-", false);
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let outcome = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect_err("sides that share no line ending cannot merge");

        match outcome {
            Outcome::Escalated { output_tail, .. } => {
                assert!(
                    output_tail.contains("f.ts: master has CRLF, feat/x has LF"),
                    "the note must name the file and which side has which: {output_tail}"
                );
                assert!(
                    output_tail.contains("Normalise one side"),
                    "a diagnosis without the one thing to do about it is half a note: {output_tail}"
                );
                assert!(
                    !output_tail.contains("NUL"),
                    "there is no NUL in this file, and a cause named where there is none sends the \
                     reader hunting for it: {output_tail}"
                );
                assert!(
                    output_tail.contains("CONFLICT"),
                    "the note is added to what git said, not instead of it: {output_tail}"
                );
            }
            other => panic!("a conflict is an Escalated, got {other:?}"),
        }
    }

    /// The byte behind the endings, named — because it is the cause, it is invisible, and it is the
    /// half nobody guesses.
    ///
    /// This is the measured case rather than a constructed one: `shell/src/preview/daemon.ts` held a
    /// NUL, so git classified it as binary, so `core.autocrlf` never touched it, so its blob stayed
    /// CRLF while every sibling was LF, so a three-line change conflicted end to end. Two
    /// resolution agents were defeated by it before the byte was found.
    #[tokio::test]
    async fn the_nul_byte_behind_the_endings_is_named_when_there_is_one() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_endings_that_disagree("nucleos-gitexec-eol-nul-", true);
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let outcome = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect_err("sides that share no line ending cannot merge");

        match outcome {
            Outcome::Escalated { output_tail, .. } => {
                assert!(
                    output_tail.contains("f.ts: master has CRLF, feat/x has LF"),
                    "the endings are still the finding: {output_tail}"
                );
                assert!(
                    output_tail.contains("both sides contain a NUL byte"),
                    "the cause has to be named, or the reader normalises the file and never learns \
                     why it drifted: {output_tail}"
                );
            }
            other => panic!("a conflict is an Escalated, got {other:?}"),
        }
    }

    /// A file that is binary in earnest gets no note, and the guard is not decoration: newline
    /// counts inside a PNG are an accident of its bytes, and an escalation is the wrong place to
    /// print an accident. The escape for a text file with a NUL in it is that its two answers are
    /// PURE and opposite — all CRLF against all LF — which random bytes essentially never are.
    #[tokio::test]
    async fn a_conflict_in_a_file_that_is_binary_in_earnest_gets_no_endings_note() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = init_contained_repo("nucleos-gitexec-binary-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("-M"), OsStr::new("master")]
        ));
        std::fs::write(repo.join("f.bin"), b"\0seed\n").expect("write");
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("base")]
        ));
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        // Not a line ending in sight that means anything: one side happens to hold a CR before one
        // of its newlines, the other does not.
        std::fs::write(repo.join("f.bin"), b"\0a\nb\nc\n").expect("write");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-am"),
                OsStr::new("theirs")
            ]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        std::fs::write(repo.join("f.bin"), b"\0a\r\nb\n").expect("write");
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("ours")]
        ));

        let outcome = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect_err("two different binaries cannot be merged");

        match outcome {
            Outcome::Escalated { output_tail, .. } => assert!(
                !output_tail.contains("the queue's note"),
                "a note about the line endings of a binary file is noise inside an escalation: \
                 {output_tail}"
            ),
            other => panic!("a binary conflict is still a conflict, got {other:?}"),
        }
    }

    /// The classification, alone, because the interesting answers are the ones no fixture is
    /// convenient for.
    #[test]
    fn how_a_blob_ends_its_lines_is_read_out_of_the_blob() {
        assert_eq!(Endings::of("one\r\ntwo\r\n"), Endings::Crlf);
        assert_eq!(Endings::of("one\ntwo\n"), Endings::Lf);
        assert_eq!(Endings::of("one\r\ntwo\n"), Endings::Mixed);
        assert_eq!(
            Endings::of("one line and no ending"),
            Endings::Absent,
            "a file with nothing to disagree about must not be reported as disagreeing"
        );
        assert_eq!(Endings::of(""), Endings::Absent);
        // A lone CR is deliberately not counted. Nothing in this decade writes them, and treating
        // one as an ending would classify a file by a stray byte in the middle of a line.
        assert_eq!(Endings::of("one\rtwo\n"), Endings::Lf);
    }

    /// The listing is parsed on the TAB, not on whitespace, because a path with a space in it is
    /// exactly what a naive split loses — and it loses it silently, by reading the path's first word
    /// as the whole path.
    #[test]
    fn the_two_sides_of_a_conflict_are_read_out_of_the_index_listing() {
        let listing = "100644 aaa 1\tf.ts\n\
                       100644 bbb 2\tf.ts\n\
                       100644 ccc 3\tf.ts\n\
                       100644 ddd 1\tgone.txt\n\
                       100644 eee 2\tgone.txt\n\
                       100644 fff 2\tsrc/two words.ts\n\
                       100644 ggg 3\tsrc/two words.ts\n";

        assert_eq!(
            sides(listing),
            vec![
                (
                    "f.ts".to_owned(),
                    Some("bbb".to_owned()),
                    Some("ccc".to_owned())
                ),
                // A delete/modify conflict: one side and the base, so there is no second set of
                // endings to compare and the caller must skip it rather than compare with nothing.
                ("gone.txt".to_owned(), Some("eee".to_owned()), None),
                (
                    "src/two words.ts".to_owned(),
                    Some("fff".to_owned()),
                    Some("ggg".to_owned())
                ),
            ]
        );
    }

    /// `--no-ff` is what makes publishing uniform, and this is the case that proves it is doing
    /// something: without it git would fast-forward and there would be no merge commit to publish.
    #[tokio::test]
    async fn a_merge_that_could_fast_forward_still_produces_a_commit_to_publish() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-noff-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let computed = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("the merge should compute");

        assert_eq!(
            sha_of(&repo, &format!("{}^2", computed.new)),
            sha_of(&repo, "feat/x"),
            "a real merge commit with two parents, not a fast-forwarded branch tip"
        );
    }

    /// Nothing to do is not a failure. The queue must not report an error for a merge somebody
    /// already landed by hand.
    #[tokio::test]
    async fn a_merge_with_nothing_left_to_do_succeeds_without_moving_anything() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-uptodate-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // master already contains feat/x.
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("merge"),
                OsStr::new("--no-ff"),
                OsStr::new("-m"),
                OsStr::new("m"),
                OsStr::new("feat/x")
            ]
        ));
        let before = sha_of(&repo, "master");

        let computed = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("already-merged is not an error");

        assert_eq!(computed.old, before);
        assert_eq!(
            computed.new, before,
            "nothing to merge means nothing to publish"
        );
    }

    /// Runs a plain git command in `repo` and requires it to succeed. Test setup only.
    fn git_must(repo: &Path, args: &[&str]) {
        let os_args: Vec<&OsStr> = args.iter().map(|a| OsStr::new(*a)).collect();
        assert!(git_ok(repo, &os_args), "git {args:?} failed in {repo:?}");
    }

    /// Queues a `Revert` of `merge_sha` on `master` through the real executor.
    async fn revert_through_the_queue(repo: &Path, merge_sha: &str) -> Outcome {
        revert_approved(repo, merge_sha, None).await
    }

    /// The same, carrying the map blob the owner approved (if any).
    async fn revert_approved(repo: &Path, merge_sha: &str, approved: Option<&str>) -> Outcome {
        use crate::execution::vcs::VcsExecutor;
        GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Revert {
                    merge_sha: merge_sha.into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: approved.map(str::to_owned),
                integration_branch: None,
            })
            .await
    }

    /// Lands `feat/x` plus a `nucleos.tests.yaml` committed on it into `master` with a real
    /// `--no-ff` merge, and returns the merge commit: reverting it deletes the map.
    fn land_a_merge_that_adds_the_map(repo: &Path) -> String {
        git_must(repo, &["checkout", "feat/x"]);
        std::fs::write(repo.join(crate::tests_map::MAP_FILE), "version: 1\n").expect("write map");
        git_must(repo, &["add", "-A"]);
        git_must(repo, &["commit", "-m", "add the map on the branch"]);
        git_must(repo, &["checkout", "master"]);
        git_must(repo, &["merge", "--no-ff", "-m", "land", "feat/x"]);
        sha_of(repo, "master")
    }

    /// F3-9: a revert whose merge added the test map would delete it, and that is a map change
    /// like any other: it waits for the owner, and the target does not move.
    #[tokio::test]
    async fn a_revert_that_changes_the_test_map_waits_for_the_owner() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-revert-map-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        let landed = land_a_merge_that_adds_the_map(&repo);

        match revert_through_the_queue(&repo, &landed).await {
            Outcome::AwaitingOwner { map_blob, .. } => assert_eq!(map_blob, DELETED_MAP),
            other => panic!("a revert that changes the map must wait for the owner, got {other:?}"),
        }

        assert_eq!(sha_of(&repo, "master"), landed, "master moved");
    }

    /// F3-9: the same revert, with the owner's approval of exactly that content, publishes.
    #[tokio::test]
    async fn an_approved_revert_of_a_map_change_publishes() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-revert-map-ok-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        let landed = land_a_merge_that_adds_the_map(&repo);

        match revert_approved(&repo, &landed, Some(DELETED_MAP)).await {
            Outcome::Succeeded { sha, .. } => {
                let reverted = sha.expect("a revert names what it published");
                assert_ne!(reverted, landed);
                assert_eq!(sha_of(&repo, "master"), reverted);
            }
            other => panic!("an approved revert must publish, got {other:?}"),
        }

        assert!(
            !repo.join(crate::tests_map::MAP_FILE).exists(),
            "the map is gone from the checkout"
        );
    }

    /// D4: a revert is computed in the integration worktree and published by the same
    /// compare-and-swap as a merge. The new commit sits on the old tip, and it puts the tree back
    /// to what the merge's first parent had.
    #[tokio::test]
    async fn a_revert_publishes_a_revert_of_the_merge_by_compare_and_swap() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-revert-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let landed = match land_ordinarily(&repo, container.path()).await {
            Outcome::Succeeded { sha, .. } => sha.expect("a merge names what it published"),
            other => panic!("the merge to revert must land first, got {other:?}"),
        };
        assert_eq!(sha_of(&repo, "master"), landed);
        assert!(repo.join("feature.txt").exists());

        let reverted = match revert_through_the_queue(&repo, &landed).await {
            Outcome::Succeeded { sha, .. } => sha.expect("a revert names what it published"),
            other => panic!("a revert of a merge on the target must publish, got {other:?}"),
        };

        assert_ne!(reverted, landed);
        assert_eq!(
            sha_of(&repo, "master"),
            reverted,
            "the target now points at the revert"
        );
        assert_eq!(
            sha_of(&repo, &format!("{reverted}^1")),
            landed,
            "the revert sits on the old tip, so the swap was against the tip it computed on"
        );
        assert_eq!(
            sha_of(&repo, &format!("{reverted}^{{tree}}")),
            sha_of(&repo, &format!("{landed}^1^{{tree}}")),
            "reverting the merge restores the tree its first parent had"
        );
        assert!(
            !repo.join("feature.txt").exists(),
            "the checkout that stands on the target moves with it"
        );
    }

    /// A commit that is not on the target is refused up front and nothing moves: a revert is only
    /// ever of something that landed.
    #[tokio::test]
    async fn a_revert_of_a_commit_not_on_the_target_is_refused_and_nothing_moves() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-revert-off-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        let before = sha_of(&repo, "master");
        // `feat/x` was never merged, so its tip is not an ancestor of master.
        let stranger = sha_of(&repo, "feat/x");

        match revert_through_the_queue(&repo, &stranger).await {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("nothing was reverted"),
                "the refusal says nothing moved: {reason}"
            ),
            other => panic!("a commit off the target must be refused, got {other:?}"),
        }

        assert_eq!(sha_of(&repo, "master"), before, "master moved");
        assert!(
            !repo.join("feature.txt").exists(),
            "the working copy was touched"
        );
    }

    /// The correction branch is only ever CREATED: a second call adopts the branch it made itself,
    /// and a branch somebody else made under that name is refused and left exactly as it was.
    #[tokio::test]
    async fn the_fix_branch_is_created_once_and_never_overwritten() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-fixbranch-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // A merge, then its revert, both made by hand on master: the sha under test is the revert.
        let seed = sha_of(&repo, "master");
        git_must(&repo, &["merge", "--no-ff", "-m", "merge feat/x", "feat/x"]);
        let merge = sha_of(&repo, "master");
        git_must(&repo, &["revert", "-m", "1", "--no-edit", "HEAD"]);
        let revert = sha_of(&repo, "master");

        let branch = "fix/feat-x-1a2b3c4";
        let tip = prepare_fix_branch(&repo, &revert, branch, deadline())
            .await
            .expect("the first call creates the branch");

        assert_eq!(sha_of(&repo, &format!("refs/heads/{branch}")), tip);
        assert_eq!(
            sha_of(&repo, &format!("{tip}^1")),
            revert,
            "the branch's first commit is built on the revert"
        );
        assert_eq!(
            sha_of(&repo, &format!("{tip}^{{tree}}")),
            sha_of(&repo, &format!("{merge}^{{tree}}")),
            "the revert of the revert puts the merge's tree back"
        );
        assert_ne!(seed, tip);

        let again = prepare_fix_branch(&repo, &revert, branch, deadline())
            .await
            .expect("a second call adopts the branch it made");
        assert_eq!(again, tip, "adopted, not recreated");

        // A branch of somebody else's under the same name is never moved.
        let foreign = "fix/feat-x-aaaaaaa";
        git_must(&repo, &["branch", foreign, &seed]);
        let error = prepare_fix_branch(&repo, &revert, foreign, deadline())
            .await
            .expect_err("a branch that is not ours must be refused");
        assert!(!error.is_empty());
        assert_eq!(
            sha_of(&repo, &format!("refs/heads/{foreign}")),
            seed,
            "the foreign branch was moved"
        );
    }

    /// Spec §7 ("worktree de integração suja — reposta antes da operação seguinte") and §8, which
    /// lists it as a required lifecycle test.
    ///
    /// This is the branch that runs on **every operation after the first** in a project's life, and
    /// no other test in this chunk reaches it: each of the others starts from a fresh worktree root
    /// and computes exactly once, so they all take the create path. Without this test the reset path
    /// ships unexecuted.
    #[tokio::test]
    async fn a_dirty_integration_worktree_is_reset_before_the_next_operation() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-reset-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("the first compute creates the worktree");

        // Left exactly as a killed operation would leave it: HEAD on the merge commit, a tracked
        // file modified, and untracked litter. `feature.txt` is the one that bites — `master` does
        // not have it, so the next `checkout --detach master` has to delete it, and git refuses to
        // delete a file with local modifications. Without the reset, the second compute fails at its
        // first command.
        let integration = integration_worktree(&repo);
        std::fs::write(integration.join("feature.txt"), "half-applied\n").expect("write");
        std::fs::write(integration.join("litter.txt"), "left behind\n").expect("write");

        let computed = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("a dirty integration worktree must not fail the next operation");

        assert_ne!(
            computed.new, computed.old,
            "the second merge still computed"
        );
        assert!(
            !integration.join("litter.txt").exists(),
            "untracked litter is cleaned too, or it accumulates for the life of the project"
        );
    }

    /// The post-merge gate's tree: created detached at the asked sha, and on reuse reset, cleaned
    /// and moved to the next sha. It never touches a branch ref, so it is not a vcs-queue operation.
    #[tokio::test]
    async fn the_postgate_worktree_is_created_and_reset_at_the_target_sha() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-postgate-");
        let roots = space_free_tempdir("nucleos-gitexec-pgwt-");
        let _env = WorktreeRootEnv::set(roots.path());
        let master = sha_of(&repo, "master");
        let feature = sha_of(&repo, "feat/x");
        assert_ne!(master, feature);

        let path = prepare_postgate_worktree(&repo, &master, deadline())
            .await
            .expect("the first prepare creates the worktree");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("postgate-repo")
        );
        assert_eq!(sha_of(&path, "HEAD"), master);
        assert!(!path.join("feature.txt").exists());

        // Left as a killed gate would leave it: a tracked file modified and untracked litter.
        std::fs::write(path.join("seed.txt"), "half-applied\n").expect("write");
        std::fs::write(path.join("litter.txt"), "left behind\n").expect("write");

        let again = prepare_postgate_worktree(&repo, &feature, deadline())
            .await
            .expect("a dirty postgate worktree must not fail the next gate");
        assert_eq!(again, path, "the same directory is reused");
        assert_eq!(sha_of(&path, "HEAD"), feature, "moved to the new sha");
        assert!(!path.join("litter.txt").exists(), "litter is cleaned");
        assert_eq!(
            std::fs::read_to_string(path.join("seed.txt")).expect("seed"),
            "seed\n",
            "tracked edits are reset"
        );
        assert!(path.join("feature.txt").exists());
    }

    /// The proof tree of `verify::probe` is its own directory, apart from the post-merge gate's.
    #[tokio::test]
    async fn the_probe_worktree_is_its_own_directory_and_moves_to_the_sha() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-probe-");
        let roots = space_free_tempdir("nucleos-gitexec-pbwt-");
        let _env = WorktreeRootEnv::set(roots.path());
        let master = sha_of(&repo, "master");
        let feature = sha_of(&repo, "feat/x");

        let postgate = prepare_postgate_worktree(&repo, &feature, deadline())
            .await
            .expect("postgate tree");
        let probe = prepare_probe_worktree(&repo, &master, deadline())
            .await
            .expect("probe tree");

        assert_ne!(postgate, probe);
        assert_eq!(
            probe.file_name().and_then(|n| n.to_str()),
            Some("probe-repo")
        );
        assert_eq!(sha_of(&probe, "HEAD"), master);
        assert_eq!(
            sha_of(&postgate, "HEAD"),
            feature,
            "the postgate tree is untouched"
        );

        // Dirty, then re-prepared: the same directory, reset.
        std::fs::write(
            probe.join("litter.txt"),
            "left behind
",
        )
        .expect("write");
        let again = prepare_probe_worktree(&repo, &feature, deadline())
            .await
            .expect("re-prepare");
        assert_eq!(again, probe);
        assert_eq!(sha_of(&probe, "HEAD"), feature);
        assert!(!probe.join("litter.txt").exists());
    }

    /// The bisection's candidate list: the first-parent commits of `(base, tip]`, oldest first,
    /// with the commits of a merged side branch left out and a flag on whoever changes the map.
    #[tokio::test]
    async fn first_parent_commits_lists_the_range_oldest_first_and_flags_map_changes() {
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-fpc-");
        let commit = |name: &str, content: &str, message: &str| {
            std::fs::write(repo.join(name), content).expect("write");
            assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
            assert!(git_ok(
                &repo,
                &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new(message)]
            ));
            sha_of(&repo, "HEAD")
        };
        let base = sha_of(&repo, "master");
        let c1 = commit("one.txt", "1\n", "c1");
        // A merge whose side branch carries a commit of its own: only the merge is first-parent.
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("merge"),
                OsStr::new("--no-ff"),
                OsStr::new("-m"),
                OsStr::new("merge feat/x"),
                OsStr::new("feat/x")
            ]
        ));
        let merge = sha_of(&repo, "HEAD");
        let side = sha_of(&repo, "feat/x");
        let c3 = commit(
            crate::tests_map::MAP_FILE,
            "version: 1\n",
            "c3 adds the map",
        );

        let listed = first_parent_commits(&repo, &base, &c3, deadline())
            .await
            .expect("the range lists");
        assert_eq!(
            listed,
            vec![
                (c1.clone(), false),
                (merge.clone(), false),
                (c3.clone(), true)
            ],
            "oldest first, the side branch's own commit absent, only the map change flagged"
        );
        assert!(listed.iter().all(|(sha, _)| *sha != side));

        // An empty range lists nothing, and a base git cannot resolve is an error.
        assert!(
            first_parent_commits(&repo, &c3, &c3, deadline())
                .await
                .expect("an empty range")
                .is_empty()
        );
        assert!(
            first_parent_commits(&repo, "no-such-ref", &c3, deadline())
                .await
                .is_err()
        );
    }

    /// The `.git` guard, and the reason it must run before any git command is pointed at that
    /// directory. `git -C <dir>` walks UP until it finds a repository, so a directory that is not a
    /// worktree does not make `reset --hard` fail — it makes it succeed against whatever repository
    /// encloses it, reverting every uncommitted tracked change there and exiting 0.
    ///
    /// **The enclosure is built on purpose, never borrowed from the ambient checkout.**
    /// `space_free_tempdir` puts these directories inside this very repository, so a regression that
    /// removed the guard would otherwise revert the developer's own uncommitted work instead of
    /// reporting a failure. A repository of our own, between the plain directory and the real one,
    /// stops the walk-up at something we are allowed to lose.
    #[tokio::test]
    async fn a_directory_that_is_not_a_worktree_is_refused_before_git_is_pointed_at_it() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-guard-");

        // The sacrificial repository, left in the three states the commands behind the guard would
        // each destroy: mid-conflict (what `merge --abort` clears), one tracked file modified (what
        // `reset --hard` reverts) and one untracked file (what a `clean` given a wider scope would
        // remove). `work.txt` is committed before the conflict exists, so neither the merge nor its
        // abort has any business touching it — it moves only if `reset --hard` walked up.
        let (_enclosure, enclosing) = init_contained_repo("nucleos-gitexec-enclosing-");
        std::fs::write(enclosing.join("work.txt"), "committed\n").expect("write");
        assert!(git_ok(&enclosing, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &enclosing,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("work")]
        ));
        assert!(git_ok(
            &enclosing,
            &[OsStr::new("branch"), OsStr::new("-M"), OsStr::new("master")]
        ));
        assert!(git_ok(
            &enclosing,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        std::fs::write(enclosing.join("seed.txt"), "theirs\n").expect("write");
        assert!(git_ok(
            &enclosing,
            &[
                OsStr::new("commit"),
                OsStr::new("-am"),
                OsStr::new("theirs")
            ]
        ));
        assert!(git_ok(
            &enclosing,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        std::fs::write(enclosing.join("seed.txt"), "ours\n").expect("write");
        assert!(git_ok(
            &enclosing,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("ours")]
        ));
        assert!(
            !git_ok(&enclosing, &[OsStr::new("merge"), OsStr::new("feat/x")]),
            "the setup needs the conflict, so that there is a merge left to abort"
        );
        let merge_head = enclosing.join(".git").join("MERGE_HEAD");
        assert!(merge_head.exists(), "the enclosing repository is mid-merge");
        std::fs::write(enclosing.join("work.txt"), "uncommitted work\n").expect("write");
        std::fs::write(enclosing.join("untracked.txt"), "not in the index\n").expect("write");

        // The worktree root, and so the integration directory, INSIDE that repository.
        let roots = enclosing.join("roots");
        let _env = WorktreeRootEnv::set(&roots);
        let integration = integration_worktree(&repo);
        std::fs::create_dir_all(&integration).expect("a directory that is not a worktree");

        let outcome = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect_err("a directory that is not a worktree must be refused, not used");

        match outcome {
            // The shape `vcs.rs`'s `Outcome::Unexecutable` doc already predicts for exactly this
            // case: the row was executable and the environment was not, so an EMPTY tail — `failed`
            // with `output_tail IS NOT NULL` — rather than the NULL that means the row itself could
            // not be executed.
            Outcome::Failed {
                reason,
                exit_code,
                output_tail,
            } => {
                assert!(reason.contains("is not a git worktree"), "got: {reason}");
                assert_eq!(exit_code, None, "nothing ran, so nothing exited");
                assert!(
                    output_tail.is_empty(),
                    "no command printed anything: {output_tail}"
                );
            }
            other => panic!("the guard reports a Failed, got {other:?}"),
        }

        // The two that carry the property: each goes red if the guard is deleted, or moved below the
        // command it stands in front of.
        assert!(
            merge_head.exists(),
            "the guard runs BEFORE `merge --abort`, which would otherwise have cleared this"
        );
        assert_eq!(
            std::fs::read_to_string(enclosing.join("work.txt")).expect("read"),
            "uncommitted work\n",
            "`reset --hard` walked up and reverted the enclosing repository's uncommitted work"
        );
        // Weaker than those two, and kept for what it pins rather than what it catches today:
        // `clean -fd` is cwd-relative, so it could only ever reach this file if the argv gained a
        // path or the command were pointed somewhere wider.
        assert!(
            enclosing.join("untracked.txt").exists(),
            "`clean` reached beyond the directory it was pointed at"
        );
        assert!(
            integration.exists(),
            "the guard reports the directory and never removes it — a wrong removal is the thing it exists to avoid"
        );
    }

    /// The other half of that guard, and the half `is_file()` is the whole point of.
    ///
    /// The test above builds a directory with no `.git` at all, so it cannot tell an existence check
    /// from `is_file()` — weakening the guard to mere existence leaves it green (measured: `git_exec`
    /// 19 passed, `vcs` 40 passed). A `.git` DIRECTORY here is a different animal: not a broken
    /// worktree but a standalone repository somebody put where the daemon wants to work, which under
    /// `NUCLEOS_WORKTREE_ROOT` is a directory a human picked and so not far-fetched. It passes an
    /// existence check, and the three commands behind the guard then land inside it — `reset --hard`
    /// reverting their uncommitted work and `clean -fd` removing their untracked files, in a
    /// repository nobody named.
    ///
    /// **No sacrificial enclosure here, unlike the test above, and the difference is the point.**
    /// There the directory was not a repository at all, so every `git -C` walked UP and had to be
    /// stopped at something we are allowed to lose. Here the directory IS a repository, so the walk
    /// stops inside it by construction — the damage a weakened guard does is to this repository, and
    /// that is exactly what the assertions below read.
    #[tokio::test]
    async fn a_standalone_repository_where_the_integration_worktree_goes_is_refused_rather_than_reset()
     {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-standalone-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // Somebody's own repository, exactly where the integration worktree belongs, holding the two
        // things `reset --hard` and `clean -fd` would each take.
        let integration = integration_worktree(&repo);
        initialize_repo(&integration);
        assert!(
            integration.join(".git").is_dir(),
            "a standalone repository has a `.git` DIRECTORY — a linked worktree has a `.git` file, and telling the two apart is what this guard does"
        );
        std::fs::write(integration.join("seed.txt"), "somebody's work\n").expect("write");
        std::fs::write(integration.join("untracked.txt"), "not in the index\n").expect("write");

        let outcome = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect_err("a repository that is not our worktree must be refused, not reset");

        match outcome {
            Outcome::Failed { reason, .. } => {
                assert!(reason.contains("is not a git worktree"), "got: {reason}")
            }
            other => panic!("the guard reports a Failed, got {other:?}"),
        }

        // The two that carry the property, and the two that survive the guard merely being MOVED
        // below the commands it stands in front of — where the reason above still reads correctly.
        assert_eq!(
            std::fs::read_to_string(integration.join("seed.txt")).expect("read"),
            "somebody's work\n",
            "`reset --hard` reverted uncommitted work in a repository the daemon does not own"
        );
        assert!(
            integration.join("untracked.txt").exists(),
            "`clean -fd` removed a file from a repository the daemon does not own"
        );
    }

    /// The target is a branch nobody has checked out — `release`, not `master`. That is the case this
    /// row of §6.3 is for, and it is common: an agent merging into a branch no human is standing on.
    #[tokio::test]
    async fn a_branch_nobody_has_open_is_moved_by_compare_and_swap() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-cas-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("release")]
        ));

        // Captured before the *compute*, not merely before the publish: `compute_merge` checks
        // `release` out in the integration worktree, and taking the reading here pins that it does
        // so without disturbing the branch the user is standing on either.
        //
        // Compared against itself afterwards. Comparing `master` to `HEAD` instead — the shape this
        // assertion started as — cannot fail: in the main checkout `HEAD` is a symref to
        // `refs/heads/master`, so the two resolve to the same object however far a publish strayed.
        // It proved a symref is a symref.
        let master_before = sha_of(&repo, "master");

        let computed = compute_merge(&repo, "feat/x", "release", deadline())
            .await
            .expect("compute");
        let new = computed.new.clone();

        let outcome = publish(&repo, "release", computed, deadline()).await;

        match outcome {
            Outcome::Succeeded { sha, .. } => assert_eq!(sha.as_deref(), Some(new.as_str())),
            other => panic!("expected a published merge, got {other:?}"),
        }
        assert_eq!(sha_of(&repo, "release"), new, "the branch moved");
        assert_eq!(
            sha_of(&repo, "master"),
            master_before,
            "publishing into `release` must not move the branch the user is standing on"
        );
    }

    /// The second lock doing its job. Between computing and publishing, something else moved the ref —
    /// another daemon, a human in a terminal. Overwriting would silently discard their commit.
    #[tokio::test]
    async fn a_target_that_moved_between_computing_and_publishing_is_refused() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-raced-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("release")]
        ));

        let computed = compute_merge(&repo, "feat/x", "release", deadline())
            .await
            .expect("compute");

        // Somebody else moves `release` while the merge was being computed.
        let elsewhere = sha_of(&repo, "feat/x");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("update-ref"),
                OsStr::new("refs/heads/release"),
                OsStr::new(&elsewhere)
            ]
        ));

        let outcome = publish(&repo, "release", computed, deadline()).await;

        match outcome {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("moved"),
                "the reason must say what to do about it: {reason}"
            ),
            other => panic!("a raced publish is a Failed, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "release"),
            elsewhere,
            "the other party's commit is still there — nothing was overwritten"
        );
    }

    /// The ordinary case, and the one §6.1 is about: HEAD, the index and the files move together, so
    /// `git status` tells the truth immediately afterwards.
    #[tokio::test]
    async fn a_branch_somebody_has_open_is_fast_forwarded_in_place() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-ff-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        let computed = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute");
        let new = computed.new.clone();

        let outcome = publish(&repo, "master", computed, deadline()).await;

        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert_eq!(sha_of(&repo, "HEAD"), new, "HEAD moved");
        assert_eq!(sha_of(&repo, "master"), new, "and so did the branch");
        assert_eq!(
            std::fs::read_to_string(repo.join("feature.txt")).expect("read"),
            "from the branch\n",
            "and so did the files — this is what publishing separately buys"
        );
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["status", "--porcelain"])
            .output()
            .expect("git");
        assert!(
            status.stdout.is_empty(),
            "nothing shows as uncommitted: the repository is not lying about what happened"
        );
    }

    /// **The founding scenario of the whole pillar, end to end: somebody is standing on the target
    /// branch in a worktree of their own, and the queue lands the merge underneath them — HEAD, the
    /// index and the files together — without their working copy ever telling a lie about it.**
    ///
    /// Nothing else in this module covers it, and the gap is not obvious from the names. The test
    /// above holds `master` in the MAIN checkout, so it never stands up a linked worktree at all;
    /// the only two tests that do — `a_refusal_the_user_cannot_fix_by_committing_is_not_reported_as_blocked`
    /// and `a_holder_worktree_whose_git_file_is_gone_is_refused_rather_than_blamed_on_the_user` —
    /// hand the publish a deliberately BROKEN holder, because their subject is the guards that
    /// refuse one. Every piece of the composition was tested; that the pieces fit was not.
    ///
    /// So this runs through `GitExecutor::execute` with a `ClaimedRequest`, the way the daemon does,
    /// rather than calling `publish` directly. Entering below the executor would skip everything
    /// that decides WHICH publish runs — `project_root`'s guard, `compute_merge`'s detached HEAD,
    /// and `worktree_holding`'s answer — and the composition is the whole point.
    ///
    /// **`status --porcelain` is the assertion that carries this test**, and it is not a tidiness
    /// check. Moving the ref out from under a holder — what the compare-and-swap route would do
    /// here — leaves HEAD at the new commit while the index and the files stay at the old one, and
    /// `git status` in that worktree then reports the merge BACKWARDS: every file the merge brought
    /// in shows as a staged deletion, waiting for a human to "restore" it. Neither sha assertion can
    /// see that state, because the ref really did move. Spec §6.1 exists to prevent exactly it.
    #[tokio::test]
    async fn a_merge_into_a_branch_somebody_has_open_moves_their_whole_worktree() {
        use crate::execution::vcs::VcsExecutor;

        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-holderff-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // The holder: somebody's own linked worktree with `release` checked out and a clean working
        // copy. `release` rather than `master`, because `master` is held by the main checkout and a
        // LINKED worktree is precisely what the test above cannot reach. Placed beside the
        // repository rather than under `NUCLEOS_WORKTREE_ROOT`: this worktree belongs to the user,
        // and that root is where the daemon's own directories go.
        let holder = container.path().join("holder");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("-b"),
                OsStr::new("release"),
                holder.as_os_str()
            ]
        ));
        let before = sha_of(&repo, "release");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "release".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        let sha = match outcome {
            Outcome::Succeeded { sha, .. } => sha.expect("this operation names an object id"),
            other => panic!("expected a published merge, got {other:?}"),
        };
        // The sha the row reports IS the merge commit, established from its parents rather than by
        // taking the row's word for it: first parent the old target, second the source.
        assert_eq!(
            sha_of(&repo, &format!("{sha}^1")),
            before,
            "the reported sha is a merge whose first parent is where `release` was"
        );
        assert_eq!(
            sha_of(&repo, &format!("{sha}^2")),
            sha_of(&repo, "feat/x"),
            "and whose second parent is what was merged in"
        );

        assert_eq!(
            sha_of(&repo, "refs/heads/release"),
            sha,
            "the target branch ref moved"
        );
        // Stated because the property is "the holder moved", and kept knowing what it can and cannot
        // catch: the holder's `HEAD` is a SYMREF to `refs/heads/release`, so it follows the ref and
        // cannot go red while the assertion above is green — the same trap the compare-and-swap test
        // documents at `master`/`HEAD`. What it would catch is a publish that ever left the holder
        // DETACHED rather than moving its branch. The two assertions below are the ones that see the
        // difference between a ref that moved and a worktree that moved.
        assert_eq!(
            sha_of(&holder, "HEAD"),
            sha,
            "and the holder's HEAD is at it"
        );

        // **The one that separates "the ref moved" from "the user's worktree moved."**
        let status = Command::new("git")
            .arg("-C")
            .arg(&holder)
            .args(["status", "--porcelain"])
            .output()
            .expect("git should start");
        assert!(
            status.stdout.is_empty(),
            "the holder's index and files agree with its HEAD — a ref moved out from under it would show the merge backwards here; got: {}",
            String::from_utf8_lossy(&status.stdout)
        );

        // And the merge is on disk where the human will look for it. Byte-exact rather than merely
        // present, which `initialize_repo`'s `core.autocrlf false` is what makes possible on Windows.
        assert_eq!(
            std::fs::read_to_string(holder.join("feature.txt"))
                .expect("the file the merged branch introduced is in the holder's working copy"),
            "from the branch\n",
            "with the bytes the branch had"
        );
    }

    /// The whole design in one assertion. The user was in the middle of editing a file the merge
    /// touches; the merge does not happen to them, and their bytes are exactly where they left them.
    #[tokio::test]
    async fn a_user_s_uncommitted_file_blocks_the_publish_and_survives_it_untouched() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-blocked-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // The user is mid-edit on the file `feat/x` also changed.
        std::fs::write(repo.join("feature.txt"), "half-finished thought\n").expect("write");

        let before = sha_of(&repo, "master");
        let computed = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute");
        let new = computed.new.clone();
        let computed_tail = computed.output_tail.clone();

        let outcome = publish(&repo, "master", computed, deadline()).await;

        match outcome {
            Outcome::Blocked { output_tail, .. } => {
                assert!(
                    output_tail.contains("feature.txt"),
                    "git names the files itself; we pass them through: {output_tail}"
                );
                // Not enough on its own, and that was measured rather than assumed: the COMPUTE's
                // own diffstat names `feature.txt` too — it is the file the merge adds — so the
                // assertion above stays green when the row is wired to `computed.output_tail`
                // instead of the refusal's. What spec §6.3 promises a blocked row is the output
                // that says which file stopped the publish, which is the fast-forward's, and only
                // this comparison tells the two apart.
                assert_ne!(
                    output_tail, computed_tail,
                    "the row carries git's refusal, not the merge's own diffstat"
                );
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(repo.join("feature.txt")).expect("read"),
            "half-finished thought\n",
            "byte-for-byte what the user left"
        );
        assert_eq!(sha_of(&repo, "master"), before, "the branch did not move");

        // Nothing was lost: a refused publish rolls nothing back, so the tree, the blobs and both
        // parents are still in this repository and a resubmission after committing or stashing costs
        // one merge and no network.
        //
        // **Not a cached commit waiting to be published, and reading it as one is the mistake this
        // comment is this long to prevent.** A blocked row records no `result_sha` (`vcs.rs`'s
        // `finish` writes `None`) and nothing looks one up, so resubmitting goes through
        // `compute_merge` again and lands its own commit rather than this one — a merge commit
        // embeds its committer timestamp, and the human had to commit or stash in between, so it is
        // not even the same sha. This one is left unreferenced for `gc`. What the assertion pins is
        // therefore the OBJECTS, not the commit: the user's bytes stayed put and their repository
        // kept everything a recompute needs.
        assert!(git_ok(
            &repo,
            &[OsStr::new("cat-file"), OsStr::new("-e"), OsStr::new(&new)]
        ));
    }

    /// The invariant `worktree_holding`'s doc comment relies on: the daemon's own worktree is detached,
    /// so it never answers "I have the target open" and can never be fast-forwarded into.
    #[tokio::test]
    async fn an_integration_worktree_never_holds_the_branch_it_merges_into() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-detached-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute");

        let holder = worktree_holding(&repo, "master", deadline())
            .await
            .expect("list")
            .expect("some worktree has master open — the user's checkout");
        // Compared by what the directory *is* rather than by its path string: a temp path can arrive
        // symlinked or 8.3-shortened, and `Path` equality is case-sensitive for non-prefix components.
        // The integration worktree is detached at the merge commit, so this sha tells the two apart.
        assert_eq!(
            sha_of(&holder, "HEAD"),
            sha_of(&repo, "master"),
            "the user's checkout holds master; the integration worktree is detached and holds nothing"
        );
    }

    /// The re-read of the target ref before the fast-forward, and the one case where it does work
    /// git would not have done for us anyway.
    ///
    /// A target moved *forward* is the obvious test to write and proves nothing: the merge commit is
    /// not a descendant of the new tip, so `merge --ff-only` refuses on its own and the operation
    /// fails with or without the check — only the reason differs. A target moved *backwards* is the
    /// case that needs it. `old~1` is still an ancestor of the merge commit, so the fast-forward is
    /// perfectly possible: without the re-read the publish silently undoes the user's reset, moves
    /// the branch and the files back, and reports success. Deleting the check turns this test from
    /// `Failed` into exactly that — measured, not assumed.
    #[tokio::test]
    async fn a_target_rewound_while_the_merge_computed_is_not_fast_forwarded_back_over() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-rewound-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // A second commit on `master`, so that there is something to rewind past.
        std::fs::write(repo.join("work.txt"), "work\n").expect("write");
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("work")]
        ));

        let computed = compute_merge(&repo, "feat/x", "master", deadline())
            .await
            .expect("compute");

        // The user throws that commit away while the merge is being computed — in the very worktree
        // the publish is about to fast-forward.
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("reset"),
                OsStr::new("--hard"),
                OsStr::new("HEAD~1")
            ]
        ));
        let rewound = sha_of(&repo, "master");

        let outcome = publish(&repo, "master", computed, deadline()).await;

        match outcome {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("moved"),
                "the reason must say what to do about it: {reason}"
            ),
            other => panic!("a raced publish is a Failed, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "master"),
            rewound,
            "the branch is where the user left it — the publish did not undo their reset"
        );
        assert!(
            !repo.join("work.txt").exists(),
            "and their working copy did not get the discarded file back either"
        );
    }

    /// The counterexample that keeps `Blocked` honest, and the reason the classification asks a second
    /// question instead of treating every refusal as the user's fault.
    ///
    /// `Blocked` is terminal and tells a human to commit, stash or move something. Here the worktree
    /// holding the branch has had its directory deleted — git still lists it, the fast-forward still
    /// fails, and none of those three would change that. It must come back `Failed`.
    #[tokio::test]
    async fn a_refusal_the_user_cannot_fix_by_committing_is_not_reported_as_blocked() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-notblocked-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // A second worktree holds `release`; then its directory goes away. Git keeps the registration
        // until something prunes it, so `worktree list` still names a path that is not there.
        let gone = roots.path().join("gone");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("-b"),
                OsStr::new("release"),
                gone.as_os_str()
            ]
        ));
        std::fs::remove_dir_all(&gone).expect("remove the worktree directory");

        let computed = compute_merge(&repo, "feat/x", "release", deadline())
            .await
            .expect("compute");
        let outcome = publish(&repo, "release", computed, deadline()).await;

        assert!(
            matches!(outcome, Outcome::Failed { .. }),
            "a worktree that is not there is not something a human fixes by stashing: {outcome:?}"
        );
    }

    /// The mirror of the test above, and the worse half of it. There the holder's directory was gone
    /// and git could not answer at all. Here the directory is still standing and only its `.git` has
    /// gone — so every `git -C` pointed at it walks **up** and the *enclosing* repository answers in
    /// its place, exit 0 and all. `status --porcelain` then reports a stranger's uncommitted files,
    /// and the row comes back `Blocked`: terminal, and telling a human to commit work in a repository
    /// nobody named. Measured before it was guarded — `worktree list` keeps emitting the holder's
    /// `branch refs/heads/release` line (plus a `prunable` line this module does not read).
    ///
    /// **The enclosure is built on purpose, never borrowed from the ambient checkout**, for the
    /// reason `a_directory_that_is_not_a_worktree_is_refused_before_git_is_pointed_at_it` states at
    /// length: `space_free_tempdir` places these directories inside this very repository, so the
    /// walk-up has to be stopped at a repository we are allowed to lose.
    #[tokio::test]
    async fn a_holder_worktree_whose_git_file_is_gone_is_refused_rather_than_blamed_on_the_user() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-holdergone-");

        // The sacrificial repository the walk-up will reach.
        let (_enclosure, enclosing) = init_contained_repo("nucleos-gitexec-holderencl-");
        let enclosing_before = sha_of(&enclosing, "HEAD");

        // The worktree root, and so the holder, INSIDE that repository.
        let roots = enclosing.join("roots");
        let _env = WorktreeRootEnv::set(&roots);
        let holder = roots.join("holder");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("-b"),
                OsStr::new("release"),
                holder.as_os_str()
            ]
        ));
        // The directory survives; only the file naming its admin directory goes. This is what an
        // interrupted move or a half-finished cleanup leaves behind.
        std::fs::remove_file(holder.join(".git")).expect("remove the worktree's .git file");
        assert!(holder.is_dir(), "the directory itself is still standing");

        // The enclosing repository's own uncommitted work — what `status` would report in its place.
        std::fs::write(enclosing.join("seed.txt"), "uncommitted work\n").expect("write");

        let computed = compute_merge(&repo, "feat/x", "release", deadline())
            .await
            .expect("compute");
        let outcome = publish(&repo, "release", computed, deadline()).await;

        match outcome {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("no longer a git worktree"),
                "the reason must name the directory and what to do: {reason}"
            ),
            other => panic!(
                "a stranger's dirty files are not this user's to commit — expected Failed, got {other:?}"
            ),
        }
        assert_eq!(
            std::fs::read_to_string(enclosing.join("seed.txt")).expect("read"),
            "uncommitted work\n",
            "the enclosing repository's work is untouched"
        );
        assert_eq!(
            sha_of(&enclosing, "HEAD"),
            enclosing_before,
            "and nothing was fast-forwarded into a repository nobody named"
        );
    }

    /// The third guard, on the directory the queue row supplies rather than one this module made.
    ///
    /// The two guards above protect directories git or this module produced; `project_root` is
    /// handed over, and it is where `worktree add`, `worktree list` and `update-ref` all point. So
    /// the walk-up costs more here than anywhere else: a root that exists and is not a repository
    /// does not fail, it runs the ENTIRE operation — compute and publish both — against whatever
    /// encloses it. Measured before the guard existed: `Succeeded`, with the enclosing repository's
    /// `master` moved and the sha of a merge commit nobody asked for.
    ///
    /// So the enclosing repository here is not a bystander to be kept safe, it is the thing the
    /// unguarded run would have merged into — it gets `master` and `feat/x` on purpose, so that the
    /// operation is one that *would* have completely succeeded.
    #[tokio::test]
    async fn a_project_root_that_is_not_a_repository_is_refused_before_git_is_pointed_at_it() {
        use crate::execution::vcs::VcsExecutor;

        let _lock = crate::execution::worktree::test_env_lock();
        // The sacrificial repository, mergeable exactly as the real project would be.
        let (_enclosure, enclosing) = repo_with_a_branch_to_merge("nucleos-gitexec-rootencl-");
        let enclosing_before = sha_of(&enclosing, "master");
        // Its own uncommitted work, in a file `feat/x` does not touch — so a publish that reached
        // it would have fast-forwarded straight past this rather than being blocked by it.
        std::fs::write(enclosing.join("seed.txt"), "uncommitted work\n").expect("write");

        // What the row names: a directory that exists, is not a repository, and sits INSIDE one.
        // Chunk 3 resolves this string from an agent's cwd, where a stale root or a cwd one level
        // off the project produces exactly this.
        let project_root = enclosing.join("project");
        std::fs::create_dir_all(&project_root).expect("a directory that is not a repository");
        let roots = enclosing.join("roots");
        let _env = WorktreeRootEnv::set(&roots);

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: project_root.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match outcome {
            // The same shape the other two guards report, and for the same reason: the row was
            // executable and the environment was not, so an empty tail rather than the NULL that
            // means the row itself could not be executed.
            Outcome::Failed {
                reason,
                exit_code,
                output_tail,
            } => {
                assert!(reason.contains("is not a git repository"), "got: {reason}");
                assert!(
                    reason.contains(&project_root.display().to_string()),
                    "the reason names the directory, or nobody can tell which root was wrong: {reason}"
                );
                assert_eq!(exit_code, None, "nothing ran, so nothing exited");
                assert!(
                    output_tail.is_empty(),
                    "no command printed anything: {output_tail}"
                );
            }
            other => panic!("the guard reports a Failed, got {other:?}"),
        }

        // The one that carries the property: this is what moved when the guard was not there.
        assert_eq!(
            sha_of(&enclosing, "master"),
            enclosing_before,
            "the operation ran against the enclosing repository and published into it"
        );
        // The guard runs before ANY git command, not merely before the publish — `worktree add` is
        // the first thing the compute reaches, and it would have registered a worktree in the
        // enclosing repository and created this directory to hold it.
        assert!(
            !roots.exists(),
            "the guard runs BEFORE `worktree add`, which would otherwise have added a worktree to a repository nobody named"
        );
        // Weaker than those two and kept for what it states rather than what it catches: a
        // fast-forward leaves a dirty file it does not need alone, so this survives the unguarded
        // run as well. It pins the promise the pillar makes about the user's bytes.
        assert_eq!(
            std::fs::read_to_string(enclosing.join("seed.txt")).expect("read"),
            "uncommitted work\n",
            "the enclosing repository's uncommitted work is untouched"
        );
    }

    /// The key is a property of the REPOSITORY, and a linked worktree is the case that proves it: its
    /// own directory, its own `.git` (a file, not a directory), its own checked-out branch — and the
    /// same repository. A key that disagreed here would let a merge computed in one worktree run at the
    /// same moment as a merge publishing into another, which is the collision the pillar exists for.
    #[tokio::test]
    async fn a_linked_worktree_and_its_main_checkout_share_one_key() {
        let (container, repo) = init_contained_repo("nucleos-gitexec-key-");
        let linked = container.path().join("linked");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                linked.as_os_str(),
                OsStr::new("-b"),
                OsStr::new("side"),
            ]
        ));

        assert_eq!(
            repo_key(&repo, deadline()).await.unwrap(),
            repo_key(&linked, deadline()).await.unwrap()
        );
    }

    /// Two repositories must not collapse into one key, or the fix would buy exclusivity by serializing
    /// the whole machine.
    #[tokio::test]
    async fn two_repositories_have_two_keys() {
        let (_first_container, first) = init_contained_repo("nucleos-gitexec-key-a-");
        let (_second_container, second) = init_contained_repo("nucleos-gitexec-key-b-");

        assert_ne!(
            repo_key(&first, deadline()).await.unwrap(),
            repo_key(&second, deadline()).await.unwrap()
        );
    }

    /// The `git -C` walk-up, for the third time in this file and in a new place.
    ///
    /// `rev-parse` inside a plain subdirectory SUCCEEDS and answers about the enclosing repository. A
    /// project whose recorded root is wrong by one level would be handed a key naming a repository
    /// nobody chose — and everything downstream, the merge included, would then operate on it while
    /// reporting the name of the project that was asked for.
    #[tokio::test]
    async fn a_directory_inside_a_repository_is_not_given_that_repositorys_key() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-key-inside-");
        let inside = repo.join("subdir");
        std::fs::create_dir(&inside).expect("create subdirectory");

        let error = repo_key(&inside, deadline()).await.unwrap_err();

        assert!(
            error.contains("not the root of a repository"),
            "unexpected error: {error}"
        );
    }

    /// A directory outside every repository is an error rather than a key — and this is the ONLY
    /// test that reaches the non-zero-exit branch, which is why it does not use
    /// `space_free_tempdir`.
    ///
    /// That helper creates its directory *under the checkout* (cargo cannot link beneath a path
    /// containing a space), so a directory it makes is inside this very repository: `git -C` walks
    /// up, succeeds, and answers about the enclosing checkout — the refusal then comes from the
    /// top-level guard rather than from git. Written that way, this test passed for a reason its own
    /// name denied, and deleting the non-zero-exit branch altogether left the suite green. The
    /// system temp directory is outside every repository, and pointing git at one costs no linking.
    ///
    /// The assertion is on the message rather than on `is_err()` for the same reason: two guards
    /// refuse here, and only the wording says which one did. If this ever fails with the *other*
    /// message, the machine's temp directory has ended up inside a repository — the assertion will
    /// say so in as many words, which is the whole point of asserting on it.
    #[tokio::test]
    async fn a_directory_outside_every_repository_has_no_key() {
        let container = tempfile::tempdir().expect("create a temp directory outside the checkout");

        let error = repo_key(container.path(), deadline()).await.unwrap_err();

        assert!(
            error.contains("is not inside a git repository"),
            "unexpected error: {error}"
        );
    }

    /// The branch a worktree stands on, which is what an approved `git merge X` merges INTO.
    #[tokio::test]
    async fn the_branch_a_worktree_stands_on_is_readable() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-branch-");

        // `git init` picks `master` or `main` depending on the installed default, and which one it
        // chose is not this function's business — that HEAD's branch is what comes back is.
        let initial = current_branch(&repo, deadline()).await.unwrap();
        assert!(matches!(initial.as_str(), "master" | "main"), "{initial}");

        assert!(git_ok(
            &repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        assert_eq!(current_branch(&repo, deadline()).await.unwrap(), "feat/x");
    }

    /// A detached HEAD is an answer, not a failure. The decision to refuse it as a merge target is
    /// `vcs::merge_from_command`'s, so that every reason a command is unqueueable lives in one place
    /// rather than half here and half there.
    #[tokio::test]
    async fn a_detached_head_is_reported_rather_than_refused() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-detached-");
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("--detach")]
        ));

        assert_eq!(current_branch(&repo, deadline()).await.unwrap(), "HEAD");
    }

    /// The walk-up guard, and here it is worth more than it is for `repo_key`.
    ///
    /// A wrong key is refused downstream by a project that does not match; a wrong BRANCH is a
    /// perfectly ordinary name that the queue would then merge into. Asked about a directory that is
    /// not a worktree root — a subdirectory, or a resumed run's worktree that has since been removed
    /// — `git -C` walks up and answers about the enclosing checkout with exit code 0.
    #[tokio::test]
    async fn a_directory_inside_a_worktree_is_not_given_that_worktrees_branch() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-branch-inside-");
        let inside = repo.join("subdir");
        std::fs::create_dir(&inside).expect("create subdirectory");

        let error = current_branch(&inside, deadline()).await.unwrap_err();

        assert!(
            error.contains("not the root of a worktree"),
            "unexpected error: {error}"
        );
    }

    /// The other refusal, and — like `a_directory_outside_every_repository_has_no_key`, whose doc
    /// comment argues this at length — it does NOT use `space_free_tempdir`, because that helper
    /// builds under the checkout and so inside this very repository. Written that way the test would
    /// pass through the top-level guard instead, and the non-zero-exit branch could be deleted whole
    /// with the suite still green.
    #[tokio::test]
    async fn a_directory_outside_every_repository_has_no_branch() {
        let container = tempfile::tempdir().expect("create a temp directory outside the checkout");

        let error = current_branch(container.path(), deadline())
            .await
            .unwrap_err();

        assert!(
            error.contains("is not inside a git repository"),
            "unexpected error: {error}"
        );
    }

    /// A spent budget refuses before spawning, which is what puts this on `run_git`'s sanctioned
    /// list at all.
    #[tokio::test]
    async fn reading_a_branch_on_an_exhausted_budget_refuses_before_running_git() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-branch-budget-");

        let error = current_branch(&repo, std::time::Instant::now())
            .await
            .unwrap_err();

        assert!(
            error.contains("ran out of time"),
            "unexpected error: {error}"
        );
    }

    /// The comparison the guard makes is between two paths that git and the caller spell differently:
    /// git answers with forward slashes, the caller holds a Windows path. Canonicalising BOTH sides is
    /// what makes the comparison mean "same directory" rather than "same string" — and without it the
    /// guard would reject every valid root, so the two tests above are what hold it.
    ///
    /// This test covers `canonical` itself, on the one property `repo_key`'s own tests cannot see.
    #[tokio::test]
    async fn two_spellings_of_one_directory_canonicalise_together() {
        let container = space_free_tempdir("nucleos-gitexec-canon-");
        let nested = container.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create nested directories");

        assert_eq!(
            canonical(&nested).await.unwrap(),
            canonical(&nested.join("..").join("b")).await.unwrap()
        );
    }

    /// The budget is for the whole operation, and a spent one must refuse before spawning git — the
    /// same guarantee `remaining` gives the merge path, for the same reason: `tokio`'s `Command::output`
    /// spawns the child eagerly, so a spent budget would launch git only to kill it.
    #[tokio::test]
    async fn an_exhausted_budget_refuses_before_running_git() {
        let (_container, repo) = init_contained_repo("nucleos-gitexec-key-budget-");

        let error = repo_key(&repo, std::time::Instant::now())
            .await
            .unwrap_err();

        assert!(
            error.contains("ran out of time"),
            "unexpected error: {error}"
        );
    }

    /// A bare repository beside `repo`, registered as `origin`.
    ///
    /// A real remote on the filesystem rather than a fake: the point of these tests is that the argv
    /// this module builds is one git accepts and that the refs move where they should, and a stub
    /// would answer neither question. Bare because a push to a non-bare checkout's current branch is
    /// refused by git itself, which would make every test below fail for a reason none of them is
    /// about.
    fn remote_beside(container: &Path, repo: &Path) -> PathBuf {
        let remote = container.join("remote.git");
        assert!(
            Command::new("git")
                .args(["init", "--bare"])
                .arg(&remote)
                .status()
                .expect("git should start")
                .success()
        );
        assert!(git_ok(
            repo,
            &[
                OsStr::new("remote"),
                OsStr::new("add"),
                OsStr::new("origin"),
                remote.as_os_str()
            ]
        ));
        remote
    }

    /// The push end to end, through `execute` the way the daemon runs it.
    ///
    /// **The assertion is on the REMOTE's ref, not on the exit code**, and that is the difference
    /// between testing that git was run and testing that the operation happened. A refspec built
    /// wrong — the two halves swapped, `refs/heads/` missing, the sha resolved from the wrong side —
    /// leaves git exiting 0 in several of those spellings while the remote's `master` sits where it
    /// was or moves somewhere nobody asked for.
    ///
    /// The reported sha is checked against the LOCAL branch as well, because the row's `result_sha`
    /// is what a human reads the queue backwards from: a push that reported the remote's previous
    /// value, or an empty string, would be green on the ref assertion alone.
    #[tokio::test]
    async fn a_push_moves_the_remote_ref_to_the_sha_it_reports() {
        use crate::execution::vcs::VcsExecutor;

        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-push-");
        let remote = remote_beside(container.path(), &repo);
        let local = sha_of(&repo, "refs/heads/master");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Push {
                    remote: "origin".into(),
                    branch: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        let sha = match outcome {
            Outcome::Succeeded { sha, .. } => sha.expect("this operation names an object id"),
            other => panic!("expected a published push, got {other:?}"),
        };
        assert_eq!(sha, local, "the row reports the sha it sent");
        assert_eq!(
            sha_of(&remote, "refs/heads/master"),
            local,
            "and the remote's branch is at it"
        );
        // The branch NAMED is the one pushed, and nothing else went with it. `feat/x` exists locally
        // and a refspec built from the wrong end — or a `--all` creeping in — would carry it too.
        assert!(
            !Command::new("git")
                .arg("-C")
                .arg(&remote)
                .args(["rev-parse", "--verify", "refs/heads/feat/x"])
                .output()
                .expect("git should start")
                .status
                .success(),
            "only the branch the operation named may be published"
        );
    }

    // ---- a push of the integration branch waits for the post-land gate, never for its caller ------

    const GATED_RULES: &str = "gate_command: x\ngate_after_land: true\n";
    /// A well-formed sha no commit of the fixture has: "the gate last judged some other tip green".
    const OTHER_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A repository with a remote, a rostered project `alpha` whose rules are `rules`, and a pool.
    async fn gated_push_fixture(
        rules: &str,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        PathBuf,
        sqlx::SqlitePool,
    ) {
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gatedpush-");
        let remote = remote_beside(container.path(), &repo);
        let machine_root = container.path().join("machine");
        write_autopilot_rules(&machine_root, rules);
        let pool = crate::testdb::fresh_pool().await;
        roster(&pool, &repo).await;
        (container, repo, remote, machine_root, pool)
    }

    fn push_request(repo: &Path, branch: &str, id: i64) -> crate::execution::vcs::ClaimedRequest {
        crate::execution::vcs::ClaimedRequest {
            id,
            op: crate::execution::vcs::Op::Push {
                remote: "origin".into(),
                branch: branch.into(),
            },
            project_id: "alpha".to_owned(),
            project_root: repo.to_string_lossy().into_owned(),
            from_resolution: false,
            run_id: None,
            approved_map_blob: None,
            integration_branch: None,
        }
    }

    async fn execute_push(
        repo: &Path,
        machine_root: &Path,
        pool: &sqlx::SqlitePool,
        branch: &str,
    ) -> Outcome {
        use crate::execution::vcs::VcsExecutor;
        GitExecutor {
            machine_root: Some(machine_root.to_path_buf()),
            pool: Some(pool.clone()),
            ..GitExecutor::default()
        }
        .execute(&push_request(repo, branch, 1))
        .await
    }

    fn remote_has(remote: &Path, branch: &str) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(remote)
            .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
            .output()
            .expect("git should start")
            .status
            .success()
    }

    #[tokio::test]
    async fn a_push_of_a_green_integration_tip_goes_out_at_once() {
        let (_container, repo, remote, machine_root, pool) = gated_push_fixture(GATED_RULES).await;
        let tip = sha_of(&repo, "refs/heads/master");
        crate::verify::postgate::mark_covered(&pool, "alpha", "master", &tip)
            .await
            .expect("the gate judged the tip green");

        let outcome = execute_push(&repo, &machine_root, &pool, "master").await;

        assert!(
            matches!(&outcome, Outcome::Succeeded { sha: Some(sha), .. } if *sha == tip),
            "expected the push to go out, got {outcome:?}"
        );
        assert_eq!(sha_of(&remote, "refs/heads/master"), tip);
    }

    #[tokio::test]
    async fn a_tip_covered_by_a_verification_counts_as_green() {
        let (_container, repo, remote, machine_root, pool) = gated_push_fixture(GATED_RULES).await;
        let tip = sha_of(&repo, "refs/heads/master");
        crate::verify::postgate::mark_covered(&pool, "alpha", "master", OTHER_SHA)
            .await
            .expect("some other tip is green");
        sqlx::query("UPDATE postgate_state SET covered_sha = ? WHERE project_id = 'alpha'")
            .bind(&tip)
            .execute(&pool)
            .await
            .expect("cover the tip");

        let outcome = execute_push(&repo, &machine_root, &pool, "master").await;

        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert_eq!(sha_of(&remote, "refs/heads/master"), tip);
    }

    #[tokio::test]
    async fn a_push_of_a_pending_integration_tip_parks_and_the_remote_does_not_move() {
        let (_container, repo, remote, machine_root, pool) = gated_push_fixture(GATED_RULES).await;
        let tip = sha_of(&repo, "refs/heads/master");
        crate::verify::postgate::mark_covered(&pool, "alpha", "master", OTHER_SHA)
            .await
            .expect("some other tip is green");

        let outcome = execute_push(&repo, &machine_root, &pool, "master").await;

        match outcome {
            Outcome::AwaitingGate { sha, reason } => {
                assert_eq!(sha, tip, "the park carries the sha it decided on");
                assert!(reason.contains(&tip), "the reason names the sha: {reason}");
                assert!(
                    reason.contains("post-land gate"),
                    "and what it waits for: {reason}"
                );
            }
            other => panic!("expected the push to park, got {other:?}"),
        }
        assert!(
            !remote_has(&remote, "master"),
            "nothing may reach the remote"
        );
    }

    #[tokio::test]
    async fn a_push_with_no_gate_state_at_all_parks_too() {
        let (_container, repo, remote, machine_root, pool) = gated_push_fixture(GATED_RULES).await;

        let outcome = execute_push(&repo, &machine_root, &pool, "master").await;

        assert!(
            matches!(outcome, Outcome::AwaitingGate { .. }),
            "got {outcome:?}"
        );
        assert!(!remote_has(&remote, "master"));
    }

    #[tokio::test]
    async fn a_push_of_another_branch_ignores_the_gate() {
        let (_container, repo, remote, machine_root, pool) = gated_push_fixture(GATED_RULES).await;
        // The gate is not green for anything, and `feat/x` is not the integration branch.
        let outcome = execute_push(&repo, &machine_root, &pool, "feat/x").await;

        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert_eq!(
            sha_of(&remote, "refs/heads/feat/x"),
            sha_of(&repo, "refs/heads/feat/x")
        );
    }

    #[tokio::test]
    async fn a_push_without_gate_after_land_ignores_the_gate() {
        let (_container, repo, remote, machine_root, pool) =
            gated_push_fixture("gate_command: x\n").await;

        let outcome = execute_push(&repo, &machine_root, &pool, "master").await;

        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert_eq!(
            sha_of(&remote, "refs/heads/master"),
            sha_of(&repo, "refs/heads/master")
        );
    }

    /// No `gate_command` means no gate will ever run, so parking would wait for ever: fail open.
    #[tokio::test]
    async fn gate_after_land_without_a_gate_command_pushes_at_once() {
        let (_container, repo, remote, machine_root, pool) = gated_push_fixture(
            "gate_after_land: true
",
        )
        .await;

        let outcome = execute_push(&repo, &machine_root, &pool, "master").await;

        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert_eq!(
            sha_of(&remote, "refs/heads/master"),
            sha_of(&repo, "refs/heads/master")
        );
    }

    #[tokio::test]
    async fn an_executor_without_a_database_pushes_as_before() {
        use crate::execution::vcs::VcsExecutor;
        let (_container, repo, remote, machine_root, _pool) = gated_push_fixture(GATED_RULES).await;

        let outcome = GitExecutor {
            machine_root: Some(machine_root),
            ..GitExecutor::default()
        }
        .execute(&push_request(&repo, "master", 1))
        .await;

        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert!(remote_has(&remote, "master"));
    }

    /// The whole loop through the real queue: the push parks, a parked row is not claimed (so there
    /// is no hot loop), a green write requeues it, and the next claim pushes the tip.
    #[tokio::test]
    async fn a_green_write_releases_a_parked_push_and_it_then_goes_out() {
        use crate::execution::vcs::{Op, Origin, ResolvedRepo};

        let (_container, repo, remote, machine_root, pool) = gated_push_fixture(GATED_RULES).await;
        let tip = sha_of(&repo, "refs/heads/master");
        let resolved = ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha");
        let op = Op::Push {
            remote: "origin".into(),
            branch: "master".into(),
        };
        let id = crate::execution::vcs::submit(&pool, &resolved, &op, Origin::Shell)
            .await
            .expect("admit the push");

        let claimed = crate::execution::vcs::claim_next(&pool, "alpha")
            .await
            .expect("claim")
            .expect("the push is queued");
        let outcome = execute_push_claimed(&machine_root, &pool, &claimed).await;
        assert!(
            matches!(outcome, Outcome::AwaitingGate { .. }),
            "got {outcome:?}"
        );
        crate::execution::vcs::finish(&pool, id, outcome)
            .await
            .expect("record the park");

        let status: String = sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("read the status");
        assert_eq!(status, "awaiting_gate");
        assert!(
            crate::execution::vcs::claim_next(&pool, "alpha")
                .await
                .expect("claim")
                .is_none(),
            "a parked push is not claimable"
        );
        assert!(!remote_has(&remote, "master"));

        // The gate turns the tip green; the write itself requeues the parked push.
        crate::verify::postgate::mark_covered(&pool, "alpha", "master", &tip)
            .await
            .expect("green");
        let claimed = crate::execution::vcs::claim_next(&pool, "alpha")
            .await
            .expect("claim")
            .expect("the green write requeued the push");
        assert_eq!(claimed.id, id);
        let outcome = execute_push_claimed(&machine_root, &pool, &claimed).await;
        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert_eq!(sha_of(&remote, "refs/heads/master"), tip);
    }

    async fn execute_push_claimed(
        machine_root: &Path,
        pool: &sqlx::SqlitePool,
        claimed: &crate::execution::vcs::ClaimedRequest,
    ) -> Outcome {
        use crate::execution::vcs::VcsExecutor;
        GitExecutor {
            machine_root: Some(machine_root.to_path_buf()),
            pool: Some(pool.clone()),
            ..GitExecutor::default()
        }
        .execute(claimed)
        .await
    }

    /// Released by every write that makes a sha green, not only `mark_covered`.
    #[tokio::test]
    async fn finish_green_requeues_a_parked_push_too() {
        let (_container, repo, _remote, _machine_root, pool) =
            gated_push_fixture(GATED_RULES).await;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at, repo_key)
             VALUES ('push', '{\"op\":\"push\",\"remote\":\"origin\",\"branch\":\"master\"}', 'alpha', ?,
                     'shell', 'awaiting_gate', '2026-10-19T00:00:00+00:00', 'alpha')
             RETURNING id",
        )
        .bind(repo.to_string_lossy().into_owned())
        .fetch_one(&pool)
        .await
        .expect("park a push");
        let tip = sha_of(&repo, "refs/heads/master");
        assert!(
            crate::verify::postgate::start(&pool, "alpha", "master", &tip, None)
                .await
                .expect("start the gate")
        );

        assert!(
            crate::verify::postgate::finish_green(&pool, "alpha", &tip)
                .await
                .expect("finish green")
        );

        let status: String = sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("read the status");
        assert_eq!(status, "queued");
    }

    /// A rebase nobody has open is computed in isolation and published by compare-and-swap.
    ///
    /// The assertion is on the SHAPE of the result, not just that the ref moved: the rebased tip's
    /// first parent must be `master`, which is what says the commits were replayed rather than
    /// merged. A `compute_merge` accidentally wired here would move the ref too, and leave a tip with
    /// two parents.
    #[tokio::test]
    async fn a_rebase_of_a_branch_nobody_holds_is_published_by_compare_and_swap() {
        use crate::execution::vcs::VcsExecutor;

        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-rebase-");
        let _env = WorktreeRootEnv::set(&container.path().join("roots"));
        // `master` moves on, so `feat/x` genuinely has somewhere to be replayed onto.
        std::fs::write(repo.join("later.txt"), "later\n").expect("write");
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("later")]
        ));
        let master = sha_of(&repo, "refs/heads/master");
        let before = sha_of(&repo, "refs/heads/feat/x");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Rebase {
                    branch: "feat/x".into(),
                    onto: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        let sha = match outcome {
            Outcome::Succeeded { sha, .. } => sha.expect("a rebase names its new tip"),
            other => panic!("expected a published rebase, got {other:?}"),
        };
        assert_ne!(
            sha, before,
            "the commits were replayed, so they are new objects"
        );
        assert_eq!(
            sha_of(&repo, "refs/heads/feat/x"),
            sha,
            "and the branch ref moved to them"
        );
        assert_eq!(
            sha_of(&repo, &format!("{sha}^1")),
            master,
            "replayed onto master, so its first parent is master — a merge would have two parents"
        );
        assert!(
            !Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "--verify", &format!("{sha}^2")])
                .output()
                .expect("git should start")
                .status
                .success(),
            "and no second parent, which is what tells a rebase from a merge"
        );
    }

    /// A branch checked out WHILE it is being rebased is not moved under that checkout. A
    /// `post-rewrite` hook — which git runs in the integration worktree as the rebase finishes —
    /// opens the branch in a worktree of its own, after the first holder check has already passed.
    #[tokio::test]
    async fn a_branch_checked_out_during_its_rebase_is_not_published() {
        use crate::execution::vcs::VcsExecutor;

        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-rebase-race-");
        let _env = WorktreeRootEnv::set(&container.path().join("roots"));
        std::fs::write(repo.join("later.txt"), "later\n").expect("write");
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("later")]
        ));
        let before = sha_of(&repo, "refs/heads/feat/x");

        let holder = container.path().join("holder");
        let hooks = container.path().join("hooks");
        std::fs::create_dir_all(&hooks).expect("create hooks directory");
        let slashed = |path: &Path| path.to_string_lossy().replace('\\', "/");
        let hook = hooks.join("post-rewrite");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE\n\
                 git -C \"{}\" worktree add \"{}\" feat/x >/dev/null 2>&1\n",
                slashed(&repo),
                slashed(&holder)
            ),
        )
        .expect("write hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                .expect("make hook executable");
        }
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("config"),
                OsStr::new("core.hooksPath"),
                hooks.as_os_str()
            ]
        ));

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Rebase {
                    branch: "feat/x".into(),
                    onto: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        assert!(
            holder.join(".git").exists(),
            "the hook did not open the branch, so this test proves nothing"
        );
        match &outcome {
            Outcome::Blocked { reason, .. } => assert!(
                reason.contains("while it was being rebased"),
                "the refusal must say why: {reason}"
            ),
            other => panic!("a branch opened mid-rebase must be blocked, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "refs/heads/feat/x"),
            before,
            "the branch moved under the worktree that holds it"
        );
    }

    /// **A branch somebody has open is refused before anything is computed.**
    ///
    /// Both halves matter. `Blocked` rather than `Failed` says nothing will change on its own to
    /// make this publishable. And the branch not having moved is what says the refusal came BEFORE
    /// the work — an executor that computed first and discovered the holder afterwards would leave
    /// the same status behind, having spent the operation's budget and left unreferenced objects.
    #[tokio::test]
    async fn a_rebase_of_a_branch_somebody_holds_is_blocked_before_anything_is_computed() {
        use crate::execution::vcs::VcsExecutor;

        let _lock = crate::execution::worktree::test_env_lock();
        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-rebase-held-");
        let roots = container.path().join("roots");
        let _env = WorktreeRootEnv::set(&roots);
        let holder = container.path().join("holder");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                holder.as_os_str(),
                OsStr::new("feat/x")
            ]
        ));
        let before = sha_of(&repo, "refs/heads/feat/x");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Rebase {
                    branch: "feat/x".into(),
                    onto: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match &outcome {
            Outcome::Blocked { reason, .. } => assert!(
                reason.contains("will not reset a worktree it does not own"),
                "the refusal must say what it declined to do: {reason}"
            ),
            other => panic!("a held branch must be blocked, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "refs/heads/feat/x"),
            before,
            "and nothing was rewritten"
        );
        assert!(
            !integration_worktree(&repo).exists(),
            "the refusal came before any computation, so no integration worktree was even made"
        );
    }

    /// A fetch moves the tracking ref and reports NO object id.
    ///
    /// **Both halves are the test.** That `refs/remotes/origin/master` catches up is what says the
    /// operation happened; that `sha` is `None` is what says the type change was not cosmetic — an
    /// executor returning `Some(String::new())` would satisfy every other assertion here and put an
    /// empty string in a column a reader cannot tell from a capture that failed.
    #[tokio::test]
    async fn a_fetch_moves_the_tracking_ref_and_names_no_object_id() {
        use crate::execution::vcs::VcsExecutor;

        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-fetch-");
        let remote = remote_beside(container.path(), &repo);
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("push"),
                OsStr::new("origin"),
                OsStr::new("master")
            ]
        ));
        // Move the remote on from a second clone, so there is genuinely something to fetch.
        let other = container.path().join("other");
        assert!(
            Command::new("git")
                .arg("clone")
                .arg(&remote)
                .arg(&other)
                .status()
                .expect("git should start")
                .success()
        );
        for (key, value) in [("user.email", "test@x"), ("user.name", "test")] {
            assert!(git_ok(
                &other,
                &[OsStr::new("config"), OsStr::new(key), OsStr::new(value)]
            ));
        }
        std::fs::write(other.join("theirs.txt"), "theirs\n").expect("write");
        assert!(git_ok(&other, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &other,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("theirs")]
        ));
        assert!(git_ok(&other, &[OsStr::new("push")]));
        let theirs = sha_of(&remote, "refs/heads/master");
        assert_ne!(
            sha_of(&repo, "refs/remotes/origin/master"),
            theirs,
            "the tracking ref must be behind before the fetch, or this proves nothing"
        );

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Fetch {
                    remote: "origin".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match outcome {
            Outcome::Succeeded { sha, .. } => assert_eq!(
                sha, None,
                "a fetch moves however many refs the remote had news about, so it names none"
            ),
            other => panic!("expected a completed fetch, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "refs/remotes/origin/master"),
            theirs,
            "the tracking ref caught up"
        );
    }

    /// Deleting a branch records the sha it pointed at, which is the row's whole value as an undo.
    ///
    /// A test that only checked the branch was gone would pass against an executor that deleted
    /// first and reported nothing — and afterwards nothing on the machine can answer what was lost.
    #[tokio::test]
    async fn deleting_a_branch_records_the_sha_that_restores_it() {
        use crate::execution::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-branchdel-");
        // Merged into master, so `--delete` is willing: the unmerged case is the next test.
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("merge"),
                OsStr::new("--no-ff"),
                OsStr::new("-m"),
                OsStr::new("bring it in"),
                OsStr::new("feat/x")
            ]
        ));
        let was = sha_of(&repo, "refs/heads/feat/x");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::BranchDelete {
                    branch: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match outcome {
            Outcome::Succeeded { sha, .. } => assert_eq!(
                sha.as_deref(),
                Some(was.as_str()),
                "the row is the only place left holding what would restore this"
            ),
            other => panic!("expected a deleted branch, got {other:?}"),
        }
        assert!(
            !Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "--verify", "refs/heads/feat/x"])
                .output()
                .expect("git should start")
                .status
                .success(),
            "and the branch is gone"
        );
    }

    /// **The refusal this operation is built on belongs to git, and this is what proves the queue
    /// asks for it.**
    ///
    /// `feat/x` here is NOT merged, so `--delete` refuses and `-D` would not. The branch surviving
    /// is the assertion: an executor that reached for `-D` — the obvious "fix" for a failing delete
    /// — passes every other check in this file and silently discards work nobody can get back.
    #[tokio::test]
    async fn deleting_an_unmerged_branch_is_refused_by_git_rather_than_forced() {
        use crate::execution::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-branchdel-unmerged-");
        let was = sha_of(&repo, "refs/heads/feat/x");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::BranchDelete {
                    branch: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match &outcome {
            Outcome::Failed { output_tail, .. } => assert!(
                !output_tail.is_empty(),
                "git's own refusal is the only diagnostic the row carries"
            ),
            other => panic!("an unmerged branch must not be deleted, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "refs/heads/feat/x"),
            was,
            "the branch is still there, with its commits — this queue never forces"
        );
    }

    /// A delete of `feat/x`, judged against `integration` when there is one.
    async fn delete_feat_x(repo: &Path, integration: Option<&str>) -> Outcome {
        use crate::execution::vcs::VcsExecutor;

        GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::BranchDelete {
                    branch: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: integration.map(Into::into),
            })
            .await
    }

    /// What `feat/x` tracks, as `for-each-ref` reports it: empty when nothing.
    fn upstream_of_feat_x(repo: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["for-each-ref", "--format=%(upstream)", "refs/heads/feat/x"])
            .output()
            .expect("git should start");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// `feat/x` merged into `master`, and the main checkout parked on `master~1`, which does not
    /// contain it: the state of the machine on 2026-09-12, reduced.
    fn a_landed_branch_under_a_parked_checkout(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = repo_with_a_branch_to_merge(prefix);
        for args in [
            &["merge", "--no-ff", "-m", "land feat/x", "feat/x"][..],
            &["checkout", "-q", "-b", "chore/parked", "master~1"][..],
        ] {
            let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
            assert!(git_ok(&repo, &args));
        }
        (container, repo)
    }

    /// **The defect, and why the executor needs the integration branch at all.** With nothing to be
    /// judged against, git asks the parked HEAD, which never saw the landing, and refuses a branch
    /// whose every commit is on `master`.
    #[tokio::test]
    async fn a_landed_branch_is_refused_when_judged_against_a_parked_head() {
        let (_container, repo) = a_landed_branch_under_a_parked_checkout("nucleos-gitexec-parked-");

        let outcome = delete_feat_x(&repo, None).await;

        assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
    }

    /// Judged against the integration branch, the same landed branch is deleted, and it leaves no
    /// borrowed upstream behind in the config.
    #[tokio::test]
    async fn a_landed_branch_is_deleted_wherever_the_main_checkout_stands() {
        let (_container, repo) = a_landed_branch_under_a_parked_checkout("nucleos-gitexec-landed-");
        let was = sha_of(&repo, "refs/heads/feat/x");

        let outcome = delete_feat_x(&repo, Some("master")).await;

        match outcome {
            Outcome::Succeeded { sha, .. } => assert_eq!(sha.as_deref(), Some(was.as_str())),
            other => panic!("a branch merged into master must be deletable, got {other:?}"),
        }
        let config = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "--get-regexp", r"^branch\.feat/x\."])
            .output()
            .expect("git should start");
        assert!(
            String::from_utf8_lossy(&config.stdout).trim().is_empty(),
            "the delete takes the borrowed upstream with it"
        );
    }

    /// Still git's refusal, whatever it is compared with: an unmerged branch survives, and the
    /// upstream it borrowed for the attempt is given back.
    #[tokio::test]
    async fn an_unmerged_branch_is_still_refused_and_keeps_no_upstream() {
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-unmerged-judged-");
        let was = sha_of(&repo, "refs/heads/feat/x");

        let outcome = delete_feat_x(&repo, Some("master")).await;

        assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
        assert_eq!(
            sha_of(&repo, "refs/heads/feat/x"),
            was,
            "this queue never forces"
        );
        assert_eq!(
            upstream_of_feat_x(&repo),
            "",
            "a refused delete leaves the branch as it found it"
        );
    }

    /// A branch that already tracks something keeps being judged by it: its owner chose that.
    #[tokio::test]
    async fn a_branch_that_tracks_something_is_judged_by_what_it_tracks() {
        let (_container, repo) = a_landed_branch_under_a_parked_checkout("nucleos-gitexec-tracks-");
        let tracks = ["branch", "--set-upstream-to=chore/parked", "feat/x"];
        let tracks: Vec<&OsStr> = tracks.iter().map(OsStr::new).collect();
        assert!(git_ok(&repo, &tracks));

        let outcome = delete_feat_x(&repo, Some("master")).await;

        assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
        assert_eq!(upstream_of_feat_x(&repo), "refs/heads/chore/parked");
    }

    /// A branch that is not there fails before anything reaches the network, naming the ref.
    ///
    /// The message is the assertion: git's own answer to a push of a missing branch is `src refspec
    /// … does not match any`, which says nothing about which of the two halves of the refspec was
    /// wrong. Resolving the sha first turns that into a sentence naming the branch — and it is what
    /// keeps a typo from costing a network round trip while holding the repository's only slot.
    #[tokio::test]
    async fn pushing_a_branch_that_does_not_exist_fails_before_the_network() {
        use crate::execution::vcs::VcsExecutor;

        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-push-missing-");
        let _remote = remote_beside(container.path(), &repo);

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Push {
                    remote: "origin".into(),
                    branch: "no-such-branch".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match outcome {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("no-such-branch"),
                "the message must name the ref that could not be resolved: {reason}"
            ),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// The tag end to end, through `execute` the way the daemon runs it.
    ///
    /// **The assertion is that `refs/tags/v1.0` resolves to the sha the row reports**, which is what
    /// separates a tag that was written from a git command that merely exited 0. It is also what
    /// catches the argv built the other way round: `git tag <sha> <name>` is a perfectly valid
    /// command — it creates a tag NAMED after the object id, pointing at whatever `<name>` resolves
    /// to — so a swap exits 0 and leaves `v1.0` absent.
    #[tokio::test]
    async fn a_tag_lands_on_the_branch_tip_it_names() {
        use crate::execution::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-tag-");
        let tip = sha_of(&repo, "refs/heads/feat/x");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Tag {
                    name: "v1.0".into(),
                    // Deliberately NOT the checked-out branch: a tag is written to the ref store and
                    // never touches a worktree, so nothing about this needs `master`.
                    at: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        let sha = match outcome {
            Outcome::Succeeded { sha, .. } => sha.expect("this operation names an object id"),
            other => panic!("expected a written tag, got {other:?}"),
        };
        assert_eq!(sha, tip, "the row reports the object it tagged");
        assert_eq!(
            sha_of(&repo, "refs/tags/v1.0"),
            tip,
            "and the tag is on it, under the name that was asked for"
        );
    }

    /// **A name already taken is git's refusal to report, not ours to overrule.**
    ///
    /// The second half is the assertion that matters: the existing tag must still point where it
    /// did. A `-f` creeping into the argv would make this operation succeed and move somebody's
    /// release tag onto a different commit — the tag-shaped force push, and the one failure here
    /// that nobody downstream can detect from their own clone.
    #[tokio::test]
    async fn a_tag_that_already_exists_is_refused_rather_than_moved() {
        use crate::execution::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-tag-taken-");
        let original = sha_of(&repo, "refs/heads/master");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("tag"),
                OsStr::new("v1.0"),
                OsStr::new("refs/heads/master")
            ]
        ));

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Tag {
                    name: "v1.0".into(),
                    at: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match &outcome {
            Outcome::Failed { output_tail, .. } => assert!(
                !output_tail.is_empty(),
                "git's own refusal is the only diagnostic the row carries"
            ),
            other => panic!("a taken tag name is a retryable failure, got {other:?}"),
        }
        assert_eq!(
            sha_of(&repo, "refs/tags/v1.0"),
            original,
            "the tag that was already there did not move — this queue never forces"
        );
    }

    /// `at` is a BRANCH, and the executor is where that stops being a claim about the type's name.
    ///
    /// A raw object id passes `Branch`'s argv rules — it is one word with no leading dash — so
    /// nothing before this point can refuse it. `refs/heads/<at>` is what does, and the message names
    /// the ref rather than leaving a caller to wonder why a perfectly good sha was rejected.
    #[tokio::test]
    async fn tagging_something_that_is_not_a_branch_is_refused_by_the_ref_it_resolves() {
        use crate::execution::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-tag-notbranch-");
        let sha = sha_of(&repo, "refs/heads/master");

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Tag {
                    name: "v1.0".into(),
                    at: sha.as_str().into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match outcome {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("refs/heads/") && reason.contains(&sha),
                "the message must name the ref it could not resolve: {reason}"
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            !Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "--verify", "refs/tags/v1.0"])
                .output()
                .expect("git should start")
                .status
                .success(),
            "and nothing was tagged"
        );
    }

    /// **A remote that moved refuses the push, and the row is `Failed` rather than `Blocked`.**
    ///
    /// Both halves matter and neither implies the other. That git refuses a non-fast-forward is git's
    /// business; that this module records the refusal as retryable is the decision — `Blocked` is
    /// terminal and means a human must intervene, and what actually fixes this is bringing the remote
    /// in and resubmitting, possibly by an operation already queued behind this one.
    ///
    /// The divergence is built by pushing from a second clone, which is exactly how it happens in
    /// life: somebody else got there first.
    #[tokio::test]
    async fn a_remote_that_moved_refuses_the_push_and_the_row_stays_retryable() {
        use crate::execution::vcs::VcsExecutor;

        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-push-behind-");
        let remote = remote_beside(container.path(), &repo);
        // Seed the remote from this repository, then move it on from somewhere else, so the local
        // `master` is genuinely behind rather than merely unrelated.
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("push"),
                OsStr::new("origin"),
                OsStr::new("master")
            ]
        ));
        let other = container.path().join("other");
        assert!(
            Command::new("git")
                .arg("clone")
                .arg(&remote)
                .arg(&other)
                .status()
                .expect("git should start")
                .success()
        );
        assert!(git_ok(
            &other,
            &[
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new("test@x")
            ]
        ));
        assert!(git_ok(
            &other,
            &[
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new("test")
            ]
        ));
        std::fs::write(other.join("theirs.txt"), "theirs\n").expect("write");
        assert!(git_ok(&other, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &other,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("theirs")]
        ));
        assert!(git_ok(&other, &[OsStr::new("push")]));
        let theirs = sha_of(&remote, "refs/heads/master");

        // Now put a commit on the local side too, so the two have genuinely diverged.
        std::fs::write(repo.join("ours.txt"), "ours\n").expect("write");
        assert!(git_ok(&repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("ours")]
        ));

        let outcome = GitExecutor::default()
            .execute(&crate::execution::vcs::ClaimedRequest {
                id: 1,
                op: crate::execution::vcs::Op::Push {
                    remote: "origin".into(),
                    branch: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
                run_id: None,
                approved_map_blob: None,
                integration_branch: None,
            })
            .await;

        match &outcome {
            Outcome::Failed { output_tail, .. } => assert!(
                !output_tail.is_empty(),
                "git's own refusal is the only diagnostic the row carries"
            ),
            other => panic!("a rejected push is a retryable failure, got {other:?}"),
        }
        assert_eq!(
            sha_of(&remote, "refs/heads/master"),
            theirs,
            "and nothing of theirs was overwritten — this queue never forces"
        );
    }

    /// Builds and publishes a resolution the way the daemon does — a branch off the target with the
    /// conflict merged into it and resolved to `resolved`, then merged into the target with `--no-ff`
    /// — and answers with the published merge commit.
    ///
    /// Plain git rather than the queue, because what these tests are about is what the COMMITS say.
    /// Going through `create_resolution_run` would need a runner, an `AppState` and an agent, to
    /// arrive at the same three commits.
    fn publish_a_resolution(repo: &Path, resolved: Option<&str>) -> String {
        assert!(git_ok(
            repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("resolve"),
                OsStr::new("master"),
            ]
        ));
        // Conflicts, deliberately: this is the half-finished merge the daemon leaves behind.
        assert!(!git_ok(repo, &[OsStr::new("merge"), OsStr::new("feat/x")]));
        match resolved {
            Some(content) => std::fs::write(repo.join("seed.txt"), content).expect("resolve"),
            // The other way of losing work: the file goes away entirely.
            None => std::fs::remove_file(repo.join("seed.txt")).expect("delete"),
        }
        assert!(git_ok(repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo,
            &[OsStr::new("commit"), OsStr::new("--no-edit")]
        ));
        assert!(git_ok(
            repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("merge"),
                OsStr::new("--no-ff"),
                OsStr::new("-m"),
                OsStr::new("publish the resolution"),
                OsStr::new("resolve"),
            ]
        ));
        sha_of(repo, "master")
    }

    /// **The third proof, and the spec says the mechanism is not ready without it.** A resolution
    /// that keeps one side and drops the other passes every structural check there is: two parents,
    /// no markers, a green tree. It is a correct-looking merge with half the work gone, and the only
    /// thing that catches it is counting what went missing.
    #[tokio::test]
    async fn a_resolution_that_kept_one_side_is_published_and_the_row_says_what_it_cost() {
        let (_container, repo) = repo_with_a_conflict("nucleos-gitexec-discard-");
        // `ours` only. `feat/x`'s line is simply not there.
        let published = publish_a_resolution(&repo, Some("ours\n"));

        let record = discarded_by_resolution(&repo, &published, deadline())
            .await
            .expect("the account should be computable");

        assert!(
            record.contains("1 line(s)"),
            "the count is the whole point of computing rather than asking: {record}"
        );
        // On the FIRST line, because the feed carries one line of this and a headline that makes
        // the reader query the database to find out where is a headline that gets skipped.
        let headline = record.lines().next().expect("a record has a first line");
        assert!(
            headline.contains("seed.txt (1)"),
            "the headline has to name where, not only how much: {headline}"
        );
        assert!(
            record.contains("theirs"),
            "and it quotes what went missing, or there is nothing to recognise: {record}"
        );
    }

    /// The control, and without it the test above proves only that this function returns text. A
    /// resolution that kept BOTH sides has to come back clean — otherwise every resolution is
    /// reported as lossy and the record means nothing.
    #[tokio::test]
    async fn a_resolution_that_kept_both_sides_costs_nothing_and_says_so() {
        let (_container, repo) = repo_with_a_conflict("nucleos-gitexec-keep-");
        let published = publish_a_resolution(&repo, Some("ours\ntheirs\n"));

        let record = discarded_by_resolution(&repo, &published, deadline())
            .await
            .expect("the account should be computable");

        assert_eq!(
            record, "nothing",
            "a resolution that lost nothing must read as nothing, not as an empty report"
        );
    }

    /// The other shape of the same loss, and the reason a file missing from the published tree is
    /// read rather than errored on: everything the incoming branch put in it is gone, which is the
    /// answer, not a failure to find one.
    #[tokio::test]
    async fn a_resolution_that_deleted_the_contested_file_is_accounted_for_the_same_way() {
        let (_container, repo) = repo_with_a_conflict("nucleos-gitexec-deleted-");
        let published = publish_a_resolution(&repo, None);

        let record = discarded_by_resolution(&repo, &published, deadline())
            .await
            .expect("a deleted file is an answer, not an error");

        assert!(
            record.contains("seed.txt") && record.contains("theirs"),
            "a file that went away loses everything the branch put in it: {record}"
        );
    }

    /// An ordinary merge is not a resolution, and asking what one discarded is a question about a
    /// commit that never brought a resolution in. It answers rather than inventing: `^2^2` does not
    /// exist, and saying so is what stops the accounting pass from writing a number nobody can trace.
    #[tokio::test]
    async fn an_ordinary_merge_has_no_resolution_to_account_for() {
        let _lock = crate::execution::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-plain-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("merge"),
                OsStr::new("--no-ff"),
                OsStr::new("-m"),
                OsStr::new("plain merge"),
                OsStr::new("feat/x"),
            ]
        ));

        let refused = discarded_by_resolution(&repo, &sha_of(&repo, "master"), deadline())
            .await
            .expect_err("there is no resolution under an ordinary merge");
        assert!(
            refused.contains("not a merge commit"),
            "the refusal has to say which commit it could not walk: {refused}"
        );
    }
}
