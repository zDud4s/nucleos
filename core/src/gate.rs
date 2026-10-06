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

/// The command's arguments that name files inside the worktree. Repo-relative only: an absolute
/// path is the interpreter, which lives outside the worktree and the run cannot have rewritten it.
fn worktree_scripts(worktree: &Path, words: &[String]) -> Vec<String> {
    words
        .iter()
        .filter(|word| !Path::new(word.as_str()).is_absolute())
        .filter(|word| worktree.join(word.as_str()).is_file())
        .cloned()
        .collect()
}

/// Whether the gate is about to run a script the run it measures could have rewritten.
///
/// The configuration was already safe: `runs.rs` reads `gate_command` from the project root before
/// the worktree exists, so an agent cannot repoint the gate. The script that configuration NAMES
/// was not. `scripts/gates.sh core` resolves inside the worktree and is an ordinary tracked file, so
/// an agent that could not make the suite green could make the gate green instead — and the verdict
/// stopped being independent of the work it judges.
///
/// Compared against the project root rather than against a base commit: the question is whether this
/// is the gate the operator configured, and the project root is where they configured it. It also
/// catches an uncommitted edit, which a git comparison would miss.
fn tampered_gate_script(
    project_root: &Path,
    worktree: &Path,
    scripts: &[String],
) -> Option<String> {
    for relative in scripts {
        match (
            std::fs::read(project_root.join(relative)),
            std::fs::read(worktree.join(relative)),
        ) {
            (Ok(configured), Ok(used)) if configured == used => {}
            (Ok(_), Ok(_)) => {
                return Some(format!(
                    "gate script `{relative}` differs from the project root's copy; the run modified \
                     the command that measures it"
                ));
            }
            (Err(error), _) => {
                return Some(format!(
                    "gate script `{relative}` is unreadable in the project root: {error}"
                ));
            }
            (_, Err(error)) => {
                return Some(format!(
                    "gate script `{relative}` is unreadable in the worktree: {error}"
                ));
            }
        }
    }
    None
}

/// Leading `NAME=value` words, taken off the front of the command they precede.
///
/// A gate command is spawned directly and never through a shell, so a project that needs a variable
/// set for its own suite had no way to ask for one: written as a prefix it named a program that
/// does not exist, and there was nowhere else to put it. What a given project needs set, and why,
/// belongs to that project's `gate_command` rather than here.
///
/// **Read here rather than by wrapping the configured line in `bash -c`, because the wrapper costs
/// the tamper check.** [`worktree_scripts`] looks for the gate's script among the command's WORDS,
/// and `-c "…"` collapses the whole line into a single word that names no file: the check would
/// find nothing to compare and pass in silence, which is the one failure mode it exists to prevent.
/// A prefix keeps the script a word of its own.
///
/// Only a PREFIX is honoured. A `NAME=value` after the program is an ordinary argument, which is
/// what a shell does with it too, and a gate command that passes one to its script must keep being
/// able to. A word counts as an assignment only when the name before `=` is non-empty and spelled
/// the way an environment variable is — otherwise a relative path such as `a=b/script.sh` would be
/// eaten, and the script it names would stop being checked.
fn split_environment(mut words: Vec<String>) -> (Vec<(String, String)>, Vec<String>) {
    let is_assignment = |word: &String| match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        }
        None => false,
    };
    let at = words.iter().take_while(|word| is_assignment(word)).count();
    let rest = words.split_off(at);
    let environment = words
        .into_iter()
        .map(|word| {
            let (name, value) = word.split_once('=').expect("checked by is_assignment");
            (name.to_owned(), value.to_owned())
        })
        .collect();
    (environment, rest)
}

/// Runs a verification command in `worktree` without involving agent hooks or classification.
///
/// `command` is a program followed by arguments, not a shell line. Shell operators such as `&&`
/// are passed as ordinary arguments; callers that need them must name a shell explicitly, for
/// example `bash -c "cargo test && cargo clippy"`. Leading `NAME=value` words are the one shell
/// shape read here rather than passed on — see [`split_environment`] for why they cannot be
/// delegated to a wrapper.
///
/// `project_root` is the un-agented copy of the repository. It is not where the command runs — that
/// is always the worktree — but the reference the gate's own script is checked against first.
pub async fn run_gate(
    worktree: &Path,
    project_root: &Path,
    command: &str,
    timeout: Duration,
) -> GateOutcome {
    let words = match split_command(command) {
        Ok(words) => words,
        Err(reason) => return GateOutcome::Errored { reason },
    };
    // Before the emptiness check, so the program is the first word that is not an assignment. `words` from
    // here on is the command proper, which is also what the tamper check below must see.
    let (environment, words) = split_environment(words);
    if words.is_empty() {
        return GateOutcome::Errored {
            reason: "gate command is empty".to_owned(),
        };
    }

    // Before anything is spawned: a gate the measured run rewrote is not a measurement. `Errored`,
    // not `Failed` — the code may be perfectly fine; what broke is our ability to tell.
    let scripts = worktree_scripts(worktree, &words);
    if let Some(reason) = tampered_gate_script(project_root, worktree, &scripts) {
        return GateOutcome::Errored { reason };
    }

    let outcome = run_argv(&words, worktree, &environment, timeout).await;
    if outcome.timed_out {
        return GateOutcome::Errored {
            reason: format!("gate command timed out after {timeout:?}"),
        };
    }
    if let Some(reason) = outcome.error {
        return GateOutcome::Errored { reason };
    }

    match classify_exit(outcome.exit_code) {
        ExitVerdict::Passed => GateOutcome::Passed,
        ExitVerdict::Failed(exit_code) => GateOutcome::Failed {
            exit_code,
            output: outcome.tail,
        },
        ExitVerdict::Signalled => GateOutcome::Errored {
            reason: format!(
                "gate command was killed by a signal before it could report a result; captured output: {}",
                outcome.tail
            ),
        },
    }
}

/// What a child run produced, before anyone decides what it means.
#[derive(Debug)]
pub(crate) struct ArgvOutcome {
    /// `None` when the process was signalled, timed out, or never started.
    pub exit_code: Option<i32>,
    pub tail: String,
    pub duration: Duration,
    pub timed_out: bool,
    /// Spawn or wait failure, already phrased for a person.
    pub error: Option<String>,
}

/// Runs `argv` in `cwd` with `env` added to the inherited environment and returns what happened.
///
/// This is the spawn-and-drain half of [`run_gate`], with no tamper check and no verdict: callers
/// that run something other than the project's gate command (the verification executor) share the
/// same process-tree kill, output cap and drain deadline.
pub(crate) async fn run_argv(
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> ArgvOutcome {
    let started = std::time::Instant::now();
    let failed = |error: String| ArgvOutcome {
        exit_code: None,
        tail: String::new(),
        duration: started.elapsed(),
        timed_out: false,
        error: Some(error),
    };
    let Some((program, arguments)) = argv.split_first() else {
        return failed("empty command".to_owned());
    };

    let mut command = Command::new(program);
    command
        .args(arguments)
        .envs(
            env.iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        )
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::process_tree::spawn_in_own_group(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed(format!("failed to start gate command: {error}")),
    };

    // Declared after the child so it drops first, while the process handle still pins the pid — the
    // invariant `process_tree::TreeKiller` documents. The tree rather than the child because a gate
    // command is whatever the project put in it, and a shell may have spawned the actual work.
    let mut tree_killer = child.id().map(crate::process_tree::TreeKiller::new);

    let output = Arc::new(Mutex::new(TailBuffer::default()));
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout_task = tokio::spawn(drain_output(stdout, Arc::clone(&output)));
    let stderr_task = tokio::spawn(drain_output(stderr, Arc::clone(&output)));

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
            return failed(format!("failed while waiting for gate command: {error}"));
        }
        Err(_) => {
            if let Some(killer) = tree_killer.as_mut() {
                killer.kill_now();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return ArgvOutcome {
                exit_code: None,
                tail: output.lock().await.render(),
                duration: started.elapsed(),
                timed_out: true,
                error: None,
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
        Ok(Err(reason)) => return failed(reason),
        // The verdict survives a stuck drain. The command ran and its status is known; only the tail
        // is short. Returning an error here would throw away a real measurement because a leftover
        // process would not let go of a pipe. The killer stays armed, so dropping it takes the
        // subtree down on the way out.
        Err(_) => {
            tracing::warn!(
                ?drain_budget,
                "gate output did not finish draining; reporting the verdict with a truncated tail"
            );
        }
    }

    ArgvOutcome {
        exit_code: status.code(),
        tail: output.lock().await.render(),
        duration: started.elapsed(),
        timed_out: false,
        error: None,
    }
}

/// Splits a program-and-arguments command while preserving single- or double-quoted argument groups.
///
/// This is not a shell parser: operators such as `&&` have no special meaning. Use an explicit
/// shell command such as `bash -c "cargo test && cargo clippy"` when shell evaluation is required.
pub(crate) fn split_command(command: &str) -> Result<Vec<String>, String> {
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

#[rustfmt::skip]
#[cfg(test)]
mod tests {
    use super::{ExitVerdict, GateOutcome, classify_exit, run_argv, run_gate, split_environment};
    use std::time::Duration;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[tokio::test]
    async fn run_argv_reports_the_exit_code_and_the_tail() {
        let cwd = tempfile::tempdir().expect("create temporary directory");
        let outcome = run_argv(&argv(&["git", "--version"]), cwd.path(), &[], Duration::from_secs(30)).await;
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.tail.contains("git version"), "tail was: {}", outcome.tail);
        assert!(!outcome.timed_out);
        assert!(outcome.error.is_none());
    }

    #[tokio::test]
    async fn run_argv_reports_a_failing_exit_code() {
        let cwd = tempfile::tempdir().expect("create temporary directory");
        let outcome = run_argv(&argv(&["git", "definitely-not-a-subcommand"]), cwd.path(), &[], Duration::from_secs(30)).await;
        assert!(matches!(outcome.exit_code, Some(code) if code != 0), "got {:?}", outcome.exit_code);
        assert!(outcome.error.is_none());
    }

    #[tokio::test]
    async fn run_argv_passes_the_environment() {
        let cwd = tempfile::tempdir().expect("create temporary directory");
        let env = vec![
            ("GIT_CONFIG_COUNT".to_owned(), "1".to_owned()),
            ("GIT_CONFIG_KEY_0".to_owned(), "nucleos.probe".to_owned()),
            ("GIT_CONFIG_VALUE_0".to_owned(), "warm-ok".to_owned()),
        ];
        let outcome = run_argv(&argv(&["git", "config", "--get", "nucleos.probe"]), cwd.path(), &env, Duration::from_secs(30)).await;
        assert!(outcome.tail.contains("warm-ok"), "tail was: {}", outcome.tail);
    }

    #[tokio::test]
    async fn run_argv_refuses_an_empty_argv() {
        let cwd = tempfile::tempdir().expect("create temporary directory");
        let outcome = run_argv(&[], cwd.path(), &[], Duration::from_secs(5)).await;
        assert!(outcome.error.is_some());
        assert!(outcome.exit_code.is_none());
    }

    #[tokio::test]
    async fn run_argv_names_a_program_that_does_not_exist() {
        let cwd = tempfile::tempdir().expect("create temporary directory");
        let outcome = run_argv(&argv(&["nucleos-no-such-program-xyz"]), cwd.path(), &[], Duration::from_secs(5)).await;
        assert!(outcome.error.is_some());
        assert!(outcome.exit_code.is_none());
    }

    #[tokio::test]
    async fn a_missing_binary_is_not_a_failing_gate() {
        let worktree = tempfile::tempdir().expect("create temporary worktree");

        let outcome = run_gate(
            worktree.path(),
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
            worktree.path(),
            r#"sh -c "exit 3""#,
            // The command exits at once; the number only has to survive spawning `sh` on a
            // loaded Windows machine, where one second was not enough.
            Duration::from_secs(30),
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

    /// The gate runs a script from inside the worktree the agent just wrote to. The CONFIG was
    /// already safe — read from the project root before the worktree exists — but the script it
    /// names was not, so an agent that could not make the suite green could make the gate green.
    #[tokio::test]
    async fn a_gate_script_the_run_rewrote_is_not_a_measurement() {
        let project = tempfile::tempdir().expect("project root");
        let worktree = tempfile::tempdir().expect("worktree");
        std::fs::create_dir_all(project.path().join("scripts")).unwrap();
        std::fs::create_dir_all(worktree.path().join("scripts")).unwrap();
        std::fs::write(project.path().join("scripts/g.sh"), "exit 1\n").unwrap();
        // What the agent left behind: same path, now passing.
        std::fs::write(worktree.path().join("scripts/g.sh"), "exit 0\n").unwrap();

        let outcome = run_gate(
            worktree.path(),
            project.path(),
            "sh scripts/g.sh",
            Duration::from_secs(5),
        )
        .await;

        match outcome {
            GateOutcome::Errored { reason } => {
                assert!(reason.contains("scripts/g.sh"), "must name it: {reason}");
            }
            other => panic!("a rewritten gate script must not yield a verdict, got {other:?}"),
        }
    }

    /// The control. Without it the test above could pass because the gate errors on everything.
    #[tokio::test]
    async fn an_untouched_gate_script_still_measures() {
        let project = tempfile::tempdir().expect("project root");
        let worktree = tempfile::tempdir().expect("worktree");
        for root in [project.path(), worktree.path()] {
            std::fs::create_dir_all(root.join("scripts")).unwrap();
            std::fs::write(root.join("scripts/g.sh"), "exit 0\n").unwrap();
        }

        let outcome = run_gate(
            worktree.path(),
            project.path(),
            "sh scripts/g.sh",
            Duration::from_secs(5),
        )
        .await;

        assert!(
            matches!(outcome, GateOutcome::Passed),
            "an untouched script must be trusted, got {outcome:?}"
        );
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

    fn words(command: &str) -> Vec<String> {
        super::split_command(command).expect("splits")
    }

    #[test]
    fn a_leading_assignment_is_environment_and_not_the_program() {
        let (environment, rest) = split_environment(words("BUILD_DIR=elsewhere sh scripts/g.sh"));

        assert_eq!(environment, vec![("BUILD_DIR".to_owned(), "elsewhere".to_owned())]);
        assert_eq!(rest, vec!["sh".to_owned(), "scripts/g.sh".to_owned()]);
    }

    /// Only a prefix. A shell would pass this one through as an argument, and a gate script that
    /// takes `KEY=VALUE` arguments must keep receiving them.
    #[test]
    fn an_assignment_after_the_program_stays_an_argument() {
        let (environment, rest) = split_environment(words("sh scripts/g.sh MODE=fast"));

        assert!(environment.is_empty(), "not a prefix: {environment:?}");
        assert_eq!(rest, vec!["sh".to_owned(), "scripts/g.sh".to_owned(), "MODE=fast".to_owned()]);
    }

    /// A word with `=` in it is not an assignment unless the name is spelled like one. Without this
    /// the FIRST word of `a/g.sh=x sh` would be swallowed as environment, and a path that should
    /// have been compared against the project root would stop being compared at all.
    #[test]
    fn a_path_that_merely_contains_an_equals_sign_is_not_environment() {
        let (environment, rest) = split_environment(words("scripts/a=b.sh --now"));

        assert!(environment.is_empty(), "a path is not an assignment: {environment:?}");
        assert_eq!(rest, vec!["scripts/a=b.sh".to_owned(), "--now".to_owned()]);
    }

    /// The variable has to reach the child, not merely leave the parser.
    ///
    /// The child exits with the variable's own value, because that needs no nested quotes:
    /// `split_command` has no escape character, so a `"` inside a `"…"` word ends it early and the
    /// probe would be testing the splitter rather than the environment. An unset `GATE_PROBE` makes
    /// this `exit` with no argument, which is exit 0 — so `Failed { exit_code: 7 }` is reachable
    /// only if the value actually arrived.
    #[tokio::test]
    async fn the_named_variable_reaches_the_command() {
        let worktree = tempfile::tempdir().expect("create temporary worktree");

        let outcome = run_gate(
            worktree.path(),
            worktree.path(),
            r#"GATE_PROBE=7 sh -c "exit $GATE_PROBE""#,
            Duration::from_secs(5),
        )
        .await;

        match outcome {
            GateOutcome::Failed { exit_code: 7, .. } => {}
            other => panic!("the child must see GATE_PROBE=7, got {other:?}"),
        }
    }

    /// **The reason the prefix is parsed here instead of wrapping the whole line in `bash -c`.**
    /// A wrapper collapses the command into one word that names no file, so `worktree_scripts`
    /// finds nothing, `tampered_gate_script` compares nothing, and a run that rewrote its own gate
    /// is measured by the rewritten copy — silently, with a verdict that looks ordinary. This
    /// asserts the check still fires with an assignment in front of the script.
    #[tokio::test]
    async fn an_environment_prefix_does_not_blind_the_tamper_check() {
        let project = tempfile::tempdir().expect("project root");
        let worktree = tempfile::tempdir().expect("worktree");
        std::fs::create_dir_all(project.path().join("scripts")).unwrap();
        std::fs::create_dir_all(worktree.path().join("scripts")).unwrap();
        std::fs::write(project.path().join("scripts/g.sh"), "exit 1\n").unwrap();
        // What the agent left behind: same path, now passing.
        std::fs::write(worktree.path().join("scripts/g.sh"), "exit 0\n").unwrap();

        let outcome = run_gate(
            worktree.path(),
            project.path(),
            "BUILD_DIR=elsewhere sh scripts/g.sh",
            Duration::from_secs(5),
        )
        .await;

        match outcome {
            GateOutcome::Errored { reason } => {
                assert!(reason.contains("scripts/g.sh"), "must name it: {reason}");
            }
            other => panic!("an env prefix must not hide the rewritten script, got {other:?}"),
        }
    }
}
