//! GitHub CLI execution, outcomes, proposals and approvals.

use super::*;

/// How long one `gh` invocation may take before its whole process tree goes down.
///
/// Shorter than `git_exec::OPERATION_TIMEOUT`'s five minutes, and the difference is what the two
/// wait for: a git operation can be pushing objects over a slow link, and every operation here is
/// one HTTP request to GitHub with a CLI wrapped around it. Two minutes is generous for that and
/// short enough that a wedged call does not hold a caller for the length of a coffee break.
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);

/// The Credential Manager key the token lives under (`secrets.rs`).
///
/// **`gh auth login` is not an alternative and the two are not equivalent.** That login writes the
/// credential into a keyring belonging to the interactive session of whoever ran it, and this daemon
/// runs as a scheduled task. Worse, a green `gh auth status` would then read "healthy" while the
/// module could not act — so the health probe asks about the token the module will USE, never about
/// the CLI's own opinion of itself.
pub const TOKEN_KEY: &str = "github-token";

/// What comes back with the answer, before any of it reaches a caller's context.
///
/// A workflow log is unbounded and a run's context is not, so `stdout` is capped and says when the
/// cap bit. Truncation is at the START rather than the end: the tail of a failing log is where the
/// error is, and clipping that to keep the setup lines would keep the half nobody needs.
pub(super) const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// The result of one `gh` invocation.
///
/// There is no `github_requests` table for this to be written to, and that is deliberate: `vcs.rs`
/// has one because its queue is asynchronous and somebody has to be able to read the ticket later,
/// while here the answer is synchronous and the caller already holds it. A `github-action` proposal
/// records its own outcome on its own row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub kind: &'static str,
    pub exit_code: Option<i32>,
    /// Standard output, capped and already through `redact.rs`.
    pub stdout: String,
    /// stdout then stderr, for the caller that wants to see why a non-zero exit happened.
    pub output_tail: String,
}

/// Why an invocation did not happen, in the vocabulary `health.rs` already has.
///
/// The three that look alike are kept apart on purpose. `MissingCli` is a thing this computer cannot
/// do; `MissingToken` is a thing it could do if somebody pasted a credential; `NotConfigured` is a
/// pillar nobody asked for. Collapsing them costs an hour of looking in the wrong place, which is
/// exactly what `health.rs`'s own doc says about `NotRunning` versus `Missing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// `enabled: false`. Not a fault.
    NotConfigured,
    /// No `gh` on PATH.
    MissingCli,
    /// No `github-token` in the Credential Manager, or it could not be read.
    MissingToken,
    /// The command outlived `OPERATION_TIMEOUT` and its tree was killed.
    TimedOut,
    Unknown(String),
}

impl Failure {
    pub fn category(&self) -> crate::health::FailureCategory {
        match self {
            Failure::NotConfigured => crate::health::FailureCategory::NotConfigured,
            Failure::MissingCli => crate::health::FailureCategory::Missing,
            Failure::MissingToken => crate::health::FailureCategory::PermissionDenied,
            Failure::TimedOut => crate::health::FailureCategory::Timeout,
            Failure::Unknown(_) => crate::health::FailureCategory::Unknown,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::NotConfigured => write!(formatter, "the github pillar is switched off"),
            Failure::MissingCli => write!(formatter, "gh is not on this machine's PATH"),
            Failure::MissingToken => write!(
                formatter,
                "no github token is stored; run the daemon with --set-github-token"
            ),
            Failure::TimedOut => {
                write!(formatter, "gh did not finish within {OPERATION_TIMEOUT:?}")
            }
            Failure::Unknown(reason) => write!(formatter, "{reason}"),
        }
    }
}

/// Reads the token out of the Credential Manager.
///
/// `spawn_blocking` because `keyring` is a blocking OS call and this runs on the async runtime.
/// An error reading the store and an empty store are the same answer here — neither yields a token,
/// and the distinction would only ever be repeated back as a message.
pub async fn load_token() -> Option<String> {
    tokio::task::spawn_blocking(|| crate::secrets::load_secret(TOKEN_KEY))
        .await
        .ok()?
        .ok()?
        .filter(|token| !token.trim().is_empty())
}

/// Runs one operation and returns what `gh` said.
///
/// **It decides HOW, never WHETHER.** `Policy` answers whether it runs without asking and `auth.rs`
/// answers whether the actor could ask at all; nothing in here consults either. That separation is
/// `vcs.rs`'s between when an operation runs and whether the actor was allowed to request it, and
/// keeping it means a second caller cannot acquire permission by calling this instead.
///
/// Four things it does that are each somebody's past bug:
///
/// 1. **The argv is built and never interpreted.** `op.argv()` produces a list, `Command` takes a
///    list, and no shell is anywhere on the path — so a `;` inside a PR body is one more character
///    in an argument.
/// 2. **The token goes in by ENVIRONMENT and never as an argument**, because a process's argv is
///    readable by any process on the machine. `GITHUB_TOKEN` is removed rather than left alone: `gh`
///    prefers `GH_TOKEN` so it would not win, and an inherited variable that cannot win is one
///    somebody will later assume is being used.
/// 3. **The whole process tree goes down on a deadline.** It is the obligation `vcs.rs` records,
///    born of `run_git` saying "nothing here hands git a shell" and that ceasing to be true without
///    anybody editing the comment. `gh` spawns a credential helper and an editor given the chance,
///    so the direct child is not the whole of what was started.
/// 4. **A non-zero exit is returned and never retried.** Retrying a `PrComment` that failed halfway
///    posts it twice, and this module cannot tell halfway from not-at-all.
pub async fn execute(runtime: &GithubRuntime, op: &Op) -> Result<Outcome, Failure> {
    if !runtime.enabled {
        return Err(Failure::NotConfigured);
    }
    let token = load_token().await.ok_or(Failure::MissingToken)?;
    spawn_gh(runtime, op, &token, OPERATION_TIMEOUT).await
}

/// Everything `execute` does once it holds a credential and a deadline.
///
/// Split out for one reason, and it is worth stating rather than leaving as a shape: **the keyring
/// and the production deadline are the two things a test cannot have.** The Credential Manager is a
/// real OS store with no fake behind it, and 120 seconds is not a wait a suite can take. With both
/// passed in, everything below — the argv built and never interpreted, the token by environment
/// rather than by argument, the terminator, the tree going down on the deadline, the output capped
/// and redacted, a non-zero exit returned and never retried — is exercised against a stub program.
///
/// The four obligations named in `execute`'s own doc live down here, so that is where they are read.
pub(super) async fn spawn_gh(
    runtime: &GithubRuntime,
    op: &Op,
    token: &str,
    deadline: Duration,
) -> Result<Outcome, Failure> {
    let argv = op.argv();

    let mut command = tokio::process::Command::new(&runtime.binary);
    command
        .args(&argv)
        .env("GH_TOKEN", token)
        .env_remove("GITHUB_TOKEN")
        // `gh` opens a prompt or a pager given half a chance, and there is nobody here to answer one.
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        // Otherwise every answer arrives wrapped in escape sequences, and `Body::new` would refuse
        // its own module's output if it ever came back round.
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::process_tree::spawn_in_own_group(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        // The one spawn error worth its own variant. Everything else is a real failure to start a
        // program that exists; this is a machine that does not have `gh` on it, which is not a fault.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Failure::MissingCli);
        }
        Err(error) => return Err(Failure::Unknown(format!("could not run gh: {error}"))),
    };

    // Declared AFTER the child so it drops FIRST, while tokio's handle still pins the pid — the
    // invariant `TreeKiller` documents, and the only thing keeping the pid it names ours.
    let mut killer = child.id().map(crate::process_tree::TreeKiller::new);

    let stdout_bytes = Arc::new(Mutex::new(Vec::new()));
    let stderr_bytes = Arc::new(Mutex::new(Vec::new()));
    let stdout_task = tokio::spawn(drain(
        child.stdout.take().expect("stdout was piped"),
        Arc::clone(&stdout_bytes),
    ));
    let stderr_task = tokio::spawn(drain(
        child.stderr.take().expect("stderr was piped"),
        Arc::clone(&stderr_bytes),
    ));

    let status = match tokio::time::timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            if let Some(killer) = killer.as_mut() {
                killer.kill_now();
            }
            return Err(Failure::Unknown(format!("could not wait for gh: {error}")));
        }
        Err(_) => {
            // The tree goes down HERE and not at drop, because `child` is still alive at this point
            // and its handle is what stops the pid being reused under the killer.
            if let Some(killer) = killer.as_mut() {
                killer.kill_now();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            tracing::warn!(
                kind = op.kind(),
                "gh timed out; its process tree was killed"
            );
            return Err(Failure::TimedOut);
        }
    };

    let drained = async {
        for task in [stdout_task, stderr_task] {
            let _ = task.await;
        }
    };
    if tokio::time::timeout(DRAIN_GRACE, drained).await.is_ok() {
        if let Some(killer) = killer.as_mut() {
            killer.disarm();
        }
    } else {
        // The exit code survives a stuck drain: `gh` ran and said how it ended, and only the tail is
        // short. The killer stays ARMED, so dropping it takes whatever still holds the pipe down.
        tracing::warn!(
            kind = op.kind(),
            "gh's output did not finish draining; reporting the result with a truncated tail"
        );
    }

    let stdout = std::mem::take(&mut *stdout_bytes.lock().expect("the buffer is not poisoned"));
    let stderr = std::mem::take(&mut *stderr_bytes.lock().expect("the buffer is not poisoned"));
    let stdout = clip(&String::from_utf8_lossy(&stdout));
    let stderr = clip(&String::from_utf8_lossy(&stderr));

    let exit_code = status.code();
    if exit_code != Some(0) {
        // A line, and never a row: see `Outcome`. The kind and the code, and deliberately not the
        // output — the caller has that, and a log file is a worse place for a stranger's prose than
        // a turn's context is.
        tracing::warn!(kind = op.kind(), ?exit_code, "a gh operation failed");
    }

    Ok(Outcome {
        kind: op.kind(),
        exit_code,
        // Through `redact.rs`, which already knows the five `gh*_` prefixes and `github_pat_`. A
        // token can come back out of `gh`'s own diagnostics, and the shortest path from there to a
        // third party is a caller pasting the answer somewhere.
        stdout: crate::governance::redact::redact_secrets(&stdout),
        output_tail: crate::governance::redact::redact_secrets(&format!("{stdout}{stderr}")),
    })
}

/// How long the pipes get to finish after the process has exited.
pub(super) const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Reads one pipe to EOF into a buffer the caller keeps, so a drain that has to be abandoned still
/// leaves behind what it managed to read.
///
/// A `std::sync::Mutex` and never held across an `await`: the lock is taken per chunk and released
/// before the next read, which is what keeps a blocking lock correct inside an async task.
pub(super) async fn drain<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    into: Arc<Mutex<Vec<u8>>>,
) {
    use tokio::io::AsyncReadExt;
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let mut buffer = into.lock().expect("the buffer is not poisoned");
                buffer.extend_from_slice(&chunk[..read]);
                // A sliding TAIL, because `clip` promises one and this is the half that has to
                // deliver it.
                //
                // This used to STOP appending once the buffer reached the cap, which keeps the
                // head. `clip` then kept the last `MAX_OUTPUT_BYTES` of the first
                // `MAX_OUTPUT_BYTES` and stamped it with a marker announcing the tail — so the one
                // case the cap exists for, a workflow log that failed, came back holding the setup
                // lines with the errors thrown away, while saying the opposite in writing.
                //
                // Measured on a real red run before it was fixed: the `failures:` block cargo
                // prints after every test is exactly what went missing, which is the only part
                // anybody reads a failing log for.
                //
                // Trimmed in one step at twice the cap rather than on every chunk, because trimming
                // per read would memmove a quarter of a megabyte for every 8 KiB that arrives. What
                // is left is never smaller than `MAX_OUTPUT_BYTES + 1`, so `clip` still sees that
                // something was cut and still says so.
                if buffer.len() > MAX_OUTPUT_BYTES * 2 {
                    let excess = buffer.len() - (MAX_OUTPUT_BYTES + 1);
                    buffer.drain(..excess);
                }
            }
        }
    }
}

/// PURE: keeps the LAST `MAX_OUTPUT_BYTES` and says so when that clipped anything.
///
/// The tail rather than the head, because the tail of a failing workflow log is where the error is.
/// Reading is done on characters and not bytes so the cut never lands mid-codepoint.
pub(super) fn clip(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text.to_owned();
    }
    let kept: String = text
        .chars()
        .rev()
        .take(MAX_OUTPUT_BYTES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("[... clipped to the last {MAX_OUTPUT_BYTES} characters ...]\n{kept}")
}

/// What a door gets back: the operation ran, or it is waiting for a person.
///
/// **Neither outcome blocks the caller**, and that is the decision rather than a convenience. An
/// item waiting on a human would be a run sitting in `working` for days, holding a concurrency slot
/// and counting against the ceiling — so filing answers immediately and the turn carries on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    Ran(Outcome),
    Filed {
        proposal_id: i64,
        kind: &'static str,
    },
}

/// Something went wrong deciding what to do, as opposed to doing it.
#[derive(Debug)]
pub enum DecisionError {
    NotFound,
    NotPending,
    /// The proposal carries nothing this module can read as an operation.
    Malformed,
    /// It was claimed and then `gh` would not run it.
    Failed(Failure),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for DecisionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// The single place that answers "does this run now, or does it wait for a person".
///
/// Shared by the HTTP route and by whatever internal trigger comes later, because two copies of this
/// decision is how the tool and the trigger would come to disagree about the same `~/.nucleos/github.yaml`.
///
/// **A READ never files a proposal, and that is not an omission.** What limits reads is the EFFECT
/// (`ReadOp::effect`) and not the autonomy list: a read that returns a stranger's prose marks the
/// turn and burns its right to act, which is a stronger and more precise brake than an approval
/// prompt would be. `autonomous_actions` governs the acting half alone, and the acting half is the
/// only half whose refusal has anywhere to go.
pub async fn submit(
    pool: &sqlx::SqlitePool,
    runtime: &GithubRuntime,
    op: Op,
) -> Result<Submitted, Failure> {
    if !runtime.enabled {
        return Err(Failure::NotConfigured);
    }
    match &op {
        Op::Read(_) => execute(runtime, &op).await.map(Submitted::Ran),
        Op::Act(act) if runtime.policy.action_is_autonomous(act.kind()) => {
            execute(runtime, &op).await.map(Submitted::Ran)
        }
        Op::Act(act) => {
            let kind = act.kind();
            let why = act.describe();
            let payload = serde_json::to_string(&op).map_err(|error| {
                Failure::Unknown(format!("the operation could not be recorded: {error}"))
            })?;
            let proposal_id = crate::proposals::create_github_action(pool, kind, &why, &payload)
                .await
                .map_err(|error| {
                    tracing::warn!(kind, %error, "filing a github action for approval failed");
                    Failure::Unknown("the operation could not be filed for approval".to_owned())
                })?;
            Ok(Submitted::Filed { proposal_id, kind })
        }
    }
}

/// Runs a `github-action` a person has just approved.
///
/// **The claim comes BEFORE the run, and the order is the safety argument.** Marking first means a
/// second approval racing this one loses at the compare-and-set and cannot post the same comment
/// twice; the cost is that a `gh` failure leaves a row reading `approved` with nothing published,
/// which the note beneath it says in words. The other order trades a readable row for a double post,
/// and `execute` deliberately never retries for the same reason.
///
/// The outcome goes on the proposal's own row rather than into a table of its own — see `Outcome`.
pub async fn approve_proposed_operation(
    pool: &sqlx::SqlitePool,
    runtime: &GithubRuntime,
    proposal_id: i64,
) -> Result<Outcome, DecisionError> {
    let proposal = crate::proposals::get(pool, proposal_id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if proposal.kind != "github-action" || proposal.status != "pending" {
        return Err(DecisionError::NotPending);
    }
    let op: Op = proposal
        .tool_input
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .ok_or(DecisionError::Malformed)?;
    // Read back through the same validating `Deserialize` every other route in, so a row edited by
    // hand in the database cannot put a dashed string on a command line months later.

    if !crate::proposals::transition(pool, proposal_id, "approved", "approved by user").await? {
        return Err(DecisionError::NotPending);
    }

    match execute(runtime, &op).await {
        Ok(outcome) => {
            let note = match outcome.exit_code {
                Some(0) => format!("gh {} ran and succeeded", outcome.kind),
                Some(code) => format!("gh {} ran and exited {code}", outcome.kind),
                None => format!("gh {} was terminated by a signal", outcome.kind),
            };
            crate::proposals::note(pool, proposal_id, &note).await?;
            Ok(outcome)
        }
        Err(failure) => {
            crate::proposals::note(
                pool,
                proposal_id,
                &format!("gh {} did not run: {failure}", op.kind()),
            )
            .await?;
            Err(DecisionError::Failed(failure))
        }
    }
}
