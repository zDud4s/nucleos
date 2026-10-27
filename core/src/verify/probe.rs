//! Proofs on the master head (task-claims spec 2026-10-22, section 3 Verify, cases 2 and 3).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::execution::{git_exec, land};
use crate::task_claims::{FailedProbe, PROBE_LEASE_SECONDS, TaskRow, store};
use crate::verify::exec::Executor;
use crate::verify::failures::Failures;
use crate::verify::inherit::detail_strings;
use crate::verify::plan::{Kind, ScopeArg, Ticket};
use crate::verify::postgate_worker::GATE_UNIT;
use crate::verify::resolve;
use crate::verify::runs as verify_runs;
use crate::verify::store as verify_store;
use crate::verify::{self, Caller, VerifyArgs};

pub const PROBE_PREFIX: &str = "postgate-probe-";

/// `<worktree root>/postgate-probe-<project>`: the proof's own tree, separate from the post-merge gate's.
pub fn probe_worktree(project_root: &Path) -> PathBuf {
    let project = project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_owned());
    crate::execution::worktree::worktree_root(project_root).join(format!("{PROBE_PREFIX}{project}"))
}

/// Each proof unit is cut off by the executor after this, well inside the proof's lease.
pub const UNIT_TIMEOUT: Duration = Duration::from_secs(420);
/// The proof stops queueing new work after this: still-queued units are cancelled, and the ones
/// running are then awaited (they end by `UNIT_TIMEOUT` at the latest).
const QUEUE_BUDGET: Duration = Duration::from_secs(540);
const GIT_BUDGET: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);

/// The tip of the project's integration branch, as a full sha.
pub(super) async fn master_head(
    pool: &SqlitePool,
    project_id: &str,
    project_root: &Path,
    deadline: Instant,
) -> Result<String, String> {
    let branch = land::integration_branch(pool, project_id, project_root, deadline).await?;
    resolve::resolve_commit(project_root, &format!("refs/heads/{}", branch.as_str()), deadline).await
}

/// How a proof ended; kept per ticket so a ticket never repeats a proof it already has the answer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeResult {
    Passed,
    Failed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeOutcome {
    pub task_id: i64,
    pub result: ProbeResult,
}

impl ProbeOutcome {
    pub fn of(request: &verify_store::RequestRow) -> Option<ProbeOutcome> {
        request.probe_outcome.as_deref().and_then(|raw| serde_json::from_str(raw).ok())
    }

    /// What the unit is told.
    pub fn note(&self, head: Option<&str>) -> String {
        match self.result {
            ProbeResult::Passed => format!(
                "passes on master{}: the failure is yours",
                head.map(|h| format!(" at {}", h.chars().take(10).collect::<String>())).unwrap_or_default()
            ),
            ProbeResult::Aborted => {
                "the proof on master did not finish; treated as your failure (a later verify tries again)".to_owned()
            }
            ProbeResult::Failed => {
                "the master incident this proof opened has since closed; the failure is yours".to_owned()
            }
        }
    }
}

// ---- one proof per master sha ------------------------------------------------------------------

/// A pass is remembered this long (the proof's own lease).
const PASS_TTL: Duration = Duration::from_secs(PROBE_LEASE_SECONDS as u64);

/// "These groups passed on this master head": keyed by project and sha so the next ticket with the
/// same suspect groups does not start a second proof (a pass deletes the `probing` row, which is
/// all the store remembers). In memory: a restart costs one repeated proof.
struct ProbeMark {
    groups: BTreeSet<String>,
    at: Instant,
}

static PASSES: LazyLock<Mutex<HashMap<(String, String), ProbeMark>>> = LazyLock::new(Default::default);

pub(super) fn remember_pass(project_id: &str, sha: &str, groups: &[String], at: Instant) {
    let mut passes = PASSES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    passes.retain(|_, mark| at.saturating_duration_since(mark.at) <= PASS_TTL);
    let mark = passes
        .entry((project_id.to_owned(), sha.to_owned()))
        .or_insert_with(|| ProbeMark { groups: BTreeSet::new(), at });
    mark.groups.extend(groups.iter().cloned());
    mark.at = at;
}

pub(super) fn passed_recently(project_id: &str, sha: &str, groups: &[String], at: Instant) -> bool {
    let passes = PASSES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    passes.get(&(project_id.to_owned(), sha.to_owned())).is_some_and(|mark| {
        at.saturating_duration_since(mark.at) <= PASS_TTL && groups.iter().all(|g| mark.groups.contains(g))
    })
}

// ---- single flight inside the process -------------------------------------------------------------

/// Tasks with a proof running in this process. `begin_probe` / `begin_reprobe` are the cross-process
/// guards; this one also stops two concurrent reads of the same ticket from launching it twice.
static ACTIVE: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(Default::default);

struct ActiveGuard(i64);

impl ActiveGuard {
    fn enter(task_id: i64) -> Option<ActiveGuard> {
        let mut active = ACTIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        active.insert(task_id).then_some(ActiveGuard(task_id))
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&self.0);
    }
}

/// One proof per project at a time, held until its units are over. A lapsed lease can let a second
/// proof start while the first still runs; it then waits here, so the tree is never reset under a unit.
static TREES: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(Default::default);

fn tree_lock(project_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    TREES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(project_id.to_owned())
        .or_default()
        .clone()
}

// ---- the proof itself -----------------------------------------------------------------------------

pub(super) enum What {
    /// Case 2: these groups, run whole.
    Groups(Vec<String>),
    /// Case 3: the incident's red tests -- by the files that hold them when those still exist at the
    /// master head, else by group, else (only `gate_command`) the whole gate.
    Incident { guard_paths: Vec<String>, red_groups: Vec<String> },
}

pub(super) struct Proof {
    pub head: String,
    pub tree: PathBuf,
    pub any_failed: bool,
    pub failed_groups: Vec<String>,
    pub failures: Failures,
    /// A failed unit whose runner gave no ids: "nothing failed that we know of" cannot be said.
    pub ids_missing: bool,
}

pub(super) enum ProofJob {
    Probe { project_id: String, project_root: PathBuf, task_id: i64, ticket_id: i64, groups: Vec<String> },
    Reprobe { project_id: String, project_root: PathBuf, task: TaskRow },
}

impl ProofJob {
    fn task_id(&self) -> i64 {
        match self {
            ProofJob::Probe { task_id, .. } => *task_id,
            ProofJob::Reprobe { task, .. } => task.id,
        }
    }
}

/// Launches a proof unless this process already runs one for the task. Returns whether it did.
pub(super) fn spawn_proof(executor: Arc<Executor>, job: ProofJob) -> bool {
    let Some(guard) = ActiveGuard::enter(job.task_id()) else { return false };
    tokio::spawn(async move {
        let _guard = guard;
        match job {
            ProofJob::Probe { project_id, project_root, task_id, ticket_id, groups } => {
                run_probe(&executor, &project_id, &project_root, task_id, ticket_id, groups).await
            }
            ProofJob::Reprobe { project_id, project_root, task } => {
                run_reprobe(&executor, &project_id, &project_root, task).await
            }
        }
    });
    true
}

pub(super) async fn prove(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    what: What,
) -> Result<Proof, String> {
    let _tree = tree_lock(project_id).lock_owned().await;
    let deadline = Instant::now() + GIT_BUDGET;
    let head = master_head(&executor.pool, project_id, project_root, deadline).await?;
    let tree = git_exec::prepare_probe_worktree(project_root, &head, deadline).await?;

    let (scope, files, groups) = match what {
        What::Groups(groups) => (ScopeArg::Own, None, Some(groups)),
        What::Incident { guard_paths, red_groups } => {
            let existing: Vec<String> = guard_paths.into_iter().filter(|p| tree.join(p).is_file()).collect();
            let named: Vec<String> = red_groups.into_iter().filter(|g| g != GATE_UNIT).collect();
            if !existing.is_empty() {
                (ScopeArg::Own, Some(existing), None)
            } else if !named.is_empty() {
                (ScopeArg::Own, None, Some(named))
            } else {
                (ScopeArg::Full, None, None)
            }
        }
    };
    let request = VerifyArgs {
        kind: Kind::Test,
        scope,
        worktree: Some(tree.to_string_lossy().into_owned()),
        files,
        base: None,
        wait: false,
        cover: false,
        task_id: None,
        groups,
    };
    let ticket_id = verify::submit(executor, Caller::Probe, &request)
        .await
        .map_err(|error| error.message().to_owned())?;
    let ticket = settle_units(executor, ticket_id).await?;

    let mut proof = Proof {
        head,
        tree,
        any_failed: false,
        failed_groups: Vec::new(),
        failures: Failures::default(),
        ids_missing: false,
    };
    for unit in &ticket.units {
        match unit.status.as_str() {
            verify_runs::STATUS_FAILED => {
                proof.any_failed = true;
                proof.failed_groups.push(unit.group.clone().unwrap_or_else(|| GATE_UNIT.to_owned()));
                match &unit.failed_tests {
                    Some(ids) => proof.failures.tests.extend(ids.iter().cloned()),
                    None => proof.ids_missing = true,
                }
                proof.failures.files.extend(unit.failed_files.iter().cloned());
            }
            verify_runs::STATUS_ERRORED | "unknown" => {
                return Err(format!("a proof unit ended {}", unit.status));
            }
            _ => {}
        }
    }
    Ok(proof)
}

/// Waits for the probe ticket's units. At `QUEUE_BUDGET` the units still queued are cancelled and
/// the ones running are awaited (the executor cuts them off at `UNIT_TIMEOUT`); a proof that had to
/// cancel is an `Err`. The tree is never handed back while a unit still runs in it.
async fn settle_units(executor: &Arc<Executor>, ticket_id: i64) -> Result<Ticket, String> {
    let started = Instant::now();
    let mut cancelled = false;
    loop {
        let read = verify::read_ticket_with(&executor.pool, None, ticket_id, false)
            .await
            .map_err(|error| error.to_string())?;
        let Some((ticket, _)) = read else { return Err(format!("proof ticket {ticket_id} vanished")) };
        if ticket.done {
            return if cancelled { Err("the proof ran out of time".to_owned()) } else { Ok(ticket) };
        }
        let elapsed = started.elapsed();
        if elapsed >= QUEUE_BUDGET && !cancelled {
            if let Err(error) =
                verify::cancel_queued_units(&executor.pool, ticket_id, "the proof ran out of time").await
            {
                tracing::warn!(%error, ticket_id, "verify probe: cannot cancel the queued units");
            }
            cancelled = true;
        }
        if elapsed >= QUEUE_BUDGET + UNIT_TIMEOUT + Duration::from_secs(30) {
            return Err("a proof unit outlived its timeout".to_owned());
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Case 2's proof: `groups` on the master head.
async fn run_probe(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    task_id: i64,
    ticket_id: i64,
    groups: Vec<String>,
) {
    let proved = prove(executor, project_id, project_root, What::Groups(groups.clone())).await;
    let (result, failed) = match &proved {
        Ok(proof) if proof.any_failed => (
            ProbeResult::Failed,
            Some(FailedProbe {
                master_sha: proof.head.clone(),
                red_groups: proof.failed_groups.clone(),
                red_tests: proof.failures.tests.iter().cloned().collect(),
                guard_paths: proof
                    .failures
                    .files
                    .iter()
                    .filter(|file| proof.tree.join(file).is_file())
                    .cloned()
                    .collect(),
            }),
        ),
        Ok(proof) => {
            remember_pass(project_id, &proof.head, &groups, Instant::now());
            (ProbeResult::Passed, None)
        }
        Err(error) => {
            tracing::warn!(%error, project_id, "verify probe: the proof on master did not finish");
            (ProbeResult::Aborted, None)
        }
    };
    // The answer is recorded BEFORE the claim row changes: a crash between the two writes leaves
    // the answer behind, so the next read of this ticket does not repeat the proof.
    let outcome = ProbeOutcome { task_id, result };
    if let Ok(json) = serde_json::to_string(&outcome)
        && let Err(error) = verify_store::set_probe_outcome(&executor.pool, ticket_id, &json).await
    {
        tracing::warn!(%error, ticket_id, "verify probe: cannot record the outcome");
    }
    // `None` deletes the probing row (passed, or aborted: either way nothing is proven red).
    // The ticket is passed so a stale probe can never settle another ticket's live probe
    // (chunk 1, Task 8: a foreign probe comes back untouched, still `probing`).
    if let Err(error) = store::finish_probe(&executor.pool, task_id, &ticket_id.to_string(), failed, Utc::now()).await
    {
        tracing::warn!(%error, task_id, "verify probe: cannot settle the probing row");
    }
}

/// Case 3's proof: the incident's red tests on the master head.
async fn run_reprobe(executor: &Arc<Executor>, project_id: &str, project_root: &Path, task: TaskRow) {
    let red_tests = detail_strings(&task.detail, "red_tests");
    let what = What::Incident {
        guard_paths: task.guard_paths.clone(),
        red_groups: detail_strings(&task.detail, "red_groups"),
    };
    match prove(executor, project_id, project_root, what).await {
        Ok(proof) => {
            // Still red when something failed AND (the incident names no tests, or a failed unit gave
            // no ids so we cannot say it is another test, or one of the red tests failed again).
            let still_red = proof.any_failed
                && (red_tests.is_empty()
                    || proof.ids_missing
                    || red_tests.iter().any(|test| proof.failures.tests.contains(test)));
            if let Err(error) =
                store::finish_reprobe(&executor.pool, task.id, &proof.head, still_red, Utc::now()).await
            {
                tracing::warn!(%error, task = task.id, "verify probe: cannot settle the re-proof");
            }
        }
        // No `finish_reprobe`: the flight lapses by itself and a later read repeats the proof, at
        // most once per PROBE_LEASE_SECONDS.
        Err(error) => tracing::warn!(%error, task = task.id, "verify probe: the re-proof did not finish"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_is_remembered_per_project_sha_and_group_until_it_expires() {
        let t0 = Instant::now();
        let groups = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        remember_pass("proj-pass-test", "sha1", &groups(&["py", "core"]), t0);

        assert!(passed_recently("proj-pass-test", "sha1", &groups(&["py"]), t0 + Duration::from_secs(5)));
        assert!(passed_recently("proj-pass-test", "sha1", &groups(&["core", "py"]), t0));
        assert!(!passed_recently("proj-pass-test", "sha1", &groups(&["py", "other"]), t0), "an unproved group");
        assert!(!passed_recently("proj-pass-test", "sha2", &groups(&["py"]), t0), "another master head");
        assert!(!passed_recently("other-proj", "sha1", &groups(&["py"]), t0), "another project");
        assert!(!passed_recently("proj-pass-test", "sha1", &groups(&["py"]), t0 + PASS_TTL + Duration::from_secs(1)));
    }

    #[test]
    fn a_proof_unit_is_cut_off_well_before_the_proof_lease_lapses() {
        assert!(UNIT_TIMEOUT < Duration::from_secs(PROBE_LEASE_SECONDS as u64));
        assert!(QUEUE_BUDGET + Duration::from_secs(30) < Duration::from_secs(PROBE_LEASE_SECONDS as u64));
    }
}
