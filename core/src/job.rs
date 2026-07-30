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

/// Starts a job and returns its id.
///
/// Fails when the project already has a live one. That refusal is the unique index
/// `one_live_job_per_project` (migration 0036) rather than a check here, deliberately: with the
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
