//! Proofs on the master head (task-claims spec 2026-10-22, section 3 Verify, cases 2 and 3).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::execution::{git_exec, land};
use crate::task_claims::{FailedProbe, PROBE_LEASE_SECONDS, Status, TaskRow, store};
use crate::verify::exec::Executor;
use crate::verify::failures::Failures;
use crate::verify::inherit::detail_strings;
use crate::verify::plan::{Kind, ScopeArg, Ticket, UnitReport};
use crate::verify::postgate_worker::GATE_UNIT;
use crate::verify::resolve;
use crate::verify::runs as verify_runs;
use crate::verify::store as verify_store;
use crate::verify::{self, Caller, VerifyArgs};

/// Not `postgate-probe-`: that equals the post-merge gate's `postgate-<leaf>` of a project named
/// `probe-<x>`. Nothing `postgate_worktree` builds starts with `probe-`, and `owner_from_dir_name`
/// claims only `run-`, `job-` and `item-`, so the orphan sweep leaves this directory alone.
pub const PROBE_PREFIX: &str = "probe-";

/// `<worktree root>/probe-<project>`: the proof's own tree, separate from the post-merge gate's.
pub fn probe_worktree(project_root: &Path) -> PathBuf {
    let project = project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_owned());
    crate::execution::worktree::worktree_root(project_root).join(format!("{PROBE_PREFIX}{project}"))
}

/// Each proof unit is cut off by the executor after this.
///
/// The whole proof must end inside the store's `PROBE_LEASE_SECONDS` (600 s): git preparation
/// (`GIT_BUDGET`) + `QUEUE_BUDGET` + `UNIT_TIMEOUT` + `SETTLE_GRACE` = 540 s, which leaves a 60 s
/// margin for the final writes. A proof that outlived its lease would let a second one start.
pub const UNIT_TIMEOUT: Duration = Duration::from_secs(240);
/// The proof stops queueing new work after this: still-queued units are cancelled, and the ones
/// running are then awaited (they end by `UNIT_TIMEOUT` at the latest).
const QUEUE_BUDGET: Duration = Duration::from_secs(240);
/// Slack after the last unit's timeout for the executor to record the unit as ended.
const SETTLE_GRACE: Duration = Duration::from_secs(30);
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
    resolve::resolve_commit(
        project_root,
        &format!("refs/heads/{}", branch.as_str()),
        deadline,
    )
    .await
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
        request
            .probe_outcome
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
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

static PASSES: LazyLock<Mutex<HashMap<(String, String), ProbeMark>>> =
    LazyLock::new(Default::default);

pub(super) fn remember_pass(project_id: &str, sha: &str, groups: &[String], at: Instant) {
    let mut passes = PASSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    passes.retain(|_, mark| at.saturating_duration_since(mark.at) <= PASS_TTL);
    let mark = passes
        .entry((project_id.to_owned(), sha.to_owned()))
        .or_insert_with(|| ProbeMark {
            groups: BTreeSet::new(),
            at,
        });
    mark.groups.extend(groups.iter().cloned());
    mark.at = at;
}

pub(super) fn passed_recently(project_id: &str, sha: &str, groups: &[String], at: Instant) -> bool {
    let passes = PASSES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    passes
        .get(&(project_id.to_owned(), sha.to_owned()))
        .is_some_and(|mark| {
            at.saturating_duration_since(mark.at) <= PASS_TTL
                && groups.iter().all(|g| mark.groups.contains(g))
        })
}

// ---- single flight inside the process -------------------------------------------------------------

/// Tasks with a proof running in this process. `begin_probe` / `begin_reprobe` are the cross-process
/// guards; this one also stops two concurrent reads of the same ticket from launching it twice.
static ACTIVE: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(Default::default);

struct ActiveGuard(i64);

impl ActiveGuard {
    fn enter(task_id: i64) -> Option<ActiveGuard> {
        let mut active = ACTIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.insert(task_id).then_some(ActiveGuard(task_id))
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

/// One proof per project at a time, held until its units are over. A lapsed lease can let a second
/// proof start while the first still runs; it then waits here, so the tree is never reset under a unit.
static TREES: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

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
    Incident {
        guard_paths: Vec<String>,
        red_groups: Vec<String>,
    },
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
    Probe {
        project_id: String,
        project_root: PathBuf,
        task_id: i64,
        ticket_id: i64,
        groups: Vec<String>,
    },
    Reprobe {
        project_id: String,
        project_root: PathBuf,
        task: TaskRow,
    },
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
    let Some(guard) = ActiveGuard::enter(job.task_id()) else {
        return false;
    };
    tokio::spawn(async move {
        let _guard = guard;
        match job {
            ProofJob::Probe {
                project_id,
                project_root,
                task_id,
                ticket_id,
                groups,
            } => {
                run_probe(
                    &executor,
                    &project_id,
                    &project_root,
                    task_id,
                    ticket_id,
                    groups,
                )
                .await
            }
            ProofJob::Reprobe {
                project_id,
                project_root,
                task,
            } => run_reprobe(&executor, &project_id, &project_root, task).await,
        }
    });
    true
}

/// One submitted ticket of a proof.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    scope: ScopeArg,
    files: Option<Vec<String>>,
    groups: Option<Vec<String>>,
}

impl Step {
    /// The groups this step names: each of them has to have really run, or the proof says nothing.
    fn named(&self) -> Option<&[String]> {
        self.groups.as_deref()
    }
}

/// What to submit for `what`; `exists` tells whether a repo-relative path is a file at the master head.
///
/// An incident is re-proved by every handle it has: the guard files that still exist (the narrow
/// proof), AND its `red_groups` whole (a group the guard files do not map to is not forgotten). A
/// `gate_command` among the red groups, or no handle at all, means the whole gate.
fn plan_proof(what: What, exists: impl Fn(&str) -> bool) -> Vec<Step> {
    match what {
        What::Groups(groups) => vec![Step {
            scope: ScopeArg::Own,
            files: None,
            groups: Some(groups),
        }],
        What::Incident {
            guard_paths,
            red_groups,
        } => {
            if red_groups.iter().any(|g| g == GATE_UNIT) {
                return vec![Step {
                    scope: ScopeArg::Full,
                    files: None,
                    groups: None,
                }];
            }
            let existing: Vec<String> = guard_paths.into_iter().filter(|p| exists(p)).collect();
            let mut named: Vec<String> = Vec::new();
            for group in red_groups {
                if !named.contains(&group) {
                    named.push(group);
                }
            }
            let mut steps = Vec::new();
            if !existing.is_empty() {
                steps.push(Step {
                    scope: ScopeArg::Own,
                    files: Some(existing),
                    groups: None,
                });
            }
            if !named.is_empty() {
                steps.push(Step {
                    scope: ScopeArg::Own,
                    files: None,
                    groups: Some(named),
                });
            }
            if steps.is_empty() {
                steps.push(Step {
                    scope: ScopeArg::Full,
                    files: None,
                    groups: None,
                });
            }
            steps
        }
    }
}

/// A unit that really measured something: it passed (or reused a green), or it failed.
fn measured(status: &str) -> bool {
    matches!(
        status,
        verify_runs::STATUS_PASSED
            | verify_runs::STATUS_FAILED
            | verify_runs::STATUS_SKIPPED_CACHED
    )
}

/// Whether the ticket's units can answer the proof. With `named` groups, each one must have a unit
/// that measured (a skipped or missing group is not a pass); without, at least one unit must have.
/// A ticket where nothing ran (all skipped, none selected) answers nothing.
fn answered(units: &[UnitReport], named: Option<&[String]>) -> bool {
    match named {
        Some(names) => names.iter().all(|name| {
            units
                .iter()
                .any(|u| u.group.as_deref() == Some(name.as_str()) && measured(&u.status))
        }),
        None => units.iter().any(|u| measured(&u.status)),
    }
}

/// Proves `what` on the master head. `Ok` is conclusive: either a unit failed, or every step
/// measured something and nothing failed. A proof where nothing ran is an `Err`, never a pass.
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

    let steps = plan_proof(what, |path| tree.join(path).is_file());
    let mut ticket_ids: Vec<i64> = Vec::new();
    for step in &steps {
        let request = VerifyArgs {
            kind: Kind::Test,
            scope: step.scope,
            worktree: Some(tree.to_string_lossy().into_owned()),
            files: step.files.clone(),
            base: None,
            wait: false,
            cover: false,
            task_id: None,
            groups: step.groups.clone(),
        };
        match verify::submit(executor, Caller::Probe, &request).await {
            Ok(id) => ticket_ids.push(id),
            Err(error) => {
                for id in &ticket_ids {
                    let _ = verify::cancel_queued_units(
                        &executor.pool,
                        *id,
                        "the proof could not start",
                    )
                    .await;
                }
                return Err(error.message().to_owned());
            }
        }
    }
    let tickets = settle_units(executor, &ticket_ids).await?;

    let mut proof = Proof {
        head,
        tree,
        any_failed: false,
        failed_groups: Vec::new(),
        failures: Failures::default(),
        ids_missing: false,
    };
    let mut every_step_answered = true;
    for (ticket, step) in tickets.iter().zip(&steps) {
        every_step_answered &= answered(&ticket.units, step.named());
        for unit in &ticket.units {
            match unit.status.as_str() {
                verify_runs::STATUS_FAILED => {
                    proof.any_failed = true;
                    let group = unit.group.clone().unwrap_or_else(|| GATE_UNIT.to_owned());
                    if !proof.failed_groups.contains(&group) {
                        proof.failed_groups.push(group);
                    }
                    match &unit.failed_tests {
                        Some(ids) => proof.failures.tests.extend(ids.iter().cloned()),
                        None => proof.ids_missing = true,
                    }
                    proof
                        .failures
                        .files
                        .extend(unit.failed_files.iter().cloned());
                }
                verify_runs::STATUS_ERRORED | "unknown" => {
                    return Err(format!("a proof unit ended {}", unit.status));
                }
                _ => {}
            }
        }
    }
    // A failure is real whatever else did not run; a pass needs every step to have measured.
    if !proof.any_failed && !every_step_answered {
        return Err("nothing ran: a proof unit was skipped or none was selected".to_owned());
    }
    Ok(proof)
}

/// Waits for the probe tickets' units. At `QUEUE_BUDGET` the units still queued are cancelled and
/// the ones running are awaited (the executor cuts them off at `UNIT_TIMEOUT`); a proof that had to
/// cancel is an `Err`. The tree is never handed back while a unit still runs in it. One budget covers
/// all the tickets, so the proof as a whole stays inside the probe lease.
async fn settle_units(executor: &Arc<Executor>, ticket_ids: &[i64]) -> Result<Vec<Ticket>, String> {
    let started = Instant::now();
    let mut cancelled = false;
    loop {
        let mut tickets = Vec::with_capacity(ticket_ids.len());
        for &ticket_id in ticket_ids {
            let read = verify::read_ticket_with(&executor.pool, None, ticket_id, false)
                .await
                .map_err(|error| error.to_string())?;
            let Some((ticket, _)) = read else {
                return Err(format!("proof ticket {ticket_id} vanished"));
            };
            tickets.push(ticket);
        }
        if tickets.iter().all(|ticket| ticket.done) {
            return if cancelled {
                Err("the proof ran out of time".to_owned())
            } else {
                Ok(tickets)
            };
        }
        let elapsed = started.elapsed();
        if elapsed >= QUEUE_BUDGET && !cancelled {
            for &ticket_id in ticket_ids {
                if let Err(error) = verify::cancel_queued_units(
                    &executor.pool,
                    ticket_id,
                    "the proof ran out of time",
                )
                .await
                {
                    tracing::warn!(%error, ticket_id, "verify probe: cannot cancel the queued units");
                }
            }
            cancelled = true;
        }
        if elapsed >= QUEUE_BUDGET + UNIT_TIMEOUT + SETTLE_GRACE {
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
    let proved = prove(
        executor,
        project_id,
        project_root,
        What::Groups(groups.clone()),
    )
    .await;
    let (result, failed, passed_head) = match &proved {
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
            None,
        ),
        Ok(proof) => (ProbeResult::Passed, None, Some(proof.head.clone())),
        Err(error) => {
            tracing::warn!(%error, project_id, "verify probe: the proof on master did not finish");
            (ProbeResult::Aborted, None, None)
        }
    };
    // `None` deletes the probing row (passed, or aborted: either way nothing is proven red).
    // The ticket is passed so a stale probe can never settle another ticket's live probe
    // (chunk 1, Task 8: a foreign probe comes back untouched, still `probing`).
    let settled = store::finish_probe(
        &executor.pool,
        task_id,
        &ticket_id.to_string(),
        failed,
        Utc::now(),
    )
    .await;
    match settled {
        // Another ticket's proof holds the row: this result says nothing about the task, so
        // neither the answer nor the pass is recorded (the store doc: conclude nothing).
        Ok(Some(row)) if row.status == Status::Probing => {
            tracing::warn!(
                task_id,
                ticket_id,
                "verify probe: the probing row belongs to another proof; nothing concluded"
            );
        }
        Ok(_) => {
            if let Some(head) = passed_head {
                remember_pass(project_id, &head, &groups, Instant::now());
            }
            let outcome = ProbeOutcome { task_id, result };
            if let Ok(json) = serde_json::to_string(&outcome)
                && let Err(error) =
                    verify_store::set_probe_outcome(&executor.pool, ticket_id, &json).await
            {
                tracing::warn!(%error, ticket_id, "verify probe: cannot record the outcome");
            }
        }
        Err(error) => {
            tracing::warn!(%error, task_id, "verify probe: cannot settle the probing row");
        }
    }
}

/// Case 3's proof: the incident's red tests on the master head.
async fn run_reprobe(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    task: TaskRow,
) {
    let red_tests = detail_strings(&task.detail, "red_tests");
    let what = What::Incident {
        guard_paths: task.guard_paths.clone(),
        red_groups: detail_strings(&task.detail, "red_groups"),
    };
    // `prove` is an `Err` when nothing ran, so an `Ok` that did not fail is a real pass.
    match prove(executor, project_id, project_root, what).await {
        Ok(proof) => {
            // Still red when something failed AND (the incident names no tests, or a failed unit gave
            // no ids so we cannot say it is another test, or one of the red tests failed again).
            let still_red = proof.any_failed
                && (red_tests.is_empty()
                    || proof.ids_missing
                    || red_tests
                        .iter()
                        .any(|test| proof.failures.tests.contains(test)));
            if let Err(error) =
                store::finish_reprobe(&executor.pool, task.id, &proof.head, still_red, Utc::now())
                    .await
            {
                tracing::warn!(%error, task = task.id, "verify probe: cannot settle the re-proof");
            }
        }
        // No `finish_reprobe`: the flight lapses by itself and a later read repeats the proof, at
        // most once per PROBE_LEASE_SECONDS. An inconclusive proof (nothing ran) never closes it.
        Err(error) => {
            tracing::warn!(%error, task = task.id, "verify probe: the re-proof did not finish")
        }
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

        assert!(passed_recently(
            "proj-pass-test",
            "sha1",
            &groups(&["py"]),
            t0 + Duration::from_secs(5)
        ));
        assert!(passed_recently(
            "proj-pass-test",
            "sha1",
            &groups(&["core", "py"]),
            t0
        ));
        assert!(
            !passed_recently("proj-pass-test", "sha1", &groups(&["py", "other"]), t0),
            "an unproved group"
        );
        assert!(
            !passed_recently("proj-pass-test", "sha2", &groups(&["py"]), t0),
            "another master head"
        );
        assert!(
            !passed_recently("other-proj", "sha1", &groups(&["py"]), t0),
            "another project"
        );
        assert!(!passed_recently(
            "proj-pass-test",
            "sha1",
            &groups(&["py"]),
            t0 + PASS_TTL + Duration::from_secs(1)
        ));
    }

    #[test]
    fn a_whole_proof_ends_inside_the_probe_lease_with_a_margin() {
        let lease = Duration::from_secs(PROBE_LEASE_SECONDS as u64);
        let worst = GIT_BUDGET + QUEUE_BUDGET + UNIT_TIMEOUT + SETTLE_GRACE;
        assert!(
            worst + Duration::from_secs(30) <= lease,
            "git + queue + unit + settle = {worst:?}, lease {lease:?}"
        );
    }

    #[test]
    fn the_probe_tree_cannot_be_the_postgate_tree_of_another_project() {
        // `postgate-<leaf>` of a project named `probe-x` is `postgate-probe-x`.
        let root = Path::new("/work/repo");
        let name = |p: PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name(probe_worktree(root)), "probe-repo");
        assert!(!PROBE_PREFIX.starts_with("postgate-"));
    }

    fn unit(group: Option<&str>, status: &str) -> UnitReport {
        UnitReport {
            group: group.map(str::to_owned),
            argv: Vec::new(),
            why: String::new(),
            status: status.to_owned(),
            duration_ms: None,
            exit_code: None,
            output_tail: None,
            skipped_reason: None,
            run_id: None,
            cached_from: None,
            already_failing: None,
            failed_tests: None,
            failed_files: Vec::new(),
            inherited: None,
            pending: None,
            not_inherited: None,
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn a_ticket_where_nothing_ran_answers_nothing() {
        assert!(!answered(&[], None), "no unit selected");
        assert!(
            !answered(&[unit(Some("core"), "skipped")], None),
            "all skipped"
        );
        assert!(answered(
            &[unit(Some("core"), "skipped"), unit(Some("py"), "passed")],
            None
        ));
        assert!(answered(&[unit(Some("core"), "failed")], None));
    }

    #[test]
    fn a_skipped_or_missing_named_group_is_not_a_pass() {
        let wanted = names(&["core", "py"]);
        assert!(
            !answered(&[unit(Some("core"), "passed")], Some(wanted.as_slice())),
            "py is missing"
        );
        assert!(!answered(
            &[unit(Some("core"), "passed"), unit(Some("py"), "skipped")],
            Some(wanted.as_slice())
        ));
        assert!(answered(
            &[unit(Some("core"), "passed"), unit(Some("py"), "failed")],
            Some(wanted.as_slice())
        ));
    }

    #[test]
    fn an_incident_is_re_proved_by_its_guard_files_and_its_red_groups() {
        let steps = plan_proof(
            What::Incident {
                guard_paths: names(&["a.rs", "gone.rs"]),
                red_groups: names(&["core", "py", "core"]),
            },
            |p| p == "a.rs",
        );
        assert_eq!(
            steps,
            vec![
                Step {
                    scope: ScopeArg::Own,
                    files: Some(names(&["a.rs"])),
                    groups: None
                },
                Step {
                    scope: ScopeArg::Own,
                    files: None,
                    groups: Some(names(&["core", "py"]))
                },
            ]
        );
    }

    #[test]
    fn a_gate_command_red_group_is_never_dropped_and_runs_the_whole_gate() {
        let full = vec![Step {
            scope: ScopeArg::Full,
            files: None,
            groups: None,
        }];
        let with_gate = What::Incident {
            guard_paths: names(&["a.rs"]),
            red_groups: names(&["core", GATE_UNIT]),
        };
        assert_eq!(plan_proof(with_gate, |_| true), full);
        let nothing = What::Incident {
            guard_paths: names(&["gone.rs"]),
            red_groups: Vec::new(),
        };
        assert_eq!(plan_proof(nothing, |_| false), full);
    }

    #[test]
    fn case_two_runs_exactly_its_groups() {
        let steps = plan_proof(What::Groups(names(&["core"])), |_| false);
        assert_eq!(
            steps,
            vec![Step {
                scope: ScopeArg::Own,
                files: None,
                groups: Some(names(&["core"]))
            }]
        );
    }
}
