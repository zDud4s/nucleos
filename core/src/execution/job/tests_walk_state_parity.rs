//! M4 C6 DB-level tests: the real `WalkState` builder against the legacy loader, and the shadow
//! differential that never changes what runs.

use super::engine::{load_graph, walk_state};
use super::tests::{seed_job, seed_node, seed_worktree, test_pool, test_state};
use super::*;
use crate::workflows::walk::NodeState;

/// One fan item of the autopilot's `work` fan, in the given round and status.
async fn item(
    pool: &sqlx::SqlitePool,
    job_id: i64,
    ordinal: i64,
    round: i64,
    status: &str,
    gate_attempts: i64,
) {
    let id = insert_fan_item(
        pool,
        &NewFanItem {
            job_id,
            node_id: "implement",
            ordinal,
            round,
            description: "an item",
            files: None,
            depends_on: None,
            agent_id: None,
        },
    )
    .await
    .expect("queue an item");
    sqlx::query("UPDATE job_nodes SET status = ?, gate_attempts = ? WHERE id = ?")
        .bind(status)
        .bind(gate_attempts)
        .bind(id)
        .execute(pool)
        .await
        .expect("move the item");
}

async fn plan_row(pool: &sqlx::SqlitePool, job_id: i64) {
    insert_singleton_node(pool, job_id, "plan", 0, "passed")
        .await
        .expect("record the plan");
}

async fn set(pool: &sqlx::SqlitePool, job_id: i64, column: &str, value: i64) {
    // Test-only: `column` is a literal of this file.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE jobs SET {column} = ? WHERE id = ?"
    )))
    .bind(value)
    .bind(job_id)
    .execute(pool)
    .await
    .expect("set a job column");
}

/// The new loader's `WalkState` must equal the legacy bridge's reading of `load_view`, for a job
/// in whatever state the rows are in.
async fn same_as_the_legacy_reading(pool: &sqlx::SqlitePool, job_id: i64, label: &str) {
    let job = load_job(pool, job_id).await.unwrap();
    let graph = load_graph(pool, job_id).await.unwrap();
    let got = walk_state(pool, &job, &graph).await.unwrap();
    let view = load_view(pool, job_id).await.unwrap();
    let (want, _facts) = super::bridge::walk_state_of(&graph.graph, &view);
    assert_eq!(got, want, "{label}");
}

#[tokio::test]
async fn walk_state_matches_walk_state_of_for_every_legacy_fixture() {
    let pool = test_pool().await;
    let mut n = 0;
    macro_rules! scenario {
        ($label:expr, |$job:ident| $body:block) => {{
            n += 1;
            let $job = seed_job(&pool, &format!("project-{n}"), "running")
                .await
                .unwrap();
            $body
            same_as_the_legacy_reading(&pool, $job, $label).await;
        }};
    }

    scenario!("a job with nothing done", |job| {
        let _ = job;
    });
    scenario!("a spec in flight", |job| {
        seed_node(&pool, job, "spec", "running").await;
    });
    scenario!("a spec that ended", |job| {
        seed_node(&pool, job, "spec", "completed").await;
    });
    scenario!("a plan in flight", |job| {
        seed_node(&pool, job, "spec", "completed").await;
        seed_node(&pool, job, "plan", "running").await;
    });
    scenario!("a plan that found nothing", |job| {
        plan_row(&pool, job).await;
    });
    scenario!("a queue with a review owed", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "passed", 0).await;
        item(&pool, job, 1, 0, "pending", 0).await;
    });
    scenario!("a review in flight", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "passed", 0).await;
        seed_node(&pool, job, "review", "running").await;
    });
    scenario!("a review that landed", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "passed", 0).await;
        seed_node(&pool, job, "review", "completed").await;
    });
    scenario!("a review that failed", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "passed", 0).await;
        seed_node(&pool, job, "review", "failed").await;
    });
    scenario!("a job that wants no review", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "passed", 0).await;
        set(&pool, job, "review", 0).await;
    });
    scenario!("a replan in flight", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "passed", 0).await;
        seed_node(&pool, job, "replan", "running").await;
    });
    scenario!("a replan that said done", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "passed", 0).await;
        set(&pool, job, "replan_done", 1).await;
    });
    scenario!("a red gate with a retry left", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "gate_failed", 1).await;
        set(&pool, job, "gate_retries", 1).await;
    });
    scenario!("a red gate out of retries", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "gate_failed", 2).await;
        set(&pool, job, "gate_retries", 1).await;
    });
    scenario!("a round in which nothing passed", |job| {
        plan_row(&pool, job).await;
        item(&pool, job, 0, 0, "gate_failed", 3).await;
    });
    scenario!(
        "the second round, the first round's review behind the line",
        |job| {
            plan_row(&pool, job).await;
            item(&pool, job, 0, 0, "passed", 0).await;
            seed_node(&pool, job, "review", "completed").await;
            let replan = seed_node(&pool, job, "replan", "completed").await;
            item(&pool, job, 1, 1, "passed", 0).await;
            set(&pool, job, "round", 1).await;
            set(&pool, job, "max_rounds", 3).await;
            set(&pool, job, "dry_rounds", 1).await;
            set(&pool, job, "replan_run_id", replan).await;
        }
    );
}

#[tokio::test]
async fn a_round_that_queued_nothing_reads_review_as_skipped() {
    let pool = test_pool().await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    plan_row(&pool, job_id).await;
    let job = load_job(&pool, job_id).await.unwrap();
    let graph = load_graph(&pool, job_id).await.unwrap();

    // Nothing queued this round.
    let state = walk_state(&pool, &job, &graph).await.unwrap();
    assert_eq!(state.nodes.get("review"), Some(&NodeState::Skipped));

    // Queued, and nothing passed: still no diff for a review to judge.
    item(&pool, job_id, 0, 0, "gate_failed", 3).await;
    let state = walk_state(&pool, &job, &graph).await.unwrap();
    assert_eq!(state.nodes.get("review"), Some(&NodeState::Skipped));

    // Something passed: the review is owed, which is "absent from the map".
    item(&pool, job_id, 1, 0, "passed", 0).await;
    let state = walk_state(&pool, &job, &graph).await.unwrap();
    assert_eq!(state.nodes.get("review"), None);
}

#[tokio::test]
async fn a_replan_in_flight_reads_replanning_true() {
    let pool = test_pool().await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    plan_row(&pool, job_id).await;
    let job = load_job(&pool, job_id).await.unwrap();
    let graph = load_graph(&pool, job_id).await.unwrap();

    assert!(!walk_state(&pool, &job, &graph).await.unwrap().replanning);
    let run = seed_node(&pool, job_id, "replan", "running").await;
    assert!(walk_state(&pool, &job, &graph).await.unwrap().replanning);
    sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!walk_state(&pool, &job, &graph).await.unwrap().replanning);
}

#[tokio::test]
async fn a_project_disabled_node_is_in_the_walk_state() {
    let pool = test_pool().await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    let job = load_job(&pool, job_id).await.unwrap();
    let mut graph = load_graph(&pool, job_id).await.unwrap();
    graph.disabled.insert("review".to_string());
    let state = walk_state(&pool, &job, &graph).await.unwrap();
    assert_eq!(
        state.disabled.get("review"),
        Some(&crate::workflows::walk::Disabled::Project)
    );
}

async fn diverged_lines(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'job_walker_diverged'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_divergence_is_said_once_and_does_not_change_what_runs() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    seed_worktree(&pool, job_id, worktree.path()).await;
    let job = load_job(&pool, job_id).await.unwrap();

    // A fresh job: the walker starts the spec node, the driver is told something else.
    super::drive::shadow_differential(&state, &job, 0, &Next::SpawnPlan).await;
    super::drive::shadow_differential(&state, &job, 0, &Next::SpawnPlan).await;
    assert_eq!(
        diverged_lines(&pool).await,
        1,
        "said once, not once per pass"
    );

    // Agreement says nothing.
    super::drive::shadow_differential(&state, &job, 0, &Next::SpawnSpec).await;
    assert_eq!(diverged_lines(&pool).await, 1);

    // And nothing ran or moved.
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE job_id = ?")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
    assert_eq!(super::tests::job_status(&pool, job_id).await, "running");
}

#[tokio::test]
async fn a_job_without_a_worktree_is_not_said_to_diverge() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    let job = load_job(&pool, job_id).await.unwrap();
    super::drive::shadow_differential(&state, &job, 0, &Next::SpawnPlan).await;
    assert_eq!(diverged_lines(&pool).await, 0);
}
