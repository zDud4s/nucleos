//! The post-merge gate worker (spec 2026-10-05 §6.1, F3-2).
//!
//! `tick` is one stateless pass over the project roster. Everything it needs lives in
//! `postgate_state` and `verify_requests`, so resuming after a restart is simply the next tick.
//! A project is touched only when its rules set `gate_after_land`; for every other project the
//! tick reads the rules file and stops, which is the default for all of them.
//!
//! The gate it runs is the full one (`verify` scope `full`, the project's `gate_command`), as
//! `Caller::Postgate` at priority 2, in the `postgate-<project>` worktree checked out at the land
//! target's tip. The cache is never consulted.
//!
//! A red (F3-3, spec §6.2) is handled in two persisted phases, one action per tick, and by default
//! nothing is reverted (F3-4 adds the opt-in revert, below). First the same sha is checked again (`Caller::FlakeCheck`): if it passes the red was
//! not the target's, the sha counts green and nobody is told. Otherwise the first-parent commits of
//! `(last green, red]` are bisected with `verify_bisect`, one full probe per step
//! (`Caller::Bisect`), and the outcome goes to the owner as one `postgate_red` feed line. A probe
//! left running is followed from its stored ticket, never submitted again, and while a red is being
//! handled no new gate starts.
//!
//! With the project's `revert_on_red` on (F3-4, D4), a culprit is also reverted: the report starts
//! the revert in the same transaction, the next ticks queue it as an `Op::Revert` and follow its
//! ticket, and once the queue has published it `fix/<source>-<sha7>` is built on top of it. The
//! correction run on that branch is opened by `run_correction_loop`, which holds an `AppState`.
//!
//! F3-12 spares a gate that would only repeat a proof. When a new tip appears, the tick asks
//! whether a daemon `verify` already passed on that tip's exact tree over the last full green or the
//! last covered tip (`verify::covering_request`); if so the tip is recorded as covered, with one
//! feed line, and no gate runs. A cover never moves `last_green_sha`, so a later red still bisects
//! from the last COMPLETE green. The check lives in the tick and not on the merge path: the proof
//! may arrive before or after the land, it works with `gate_before_publish` off, and it adds
//! nothing to the queue's critical path. Two opt-in calm rules (`postgate_min_interval_mins`,
//! `postgate_idle_command`) hold only the start of a NEW batch gate, never a recheck, a bisection
//! or a revert.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::execution::git_exec;
use crate::execution::land;
use crate::execution::vcs::{self, Branch, CommitSha, Op, Origin};
use crate::verify::batch::{self as verify_batch, Commit, Decision, TargetState};
use crate::verify::bisect::{Bisection, Candidate, Next, Probe, Verdict};
use crate::verify::exec::Executor;
use crate::verify::flaky::{self as verify_flaky, Rerun};
use crate::verify::plan::{Kind, ScopeArg};
use crate::verify::postgate::{self as verify_postgate, RedPhase, State};
use crate::verify::runs::{STATUS_FAILED, STATUS_PASSED};
use crate::verify::{self, Caller, VerifyArgs};

mod correction;
mod revert;

pub use self::correction::*;
use self::revert::*;

/// How often the loop looks at the roster.
pub const POSTGATE_POLL: Duration = Duration::from_secs(30);

/// A cached tip is re-read with git at least this often, whatever the ref files say.
const TIP_RECHECK: Duration = Duration::from_secs(600);

/// A ref file touched less than this long ago is not trusted as unchanged: a loose ref is always
/// 41 bytes, so its mtime is the only signal and a second write inside one timestamp tick would
/// be invisible.
const TIP_SETTLE: Duration = Duration::from_secs(2);

/// What `stat` says about the two files a branch tip can live in. `None` inside means the file is
/// absent, which is normal (a packed ref has no loose file).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefFingerprint {
    loose: Option<(std::time::SystemTime, u64)>,
    packed: Option<(std::time::SystemTime, u64)>,
}

struct CachedTip {
    fingerprint: RefFingerprint,
    sha: String,
    read_at: Instant,
}

static TIP_CACHE: LazyLock<Mutex<HashMap<(String, String), CachedTip>>> =
    LazyLock::new(Default::default);

fn stat_of(path: &Path) -> Result<Option<(std::time::SystemTime, u64)>, ()> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some((meta.modified().map_err(|_| ())?, meta.len()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// The fingerprint of `refs/heads/<target>` for a main checkout (`.git` is a directory). `None`
/// when it cannot be taken cheaply and surely — a linked worktree (`.git` is a file), any stat
/// error — and the caller then simply asks git.
fn ref_fingerprint(project_root: &Path, target: &str) -> Option<RefFingerprint> {
    let git_dir = project_root.join(".git");
    if !git_dir.is_dir() {
        return None;
    }
    Some(RefFingerprint {
        loose: stat_of(&git_dir.join("refs").join("heads").join(target)).ok()?,
        packed: stat_of(&git_dir.join("packed-refs")).ok()?,
    })
}

/// Whether the cached sha may stand in for a `git rev-parse`: same fingerprint, read recently, and
/// the files were already settled when it was read.
fn tip_reusable(
    cached: &RefFingerprint,
    cached_age: Duration,
    current: &RefFingerprint,
    settled: bool,
) -> bool {
    settled && cached == current && cached_age < TIP_RECHECK
}

/// True when no file of the fingerprint was modified within `TIP_SETTLE` of `now`.
fn fingerprint_settled(fingerprint: &RefFingerprint, now: std::time::SystemTime) -> bool {
    [fingerprint.loose, fingerprint.packed]
        .into_iter()
        .flatten()
        .all(|(modified, _)| {
            now.duration_since(modified)
                .map(|age| age >= TIP_SETTLE)
                .unwrap_or(false)
        })
}

/// The tip of the land target. The poll runs every 30 s per gated project, and most of the time
/// the branch has not moved, so a stat of the ref files replaces the `git rev-parse` spawn while
/// they are unchanged (and git is still asked at least every `TIP_RECHECK`).
async fn target_tip(
    project_root: &Path,
    target: &str,
    deadline: Instant,
) -> Result<String, String> {
    let key = (
        project_root.to_string_lossy().into_owned(),
        target.to_owned(),
    );
    let fingerprint = ref_fingerprint(project_root, target);
    if let Some(current) = fingerprint.as_ref() {
        let cache = TIP_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = cache.get(&key)
            && tip_reusable(
                &hit.fingerprint,
                hit.read_at.elapsed(),
                current,
                fingerprint_settled(current, std::time::SystemTime::now()),
            )
        {
            return Ok(hit.sha.clone());
        }
    }
    let tip =
        verify::resolve_commit(project_root, &format!("refs/heads/{target}"), deadline).await?;
    let mut cache = TIP_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    match fingerprint {
        Some(fingerprint) => {
            cache.insert(
                key,
                CachedTip {
                    fingerprint,
                    sha: tip.clone(),
                    read_at: Instant::now(),
                },
            );
        }
        None => {
            cache.remove(&key);
        }
    }
    Ok(tip)
}

/// Names a failing unit that has no group: the project's `gate_command`.
pub const GATE_UNIT: &str = "gate_command";

/// What one tick did for one project.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// `verify_batch::decide` found nothing to start.
    Idle(verify_batch::Idle),
    /// A daemon verification of the tip's exact tree passed over a green-or-covered base;
    /// recorded, nothing run.
    Covered(String),
    /// A new batch gate for `sha` is held back, for `hold`; the reason went to the feed once.
    Held {
        sha: String,
        hold: verify_batch::Hold,
    },
    /// A gate was submitted for `sha` as ticket `request`.
    Started { sha: String, request: i64 },
    /// The gate for `sha` is still running.
    Waiting { sha: String },
    /// The gate for `sha` passed.
    Green(String),
    /// The gate for `sha` failed in `groups`.
    Red { sha: String, groups: Vec<String> },
    /// The gate for `sha` produced no verdict; the slot is released and the tip is not retried.
    Abandoned { sha: String, reason: String },
    /// The tick could not read or write its state; nothing was changed by this step.
    Failed(String),
    /// A red was found: the gate was submitted again on `sha`, as ticket `request`.
    Recheck { sha: String, request: i64 },
    /// The recheck passed: `sha` counts as green and nobody was told.
    Flaky(String),
    /// The recheck did not clear `sha`: the bisection begins.
    Confirmed(String),
    /// The bisection submitted a probe of `sha`, as ticket `request`.
    Probe { sha: String, request: i64 },
    /// The probe of `sha` finished and its result was recorded.
    Probed { sha: String, probe: Probe },
    /// The red on `sha` was reported to the owner and its handling is over.
    Reported { sha: String, verdict: Verdict },
    /// The culprit `merge_sha` is being reverted: the revert was queued as ticket `request`.
    Reverting { merge_sha: String, request: i64 },
    /// The queue published the revert of `merge_sha` as `revert_sha`.
    Reverted {
        merge_sha: String,
        revert_sha: String,
    },
    /// The revert of `merge_sha` did not happen, or left no branch; the owner was told and the
    /// revert state is free again.
    RevertFailed { merge_sha: String, reason: String },
    /// The correction branch `branch` was created on top of the published revert.
    FixBranch { branch: String },
}

/// One project's step in one tick.
#[derive(Debug, PartialEq, Eq)]
pub struct Pass {
    pub project_id: String,
    pub step: Step,
}

/// One pass over the roster. Projects whose rules do not set `gate_after_land` yield no pass and
/// nothing beyond their rules file is read.
pub async fn tick(executor: &Arc<Executor>) -> Vec<Pass> {
    let roster: Vec<(String, String)> = match sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL \
         ORDER BY project_id",
    )
    .fetch_all(&executor.pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "postgate: cannot read the project roster");
            return Vec::new();
        }
    };

    let mut passes = Vec::new();
    for (project_id, root) in roster {
        let rules =
            match crate::config::load_schedule_rules(executor.machine_root.as_deref(), &project_id)
            {
                Ok(rules) if rules.gate_after_land => rules,
                _ => continue,
            };
        let step = tick_project(executor, &project_id, Path::new(&root), &rules).await;
        passes.push(Pass { project_id, step });
    }
    passes
}

async fn tick_project(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    rules: &crate::config::AutopilotRules,
) -> Step {
    let pool = &executor.pool;
    let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;
    let state = match verify_postgate::load(pool, project_id).await {
        Ok(state) => state,
        Err(error) => return Step::Failed(format!("cannot read postgate_state: {error}")),
    };

    // A gate is running: finish it from its ticket, never start another. This needs neither the
    // target nor its tip, so a gate left by a restart is settled even if the branch moved on.
    if let Some(state) = state.as_ref()
        && let Some(sha) = state.running_sha.as_deref()
    {
        let Some(request) = state.running_request_id else {
            // Started but never submitted (a crash between the two): submit it now.
            return launch(executor, project_id, project_root, sha, deadline).await;
        };
        return follow(pool, project_id, sha, request).await;
    }

    // A red is being handled: see it through before looking at the tip, so a merge that lands
    // meanwhile starts no gate and joins the next batch.
    if let Some(state) = state.as_ref()
        && state.red_phase.is_some()
    {
        return handle_red(executor, project_id, project_root, state).await;
    }

    // A revert of a confirmed culprit is under way (F3-4): queue it, follow it, build the fix
    // branch. As with a red, no new gate starts meanwhile: the revert changes the tip anyway.
    if let Some(state) = state.as_ref()
        && state.revert_pending()
    {
        return revert_step(executor, project_id, project_root, state).await;
    }

    let target = match land::integration_branch(pool, project_id, project_root, deadline).await {
        Ok(branch) => branch,
        Err(error) => return Step::Failed(format!("no land target: {error}")),
    };
    let tip = match target_tip(project_root, target.as_str(), deadline).await {
        Ok(tip) => tip,
        Err(error) => return Step::Failed(format!("cannot read the target tip: {error}")),
    };

    // After a change of land target the stored shas describe another branch: start fresh.
    let stored = state.as_ref().filter(|s| s.target == target.as_str());
    // A tip whose gate measured nothing is not up to date yet: it reads as never attempted, so
    // `decide` starts it again, within the retry budget.
    let retry = stored.is_some_and(|s| s.unmeasured_retry_due(&tip));
    let view = TargetState {
        tip: &tip,
        last_green: stored.and_then(|s| s.last_green_sha.as_deref()),
        last_attempted: stored
            .filter(|_| !retry)
            .and_then(|s| s.last_attempted_sha.as_deref()),
        running: false,
    };
    // F3-12: a new tip is covered when a daemon `verify` passed on its exact tree over a base the
    // gate holds green or covered. Only asked for a tip the gate has not seen, and any error means
    // no proof: the worst outcome is one more gate, never a false green.
    let mut evidence: Option<verify::Evidence> = None;
    if let Some(stored) = stored
        && stored.last_green_sha.as_deref() != Some(tip.as_str())
        && stored.last_attempted_sha.as_deref() != Some(tip.as_str())
    {
        let bases: Vec<&str> = [
            stored.last_green_sha.as_deref(),
            stored.covered_sha.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !bases.is_empty() {
            let found = match verify::tree_of(project_root, &tip, deadline).await {
                Ok(tree) => verify::covering_request(pool, project_id, &tree, &bases)
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            };
            match found {
                Ok(found) => evidence = found,
                Err(error) => {
                    tracing::warn!(project = project_id, %error, "postgate: no covering proof");
                }
            }
        }
    }
    // The range `(last_green, tip]` is later work; the tip stands alone, covered only by a proof.
    let since_green = [Commit {
        sha: tip.clone(),
        covered: evidence.is_some(),
    }];
    match verify_batch::decide(&view, &since_green) {
        Decision::Idle(idle) => Step::Idle(idle),
        Decision::MarkCovered { sha } => {
            let Some(evidence) = evidence else {
                return Step::Failed(format!("cover of {sha} has no proof"));
            };
            let summary = format!(
                "post-merge gate on {}: {} was not gated. `verify` ticket {} passed on this exact \
                 tree, from a clean worktree, over its diff from {}, which the gate holds green \
                 or covered. The last full green stays {}, so a later red still bisects from \
                 there.",
                target.as_str(),
                short(&sha),
                evidence.ticket,
                short(&evidence.base),
                short(
                    stored
                        .and_then(|s| s.last_green_sha.as_deref())
                        .unwrap_or("unknown")
                ),
            );
            match verify_postgate::settle_covered(
                pool,
                project_id,
                target.as_str(),
                &sha,
                &evidence.base,
                &summary,
            )
            .await
            {
                Ok(true) => Step::Covered(sha),
                Ok(false) => Step::Failed(format!(
                    "cover of {sha} refused: the post-merge state moved"
                )),
                Err(error) => Step::Failed(format!("cannot mark {sha} covered: {error}")),
            }
        }
        Decision::Start(batch) => {
            let target_name = target.as_str();
            if let Some(held) = batch_hold(
                pool,
                project_id,
                project_root,
                target_name,
                &batch.tip,
                rules,
            )
            .await
            {
                return held;
            }
            match verify_postgate::start(pool, project_id, target.as_str(), &batch.tip, None).await
            {
                Ok(true) => launch(executor, project_id, project_root, &batch.tip, deadline).await,
                Ok(false) => Step::Idle(verify_batch::Idle::Running),
                Err(error) => Step::Failed(format!("cannot start the gate: {error}")),
            }
        }
    }
}

/// How long the idle probe may run before the machine counts as busy.
const IDLE_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// The two calm rules for a NEW batch gate (F3-12, decision 12), both off by default: the
/// minimum interval since the last start, then the owner's idle command. `Some` is the step to
/// return (a hold, or a failure to read the state); `None` lets the gate start. Each reason is
/// written to the feed once per episode by `verify_postgate::hold`.
async fn batch_hold(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    target: &str,
    tip: &str,
    rules: &crate::config::AutopilotRules,
) -> Option<Step> {
    if rules.postgate_min_interval_mins > 0 {
        let since = match verify_postgate::seconds_since_last_start(pool, project_id).await {
            Ok(since) => since,
            Err(error) => {
                return Some(Step::Failed(format!("cannot read the last start: {error}")));
            }
        };
        if let Some(remaining_secs) =
            verify_batch::interval_hold(rules.postgate_min_interval_mins, since)
        {
            let summary = format!(
                "post-merge gate on {target}: the next gate waits for the {} min interval since \
                 the last one (about {} min left); merges that land meanwhile join the same gate.",
                rules.postgate_min_interval_mins,
                remaining_secs.div_ceil(60),
            );
            return match verify_postgate::hold(pool, project_id, target, "interval", &summary).await
            {
                Ok(_) => Some(Step::Held {
                    sha: tip.to_owned(),
                    hold: verify_batch::Hold::Interval { remaining_secs },
                }),
                Err(error) => Some(Step::Failed(format!("cannot record the hold: {error}"))),
            };
        }
    }
    if let Some(command) = rules.postgate_idle_command.as_deref()
        && let Err(why) = machine_idle(command, project_root).await
    {
        let summary = format!(
            "post-merge gate on {target}: the next gate waits for an idle machine (`{command}`: \
             {why}); merges that land meanwhile join the same gate."
        );
        return match verify_postgate::hold(pool, project_id, target, "busy", &summary).await {
            Ok(_) => Some(Step::Held {
                sha: tip.to_owned(),
                hold: verify_batch::Hold::Busy,
            }),
            Err(error) => Some(Step::Failed(format!("cannot record the hold: {error}"))),
        };
    }
    None
}

/// Runs the owner's idle command in `root`: `Ok` only when it exits 0 within the probe timeout.
/// Anything else (another exit, a start failure, the timeout) is "busy", with the reason.
async fn machine_idle(command: &str, root: &Path) -> Result<(), String> {
    let words = crate::gate::split_command(command)?;
    let outcome = crate::gate::run_argv(&words, root, &[], IDLE_PROBE_TIMEOUT).await;
    if let Some(error) = outcome.error {
        return Err(error);
    }
    if outcome.timed_out {
        return Err(format!("no answer in {} s", IDLE_PROBE_TIMEOUT.as_secs()));
    }
    match outcome.exit_code {
        Some(0) => Ok(()),
        Some(code) => Err(format!("exit {code}")),
        None => Err("no exit code".to_owned()),
    }
}

/// Reads the ticket of the gate running for `sha` and records its verdict once it is done.
async fn follow(pool: &sqlx::SqlitePool, project_id: &str, sha: &str, request: i64) -> Step {
    let ticket = match verify::read_ticket(pool, request).await {
        Ok(Some((ticket, _))) => ticket,
        Ok(None) => {
            return give_up(pool, project_id, sha, format!("ticket {request} vanished")).await;
        }
        Err(error) => return Step::Failed(format!("cannot read ticket {request}: {error}")),
    };
    if !ticket.done {
        return Step::Waiting {
            sha: sha.to_owned(),
        };
    }
    match ticket.verdict.as_deref() {
        Some("passed") => match verify_postgate::finish_green(pool, project_id, sha).await {
            Ok(_) => Step::Green(sha.to_owned()),
            Err(error) => Step::Failed(format!("cannot record the green: {error}")),
        },
        Some("failed") => {
            let mut groups: Vec<String> = Vec::new();
            for unit in ticket.units.iter().filter(|u| u.status == "failed") {
                let name = unit.group.clone().unwrap_or_else(|| GATE_UNIT.to_owned());
                if !groups.contains(&name) {
                    groups.push(name);
                }
            }
            if groups.is_empty() {
                groups.push(GATE_UNIT.to_owned());
            }
            match verify_postgate::finish_red(pool, project_id, sha, &groups).await {
                Ok(_) => Step::Red {
                    sha: sha.to_owned(),
                    groups,
                },
                Err(error) => Step::Failed(format!("cannot record the red: {error}")),
            }
        }
        other => {
            let reason = format!("ticket {request} ended without a verdict ({other:?})");
            tracing::warn!(project = project_id, sha, %reason, "postgate: gate measured nothing");
            match verify_postgate::abandon_unmeasured(pool, project_id, sha).await {
                Ok(_) => Step::Abandoned {
                    sha: sha.to_owned(),
                    reason,
                },
                Err(error) => Step::Failed(format!("cannot abandon the gate for {sha}: {error}")),
            }
        }
    }
}

/// Prepares the postgate worktree at `sha`, submits the full gate and stores the ticket id.
/// Any failure to launch abandons the gate: nothing was measured, so it is not red.
async fn launch(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    sha: &str,
    deadline: Instant,
) -> Step {
    let pool = &executor.pool;
    let request = match submit_full(
        executor,
        project_id,
        Caller::Postgate,
        project_root,
        sha,
        deadline,
    )
    .await
    {
        Ok(id) => id,
        Err(reason) => return give_up(pool, project_id, sha, reason).await,
    };
    match verify_postgate::set_request(pool, project_id, sha, request).await {
        Ok(true) => Step::Started {
            sha: sha.to_owned(),
            request,
        },
        // The gate was settled by someone else between submit and now; the next tick reads it.
        Ok(false) => Step::Waiting {
            sha: sha.to_owned(),
        },
        Err(error) => Step::Failed(format!("cannot store ticket {request}: {error}")),
    }
}

/// Prepares the postgate worktree at `sha` and submits the full gate for it as `caller`; returns
/// the ticket id, or the reason nothing could be submitted.
///
/// A request the previous daemon submitted under this very claim and never got to record is
/// adopted instead of submitted again (`verify_postgate::orphaned_request`).
async fn submit_full(
    executor: &Arc<Executor>,
    project_id: &str,
    caller: Caller,
    project_root: &Path,
    sha: &str,
    deadline: Instant,
) -> Result<i64, String> {
    match verify_postgate::orphaned_request(&executor.pool, project_id).await {
        Ok(Some(request)) => {
            tracing::info!(
                project = project_id,
                sha,
                request,
                "postgate: adopting the ticket submitted before the daemon stopped"
            );
            return Ok(request);
        }
        Ok(None) => {}
        // Not fatal: the worst case is the duplicate this lookup exists to avoid.
        Err(error) => {
            tracing::warn!(
                project = project_id,
                sha,
                %error,
                "postgate: cannot look for an earlier ticket"
            );
        }
    }
    let tree = git_exec::prepare_postgate_worktree(project_root, sha, deadline).await?;
    let args = VerifyArgs {
        kind: Kind::Test,
        scope: ScopeArg::Full,
        worktree: Some(tree.to_string_lossy().into_owned()),
        files: None,
        base: None,
        wait: false,
        cover: false,
        task_id: None,
    };
    verify::submit(executor, caller, &args)
        .await
        .map_err(|error| error.message().to_owned())
}

/// Releases the slot of the gate running for `sha` without a verdict.
async fn give_up(pool: &sqlx::SqlitePool, project_id: &str, sha: &str, reason: String) -> Step {
    tracing::warn!(project = project_id, sha, %reason, "postgate: gate abandoned");
    match verify_postgate::abandon(pool, project_id, sha).await {
        Ok(_) => Step::Abandoned {
            sha: sha.to_owned(),
            reason,
        },
        Err(error) => Step::Failed(format!("cannot abandon the gate for {sha}: {error}")),
    }
}

/// One action on the red being handled: follow the check or probe in flight, or start the next.
/// Everything it needs is in `state`, so a restart lands here and picks up where it stopped.
async fn handle_red(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    state: &State,
) -> Step {
    let pool = &executor.pool;
    let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;
    let (Some(phase), Some(red_sha)) = (state.red_phase, state.red_sha.as_deref()) else {
        return Step::Failed("a red is being handled without its sha".to_owned());
    };

    // A check or probe is claimed: follow its ticket, or submit it if the daemon died before the
    // ticket was stored.
    if let Some(sha) = state.probe_sha.as_deref() {
        let Some(request) = state.probe_request_id else {
            return launch_probe(executor, project_id, project_root, phase, sha, deadline).await;
        };
        let ticket = match verify::read_ticket(pool, request).await {
            Ok(Some((ticket, _))) => ticket,
            // Nothing can be learned from a ticket that is gone: the probe could not tell.
            Ok(None) => return settle(pool, project_id, phase, sha, "").await,
            Err(error) => return Step::Failed(format!("cannot read ticket {request}: {error}")),
        };
        if !ticket.done {
            return Step::Waiting {
                sha: sha.to_owned(),
            };
        }
        let status = ticket.verdict.as_deref().unwrap_or("");
        return settle(pool, project_id, phase, sha, status).await;
    }

    match phase {
        RedPhase::FlakeCheck => {
            let claimed = verify_postgate::claim_probe(pool, project_id, red_sha).await;
            match claimed {
                Ok(true) => {
                    launch_probe(executor, project_id, project_root, phase, red_sha, deadline).await
                }
                Ok(false) => Step::Waiting {
                    sha: red_sha.to_owned(),
                },
                Err(error) => Step::Failed(format!("cannot claim the recheck: {error}")),
            }
        }
        RedPhase::Bisect => {
            bisect_step(executor, project_id, project_root, state, red_sha, deadline).await
        }
    }
}

/// Submits the check or probe already claimed for `sha` and stores its ticket. A launch that fails
/// settles as inconclusive: nothing was measured, the same as `git bisect skip`.
async fn launch_probe(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    phase: RedPhase,
    sha: &str,
    deadline: Instant,
) -> Step {
    let pool = &executor.pool;
    let caller = match phase {
        RedPhase::FlakeCheck => Caller::FlakeCheck,
        RedPhase::Bisect => Caller::Bisect,
    };
    let request = match submit_full(executor, project_id, caller, project_root, sha, deadline).await
    {
        Ok(id) => id,
        Err(reason) => {
            tracing::warn!(project = project_id, sha, %reason, "postgate: probe not launched");
            return settle(pool, project_id, phase, sha, "").await;
        }
    };
    match verify_postgate::set_probe_request(pool, project_id, sha, request).await {
        Ok(true) => match phase {
            RedPhase::FlakeCheck => Step::Recheck {
                sha: sha.to_owned(),
                request,
            },
            RedPhase::Bisect => Step::Probe {
                sha: sha.to_owned(),
                request,
            },
        },
        // Settled by someone else between submit and now; the next tick reads it.
        Ok(false) => Step::Waiting {
            sha: sha.to_owned(),
        },
        Err(error) => Step::Failed(format!("cannot store ticket {request}: {error}")),
    }
}

/// Records how the check or probe of `sha` ended; `status` is the ticket's verdict, empty when
/// there was none. A refused write means someone else settled it, and the next tick reads that.
async fn settle(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    phase: RedPhase,
    sha: &str,
    status: &str,
) -> Step {
    let waiting = || Step::Waiting {
        sha: sha.to_owned(),
    };
    match phase {
        RedPhase::FlakeCheck => {
            // Only a pass clears the red. A recheck with no verdict proves nothing, so the
            // original red stands and the bisection goes ahead.
            if verify_flaky::rerun_verdict(status) == Rerun::Flaky {
                match verify_postgate::flake_green(pool, project_id, sha).await {
                    Ok(true) => Step::Flaky(sha.to_owned()),
                    Ok(false) => waiting(),
                    Err(error) => Step::Failed(format!("cannot record the flake: {error}")),
                }
            } else {
                match verify_postgate::begin_bisect(pool, project_id, sha).await {
                    Ok(true) => Step::Confirmed(sha.to_owned()),
                    Ok(false) => waiting(),
                    Err(error) => Step::Failed(format!("cannot begin the bisection: {error}")),
                }
            }
        }
        RedPhase::Bisect => {
            let probe = match status {
                STATUS_PASSED => Probe::Green,
                STATUS_FAILED => Probe::Red,
                _ => Probe::Inconclusive,
            };
            match verify_postgate::record_probe(pool, project_id, sha, probe).await {
                Ok(true) => Step::Probed {
                    sha: sha.to_owned(),
                    probe,
                },
                Ok(false) => waiting(),
                Err(error) => Step::Failed(format!("cannot record the probe: {error}")),
            }
        }
    }
}

/// The bisection has no probe in flight: rebuild it from the stored probes and either claim the
/// next sha to probe or report the verdict. Without a green base, or when git cannot list the
/// range, there is nothing to bisect and the red is reported as inconclusive.
async fn bisect_step(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    state: &State,
    red_sha: &str,
    deadline: Instant,
) -> Step {
    let pool = &executor.pool;
    let inconclusive = || Verdict::Inconclusive {
        candidates: Vec::new(),
        also_suspect: Vec::new(),
    };
    let Some(base) = state.red_base_sha.as_deref() else {
        return report_red(executor, project_id, state, red_sha, inconclusive()).await;
    };
    let commits = match git_exec::first_parent_commits(project_root, base, red_sha, deadline).await
    {
        Ok(commits) => commits,
        Err(reason) => {
            tracing::warn!(project = project_id, %reason, "postgate: cannot list the range");
            return report_red(executor, project_id, state, red_sha, inconclusive()).await;
        }
    };
    let mut bisection = Bisection::new(
        commits
            .into_iter()
            .map(|(sha, changes_map)| Candidate { sha, changes_map })
            .collect(),
    );
    for (sha, probe) in &state.probes {
        if let Err(error) = bisection.record(sha, *probe) {
            tracing::warn!(
                project = project_id,
                ?error,
                "postgate: stored probe ignored"
            );
        }
    }
    match bisection.next() {
        Next::Done(verdict) => report_red(executor, project_id, state, red_sha, verdict).await,
        Next::Probe(sha) => {
            let claimed = verify_postgate::claim_probe(pool, project_id, &sha).await;
            match claimed {
                Ok(true) => {
                    let phase = RedPhase::Bisect;
                    launch_probe(executor, project_id, project_root, phase, &sha, deadline).await
                }
                Ok(false) => Step::Waiting { sha },
                Err(error) => Step::Failed(format!("cannot claim the probe: {error}")),
            }
        }
    }
}

/// Stores `verdict` and tells the owner, in one transaction (`verify_postgate::report`). With the
/// project's `revert_on_red` on, a culprit that is not already being reverted also starts its
/// revert in that same transaction (`verify_postgate::report_with_revert`), and the line says so.
/// With it off, and for every other verdict, the line and the state are exactly what they were
/// before F3-4.
async fn report_red(
    executor: &Arc<Executor>,
    project_id: &str,
    state: &State,
    red_sha: &str,
    verdict: Verdict,
) -> Step {
    let pool = &executor.pool;
    let base = state.red_base_sha.as_deref();
    let reverting = matches!(verdict, Verdict::Culprit { .. })
        && crate::config::load_schedule_rules(executor.machine_root.as_deref(), project_id)
            .is_ok_and(|rules| rules.revert_on_red);
    // A culprit always comes with a base (no base reports inconclusive); the check keeps the line
    // and the state in step should that ever change.
    let in_flight = reverting && state.revert_in_flight();
    let start_revert = reverting && base.is_some() && !in_flight;
    let mut summary = match (&verdict, base) {
        (Verdict::Culprit { sha, also_suspect }, Some(base)) if start_revert => {
            reverting_summary(&state.target, red_sha, base, sha, also_suspect)
        }
        _ => red_summary(&state.target, red_sha, base, &verdict),
    };
    if in_flight {
        summary.push_str(" A revert started earlier is still in flight, so none was queued.");
    }
    let reported = if start_revert {
        verify_postgate::report_with_revert(pool, project_id, red_sha, &verdict, &summary).await
    } else {
        verify_postgate::report(pool, project_id, red_sha, &verdict, &summary).await
    };
    match reported {
        Ok(true) => Step::Reported {
            sha: red_sha.to_owned(),
            verdict,
        },
        Ok(false) => Step::Waiting {
            sha: red_sha.to_owned(),
        },
        Err(error) => Step::Failed(format!("cannot report the red: {error}")),
    }
}

/// `sha` cut to the ten characters the feed shows.
fn short(sha: &str) -> &str {
    sha.get(..10).unwrap_or(sha)
}

/// The shas of `list`, shortened and comma-separated.
fn short_list(list: &[String]) -> String {
    list.iter()
        .map(|sha| short(sha))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The feed line for a red target. It always says that nothing was reverted: the owner decides.
fn red_summary(target: &str, red: &str, base: Option<&str>, verdict: &Verdict) -> String {
    let head = format!("post-merge gate red on {target} at {}", short(red));
    let Some(base) = base else {
        return format!("{head}: no green commit to bisect from. Nothing was reverted.");
    };
    let range = format!("range {}..{}", short(base), short(red));
    let suspects = |also: &[String]| {
        if also.is_empty() {
            String::new()
        } else {
            format!(" (also suspect: {})", short_list(also))
        }
    };
    match verdict {
        Verdict::Culprit { sha, also_suspect } => format!(
            "{head}: culprit {}{}; {range}. Nothing was reverted.",
            short(sha),
            suspects(also_suspect)
        ),
        Verdict::Inconclusive {
            candidates,
            also_suspect,
        } if !candidates.is_empty() => format!(
            "{head}: bisection inconclusive, candidates {}{}; {range}. Nothing was reverted.",
            short_list(candidates),
            suspects(also_suspect)
        ),
        Verdict::Inconclusive { .. } | Verdict::NoCandidates => format!(
            "{head}: bisection inconclusive, the range could not be narrowed; {range}. \
             Nothing was reverted."
        ),
    }
}

/// The loop `main` spawns. A failing project is logged and never stops the loop.
pub async fn run_postgate_worker(executor: Arc<Executor>) {
    let mut interval = tokio::time::interval(POSTGATE_POLL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        for pass in tick(&executor).await {
            match pass.step {
                Step::Abandoned { sha, reason } => {
                    tracing::warn!(project = %pass.project_id, %sha, %reason, "postgate: abandoned");
                }
                Step::Failed(error) => {
                    tracing::warn!(project = %pass.project_id, %error, "postgate: tick failed");
                }
                Step::Reported { sha, verdict } => {
                    tracing::info!(
                        project = %pass.project_id, %sha, ?verdict, "postgate: red reported"
                    );
                }
                Step::Reverting { merge_sha, request } => {
                    tracing::info!(
                        project = %pass.project_id, %merge_sha, request, "postgate: revert queued"
                    );
                }
                Step::RevertFailed { merge_sha, reason } => {
                    tracing::warn!(
                        project = %pass.project_id, %merge_sha, %reason, "postgate: revert failed"
                    );
                }
                Step::FixBranch { branch } => {
                    tracing::info!(
                        project = %pass.project_id, %branch, "postgate: fix branch ready"
                    );
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // The fixture holds the process-wide `test_env_lock` across awaits on purpose: it serialises
    // mutation of `NUCLEOS_WORKTREE_ROOT`, and no multi-thread runtime is waiting on it.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    fn fp(loose_len: u64) -> RefFingerprint {
        RefFingerprint {
            loose: Some((std::time::SystemTime::UNIX_EPOCH, loose_len)),
            packed: None,
        }
    }

    #[test]
    fn an_unchanged_settled_fingerprint_reuses_the_cached_tip() {
        assert!(tip_reusable(
            &fp(41),
            Duration::from_secs(30),
            &fp(41),
            true
        ));
    }

    #[test]
    fn a_changed_unsettled_or_stale_fingerprint_asks_git() {
        assert!(!tip_reusable(
            &fp(41),
            Duration::from_secs(30),
            &fp(42),
            true
        ));
        assert!(!tip_reusable(
            &fp(41),
            Duration::from_secs(30),
            &fp(41),
            false
        ));
        assert!(!tip_reusable(&fp(41), TIP_RECHECK, &fp(41), true));
    }

    #[test]
    fn a_recently_touched_ref_file_is_not_settled() {
        let now = std::time::SystemTime::now();
        let fresh = RefFingerprint {
            loose: Some((now, 41)),
            packed: None,
        };
        assert!(!fingerprint_settled(&fresh, now));
        assert!(fingerprint_settled(&fp(41), now));
    }
    use crate::config::VerifyConfig;
    use crate::execution::git_exec::tests::{WorktreeRootEnv, space_free_tempdir};
    use crate::verify::exec::{self as verify_exec, Executor};
    use crate::verify::postgate as verify_postgate;
    use crate::verify::runs::PRIORITY_POSTGATE;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, MutexGuard};
    use std::time::{Duration, Instant};

    /// Long enough for a `git --version` unit on a loaded machine, short enough to bound a test.
    const WAIT: Duration = Duration::from_secs(30);

    /// Runs git in `dir`, asserts it succeeded, and returns its trimmed stdout.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    struct Fixture {
        // Field order is drop order: the lock is released last.
        pool: sqlx::SqlitePool,
        ex: Arc<Executor>,
        root: PathBuf,
        /// The tip of `main`.
        tip: String,
        _env: WorktreeRootEnv,
        _roots: tempfile::TempDir,
        _machine: tempfile::TempDir,
        _repo: tempfile::TempDir,
        _lock: MutexGuard<'static, ()>,
    }

    /// A repository with two commits on `main`, rostered as project `alpha`, whose
    /// `autopilot.yaml` holds `rules` when given. The executor loop is NOT running.
    async fn fixture(rules: Option<&str>) -> Fixture {
        let lock = crate::execution::worktree::test_env_lock();
        let repo = space_free_tempdir("nucleos-postgate-");
        let root = repo.path().to_path_buf();
        git_in(&root, &["init", "-q", "-b", "main"]);
        // The daemon's own git calls (the correction branch's `git revert` in the postgate
        // worktree) carry no identity of their own and rely on the repository's. A runner with no
        // global `user.name` (GitHub's ubuntu image) refuses the commit, `prepare_fix_branch`
        // fails and the worker settles the revert as failed, so the step a test waits for never
        // comes. Linked worktrees share this repository config.
        git_in(&root, &["config", "user.name", "t"]);
        git_in(&root, &["config", "user.email", "t@t"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-q", "-m", "one"]);
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        git_in(&root, &["commit", "-q", "-am", "two"]);
        let tip = git_in(&root, &["rev-parse", "HEAD"]);

        let roots = space_free_tempdir("nucleos-postgate-wt-");
        let env = WorktreeRootEnv::set(roots.path());

        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) \
             VALUES ('alpha', 'active', ?)",
        )
        .bind(root.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .unwrap();

        let machine = tempfile::tempdir().unwrap();
        if let Some(rules) = rules {
            crate::project::state::write_for_test(
                machine.path(),
                "alpha",
                crate::project::state::AUTOPILOT_FILE,
                rules,
            );
        }
        let ex = Executor::new(
            pool.clone(),
            VerifyConfig::default(),
            Some(machine.path().to_path_buf()),
        );
        Fixture {
            pool,
            ex,
            root,
            tip,
            _env: env,
            _roots: roots,
            _machine: machine,
            _repo: repo,
            _lock: lock,
        }
    }

    /// The step `tick` reported for `alpha`, if it reported one.
    async fn step_of(f: &Fixture) -> Option<Step> {
        tick(&f.ex)
            .await
            .into_iter()
            .find(|pass| pass.project_id == "alpha")
            .map(|pass| pass.step)
    }

    /// Ticks until `alpha`'s step satisfies `want`, and returns it. Panics after `WAIT`.
    async fn tick_until(f: &Fixture, want: impl Fn(&Step) -> bool) -> Step {
        let started = Instant::now();
        loop {
            if let Some(step) = step_of(f).await
                && want(&step)
            {
                return step;
            }
            assert!(
                started.elapsed() < WAIT,
                "the gate did not reach the expected step in {WAIT:?}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    const GATE_ON: &str = "gate_command: \"git --version\"\ngate_after_land: true\n";

    #[tokio::test]
    async fn with_gate_after_land_off_the_tick_reads_the_rules_and_nothing_else() {
        // The switch absent from a rules file that exists, and then no rules file at all: the
        // default every project has today.
        for rules in [Some("gate_command: \"git --version\"\n"), None] {
            let f = fixture(rules).await;

            let passes = tick(&f.ex).await;

            assert!(
                passes.is_empty(),
                "no pass for a project that is switched off"
            );
            assert_eq!(count(&f.pool, "postgate_state").await, 0);
            assert_eq!(count(&f.pool, "verify_requests").await, 0);
            assert_eq!(count(&f.pool, "verify_runs").await, 0);
            assert!(
                !verify_postgate::postgate_worktree(&f.root).exists(),
                "no postgate directory is created"
            );
        }
    }

    #[tokio::test]
    async fn a_new_tip_starts_a_full_gate_in_the_postgate_worktree() {
        let f = fixture(Some(GATE_ON)).await;

        let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        let Step::Started { sha, request } = step else {
            unreachable!()
        };
        assert_eq!(sha, f.tip);

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .expect("a row after the gate started");
        assert_eq!(s.target, "main");
        assert_eq!(s.running_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(s.running_request_id, Some(request));
        assert_eq!(s.last_attempted_sha.as_deref(), Some(f.tip.as_str()));

        let (scope, caller, priority, worktree): (String, String, i64, String) = sqlx::query_as(
            "SELECT scope, caller, priority, worktree FROM verify_requests WHERE id = ?",
        )
        .bind(request)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(scope, "full");
        assert_eq!(caller, "postgate");
        assert_eq!(priority, PRIORITY_POSTGATE);

        let tree = verify_postgate::postgate_worktree(&f.root);
        assert!(tree.is_dir(), "the postgate worktree exists");
        assert_eq!(git_in(&tree, &["rev-parse", "HEAD"]), f.tip);
        assert_eq!(
            Path::new(&worktree).file_name(),
            tree.file_name(),
            "the request points at the postgate tree"
        );

        let units: Vec<String> = sqlx::query_scalar("SELECT requested_by FROM verify_runs")
            .fetch_all(&f.pool)
            .await
            .unwrap();
        assert_eq!(
            units,
            vec!["postgate".to_owned()],
            "one unit, labelled postgate"
        );
    }

    #[tokio::test]
    async fn a_green_full_gate_moves_last_green() {
        let f = fixture(Some(GATE_ON)).await;
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));

        let step = tick_until(&f, |s| matches!(s, Step::Green(_))).await;
        assert_eq!(step, Step::Green(f.tip.clone()));

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(s.running_sha, None);
        assert_eq!(s.running_request_id, None);
        assert!(s.red_groups.is_empty());
    }

    #[tokio::test]
    async fn a_red_full_gate_records_the_failing_unit() {
        let f = fixture(Some(
            "gate_command: \"git no-such-subcommand\"\ngate_after_land: true\n",
        ))
        .await;
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));

        let step = tick_until(&f, |s| matches!(s, Step::Red { .. })).await;
        assert_eq!(
            step,
            Step::Red {
                sha: f.tip.clone(),
                groups: vec!["gate_command".to_owned()],
            }
        );

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        // The gate unit has no group, so the failing unit is named by the command's key.
        assert_eq!(s.red_groups, vec!["gate_command".to_owned()]);
        assert_eq!(s.red_since_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(s.last_green_sha, None);
        assert_eq!(s.running_sha, None);
    }

    #[tokio::test]
    async fn a_gate_left_running_across_a_restart_is_finished_not_resubmitted() {
        let f = fixture(Some(GATE_ON)).await;

        // The daemon "dies" with the gate submitted: no executor is running yet.
        let first = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        let Step::Started { request, .. } = first else {
            unreachable!()
        };
        assert_eq!(count(&f.pool, "verify_requests").await, 1);

        // The restarted daemon: a later tick finds the gate running with its ticket id.
        assert!(matches!(step_of(&f).await, Some(Step::Waiting { .. })));
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));

        let step = tick_until(&f, |s| matches!(s, Step::Green(_))).await;
        assert_eq!(step, Step::Green(f.tip.clone()));
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            1,
            "the ticket {request} was polled, never resubmitted"
        );
    }

    #[tokio::test]
    async fn a_gate_started_but_never_submitted_is_submitted_on_the_next_tick() {
        let f = fixture(Some(GATE_ON)).await;
        // The crash between `start` and `submit`: the slot is claimed, no ticket id was stored.
        assert!(
            verify_postgate::start(&f.pool, "alpha", "main", &f.tip, None)
                .await
                .unwrap()
        );
        assert_eq!(count(&f.pool, "verify_requests").await, 0);

        let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        let Step::Started { sha, request } = step else {
            unreachable!()
        };
        assert_eq!(sha, f.tip);

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.running_request_id, Some(request));
        assert_eq!(count(&f.pool, "verify_requests").await, 1);
    }

    /// The window `a_gate_started_but_never_submitted...` leaves out: the request WAS submitted and
    /// the daemon died before `set_request`. Submitting again would queue the whole gate twice.
    #[tokio::test]
    async fn a_gate_submitted_but_never_recorded_adopts_its_request_instead_of_submitting_another()
    {
        let f = fixture(Some(GATE_ON)).await;
        let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        let Step::Started { request, .. } = step else {
            unreachable!()
        };
        assert_eq!(count(&f.pool, "verify_requests").await, 1);

        // The crash: the ticket id never reached `postgate_state`.
        sqlx::query(
            "UPDATE postgate_state SET running_request_id = NULL WHERE project_id = 'alpha'",
        )
        .execute(&f.pool)
        .await
        .unwrap();

        let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        assert_eq!(
            step,
            Step::Started {
                sha: f.tip.clone(),
                request
            }
        );
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.running_request_id, Some(request));
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            1,
            "the request already submitted was adopted, not queued a second time"
        );
    }

    #[tokio::test]
    async fn the_same_tip_is_not_gated_twice() {
        let f = fixture(Some(GATE_ON)).await;
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));
        tick_until(&f, |s| matches!(s, Step::Green(_))).await;

        let again = step_of(&f).await;

        assert_eq!(
            again,
            Some(Step::Idle(crate::verify::batch::Idle::UpToDate)),
            "a green tip is up to date"
        );
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            1,
            "nothing was queued for the same tip"
        );
    }

    #[tokio::test]
    async fn a_launch_that_cannot_submit_is_abandoned() {
        // The switch is on but there is no `gate_command`, so `verify::submit` has nothing to run.
        let f = fixture(Some("gate_after_land: true\n")).await;

        let step = tick_until(&f, |s| matches!(s, Step::Abandoned { .. })).await;
        let Step::Abandoned { sha, .. } = step else {
            unreachable!()
        };
        assert_eq!(sha, f.tip);

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.running_sha, None, "the slot is released");
        assert_eq!(s.running_request_id, None);
        assert_eq!(s.last_attempted_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(s.last_green_sha, None);

        // The same tip is not retried every tick; only a new merge is.
        assert_eq!(
            step_of(&f).await,
            Some(Step::Idle(crate::verify::batch::Idle::UpToDate))
        );
        assert_eq!(count(&f.pool, "verify_requests").await, 0);
    }

    #[tokio::test]
    async fn a_gate_that_ends_without_a_verdict_runs_the_same_tip_once_more() {
        // No executor: each gate unit stays queued until the test errors it, as a timeout would.
        let f = fixture(Some(GATE_ON)).await;

        for run in 1..=2 {
            let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
            assert!(matches!(step, Step::Started { ref sha, .. } if *sha == f.tip));
            sqlx::query(
                "UPDATE verify_runs SET status = 'errored', finished_at = ?, output_tail = 'timeout' \
                 WHERE status = 'queued'",
            )
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&f.pool)
            .await
            .unwrap();
            let step = tick_until(&f, |s| matches!(s, Step::Abandoned { .. })).await;
            assert!(matches!(step, Step::Abandoned { ref sha, .. } if *sha == f.tip));

            let s = verify_postgate::load(&f.pool, "alpha")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(s.running_sha, None, "the slot is released");
            assert_eq!(s.last_attempted_sha.as_deref(), Some(f.tip.as_str()));
            assert_eq!(s.last_green_sha, None);
            assert_eq!(s.unmeasured_runs, run);
        }

        // The retry measured nothing either: the tip is left alone until a new merge.
        assert_eq!(
            step_of(&f).await,
            Some(Step::Idle(crate::verify::batch::Idle::UpToDate))
        );
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            2,
            "one gate and one retry"
        );
    }

    /// How many `verify_runs` units a given caller label produced.
    async fn units_by(pool: &sqlx::SqlitePool, requested_by: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM verify_runs WHERE requested_by = ?")
            .bind(requested_by)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Commits `content` into `file` on `main` and returns the new sha.
    fn commit_file(root: &Path, file: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(file), content).unwrap();
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-q", "-m", message]);
        git_in(root, &["rev-parse", "HEAD"])
    }

    #[tokio::test]
    async fn a_red_that_passes_on_recheck_counts_green_and_tells_nobody() {
        // The gate passes only once the branch `pass` exists. Branches are shared by every
        // worktree, so the postgate tree sees the one created in the fixture's repository.
        let f = fixture(Some(
            "gate_command: \"git rev-parse --verify --quiet refs/heads/pass\"\n\
             gate_after_land: true\n",
        ))
        .await;
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));

        tick_until(&f, |s| matches!(s, Step::Red { .. })).await;
        git_in(&f.root, &["branch", "pass"]);

        let step = tick_until(&f, |s| matches!(s, Step::Flaky(_))).await;
        assert_eq!(step, Step::Flaky(f.tip.clone()));

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(s.red_phase, None);
        assert!(s.red_groups.is_empty());
        assert_eq!(count(&f.pool, "feed").await, 0, "a flake tells nobody");
        assert_eq!(units_by(&f.pool, "flake-check").await, 1);
        assert_eq!(units_by(&f.pool, "bisect").await, 0);
    }

    #[tokio::test]
    async fn a_confirmed_red_bisects_to_the_culprit_and_tells_the_owner() {
        let f = fixture(Some(
            "gate_command: \"git grep -q good -- a.txt\"\ngate_after_land: true\n",
        ))
        .await;
        let c1 = commit_file(&f.root, "a.txt", "good\n", "c1");
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));
        let step = tick_until(&f, |s| matches!(s, Step::Green(_))).await;
        assert_eq!(step, Step::Green(c1.clone()));

        let _c2 = commit_file(&f.root, "b.txt", "b\n", "c2");
        let c3 = commit_file(&f.root, "a.txt", "bad\n", "c3 breaks the gate");
        let c4 = commit_file(&f.root, "c.txt", "c\n", "c4");

        let step = tick_until(&f, |s| matches!(s, Step::Reported { .. })).await;
        assert_eq!(
            step,
            Step::Reported {
                sha: c4.clone(),
                verdict: crate::verify::bisect::Verdict::Culprit {
                    sha: c3.clone(),
                    also_suspect: Vec::new(),
                },
            }
        );

        assert_eq!(units_by(&f.pool, "flake-check").await, 1);
        assert_eq!(units_by(&f.pool, "bisect").await, 2);
        let lines: Vec<(Option<String>, String, String)> =
            sqlx::query_as("SELECT project_id, kind, summary FROM feed")
                .fetch_all(&f.pool)
                .await
                .unwrap();
        assert_eq!(lines.len(), 1, "one line for the owner");
        assert_eq!(lines[0].0.as_deref(), Some("alpha"));
        assert_eq!(lines[0].1, "postgate_red");
        assert!(
            lines[0].2.contains(&c3[..10]),
            "the culprit is named: {}",
            lines[0].2
        );
        assert!(
            lines[0].2.contains("Nothing was reverted"),
            "{}",
            lines[0].2
        );

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.red_phase, None);
        assert_eq!(s.culprit_sha.as_deref(), Some(c3.as_str()));
        assert_eq!(s.last_green_sha.as_deref(), Some(c1.as_str()));
        assert_eq!(
            git_in(&f.root, &["rev-parse", "main"]),
            c4,
            "nothing was reverted"
        );
    }

    #[tokio::test]
    async fn a_red_with_no_green_baseline_is_reported_inconclusive_without_probing() {
        let f = fixture(Some(
            "gate_command: \"git no-such-subcommand\"\ngate_after_land: true\n",
        ))
        .await;
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));

        let step = tick_until(&f, |s| matches!(s, Step::Reported { .. })).await;
        assert_eq!(
            step,
            Step::Reported {
                sha: f.tip.clone(),
                verdict: crate::verify::bisect::Verdict::Inconclusive {
                    candidates: Vec::new(),
                    also_suspect: Vec::new(),
                },
            }
        );
        assert_eq!(units_by(&f.pool, "bisect").await, 0, "nothing to probe");
        assert_eq!(count(&f.pool, "feed").await, 1, "the owner is told");
        let summary: String = sqlx::query_scalar("SELECT summary FROM feed")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert!(summary.contains("no green commit"), "{summary}");
    }

    #[tokio::test]
    async fn a_bisect_probe_left_running_across_a_restart_is_followed_not_resubmitted() {
        let f = fixture(Some(GATE_ON)).await;
        let base = f.tip.clone();
        let c2 = commit_file(&f.root, "b.txt", "b\n", "c2");
        let c3 = commit_file(&f.root, "c.txt", "c\n", "c3");
        // The daemon "died" mid-bisection: green at the base, red at c3, candidates [c2, c3].
        verify_postgate::mark_covered(&f.pool, "alpha", "main", &base)
            .await
            .unwrap();
        assert!(
            verify_postgate::start(&f.pool, "alpha", "main", &c3, None)
                .await
                .unwrap()
        );
        assert!(
            verify_postgate::finish_red(&f.pool, "alpha", &c3, &["gate_command".to_owned()])
                .await
                .unwrap()
        );
        assert!(
            verify_postgate::begin_bisect(&f.pool, "alpha", &c3)
                .await
                .unwrap()
        );

        // No executor yet: the probe is submitted once and then only polled.
        let step = step_of(&f).await.expect("a pass for alpha");
        assert!(
            matches!(&step, Step::Probe { sha, .. } if *sha == c2),
            "the middle candidate is probed first: {step:?}"
        );
        assert_eq!(count(&f.pool, "verify_requests").await, 1);
        assert!(matches!(step_of(&f).await, Some(Step::Waiting { .. })));

        tokio::spawn(verify_exec::run_executor(f.ex.clone()));
        let step = tick_until(&f, |s| matches!(s, Step::Reported { .. })).await;
        assert_eq!(
            step,
            Step::Reported {
                sha: c3.clone(),
                verdict: crate::verify::bisect::Verdict::Culprit {
                    sha: c3.clone(),
                    also_suspect: Vec::new(),
                },
            }
        );
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            1,
            "the running probe was followed, never resubmitted"
        );
    }

    /// The same window for a probe: submitted, then the daemon died before `set_probe_request`.
    #[tokio::test]
    async fn a_probe_submitted_but_never_recorded_adopts_its_request_instead_of_submitting_another()
    {
        let f = fixture(Some(GATE_ON)).await;
        let base = f.tip.clone();
        let c2 = commit_file(
            &f.root, "b.txt", "b
", "c2",
        );
        let c3 = commit_file(
            &f.root, "c.txt", "c
", "c3",
        );
        verify_postgate::mark_covered(&f.pool, "alpha", "main", &base)
            .await
            .unwrap();
        assert!(
            verify_postgate::start(&f.pool, "alpha", "main", &c3, None)
                .await
                .unwrap()
        );
        assert!(
            verify_postgate::finish_red(&f.pool, "alpha", &c3, &["gate_command".to_owned()])
                .await
                .unwrap()
        );
        assert!(
            verify_postgate::begin_bisect(&f.pool, "alpha", &c3)
                .await
                .unwrap()
        );

        let step = step_of(&f).await.expect("a pass for alpha");
        let Step::Probe { sha, request } = step else {
            panic!("the middle candidate is probed first: {step:?}")
        };
        assert_eq!(sha, c2);
        assert_eq!(count(&f.pool, "verify_requests").await, 1);

        // The crash: the probe is claimed and submitted, its ticket id was never stored.
        sqlx::query("UPDATE postgate_state SET probe_request_id = NULL WHERE project_id = 'alpha'")
            .execute(&f.pool)
            .await
            .unwrap();

        let step = step_of(&f).await.expect("a pass for alpha");
        assert_eq!(step, Step::Probe { sha: c2, request });
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.probe_request_id, Some(request));
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            1,
            "the probe already submitted was adopted, not queued a second time"
        );
    }

    /// Adoption is bounded by the claim: a request from an EARLIER claim is never taken for the
    /// current one, or a new tip would inherit the previous tip's verdict.
    #[tokio::test]
    async fn a_request_older_than_the_claim_is_not_adopted() {
        let f = fixture(Some(GATE_ON)).await;
        let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        let Step::Started { request, .. } = step else {
            unreachable!()
        };
        // That gate ends and a new one is claimed: the floor moves past the old request.
        verify_postgate::abandon(&f.pool, "alpha", &f.tip)
            .await
            .unwrap();
        assert!(
            verify_postgate::start(&f.pool, "alpha", "main", &f.tip, None)
                .await
                .unwrap()
        );
        assert_eq!(
            verify_postgate::orphaned_request(&f.pool, "alpha")
                .await
                .unwrap(),
            None,
            "request {request} belongs to the claim before this one"
        );
    }

    #[tokio::test]
    async fn no_new_gate_starts_while_a_red_is_being_handled() {
        let f = fixture(Some(GATE_ON)).await;
        // A red on the current tip, waiting for its recheck. No executor: nothing finishes.
        assert!(
            verify_postgate::start(&f.pool, "alpha", "main", &f.tip, None)
                .await
                .unwrap()
        );
        assert!(
            verify_postgate::finish_red(&f.pool, "alpha", &f.tip, &["gate_command".to_owned()])
                .await
                .unwrap()
        );
        // A merge lands meanwhile.
        let newer = commit_file(&f.root, "b.txt", "b\n", "newer");
        assert_ne!(newer, f.tip);

        let step = step_of(&f).await.expect("a pass for alpha");
        assert!(
            matches!(&step, Step::Recheck { sha, .. } if *sha == f.tip),
            "the red is rechecked at its own sha, not the new tip: {step:?}"
        );

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.running_sha, None, "no new gate was started");
        assert_eq!(s.last_attempted_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(s.red_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(count(&f.pool, "verify_requests").await, 1);
    }

    // --- F3-4: the revert of a confirmed culprit (spec 2026-10-05 §6.2, D4) ---

    /// The rules of the confirmed-culprit scenarios: the gate passes while `a.txt` says `good`.
    const REVERT_OFF: &str = "gate_command: \"git grep -q good -- a.txt\"\ngate_after_land: true\n";
    const REVERT_ON: &str = "gate_command: \"git grep -q good -- a.txt\"\ngate_after_land: true\n\
                             revert_on_red: true\n";

    /// Drives `alpha` to a confirmed culprit: green at `c1`, then `c3` breaks the gate and `c4` lands
    /// on top of it. Returns `(c3, c4)` once the owner has been told. The verify executor is left
    /// running; the vcs queue's executor is not, so a queued revert stays `queued`.
    async fn confirmed_culprit(f: &Fixture) -> (String, String) {
        let c1 = commit_file(&f.root, "a.txt", "good\n", "c1");
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));
        tick_until(f, |s| matches!(s, Step::Green(sha) if *sha == c1)).await;

        commit_file(&f.root, "b.txt", "b\n", "c2");
        let c3 = commit_file(&f.root, "a.txt", "bad\n", "c3 breaks the gate");
        let c4 = commit_file(&f.root, "c.txt", "c\n", "c4");

        let step = tick_until(f, |s| matches!(s, Step::Reported { .. })).await;
        assert!(
            matches!(
                &step,
                Step::Reported {
                    verdict: crate::verify::bisect::Verdict::Culprit { sha, .. },
                    ..
                } if *sha == c3
            ),
            "the culprit is c3: {step:?}"
        );
        (c3, c4)
    }

    /// Every summary the feed holds, oldest first.
    async fn feed_summaries(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// `(id, op, origin, status, args)` of every row in the vcs queue.
    async fn queue_rows(pool: &sqlx::SqlitePool) -> Vec<(i64, String, String, String, String)> {
        sqlx::query_as("SELECT id, op, origin, status, args FROM vcs_requests ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// With the switch off (every project today) the owner gets the same line as before F3-4 and
    /// nothing else moves: no queued request, no revert columns, `main` where it was. This is the
    /// default's whole contract, so it is pinned beside the tests that turn the switch on.
    #[tokio::test]
    async fn with_the_revert_switch_off_a_culprit_is_reported_and_nothing_is_queued() {
        let f = fixture(Some(REVERT_OFF)).await;
        let (c3, c4) = confirmed_culprit(&f).await;

        let lines = feed_summaries(&f.pool).await;
        assert_eq!(lines.len(), 1, "one line for the owner: {lines:?}");
        assert!(lines[0].contains(&c3[..10]), "{}", lines[0]);
        assert!(lines[0].contains("Nothing was reverted"), "{}", lines[0]);

        // A further pass must not queue anything either.
        let _ = step_of(&f).await;
        assert!(queue_rows(&f.pool).await.is_empty(), "nothing is queued");

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.culprit_sha.as_deref(), Some(c3.as_str()));
        assert_eq!(s.revert_merge_sha, None);
        assert_eq!(s.revert_request_id, None);
        assert_eq!(s.revert_sha, None);
        assert_eq!(s.fix_branch, None);
        assert_eq!(s.fix_run_id, None);
        assert_eq!(git_in(&f.root, &["rev-parse", "main"]), c4);
    }

    /// With the switch on, exactly one `revert` is queued for the culprit, by the daemon on the
    /// owner's standing order (`daemon`, already `queued`), and the feed says what is about to be
    /// undone rather than "nothing was reverted".
    #[tokio::test]
    async fn with_the_revert_switch_on_a_culprit_queues_one_revert_and_the_feed_says_so() {
        let f = fixture(Some(REVERT_ON)).await;
        let (c3, c4) = confirmed_culprit(&f).await;

        let lines = feed_summaries(&f.pool).await;
        assert_eq!(lines.len(), 1, "one line for the owner: {lines:?}");
        assert!(lines[0].contains(&c3[..10]), "{}", lines[0]);
        assert!(lines[0].contains("reverting it on main"), "{}", lines[0]);
        assert!(!lines[0].contains("Nothing was reverted"), "{}", lines[0]);

        let step = tick_until(&f, |s| matches!(s, Step::Reverting { .. })).await;
        let Step::Reverting { merge_sha, request } = step else {
            unreachable!("tick_until returned a step it was not asked for");
        };
        assert_eq!(merge_sha, c3);

        let rows = queue_rows(&f.pool).await;
        assert_eq!(rows.len(), 1, "exactly one request: {rows:?}");
        assert_eq!(rows[0].0, request);
        assert_eq!(rows[0].1, "revert");
        assert_eq!(rows[0].2, "daemon");
        assert_eq!(rows[0].3, "queued");
        assert!(rows[0].4.contains(&c3), "the args name the culprit");

        // Followed, never resubmitted.
        assert!(matches!(step_of(&f).await, Some(Step::Waiting { .. })));
        assert_eq!(queue_rows(&f.pool).await.len(), 1);

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.revert_merge_sha.as_deref(), Some(c3.as_str()));
        assert_eq!(s.revert_request_id, Some(request));
        assert_eq!(s.revert_sha, None);
        assert_eq!(
            git_in(&f.root, &["rev-parse", "main"]),
            c4,
            "the queue's executor is not running here: nothing has moved"
        );
    }

    /// An engaged kill switch holds the revert before its first submit: nothing is queued, and it
    /// goes ahead on the pass after the switch is released.
    #[tokio::test]
    async fn a_pending_revert_waits_while_the_kill_switch_is_engaged() {
        let f = fixture(Some(REVERT_ON)).await;
        let (c3, _c4) = confirmed_culprit(&f).await;

        crate::governance::autopilot::set_kill_switch(&f.pool, true)
            .await
            .unwrap();
        for _ in 0..3 {
            assert!(matches!(step_of(&f).await, Some(Step::Waiting { .. })));
        }
        assert!(
            queue_rows(&f.pool).await.is_empty(),
            "nothing is queued under the kill switch"
        );
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.revert_merge_sha.as_deref(), Some(c3.as_str()));
        assert_eq!(s.revert_request_id, None);

        crate::governance::autopilot::set_kill_switch(&f.pool, false)
            .await
            .unwrap();
        tick_until(&f, |s| matches!(s, Step::Reverting { .. })).await;
        assert_eq!(queue_rows(&f.pool).await.len(), 1);
    }

    /// `revert_on_red` turned off between the report and the submit: nothing is queued, the
    /// pending revert is cleared, and the owner is told plainly that nothing was reverted.
    #[tokio::test]
    async fn a_pending_revert_is_dropped_when_the_switch_is_turned_off() {
        let f = fixture(Some(REVERT_ON)).await;
        let (c3, c4) = confirmed_culprit(&f).await;

        crate::project::state::write_for_test(
            f._machine.path(),
            "alpha",
            crate::project::state::AUTOPILOT_FILE,
            REVERT_OFF,
        );
        let step = tick_until(&f, |s| matches!(s, Step::RevertFailed { .. })).await;
        let Step::RevertFailed { merge_sha, .. } = step else {
            unreachable!("tick_until returned a step it was not asked for");
        };
        assert_eq!(merge_sha, c3);

        assert!(queue_rows(&f.pool).await.is_empty(), "nothing is queued");
        let lines = feed_summaries(&f.pool).await;
        let last = lines.last().expect("the owner is told");
        assert!(last.contains("NOT made"), "{last}");
        assert!(last.contains("revert_on_red"), "{last}");
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.revert_merge_sha, None);
        assert_eq!(s.revert_request_id, None);
        assert_eq!(git_in(&f.root, &["rev-parse", "main"]), c4);
    }

    /// F3-9: rules that cannot be read at the moment of the submit do not drop the revert as if the
    /// switch were off. It is held (`Waiting`), nothing is queued, the feed says the rules could not
    /// be read, and once they are readable again the revert goes ahead.
    #[tokio::test]
    async fn a_pending_revert_is_held_when_the_rules_cannot_be_read() {
        let f = fixture(Some(REVERT_ON)).await;
        let (c3, c4) = confirmed_culprit(&f).await;

        crate::project::state::write_for_test(
            f._machine.path(),
            "alpha",
            crate::project::state::AUTOPILOT_FILE,
            "gate_after_land: true\nrevert_on_red: [\n",
        );
        let state = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        let step = revert_step(&f.ex, "alpha", &f.root, &state).await;
        assert!(
            matches!(step, Step::Waiting { ref sha } if *sha == c3),
            "{step:?}"
        );

        assert!(queue_rows(&f.pool).await.is_empty(), "nothing is queued");
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.revert_merge_sha.as_deref(), Some(c3.as_str()));
        let lines = feed_summaries(&f.pool).await;
        let last = lines.last().expect("the owner is told");
        assert!(last.contains("could not be read"), "{last}");
        assert!(!last.contains("turned off"), "{last}");
        assert_eq!(git_in(&f.root, &["rev-parse", "main"]), c4);

        crate::project::state::write_for_test(
            f._machine.path(),
            "alpha",
            crate::project::state::AUTOPILOT_FILE,
            REVERT_ON,
        );
        tick_until(&f, |s| matches!(s, Step::Reverting { .. })).await;
        assert_eq!(queue_rows(&f.pool).await.len(), 1);
    }

    /// A crash between the submit and the store of the ticket leaves a queued revert with no id in
    /// the state: the restart adopts that row and does not submit a second one.
    #[tokio::test]
    async fn a_revert_already_queued_for_the_culprit_is_adopted_not_resubmitted() {
        let f = fixture(Some(REVERT_ON)).await;
        let (c3, _c4) = confirmed_culprit(&f).await;

        let repo = vcs::resolve_repo(&f.pool, "alpha").await.unwrap();
        let op = Op::Revert {
            merge_sha: CommitSha::new(&c3).unwrap(),
            target: Branch::new("main").unwrap(),
        };
        let existing = vcs::submit_declared(&f.pool, &repo, &op, Origin::Human)
            .await
            .unwrap();

        let step = tick_until(&f, |s| matches!(s, Step::Reverting { .. })).await;
        let Step::Reverting { request, .. } = step else {
            unreachable!("tick_until returned a step it was not asked for");
        };
        assert_eq!(request, existing);
        let rows = queue_rows(&f.pool).await;
        assert_eq!(rows.len(), 1, "no second revert: {rows:?}");
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.revert_request_id, Some(existing));
    }

    /// Once the queue has published the revert, the worker prepares `fix/<source>-<sha7>`: a branch
    /// whose tip is a revert of the revert, so it carries the culprit's tree again on top of the
    /// target. Here there is no queued merge row to name the source, so the name falls back to
    /// `merge`.
    #[tokio::test]
    async fn a_succeeded_revert_opens_a_fix_branch_whose_first_commit_reverts_the_revert() {
        let f = fixture(Some(REVERT_ON)).await;
        let (c3, c4) = confirmed_culprit(&f).await;
        let step = tick_until(&f, |s| matches!(s, Step::Reverting { .. })).await;
        let Step::Reverting { request, .. } = step else {
            unreachable!("tick_until returned a step it was not asked for");
        };

        // What the queue would have published: a revert of the culprit on top of the target.
        git_in(&f.root, &["revert", "--no-edit", &c3]);
        let revert = git_in(&f.root, &["rev-parse", "HEAD"]);
        sqlx::query("UPDATE vcs_requests SET status = 'succeeded', result_sha = ? WHERE id = ?")
            .bind(&revert)
            .bind(request)
            .execute(&f.pool)
            .await
            .unwrap();

        let step = tick_until(&f, |s| matches!(s, Step::FixBranch { .. })).await;
        let expected = format!("fix/merge-{}", &c3[..7]);
        assert_eq!(
            step,
            Step::FixBranch {
                branch: expected.clone()
            }
        );

        let branch_ref = format!("refs/heads/{expected}");
        assert_eq!(
            git_in(&f.root, &["rev-parse", &format!("{branch_ref}^1")]),
            revert,
            "the first commit sits directly on the revert"
        );
        assert_eq!(
            git_in(&f.root, &["rev-parse", &format!("{branch_ref}^{{tree}}")]),
            git_in(&f.root, &["rev-parse", &format!("{c4}^{{tree}}")]),
            "reverting the revert puts the culprit's work back"
        );
        assert_eq!(
            git_in(&f.root, &["rev-parse", "main"]),
            revert,
            "main is the revert and stays there"
        );

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.revert_sha.as_deref(), Some(revert.as_str()));
        assert_eq!(s.fix_branch.as_deref(), Some(expected.as_str()));
        assert_eq!(
            s.fix_run_id, None,
            "no run yet: the correction loop opens it"
        );
        let lines = feed_summaries(&f.pool).await;
        assert!(
            lines.last().is_some_and(|line| line.contains(&expected)),
            "the feed names the branch: {lines:?}"
        );
    }

    /// A revert the queue could not perform is told to the owner with its reason, no branch is
    /// made, and the state is free again: the same request is not retried and nothing is left half
    /// open.
    #[tokio::test]
    async fn a_failed_revert_is_reported_and_no_branch_is_made() {
        let f = fixture(Some(REVERT_ON)).await;
        let (c3, c4) = confirmed_culprit(&f).await;
        let step = tick_until(&f, |s| matches!(s, Step::Reverting { .. })).await;
        let Step::Reverting { request, .. } = step else {
            unreachable!("tick_until returned a step it was not asked for");
        };

        sqlx::query("UPDATE vcs_requests SET status = 'failed', failure_reason = ? WHERE id = ?")
            .bind("the revert conflicted")
            .bind(request)
            .execute(&f.pool)
            .await
            .unwrap();

        let step = tick_until(&f, |s| matches!(s, Step::RevertFailed { .. })).await;
        let Step::RevertFailed { merge_sha, reason } = step else {
            unreachable!("tick_until returned a step it was not asked for");
        };
        assert_eq!(merge_sha, c3);
        assert!(reason.contains("the revert conflicted"), "{reason}");

        let lines = feed_summaries(&f.pool).await;
        let last = lines.last().expect("the owner is told");
        assert!(last.contains("the revert conflicted"), "{last}");
        assert!(last.to_lowercase().contains("fail"), "{last}");
        assert!(
            last.contains(&format!("vcs request #{request}")),
            "the line names the vcs request: {last}"
        );

        assert_eq!(
            git_in(&f.root, &["branch", "--list", "fix/*"]),
            "",
            "no branch is made"
        );
        assert_eq!(git_in(&f.root, &["rev-parse", "main"]), c4);
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.revert_merge_sha, None);
        assert_eq!(s.revert_request_id, None);
        assert_eq!(s.revert_sha, None);
        assert_eq!(s.fix_branch, None);
        assert!(!s.revert_in_flight() && !s.revert_pending());

        // Free means free: the next pass queues nothing.
        let _ = step_of(&f).await;
        assert_eq!(queue_rows(&f.pool).await.len(), 1, "never resubmitted");
    }

    /// Puts `NUCLEOS_MIN_FREE_DISK_GB` at 0 for a test, and back as it stood: the disk floor
    /// measures the machine, not the code under test.
    struct DiskFloorOff(Option<std::ffi::OsString>);

    impl DiskFloorOff {
        fn new() -> Self {
            let previous = std::env::var_os("NUCLEOS_MIN_FREE_DISK_GB");
            unsafe { std::env::set_var("NUCLEOS_MIN_FREE_DISK_GB", "0") };
            Self(previous)
        }
    }

    impl Drop for DiskFloorOff {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(value) => std::env::set_var("NUCLEOS_MIN_FREE_DISK_GB", value),
                    None => std::env::remove_var("NUCLEOS_MIN_FREE_DISK_GB"),
                }
            }
        }
    }

    /// A project whose revert has landed and whose fix branch is prepared, waiting for its
    /// correction run, with a real `AppState` to open it in.
    struct CorrectionFixture {
        // Field order is drop order: the lock is released last.
        state: crate::state::AppState,
        fix_branch: String,
        _floor: DiskFloorOff,
        _env: WorktreeRootEnv,
        _roots: tempfile::TempDir,
        _container: tempfile::TempDir,
        _lock: MutexGuard<'static, ()>,
    }

    async fn correction_fixture() -> CorrectionFixture {
        let lock = crate::execution::worktree::test_env_lock();
        let container = space_free_tempdir("nucleos-correction-");
        let root = container.path().join("repo");
        crate::execution::git_exec::testkit::initialize_repo(&root);
        let roots = space_free_tempdir("nucleos-correction-wt-");
        let env = WorktreeRootEnv::set(roots.path());
        let floor = DiskFloorOff::new();

        let fix_branch = "fix/merge-1a2b3c4".to_owned();
        let tip = git_in(&root, &["rev-parse", "HEAD"]);
        git_in(&root, &["checkout", "-q", "-b", &fix_branch]);
        std::fs::write(root.join("fix.txt"), "the work the run builds on\n").unwrap();
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-q", "-m", "revert the revert"]);
        git_in(&root, &["checkout", "-q", "-"]);

        let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(30)),
            Duration::from_secs(120),
        )
        .await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) \
             VALUES ('alpha', 'active', ?)",
        )
        .bind(root.to_string_lossy().into_owned())
        .execute(&state.pool)
        .await
        .unwrap();
        verify_postgate::mark_covered(&state.pool, "alpha", "main", &tip)
            .await
            .unwrap();
        // The state the worker leaves behind once the fix branch exists and no run does.
        sqlx::query(
            "UPDATE postgate_state SET culprit_sha = ?, revert_merge_sha = ?, \
                    revert_request_id = 7, revert_sha = ?, fix_branch = ? \
             WHERE project_id = 'alpha'",
        )
        .bind(&tip)
        .bind(&tip)
        .bind(&tip)
        .bind(&fix_branch)
        .execute(&state.pool)
        .await
        .unwrap();

        CorrectionFixture {
            state,
            fix_branch,
            _floor: floor,
            _env: env,
            _roots: roots,
            _container: container,
            _lock: lock,
        }
    }

    /// One correction run per fix branch: it is opened on that branch, the id is recorded so no
    /// second pass opens another, and the owner is told which run to look at.
    #[tokio::test]
    async fn open_corrections_opens_one_run_per_fix_branch_and_records_it() {
        let f = correction_fixture().await;

        let opened = open_corrections(&f.state).await;
        assert_eq!(opened.len(), 1, "one run for the one fix branch");
        let run = opened[0];

        let s = verify_postgate::load(&f.state.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.fix_run_id, Some(run));

        let (cwd, prompt): (String, String) =
            sqlx::query_as("SELECT cwd, prompt FROM runs WHERE id = ?")
                .bind(run)
                .fetch_one(&f.state.pool)
                .await
                .unwrap();
        assert!(
            prompt.contains(&f.fix_branch),
            "the prompt names the branch"
        );
        assert!(
            Path::new(&cwd).join("fix.txt").exists(),
            "the run starts on the fix branch's work"
        );

        let lines = feed_summaries(&f.state.pool).await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("correction run {run}")) && l.contains(&f.fix_branch)),
            "the feed names the run and the branch: {lines:?}"
        );

        assert!(
            open_corrections(&f.state).await.is_empty(),
            "a branch that has its run gets no second one"
        );
        assert_eq!(count(&f.state.pool, "runs").await, 1);
    }

    /// A full project is not a failure: the claim is given back so a later pass tries again, and
    /// the engaged kill switch stops every pass before it claims anything.
    #[tokio::test]
    async fn open_corrections_releases_its_claim_when_busy_and_waits_for_the_kill_switch() {
        let f = correction_fixture().await;
        let pool = &f.state.pool;

        // The project's only slot is held by a run parked for approval.
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = 1")
            .execute(pool)
            .await
            .unwrap();
        let pinned = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at) \
             VALUES ('alpha', 'unused', 'pinned', 'awaiting_approval', 'worktree', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        crate::execution::concurrency::claim(
            pool,
            "alpha",
            crate::execution::worktree::Owner::Run(pinned),
        )
        .await
        .expect("the parked run holds the slot");

        assert!(open_corrections(&f.state).await.is_empty(), "no room");
        let s = verify_postgate::load(pool, "alpha").await.unwrap().unwrap();
        assert_eq!(s.fix_run_id, None, "the claim was released, not kept at 0");
        assert_eq!(count(pool, "runs").await, 1, "only the parked run exists");

        // Room again, but the kill switch is engaged: nothing is claimed or opened.
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = 8")
            .execute(pool)
            .await
            .unwrap();
        crate::governance::autopilot::set_kill_switch(pool, true)
            .await
            .unwrap();
        assert!(open_corrections(&f.state).await.is_empty());
        let s = verify_postgate::load(pool, "alpha").await.unwrap().unwrap();
        assert_eq!(
            s.fix_run_id, None,
            "nothing was claimed under the kill switch"
        );
        assert_eq!(count(pool, "runs").await, 1);

        // Released: the same branch gets its run on the next pass.
        crate::governance::autopilot::set_kill_switch(pool, false)
            .await
            .unwrap();
        let opened = open_corrections(&f.state).await;
        assert_eq!(opened.len(), 1);
        let s = verify_postgate::load(pool, "alpha").await.unwrap().unwrap();
        assert_eq!(s.fix_run_id, Some(opened[0]));
    }

    /// F3-10: a definitive failure no longer leaves the claim at 0 for good. A branch may be
    /// tried `CORRECTION_ATTEMPTS` times, `CORRECTION_BACKOFF` apart; after the last it is given up.
    #[test]
    fn correction_retry_allows_three_attempts_spaced_by_the_backoff() {
        let base = Instant::now();
        assert_eq!(CORRECTION_ATTEMPTS, 3);
        assert_eq!(CORRECTION_BACKOFF, Duration::from_secs(10 * 60));

        assert!(may_try(None, base), "a branch never tried is tried at once");

        let tried = Attempts {
            count: 1,
            last: base,
        };
        assert!(!may_try(Some(&tried), base), "not in the same instant");
        assert!(
            !may_try(
                Some(&tried),
                base + CORRECTION_BACKOFF - Duration::from_secs(1)
            ),
            "not a second before the backoff is over"
        );
        assert!(
            may_try(Some(&tried), base + CORRECTION_BACKOFF),
            "once the backoff has passed"
        );

        assert!(after_failure(1), "the first failure releases the claim");
        assert!(after_failure(2), "the second failure releases the claim");
        assert!(!after_failure(3), "the third failure gives up");
    }

    /// F3-10: a branch that cannot be opened (it does not exist) is a definitive failure. The claim
    /// goes back to `None` and the feed says it will try again; a second pass straight after opens
    /// nothing and writes nothing, because the backoff has not elapsed.
    #[tokio::test]
    async fn open_corrections_releases_its_claim_after_a_definitive_failure_and_waits_the_backoff()
    {
        let f = correction_fixture().await;
        let pool = &f.state.pool;
        // The retry count is process-wide, so the branch name must not collide with another test.
        let missing = format!(
            "fix/missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        sqlx::query("UPDATE postgate_state SET fix_branch = ? WHERE project_id = 'alpha'")
            .bind(&missing)
            .execute(pool)
            .await
            .unwrap();

        assert!(open_corrections(&f.state).await.is_empty(), "nothing opens");
        let s = verify_postgate::load(pool, "alpha").await.unwrap().unwrap();
        assert_eq!(
            s.fix_run_id, None,
            "a definitive failure releases the claim instead of keeping it at 0"
        );
        assert_eq!(
            count(pool, "runs").await,
            1,
            "the failed provisioning is kept as a failed run"
        );
        let lines = feed_summaries(pool).await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("trying again") && l.contains(&missing)),
            "the feed says it will try again and names the branch: {lines:?}"
        );

        assert!(
            open_corrections(&f.state).await.is_empty(),
            "the backoff has not elapsed"
        );
        let s = verify_postgate::load(pool, "alpha").await.unwrap().unwrap();
        assert_eq!(s.fix_run_id, None, "the second pass did not even claim");
        assert_eq!(
            count(pool, "runs").await,
            1,
            "the backoff opens no second run"
        );
        assert_eq!(
            feed_summaries(pool).await,
            lines,
            "the second pass wrote nothing to the feed"
        );
    }

    /// F3-11: the backoff in the feed is derived from `CORRECTION_BACKOFF`, not typed by hand.
    #[test]
    fn backoff_text_names_whole_minutes_and_falls_back_to_seconds() {
        assert_eq!(backoff_text(CORRECTION_BACKOFF), "10 minutes");
        assert_eq!(backoff_text(Duration::from_secs(60)), "1 minute");
        assert_eq!(backoff_text(Duration::from_secs(90)), "90 seconds");
    }

    /// F3-11: a released claim will be picked again, and the feed says when.
    #[test]
    fn correction_outcome_after_a_released_claim_names_the_backoff() {
        assert_eq!(correction_outcome(true, true), "trying again in 10 minutes");
    }

    /// F3-11: a claim that could not be released stays at 0, so the feed must not promise a retry.
    #[test]
    fn correction_outcome_after_a_failed_release_says_nothing_will_retry() {
        let outcome = correction_outcome(true, false);
        assert!(!outcome.contains("trying again"), "{outcome}");
        assert!(outcome.contains("nothing will retry"), "{outcome}");
    }

    /// F3-11: on the last attempt the feed gives up, whether or not a release was tried.
    #[test]
    fn correction_outcome_after_the_last_attempt_says_it_gave_up() {
        assert_eq!(correction_outcome(false, true), "gave up");
        assert_eq!(correction_outcome(false, false), "gave up");
    }

    /// F3-12: how many `postgate_held` lines the feed holds.
    async fn held_lines(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'postgate_held'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// F3-12: plants what a passed daemon `verify` leaves behind: a `test`/`scope` ticket over
    /// `base` whose only unit counts as passed (reused from a green run) and whose recorded tree is
    /// the tree of `covered_tip`. Returns the ticket id.
    async fn seed_proof(f: &Fixture, base: &str, covered_tip: &str) -> i64 {
        use crate::verify::store::{self as verify_store, NewRequest, PlannedUnit};

        let worktree = f.root.to_string_lossy().into_owned();
        let id = verify_store::insert_request(
            &f.pool,
            &NewRequest {
                project_id: "alpha",
                worktree: &worktree,
                kind: "test",
                scope: "scope",
                base: Some(base),
                priority: crate::verify::runs::PRIORITY_INTERACTIVE,
                caller: "owner",
                note: None,
                unclaimed: &[],
            },
        )
        .await
        .unwrap();
        let unit = PlannedUnit {
            group: Some("g".to_owned()),
            argv: vec!["git".to_owned(), "--version".to_owned()],
            why: "paths: x".to_owned(),
            fingerprint: Some("f".to_owned()),
            cacheable: true,
            run_id: Some(1),
            cached_from: Some(1),
            skipped: None,
        };
        verify_store::set_plan(&f.pool, id, &[unit]).await.unwrap();
        let tree = git_in(&f.root, &["rev-parse", &format!("{covered_tip}^{{tree}}")]);
        assert!(
            verify_store::set_measured_tree(&f.pool, id, &tree)
                .await
                .unwrap()
        );
        id
    }

    #[tokio::test]
    async fn a_tip_whose_tree_a_daemon_verification_passed_is_not_gated_and_the_feed_says_so_once()
    {
        let f = fixture(Some(GATE_ON)).await;
        let first = git_in(&f.root, &["rev-parse", "HEAD~1"]);
        verify_postgate::mark_covered(&f.pool, "alpha", "main", &first)
            .await
            .unwrap();
        seed_proof(&f, &first, &f.tip).await;

        let step = step_of(&f).await;
        assert_eq!(step, Some(Step::Covered(f.tip.clone())));
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            1,
            "only the seeded proof: no gate was submitted"
        );
        assert_eq!(held_lines(&f.pool).await, 1);
        let summaries = feed_summaries(&f.pool).await;
        assert!(
            summaries.iter().any(|s| s.contains(&f.tip[..7])),
            "the feed names the tip: {summaries:?}"
        );

        // A second look finds the tip attempted: still one line, still no gate.
        assert_eq!(
            step_of(&f).await,
            Some(Step::Idle(verify_batch::Idle::UpToDate))
        );
        assert_eq!(held_lines(&f.pool).await, 1);
        assert_eq!(count(&f.pool, "verify_requests").await, 1);

        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some(first.as_str()));
        assert_eq!(s.covered_sha.as_deref(), Some(f.tip.as_str()));
        assert_eq!(s.running_sha, None);
    }

    #[tokio::test]
    async fn a_cover_chains_on_the_previous_covered_tip_and_the_green_stays() {
        let f = fixture(Some(GATE_ON)).await;
        let first = git_in(&f.root, &["rev-parse", "HEAD~1"]);
        verify_postgate::mark_covered(&f.pool, "alpha", "main", &first)
            .await
            .unwrap();
        seed_proof(&f, &first, &f.tip).await;
        assert_eq!(step_of(&f).await, Some(Step::Covered(f.tip.clone())));

        // A later merge, proven over the previously covered tip (not over the green).
        let t2 = commit_file(&f.root, "b.txt", "x\n", "three");
        seed_proof(&f, &f.tip, &t2).await;

        assert_eq!(step_of(&f).await, Some(Step::Covered(t2.clone())));
        let s = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.covered_sha.as_deref(), Some(t2.as_str()));
        assert_eq!(
            s.last_green_sha.as_deref(),
            Some(first.as_str()),
            "a cover never moves the full green"
        );
        assert_eq!(
            count(&f.pool, "verify_requests").await,
            2,
            "two seeded proofs, no gate"
        );
        assert_eq!(held_lines(&f.pool).await, 2);
    }

    #[tokio::test]
    async fn with_an_interval_later_merges_wait_and_join_one_gate() {
        let f = fixture(Some(
            "gate_command: \"git --version\"\ngate_after_land: true\n\
             postgate_min_interval_mins: 60\n",
        ))
        .await;
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));
        tick_until(&f, |s| matches!(s, Step::Green(_))).await;
        assert_eq!(count(&f.pool, "verify_requests").await, 1);

        // Two merges land inside the interval: each tick holds, on the newest tip.
        let c1 = commit_file(&f.root, "b.txt", "1\n", "three");
        let step = step_of(&f).await;
        assert!(
            matches!(&step, Some(Step::Held { sha, hold: verify_batch::Hold::Interval { .. } }) if *sha == c1),
            "{step:?}"
        );
        let c2 = commit_file(&f.root, "b.txt", "2\n", "four");
        let step = step_of(&f).await;
        assert!(
            matches!(&step, Some(Step::Held { sha, hold: verify_batch::Hold::Interval { .. } }) if *sha == c2),
            "{step:?}"
        );
        assert_eq!(held_lines(&f.pool).await, 1, "one line per episode");
        assert_eq!(count(&f.pool, "verify_requests").await, 1);

        // Once the interval has passed, ONE gate starts, for the latest tip.
        sqlx::query("UPDATE postgate_state SET last_started_at = datetime('now','-2 hours')")
            .execute(&f.pool)
            .await
            .unwrap();
        let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        let Step::Started { sha, .. } = step else {
            unreachable!()
        };
        assert_eq!(sha, c2);
        assert_eq!(count(&f.pool, "verify_requests").await, 2);
    }

    #[tokio::test]
    async fn a_busy_machine_holds_the_gate_and_an_idle_one_starts_it() {
        let f = fixture(Some(
            "gate_command: \"git --version\"\ngate_after_land: true\n\
             postgate_idle_command: \"git no-such-subcommand\"\n",
        ))
        .await;

        for _ in 0..2 {
            let step = step_of(&f).await;
            assert_eq!(
                step,
                Some(Step::Held {
                    sha: f.tip.clone(),
                    hold: verify_batch::Hold::Busy,
                })
            );
        }
        assert_eq!(held_lines(&f.pool).await, 1, "one line per episode");
        assert_eq!(count(&f.pool, "verify_requests").await, 0);
        let summaries = feed_summaries(&f.pool).await;
        assert!(
            summaries.iter().any(|s| s.contains("no-such-subcommand")),
            "the feed names the command: {summaries:?}"
        );

        // The machine goes quiet: the same command now answers 0.
        crate::project::state::write_for_test(
            f._machine.path(),
            "alpha",
            crate::project::state::AUTOPILOT_FILE,
            "gate_command: \"git --version\"\ngate_after_land: true\n\
             postgate_idle_command: \"git --version\"\n",
        );
        let step = tick_until(&f, |s| matches!(s, Step::Started { .. })).await;
        let Step::Started { sha, .. } = step else {
            unreachable!()
        };
        assert_eq!(sha, f.tip);
        assert_eq!(count(&f.pool, "verify_requests").await, 1);
    }
}
