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

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::git_exec;
use crate::land;
use crate::vcs::{self, Branch, CommitSha, Op, Origin};
use crate::verify::{self, Caller, VerifyArgs};
use crate::verify_batch::{self, Commit, Decision, TargetState};
use crate::verify_bisect::{Bisection, Candidate, Next, Probe, Verdict};
use crate::verify_exec::Executor;
use crate::verify_flaky::{self, Rerun};
use crate::verify_plan::{Kind, ScopeArg};
use crate::verify_postgate::{self, RedPhase, State};
use crate::verify_runs::{STATUS_FAILED, STATUS_PASSED};

/// How often the loop looks at the roster.
pub const POSTGATE_POLL: Duration = Duration::from_secs(30);

/// Names a failing unit that has no group: the project's `gate_command`.
pub(crate) const GATE_UNIT: &str = "gate_command";

/// What one tick did for one project.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// `verify_batch::decide` found nothing to start.
    Idle(verify_batch::Idle),
    /// The tip had already passed the full gate before publish; recorded as green, nothing run.
    Covered(String),
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
        match crate::config::load_schedule_rules(executor.machine_root.as_deref(), &project_id) {
            Ok(rules) if rules.gate_after_land => {}
            _ => continue,
        }
        let step = tick_project(executor, &project_id, Path::new(&root)).await;
        passes.push(Pass { project_id, step });
    }
    passes
}

async fn tick_project(executor: &Arc<Executor>, project_id: &str, project_root: &Path) -> Step {
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
    let tip = match verify::resolve_commit(
        project_root,
        &format!("refs/heads/{}", target.as_str()),
        deadline,
    )
    .await
    {
        Ok(tip) => tip,
        Err(error) => return Step::Failed(format!("cannot read the target tip: {error}")),
    };

    // After a change of land target the stored shas describe another branch: start fresh.
    let stored = state.as_ref().filter(|s| s.target == target.as_str());
    let view = TargetState {
        tip: &tip,
        last_green: stored.and_then(|s| s.last_green_sha.as_deref()),
        last_attempted: stored.and_then(|s| s.last_attempted_sha.as_deref()),
        running: false,
    };
    // The range `(last_green, tip]` is later work; the tip stands alone, not covered.
    let since_green = [Commit {
        sha: tip.clone(),
        covered: false,
    }];
    match verify_batch::decide(&view, &since_green) {
        Decision::Idle(idle) => Step::Idle(idle),
        Decision::MarkCovered { sha } => {
            match verify_postgate::mark_covered(pool, project_id, target.as_str(), &sha).await {
                Ok(()) => Step::Covered(sha),
                Err(error) => Step::Failed(format!("cannot mark {sha} covered: {error}")),
            }
        }
        Decision::Start(batch) => {
            match verify_postgate::start(pool, project_id, target.as_str(), &batch.tip, None).await
            {
                Ok(true) => launch(executor, project_id, project_root, &batch.tip, deadline).await,
                Ok(false) => Step::Idle(verify_batch::Idle::Running),
                Err(error) => Step::Failed(format!("cannot start the gate: {error}")),
            }
        }
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
            give_up(pool, project_id, sha, reason).await
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
    let request = match submit_full(executor, Caller::Postgate, project_root, sha, deadline).await {
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
async fn submit_full(
    executor: &Arc<Executor>,
    caller: Caller,
    project_root: &Path,
    sha: &str,
    deadline: Instant,
) -> Result<i64, String> {
    let tree = git_exec::prepare_postgate_worktree(project_root, sha, deadline).await?;
    let args = VerifyArgs {
        kind: Kind::Test,
        scope: ScopeArg::Full,
        worktree: Some(tree.to_string_lossy().into_owned()),
        files: None,
        base: None,
        wait: false,
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
    let request = match submit_full(executor, caller, project_root, sha, deadline).await {
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

/// The feed line for a culprit whose revert has just been started: what is about to be undone, on
/// which branch, and that a correction run follows. Only the revert switch reaches it.
fn reverting_summary(
    target: &str,
    red: &str,
    base: &str,
    culprit: &str,
    also_suspect: &[String],
) -> String {
    let suspects = if also_suspect.is_empty() {
        String::new()
    } else {
        format!(" (also suspect: {})", short_list(also_suspect))
    };
    format!(
        "post-merge gate red on {target} at {}: culprit {}{suspects}; range {}..{}; \
         reverting it on {target} through the queue, then a correction run follows on a \
         fix/ branch.",
        short(red),
        short(culprit),
        short(base),
        short(red)
    )
}

/// One action on the revert of a confirmed culprit: queue it, follow its ticket, or build the
/// correction branch once the queue has published it. Everything it needs is in `state`, so a
/// restart lands here and picks up where it stopped; a request is never submitted twice.
async fn revert_step(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    state: &State,
) -> Step {
    let pool = &executor.pool;
    let Some(merge_sha) = state.revert_merge_sha.as_deref() else {
        return Step::Failed("a revert is pending without its culprit".to_owned());
    };
    if let Some(revert_sha) = state.revert_sha.as_deref() {
        return fix_branch_step(executor, project_id, project_root, state, revert_sha).await;
    }
    let Some(request) = state.revert_request_id else {
        return queue_revert(executor, project_id, state, merge_sha).await;
    };
    let ticket = match vcs::wait_for(pool, request, Duration::ZERO).await {
        Ok(ticket) => ticket,
        Err(sqlx::Error::RowNotFound) => {
            let reason = format!("revert request {request} vanished");
            return revert_failed(pool, project_id, state, merge_sha, reason).await;
        }
        Err(error) => {
            return Step::Failed(format!("cannot read revert request {request}: {error}"));
        }
    };
    if !vcs::TERMINAL_STATUSES.contains(&ticket.status.as_str()) {
        return Step::Waiting {
            sha: merge_sha.to_owned(),
        };
    }
    match (ticket.status.as_str(), ticket.result_sha) {
        ("succeeded", Some(revert_sha)) => {
            match verify_postgate::record_revert(pool, project_id, merge_sha, &revert_sha).await {
                Ok(true) => Step::Reverted {
                    merge_sha: merge_sha.to_owned(),
                    revert_sha,
                },
                Ok(false) => Step::Waiting {
                    sha: merge_sha.to_owned(),
                },
                Err(error) => Step::Failed(format!("cannot record the revert: {error}")),
            }
        }
        ("succeeded", None) => {
            let reason = "the queue reported success without a commit".to_owned();
            revert_failed(pool, project_id, state, merge_sha, reason).await
        }
        (status, _) => {
            let reason = ticket
                .failure_reason
                .unwrap_or_else(|| format!("the request ended {status}"));
            revert_failed(pool, project_id, state, merge_sha, reason).await
        }
    }
}

/// Submits the revert of `merge_sha` to the vcs queue and stores its ticket. It goes in as the
/// owner's standing order (`Origin::Daemon`): turning `revert_on_red` on is that order.
///
/// A request already queued for this culprit (a crash between the submit and the store of its
/// ticket) is adopted instead of submitted again. Before the first submit the kill switch is read
/// fail-closed, and `revert_on_red` is read again: the owner may have turned it off since the
/// report, and an autonomous publish to the target must honour that.
async fn queue_revert(
    executor: &Arc<Executor>,
    project_id: &str,
    state: &State,
    merge_sha: &str,
) -> Step {
    let pool = &executor.pool;
    let waiting = || Step::Waiting {
        sha: merge_sha.to_owned(),
    };
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM vcs_requests \
         WHERE project_id = ? AND op = 'revert' AND json_extract(args, '$.merge_sha') = ? \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(merge_sha)
    .fetch_optional(pool)
    .await;
    match existing {
        Ok(Some(request)) => {
            return match verify_postgate::set_revert_request(pool, project_id, merge_sha, request)
                .await
            {
                Ok(true) => Step::Reverting {
                    merge_sha: merge_sha.to_owned(),
                    request,
                },
                Ok(false) => waiting(),
                Err(error) => {
                    Step::Failed(format!("cannot store revert request {request}: {error}"))
                }
            };
        }
        Ok(None) => {}
        Err(error) => return Step::Failed(format!("cannot look for a queued revert: {error}")),
    }
    if crate::autopilot::kill_switch_engaged(pool)
        .await
        .unwrap_or(true)
    {
        return waiting();
    }
    let still_on =
        match crate::config::load_schedule_rules(executor.machine_root.as_deref(), project_id) {
            Ok(rules) => rules.revert_on_red,
            Err(error) => {
                // Unreadable is not "off": hold the revert and say so, so it goes ahead once the
                // rules can be read again and the switch is still on.
                let rules_path = crate::project_state::display_path(
                    project_id,
                    crate::project_state::AUTOPILOT_FILE,
                );
                let summary = format!(
                    "post-merge gate on {target}: the revert of {} is held because {rules_path} \
                     could not be read ({error}). Nothing was reverted yet; it goes ahead once \
                     the rules can be read and revert_on_red is still on.",
                    short(merge_sha),
                    target = state.target
                );
                if let Err(error) = crate::feed::append(
                    pool,
                    Some(project_id),
                    crate::verify_postgate::POSTGATE_RED_KIND,
                    &summary,
                    None,
                    None,
                )
                .await
                {
                    tracing::warn!(%error, "postgate: cannot tell the owner the revert is held");
                }
                return waiting();
            }
        };
    if !still_on {
        let summary = format!(
            "post-merge gate on {target}: the revert of {} was NOT made because revert_on_red \
             was turned off before it was queued. Nothing was reverted and no correction branch \
             was made.",
            short(merge_sha),
            target = state.target
        );
        let reason = "revert_on_red was turned off before the revert was queued".to_owned();
        return settle_failed_revert(pool, project_id, merge_sha, &summary, reason).await;
    }
    let (Ok(sha), Ok(target)) = (CommitSha::new(merge_sha), Branch::new(&state.target)) else {
        let reason = "the culprit or the target is not something the queue accepts".to_owned();
        return revert_failed(pool, project_id, state, merge_sha, reason).await;
    };
    let repo = match vcs::resolve_repo(pool, project_id).await {
        Ok(repo) => repo,
        Err(error) => return Step::Failed(format!("cannot resolve the repository: {error:?}")),
    };
    let op = Op::Revert {
        merge_sha: sha,
        target,
    };
    let request = match vcs::submit_declared(pool, &repo, &op, Origin::Daemon).await {
        Ok(id) => id,
        Err(error) => return Step::Failed(format!("cannot queue the revert: {error}")),
    };
    match verify_postgate::set_revert_request(pool, project_id, merge_sha, request).await {
        Ok(true) => Step::Reverting {
            merge_sha: merge_sha.to_owned(),
            request,
        },
        // Settled by someone else between submit and now; the next tick reads it.
        Ok(false) => Step::Waiting {
            sha: merge_sha.to_owned(),
        },
        Err(error) => Step::Failed(format!("cannot store revert request {request}: {error}")),
    }
}

/// The revert of `merge_sha` did not happen: tell the owner why and free the state.
async fn revert_failed(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    state: &State,
    merge_sha: &str,
    reason: String,
) -> Step {
    let summary = format!(
        "post-merge gate on {target}: the revert of {}{} failed ({reason}). No correction \
         branch was made; check {target} before acting.",
        short(merge_sha),
        ticket_note(state),
        target = state.target
    );
    settle_failed_revert(pool, project_id, merge_sha, &summary, reason).await
}

/// ` (vcs request #N)` when the state holds the revert's ticket, nothing otherwise.
fn ticket_note(state: &State) -> String {
    state
        .revert_request_id
        .map(|id| format!(" (vcs request #{id})"))
        .unwrap_or_default()
}

/// Clears the revert state of `merge_sha` and writes `summary` to the feed.
async fn settle_failed_revert(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    merge_sha: &str,
    summary: &str,
    reason: String,
) -> Step {
    match verify_postgate::fail_revert(pool, project_id, merge_sha, summary).await {
        Ok(true) => Step::RevertFailed {
            merge_sha: merge_sha.to_owned(),
            reason,
        },
        Ok(false) => Step::Waiting {
            sha: merge_sha.to_owned(),
        },
        Err(error) => Step::Failed(format!("cannot record the failed revert: {error}")),
    }
}

/// The name of the correction branch: `fix/<source>-<sha7>`, `source` being the branch the culprit
/// was merged from (`merge` when the queue has no record of it) with its `/` turned into `-`, so
/// that no ref ever has to live under another (`fix/feat` against `fix/feat/x`).
fn fix_branch_name(source: Option<&str>, merge_sha: &str) -> String {
    let source = source.unwrap_or("merge").replace('/', "-");
    format!("fix/{source}-{}", merge_sha.get(..7).unwrap_or(merge_sha))
}

/// The revert is published: build `fix/...` on top of it and tell the owner, or free the state and
/// say that the revert stands without a branch.
async fn fix_branch_step(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    state: &State,
    revert_sha: &str,
) -> Step {
    let pool = &executor.pool;
    let Some(merge_sha) = state.revert_merge_sha.as_deref() else {
        return Step::Failed("a revert is pending without its culprit".to_owned());
    };
    let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;
    let source = sqlx::query_scalar::<_, Option<String>>(
        "SELECT json_extract(args, '$.source') FROM vcs_requests \
         WHERE project_id = ? AND op = 'merge' AND status = 'succeeded' AND result_sha = ? \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(merge_sha)
    .fetch_optional(pool)
    .await
    .unwrap_or_default()
    .flatten();
    let branch = fix_branch_name(source.as_deref(), merge_sha);
    let target = &state.target;
    match git_exec::prepare_fix_branch(project_root, revert_sha, &branch, deadline).await {
        Ok(_) => {
            let ticket = ticket_note(state);
            let summary = format!(
                "post-merge gate on {target}: reverted culprit {} as {}{ticket}; correction \
                 branch {branch} is ready and a correction run will open on it.",
                short(merge_sha),
                short(revert_sha)
            );
            match verify_postgate::set_fix_branch(pool, project_id, merge_sha, &branch, &summary)
                .await
            {
                Ok(true) => Step::FixBranch { branch },
                Ok(false) => Step::Waiting {
                    sha: merge_sha.to_owned(),
                },
                Err(error) => Step::Failed(format!("cannot record the fix branch: {error}")),
            }
        }
        Err(reason) => {
            let summary = format!(
                "post-merge gate on {target}: the revert of {}{} is on {target} as {}, but the \
                 correction branch {branch} was not created ({reason}).",
                short(merge_sha),
                ticket_note(state),
                short(revert_sha)
            );
            settle_failed_revert(pool, project_id, merge_sha, &summary, reason).await
        }
    }
}

/// A prepared `fix/` branch still waiting for its correction run.
#[derive(sqlx::FromRow)]
struct PendingCorrection {
    project_id: String,
    project_root: String,
    target: String,
    revert_merge_sha: Option<String>,
    revert_sha: Option<String>,
    fix_branch: String,
    red_groups: String,
}

/// The task of a correction run: what was reverted, where the work stands, and how it ends.
fn correction_prompt(row: &PendingCorrection) -> String {
    let groups: Vec<String> = serde_json::from_str(&row.red_groups).unwrap_or_default();
    let red = if groups.is_empty() {
        "unknown".to_owned()
    } else {
        groups.join(", ")
    };
    format!(
        "The post-merge gate on `{target}` went red and bisection pointed at merge {culprit}. The \
         daemon reverted it on `{target}` as {revert}, and your worktree starts on branch `{fix}`, \
         whose first commit reverts that revert, so the culprit's change is back in front of \
         you.\n\n\
         Failing gate groups: {red}.\n\n\
         Fix the change so the gate passes, then run `verify scope` and land it normally.",
        target = row.target,
        culprit = row.revert_merge_sha.as_deref().unwrap_or("(unknown)"),
        revert = row.revert_sha.as_deref().unwrap_or("(unknown)"),
        fix = row.fix_branch,
    )
}

/// Sets the correction run of `row`'s branch: its id once opened, `None` to give the claim back.
async fn set_fix_run(
    pool: &sqlx::SqlitePool,
    row: &PendingCorrection,
    value: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE postgate_state SET fix_run_id = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND fix_branch = ?",
    )
    .bind(value)
    .bind(&row.project_id)
    .bind(&row.fix_branch)
    .execute(pool)
    .await
    .map(|_| ())
}

/// How many times a `fix/` branch's correction run is asked for after definitive failures.
const CORRECTION_ATTEMPTS: u32 = 3;

/// The wait between two attempts on the same branch.
const CORRECTION_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// What the process remembers of one branch's failed attempts.
struct Attempts {
    count: u32,
    last: Instant,
}

/// Failed attempts per `(project, fix branch)`. In memory on purpose: no migration, and a daemon
/// restart is a human act that may start over.
static CORRECTION_TRIES: LazyLock<Mutex<HashMap<(String, String), Attempts>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn correction_tries() -> MutexGuard<'static, HashMap<(String, String), Attempts>> {
    CORRECTION_TRIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn correction_key(row: &PendingCorrection) -> (String, String) {
    (row.project_id.clone(), row.fix_branch.clone())
}

/// Whether a branch may be claimed now: it never failed, or its backoff is over.
fn may_try(entry: Option<&Attempts>, now: Instant) -> bool {
    entry.is_none_or(|attempts| now.duration_since(attempts.last) >= CORRECTION_BACKOFF)
}

/// Whether the claim is given back after the `count`-th definitive failure (false: give up).
fn after_failure(count: u32) -> bool {
    count < CORRECTION_ATTEMPTS
}

/// Notes one more definitive failure of `row`'s branch and returns how many there have been.
fn record_failure(row: &PendingCorrection) -> u32 {
    let mut tries = correction_tries();
    let attempts = tries.entry(correction_key(row)).or_insert(Attempts {
        count: 0,
        last: Instant::now(),
    });
    attempts.count += 1;
    attempts.last = Instant::now();
    attempts.count
}

/// Opens one correction run for every prepared `fix/` branch that has none, and returns their ids.
///
/// A branch is claimed (`fix_run_id = 0`) before its run is asked for, so a run is never attempted
/// twice at once. A full project or disk gives the claim back and the next pass asks again; any
/// other failure gives it back too, up to `CORRECTION_ATTEMPTS` times and `CORRECTION_BACKOFF`
/// apart (counted in this process, so a restart starts over), and the last one leaves the claim at 0
/// and says in the feed that it gave up. The kill switch and the budget stop
/// it like any other run the daemon starts on its own; a switch that cannot be read stops it too.
pub async fn open_corrections(state: &crate::state::AppState) -> Vec<i64> {
    let pool = &state.pool;
    if crate::autopilot::kill_switch_engaged(pool)
        .await
        .unwrap_or(true)
    {
        return Vec::new();
    }
    let rows = match sqlx::query_as::<_, PendingCorrection>(
        "SELECT p.project_id, a.project_root, p.target, p.revert_merge_sha, p.revert_sha, \
                p.fix_branch, p.red_groups \
         FROM postgate_state p JOIN autopilot_state a ON a.project_id = p.project_id \
         WHERE p.fix_branch IS NOT NULL AND p.fix_run_id IS NULL \
         ORDER BY p.project_id",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "postgate: cannot look for correction branches");
            return Vec::new();
        }
    };
    // After the query, so an exhausted budget is only logged when a branch is actually waiting.
    if rows.is_empty() {
        return Vec::new();
    }
    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::quota::permits_new_run(state, chrono::Utc::now()).await
    {
        tracing::info!(%reason, "budget exhausted; no correction run opened this tick");
        return Vec::new();
    }
    let mut opened = Vec::new();
    for row in rows {
        // A branch that failed definitively waits out its backoff before it is claimed again.
        if !may_try(
            correction_tries().get(&correction_key(&row)),
            Instant::now(),
        ) {
            continue;
        }
        let claimed = sqlx::query(
            "UPDATE postgate_state SET fix_run_id = 0, updated_at = CURRENT_TIMESTAMP \
             WHERE project_id = ? AND fix_branch = ? AND fix_run_id IS NULL",
        )
        .bind(&row.project_id)
        .bind(&row.fix_branch)
        .execute(pool)
        .await;
        match claimed {
            Ok(done) if done.rows_affected() == 1 => {}
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(
                    project = %row.project_id, %error,
                    "postgate: cannot claim a fix branch"
                );
                continue;
            }
        }
        let started = crate::runs::create_correction_run(
            state,
            correction_prompt(&row),
            row.project_id.clone(),
            row.project_root.clone(),
            crate::runs::Correction {
                base: row.fix_branch.clone(),
            },
        )
        .await;
        match started {
            Ok(run_id) => {
                correction_tries().remove(&correction_key(&row));
                if let Err(error) = set_fix_run(pool, &row, Some(run_id)).await {
                    tracing::warn!(
                        project = %row.project_id, run_id, %error,
                        "postgate: cannot record the correction run"
                    );
                }
                let _ = crate::feed::append(
                    pool,
                    Some(&row.project_id),
                    crate::verify_postgate::POSTGATE_RED_KIND,
                    &format!("correction run {run_id} opened on {}", row.fix_branch),
                    Some(run_id),
                    None,
                )
                .await;
                opened.push(run_id);
            }
            // Nothing was spent: give the claim back and ask again on a later pass.
            Err(
                crate::runs::CreateRunError::Busy | crate::runs::CreateRunError::NoRoomOnDisk(_),
            ) => {
                if let Err(error) = set_fix_run(pool, &row, None).await {
                    tracing::warn!(
                        project = %row.project_id, %error,
                        "postgate: cannot release a correction claim"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    project = %row.project_id, ?error,
                    "postgate: the correction run could not be opened"
                );
                let count = record_failure(&row);
                let retry = after_failure(count);
                if retry && let Err(error) = set_fix_run(pool, &row, None).await {
                    tracing::warn!(
                        project = %row.project_id, %error,
                        "postgate: cannot release a correction claim"
                    );
                }
                let outcome = if retry {
                    "trying again in 10 minutes"
                } else {
                    "gave up"
                };
                let _ = crate::feed::append(
                    pool,
                    Some(&row.project_id),
                    crate::verify_postgate::POSTGATE_RED_KIND,
                    &format!(
                        "the correction run could not be opened: {error:?}; attempt {count} of \
                         {CORRECTION_ATTEMPTS}, {outcome}; the work is on {}",
                        row.fix_branch
                    ),
                    None,
                    None,
                )
                .await;
            }
        }
    }
    opened
}

/// The loop `main` spawns beside the gate worker, because opening a run needs an `AppState` and the
/// worker only holds an executor. With no `fix/` branch waiting, which is every project that has
/// not turned `revert_on_red` on, a pass reads one empty query and stops.
pub async fn run_correction_loop(state: crate::state::AppState) {
    let mut interval = tokio::time::interval(POSTGATE_POLL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        for run_id in open_corrections(&state).await {
            tracing::info!(run_id, "postgate: correction run opened");
        }
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
    use crate::config::VerifyConfig;
    use crate::git_exec::tests::{WorktreeRootEnv, space_free_tempdir};
    use crate::verify_exec::{self, Executor};
    use crate::verify_postgate;
    use crate::verify_runs::PRIORITY_POSTGATE;
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
        let lock = crate::worktree::test_env_lock();
        let repo = space_free_tempdir("nucleos-postgate-");
        let root = repo.path().to_path_buf();
        git_in(&root, &["init", "-q", "-b", "main"]);
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
            crate::project_state::write_for_test(
                machine.path(),
                "alpha",
                crate::project_state::AUTOPILOT_FILE,
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

    #[tokio::test]
    async fn the_same_tip_is_not_gated_twice() {
        let f = fixture(Some(GATE_ON)).await;
        tokio::spawn(verify_exec::run_executor(f.ex.clone()));
        tick_until(&f, |s| matches!(s, Step::Green(_))).await;

        let again = step_of(&f).await;

        assert_eq!(
            again,
            Some(Step::Idle(crate::verify_batch::Idle::UpToDate)),
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
            Some(Step::Idle(crate::verify_batch::Idle::UpToDate))
        );
        assert_eq!(count(&f.pool, "verify_requests").await, 0);
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
                verdict: crate::verify_bisect::Verdict::Culprit {
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
                verdict: crate::verify_bisect::Verdict::Inconclusive {
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
                verdict: crate::verify_bisect::Verdict::Culprit {
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
                    verdict: crate::verify_bisect::Verdict::Culprit { sha, .. },
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

        crate::autopilot::set_kill_switch(&f.pool, true)
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

        crate::autopilot::set_kill_switch(&f.pool, false)
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

        crate::project_state::write_for_test(
            f._machine.path(),
            "alpha",
            crate::project_state::AUTOPILOT_FILE,
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

        crate::project_state::write_for_test(
            f._machine.path(),
            "alpha",
            crate::project_state::AUTOPILOT_FILE,
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

        crate::project_state::write_for_test(
            f._machine.path(),
            "alpha",
            crate::project_state::AUTOPILOT_FILE,
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
        let lock = crate::worktree::test_env_lock();
        let container = space_free_tempdir("nucleos-correction-");
        let root = container.path().join("repo");
        crate::git_exec::testkit::initialize_repo(&root);
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

        let (state, _runner) = crate::runs::testkit::test_state_with_runner(
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
        crate::concurrency::claim(pool, "alpha", crate::worktree::Owner::Run(pinned))
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
        crate::autopilot::set_kill_switch(pool, true).await.unwrap();
        assert!(open_corrections(&f.state).await.is_empty());
        let s = verify_postgate::load(pool, "alpha").await.unwrap().unwrap();
        assert_eq!(
            s.fix_run_id, None,
            "nothing was claimed under the kill switch"
        );
        assert_eq!(count(pool, "runs").await, 1);

        // Released: the same branch gets its run on the next pass.
        crate::autopilot::set_kill_switch(pool, false)
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
}
