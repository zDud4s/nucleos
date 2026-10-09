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
//! A red (F3-3, spec §6.2) is handled in two persisted phases, one action per tick, and nothing is
//! reverted. First the same sha is checked again (`Caller::FlakeCheck`): if it passes the red was
//! not the target's, the sha counts green and nobody is told. Otherwise the first-parent commits of
//! `(last green, red]` are bisected with `verify_bisect`, one full probe per step
//! (`Caller::Bisect`), and the outcome goes to the owner as one `postgate_red` feed line. A probe
//! left running is followed from its stored ticket, never submitted again, and while a red is being
//! handled no new gate starts.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::git_exec;
use crate::land;
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
        return report_red(pool, project_id, state, red_sha, inconclusive()).await;
    };
    let commits = match git_exec::first_parent_commits(project_root, base, red_sha, deadline).await
    {
        Ok(commits) => commits,
        Err(reason) => {
            tracing::warn!(project = project_id, %reason, "postgate: cannot list the range");
            return report_red(pool, project_id, state, red_sha, inconclusive()).await;
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
        Next::Done(verdict) => report_red(pool, project_id, state, red_sha, verdict).await,
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

/// Stores `verdict` and tells the owner, in one transaction (`verify_postgate::report`).
async fn report_red(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    state: &State,
    red_sha: &str,
    verdict: Verdict,
) -> Step {
    let summary = red_summary(
        &state.target,
        red_sha,
        state.red_base_sha.as_deref(),
        &verdict,
    );
    match verify_postgate::report(pool, project_id, red_sha, &verdict, &summary).await {
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
}
