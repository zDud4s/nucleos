//! Landing the singleton agent nodes of a bundle graph: validating a finished node's answer,
//! the single retry, and telling the router what became of the run.

use super::*;

use super::dispatch::{AgentNodeLauncher, resolve_launch_with};
use super::engine::{
    build_facts, clear_stale_emission, is_autopilot_stage, keys_read_from, load_graph,
};
use crate::workflows::facts::Facts;

/// What a finished, completed node's answer earns (spec agentes 7 / motor 3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Landing {
    /// The answer is acceptable: the node is `passed`.
    Pass,
    /// Invalid on the first attempt: relaunch once with this report appended to the prompt.
    Retry(String),
    /// Invalid on the retry: the node is `failed`.
    Fail(String),
}

/// PURE. `attempts` is how many retries this node has already had (0 on the first answer).
pub(super) fn validation_verdict(attempts: usize, check: Result<(), String>) -> Landing {
    match (check, attempts) {
        (Ok(()), _) => Landing::Pass,
        (Err(report), 0) => Landing::Retry(report),
        (Err(report), _) => Landing::Fail(report),
    }
}

/// PURE. The launch of the single retry: the same launch, pinned to the engine and model of the
/// run that was refused (agentes 7), with the validator's report appended to the prompt.
pub(super) fn retried_launch(
    mut launch: super::dispatch::NodeLaunch,
    runner: Option<&str>,
    model: Option<String>,
    effort: Option<String>,
    report: &str,
) -> super::dispatch::NodeLaunch {
    if let Some(kind) = runner.and_then(crate::models::route_advice::RunnerKind::parse) {
        launch.pin = Some(super::dispatch::EnginePin {
            runner: kind,
            model,
            effort,
        });
    }
    launch.prompt = super::dispatch::retry_prompt(&launch.prompt, report);
    launch
}

/// Tells the router what became of a routed run. Silent when the run had no decision or the
/// daemon has no router.
fn tell_the_router(
    state: &AppState,
    decision: Option<String>,
    outcome: crate::models::route_advice::Outcome,
) {
    if let (Some(router), Some(decision)) = (
        state.runner.router(),
        decision.filter(|decision| !decision.is_empty()),
    ) {
        crate::models::route_advice::report_detached(router, decision, outcome);
    }
}

/// Drops the singleton rows a pass left `running` with no run behind them: a command node whose
/// daemon died mid-command, or an agent node that was opened and never got its run. Passes of one
/// job are serial and this runs at the start of one, so such a row belongs to nobody; left alone the
/// walker would read it as a node in flight and wait on it for ever. Deleting it makes the node
/// "not entered", so the walker dispatches it again (what a fan item with a lost run gets, bar the
/// failure: a command is safe to run twice).
async fn release_orphaned_rows(pool: &SqlitePool, job: &JobRow) -> sqlx::Result<()> {
    let released = sqlx::query(
        "DELETE FROM job_nodes
         WHERE job_id = ? AND ordinal IS NULL AND status = 'running' AND run_id IS NULL",
    )
    .bind(job.id)
    .execute(pool)
    .await?
    .rows_affected();
    if released > 0 {
        tracing::warn!(
            job_id = job.id,
            released,
            "released singleton node row(s) left running with no run; they will be dispatched again"
        );
    }
    Ok(())
}

/// The autopilot's `spec` and `review` nodes keep no row of their own while they run (the walker
/// reads them from their runs), but a workflow can read their emissions, and facts are published
/// only from a node whose latest row is `passed`. So when such a run has completed, its singleton row
/// is written `passed` here, as the plan's is by `ingest_plan`. `review` only counts runs of the
/// current round (after the replan that opened it); `spec` runs once per job.
async fn record_autopilot_passes(state: &AppState, job: &JobRow) -> sqlx::Result<()> {
    let pool = &state.pool;
    let opened_by: Option<i64> = sqlx::query_scalar("SELECT replan_run_id FROM jobs WHERE id = ?")
        .bind(job.id)
        .fetch_one(pool)
        .await?;
    for (stage, after) in [("spec", 0), ("review", opened_by.unwrap_or(0))] {
        let latest: Option<(i64, String)> = sqlx::query_as(
            "SELECT id, status FROM runs WHERE job_id = ? AND stage = ? AND id > ?
             ORDER BY id DESC LIMIT 1",
        )
        .bind(job.id)
        .bind(stage)
        .bind(after)
        .fetch_optional(pool)
        .await?;
        let Some((run_id, status)) = latest else {
            continue;
        };
        // A completed run whose task is still registered has not finished handing off.
        if status != "completed"
            || state
                .run_handles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&run_id)
        {
            continue;
        }
        insert_singleton_node(pool, job.id, stage, job.round, "passed").await?;
    }
    Ok(())
}

/// Lands every singleton agent node whose run has ended: `running` row + terminal, unregistered
/// run. The autopilot's own nodes (spec/plan/review/replan) keep landing through `reconcile_nodes`
/// and the ingest functions, untouched. Runs right after `reconcile_nodes` on every pass.
///
/// - a run that did not complete -> the row is `failed` (`cancelled` for a cancel), no validation;
/// - completed, no output check -> `passed`;
/// - completed, check Ok -> router `pass`, then `passed` (the emission is read only after this);
/// - completed, check Err -> one relaunch on the same engine and model with the report appended,
///   and a second Err -> `failed`, router `fail`, feed `job_node_invalid_output`.
///
/// The retry count lives in the row's `gate_attempts`, which a singleton row otherwise never uses:
/// it is bumped when the relaunch's run exists.
pub(super) async fn land_nodes(state: &AppState, job: &JobRow) -> sqlx::Result<()> {
    let launcher = super::dispatch::launcher();
    land_nodes_with(launcher.as_ref(), state, job).await
}

/// [`land_nodes`] against an explicit launcher (what the tests use).
pub(super) async fn land_nodes_with(
    launcher: &dyn AgentNodeLauncher,
    state: &AppState,
    job: &JobRow,
) -> sqlx::Result<()> {
    let pool = &state.pool;
    release_orphaned_rows(pool, job).await?;
    record_autopilot_passes(state, job).await?;
    let landed: Vec<(String, i64, i64, i64, String)> = sqlx::query_as(
        "SELECT i.node_id, i.round, i.gate_attempts, r.id, r.status
         FROM job_nodes i JOIN runs r ON r.id = i.run_id
         WHERE i.job_id = ? AND i.ordinal IS NULL AND i.status = 'running'
           AND r.status NOT IN ('running','awaiting_approval')
         ORDER BY i.id",
    )
    .bind(job.id)
    .fetch_all(pool)
    .await?;

    for (node_id, round, attempts, run_id, run_status) in landed {
        if is_autopilot_stage(&node_id) {
            continue;
        }
        // The same guard as `reconcile_nodes`: a terminal run whose task is still registered has
        // not finished handing off yet.
        if state
            .run_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&run_id)
        {
            continue;
        }
        land_node(
            launcher,
            state,
            job,
            &node_id,
            round,
            attempts,
            run_id,
            &run_status,
        )
        .await?;
    }
    Ok(())
}

/// Writes `<artifacts>/<node_id>.json` atomically (temp file in the same directory, then rename),
/// replacing whatever the agent may have written there.
async fn write_emission(
    worktree: &Path,
    node_id: &str,
    facts: &serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    let path = crate::workflows::facts::emission_path(worktree, node_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let dir = path.parent().unwrap_or(worktree);
    tokio::fs::create_dir_all(dir).await?;
    let tmp = dir.join(format!(".{node_id}.json.tmp"));
    let body = serde_json::to_vec(facts)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    tokio::fs::write(&tmp, body).await?;
    if let Err(error) = tokio::fs::rename(&tmp, &path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(error);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn land_node(
    launcher: &dyn AgentNodeLauncher,
    state: &AppState,
    job: &JobRow,
    node_id: &str,
    round: i64,
    attempts: i64,
    run_id: i64,
    run_status: &str,
) -> sqlx::Result<()> {
    let pool = &state.pool;
    if run_status != "completed" {
        let to = if run_status == STATUS_CANCELLED {
            STATUS_CANCELLED
        } else {
            "failed"
        };
        if set_singleton_status(pool, job.id, node_id, round, &["running"], to).await?
            && to == "failed"
        {
            say(
                pool,
                job,
                "job_node_failed",
                &format!("job {} node {node_id} ended `{run_status}`", job.id),
            )
            .await;
        }
        return Ok(());
    }

    #[allow(clippy::type_complexity)]
    let (stdout, runner, model, effort, decision): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT stdout, runner, model, effort, route_decision_id FROM runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await?;

    let Ok(Some(worktree)) = job_worktree(pool, job.id).await else {
        return Ok(());
    };
    let graph = match load_graph(pool, job.id).await {
        Ok(graph) => graph,
        Err(error) => {
            tracing::warn!(job_id = job.id, node_id, %error, "cannot land a node: its graph is unreadable");
            return Ok(());
        }
    };
    let facts = build_facts(pool, job, &worktree.0, &graph)
        .await
        .unwrap_or_else(|_| Facts::new());

    // The check and, when it comes to a retry, the launch to repeat. Scoped: the context borrows the
    // worktree, which `spawn_node` takes by value afterwards.
    let (mut verdict, mut retry_launch): (Landing, Option<super::dispatch::NodeLaunch>) =
        (Landing::Pass, None);
    // Set only for a node with an output check whose reply passed: the facts the daemon writes.
    let mut emission: Option<serde_json::Map<String, serde_json::Value>> = None;
    if let Some(node) = graph.graph.nodes.iter().find(|node| node.id == node_id) {
        let ctx = super::dispatch::NodeCtx {
            state,
            job,
            node,
            round,
            ordinal: None,
            worktree: &worktree.0,
            facts: &facts,
        };
        if let Some(check) = launcher.output_check(&ctx).await {
            // The reply, not the stream: `runs.stdout` is the CLI's whole stream-json, where a
            // `## Section` heading never starts a line (job 30, run 900727). A run with no result
            // event is checked as printed.
            let stdout = stdout.unwrap_or_default();
            let reply = crate::execution::runner::extract_reply(&stdout).unwrap_or(stdout);
            verdict = validation_verdict(attempts.max(0) as usize, check.check(&reply));
            if matches!(verdict, Landing::Pass) {
                emission = Some(check.facts(&reply));
            }
            // A row left over from an earlier round cannot be relaunched here: `spawn_node` links
            // the run of the CURRENT round's row.
            if let Landing::Retry(report) = verdict.clone()
                && round != job.round
            {
                verdict = Landing::Fail(report);
            }
            if let Landing::Retry(report) = verdict.clone() {
                let keys = keys_read_from(&graph.graph, node_id);
                let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                let protocol = super::dispatch::composed_protocol(
                    node_id,
                    &artifacts_for(&worktree.0),
                    job.prompt.as_deref().unwrap_or_default(),
                    round,
                    &keys,
                    true,
                );
                match resolve_launch_with(launcher, &ctx, protocol).await {
                    Ok(launch) => {
                        retry_launch = Some(retried_launch(
                            launch,
                            runner.as_deref(),
                            model.clone(),
                            effort.clone(),
                            &report,
                        ));
                    }
                    Err(refused) => {
                        verdict = Landing::Fail(format!(
                            "{report}; the retry was refused: {}",
                            refused.reason()
                        ));
                    }
                }
            }
        }
    }

    use crate::models::route_advice::Outcome;
    match verdict {
        Landing::Pass => {
            if let Some(facts) = emission {
                // Choice: a failed write is logged and the node still passes. The reply was valid;
                // the missing emission makes the engine's section 4.5 rule stop the job if a
                // `when:` reads one of its keys, and a job that reads none is unaffected.
                if let Err(error) = write_emission(&worktree.0, node_id, &facts).await {
                    tracing::warn!(job_id = job.id, node_id, %error, "could not write the node's emission from its reply");
                }
            }
            // Reported HERE and not at the run's end: `judged_later` kept `outcome_at_run_end`
            // silent for this run, because passing the validator is part of the verdict.
            tell_the_router(state, decision, Outcome::Pass);
            set_singleton_status(pool, job.id, node_id, round, &["running"], "passed").await?;
        }
        Landing::Retry(report) => {
            let Some(launch) = retry_launch else {
                return Ok(());
            };
            tell_the_router(state, decision, Outcome::Fail);
            let claimed = sqlx::query(
                "UPDATE job_nodes SET gate_attempts = gate_attempts + 1
                 WHERE job_id = ? AND node_id = ? AND round = ? AND ordinal IS NULL
                   AND status = 'running' AND gate_attempts = ?",
            )
            .bind(job.id)
            .bind(node_id)
            .bind(round)
            .bind(attempts)
            .execute(pool)
            .await?;
            if claimed.rows_affected() != 1 {
                return Ok(());
            }
            if let Err(error) = clear_stale_emission(&worktree.0, node_id).await {
                tracing::warn!(job_id = job.id, node_id, %error, "could not clear a refused answer's emission; not relaunching");
            }
            say(
                pool,
                job,
                "job_node_retried",
                &format!(
                    "job {} node {node_id}: its answer was refused ({report}); it is run once more",
                    job.id
                ),
            )
            .await;
            spawn_node(
                state,
                job,
                NodeSpec {
                    node_id,
                    claim: None,
                    launch,
                },
                worktree,
                NoRoom::Park,
            )
            .await;
            // No new run (parked for want of a slot, a transient error): the retry has not been
            // spent, so the next pass asks again.
            let linked: Option<i64> = sqlx::query_scalar(
                "SELECT run_id FROM job_nodes
                 WHERE job_id = ? AND node_id = ? AND round = ? AND ordinal IS NULL",
            )
            .bind(job.id)
            .bind(node_id)
            .bind(round)
            .fetch_optional(pool)
            .await?
            .flatten();
            if linked == Some(run_id) {
                sqlx::query(
                    "UPDATE job_nodes SET gate_attempts = gate_attempts - 1
                     WHERE job_id = ? AND node_id = ? AND round = ? AND ordinal IS NULL
                       AND status = 'running' AND gate_attempts > 0",
                )
                .bind(job.id)
                .bind(node_id)
                .bind(round)
                .execute(pool)
                .await?;
            }
        }
        Landing::Fail(report) => {
            tell_the_router(state, decision, Outcome::Fail);
            if set_singleton_status(pool, job.id, node_id, round, &["running"], "failed").await? {
                say(
                    pool,
                    job,
                    "job_node_invalid_output",
                    &format!(
                        "job {} node {node_id}: its answer was refused by the output validator: {report}",
                        job.id
                    ),
                )
                .await;
            }
        }
    }
    Ok(())
}
