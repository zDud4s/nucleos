//! Spawning git under a deadline: the operation budget, the spawn and in-flight counters, stale `index.lock` removal, `run_git` and what a command printed.

use super::*;

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
pub(super) const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

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
#[cfg(any(test, feature = "testkit"))]
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

/// How many [`run_git`] processes are alive against each repository path, so a timeout can tell
/// whether the `index.lock` it finds was left by the process it just killed or is held by a sibling
/// that is still working.
pub(super) mod in_flight {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex};

    static LIVE: LazyLock<Mutex<HashMap<PathBuf, usize>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Counts one process against `repo` for as long as it is held.
    pub(in crate::execution::git_exec) struct Guard(PathBuf);

    impl Guard {
        pub(in crate::execution::git_exec) fn enter(repo: &Path) -> Self {
            *LIVE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(repo.to_path_buf())
                .or_default() += 1;
            Guard(repo.to_path_buf())
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let mut live = LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(count) = live.get_mut(&self.0) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    live.remove(&self.0);
                }
            }
        }
    }

    pub(in crate::execution::git_exec) fn any(repo: &Path) -> bool {
        LIVE.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(repo)
            .is_some_and(|count| *count > 0)
    }
}

/// Removes the `index.lock` a timed-out git was killed holding, when nothing of ours still runs there.
///
/// A git killed mid-write never gets to delete its lock, and every later index-touching command in
/// that checkout — the integration worktree's `reset --hard` first of all — then fails on "Unable to
/// create index.lock: File exists", which wedges the project's whole queue until somebody deletes the
/// file by hand. Removed only when no other [`run_git`] process is alive against the same path, since
/// a live sibling's lock is not stale. Best effort: any failure to resolve or remove leaves the file.
pub(super) async fn remove_stale_index_lock(repo: &Path) {
    if in_flight::any(repo) {
        return;
    }
    let mut command = crate::execution::worktree::git();
    command
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--git-path", "index.lock"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(10), command.output()).await
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if reported.is_empty() {
        return;
    }
    // `--git-path` answers relative to the directory git was pointed at unless it is absolute.
    let lock = Path::new(&reported);
    let lock = if lock.is_absolute() {
        lock.to_path_buf()
    } else {
        repo.join(lock)
    };
    if in_flight::any(repo) {
        return;
    }
    match tokio::fs::remove_file(&lock).await {
        Ok(()) => tracing::warn!(
            path = %lock.display(),
            "removed the index.lock a timed-out git left behind"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            path = %lock.display(),
            %error,
            "could not remove the index.lock a timed-out git left behind"
        ),
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
pub(super) const DRAIN_GRACE: Duration = Duration::from_secs(5);

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
/// The environment variable that says "the queue is doing this", read by `.githooks/`.
///
/// **A convention between two halves of this repository, and never a security boundary.** Anything
/// that can run git can also set an environment variable, so this stops nobody who means to get
/// past it. What it stops is the case that actually happened: a client performing a queue operation
/// because nothing told it not to. The gate that governs an agent is
/// `hooks::session_git_decision`; this is the second layer, and it exists because a client-side
/// hook is only ever as good as the harness that loads it, while a git hook runs whatever ran git.
pub const QUEUE_MARKER: &str = "NUCLEOS_QUEUE_EXEC";

pub async fn run_git(
    repo: &Path,
    args: &[&OsStr],
    timeout: Duration,
) -> Result<CommandResult, String> {
    let mut command = crate::execution::worktree::git();
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        // Marks every git process the queue itself runs, so `.githooks/` can tell the queue's own
        // merge from one made by hand. Set at the single place every git process in this crate goes
        // through, for the reason the spawn counter below is: a marker the caller sets beside its own
        // call is one a refactor moving that call leaves behind — and a queue operation the hook
        // cannot recognise is a queue that refuses itself.
        .env(QUEUE_MARKER, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::process_tree::spawn_in_own_group(&mut command);

    // Counted at the one place every git process in this crate goes through, rather than in the
    // module whose cost is being asserted — a counter the caller increments beside its own call is
    // one a refactor moving that call takes with it, which is exactly the refactor the count exists
    // to catch. Compiled out of the daemon entirely; see [`spawns`].
    #[cfg(any(test, feature = "testkit"))]
    spawns::record(repo);

    let live = in_flight::Guard::enter(repo);
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
            // The process is gone, so this one no longer counts — and a kill mid-write leaves its
            // `index.lock` behind, which would wedge every later command in this checkout.
            drop(live);
            remove_stale_index_lock(repo).await;
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
pub(super) async fn drain_pipe<R: tokio::io::AsyncRead + Unpin>(
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
pub(super) fn rendered(args: &[&OsStr]) -> String {
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
pub(super) fn tail(stdout: &[u8], stderr: &[u8]) -> String {
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
