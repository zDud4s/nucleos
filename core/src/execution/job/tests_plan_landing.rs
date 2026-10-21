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
