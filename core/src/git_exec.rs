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
// **Task 7 must delete this attribute** — it spawns the worker, which is what gives this module its
// first production caller — then fix what the compiler reports rather than putting it back: anything
// still dead once a caller exists is dead for a reason worth reading. Naming the task rather than
// the condition is deliberate: a condition is not greppable and nobody is watching for it. If one
// item genuinely has no caller yet, narrow it to an
// `#[allow(dead_code)]` on that item carrying the reason — do not keep the blanket. `vcs.rs:17-30`
// is this same instruction, and says at length why the descriptive version of it rots.
#![cfg_attr(not(test), allow(dead_code))]

// `Outcome` comes from `vcs.rs`, and that is the right direction of dependency: `git_exec` produces
// outcomes for the queue to record, and knows nothing about rows, claims or ordering.
use crate::vcs::Outcome;
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
    /// stdout then stderr, kept to the last `OUTPUT_TAIL_BYTES` and marked when that clipped it.
    /// What the row records — see `tail` for why the diagnostic goes last.
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
/// **`git` and `add_worktree` are the only sanctioned production entries**, and a new caller belongs
/// behind one of them rather than here: they are where whatever is left of the operation's budget is
/// computed and an already-spent one is refused *before* a child is spawned, which `output()` would
/// otherwise do eagerly. Tasks 4-7 add call sites; one that reaches past those two takes its
/// `Duration` from somewhere else and quietly loses that gate. Naming them makes the gate greppable
/// rather than conventional. The tests below call this directly on purpose — they are testing the
/// transport itself.
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
        output_tail: tail(&output.stdout, &output.stderr),
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

/// stderr **last**, because this keeps a tail and a tail drops what comes first.
///
/// git says what went wrong on stderr — `CONFLICT`, `fatal:` — and that line is the only diagnostic
/// the row carries. Putting it first would guarantee it is the first thing dropped, which is exactly
/// backwards: `clean -fd` prints a line per file and a large merge a diffstat, in repositories the
/// daemon does not control, so stdout alone can be the size that forces the truncation. Last, it
/// survives; and when stderr alone overruns the ceiling, what is kept is the END of stderr, which is
/// where git puts its `fatal:` line.
fn tail(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = String::from_utf8_lossy(stdout).into_owned();
    if !combined.is_empty() && !stderr.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&String::from_utf8_lossy(stderr));
    if combined.len() <= OUTPUT_TAIL_BYTES {
        return combined;
    }
    // Slicing a `String` off a byte offset panics mid-character, and lossy conversion guarantees
    // valid UTF-8 but says nothing about where the boundaries fall.
    let wanted = combined.len() - OUTPUT_TAIL_BYTES;
    let start = (wanted..combined.len())
        .find(|index| combined.is_char_boundary(*index))
        .unwrap_or(combined.len());
    // The same marker `gate.rs`'s `TailBuffer::render` prepends, spelled identically on purpose: a
    // bare 8 KiB cannot be told apart from the tail of a 2 MB one, and two spellings of the same
    // notice would make the row harder to read than one.
    format!("…[output truncated; showing tail]\n{}", &combined[start..])
}

/// The prefix of the daemon's own worktree directory inside a project's worktree root.
///
/// Named, and `pub`, because `worktree.rs`'s orphan sweeper must be provably unable to see it: spec
/// §7.1 established that `owner_from_dir_name` only claims `run-<id>` and `job-<id>`, so this name is
/// invisible to the GC *by construction* — which is an accident of a parser until a test pins it.
/// Task 8 writes that test, against `integration_worktree` itself rather than against this string.
pub const INTEGRATION_PREFIX: &str = "integration-";

/// Where this project's integration worktree lives.
///
/// **Qualified by the project's directory name, and that is not decoration.**
/// `worktree::worktree_root` returns `NUCLEOS_WORKTREE_ROOT` *verbatim when it is set*, ignoring
/// `project_root` entirely (`worktree.rs:25-32`) — and the daemon's own error messages tell users to
/// set it (`worktree.rs:107-111`). One unqualified `integration` directory would then be shared by
/// every project on the machine — and sharing it is worse than refusing it, because the sharing is
/// silent: the second project finds a directory that exists and is a perfectly valid worktree, so
/// nothing declines. Run worktrees do not have this problem, because run and job ids are already
/// globally unique; a fixed name is not.
///
/// Two projects whose directory *leaf names* match still collide under that override, and the way
/// that fails is worth stating exactly rather than hand-waving: the second project finds the
/// directory already there, takes the reset path, and computes inside the **first** project's
/// worktree and object database. Nothing is published — `publish` reads the target ref from the
/// second project's root and finds a sha the first project's merge never saw — but the failure
/// arrives late, wearing a message about the branch having moved. Left unfixed here and carried
/// forward; the fix, when it is wanted, is a name derived from the whole project path rather than
/// its last component.
pub fn integration_worktree(project_root: &Path) -> std::path::PathBuf {
    let project = project_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_owned());
    crate::worktree::worktree_root(project_root).join(format!("{INTEGRATION_PREFIX}{project}"))
}

/// A merge that exists as an object and has not been published.
///
/// `Debug` because the tests `expect_err` on a `Result<Computed, Outcome>`, and `expect_err` is
/// bounded on the *success* type being printable.
#[derive(Debug)]
pub struct Computed {
    /// Where the target branch was when the merge was computed against it. The compare-and-swap
    /// value: publishing is only safe if the branch is still here.
    pub old: String,
    pub new: String,
    /// What the merge printed, for the row to record. `publish` moves it into the `Outcome`, which
    /// is the reader Task 3 wrote a `cfg_attr(test, expect(dead_code))` here to wait for — the
    /// `expect` was written to delete itself at exactly this task, and did.
    pub output_tail: String,
}

/// Computes `source` into `target` on a detached HEAD in the daemon's integration worktree.
///
/// `Err` is the outcome to record, not an error to propagate: a conflict is the answer to the
/// question that was asked, and every failure of a compute *is* how that request ended. The single
/// call site would map any other type onto an `Outcome` immediately.
///
/// `--no-ff` always, including when a fast-forward would do. The merge commit's first parent is then
/// always the old target, which is exactly what makes every publish a fast-forward (spec §6.3) —
/// including the case that looks backwards, merging `master` into an agent's own branch, where the
/// branch's own tip is the first parent and the agent's worktree can therefore be fast-forwarded to
/// it.
pub async fn compute_merge(
    project_root: &Path,
    source: &str,
    target: &str,
    deadline: std::time::Instant,
) -> Result<Computed, Outcome> {
    let integration = prepare_integration_worktree(project_root, deadline).await?;

    // Detach rather than checkout: `--detach` is permitted even when `target` is checked out in
    // another worktree, which is the ordinary case — the user is standing on it.
    let checkout = git(&integration, &["checkout", "--detach", target], deadline).await?;
    if !checkout.succeeded() {
        return Err(failed(
            format!("could not check out {target} to merge into"),
            &checkout,
        ));
    }

    let old = revision(&integration, "HEAD", deadline).await?;

    let merge = git(
        &integration,
        &[
            "merge",
            "--no-ff",
            "-m",
            &format!("merge {source} into {target}"),
            source,
        ],
        deadline,
    )
    .await?;
    if !merge.succeeded() {
        // Best-effort: the next operation resets this worktree anyway, and a failure to abort must
        // not replace the conflict — the conflict is what the caller needs to read.
        let _ = git(&integration, &["merge", "--abort"], deadline).await;
        return Err(failed(
            format!("merging {source} into {target} failed"),
            &merge,
        ));
    }

    let new = revision(&integration, "HEAD", deadline).await?;
    Ok(Computed {
        old,
        new,
        output_tail: merge.output_tail,
    })
}

/// One git command, given whatever is left of the operation's budget.
///
/// A spent budget is refused **here, before anything is spawned**, and that is not the same as
/// letting `run_git` time it out. `tokio::process::Command::output()` spawns the child eagerly, as
/// the argument to `timeout` is evaluated — so handing `run_git` a `Duration::ZERO` would launch git
/// and immediately kill it. For `rev-parse` that is merely wasteful; for `merge` or `update-ref` it
/// is a mutation killed mid-flight, which is a worse thing to do to a repository than declining to
/// start. `run_git`'s own zero handling stays as the transport-level backstop it was written to be.
async fn git(
    repo: &Path,
    args: &[&str],
    deadline: std::time::Instant,
) -> Result<CommandResult, Outcome> {
    let budget = remaining(deadline, &args.join(" "))?;
    let arguments: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    run_git(repo, &arguments, budget)
        .await
        // An `Outcome::Failed` with an empty tail, which is right whenever no command produced
        // output: git did not run, so there is nothing it printed. Not `Unexecutable` — that one
        // means the *row* could not be executed, and this row was fine; the subprocess was not.
        .map_err(|reason| Outcome::Failed {
            reason,
            exit_code: None,
            output_tail: String::new(),
        })
}

/// What is left of the operation's budget, or the outcome to record if it is already spent.
///
/// Separate from `git` because `add_worktree` needs it too — it is the one call that goes to
/// `run_git` directly, and it must not be the one place a spent budget still spawns a process.
fn remaining(deadline: std::time::Instant, what: &str) -> Result<Duration, Outcome> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(Outcome::Failed {
            reason: format!("the operation ran out of time before `git {what}` could start"),
            exit_code: None,
            output_tail: String::new(),
        });
    }
    Ok(budget)
}

/// The object id `rev` names, or the outcome to record when git will not resolve it.
///
/// Reads `stdout` rather than `output_tail`: the tail is a diagnostic and may carry a truncation
/// marker, while this is a value the caller compare-and-swaps against.
async fn revision(repo: &Path, rev: &str, deadline: std::time::Instant) -> Result<String, Outcome> {
    let resolved = git(repo, &["rev-parse", rev], deadline).await?;
    if !resolved.succeeded() {
        return Err(failed(
            format!("could not resolve {rev} in {}", repo.display()),
            &resolved,
        ));
    }
    Ok(resolved.stdout.trim().to_owned())
}

/// The outcome for a git command that ran and refused: what was being attempted, plus git's own
/// answer, which is the only diagnostic the row will carry.
fn failed(reason: String, result: &CommandResult) -> Outcome {
    Outcome::Failed {
        reason,
        exit_code: result.exit_code,
        output_tail: result.output_tail.clone(),
    }
}

/// Ensures the integration worktree exists and is clean, and returns its path.
///
/// Resetting is safe here in a way it never is in a user's copy, and that asymmetry is the whole
/// design: this directory is the daemon's property, always detached, and nobody edits anything in it.
/// Spec §7 says a dirty integration worktree is reset before the next operation, and this is where.
async fn prepare_integration_worktree(
    project_root: &Path,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, Outcome> {
    let integration = integration_worktree(project_root);

    if tokio::fs::metadata(&integration).await.is_err() {
        return create_integration_worktree(project_root, &integration, deadline).await;
    }

    // **This check runs before any git command is pointed at that directory, and it is load-bearing.**
    // `git -C <dir>` walks UP until it finds a repository. So a directory that exists but is not a
    // worktree does not make `reset --hard` fail — it makes it *succeed* against whatever ancestor
    // repository encloses it — reverting every uncommitted tracked change in that repository, by way
    // of a command that reports success. That is precisely the runtime destruction this function is
    // written to avoid. The two commands below are not equally dangerous, and saying so exactly
    // matters: `reset --hard` is repository-wide, while the `clean -fd` is *cwd-relative* and would
    // remove only this directory's own contents. The reset is the half that destroys work. None of
    // this is hypothetical in the tests either: `space_free_tempdir` places the worktree root inside
    // this very checkout, which is why the test for this guard builds a sacrificial repository
    // between the two rather than letting the walk-up reach the real one.
    //
    // A linked worktree always has a `.git` FILE naming its admin directory, and `is_file` rather
    // than mere existence is deliberate: a `.git` *directory* at that path is a standalone
    // repository somebody put there, which would pass an existence check and then be reset and
    // cleaned. Every other state that reaches here is not a worktree either — an interrupted
    // `worktree add`, a hand-deleted `.git`, a directory somebody created by mistake.
    if !tokio::fs::metadata(integration.join(".git"))
        .await
        .is_ok_and(|entry| entry.is_file())
    {
        return Err(Outcome::Failed {
            reason: format!(
                "{} exists but is not a git worktree; remove that directory and resubmit",
                integration.display()
            ),
            exit_code: None,
            output_tail: String::new(),
        });
    }

    // It already existed, so it may be mid-merge, dirty, or both. `merge --abort` fails loudly when
    // there is no merge to abort, which is the ordinary case — its exit code is deliberately unread.
    let _ = git(&integration, &["merge", "--abort"], deadline).await;
    let reset = git(&integration, &["reset", "--hard"], deadline).await?;
    if !reset.succeeded() {
        // A `.git` that is present but no longer resolves — the project was re-cloned, so the admin
        // directory it names is gone. Reported rather than repaired, for the reason `worktree.rs`
        // guards its own deletes with `is_dangerous_removal_path`: a runtime `remove_dir_all` that
        // is wrong once is far worse than a merge that fails with an actionable message, and that
        // guard is not free to reproduce.
        return Err(failed(
            format!(
                "{} is a worktree but is not usable; remove that directory and resubmit",
                integration.display()
            ),
            &reset,
        ));
    }
    // `-fd`, not `-fdx`: ignored files are build output that costs nothing to keep and minutes to
    // rebuild, and nothing git ignores can affect a merge.
    //
    // Discarded like the abort above, but not for the same reason and not for free: a failing
    // `clean` is abnormal rather than ordinary — a file held open by an indexer or an editor is the
    // usual Windows cause — and what it then leaves behind resurfaces as the next command's "could
    // not check out <target> to merge into", which does not look related to it. Accepted anyway,
    // because the checkout is the step that actually knows whether the litter is in the way, and
    // refusing here would fail operations it would have completed.
    let _ = git(&integration, &["clean", "-fd"], deadline).await;
    Ok(integration)
}

/// Creates it, retrying once behind a `worktree prune`.
///
/// The retry is not defensive padding — it is the only way out of one specific state. A directory
/// deleted by hand while git still holds a row for it makes `worktree add` refuse that path forever,
/// and `prune` is what drops rows whose directory is missing (`worktree.rs:368-373`): exactly that
/// row, and by construction only rows already describing something that is gone.
///
/// Pruning is deliberately **not** done up front on every operation. It rewrites the worktree
/// registry for the whole project, including the rows `worktree.rs` owns for run and job worktrees,
/// and doing that on every merge would quietly make this module a co-owner of a lifecycle it has no
/// business in. Reaching for it only after `worktree add` has actually failed keeps the blast radius
/// to the case that needs it.
async fn create_integration_worktree(
    project_root: &Path,
    integration: &Path,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, Outcome> {
    if let Some(parent) = integration.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| Outcome::Failed {
                reason: format!("could not create the worktree root: {error}"),
                exit_code: None,
                output_tail: String::new(),
            })?;
    }

    let mut added = add_worktree(project_root, integration, deadline).await?;
    if !added.succeeded() {
        let _ = git(project_root, &["worktree", "prune"], deadline).await;
        added = add_worktree(project_root, integration, deadline).await?;
    }
    if !added.succeeded() {
        return Err(failed(
            "could not create the integration worktree".to_owned(),
            &added,
        ));
    }
    Ok(integration.to_path_buf())
}

/// The only argv in this module with a path in it, and therefore the only *production* caller of
/// `run_git` that is not `git`. The test module below calls it directly three more times, testing
/// the transport rather than going through this module's budget gate.
async fn add_worktree(
    project_root: &Path,
    integration: &Path,
    deadline: std::time::Instant,
) -> Result<CommandResult, Outcome> {
    let budget = remaining(deadline, "worktree add")?;
    run_git(
        project_root,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            integration.as_os_str(),
        ],
        budget,
    )
    .await
    .map_err(|reason| Outcome::Failed {
        reason,
        exit_code: None,
        output_tail: String::new(),
    })
}

/// Publishes a computed merge onto `target`, by whichever of spec §6.3's routes applies.
///
/// Takes the `Computed` by value: it is consumed exactly once, and a publish that could be attempted
/// twice against the same `old` is a shape worth making impossible rather than documenting.
pub async fn publish(
    project_root: &Path,
    target: &str,
    computed: Computed,
    deadline: std::time::Instant,
) -> Outcome {
    // Nothing to publish: the merge found the target already contained the source.
    if computed.new == computed.old {
        return Outcome::Succeeded {
            sha: computed.new,
            output_tail: computed.output_tail,
        };
    }

    let holder = match worktree_holding(project_root, target, deadline).await {
        Ok(holder) => holder,
        Err(outcome) => return outcome,
    };

    match holder {
        None => publish_by_update_ref(project_root, target, computed, deadline).await,
        // Task 5 renames this binding to `worktree` and replaces the `todo!` — leave it prefixed
        // with an underscore until then, or this task does not compile warning-free.
        Some(_worktree) => todo!("Task 5"),
    }
}

async fn publish_by_update_ref(
    project_root: &Path,
    target: &str,
    computed: Computed,
    deadline: std::time::Instant,
) -> Outcome {
    let reference = format!("refs/heads/{target}");
    // The third argument is git's own compare-and-swap. It is checked and applied inside git, so no
    // gap exists between reading the ref and moving it — which is the whole reason not to do this as
    // a rev-parse followed by an update.
    let updated = match git(
        project_root,
        &["update-ref", &reference, &computed.new, &computed.old],
        deadline,
    )
    .await
    {
        Ok(updated) => updated,
        Err(outcome) => return outcome,
    };
    if !updated.succeeded() {
        return failed(
            format!(
                "{target} moved while the merge was being computed, so it was not published; resubmit"
            ),
            &updated,
        );
    }
    Outcome::Succeeded {
        sha: computed.new,
        output_tail: computed.output_tail,
    }
}

/// The worktree that has `target` checked out, if any.
///
/// Read out of `git worktree list --porcelain`, whose blocks are `worktree <path>` / `HEAD <sha>` /
/// then either `branch refs/heads/<name>` or `detached`. The main checkout is in that list too,
/// which is the point — "the user has master open" is the ordinary answer.
///
/// The daemon's own integration worktree can never be the answer, and not because it is filtered
/// out: it is always detached, so it has no `branch` line to match. Task 3's `--detach` is what makes
/// that true, and `an_integration_worktree_never_holds_the_branch_it_merges_into` — written later in
/// this chunk, and deliberately named here before it exists — is what pins it.
///
/// An error here is `Outcome::Failed` and never `Ok(None)`: "we could not find out" must not read as
/// "nobody has it", which would send the publish down the compare-and-swap route and move a ref out
/// from under somebody. `worktree.rs:321-327` makes the identical argument about the identical
/// command.
async fn worktree_holding(
    project_root: &Path,
    target: &str,
    deadline: std::time::Instant,
) -> Result<Option<std::path::PathBuf>, Outcome> {
    let listed = git(project_root, &["worktree", "list", "--porcelain"], deadline).await?;
    if !listed.succeeded() {
        return Err(failed(
            "could not find out which worktree has the target branch open".to_owned(),
            &listed,
        ));
    }

    let wanted = format!("branch refs/heads/{target}");
    let mut current: Option<std::path::PathBuf> = None;
    for line in listed.stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(std::path::PathBuf::from(path));
        } else if line == wanted {
            return Ok(current);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    // A process-wide guard held across awaits on purpose: it serialises mutation of the shared
    // NUCLEOS_WORKTREE_ROOT override, and there is no multi-thread runtime here to starve.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::ffi::{OsStr, OsString};
    // `Path` explicitly, not via `use super::*`: at Step 1 this file contains nothing BUT this
    // module, so the glob imports nothing and the helpers below would not resolve it.
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct WorktreeRootEnv {
        previous: Option<OsString>,
    }

    impl WorktreeRootEnv {
        fn set(path: &Path) -> Self {
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

    /// `master` with a `feat/x` that touched one file, and back on `master`. Every test in this task
    /// starts here.
    fn repo_with_a_branch_to_merge(prefix: &str) -> (tempfile::TempDir, PathBuf) {
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

    fn sha_of(repo: &Path, revision: &str) -> String {
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
        let _lock = crate::worktree::test_env_lock();
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

    /// Spec §7, first row. A conflict is a reported failure, not a problem this module solves — and
    /// the point of computing on the side is that the user's copy is not where it happens.
    #[tokio::test]
    async fn a_conflicted_merge_fails_without_touching_the_user_s_copy() {
        let _lock = crate::worktree::test_env_lock();
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
            Outcome::Failed { output_tail, .. } => assert!(
                output_tail.contains("CONFLICT"),
                "the row must carry what git said: {output_tail}"
            ),
            other => panic!("a conflict is a Failed, got {other:?}"),
        }

        assert_eq!(sha_of(&repo, "master"), before);
        assert_eq!(
            std::fs::read_to_string(repo.join("seed.txt")).expect("read"),
            "ours\n",
            "the user's file is byte-identical"
        );
    }

    /// `--no-ff` is what makes publishing uniform, and this is the case that proves it is doing
    /// something: without it git would fast-forward and there would be no merge commit to publish.
    #[tokio::test]
    async fn a_merge_that_could_fast_forward_still_produces_a_commit_to_publish() {
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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

    /// Spec §7 ("worktree de integração suja — reposta antes da operação seguinte") and §8, which
    /// lists it as a required lifecycle test.
    ///
    /// This is the branch that runs on **every operation after the first** in a project's life, and
    /// no other test in this chunk reaches it: each of the others starts from a fresh worktree root
    /// and computes exactly once, so they all take the create path. Without this test the reset path
    /// ships unexecuted.
    #[tokio::test]
    async fn a_dirty_integration_worktree_is_reset_before_the_next_operation() {
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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

    /// The target is a branch nobody has checked out — `release`, not `master`. That is the case this
    /// row of §6.3 is for, and it is common: an agent merging into a branch no human is standing on.
    #[tokio::test]
    async fn a_branch_nobody_has_open_is_moved_by_compare_and_swap() {
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-cas-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        assert!(git_ok(
            &repo,
            &[OsStr::new("branch"), OsStr::new("release")]
        ));

        let computed = compute_merge(&repo, "feat/x", "release", deadline())
            .await
            .expect("compute");
        let new = computed.new.clone();

        // Captured before the publish and compared against itself afterwards. Comparing `master` to
        // `HEAD` instead — the shape this assertion started as — cannot fail: in the main checkout
        // `HEAD` is a symref to `refs/heads/master`, so the two resolve to the same object however
        // far a publish strayed. It proved a symref is a symref.
        let master_before = sha_of(&repo, "master");

        let outcome = publish(&repo, "release", computed, deadline()).await;

        match outcome {
            Outcome::Succeeded { sha, .. } => assert_eq!(sha, new),
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
        let _lock = crate::worktree::test_env_lock();
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
}
