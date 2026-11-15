//! The CI watch: state of the CI of every sha an automation pushed, and the pure decision of what
//! one poll of `gh` concludes. The state lives in the `ci_watch` table so a restart resumes where the
//! watch was; [`decide`] touches neither the database nor `gh`.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

use super::{Action, CiConclusion, Event, Trigger, firings};
use crate::integrations::github::{
    CiError, CiGh, CommitSha, ProjectRepo, RealCiGh, RunState, RunSummary, WorkflowName,
};
use crate::state::AppState;

/// A pushed sha with no run on the CI this long after the push: one feed line, never an incident.
pub const NO_RUN_AFTER: Duration = Duration::minutes(15);
/// A run created this long ago and still not completed is stuck.
pub const STUCK_AFTER: Duration = Duration::minutes(120);
/// How often the watch polls.
pub const INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    Watching,
    RerunWaiting,
    Done,
    Superseded,
    GaveUp,
}

impl WatchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::RerunWaiting => "rerun_waiting",
            Self::Done => "done",
            Self::Superseded => "superseded",
            Self::GaveUp => "gave_up",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "rerun_waiting" => Self::RerunWaiting,
            "done" => Self::Done,
            "superseded" => Self::Superseded,
            "gave_up" => Self::GaveUp,
            _ => Self::Watching,
        }
    }
}

/// One watched sha. `wants_rerun` and `newest` are not stored: the caller derives them from the
/// rules and from the branch's latest push on every poll ([`load`] returns the neutral
/// `wants_rerun: false, newest: true`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    pub project_id: String,
    pub branch: String,
    pub sha: String,
    pub workflow: String,
    pub pushed_at: DateTime<Utc>,
    pub state: WatchState,
    pub run_id: Option<u64>,
    pub run_created_at: Option<DateTime<Utc>>,
    pub rerun_from_attempt: Option<u32>,
    pub wants_rerun: bool,
    pub newest: bool,
}

/// What one poll of one watched sha concludes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// Not the newest push of its branch, or a cancelled run with a newer push.
    Superseded,
    /// 15 minutes since the push and `gh` answered with no run: one feed line, no incident.
    NoRun,
    /// 120 minutes since the run was created and still not completed.
    Stuck,
    /// First failure, `wants_rerun`, not rerun yet.
    RerunFailed { run: u64 },
    Finished { run: u64, conclusion: CiConclusion },
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// The most recent run of `sha` (a manual re-dispatch leaves several).
fn newest_run<'a>(sha: &str, runs: &'a [RunSummary]) -> Option<&'a RunSummary> {
    runs.iter()
        .filter(|run| run.head_sha == sha)
        .max_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)))
}

/// The pure decision. Order: an older push is superseded; no run waits for 15 minutes; an
/// unfinished run waits for 120; a completed run that still shows the pre-rerun result waits (the
/// race right after `gh run rerun`); then the conclusion decides.
pub fn decide(w: &Watched, runs: &[RunSummary], now: DateTime<Utc>) -> Step {
    if !w.newest {
        return Step::Superseded;
    }
    let Some(run) = newest_run(&w.sha, runs) else {
        return if now - w.pushed_at >= NO_RUN_AFTER {
            Step::NoRun
        } else {
            Step::Wait
        };
    };
    if run.state != RunState::Completed {
        let created = parse_time(&run.created_at)
            .or(w.run_created_at)
            .unwrap_or(w.pushed_at);
        return if now - created >= STUCK_AFTER {
            Step::Stuck
        } else {
            Step::Wait
        };
    }
    let rerun_requested = w.state == WatchState::RerunWaiting || w.rerun_from_attempt.is_some();
    if w.state == WatchState::RerunWaiting
        && w.rerun_from_attempt
            .is_some_and(|from| run.attempt <= from)
    {
        return Step::Wait;
    }
    match run.conclusion.as_deref().unwrap_or("") {
        "success" | "neutral" | "skipped" => Step::Finished {
            run: run.id,
            conclusion: CiConclusion::Success,
        },
        "cancelled" => Step::Finished {
            run: run.id,
            conclusion: CiConclusion::Cancelled,
        },
        _ if w.wants_rerun && !rerun_requested => Step::RerunFailed { run: run.id },
        _ => Step::Finished {
            run: run.id,
            conclusion: CiConclusion::Failure,
        },
    }
}

// ---- state -----------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct Row {
    project_id: String,
    branch: String,
    sha: String,
    workflow: String,
    pushed_at: String,
    state: String,
    run_id: Option<i64>,
    run_created_at: Option<String>,
    rerun_from_attempt: Option<i64>,
}

impl Row {
    fn into_watched(self) -> Option<Watched> {
        Some(Watched {
            project_id: self.project_id,
            branch: self.branch,
            sha: self.sha,
            workflow: self.workflow,
            pushed_at: parse_time(&self.pushed_at)?,
            state: WatchState::parse(&self.state),
            run_id: self.run_id.and_then(|id| u64::try_from(id).ok()),
            run_created_at: self.run_created_at.as_deref().and_then(parse_time),
            rerun_from_attempt: self.rerun_from_attempt.and_then(|a| u32::try_from(a).ok()),
            wants_rerun: false,
            newest: true,
        })
    }
}

/// The selected columns, as a macro so each query stays one `concat!`-ed literal (sqlx refuses a
/// `format!`-ed SQL string).
macro_rules! columns {
    () => {
        "project_id, branch, sha, workflow, pushed_at, state, run_id, run_created_at, \
         rerun_from_attempt"
    };
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Starts watching a sha. A row that exists is left as it is, whatever its state.
pub async fn ensure_row(
    pool: &SqlitePool,
    project_id: &str,
    branch: &str,
    sha: &str,
    workflow: &str,
    pushed_at: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO ci_watch (project_id, branch, sha, workflow, pushed_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (project_id, sha, workflow) DO NOTHING",
    )
    .bind(project_id)
    .bind(branch)
    .bind(sha)
    .bind(workflow)
    .bind(stamp(pushed_at))
    .execute(pool)
    .await?;
    Ok(())
}

/// One row, whatever its state.
pub async fn load(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
    workflow: &str,
) -> sqlx::Result<Option<Watched>> {
    let row = sqlx::query_as::<_, Row>(concat!(
        "SELECT ",
        columns!(),
        " FROM ci_watch WHERE project_id = ? AND sha = ? AND workflow = ?"
    ))
    .bind(project_id)
    .bind(sha)
    .bind(workflow)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(Row::into_watched))
}

/// The rows still being watched (`watching` or `rerun_waiting`), oldest push first. A `done`,
/// `superseded` or `gave_up` row is never returned.
pub async fn active(pool: &SqlitePool) -> sqlx::Result<Vec<Watched>> {
    let rows = sqlx::query_as::<_, Row>(concat!(
        "SELECT ",
        columns!(),
        " FROM ci_watch WHERE state IN ('watching', 'rerun_waiting') ORDER BY pushed_at, sha"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().filter_map(Row::into_watched).collect())
}

/// Writes back the mutable part of `w` (state, run, rerun marker) and the outcome, if any.
pub async fn save(pool: &SqlitePool, w: &Watched, outcome: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE ci_watch SET state = ?, run_id = ?, run_created_at = ?, rerun_from_attempt = ?,
             outcome = COALESCE(?, outcome)
         WHERE project_id = ? AND sha = ? AND workflow = ?",
    )
    .bind(w.state.as_str())
    .bind(w.run_id.and_then(|id| i64::try_from(id).ok()))
    .bind(w.run_created_at.map(stamp))
    .bind(w.rerun_from_attempt.map(i64::from))
    .bind(outcome)
    .bind(&w.project_id)
    .bind(&w.sha)
    .bind(&w.workflow)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records that the feed line for `reason` was written for this sha. `true` when it is the first
/// one: the caller writes the line only then.
pub async fn warn_once(
    pool: &SqlitePool,
    w: &Watched,
    reason: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE ci_watch SET warned_at = ?, outcome = ?
         WHERE project_id = ? AND sha = ? AND workflow = ?
           AND (warned_at IS NULL OR outcome IS NOT ?)",
    )
    .bind(stamp(now))
    .bind(reason)
    .bind(&w.project_id)
    .bind(&w.sha)
    .bind(&w.workflow)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Every still-watched sha of `branch` other than `newest_sha` is superseded.
pub async fn mark_superseded(
    pool: &SqlitePool,
    project_id: &str,
    branch: &str,
    newest_sha: &str,
) -> sqlx::Result<u64> {
    let done = sqlx::query(
        "UPDATE ci_watch SET state = 'superseded', outcome = 'superseded'
         WHERE project_id = ? AND branch = ? AND sha <> ?
           AND state IN ('watching', 'rerun_waiting')",
    )
    .bind(project_id)
    .bind(branch)
    .bind(newest_sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

// ---- the loop ----------------------------------------------------------------------------------

/// What one tick did, for the tests and the log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    /// `Event::CiFinished` dispatched.
    pub finished: u32,
    /// `gh run rerun --failed` asked for.
    pub reruns: u32,
    /// Feed lines written.
    pub warned: u32,
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// One pass over every project with a `ci_finished` rule on. Per project: the pushes of automations
/// become watched rows (an older sha of a branch is superseded), then each live row is polled
/// through `gh` and acted on. A project that fails is logged and the rest still run.
pub async fn tick(
    pool: &SqlitePool,
    gh: &dyn CiGh,
    now: DateTime<Utc>,
) -> sqlx::Result<TickReport> {
    let mut report = TickReport::default();
    let projects: Vec<String> =
        sqlx::query_scalar("SELECT project_id FROM autopilot_state ORDER BY project_id")
            .fetch_all(pool)
            .await?;
    for project_id in &projects {
        if let Err(error) = tick_project(pool, gh, project_id, now, &mut report).await {
            tracing::warn!(project_id, %error, "the CI watch of a project failed");
        }
    }
    Ok(report)
}

/// Writes the feed line for `reason` once per sha.
async fn warn(
    pool: &SqlitePool,
    w: &Watched,
    reason: &str,
    text: String,
    now: DateTime<Utc>,
    report: &mut TickReport,
) -> sqlx::Result<()> {
    if warn_once(pool, w, reason, now).await? {
        firings::note(pool, &w.project_id, &text).await;
        report.warned += 1;
    }
    Ok(())
}

async fn unavailable(
    pool: &SqlitePool,
    w: &Watched,
    reason: &str,
    now: DateTime<Utc>,
    report: &mut TickReport,
) -> sqlx::Result<()> {
    let text = format!(
        "CI watch of {} on {}: `gh` is unavailable ({reason}); the rule stays on and the watch resumes when it is back",
        short(&w.sha),
        w.branch
    );
    warn(pool, w, "ci_unavailable", text, now, report).await
}

async fn tick_project(
    pool: &SqlitePool,
    gh: &dyn CiGh,
    project_id: &str,
    now: DateTime<Utc>,
    report: &mut TickReport,
) -> sqlx::Result<()> {
    let rules = match super::enabled_rules(project_id) {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(project_id, %error, "the automations could not be read; the CI watch skips the project");
            return Ok(());
        }
    };
    // workflow -> whether a red of it is worth one rerun (only an `open_incident` rule wants that).
    let mut workflows: BTreeMap<String, bool> = BTreeMap::new();
    for rule in &rules {
        if let Trigger::CiFinished { workflow } = &rule.on {
            let wants = matches!(rule.action, Action::OpenIncident { .. });
            *workflows.entry(workflow.clone()).or_insert(false) |= wants;
        }
    }
    if workflows.is_empty() {
        return Ok(());
    }

    // The newest push of each branch is the one watched; an owner's own push has no firing, so it
    // never gets a row.
    let mut newest: BTreeMap<String, String> = BTreeMap::new();
    for branch in firings::push_branches(pool, project_id).await? {
        let Some(push) = firings::last_push(pool, project_id, &branch).await? else {
            continue;
        };
        let pushed_at = parse_time(&push.at).unwrap_or(now);
        for workflow in workflows.keys() {
            ensure_row(pool, project_id, &branch, &push.sha, workflow, pushed_at).await?;
        }
        mark_superseded(pool, project_id, &branch, &push.sha).await?;
        newest.insert(branch, push.sha);
    }
    let rows: Vec<Watched> = active(pool)
        .await?
        .into_iter()
        .filter(|w| w.project_id == project_id && workflows.contains_key(&w.workflow))
        .collect();
    if rows.is_empty() {
        return Ok(());
    }

    let deadline = std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let repo = match crate::integrations::github::project_repo(pool, project_id, deadline).await? {
        Some(ProjectRepo::Known { repo, .. }) => repo,
        _ => {
            for w in &rows {
                let text = format!(
                    "CI watch of {} on {}: the project's origin is not a GitHub repository",
                    short(&w.sha),
                    w.branch
                );
                warn(pool, w, "no_github_repo", text, now, report).await?;
            }
            return Ok(());
        }
    };

    for mut w in rows {
        w.wants_rerun = workflows.get(&w.workflow).copied().unwrap_or(false);
        w.newest = newest.get(&w.branch).is_none_or(|sha| *sha == w.sha);
        if !w.newest {
            w.state = WatchState::Superseded;
            save(pool, &w, Some("superseded")).await?;
            continue;
        }
        let (Ok(sha), Ok(workflow)) = (CommitSha::new(&w.sha), WorkflowName::new(&w.workflow))
        else {
            tracing::warn!(project_id, sha = %w.sha, workflow = %w.workflow, "not something `gh` accepts; skipped");
            continue;
        };
        let runs = match gh.runs_for_commit(&repo, &sha, &workflow).await {
            Ok(runs) => runs,
            Err(CiError::Unavailable(reason)) => {
                unavailable(pool, &w, &reason, now, report).await?;
                continue;
            }
            Err(CiError::Failed(reason)) => {
                tracing::warn!(project_id, sha = %w.sha, %reason, "could not list the CI runs; trying again next tick");
                continue;
            }
        };
        match decide(&w, &runs, now) {
            Step::Wait => {
                if let Some(run) = newest_run(&w.sha, &runs) {
                    let created = parse_time(&run.created_at);
                    if w.run_id != Some(run.id) || w.run_created_at != created {
                        w.run_id = Some(run.id);
                        w.run_created_at = created.or(w.run_created_at);
                        save(pool, &w, None).await?;
                    }
                }
            }
            Step::Superseded => {
                w.state = WatchState::Superseded;
                save(pool, &w, Some("superseded")).await?;
            }
            Step::NoRun => {
                let text = format!(
                    "CI watch of {} on {}: no `{}` run appeared {} minutes after the push; no incident opened",
                    short(&w.sha),
                    w.branch,
                    w.workflow,
                    NO_RUN_AFTER.num_minutes()
                );
                warn(pool, &w, "no_run", text, now, report).await?;
                w.state = WatchState::GaveUp;
                save(pool, &w, Some("no_run")).await?;
            }
            Step::Stuck => {
                let text = format!(
                    "CI watch of {} on {}: the `{}` run has not finished after {} minutes; no incident opened",
                    short(&w.sha),
                    w.branch,
                    w.workflow,
                    STUCK_AFTER.num_minutes()
                );
                warn(pool, &w, "stuck", text, now, report).await?;
                w.state = WatchState::GaveUp;
                save(pool, &w, Some("stuck")).await?;
            }
            Step::RerunFailed { run } => match gh.rerun_failed(&repo, run).await {
                Ok(()) => {
                    // The run keeps its id across a rerun; only `attempt` moves, and the next polls
                    // wait until it has.
                    w.state = WatchState::RerunWaiting;
                    w.run_id = Some(run);
                    w.rerun_from_attempt = runs
                        .iter()
                        .find(|seen| seen.id == run)
                        .map(|seen| seen.attempt)
                        .or(Some(1));
                    save(pool, &w, None).await?;
                    report.reruns += 1;
                }
                Err(CiError::Unavailable(reason)) => {
                    unavailable(pool, &w, &reason, now, report).await?;
                }
                Err(CiError::Failed(reason)) => {
                    tracing::warn!(project_id, run, %reason, "could not rerun the failed jobs; trying again next tick");
                }
            },
            Step::Finished { run, conclusion } => {
                // Dispatch first, then record `done`: a restart in between re-emits, and the
                // firing log's unique key makes that harmless.
                let event = Event::CiFinished {
                    sha: w.sha.clone(),
                    branch: w.branch.clone(),
                    workflow: w.workflow.clone(),
                    conclusion,
                    run_id: run,
                };
                super::dispatch_with(pool, project_id, event, gh).await;
                w.state = WatchState::Done;
                w.run_id = Some(run);
                save(pool, &w, Some("finished")).await?;
                report.finished += 1;
            }
        }
    }
    Ok(())
}

/// The watch's own loop. Its own and not a step of the resolver's, which holds repository locks:
/// this only talks to `gh` and the database. A failed tick is logged and the loop goes on.
pub async fn run_ci_watch_loop(state: AppState) {
    let gh = RealCiGh {
        runtime: state.github.clone(),
    };
    let mut interval = tokio::time::interval(INTERVAL);
    loop {
        interval.tick().await;
        if let Err(error) = tick(&state.pool, &gh, Utc::now()).await {
            tracing::warn!(%error, "the CI watch tick failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 15, 10, 0, 0).unwrap()
    }

    fn mins(n: i64) -> Duration {
        Duration::minutes(n)
    }

    fn watched() -> Watched {
        Watched {
            project_id: "p".into(),
            branch: "master".into(),
            sha: "abc".into(),
            workflow: "ci.yml".into(),
            pushed_at: t0(),
            state: WatchState::Watching,
            run_id: None,
            run_created_at: None,
            rerun_from_attempt: None,
            wants_rerun: false,
            newest: true,
        }
    }

    fn run(state: RunState, conclusion: Option<&str>, attempt: u32) -> RunSummary {
        RunSummary {
            id: 7,
            state,
            conclusion: conclusion.map(str::to_owned),
            head_sha: "abc".into(),
            url: "https://example.test/run/7".into(),
            attempt,
            created_at: stamp(t0() + mins(1)),
        }
    }

    fn done(conclusion: &str, attempt: u32) -> RunSummary {
        run(RunState::Completed, Some(conclusion), attempt)
    }

    #[test]
    fn no_run_before_15_minutes_waits() {
        assert_eq!(decide(&watched(), &[], t0() + mins(14)), Step::Wait);
    }

    #[test]
    fn no_run_after_15_minutes_is_no_run() {
        assert_eq!(decide(&watched(), &[], t0() + mins(15)), Step::NoRun);
        let other_sha = RunSummary {
            head_sha: "zzz".into(),
            ..done("success", 1)
        };
        assert_eq!(
            decide(&watched(), &[other_sha], t0() + mins(20)),
            Step::NoRun,
            "a run of another sha does not count"
        );
    }

    #[test]
    fn run_in_progress_waits_until_120_minutes_then_stuck() {
        let running = [run(RunState::InProgress, None, 1)];
        assert_eq!(decide(&watched(), &running, t0() + mins(100)), Step::Wait);
        // The run was created one minute after the push.
        assert_eq!(decide(&watched(), &running, t0() + mins(121)), Step::Stuck);
    }

    #[test]
    fn an_older_push_is_superseded_without_asking_gh() {
        let mut w = watched();
        w.newest = false;
        assert_eq!(decide(&w, &[], t0()), Step::Superseded);
    }

    #[test]
    fn a_cancelled_run_with_a_newer_push_is_superseded() {
        let mut w = watched();
        w.newest = false;
        assert_eq!(
            decide(&w, &[done("cancelled", 1)], t0() + mins(5)),
            Step::Superseded
        );
    }

    #[test]
    fn a_cancelled_run_without_a_newer_push_finishes_as_cancelled() {
        assert_eq!(
            decide(&watched(), &[done("cancelled", 1)], t0() + mins(5)),
            Step::Finished {
                run: 7,
                conclusion: CiConclusion::Cancelled
            }
        );
    }

    #[test]
    fn first_failure_with_wants_rerun_asks_for_a_rerun() {
        let mut w = watched();
        w.wants_rerun = true;
        assert_eq!(
            decide(&w, &[done("failure", 1)], t0() + mins(5)),
            Step::RerunFailed { run: 7 }
        );
        assert_eq!(
            decide(&w, &[done("timed_out", 1)], t0() + mins(5)),
            Step::RerunFailed { run: 7 }
        );
    }

    #[test]
    fn a_failure_after_the_rerun_finishes_failure() {
        let mut w = watched();
        w.wants_rerun = true;
        w.state = WatchState::RerunWaiting;
        w.rerun_from_attempt = Some(1);
        assert_eq!(
            decide(&w, &[done("failure", 2)], t0() + mins(9)),
            Step::Finished {
                run: 7,
                conclusion: CiConclusion::Failure
            }
        );
    }

    #[test]
    fn rerun_waiting_ignores_the_stale_completed_run() {
        let mut w = watched();
        w.wants_rerun = true;
        w.state = WatchState::RerunWaiting;
        w.rerun_from_attempt = Some(1);
        assert_eq!(
            decide(&w, &[done("failure", 1)], t0() + mins(9)),
            Step::Wait
        );
    }

    #[test]
    fn rerun_waiting_accepts_a_higher_attempt() {
        let mut w = watched();
        w.wants_rerun = true;
        w.state = WatchState::RerunWaiting;
        w.rerun_from_attempt = Some(1);
        assert_eq!(
            decide(&w, &[done("success", 2)], t0() + mins(9)),
            Step::Finished {
                run: 7,
                conclusion: CiConclusion::Success
            }
        );
    }

    #[test]
    fn a_failure_without_wants_rerun_finishes_failure_at_once() {
        assert_eq!(
            decide(&watched(), &[done("failure", 1)], t0() + mins(5)),
            Step::Finished {
                run: 7,
                conclusion: CiConclusion::Failure
            }
        );
    }

    #[test]
    fn success_finishes_success() {
        let mut w = watched();
        w.wants_rerun = true;
        assert_eq!(
            decide(&w, &[done("success", 1)], t0() + mins(5)),
            Step::Finished {
                run: 7,
                conclusion: CiConclusion::Success
            }
        );
    }

    #[tokio::test]
    async fn watch_rows_survive_a_restart() {
        let pool = crate::testdb::fresh_pool().await;
        ensure_row(&pool, "p", "master", "abc", "ci.yml", t0())
            .await
            .unwrap();
        let mut w = load(&pool, "p", "abc", "ci.yml").await.unwrap().unwrap();
        assert_eq!(w, watched());
        w.state = WatchState::RerunWaiting;
        w.run_id = Some(7);
        w.run_created_at = Some(t0() + mins(1));
        w.rerun_from_attempt = Some(1);
        save(&pool, &w, None).await.unwrap();
        // A second ensure_row (the next tick) leaves the saved state alone.
        ensure_row(&pool, "p", "master", "abc", "ci.yml", t0() + mins(30))
            .await
            .unwrap();
        let reopened = load(&pool, "p", "abc", "ci.yml").await.unwrap().unwrap();
        assert_eq!(reopened, w);
        assert_eq!(active(&pool).await.unwrap(), vec![w]);
    }

    #[tokio::test]
    async fn a_done_row_is_never_selected_again() {
        let pool = crate::testdb::fresh_pool().await;
        ensure_row(&pool, "p", "master", "abc", "ci.yml", t0())
            .await
            .unwrap();
        ensure_row(&pool, "p", "master", "def", "ci.yml", t0() + mins(5))
            .await
            .unwrap();
        let mut w = load(&pool, "p", "abc", "ci.yml").await.unwrap().unwrap();
        w.state = WatchState::Done;
        save(&pool, &w, Some("finished")).await.unwrap();
        let live = active(&pool).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].sha, "def");
        // A superseding push marks only the still-watched older shas.
        ensure_row(&pool, "p", "master", "ghi", "ci.yml", t0() + mins(10))
            .await
            .unwrap();
        assert_eq!(mark_superseded(&pool, "p", "master", "ghi").await.unwrap(), 1);
        let live = active(&pool).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].sha, "ghi");
        assert_eq!(
            load(&pool, "p", "abc", "ci.yml").await.unwrap().unwrap().state,
            WatchState::Done,
            "a done row stays done"
        );
    }

    // ---- tick ----------------------------------------------------------------------------------

    use crate::automations::testkit::{
        FakeCiGh, SHA_1, SHA_2, finished_run, github_project, pushed, run_of,
    };
    use crate::project::rules::testing::publish_automations;

    const WATCH_RULE: &str = "rules:\n  - name: watch\n    enabled: true\n    on: {event: ci_finished, workflow: ci.yml}\n    do: {action: open_incident, auto_fix: true}\n";

    /// A project with the rule on, a GitHub origin and an automation push of `SHA_1` to `master`.
    async fn ready(project: &str) -> (SqlitePool, tempfile::TempDir, FakeCiGh) {
        publish_automations(project, WATCH_RULE);
        let pool = crate::testdb::fresh_pool().await;
        let dir = github_project(&pool, project).await;
        pushed(&pool, project, "master", SHA_1).await;
        (pool, dir, FakeCiGh::new())
    }

    async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    async fn incident_firings(pool: &SqlitePool) -> i64 {
        count(
            pool,
            "SELECT COUNT(*) FROM automation_firings WHERE action = 'open_incident'",
        )
        .await
    }

    async fn feed_lines(pool: &SqlitePool) -> i64 {
        count(pool, "SELECT COUNT(*) FROM feed WHERE kind = 'automation_firing'").await
    }

    async fn tickets(pool: &SqlitePool) -> i64 {
        count(
            pool,
            "SELECT COUNT(*) FROM task_claims WHERE task_key LIKE 'ci-red:%'",
        )
        .await
    }

    async fn state_of(pool: &SqlitePool, project: &str, sha: &str) -> WatchState {
        load(pool, project, sha, "ci.yml")
            .await
            .unwrap()
            .expect("a watch row")
            .state
    }

    fn soon() -> DateTime<Utc> {
        Utc::now() + mins(1)
    }

    fn running(sha: &str) -> Result<Vec<RunSummary>, CiError> {
        Ok(vec![run_of(sha, 7, RunState::InProgress, None, 1)])
    }

    fn ended(sha: &str, conclusion: &str, attempt: u32) -> Result<Vec<RunSummary>, CiError> {
        Ok(vec![finished_run(sha, 7, conclusion, attempt)])
    }

    #[tokio::test]
    async fn a_project_without_a_ci_rule_is_never_polled() {
        let project = "ciw-no-rule";
        publish_automations(project, "rules: []\n");
        let pool = crate::testdb::fresh_pool().await;
        let _dir = github_project(&pool, project).await;
        pushed(&pool, project, "master", SHA_1).await;
        let gh = FakeCiGh::new();
        gh.script(SHA_1, vec![ended(SHA_1, "failure", 1)]);

        let report = tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(report, TickReport::default());
        assert_eq!(gh.polls(), 0);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM ci_watch").await, 0);
    }

    #[tokio::test]
    async fn only_the_newest_automation_push_per_branch_is_watched() {
        let project = "ciw-newest";
        let (pool, _dir, gh) = ready(project).await;
        gh.script(SHA_1, vec![running(SHA_1)]);
        gh.script(SHA_2, vec![running(SHA_2)]);
        tick(&pool, &gh, soon()).await.unwrap();
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Watching);

        pushed(&pool, project, "master", SHA_2).await;
        tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Superseded);
        assert_eq!(state_of(&pool, project, SHA_2).await, WatchState::Watching);
        assert_eq!(
            gh.polled_shas(),
            vec![SHA_1.to_owned(), SHA_2.to_owned()],
            "the older sha is asked about once, before it was superseded"
        );
    }

    #[tokio::test]
    async fn a_manual_push_is_ignored() {
        let project = "ciw-manual";
        publish_automations(project, WATCH_RULE);
        let pool = crate::testdb::fresh_pool().await;
        let _dir = github_project(&pool, project).await;
        let gh = FakeCiGh::new();
        // A run exists for a sha no automation pushed.
        gh.script(SHA_1, vec![ended(SHA_1, "failure", 1)]);

        let report = tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(report, TickReport::default());
        assert_eq!(gh.polls(), 0);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM ci_watch").await, 0);
    }

    #[tokio::test]
    async fn a_green_run_dispatches_ci_finished_once() {
        let project = "ciw-green";
        let (pool, _dir, gh) = ready(project).await;
        gh.script(SHA_1, vec![ended(SHA_1, "success", 1)]);

        let first = tick(&pool, &gh, soon()).await.unwrap();
        let second = tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(first.finished, 1);
        assert_eq!(second.finished, 0);
        assert_eq!(incident_firings(&pool).await, 1);
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Done);
        assert_eq!(gh.polls(), 1, "a done row is not polled again");
    }

    #[tokio::test]
    async fn a_red_run_is_rerun_once_then_dispatched_red() {
        let project = "ciw-red";
        let (pool, _dir, gh) = ready(project).await;
        gh.script(
            SHA_1,
            vec![
                ended(SHA_1, "failure", 1),
                ended(SHA_1, "failure", 1),
                ended(SHA_1, "failure", 2),
            ],
        );

        let first = tick(&pool, &gh, soon()).await.unwrap();
        assert_eq!(first.reruns, 1);
        assert_eq!(gh.reruns(), vec![7]);
        assert_eq!(incident_firings(&pool).await, 0, "no event on the first red");
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::RerunWaiting);

        let second = tick(&pool, &gh, soon()).await.unwrap();
        assert_eq!(second, TickReport::default(), "the stale result is not accepted");

        let third = tick(&pool, &gh, soon()).await.unwrap();
        assert_eq!(third.finished, 1);
        assert_eq!(gh.reruns().len(), 1, "no second rerun");
        assert_eq!(incident_firings(&pool).await, 1);
        assert_eq!(tickets(&pool).await, 1, "the second red is the incident");
    }

    #[tokio::test]
    async fn a_red_then_green_rerun_dispatches_success_and_opens_nothing() {
        let project = "ciw-flaky";
        let (pool, _dir, gh) = ready(project).await;
        gh.script(
            SHA_1,
            vec![ended(SHA_1, "failure", 1), ended(SHA_1, "success", 2)],
        );

        tick(&pool, &gh, soon()).await.unwrap();
        let second = tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(second.finished, 1);
        assert_eq!(gh.reruns().len(), 1);
        assert_eq!(tickets(&pool).await, 0);
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Done);
    }

    #[tokio::test]
    async fn a_cancelled_run_superseded_by_a_newer_push_is_ignored() {
        let project = "ciw-cancelled";
        let (pool, _dir, gh) = ready(project).await;
        gh.script(SHA_1, vec![running(SHA_1), ended(SHA_1, "cancelled", 1)]);
        gh.script(SHA_2, vec![running(SHA_2)]);
        tick(&pool, &gh, soon()).await.unwrap();

        // The newer push cancels the older run on the CI, and the watch has moved on.
        pushed(&pool, project, "master", SHA_2).await;
        let report = tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(report.finished, 0);
        assert_eq!(incident_firings(&pool).await, 0);
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Superseded);
    }

    #[tokio::test]
    async fn no_run_after_15_minutes_posts_one_feed_line_and_no_incident() {
        let project = "ciw-norun";
        let (pool, _dir, _gh) = ready(project).await;
        let gh = FakeCiGh::new();
        let late = Utc::now() + mins(16);

        let first = tick(&pool, &gh, late).await.unwrap();
        let second = tick(&pool, &gh, late).await.unwrap();

        assert_eq!(first.warned, 1);
        assert_eq!(second.warned, 0);
        assert_eq!(feed_lines(&pool).await, 1);
        assert_eq!(tickets(&pool).await, 0);
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::GaveUp);
    }

    #[tokio::test]
    async fn a_stuck_run_posts_one_feed_line_after_120_minutes() {
        let project = "ciw-stuck";
        let (pool, _dir, gh) = ready(project).await;
        gh.script(SHA_1, vec![running(SHA_1)]);

        let early = tick(&pool, &gh, Utc::now() + mins(100)).await.unwrap();
        assert_eq!(early.warned, 0, "an unfinished run waits");
        let late = Utc::now() + mins(121);
        let first = tick(&pool, &gh, late).await.unwrap();
        let second = tick(&pool, &gh, late).await.unwrap();

        assert_eq!(first.warned, 1);
        assert_eq!(second.warned, 0);
        assert_eq!(feed_lines(&pool).await, 1);
        assert_eq!(tickets(&pool).await, 0);
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::GaveUp);
    }

    #[tokio::test]
    async fn gh_unavailable_posts_one_line_marks_ci_unavailable_and_keeps_the_rule_enabled() {
        let project = "ciw-unavailable";
        let (pool, _dir, gh) = ready(project).await;
        let down = || Err(CiError::Unavailable("gh is not installed".to_owned()));
        gh.script(SHA_1, vec![down(), down(), ended(SHA_1, "success", 1)]);

        let first = tick(&pool, &gh, soon()).await.unwrap();
        let second = tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(first.warned, 1);
        assert_eq!(second.warned, 0, "one line per sha");
        assert_eq!(feed_lines(&pool).await, 1);
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Watching);
        let outcome: Option<String> =
            sqlx::query_scalar("SELECT outcome FROM ci_watch WHERE sha = ?")
                .bind(SHA_1)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(outcome.as_deref(), Some("ci_unavailable"));
        assert!(crate::automations::automation_enabled(project, "watch"));

        // `gh` is back: the same sha finishes.
        let third = tick(&pool, &gh, soon()).await.unwrap();
        assert_eq!(third.finished, 1);
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Done);
    }

    #[tokio::test]
    async fn a_restart_between_dispatch_and_done_does_not_double_fire() {
        let project = "ciw-restart";
        let (pool, _dir, gh) = ready(project).await;
        gh.script(SHA_1, vec![ended(SHA_1, "success", 1)]);
        // The daemon died after the dispatch and before `done` was written.
        crate::automations::dispatch_with(
            &pool,
            project,
            Event::CiFinished {
                sha: SHA_1.to_owned(),
                branch: "master".to_owned(),
                workflow: "ci.yml".to_owned(),
                conclusion: CiConclusion::Success,
                run_id: 7,
            },
            &gh,
        )
        .await;
        assert_eq!(incident_firings(&pool).await, 1);

        tick(&pool, &gh, soon()).await.unwrap();

        assert_eq!(incident_firings(&pool).await, 1, "the re-emit is a no-op");
        assert_eq!(state_of(&pool, project, SHA_1).await, WatchState::Done);
    }
}
