//! Gates, merge and land of a tree job (spec R12), with the reviewer's findings kept as data (L40).
//!
//! The state decisions are pure over a `Manifest` or a `Branch`, so they can be tested without a
//! daemon. The async parts wrap them around the verify gate, the VCS queue and `land::submit`, and
//! persist the job themselves. Merge and land are each two steps, submit and settle, because the
//! queue runs the operation later and a daemon that restarts in between has to be able to resume.
//!
//! **Origins, since no production caller exists yet.** `origin` is a parameter of `submit_merge` and
//! `land_job`; nothing here refuses by it. A branch merging into the tree's own base
//! (`feature_branch`) should pass `Origin::Daemon`: the daemon drives it, and `Run`/`Job` rows would
//! start `awaiting_approval` with nothing to move them. The landing of the whole tree into the
//! integration branch should pass `Origin::Shell`, which is what the `--land` path
//! (`POST /vcs/land`, `http/run_queue.rs`) uses; it goes through the task-claims gate, and
//! `Origin::Daemon` must not be used to step past that gate.
//!
//! **Escalated merges and the conflict resolver.** A conflicting branch merge escalates in the
//! queue, and `resolver::next_conflict` would mint an agent for any such row. A tree merge has its
//! own owner (the branch stays `green` and resubmits), so `settle_merge` marks the row settled while
//! no resolution has claimed it. The resolver can claim it first, on its tick between the
//! escalation and the settle; then the settle reports `Pending` while that run is live, and follows
//! a later identical success or a landed resolution as a merge. What stays open: a resolution that
//! claimed the row and then ends without landing is reported as `NotMerged`, and one the resolver
//! lands after that is seen only by a resubmit (which merges trivially).

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

use super::engine::apply_transition;
use super::model::{Branch, JOB_TERMINAL_STATE, Manifest};
use super::store::{self, StoreError, TreeJob};
use crate::execution::vcs::{self, Op, Origin, ResolvedRepo};
use crate::gate::GateOutcome;
use crate::verify::exec::Executor;
use crate::verify::{ScopeVerdict, gate_job_scope};

/// The states of a finding (`FINDING_STATES` in `tree_state.py`).
const FINDING_STATES: [&str; 4] = ["open", "fixed", "drop", "defer"];

/// The branch states a gate may run from. `merged`, `parked`, `dropped`, `queued`, `open` and a
/// docs branch's `done` have nothing to gate.
const GATE_STATES: [&str; 6] = ["building", "gating", "fixing", "red", "green", "reviewing"];

/// Request statuses that can still change, so a request in one of them is not a dead merge.
const LIVE_REQUEST_STATUSES: [&str; 5] = [
    "queued",
    "running",
    "awaiting_approval",
    "awaiting_owner",
    "awaiting_gate",
];

/// Longest tail of a failed gate's output kept in a gate entry.
const LOG_TAIL_MAX: usize = 4000;

#[derive(Debug)]
pub enum LandingError {
    /// The manifest has no branch of this name.
    UnknownBranch(String),
    /// The reviewer's findings are not a well-formed list.
    InvalidFindings(String),
    /// A rule says no (state, findings, base, nothing to settle); the message names the cause.
    Refused(String),
    /// `land::submit` refused the landing; the text is its own.
    LandRefused(String),
    /// The branch's uncommitted work could not be committed, so nothing was queued.
    Checkpoint(String),
    Store(StoreError),
    Db(sqlx::Error),
}

impl fmt::Display for LandingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LandingError::UnknownBranch(name) => write!(f, "unknown branch {name}"),
            LandingError::InvalidFindings(reason) => write!(f, "invalid findings: {reason}"),
            LandingError::Refused(reason) => write!(f, "{reason}"),
            LandingError::LandRefused(reason) => write!(f, "landing refused: {reason}"),
            LandingError::Checkpoint(reason) => {
                write!(f, "could not commit the branch's work: {reason}")
            }
            LandingError::Store(error) => write!(f, "{error}"),
            LandingError::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

impl std::error::Error for LandingError {}

impl From<StoreError> for LandingError {
    fn from(error: StoreError) -> Self {
        LandingError::Store(error)
    }
}

impl From<sqlx::Error> for LandingError {
    fn from(error: sqlx::Error) -> Self {
        LandingError::Db(error)
    }
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// Stamps the job and writes it as it stands.
async fn persist(pool: &SqlitePool, job: &mut TreeJob) -> Result<(), LandingError> {
    job.updated_at = now_iso();
    store::update(pool, job).await?;
    Ok(())
}

fn branch_of<'a>(manifest: &'a Manifest, name: &str) -> Result<&'a Branch, LandingError> {
    manifest
        .branches
        .get(name)
        .ok_or_else(|| LandingError::UnknownBranch(name.to_owned()))
}

// ---- findings as data --------------------------------------------------------------------------

/// `read_findings` (`tree.py`): a list of objects with a non-blank text `id` and a known `state`,
/// no id twice. The list is taken whole, so one bad entry keeps all of them out.
fn read_findings(findings: &Value) -> Result<Vec<Map<String, Value>>, LandingError> {
    let invalid = |reason: String| LandingError::InvalidFindings(reason);
    let list = findings
        .as_array()
        .ok_or_else(|| invalid("the findings must be a JSON list".to_owned()))?;
    let mut seen: Vec<&str> = Vec::new();
    let mut read = Vec::with_capacity(list.len());
    for finding in list {
        let object = finding
            .as_object()
            .ok_or_else(|| invalid("every finding needs an id".to_owned()))?;
        let id = match object.get("id").and_then(Value::as_str) {
            Some(id) if !id.trim().is_empty() => id,
            _ => return Err(invalid("every finding needs an id".to_owned())),
        };
        let known = object
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| FINDING_STATES.contains(&state));
        if !known {
            return Err(invalid(format!(
                "finding {id}: state must be one of {FINDING_STATES:?}"
            )));
        }
        if seen.contains(&id) {
            return Err(invalid(format!("duplicate finding id {id}")));
        }
        seen.push(id);
        read.push(object.clone());
    }
    Ok(read)
}

/// Appends a review to `branch`. The new list is folded over the last review's findings, as
/// `run_review` does (`tree.py:651-655`): a finding the reviewer does not mention stays as it was,
/// and one that is mentioned takes the new values and keeps the keys it had. Without that, silence
/// would close a finding. Nothing changes when the list is refused.
pub fn record_findings(
    manifest: &mut Manifest,
    branch: &str,
    findings: Value,
) -> Result<(), LandingError> {
    let holder = manifest
        .branches
        .get_mut(branch)
        .ok_or_else(|| LandingError::UnknownBranch(branch.to_owned()))?;
    let submitted = read_findings(&findings)?;

    let mut merged: Vec<Map<String, Value>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let before = holder
        .reviews
        .last()
        .and_then(|review| review.get("findings"))
        .and_then(Value::as_array);
    for prior in before.into_iter().flatten() {
        if let Some(object) = prior.as_object()
            && let Some(id) = object.get("id").and_then(Value::as_str)
        {
            index.insert(id.to_owned(), merged.len());
            merged.push(object.clone());
        }
    }
    for finding in submitted {
        let id = finding
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match index.get(&id) {
            Some(&at) => merged[at].extend(finding),
            None => {
                index.insert(id, merged.len());
                merged.push(finding);
            }
        }
    }

    let list: Vec<Value> = merged.into_iter().map(Value::Object).collect();
    holder
        .reviews
        .push(json!({"at": now_iso(), "findings": list}));
    Ok(())
}

/// The findings of the last review whose state is `open`. Only the last review counts, because it
/// already carries every earlier finding forward.
pub fn open_findings(branch: &Branch) -> Vec<Value> {
    branch
        .reviews
        .last()
        .and_then(|review| review.get("findings"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|finding| finding.get("state").and_then(Value::as_str) == Some("open"))
        .cloned()
        .collect()
}

// ---- the gate ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    Green,
    Red,
    /// Nothing was measured that could bless or fail the work; the branch stays where it was.
    Undecided {
        reason: String,
    },
}

fn tail(text: &str) -> String {
    if text.len() <= LOG_TAIL_MAX {
        return text.to_owned();
    }
    let mut start = text.len() - LOG_TAIL_MAX;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

fn gate_refusal(branch: &Branch) -> Result<(), LandingError> {
    if GATE_STATES.contains(&branch.state.as_str()) {
        return Ok(());
    }
    Err(LandingError::Refused(format!(
        "branch {} is {}: only a branch at work or under review is gated",
        branch.name, branch.state
    )))
}

/// Records a gate verdict on the branch: `Passed` moves it to `green`, `Failed` to `red`, and
/// anything undecided leaves the state and says why. A repeated verdict is legal (`run_gate` has no
/// source guard) and still appends its entry. `merged` is never reached from here.
pub fn apply_gate_verdict(
    branch: &mut Branch,
    verdict: &ScopeVerdict,
) -> Result<GateResult, LandingError> {
    gate_refusal(branch)?;
    let at = now_iso();
    let (result, entry) = match verdict {
        ScopeVerdict::Measured(GateOutcome::Passed) => (
            GateResult::Green,
            json!({"at": at, "verdict": "green", "exit": 0}),
        ),
        ScopeVerdict::Measured(GateOutcome::Failed { exit_code, output }) => (
            GateResult::Red,
            json!({"at": at, "verdict": "red", "exit": exit_code, "log_tail": tail(output)}),
        ),
        undecided => {
            let reason = match undecided {
                ScopeVerdict::Measured(GateOutcome::Errored { reason }) => reason.clone(),
                ScopeVerdict::NothingRan => {
                    "no test unit agrees with the branch's diff, so nothing was measured".to_owned()
                }
                _ => "the scope gate could not be submitted, so nothing was measured".to_owned(),
            };
            (
                GateResult::Undecided {
                    reason: reason.clone(),
                },
                json!({"at": at, "verdict": "undecided", "reason": reason}),
            )
        }
    };
    match result {
        GateResult::Green => branch.state = "green".to_owned(),
        GateResult::Red => branch.state = "red".to_owned(),
        GateResult::Undecided { .. } => {}
    }
    branch.gates.push(entry);
    Ok(result)
}

/// The number `gate_job_scope` takes for a tree job, whose id is text. It only labels the
/// `verify_requests.caller` row as `job:{id}`, so it is derived from the text (FNV-1a) instead of
/// read from a table: stable for the life of the job, and above `u32::MAX` so it does not read as
/// a row of the `jobs` table.
pub fn scope_caller_id(job_id: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in job_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // 62 bits keep it positive; bit 40 keeps it above `u32::MAX`.
    ((hash & 0x3fff_ffff_ffff_ffff) | (1 << 40)) as i64
}

/// Gates the branch's worktree with the verify `scope` gate, applies the verdict and persists the
/// job. A branch that may not gate, or has no worktree, is refused before anything is submitted.
pub async fn gate_branch(
    executor: &Arc<Executor>,
    job: &mut TreeJob,
    branch: &str,
    wait: Duration,
) -> Result<GateResult, LandingError> {
    let held = branch_of(&job.manifest, branch)?;
    gate_refusal(held)?;
    let worktree = held
        .worktree
        .clone()
        .ok_or_else(|| LandingError::Refused(format!("branch {branch} has no worktree to gate")))?;

    let verdict = gate_job_scope(
        executor,
        scope_caller_id(&job.id),
        Path::new(&worktree),
        wait,
    )
    .await;

    let held = job
        .manifest
        .branches
        .get_mut(branch)
        .ok_or_else(|| LandingError::UnknownBranch(branch.to_owned()))?;
    let result = apply_gate_verdict(held, &verdict)?;
    persist(&executor.pool, job).await?;
    Ok(result)
}

// ---- merge -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDecision {
    /// Merge `source` into `target`.
    Proceed { source: String, target: String },
    /// Already merged; `sha` is the one stored then.
    AlreadyMerged { sha: String },
}

/// PURE: whether `branch` may be queued for a merge into the tree's feature branch. A merged branch
/// answers with its stored sha. Otherwise it has to be a green code branch with a git branch, no
/// open finding, and a base (if any) that is merged already (`run_merge`, `tree.py:693`).
pub fn check_may_merge(manifest: &Manifest, branch: &str) -> Result<MergeDecision, LandingError> {
    let held = branch_of(manifest, branch)?;
    if held.state == "merged" {
        return match held.extra.get("merge_sha").and_then(Value::as_str) {
            Some(sha) => Ok(MergeDecision::AlreadyMerged {
                sha: sha.to_owned(),
            }),
            None => Err(LandingError::Refused(format!(
                "branch {branch} is merged but records no merge sha"
            ))),
        };
    }
    if held.state != "green" {
        return Err(LandingError::Refused(format!(
            "branch {branch} is {}: only a green branch merges",
            held.state
        )));
    }
    if held.layer == "docs" {
        return Err(LandingError::Refused(format!(
            "branch {branch} is a docs branch: it closes on done and never merges"
        )));
    }
    let Some(source) = held.git_branch.clone() else {
        return Err(LandingError::Refused(format!(
            "branch {branch} has no git branch to merge"
        )));
    };
    if let Some(base) = &held.base
        && manifest.branches.get(base).map(|b| b.state.as_str()) != Some("merged")
    {
        return Err(LandingError::Refused(format!(
            "branch {branch} stands on {base}, which is not merged yet"
        )));
    }
    let open = open_findings(held);
    if !open.is_empty() {
        let ids: Vec<&str> = open
            .iter()
            .filter_map(|finding| finding.get("id").and_then(Value::as_str))
            .collect();
        return Err(LandingError::Refused(format!(
            "branch {branch} has open findings: {}",
            ids.join(", ")
        )));
    }
    Ok(MergeDecision::Proceed {
        source,
        target: manifest.feature_branch.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeSubmission {
    /// A merge request is in the queue (new, or the one already there).
    Queued {
        request_id: i64,
    },
    AlreadyMerged {
        sha: String,
    },
}

fn stored_request_id(branch: &Branch, key: &str) -> Option<i64> {
    branch.extra.get(key).and_then(Value::as_i64)
}

async fn request_is_live(pool: &SqlitePool, id: i64) -> Result<bool, LandingError> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(status.is_some_and(|status| LIVE_REQUEST_STATUSES.contains(&status.as_str())))
}

/// The id of a request still in the queue for exactly this operation, if any: what a daemon that
/// died between the INSERT and the store write has left behind.
async fn live_request_for(
    pool: &SqlitePool,
    project_id: &str,
    op: &Op,
) -> Result<Option<i64>, LandingError> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM vcs_requests
          WHERE project_id = ? AND op = 'merge' AND args = ?
            AND status IN ('queued','running','awaiting_approval','awaiting_owner','awaiting_gate')
          ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(op.to_args())
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Queues the merge of a green branch into the feature branch and records the request on the
/// branch. A merge already queued is answered with its own id and enqueues nothing; a dead one (the
/// queue escalated or failed it) does not answer, so a conflicted branch can resubmit. The branch's
/// worktree is checkpointed first: an implement node leaves its work uncommitted, and merging the
/// bare branch would succeed with nothing in it. `origin`: see the module doc.
pub async fn submit_merge(
    pool: &SqlitePool,
    repo: &ResolvedRepo,
    job: &mut TreeJob,
    branch: &str,
    origin: Origin,
) -> Result<MergeSubmission, LandingError> {
    let (source, target) = match check_may_merge(&job.manifest, branch)? {
        MergeDecision::AlreadyMerged { sha } => return Ok(MergeSubmission::AlreadyMerged { sha }),
        MergeDecision::Proceed { source, target } => (source, target),
    };
    let held = branch_of(&job.manifest, branch)?;
    if let Some(id) = stored_request_id(held, "merge_request_id")
        && request_is_live(pool, id).await?
    {
        return Ok(MergeSubmission::Queued { request_id: id });
    }

    let op = Op::Merge {
        source: vcs::Branch::new(&source).map_err(LandingError::Refused)?,
        target: vcs::Branch::new(&target).map_err(LandingError::Refused)?,
    };
    let request_id = match live_request_for(pool, repo.project_id(), &op).await? {
        Some(id) => id,
        None => {
            if let Some(worktree) = held.worktree.clone() {
                crate::execution::worktree::checkpoint(Path::new(&worktree))
                    .await
                    .map_err(|error| LandingError::Checkpoint(error.to_string()))?;
            }
            vcs::submit(pool, repo, &op, origin).await?
        }
    };

    let held = job
        .manifest
        .branches
        .get_mut(branch)
        .ok_or_else(|| LandingError::UnknownBranch(branch.to_owned()))?;
    held.extra
        .insert("merge_request_id".to_owned(), json!(request_id));
    persist(pool, job).await?;
    Ok(MergeSubmission::Queued { request_id })
}

/// How a queued request stands.
enum RequestEnd {
    Pending,
    Succeeded(Option<String>),
    Failed { reason: String, escalated: bool },
}

/// Reads request `id` (following `resolved_by` to a landed resolution, as `wait_for` does) for up to
/// `wait`. `claim_conflict` is set for a tree branch merge: an escalated row that no resolution has
/// claimed is marked settled so `resolver::next_conflict` never picks it up. See the module doc for
/// the race with the resolver.
async fn settle_request(
    pool: &SqlitePool,
    id: i64,
    wait: Duration,
    claim_conflict: bool,
) -> Result<RequestEnd, LandingError> {
    let ticket = vcs::wait_for(pool, id, wait).await?;
    let reason = || {
        ticket
            .failure_reason
            .clone()
            .filter(|reason| !reason.trim().is_empty())
            .unwrap_or_else(|| format!("the queue ended request {id} as {}", ticket.status))
    };
    match ticket.status.as_str() {
        "succeeded" => Ok(RequestEnd::Succeeded(ticket.result_sha.clone())),
        "escalated" => settle_escalated(pool, id, reason(), claim_conflict).await,
        "failed" | "blocked" | "rejected" | "cancelled" | "interrupted" => Ok(RequestEnd::Failed {
            reason: reason(),
            escalated: false,
        }),
        _ => Ok(RequestEnd::Pending),
    }
}

async fn settle_escalated(
    pool: &SqlitePool,
    id: i64,
    reason: String,
    claim_conflict: bool,
) -> Result<RequestEnd, LandingError> {
    let row: Option<(String, String, Option<i64>)> =
        sqlx::query_as("SELECT project_id, args, resolution_run_id FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let Some((project_id, args, resolution_run)) = row else {
        return Ok(RequestEnd::Failed {
            reason,
            escalated: true,
        });
    };

    // A later identical success (somebody merged it by hand) is a merge.
    let later: Option<Option<String>> = sqlx::query_scalar(
        "SELECT result_sha FROM vcs_requests
          WHERE project_id = ? AND args = ? AND status = 'succeeded' AND id > ?
          ORDER BY id LIMIT 1",
    )
    .bind(&project_id)
    .bind(&args)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    if let Some(sha) = later {
        return Ok(RequestEnd::Succeeded(sha));
    }

    match resolution_run {
        Some(run) => {
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(run)
                .fetch_optional(pool)
                .await?;
            if matches!(status.as_deref(), Some("running" | "awaiting_approval")) {
                tracing::info!(
                    request_id = id,
                    run,
                    "a conflict resolution is still running"
                );
                return Ok(RequestEnd::Pending);
            }
        }
        None if claim_conflict => {
            // Guarded by `resolution_run_id IS NULL`, so a resolution that claimed the row first is
            // never overwritten.
            sqlx::query(
                "UPDATE vcs_requests SET settled_at = ?, settled_reason = 'tree-merge'
                  WHERE id = ? AND status = 'escalated' AND resolution_run_id IS NULL
                    AND settled_at IS NULL",
            )
            .bind(Utc::now().to_rfc3339())
            .bind(id)
            .execute(pool)
            .await?;
        }
        None => {}
    }
    Ok(RequestEnd::Failed {
        reason,
        escalated: true,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeSettlement {
    Merged {
        sha: String,
    },
    /// The queue did not merge it. The branch stays `green` and may resubmit.
    NotMerged {
        reason: String,
    },
    /// The queue escalated the merge and the caller asked not to claim it (`claim_conflict` false).
    /// While `resolution_failed` is false the request id stays on the branch for the conflict
    /// resolver. When true a resolution ran and ended without landing: the request id is dropped
    /// so the branch can resubmit, as for `NotMerged`.
    Conflicted {
        reason: String,
        resolution_failed: bool,
    },
    /// The queue has not finished (or a resolution is running); nothing changed.
    Pending,
}

/// Reads the outcome of the branch's queued merge for up to `wait` and records it: a success moves
/// the branch to `merged` with `merge_sha` and `merged_at`; a failure or conflict keeps it `green`
/// and records `merge_failure`. Either way the request id is dropped so a new submit queues afresh.
///
/// `claim_conflict` false leaves an escalated, unclaimed row to the conflict resolver: the branch
/// keeps its request id, and `conflict_attempts` counts one attempt per merge request (the request
/// last counted is `conflict_request_id`, so a repeated pass over the same conflict adds nothing).
pub async fn settle_merge(
    pool: &SqlitePool,
    job: &mut TreeJob,
    branch: &str,
    wait: Duration,
    claim_conflict: bool,
) -> Result<MergeSettlement, LandingError> {
    let held = branch_of(&job.manifest, branch)?;
    if held.state == "merged"
        && let Some(sha) = held.extra.get("merge_sha").and_then(Value::as_str)
    {
        return Ok(MergeSettlement::Merged {
            sha: sha.to_owned(),
        });
    }
    let id = stored_request_id(held, "merge_request_id").ok_or_else(|| {
        LandingError::Refused(format!("branch {branch} has no merge in the queue"))
    })?;

    let end = settle_request(pool, id, wait, claim_conflict).await?;
    // Read before `held` borrows the job: whether a resolution run claimed the escalated row.
    let unresolved = if matches!(
        end,
        RequestEnd::Failed {
            escalated: true,
            ..
        }
    ) && !claim_conflict
    {
        let claimed: Option<Option<i64>> =
            sqlx::query_scalar("SELECT resolution_run_id FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        matches!(claimed, Some(None))
    } else {
        false
    };
    let held = job
        .manifest
        .branches
        .get_mut(branch)
        .ok_or_else(|| LandingError::UnknownBranch(branch.to_owned()))?;
    match end {
        RequestEnd::Pending => Ok(MergeSettlement::Pending),
        RequestEnd::Failed {
            reason,
            escalated: true,
        } if !claim_conflict => {
            // One attempt per merge request, however many passes see the same conflict.
            let counted = held
                .extra
                .get("conflict_request_id")
                .and_then(Value::as_i64);
            if counted != Some(id) {
                let attempts = held
                    .extra
                    .get("conflict_attempts")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                held.extra
                    .insert("conflict_attempts".to_owned(), json!(attempts + 1));
                held.extra
                    .insert("conflict_request_id".to_owned(), json!(id));
                if unresolved {
                    persist(pool, job).await?;
                    return Ok(MergeSettlement::Conflicted {
                        reason,
                        resolution_failed: false,
                    });
                }
            } else if unresolved {
                return Ok(MergeSettlement::Conflicted {
                    reason,
                    resolution_failed: false,
                });
            }
            // A resolution ran and ended without landing: the request is dead.
            held.extra
                .insert("merge_failure".to_owned(), json!(reason.clone()));
            held.extra.remove("merge_request_id");
            persist(pool, job).await?;
            Ok(MergeSettlement::Conflicted {
                reason,
                resolution_failed: true,
            })
        }
        RequestEnd::Succeeded(sha) => {
            let sha = sha.filter(|sha| !sha.trim().is_empty()).ok_or_else(|| {
                LandingError::Refused(format!(
                    "the queue merged branch {branch} but recorded no sha"
                ))
            })?;
            apply_transition(held, "merged")
                .map_err(|error| LandingError::Refused(error.to_string()))?;
            held.extra.insert("merge_sha".to_owned(), json!(sha));
            held.extra.insert("merged_at".to_owned(), json!(now_iso()));
            held.extra.remove("merge_request_id");
            held.extra.remove("merge_failure");
            persist(pool, job).await?;
            Ok(MergeSettlement::Merged { sha })
        }
        RequestEnd::Failed { reason, .. } => {
            held.extra.insert("merge_failure".to_owned(), json!(reason));
            held.extra.remove("merge_request_id");
            persist(pool, job).await?;
            Ok(MergeSettlement::NotMerged { reason })
        }
    }
}

// ---- land --------------------------------------------------------------------------------------

/// PURE: whether the whole tree may land. Every branch has to be `merged`, `dropped`, or a docs
/// branch that is `done`, and at least one has to be merged. A branch that is still at work, or
/// parked, blocks the landing and is named.
pub fn check_may_land(manifest: &Manifest) -> Result<(), LandingError> {
    let mut merged = 0usize;
    let mut blocking: Vec<String> = Vec::new();
    for branch in manifest.branches.values() {
        match branch.state.as_str() {
            "merged" => merged += 1,
            "dropped" => {}
            "done" if branch.layer == "docs" => {}
            state => blocking.push(format!("{} ({state})", branch.name)),
        }
    }
    if !blocking.is_empty() {
        return Err(LandingError::Refused(format!(
            "the tree cannot land while these branches are not finished: {}",
            blocking.join(", ")
        )));
    }
    if merged == 0 {
        return Err(LandingError::Refused(
            "the tree cannot land: no branch has been merged".to_owned(),
        ));
    }
    Ok(())
}

/// Admits the landing of the feature branch into the project's integration branch through
/// `land::submit` and records its id, which a second call answers without queuing again. Admission
/// is not landing: the job stays live until `settle_land` sees the queue succeed. A refusal from
/// `land::submit` is recorded as `land_refusal` and returned as `LandRefused`. `origin`: see the
/// module doc (`Origin::Shell`, so the claims gate applies).
pub async fn land_job(
    pool: &SqlitePool,
    repo: &ResolvedRepo,
    job: &mut TreeJob,
    deadline: Instant,
    origin: Origin,
) -> Result<i64, LandingError> {
    if let Some(id) = job.manifest.extra.get("land_id").and_then(Value::as_i64) {
        return Ok(id);
    }
    check_may_land(&job.manifest)?;

    let admitted = crate::execution::land::submit(
        pool,
        repo,
        Path::new(repo.root()),
        &job.manifest.feature_branch,
        None,
        deadline,
        origin,
    )
    .await;
    match admitted {
        Ok(id) => {
            job.manifest.extra.insert("land_id".to_owned(), json!(id));
            job.manifest.extra.remove("land_refusal");
            persist(pool, job).await?;
            Ok(id)
        }
        Err(refusal) => {
            let reason = refusal.message().to_owned();
            job.manifest
                .extra
                .insert("land_refusal".to_owned(), json!(reason));
            persist(pool, job).await?;
            Err(LandingError::LandRefused(reason))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandSettlement {
    /// The queue landed it; the job is `finished`.
    Finished {
        sha: Option<String>,
    },
    /// The queue did not land it; the job stays live with `land_failure` recorded.
    NotLanded {
        reason: String,
    },
    Pending,
}

/// Reads the outcome of the queued landing for up to `wait`. Only a success finishes the job. A
/// failure is recorded as `land_failure` and drops the landing id so `land_job` can try again; a
/// conflict keeps the id, because the conflict resolver owns that request and may still land it
/// under a link `settle_land` follows.
pub async fn settle_land(
    pool: &SqlitePool,
    job: &mut TreeJob,
    wait: Duration,
) -> Result<LandSettlement, LandingError> {
    let id = job
        .manifest
        .extra
        .get("land_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| LandingError::Refused("the job has no landing in the queue".to_owned()))?;

    match settle_request(pool, id, wait, false).await? {
        RequestEnd::Pending => Ok(LandSettlement::Pending),
        RequestEnd::Succeeded(sha) => {
            job.state = JOB_TERMINAL_STATE.to_owned();
            job.manifest.state = JOB_TERMINAL_STATE.to_owned();
            job.manifest.extra.remove("land_failure");
            job.manifest.extra.remove("land_refusal");
            if let Some(sha) = &sha {
                job.manifest.extra.insert("land_sha".to_owned(), json!(sha));
            }
            persist(pool, job).await?;
            Ok(LandSettlement::Finished { sha })
        }
        RequestEnd::Failed { reason, escalated } => {
            job.manifest
                .extra
                .insert("land_failure".to_owned(), json!(reason));
            if !escalated {
                job.manifest.extra.remove("land_id");
            }
            persist(pool, job).await?;
            Ok(LandSettlement::NotLanded { reason })
        }
    }
}

#[cfg(test)]
mod tests {
    // A process-wide guard held across awaits on purpose: it serialises mutation of the shared
    // NUCLEOS_WORKTREE_ROOT override, as in `land::tests` and `git_exec::tests`.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use sqlx::SqlitePool;

    use crate::config::VerifyConfig;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use crate::execution::git_exec::tests::{WorktreeRootEnv, sha_of};
    use crate::execution::git_exec::{GitExecutor, OPERATION_TIMEOUT};
    use crate::execution::vcs::{Origin, ResolvedRepo, drain_once};
    use crate::execution::worktree::test_env_lock;
    use crate::gate::GateOutcome;
    use crate::tree_job::engine::branch_node_may_close;
    use crate::tree_job::model::{Branch, JOB_TERMINAL_STATE, Manifest};
    use crate::tree_job::store::{self, TreeJob};
    use crate::verify::ScopeVerdict;
    use crate::verify::exec::Executor;

    const STAMP: &str = "2026-10-30T10:00:00+00:00";
    const SETTLE_WAIT: Duration = Duration::from_secs(5);

    // ---- fixtures: manifests -------------------------------------------------------------------

    fn branch_at(name: &str, state: &str) -> Branch {
        let mut branch = Branch::new(name, &[format!("{name}/**")], "backend", None);
        branch.state = state.to_owned();
        branch.git_branch = Some(format!("tree/{name}"));
        branch
    }

    fn docs_branch_at(name: &str, state: &str) -> Branch {
        let mut branch = Branch::new(name, &[format!(".ai/{name}/**")], "docs", None);
        branch.state = state.to_owned();
        branch
    }

    fn manifest_with(branches: Vec<Branch>) -> Manifest {
        manifest_on("C:/nowhere", branches)
    }

    fn manifest_on(project_root: &str, branches: Vec<Branch>) -> Manifest {
        let mut manifest = Manifest::new("job-1", "feat/tree", "Ship it", project_root, &[]);
        for branch in branches {
            manifest.branches.insert(branch.name.clone(), branch);
        }
        manifest
    }

    fn job_from(manifest: Manifest) -> TreeJob {
        TreeJob {
            id: manifest.tree_id.clone(),
            root_node_id: format!("root-of-{}", manifest.tree_id),
            project_id: Some("alpha".to_owned()),
            feature_branch: Some(manifest.feature_branch.clone()),
            goal: manifest.goal.clone(),
            state: manifest.state.clone(),
            manifest,
            contract: None,
            created_at: STAMP.to_owned(),
            updated_at: STAMP.to_owned(),
        }
    }

    async fn saved_job(pool: &SqlitePool, manifest: Manifest) -> TreeJob {
        let job = job_from(manifest);
        store::save(pool, &job).await.unwrap();
        job
    }

    async fn stored(pool: &SqlitePool, id: &str) -> TreeJob {
        store::load(pool, id)
            .await
            .unwrap()
            .expect("the job is stored")
    }

    async fn listed_ids(pool: &SqlitePool) -> Vec<String> {
        store::list_non_terminal(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect()
    }

    fn finding(id: &str, state: &str) -> Value {
        json!({"id": id, "state": state})
    }

    fn reviewed(mut branch: Branch, findings: Value) -> Branch {
        branch
            .reviews
            .push(json!({"at": STAMP, "findings": findings}));
        branch
    }

    fn passed() -> ScopeVerdict {
        ScopeVerdict::Measured(GateOutcome::Passed)
    }

    fn failed() -> ScopeVerdict {
        ScopeVerdict::Measured(GateOutcome::Failed {
            exit_code: 3,
            output: "boom at the end".to_owned(),
        })
    }

    fn errored() -> ScopeVerdict {
        ScopeVerdict::Measured(GateOutcome::Errored {
            reason: "did not finish in time".to_owned(),
        })
    }

    fn undecided_verdicts() -> Vec<ScopeVerdict> {
        vec![
            errored(),
            ScopeVerdict::NothingRan,
            ScopeVerdict::Unavailable,
        ]
    }

    // ---- fixtures: repositories and the queue --------------------------------------------------

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn porcelain(dir: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["status", "--porcelain"])
            .output()
            .expect("git should start");
        assert!(output.status.success());
        String::from_utf8(output.stdout).expect("utf-8")
    }

    fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
        std::fs::write(repo.join(file), text).expect("write");
        assert!(git_in(repo, &["add", "-A"]));
        assert!(git_in(repo, &["commit", "-q", "-m", message]));
    }

    fn fresh_repo(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let container = space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        (container, repo)
    }

    /// `master`, the job's `feat/tree`, and `tree/api` one commit ahead of it. The main checkout
    /// stands on `feat/tree`, the target of a branch merge.
    fn repo_with_a_tree_branch(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "tree/api"]));
        commit_file(&repo, "api.txt", "from the branch\n", "api");
        assert!(git_in(&repo, &["checkout", "-q", "feat/tree"]));
        (container, repo)
    }

    /// `tree/api` and `feat/tree` each change `seed.txt`, so merging one into the other conflicts.
    /// The main checkout stands on `feat/tree`.
    fn repo_with_a_conflicting_tree_branch(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", "tree/api"]));
        commit_file(&repo, "seed.txt", "theirs\n", "theirs");
        assert!(git_in(&repo, &["checkout", "-q", "feat/tree"]));
        commit_file(&repo, "seed.txt", "ours\n", "ours");
        (container, repo)
    }

    /// `feat/tree` one commit ahead of `master`, with the main checkout on `master`.
    fn repo_with_a_feature_to_land(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        commit_file(&repo, "feature.txt", "from the tree\n", "feature");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        (container, repo)
    }

    /// `feat/tree` and `master` each change `seed.txt`, so the landing conflicts in the queue.
    fn repo_with_a_conflicting_feature(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = fresh_repo(prefix);
        assert!(git_in(&repo, &["checkout", "-q", "-b", "feat/tree"]));
        commit_file(&repo, "seed.txt", "theirs\n", "theirs");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        commit_file(&repo, "seed.txt", "ours\n", "ours");
        (container, repo)
    }

    /// A pool, a project whose integration branch is `master`, and the worktree root the queue's
    /// integration worktrees go under. The caller holds `test_env_lock` for as long as this lives.
    struct Harness {
        pool: SqlitePool,
        resolved: ResolvedRepo,
        _roots: tempfile::TempDir,
        _env: WorktreeRootEnv,
    }

    async fn harness(repo: &Path) -> Harness {
        let roots = space_free_tempdir("nucleos-landing-wt-");
        let env = WorktreeRootEnv::set(roots.path());
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES (?, 'active', ?, 'master')",
        )
        .bind("alpha")
        .bind(repo.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .expect("seed the project's roster row");
        Harness {
            pool,
            resolved: ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha"),
            _roots: roots,
            _env: env,
        }
    }

    /// For a test that never reaches git: nothing here is on disk.
    fn synthetic_repo() -> ResolvedRepo {
        ResolvedRepo::synthetic("alpha", "C:/nowhere", "alpha")
    }

    fn manifest_for_repo(repo: &Path, branches: Vec<Branch>) -> Manifest {
        manifest_on(&repo.to_string_lossy(), branches)
    }

    async fn drain(pool: &SqlitePool) {
        assert!(
            drain_once(pool, "alpha", &GitExecutor::default()).await,
            "the queue had a request to run"
        );
    }

    async fn request_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn land_deadline() -> Instant {
        Instant::now() + OPERATION_TIMEOUT
    }

    // ---- findings as data ----------------------------------------------------------------------

    #[test]
    fn record_findings_appends_a_review_holding_the_reviewers_list() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let list = json!([
            {"id": "f1", "state": "open", "file": "core/src/a.rs", "note": "wrong"},
            {"id": "f2", "state": "defer"},
        ]);

        record_findings(&mut manifest, "api", list.clone()).unwrap();

        let reviews = &manifest.branches["api"].reviews;
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0]["findings"], list);
        chrono::DateTime::parse_from_rfc3339(reviews[0]["at"].as_str().unwrap())
            .expect("a review is stamped, as tree.py's run_review does");
    }

    #[test]
    fn each_recording_appends_one_more_review() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);

        record_findings(&mut manifest, "api", json!([finding("f1", "open")])).unwrap();
        record_findings(&mut manifest, "api", json!([finding("f1", "fixed")])).unwrap();

        assert_eq!(manifest.branches["api"].reviews.len(), 2);
    }

    #[test]
    fn an_empty_list_records_a_clean_review() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);

        record_findings(&mut manifest, "api", json!([])).unwrap();

        let branch = &manifest.branches["api"];
        assert_eq!(branch.reviews.len(), 1);
        assert_eq!(branch.reviews[0]["findings"], json!([]));
        assert!(open_findings(branch).is_empty());
    }

    #[tokio::test]
    async fn recorded_findings_survive_a_store_update_and_load() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;
        let list = json!([{"id": "f1", "state": "open", "note": "wrong"}]);

        record_findings(&mut job.manifest, "api", list.clone()).unwrap();
        store::update(&pool, &job).await.unwrap();

        let loaded = stored(&pool, "job-1").await;
        assert_eq!(loaded.manifest.branches["api"].reviews[0]["findings"], list);
        assert_eq!(open_findings(&loaded.manifest.branches["api"]).len(), 1);
    }

    #[test]
    fn findings_for_an_unknown_branch_are_refused_without_change() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let before = manifest.clone();

        let error =
            record_findings(&mut manifest, "ghost", json!([finding("f1", "open")])).unwrap_err();

        assert!(matches!(error, LandingError::UnknownBranch(_)), "{error:?}");
        assert!(error.to_string().contains("ghost"), "{error}");
        assert_eq!(manifest, before);
    }

    #[test]
    fn findings_that_are_not_a_list_are_refused_without_change() {
        for not_a_list in [
            json!({"id": "f1", "state": "open"}),
            json!("f1"),
            json!(7),
            Value::Null,
        ] {
            let mut manifest = manifest_with(vec![branch_at("api", "green")]);
            let before = manifest.clone();

            let error = record_findings(&mut manifest, "api", not_a_list.clone()).unwrap_err();

            assert!(
                matches!(error, LandingError::InvalidFindings(_)),
                "{not_a_list}: {error:?}"
            );
            assert_eq!(manifest, before, "{not_a_list}");
        }
    }

    /// The good finding ahead of the bad one must not be kept either: the list is taken whole.
    #[test]
    fn a_finding_without_an_id_is_refused_without_change() {
        for bad in [
            json!({"state": "open"}),
            json!({"id": "", "state": "open"}),
            json!({"id": "   ", "state": "open"}),
            json!({"id": 7, "state": "open"}),
            json!("f1"),
        ] {
            let mut manifest = manifest_with(vec![branch_at("api", "green")]);
            let before = manifest.clone();

            let error = record_findings(
                &mut manifest,
                "api",
                json!([finding("good", "open"), bad.clone()]),
            )
            .unwrap_err();

            assert!(
                matches!(error, LandingError::InvalidFindings(_)),
                "{bad}: {error:?}"
            );
            assert_eq!(manifest, before, "{bad}");
        }
    }

    #[test]
    fn a_duplicate_id_in_one_list_is_refused_without_change() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let before = manifest.clone();

        let error = record_findings(
            &mut manifest,
            "api",
            json!([finding("f1", "open"), finding("f1", "fixed")]),
        )
        .unwrap_err();

        assert!(
            matches!(error, LandingError::InvalidFindings(_)),
            "{error:?}"
        );
        assert!(error.to_string().contains("f1"), "{error}");
        assert_eq!(manifest, before);
    }

    /// `read_findings` (`tree.py:626`) has no default state: a finding must carry one of
    /// `FINDING_STATES`, or the whole list is refused. A default of anything but `open` would let a
    /// finding that forgot its state stop blocking the merge.
    #[test]
    fn a_finding_without_a_known_state_is_refused_without_change() {
        for bad in [
            json!({"id": "f1"}),
            json!({"id": "f1", "state": "closed"}),
            json!({"id": "f1", "state": 1}),
        ] {
            let mut manifest = manifest_with(vec![branch_at("api", "green")]);
            let before = manifest.clone();

            let error = record_findings(&mut manifest, "api", json!([bad.clone()])).unwrap_err();

            assert!(
                matches!(error, LandingError::InvalidFindings(_)),
                "{bad}: {error:?}"
            );
            assert_eq!(manifest, before, "{bad}");
        }
    }

    #[test]
    fn every_finding_state_of_tree_py_is_accepted() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        let list = json!([
            finding("a", "open"),
            finding("b", "fixed"),
            finding("c", "drop"),
            finding("d", "defer"),
        ]);

        record_findings(&mut manifest, "api", list.clone()).unwrap();

        assert_eq!(manifest.branches["api"].reviews[0]["findings"], list);
    }

    /// `run_review` (`tree.py:651-655`) folds a new list over the last review's findings. Without
    /// that, a reviewer who simply does not mention an open finding would close it, and open it for
    /// the merge. A finding that is mentioned takes the new values and keeps the keys it had.
    #[test]
    fn a_finding_the_next_review_does_not_mention_stays_open() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);
        record_findings(
            &mut manifest,
            "api",
            json!([
                {"id": "f1", "state": "open", "note": "kept"},
                {"id": "f2", "state": "open"},
            ]),
        )
        .unwrap();

        record_findings(&mut manifest, "api", json!([finding("f2", "fixed")])).unwrap();

        let branch = &manifest.branches["api"];
        let open = open_findings(branch);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["id"], "f1");
        let last = branch.reviews[1]["findings"].as_array().unwrap();
        assert_eq!(last.len(), 2);

        record_findings(&mut manifest, "api", json!([finding("f1", "fixed")])).unwrap();

        let branch = &manifest.branches["api"];
        assert!(open_findings(branch).is_empty());
        let last = branch.reviews[2]["findings"].as_array().unwrap();
        let f1 = last.iter().find(|one| one["id"] == "f1").unwrap();
        assert_eq!(f1["note"], "kept");
    }

    #[test]
    fn a_branch_with_no_reviews_has_no_open_findings() {
        assert!(open_findings(&branch_at("api", "green")).is_empty());
    }

    #[test]
    fn open_findings_returns_only_the_open_ones() {
        let branch = reviewed(
            branch_at("api", "green"),
            json!([
                finding("a", "open"),
                finding("b", "fixed"),
                finding("c", "drop"),
                finding("d", "defer"),
                finding("e", "open"),
            ]),
        );

        let ids: Vec<Value> = open_findings(&branch)
            .into_iter()
            .map(|one| one["id"].clone())
            .collect();

        assert_eq!(ids, vec![json!("a"), json!("e")]);
    }

    #[test]
    fn open_findings_reads_only_the_last_review() {
        let closed_later = reviewed(
            reviewed(branch_at("api", "green"), json!([finding("a", "open")])),
            json!([finding("b", "fixed")]),
        );
        assert!(open_findings(&closed_later).is_empty());

        let opened_later = reviewed(
            reviewed(branch_at("api", "green"), json!([finding("a", "fixed")])),
            json!([finding("b", "open")]),
        );
        let open = open_findings(&opened_later);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0]["id"], "b");
    }

    // ---- the gate ------------------------------------------------------------------------------

    #[test]
    fn a_passed_verdict_moves_the_branch_to_green() {
        for from in ["building", "gating", "fixing", "red", "reviewing"] {
            let mut branch = branch_at("api", from);

            let result = apply_gate_verdict(&mut branch, &passed()).unwrap();

            assert_eq!(result, GateResult::Green, "from {from}");
            assert_eq!(branch.state, "green", "from {from}");
            assert_eq!(branch.gates.len(), 1, "from {from}");
            assert_eq!(branch.gates[0]["verdict"], "green");
            assert_eq!(branch.gates[0]["exit"], 0);
            chrono::DateTime::parse_from_rfc3339(branch.gates[0]["at"].as_str().unwrap())
                .expect("a gate entry is stamped, as tree_resources.py's run_gate does");
        }
    }

    #[test]
    fn a_failed_verdict_moves_the_branch_to_red_and_keeps_the_exit_and_the_tail() {
        for from in ["building", "gating", "fixing", "green", "reviewing"] {
            let mut branch = branch_at("api", from);

            let result = apply_gate_verdict(&mut branch, &failed()).unwrap();

            assert_eq!(result, GateResult::Red, "from {from}");
            assert_eq!(branch.state, "red", "from {from}");
            assert_eq!(branch.gates.len(), 1, "from {from}");
            assert_eq!(branch.gates[0]["verdict"], "red");
            assert_eq!(branch.gates[0]["exit"], 3);
            assert!(
                branch.gates[0]["log_tail"]
                    .as_str()
                    .unwrap()
                    .contains("boom at the end")
            );
        }
    }

    /// `apply_transition` refuses `green` to `green` and `red` to `red`, and a repeated verdict is
    /// still a legal gate (`can_transition`'s doc, `run_gate` has no source guard).
    #[test]
    fn a_repeated_verdict_keeps_the_state_and_still_appends_its_entry() {
        let mut green = branch_at("api", "green");
        assert_eq!(
            apply_gate_verdict(&mut green, &passed()).unwrap(),
            GateResult::Green
        );
        assert_eq!(green.state, "green");
        assert_eq!(green.gates.len(), 1);

        let mut red = branch_at("api", "red");
        assert_eq!(
            apply_gate_verdict(&mut red, &failed()).unwrap(),
            GateResult::Red
        );
        assert_eq!(red.state, "red");
        assert_eq!(red.gates.len(), 1);
    }

    /// An undecided gate is never green: `Errored`, `NothingRan` and `Unavailable` measured nothing
    /// that could bless the work, so the branch stays where it was and the entry says why.
    #[test]
    fn an_undecided_verdict_leaves_the_state_and_records_why() {
        for from in ["building", "gating", "fixing", "red", "reviewing"] {
            for verdict in undecided_verdicts() {
                let mut branch = branch_at("api", from);

                let result = apply_gate_verdict(&mut branch, &verdict).unwrap();

                let GateResult::Undecided { reason } = result else {
                    panic!("{verdict:?} from {from} must be undecided, got {result:?}");
                };
                assert!(!reason.trim().is_empty(), "{verdict:?}");
                assert_eq!(branch.state, from, "{verdict:?}");
                assert_eq!(branch.gates.len(), 1, "{verdict:?}");
                assert_eq!(branch.gates[0]["verdict"], "undecided", "{verdict:?}");
                assert_eq!(branch.gates[0]["reason"], json!(reason), "{verdict:?}");
            }
        }
    }

    #[test]
    fn an_errored_verdict_records_the_reason_the_gate_gave() {
        let mut branch = branch_at("api", "building");

        let result = apply_gate_verdict(&mut branch, &errored()).unwrap();

        let GateResult::Undecided { reason } = result else {
            panic!("expected undecided, got {result:?}");
        };
        assert!(reason.contains("did not finish in time"), "{reason}");
    }

    #[test]
    fn each_verdict_appends_one_gate_entry_in_order() {
        let mut branch = branch_at("api", "building");

        apply_gate_verdict(&mut branch, &failed()).unwrap();
        apply_gate_verdict(&mut branch, &ScopeVerdict::NothingRan).unwrap();
        apply_gate_verdict(&mut branch, &passed()).unwrap();

        let verdicts: Vec<&Value> = branch.gates.iter().map(|entry| &entry["verdict"]).collect();
        assert_eq!(
            verdicts,
            vec![&json!("red"), &json!("undecided"), &json!("green")]
        );
        assert_eq!(branch.state, "green");
    }

    #[test]
    fn a_gate_on_a_branch_that_may_not_gate_is_refused_without_change() {
        let refused = [
            branch_at("api", "queued"),
            branch_at("api", "open"),
            branch_at("api", "merged"),
            branch_at("api", "parked"),
            branch_at("api", "dropped"),
            docs_branch_at("api", "done"),
        ];
        for original in refused {
            for verdict in [passed(), failed(), ScopeVerdict::NothingRan] {
                let mut branch = original.clone();

                let error = apply_gate_verdict(&mut branch, &verdict).unwrap_err();

                assert!(
                    matches!(error, LandingError::Refused(_)),
                    "{}: {error:?}",
                    original.state
                );
                assert!(
                    error.to_string().contains(&original.state),
                    "the refusal names the state: {error}"
                );
                assert_eq!(branch, original);
            }
        }
    }

    /// `merged` comes only out of a settled merge, never out of a gate.
    #[test]
    fn no_verdict_moves_a_branch_to_merged() {
        for verdict in [
            passed(),
            failed(),
            errored(),
            ScopeVerdict::NothingRan,
            ScopeVerdict::Unavailable,
        ] {
            let mut branch = branch_at("api", "green");
            apply_gate_verdict(&mut branch, &verdict).unwrap();
            assert_ne!(branch.state, "merged", "{verdict:?}");
        }
    }

    /// A tree job id is TEXT and `gate_job_scope` takes an `i64` that only ever labels the
    /// `verify_requests.caller` row as `job:{id}`. So it is derived from the text: no database read,
    /// the same for the life of the job (an implicit rowid would move on VACUUM), and not a small
    /// integer that would read as a row of the `jobs` table.
    #[test]
    fn the_scope_caller_id_is_derived_from_the_text_id_and_is_stable() {
        let first = scope_caller_id("5d6616a7-71f0-4414-a717-99d697a4c6d0");

        assert_eq!(
            first,
            scope_caller_id("5d6616a7-71f0-4414-a717-99d697a4c6d0")
        );
        assert_ne!(
            first,
            scope_caller_id("204d71d1-6b8b-4606-8e2b-d612bdbc5e00")
        );
        assert!(first > i64::from(u32::MAX), "{first}");
        assert!(scope_caller_id("") > i64::from(u32::MAX));
        assert!(scope_caller_id("job-1") > i64::from(u32::MAX));
    }

    fn idle_executor(pool: &SqlitePool, machine: &Path) -> std::sync::Arc<Executor> {
        Executor::new(
            pool.clone(),
            VerifyConfig::default(),
            Some(machine.to_path_buf()),
        )
    }

    /// `gate_branch` over a live `Executor` whose request cannot even be submitted, the one verdict
    /// reachable with no unit run (`a_job_gate_scope_that_cannot_be_submitted_is_unavailable`).
    #[tokio::test]
    async fn gate_branch_persists_an_unavailable_verdict_and_leaves_the_state() {
        let pool = crate::testdb::fresh_pool().await;
        let machine = tempfile::tempdir().unwrap();
        let not_a_worktree = tempfile::tempdir().unwrap();
        let executor = idle_executor(&pool, machine.path());
        let mut branch = branch_at("api", "building");
        branch.worktree = Some(not_a_worktree.path().to_string_lossy().into_owned());
        let mut job = saved_job(&pool, manifest_with(vec![branch])).await;

        let result = gate_branch(&executor, &mut job, "api", Duration::from_secs(5))
            .await
            .unwrap();

        assert!(matches!(result, GateResult::Undecided { .. }), "{result:?}");
        let loaded = stored(&pool, "job-1").await;
        assert_eq!(loaded.manifest, job.manifest, "the job was persisted");
        let branch = &loaded.manifest.branches["api"];
        assert_eq!(branch.state, "building");
        assert_eq!(branch.gates.len(), 1);
    }

    #[tokio::test]
    async fn gate_branch_refuses_a_branch_it_cannot_gate_without_change() {
        let pool = crate::testdb::fresh_pool().await;
        let machine = tempfile::tempdir().unwrap();
        let executor = idle_executor(&pool, machine.path());
        let mut no_worktree = branch_at("api", "building");
        no_worktree.worktree = None;
        let mut parked = branch_at("ui", "parked");
        parked.worktree = Some(machine.path().to_string_lossy().into_owned());
        let mut job = saved_job(&pool, manifest_with(vec![no_worktree, parked])).await;
        let before = job.manifest.clone();

        for name in ["api", "ui", "ghost"] {
            let result = gate_branch(&executor, &mut job, name, Duration::from_secs(5)).await;
            assert!(result.is_err(), "{name}: {result:?}");
            assert_eq!(job.manifest, before, "{name}");
        }
        assert_eq!(stored(&pool, "job-1").await.manifest, before);
    }

    // ---- merge: the decision -------------------------------------------------------------------

    #[test]
    fn a_green_branch_with_no_open_findings_may_merge() {
        let manifest = manifest_with(vec![branch_at("api", "green")]);

        assert_eq!(
            check_may_merge(&manifest, "api").unwrap(),
            MergeDecision::Proceed {
                source: "tree/api".to_owned(),
                target: "feat/tree".to_owned(),
            }
        );
    }

    #[test]
    fn findings_that_are_not_open_do_not_block_a_merge() {
        let branch = reviewed(
            branch_at("api", "green"),
            json!([
                finding("a", "fixed"),
                finding("b", "drop"),
                finding("c", "defer"),
            ]),
        );
        let manifest = manifest_with(vec![branch]);

        assert!(matches!(
            check_may_merge(&manifest, "api").unwrap(),
            MergeDecision::Proceed { .. }
        ));
    }

    #[test]
    fn every_state_but_green_refuses_a_merge_and_names_itself() {
        for state in [
            "queued",
            "open",
            "building",
            "gating",
            "red",
            "reviewing",
            "fixing",
            "parked",
            "dropped",
        ] {
            let manifest = manifest_with(vec![branch_at("api", state)]);

            let error = check_may_merge(&manifest, "api").unwrap_err();

            assert!(
                matches!(error, LandingError::Refused(_)),
                "{state}: {error:?}"
            );
            assert!(error.to_string().contains(state), "{state}: {error}");
        }
    }

    #[test]
    fn an_open_finding_refuses_a_merge_and_is_named() {
        let branch = reviewed(
            branch_at("api", "green"),
            json!([finding("f1", "open"), finding("f2", "fixed")]),
        );
        let manifest = manifest_with(vec![branch]);

        let error = check_may_merge(&manifest, "api").unwrap_err();

        assert!(matches!(error, LandingError::Refused(_)), "{error:?}");
        assert!(error.to_string().contains("f1"), "{error}");
        assert!(!error.to_string().contains("f2"), "{error}");
    }

    /// Through `record_findings` rather than a review written by hand: what the reviewer recorded
    /// is what stops the merge, whichever state the recording leaves the branch in.
    #[test]
    fn a_finding_the_reviewer_recorded_stops_the_merge() {
        let mut manifest = manifest_with(vec![branch_at("api", "green")]);

        record_findings(&mut manifest, "api", json!([finding("f1", "open")])).unwrap();

        assert!(matches!(
            check_may_merge(&manifest, "api"),
            Err(LandingError::Refused(_))
        ));
    }

    /// `run_merge` (`tree.py:693`): a stacked branch merges only after the branch it stands on.
    #[test]
    fn a_branch_whose_base_is_not_merged_refuses_a_merge_and_names_the_base() {
        let mut stacked = branch_at("api", "green");
        stacked.base = Some("db".to_owned());

        let waiting = manifest_with(vec![stacked.clone(), branch_at("db", "green")]);
        let error = check_may_merge(&waiting, "api").unwrap_err();
        assert!(matches!(error, LandingError::Refused(_)), "{error:?}");
        assert!(error.to_string().contains("db"), "{error}");

        let ready = manifest_with(vec![stacked, branch_at("db", "merged")]);
        assert!(matches!(
            check_may_merge(&ready, "api").unwrap(),
            MergeDecision::Proceed { .. }
        ));
    }

    #[test]
    fn a_green_branch_without_a_git_branch_refuses_a_merge() {
        let mut branch = branch_at("api", "green");
        branch.git_branch = None;
        let manifest = manifest_with(vec![branch]);

        assert!(matches!(
            check_may_merge(&manifest, "api"),
            Err(LandingError::Refused(_))
        ));
    }

    /// `can_transition`: a docs branch closes on `done` and never merges.
    #[test]
    fn a_docs_branch_refuses_a_merge() {
        let mut branch = docs_branch_at("notes", "green");
        branch.git_branch = Some("tree/notes".to_owned());
        let manifest = manifest_with(vec![branch]);

        assert!(matches!(
            check_may_merge(&manifest, "notes"),
            Err(LandingError::Refused(_))
        ));
    }

    #[test]
    fn a_merge_of_an_unknown_branch_is_refused() {
        let manifest = manifest_with(vec![branch_at("api", "green")]);

        assert!(matches!(
            check_may_merge(&manifest, "ghost"),
            Err(LandingError::UnknownBranch(_))
        ));
    }

    fn merged_branch(name: &str, sha: &str) -> Branch {
        let mut branch = branch_at(name, "merged");
        branch.extra.insert("merge_sha".to_owned(), json!(sha));
        branch.extra.insert("merged_at".to_owned(), json!(STAMP));
        branch
    }

    #[test]
    fn an_already_merged_branch_answers_with_its_stored_sha() {
        let manifest = manifest_with(vec![merged_branch("api", "abc123")]);

        assert_eq!(
            check_may_merge(&manifest, "api").unwrap(),
            MergeDecision::AlreadyMerged {
                sha: "abc123".to_owned()
            }
        );
    }

    // ---- merge: through the queue --------------------------------------------------------------

    #[tokio::test]
    async fn a_refused_merge_enqueues_nothing_and_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(
            &pool,
            manifest_with(vec![
                branch_at("api", "red"),
                reviewed(branch_at("ui", "green"), json!([finding("f1", "open")])),
            ]),
        )
        .await;
        let before = job.manifest.clone();

        for name in ["api", "ui"] {
            let result =
                submit_merge(&pool, &synthetic_repo(), &mut job, name, Origin::Daemon).await;
            assert!(
                matches!(result, Err(LandingError::Refused(_))),
                "{name}: {result:?}"
            );
        }

        assert_eq!(request_count(&pool).await, 0);
        assert_eq!(job.manifest, before);
        assert_eq!(stored(&pool, "job-1").await.manifest, before);
    }

    #[tokio::test]
    async fn a_replayed_merge_returns_the_stored_sha_and_enqueues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![merged_branch("api", "abc123")])).await;
        let before = job.manifest.clone();

        let result = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert_eq!(
            result,
            MergeSubmission::AlreadyMerged {
                sha: "abc123".to_owned()
            }
        );
        assert_eq!(request_count(&pool).await, 0);
        assert_eq!(job.manifest, before);
    }

    #[tokio::test]
    async fn a_green_branch_merges_through_the_queue_into_the_feature_branch() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_tree_branch("nucleos-landing-merge-");
        let h = harness(&repo).await;
        let tip = sha_of(&repo, "tree/api");
        let master = sha_of(&repo, "master");
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        let submitted = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert!(
            matches!(submitted, MergeSubmission::Queued { .. }),
            "{submitted:?}"
        );
        assert_eq!(
            job.manifest.branches["api"].state, "green",
            "a queued merge is not a merge yet"
        );
        assert_eq!(request_count(&h.pool).await, 1);

        drain(&h.pool).await;
        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT, true)
            .await
            .unwrap();

        let MergeSettlement::Merged { sha } = settled else {
            panic!("expected a merge, got {settled:?}");
        };
        assert_eq!(sha, sha_of(&repo, "feat/tree"));
        assert_eq!(sha_of(&repo, &format!("{sha}^2")), tip);
        assert_eq!(sha_of(&repo, "master"), master, "only the target moved");

        let branch = &job.manifest.branches["api"];
        assert_eq!(branch.state, "merged");
        assert_eq!(branch.extra["merge_sha"], json!(sha));
        chrono::DateTime::parse_from_rfc3339(branch.extra["merged_at"].as_str().unwrap())
            .expect("merged_at is RFC 3339");
        assert!(branch_node_may_close(branch));

        assert_eq!(
            stored(&h.pool, "job-1").await.manifest,
            job.manifest,
            "the merge was persisted"
        );

        let replay = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        assert_eq!(replay, MergeSubmission::AlreadyMerged { sha });
        assert_eq!(
            request_count(&h.pool).await,
            1,
            "the replay enqueued nothing"
        );
    }

    /// The pattern of `merge_item`: an implement node leaves its tree uncommitted, and a merge of
    /// the branch without a checkpoint would bring an empty branch across and call it success.
    #[tokio::test]
    async fn a_merge_commits_what_the_branch_worktree_left_uncommitted() {
        let _lock = test_env_lock();
        let (container, repo) = repo_with_a_tree_branch("nucleos-landing-checkpoint-");
        let h = harness(&repo).await;
        let worktree = container.path().join("api-tree");
        assert!(git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                &worktree.to_string_lossy(),
                "tree/api"
            ]
        ));
        std::fs::write(worktree.join("wip.txt"), "not committed yet\n").expect("write");
        let mut branch = branch_at("api", "green");
        branch.worktree = Some(worktree.to_string_lossy().into_owned());
        let mut job = saved_job(&h.pool, manifest_for_repo(&repo, vec![branch])).await;

        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;
        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT, true)
            .await
            .unwrap();

        assert!(
            matches!(settled, MergeSettlement::Merged { .. }),
            "{settled:?}"
        );
        assert!(
            git_in(&repo, &["cat-file", "-e", "feat/tree:wip.txt"]),
            "the worktree's uncommitted file reached the target"
        );
    }

    /// A checkpoint that fails means the work exists only in a working tree, so nothing is queued.
    #[tokio::test]
    async fn a_checkpoint_that_fails_queues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let not_a_worktree = tempfile::tempdir().unwrap();
        let mut branch = branch_at("api", "green");
        branch.worktree = Some(not_a_worktree.path().to_string_lossy().into_owned());
        let mut job = saved_job(&pool, manifest_with(vec![branch])).await;

        let result = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon).await;

        assert!(result.is_err(), "{result:?}");
        assert_eq!(request_count(&pool).await, 0);
        assert_eq!(job.manifest.branches["api"].state, "green");
    }

    /// A daemon restarted between submit and settle replays the submit, and a second row would run
    /// the same merge twice.
    #[tokio::test]
    async fn a_second_submit_while_one_is_queued_enqueues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;

        let first = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        let second = submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(request_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn the_queued_merge_survives_in_the_stored_job() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_tree_branch("nucleos-landing-resume-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;

        // As boot would: the job comes back from the store and nothing else is remembered.
        let mut resumed = stored(&h.pool, "job-1").await;
        let settled = settle_merge(&h.pool, &mut resumed, "api", SETTLE_WAIT, true)
            .await
            .unwrap();

        assert!(
            matches!(settled, MergeSettlement::Merged { .. }),
            "{settled:?}"
        );
    }

    #[tokio::test]
    async fn settling_a_merge_the_queue_has_not_run_is_pending_and_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;
        submit_merge(&pool, &synthetic_repo(), &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        let before = job.manifest.clone();

        let settled = settle_merge(&pool, &mut job, "api", Duration::ZERO, true)
            .await
            .unwrap();

        assert_eq!(settled, MergeSettlement::Pending);
        assert_eq!(job.manifest, before);
    }

    /// `merged` is reachable only through a merge the queue ran: there is no settling without one.
    #[tokio::test]
    async fn settling_a_branch_with_no_queued_merge_is_refused_without_change() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "green")])).await;
        let before = job.manifest.clone();

        let result = settle_merge(&pool, &mut job, "api", Duration::ZERO, true).await;

        assert!(
            matches!(result, Err(LandingError::Refused(_))),
            "{result:?}"
        );
        assert_eq!(job.manifest, before);
    }

    #[tokio::test]
    async fn a_conflict_is_aborted_leaves_the_target_clean_and_is_recorded() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_tree_branch("nucleos-landing-conflict-");
        let h = harness(&repo).await;
        let target = sha_of(&repo, "feat/tree");
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;
        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT, true)
            .await
            .unwrap();

        let MergeSettlement::NotMerged { reason } = settled else {
            panic!("expected a refusal, got {settled:?}");
        };
        assert!(!reason.trim().is_empty());

        assert_eq!(
            sha_of(&repo, "feat/tree"),
            target,
            "the target did not move"
        );
        assert_eq!(porcelain(&repo), "", "the target's checkout is clean");
        assert!(
            !repo.join(".git").join("MERGE_HEAD").exists(),
            "no merge was left half done"
        );

        let branch = &job.manifest.branches["api"];
        assert_ne!(branch.state, "merged");
        assert!(!branch.extra.contains_key("merge_sha"));
        assert!(!branch.extra.contains_key("merged_at"));
        assert!(!branch_node_may_close(branch));
        assert_eq!(branch.extra["merge_failure"], json!(reason));
        assert_eq!(
            stored(&h.pool, "job-1").await.manifest,
            job.manifest,
            "the reason was persisted"
        );
    }

    /// The branch's gate still stands and the tree has no `conflicted` state, so it stays `green`;
    /// and the dead request must not answer the next submit, or the branch could never merge.
    #[tokio::test]
    async fn after_a_conflict_the_branch_is_green_and_a_new_submit_queues_a_new_request() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_tree_branch("nucleos-landing-retry-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;

        let first = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        drain(&h.pool).await;
        settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT, true)
            .await
            .unwrap();
        assert_eq!(job.manifest.branches["api"].state, "green");

        let second = submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();

        assert!(
            matches!(second, MergeSubmission::Queued { .. }),
            "{second:?}"
        );
        assert_ne!(first, second);
        assert_eq!(request_count(&h.pool).await, 2);
    }

    /// `(status, settled_at, settled_reason)` of one `vcs_requests` row.
    async fn request_row(pool: &SqlitePool, id: i64) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT status, settled_at, settled_reason FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Gap 4. With `claim_conflict = false` the escalated row stays the resolver's to pick up, the
    /// branch keeps its request id, and the attempt is counted in `branch.extra`.
    #[tokio::test]
    async fn settle_merge_with_claim_false_leaves_the_escalated_row_unclaimed_and_keeps_the_request()
     {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_tree_branch("nucleos-landing-unclaimed-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;
        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        let request_id = job.manifest.branches["api"].extra["merge_request_id"].clone();
        drain(&h.pool).await;

        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT, false)
            .await
            .unwrap();

        assert!(
            matches!(settled, MergeSettlement::Conflicted { .. }),
            "{settled:?}"
        );
        let branch = &job.manifest.branches["api"];
        assert_eq!(branch.state, "green");
        assert_eq!(branch.extra["merge_request_id"], request_id);
        assert_eq!(branch.extra["conflict_attempts"], json!(1));
        assert!(!branch.extra.contains_key("merge_sha"));
        let (status, settled_at, settled_reason) =
            request_row(&h.pool, request_id.as_i64().unwrap()).await;
        assert_eq!(status, "escalated");
        assert_eq!(settled_at, None, "the resolver may still claim the row");
        assert_eq!(settled_reason, None);
        assert_eq!(
            stored(&h.pool, "job-1").await.manifest,
            job.manifest,
            "the attempt was persisted"
        );
    }

    /// Regression, not RED: the claiming form keeps today's behaviour and passes today once the
    /// extra argument compiles. The row is marked `tree-merge` and the request id is dropped.
    #[tokio::test]
    async fn settle_merge_with_claim_true_marks_tree_merge() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_tree_branch("nucleos-landing-claimed-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "green")]),
        )
        .await;
        submit_merge(&h.pool, &h.resolved, &mut job, "api", Origin::Daemon)
            .await
            .unwrap();
        let request_id = job.manifest.branches["api"].extra["merge_request_id"]
            .as_i64()
            .unwrap();
        drain(&h.pool).await;

        let settled = settle_merge(&h.pool, &mut job, "api", SETTLE_WAIT, true)
            .await
            .unwrap();

        assert!(
            matches!(settled, MergeSettlement::NotMerged { .. }),
            "{settled:?}"
        );
        let (status, settled_at, settled_reason) = request_row(&h.pool, request_id).await;
        assert_eq!(status, "escalated");
        assert!(settled_at.is_some());
        assert_eq!(settled_reason.as_deref(), Some("tree-merge"));
        let branch = &job.manifest.branches["api"];
        assert!(!branch.extra.contains_key("merge_request_id"));
        assert!(branch.extra.contains_key("merge_failure"));
    }

    // ---- land: the decision --------------------------------------------------------------------

    #[test]
    fn a_tree_of_merged_branches_may_land() {
        let manifest = manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "merged")]);

        check_may_land(&manifest).unwrap();
    }

    #[test]
    fn dropped_branches_and_finished_docs_do_not_stop_a_landing() {
        let manifest = manifest_with(vec![
            branch_at("api", "merged"),
            branch_at("old", "dropped"),
            docs_branch_at("notes", "done"),
        ]);

        check_may_land(&manifest).unwrap();
    }

    #[test]
    fn a_tree_with_nothing_merged_may_not_land() {
        for branches in [
            vec![],
            vec![branch_at("old", "dropped")],
            vec![docs_branch_at("notes", "done")],
            vec![branch_at("old", "dropped"), docs_branch_at("notes", "done")],
        ] {
            let manifest = manifest_with(branches);

            assert!(
                matches!(check_may_land(&manifest), Err(LandingError::Refused(_))),
                "{:?}",
                manifest.branches.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_parked_branch_blocks_the_landing_and_is_named() {
        let manifest = manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "parked")]);

        let error = check_may_land(&manifest).unwrap_err();

        assert!(matches!(error, LandingError::Refused(_)), "{error:?}");
        assert!(error.to_string().contains("ui"), "{error}");
    }

    #[test]
    fn a_branch_still_at_work_blocks_the_landing_and_is_named() {
        for state in [
            "queued",
            "open",
            "building",
            "gating",
            "green",
            "red",
            "reviewing",
            "fixing",
        ] {
            let manifest = manifest_with(vec![branch_at("api", "merged"), branch_at("ui", state)]);

            let error = check_may_land(&manifest).unwrap_err();

            assert!(
                matches!(error, LandingError::Refused(_)),
                "{state}: {error:?}"
            );
            assert!(error.to_string().contains("ui"), "{state}: {error}");
        }
    }

    /// `done` closes a docs branch only: on a code branch it is not one of the landing states.
    #[test]
    fn a_code_branch_marked_done_blocks_the_landing() {
        let manifest = manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "done")]);

        let error = check_may_land(&manifest).unwrap_err();

        assert!(error.to_string().contains("ui"), "{error}");
    }

    // ---- land: through `land::submit` ----------------------------------------------------------

    #[tokio::test]
    async fn a_landing_the_rules_refuse_enqueues_nothing_and_stays_listed() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(
            &pool,
            manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "green")]),
        )
        .await;

        let result = land_job(
            &pool,
            &synthetic_repo(),
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await;

        assert!(
            matches!(result, Err(LandingError::Refused(_))),
            "{result:?}"
        );
        assert_eq!(request_count(&pool).await, 0);
        assert!(!job.manifest.extra.contains_key("land_id"));
        assert_eq!(listed_ids(&pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn a_landing_submits_and_records_the_land_id_and_the_job_is_not_over_yet() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-land-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        let id = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();

        assert_eq!(job.manifest.extra["land_id"], json!(id));
        assert_eq!(request_count(&h.pool).await, 1);
        let loaded = stored(&h.pool, "job-1").await;
        assert_eq!(loaded.manifest.extra["land_id"], json!(id), "persisted");
        assert_ne!(job.state, JOB_TERMINAL_STATE, "admitted is not landed");
        assert_eq!(listed_ids(&h.pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn a_second_landing_answers_with_the_recorded_id_and_enqueues_nothing() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-replay-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        let first = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        let second = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(request_count(&h.pool).await, 1);
    }

    /// `feat/tree` is `master`, so `land::submit` answers `LandRefusal::NothingToLand`.
    #[tokio::test]
    async fn a_land_refusal_keeps_the_job_live_and_returns_and_records_the_reason() {
        let _lock = test_env_lock();
        let (_container, repo) = fresh_repo("nucleos-landing-refusal-");
        assert!(git_in(&repo, &["branch", "feat/tree"]));
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        let result = land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await;

        let Err(LandingError::LandRefused(reason)) = result else {
            panic!("expected a land refusal, got {result:?}");
        };
        assert!(reason.contains("nothing to land"), "{reason}");
        assert_eq!(job.state, "open");
        assert_eq!(job.manifest.state, "open");
        assert_eq!(job.manifest.extra["land_refusal"], json!(reason));
        assert!(!job.manifest.extra.contains_key("land_id"));
        assert_eq!(request_count(&h.pool).await, 0);

        let loaded = stored(&h.pool, "job-1").await;
        assert_eq!(loaded.state, "open");
        assert_eq!(
            loaded.manifest.extra["land_refusal"],
            json!(reason),
            "persisted"
        );
        assert_eq!(listed_ids(&h.pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn a_landed_job_is_finished_and_leaves_the_list() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-finish-");
        let h = harness(&repo).await;
        let feature = sha_of(&repo, "feat/tree");
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        drain(&h.pool).await;
        let settled = settle_land(&h.pool, &mut job, SETTLE_WAIT).await.unwrap();

        assert!(
            matches!(settled, LandSettlement::Finished { .. }),
            "{settled:?}"
        );
        assert_eq!(sha_of(&repo, "master^2"), feature);
        assert_eq!(job.state, JOB_TERMINAL_STATE);
        assert_eq!(job.manifest.state, JOB_TERMINAL_STATE);
        let loaded = stored(&h.pool, "job-1").await;
        assert_eq!(loaded.state, JOB_TERMINAL_STATE);
        assert_eq!(loaded.manifest.state, JOB_TERMINAL_STATE);
        assert!(listed_ids(&h.pool).await.is_empty());
    }

    /// `land::submit` only admits. A landing that conflicts in the queue has not landed anything,
    /// so finishing the job at admission would strand its work with nothing left to resume it.
    #[tokio::test]
    async fn a_landing_that_fails_in_the_queue_keeps_the_job_live_and_records_why() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_conflicting_feature("nucleos-landing-land-conflict-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;

        land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        drain(&h.pool).await;
        let settled = settle_land(&h.pool, &mut job, SETTLE_WAIT).await.unwrap();

        let LandSettlement::NotLanded { reason } = settled else {
            panic!("expected no landing, got {settled:?}");
        };
        assert!(!reason.trim().is_empty());
        assert_ne!(job.state, JOB_TERMINAL_STATE);
        assert_eq!(job.manifest.extra["land_failure"], json!(reason));
        assert_eq!(
            stored(&h.pool, "job-1").await.manifest,
            job.manifest,
            "persisted"
        );
        assert_eq!(listed_ids(&h.pool).await, ["job-1"]);
    }

    #[tokio::test]
    async fn settling_a_landing_the_queue_has_not_run_is_pending_and_changes_nothing() {
        let _lock = test_env_lock();
        let (_container, repo) = repo_with_a_feature_to_land("nucleos-landing-pending-");
        let h = harness(&repo).await;
        let mut job = saved_job(
            &h.pool,
            manifest_for_repo(&repo, vec![branch_at("api", "merged")]),
        )
        .await;
        land_job(
            &h.pool,
            &h.resolved,
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap();
        let before = job.clone();

        let settled = settle_land(&h.pool, &mut job, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(settled, LandSettlement::Pending);
        assert_eq!(job, before);
    }

    #[tokio::test]
    async fn settling_a_job_with_no_landing_is_refused_and_does_not_finish_it() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = saved_job(&pool, manifest_with(vec![branch_at("api", "merged")])).await;

        let result = settle_land(&pool, &mut job, Duration::ZERO).await;

        assert!(
            matches!(result, Err(LandingError::Refused(_))),
            "{result:?}"
        );
        assert_ne!(job.state, JOB_TERMINAL_STATE);
        assert_eq!(listed_ids(&pool).await, ["job-1"]);
    }

    /// Decision of the packet: a parked job is not over, it resumes at boot. Only `finished` ends a
    /// job, whatever `TERMINAL_STATES` says about a branch.
    #[tokio::test]
    async fn a_parked_job_cannot_land_past_its_parked_branch_and_stays_listed() {
        let pool = crate::testdb::fresh_pool().await;
        let mut manifest =
            manifest_with(vec![branch_at("api", "merged"), branch_at("ui", "parked")]);
        manifest.state = "parked".to_owned();
        let mut job = saved_job(&pool, manifest).await;

        let error = land_job(
            &pool,
            &synthetic_repo(),
            &mut job,
            land_deadline(),
            Origin::Daemon,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("ui"), "{error}");
        assert_eq!(job.state, "parked");
        assert_eq!(listed_ids(&pool).await, ["job-1"]);
    }
}
