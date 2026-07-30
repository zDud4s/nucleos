//! The job state machine: a sequence of runs over one shared worktree.
//!
//! A run is one `claude -p` subprocess and therefore one context window, which caps how large a
//! piece of autonomous work can be. A job lifts that cap by running several nodes in sequence —
//! `plan → implement×N → gate → review` — each with a fresh window, sharing state through the
//! worktree on disk rather than through a transcript.
//!
//! The module is being built bottom-up: the schema, the exclusivity invariant and the plan parser
//! land before the state machine that drives them, so each can be tested on its own terms. Until
//! that machine exists nothing in production calls any of this, hence the allow — it comes off with
//! the first caller, and if it is still here after that, something was built and never wired up.
#![allow(dead_code)]

use serde::Deserialize;
use sqlx::SqlitePool;

/// Why a plan node produced no usable queue.
///
/// `Absent` and an empty `items` list are different outcomes and must stay that way: the first says
/// the plan node failed to produce anything, the second says it looked and found no work. Only the
/// first is a failure.
#[derive(Debug)]
pub enum PlanError {
    Absent,
    Unreadable(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Absent => write!(formatter, "the plan node wrote no plan.json"),
            PlanError::Unreadable(reason) => write!(formatter, "plan.json is unreadable: {reason}"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PlanFile {
    items: Vec<PlanItem>,
}

#[derive(Debug, Deserialize)]
struct PlanItem {
    description: String,
}

/// A validated work queue, plus however much of it did not fit.
#[derive(Debug, PartialEq, Eq)]
pub struct PlannedItems {
    pub items: Vec<String>,
    /// Items the daemon's ceiling cut. Carried rather than discarded so the feed can say what was
    /// left out — a queue silently trimmed reads downstream as the whole of what the planner found.
    pub dropped: usize,
}

/// Reads the queue a plan node produced.
///
/// Takes bytes rather than a path so the decisions here stay testable without a filesystem, and so
/// "the file was missing" is expressed by the caller as `None` rather than inferred from an IO error
/// that could equally mean a permissions problem.
pub fn parse_plan(contents: Option<&[u8]>, max_items: usize) -> Result<PlannedItems, PlanError> {
    let bytes = contents.ok_or(PlanError::Absent)?;
    let parsed: PlanFile =
        serde_json::from_slice(bytes).map_err(|error| PlanError::Unreadable(error.to_string()))?;

    let total = parsed.items.len();
    let items: Vec<String> = parsed
        .items
        .into_iter()
        .take(max_items)
        .map(|item| item.description)
        .collect();

    Ok(PlannedItems {
        dropped: total - items.len(),
        items,
    })
}

/// Where one item of a job's queue has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Pending,
    Running,
    /// The run finished cleanly but the gate has not measured it yet.
    Implemented,
    /// Gated green, or no gate is configured for this project.
    Passed,
    Failed,
    GateFailed,
    GateErrored,
}

/// Whether the job still owes a review node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    NotWanted,
    Pending,
    Running,
    Done,
}

/// How a job ended.
///
/// `GateFailed` and `GateErrored` stay apart all the way up from `gate::GateOutcome`: a non-zero
/// exit says the code is broken, a binary that would not start says the measurement never happened.
/// Collapsing them would report silence as a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    GateFailed,
    GateErrored,
}

/// What the daemon should do next for a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    SpawnPlan,
    SpawnImplement {
        ordinal: usize,
    },
    RunGate {
        ordinal: usize,
    },
    SpawnReview,
    /// A node is in flight; nothing to do until it lands.
    Wait,
    Finish(Outcome),
}

/// Everything the decision below needs to see, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    /// Whether a plan node has already produced a queue. Distinct from `items` being empty, which
    /// is a planner that looked and found no work.
    pub planned: bool,
    pub items: Vec<ItemState>,
    pub review: ReviewState,
}

/// Decides a job's next move from what is observable about it.
///
/// Pure, and kept that way on purpose — the same split `classifier.rs` has from `hooks.rs`. Every
/// interesting question about a job ("does a red gate stop the chain?", "where does a resume pick
/// up?") becomes a table test with no database, no worktree and no subprocess. The caller owns the
/// I/O and is free to be dumb.
pub fn next_step(job: &JobView) -> Next {
    // Failures first, and before the in-flight check: a chain with a broken item must stop even if
    // another node is still running, rather than spending budget on work about to be thrown away.
    for item in &job.items {
        match item {
            ItemState::Failed => return Next::Finish(Outcome::Failed),
            ItemState::GateFailed => return Next::Finish(Outcome::GateFailed),
            ItemState::GateErrored => return Next::Finish(Outcome::GateErrored),
            _ => {}
        }
    }

    if !job.planned {
        return Next::SpawnPlan;
    }
    if job.items.is_empty() {
        return Next::Finish(Outcome::Completed);
    }
    if job.items.contains(&ItemState::Running) {
        return Next::Wait;
    }
    // Gate before starting the next item: on a shared worktree, letting item i+1 build on unmeasured
    // work means a later red gate cannot say which item broke it.
    if let Some(ordinal) = job
        .items
        .iter()
        .position(|item| *item == ItemState::Implemented)
    {
        return Next::RunGate { ordinal };
    }
    if let Some(ordinal) = job
        .items
        .iter()
        .position(|item| *item == ItemState::Pending)
    {
        return Next::SpawnImplement { ordinal };
    }

    match job.review {
        ReviewState::Pending => Next::SpawnReview,
        ReviewState::Running => Next::Wait,
        ReviewState::NotWanted | ReviewState::Done => Next::Finish(Outcome::Completed),
    }
}

impl Outcome {
    /// The `jobs.status` this outcome is stored as.
    pub fn as_status(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Failed => "failed",
            Outcome::GateFailed => "gate_failed",
            Outcome::GateErrored => "gate_errored",
        }
    }
}

fn item_state_from(status: &str) -> ItemState {
    match status {
        "running" => ItemState::Running,
        "implemented" => ItemState::Implemented,
        "passed" => ItemState::Passed,
        "failed" => ItemState::Failed,
        "gate_failed" => ItemState::GateFailed,
        "gate_errored" => ItemState::GateErrored,
        // An unrecognised item status is treated as still to do rather than as done. Erring toward
        // "not finished" costs a repeated item; erring the other way silently skips work the job
        // was created to perform and reports it complete.
        _ => ItemState::Pending,
    }
}

/// Assembles what `next_step` needs from the two tables.
pub async fn load_view(pool: &SqlitePool, job_id: i64) -> sqlx::Result<JobView> {
    let (status, review_wanted): (String, i64) =
        sqlx::query_as("SELECT status, review FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await?;

    let items: Vec<String> =
        sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
            .bind(job_id)
            .fetch_all(pool)
            .await?;

    // A review node is identified by its stage, not by the job's status: the job can be sitting in
    // `waiting` for budget while its review is the thing that has yet to run.
    let review_run: Option<String> = sqlx::query_scalar(
        "SELECT status FROM runs WHERE job_id = ? AND stage = 'review' ORDER BY id DESC LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;

    let review = match (review_wanted != 0, review_run.as_deref()) {
        (false, _) => ReviewState::NotWanted,
        (true, None) => ReviewState::Pending,
        (true, Some("running")) => ReviewState::Running,
        (true, Some(_)) => ReviewState::Done,
    };

    Ok(JobView {
        // `planning` is the one status that means the queue does not exist yet. Everything else has
        // been past the plan node, including a job whose planner honestly found nothing to do.
        planned: status != "planning",
        items: items.iter().map(|s| item_state_from(s)).collect(),
        review,
    })
}

/// Records how a job ended.
pub async fn finish(pool: &SqlitePool, job_id: i64, outcome: Outcome) -> sqlx::Result<()> {
    sqlx::query("UPDATE jobs SET status = ?, completed_at = ? WHERE id = ?")
        .bind(outcome.as_status())
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Parks a job that could not start its next node, so it is retried rather than abandoned.
///
/// Decision 15 of the design: the exclusivity slot belongs to a *run*, so nobody holds it in the gap
/// between two of a job's nodes, and an ordinary scheduler tick can take it. Losing that race is not
/// a failure of the work — the worktree and everything gated green so far are untouched — so the job
/// waits rather than reporting a partial. The 4-hour ceiling is what stops waiting forever.
///
/// `reason` is stored because `waiting` now means two different things — a budget window that will
/// reopen, and a slot another run is holding — and they call for opposite responses from a reader.
pub async fn wait(pool: &SqlitePool, job_id: i64, reason: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE jobs SET status = 'waiting', wait_reason = ? WHERE id = ?")
        .bind(reason)
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Starts a job and returns its id.
///
/// Fails when the project already has a live one. That refusal is the unique index
/// `one_live_job_per_project` (migration 0037) rather than a check here, deliberately: with the
/// constraint in the storage layer the INSERT itself is the lock, so a scheduler tick and a manual
/// request racing for the same project cannot both pass a check and then both proceed. It mirrors
/// what `one_open_worktree_run_per_project` already does for runs.
pub async fn insert_job(
    pool: &SqlitePool,
    project_id: &str,
    project_root: &str,
    status: &str,
    max_items: i64,
) -> sqlx::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(project_root)
    .bind(status)
    .bind(max_items)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn view(planned: bool, items: &[ItemState], review: ReviewState) -> JobView {
        JobView {
            planned,
            items: items.to_vec(),
            review,
        }
    }

    #[test]
    fn a_job_walks_plan_then_each_item_then_review() {
        // One walk rather than five isolated assertions: the sequence is the behaviour, and a
        // transition that is right in isolation can still be reached in the wrong order.
        let mut items = vec![ItemState::Pending, ItemState::Pending];

        assert_eq!(
            next_step(&view(false, &[], ReviewState::Pending)),
            Next::SpawnPlan
        );
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement { ordinal: 0 }
        );

        items[0] = ItemState::Implemented;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::RunGate { ordinal: 0 }
        );

        items[0] = ItemState::Passed;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement { ordinal: 1 }
        );

        items[1] = ItemState::Passed;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnReview
        );
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Done)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn an_empty_queue_completes_without_implementing_anything() {
        // A planner that found no work had a successful night. Only an absent plan.json is a
        // failure, and that distinction is made before this function ever sees the job.
        assert_eq!(
            next_step(&view(true, &[], ReviewState::Pending)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_failed_item_stops_the_chain_instead_of_moving_on() {
        let items = [ItemState::Failed, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::Finish(Outcome::Failed)
        );
    }

    #[test]
    fn a_failed_gate_and_an_errored_gate_end_the_job_differently() {
        // The distinction gate.rs guards and §7 of the spec insists must survive the trip up to the
        // job: a non-zero exit says the code is broken, a binary that would not start says the
        // measurement never happened. One of those is a verdict; the other is silence.
        assert_eq!(
            next_step(&view(true, &[ItemState::GateFailed], ReviewState::Pending)),
            Next::Finish(Outcome::GateFailed)
        );
        assert_eq!(
            next_step(&view(true, &[ItemState::GateErrored], ReviewState::Pending)),
            Next::Finish(Outcome::GateErrored)
        );
    }

    #[test]
    fn work_resumes_at_the_first_unfinished_item_not_the_first_item() {
        // What `stage_cursor` exists for. Resuming at zero would redo work the gate already passed,
        // on a worktree that still holds it.
        let items = [ItemState::Passed, ItemState::Passed, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement { ordinal: 2 }
        );
    }

    #[test]
    fn a_running_node_is_waited_for_rather_than_raced() {
        let items = [ItemState::Running, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::Wait
        );
    }

    #[test]
    fn a_job_without_review_completes_at_the_last_item() {
        let items = [ItemState::Passed];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::NotWanted)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn an_absent_plan_is_a_planning_failure_not_an_empty_queue() {
        // The pair that matters most in this module. "The planner produced nothing" is a failure;
        // "the planner found no work" is a success. Collapsing them would make every crashed plan
        // node look like a quiet, successful night, and teach the reader to ignore the feed.
        assert!(matches!(parse_plan(None, 5), Err(PlanError::Absent)));
    }

    #[test]
    fn an_unparseable_plan_is_a_planning_failure() {
        let garbage = br#"{"items": [ truncated"#;
        assert!(matches!(
            parse_plan(Some(garbage), 5),
            Err(PlanError::Unreadable(_))
        ));
    }

    #[test]
    fn an_empty_item_list_is_a_legitimate_result() {
        let empty = br#"{"items": []}"#;
        let plan = parse_plan(Some(empty), 5).expect("an empty queue is a result, not an error");
        assert!(plan.items.is_empty());
        assert_eq!(plan.dropped, 0);
    }

    #[test]
    fn a_plan_over_max_items_is_truncated_and_says_how_much() {
        let seven = br#"{"items":[{"description":"a"},{"description":"b"},{"description":"c"},
                                  {"description":"d"},{"description":"e"},{"description":"f"},
                                  {"description":"g"}]}"#;
        let plan =
            parse_plan(Some(seven), 5).expect("an oversized plan is truncated, not rejected");

        assert_eq!(plan.items.len(), 5);
        // Reported, never silent: a queue quietly cut from seven to five reads downstream as "the
        // planner found five things", which is a different and wrong statement about the work.
        assert_eq!(plan.dropped, 2);
    }

    async fn seed_items(pool: &sqlx::SqlitePool, job_id: i64, statuses: &[&str]) {
        for (ordinal, status) in statuses.iter().enumerate() {
            sqlx::query(
                "INSERT INTO job_items (job_id, ordinal, description, status)
                 VALUES (?, ?, 'an item', ?)",
            )
            .bind(job_id)
            .bind(ordinal as i64)
            .bind(status)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn a_job_still_planning_has_no_queue_yet() {
        let pool = test_pool().await;
        let job_id = insert_job(&pool, "project-a", "/project/a", "planning", 5)
            .await
            .unwrap();

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(!view.planned);
        assert_eq!(next_step(&view), Next::SpawnPlan);
    }

    #[tokio::test]
    async fn a_planner_that_found_nothing_is_not_a_job_still_planning() {
        let pool = test_pool().await;
        // Past the plan node with an empty queue: the honest "there was no work" night. Reading
        // this as still-planning would spawn a second planner every tick, forever.
        let job_id = insert_job(&pool, "project-a", "/project/a", "implementing", 5)
            .await
            .unwrap();

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(view.planned);
        assert_eq!(next_step(&view), Next::Finish(Outcome::Completed));
    }

    #[tokio::test]
    async fn a_loaded_view_resumes_at_the_first_unfinished_item() {
        let pool = test_pool().await;
        let job_id = insert_job(&pool, "project-a", "/project/a", "implementing", 5)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["passed", "passed", "pending"]).await;

        let view = load_view(&pool, job_id).await.unwrap();

        assert_eq!(next_step(&view), Next::SpawnImplement { ordinal: 2 });
    }

    #[tokio::test]
    async fn an_unrecognised_item_status_is_unfinished_rather_than_done() {
        let pool = test_pool().await;
        let job_id = insert_job(&pool, "project-a", "/project/a", "implementing", 5)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["passed", "something-new"]).await;

        let view = load_view(&pool, job_id).await.unwrap();

        // Erring toward "not finished" costs a repeated item. Erring the other way skips work the
        // job exists to do and reports it complete, which is the failure nobody sees.
        assert_eq!(next_step(&view), Next::SpawnImplement { ordinal: 1 });
    }

    #[tokio::test]
    async fn losing_the_slot_parks_the_job_instead_of_failing_it() {
        let pool = test_pool().await;
        let job_id = insert_job(&pool, "project-a", "/project/a", "implementing", 5)
            .await
            .unwrap();

        wait(&pool, job_id, "slot").await.unwrap();

        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "waiting");
        // The reason is stored because `waiting` now covers a budget window that will reopen and a
        // slot another run holds, and those ask opposite things of whoever reads the feed.
        assert_eq!(reason.as_deref(), Some("slot"));
    }

    #[tokio::test]
    async fn a_finished_job_records_which_kind_of_ending_it_had() {
        let pool = test_pool().await;
        let broken = insert_job(&pool, "project-a", "/project/a", "gating", 5)
            .await
            .unwrap();
        finish(&pool, broken, Outcome::GateFailed).await.unwrap();
        let unmeasured = insert_job(&pool, "project-b", "/project/b", "gating", 5)
            .await
            .unwrap();
        finish(&pool, unmeasured, Outcome::GateErrored)
            .await
            .unwrap();

        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        // Distinct in the database, not just in the enum: a reader querying jobs must still be able
        // to tell broken code from a measurement that never happened.
        assert_eq!(statuses, vec!["gate_failed", "gate_errored"]);
    }

    #[tokio::test]
    async fn a_second_live_job_for_a_project_is_rejected_by_the_index() {
        let pool = test_pool().await;
        insert_job(&pool, "project-a", "/project/a", "planning", 5)
            .await
            .expect("the first job starts");

        let second = insert_job(&pool, "project-a", "/project/a", "planning", 5).await;

        // Rejected by the unique index rather than by a check in this module: the INSERT is the
        // lock, so the scheduler tick and a manual POST racing for the same project cannot both win.
        assert!(second.is_err(), "a project may have only one live job");
    }

    #[tokio::test]
    async fn a_finished_job_does_not_hold_the_project_slot() {
        let pool = test_pool().await;
        insert_job(&pool, "project-a", "/project/a", "completed", 5)
            .await
            .expect("a job that has finished");

        // The partial index covers live statuses only. Without that, a project would be wedged
        // forever by its own first job — a worse failure than the race the index exists to stop,
        // because nothing would ever clear it.
        insert_job(&pool, "project-a", "/project/a", "planning", 5)
            .await
            .expect("a new job may start once the last one is done");
    }
}
