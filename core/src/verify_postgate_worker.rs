//! The post-merge gate worker (spec 2026-10-05 §6.1, F3-2).
//!
//! `tick` is one stateless pass over the project roster. Everything it needs lives in
//! `postgate_state` and `verify_requests`, so resuming after a restart is simply the next tick.
//! A project is touched only when its rules set `gate_after_land`; for every other project the
//! tick reads the rules file and stops, which is the default for all of them.
//!
//! The gate it runs is the full one (`verify` scope `full`, the project's `gate_command`), as
//! `Caller::Postgate` at priority 2, in the `postgate-<project>` worktree checked out at the land
//! target's tip. The cache is never consulted. Bisection, flake re-runs and alerts are later work.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::git_exec;
use crate::land;
use crate::verify::{self, Caller, VerifyArgs};
use crate::verify_batch::{self, Commit, Decision, TargetState};
use crate::verify_exec::Executor;
use crate::verify_plan::{Kind, ScopeArg};
use crate::verify_postgate;

/// How often the loop looks at the roster.
pub const POSTGATE_POLL: Duration = Duration::from_secs(30);

/// Names a failing unit that has no group: the project's `gate_command`.
const GATE_UNIT: &str = "gate_command";

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
    let tree = match git_exec::prepare_postgate_worktree(project_root, sha, deadline).await {
        Ok(tree) => tree,
        Err(reason) => return give_up(pool, project_id, sha, reason).await,
    };
    let args = VerifyArgs {
        kind: Kind::Test,
        scope: ScopeArg::Full,
        worktree: Some(tree.to_string_lossy().into_owned()),
        files: None,
        base: None,
        wait: false,
    };
    let request = match verify::submit(executor, Caller::Postgate, &args).await {
        Ok(id) => id,
        Err(error) => {
            return give_up(pool, project_id, sha, error.message().to_owned()).await;
        }
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
}
