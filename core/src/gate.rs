// This phase introduces the gate module before a later phase wires it into the daemon.
#![allow(dead_code)]

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

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            if let Some(killer) = tree_killer.as_mut() {
                killer.disarm();
            }
            status
        }
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

    for task in [stdout_task, stderr_task] {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return GateOutcome::Errored {
                    reason: format!("failed to capture gate output: {error}"),
                };
            }
            Err(error) => {
                return GateOutcome::Errored {
                    reason: format!("gate output task failed: {error}"),
                };
            }
        }
    }

    if status.success() {
        GateOutcome::Passed
    } else {
        GateOutcome::Failed {
            exit_code: status.code().unwrap_or(-1),
            output: output.lock().await.render(),
        }
    }
}

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
    use super::{GateOutcome, run_gate};
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
}
