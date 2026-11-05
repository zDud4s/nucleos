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
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::decisions;
use super::engine::{apply_transition, branch_node_may_close};
use super::http::job_lock;
use super::landing::{
    self, LandingError, MergeSettlement, apply_gate_verdict, check_may_land, check_may_merge,
    land_job, settle_land, settle_merge, submit_merge,
};
use super::model::{JOB_TERMINAL_STATE, Manifest};
use super::open::{FIX_PREFIX, relaunch_node, run_is_live};
use super::relaunch::{budget_refusal, refusal_question};
use super::store::{self, TreeJob};
use crate::execution::git_exec::OPERATION_TIMEOUT;
use crate::execution::vcs::{Origin, ResolvedRepo, resolve_repo};
use crate::gate::GateOutcome;
use crate::state::AppState;
use crate::tree_cost::budget::{self as tree_budget, Permit};
use crate::verify::ScopeVerdict;
use crate::work_nodes::{self, CLOSE_STATUSES, CloseArgs, NodeLimits};

/// How long the production gate waits for the verify queue.
const GATE_WAIT: Duration = Duration::from_secs(900);
/// Cap of the relaunches that follow a red gate (key prefix `FIX_PREFIX`).
const FIX_CAP: i64 = 2;
/// Merge requests of one branch that the conflict resolver is given before the owner is asked.
const CONFLICT_ATTEMPTS: i64 = 2;
/// How long a conflict may wait for a resolver run before the owner is asked instead.
const RESOLVER_GRACE: Duration = Duration::from_secs(15 * 60);
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
#[cfg(test)]
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
            super::gate::gate_branch(&executor, &pool, &job_id, &branch, &worktree, GATE_WAIT).await
        })
    });
    Some(gate)
}

pub(super) fn verdict_of(verdict: ScopeVerdict) -> Result<GateVerdict, String> {
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
        if let Err(error) = decisions::park_or_ask(&state.pool, job_id, &node, &question).await {
            tracing::warn!(job = job_id, %node, %error, "tree driver could not park the branch");
        }
    }
    // Outside the job's lock: the start goes through the routes that take it.
    if let Err(error) = retry_pending_starts(state, job_id).await {
        tracing::warn!(job = job_id, %error, "tree driver could not retry the pending starts");
    }
}

// ---- pending starts: a start the disk refused ----------------------------------------------------

/// Replays each start that `branch_start` stored because the disk alone refused it, once the disk
/// admits the branch. Still short: nothing is written or announced. Refused for another reason now,
/// or the caller is gone: the record is dropped and the decision stays open for the owner.
async fn retry_pending_starts(state: &AppState, job_id: &str) -> Result<(), String> {
    let Some(job) = store::load(&state.pool, job_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if job.state == JOB_TERMINAL_STATE {
        return Ok(());
    }
    let Some(pending) = job
        .manifest
        .extra
        .get("pending_starts")
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    for (name, record) in pending {
        if let Err(error) = retry_pending_start(state, &job, name, record).await {
            tracing::warn!(job = job_id, branch = %name, %error, "a pending start could not be retried");
        }
    }
    Ok(())
}

async fn retry_pending_start(
    state: &AppState,
    job: &TreeJob,
    name: &str,
    record: &Value,
) -> Result<(), String> {
    let pool = &state.pool;
    let caller = record["caller"].as_str().unwrap_or_default();
    if work_nodes::get(pool, caller).await.is_err() {
        return drop_pending_start(pool, &job.id, name).await;
    }
    // A branch already added by an earlier, half-finished replay needs no admission: the replay
    // only finishes its remaining steps.
    if !job.manifest.branches.contains_key(name) {
        let layer = record["layer"].as_str().unwrap_or("code");
        let admission = super::http::current_admission(&job.manifest, layer).await?;
        if !admission.admit {
            if admission.disk_only {
                // A record a crash left without its decision asks now, once.
                if record["decision_id"].as_str().is_none() {
                    super::branch_start::heal_pending_decision(state, &job.id, name).await?;
                }
                return Ok(());
            }
            return drop_pending_start(pool, &job.id, name).await;
        }
    }
    super::branch_start::start_pending(state, &job.id, name, record).await?;

    // Start, then answer, then drop the record: a crash between any two repeats safely. A record
    // with no id may still have a decision (a crash between the ask and the id): the id is fixed.
    let generation = record["generation"].as_u64().unwrap_or(1);
    let fallback = super::branch_start::pending_decision_id(&job.id, name, generation);
    let decision = record["decision_id"].as_str().unwrap_or(&fallback);
    let text = format!("disk freed: branch {name} admitted automatically");
    match decisions::answer(pool, decision, &text, "driver").await {
        // The owner answered first: their answer stays. No row: nothing was ever asked.
        Ok(_)
        | Err(
            decisions::DecisionError::AlreadyDecided(_) | decisions::DecisionError::NotFound(_),
        ) => {}
        Err(error) => {
            tracing::warn!(job = %job.id, decision, %error, "could not answer the pending start's decision");
        }
    }
    drop_pending_start(pool, &job.id, name).await?;
    let line = format!(
        "{}: branch {name} admitted automatically, the disk is freed",
        job.goal
    );
    if let Err(error) = crate::feed::append(
        pool,
        job.project_id.as_deref(),
        "work_tree_pending",
        &line,
        None,
        None,
    )
    .await
    {
        tracing::warn!(job = %job.id, %error, "could not announce an admitted pending start");
    }
    Ok(())
}

/// Removes the pending record of `name` under the job's lock, from a fresh read of the manifest.
async fn drop_pending_start(pool: &SqlitePool, job_id: &str, name: &str) -> Result<(), String> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let Some(pending) = job
        .manifest
        .extra
        .get_mut("pending_starts")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    if pending.remove(name).is_none() {
        return Ok(());
    }
    if pending.is_empty() {
        job.manifest.extra.remove("pending_starts");
    }
    // The record is consumed: the next refusal of a branch is a new question. Same write as the
    // drop, so a crash cannot leave one without the other.
    let next = super::branch_start::current_generation(&job.manifest.extra) + 1;
    job.manifest
        .extra
        .insert(super::branch_start::GENERATION_KEY.to_owned(), json!(next));
    persist(pool, &mut job).await
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

    root_step(pool, &job, parks).await?;
    landing_step(state, &mut job).await
}

/// A root whose run is over and reported `blocked` or `failed` has nobody else to ask: the owner is
/// asked on the root itself. `park_or_ask` asks once while the decision is open, and the job stays
/// unfinished and unlanded because the root is not `done`.
async fn root_step(pool: &SqlitePool, job: &TreeJob, parks: &mut Vec<Park>) -> Result<(), String> {
    let root = &job.root_node_id;
    // A root with no node row has no run to report; `landing_step` is where that is an error.
    let Ok(node) = work_nodes::get(pool, root).await else {
        return Ok(());
    };
    if node.closed_at.is_some() {
        return Ok(());
    }
    let report = node_report(pool, root).await?;
    if let (false, Some(outcome @ ("blocked" | "failed"))) = (report.live, report.result.as_deref())
    {
        let question = stopped_question(root, None, outcome)
            + &super::gate_summary::reason_suffix(pool, root).await;
        parks.push((root.clone(), question));
    }
    Ok(())
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
        "building" | "fixing" | "reviewing" => at_work(state, job, name, gate, parks).await,
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
    let run: Option<Option<String>> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
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
    let live = run_is_live(pool, &run_id)
        .await
        .map_err(|e| e.to_string())?;
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

/// A branch at work whose node's run is over. Reported `done`: a docs branch is done, a `root`
/// branch with no diff is dropped, anything else is gated. Reported `blocked` or `failed`: the
/// branch is parked on a question for the owner. The report belongs to the run, so the new run an
/// answer starts is not parked again by the old report.
async fn at_work(
    state: &AppState,
    job: &mut TreeJob,
    name: &str,
    gate: Option<&GateFn>,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let pool = &state.pool;
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(());
    };
    let Some(node) = branch.node_id.clone() else {
        return Ok(());
    };
    let report = node_report(pool, &node).await?;
    if report.live {
        return Ok(());
    }
    match report.result.as_deref() {
        Some("done") => {}
        Some(outcome @ ("blocked" | "failed")) => {
            let question = stopped_question(&node, Some(name), outcome)
                + &super::gate_summary::reason_suffix(pool, &node).await;
            parks.push((node.clone(), question));
            return Ok(());
        }
        _ => return Ok(()),
    }

    if branch.layer == "docs" {
        if branch.state == "building" {
            let held = job
                .manifest
                .branches
                .get_mut(name)
                .ok_or("branch vanished")?;
            apply_transition(held, "done").map_err(|e| e.to_string())?;
            persist(pool, job).await?;
        }
        return Ok(());
    }
    if name == "root" {
        let changed = has_diff(job, branch).await;
        match changed {
            Ok(false) => {
                let held = job
                    .manifest
                    .branches
                    .get_mut(name)
                    .ok_or("branch vanished")?;
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

/// The question for a node whose run ended `blocked` or `failed`; `branch` is `None` for the root.
fn stopped_question(node: &str, branch: Option<&str>, outcome: &str) -> String {
    let whose = match branch {
        Some(branch) => format!("node {node} of branch {branch}"),
        None => format!("the root node {node}"),
    };
    format!("{whose} reported {outcome}; how should it go on?")
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
    let entry = branch
        .gates
        .iter()
        .rev()
        .find(|entry| entry.get("verdict").and_then(Value::as_str) == Some("red"));
    let text = entry
        .and_then(|entry| entry.get("log_tail").and_then(Value::as_str))
        .unwrap_or("the gate failed without output");
    let failures = entry
        .and_then(|entry| entry.get("failures").and_then(Value::as_str))
        .unwrap_or("");
    super::gate_summary::findings(failures, text, FINDINGS_MAX)
}

/// True when the gate findings name a failed formatter step (`FAIL core: fmt`, `  core: fmt`).
fn findings_show_fmt_failure(findings: &str) -> bool {
    findings.lines().any(|line| line.trim().ends_with(": fmt"))
}

/// The prompt suffix of a gate-fix relaunch; a formatter failure gets the command that fixes it,
/// because the node rules otherwise forbid every cargo command.
fn fix_suffix(findings: &str) -> String {
    let mut suffix =
        format!("\n\nThe gate failed on your branch with these findings:\n{findings}\n\n");
    if findings_show_fmt_failure(findings) {
        suffix.push_str(
            "The formatter failed: run `cargo fmt -p nucleos-core` (or the crate of the files you changed) from the worktree root \u{2014} do not format by hand and do not run bare `rustfmt` \u{2014} then finish the node again.",
        );
    } else {
        suffix.push_str("Fix them, then finish the node again.");
    }
    suffix
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
        let held = job
            .manifest
            .branches
            .get_mut(name)
            .ok_or("branch vanished")?;
        apply_transition(held, "fixing").map_err(|e| e.to_string())?;
        return persist(pool, job).await;
    }

    let key = format!("{FIX_PREFIX}{run}");
    // The key being replayed is not a prior link. Only fixes after the owner's last answer
    // (`decision-<id>` launch) count: an answer is a fresh allowance.
    let prior: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tree_launches \
         WHERE node_id = ? AND key LIKE ? AND key <> ? \
           AND seq > COALESCE((SELECT MAX(seq) FROM tree_launches \
                               WHERE node_id = ? AND key LIKE 'decision-%'), 0)",
    )
    .bind(&node)
    .bind(format!("{FIX_PREFIX}%"))
    .bind(&key)
    .bind(&node)
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

    if let Some(reason) = budget_refusal(pool, &node).await {
        // A fix run is paid for: past the tree's limit the owner is asked, and the run id stays.
        parks.push((node.clone(), refusal_question(&node, &reason)));
        return Ok(());
    }
    let suffix = fix_suffix(&findings);
    relaunch_node(state, &node, &run, &key, Some(&suffix)).await?;
    let held = job
        .manifest
        .branches
        .get_mut(name)
        .ok_or("branch vanished")?;
    apply_transition(held, "fixing").map_err(|e| e.to_string())?;
    persist(pool, job).await
}

// ---- green: merge ------------------------------------------------------------------------------

async fn repo_of(pool: &SqlitePool, job: &TreeJob) -> Result<ResolvedRepo, String> {
    let project = job
        .project_id
        .as_deref()
        .ok_or_else(|| format!("tree job {} has no project", job.id))?;
    resolve_repo(pool, project).await.map_err(|error| {
        format!("could not resolve the repository of project {project}: {error:?}")
    })
}

/// How many merge requests of the branch the conflict resolver was given so far.
fn conflict_attempts(job: &TreeJob, name: &str) -> i64 {
    job.manifest
        .branches
        .get(name)
        .and_then(|branch| branch.extra.get("conflict_attempts"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// Stamps `conflict_since` (RFC 3339) on a branch whose conflict is left to the resolver, keeping it
/// across passes for the same merge request (`conflict_request_id`) and renewing it for a new one.
/// Returns `(stamped, overdue)`: whether a stamp was written, and whether the conflict has waited
/// past [`RESOLVER_GRACE`].
fn note_unclaimed_conflict(job: &mut TreeJob, name: &str) -> (bool, bool) {
    let Some(branch) = job.manifest.branches.get_mut(name) else {
        return (false, false);
    };
    let request = branch.extra.get("conflict_request_id").cloned();
    let since = branch
        .extra
        .get("conflict_since")
        .and_then(Value::as_str)
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok());
    let same = branch.extra.get("conflict_since_request") == request.as_ref();
    if let Some(since) = since
        && same
    {
        let late = (Utc::now() - since.with_timezone(&Utc))
            .to_std()
            .is_ok_and(|waited| waited > RESOLVER_GRACE);
        return (false, late);
    }
    branch
        .extra
        .insert("conflict_since".to_owned(), Value::from(now()));
    match request {
        Some(request) => branch
            .extra
            .insert("conflict_since_request".to_owned(), request),
        None => branch.extra.remove("conflict_since_request"),
    };
    (true, false)
}

/// A conflict left to the resolver that no resolver run picked up within [`RESOLVER_GRACE`] (quota
/// pause, kill switch, resolver loop down) is claimed, so a late resolver never spends on it, and
/// parked on a question for the owner. Inside the grace it only keeps the stamp.
async fn park_if_unclaimed(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
    node: Option<&str>,
    reason: &str,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let (stamped, overdue) = note_unclaimed_conflict(job, name);
    if !overdue {
        if stamped {
            persist(pool, job).await?;
        }
        return Ok(());
    }
    if let Some(branch) = job.manifest.branches.get_mut(name) {
        branch.extra.remove("conflict_since");
        branch.extra.remove("conflict_since_request");
    }
    let settled = settle_merge(pool, job, name, Duration::ZERO, true)
        .await
        .map_err(|e| e.to_string())?;
    if matches!(settled, MergeSettlement::NotMerged { .. })
        && let Some(node) = node
    {
        parks.push((
            node.to_owned(),
            format!(
                "The merge of branch {name} conflicts and no conflict resolver picked it up within {} minutes: {reason}",
                RESOLVER_GRACE.as_secs() / 60
            ),
        ));
    }
    Ok(())
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
        let before = conflict_attempts(job, name);
        // Past the tree's budget no resolution is paid for: the conflict is claimed (so the
        // resolver never picks it up) and the owner is asked at once.
        let claim = !matches!(
            tree_budget::permits_next_node(pool, &job.root_node_id).await,
            Permit::Yes
        );
        let settled = settle_merge(pool, job, name, Duration::ZERO, claim)
            .await
            .map_err(|e| e.to_string())?;
        match settled {
            MergeSettlement::NotMerged { reason } => {
                tracing::warn!(job = %job.id, branch = name, %reason, "the queue did not merge the branch");
                // Resubmitting a conflicted branch would conflict again: an owner decides.
                if let Some(node) = node {
                    parks.push((node, format!("The merge of branch {name} failed: {reason}")));
                }
            }
            MergeSettlement::Conflicted {
                reason,
                resolution_failed,
            } => {
                // The conflict is the resolver's first: it is announced once per attempt and the
                // owner is asked only when the last attempt's resolution ended without landing.
                // Otherwise a dead request is dropped and the next pass resubmits it.
                let attempts = conflict_attempts(job, name);
                if attempts != before {
                    let line = format!(
                        "{}: branch {name} conflicts; resolver attempt {attempts}",
                        job.goal
                    );
                    if let Err(error) = crate::feed::append(
                        pool,
                        job.project_id.as_deref(),
                        "work_tree_conflict",
                        &line,
                        None,
                        None,
                    )
                    .await
                    {
                        tracing::warn!(job = %job.id, %error, "could not announce a conflict");
                    }
                }
                if !resolution_failed {
                    park_if_unclaimed(pool, job, name, node.as_deref(), &reason, parks).await?;
                }
                if resolution_failed
                    && attempts >= CONFLICT_ATTEMPTS
                    && let Some(node) = node
                {
                    parks.push((
                        node,
                        format!(
                            "The merge of branch {name} conflicts and {attempts} resolution attempts did not land it: {reason}"
                        ),
                    ));
                }
            }
            _ => {}
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
    let node = work_nodes::get(pool, node_id)
        .await
        .map_err(|e| e.to_string())?;
    if node.closed_at.is_some() {
        return Ok(false);
    }
    let Some(report) = (node_report(pool, node_id).await?).result else {
        return Ok(false);
    };
    if node_report(pool, node_id).await?.live || !CLOSE_STATUSES.contains(&report.as_str()) {
        return Ok(false);
    }
    let Some(handoff) = node.handoff_path.filter(|path| Path::new(path).is_file()) else {
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
    let node = work_nodes::get(pool, root_id)
        .await
        .map_err(|e| e.to_string())?;
    if node.closed_at.is_some() {
        return Ok(node.status == "done");
    }
    let report = node_report(pool, root_id).await?;
    Ok(!report.live && report.result.as_deref() == Some("done"))
}

/// PURE: a tree with nothing to land. No branch was merged and every branch is `dropped` or a
/// `done` docs branch, so nothing is in flight, parked or failed. `check_may_land` refuses such a
/// tree on purpose; the driver finishes it held instead of leaving it open for ever.
fn is_empty_tree(manifest: &Manifest) -> bool {
    !manifest.branches.is_empty()
        && manifest
            .branches
            .values()
            .all(|branch| match branch.state.as_str() {
                "dropped" => true,
                "done" => branch.layer == "docs",
                _ => false,
            })
}

async fn landing_step(state: &AppState, job: &mut TreeJob) -> Result<(), String> {
    let pool = &state.pool;
    if job
        .manifest
        .extra
        .get("land_id")
        .and_then(Value::as_i64)
        .is_some()
    {
        let settled = settle_land(pool, job, Duration::ZERO)
            .await
            .map_err(|e| e.to_string())?;
        if let landing::LandSettlement::NotLanded { reason } = settled {
            tracing::warn!(job = %job.id, %reason, "the queue did not land the job");
        }
        return Ok(());
    }
    if !root_is_done(pool, &job.root_node_id).await? {
        return Ok(());
    }
    let empty = is_empty_tree(&job.manifest);
    if !empty && check_may_land(&job.manifest).is_err() {
        return Ok(());
    }

    // The children closed earlier in the pass, so the root closes last.
    if let Err(error) = close_with_report(pool, &job.root_node_id).await {
        tracing::warn!(job = %job.id, %error, "tree driver could not close the root node");
    }

    if empty {
        job.manifest
            .extra
            .insert("empty".to_owned(), Value::Bool(true));
        return finish_held(pool, job).await;
    }

    let integration =
        job.manifest.extra.get("landing").and_then(Value::as_str) == Some("integration");
    if !integration {
        return finish_held(pool, job).await;
    }
    let repo = repo_of(pool, job).await?;
    match land_job(
        pool,
        &repo,
        job,
        Instant::now() + OPERATION_TIMEOUT,
        Origin::Shell,
    )
    .await
    {
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

    #[test]
    fn a_fmt_finding_adds_the_formatter_hint() {
        let findings = "gates FAILED:\n  core: fmt\n      Diff in x.rs";
        let suffix = fix_suffix(findings);
        assert!(suffix.contains(findings), "{suffix}");
        assert!(suffix.contains("cargo fmt -p nucleos-core"), "{suffix}");
        assert!(suffix.contains("bare `rustfmt`"), "{suffix}");
    }

    #[test]
    fn other_findings_keep_the_plain_suffix() {
        let findings = "gates FAILED:\n  core: test\n      assertion failed: fmt output";
        let suffix = fix_suffix(findings);
        assert!(!suffix.contains("rustfmt"), "{suffix}");
        assert!(
            suffix.ends_with("Fix them, then finish the node again."),
            "{suffix}"
        );
    }
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};

    const FIX_TAIL: &str = "Fix them, then finish the node again.";
    const FIX_HEAD: &str = "\n\nThe gate failed on your branch with these findings:\n";

    #[test]
    fn fix_suffix_fmt_failure_gets_the_cargo_fmt_hint() {
        let findings = "gates FAILED:\n  core: fmt\n";
        let suffix = fix_suffix(findings);
        assert!(suffix.contains("cargo fmt -p nucleos-core"));
        assert!(!suffix.contains(FIX_TAIL));
        assert!(suffix.starts_with(&format!("{FIX_HEAD}{findings}")));
    }

    #[test]
    fn fix_suffix_other_failure_keeps_the_generic_tail() {
        let findings = "FAIL core: clippy\ngates FAILED:\n  core: clippy";
        let suffix = fix_suffix(findings);
        assert!(!suffix.contains("cargo fmt"));
        assert!(suffix.contains(FIX_TAIL));
        assert!(suffix.starts_with(&format!("{FIX_HEAD}{findings}")));
    }

    #[test]
    fn fix_suffix_ignores_a_mid_line_fmt_mention() {
        let findings = "note: run fmt later\nFAIL core: clippy";
        assert!(!findings_show_fmt_failure(findings));
        assert!(fix_suffix(findings).contains(FIX_TAIL));
    }
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
            assert!(git_in(
                &repo,
                &["checkout", "-q", "-b", git_branch.as_str()]
            ));
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
            let mut manifest = Manifest::new(
                id,
                "feat/tree",
                "Ship it",
                &self.repo.to_string_lossy(),
                &[],
            );
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
            sqlx::query(
                "INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'seeded', ?, ?)",
            )
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
            assert!(
                !stored.manifest.extra.contains_key("land_id"),
                "no landing admitted"
            );
            assert_eq!(stored.state, "open");
            assert_eq!(self.requests().await, 0, "nothing was queued");
        }

        /// The questions of the open decisions the job asked about `node`.
        async fn open_questions(&self, job: &str, node: &str) -> Vec<String> {
            sqlx::query_scalar(
                "SELECT question FROM decisions WHERE tree_id = ? AND node_id = ? AND status = ?",
            )
            .bind(job)
            .bind(node)
            .bind(decisions::STATUS_OPEN)
            .fetch_all(&self.state.pool)
            .await
            .unwrap()
        }

        /// Gives the job's tree a limit of `limit` dollars and charges `spent` dollars to the
        /// node's current run, so the tree's budget refuses its next node.
        async fn exhaust_budget(&self, job: &str, node: &str, limit: f64, spent: f64) {
            sqlx::query(
                "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
                 VALUES (?, ?, 80.0, ?, ?)",
            )
            .bind(root_node_of(job))
            .bind(limit)
            .bind(STAMP)
            .bind(STAMP)
            .execute(&self.state.pool)
            .await
            .unwrap();
            sqlx::query(
                "UPDATE runs SET cost_usd = ? \
                 WHERE id = (SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?)",
            )
            .bind(spent)
            .bind(node)
            .execute(&self.state.pool)
            .await
            .unwrap();
        }
    }

    // ---- the merge path --------------------------------------------------------------------------

    #[tokio::test]
    async fn a_green_branch_is_submitted_then_settled_into_the_feature_branch() {
        const JOB: &str = "drv-merge";
        let fx = Fixture::new().await;
        fx.seed_job(
            JOB,
            vec![code_branch("api", "green", Some("api-node"))],
            None,
        )
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
        assert_eq!(
            sha_of(&fx.repo, "master"),
            master,
            "only the feature branch moved"
        );
        assert!(!merged.extra.contains_key("merge_request_id"));
        assert_eq!(fx.requests().await, 1, "settling queued nothing");
        let node = fx.node("api-node").await;
        assert_eq!(
            node.status, "done",
            "the merged branch's node is closed with its report"
        );
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
            assert!(
                prompt.contains(finding),
                "the relaunch carries the findings: {prompt:?}"
            );
            fx.relaunched_run_reports_done(NODE).await;
        }

        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 3).await;
        fx.wait_branch_state(JOB, "api", "red").await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        assert_eq!(
            fx.launches(NODE, "fix-").await,
            2,
            "the cap is two relaunches"
        );
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
        let question: String =
            sqlx::query_scalar("SELECT question FROM decisions WHERE node_id = ?")
                .bind(NODE)
                .fetch_one(&fx.state.pool)
                .await
                .unwrap();
        assert!(question.contains("boom-3"), "{question:?}");
    }

    /// Drives `api` through two red gates and two fixes, then a third red gate that parks it on the
    /// fix-cap question; answers that question and returns the run the answer started.
    async fn answered_after_the_fix_cap(
        fx: &Fixture,
        job: &str,
        node: &str,
        gate: &FakeGate,
    ) -> i64 {
        fx.seed_job(job, vec![code_branch("api", "building", Some(node))], None)
            .await;
        fx.add_node(job, node, "orchestrator").await;
        fx.run_ended(node, Some("done")).await;
        fx.save_launch(node).await;
        for attempt in 1..=2 {
            fx.tick(job, gate).await;
            wait_calls(gate, attempt).await;
            fx.wait_branch_state(job, "api", "red").await;
            fx.tick(job, gate).await;
            assert_eq!(fx.branch(job, "api").await.state, "fixing");
            fx.relaunched_run_reports_done(node).await;
        }
        fx.tick(job, gate).await;
        wait_calls(gate, 3).await;
        fx.wait_branch_state(job, "api", "red").await;
        fx.tick(job, gate).await;
        assert_eq!(fx.branch(job, "api").await.state, "parked");
        let decision: String = sqlx::query_scalar("SELECT id FROM decisions WHERE node_id = ?")
            .bind(node)
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();
        open::answer_and_relaunch(&fx.state, &decision, "carry on", "shell")
            .await
            .unwrap()
            .relaunched
            .expect("the answer relaunched the node")
    }

    /// An answered fix-cap park must not park again: the relaunched node's `done` report is gated.
    #[tokio::test]
    async fn an_answered_fix_cap_park_gates_the_relaunched_nodes_work() {
        const JOB: &str = "drv-fix-answered";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        let gate = FakeGate::answering(vec![
            red("boom-1"),
            red("boom-2"),
            red("boom-3"),
            GateVerdict::Green,
        ]);
        answered_after_the_fix_cap(&fx, JOB, NODE, &gate).await;
        fx.relaunched_run_reports_done(NODE).await;

        fx.tick(JOB, &gate).await;

        wait_calls(&gate, 4).await;
        fx.wait_branch_state(JOB, "api", "green").await;
        assert!(
            fx.open_questions(JOB, NODE).await.is_empty(),
            "the owner is not asked the same question again"
        );
    }

    /// The owner's answer is a fresh allowance of fixes: a red gate after it fixes again.
    #[tokio::test]
    async fn an_answer_after_the_fix_cap_gives_a_fresh_fix_allowance() {
        const JOB: &str = "drv-fix-fresh";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        let gate = FakeGate::answering(vec![
            red("boom-1"),
            red("boom-2"),
            red("boom-3"),
            red("boom-4"),
        ]);
        answered_after_the_fix_cap(&fx, JOB, NODE, &gate).await;
        fx.relaunched_run_reports_done(NODE).await;
        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 4).await;
        fx.wait_branch_state(JOB, "api", "red").await;

        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "fixing");
        assert_eq!(fx.launches(NODE, "fix-").await, 3);
        assert!(fx.open_questions(JOB, NODE).await.is_empty());
    }

    #[tokio::test]
    async fn a_repeated_tick_queues_no_second_merge_and_writes_nothing() {
        const JOB: &str = "drv-replay";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "green", None)], None)
            .await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        assert_eq!(fx.requests().await, 1);
        let before = fx.job(JOB).await;
        fx.watch_writes().await;

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(
            fx.writes().await,
            Vec::<String>::new(),
            "a quiet tick writes nothing"
        );
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
        fx.seed_job(
            JOB,
            vec![dependent, code_branch("storage", "green", None)],
            None,
        )
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
        assert!(
            !fx.branch(JOB, "storage")
                .await
                .extra
                .contains_key("merge_sha")
        );
        let waiting = fx.branch(JOB, "api").await;
        assert_eq!(waiting.state, "green");
        assert!(!waiting.extra.contains_key("merge_request_id"));

        fx.drain().await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "storage").await.state, "merged");
        assert_eq!(
            fx.requests().await,
            2,
            "the dependent is queued once its base is merged"
        );
        fx.drain().await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "merged");
        assert!(git_in(
            &fx.repo,
            &["cat-file", "-e", "feat/tree:storage.txt"]
        ));
        assert!(git_in(&fx.repo, &["cat-file", "-e", "feat/tree:api.txt"]));
    }

    // ---- landing ---------------------------------------------------------------------------------

    #[tokio::test]
    async fn the_job_lands_only_when_the_root_is_done_and_every_branch_is_merged() {
        const JOB: &str = "drv-land-gate";
        let fx = Fixture::new().await;
        fx.seed_job(
            JOB,
            vec![
                merged_branch("api", None),
                code_branch("web", "dropped", None),
            ],
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
        assert!(
            landing
                .manifest
                .extra
                .get("land_id")
                .and_then(Value::as_i64)
                .is_some()
        );
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
            fx.seed_job(job, vec![merged_branch("api", None)], landing)
                .await;
            fx.root_done(job).await;

            fx.tick(job, &gate).await;

            let held = fx.job(job).await;
            assert_eq!(held.state, "finished", "{job}");
            assert_eq!(held.manifest.state, "finished", "{job}");
            assert_eq!(held.manifest.extra["landing"], json!("held"), "{job}");
            assert!(!fx.open_jobs().await.contains(&job.to_owned()), "{job}");
            let root = fx.node(&root_node_of(job)).await;
            assert_eq!(
                root.status, "done",
                "{job}: the root closes with its report"
            );
            assert!(root.closed_at.is_some());
        }

        assert_eq!(fx.requests().await, 0, "a held job queues nothing");
        assert_eq!(sha_of(&fx.repo, "master"), master);
    }

    #[tokio::test]
    async fn an_empty_tree_finishes_held_without_merging() {
        const JOB: &str = "drv-empty-held";
        let fx = Fixture::new().await;
        let master = sha_of(&fx.repo, "master");
        let gate = FakeGate::answering(vec![]);
        fx.seed_job(JOB, vec![code_branch("root", "dropped", None)], None)
            .await;
        fx.root_done(JOB).await;

        fx.tick(JOB, &gate).await;

        let held = fx.job(JOB).await;
        assert_eq!(held.state, "finished");
        assert_eq!(held.manifest.state, "finished");
        assert_eq!(fx.requests().await, 0, "an empty tree queues no landing");
        let root = fx.node(&root_node_of(JOB)).await;
        assert_eq!(root.status, "done");
        assert!(root.closed_at.is_some());
        assert_eq!(sha_of(&fx.repo, "master"), master);
    }

    #[tokio::test]
    async fn an_empty_integration_tree_queues_no_landing() {
        const JOB: &str = "drv-empty-integration";
        let fx = Fixture::new().await;
        let gate = FakeGate::answering(vec![]);
        fx.seed_job(
            JOB,
            vec![code_branch("root", "dropped", None)],
            Some("integration"),
        )
        .await;
        fx.root_done(JOB).await;

        fx.tick(JOB, &gate).await;

        assert_eq!(fx.job(JOB).await.state, "finished");
        assert_eq!(fx.requests().await, 0, "an empty tree queues no landing");
    }

    /// Guard: this may pass before the empty-tree completion change, but work in flight must not
    /// finish the job.
    #[tokio::test]
    async fn a_branch_still_building_keeps_an_unmerged_tree_open() {
        const JOB: &str = "drv-empty-building";
        let fx = Fixture::new().await;
        let gate = FakeGate::answering(vec![]);
        fx.seed_job(JOB, vec![code_branch("api", "building", None)], None)
            .await;
        fx.root_done(JOB).await;

        fx.tick(JOB, &gate).await;

        assert_ne!(fx.job(JOB).await.state, "finished");
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
        assert!(
            admitted
                .manifest
                .extra
                .get("land_id")
                .and_then(Value::as_i64)
                .is_some()
        );
        assert_eq!(admitted.state, "open", "admitted is not landed");
        assert_eq!(fx.requests().await, 1);
        assert_eq!(
            sha_of(&fx.repo, "master"),
            master,
            "the queue has not run yet"
        );
        let child = fx.node("api-node").await;
        let root = fx.node(&root_node_of(JOB)).await;
        assert_eq!(
            (child.status.as_str(), root.status.as_str()),
            ("done", "done")
        );
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
        fx.seed_job(MERGING, vec![code_branch("api", "green", None)], None)
            .await;
        let mut stale = fx.job(MERGING).await;
        submit_merge(pool, &resolved, &mut stale, "api", Origin::Daemon)
            .await
            .unwrap();
        fx.drain().await;
        assert_eq!(fx.requests().await, 1);

        fx.tick(MERGING, &gate).await;

        let merged = fx.branch(MERGING, "api").await;
        assert_eq!(merged.state, "merged", "the stored request id is settled");
        assert_eq!(
            merged.extra["merge_sha"],
            json!(sha_of(&fx.repo, "feat/tree"))
        );
        assert_eq!(fx.requests().await, 1, "no second merge was queued");

        // And one admitted a landing and went down before it saw that outcome.
        assert!(git_in(&fx.repo, &["checkout", "-q", "master"]));
        let feature = sha_of(&fx.repo, "feat/tree");
        fx.seed_job(
            LANDING,
            vec![merged_branch("api", None)],
            Some("integration"),
        )
        .await;
        fx.root_done(LANDING).await;
        let mut stale = fx.job(LANDING).await;
        land_job(
            pool,
            &resolved,
            &mut stale,
            Instant::now() + OPERATION_TIMEOUT,
            Origin::Daemon,
        )
        .await
        .unwrap();
        fx.drain().await;
        assert_eq!(fx.requests().await, 2);

        fx.tick(LANDING, &gate).await;

        assert_eq!(
            fx.job(LANDING).await.state,
            "finished",
            "the stored land id is settled"
        );
        assert_eq!(sha_of(&fx.repo, "master^2"), feature);
        assert_eq!(fx.requests().await, 2, "no second landing was queued");
    }

    #[tokio::test]
    async fn the_tick_holds_no_connection_while_calling_the_queue() {
        const JOB: &str = "drv-pool";
        let fx = Fixture::new().await;
        // The pool has one connection, so a queue call that needs one while the tick holds it never
        // returns; the timeout turns that into a failure.
        fx.seed_job(
            JOB,
            vec![code_branch("api", "green", None)],
            Some("integration"),
        )
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
        assert!(
            landing
                .manifest
                .extra
                .get("land_id")
                .and_then(Value::as_i64)
                .is_some()
        );
        assert_eq!(fx.requests().await, 2);
    }

    #[tokio::test]
    async fn a_tick_does_not_wait_for_a_gate_in_progress() {
        const JOB: &str = "drv-slow-gate";
        let fx = Fixture::new().await;
        fx.seed_job(
            JOB,
            vec![code_branch("api", "building", Some("api-node"))],
            None,
        )
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
        assert_eq!(
            gate.calls().len(),
            1,
            "a gate in progress is not started a second time"
        );
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

    // ---- a report that is not `done` ---------------------------------------------------------------

    /// A job with one branch `api` at work behind `api-node`, whose run ended with `outcome`.
    async fn job_whose_branch_reported(fx: &Fixture, job: &str, outcome: &str) {
        fx.seed_job(
            job,
            vec![code_branch("api", "building", Some("api-node"))],
            None,
        )
        .await;
        fx.add_node(job, "api-node", "orchestrator").await;
        fx.save_launch("api-node").await;
        fx.run_ended("api-node", Some(outcome)).await;
    }

    #[tokio::test]
    async fn a_branch_reporting_blocked_parks_and_asks_the_owner() {
        const JOB: &str = "drv-blocked";
        let fx = Fixture::new().await;
        job_whose_branch_reported(&fx, JOB, "blocked").await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;

        let asked = fx.open_questions(JOB, "api-node").await;
        assert_eq!(asked.len(), 1, "the owner is asked once: {asked:?}");
        assert!(
            asked[0].to_lowercase().contains("blocked"),
            "{:?}",
            asked[0]
        );
        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        assert!(gate.calls().is_empty(), "a blocked branch is not gated");
    }

    #[tokio::test]
    async fn a_branch_reporting_failed_parks_and_asks_the_owner() {
        const JOB: &str = "drv-failed";
        let fx = Fixture::new().await;
        job_whose_branch_reported(&fx, JOB, "failed").await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;

        let asked = fx.open_questions(JOB, "api-node").await;
        assert_eq!(asked.len(), 1, "the owner is asked once: {asked:?}");
        assert!(asked[0].to_lowercase().contains("failed"), "{:?}", asked[0]);
        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        assert!(gate.calls().is_empty(), "a failed branch is not gated");
    }

    #[tokio::test]
    async fn a_blocked_branch_parks_once_across_ticks() {
        const JOB: &str = "drv-blocked-once";
        let fx = Fixture::new().await;
        job_whose_branch_reported(&fx, JOB, "blocked").await;
        let gate = FakeGate::answering(vec![]);

        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }

        // The first tick must have parked it; the later ones must add nothing.
        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        assert_eq!(fx.open_questions(JOB, "api-node").await.len(), 1);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions")
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "repeated ticks asked nothing more");
    }

    /// Guard: passes today and must keep passing. A run that is still live has not stopped, so its
    /// report is not yet the node's last word.
    #[tokio::test]
    async fn a_blocked_branch_whose_run_is_still_live_does_not_park() {
        const JOB: &str = "drv-blocked-live";
        let fx = Fixture::new().await;
        fx.seed_job(
            JOB,
            vec![code_branch("api", "building", Some("api-node"))],
            None,
        )
        .await;
        fx.add_node(JOB, "api-node", "orchestrator").await;
        fx.run_with("api-node", "running", Some("blocked")).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "building");
        assert!(fx.open_questions(JOB, "api-node").await.is_empty());
    }

    /// The job's root tops no branch, so there is nothing for `decisions::park` to park: GREEN has
    /// to give the root another way to ask. What the owner needs is pinned instead: one open
    /// decision of this job, and the job neither finished nor landing.
    #[tokio::test]
    async fn a_root_reporting_blocked_parks_and_asks_the_owner() {
        const JOB: &str = "drv-root-blocked";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![merged_branch("api", None)], Some("integration"))
            .await;
        fx.run_ended(&root_node_of(JOB), Some("blocked")).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        let asked = fx.open_questions(JOB, &root_node_of(JOB)).await;
        assert_eq!(asked.len(), 1, "the owner is asked once: {asked:?}");
        assert!(
            asked[0].to_lowercase().contains("blocked"),
            "{:?}",
            asked[0]
        );
        fx.assert_not_landing(JOB).await;
        assert_eq!(fx.job(JOB).await.state, "open", "the job is not finished");
    }

    /// Guard: this passes today because the second park failure is only logged. It ensures that a
    /// root which tops a branch still asks once and never starts a paid run.
    #[tokio::test]
    async fn a_blocked_root_that_tops_a_branch_parks_once_across_ticks() {
        const JOB: &str = "drv-root-branch-blocked-once";
        let fx = Fixture::new().await;
        let root = root_node_of(JOB);
        fx.seed_job(
            JOB,
            vec![code_branch("root", "building", Some(&root))],
            Some("integration"),
        )
        .await;
        fx.run_ended(&root, Some("blocked")).await;
        let gate = FakeGate::answering(vec![]);

        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }

        assert_eq!(fx.branch(JOB, "root").await.state, "parked");
        assert_eq!(fx.open_questions(JOB, &root).await.len(), 1);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions")
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "repeated ticks asked nothing more");
        fx.assert_not_landing(JOB).await;
        assert_eq!(fx.job(JOB).await.state, "open", "the job is not finished");
        assert!(gate.calls().is_empty(), "a blocked root is not gated");
    }

    #[tokio::test]
    async fn an_answered_blocked_branch_is_not_parked_again_while_its_new_run_lives() {
        const JOB: &str = "drv-blocked-answered";
        let fx = Fixture::new().await;
        job_whose_branch_reported(&fx, JOB, "blocked").await;
        let gate = FakeGate::answering(vec![]);
        fx.tick(JOB, &gate).await;
        let asked = fx.open_questions(JOB, "api-node").await;
        assert_eq!(asked.len(), 1, "the branch parked on the first tick");
        let decision: String = sqlx::query_scalar("SELECT id FROM decisions WHERE node_id = ?")
            .bind("api-node")
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();

        let answered = open::answer_and_relaunch(&fx.state, &decision, "go ahead", "shell")
            .await
            .unwrap();
        let new_run = answered.relaunched.expect("the answer relaunched the node");
        // The new run is live: it has not reported, and the old `blocked` belongs to the old run.
        sqlx::query("UPDATE runs SET status = 'running' WHERE id = ?")
            .bind(new_run)
            .execute(&fx.state.pool)
            .await
            .unwrap();
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "building");
        assert!(fx.open_questions(JOB, "api-node").await.is_empty());
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions")
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "the old report asked nothing a second time");
    }

    /// The root's question is answered like a branch's: the answer starts the root again, under
    /// `decision-<id>`, and its new run is not parked again by the old report.
    #[tokio::test]
    async fn an_answered_blocked_root_is_relaunched() {
        const JOB: &str = "drv-root-answered";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![merged_branch("api", None)], Some("integration"))
            .await;
        let root = root_node_of(JOB);
        fx.save_launch(&root).await;
        let dead = fx.run_ended(&root, Some("blocked")).await;
        let gate = FakeGate::answering(vec![]);
        fx.tick(JOB, &gate).await;
        let decision: String = sqlx::query_scalar("SELECT id FROM decisions WHERE node_id = ?")
            .bind(&root)
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();

        let answered = open::answer_and_relaunch(&fx.state, &decision, "go ahead", "shell")
            .await
            .unwrap();

        let new_run = answered.relaunched.expect("the answer relaunched the root");
        assert_ne!(new_run, dead);
        assert_eq!(fx.current_run(&root).await, new_run);
        assert_eq!(fx.launches(&root, &format!("decision-{decision}")).await, 1);
        assert!(fx.open_questions(JOB, &root).await.is_empty());
    }

    // ---- the tree budget ---------------------------------------------------------------------------

    #[tokio::test]
    async fn a_red_gate_at_the_budget_limit_parks_instead_of_relaunching() {
        const JOB: &str = "drv-fix-budget";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "red", Some(NODE))], None)
            .await;
        fx.add_node(JOB, NODE, "orchestrator").await;
        fx.run_ended(NODE, Some("done")).await;
        fx.save_launch(NODE).await;
        fx.exhaust_budget(JOB, NODE, 1.0, 5.0).await;
        let run_before = fx.current_run(NODE).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(
            fx.launches(NODE, "fix-").await,
            0,
            "no paid run was started"
        );
        assert_eq!(fx.current_run(NODE).await, run_before);
        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        let asked = fx.open_questions(JOB, NODE).await;
        assert_eq!(asked.len(), 1, "the owner is told once: {asked:?}");
        assert!(asked[0].to_lowercase().contains("budget"), "{:?}", asked[0]);
    }

    // ---- gap 3: a start refused for disk alone is retried by the driver ---------------------------

    /// A layer no real disk can hold, and the same layer measured at no cost.
    const SHORT_GB: f64 = 1e12;
    const FREED_GB: f64 = 0.0;
    const PENDING_LAYER: &str = "backend";
    const PENDING_NAME: &str = "late";

    async fn set_layer_cost(fx: &Fixture, job: &str, layer: &str, gb: f64) {
        let mut stored = fx.job(job).await;
        stored.manifest.costs_gb.insert(layer.to_owned(), json!(gb));
        store::update(&fx.state.pool, &stored).await.unwrap();
    }

    /// A job with no branch, a pending start of `PENDING_NAME` that the disk refused, and the one
    /// open decision `post_branch_start` would have opened for it. Returns the decision's id.
    async fn job_with_a_pending_start(fx: &Fixture, job: &str) -> String {
        fx.seed_job(job, Vec::new(), None).await;
        // The replay opens the root: name its base branch, and a 0 floor so only the cost refuses.
        let workflow = fx.repo.join(".nucleos").join("workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(
            workflow.join("speed.yaml"),
            "tree:\n  base_branch: master\n  floor_gb: 0\n",
        )
        .unwrap();
        let caller = root_node_of(job);
        let asked = decisions::ask_pending(
            &fx.state.pool,
            job,
            &caller,
            "branch late waits for disk space",
        )
        .await
        .unwrap();
        let mut stored = fx.job(job).await;
        stored
            .manifest
            .costs_gb
            .insert(PENDING_LAYER.to_owned(), json!(SHORT_GB));
        stored.manifest.extra.insert(
            "pending_starts".to_owned(),
            json!({ PENDING_NAME: {
                "caller": caller,
                "goal": "Do the work",
                "prompt": "Do not run.",
                "paths": ["late/**"],
                "layer": PENDING_LAYER,
                "base": null,
                "decision_id": asked.id,
            }}),
        );
        store::update(&fx.state.pool, &stored).await.unwrap();
        asked.id
    }

    fn pending_starts(job: &TreeJob) -> Value {
        job.manifest
            .extra
            .get("pending_starts")
            .cloned()
            .unwrap_or_else(|| json!({}))
    }

    async fn decision_row(fx: &Fixture, id: &str) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT status, answer, origin FROM decisions WHERE id = ?")
            .bind(id)
            .fetch_one(&fx.state.pool)
            .await
            .unwrap()
    }

    async fn count_of(fx: &Fixture, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(&fx.state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_pending_start_is_admitted_when_disk_frees_and_its_decision_is_answered() {
        const JOB: &str = "drv-pending-admit";
        let fx = Fixture::new().await;
        let decision = job_with_a_pending_start(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        assert!(fx.job(JOB).await.manifest.branches.is_empty());

        set_layer_cost(&fx, JOB, PENDING_LAYER, FREED_GB).await;
        fx.tick(JOB, &gate).await;

        let stored = fx.job(JOB).await;
        assert!(
            pending_starts(&stored).get(PENDING_NAME).is_none(),
            "the pending record is removed: {:?}",
            pending_starts(&stored)
        );
        let branch = stored
            .manifest
            .branches
            .get(PENDING_NAME)
            .expect("the branch was started from the stored body");
        let node = branch
            .node_id
            .clone()
            .expect("the branch is bound to a node");
        assert_eq!(fx.launches(&node, "branch-late").await, 1);
        let (status, answer, origin) = decision_row(&fx, &decision).await;
        assert_eq!(status, decisions::STATUS_ANSWERED);
        assert_eq!(origin.as_deref(), Some("driver"));
        assert!(
            answer.as_deref().unwrap_or_default().contains("disk freed"),
            "{answer:?}"
        );
        assert!(fx.open_questions(JOB, &root_node_of(JOB)).await.is_empty());
        assert_ne!(fx.branch(JOB, "late").await.state, "parked");
    }

    #[tokio::test]
    async fn a_pending_start_stays_while_the_disk_is_still_short_and_asks_no_second_time() {
        const JOB: &str = "drv-pending-short";
        let fx = Fixture::new().await;
        let decision = job_with_a_pending_start(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        let decisions_before = count_of(&fx, "decisions").await;
        let feed_before = count_of(&fx, "feed").await;
        let before = pending_starts(&fx.job(JOB).await);

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        let stored = fx.job(JOB).await;
        assert_eq!(pending_starts(&stored), before, "the record is untouched");
        assert!(stored.manifest.branches.is_empty(), "no branch was started");
        assert_eq!(count_of(&fx, "decisions").await, decisions_before);
        assert_eq!(
            count_of(&fx, "feed").await,
            feed_before,
            "nothing is announced"
        );
        assert_eq!(decision_row(&fx, &decision).await.0, decisions::STATUS_OPEN);
    }

    #[tokio::test]
    async fn an_owner_answer_before_the_disk_frees_does_not_break_the_retry() {
        const JOB: &str = "drv-pending-owner";
        let fx = Fixture::new().await;
        let decision = job_with_a_pending_start(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        decisions::answer(&fx.state.pool, &decision, "I will free some space", "owner")
            .await
            .unwrap();

        fx.tick(JOB, &gate).await;
        assert!(
            pending_starts(&fx.job(JOB).await)
                .get(PENDING_NAME)
                .is_some(),
            "the retry waits for the disk, not for the owner"
        );

        set_layer_cost(&fx, JOB, PENDING_LAYER, FREED_GB).await;
        fx.tick(JOB, &gate).await;

        let stored = fx.job(JOB).await;
        assert!(pending_starts(&stored).get(PENDING_NAME).is_none());
        assert!(stored.manifest.branches.contains_key(PENDING_NAME));
        let (status, answer, origin) = decision_row(&fx, &decision).await;
        assert_eq!(status, decisions::STATUS_ANSWERED);
        assert_eq!(
            answer.as_deref(),
            Some("I will free some space"),
            "the owner's answer stays"
        );
        assert_eq!(origin.as_deref(), Some("owner"));
    }

    // ---- pending start crash windows ---------------------------------------------------------------

    /// What a crash right after the manifest record leaves: the record is stored, no decision was
    /// asked, and the record carries no `decision_id`.
    async fn job_with_a_record_but_no_decision(fx: &Fixture, job: &str) {
        let decision = job_with_a_pending_start(fx, job).await;
        sqlx::query("DELETE FROM decisions WHERE id = ?")
            .bind(&decision)
            .execute(&fx.state.pool)
            .await
            .unwrap();
        let mut stored = fx.job(job).await;
        stored
            .manifest
            .extra
            .get_mut("pending_starts")
            .and_then(Value::as_object_mut)
            .and_then(|pending| pending.get_mut(PENDING_NAME))
            .and_then(Value::as_object_mut)
            .expect("the record is there")
            .remove("decision_id");
        store::update(&fx.state.pool, &stored).await.unwrap();
    }

    fn recorded_decision(job: &TreeJob) -> Option<String> {
        pending_starts(job)[PENDING_NAME]["decision_id"]
            .as_str()
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn a_pending_record_with_no_decision_asks_once_on_the_next_pass() {
        const JOB: &str = "drv-pending-nodecision";
        let fx = Fixture::new().await;
        job_with_a_record_but_no_decision(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        let decisions_before = count_of(&fx, "decisions").await;
        assert_eq!(recorded_decision(&fx.job(JOB).await), None);

        // The disk is still short: the pass heals the record by asking, exactly once.
        fx.tick(JOB, &gate).await;
        let healed = recorded_decision(&fx.job(JOB).await)
            .expect("the pass stored the id of the decision it asked");
        assert_eq!(count_of(&fx, "decisions").await, decisions_before + 1);
        assert_eq!(decision_row(&fx, &healed).await.0, decisions::STATUS_OPEN);

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;
        assert_eq!(
            count_of(&fx, "decisions").await,
            decisions_before + 1,
            "later passes ask nothing more"
        );
        assert_eq!(
            recorded_decision(&fx.job(JOB).await),
            Some(healed.clone()),
            "the id stays"
        );
        assert_eq!(fx.open_questions(JOB, &root_node_of(JOB)).await.len(), 1);

        // Once the disk frees, the healed decision is the one answered.
        set_layer_cost(&fx, JOB, PENDING_LAYER, FREED_GB).await;
        fx.tick(JOB, &gate).await;
        let stored = fx.job(JOB).await;
        assert!(pending_starts(&stored).get(PENDING_NAME).is_none());
        assert!(stored.manifest.branches.contains_key(PENDING_NAME));
        let (status, _, origin) = decision_row(&fx, &healed).await;
        assert_eq!(status, decisions::STATUS_ANSWERED);
        assert_eq!(origin.as_deref(), Some("driver"));
        assert_eq!(count_of(&fx, "decisions").await, decisions_before + 1);
    }

    // ---- Fix J: a second disk refusal of the same branch is asked about again -----------------------
    //
    // The refusals below go through the real route (`start_pending` posts `/branch_start`), so the
    // record, the ask and the id are written by the production code. Assumed seams:
    // `pending_decision_id(job_id, name, generation)` and the manifest key
    // `pending_start_generation`, which starts at 1 and grows when the replay drops a record.

    /// A job with no branch whose `PENDING_LAYER` the disk refuses, and no pending record yet.
    async fn job_the_disk_refuses(fx: &Fixture, job: &str) {
        fx.seed_job(job, Vec::new(), None).await;
        let workflow = fx.repo.join(".nucleos").join("workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(
            workflow.join("speed.yaml"),
            "tree:\n  base_branch: master\n  floor_gb: 0\n",
        )
        .unwrap();
        set_layer_cost(fx, job, PENDING_LAYER, SHORT_GB).await;
    }

    /// The start body of `PENDING_NAME`, as a pending record stores it.
    fn pending_body(job: &str) -> Value {
        json!({
            "caller": root_node_of(job),
            "goal": "Do the work",
            "prompt": "Do not run.",
            "paths": ["late/**"],
            "layer": PENDING_LAYER,
            "base": null,
        })
    }

    /// Posts the start of `PENDING_NAME`; the disk refuses it, which records and asks.
    async fn refuse_the_start(fx: &Fixture, job: &str) {
        let refused = crate::tree_job::branch_start::start_pending(
            &fx.state,
            job,
            PENDING_NAME,
            &pending_body(job),
        )
        .await;
        assert!(refused.is_err(), "the disk refuses the start: {refused:?}");
    }

    async fn decisions_of(fx: &Fixture, job: &str) -> Vec<(String, String)> {
        sqlx::query_as("SELECT id, status FROM decisions WHERE tree_id = ? ORDER BY id")
            .bind(job)
            .fetch_all(&fx.state.pool)
            .await
            .unwrap()
    }

    async fn feed_lines_naming(fx: &Fixture, text: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE summary LIKE ?")
            .bind(format!("%{text}%"))
            .fetch_one(&fx.state.pool)
            .await
            .unwrap()
    }

    /// Frees the disk, lets the driver admit the pending start, then ends the branch so that the
    /// same name can be refused again.
    async fn admit_and_end_the_branch(fx: &Fixture, job: &str, gate: &FakeGate) {
        set_layer_cost(fx, job, PENDING_LAYER, FREED_GB).await;
        fx.tick(job, gate).await;
        let mut stored = fx.job(job).await;
        assert!(
            stored.manifest.branches.contains_key(PENDING_NAME),
            "the branch was admitted"
        );
        assert!(pending_starts(&stored).get(PENDING_NAME).is_none());
        stored.manifest.branches.remove(PENDING_NAME);
        store::update(&fx.state.pool, &stored).await.unwrap();
        set_layer_cost(fx, job, PENDING_LAYER, SHORT_GB).await;
    }

    #[tokio::test]
    async fn a_second_disk_refusal_after_an_admission_asks_the_owner_again() {
        const JOB: &str = "drv-pending-second";
        let fx = Fixture::new().await;
        job_the_disk_refuses(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        let first_id = crate::tree_job::branch_start::pending_decision_id(JOB, PENDING_NAME, 1);
        let second_id = crate::tree_job::branch_start::pending_decision_id(JOB, PENDING_NAME, 2);
        assert_eq!(first_id, format!("pending-{JOB}-{PENDING_NAME}-1"));
        assert_ne!(first_id, second_id);

        refuse_the_start(&fx, JOB).await;
        assert_eq!(
            decisions_of(&fx, JOB).await,
            vec![(first_id.clone(), decisions::STATUS_OPEN.to_owned())]
        );
        assert_eq!(
            recorded_decision(&fx.job(JOB).await),
            Some(first_id.clone())
        );

        admit_and_end_the_branch(&fx, JOB, &gate).await;
        assert_eq!(
            decision_row(&fx, &first_id).await.0,
            decisions::STATUS_ANSWERED
        );

        refuse_the_start(&fx, JOB).await;

        let mut expected = vec![
            (first_id.clone(), decisions::STATUS_ANSWERED.to_owned()),
            (second_id.clone(), decisions::STATUS_OPEN.to_owned()),
        ];
        expected.sort();
        assert_eq!(
            decisions_of(&fx, JOB).await,
            expected,
            "two distinct decisions, the second one open"
        );
        assert_eq!(
            feed_lines_naming(&fx, &second_id).await,
            1,
            "the owner is told in the feed"
        );
        assert_eq!(
            recorded_decision(&fx.job(JOB).await),
            Some(second_id),
            "the new record names the new decision"
        );
    }

    #[tokio::test]
    async fn a_repeated_refusal_while_the_start_is_pending_asks_nothing_more() {
        const JOB: &str = "drv-pending-repeat";
        let fx = Fixture::new().await;
        job_the_disk_refuses(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);

        refuse_the_start(&fx, JOB).await;
        admit_and_end_the_branch(&fx, JOB, &gate).await;
        refuse_the_start(&fx, JOB).await;
        let decisions_before = decisions_of(&fx, JOB).await;
        let record_before = pending_starts(&fx.job(JOB).await);
        let feed_before = count_of(&fx, "feed").await;

        refuse_the_start(&fx, JOB).await;
        fx.tick(JOB, &gate).await;
        refuse_the_start(&fx, JOB).await;

        assert_eq!(decisions_of(&fx, JOB).await, decisions_before);
        assert_eq!(
            pending_starts(&fx.job(JOB).await),
            record_before,
            "the record keeps its generation"
        );
        assert_eq!(count_of(&fx, "feed").await, feed_before);
    }

    #[tokio::test]
    async fn the_heal_of_a_record_with_no_decision_asks_with_the_records_generation() {
        const JOB: &str = "drv-pending-heal-generation";
        let fx = Fixture::new().await;
        job_the_disk_refuses(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        refuse_the_start(&fx, JOB).await;
        admit_and_end_the_branch(&fx, JOB, &gate).await;
        refuse_the_start(&fx, JOB).await;
        let second_id = crate::tree_job::branch_start::pending_decision_id(JOB, PENDING_NAME, 2);
        assert_eq!(
            recorded_decision(&fx.job(JOB).await),
            Some(second_id.clone())
        );

        // The crash between the record and the ask: no decision, and the record has no id.
        sqlx::query("DELETE FROM decisions WHERE id = ?")
            .bind(&second_id)
            .execute(&fx.state.pool)
            .await
            .unwrap();
        let mut stored = fx.job(JOB).await;
        stored
            .manifest
            .extra
            .get_mut("pending_starts")
            .and_then(Value::as_object_mut)
            .and_then(|pending| pending.get_mut(PENDING_NAME))
            .and_then(Value::as_object_mut)
            .expect("the record is there")
            .remove("decision_id");
        store::update(&fx.state.pool, &stored).await.unwrap();

        fx.tick(JOB, &gate).await;

        assert_eq!(
            recorded_decision(&fx.job(JOB).await),
            Some(second_id.clone()),
            "the heal asks under the record's generation"
        );
        assert_eq!(
            decision_row(&fx, &second_id).await.0,
            decisions::STATUS_OPEN
        );
        assert_eq!(
            decisions_of(&fx, JOB).await.len(),
            2,
            "one answered, one open"
        );
    }

    #[tokio::test]
    async fn a_replay_interrupted_before_the_answer_finishes_on_the_next_pass() {
        const JOB: &str = "drv-pending-halfreplay";
        let fx = Fixture::new().await;
        let decision = job_with_a_pending_start(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        set_layer_cost(&fx, JOB, PENDING_LAYER, FREED_GB).await;
        // The crash: the branch was started, but neither the answer nor the record's drop happened.
        let stored = fx.job(JOB).await;
        let record = pending_starts(&stored)[PENDING_NAME].clone();
        crate::tree_job::branch_start::start_pending(&fx.state, JOB, PENDING_NAME, &record)
            .await
            .unwrap();
        assert_eq!(decision_row(&fx, &decision).await.0, decisions::STATUS_OPEN);
        assert!(
            pending_starts(&fx.job(JOB).await)
                .get(PENDING_NAME)
                .is_some()
        );

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        let stored = fx.job(JOB).await;
        assert!(
            pending_starts(&stored).get(PENDING_NAME).is_none(),
            "the record is dropped"
        );
        let node = stored.manifest.branches[PENDING_NAME]
            .node_id
            .clone()
            .expect("the branch is bound to a node");
        assert_eq!(
            fx.launches(&node, "branch-late").await,
            1,
            "the branch is not started a second time"
        );
        let (status, _, origin) = decision_row(&fx, &decision).await;
        assert_eq!(status, decisions::STATUS_ANSWERED);
        assert_eq!(origin.as_deref(), Some("driver"));
        let announced: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'work_tree_pending'")
                .fetch_one(&fx.state.pool)
                .await
                .unwrap();
        assert_eq!(announced, 1, "admitted is announced once");
    }

    // ---- gap 4: a conflict is the resolver's before it is the owner's ------------------------------

    const CLASH_JOB: &str = "drv-clash";
    const CLASH_NODE: &str = "api-node";

    /// `tree/api` and `feat/tree` each change `feature.txt`, so merging one into the other conflicts.
    fn make_api_conflict(repo: &Path) {
        assert!(git_in(repo, &["checkout", "-q", "tree/api"]));
        commit_file(repo, "feature.txt", "theirs\n", "theirs");
        assert!(git_in(repo, &["checkout", "-q", "feat/tree"]));
        commit_file(repo, "feature.txt", "ours\n", "ours");
    }

    /// A job with one green branch `api` whose merge conflicts, behind a node that reported `done`.
    async fn conflicting_job(fx: &Fixture) {
        make_api_conflict(&fx.repo);
        fx.seed_job(
            CLASH_JOB,
            vec![code_branch("api", "green", Some(CLASH_NODE))],
            None,
        )
        .await;
        fx.add_node(CLASH_JOB, CLASH_NODE, "orchestrator").await;
        fx.run_ended(CLASH_NODE, Some("done")).await;
    }

    /// Runs the queue once, with or without a request waiting.
    async fn run_queue(fx: &Fixture) {
        drain_once(&fx.state.pool, &fx.key, &GitExecutor::default()).await;
    }

    /// The resolver claimed the newest escalated request and its run ended without landing.
    async fn resolution_ends_without_landing(fx: &Fixture) {
        let run = fx.next_run.get() + 1;
        fx.next_run.set(run);
        sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'resolve', 'interrupted', ?)")
            .bind(run)
            .bind(STAMP)
            .execute(&fx.state.pool)
            .await
            .unwrap();
        let claimed = sqlx::query(
            "UPDATE vcs_requests SET resolution_run_id = ?
              WHERE id = (SELECT MAX(id) FROM vcs_requests)
                AND status = 'escalated' AND resolution_run_id IS NULL",
        )
        .bind(run)
        .execute(&fx.state.pool)
        .await
        .unwrap();
        assert_eq!(
            claimed.rows_affected(),
            1,
            "an escalated request was waiting"
        );
    }

    async fn conflict_attempts(fx: &Fixture) -> Option<i64> {
        fx.branch(CLASH_JOB, "api")
            .await
            .extra
            .get("conflict_attempts")
            .and_then(Value::as_i64)
    }

    #[tokio::test]
    async fn a_conflicting_green_branch_is_left_to_the_resolver_and_not_parked() {
        let fx = Fixture::new().await;
        conflicting_job(&fx).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(CLASH_JOB, &gate).await;
        fx.drain().await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;

        let branch = fx.branch(CLASH_JOB, "api").await;
        assert_eq!(branch.state, "green", "the branch waits, it is not parked");
        let request = branch.extra["merge_request_id"]
            .as_i64()
            .expect("the request stays on the branch for the resolver");
        assert_eq!(conflict_attempts(&fx).await, Some(1));
        assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());
        let row: (String, Option<String>) =
            sqlx::query_as("SELECT status, settled_at FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&fx.state.pool)
                .await
                .unwrap();
        assert_eq!(row.0, "escalated");
        assert_eq!(row.1, None, "the resolver may still claim the request");
        assert_eq!(fx.requests().await, 1, "nothing is resubmitted meanwhile");
    }

    #[tokio::test]
    async fn a_second_failed_resolution_parks_once() {
        let fx = Fixture::new().await;
        conflicting_job(&fx).await;
        let gate = FakeGate::answering(vec![]);

        // Attempt 1: the queue escalates, the resolver fails.
        fx.tick(CLASH_JOB, &gate).await;
        fx.drain().await;
        fx.tick(CLASH_JOB, &gate).await;
        assert_eq!(conflict_attempts(&fx).await, Some(1));
        resolution_ends_without_landing(&fx).await;
        // The driver drops the dead request and resubmits (in this pass or the next).
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;
        assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());

        // Attempt 2: the queue escalates again, the resolver fails again.
        run_queue(&fx).await;
        fx.tick(CLASH_JOB, &gate).await;
        assert_eq!(conflict_attempts(&fx).await, Some(2));
        assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());
        resolution_ends_without_landing(&fx).await;

        // At the limit the conflict is claimed and the owner is asked, once.
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;

        assert_eq!(fx.branch(CLASH_JOB, "api").await.state, "parked");
        assert_eq!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.len(), 1);
        assert_eq!(fx.requests().await, 2, "no third attempt was queued");
    }

    #[tokio::test]
    async fn a_resolution_that_lands_merges_the_branch_with_no_park() {
        let fx = Fixture::new().await;
        conflicting_job(&fx).await;
        let gate = FakeGate::answering(vec![]);
        fx.tick(CLASH_JOB, &gate).await;
        fx.drain().await;
        fx.tick(CLASH_JOB, &gate).await;
        assert_eq!(fx.branch(CLASH_JOB, "api").await.state, "green");

        // The resolver landed the same merge as a later request.
        let sha = sha_of(&fx.repo, "feat/tree");
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status,
                                       result_sha, created_at)
             SELECT op, args, project_id, project_root, origin, 'succeeded', ?, created_at
               FROM vcs_requests ORDER BY id LIMIT 1",
        )
        .bind(&sha)
        .execute(&fx.state.pool)
        .await
        .unwrap();
        fx.tick(CLASH_JOB, &gate).await;

        let branch = fx.branch(CLASH_JOB, "api").await;
        assert_eq!(branch.state, "merged");
        assert_eq!(branch.extra["merge_sha"], json!(sha));
        assert!(!branch.extra.contains_key("merge_request_id"));
        assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());
        assert_eq!(count_of(&fx, "decisions").await, 0, "nobody was asked");
    }

    #[tokio::test]
    async fn an_exhausted_tree_budget_parks_the_conflict_without_a_resolver() {
        let fx = Fixture::new().await;
        conflicting_job(&fx).await;
        fx.exhaust_budget(CLASH_JOB, CLASH_NODE, 1.0, 5.0).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(CLASH_JOB, &gate).await;
        fx.drain().await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;

        assert_eq!(fx.branch(CLASH_JOB, "api").await.state, "parked");
        assert_eq!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.len(), 1);
        let settled: Vec<Option<String>> =
            sqlx::query_scalar("SELECT settled_reason FROM vcs_requests")
                .fetch_all(&fx.state.pool)
                .await
                .unwrap();
        assert_eq!(
            settled,
            [Some("tree-merge".to_owned())],
            "the request is claimed so the resolver never spends on it"
        );
    }

    // ---- a conflict nobody claims ------------------------------------------------------------------

    /// The `conflict_since` the driver stored on `api`, as the string it wrote.
    async fn conflict_since(fx: &Fixture) -> Option<String> {
        fx.branch(CLASH_JOB, "api")
            .await
            .extra
            .get("conflict_since")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Rewrites `conflict_since` on `api` to `minutes` ago, as if the resolver never came.
    async fn age_the_conflict(fx: &Fixture, minutes: i64) {
        let mut stored = fx.job(CLASH_JOB).await;
        let since = (chrono::Utc::now() - chrono::Duration::minutes(minutes)).to_rfc3339();
        stored
            .manifest
            .branches
            .get_mut("api")
            .expect("the branch is in the manifest")
            .extra
            .insert("conflict_since".to_owned(), json!(since));
        store::update(&fx.state.pool, &stored).await.unwrap();
    }

    /// The conflict is escalated and left to the resolver, which never claims it.
    async fn conflict_left_unclaimed(fx: &Fixture, gate: &FakeGate) {
        fx.tick(CLASH_JOB, gate).await;
        fx.drain().await;
        fx.tick(CLASH_JOB, gate).await;
        assert_eq!(fx.branch(CLASH_JOB, "api").await.state, "green");
    }

    #[tokio::test]
    async fn a_conflict_no_resolver_picks_up_parks_once_after_the_grace() {
        let fx = Fixture::new().await;
        conflicting_job(&fx).await;
        let gate = FakeGate::answering(vec![]);
        conflict_left_unclaimed(&fx, &gate).await;
        let since = conflict_since(&fx)
            .await
            .expect("leaving the conflict to the resolver stamps conflict_since");
        chrono::DateTime::parse_from_rfc3339(&since).expect("conflict_since is RFC3339");
        fx.tick(CLASH_JOB, &gate).await;
        assert_eq!(
            conflict_since(&fx).await,
            Some(since),
            "the stamp is kept across passes for the same merge request"
        );
        assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());

        // Nobody picked the request up for longer than the grace (quota pause, loop down).
        age_the_conflict(&fx, 16).await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;

        assert_eq!(fx.branch(CLASH_JOB, "api").await.state, "parked");
        let questions = fx.open_questions(CLASH_JOB, CLASH_NODE).await;
        assert_eq!(questions.len(), 1, "the owner is asked once: {questions:?}");
        assert!(
            questions[0].to_lowercase().contains("resolver"),
            "the reason says the resolver never picked it up: {:?}",
            questions[0]
        );
        let settled: Vec<Option<String>> =
            sqlx::query_scalar("SELECT settled_reason FROM vcs_requests")
                .fetch_all(&fx.state.pool)
                .await
                .unwrap();
        assert_eq!(
            settled,
            [Some("tree-merge".to_owned())],
            "the request is claimed so a late resolver never spends on it"
        );
        assert_eq!(fx.requests().await, 1, "no further attempt was queued");
    }

    #[tokio::test]
    async fn a_conflict_inside_the_grace_stays_with_the_resolver() {
        let fx = Fixture::new().await;
        conflicting_job(&fx).await;
        let gate = FakeGate::answering(vec![]);
        conflict_left_unclaimed(&fx, &gate).await;

        age_the_conflict(&fx, 14).await;
        let aged = conflict_since(&fx).await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;

        let branch = fx.branch(CLASH_JOB, "api").await;
        assert_eq!(branch.state, "green", "the branch still waits");
        assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());
        assert_eq!(count_of(&fx, "decisions").await, 0, "nobody was asked");
        assert_eq!(conflict_since(&fx).await, aged, "the stamp is not reset");
        let settled: Vec<Option<String>> =
            sqlx::query_scalar("SELECT settled_reason FROM vcs_requests")
                .fetch_all(&fx.state.pool)
                .await
                .unwrap();
        assert_eq!(settled, [None], "the resolver may still claim the request");
        assert_eq!(fx.requests().await, 1);
    }
}
