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

use super::autofmt;
use super::decisions;
use super::engine::{apply_transition, branch_node_may_close};
use super::http::job_lock;
use super::ladder::{self, Kind, Spent, Step};
use super::landing::{
    self, LandingError, MergeSettlement, check_may_land, check_may_merge, land_job, settle_land,
    settle_merge, submit_merge,
};
use super::model::{JOB_TERMINAL_STATE, Manifest};
use super::open::{FIX_PREFIX, relaunch_node, run_is_live};
use super::relaunch::{budget_refusal, refusal_question};
use super::rescue;
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
/// How long a conflict may wait for a resolver run before the owner is asked instead.
pub(super) const RESOLVER_GRACE: Duration = Duration::from_secs(15 * 60);
/// Longest tail of the gate's findings that goes into a relaunch prompt.
const FINDINGS_MAX: usize = 3000;

/// What a gate measured for one branch. `Err` from a [`GateFn`] means nothing was measured (see
/// [`NotMeasured`]), so the branch stays where it was. `retry::apply_gate_answer` records it:
/// `Green` and `Red` as measured, `NothingRan` as green unmeasured, `Inherited` as green with the
/// base's incident named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GateVerdict {
    Green,
    Red {
        summary: String,
    },
    /// The test map selects no unit for the branch's diff: nothing ran, so nothing could fail.
    NothingRan,
    /// Red, but every failed test is already red on the base, under the master incident `note`
    /// names (`verify::inherit::inherited_red`): not the branch's failure, so no fix is spent on it.
    Inherited {
        note: String,
    },
}

/// Why a gate has no verdict for a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NotMeasured {
    /// The ticket is still running and a later tick reads it. Waiting is never counted.
    Running(String),
    /// The gate ended without measuring anything: the ticket errored or vanished, the submission
    /// was refused, the worktree could not be read. Counted against `retry::GATE_ATTEMPTS`.
    Failed(String),
}

impl From<String> for NotMeasured {
    fn from(reason: String) -> Self {
        NotMeasured::Failed(reason)
    }
}

pub(super) type GateFuture = Pin<Box<dyn Future<Output = Result<GateVerdict, NotMeasured>> + Send>>;

/// Runs the gate of one branch: called with `(job id, branch name)`. The driver spawns the future
/// and never awaits it inside a tick.
pub(super) type GateFn = Arc<dyn Fn(String, String) -> GateFuture + Send + Sync>;

/// A branch to park after the job's lock is released: `(node id, question)`.
pub(super) type Park = (String, String);

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

pub(super) fn verdict_of(verdict: ScopeVerdict) -> Result<GateVerdict, NotMeasured> {
    match verdict {
        ScopeVerdict::Measured(GateOutcome::Passed) => Ok(GateVerdict::Green),
        ScopeVerdict::Measured(GateOutcome::Failed { output, .. }) => {
            match super::retry::infrastructure_failure(&output) {
                Some(reason) => Err(NotMeasured::Failed(reason)),
                None => Ok(GateVerdict::Red { summary: output }),
            }
        }
        ScopeVerdict::Measured(GateOutcome::Errored { reason }) => Err(NotMeasured::Failed(reason)),
        ScopeVerdict::NothingRan => Ok(GateVerdict::NothingRan),
        ScopeVerdict::Unavailable => Err(NotMeasured::Failed(
            "the scope gate could not be submitted, so nothing was measured".to_owned(),
        )),
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
    if let Err(error) = super::pending_start::retry_pending_starts(state, job_id).await {
        tracing::warn!(job = job_id, %error, "tree driver could not retry the pending starts");
    }
    // A root run the disk or git refused at launch: marked on the job, retried here every sweep.
    if let Err(error) = super::root_pending::retry(state, job_id).await {
        tracing::warn!(job = job_id, %error, "tree driver could not retry the root start");
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

    root_step(pool, &job, parks).await?;
    landing_step(state, &mut job, parks).await
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

pub(super) fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

pub(super) async fn persist(pool: &SqlitePool, job: &mut TreeJob) -> Result<(), String> {
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
            let handoff = rescue::handoff_text(pool, &node).await;
            // A leaf that only asks the controller to run the formatter is not blocked: the daemon
            // formats and the branch goes on to its gate.
            let asks_formatter = outcome == "blocked"
                && handoff
                    .as_deref()
                    .is_some_and(autofmt::blocked_asks_only_formatter);
            let mut formatted = false;
            if asks_formatter && autofmt::allowed(branch) {
                let result = autofmt::format_owned(branch).await;
                // Every attempt is counted, whatever its result, so the budget bounds retries.
                if let Some(held) = job.manifest.branches.get_mut(name) {
                    autofmt::count_run(held);
                    persist(pool, job).await?;
                }
                // A formatter that errored falls through to the rescue/park branch below.
                formatted = result.is_ok();
            }
            if !formatted {
                // One rescue run with the failure in its prompt, then the owner is asked.
                if let Some(run) = &report.run_id {
                    let suffix = rescue::stopped_suffix(outcome, handoff.as_deref());
                    if rescue::attempt(state, &node, run, &suffix).await? {
                        return Ok(());
                    }
                }
                let question = stopped_question(&node, Some(name), outcome)
                    + &super::gate_summary::reason_suffix(pool, &node).await;
                parks.push((node.clone(), question));
                return Ok(());
            }
        }
        _ => return Ok(()),
    }
    // Rust the branch owns is formatted before its gate, so a rustfmt red does not happen.
    if let Some(branch) = job.manifest.branches.get(name)
        && branch.layer != "docs"
        && autofmt::allowed(branch)
        && matches!(autofmt::format_owned(branch).await, Ok(true))
        && let Some(held) = job.manifest.branches.get_mut(name)
    {
        autofmt::count_run(held);
        persist(pool, job).await?;
    }
    let Some(branch) = job.manifest.branches.get(name) else {
        return Ok(());
    };

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
    // An orchestrator branch waits for its children: its gate runs after they merged into it.
    if !super::nesting::children_settled(&job.manifest, name) {
        return Ok(());
    }
    if super::nesting::all_children_dropped(&job.manifest, name) {
        let held = job
            .manifest
            .branches
            .get_mut(name)
            .ok_or("branch vanished")?;
        apply_transition(held, "dropped").map_err(|e| e.to_string())?;
        held.extra.insert(
            "drop_reason".to_owned(),
            Value::String("every child was dropped, so it changed nothing".to_owned()),
        );
        return persist(pool, job).await;
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
    let backing_off = job
        .manifest
        .branches
        .get(name)
        .is_some_and(|held| super::retry::gate_backing_off(&held.extra));
    if let Some(gate) = gate
        && !backing_off
    {
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
        // A repo-local `core.fsmonitor` would run a command on any call that reads the index.
        .args(["-c", "core.fsmonitor="])
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

/// Records the gate task's answer under the job's lock, then asks the park it may call for
/// outside it (`park` takes the lock itself).
async fn record_verdict(
    state: &AppState,
    job_id: &str,
    branch: &str,
    verdict: Result<GateVerdict, NotMeasured>,
) {
    if let Err(NotMeasured::Running(reason) | NotMeasured::Failed(reason)) = &verdict {
        tracing::warn!(job = job_id, branch, %reason, "the gate measured nothing");
    }
    let applied = {
        let lock = job_lock(job_id);
        let _guard = lock.lock().await;
        super::retry::apply_gate_answer(&state.pool, job_id, branch, verdict).await
    };
    match applied {
        Ok(Some((node, question))) => {
            if let Err(error) = decisions::park_or_ask(&state.pool, job_id, &node, &question).await
            {
                tracing::warn!(job = job_id, branch, %error, "could not park the ungated branch");
            }
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(job = job_id, branch, %error, "could not record the gate verdict")
        }
    }
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

    // A rustfmt-only red is deterministic: the daemon formats the branch itself and re-gates the new
    // HEAD (next tick, `at_work` sees the finished run). No leaf run, so no fix is counted.
    if autofmt::heal_red(branch, &findings).await {
        let held = job
            .manifest
            .branches
            .get_mut(name)
            .ok_or("branch vanished")?;
        autofmt::count_run(held);
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
    let step = ladder::next_step(
        Kind::Fix,
        Spent {
            retries: prior,
            ..Spent::default()
        },
    );
    if !matches!(step, Step::Retry { .. }) {
        // The ladder's diagnose rung: a rescue run, and only if that fails too the owner is asked.
        let suffix = rescue::red_suffix(&fix_suffix(&findings));
        if rescue::attempt(state, &node, &run, &suffix).await? {
            let held = job
                .manifest
                .branches
                .get_mut(name)
                .ok_or("branch vanished")?;
            apply_transition(held, "fixing").map_err(|e| e.to_string())?;
            return persist(pool, job).await;
        }
        parks.push((
            node,
            format!(
                "The gate of branch {name} failed after {prior} automatic fixes and a rescue run; last findings:\n{findings}"
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
                // `attempts` counts merge requests, the first included, so the retries are one
                // fewer. The resolver is an external run: the ladder's diagnose rung is a park.
                let conflict_step = ladder::next_step(
                    Kind::Conflict,
                    Spent {
                        retries: attempts - 1,
                        ..Spent::default()
                    },
                );
                if resolution_failed
                    && !matches!(conflict_step, Step::Retry { .. })
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

    // Not mergeable yet (its base waits, a finding is open): a base that is parked or dropped is
    // said once; anything else waits silently for this pass.
    if check_may_merge(&job.manifest, name).is_err() {
        return super::retry::note_blocked_base(pool, job, name, parks).await;
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

async fn landing_step(
    state: &AppState,
    job: &mut TreeJob,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let pool = &state.pool;
    if let Some(request) = job.manifest.extra.get("land_id").and_then(Value::as_i64) {
        let settled = settle_land(pool, job, Duration::ZERO)
            .await
            .map_err(|e| e.to_string())?;
        // A red final verify carries its failing tests into the record and the question.
        let settled = super::final_verify::with_failing_tests(pool, job, request, settled).await;
        if let landing::LandSettlement::NotLanded { reason } = &settled {
            tracing::warn!(job = %job.id, %reason, "the queue did not land the job");
        }
        return super::retry::after_settle(pool, job, settled, parks).await;
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
    // Past the allowance of failed landings the owner decides; an answer is a fresh one.
    if !super::retry::land_allowed(pool, job, parks).await? {
        return Ok(());
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
            // `land_job` recorded it as `land_refusal`; the job stays open and the refusal counts.
            tracing::warn!(job = %job.id, %reason, "the landing was refused");
            super::retry::count_land_attempt(pool, job, &reason, parks).await
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
pub(super) mod tests {
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

    /// Fix relaunches before the rescue run, and conflict merge requests before the park, as the
    /// ladder sets them.
    const FIX_CAP: i64 = ladder::retry_cap(Kind::Fix);
    const CONFLICT_ATTEMPTS: i64 = ladder::retry_cap(Kind::Conflict) + 1;

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
    use crate::tree_job::retry::{
        GATE_ATTEMPTS, GATE_FAILURES, LAND_ATTEMPTS, LAND_ATTEMPTS_KEY, LAND_ESCALATED_SINCE,
        gate_backing_off,
    };
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

    pub(crate) fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    pub(crate) fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
        std::fs::write(repo.join(file), text).expect("write");
        assert!(git_in(repo, &["add", "-A"]));
        assert!(git_in(repo, &["commit", "-q", "-m", message]));
    }

    /// `master`; `feat/tree` one commit ahead of it; `tree/api`, `tree/web` and `tree/storage`, each
    /// one commit ahead of `feat/tree` in a file of its own so every merge is clean; `tree/root-empty`
    /// equal to `feat/tree` and `tree/root-full` one commit ahead. The main checkout stands on
    /// `feat/tree`, the target of a branch merge.
    pub(crate) fn tree_repo() -> (tempfile::TempDir, PathBuf) {
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

    pub(crate) fn code_branch(name: &str, state: &str, node: Option<&str>) -> Branch {
        let mut branch = Branch::new(name, &[format!("{name}/**")], "backend", None);
        branch.state = state.to_owned();
        branch.git_branch = Some(format!("tree/{name}"));
        branch.node_id = node.map(str::to_owned);
        branch
    }

    pub(crate) fn merged_branch(name: &str, node: Option<&str>) -> Branch {
        let mut branch = code_branch(name, "merged", node);
        branch
            .extra
            .insert("merge_sha".to_owned(), json!("0123456789abcdef"));
        branch
    }

    pub(crate) fn root_node_of(job: &str) -> String {
        format!("root-{job}")
    }

    pub(crate) fn red(summary: &str) -> GateVerdict {
        GateVerdict::Red {
            summary: summary.to_owned(),
        }
    }

    // ---- fixtures: the scripted gate -------------------------------------------------------------

    /// A gate that records its calls and answers from a script. With `release` set, every call waits
    /// on it first, which is how a test keeps a gate in progress.
    #[derive(Clone)]
    pub(crate) struct FakeGate {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        script: Arc<Mutex<VecDeque<Result<GateVerdict, NotMeasured>>>>,
        release: Option<Arc<Notify>>,
    }

    impl FakeGate {
        pub(crate) fn answering(script: Vec<GateVerdict>) -> FakeGate {
            FakeGate::scripted(script.into_iter().map(Ok).collect())
        }

        /// A gate that also answers "nothing measured". Once the script runs out it answers that
        /// its ticket is still running, which the driver only waits on.
        pub(crate) fn scripted(script: Vec<Result<GateVerdict, NotMeasured>>) -> FakeGate {
            FakeGate {
                calls: Arc::default(),
                script: Arc::new(Mutex::new(script.into())),
                release: None,
            }
        }

        pub(crate) fn held(verdict: GateVerdict) -> (FakeGate, Arc<Notify>) {
            let release = Arc::new(Notify::new());
            let mut gate = FakeGate::answering(vec![verdict]);
            gate.release = Some(release.clone());
            (gate, release)
        }

        pub(crate) fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }

        pub(crate) fn gate(&self) -> GateFn {
            let fake = self.clone();
            Arc::new(move |job: String, branch: String| -> GateFuture {
                let fake = fake.clone();
                Box::pin(async move {
                    fake.calls.lock().unwrap().push((job, branch));
                    if let Some(release) = &fake.release {
                        release.notified().await;
                    }
                    let next = fake.script.lock().unwrap().pop_front();
                    next.unwrap_or_else(|| {
                        Err(NotMeasured::Running("the gate script ran out".to_owned()))
                    })
                })
            })
        }
    }

    pub(crate) async fn wait_calls(gate: &FakeGate, count: usize) {
        // Hang guard only: a loaded machine can be slow, so wait for the event, not a budget.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while gate.calls().len() < count && std::time::Instant::now() < deadline {
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
    pub(crate) struct Fixture {
        pub(crate) state: AppState,
        pub(crate) repo: PathBuf,
        /// The queue's repository key: what `drain_once` claims by.
        pub(crate) key: String,
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
        pub(crate) async fn new() -> Fixture {
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

        pub(crate) fn resolved(&self) -> ResolvedRepo {
            ResolvedRepo::synthetic(PROJECT, &self.repo.to_string_lossy(), &self.key)
        }

        /// Saves the job with its root node (running, no run yet). `landing` is the manifest's
        /// landing mode; `None` leaves it absent.
        pub(crate) async fn seed_job(
            &self,
            id: &str,
            branches: Vec<Branch>,
            landing: Option<&str>,
        ) {
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

        pub(crate) async fn add_node(&self, job: &str, id: &str, kind: &str) {
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
        pub(crate) async fn run_ended(&self, node: &str, report: Option<&str>) -> i64 {
            self.run_with(node, "interrupted", report).await
        }

        pub(crate) async fn run_live(&self, node: &str) -> i64 {
            self.run_with(node, "running", None).await
        }

        pub(crate) async fn run_with(&self, node: &str, status: &str, report: Option<&str>) -> i64 {
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

        pub(crate) async fn root_done(&self, job: &str) {
            self.run_ended(&root_node_of(job), Some("done")).await;
        }

        /// The node's agent, started again by the driver, ends and reports `done`.
        pub(crate) async fn relaunched_run_reports_done(&self, node: &str) {
            self.relaunched_run_reports(node, "done").await;
        }

        /// The node's agent, started again by the driver, ends and reports `result`.
        pub(crate) async fn relaunched_run_reports(&self, node: &str, result: &str) {
            let run = self.current_run(node).await;
            sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?")
                .bind(run)
                .execute(&self.state.pool)
                .await
                .unwrap();
            node_run::apply(&self.state.pool, node, "report", &run.to_string(), result)
                .await
                .unwrap();
        }

        pub(crate) async fn current_run(&self, node: &str) -> i64 {
            sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
                .bind(node)
                .fetch_one(&self.state.pool)
                .await
                .unwrap()
        }

        pub(crate) async fn prompt_of(&self, run: i64) -> String {
            sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
                .bind(run)
                .fetch_one(&self.state.pool)
                .await
                .unwrap()
        }

        pub(crate) async fn launches(&self, node: &str, prefix: &str) -> i64 {
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

        pub(crate) async fn save_launch(&self, node: &str) {
            self.save_launch_keyed(node, "initial").await;
        }

        pub(crate) async fn save_launch_keyed(&self, node: &str, key: &str) {
            open::save_launch(
                &self.state.pool,
                &StoredLaunch {
                    node_id: node.to_owned(),
                    key: key.to_owned(),
                    prompt: "build the api".to_owned(),
                    project_id: None,
                    cwd: None,
                    mode: "shadow".to_owned(),
                },
            )
            .await
            .unwrap();
        }

        pub(crate) async fn job(&self, id: &str) -> TreeJob {
            store::load(&self.state.pool, id)
                .await
                .unwrap()
                .expect("the job is stored")
        }

        pub(crate) async fn branch(&self, job: &str, name: &str) -> Branch {
            self.job(job)
                .await
                .manifest
                .branches
                .get(name)
                .cloned()
                .expect("the branch is in the manifest")
        }

        pub(crate) async fn set_branch_state(&self, job: &str, name: &str, state: &str) {
            let mut stored = self.job(job).await;
            stored
                .manifest
                .branches
                .get_mut(name)
                .expect("the branch is in the manifest")
                .state = state.to_owned();
            store::update(&self.state.pool, &stored).await.unwrap();
        }

        pub(crate) async fn node(&self, id: &str) -> work_nodes::Node {
            work_nodes::get(&self.state.pool, id).await.unwrap()
        }

        pub(crate) async fn open_jobs(&self) -> Vec<String> {
            store::list_non_terminal(&self.state.pool)
                .await
                .unwrap()
                .into_iter()
                .map(|job| job.id)
                .collect()
        }

        pub(crate) async fn requests(&self) -> i64 {
            sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
                .fetch_one(&self.state.pool)
                .await
                .unwrap()
        }

        pub(crate) async fn drain(&self) {
            assert!(
                drain_once(&self.state.pool, &self.key, &GitExecutor::default()).await,
                "the queue had a request to run"
            );
        }

        pub(crate) async fn tick_within(&self, job: &str, gate: &FakeGate, limit: Duration) {
            tokio::time::timeout(limit, advance_job_with_gate(&self.state, job, gate.gate()))
                .await
                .expect("the tick returned in time");
        }

        pub(crate) async fn tick(&self, job: &str, gate: &FakeGate) {
            self.tick_within(job, gate, Duration::from_secs(30)).await;
        }

        /// Waits for the spawned gate to settle the branch, then lets its task finish.
        pub(crate) async fn wait_branch_state(&self, job: &str, name: &str, want: &str) {
            // Hang guard only: wait for the state, not for a short budget.
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            let seen = loop {
                let state = self.branch(job, name).await.state;
                if state == want || std::time::Instant::now() >= deadline {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert_eq!(seen, want, "branch {name} of {job} never reached {want}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }

        /// From now on every write to a watched table leaves a line in `write_log`.
        pub(crate) async fn watch_writes(&self) {
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

        pub(crate) async fn writes(&self) -> Vec<String> {
            sqlx::query_scalar("SELECT what FROM write_log ORDER BY rowid")
                .fetch_all(&self.state.pool)
                .await
                .unwrap()
        }

        pub(crate) async fn assert_not_landing(&self, job: &str) {
            let stored = self.job(job).await;
            assert!(
                !stored.manifest.extra.contains_key("land_id"),
                "no landing admitted"
            );
            assert_eq!(stored.state, "open");
            assert_eq!(self.requests().await, 0, "nothing was queued");
        }

        /// The questions of the open decisions the job asked about `node`.
        pub(crate) async fn open_questions(&self, job: &str, node: &str) -> Vec<String> {
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
        pub(crate) async fn exhaust_budget(&self, job: &str, node: &str, limit: f64, spent: f64) {
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

    /// Waits for the spawned gate to count `want` gates in a row that measured nothing, then lets
    /// its task finish.
    async fn wait_gate_failures(fx: &Fixture, job: &str, name: &str, want: i64) {
        // Hang guard only: wait for the count, not for a short budget.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let seen = loop {
            let count = fx
                .branch(job, name)
                .await
                .extra
                .get(GATE_FAILURES)
                .and_then(Value::as_i64);
            if count == Some(want) || std::time::Instant::now() >= deadline {
                break count;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(
            seen,
            Some(want),
            "branch {name} of {job} never counted {want} failure(s)"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    /// Ends the backoff an unmeasured gate left on the branch, so the next tick gates at once.
    async fn skip_gate_backoff(fx: &Fixture, job: &str, name: &str) {
        let mut stored = fx.job(job).await;
        stored
            .manifest
            .branches
            .get_mut(name)
            .expect("the branch is in the manifest")
            .extra
            .remove("gate_backoff_until");
        store::update(&fx.state.pool, &stored).await.unwrap();
    }

    /// A branch at work with a `done` report, gated on every tick.
    async fn gated_job(fx: &Fixture, job: &str, node: &str) {
        fx.seed_job(job, vec![code_branch("api", "building", Some(node))], None)
            .await;
        fx.add_node(job, node, "orchestrator").await;
        fx.run_ended(node, Some("done")).await;
    }

    #[tokio::test]
    async fn a_gate_that_keeps_measuring_nothing_parks_after_the_cap() {
        const JOB: &str = "drv-unmeasured";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        gated_job(&fx, JOB, NODE).await;
        let unavailable = || {
            Err(NotMeasured::Failed(
                "the scope gate could not be submitted: queue-down".to_owned(),
            ))
        };
        let gate = FakeGate::scripted((0..GATE_ATTEMPTS).map(|_| unavailable()).collect());

        for attempt in 1..GATE_ATTEMPTS {
            fx.tick(JOB, &gate).await;
            wait_calls(&gate, attempt as usize).await;
            wait_gate_failures(&fx, JOB, "api", attempt).await;
            assert_eq!(fx.branch(JOB, "api").await.state, "building");
            assert!(fx.open_questions(JOB, NODE).await.is_empty());
            // Each unmeasured gate leaves a backoff of minutes; further ticks inside it do not gate.
            let held = fx.branch(JOB, "api").await;
            assert!(gate_backing_off(&held.extra), "attempt {attempt}");
            fx.tick(JOB, &gate).await;
            assert_eq!(
                gate.calls().len(),
                attempt as usize,
                "gated inside the backoff"
            );
            skip_gate_backoff(&fx, JOB, "api").await;
        }
        fx.tick(JOB, &gate).await;
        wait_calls(&gate, GATE_ATTEMPTS as usize).await;
        fx.wait_branch_state(JOB, "api", "parked").await;

        let questions = fx.open_questions(JOB, NODE).await;
        assert_eq!(questions.len(), 1, "the owner is asked once: {questions:?}");
        assert!(questions[0].contains("queue-down"), "{:?}", questions[0]);
        let parked = fx.branch(JOB, "api").await;
        assert!(
            !parked.extra.contains_key(GATE_FAILURES),
            "an answer starts a fresh count"
        );
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;
        assert_eq!(
            gate.calls().len(),
            GATE_ATTEMPTS as usize,
            "a parked branch is not gated again"
        );
        assert_eq!(fx.open_questions(JOB, NODE).await.len(), 1);
    }

    #[tokio::test]
    async fn a_gate_starved_past_the_six_hour_cap_parks_and_asks_once() {
        const JOB: &str = "drv-load-cap";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        let mut branch = code_branch("api", "building", Some(NODE));
        branch.extra.insert(
            "gate_load_waited_secs".to_owned(),
            json!(crate::tree_job::retry::GATE_LOAD_CAP_SECS - 1),
        );
        fx.seed_job(JOB, vec![branch], None).await;
        fx.add_node(JOB, NODE, "orchestrator").await;
        fx.run_ended(NODE, Some("done")).await;
        let gate = FakeGate::scripted(vec![Err(NotMeasured::Failed(
            "database is locked".to_owned(),
        ))]);

        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 1).await;
        fx.wait_branch_state(JOB, "api", "parked").await;

        let questions = fx.open_questions(JOB, NODE).await;
        assert_eq!(questions.len(), 1, "{questions:?}");
        assert!(
            questions[0].contains("database is locked"),
            "{:?}",
            questions[0]
        );
        let parked = fx.branch(JOB, "api").await;
        assert!(
            !parked.extra.contains_key(GATE_FAILURES),
            "no attempt was spent"
        );
        assert!(!parked.extra.contains_key("gate_load_waited_secs"));
    }

    #[tokio::test]
    async fn a_gate_starved_by_load_backs_off_without_spending_attempts_or_asking() {
        const JOB: &str = "drv-load-backoff";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        gated_job(&fx, JOB, NODE).await;
        let starved = || {
            Err(NotMeasured::Failed(
                "pool timed out while waiting for an open connection".to_owned(),
            ))
        };
        let gate = FakeGate::scripted((0..GATE_ATTEMPTS + 2).map(|_| starved()).collect());

        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 1).await;
        let mut until = None;
        for _ in 0..300 {
            until = fx
                .branch(JOB, "api")
                .await
                .extra
                .get("gate_backoff_until")
                .cloned();
            if until.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(until.is_some(), "a backoff is scheduled");
        // Further ticks inside the backoff do not gate again.
        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_eq!(gate.calls().len(), 1, "the gate waits out its backoff");
        let api = fx.branch(JOB, "api").await;
        assert_eq!(api.state, "building");
        assert!(
            !api.extra.contains_key(GATE_FAILURES),
            "load does not spend the attempts"
        );
        assert!(fx.open_questions(JOB, NODE).await.is_empty());
        assert_eq!(count_of(&fx, "decisions").await, 0, "nobody was asked");
    }

    #[tokio::test]
    async fn a_gate_that_hit_the_time_ceiling_is_remeasured_bigger_and_counts_as_nothing() {
        const JOB: &str = "drv-time-ceiling";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        gated_job(&fx, JOB, NODE).await;
        let hit = Err(NotMeasured::Failed(crate::verify::time_ceiling_reason(
            1800, 4,
        )));
        let gate = FakeGate::scripted(vec![hit]);

        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 1).await;
        let mut api = fx.branch(JOB, "api").await;
        for _ in 0..300 {
            api = fx.branch(JOB, "api").await;
            if api.extra.contains_key("gate_ceiling_hits") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(api.extra["gate_ceiling_hits"], json!(1), "{:?}", api.extra);
        assert_eq!(api.extra["gate_ceiling_units"], json!(4));
        assert_eq!(api.state, "building", "not a red");
        assert!(
            !api.extra.contains_key(GATE_FAILURES),
            "not an attempt either"
        );
        assert!(fx.open_questions(JOB, NODE).await.is_empty());
        assert_eq!(
            crate::tree_job::retry::branch_ceiling_secs(&api.extra, 1800),
            5400
        );
    }

    #[test]
    fn load_reasons_are_told_from_genuine_failures() {
        use super::super::retry::is_load_reason;
        assert!(is_load_reason(
            "pool timed out while waiting for an open connection"
        ));
        assert!(is_load_reason("the heavy broker is full"));
        assert!(is_load_reason("held: disk pressure"));
        assert!(is_load_reason("failed to spawn cargo"));
        assert!(!is_load_reason(
            "the scope gate could not be submitted: queue-down"
        ));
        assert!(!is_load_reason("scope verification ticket 7 vanished"));
    }

    const SCCACHE_CRASH: &str = "== core (scope)\n   Compiling stacker v0.1.15\nerror: could not compile `stacker` (lib)\n\nCaused by:\n  process didn't exit successfully: `sccache rustc --crate-name stacker` (exit status: 2)\n";

    #[test]
    fn a_compiler_dying_without_a_diagnostic_is_unmeasured_load_not_a_red() {
        use super::super::retry::is_load_reason;
        let verdict = verdict_of(ScopeVerdict::Measured(GateOutcome::Failed {
            exit_code: 101,
            output: SCCACHE_CRASH.to_owned(),
        }));
        match verdict {
            Err(NotMeasured::Failed(reason)) => {
                assert!(is_load_reason(&reason), "{reason}");
            }
            other => panic!("expected an unmeasured gate, got {other:?}"),
        }
    }

    #[test]
    fn a_compile_error_with_a_diagnostic_stays_a_red() {
        let real = "== core (scope)\nerror[E0425]: cannot find value `x` in this scope\n --> src/a.rs:3:5\nerror: could not compile `core` (lib) due to 1 previous error\n\nCaused by:\n  process didn't exit successfully: `rustc` (exit status: 1)\n";
        let plain = "== core (scope)\nerror: expected one of `;`, found `}`\nerror: could not compile `core` (lib)\n  process didn't exit successfully: `rustc` (exit status: 1)\n";
        for output in [real, plain] {
            let verdict = verdict_of(ScopeVerdict::Measured(GateOutcome::Failed {
                exit_code: 101,
                output: output.to_owned(),
            }));
            assert!(
                matches!(verdict, Ok(GateVerdict::Red { .. })),
                "{verdict:?}"
            );
        }
    }

    #[test]
    fn sccache_errors_are_load_reasons() {
        use super::super::retry::is_load_reason;
        assert!(is_load_reason(
            "sccache: error: Failed to send data to server"
        ));
    }

    #[tokio::test]
    async fn a_gate_that_selects_no_unit_turns_the_branch_green_unmeasured() {
        const JOB: &str = "drv-nothing-ran";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        gated_job(&fx, JOB, NODE).await;
        let gate = FakeGate::answering(vec![GateVerdict::NothingRan]);

        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 1).await;
        fx.wait_branch_state(JOB, "api", "green").await;

        let api = fx.branch(JOB, "api").await;
        let last = api.gates.last().expect("the verdict is recorded");
        assert_eq!(last["verdict"], json!("green"));
        assert_eq!(last["unmeasured"], json!(true), "{last}");
        fx.tick(JOB, &gate).await;
        assert_eq!(fx.requests().await, 1, "the branch goes on to its merge");
        assert_eq!(gate.calls().len(), 1);
        assert_eq!(count_of(&fx, "decisions").await, 0, "nobody was asked");
    }

    #[tokio::test]
    async fn an_inherited_red_turns_the_branch_green_without_spending_a_fix() {
        const JOB: &str = "drv-inherited";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        gated_job(&fx, JOB, NODE).await;
        let gate = FakeGate::answering(vec![GateVerdict::Inherited {
            note: "incident #4 (open, nobody has claimed it)".to_owned(),
        }]);

        fx.tick(JOB, &gate).await;
        wait_calls(&gate, 1).await;
        fx.wait_branch_state(JOB, "api", "green").await;

        let api = fx.branch(JOB, "api").await;
        let last = api.gates.last().expect("the verdict is recorded");
        assert_eq!(last["verdict"], json!("green"));
        assert!(
            last["inherited"]
                .as_str()
                .is_some_and(|note| note.contains("#4")),
            "{last}"
        );
        let fixes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tree_launches WHERE node_id = ? AND key LIKE ?",
        )
        .bind(NODE)
        .bind(format!("{FIX_PREFIX}%"))
        .fetch_one(&fx.state.pool)
        .await
        .unwrap();
        assert_eq!(fixes, 0, "no fix is spent on the base's red");
        fx.tick(JOB, &gate).await;
        assert_eq!(fx.requests().await, 1, "the branch goes on to its merge");
        assert_eq!(gate.calls().len(), 1);
        assert_eq!(count_of(&fx, "decisions").await, 0, "nobody was asked");
    }

    #[tokio::test]
    async fn a_gate_still_running_keeps_waiting_without_counting() {
        const JOB: &str = "drv-gate-running";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        gated_job(&fx, JOB, NODE).await;
        let running = || {
            Err(NotMeasured::Running(
                "scope verification ticket 7 is still running".to_owned(),
            ))
        };
        let attempts = GATE_ATTEMPTS as usize + 2;
        let gate = FakeGate::scripted((0..attempts).map(|_| running()).collect());

        for attempt in 1..=attempts {
            fx.tick(JOB, &gate).await;
            wait_calls(&gate, attempt).await;
            // Lets the gate's task end, so the next tick may start another.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let api = fx.branch(JOB, "api").await;
        assert_eq!(api.state, "building", "a running gate is waited on");
        assert!(
            !api.extra.contains_key(GATE_FAILURES),
            "waiting is never counted"
        );
        assert!(
            api.gates.is_empty(),
            "nothing was recorded: {:?}",
            api.gates
        );
        assert_eq!(count_of(&fx, "decisions").await, 0, "nobody was asked");
    }

    #[tokio::test]
    async fn a_red_gate_relaunches_up_to_the_ladder_cap_then_rescues_once_and_then_parks() {
        const JOB: &str = "drv-fix";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "building", Some(NODE))], None)
            .await;
        fx.add_node(JOB, NODE, "orchestrator").await;
        fx.run_ended(NODE, Some("done")).await;
        fx.save_launch(NODE).await;
        let cap = FIX_CAP as usize;
        let gate = FakeGate::answering(
            (1..=cap + 2)
                .map(|n| red(&format!("failure: boom-{n}-end")))
                .collect(),
        );

        for attempt in 1..=cap {
            fx.tick(JOB, &gate).await;
            wait_calls(&gate, attempt).await;
            fx.wait_branch_state(JOB, "api", "red").await;

            fx.tick(JOB, &gate).await;

            assert_eq!(fx.branch(JOB, "api").await.state, "fixing");
            assert_eq!(fx.launches(NODE, "fix-").await, attempt as i64);
            let prompt = fx.prompt_of(fx.current_run(NODE).await).await;
            assert!(
                prompt.contains(&format!("boom-{attempt}-end")),
                "the relaunch carries the findings: {prompt:?}"
            );
            fx.relaunched_run_reports_done(NODE).await;
        }

        // The next red is past the cap: one rescue run with the failure in its prompt, no park.
        fx.tick(JOB, &gate).await;
        wait_calls(&gate, cap + 1).await;
        fx.wait_branch_state(JOB, "api", "red").await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "fixing");
        assert_eq!(fx.launches(NODE, "rescue-").await, 1);
        assert!(fx.open_questions(JOB, NODE).await.is_empty());
        let prompt = fx.prompt_of(fx.current_run(NODE).await).await;
        assert!(
            prompt.contains(&format!("boom-{}-end", cap + 1)),
            "the rescue carries the failure: {prompt:?}"
        );
        fx.relaunched_run_reports_done(NODE).await;

        // The rescue's work is red too: now the owner is asked, and no second rescue starts.
        fx.tick(JOB, &gate).await;
        wait_calls(&gate, cap + 2).await;
        fx.wait_branch_state(JOB, "api", "red").await;
        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        assert_eq!(
            fx.launches(NODE, "fix-").await,
            FIX_CAP,
            "the cap is the ladder's fix retries"
        );
        assert_eq!(fx.launches(NODE, "rescue-").await, 1, "one rescue only");
        assert_eq!(gate.calls().len(), cap + 2);
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
        assert!(
            question.contains(&format!("boom-{}-end", cap + 2)),
            "{question:?}"
        );
    }

    /// Drives `api` through the ladder's fix retries, a rescue run, then one more red gate that parks it on the
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
        for attempt in 1..=FIX_CAP as usize {
            fx.tick(job, gate).await;
            wait_calls(gate, attempt).await;
            fx.wait_branch_state(job, "api", "red").await;
            fx.tick(job, gate).await;
            assert_eq!(fx.branch(job, "api").await.state, "fixing");
            fx.relaunched_run_reports_done(node).await;
        }
        // The next red gets its one rescue; the one after parks.
        fx.tick(job, gate).await;
        wait_calls(gate, FIX_CAP as usize + 1).await;
        fx.wait_branch_state(job, "api", "red").await;
        fx.tick(job, gate).await;
        assert_eq!(fx.branch(job, "api").await.state, "fixing");
        fx.relaunched_run_reports_done(node).await;
        fx.tick(job, gate).await;
        wait_calls(gate, FIX_CAP as usize + 2).await;
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

    /// The verdicts `answered_after_the_fix_cap` consumes (a red per fix, the rescue's and the
    /// parking one), then `after` for the gate that follows the owner's answer.
    fn fix_cap_reds(after: GateVerdict) -> Vec<GateVerdict> {
        let mut verdicts: Vec<GateVerdict> = (1..=FIX_CAP + 2)
            .map(|n| red(&format!("boom-{n}")))
            .collect();
        verdicts.push(after);
        verdicts
    }

    /// An answered fix-cap park must not park again: the relaunched node's `done` report is gated.
    #[tokio::test]
    async fn an_answered_fix_cap_park_gates_the_relaunched_nodes_work() {
        const JOB: &str = "drv-fix-answered";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        let gate = FakeGate::answering(fix_cap_reds(GateVerdict::Green));
        answered_after_the_fix_cap(&fx, JOB, NODE, &gate).await;
        fx.relaunched_run_reports_done(NODE).await;

        fx.tick(JOB, &gate).await;

        wait_calls(&gate, FIX_CAP as usize + 3).await;
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
        let gate = FakeGate::answering(fix_cap_reds(red("boom-after")));
        answered_after_the_fix_cap(&fx, JOB, NODE, &gate).await;
        fx.relaunched_run_reports_done(NODE).await;
        fx.tick(JOB, &gate).await;
        wait_calls(&gate, FIX_CAP as usize + 3).await;
        fx.wait_branch_state(JOB, "api", "red").await;

        fx.tick(JOB, &gate).await;

        assert_eq!(fx.branch(JOB, "api").await.state, "fixing");
        assert_eq!(fx.launches(NODE, "fix-").await, FIX_CAP + 1);
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
        assert_eq!(
            feed_lines_naming(&fx, "waits on its base").await,
            0,
            "a base still on its way is not announced"
        );
    }

    /// Parks `storage` on decision `decision`, from green.
    async fn park_the_base(fx: &Fixture, job: &str, decision: &str) {
        let mut stored = fx.job(job).await;
        let base = stored
            .manifest
            .branches
            .get_mut("storage")
            .expect("the base is in the manifest");
        base.state = "parked".to_owned();
        base.park = Some(json!({ "prev_state": "green", "decision_id": decision }));
        store::update(&fx.state.pool, &stored).await.unwrap();
    }

    #[tokio::test]
    async fn a_green_branch_on_a_parked_base_is_announced_once() {
        const JOB: &str = "drv-base-parked";
        let fx = Fixture::new().await;
        let mut dependent = code_branch("api", "green", None);
        dependent.base = Some("storage".to_owned());
        fx.seed_job(
            JOB,
            vec![dependent, code_branch("storage", "green", None)],
            None,
        )
        .await;
        park_the_base(&fx, JOB, "d-1").await;
        let gate = FakeGate::answering(vec![]);

        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }

        let line = "branch api is green but waits on its base storage";
        assert_eq!(feed_lines_naming(&fx, line).await, 1, "announced once");
        assert_eq!(
            feed_lines_naming(&fx, "d-1").await,
            1,
            "naming the base's decision"
        );
        let waiting = fx.branch(JOB, "api").await;
        assert_eq!(waiting.state, "green", "the dependent keeps waiting");
        assert_eq!(fx.requests().await, 0, "nothing was queued");
        assert_eq!(
            count_of(&fx, "decisions").await,
            0,
            "the base's question is the one asked"
        );

        // The base parks again on another question: that wait is announced too, once.
        park_the_base(&fx, JOB, "d-2").await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;
        assert_eq!(feed_lines_naming(&fx, line).await, 2);
    }

    #[tokio::test]
    async fn a_green_branch_on_a_dropped_base_parks_once() {
        const JOB: &str = "drv-base-dropped";
        const NODE: &str = "api-node";
        let fx = Fixture::new().await;
        let mut dependent = code_branch("api", "green", Some(NODE));
        dependent.base = Some("storage".to_owned());
        fx.seed_job(
            JOB,
            vec![dependent, code_branch("storage", "dropped", None)],
            None,
        )
        .await;
        fx.add_node(JOB, NODE, "orchestrator").await;
        let gate = FakeGate::answering(vec![]);

        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }

        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
        let questions = fx.open_questions(JOB, NODE).await;
        assert_eq!(questions.len(), 1, "the owner is asked once: {questions:?}");
        assert!(questions[0].contains("dropped"), "{:?}", questions[0]);
        assert_eq!(fx.requests().await, 0, "nothing was queued");
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

    /// The `count` of the job's `land_attempts`.
    async fn land_attempts(fx: &Fixture, job: &str) -> Option<i64> {
        fx.job(job)
            .await
            .manifest
            .extra
            .get(LAND_ATTEMPTS_KEY)
            .and_then(|attempts| attempts.get("count"))
            .and_then(Value::as_i64)
    }

    #[tokio::test]
    async fn a_land_refusal_parks_after_the_cap() {
        const JOB: &str = "drv-refusal-cap";
        let fx = Fixture::new().await;
        // `feat/tree` is `master`, so every landing is refused as nothing to land.
        assert!(git_in(&fx.repo, &["checkout", "-q", "master"]));
        assert!(git_in(&fx.repo, &["branch", "-f", "feat/tree", "master"]));
        fx.seed_job(JOB, vec![merged_branch("api", None)], Some("integration"))
            .await;
        fx.root_done(JOB).await;
        let gate = FakeGate::answering(vec![]);
        let root = root_node_of(JOB);

        for attempt in 1..LAND_ATTEMPTS {
            fx.tick(JOB, &gate).await;
            assert_eq!(land_attempts(&fx, JOB).await, Some(attempt));
            assert!(fx.open_questions(JOB, &root).await.is_empty());
        }
        for _ in 0..4 {
            fx.tick(JOB, &gate).await;
        }

        let questions = fx.open_questions(JOB, &root).await;
        assert_eq!(questions.len(), 1, "the owner is asked once: {questions:?}");
        assert!(
            questions[0].contains("nothing to land"),
            "{:?}",
            questions[0]
        );
        assert_eq!(
            land_attempts(&fx, JOB).await,
            Some(LAND_ATTEMPTS),
            "no landing is tried past the cap"
        );
        let stored = fx.job(JOB).await;
        assert_eq!(stored.state, "open");
        assert_eq!(fx.requests().await, 0);

        // The owner's answer gives the landing a fresh allowance.
        let decision: String = sqlx::query_scalar(
            "SELECT id FROM decisions WHERE tree_id = ? AND node_id = ? AND status = ?",
        )
        .bind(JOB)
        .bind(&root)
        .bind(decisions::STATUS_OPEN)
        .fetch_one(&fx.state.pool)
        .await
        .unwrap();
        decisions::answer(&fx.state.pool, &decision, "try once more", "shell")
            .await
            .unwrap();
        fx.tick(JOB, &gate).await;
        assert_eq!(
            land_attempts(&fx, JOB).await,
            Some(1),
            "an answered cap starts a fresh count"
        );
    }

    /// An integration job whose landing the queue admits: `api` merged, the root done.
    async fn landable_job(fx: &Fixture, job: &str) {
        assert!(git_in(&fx.repo, &["checkout", "-q", "master"]));
        fx.seed_job(job, vec![merged_branch("api", None)], Some("integration"))
            .await;
        fx.root_done(job).await;
    }

    /// Ends the job's queued landing as `status` with `reason`, as the queue would.
    async fn end_the_landing(fx: &Fixture, job: &str, status: &str, reason: &str) {
        let id = fx
            .job(job)
            .await
            .manifest
            .extra
            .get("land_id")
            .and_then(Value::as_i64)
            .expect("a landing is queued");
        sqlx::query(
            "UPDATE vcs_requests SET status = ?, failure_reason = ?, finished_at = ? WHERE id = ?",
        )
        .bind(status)
        .bind(reason)
        .bind(STAMP)
        .bind(id)
        .execute(&fx.state.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_failed_land_parks_after_the_cap() {
        const JOB: &str = "drv-land-fails";
        let fx = Fixture::new().await;
        landable_job(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        let root = root_node_of(JOB);

        for attempt in 1..=LAND_ATTEMPTS {
            fx.tick(JOB, &gate).await;
            assert_eq!(fx.requests().await, attempt, "landing {attempt} is queued");
            end_the_landing(&fx, JOB, "failed", &format!("hook-refused-{attempt}")).await;
            fx.tick(JOB, &gate).await;
            assert_eq!(land_attempts(&fx, JOB).await, Some(attempt));
            if attempt < LAND_ATTEMPTS {
                assert!(fx.open_questions(JOB, &root).await.is_empty());
            }
        }
        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }

        let questions = fx.open_questions(JOB, &root).await;
        assert_eq!(questions.len(), 1, "the owner is asked once: {questions:?}");
        assert!(
            questions[0].contains(&format!("hook-refused-{LAND_ATTEMPTS}")),
            "the question carries the last failure: {:?}",
            questions[0]
        );
        assert_eq!(
            fx.requests().await,
            LAND_ATTEMPTS,
            "no landing is queued past the cap"
        );
        let stored = fx.job(JOB).await;
        assert_eq!(stored.state, "open");
        assert!(!stored.manifest.extra.contains_key("land_id"));
    }

    #[tokio::test]
    async fn an_escalated_land_no_resolver_picks_up_parks_after_the_grace() {
        const JOB: &str = "drv-land-escalated";
        let fx = Fixture::new().await;
        landable_job(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        let root = root_node_of(JOB);
        fx.tick(JOB, &gate).await;
        end_the_landing(&fx, JOB, "escalated", "the landing conflicts").await;

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        let waiting = fx.job(JOB).await;
        assert!(
            waiting.manifest.extra.contains_key("land_id"),
            "the resolver owns the escalated request"
        );
        let since = waiting.manifest.extra[LAND_ESCALATED_SINCE]
            .as_str()
            .expect("the wait for the resolver is stamped")
            .to_owned();
        assert!(fx.open_questions(JOB, &root).await.is_empty());
        assert_eq!(fx.requests().await, 1);

        // Nobody picked the request up for longer than the grace.
        let mut stored = fx.job(JOB).await;
        let aged = (chrono::Utc::now() - chrono::Duration::minutes(16)).to_rfc3339();
        assert_ne!(aged, since);
        stored
            .manifest
            .extra
            .insert(LAND_ESCALATED_SINCE.to_owned(), json!(aged));
        store::update(&fx.state.pool, &stored).await.unwrap();
        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }

        let questions = fx.open_questions(JOB, &root).await;
        assert_eq!(questions.len(), 1, "the owner is asked once: {questions:?}");
        assert!(
            questions[0].to_lowercase().contains("resolver"),
            "{:?}",
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
        assert_eq!(fx.requests().await, 1, "no further landing was queued");
        let parked = fx.job(JOB).await;
        assert!(!parked.manifest.extra.contains_key("land_id"));
        assert_eq!(parked.state, "open");
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
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            while std::time::Instant::now() < deadline {
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

    /// `api` (parent, its run done) with the given children as `(name, state)`.
    async fn parent_with_children(fx: &Fixture, job: &str, kids: &[(&str, &str)]) {
        let mut branches = vec![code_branch("api", "building", Some("api-node"))];
        for (name, state) in kids {
            let mut kid = code_branch(name, state, Some(&format!("{name}-node")));
            kid.parent = Some("api".to_owned());
            branches.push(kid);
        }
        fx.seed_job(job, branches, None).await;
        fx.add_node(job, "api-node", "orchestrator").await;
        fx.run_ended("api-node", Some("done")).await;
        for (name, _) in kids {
            let node = format!("{name}-node");
            fx.add_node(job, &node, "leaf").await;
            fx.run_live(&node).await;
        }
    }

    #[tokio::test]
    async fn a_parent_is_not_gated_while_a_child_is_building() {
        const JOB: &str = "drv-parent-waits";
        let fx = Fixture::new().await;
        parent_with_children(&fx, JOB, &[("api-x", "building")]).await;
        let gate = FakeGate::answering(vec![GateVerdict::Green]);

        fx.tick(JOB, &gate).await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(gate.calls().iter().all(|(_, branch)| branch != "api"));
        assert_eq!(fx.branch(JOB, "api").await.state, "building");
    }

    #[tokio::test]
    async fn a_parent_is_gated_once_its_children_merged() {
        const JOB: &str = "drv-parent-gated";
        let fx = Fixture::new().await;
        parent_with_children(&fx, JOB, &[("api-x", "merged")]).await;
        let gate = FakeGate::answering(vec![GateVerdict::Green]);

        fx.tick(JOB, &gate).await;

        wait_calls(&gate, 1).await;
        assert_eq!(gate.calls(), vec![(JOB.to_owned(), "api".to_owned())]);
    }

    #[tokio::test]
    async fn a_parent_whose_children_were_all_dropped_is_dropped_as_changed_nothing() {
        const JOB: &str = "drv-parent-all-dropped";
        let fx = Fixture::new().await;
        parent_with_children(&fx, JOB, &[("api-x", "dropped"), ("api-y", "dropped")]).await;
        let gate = FakeGate::answering(vec![GateVerdict::Green]);

        fx.tick(JOB, &gate).await;

        let parent = fx.branch(JOB, "api").await;
        assert_eq!(parent.state, "dropped");
        assert!(
            parent.extra["drop_reason"]
                .as_str()
                .unwrap()
                .contains("changed nothing")
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(gate.calls().is_empty(), "an empty parent is not gated");
        assert_eq!(fx.requests().await, 0, "an empty parent is not merged");
    }

    #[tokio::test]
    async fn a_parked_child_keeps_the_parent_waiting_and_the_job_unlanded() {
        const JOB: &str = "drv-parent-parked-child";
        let fx = Fixture::new().await;
        parent_with_children(&fx, JOB, &[("api-x", "parked"), ("api-y", "merged")]).await;
        let gate = FakeGate::answering(vec![GateVerdict::Green]);

        fx.tick(JOB, &gate).await;

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(gate.calls().iter().all(|(_, branch)| branch != "api"));
        assert_eq!(fx.branch(JOB, "api").await.state, "building");
        fx.assert_not_landing(JOB).await;
    }

    /// The queue publishes a child's merge onto the parent's branch, which a worktree holds open:
    /// the publish fast-forwards that worktree, so the parent's files hold the child's work and the
    /// parent's own gate then reads it.
    #[tokio::test]
    async fn a_merged_childs_files_reach_its_parents_worktree() {
        const JOB: &str = "drv-parent-worktree";
        let fx = Fixture::new().await;
        let holder = space_free_tempdir("nucleos-driver-parent-wt-");
        let parent_tree = holder.path().join("api");
        assert!(git_in(
            &fx.repo,
            &[
                "worktree",
                "add",
                "-q",
                &parent_tree.to_string_lossy(),
                "tree/api"
            ]
        ));
        // The child branch stands one commit above the parent's tip, in a file of its own.
        assert!(git_in(&fx.repo, &["branch", "tree/api-x", "tree/api"]));
        let child_tree = holder.path().join("api-x");
        assert!(git_in(
            &fx.repo,
            &[
                "worktree",
                "add",
                "-q",
                &child_tree.to_string_lossy(),
                "tree/api-x"
            ]
        ));
        commit_file(&child_tree, "api-x.txt", "from the child\n", "child");
        let child_tip = sha_of(&fx.repo, "tree/api-x");
        let feature_before = sha_of(&fx.repo, "feat/tree");

        let mut parent = code_branch("api", "building", Some("api-node"));
        parent.worktree = Some(parent_tree.to_string_lossy().into_owned());
        let mut child = code_branch("api-x", "green", Some("api-x-node"));
        child.parent = Some("api".to_owned());
        fx.seed_job(JOB, vec![parent, child], None).await;
        fx.add_node(JOB, "api-node", "orchestrator").await;
        fx.run_live("api-node").await;
        fx.add_node(JOB, "api-x-node", "leaf").await;
        fx.run_ended("api-x-node", Some("done")).await;
        let gate = FakeGate::answering(vec![]);
        assert!(!parent_tree.join("api-x.txt").exists());

        fx.tick(JOB, &gate).await;
        fx.drain().await;
        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        let merged = fx.branch(JOB, "api-x").await;
        assert_eq!(merged.state, "merged");
        let sha = merged.extra["merge_sha"].as_str().expect("the merge sha");
        assert_eq!(
            sha,
            sha_of(&fx.repo, "tree/api"),
            "the parent's branch moved"
        );
        assert_eq!(sha_of(&fx.repo, &format!("{sha}^2")), child_tip);
        assert_eq!(
            sha_of(&fx.repo, "feat/tree"),
            feature_before,
            "a child does not merge into the feature branch"
        );
        assert_eq!(
            sha_of(&parent_tree, "HEAD"),
            sha,
            "the worktree's HEAD followed the ref"
        );
        assert_eq!(
            std::fs::read_to_string(parent_tree.join("api-x.txt")).unwrap(),
            "from the child\n"
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&parent_tree)
                .args(["status", "--porcelain"])
                .output()
                .unwrap()
                .stdout
                .is_empty(),
            "the parent's worktree is clean after the publish"
        );
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
        stopped_job(fx, job, outcome).await;
        // The one rescue is already spent, so the branch parks as it did before rescues existed.
        fx.save_launch_keyed("api-node", "rescue-0").await;
    }

    /// `job_whose_branch_reported` with its rescue still unspent.
    async fn stopped_job(fx: &Fixture, job: &str, outcome: &str) {
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
    async fn a_blocked_branch_gets_one_rescue_run_before_the_owner_is_asked() {
        const JOB: &str = "drv-rescue-blocked";
        let fx = Fixture::new().await;
        stopped_job(&fx, JOB, "blocked").await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;

        assert_eq!(fx.launches("api-node", "rescue-").await, 1);
        assert!(fx.open_questions(JOB, "api-node").await.is_empty());
        assert_eq!(fx.branch(JOB, "api").await.state, "building");
        let prompt = fx.prompt_of(fx.current_run("api-node").await).await;
        assert!(prompt.contains("ended blocked"), "{prompt:?}");
        assert!(prompt.contains("# handoff"), "the last handoff: {prompt:?}");

        // The rescue run is still live: nothing more happens.
        sqlx::query("UPDATE runs SET status = 'running' WHERE id = ?")
            .bind(fx.current_run("api-node").await)
            .execute(&fx.state.pool)
            .await
            .unwrap();
        fx.tick(JOB, &gate).await;
        assert_eq!(fx.launches("api-node", "rescue-").await, 1);

        // It ends failed too: the owner is asked, once, and no second rescue starts.
        fx.relaunched_run_reports("api-node", "failed").await;
        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }
        assert_eq!(fx.launches("api-node", "rescue-").await, 1);
        assert_eq!(fx.open_questions(JOB, "api-node").await.len(), 1);
        assert_eq!(fx.branch(JOB, "api").await.state, "parked");
    }

    #[tokio::test]
    async fn an_owner_answer_gives_the_rescue_a_fresh_allowance() {
        const JOB: &str = "drv-rescue-fresh";
        let fx = Fixture::new().await;
        stopped_job(&fx, JOB, "failed").await;
        let gate = FakeGate::answering(vec![]);
        fx.tick(JOB, &gate).await;
        fx.relaunched_run_reports("api-node", "failed").await;
        fx.tick(JOB, &gate).await;
        let decision: String = sqlx::query_scalar("SELECT id FROM decisions WHERE node_id = ?")
            .bind("api-node")
            .fetch_one(&fx.state.pool)
            .await
            .unwrap();
        open::answer_and_relaunch(&fx.state, &decision, "try again", "shell")
            .await
            .unwrap();
        fx.relaunched_run_reports("api-node", "blocked").await;

        fx.tick(JOB, &gate).await;

        assert_eq!(fx.launches("api-node", "rescue-").await, 2);
        assert!(fx.open_questions(JOB, "api-node").await.is_empty());
    }

    #[tokio::test]
    async fn a_land_the_final_verify_refused_asks_at_once_with_the_failing_tests() {
        const JOB: &str = "drv-land-red";
        let fx = Fixture::new().await;
        landable_job(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);
        let root = root_node_of(JOB);

        fx.tick(JOB, &gate).await;
        end_the_landing(
            &fx,
            JOB,
            "failed",
            "the merge does not pass this project's gate, so nothing was published.",
        )
        .await;
        let id = fx.job(JOB).await.manifest.extra["land_id"]
            .as_i64()
            .unwrap();
        sqlx::query("UPDATE vcs_requests SET output_tail = ? WHERE id = ?")
            .bind("FAIL core: tests::boom")
            .bind(id)
            .execute(&fx.state.pool)
            .await
            .unwrap();
        for _ in 0..3 {
            fx.tick(JOB, &gate).await;
        }

        assert_eq!(land_attempts(&fx, JOB).await, Some(LAND_ATTEMPTS));
        let questions = fx.open_questions(JOB, &root).await;
        assert_eq!(questions.len(), 1, "{questions:?}");
        assert!(questions[0].contains("tests::boom"), "{:?}", questions[0]);
        assert_eq!(fx.requests().await, 1, "the same tip is not landed again");
    }

    #[tokio::test]
    async fn a_land_that_errored_stays_a_counted_attempt() {
        const JOB: &str = "drv-land-errored";
        let fx = Fixture::new().await;
        landable_job(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;
        end_the_landing(
            &fx,
            JOB,
            "failed",
            "the merge could not be measured, so nothing was published: timeout",
        )
        .await;
        fx.tick(JOB, &gate).await;

        assert_eq!(land_attempts(&fx, JOB).await, Some(1));
        assert!(fx.open_questions(JOB, &root_node_of(JOB)).await.is_empty());
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
    async fn a_pending_start_whose_base_was_dropped_drops_the_waiting_branch_naming_the_base() {
        const JOB: &str = "drv-pending-dropped-base";
        let fx = Fixture::new().await;
        fx.seed_job(JOB, Vec::new(), None).await;
        let mut stored = fx.job(JOB).await;
        let mut base = crate::tree_job::model::Branch::new("base", &[], "code", None);
        base.state = "dropped".to_owned();
        let late = crate::tree_job::model::Branch::new(PENDING_NAME, &[], "code", Some("base"));
        stored.manifest.branches.insert("base".to_owned(), base);
        stored
            .manifest
            .branches
            .insert(PENDING_NAME.to_owned(), late);
        stored.manifest.extra.insert(
            "pending_starts".to_owned(),
            json!({ PENDING_NAME: {
                "caller": root_node_of(JOB),
                "layer": "code",
                "base": "base",
                "waits_for_base": "base",
            }}),
        );
        store::update(&fx.state.pool, &stored).await.unwrap();

        fx.tick(JOB, &FakeGate::answering(vec![])).await;

        let stored = fx.job(JOB).await;
        let waiting = &stored.manifest.branches[PENDING_NAME];
        assert_eq!(waiting.state, "dropped");
        let reason = waiting.extra["drop_reason"].as_str().unwrap();
        assert!(reason.contains("base"), "{reason}");
        assert!(pending_starts(&stored).get(PENDING_NAME).is_none());
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

    // ---- pending start: terminal parent, backoff, no slot ----------------------------------------

    /// A job whose pending start of `PENDING_NAME` names a non-root caller, `mid`, that is already
    /// `done`: a condition no later sweep can satisfy. The disk would admit the branch.
    async fn job_with_a_done_parent(fx: &Fixture, job: &str) {
        fx.seed_job(job, Vec::new(), None).await;
        let workflow = fx.repo.join(".nucleos").join("workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(
            workflow.join("speed.yaml"),
            "tree:\n  base_branch: master\n  floor_gb: 0\n",
        )
        .unwrap();
        fx.add_node(job, "mid", "orchestrator").await;
        sqlx::query("UPDATE work_nodes SET status = 'done' WHERE id = 'mid'")
            .execute(&fx.state.pool)
            .await
            .unwrap();
        let mut stored = fx.job(job).await;
        stored.manifest.extra.insert(
            "pending_starts".to_owned(),
            json!({ PENDING_NAME: {
                "caller": "mid",
                "goal": "Do the work",
                "prompt": "Do not run.",
                "paths": ["late/**"],
                "layer": PENDING_LAYER,
                "base": null,
            }}),
        );
        store::update(&fx.state.pool, &stored).await.unwrap();
    }

    async fn set_record_field(fx: &Fixture, job: &str, key: &str, value: Value) {
        let mut stored = fx.job(job).await;
        stored
            .manifest
            .extra
            .get_mut("pending_starts")
            .and_then(Value::as_object_mut)
            .and_then(|pending| pending.get_mut(PENDING_NAME))
            .and_then(Value::as_object_mut)
            .expect("the record is there")
            .insert(key.to_owned(), value);
        store::update(&fx.state.pool, &stored).await.unwrap();
    }

    #[tokio::test]
    async fn a_pending_start_under_a_done_parent_fails_the_branch_with_a_reason_and_never_loops() {
        const JOB: &str = "drv-pending-done-parent";
        let fx = Fixture::new().await;
        job_with_a_done_parent(&fx, JOB).await;
        let gate = FakeGate::answering(vec![]);

        fx.tick(JOB, &gate).await;

        let stored = fx.job(JOB).await;
        assert!(
            pending_starts(&stored).get(PENDING_NAME).is_none(),
            "the record is consumed: {:?}",
            pending_starts(&stored)
        );
        let branch = stored
            .manifest
            .branches
            .get(PENDING_NAME)
            .expect("the branch is recorded, so the failure is visible");
        assert_eq!(branch.state, "dropped");
        let reason = branch.extra["drop_reason"].as_str().unwrap();
        assert!(
            reason.contains("mid") && reason.contains("done"),
            "{reason}"
        );
        let after = (
            count_of(&fx, "feed").await,
            count_of(&fx, "work_nodes").await,
        );

        fx.tick(JOB, &gate).await;
        fx.tick(JOB, &gate).await;

        assert_eq!(
            (
                count_of(&fx, "feed").await,
                count_of(&fx, "work_nodes").await
            ),
            after,
            "later sweeps do nothing"
        );
    }

    #[tokio::test]
    async fn a_pending_start_inside_its_backoff_window_is_not_retried() {
        const JOB: &str = "drv-pending-backoff-wait";
        let fx = Fixture::new().await;
        job_with_a_done_parent(&fx, JOB).await;
        let later = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        set_record_field(&fx, JOB, "next_attempt_at", json!(later)).await;
        let before = pending_starts(&fx.job(JOB).await);

        fx.tick(JOB, &FakeGate::answering(vec![])).await;

        let stored = fx.job(JOB).await;
        assert_eq!(pending_starts(&stored), before, "the record is untouched");
        assert!(stored.manifest.branches.is_empty());
    }

    #[tokio::test]
    async fn a_failed_retry_doubles_the_wait_up_to_a_cap() {
        const JOB: &str = "drv-pending-backoff-grow";
        let fx = Fixture::new().await;
        job_with_a_done_parent(&fx, JOB).await;
        let wait_of = |job: TreeJob| {
            let record = pending_starts(&job)[PENDING_NAME].clone();
            let at =
                chrono::DateTime::parse_from_rfc3339(record["next_attempt_at"].as_str().unwrap())
                    .unwrap();
            (
                record["attempts"].as_u64().unwrap(),
                (at.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds(),
            )
        };

        crate::tree_job::pending_start::record_failure(&fx.state.pool, JOB, PENDING_NAME, "boom")
            .await
            .unwrap();
        let (first_attempts, first_wait) = wait_of(fx.job(JOB).await);
        crate::tree_job::pending_start::record_failure(&fx.state.pool, JOB, PENDING_NAME, "boom")
            .await
            .unwrap();
        let (second_attempts, second_wait) = wait_of(fx.job(JOB).await);

        assert_eq!((first_attempts, second_attempts), (1, 2));
        assert!(second_wait > first_wait, "{first_wait} then {second_wait}");
        for _ in 0..12 {
            crate::tree_job::pending_start::record_failure(
                &fx.state.pool,
                JOB,
                PENDING_NAME,
                "boom",
            )
            .await
            .unwrap();
        }
        let (_, capped) = wait_of(fx.job(JOB).await);
        assert!(capped <= 3600, "{capped}");
        assert_eq!(
            pending_starts(&fx.job(JOB).await)[PENDING_NAME]["last_error"],
            "boom"
        );
    }

    #[tokio::test]
    async fn a_pending_start_refused_for_lack_of_a_slot_keeps_its_record() {
        const JOB: &str = "drv-pending-no-slot";
        let fx = Fixture::new().await;
        let mut live = crate::tree_job::model::Branch::new("other", &[], PENDING_LAYER, None);
        // `open` holds a slot (`LIVE_STATES`) and the driver leaves it alone; `running` is no branch
        // state, so it would not count and the cap of 1 would never refuse.
        live.state = "open".to_owned();
        fx.seed_job(JOB, vec![live], None).await;
        let workflow = fx.repo.join(".nucleos").join("workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(
            workflow.join("speed.yaml"),
            "tree:\n  base_branch: master\n  floor_gb: 0\n  max_live_branches: 1\n",
        )
        .unwrap();
        let mut stored = fx.job(JOB).await;
        stored.manifest.extra.insert(
            "pending_starts".to_owned(),
            json!({ PENDING_NAME: {
                "caller": root_node_of(JOB),
                "goal": "Do the work",
                "prompt": "Do not run.",
                "paths": ["late/**"],
                "layer": PENDING_LAYER,
                "base": null,
            }}),
        );
        store::update(&fx.state.pool, &stored).await.unwrap();
        let before = pending_starts(&fx.job(JOB).await);

        fx.tick(JOB, &FakeGate::answering(vec![])).await;
        fx.tick(JOB, &FakeGate::answering(vec![])).await;

        let stored = fx.job(JOB).await;
        assert_eq!(pending_starts(&stored), before, "the record is kept");
        assert!(!stored.manifest.branches.contains_key(PENDING_NAME));
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
    async fn the_last_failed_resolution_of_the_ladder_parks_once() {
        let fx = Fixture::new().await;
        conflicting_job(&fx).await;
        let gate = FakeGate::answering(vec![]);

        // Attempt 1: the queue escalates, the resolver fails.
        fx.tick(CLASH_JOB, &gate).await;
        fx.drain().await;
        fx.tick(CLASH_JOB, &gate).await;
        assert_eq!(conflict_attempts(&fx).await, Some(1));
        for attempt in 2..=CONFLICT_ATTEMPTS {
            resolution_ends_without_landing(&fx).await;
            // The driver drops the dead request and resubmits (in this pass or the next).
            fx.tick(CLASH_JOB, &gate).await;
            fx.tick(CLASH_JOB, &gate).await;
            assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());

            // The queue escalates again, the resolver fails again.
            run_queue(&fx).await;
            fx.tick(CLASH_JOB, &gate).await;
            assert_eq!(conflict_attempts(&fx).await, Some(attempt));
            assert!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.is_empty());
        }
        resolution_ends_without_landing(&fx).await;

        // At the limit the conflict is claimed and the owner is asked, once.
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;
        fx.tick(CLASH_JOB, &gate).await;

        assert_eq!(fx.branch(CLASH_JOB, "api").await.state, "parked");
        assert_eq!(fx.open_questions(CLASH_JOB, CLASH_NODE).await.len(), 1);
        assert_eq!(
            fx.requests().await,
            CONFLICT_ATTEMPTS,
            "no further attempt was queued"
        );
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
