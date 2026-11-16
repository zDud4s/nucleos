//! The resolver's one retry of a gate that failed on a finished resolution.
//!
//! A conflict resolution whose run `completed` with a committed merge but a `failed` gate used to be
//! refused for good (`land_finished`). Two such refusals on 2026-11-14 were not about the conflict at
//! all (a flaky test, a badly formatted file elsewhere) and still needed a person. So the gate is run
//! ONCE more on the resolution's worktree at the head the first one measured; only a second red is a
//! refusal.
//!
//! **The gate is slow, the tick is not.** The re-run is spawned and the tick moves on (`Retry::Pending`);
//! the next tick reads what it recorded. **The guard is in memory**, keyed like the resolver's refusal
//! set by (request, head): a head is retried at most once per daemon process, a new head (the agent
//! resumed and committed again) gets its own. A restart forgets it and costs at most one more re-run
//! of a gate; the announcement itself stays deduplicated durably (`announce_once`), so nobody is told
//! twice. Making it durable would need a column on `vcs_requests` for a cost this small.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::gate::GateOutcome;

/// What the retry needs to measure the resolution again.
pub(super) struct GateRequest {
    pub pool: sqlx::SqlitePool,
    pub project_id: String,
    pub run_id: i64,
    pub worktree: std::path::PathBuf,
}

/// Runs the project's gate for a `GateRequest`. A seam so tests do not run a real gate.
pub(super) type GateRunner =
    Arc<dyn Fn(GateRequest) -> Pin<Box<dyn Future<Output = GateOutcome> + Send>> + Send + Sync>;

/// The project's configured gate, run in full on the resolution's worktree and recorded as a
/// `verify_runs` row of the run, like the gate the run itself ended with.
pub(super) fn project_gate() -> GateRunner {
    Arc::new(|request| Box::pin(run_project_gate(request)))
}

async fn run_project_gate(request: GateRequest) -> GateOutcome {
    let rules = match crate::project::rules::autopilot(&request.project_id) {
        Ok(rules) => rules,
        Err(error) => {
            return GateOutcome::Errored {
                reason: format!("gate configuration is unreadable: {error}"),
            };
        }
    };
    let Some(command) = rules.gate_command else {
        return GateOutcome::Errored {
            reason: "the project has no gate command configured".to_owned(),
        };
    };
    let repo = match crate::execution::vcs::resolve_repo(&request.pool, &request.project_id).await {
        Ok(repo) => repo,
        Err(error) => {
            return GateOutcome::Errored {
                reason: format!("could not resolve the repository: {error:?}"),
            };
        }
    };
    let project_root = std::path::PathBuf::from(repo.root());
    crate::verify::runs::timed_gate(
        Some(&request.pool),
        crate::verify::runs::GateContext {
            project_id: Some(&request.project_id),
            origin: crate::verify::runs::ORIGIN_RUN,
            origin_id: Some(request.run_id),
            ordinal: None,
        },
        &request.worktree,
        &project_root,
        &command,
        crate::state::DEFAULT_GATE_TIMEOUT,
    )
    .await
}

/// Where the retry of one (request, head) stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Retry {
    /// Still running: look again next tick.
    Pending,
    Passed,
    /// Red again; carries what failed.
    Failed(String),
    /// The re-run could not measure anything; carries why.
    Unrun(String),
}

enum Slot {
    Running(tokio::task::JoinHandle<GateOutcome>),
    Done(Retry),
}

/// What the resolver remembers between ticks: the (request, head) pairs it refused, and the retry
/// of each failed gate. Empty after a restart.
#[derive(Default)]
pub(super) struct Refusals {
    refused: HashSet<(i64, String)>,
    retries: HashMap<(i64, String), Slot>,
}

impl Refusals {
    /// `HashSet::insert`: whether the pair was new.
    pub(super) fn insert(&mut self, key: (i64, String)) -> bool {
        self.refused.insert(key)
    }

    pub(super) fn contains(&self, key: &(i64, String)) -> bool {
        self.refused.contains(key)
    }

    /// Starts the one re-run of the gate for `key` the first time it is asked, and reports on it
    /// afterwards. Never starts a second one for the same key, whatever the first said.
    pub(super) async fn retry_gate(
        &mut self,
        key: (i64, String),
        request: GateRequest,
        runner: &GateRunner,
    ) -> Retry {
        let slot = match self.retries.remove(&key) {
            None => {
                let running = tokio::spawn(runner(request));
                self.retries.insert(key, Slot::Running(running));
                return Retry::Pending;
            }
            Some(slot) => slot,
        };
        let verdict = match slot {
            Slot::Done(verdict) => verdict,
            Slot::Running(running) if !running.is_finished() => {
                self.retries.insert(key, Slot::Running(running));
                return Retry::Pending;
            }
            Slot::Running(running) => match running.await {
                Ok(GateOutcome::Passed) => Retry::Passed,
                Ok(GateOutcome::Failed { exit_code, output }) => {
                    Retry::Failed(format!("exit {exit_code}: {}", excerpt(&output)))
                }
                Ok(GateOutcome::Errored { reason }) => Retry::Unrun(clip(&reason)),
                Err(error) => Retry::Unrun(format!("the retry task died: {error}")),
            },
        };
        self.retries.insert(key, Slot::Done(verdict.clone()));
        verdict
    }
}

/// Longest excerpt of a gate's output put in an announcement.
const EXCERPT_CHARS: usize = 200;

fn clip(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= EXCERPT_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(EXCERPT_CHARS).collect();
    format!("{kept}...")
}

/// What failed in a gate's output, short: up to three lines that name a failure (`test x ... FAILED`,
/// `FAILED x`, `--- FAIL: x`), otherwise the last line there is, which is where a formatter or a
/// linter says which check it was.
pub(super) fn excerpt(output: &str) -> String {
    let lines = || {
        output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
    };
    let failing: Vec<&str> = lines()
        .filter(|line| {
            (line.starts_with("test ") && line.ends_with("FAILED"))
                || line.starts_with("FAILED ")
                || line.starts_with("--- FAIL")
        })
        .take(3)
        .collect();
    if !failing.is_empty() {
        return clip(&failing.join("; "));
    }
    lines()
        .next_back()
        .map_or_else(|| "no output".to_owned(), clip)
}

/// The announcement for a resolution whose gate was not green. `first_output` is the output the
/// run stored for its own gate; `retry` is what the re-run said, `None` when none was made (a gate
/// that `errored` is not retried here, and a retry never ran for it).
pub(super) fn refusal(
    project: &str,
    branch: &str,
    head: &str,
    gate: &str,
    first_output: Option<&str>,
    retry: Option<&Retry>,
) -> String {
    let first = first_output.map_or_else(|| "no output stored".to_owned(), excerpt);
    match retry {
        Some(Retry::Failed(again)) => format!(
            "{project}'s conflict resolution on {branch} at {head} was resolved and committed by \
             the resolver, but its gate failed twice (first: {first}; retry: {again}); it was not \
             handed to the queue and needs a person"
        ),
        Some(Retry::Unrun(why)) => format!(
            "{project}'s conflict resolution on {branch} at {head} was resolved and committed by \
             the resolver, but its gate failed ({first}) and the retry could not run ({why}); it \
             was not handed to the queue and needs a person"
        ),
        _ => format!(
            "{project}'s conflict resolution on {branch} at {head} is merged but its run's gate \
             {gate} ({first}); it was not handed to the queue and needs a person"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_excerpt_names_the_failing_tests() {
        let output = "running 3 tests\ntest a::ok ... ok\ntest a::flaky ... FAILED\n\
                      test b::bad ... FAILED\n\nfailures:\n";
        assert_eq!(
            excerpt(output),
            "test a::flaky ... FAILED; test b::bad ... FAILED"
        );
    }

    #[test]
    fn the_excerpt_falls_back_to_the_last_line_and_is_bounded() {
        assert_eq!(excerpt("checking\nDiff in src/x.rs\n"), "Diff in src/x.rs");
        assert_eq!(excerpt("  \n"), "no output");
        assert!(excerpt(&"x".repeat(1000)).chars().count() <= EXCERPT_CHARS + 3);
    }

    #[test]
    fn the_refusal_says_the_merge_was_committed_and_how_often_the_gate_failed() {
        let twice = refusal(
            "p",
            "b",
            "abcd1234",
            "failed",
            Some("test t ... FAILED"),
            Some(&Retry::Failed("exit 1: test t ... FAILED".into())),
        );
        assert!(twice.contains("resolved and committed"), "{twice}");
        assert!(twice.contains("failed twice"), "{twice}");
        assert!(twice.contains("test t ... FAILED"), "{twice}");
        let unrun = refusal(
            "p",
            "b",
            "abcd1234",
            "failed",
            None,
            Some(&Retry::Unrun("no gate".into())),
        );
        assert!(unrun.contains("retry could not run (no gate)"), "{unrun}");
    }
}
