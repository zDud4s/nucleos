//! M4 C3 DB-level tests: the command dispatcher against a real pool and real child processes.
//!
//! The ones that need the driver loop (`advance`) are covered by calling `run_command_node` and
//! `follow_for` directly: the caller that maps `CommandFollow` to a job outcome arrives in a later
//! chunk. Windows: `sh` must be on PATH (PowerShell: prepend `C:\Program Files\Git\usr\bin`; from
//! Git Bash never `export PATH=`), the same rule the `run_gate` tests live under.

use super::command_node::{
    CommandFollow, CommandResult, InstanceRef, follow_for, run_command_node,
};
use super::tests::{seed_job, test_pool, test_state};
use super::*;
use crate::workflows::graph::Node;

fn node(yaml: &str) -> Node {
    serde_yaml::from_str(yaml).expect("a node")
}

const INSTANCE: InstanceRef = InstanceRef {
    round: 0,
    ordinal: None,
};

async fn setup() -> (AppState, JobRow, tempfile::TempDir) {
    let pool = test_pool().await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    let state = test_state(pool.clone()).await;
    let job = load_job(&pool, job_id).await.unwrap();
    (state, job, tempfile::tempdir().unwrap())
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    // Only this file's own literal table names reach here.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// `(status, gate_status)` of the node's singleton row in round 0.
async fn row_of(pool: &SqlitePool, job_id: i64, node_id: &str) -> Option<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT status, gate_status FROM job_nodes
         WHERE job_id = ? AND node_id = ? AND round = 0 AND ordinal IS NULL",
    )
    .bind(job_id)
    .bind(node_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_command_node_creates_no_runs_row() {
    let (state, job, tree) = setup().await;
    let before = count(&state.pool, "runs").await;
    let result = run_command_node(
        &state,
        &job,
        &node("id: lint\ntype: command\ncommand: git --version\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    assert_eq!(result, CommandResult::Passed);
    assert_eq!(count(&state.pool, "runs").await, before);
    assert_eq!(
        row_of(&state.pool, job.id, "lint").await,
        Some(("passed".to_owned(), Some("passed".to_owned())))
    );
}

#[tokio::test]
async fn a_command_node_runs_with_the_budget_exhausted() {
    let (state, job, tree) = setup().await;
    sqlx::query("UPDATE jobs SET budget_usd = 0.0001 WHERE id = ?")
        .bind(job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let job = load_job(&state.pool, job.id).await.unwrap();
    // The precondition: an agent node would not start, the allowance cannot cover another reserve.
    assert!(
        job_over_budget(&state.pool, &job, 0.0001, Utc::now())
            .await
            .unwrap()
            .is_some()
    );
    assert!(!matches!(brakes(&state, &job, Utc::now()).await, Brake::Go));
    // A command node consumes no quota, so it does.
    let result = run_command_node(
        &state,
        &job,
        &node("id: lint\ntype: command\ncommand: git --version\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    assert_eq!(result, CommandResult::Passed);
}

#[tokio::test]
async fn a_command_node_obeys_the_kill_switch() {
    let (state, job, tree) = setup().await;
    crate::governance::autopilot::set_kill_switch(&state.pool, true)
        .await
        .unwrap();
    let result = run_command_node(
        &state,
        &job,
        &node("id: lint\ntype: command\ncommand: git --version\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    assert_eq!(result, CommandResult::Parked);
    assert_eq!(follow_for(&result, false), CommandFollow::Parked);
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
            .bind(job.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(status, "waiting");
    assert_eq!(reason.as_deref(), Some("kill-switch"));
    assert_eq!(count(&state.pool, "verify_runs").await, 0, "no process ran");
    assert_eq!(row_of(&state.pool, job.id, "lint").await, None);
}

#[tokio::test]
async fn a_command_timeout_is_a_wall_clock_ceiling_that_errors_not_fails() {
    let (state, job, tree) = setup().await;
    let result = run_command_node(
        &state,
        &job,
        &node("id: slow\ntype: command\ncommand: 'sh -c \"sleep 5\"'\ntimeout: 1s\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    assert!(matches!(result, CommandResult::Errored(_)), "{result:?}");
    assert_eq!(follow_for(&result, true), CommandFollow::GateErrored);
    assert_eq!(
        row_of(&state.pool, job.id, "slow").await,
        Some(("gate_errored".to_owned(), Some("errored".to_owned())))
    );
}

#[tokio::test]
async fn a_command_that_will_not_start_is_gate_errored_and_stops_the_job() {
    let (state, job, tree) = setup().await;
    let result = run_command_node(
        &state,
        &job,
        &node("id: lint\ntype: command\ncommand: nucleos-no-such-program-xyz\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    assert!(matches!(result, CommandResult::Errored(_)), "{result:?}");
    // Silence is not a verdict, with or without verdict edges.
    assert_eq!(follow_for(&result, true), CommandFollow::GateErrored);
    assert_eq!(follow_for(&result, false), CommandFollow::GateErrored);
}

#[tokio::test]
async fn a_nonzero_exit_follows_the_fail_edge() {
    let (state, job, tree) = setup().await;
    let result = run_command_node(
        &state,
        &job,
        &node("id: check\ntype: command\ncommand: 'sh -c \"exit 3\"'\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    assert!(
        matches!(result, CommandResult::Failed { exit_code: 3, .. }),
        "{result:?}"
    );
    assert_eq!(follow_for(&result, true), CommandFollow::Fail);
    assert_eq!(
        row_of(&state.pool, job.id, "check").await,
        Some(("failed".to_owned(), Some("failed".to_owned())))
    );
}

#[tokio::test]
async fn no_verdict_edges_nonzero_fails_the_job() {
    let (state, job, tree) = setup().await;
    let result = run_command_node(
        &state,
        &job,
        &node("id: check\ntype: command\ncommand: 'sh -c \"exit 3\"'\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    assert!(matches!(
        follow_for(&result, false),
        CommandFollow::JobFails(_)
    ));
    assert_eq!(
        row_of(&state.pool, job.id, "check").await.map(|(s, _)| s),
        Some("failed".to_owned())
    );
}

#[tokio::test]
async fn a_verify_runs_row_is_recorded_with_origin_job_node() {
    let (state, job, tree) = setup().await;
    run_command_node(
        &state,
        &job,
        &node("id: lint\ntype: command\ncommand: git --version\n"),
        &INSTANCE,
        tree.path(),
    )
    .await;
    let (origin, origin_id, status): (String, Option<i64>, String) = sqlx::query_as(
        "SELECT origin, origin_id, status FROM verify_runs ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(origin, crate::verify::runs::ORIGIN_JOB_NODE);
    assert_eq!(origin_id, Some(job.id));
    assert_eq!(status, "passed");
}
