//! M4 C4 DB-level tests: plan.json on the M1 emission reader.

use super::engine::{PLAN_READS, clear_stale_emission};
use super::tests::{
    job_status, seed_closed_round, seed_job, seed_node, seed_worktree, test_pool, test_state,
    write_plan,
};
use super::*;

/// A replan node that writes nothing used to re-read the previous round's `plan.json` and could
/// re-open the same queue. The dispatcher now clears the file after `archive_plans`; the ingest of
/// that landing sees an absent file and takes the existing `stop_after_replan` path.
#[tokio::test]
async fn a_replan_that_writes_nothing_stops_the_job_and_does_not_reuse_the_last_plan() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    // The previous round's plan is still on disk when the replan node is about to be dispatched.
    let job_id = seed_closed_round(
        &pool,
        worktree.path(),
        "completed",
        Some(r#"{"items": [{"description": "the same queue again"}]}"#),
    )
    .await;
    archive_plans(worktree.path(), 0).await;
    clear_stale_emission(worktree.path(), "plan").await.unwrap();

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(job_status(&pool, job_id).await, STATUS_STOPPED);
    let round: i64 = sqlx::query_scalar("SELECT round FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(round, 0, "no new round was opened from a stale plan");
    // The archive the replan prompt names survives the clearing.
    assert!(
        worktree
            .path()
            .join(crate::execution::worktree::ARTIFACTS_DIR)
            .join("plan-0.json")
            .exists()
    );
}

/// One landing, one read: the queue and the flat facts come from the same bytes.
#[tokio::test]
async fn plan_json_is_read_once_per_landing() {
    // A plan node landing.
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    seed_node(&pool, job_id, "plan", "completed").await;
    write_plan(worktree.path(), r#"{"items":[{"description":"one"}]}"#).await;

    PLAN_READS.with(|reads| reads.set(0));
    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();
    assert_eq!(PLAN_READS.with(|reads| reads.get()), 1, "plan landing");

    // A replan node landing.
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_closed_round(
        &pool,
        worktree.path(),
        "completed",
        Some(r#"{"items": [{"description": "one more thing"}]}"#),
    )
    .await;
    PLAN_READS.with(|reads| reads.set(0));
    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();
    assert_eq!(PLAN_READS.with(|reads| reads.get()), 1, "replan landing");
}

/// An unreadable `plan.json` (a directory in its place) is a named failure, not an absent file.
#[tokio::test]
async fn an_unreadable_plan_file_fails_the_plan_node_with_the_io_reason() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    seed_node(&pool, job_id, "plan", "completed").await;
    std::fs::create_dir_all(
        worktree
            .path()
            .join(crate::execution::worktree::ARTIFACTS_DIR)
            .join(PLAN_FILE),
    )
    .unwrap();

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(job_status(&pool, job_id).await, "failed");
    let told: Vec<String> = sqlx::query_scalar("SELECT summary FROM feed ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        told.iter().any(|line| line.contains("cannot read")),
        "{told:?}"
    );
}

/// A restart marks every live run `interrupted`; that is the daemon going away, not the planner
/// failing, so the job must stay alive and its plan node be started again.
#[tokio::test]
async fn an_interrupted_plan_node_is_replanned_not_failed() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    seed_node(&pool, job_id, "plan", "interrupted").await;

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(job_status(&pool, job_id).await, "running");
    let view = load_view(&pool, job_id).await.unwrap();
    assert_eq!(next_step(&view), Next::SpawnPlan);
}

/// The count lives in the `runs` rows, so a crash loop cannot re-plan forever: two interruptions
/// are re-planned, the third fails the job and the reason names them.
#[tokio::test]
async fn a_plan_node_interrupted_three_times_fails_the_job_naming_the_interruptions() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    for _ in 0..3 {
        seed_node(&pool, job_id, "plan", "interrupted").await;
    }

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(job_status(&pool, job_id).await, "failed");
    let told: Vec<(String, String)> = sqlx::query_as("SELECT kind, summary FROM feed")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        told.iter()
            .any(|(kind, line)| kind == "job_plan_failed" && line.contains("interrupted 3 times")),
        "{told:?}"
    );
}

/// Only an interruption is retried: a plan node that failed on its own still ends the job.
#[tokio::test]
async fn a_plan_node_that_failed_by_itself_is_not_replanned() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    seed_node(&pool, job_id, "plan", "failed").await;

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(job_status(&pool, job_id).await, "failed");
}

/// Seeds one `running` item whose node run ended `status`, the way a restart leaves it.
async fn seed_item_with_node(pool: &sqlx::SqlitePool, job_id: i64, status: &str) -> i64 {
    sqlx::query(
        "INSERT INTO job_nodes (job_id, node_id, ordinal, description, status)
         VALUES (?, 'implement', 0, 'an item', 'running')",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .unwrap();
    point_item_at_new_node(pool, job_id, status).await
}

/// Adds another node run to item 0 and points the item at it, as a fresh spawn does.
async fn point_item_at_new_node(pool: &sqlx::SqlitePool, job_id: i64, status: &str) -> i64 {
    let run_id = seed_node(pool, job_id, "implement", status).await;
    sqlx::query("UPDATE job_nodes SET run_id = ? WHERE job_id = ? AND ordinal = 0")
        .bind(run_id)
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
    run_id
}

async fn item_row(pool: &sqlx::SqlitePool, job_id: i64) -> (String, i64) {
    sqlx::query_as("SELECT status, interruptions FROM job_nodes WHERE job_id = ? AND ordinal = 0")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A restart mid-item is the daemon going away, not the item failing: the item goes back to
/// `pending` so the queue gives it a fresh run, and the job carries on.
#[tokio::test]
async fn an_item_whose_node_was_interrupted_goes_back_to_pending() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    seed_item_with_node(&pool, job_id, "interrupted").await;

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(item_row(&pool, job_id).await, ("pending".to_owned(), 1));
    assert_eq!(job_status(&pool, job_id).await, "running");
    let view = load_view(&pool, job_id).await.unwrap();
    assert_eq!(view.items[0].state, ItemState::Pending);
}

/// The count is durable: two interruptions are re-run, the third fails the item and the reason
/// names them, so a crash loop cannot re-run an item forever.
#[tokio::test]
async fn an_item_interrupted_three_times_fails_naming_the_interruptions() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    seed_item_with_node(&pool, job_id, "interrupted").await;

    for round in 1..=2 {
        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();
        assert_eq!(item_row(&pool, job_id).await, ("pending".to_owned(), round));
        // The next pass claims it and starts a node, which the next restart interrupts again.
        sqlx::query("UPDATE job_nodes SET status = 'running' WHERE job_id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        point_item_at_new_node(&pool, job_id, "interrupted").await;
    }

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(item_row(&pool, job_id).await.0, "failed");
    let told: Vec<(String, String)> = sqlx::query_as("SELECT kind, summary FROM feed")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        told.iter()
            .any(|(kind, line)| kind == "job_item_failed" && line.contains("interrupted 3 times")),
        "{told:?}"
    );
}

/// Only an interruption is re-run: a node that failed on its own still fails its item.
#[tokio::test]
async fn an_item_whose_node_failed_by_itself_is_not_run_again() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    seed_item_with_node(&pool, job_id, "failed").await;

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes(&state, &job).await.unwrap();

    assert_eq!(item_row(&pool, job_id).await, ("failed".to_owned(), 0));
}

// ---- transient API errors: retried on a backoff, then failed ------------------------------------

/// The last line of a run the API refused outright: a 400 is the request, and a second attempt
/// would be refused the same way.
const REFUSED_BY_THE_API: &str = r#"{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","api_error_status":400,"result":"API Error: 400 invalid request"}"#;

/// A fixed instant, so the backoff is measured against the clock the test hands in, never a sleep.
fn at_noon() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

async fn set_stdout(pool: &sqlx::SqlitePool, run_id: i64, stdout: &str) {
    sqlx::query("UPDATE runs SET stdout = ? WHERE id = ?")
        .bind(stdout)
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
}

/// `(status, transient_retries, transient_retry_at)` of item 0.
async fn retry_row(pool: &sqlx::SqlitePool, job_id: i64) -> (String, i64, Option<String>) {
    sqlx::query_as(
        "SELECT status, transient_retries, transient_retry_at FROM job_nodes
         WHERE job_id = ? AND ordinal = 0",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn reconcile_at(state: &AppState, pool: &sqlx::SqlitePool, job_id: i64, now: DateTime<Utc>) {
    let job = load_job(pool, job_id).await.unwrap();
    reconcile_nodes_at(state, &job, now).await.unwrap();
}

/// The next pass claims the item and starts a node, which fails on the API again.
async fn fail_again_on_the_api(pool: &sqlx::SqlitePool, job_id: i64) {
    sqlx::query("UPDATE job_nodes SET status = 'running' WHERE job_id = ? AND ordinal = 0")
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
    let run_id = point_item_at_new_node(pool, job_id, "failed").await;
    set_stdout(
        pool,
        run_id,
        crate::execution::runner::REVIEW_THAT_NEVER_REACHED_THE_API,
    )
    .await;
}

/// A node that never reached the API says nothing about the item: the item waits out 30s, then
/// 2m, then 8m, each wait read off the clock the pass is handed, and goes back to `pending` for a
/// fresh run of the SAME item — the count is on the item's row, no new item is queued.
#[tokio::test]
async fn an_item_that_failed_on_a_transient_api_error_is_retried_on_the_backoff_schedule() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    let run_id = seed_item_with_node(&pool, job_id, "failed").await;
    set_stdout(
        &pool,
        run_id,
        crate::execution::runner::REVIEW_THAT_NEVER_REACHED_THE_API,
    )
    .await;

    let mut now = at_noon();
    for (retry, wait_secs) in [(1i64, 30i64), (2, 120), (3, 480)] {
        // The failure is seen: the item holds on its failed run until the backoff is out.
        reconcile_at(&state, &pool, job_id, now).await;
        let (status, retries, due) = retry_row(&pool, job_id).await;
        assert_eq!((status.as_str(), retries), ("running", retry - 1));
        let due = DateTime::parse_from_rfc3339(&due.expect("a retry time is recorded"))
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(due, now + chrono::Duration::seconds(wait_secs));

        // One second short of it, still waiting.
        reconcile_at(&state, &pool, job_id, due - chrono::Duration::seconds(1)).await;
        assert_eq!(retry_row(&pool, job_id).await.0, "running");

        // At it, the same item is put back for a fresh run, and the job carries on.
        now = due;
        reconcile_at(&state, &pool, job_id, now).await;
        assert_eq!(
            retry_row(&pool, job_id).await,
            ("pending".to_owned(), retry, None)
        );
        assert_eq!(job_status(&pool, job_id).await, "running");

        fail_again_on_the_api(&pool, job_id).await;
        now += chrono::Duration::seconds(5);
    }

    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM job_nodes WHERE job_id = ?")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        items, 1,
        "a retry is a new attempt of the item, never a new item"
    );
    let told: Vec<(String, String)> = sqlx::query_as("SELECT kind, summary FROM feed")
        .fetch_all(&pool)
        .await
        .unwrap();
    for line in [
        "retry 1 of 3 in 30s",
        "retry 2 of 3 in 2m",
        "retry 3 of 3 in 8m",
    ] {
        assert!(
            told.iter()
                .any(|(kind, summary)| kind == "job_item_retry" && summary.contains(line)),
            "{line:?} in {told:?}"
        );
    }
}

/// Only a transient error is retried: an API that refused the request fails the item at once, as
/// it always has.
#[tokio::test]
async fn an_item_that_failed_on_a_non_transient_api_error_is_not_retried() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    let run_id = seed_item_with_node(&pool, job_id, "failed").await;
    set_stdout(&pool, run_id, REFUSED_BY_THE_API).await;

    reconcile_at(&state, &pool, job_id, at_noon()).await;

    assert_eq!(
        retry_row(&pool, job_id).await,
        ("failed".to_owned(), 0, None)
    );
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM feed")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        !kinds.iter().any(|kind| kind == "job_item_retry"),
        "{kinds:?}"
    );
}

/// Three retries and no more: a service that stays down fails the item the fourth time, and the
/// reason names the retries.
#[tokio::test]
async fn an_item_that_fails_on_the_api_after_three_retries_fails() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    let run_id = seed_item_with_node(&pool, job_id, "failed").await;
    set_stdout(
        &pool,
        run_id,
        crate::execution::runner::REVIEW_THAT_NEVER_REACHED_THE_API,
    )
    .await;

    let mut now = at_noon();
    for retry in 1..=3 {
        reconcile_at(&state, &pool, job_id, now).await;
        // Well past the longest backoff.
        now += chrono::Duration::hours(1);
        reconcile_at(&state, &pool, job_id, now).await;
        assert_eq!(
            retry_row(&pool, job_id).await,
            ("pending".to_owned(), retry, None)
        );
        fail_again_on_the_api(&pool, job_id).await;
    }

    reconcile_at(&state, &pool, job_id, now).await;

    assert_eq!(
        retry_row(&pool, job_id).await,
        ("failed".to_owned(), 3, None)
    );
    let told: Vec<(String, String)> = sqlx::query_as("SELECT kind, summary FROM feed")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(
        told.iter().any(|(kind, line)| kind == "job_item_failed"
            && line.contains("transient API error")
            && line.contains("3 retries")),
        "{told:?}"
    );
}
