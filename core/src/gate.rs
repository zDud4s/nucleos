use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;

/// Gate output comes from a repository-controlled subprocess, so a ceiling prevents an unusually
/// noisy suite from growing the daemon without bound. The retained bytes are the diagnostic tail.
const GATE_OUTPUT_CAP: usize = 1024 * 1024;

/// Floor for the drain deadline, so a command that exits just as its own timeout expires still gets
/// a moment to have its output collected rather than losing the tail to arithmetic.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// What a finished gate process's exit status means, as a pure function of that status.
///
/// Split out because the async body around it spawns a real process, so the classification could
/// only ever be tested by arranging a subprocess to die in the right way — which for the signal case
/// means depending on POSIX signals from a suite that also runs on Windows.
#[derive(Debug, PartialEq, Eq)]
enum ExitVerdict {
    Passed,
    Failed(i32),
    /// No exit code at all: a signal took the process. The OOM killer reaping a large suite is the
    /// common case. That is a measurement that never finished, not a suite that failed — reporting
    /// it as `Failed` is the exact conflation `GateOutcome`'s variants exist to prevent.
    Signalled,
}

fn classify_exit(code: Option<i32>) -> ExitVerdict {
    match code {
        Some(0) => ExitVerdict::Passed,
        Some(code) => ExitVerdict::Failed(code),
        None => ExitVerdict::Signalled,
    }
}

/// The result of measuring a repository gate.
///
/// `Failed` and `Errored` are deliberately distinct: a non-zero exit says the code is broken,
/// while a command that could not start says the measurement did not happen. Collapsing them would
/// make a missing binary look like failing tests and stop the wrong work.
#[derive(Debug)]
pub enum GateOutcome {
    Passed,
    Failed { exit_code: i32, output: String },
    Errored { reason: String },
}

/// Runs a verification command in `worktree` without involving agent hooks or classification.
///
/// `command` is a program followed by arguments, not a shell line. Shell operators such as `&&`
/// are passed as ordinary arguments; callers that need them must name a shell explicitly, for
/// example `bash -c "cargo test && cargo clippy"`.
pub async fn run_gate(worktree: &Path, command: &str, timeout: Duration) -> GateOutcome {
    let words = match split_command(command) {
        Ok(words) => words,
        Err(reason) => return GateOutcome::Errored { reason },
    };
    let Some((program, arguments)) = words.split_first() else {
        return GateOutcome::Errored {
            reason: "gate command is empty".to_owned(),
        };
    };

    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(worktree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return GateOutcome::Errored {
                reason: format!("failed to start gate command: {error}"),
            };
        }
    };

    // `runner::TreeKiller` is private to that module. Keep the same lifetime invariant here:
    // declare the guard after the child so it drops first, while the process handle still pins the
    // pid, and terminate the whole tree because a shell may have spawned the actual gate process.
    let mut tree_killer = child.id().map(TreeKiller::new);

    let output = Arc::new(Mutex::new(TailBuffer::default()));
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout_task = tokio::spawn(drain_output(stdout, Arc::clone(&output)));
    let stderr_task = tokio::spawn(drain_output(stderr, Arc::clone(&output)));

    let started = std::time::Instant::now();
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        // Deliberately NOT disarming the killer here. `child.wait()` returning says the direct child
        // exited, not that its pipes closed, and the drain below still needs a way to take down
        // whatever is holding them.
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            if let Some(killer) = tree_killer.as_mut() {
                killer.kill_now();
            }
            stdout_task.abort();
            stderr_task.abort();
            return GateOutcome::Errored {
                reason: format!("failed while waiting for gate command: {error}"),
            };
        }
        Err(_) => {
            if let Some(killer) = tree_killer.as_mut() {
                killer.kill_now();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return GateOutcome::Errored {
                reason: format!("gate command timed out after {timeout:?}"),
            };
        }
    };

    // Draining needs its own bound. `drain_output` reads until EOF, and EOF arrives only when EVERY
    // holder of the write end has closed it — a background process the gate script left behind
    // inherited that handle and keeps it open after the direct child is gone. Nothing above this
    // call has a deadline: `runs.rs` wraps the agent run in one but not the gate, so blocking here
    // would leave the run row `running` for ever and migration 0009 would then refuse every later
    // worktree run for that project, until a daemon restart reconciled it.
    let drain = async {
        for task in [stdout_task, stderr_task] {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(format!("failed to capture gate output: {error}")),
                Err(error) => return Err(format!("gate output task failed: {error}")),
            }
        }
        Ok(())
    };
    let drain_budget = timeout.saturating_sub(started.elapsed()).max(DRAIN_GRACE);
    match tokio::time::timeout(drain_budget, drain).await {
        Ok(Ok(())) => {
            if let Some(killer) = tree_killer.as_mut() {
                killer.disarm();
            }
        }
        Ok(Err(reason)) => return GateOutcome::Errored { reason },
        // The verdict survives a stuck drain. The command ran and its status is known; only the tail
        // is short. Returning `Errored` here would throw away a real measurement because a leftover
        // process would not let go of a pipe. The killer stays armed, so dropping it takes the
        // subtree down on the way out.
        Err(_) => {
            tracing::warn!(
                ?drain_budget,
                "gate output did not finish draining; reporting the verdict with a truncated tail"
            );
        }
    }

    match classify_exit(status.code()) {
        ExitVerdict::Passed => GateOutcome::Passed,
        ExitVerdict::Failed(exit_code) => GateOutcome::Failed {
            exit_code,
            output: output.lock().await.render(),
        },
        ExitVerdict::Signalled => GateOutcome::Errored {
            reason: format!(
                "gate command was killed by a signal before it could report a result; captured output: {}",
                output.lock().await.render()
            ),
        },
    }
}

/// Splits a program-and-arguments command while preserving single- or double-quoted argument groups.
///
/// This is not a shell parser: operators such as `&&` have no special meaning. Use an explicit
/// shell command such as `bash -c "cargo test && cargo clippy"` when shell evaluation is required.
fn split_command(command: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;

    for character in command.chars() {
        match (quote, character) {
            (Some(active), value) if value == active => quote = None,
            (None, '\'' | '"') => {
                quote = Some(character);
                started = true;
            }
            (None, value) if value.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(character);
                started = true;
            }
        }
    }

    if quote.is_some() {
        return Err("gate command contains an unterminated quote".to_owned());
    }
    if started {
        words.push(current);
    }
    Ok(words)
}

#[derive(Default)]
struct TailBuffer {
    bytes: VecDeque<u8>,
    truncated: bool,
}

impl TailBuffer {
    fn extend(&mut self, bytes: &[u8]) {
        self.bytes.extend(bytes);
        let excess = self.bytes.len().saturating_sub(GATE_OUTPUT_CAP);
        if excess > 0 {
            self.bytes.drain(..excess);
            self.truncated = true;
        }
    }

    fn render(&self) -> String {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        let tail = String::from_utf8_lossy(&bytes);
        if self.truncated {
            format!("…[output truncated; showing tail]\n{tail}")
        } else {
            tail.into_owned()
        }
    }
}

async fn drain_output<R>(mut reader: R, output: Arc<Mutex<TailBuffer>>) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = [0_u8; 8192];
    loop {
        let read = reader.read(&mut bytes).await?;
        if read == 0 {
            return Ok(());
        }
        output.lock().await.extend(&bytes[..read]);
    }
}

struct TreeKiller {
    pid: u32,
    armed: bool,
}

impl TreeKiller {
    fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn kill_now(&mut self) {
        if self.armed {
            terminate_process_tree(self.pid);
            self.armed = false;
        }
    }
}

impl Drop for TreeKiller {
    fn drop(&mut self) {
        if self.armed {
            terminate_process_tree(self.pid);
        }
    }
}

#[cfg(windows)]
fn terminate_process_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn terminate_process_tree(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[rustfmt::skip]
#[cfg(test)]
mod tests {
    use super::{ExitVerdict, GateOutcome, classify_exit, run_gate};
    use std::time::Duration;

    #[tokio::test]
    async fn a_missing_binary_is_not_a_failing_gate() {
        let worktree = tempfile::tempdir().expect("create temporary worktree");

        let outcome = run_gate(
            worktree.path(),
            "nucleos-no-such-program-xyz",
            Duration::from_secs(1),
        )
        .await;

        match outcome {
            GateOutcome::Errored { .. } => {}
            GateOutcome::Failed { .. } => {
                panic!("a missing binary is infrastructure error, not a failing gate")
            }
            GateOutcome::Passed => panic!("a missing binary cannot pass the gate"),
        }
    }

    #[tokio::test]
    async fn a_nonzero_command_is_a_failing_gate() {
        let worktree = tempfile::tempdir().expect("create temporary worktree");

        let outcome = run_gate(
            worktree.path(),
            r#"sh -c "exit 3""#,
            Duration::from_secs(1),
        )
        .await;

        match outcome {
            GateOutcome::Failed { exit_code: 3, .. } => {}
            GateOutcome::Failed { exit_code, .. } => {
                panic!("expected exit code 3, got {exit_code}")
            }
            GateOutcome::Errored { reason } => {
                panic!("a command that ran and exited non-zero must fail, not error: {reason}")
            }
            GateOutcome::Passed => panic!("a command that exits 3 cannot pass the gate"),
        }
    }

    #[tokio::test]
    async fn a_gate_that_outruns_its_deadline_is_errored() {
        let worktree = tempfile::tempdir().expect("create temporary worktree");

        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            run_gate(
                worktree.path(),
                r#"sh -c "sleep 5""#,
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("run_gate must not hang after its deadline");

        match outcome {
            GateOutcome::Errored { .. } => {}
            GateOutcome::Failed { .. } => {
                panic!("a timed-out gate is infrastructure error, not a failing gate")
            }
            GateOutcome::Passed => panic!("a gate that timed out cannot pass"),
        }
    }

    /// A signal death carries no exit code, and `unwrap_or(-1)` used to turn that into
    /// `Failed { exit_code: -1 }` — a suite the OOM killer reaped, reported as a suite that failed.
    /// Pure, because arranging the real thing means sending POSIX signals from a suite that also
    /// runs on Windows.
    #[test]
    fn a_signal_death_is_a_measurement_that_did_not_happen() {
        assert_eq!(classify_exit(None), ExitVerdict::Signalled);
    }

    #[test]
    fn an_exit_code_is_taken_at_face_value() {
        assert_eq!(classify_exit(Some(0)), ExitVerdict::Passed);
        assert_eq!(classify_exit(Some(3)), ExitVerdict::Failed(3));
        // -1 is a real exit code a program may choose, and must not be confused with "no code".
        assert_eq!(classify_exit(Some(-1)), ExitVerdict::Failed(-1));
    }
}
