//! Tree-job driver tick: advances every unfinished tree job one step per pass.
//!
//! One pass over a job, under the job's lock (`http::job_lock`), in this order:
//!
//! 1. each branch, in name order: a branch whose node reported `done` is gated (the gate runs in a
//!    spawned task, never inside the tick); a `red` branch is fixed by relaunching its node with the
//!    findings, twice at most, then parked; a `green` branch has its merge queued or settled;
//! 2. the nodes of merged and dropped branches are closed with their report;
//! 3. landing: once the root reported `done` and every branch is finished, the root closes (last),
//!    and the job either finishes held or has its landing queued or settled.
//!
//! Every step reads the state it wrote (`merge_request_id`, `land_id`, `merge_sha`), so a pass that
//! finds nothing to do writes nothing and a restarted daemon picks up where the old one stopped.
//! Neither a transaction nor a lock on the pool outlives a call: the queue is asked with none held.
//! `decisions::park` takes the job's lock itself, so parks are collected and made after it is freed.

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use sqlx::SqlitePool;

use super::decisions;
use super::engine::{apply_transition, branch_node_may_close};
use super::http::job_lock;
use super::landing::{
    self, LandingError, MergeSettlement, apply_gate_verdict, check_may_land, check_may_merge,
    land_job, scope_caller_id, settle_land, settle_merge, submit_merge,
};
use super::model::JOB_TERMINAL_STATE;
use super::open::{relaunch_node, run_is_live};
use super::store::{self, TreeJob};
use crate::execution::git_exec::OPERATION_TIMEOUT;
use crate::execution::vcs::{Origin, ResolvedRepo, resolve_repo};
use crate::gate::GateOutcome;
use crate::state::AppState;
use crate::verify::{ScopeVerdict, gate_job_scope};
use crate::work_nodes::{self, CLOSE_STATUSES, CloseArgs, NodeLimits};

/// How long the production gate waits for the verify queue.
const GATE_WAIT: Duration = Duration::from_secs(900);
/// Key prefix and cap of the relaunches that follow a red gate.
const FIX_PREFIX: &str = "fix-";
const FIX_CAP: i64 = 2;
/// Longest tail of the gate's findings that goes into a relaunch prompt.
const FINDINGS_MAX: usize = 3000;

/// What a gate measured for one branch. `Err` from a [`GateFn`] means nothing was measured (the
/// scope gate's undecided outcomes), so the branch stays where it was. The driver maps it back onto
/// a `ScopeVerdict` and applies it with `landing::apply_gate_verdict`: `Passed` is `Green`,
/// `Failed { output }` is `Red` with the output as `summary`, anything else is `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GateVerdict {
    Green,
    Red { summary: String },
}

pub(super) type GateFuture = Pin<Box<dyn Future<Output = Result<GateVerdict, String>> + Send>>;

/// Runs the gate of one branch: called with `(job id, branch name)`. The driver spawns the future
/// and never awaits it inside a tick.
pub(super) type GateFn = Arc<dyn Fn(String, String) -> GateFuture + Send + Sync>;

/// A branch to park after the job's lock is released: `(node id, question)`.
type Park = (String, String);

/// One pass over every unfinished job. A failure on one job is logged and never stops the others.
pub async fn tick(state: &AppState) {
    let jobs = match store::list_non_terminal(&state.pool).await {
        Ok(jobs) => jobs,
        Err(error) => {
            tracing::warn!(%error, "tree driver could not list the open jobs");
            return;
        }
    };
    let gate = production_gate(state);
    for job in jobs {
        advance(state, &job.id, gate.clone()).await;
    }
}

/// Advance one stored tree job with the production gate (`crate::verify::installed()`). Without a
/// verify executor nothing is gated, and the rest of the job still moves.
pub async fn advance_job(state: &AppState, job_id: &str) {
    advance(state, job_id, production_gate(state)).await;
}

/// One step of the driver for one job, with the gate injected so tests can script it.
pub(super) async fn advance_job_with_gate(state: &AppState, job_id: &str, gate: GateFn) {
    advance(state, job_id, Some(gate)).await;
}

/// The gate over the verify queue, or `None` (with a warning) while no executor is installed.
///
/// It runs `gate_job_scope` itself rather than `landing::gate_branch`: that one persists the whole
/// job without the job's lock, and the driver records the verdict under it.
fn production_gate(state: &AppState) -> Option<GateFn> {
    let Some(executor) = crate::verify::installed() else {
        tracing::warn!("tree driver has no verify executor installed: branches are not gated");
        return None;
    };
    let pool = state.pool.clone();
    let gate: GateFn = Arc::new(move |job_id: String, branch: String| -> GateFuture {
        let executor = executor.clone();
        let pool = pool.clone();
        Box::pin(async move {
            let job = store::load(&pool, &job_id)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("tree job {job_id} not found"))?;
            let worktree = job
                .manifest
                .branches
                .get(&branch)
                .and_then(|held| held.worktree.clone())
                .ok_or_else(|| format!("branch {branch} has no worktree to gate"))?;
            let verdict = gate_job_scope(
                &executor,
                scope_caller_id(&job_id),
                Path::new(&worktree),
                GATE_WAIT,
            )
            .await;
            verdict_of(verdict)
        })
    });
    Some(gate)
}

fn verdict_of(verdict: ScopeVerdict) -> Result<GateVerdict, String> {
    match verdict {
        ScopeVerdict::Measured(GateOutcome::Passed) => Ok(GateVerdict::Green),
        ScopeVerdict::Measured(GateOutcome::Failed { output, .. }) => {
            Ok(GateVerdict::Red { summary: output })
        }
        ScopeVerdict::Measured(GateOutcome::Errored { reason }) => Err(reason),
        ScopeVerdict::NothingRan => {
            Err("no test unit agrees with the branch's diff, so nothing was measured".to_owned())
        }
        ScopeVerdict::Unavailable => {
            Err("the scope gate could not be submitted, so nothing was measured".to_owned())
        }
    }
}

/// Runs one pass over one job, then the parks it asked for. Errors are logged.
async fn advance(state: &AppState, job_id: &str, gate: Option<GateFn>) {
    let mut parks: Vec<Park> = Vec::new();
    let outcome = {
        let lock = job_lock(job_id);
        let _guard = lock.lock().await;
        pass(state, job_id, gate, &mut parks).await
    };
    if let Err(error) = outcome {
        tracing::warn!(job = job_id, %error, "tree driver failed to advance the job");
    }
    for (node, question) in parks {
        if let Err(error) = decisions::park(&state.pool, job_id, &node, &question).await {
            tracing::warn!(job = job_id, %node, %error, "tree driver could not park the branch");
        }
    }
}

async fn pass(
    state: &AppState,
    job_id: &str,
    gate: Option<GateFn>,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let pool = &state.pool;
    let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    if job.state == JOB_TERMINAL_STATE {
        return Ok(());
    }

    let names: Vec<String> = job.manifest.branches.keys().cloned().collect();
    for name in &names {
        if let Err(error) = branch_step(state, &mut job, name, gate.as_ref(), parks).await {
            tracing::warn!(job = job_id, branch = %name, %error, "tree driver failed on a branch");
        }
    }

    let closable: Vec<String> = job
        .manifest
        .branches
        .values()
        .filter(|branch| branch_node_may_close(branch))
        .filter_map(|branch| branch.node_id.clone())
        .collect();
    for node in closable {
        if let Err(error) = close_with_report(pool, &node).await {
            tracing::warn!(job = job_id, %node, %error, "tree driver could not close a node");
        }
    }

    landing_step(state, &mut job).await
}

// ---- branches ----------------------------------------------------------------------------------

async fn branch_step(
    state: &AppState,
    job: &mut TreeJob,
    name: &str,
    gate: Option<&GateFn>,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(());
    };
    match branch.state.as_str() {
        "building" | "fixing" | "reviewing" => at_work(state, job, name, gate).await,
        "red" => red(state, job, name, parks).await,
        "green" => merge(state, job, name, parks).await,
        _ => Ok(()),
    }
}

/// The report of a node's latest run.
struct NodeReport {
    run_id: Option<String>,
    live: bool,
    /// The result of the `report` transition keyed by the run, when there is one.
    result: Option<String>,
}

async fn node_report(pool: &SqlitePool, node_id: &str) -> Result<NodeReport, String> {
    let run: Option<Option<String>> = sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    let Some(run_id) = run.flatten() else {
        return Ok(NodeReport {
            run_id: None,
            live: false,
            result: None,
        });
    };
    let live = run_is_live(pool, &run_id).await.map_err(|e| e.to_string())?;
    let result: Option<String> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = 'report' AND key = ?",
    )
    .bind(node_id)
    .bind(&run_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(NodeReport {
        run_id: Some(run_id),
        live,
        result,
    })
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

async fn persist(pool: &SqlitePool, job: &mut TreeJob) -> Result<(), String> {
    job.updated_at = now();
    store::update(pool, job).await.map_err(|e| e.to_string())
}

/// A branch at work whose node reported `done` and whose run is over: a docs branch is done, a
/// `root` branch with no diff is dropped, anything else is gated.
async fn at_work(
    state: &AppState,
    job: &mut TreeJob,
    name: &str,
    gate: Option<&GateFn>,
) -> Result<(), String> {
    let pool = &state.pool;
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(());
    };
    let Some(node) = branch.node_id.clone() else {
        return Ok(());
    };
    let report = node_report(pool, &node).await?;
    if report.live || report.result.as_deref() != Some("done") {
        return Ok(());
    }

    if branch.layer == "docs" {
        if branch.state == "building" {
            let held = job.manifest.branches.get_mut(name).ok_or("branch vanished")?;
            apply_transition(held, "done").map_err(|e| e.to_string())?;
            persist(pool, job).await?;
        }
        return Ok(());
    }
    if name == "root" {
        let changed = has_diff(job, branch).await;
        match changed {
            Ok(false) => {
                let held = job.manifest.branches.get_mut(name).ok_or("branch vanished")?;
                apply_transition(held, "dropped").map_err(|e| e.to_string())?;
                held.extra.insert(
                    "drop_reason".to_owned(),
                    Value::String("the root changed nothing".to_owned()),
                );
                return persist(pool, job).await;
            }
            Ok(true) => {}
            Err(error) => {
                tracing::warn!(job = %job.id, %error, "could not tell whether the root branch changed anything");
            }
        }
    }
    if let Some(gate) = gate {
        spawn_gate(state, gate, &job.id, name);
    }
    Ok(())
}

/// Whether the branch holds a commit the feature branch does not. A worktree is checkpointed first:
/// an implement node leaves its work uncommitted.
async fn has_diff(job: &TreeJob, branch: &super::model::Branch) -> Result<bool, String> {
    if let Some(worktree) = &branch.worktree {
        crate::execution::worktree::checkpoint(Path::new(worktree))
            .await
            .map_err(|e| e.to_string())?;
    }
    let Some(git_branch) = &branch.git_branch else {
        return Ok(true);
    };
    let range = format!("{}..{git_branch}", job.manifest.feature_branch);
    let counted = git(&job.manifest.project_root, &["rev-list", "--count", &range]).await?;
    Ok(counted != "0")
}

/// Runs `git -C <root> <args>` and answers its trimmed stdout, or its stderr as the error.
async fn git(root: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

// ---- the gate task -----------------------------------------------------------------------------

/// The `(job, branch)` pairs whose gate is running in this process.
fn gating() -> &'static StdMutex<HashSet<(String, String)>> {
    static GATING: OnceLock<StdMutex<HashSet<(String, String)>>> = OnceLock::new();
    GATING.get_or_init(Default::default)
}

/// Frees the `(job, branch)` pair when the gate task ends, however it ends.
struct GateGuard((String, String));

impl Drop for GateGuard {
    fn drop(&mut self) {
        gating()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

/// Starts the gate of one branch in a task of its own, unless one is already running for it. The
/// task records the verdict under the job's lock; the tick does not wait for it.
fn spawn_gate(state: &AppState, gate: &GateFn, job_id: &str, branch: &str) {
    let key = (job_id.to_owned(), branch.to_owned());
    let fresh = gating()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.clone());
    if !fresh {
        return;
    }
    let guard = GateGuard(key);
    let future = gate(job_id.to_owned(), branch.to_owned());
    let state = state.clone();
    let (job_id, branch) = (job_id.to_owned(), branch.to_owned());
    tokio::spawn(async move {
        let _guard = guard;
        let verdict = future.await;
        record_verdict(&state, &job_id, &branch, verdict).await;
    });
}

async fn record_verdict(
    state: &AppState,
    job_id: &str,
    branch: &str,
    verdict: Result<GateVerdict, String>,
) {
    let scope = match verdict {
        Ok(GateVerdict::Green) => ScopeVerdict::Measured(GateOutcome::Passed),
        Ok(GateVerdict::Red { summary }) => ScopeVerdict::Measured(GateOutcome::Failed {
            exit_code: 1,
            output: summary,
        }),
        Err(reason) => {
            tracing::warn!(job = job_id, branch, %reason, "the gate measured nothing");
            return;
        }
    };
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    if let Err(error) = apply_verdict(&state.pool, job_id, branch, &scope).await {
        tracing::warn!(job = job_id, branch, %error, "could not record the gate verdict");
    }
}

async fn apply_verdict(
    pool: &SqlitePool,
    job_id: &str,
    branch: &str,
    scope: &ScopeVerdict,
) -> Result<(), String> {
    let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let Some(held) = job.manifest.branches.get_mut(branch) else {
        return Ok(());
    };
    // The job may have moved on (parked, dropped) while the gate ran: then the verdict is stale.
    if !matches!(held.state.as_str(), "building" | "fixing" | "reviewing") {
        return Ok(());
    }
    apply_gate_verdict(held, scope).map_err(|e| e.to_string())?;
    persist(pool, &mut job).await
}

// ---- red: fix, then park -----------------------------------------------------------------------

/// The findings of the last red gate of the branch.
fn last_findings(branch: &super::model::Branch) -> String {
    let text = branch
        .gates
        .iter()
        .rev()
        .find(|entry| entry.get("verdict").and_then(Value::as_str) == Some("red"))
        .and_then(|entry| entry.get("log_tail").and_then(Value::as_str))
        .unwrap_or("the gate failed without output");
    if text.len() <= FINDINGS_MAX {
        return text.to_owned();
    }
    let mut start = text.len() - FINDINGS_MAX;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

async fn red(
    state: &AppState,
    job: &mut TreeJob,
    name: &str,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let pool = &state.pool;
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(());
    };
    let Some(node) = branch.node_id.clone() else {
        return Ok(());
    };
    let findings = last_findings(branch);
    let report = node_report(pool, &node).await?;
    let Some(run) = report.run_id else {
        return Ok(());
    };

    if report.live {
        // The fix was started and the daemon went down before it wrote `fixing`.
        let held = job.manifest.branches.get_mut(name).ok_or("branch vanished")?;
        apply_transition(held, "fixing").map_err(|e| e.to_string())?;
        return persist(pool, job).await;
    }

    let key = format!("{FIX_PREFIX}{run}");
    // The key being replayed is not a prior link.
    let prior: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tree_transitions \
         WHERE node_id = ? AND transition = 'launch' AND key LIKE ? AND key <> ?",
    )
    .bind(&node)
    .bind(format!("{FIX_PREFIX}%"))
    .bind(&key)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if prior >= FIX_CAP {
        parks.push((
            node,
            format!(
                "The gate of branch {name} failed after {FIX_CAP} automatic fixes; last findings:\n{findings}"
            ),
        ));
        return Ok(());
    }

    let suffix = format!(
        "\n\nThe gate failed on your branch with these findings:\n{findings}\n\nFix them, then finish the node again."
    );
    relaunch_node(state, &node, &run, &key, Some(&suffix)).await?;
    let held = job.manifest.branches.get_mut(name).ok_or("branch vanished")?;
    apply_transition(held, "fixing").map_err(|e| e.to_string())?;
    persist(pool, job).await
}

// ---- green: merge ------------------------------------------------------------------------------

async fn repo_of(pool: &SqlitePool, job: &TreeJob) -> Result<ResolvedRepo, String> {
    let project = job
        .project_id
        .as_deref()
        .ok_or_else(|| format!("tree job {} has no project", job.id))?;
    resolve_repo(pool, project)
        .await
        .map_err(|error| format!("could not resolve the repository of project {project}: {error:?}"))
}

async fn merge(
    state: &AppState,
    job: &mut TreeJob,
    name: &str,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let pool = &state.pool;
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(());
    };
    let queued = branch.extra.contains_key("merge_request_id");
    let node = branch.node_id.clone();

    if queued {
        let settled = settle_merge(pool, job, name, Duration::ZERO)
            .await
            .map_err(|e| e.to_string())?;
        if let MergeSettlement::NotMerged { reason } = settled {
            tracing::warn!(job = %job.id, branch = name, %reason, "the queue did not merge the branch");
            // Resubmitting a conflicted branch would conflict again: an owner decides.
            if let Some(node) = node {
                parks.push((node, format!("The merge of branch {name} failed: {reason}")));
            }
        }
        return Ok(());
    }

    // Not mergeable yet (its base waits, a finding is open): nothing to do this pass.
    if check_may_merge(&job.manifest, name).is_err() {
        return Ok(());
    }
    let repo = repo_of(pool, job).await?;
    submit_merge(pool, &repo, job, name, Origin::Daemon)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---- closing nodes -----------------------------------------------------------------------------

/// Closes the node with the report its latest run left and the handoff file recorded for it, when it
/// is open, its run is over and both exist. `Ok(false)` when there was nothing to close.
async fn close_with_report(pool: &SqlitePool, node_id: &str) -> Result<bool, String> {
    let node = work_nodes::get(pool, node_id).await.map_err(|e| e.to_string())?;
    if node.closed_at.is_some() {
        return Ok(false);
    }
    let Some(report) = (node_report(pool, node_id).await?).result else {
        return Ok(false);
    };
    if node_report(pool, node_id).await?.live || !CLOSE_STATUSES.contains(&report.as_str()) {
        return Ok(false);
    }
    let Some(handoff) = node
        .handoff_path
        .filter(|path| Path::new(path).is_file())
    else {
        return Ok(false);
    };
    let args = CloseArgs {
        summary: format!("Closed by the tree driver with the node's {report} report."),
        status: report,
        handoff_path: handoff,
    };
    work_nodes::close(pool, node_id, &NodeLimits::default(), &args)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

// ---- landing -----------------------------------------------------------------------------------

/// The root is done when it is closed as `done`, or its run is over and reported `done`.
async fn root_is_done(pool: &SqlitePool, root_id: &str) -> Result<bool, String> {
    let node = work_nodes::get(pool, root_id).await.map_err(|e| e.to_string())?;
    if node.closed_at.is_some() {
        return Ok(node.status == "done");
    }
    let report = node_report(pool, root_id).await?;
    Ok(!report.live && report.result.as_deref() == Some("done"))
}

async fn landing_step(state: &AppState, job: &mut TreeJob) -> Result<(), String> {
    let pool = &state.pool;
    if job.manifest.extra.get("land_id").and_then(Value::as_i64).is_some() {
        let settled = settle_land(pool, job, Duration::ZERO)
            .await
            .map_err(|e| e.to_string())?;
        if let landing::LandSettlement::NotLanded { reason } = settled {
            tracing::warn!(job = %job.id, %reason, "the queue did not land the job");
        }
        return Ok(());
    }
    if !root_is_done(pool, &job.root_node_id).await? || check_may_land(&job.manifest).is_err() {
        return Ok(());
    }

    // The children closed earlier in the pass, so the root closes last.
    if let Err(error) = close_with_report(pool, &job.root_node_id).await {
        tracing::warn!(job = %job.id, %error, "tree driver could not close the root node");
    }

    let integration =
        job.manifest.extra.get("landing").and_then(Value::as_str) == Some("integration");
    if !integration {
        return finish_held(pool, job).await;
    }
    let repo = repo_of(pool, job).await?;
    match land_job(pool, &repo, job, Instant::now() + OPERATION_TIMEOUT, Origin::Shell).await {
        Ok(_) => Ok(()),
        Err(LandingError::LandRefused(reason)) => {
            // `land_job` recorded it as `land_refusal`; the job stays open.
            tracing::warn!(job = %job.id, %reason, "the landing was refused");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Ends a job that does not land: `finished`, with `landing = "held"`, the integration branch
/// untouched. `settle_land` is the only other writer of `finished`.
pub(super) async fn finish_held(pool: &SqlitePool, job: &mut TreeJob) -> Result<(), String> {
    job.state = JOB_TERMINAL_STATE.to_owned();
    job.manifest.state = JOB_TERMINAL_STATE.to_owned();
    job.manifest
        .extra
        .insert("landing".to_owned(), Value::String("held".to_owned()));
    persist(pool, job).await
}

#[cfg(test)]
mod tests {
    // The process-wide env guard is held across awaits on purpose, as in `landing::tests`.
    #![allow(clippy::await_holding_lock)]

    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::*;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use crate::execution::git_exec::tests::{WorktreeRootEnv, sha_of};
    use crate::execution::git_exec::{GitExecutor, OPERATION_TIMEOUT, repo_key};
    use crate::execution::vcs::{Origin, ResolvedRepo, drain_once};
    use crate::execution::worktree::test_env_lock;
    use crate::tree_job::decisions;
    use crate::tree_job::landing::{land_job, submit_merge};
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::node_run;
    use crate::tree_job::open::{self, StoredLaunch};
    use crate::tree_job::store::{self, TreeJob};
    use crate::work_nodes;

    const STAMP: &str = "2026-10-31T10:00:00+00:00";
    const PROJECT: &str = "alpha";
    /// Tables whose every write the repeated-tick test watches.
    const WATCHED: [&str; 8] = [
        "tree_jobs",
        "work_nodes",
        "tree_transitions",
        "tree_launches",
        "feed",
        "decisions",
        "vcs_requests",
        "runs",
    ];

    // ---- fixtures: git ---------------------------------------------------------------------------

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
        std::fs::write(repo.join(file), text).expect("write");
        assert!(git_in(repo, &["add", "-A"]));
        assert!(git_in(repo, &["commit", "-q", "-m", message]));
    }

    /// `master`; `feat/tree` one commit ahead of it; `tree/api`, `tree/web` and `tree/storage`, each
    /// one commit ahead of `feat/tree` in a file of its own so every merge is clean; `tree/root-empty`
    /// equal to `feat/tree` and `tree/root-full` one commit ahead. The main checkout stands on
    /// `feat/tree`, the target of a branch merge.
    fn tree_repo() -> (tempfile::TempDir, PathBuf) {
        let container = space_free_tempdir("nucleos-driver-repo-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        commit_file(&repo, "feature.txt", "from the tree\n", "feature");
        for name in ["api", "web", "storage"] {
            let git_branch = format!("tree/{name}");
            assert!(git_in(&repo, &["checkout", "-q", "-b", git_branch.as_str()]));
            commit_file(&repo, &format!("{name}.txt"), "from the branch\n", name);
            assert!(git_in(&repo, &["checkout", "-q", "feat/tree"]));
        }
        assert!(git_in(&repo, &["branch", "tree/root-empty", "feat/tree"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "tree/root-full"]));
        commit_file(&repo, "root.txt", "from the root\n", "root");
        assert!(git_in(&repo, &["checkout", "-q", "feat/tree"]));
        (container, repo)
    }

    // ---- fixtures: branches ----------------------------------------------------------------------

    fn code_branch(name: &str, state: &str, node: Option<&str>) -> Branch {
        let mut branch = Branch::new(name, &[format!("{name}/**")], "backend", None);
        branch.state = state.to_owned();
        branch.git_branch = Some(format!("tree/{name}"));
        branch.node_id = node.map(str::to_owned);
        branch
    }

    fn merged_branch(name: &str, node: Option<&str>) -> Branch {
        let mut branch = code_branch(name, "merged", node);
        branch
            .extra
            .insert("merge_sha".to_owned(), json!("0123456789abcdef"));
        branch
    }

    fn root_node_of(job: &str) -> String {
        format!("root-{job}")
    }

    fn red(summary: &str) -> GateVerdict {
        GateVerdict::Red {
            summary: summary.to_owned(),
        }
    }

    // ---- fixtures: the scripted gate -------------------------------------------------------------

    /// A gate that records its calls and answers from a script. With `release` set, every call waits
    /// on it first, which is how a test keeps a gate in progress.
    #[derive(Clone)]
    struct FakeGate {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        script: Arc<Mutex<VecDeque<GateVerdict>>>,
        release: Option<Arc<Notify>>,
    }

    impl FakeGate {
        fn answering(script: Vec<GateVerdict>) -> FakeGate {
            FakeGate {
                calls: Arc::default(),
                script: Arc::new(Mutex::new(script.into())),
                release: None,
            }
        }

        fn held(verdict: GateVerdict) -> (FakeGate, Arc<Notify>) {
            let release = Arc::new(Notify::new());
            let mut gate = FakeGate::answering(vec![verdict]);
            gate.release = Some(release.clone());
            (gate, release)
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }

        fn gate(&self) -> GateFn {
            let fake = self.clone();
            Arc::new(move |job: String, branch: String| -> GateFuture {
                let fake = fake.clone();
                Box::pin(async move {
                    fake.calls.lock().unwrap().push((job, branch));
                    if let Some(release) = &fake.release {
                        release.notified().await;
                    }
                    let next = fake.script.lock().unwrap().pop_front();
                    next.ok_or_else(|| "the gate script ran out".to_owned())
                })
            })
        }
    }

    async fn wait_calls(gate: &FakeGate, count: usize) {
        for _ in 0..300 {
            if gate.calls().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            gate.calls().len() >= count,
            "the gate was called {} time(s), expected {count}",
            gate.calls().len()
        );
    }

    // ---- fixtures: the world ---------------------------------------------------------------------

    /// A real git repository registered as project `alpha`, an app state over a one-connection pool
    /// and the worktree root the queue's integration worktrees go under.
    struct Fixture {
        state: AppState,
        repo: PathBuf,
        /// The queue's repository key: what `drain_once` claims by.
        key: String,
        /// Where the handoff files of the seeded reports live.
        files: tempfile::TempDir,
        next_run: Cell<i64>,
        _env: WorktreeRootEnv,
        _roots: tempfile::TempDir,
        _container: tempfile::TempDir,
        // Last, so it is released after everything that touches the shared env override.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Fixture {
        async fn new() -> Fixture {
            let lock = test_env_lock();
            let (container, repo) = tree_repo();
            let roots = space_free_tempdir("nucleos-driver-wt-");
            let env = WorktreeRootEnv::set(roots.path());
            let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
                Some(Duration::from_secs(60)),
                Duration::from_secs(600),
            )
            .await;
            runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
            sqlx::query(
                "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
                 VALUES (?, 'active', ?, 'master')",
            )
            .bind(PROJECT)
            .bind(repo.to_string_lossy().into_owned())
            .execute(&state.pool)
            .await
            .expect("seed the project's roster row");
            let key = repo_key(&repo, Instant::now() + OPERATION_TIMEOUT)
                .await
                .expect("the repository has a key");
            Fixture {
                state,
                repo,
                key,
                files: tempfile::tempdir().unwrap(),
                next_run: Cell::new(100),
                _env: env,
                _roots: roots,
                _container: container,
                _lock: lock,
            }
        }

        fn resolved(&self) -> ResolvedRepo {
            ResolvedRepo::synthetic(PROJECT, &self.repo.to_string_lossy(), &self.key)
        }

        /// Saves the job with its root node (running, no run yet). `landing` is the manifest's
        /// landing mode; `None` leaves it absent.
        async fn seed_job(&self, id: &str, branches: Vec<Branch>, landing: Option<&str>) {
            let mut manifest = Manifest::new(id, "feat/tree", "Ship it", &self.repo.to_string_lossy(), &[]);
            for branch in branches {
                manifest.branches.insert(branch.name.clone(), branch);
            }
            if let Some(mode) = landing {
                manifest.extra.insert("landing".to_owned(), json!(mode));
            }
            store::save(
                &self.state.pool,
                &TreeJob {
                    id: id.to_owned(),
                    root_node_id: root_node_of(id),
                    project_id: Some(PROJECT.to_owned()),
                    feature_branch: Some("feat/tree".to_owned()),
                    goal: "Ship it".to_owned(),
                    state: manifest.state.clone(),
                    manifest,
                    contract: None,
                    created_at: STAMP.to_owned(),
                    updated_at: STAMP.to_owned(),
                },
            )
            .await
            .unwrap();
            self.add_node(id, &root_node_of(id), "root").await;
        }

        async fn add_node(&self, job: &str, id: &str, kind: &str) {
            sqlx::query(
                "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                         tree_job_id, handoff_path, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, 'title', 'goal', 'running', NULL, ?, NULL, ?, ?)",
            )
            .bind(id)
            .bind(root_node_of(job))
            .bind(i64::from(kind != "root"))
            .bind(kind)
            .bind(job)
            .bind(STAMP)
            .bind(STAMP)
            .execute(&self.state.pool)
            .await
            .unwrap();
        }

        /// Gives the node a run that has ended, and with `report` the report row of that run and a
        /// handoff file, as `node_close` leaves them. Returns the run id.
        async fn run_ended(&self, node: &str, report: Option<&str>) -> i64 {
            self.run_with(node, "interrupted", report).await
        }

        async fn run_live(&self, node: &str) -> i64 {
            self.run_with(node, "running", None).await
        }

        async fn run_with(&self, node: &str, status: &str, report: Option<&str>) -> i64 {
            let run = self.next_run.get() + 1;
            self.next_run.set(run);
            let pool = &self.state.pool;
            sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'seeded', ?, ?)")
                .bind(run)
                .bind(status)
                .bind(STAMP)
                .execute(pool)
                .await
                .unwrap();
            sqlx::query("UPDATE work_nodes SET run_id = ? WHERE id = ?")
                .bind(run.to_string())
                .bind(node)
                .execute(pool)
                .await
                .unwrap();
            if let Some(result) = report {
                let file = self.files.path().join(format!("{node}.md"));
                std::fs::write(&file, "# handoff\n").expect("write the handoff");
                sqlx::query("UPDATE work_nodes SET handoff_path = ? WHERE id = ?")
                    .bind(file.to_string_lossy().into_owned())
                    .bind(node)
                    .execute(pool)
                    .await
                    .unwrap();
                node_run::apply(pool, node, "report", &run.to_string(), result)
                    .await
                    .unwrap();
            }
            run
        }

        async fn root_done(&self, job: &str) {
            self.run_ended(&root_node_of(job), Some("done")).await;
        }

        /// The node's agent, started again by the driver, ends and reports `done`.
        async fn relaunched_run_reports_done(&self, node: &str) {
            let run = self.current_run(node).await;
            sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?")
                .bind(run)
                .execute(&self.state.pool)
                .await
                .unwrap();
            node_run::apply(&self.state.pool, node, "report", &run.to_string(), "done")
                .await
                .unwrap();
        }

        async fn current_run(&self, node: &str) -> i64 {
            sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
                .bind(node)
                .fetch_one(&self.state.pool)
                .await
                .unwrap()
        }

        async fn prompt_of(&self, run: i64) -> String {
            sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
                .bind(run)
                .fetch_one(&self.state.pool)
                .await
                .unwrap()
        }

        async fn launches(&self, node: &str, prefix: &str) -> i64 {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM tree_transitions \
                 WHERE node_id = ? AND transition = 'launch' AND key LIKE ?",
            )
            .bind(node)
            .bind(format!("{prefix}%"))
            .fetch_one(&self.state.pool)
            .await
            .unwrap()
        }

        async fn save_launch(&self, node: &str) {
            open::save_launch(
                &self.state.pool,
                &StoredLaunch {
                    node_id: node.to_owned(),
                    key: "initial".to_owned(),
                    prompt: "build the api".to_owned(),
                    project_id: None,
                    cwd: None,
                    mode: "shadow".to_owned(),
                },
            )
            .await
            .unwrap();
        }

        async fn job(&self, id: &str) -> TreeJob {
            store::load(&self.state.pool, id)
                .await
                .unwrap()
                .expect("the job is stored")
        }

        async fn branch(&self, job: &str, name: &str) -> Branch {
            self.job(job)
                .await
                .manifest
                .branches
                .get(name)
                .cloned()
                .expect("the branch is in the manifest")
        }

        async fn set_branch_state(&self, job: &str, name: &str, state: &str) {
            let mut stored = self.job(job).await;
            stored
                .manifest
                .branches
                .get_mut(name)
                .expect("the branch is in the manifest")
                .state = state.to_owned();
            store::update(&self.state.pool, &stored).await.unwrap();
        }

        async fn node(&self, id: &str) -> work_nodes::Node {
            work_nodes::get(&self.state.pool, id).await.unwrap()
        }

        async fn open_jobs(&self) -> Vec<String> {
            store::list_non_terminal(&self.state.pool)
                .await
                .unwrap()
                .into_iter()
                .map(|job| job.id)
                .collect()
        }

        async fn requests(&self) -> i64 {
            sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
                .fetch_one(&self.state.pool)
                .await
                .unwrap()
        }

        async fn drain(&self) {
            assert!(
                drain_once(&self.state.pool, &self.key, &GitExecutor::default()).await,
                "the queue had a request to run"
            );
        }

        async fn tick_within(&self, job: &str, gate: &FakeGate, limit: Duration) {
            tokio::time::timeout(limit, advance_job_with_gate(&self.state, job, gate.gate()))
                .await
                .expect("the tick returned in time");
        }

        async fn tick(&self, job: &str, gate: &FakeGate) {
            self.tick_within(job, gate, Duration::from_secs(30)).await;
        }

        /// Waits for the spawned gate to settle the branch, then lets its task finish.
        async fn wait_branch_state(&self, job: &str, name: &str, want: &str) {
            let mut seen = String::new();
            for _ in 0..300 {
                seen = self.branch(job, name).await.state;
                if seen == want {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(seen, want, "branch {name} of {job} never reached {want}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        /// From now on every write to a watched table leaves a line in `write_log`.
        async fn watch_writes(&self) {
            sqlx::query("CREATE TABLE write_log (what TEXT NOT NULL)")
                .execute(&self.state.pool)
                .await
                .unwrap();
            for table in WATCHED {
                for event in ["INSERT", "UPDATE", "DELETE"] {
                    let sql = format!(
                        "CREATE TRIGGER log_{event}_{table} AFTER {event} ON {table} \
                         BEGIN INSERT INTO write_log (what) VALUES ('{event} {table}'); END"
                    );
                    sqlx::query(sqlx::AssertSqlSafe(sql))
                        .execute(&self.state.pool)
                        .await
                        .unwrap();
                }
            }
        }

        async fn writes(&self) -> Vec<String> {
            sqlx::query_scalar("SELECT what FROM write_log ORDER BY rowid")
                .fetch_all(&self.state.pool)
                .await
                .unwrap()
        }

        async fn assert_not_landing(&self, job: &str) {
            let stored = self.job(job).await;
            assert!(!stored.manifest.extra.contains_key("land_id"), "no landing admitted");
            assert_eq!(stored.state, "open");
            assert_eq!(self.requests().await, 0, "nothing was queued");
        }
    }

    // ---- the merge path --------------------------------------------------------------------------

    #[tokio::test]
    async fn a_green_branch_is_submitted_then_settled_into_the_feature_branch() {
        const JOB: &str = "drv-merge";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "green", Some("api-node"))], None)
            .await;
        fx.add_node(JOB, "api-node", "orchestrator").await;
        fx.run_ended("api-node", Some("done")).await;
        let gate = FakeGate::answering(vec![]);
        let tip = sha_of(&fx.repo, "tree/api");
        let master = sha_of(&fx.repo, "master");
        let feature_before = sha_of(&fx.repo, "feat/tree");

        fx.tick(JOB, &gate).await;

        assert_eq!(fx.requests().await, 1, "the merge is queued");
        let queued = fx.branch(JOB, "api").await;
        assert_eq!(queued.state, "green", "a queued merge is not a merge yet");
        let request_id = queued
            .extra
            .get("merge_request_id")
            .and_then(Value::as_i64)
            .expect("the request id is stored on the branch");
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM vcs_requests")
            .fetch_all(&fx.state.pool)
            .await
            .unwrap();
        assert_eq!(ids, [request_id]);
        assert_eq!(sha_of(&fx.repo, "feat/tree"), feature_before);
        assert!(gate.calls().is_empty(), "a green branch is not gated again");

        fx.drain().await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        let merged = fx.branch(JOB, "api").await;
        assert_eq!(merged.state, "merged");
        let sha = merged.extra["merge_sha"].as_str().expect("the merge sha");
        assert_eq!(sha, sha_of(&fx.repo, "feat/tree"));
        assert_eq!(sha_of(&fx.repo, &format!("{sha}^2")), tip);
        assert_eq!(sha_of(&fx.repo, "master"), master, "only the feature branch moved");
        assert!(!merged.extra.contains_key("merge_request_id"));
        assert_eq!(fx.requests().await, 1, "settling queued nothing");
        let node = fx.node("api-node").await;
        assert_eq!(node.status, "done", "the merged branch's node is closed with its report");
        assert!(node.closed_at.is_some());
    }

    #[tokio::test]
    async fn a_branch_with_a_done_report_is_gated() {
        const JOB: &str = "drv-gate";
        let fx = Fixture::new().await;
        fx.seed_job(
            JOB,
            vec![
                code_branch("api", "building", Some("api-node")),
                code_branch("web", "building", Some("web-node")),
            ],
            None,
        )
        .await;
        fx.add_node(JOB, "api-node", "orchestrator").await;
        fx.add_node(JOB, "web-node", "orchestrator").await;
        fx.run_ended("api-node", Some("done")).await;
        fx.run_live("web-node").await;
        let gate = FakeGate::answering(vec![GateVerdict::Green]);

        fx.tick(JOB, &gate).await;

        wait_calls(&gate, 1).await;
        assert_eq!(gate.calls(), vec![(JOB.to_owned(), "api".to_owned())]);
        fx.wait_branch_state(JOB, "api", "green").await;
        let api = fx.branch(JOB, "api").await;
        assert_eq!(api.gates.len(), 1, "the verdict is recorded on the branch");
        assert_eq!(api.gates[0]["verdict"], json!("green"));
        let web = fx.branch(JOB, "web").await;
        assert_eq!(web.state, "building", "a branch still at work is not gated");
        assert_eq!(gate.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_red_gate_relaunches_with_the_findings_twice_and_then_parks() {
        const JOB: &str = "drv-fix";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "building", Some(NODE))], None)
            .await;
        fx.add_node(JOB, NODE, "orchestrator").await;
        fx.run_ended(NODE, Some("done")).await;
        fx.save_launch(NODE).await;
        let gate = FakeGate::answering(vec![
            red("first failure: boom-1"),
            red("second failure: boom-2"),
            red("third failure: boom-3"),
        ]);

        for (attempt, finding) in [(1, "boom-1"), (2, "boom-2")] {
            fx.tick(JOB, &gate).await;
            wait_calls(&gate, attempt).await;
            fx.wait_branch_state(JOB, "api", "red").await;

            fx.tick(JOB, &gate).await;

            assert_eq!(fx.branch(JOB, "api").await.state, "fixing");
            assert_eq!(fx.launches(NODE, "fix-").await, attempt as i64);
            let prompt = fx.prompt_of(fx.current_run(NODE).await).await;
            assert!(prompt.contains(finding), "the relaunch carries the findings: {prompt:?}");
            fx.relaunched_run_reports_done(NODE).await;
        }

        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 3).await;
        fx.wait_branch_state(JOB, "api", "red").await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        assert_eq!(fx.launches(NODE, "fix-").await, 2, "the cap is two relaunches");
        assert_eq!(gate.calls().len(), 3);
        let open: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM decisions WHERE tree_id = ? AND node_id = ? AND status = ?",
        )
        .bind(JOB)
        .bind(NODE)
        .bind(decisions::STATUS_OPEN)
        .fetch_one(&fx.state.pool)
        .await
        .unwrap();
        assert_eq!(open, 1, "parking opens one decision");
        let question: String = sqlx::query_scalar("SELECT question FROM decisions WHERE node_id = ?")
            .bind(NODE)
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();
        assert!(question.contains("boom-3"), "{question:?}");
    }

    #[tokio::test]
    async fn a_repeated_tick_queues_no_second_merge_and_writes_nothing() {
        const JOB: &str = "drv-replay";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "green", None)], None).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        assert_eq!(fx.requests().await, 1);
        let before = fx.job(JOB).await;
        fx.watch_writes().await;

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.writes().await, Vec::<String>::new(), "a quiet tick writes nothing");
        assert_eq!(fx.requests().await, 1, "no second merge was queued");
        assert_eq!(fx.job(JOB).await, before);
        assert!(gate.calls().is_empty());
    }

    #[tokio::test]
    async fn a_dependent_branch_waits_for_its_base_to_merge() {
        const JOB: &str = "drv-base";
        let fx = Fixture::new().await;
        let mut dependent = code_branch("api", "green", None);
        dependent.base = Some("storage".to_owned());
        // `api` sorts before `storage`, so a driver that walks the branches in name order meets the
        // dependent first.
        fx.seed_job(JOB, vec![dependent, code_branch("storage", "green", None)], None)
            .await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.requests().await, 1, "only the base is queued");
        let args: String = sqlx::query_scalar("SELECT args FROM vcs_requests")
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();
        assert!(args.contains("tree/storage"), "{args}");
        assert!(!fx.branch(JOB, "storage").await.extra.contains_key("merge_sha"));
        let waiting = fx.branch(JOB, "api").await;
        assert_eq!(waiting.state, "green");
        assert!(!waiting.extra.contains_key("merge_request_id"));

        fx.drain().await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "storage").await.state, "merged");
        assert_eq!(fx.requests().await, 2, "the dependent is queued once its base is merged");
        fx.drain().await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "merged");
        assert!(git_in(&fx.repo, &["cat-file", "-e", "feat/tree:storage.txt"]));
        assert!(git_in(&fx.repo, &["cat-file", "-e", "feat/tree:api.txt"]));
    }

    // ---- landing ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn the_job_lands_only_when_the_root_is_done_and_every_branch_is_merged() {
        const JOB: &str = "drv-land-gate";
        let fx = Fixture::new().await;
        fx.seed_job(
            JOB,
            vec![merged_branch("api", None), code_branch("web", "dropped", None)],
            Some("integration"),
        )
        .await;
        let gate = FakeGate::answering(vec![]);

        // Every branch is finished, but the root has not reported.
        fx.tick(JOB, &gate).await;
        fx.assert_not_landing(JOB).await;

        // The root is done, but a branch is parked.
        fx.root_done(JOB).await;
        fx.set_branch_state(JOB, "web", "parked").await;
        fx.tick(JOB, &gate).await;
        fx.assert_not_landing(JOB).await;

        // Both hold.
        fx.set_branch_state(JOB, "web", "dropped").await;
        fx.tick(JOB, &gate).await;

        let landing = fx.job(JOB).await;
        assert!(landing.manifest.extra.get("land_id").and_then(Value::as_i64).is_some());
        assert_eq!(fx.requests().await, 1);
        assert_eq!(landing.state, "open", "admitted is not landed");
    }

    #[tokio::test]
    async fn a_held_job_finishes_without_moving_the_integration_branch() {
        let fx = Fixture::new().await;
        let master = sha_of(&fx.repo, "master");
        let gate = FakeGate::answering(vec![]);

        // An absent mode is a hold, and so is the explicit one.
        for (job, landing) in [("drv-held-default", None), ("drv-held-hold", Some("hold"))] {
            fx.seed_job(job, vec![merged_branch("api", None)], landing).await;
            fx.root_done(job).await;

            fx.tick(job, &gate).await;

            let held = fx.job(job).await;
            assert_eq!(held.state, "finished", "{job}");
            assert_eq!(held.manifest.state, "finished", "{job}");
            assert_eq!(held.manifest.extra["landing"], json!("held"), "{job}");
            assert!(!fx.open_jobs().await.contains(&job.to_owned()), "{job}");
            let root = fx.node(&root_node_of(job)).await;
            assert_eq!(root.status, "done", "{job}: the root closes with its report");
            assert!(root.closed_at.is_some());
        }

        assert_eq!(fx.requests().await, 0, "a held job queues nothing");
        assert_eq!(sha_of(&fx.repo, "master"), master);
    }

    #[tokio::test]
    async fn an_integration_job_lands_into_the_integration_branch_and_finishes() {
        const JOB: &str = "drv-integration";
        let fx = Fixture::new().await;
        assert!(git_in(&fx.repo, &["checkout", "-q", "master"]));
        let master = sha_of(&fx.repo, "master");
        let feature = sha_of(&fx.repo, "feat/tree");
        fx.seed_job(
            JOB,
            vec![merged_branch("api", Some("api-node"))],
            Some("integration"),
        )
        .await;
        fx.add_node(JOB, "api-node", "orchestrator").await;
        fx.run_ended("api-node", Some("done")).await;
        fx.root_done(JOB).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;

        let admitted = fx.job(JOB).await;
        assert!(admitted.manifest.extra.get("land_id").and_then(Value::as_i64).is_some());
        assert_eq!(admitted.state, "open", "admitted is not landed");
        assert_eq!(fx.requests().await, 1);
        assert_eq!(sha_of(&fx.repo, "master"), master, "the queue has not run yet");
        let child = fx.node("api-node").await;
        let root = fx.node(&root_node_of(JOB)).await;
        assert_eq!((child.status.as_str(), root.status.as_str()), ("done", "done"));
        assert!(
            child.closed_at.expect("the child closed") <= root.closed_at.expect("the root closed"),
            "children close before the root"
        );

        fx.drain().await;
        fx.tick(JOB, &gate).await;

        let landed = fx.job(JOB).await;
        assert_eq!(landed.state, "finished");
        assert_eq!(landed.manifest.state, "finished");
        assert_ne!(landed.manifest.extra.get("landing"), Some(&json!("held")));
        assert_eq!(sha_of(&fx.repo, "master^2"), feature);
        assert!(!fx.open_jobs().await.contains(&JOB.to_owned()));
        assert_eq!(fx.requests().await, 1, "settling queued nothing");
    }

    #[tokio::test]
    async fn a_land_refusal_is_recorded_and_the_job_stays_open() {
        const JOB: &str = "drv-refusal";
        let fx = Fixture::new().await;
        // `feat/tree` is `master`, so the landing is refused as nothing to land.
        assert!(git_in(&fx.repo, &["checkout", "-q", "master"]));
        assert!(git_in(&fx.repo, &["branch", "-f", "feat/tree", "master"]));
        fx.seed_job(JOB, vec![merged_branch("api", None)], Some("integration"))
            .await;
        fx.root_done(JOB).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        let stored = fx.job(JOB).await;
        let refusal = stored.manifest.extra["land_refusal"]
            .as_str()
            .expect("the refusal is recorded");
        assert!(refusal.contains("nothing to land"), "{refusal}");
        assert!(!stored.manifest.extra.contains_key("land_id"));
        assert_eq!(stored.state, "open");
        assert_eq!(stored.manifest.state, "open");
        assert_eq!(fx.requests().await, 0);
        assert!(fx.open_jobs().await.contains(&JOB.to_owned()));
    }

    // ---- restart, connections, gates -------------------------------------------------------------

    #[tokio::test]
    async fn a_tick_on_a_restarted_daemon_resumes_from_the_stored_request_ids() {
        const MERGING: &str = "drv-resume-merge";
        const LANDING: &str = "drv-resume-land";
        let fx = Fixture::new().await;
        let pool = &fx.state.pool;
        let resolved = fx.resolved();
        let gate = FakeGate::answering(vec![]);

        // The daemon that ran before queued a merge and went down before it saw the outcome.
        fx.seed_job(MERGING, vec![code_branch("api", "green", None)], None).await;
        let mut stale = fx.job(MERGING).await;
        submit_merge(pool, &resolved, &mut stale, "api", Origin::Daemon)
            .await
            .unwrap();
        fx.drain().await;
        assert_eq!(fx.requests().await, 1);

        fx.tick(MERGING, &gate).await;

        let merged = fx.branch(MERGING, "api").await;
        assert_eq!(merged.state, "merged", "the stored request id is settled");
        assert_eq!(merged.extra["merge_sha"], json!(sha_of(&fx.repo, "feat/tree")));
        assert_eq!(fx.requests().await, 1, "no second merge was queued");

        // And one admitted a landing and went down before it saw that outcome.
        assert!(git_in(&fx.repo, &["checkout", "-q", "master"]));
        let feature = sha_of(&fx.repo, "feat/tree");
        fx.seed_job(LANDING, vec![merged_branch("api", None)], Some("integration"))
            .await;
        fx.root_done(LANDING).await;
        let mut stale = fx.job(LANDING).await;
        land_job(pool, &resolved, &mut stale, Instant::now() + OPERATION_TIMEOUT, Origin::Daemon)
            .await
            .unwrap();
        fx.drain().await;
        assert_eq!(fx.requests().await, 2);

        fx.tick(LANDING, &gate).await;

        assert_eq!(fx.job(LANDING).await.state, "finished", "the stored land id is settled");
        assert_eq!(sha_of(&fx.repo, "master^2"), feature);
        assert_eq!(fx.requests().await, 2, "no second landing was queued");
    }

    #[tokio::test]
    async fn the_tick_holds_no_connection_while_calling_the_queue() {
        const JOB: &str = "drv-pool";
        let fx = Fixture::new().await;
        // The pool has one connection, so a queue call that needs one while the tick holds it never
        // returns; the timeout turns that into a failure.
        fx.seed_job(JOB, vec![code_branch("api", "green", None)], Some("integration"))
            .await;
        fx.root_done(JOB).await;
        let gate = FakeGate::answering(vec![]);
        let limit = Duration::from_secs(20);
        // A dropped connection goes back to the pool on a spawned task, so `num_idle` read right
        // after the tick is racy: poll for up to a second.
        let idle = || async {
            for _ in 0..50 {
                if fx.state.pool.size() as usize == fx.state.pool.num_idle() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            false
        };

        fx.tick_within(JOB, &gate, limit).await; // submit the merge
        assert!(idle().await, "the tick released its connection");
        assert_eq!(fx.requests().await, 1);
        fx.drain().await;
        fx.tick_within(JOB, &gate, limit).await; // settle the merge
        fx.tick_within(JOB, &gate, limit).await; // admit the landing
        assert!(idle().await, "the tick released its connection");

        assert_eq!(fx.branch(JOB, "api").await.state, "merged");
        let landing = fx.job(JOB).await;
        assert!(landing.manifest.extra.get("land_id").and_then(Value::as_i64).is_some());
        assert_eq!(fx.requests().await, 2);
    }

    #[tokio::test]
    async fn a_tick_does_not_wait_for_a_gate_in_progress() {
        const JOB: &str = "drv-slow-gate";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "building", Some("api-node"))], None)
            .await;
        fx.add_node(JOB, "api-node", "orchestrator").await;
        fx.run_ended("api-node", Some("done")).await;
        let (gate, release) = FakeGate::held(GateVerdict::Green);
        let short = Duration::from_secs(2);

        // The gate cannot finish, yet the tick returns.
        fx.tick_within(JOB, &gate, short).await;
        wait_calls(&gate, 1).await;
        fx.tick_within(JOB, &gate, short).await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(gate.calls().len(), 1, "a gate in progress is not started a second time");
        assert_eq!(fx.branch(JOB, "api").await.state, "building");
        assert_eq!(fx.requests().await, 0);

        release.notify_one();

        fx.wait_branch_state(JOB, "api", "green").await;
        assert_eq!(gate.calls().len(), 1);
        assert_eq!(fx.branch(JOB, "api").await.gates.len(), 1);
    }

    #[tokio::test]
    async fn a_root_branch_with_no_diff_is_dropped() {
        const EMPTY: &str = "drv-root-empty";
        const FULL: &str = "drv-root-full";
        let fx = Fixture::new().await;
        let mut empty = code_branch("root", "building", Some("empty-node"));
        empty.git_branch = Some("tree/root-empty".to_owned());
        let mut full = code_branch("root", "building", Some("full-node"));
        full.git_branch = Some("tree/root-full".to_owned());
        fx.seed_job(EMPTY, vec![empty], None).await;
        fx.seed_job(FULL, vec![full], None).await;
        for (job, node) in [(EMPTY, "empty-node"), (FULL, "full-node")] {
            fx.add_node(job, node, "orchestrator").await;
            fx.run_ended(node, Some("done")).await;
        }
        let gate = FakeGate::answering(vec![GateVerdict::Green]);

        fx.tick(EMPTY, &gate).await;

        assert_eq!(fx.branch(EMPTY, "root").await.state, "dropped");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(gate.calls().is_empty(), "an empty branch is not gated");
        assert_eq!(fx.requests().await, 0, "an empty branch is not merged");

        // A root branch that did change something is gated like any other.
        fx.tick(FULL, &gate).await;
        wait_calls(&gate, 1).await;
        assert_eq!(gate.calls(), vec![(FULL.to_owned(), "root".to_owned())]);
        fx.wait_branch_state(FULL, "root", "green").await;
        assert_ne!(fx.branch(FULL, "root").await.state, "dropped");
    }
}
