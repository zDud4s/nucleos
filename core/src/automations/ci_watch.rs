//! The CI watch: state of the CI of every sha an automation pushed, and the pure decision of what
//! one poll of `gh` concludes. The state lives in the `ci_watch` table so a restart resumes where the
//! watch was; [`decide`] touches neither the database nor `gh`.

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

use super::CiConclusion;
use crate::integrations::github::{RunState, RunSummary};

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

/// The pure decision. Order: an older push is superseded; no run waits for 15 minutes; an
/// unfinished run waits for 120; a completed run that still shows the pre-rerun result waits (the
/// race right after `gh run rerun`); then the conclusion decides.
pub fn decide(w: &Watched, runs: &[RunSummary], now: DateTime<Utc>) -> Step {
    if !w.newest {
        return Step::Superseded;
    }
    let run = runs
        .iter()
        .filter(|run| run.head_sha == w.sha)
        .max_by(|a, b| (&a.created_at, a.id).cmp(&(&b.created_at, b.id)));
    let Some(run) = run else {
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
         WHERE project_id = ? AND sha = ? AND workflow = ? AND warned_at IS NULL",
    )
    .bind(stamp(now))
    .bind(reason)
    .bind(&w.project_id)
    .bind(&w.sha)
    .bind(&w.workflow)
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
}
