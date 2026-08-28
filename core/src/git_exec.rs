//! Running git for the queue: argv, a deadline, and what it printed.
//!
//! Separate from `vcs.rs` because this is process transport and that is a queue —
//! `gate.rs` and `transcribe.rs` are each their own module for the same reason. Nothing here knows
//! what a request is or when it may run; it takes a repository, an argv and a deadline.

// `Outcome` comes from `vcs.rs`, and that is the right direction of dependency: `git_exec` produces
// outcomes for the queue to record, and knows nothing about rows, claims or ordering.
use crate::vcs::Outcome;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex};
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

/// How many git processes this crate has spawned against each repository.
///
/// **A test seam in transport, which wants an argument rather than a shrug.** `map_recency`'s whole
/// reason for existing is a cost — one walk of the recent commits instead of a `git log` per anchor
/// file, ~700 spawns on this repository, on a route the map performs every time it opens — and a
/// cost is not observable in an answer. Every other test of that module passes just as well against
/// the per-path loop it exists to refuse, so the one test that guards the argument has to count
/// processes, and nothing outside this function knows how many were started.
///
/// **Keyed by repository and never one global number**, which is what makes it an assertion instead
/// of a race dressed as one: the suite runs in parallel and a good deal of it spawns git, so a
/// process-wide counter would be read while `vcs`, `worktree` and `gate` were all incrementing it.
/// A fixture in its own temp directory is the only writer of its own row.
///
/// The key is the `repo` path exactly as it was passed and is deliberately not canonicalised: a
/// caller reading its own count hands the same `&Path` it handed to the call, and canonicalising
/// would introduce a second way for the two to differ (`\\?\` prefixes, 8.3 short names) in service
/// of a case no test has.
#[cfg(test)]
pub mod spawns {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex};

    static COUNTS: LazyLock<Mutex<HashMap<PathBuf, usize>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(super) fn record(repo: &Path) {
        *COUNTS
            .lock()
            .expect("the spawn counter is only ever held for one insert")
            .entry(repo.to_path_buf())
            .or_default() += 1;
    }

    /// How many git processes this crate has tried to start against `repo` since the daemon began.
    ///
    /// Attempts and not successes, because the cost being asserted is paid at the `CreateProcess`
    /// and a loop that fails seven hundred times cost seven hundred of them.
    ///
    /// Read before and after rather than compared against zero, because a fixture that also builds
    /// its repository through [`super::run_git`] would start above it.
    pub fn against(repo: &Path) -> usize {
        COUNTS
            .lock()
            .expect("the spawn counter is only ever held for one read")
            .get(repo)
            .copied()
            .unwrap_or(0)
    }
}

/// How long a *finished* git command is given to finish emptying its pipes.
///
/// Its own budget rather than a share of the operation's, for the reason `gate.rs` gives about the
/// identical situation: EOF arrives only when EVERY holder of the write end has closed it, and a
/// `push` hands that handle to ssh and to a credential helper. `child.wait()` returning says the
/// direct child is gone, not that its pipes are. Bounded at five seconds because git's own output is
/// bounded by the size of the change and is already written by the time the process exits — anything
/// still holding the pipe after that is a leftover with nothing left to say, and the `TreeKiller`
/// takes it down rather than this waiting for it.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

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
/// **`git`, `add_worktree`, `repo_key`, `current_branch`, `toplevel`, `origin_and_head`,
/// `branch_exists`, `is_ancestor` and `default_remote_branch` are the only sanctioned production
/// entries**, and a new caller belongs behind one of them rather than here: each is a place where
/// whatever is left of the operation's budget is computed and an already-spent one is refused
/// *before* a child is spawned, which `output()` would otherwise do eagerly. Everything past `git`
/// is on the list because it passes that same test rather than because of when it arrived — each
/// computes its own remaining budget and returns without spawning when there is none. A caller
/// that reaches past this list takes its `Duration` from somewhere else and quietly loses that
/// gate. Naming them makes the gate greppable rather than conventional. The tests below call this
/// directly on purpose — they are testing the transport itself.
///
/// **`map_stamp::digest` is a seventh caller and is deliberately outside that rule**, recorded here
/// so the list above stays true rather than merely old. The gate the six enforce is the VCS queue's
/// per-operation budget, and there is no operation for that one to spend down: it claims no
/// worktree, writes nothing, and runs a `git ls-files` on an HTTP read path with a ceiling of its
/// own. What still applies to it is everything else this function owns — argv, the tree-kill,
/// `GIT_TERMINAL_PROMPT=0`, the drained pipes — which is why it comes through here rather than
/// spawning its own `Command`. Any further caller should have to write a paragraph like this one, or
/// belong behind one of the six.
///
/// Output is buffered whole and truncated afterwards, unlike `gate.rs`, which streams into a
/// `TailBuffer`. That is not an oversight: a gate runs a test suite, which can print without bound
/// for as long as it likes, while a git command's output is bounded by the size of the change. The
/// two are different shapes, so this is a second implementation rather than a duplicate.
///
/// **A deadline takes the whole process TREE down, not just git.** This used to say the opposite, and
/// said it for a defensible reason: a third copy of a subtle process-lifetime guard was a worse bet
/// than the fourth-copy problem it would solve, and nothing here handed git a shell. `Op`'s own doc
/// comment recorded that the sentence would become wrong the day a network operation landed, without
/// anybody editing it — `push` hands git an ssh and a credential helper, and a fetch against a dead
/// network is the exact case spec §7's hung-command row was written about. So the copies were
/// extracted instead (`process_tree.rs`), which is what that comment said the fix would be.
///
/// Two things follow from that, and both are load-bearing rather than incidental:
///
/// - The child is spawned through `process_tree::spawn_in_own_group`, because off Windows killing a
///   tree means killing a process GROUP and a child has to lead one before it can be named.
/// - The draining of the pipes gets its OWN budget (`DRAIN_GRACE`) after the child is reaped. EOF
///   arrives only when every holder of the write end has closed it, and ssh inherited that handle.
///
/// `GIT_TERMINAL_PROMPT=0` and a closed stdin are the other half of the same problem, from the other
/// end. A credential prompt in a process nobody is watching is not a slow command — it is a command
/// that never finishes, holding the repository's only slot until the deadline. With this, the
/// commonest case (HTTPS asking for a username) fails immediately and readably instead. **What it
/// does not cover, stated rather than implied:** an ssh key with a passphrase and no agent still
/// blocks, because that prompt is ssh's rather than git's — it fails at the deadline instead of at
/// once, which is survivable precisely because the tree-kill above now reaches the ssh. Forcing
/// `BatchMode=yes` would fix that case and clobber any `core.sshCommand` the user configured, which
/// is a trade for whoever has the failing key to make, not this module.
pub async fn run_git(
    repo: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<CommandResult, String> {
    let mut command = crate::worktree::git();
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::process_tree::spawn_in_own_group(&mut command);

    // Counted at the one place every git process in this crate goes through, rather than in the
    // module whose cost is being asserted — a counter the caller increments beside its own call is
    // one a refactor moving that call takes with it, which is exactly the refactor the count exists
    // to catch. Compiled out of the daemon entirely; see [`spawns`].
    #[cfg(test)]
    spawns::record(repo);

    let mut child = command
        .spawn()
        .map_err(|error| format!("could not run git: {error}"))?;

    // Declared AFTER the child so it drops FIRST, while tokio's handle still pins the pid — the
    // invariant `TreeKiller` documents, and the only thing that keeps the pid it names ours.
    let mut killer = child.id().map(crate::process_tree::TreeKiller::new);

    let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
    let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
    let stdout_task = tokio::spawn(drain_pipe(
        child.stdout.take().expect("stdout was piped"),
        Arc::clone(&stdout_bytes),
    ));
    let stderr_task = tokio::spawn(drain_pipe(
        child.stderr.take().expect("stderr was piped"),
        Arc::clone(&stderr_bytes),
    ));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        // Deliberately NOT disarming here, for the reason `gate.rs` gives at the same point: the
        // direct child exiting says nothing about whether its pipes are closed.
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            if let Some(killer) = killer.as_mut() {
                killer.kill_now();
            }
            return Err(format!("could not run git: {error}"));
        }
        Err(_) => {
            // The tree goes down HERE rather than at drop, because `child` is still alive at this
            // point and its handle is what stops the pid being reused under the killer.
            if let Some(killer) = killer.as_mut() {
                killer.kill_now();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(format!(
                "git {} timed out after {timeout:?}",
                rendered(args)
            ));
        }
    };

    let drained = async {
        for task in [stdout_task, stderr_task] {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(format!("could not read git's output: {error}")),
                Err(error) => return Err(format!("git's output task failed: {error}")),
            }
        }
        Ok(())
    };
    match tokio::time::timeout(DRAIN_GRACE, drained).await {
        Ok(Ok(())) => {
            if let Some(killer) = killer.as_mut() {
                killer.disarm();
            }
        }
        Ok(Err(reason)) => return Err(reason),
        // The exit code survives a stuck drain: git ran and said how it ended, and only the tail is
        // short. `gate.rs` makes the same call about the same situation. The killer stays ARMED, so
        // dropping it takes the leftover holder of the pipe down on the way out.
        Err(_) => tracing::warn!(
            command = %rendered(args),
            "git's output did not finish draining; reporting the result with a truncated tail"
        ),
    }

    let stdout = std::mem::take(&mut *stdout_bytes.lock().expect("output buffer is not poisoned"));
    let stderr = std::mem::take(&mut *stderr_bytes.lock().expect("output buffer is not poisoned"));
    Ok(CommandResult {
        exit_code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        output_tail: tail(&stdout, &stderr),
    })
}

/// Reads one pipe to EOF into a buffer the caller keeps, so a drain that has to be abandoned still
/// leaves behind what it managed to read.
///
/// A `std::sync::Mutex` rather than tokio's, and never held across the `await`: the lock is taken per
/// chunk and released before the next read, which is what keeps a blocking lock correct in an async
/// task.
async fn drain_pipe<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    into: Arc<Mutex<Vec<u8>>>,
) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        into.lock()
            .expect("output buffer is not poisoned")
            .extend_from_slice(&chunk[..read]);
    }
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
        // **The reason names the owner, because the asker's instinct is to fix it.** An agent that
        // has just finished work and is told "merging failed" will reach for the conflict, and it
        // is the one thing here that is not its to reach for: the merge happened in an integration
        // worktree it does not have, was aborted, and left nothing conflicted anywhere. There is no
        // conflicted state in its copy to resolve — only the temptation to manufacture one.
        //
        // What IS the asker's is the other direction, and the message says so rather than leaving
        // it to be guessed: bringing the target INTO its branch is an ordinary queue operation, and
        // resolving there is resolving in its own worktree, on its own branch, where it belongs.
        return Err(Outcome::Escalated {
            reason: format!(
                "merging {source} into {target} conflicts, so nothing was published and no copy \
                 was left conflicted. This is the queue's to carry and not yours to fix from \
                 here — the merge ran in an integration worktree you do not have. To clear it, \
                 bring {target} into {source} in your own worktree (an ordinary queue operation), \
                 resolve it there, and ask again."
            ),
            exit_code: merge.exit_code,
            output_tail: merge.output_tail.clone(),
        });
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

/// The canonical identity of the repository at `path`, for the queue to take its lock on.
///
/// `--git-common-dir` rather than `--git-dir`: a linked worktree's `--git-dir` is its own private
/// `.git/worktrees/<name>`, so keying on it would give every worktree of one repository a key of its
/// own, and the queue would run two merges against the same refs at once — the single thing it
/// exists to prevent. `--git-common-dir` is one value for all of them.
///
/// **`--show-toplevel` is checked as well, and it is the guard rather than decoration.** `git -C
/// <dir>` walks UP until it finds a repository, so `rev-parse` inside an ordinary subdirectory
/// succeeds and answers about the enclosing one. Requiring the caller's own path to BE the top level
/// turns "found a repository" into "found this repository".
///
/// **What that guard costs, stated because it is a restriction and not a free check:** the queue
/// serves projects whose recorded root is the root of a working-tree repository. A project rooted at
/// a package inside a monorepo is refused, and so is a bare repository — `--show-toplevel` exits 128
/// there ("this operation must be run in a work tree"). Both are correct refusals today, because
/// this module computes merges in a worktree it adds under that root; both are also the first thing
/// to revisit if a project ever needs to be rooted deeper.
///
/// Both paths are canonicalised before being compared, and that comparison is where canonicalisation
/// is load-bearing: git answers with forward slashes and the caller holds a Windows path, so a raw
/// comparison would reject every valid root. Canonicalising the RETURNED key buys less than it looks
/// — git already normalises drive-letter case and `.`/`..` itself (measured) — and is kept for
/// junctions and symlinks, where two spellings genuinely reach one directory.
///
/// The root of the working tree `path` sits in, whether `path` is that root or a directory under it.
///
/// **The one question `repo_key` and `current_branch` deliberately refuse to answer**, and it exists
/// because a caller arrived that genuinely does not know: an interactive session's `cwd` is wherever
/// the person happened to be standing, which is a subdirectory far more often than not. Both of the
/// others demand the path already BE the top level, and that demand is right for them — it turns
/// "found a repository" into "found this repository". This function is how a caller earns the path
/// that satisfies it, rather than each caller inventing its own `rev-parse`.
///
/// Skipping it is the hole, not a shortcut. `git -C <dir>` walks UP, so `current_branch` called on a
/// subdirectory does not fail — it refuses with "not the root", and a caller that reads that refusal
/// as "not a repository, nothing to govern here" hands the session exactly the bypass the gate
/// exists to close. Measured: every one of this repo's own worktrees answers a different toplevel and
/// the SAME common dir.
///
/// **Named `toplevel` and not `worktree_root`, which is taken and means the opposite end of the
/// same word.** `worktree::worktree_root(project_root)` answers "where does this project's worktrees
/// get CREATED" and returns a parent directory; this answers "which working tree am I standing IN"
/// and returns a checkout. Two functions in one crate under one name, differing only by module, is a
/// misreading waiting to happen — and it nearly did: the collision surfaced only because a grep for
/// this function's name found the other one in a worktree it had never been written to. `toplevel`
/// is git's own word for the thing (`--show-toplevel`), so it borrows a name that is already exact.
///
/// A fifth sanctioned entry to `run_git` (see its doc comment): it computes what is left of the
/// budget and refuses before spawning, which is the property that comment protects.
pub async fn toplevel(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<std::path::PathBuf, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before the working tree could be located".to_owned(),
        );
    }

    let result = run_git(
        path,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
        ],
        budget,
    )
    .await?;
    if !result.succeeded() {
        return Err(format!(
            "{} is not inside a git repository: {}",
            path.display(),
            result.output_tail.trim()
        ));
    }
    let Some(toplevel) = result.stdout.lines().next() else {
        return Err(format!(
            "git reported no top level for {} — a bare repository has none",
            path.display()
        ));
    };
    Ok(std::path::PathBuf::from(
        canonical(Path::new(toplevel.trim())).await?,
    ))
}

/// Where a repository points and what it last did, for a folder nobody has registered yet.
///
/// A sixth sanctioned entry to `run_git`: it computes what is left of the budget before each spawn
/// and returns without spawning when there is none, which is the property that list protects.
///
/// **Both halves are optional and neither absence is an error.** A repository with no `origin` is
/// ordinary — a local-only project is a project — and one with no commits is a repository somebody
/// made this morning. The caller is a wizard showing a person what is in a folder, and "there is no
/// remote" is information rather than a failure to look.
pub async fn origin_and_head(
    path: &Path,
    deadline: std::time::Instant,
) -> (Option<String>, Option<String>) {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return (None, None);
    }
    let remote = run_git(
        path,
        &[
            OsStr::new("remote"),
            OsStr::new("get-url"),
            OsStr::new("origin"),
        ],
        budget,
    )
    .await
    .ok()
    .filter(|result| result.succeeded())
    .and_then(|result| {
        result
            .stdout
            .lines()
            .next()
            .map(|line| line.trim().to_owned())
    })
    .filter(|line| !line.is_empty());

    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return (remote, None);
    }
    let head = run_git(
        path,
        &[
            OsStr::new("log"),
            OsStr::new("-1"),
            OsStr::new("--date=short"),
            OsStr::new("--format=%h %ad %s"),
        ],
        budget,
    )
    .await
    .ok()
    .filter(|result| result.succeeded())
    .and_then(|result| {
        result
            .stdout
            .lines()
            .next()
            .map(|line| line.trim().to_owned())
    })
    .filter(|line| !line.is_empty());

    (remote, head)
}

/// Whether `refs/heads/<branch>` exists in the repository at `path`.
///
/// A seventh sanctioned entry to `run_git`, for the reason the others are: it computes what is
/// left of the budget and refuses before spawning. `land.rs` is the one caller — it is what stands
/// between `autopilot_state.integration_branch` and a name that used to resolve and does not any
/// more, so a landing refuses by name instead of failing three git commands deep inside a merge.
///
/// `show-ref --verify --quiet` rather than `rev-parse --verify`: the latter also accepts a sha, a
/// tag, or anything else that resolves, and what this answers is narrower — is there a LOCAL
/// BRANCH by this name, the one thing a landing may target.
pub async fn branch_exists(
    path: &Path,
    branch: &str,
    deadline: std::time::Instant,
) -> Result<bool, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err("the operation ran out of time before the branch could be checked".to_owned());
    }
    let reference = format!("refs/heads/{branch}");
    let result = run_git(
        path,
        &[
            OsStr::new("show-ref"),
            OsStr::new("--verify"),
            OsStr::new("--quiet"),
            OsStr::new(&reference),
        ],
        budget,
    )
    .await?;
    Ok(result.succeeded())
}

/// Whether `ancestor` is reachable from `descendant` — `merge-base --is-ancestor`, read as a bool
/// rather than an `Outcome`, because the one caller (`land.rs`, decision #3) uses this to decide
/// whether to submit at all and has no row yet to write an outcome onto.
///
/// An eighth sanctioned entry to `run_git`, for the reason the others are.
///
/// Non-zero is read as "no" without inspecting which non-zero. `--is-ancestor` uses 1 for a plain
/// "not an ancestor" and something else for "not even a commit", and both branches named here have
/// already been resolved by the caller before this runs — `land::submit` reads `source` off
/// `current_branch` and `target` off `integration_branch`, neither of which hands this a name git
/// cannot find. A future caller that cannot make the same guarantee owes its own check first.
pub async fn is_ancestor(
    path: &Path,
    ancestor: &str,
    descendant: &str,
    deadline: std::time::Instant,
) -> Result<bool, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before the merge base could be checked".to_owned(),
        );
    }
    let result = run_git(
        path,
        &[
            OsStr::new("merge-base"),
            OsStr::new("--is-ancestor"),
            OsStr::new(ancestor),
            OsStr::new(descendant),
        ],
        budget,
    )
    .await?;
    Ok(result.succeeded())
}

/// The branch `origin`'s `HEAD` points at, or `None` when there is no `origin` or no such symbolic
/// ref — never an error, because "cannot derive this way" is not "something is broken", and
/// `land::integration_branch` has two more ways to answer before it has to refuse.
///
/// A ninth sanctioned entry to `run_git`, for the reason the others are.
pub async fn default_remote_branch(
    path: &Path,
    deadline: std::time::Instant,
) -> Result<Option<String>, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before origin's default branch could be read"
                .to_owned(),
        );
    }
    let result = run_git(
        path,
        &[
            OsStr::new("symbolic-ref"),
            OsStr::new("refs/remotes/origin/HEAD"),
        ],
        budget,
    )
    .await?;
    if !result.succeeded() {
        return Ok(None);
    }
    Ok(result
        .stdout
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("refs/remotes/origin/"))
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned))
}

/// A third sanctioned entry to `run_git` (see its doc comment, which names all three): it
/// computes what is left of the budget and refuses before spawning, which is the property that
/// comment exists to protect.
pub async fn repo_key(path: &Path, deadline: std::time::Instant) -> Result<String, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err(
            "the operation ran out of time before the repository could be identified".to_owned(),
        );
    }

    let result = run_git(
        path,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
            OsStr::new("--git-common-dir"),
        ],
        budget,
    )
    .await?;
    // Two refusals, deliberately worded apart. This one is "git found no repository from here at
    // all"; the one below the parse is "git found one, and it is not this directory". They shared a
    // sentence until a mutation pass showed what that cost: with one wording, a test naming either
    // case passed through whichever branch happened to run, and deleting this block outright left
    // the whole suite green. A message is a test's only way to say WHICH guard answered.
    if !result.succeeded() {
        return Err(format!(
            "{} is not inside a git repository: {}",
            path.display(),
            result.output_tail.trim()
        ));
    }

    // Order matters and is fixed by the argv above: `--show-toplevel` first, `--git-common-dir`
    // second. `rev-parse` prints its answers in the order it was asked for them.
    let mut lines = result.stdout.lines();
    let (Some(toplevel), Some(common_dir)) = (lines.next(), lines.next()) else {
        return Err(format!(
            "git did not report both a top level and a common directory for {}",
            path.display()
        ));
    };

    let toplevel = canonical(Path::new(toplevel.trim())).await?;
    if canonical(path).await? != toplevel {
        return Err(format!(
            "{} is not the root of a repository — it sits inside {toplevel}",
            path.display()
        ));
    }

    canonical(Path::new(common_dir.trim())).await
}

/// The branch a worktree has checked out, or why that question has no usable answer.
///
/// Approving a paused run's `git merge X` has to know what X would have been merged INTO, and the
/// answer is the branch the run's own worktree stands on. Nothing else in the system records it:
/// `worktrees.path` is where, not what.
///
/// **`--show-toplevel` is asked for and checked for the reason `repo_key` documents at length**, and
/// the consequence of skipping it is worse here than there. `git -C <dir>` walks UP until it finds a
/// repository, so a path that has been removed — and a resumed run's worktree can be — answers with
/// the enclosing checkout's branch, cheerfully and with exit code 0. The queue would then be handed
/// a merge into whatever branch the MAIN checkout happens to have open. A wrong answer that looks
/// exactly like a right one is the failure mode this guard exists for.
///
/// A detached HEAD is `Ok("HEAD")` rather than an error: it is a fact about the worktree, not a
/// failure to find one out, and `vcs::merge_from_command` is where the decision to refuse it lives —
/// alongside the other reasons a command is not queueable, rather than split across two modules.
///
/// A fourth sanctioned entry to `run_git` (see its doc comment): it computes what is left of the
/// budget and refuses before spawning, which is the property that comment protects.
pub async fn current_branch(path: &Path, deadline: std::time::Instant) -> Result<String, String> {
    let budget = deadline.saturating_duration_since(std::time::Instant::now());
    if budget.is_zero() {
        return Err("the operation ran out of time before the branch could be read".to_owned());
    }

    let result = run_git(
        path,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new("--show-toplevel"),
            OsStr::new("--abbrev-ref"),
            OsStr::new("HEAD"),
        ],
        budget,
    )
    .await?;
    if !result.succeeded() {
        return Err(format!(
            "{} is not inside a git repository: {}",
            path.display(),
            result.output_tail.trim()
        ));
    }

    // Order is fixed by the argv above: the top level first, then HEAD abbreviated.
    let mut lines = result.stdout.lines();
    let (Some(toplevel), Some(branch)) = (lines.next(), lines.next()) else {
        return Err(format!(
            "git did not report both a top level and a branch for {}",
            path.display()
        ));
    };

    let toplevel = canonical(Path::new(toplevel.trim())).await?;
    if canonical(path).await? != toplevel {
        return Err(format!(
            "{} is not the root of a worktree — it sits inside {toplevel}",
            path.display()
        ));
    }

    Ok(branch.trim().to_owned())
}

/// A path in the one spelling the filesystem itself uses.
///
/// On Windows this returns a verbatim path (`\\?\C:\…`). That is fine for a key, whose only job is
/// to compare equal to itself, and it is worth knowing before anyone compares a stored repository
/// key against a stored `project_root` — they are in different spellings on purpose.
pub(crate) async fn canonical(path: &Path) -> Result<String, String> {
    tokio::fs::canonicalize(path)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| format!("could not canonicalise {}: {error}", path.display()))
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

/// `failed`'s sibling for the outcomes a person has to look at. Same columns, different status —
/// see `Outcome::Escalated` for why that distinction is the whole point rather than a label.
fn escalated(reason: String, result: &CommandResult) -> Outcome {
    Outcome::Escalated {
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
            sha: Some(computed.new),
            output_tail: computed.output_tail,
        };
    }

    let holder = match worktree_holding(project_root, target, deadline).await {
        Ok(holder) => holder,
        Err(outcome) => return outcome,
    };

    match holder {
        None => publish_by_update_ref(project_root, target, computed, deadline).await,
        Some(worktree) => {
            publish_by_fast_forward(project_root, &worktree, target, computed, deadline).await
        }
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
        sha: Some(computed.new),
        output_tail: computed.output_tail,
    }
}

/// Fast-forwards the worktree that has `target` open, so HEAD, the index and the files move
/// together.
///
/// It is always *possible* to fast-forward here, and that is not luck: the merge commit's first
/// parent is `computed.old` by construction (Task 3's `--no-ff`), so the branch tip is an ancestor of
/// the new commit. The ref is re-read first because it may no longer be there — and the re-read
/// matters in **both** directions, which is easy to get wrong:
///
/// - The target moved **forward**: the fast-forward is genuinely impossible and git would refuse
///   anyway, so the check only improves the message.
/// - The target was **rewound** — somebody ran `reset --hard` and discarded work: `old~1` is still
///   an ancestor of the merge commit, so the fast-forward is entirely *possible*, and without this
///   check the publish rolls straight over the rewind, restores what they threw away, and reports
///   success. This is the direction the check exists for, and the only one a test can distinguish
///   it by.
///
/// **The refusal is then classified by asking a second question, not by reading git's message.**
/// `Blocked` is terminal and its advice is "commit or stash", so it must mean the user's own work is
/// in the way and nothing else. It would be wrong for a file another process has locked open — the
/// ordinary Windows case, `unable to unlink old 'target/app.exe': Permission denied` — for a worktree
/// git lists but whose directory is gone, for `dubious ownership`, or for a full disk. None of those
/// is fixed by committing, and telling a human to stash would be actively misleading advice on a
/// state that is terminal.
///
/// So: `status --porcelain` in that worktree. Non-empty means the user has work there and `Blocked`
/// is the honest answer; empty, or a status that will not even run, means the fast-forward was
/// possible and something else refused — `Failed`, with git's own output as the diagnostic. It never
/// parses git's prose: `worktree.rs:321-327` is explicit that stderr is localised and
/// version-dependent and must not be classified on. The message is *carried*, so the human reading
/// the row sees which files git named, and never consulted.
///
/// **This is a one-directional guard, not an exact mapping, and it should not be read as one.** It
/// reliably keeps a *clean* worktree out of `Blocked` — which is the case that mattered, since a
/// clean worktree's refusal is never something committing would fix. It does not help when the
/// worktree is dirty for an unrelated reason *and* refuses for another: a locked file in a checkout
/// that also has ordinary uncommitted work still comes back `Blocked` with advice — "commit, stash
/// or move it" — that will not help. That gap is narrow, and it is not silent, because the output
/// tail carries what git actually said.
///
/// The mirror-image gap is *not* here, and that was measured rather than assumed: an **ignored** file
/// in the way would be invisible to `status --porcelain` and would land in `Failed`, but it never
/// reaches the classification — git overwrites an ignored file instead of refusing (checked on git
/// 2.50.1, with `merge.overwriteIgnore` both true and false). The publish therefore replaces such a
/// file silently, which is what a fast-forward does anywhere and not something this route adds.
///
/// `Failed` rather than `Blocked` is also the right way to be wrong: `Blocked` is terminal by
/// design, so a transient problem misfiled there would need a human to notice it and resubmit.
async fn publish_by_fast_forward(
    project_root: &Path,
    worktree: &Path,
    target: &str,
    computed: Computed,
    deadline: std::time::Instant,
) -> Outcome {
    // The mirror of `prepare_integration_worktree`'s guard, and it runs before any git command is
    // pointed at this directory for the same reason: `git -C <dir>` walks UP until it finds a
    // repository. A worktree git still has a row for, whose directory survives but whose `.git` is
    // gone, therefore answers for whatever repository *encloses* it — measured: `worktree list`
    // still emits its `branch refs/heads/<target>` line (with a `prunable` line this module does not
    // read), and `status --porcelain` there exits 0 carrying the enclosing repository's dirt. That
    // lands the row in `Blocked`, terminal, telling a human to commit files in a repository nobody
    // named. `merge --ff-only` walks up too, so where the enclosing repository is the one that owns
    // the merge object, the fast-forward can land somewhere nobody asked us to touch.
    //
    // **Existence only, deliberately — NOT `is_file()`, which is what the integration guard uses.**
    // The two look like they should agree and must not: that guard only ever inspects the daemon's
    // own worktree, which is always linked and so always has a `.git` FILE. This one is handed
    // whatever holds the branch, and the commonest holder in the whole design is the user's main
    // checkout, whose `.git` is a DIRECTORY. `is_file()` here would reject the ordinary case on
    // every merge — `a_branch_somebody_has_open_is_fast_forwarded_in_place` is what goes red if
    // somebody ever "fixes" this into agreeing with the other one.
    //
    // **Both holder tests are needed and neither subsumes the other**, which is worth saying before
    // somebody consolidates them on the grounds that they look alike. Only the one named above can
    // catch that tightening: its holder is the MAIN checkout, so its `.git` is a directory and
    // `is_file()` turns it red. `a_merge_into_a_branch_somebody_has_open_moves_their_whole_worktree`
    // holds the branch in a LINKED worktree, whose `.git` is a file — it would sail through the same
    // change untouched, which is precisely why its subject is the composition end to end and not
    // this guard.
    if tokio::fs::metadata(worktree.join(".git")).await.is_err() {
        return Outcome::Failed {
            reason: format!(
                "{target} is checked out in {}, which is no longer a git worktree; run `git worktree prune` and resubmit",
                worktree.display()
            ),
            exit_code: None,
            output_tail: String::new(),
        };
    }

    let current = match revision(project_root, &format!("refs/heads/{target}"), deadline).await {
        Ok(current) => current,
        Err(outcome) => return outcome,
    };
    if current != computed.old {
        return Outcome::Failed {
            reason: format!(
                "{target} moved while the merge was being computed, so it was not published; resubmit"
            ),
            exit_code: None,
            output_tail: String::new(),
        };
    }

    let merged = match git(worktree, &["merge", "--ff-only", &computed.new], deadline).await {
        Ok(merged) => merged,
        Err(outcome) => return outcome,
    };
    if merged.succeeded() {
        return Outcome::Succeeded {
            sha: Some(computed.new),
            output_tail: computed.output_tail,
        };
    }

    let status = git(worktree, &["status", "--porcelain"], deadline).await;
    // A status that could not run leaves the question unanswered, and unanswered must fall to
    // `Failed`: claiming the user's files are in the way when that was never established is the one
    // wrong answer here.
    let user_work_in_the_way = matches!(
        &status,
        Ok(status) if status.succeeded() && !status.stdout.trim().is_empty()
    );

    if user_work_in_the_way {
        Outcome::Blocked {
            reason: format!(
                // "or move it", because the blocked case this chunk actually tests is an UNTRACKED
                // file, and `git stash` without `-u` does not move one — git's own message for it
                // says "please move or remove them". Advice that does not work on the case in the
                // test is advice that does not work.
                "{target} is checked out in {} with uncommitted work in the way; commit, stash or move it and resubmit",
                worktree.display()
            ),
            output_tail: merged.output_tail,
        }
    } else {
        Outcome::Failed {
            reason: format!(
                "{target} is checked out in {} and could not be fast-forwarded; git's output says why",
                worktree.display()
            ),
            exit_code: merged.exit_code,
            output_tail: merged.output_tail,
        }
    }
}

/// Whether a conflict resolver's branch may be merged at all, checked before it is.
///
/// **The gate protects against "it broke" and never against "it threw work away".** A resolution
/// that keeps one side and discards the other compiles, passes every test, and is indistinguishable
/// from a good one by any measure the suite has. So the two things that cannot be checked afterwards
/// are checked here, and both are structural rather than about content — neither asks what the right
/// answer was, only whether this could possibly be one.
///
/// **A two-parent tip.** The daemon left a merge half-finished in the resolver's worktree, so an
/// ordinary `git commit` on top of that produces a merge commit with both parents and the resolver
/// has to do nothing special to earn it. One parent therefore does not mean "resolved differently";
/// it means the merge was thrown away and something else was committed in its place — a flattened
/// branch, a reset, a cherry-pick. That is refused without looking at the content at all, because
/// the content of a flattened resolution can look perfect.
///
/// **No conflict markers left in the tree.** `git diff --check` reports them, and an agent that
/// stopped halfway commits them without noticing: the file has both sides in it, the tests may even
/// pass if the markers land in a comment or a string, and the merge would publish something no
/// human wrote.
///
/// Refusing here is `Escalated` rather than `Failed` for the same reason a conflict is: somebody has
/// to look, and the row is what tells them so.
async fn verify_resolution(
    project_root: &Path,
    source: &str,
    deadline: std::time::Instant,
) -> Result<(), Outcome> {
    let parents = git(
        project_root,
        &["rev-list", "--parents", "-1", source],
        deadline,
    )
    .await?;
    if !parents.succeeded() {
        return Err(escalated(
            format!("could not read {source}'s tip to verify the resolution"),
            &parents,
        ));
    }
    // `rev-list --parents -1` prints "<commit> <parent>..." — so the parent count is the field count
    // minus the commit itself.
    if parents.stdout.split_whitespace().count() - 1 < 2 {
        return Err(Outcome::Escalated {
            reason: format!(
                "{source} was supposed to be a resolved merge and its tip has fewer than two \
                 parents, so the merge it was asked to resolve is not in it. Nothing was merged. A \
                 flattened resolution can look entirely correct and still be one side of the work \
                 thrown away, which is why this is refused without reading the content."
            ),
            exit_code: parents.exit_code,
            output_tail: parents.output_tail,
        });
    }

    let markers = git(
        project_root,
        &["diff", "--check", &format!("{source}^1"), source],
        deadline,
    )
    .await?;
    // Non-zero here is `--check` reporting, not git failing: it exits 2 when it finds markers or
    // whitespace errors. The tail is what says which, and it goes in the row.
    if !markers.succeeded() && markers.output_tail.contains("conflict marker") {
        return Err(Outcome::Escalated {
            reason: format!(
                "{source} still has conflict markers committed in it, so the resolution was left \
                 half-done. Nothing was merged."
            ),
            exit_code: markers.exit_code,
            output_tail: markers.output_tail,
        });
    }
    Ok(())
}

/// How many of the incoming branch's files are examined for lost work before the record says it
/// stopped looking. A resolution of a long-lived branch can touch hundreds, and each one costs a
/// `git show` inside the same budget the merge itself came out of.
const DISCARD_FILE_CEILING: usize = 200;

/// How many of the missing lines are quoted back. The COUNTS are always complete; this bounds only
/// the excerpt, because the column is read by a person and not by a diff tool.
const DISCARD_SAMPLE_LINES: usize = 8;

/// `git`, for a reader that is not executing a queued request and so has no `Outcome` to return.
///
/// It goes through `remaining` rather than reading the clock itself, and that is the whole point:
/// the gate that refuses a spent budget BEFORE a child is spawned lives in one place, and a second
/// entry that re-derived it would be exactly the kind of guarantee that quietly stops holding. Only
/// the error SHAPE differs, and it differs at the edge — `remaining` reports an `Outcome` because
/// every other caller is answering for a row, and this one answers for a background pass that has
/// no row to fail.
async fn git_read(
    repo: &Path,
    args: &[&str],
    deadline: std::time::Instant,
) -> Result<CommandResult, String> {
    let budget = match remaining(deadline, &args.join(" ")) {
        Ok(budget) => budget,
        // `remaining` builds this variant and no other; the second arm is the compiler's price for
        // that being a fact about the function rather than about the type.
        Err(Outcome::Failed { reason, .. }) => return Err(reason),
        Err(other) => return Err(format!("the budget could not be read: {other:?}")),
    };
    let arguments: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    run_git(repo, &arguments, budget).await
}

/// What the incoming branch added and the published resolution does not have, worked out from the
/// commits.
///
/// **This is the half of the guarantee `verify_resolution` cannot give.** That one refuses the two
/// shapes a resolution CANNOT be right in — a flattened merge, committed conflict markers — and both
/// are structural, so both can be refused without reading a line. What neither it nor the gate can
/// see is the resolution that kept one side and dropped the other: it compiles, it passes, and by
/// every measure the suite has it is indistinguishable from a good one. So it is published, and then
/// this says what it cost.
///
/// **Computed, never reported.** An agent that loses work and says it did not is exactly the case
/// this exists to catch, so nothing the agent wrote is an input.
///
/// It works from the published merge commit alone and needs no branch name, which matters more than
/// it looks: by the time this runs the resolution's branch is merged, so the worktree GC's
/// `git branch -d` is entitled to delete it. Everything needed is reachable from what was published
/// — `published^2` is the resolution, `published^2^2` is the branch the resolver was asked to bring
/// in — and shas outlive names.
///
/// **What "missing" means here, stated because the record has to be read literally:** a line the
/// incoming branch ADDED, relative to where the two sides parted, that appears nowhere in the
/// published file. A resolution that rewrote a line to carry both intents is reported by this, and
/// that is a false positive worth having — the alternative is a judgement about meaning, and a
/// mechanism that guesses at meaning is one nobody can act on. The wording says "not present
/// verbatim" for that reason.
pub async fn discarded_by_resolution(
    project_root: &Path,
    published: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let resolution = second_parent(project_root, published, deadline).await?;
    let incoming = second_parent(project_root, &resolution, deadline).await?;
    let first = format!("{resolution}^1");

    let base = git_read(project_root, &["merge-base", &first, &incoming], deadline).await?;
    if !base.succeeded() {
        return Err(format!(
            "could not find where {incoming} and the target parted: {}",
            base.output_tail
        ));
    }
    let base = base.stdout.trim().to_owned();

    let listed = git_read(
        project_root,
        &["diff", "--name-only", &base, &incoming],
        deadline,
    )
    .await?;
    if !listed.succeeded() {
        return Err(format!(
            "could not list what {incoming} changed: {}",
            listed.output_tail
        ));
    }
    let files: Vec<&str> = listed
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    let examined = files.len().min(DISCARD_FILE_CEILING);

    let mut per_file: Vec<(String, usize)> = Vec::new();
    let mut samples: Vec<String> = Vec::new();
    let mut total = 0usize;
    for file in &files[..examined] {
        let added = added_lines(project_root, &base, &incoming, file, deadline).await?;
        if added.is_empty() {
            continue;
        }
        // A file the resolution removed outright answers non-zero here, and every line it added is
        // then missing — which is the right reading, and the reason this is not an error.
        let published_file = git_read(
            project_root,
            &["show", &format!("{published}:{file}")],
            deadline,
        )
        .await?;
        let kept: std::collections::HashSet<&str> = if published_file.succeeded() {
            published_file.stdout.lines().map(str::trim).collect()
        } else {
            std::collections::HashSet::new()
        };
        let missing: Vec<&String> = added
            .iter()
            .filter(|line| !kept.contains(line.as_str()))
            .collect();
        if missing.is_empty() {
            continue;
        }
        total += missing.len();
        per_file.push(((*file).to_owned(), missing.len()));
        for line in missing {
            if samples.len() < DISCARD_SAMPLE_LINES {
                samples.push(format!(
                    "{file}: {}",
                    line.chars().take(120).collect::<String>()
                ));
            }
        }
    }

    // Named rather than swallowed: a ceiling nobody is told about reads as "we looked everywhere and
    // found nothing", which is the one thing this record must never imply.
    let truncated = files.len() > examined;
    if total == 0 {
        return Ok(if truncated {
            format!(
                "nothing, in the first {examined} of {} files — the rest were not examined",
                files.len()
            )
        } else {
            "nothing".to_owned()
        });
    }

    // **The count AND the files go on the first line**, which is not formatting: the feed carries
    // one line of this and the feed is the only surface a person passes without going looking. A
    // headline that says "12 lines went missing" and makes them query the database to find out
    // where is a headline that gets skipped.
    let where_ = per_file
        .iter()
        .map(|(file, count)| format!("{file} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut record = format!(
        "{total} line(s) that {incoming} added are not present verbatim in what was published — \
         {where_}"
    );
    if truncated {
        record.push_str(&format!(
            " (the first {examined} of {} files were examined)",
            files.len()
        ));
    }
    record.push_str("\n\nfor example:\n");
    for sample in &samples {
        record.push_str(&format!("  {sample}\n"));
    }
    Ok(record)
}

/// The second parent of `revision`, which for a merge commit is the side that was brought in.
async fn second_parent(
    project_root: &Path,
    revision: &str,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let parents = git_read(
        project_root,
        &["rev-list", "--parents", "-1", revision],
        deadline,
    )
    .await?;
    if !parents.succeeded() {
        return Err(format!(
            "could not read {revision}'s parents: {}",
            parents.output_tail
        ));
    }
    parents
        .stdout
        .split_whitespace()
        .nth(2)
        .map(str::to_owned)
        .ok_or_else(|| format!("{revision} is not a merge commit, so it brought nothing in"))
}

/// The lines `incoming` added to `file` since the two sides parted, trimmed and without the blanks.
///
/// `-U0` so nothing but the changed lines comes back. Blank lines are dropped because a blank
/// "missing" from the published file is noise in a record meant to be read by a person.
async fn added_lines(
    project_root: &Path,
    base: &str,
    incoming: &str,
    file: &str,
    deadline: std::time::Instant,
) -> Result<Vec<String>, String> {
    let diff = git_read(
        project_root,
        &["diff", "-U0", base, incoming, "--", file],
        deadline,
    )
    .await?;
    if !diff.succeeded() {
        return Err(format!("could not read what changed in {file}"));
    }
    Ok(diff
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix('+'))
        .filter(|line| !line.starts_with("++"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
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

/// The `VcsExecutor` the daemon actually runs.
///
/// The whole type is a deadline: everything else it needs comes from the claimed request, which is
/// the only thing that knows which repository and which operation.
pub struct GitExecutor {
    pub timeout: Duration,
}

impl Default for GitExecutor {
    fn default() -> Self {
        Self {
            timeout: OPERATION_TIMEOUT,
        }
    }
}

#[async_trait::async_trait]
impl crate::vcs::VcsExecutor for GitExecutor {
    async fn execute(&self, request: &crate::vcs::ClaimedRequest) -> Outcome {
        let project_root = Path::new(&request.project_root);
        // The budget starts here, once. Every git command inside the operation spends from it.
        let deadline = std::time::Instant::now() + self.timeout;
        // Also the only production reader of `ClaimedRequest::project_id` — see Task 7 Step 4, which
        // is where its absence would otherwise surface as a dead-code error. It earns its place
        // regardless: a merge in the daemon log without the repository it ran against is unreadable
        // the moment two projects are busy.
        tracing::info!(
            vcs_request_id = request.id,
            project_id = %request.project_id,
            op = request.op.kind(),
            "vcs: executing"
        );
        // **The third `.git` guard, on the one directory this module neither creates nor reads out
        // of git.** `prepare_integration_worktree` guards the directory it makes and
        // `publish_by_fast_forward` guards the one `worktree list` named; `project_root` arrives on
        // the queue row, and `worktree add`, `worktree list` and `update-ref` all run with `-C`
        // pointed at it. `git -C <dir>` walks UP, so a root that exists and is not a repository does
        // not fail — the whole operation runs against whatever repository ENCLOSES it. Measured: the
        // merge computed, the enclosing repository's `master` moved, and the row came back
        // `Succeeded` naming a sha from a repository nobody named.
        //
        // **A backstop now, and no longer the first line of defence.** `vcs::resolve_repo` puts the
        // root through `repo_key` before a row can be inserted at all, and that check is strictly
        // stronger than this one: it requires the root to BE the repository's top level, where this
        // only requires it to contain a `.git`. What is left for this guard is the window between
        // the two — a root that stopped being a repository between submitting and running, which a
        // request queued behind two slow merges has plenty of time to do — and any future caller
        // that reaches `execute` without having gone through `resolve_repo`. Both are real, and
        // naming them is the point: a guard nobody can say what it still catches is a guard somebody
        // eventually deletes.
        //
        // **Existence, not `is_file()`** — a project root is a main checkout, whose `.git` is a
        // DIRECTORY. `publish_by_fast_forward`'s guard states that distinction at length and this is
        // the same case for the same reason; it is not repeated here so there is one place to fix if
        // it is ever wrong.
        if tokio::fs::metadata(project_root.join(".git"))
            .await
            .is_err()
        {
            return Outcome::Failed {
                reason: format!(
                    "{} is not a git repository; check the project's root and resubmit",
                    project_root.display()
                ),
                exit_code: None,
                output_tail: String::new(),
            };
        }
        // Deliberately exhaustive with no `_` arm: a wildcard here would let a variant ship with no
        // executor and no compile error — a request that queues, claims the repository, and reports
        // success having done nothing.
        match &request.op {
            crate::vcs::Op::Merge { source, target } => {
                // A resolver's output is verified BEFORE it is merged, and only a resolver's. See
                // `verify_resolution` for what is checked and why the checks are shaped that way.
                if request.from_resolution
                    && let Err(outcome) =
                        verify_resolution(project_root, source.as_str(), deadline).await
                {
                    return outcome;
                }
                match compute_merge(project_root, source.as_str(), target.as_str(), deadline).await
                {
                    Ok(computed) => {
                        // Measured on the commit `compute_merge` left at the integration worktree's
                        // HEAD — the tree `target` is about to become — and BEFORE `publish` moves
                        // anything.
                        //
                        // `integration_worktree` and not `prepare_integration_worktree`: the first
                        // is a pure path function, and the second would `merge --abort`, `reset
                        // --hard` and `clean` the very checkout being measured.
                        let measured = match gate_the_merge(
                            project_root,
                            &integration_worktree(project_root),
                            crate::state::DEFAULT_GATE_TIMEOUT,
                        )
                        .await
                        {
                            Ok(measured) => measured,
                            Err(outcome) => return outcome,
                        };
                        // A gate that RAN buys the publish a fresh git budget, and one that did not
                        // changes nothing. The arithmetic is the reason rather than the taste:
                        // `deadline` was fixed at `now + OPERATION_TIMEOUT` (300s) when this
                        // operation started, and a gate may legitimately outlast it
                        // (`DEFAULT_GATE_TIMEOUT` is 900s). Letting a measurement — which spawns no
                        // git at all — spend the git budget would make every gated merge die at
                        // `update-ref` saying the budget was spent, which reads as a broken queue
                        // rather than as a slow suite. An UNGATED merge keeps the one deadline it
                        // always had, so nothing about this loosens the budget for anybody who did
                        // not ask to be measured.
                        let publishing = if measured {
                            std::time::Instant::now() + self.timeout
                        } else {
                            deadline
                        };
                        publish(project_root, target.as_str(), computed, publishing).await
                    }
                    // Computing failed, which IS how this request ended.
                    Err(outcome) => outcome,
                }
            }
            crate::vcs::Op::Push { remote, branch } => {
                push(project_root, remote.as_str(), branch.as_str(), deadline).await
            }
            crate::vcs::Op::Tag { name, at } => {
                tag(project_root, name.as_str(), at.as_str(), deadline).await
            }
            crate::vcs::Op::Fetch { remote } => {
                fetch(project_root, remote.as_str(), deadline).await
            }
            crate::vcs::Op::BranchDelete { branch } => {
                branch_delete(project_root, branch.as_str(), deadline).await
            }
            crate::vcs::Op::Rebase { branch, onto } => {
                rebase(project_root, branch.as_str(), onto.as_str(), deadline).await
            }
        }
    }
}

/// Measures the computed merge before it is published, when the project asked to be.
///
/// **This is the answer to how a target branch catches somebody else's red.** The queue merged and
/// nothing measured the result: a branch green on its own, merged into a target green on its own,
/// produces a tree neither of them ever built — and the first thing to notice was the next person's
/// build. `gate_before_publish` is a project saying it would rather wait.
///
/// **Where it runs is the whole design.** The commit under measurement is HEAD of the integration
/// worktree, left detached there by `compute_merge`, and `target` has not moved. So a red gate costs
/// a refusal and nothing else — no revert, no reset of a branch other people have already pulled, no
/// history rewritten. The two obvious alternatives are both worse: gating the SOURCE before merging
/// measures something other than what breaks, and merging first and undoing afterwards is a
/// destructive act on shared history that this queue will not take on its own.
///
/// Returns whether a measurement actually happened, because the caller owes a publish that ran after
/// a long gate its own git budget and owes one that did not exactly the budget it already had.
///
/// **Three refusals rather than one, and each is a different sentence to its reader.**
/// - The rules file exists and will not parse: we cannot tell whether this repository wanted its
///   merges measured, and publishing unmeasured is the failure itself. A project with NO file is
///   untouched — `load_schedule_rules` answers `Ok(default)` for an absent one — so this only
///   refuses a repository that has the file and broke it, which is a repository whose scheduler,
///   triggers and run gate are already all dead for the same reason.
/// - `gate_before_publish` with no `gate_command`: the configuration asks for a measurement and
///   names none. Publishing anyway would make the key decorative, which is the exact failure it was
///   added to end.
/// - The gate could not run (`Errored`): "we could not measure" is not "it passed". `gate.rs` keeps
///   those two apart precisely so that callers do not collapse them, and this is a caller.
///
/// **A consequence worth finding by reading rather than by surprise: while this key is on, a branch
/// that CHANGES the gate script cannot land.** `run_gate` compares every script the command names
/// against the project root's copy and refuses to measure when they differ — which is exactly right
/// where it was written (an agent's worktree, where a run could green itself) and reads oddly here,
/// because a branch improving `scripts/gates.sh` is not tampering with anything. It arrives as an
/// `Errored`, so nothing is published and the reason names the file. The remedy is to land that one
/// change with the key off; the alternative — measuring with a script the merge itself supplied —
/// is the door that check exists to hold shut, and it is not worth opening for the convenience.
///
/// `Failed` and never `Escalated` for a red gate. `Escalated` means a person now owns something the
/// queue cannot resolve; a red suite is owned by whoever wrote the branch, and it is fixed where
/// every other red suite is fixed — in their own worktree, on their own branch.
async fn gate_the_merge(
    project_root: &Path,
    integration: &Path,
    timeout: Duration,
) -> Result<bool, Outcome> {
    let refuse = |reason: String| Outcome::Failed {
        reason,
        exit_code: None,
        output_tail: String::new(),
    };

    let rules = match crate::config::load_schedule_rules(project_root) {
        Ok(rules) => rules,
        Err(error) => {
            return Err(refuse(format!(
                "{} could not be read, so whether this repository wants its merges measured is \
                 unknown and nothing was published: {error}",
                crate::config::AUTOPILOT_RULES_PATH
            )));
        }
    };
    if !rules.gate_before_publish {
        return Ok(false);
    }
    let Some(command) = rules.gate_command else {
        return Err(refuse(format!(
            "{} asks for merges to be gated and names no gate_command, so there is nothing to \
             measure this merge with and nothing was published",
            crate::config::AUTOPILOT_RULES_PATH
        )));
    };

    match crate::gate::run_gate(integration, project_root, &command, timeout).await {
        crate::gate::GateOutcome::Passed => Ok(true),
        crate::gate::GateOutcome::Failed { exit_code, output } => Err(Outcome::Failed {
            // Says WHERE the failure lives, because the asker's first instinct will be that their
            // branch is fine — and it may well be. What was measured is the junction, which is a
            // tree neither side had ever built.
            reason: "the merge does not pass this project's gate, so nothing was published. What \
                     was measured is the two branches TOGETHER: each can be green on its own and \
                     still make a tree that is not."
                .to_owned(),
            exit_code: Some(exit_code),
            output_tail: output,
        }),
        crate::gate::GateOutcome::Errored { reason } => Err(refuse(format!(
            "the merge could not be measured, so nothing was published: {reason}"
        ))),
    }
}

/// Replays `branch` onto `onto`, on a detached HEAD in the integration worktree.
///
/// **The holder check comes FIRST, before anything is computed, and that ordering is the operation
/// rather than an optimisation.** Every other executor here computes and then discovers whether it
/// can publish; this one cannot publish to a held branch at all — `Op::Rebase` argues why at length,
/// and the short version is that no git command moves a checkout across a divergence while refusing
/// to destroy. So a computed rebase nobody could publish would be minutes of work and a pile of
/// unreferenced objects, thrown away to say something that was knowable before it started.
///
/// `Blocked` rather than `Failed` for that case, and it is the one place in this module where
/// `Blocked`'s terminal-ness is exactly right: nothing about the repository will change on its own to
/// make this publishable. A person has to check that branch out somewhere else, and until they do a
/// retry would fail identically.
async fn rebase(
    project_root: &Path,
    branch: &str,
    onto: &str,
    deadline: std::time::Instant,
) -> Outcome {
    match worktree_holding(project_root, branch, deadline).await {
        Ok(Some(holder)) => {
            return Outcome::Blocked {
                reason: format!(
                    "{branch} is checked out in {}, and a rebase rewrites it — this queue will not \
                     reset a worktree it does not own. Check that branch out somewhere else and \
                     resubmit.",
                    holder.display()
                ),
                output_tail: String::new(),
            };
        }
        Ok(None) => {}
        Err(outcome) => return outcome,
    }

    let integration = match prepare_integration_worktree(project_root, deadline).await {
        Ok(integration) => integration,
        Err(outcome) => return outcome,
    };

    // Detached, like `compute_merge` — and here it is not merely permitted but required: `git rebase`
    // moves whatever HEAD names, so a rebase on an attached HEAD would move the integration
    // worktree's own branch instead of computing a value to publish.
    let checkout = match git(&integration, &["checkout", "--detach", branch], deadline).await {
        Ok(checkout) => checkout,
        Err(outcome) => return outcome,
    };
    if !checkout.succeeded() {
        return failed(format!("could not check out {branch} to rebase"), &checkout);
    }

    let old = match revision(&integration, "HEAD", deadline).await {
        Ok(old) => old,
        Err(outcome) => return outcome,
    };

    let rebased = match git(
        &integration,
        &["rebase", "--end-of-options", onto],
        deadline,
    )
    .await
    {
        Ok(rebased) => rebased,
        Err(outcome) => return outcome,
    };
    if !rebased.succeeded() {
        // Best effort, and for `compute_merge`'s reason: the next operation resets this worktree
        // anyway, and a failure to abort must not replace the conflict the caller needs to read.
        let _ = git(&integration, &["rebase", "--abort"], deadline).await;
        return failed(format!("rebasing {branch} onto {onto} failed"), &rebased);
    }

    let new = match revision(&integration, "HEAD", deadline).await {
        Ok(new) => new,
        Err(outcome) => return outcome,
    };

    // Already on top of `onto`: git rebased nothing and HEAD did not move. Publishing would be a
    // no-op compare-and-swap, so say so instead.
    if new == old {
        return Outcome::Succeeded {
            sha: Some(new),
            output_tail: rebased.output_tail,
        };
    }

    // The same compare-and-swap `publish_by_update_ref` performs for a merge, and reached the same
    // way: nobody holds this branch — checked above, before any of the work — so there is no
    // worktree whose files have to move with the ref.
    publish_by_update_ref(
        project_root,
        branch,
        Computed {
            old,
            new,
            output_tail: rebased.output_tail,
        },
        deadline,
    )
    .await
}

/// Fetches from `remote`, using whatever refspec that remote is configured with.
///
/// The shortest executor here, and the only one that resolves nothing first — there is no object id
/// to record, because a fetch moves however many tracking refs the remote had news about. Its
/// `Succeeded` therefore carries `None`, which is what made `Outcome::Succeeded::sha` optional.
///
/// What it still needs from everything above it is the part that does not show in these seven lines:
/// this is the operation that hands git an ssh and a credential helper, so it depends on
/// `run_git`'s tree-kill, its `GIT_TERMINAL_PROMPT=0` and its drain grace exactly as `push` does. A
/// fetch against a dead network is the case spec §7's hung-command row was written about.
async fn fetch(project_root: &Path, remote: &str, deadline: std::time::Instant) -> Outcome {
    let fetched = match git(
        project_root,
        &["fetch", "--end-of-options", remote],
        deadline,
    )
    .await
    {
        Ok(fetched) => fetched,
        Err(outcome) => return outcome,
    };
    if !fetched.succeeded() {
        return failed(
            format!("fetching from {remote} failed; git's output says why"),
            &fetched,
        );
    }
    Outcome::Succeeded {
        sha: None,
        output_tail: fetched.output_tail,
    }
}

/// Deletes `branch`, in the spelling that refuses to lose unmerged work.
///
/// **The sha is read BEFORE the delete and recorded, and that ordering is the operation's whole
/// value beyond running the command.** Afterwards the ref is gone and nothing can answer what it
/// pointed at; the row is then the only place holding the one string that undoes this
/// (`git branch <name> <sha>`). Every other executor here resolves first for a different reason — so
/// that what it publishes is a value rather than a re-read — and this one does it so that what it
/// destroys is recoverable.
///
/// `--delete` and never `-D`, which `vcs::branch_delete_from_command` argues at length: the refusal
/// on unmerged commits is git's own, computed from the commit graph, and it is what lets the queue
/// offer this without owning the question of what is safe to lose. A branch checked out in some
/// worktree is refused by git too, with `Cannot delete branch … used by worktree` — a guard this
/// module would otherwise have to reproduce against `worktree list`, and get wrong.
async fn branch_delete(project_root: &Path, branch: &str, deadline: std::time::Instant) -> Outcome {
    let reference = format!("refs/heads/{branch}");
    let sha = match revision(project_root, &reference, deadline).await {
        Ok(sha) => sha,
        Err(outcome) => return outcome,
    };

    let deleted = match git(
        project_root,
        &["branch", "--delete", "--end-of-options", branch],
        deadline,
    )
    .await
    {
        Ok(deleted) => deleted,
        Err(outcome) => return outcome,
    };
    if !deleted.succeeded() {
        return failed(
            format!("deleting {branch} failed; git's output says why"),
            &deleted,
        );
    }

    Outcome::Succeeded {
        sha: Some(sha),
        output_tail: deleted.output_tail,
    }
}

/// Creates a lightweight tag at the tip of `at`.
///
/// The sha is resolved first and the tag written AT that object id rather than at the branch name,
/// for the reason `push` gives at length: what the row records has to be a statement about what
/// happened, not a re-reading of a ref that other queued operations are moving. Here it buys one
/// thing more — the tag and the row cannot disagree even if the branch advances between the two git
/// commands, which on a busy repository is a window this queue exists to have opinions about.
///
/// **No `-f`, ever, and a tag that already exists is git's refusal to report rather than ours to
/// overrule.** Moving an existing tag is the tag-shaped force push: it invalidates what everybody who
/// already fetched believes, and unlike a branch there is no expectation that it ever moves. So the
/// row comes back `Failed` carrying git's own `tag 'v1' already exists`.
///
/// `Failed` rather than `Blocked` for every refusal, on `publish_by_fast_forward`'s reasoning:
/// `Blocked` is terminal and means a human must clear something out of the way here and now, which
/// describes none of the ways this can fail.
///
/// **`--end-of-options` earns its place HERE, unlike in `push` where it is labelled unobservable —
/// measured, both ways.** Everything after it is a positional, so a flag that reached this argv
/// would be read as a tag name rather than as a flag: moving `-f` from before it to after it turns a
/// silent `Updated tag 'v1.0' (was 129eccf)` — somebody's release tag relocated — into
/// `fatal: too many arguments`. The difference from `push` is the argv shape rather than the care
/// taken: `git push <remote> <refspec>` has no option that could masquerade as either, and
/// `git tag -f <name>` is one keystroke from the operation this performs.
async fn tag(project_root: &Path, name: &str, at: &str, deadline: std::time::Instant) -> Outcome {
    // `refs/heads/<at>` rather than `<at>` bare, which is what makes `at` a BRANCH rather than a
    // commit-ish the type merely calls one. A sha, a tag or `HEAD~1` fails here, naming the ref it
    // could not resolve — a refusal the caller can read, rather than a tag quietly written somewhere
    // the row's own type says it could not have been.
    let sha = match revision(project_root, &format!("refs/heads/{at}"), deadline).await {
        Ok(sha) => sha,
        Err(outcome) => return outcome,
    };

    let tagged = match git(
        project_root,
        &["tag", "--end-of-options", name, &sha],
        deadline,
    )
    .await
    {
        Ok(tagged) => tagged,
        Err(outcome) => return outcome,
    };
    if !tagged.succeeded() {
        return failed(
            format!("tagging {at} as {name} failed; git's output says why"),
            &tagged,
        );
    }

    Outcome::Succeeded {
        sha: Some(sha),
        output_tail: tagged.output_tail,
    }
}

/// Publishes `branch` to `remote`, by object id rather than by name.
///
/// **The sha is resolved first and then pushed as `<sha>:refs/heads/<branch>`, which is the whole
/// design of this function rather than a flourish.** Pushing the NAME would publish whatever the
/// branch points at in the instant git gets round to reading it, which is not necessarily what it
/// pointed at when the operation was admitted — the queue exists precisely because other things are
/// moving refs. Pushing the id makes the row's `result_sha` a statement about what is on the remote
/// instead of a guess, and it is what lets a human read the queue backwards afterwards.
///
/// A branch that does not exist fails at the `rev-parse`, before anything reaches the network, with a
/// message naming the ref rather than git's `src refspec … does not match any`.
///
/// **`--end-of-options` is here rather than argued away, and NO TEST CAN SEE IT — measured, not
/// assumed.** `Merge`'s arguments survive without one by a chain of accidents `Op` documents; none of
/// that chain is about `push`, whose argv is a different shape, so the flag was written rather than
/// reasoned around. Deleting it leaves every push test green, and that is not a coverage gap to be
/// filled: `Remote` and `Branch` both refuse a leading dash and the third argument is hexadecimal, so
/// there is no input that reaches here and needs it. It is the second lock on a door whose first lock
/// cannot be picked — worth having for the day a third argv shape arrives with a looser type, and
/// worth labelling as unobservable so nobody later mistakes its survival for dead weight.
///
/// **A rejected push is `Failed`, never `Blocked`.** `Blocked` is terminal and means the user's own
/// work is in the way here and now; a non-fast-forward means the remote moved, which is fixed by
/// bringing it in and resubmitting — and which the next queued operation may already be about to do.
/// `publish_by_fast_forward` makes the same call for the same reason: `Blocked` is the wrong way to
/// be wrong, because it needs a human to notice before anything can proceed.
async fn push(
    project_root: &Path,
    remote: &str,
    branch: &str,
    deadline: std::time::Instant,
) -> Outcome {
    let reference = format!("refs/heads/{branch}");
    let sha = match revision(project_root, &reference, deadline).await {
        Ok(sha) => sha,
        Err(outcome) => return outcome,
    };

    let refspec = format!("{sha}:{reference}");
    let pushed = match git(
        project_root,
        &["push", "--end-of-options", remote, &refspec],
        deadline,
    )
    .await
    {
        Ok(pushed) => pushed,
        Err(outcome) => return outcome,
    };
    if !pushed.succeeded() {
        return failed(
            format!("pushing {branch} to {remote} failed; git's output says why"),
            &pushed,
        );
    }

    Outcome::Succeeded {
        sha: Some(sha),
        output_tail: pushed.output_tail,
    }
}

// `pub(crate)` on the TEST module only, so nothing about this module's production surface widens.
// `vcs.rs`'s end-to-end test needs a real repository with a branch to merge, and the alternative is a
// fifth copy of `init_contained_repo` — four already exist in this crate. Sharing the four helpers
// `vcs.rs` reaches for (`repo_with_a_branch_to_merge`, `space_free_tempdir`, `WorktreeRootEnv`,
// `sha_of`) is the smaller cost, and it keeps one definition of what a test repository looks like.
#[cfg(test)]
pub(crate) mod tests {
    // A process-wide guard held across awaits on purpose: it serialises mutation of the shared
    // NUCLEOS_WORKTREE_ROOT override, and there is no multi-thread runtime here to starve.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::ffi::{OsStr, OsString};
    // `Path` explicitly, not via `use super::*`: at Step 1 this file contains nothing BUT this
    // module, so the glob imports nothing and the helpers below would not resolve it.
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(crate) struct WorktreeRootEnv {
        previous: Option<OsString>,
    }

    impl WorktreeRootEnv {
        // `pub(crate)` on the associated function as well as the struct: the struct alone gives
        // `error[E0624]: associated function 'set' is private` at `vcs.rs`'s call site.
        pub(crate) fn set(path: &Path) -> Self {
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
    pub(crate) fn space_free_tempdir(prefix: &str) -> tempfile::TempDir {
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

    /// `pub(crate)` for `runs.rs`, which needs a real repository to test the approval path against —
    /// the same reason `space_free_tempdir` above is.
    pub(crate) fn initialize_repo(repo: &Path) {
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
    pub(crate) fn repo_with_a_branch_to_merge(prefix: &str) -> (tempfile::TempDir, PathBuf) {
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

    pub(crate) fn sha_of(repo: &Path, revision: &str) -> String {
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
        let _lock = crate::worktree::test_env_lock();
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
        use crate::vcs::VcsExecutor;
        GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: true,
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
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_conflict("nucleos-gitexec-ordinary-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());

        // `feat/x` as any session leaves it: one parent, no merge in it. Its content conflicts with
        // master, so the expected answer is the ordinary escalation for a conflict — reached by
        // computing the merge, NOT by the shape check refusing it first.
        use crate::vcs::VcsExecutor;
        let outcome = GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-red-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(
            &repo,
            "gate_before_publish: true\ngate_command: git rev-parse --verify nao-existe\n",
        );
        let before = sha_of(&repo, "master");

        match land_ordinarily(&repo).await {
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
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-green-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        // `git --version` is the gate that always agrees, and it needs no shell to run.
        write_autopilot_rules(
            &repo,
            "gate_before_publish: true\ngate_command: git --version\n",
        );
        let before = sha_of(&repo, "master");

        let published = match land_ordinarily(&repo).await {
            Outcome::Succeeded { sha, .. } => sha.expect("a merge names the commit it published"),
            other => panic!("a green gate must let the merge through, got {other:?}"),
        };
        assert_ne!(sha_of(&repo, "master"), before, "nothing was published");
        assert_eq!(sha_of(&repo, "master"), published);
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
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-off-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(&repo, "gate_command: git rev-parse --verify nao-existe\n");
        let before = sha_of(&repo, "master");

        match land_ordinarily(&repo).await {
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
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-gate-nocmd-");
        let roots = space_free_tempdir("nucleos-gitexec-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        write_autopilot_rules(&repo, "gate_before_publish: true\n");
        let before = sha_of(&repo, "master");

        match land_ordinarily(&repo).await {
            Outcome::Failed { reason, .. } => assert!(
                reason.contains("gate_command"),
                "the refusal must name the key that is missing: {reason}"
            ),
            other => panic!("a gate asked for and not named must refuse, got {other:?}"),
        }
        assert_eq!(sha_of(&repo, "master"), before, "master moved");
    }

    /// An ordinary landing of `feat/x` into `master`, through the real executor.
    fn write_autopilot_rules(repo: &Path, contents: &str) {
        let path = repo.join(crate::config::AUTOPILOT_RULES_PATH);
        std::fs::create_dir_all(path.parent().expect("the rules file sits in a folder"))
            .expect("create the rules folder");
        std::fs::write(path, contents).expect("write the rules");
    }

    async fn land_ordinarily(repo: &Path) -> Outcome {
        use crate::vcs::VcsExecutor;
        GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
            })
            .await
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
                    reason.contains("in your own worktree"),
                    "a refusal that names no alternative sends the asker looking: {reason}"
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
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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

    /// The ordinary case, and the one §6.1 is about: HEAD, the index and the files move together, so
    /// `git status` tells the truth immediately afterwards.
    #[tokio::test]
    async fn a_branch_somebody_has_open_is_fast_forwarded_in_place() {
        let _lock = crate::worktree::test_env_lock();
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
        use crate::vcs::VcsExecutor;

        let _lock = crate::worktree::test_env_lock();
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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "release".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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
        let _lock = crate::worktree::test_env_lock();
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
        use crate::vcs::VcsExecutor;

        let _lock = crate::worktree::test_env_lock();
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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: project_root.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-push-");
        let remote = remote_beside(container.path(), &repo);
        let local = sha_of(&repo, "refs/heads/master");

        let outcome = GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Push {
                    remote: "origin".into(),
                    branch: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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

    /// A rebase nobody has open is computed in isolation and published by compare-and-swap.
    ///
    /// The assertion is on the SHAPE of the result, not just that the ref moved: the rebased tip's
    /// first parent must be `master`, which is what says the commits were replayed rather than
    /// merged. A `compute_merge` accidentally wired here would move the ref too, and leave a tip with
    /// two parents.
    #[tokio::test]
    async fn a_rebase_of_a_branch_nobody_holds_is_published_by_compare_and_swap() {
        use crate::vcs::VcsExecutor;

        let _lock = crate::worktree::test_env_lock();
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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Rebase {
                    branch: "feat/x".into(),
                    onto: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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

    /// **A branch somebody has open is refused before anything is computed.**
    ///
    /// Both halves matter. `Blocked` rather than `Failed` says nothing will change on its own to
    /// make this publishable. And the branch not having moved is what says the refusal came BEFORE
    /// the work — an executor that computed first and discovered the holder afterwards would leave
    /// the same status behind, having spent the operation's budget and left unreferenced objects.
    #[tokio::test]
    async fn a_rebase_of_a_branch_somebody_holds_is_blocked_before_anything_is_computed() {
        use crate::vcs::VcsExecutor;

        let _lock = crate::worktree::test_env_lock();
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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Rebase {
                    branch: "feat/x".into(),
                    onto: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Fetch {
                    remote: "origin".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::BranchDelete {
                    branch: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-branchdel-unmerged-");
        let was = sha_of(&repo, "refs/heads/feat/x");

        let outcome = GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::BranchDelete {
                    branch: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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

    /// A branch that is not there fails before anything reaches the network, naming the ref.
    ///
    /// The message is the assertion: git's own answer to a push of a missing branch is `src refspec
    /// … does not match any`, which says nothing about which of the two halves of the refspec was
    /// wrong. Resolving the sha first turns that into a sentence naming the branch — and it is what
    /// keeps a typo from costing a network round trip while holding the repository's only slot.
    #[tokio::test]
    async fn pushing_a_branch_that_does_not_exist_fails_before_the_network() {
        use crate::vcs::VcsExecutor;

        let (container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-push-missing-");
        let _remote = remote_beside(container.path(), &repo);

        let outcome = GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Push {
                    remote: "origin".into(),
                    branch: "no-such-branch".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-tag-");
        let tip = sha_of(&repo, "refs/heads/feat/x");

        let outcome = GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Tag {
                    name: "v1.0".into(),
                    // Deliberately NOT the checked-out branch: a tag is written to the ref store and
                    // never touches a worktree, so nothing about this needs `master`.
                    at: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Tag {
                    name: "v1.0".into(),
                    at: "feat/x".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

        let (_container, repo) = repo_with_a_branch_to_merge("nucleos-gitexec-tag-notbranch-");
        let sha = sha_of(&repo, "refs/heads/master");

        let outcome = GitExecutor::default()
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Tag {
                    name: "v1.0".into(),
                    at: sha.as_str().into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        use crate::vcs::VcsExecutor;

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
            .execute(&crate::vcs::ClaimedRequest {
                id: 1,
                op: crate::vcs::Op::Push {
                    remote: "origin".into(),
                    branch: "master".into(),
                },
                project_id: "alpha".to_owned(),
                project_root: repo.to_string_lossy().into_owned(),
                from_resolution: false,
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
        let _lock = crate::worktree::test_env_lock();
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
