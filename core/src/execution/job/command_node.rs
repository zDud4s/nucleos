//! Command-node dispatcher (M4 C3): the nodes of a workflow graph that run a program and not a model.
//!
//! A command node consumes no quota and no budget, so none of `brakes()` applies to it except the
//! kill switch, which "obeys the kill switch like any step" (spec 3.4) and is re-read immediately
//! before the process is spawned. It never creates a run, never loads a loadout and never marks a
//! note delivered: the measurement is a `verify_runs` row (`ORIGIN_JOB_NODE`) and a `job_nodes` row.
//!
//! Inside a fan with verdict edges the instance's row is the ITEM's row and the existing
//! `gate_item_with` / `record_gate` machinery does the work (scoped verify, checkpoint, retry budget,
//! revert). This module is for every OTHER command node: a lint step, a `pytest` step, a non-fan gate.
//!
//! Node status vocabulary written here is the code's own: `passed`, `failed` (a non-zero exit of a
//! node that is not a gate-of-record) and `gate_errored` (nothing was measured); `gate_status` is
//! `passed` / `failed` / `errored`, exactly what `record_gate` writes for an item.

use std::time::Duration;

use super::*;
use crate::gate::GateOutcome;
use crate::project::commands::RunnableBy;
use crate::workflows::graph::{CommandSource, Kind, Node, command_source};

/// Which row of `job_nodes` a command node writes, and what its `verify_runs` row calls its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InstanceRef {
    /// The job round the singleton row belongs to (`job_nodes.round`).
    pub round: i64,
    /// Recorded in `verify_runs.ordinal`; `None` for a singleton.
    pub ordinal: Option<i64>,
}

/// What a command node's `command` turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Resolved {
    Run {
        argv: String,
        cwd: Option<String>,
        pass_exit_code: i64,
        /// Sourced from a `project_commands` row: its `pass_exit_code` decides what passing means.
        from_project: bool,
    },
    /// `project:gate` with no source anywhere. A gate node then passes UNMEASURED, exactly as
    /// `pass_without_measuring` does for an item today.
    NotConfigured,
    /// Nothing may run, and nothing was measured. Carries the reason a person reads.
    Refused(String),
}

/// What running a command node found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CommandResult {
    Passed,
    Failed {
        exit_code: i64,
        output: String,
    },
    /// The measurement did not happen (would not start, timed out, signalled, refused, unreadable).
    Errored(String),
    /// A gate with no command anywhere: passed without measuring.
    Unconfigured,
    /// The kill switch was engaged just before the spawn; the job is parked and nothing ran.
    Parked,
}

/// What the walk does next, per spec gap S9 / OWNER DECISION D6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CommandFollow {
    /// Follow the `pass` edge (or the single out-edge).
    Pass,
    /// Follow the `fail` edge.
    Fail,
    /// No verdict edges and a non-zero exit: the node failed and so does the job.
    JobFails(String),
    /// Silence is not a verdict: the job stops `gate_errored`.
    GateErrored,
    /// Already parked by `run_command_node`; the pass ends.
    Parked,
}

/// PURE. D6: verdict edges present -> `Passed` follows pass, `Failed` follows fail; `Errored` is
/// `GateErrored` whatever the edges; no verdict edges -> a non-zero exit fails the job and a zero
/// follows the single out-edge. No retry budget outside a fan.
pub(super) fn follow_for(result: &CommandResult, has_verdict_edges: bool) -> CommandFollow {
    match result {
        CommandResult::Parked => CommandFollow::Parked,
        CommandResult::Errored(_) => CommandFollow::GateErrored,
        CommandResult::Passed | CommandResult::Unconfigured => CommandFollow::Pass,
        CommandResult::Failed { exit_code, output } => {
            if has_verdict_edges {
                CommandFollow::Fail
            } else {
                CommandFollow::JobFails(format!(
                    "the command exited with code {exit_code}\n{}",
                    super::gate::gate_output_tail(output)
                ))
            }
        }
    }
}

/// Literal -> `Run`. `project:<name>` -> OWNER DECISION D4: (1) `name == "gate"`: the project's
/// `autopilot.yaml` `gate_command` (today's source); (2) the project's `project_commands` row, which
/// brings `cwd` and `pass_exit_code`. A row that only a person may run is refused (D5) for anything
/// but `gate`. No source: `NotConfigured` for `gate`, `Refused` for any other name.
pub(super) async fn resolve_command(state: &AppState, job: &JobRow, node: &Node) -> Resolved {
    let Some(command) = node
        .command
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    else {
        return Resolved::Refused(format!("command node `{}` has no command", node.id));
    };
    // A `cwd` that leaves the tree is refused here already, lexically; `run_command_node` joins it
    // onto the real roots with the same function.
    if let Some(cwd) = node.cwd.as_deref()
        && crate::inspect::safe_join(Path::new("."), cwd).is_err()
    {
        return Resolved::Refused(format!(
            "command node `{}` has a cwd (`{cwd}`) that is absolute or leaves the project",
            node.id
        ));
    }
    match command_source(command) {
        CommandSource::Literal(text) => Resolved::Run {
            argv: text.to_owned(),
            cwd: node.cwd.clone(),
            pass_exit_code: 0,
            from_project: false,
        },
        CommandSource::Project(name) => resolve_project(state, job, node, name).await,
    }
}

async fn resolve_project(state: &AppState, job: &JobRow, node: &Node, name: &str) -> Resolved {
    if name.is_empty() {
        return Resolved::Refused(format!(
            "command node `{}` names `project:` with no command name",
            node.id
        ));
    }
    if name == "gate" {
        match crate::config::load_schedule_rules(
            state.machine_config_root.as_deref(),
            &job.project_id,
        ) {
            Ok(rules) => {
                if let Some(argv) = rules.gate_command {
                    return Resolved::Run {
                        argv,
                        cwd: node.cwd.clone(),
                        pass_exit_code: 0,
                        from_project: false,
                    };
                }
            }
            // Unreadable is not absent: a configuration nobody can read means the measurement did
            // not happen, which is not a project without a gate.
            Err(error) => {
                return Resolved::Refused(format!("gate configuration is unreadable: {error}"));
            }
        }
    }
    let declared = match crate::project::commands::list(&state.pool, &job.project_id).await {
        Ok(declared) => declared,
        Err(error) => {
            return Resolved::Refused(format!("the project's commands could not be read: {error}"));
        }
    };
    match declared.into_iter().find(|c| c.name == name) {
        Some(found) => {
            if found.runnable_by == RunnableBy::Person && name != "gate" {
                return Resolved::Refused(format!(
                    "`project:{name}` is declared as runnable only by a person, so an autonomous \
                     run does not run it"
                ));
            }
            Resolved::Run {
                argv: found.command,
                cwd: node.cwd.clone().or(found.cwd),
                pass_exit_code: found.pass_exit_code,
                from_project: true,
            }
        }
        None if name == "gate" => Resolved::NotConfigured,
        None => Resolved::Refused(format!(
            "this project declares no command named `{name}` (referenced by node `{}`)",
            node.id
        )),
    }
}

/// Writes the verdict on the node's singleton row. Best effort in the way `record_gate` is: a row
/// that cannot be written is a warning, and the result still goes back to the caller.
async fn finish_row(
    pool: &SqlitePool,
    job: &JobRow,
    node: &Node,
    instance: &InstanceRef,
    status: &str,
    gate_status: Option<&str>,
    output: Option<&str>,
) {
    let written = sqlx::query(
        "UPDATE job_nodes SET status = ?, gate_status = ?, gate_output = ?
         WHERE job_id = ? AND node_id = ? AND round = ? AND ordinal IS NULL
           AND status IN ('pending','running')",
    )
    .bind(status)
    .bind(gate_status)
    .bind(output.map(super::gate::gate_output_tail))
    .bind(job.id)
    .bind(&node.id)
    .bind(instance.round)
    .execute(pool)
    .await;
    if let Err(error) = written {
        tracing::warn!(job_id = job.id, node = %node.id, %error, "could not record a command node's verdict");
    }
}

/// Runs one command node to a verdict. See the module docs for what it deliberately does not do.
pub(super) async fn run_command_node(
    state: &AppState,
    job: &JobRow,
    node: &Node,
    instance: &InstanceRef,
    worktree: &Path,
) -> CommandResult {
    let pool = &state.pool;
    debug_assert_eq!(node.kind, Kind::Command);

    // The tick checks the switch once per tick and a pass can chain several steps, so it is re-read
    // immediately before anything is spawned. Fails closed: a switch that cannot be read holds.
    match crate::governance::autopilot::kill_switch_engaged(pool).await {
        Ok(false) => {}
        _ => {
            let _ = park(state, job, "kill-switch", "the emergency stop is engaged").await;
            return CommandResult::Parked;
        }
    }

    // Idempotent per (job, node, round): returns the row a retried pass already made.
    if let Err(error) =
        insert_singleton_node(pool, job.id, &node.id, instance.round, "running").await
    {
        tracing::warn!(job_id = job.id, node = %node.id, %error, "could not open a command node's row");
    }

    let errored = |reason: String| async move {
        finish_row(
            pool,
            job,
            node,
            instance,
            "gate_errored",
            Some("errored"),
            Some(&reason),
        )
        .await;
        CommandResult::Errored(reason)
    };

    let (argv, cwd, pass_exit_code, from_project) = match resolve_command(state, job, node).await {
        Resolved::Run {
            argv,
            cwd,
            pass_exit_code,
            from_project,
        } => (argv, cwd, pass_exit_code, from_project),
        Resolved::NotConfigured => {
            finish_row(pool, job, node, instance, "passed", None, None).await;
            tracing::debug!(job_id = job.id, node = %node.id, "gate node not measured: no gate command");
            return CommandResult::Unconfigured;
        }
        Resolved::Refused(reason) => return errored(reason).await,
    };

    let timeout = match node.timeout.as_deref() {
        None => crate::state::DEFAULT_GATE_TIMEOUT,
        Some(text) => match crate::workflows::timeout::parse(text) {
            Ok(timeout) => timeout,
            Err(reason) => {
                return errored(format!(
                    "node `{}` has an unusable timeout: {reason}",
                    node.id
                ))
                .await;
            }
        },
    };

    let root = Path::new(&job.project_root);
    let (tree, root) = match cwd.as_deref() {
        None => (worktree.to_path_buf(), root.to_path_buf()),
        Some(rel) => match (
            crate::inspect::safe_join(worktree, rel),
            crate::inspect::safe_join(root, rel),
        ) {
            (Ok(tree), Ok(root)) => (tree, root),
            _ => {
                return errored(format!(
                    "node `{}` has a cwd (`{rel}`) that leaves the worktree",
                    node.id
                ))
                .await;
            }
        },
    };

    let outcome = crate::verify::runs::timed_gate(
        Some(pool),
        crate::verify::runs::GateContext {
            project_id: Some(&job.project_id),
            origin: crate::verify::runs::ORIGIN_JOB_NODE,
            origin_id: Some(job.id),
            ordinal: instance.ordinal,
        },
        &tree,
        &root,
        &argv,
        timeout,
    )
    .await;

    // A cancel is most likely to land inside a long command: do not write a verdict onto a job that
    // has ended. `Err` says nothing about the job and is let through.
    if matches!(job_is_live(pool, job.id).await, Ok(false)) {
        tracing::info!(job_id = job.id, node = %node.id, "job ended while its command ran; the outcome is not recorded");
        return CommandResult::Errored("the job ended while the command ran".to_owned());
    }

    let result = classify(from_project, pass_exit_code, outcome);
    match &result {
        CommandResult::Passed => {
            finish_row(pool, job, node, instance, "passed", Some("passed"), None).await;
        }
        CommandResult::Failed { output, .. } => {
            finish_row(
                pool,
                job,
                node,
                instance,
                "failed",
                Some("failed"),
                Some(output),
            )
            .await;
        }
        CommandResult::Errored(reason) => {
            finish_row(
                pool,
                job,
                node,
                instance,
                "gate_errored",
                Some("errored"),
                Some(reason),
            )
            .await;
        }
        // Neither can come out of `classify`.
        CommandResult::Unconfigured | CommandResult::Parked => {}
    }
    result
}

/// PURE. A `GateOutcome` read against the rule for what passing means: a project command's own
/// `pass_exit_code` through `project::commands::verdict`, any other command's exit 0.
fn classify(from_project: bool, pass_exit_code: i64, outcome: GateOutcome) -> CommandResult {
    if !from_project {
        return match outcome {
            GateOutcome::Passed => CommandResult::Passed,
            GateOutcome::Failed { exit_code, output } => CommandResult::Failed {
                exit_code: i64::from(exit_code),
                output,
            },
            GateOutcome::Errored { reason } => CommandResult::Errored(reason),
        };
    }
    let finished = crate::project::commands::verdict(pass_exit_code, outcome);
    match finished.outcome {
        crate::project::commands::Outcome::Passed => CommandResult::Passed,
        crate::project::commands::Outcome::Failed => CommandResult::Failed {
            exit_code: finished.exit_code.unwrap_or(-1),
            output: finished.output.unwrap_or_default(),
        },
        // `Running` is never produced by `verdict`; reading it as unmeasured is the safe direction.
        crate::project::commands::Outcome::Errored | crate::project::commands::Outcome::Running => {
            CommandResult::Errored(finished.output.unwrap_or_default())
        }
    }
}

/// The ceiling a command node runs under, for callers that want to show it.
#[allow(dead_code)]
pub(super) fn timeout_of(node: &Node) -> Result<Duration, String> {
    match node.timeout.as_deref() {
        None => Ok(crate::state::DEFAULT_GATE_TIMEOUT),
        Some(text) => crate::workflows::timeout::parse(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::job::tests::{seed_job, test_pool, test_state};
    use crate::project::commands::{Declaration, declare};

    fn node(yaml: &str) -> Node {
        serde_yaml::from_str(yaml).expect("a node")
    }

    fn declaration(name: &str, command: &str, by: RunnableBy, pass: i64) -> Declaration {
        Declaration {
            name: name.to_owned(),
            command: command.to_owned(),
            cwd: None,
            is_gate: name == "gate",
            pass_exit_code: pass,
            runnable_by: by,
        }
    }

    /// A state reading project state from a temp "home", the job, and that home (kept alive).
    async fn setup(autopilot: Option<&str>) -> (AppState, JobRow, tempfile::TempDir) {
        let pool = test_pool().await;
        let home = tempfile::tempdir().unwrap();
        if let Some(text) = autopilot {
            crate::project::state::write_for_test(
                home.path(),
                "project-a",
                crate::project::state::AUTOPILOT_FILE,
                text,
            );
        }
        let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
        let state = AppState {
            machine_config_root: Some(home.path().to_path_buf()),
            ..test_state(pool.clone()).await
        };
        let job = load_job(&pool, job_id).await.unwrap();
        (state, job, home)
    }

    #[tokio::test]
    async fn a_literal_command_runs_as_written() {
        let (state, job, _home) = setup(None).await;
        let resolved = resolve_command(
            &state,
            &job,
            &node("id: lint\ntype: command\ncommand: git --version\n"),
        )
        .await;
        assert_eq!(
            resolved,
            Resolved::Run {
                argv: "git --version".to_owned(),
                cwd: None,
                pass_exit_code: 0,
                from_project: false,
            }
        );
    }

    #[tokio::test]
    async fn project_gate_prefers_the_autopilot_yaml_gate_command() {
        let (state, job, _home) = setup(Some("gate_command: \"git status\"\n")).await;
        declare(
            &state.pool,
            "project-a",
            declaration("gate", "git log", RunnableBy::Agent, 0),
        )
        .await
        .unwrap();
        let resolved = resolve_command(
            &state,
            &job,
            &node("id: gate\ntype: command\ncommand: \"project:gate\"\n"),
        )
        .await;
        assert_eq!(
            resolved,
            Resolved::Run {
                argv: "git status".to_owned(),
                cwd: None,
                pass_exit_code: 0,
                from_project: false,
            }
        );
    }

    #[tokio::test]
    async fn project_gate_falls_back_to_the_declared_project_command() {
        let (state, job, _home) = setup(None).await;
        declare(
            &state.pool,
            "project-a",
            declaration("gate", "git log", RunnableBy::Person, 0),
        )
        .await
        .unwrap();
        let resolved = resolve_command(
            &state,
            &job,
            &node("id: gate\ntype: command\ncommand: \"project:gate\"\n"),
        )
        .await;
        // D5: the gate is the one name a person-only row may still be run under.
        assert_eq!(
            resolved,
            Resolved::Run {
                argv: "git log".to_owned(),
                cwd: None,
                pass_exit_code: 0,
                from_project: true,
            }
        );
    }

    #[tokio::test]
    async fn project_gate_with_no_source_is_not_configured() {
        let (state, job, _home) = setup(None).await;
        let gate = node("id: gate\ntype: command\ncommand: \"project:gate\"\n");
        assert_eq!(
            resolve_command(&state, &job, &gate).await,
            Resolved::NotConfigured
        );
        // Any other name with no source is a refusal, not an unmeasured pass.
        let lint = node("id: lint\ntype: command\ncommand: \"project:lint\"\n");
        assert!(matches!(
            resolve_command(&state, &job, &lint).await,
            Resolved::Refused(_)
        ));
    }

    #[tokio::test]
    async fn a_person_only_command_is_refused_for_an_autonomous_run() {
        let (state, job, _home) = setup(None).await;
        declare(
            &state.pool,
            "project-a",
            declaration("deploy", "git push", RunnableBy::Person, 0),
        )
        .await
        .unwrap();
        let resolved = resolve_command(
            &state,
            &job,
            &node("id: deploy\ntype: command\ncommand: \"project:deploy\"\n"),
        )
        .await;
        assert!(matches!(resolved, Resolved::Refused(reason) if reason.contains("person")));
    }

    #[tokio::test]
    async fn pass_exit_code_of_a_project_command_is_honoured() {
        let (state, job, _home) = setup(None).await;
        declare(
            &state.pool,
            "project-a",
            declaration("lint", "git diff --quiet", RunnableBy::Agent, 1),
        )
        .await
        .unwrap();
        let resolved = resolve_command(
            &state,
            &job,
            &node("id: lint\ntype: command\ncommand: \"project:lint\"\n"),
        )
        .await;
        assert_eq!(
            resolved,
            Resolved::Run {
                argv: "git diff --quiet".to_owned(),
                cwd: None,
                pass_exit_code: 1,
                from_project: true,
            }
        );
        // And the verdict reads exit 1 as the pass and exit 0 as the failure.
        assert_eq!(
            classify(
                true,
                1,
                GateOutcome::Failed {
                    exit_code: 1,
                    output: String::new()
                }
            ),
            CommandResult::Passed
        );
        assert!(matches!(
            classify(true, 1, GateOutcome::Passed),
            CommandResult::Failed { exit_code: 0, .. }
        ));
    }

    #[tokio::test]
    async fn a_cwd_that_escapes_the_worktree_is_refused() {
        let (state, job, _home) = setup(None).await;
        for cwd in ["../outside", "a/../../b", "/etc"] {
            let resolved = resolve_command(
                &state,
                &job,
                &node(&format!(
                    "id: lint\ntype: command\ncommand: git --version\ncwd: \"{cwd}\"\n"
                )),
            )
            .await;
            assert!(
                matches!(resolved, Resolved::Refused(_)),
                "`{cwd}` must be refused, got {resolved:?}"
            );
        }
        let inside = resolve_command(
            &state,
            &job,
            &node("id: lint\ntype: command\ncommand: git --version\ncwd: sub/dir\n"),
        )
        .await;
        assert!(matches!(inside, Resolved::Run { cwd: Some(_), .. }));
    }

    #[test]
    fn follow_for_table() {
        let failed = || CommandResult::Failed {
            exit_code: 3,
            output: "boom".to_owned(),
        };
        let errored = || CommandResult::Errored("would not start".to_owned());
        // (result, has_verdict_edges, expected)
        let table = [
            (CommandResult::Passed, true, CommandFollow::Pass),
            (CommandResult::Passed, false, CommandFollow::Pass),
            (CommandResult::Unconfigured, true, CommandFollow::Pass),
            (CommandResult::Unconfigured, false, CommandFollow::Pass),
            (failed(), true, CommandFollow::Fail),
            (errored(), true, CommandFollow::GateErrored),
            (errored(), false, CommandFollow::GateErrored),
            (CommandResult::Parked, true, CommandFollow::Parked),
            (CommandResult::Parked, false, CommandFollow::Parked),
        ];
        for (result, edges, expected) in table {
            assert_eq!(
                follow_for(&result, edges),
                expected,
                "{result:?} with verdict edges {edges}"
            );
        }
        // No verdict edges: a non-zero exit fails the job and says why.
        assert!(matches!(
            follow_for(&failed(), false),
            CommandFollow::JobFails(why) if why.contains("code 3") && why.contains("boom")
        ));
    }
}
