//! Shared fixtures of the job-engine DB-level tests: graphs, seeded jobs, feed and row readers.

use super::dispatch::{AgentNodeLauncher, LaunchRefused, NodeCtx, NodeLaunch};
use super::tests::{seed_job, seed_node, seed_worktree, test_pool, test_state, write_plan};
use super::*;
use crate::workflows::effective::EffectiveGraph;
use crate::workflows::graph::Graph;

// ---- fixtures -----------------------------------------------------------------------------------

pub(super) fn graph_of(yaml: &str) -> EffectiveGraph {
    EffectiveGraph {
        graph: serde_yaml::from_str::<Graph>(yaml).expect("a graph"),
        ..Default::default()
    }
}

/// Freezes `graph` as the job's snapshot, the way `insert_job` would have.
pub(super) async fn freeze(pool: &sqlx::SqlitePool, job_id: i64, graph: &EffectiveGraph) {
    let snapshot = snapshot_of_effective(graph, None, None).expect("a snapshot");
    sqlx::query("UPDATE jobs SET graph_snapshot = ? WHERE id = ?")
        .bind(&snapshot.graph_json)
        .bind(job_id)
        .execute(pool)
        .await
        .expect("freeze the graph");
}

pub(super) async fn feed_kinds(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM feed ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

pub(super) async fn feed_summary_of(pool: &sqlx::SqlitePool, kind: &str) -> String {
    sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ? ORDER BY id LIMIT 1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|_| panic!("the feed has no `{kind}` line"))
}

pub(super) async fn count(pool: &sqlx::SqlitePool, table: &str) -> i64 {
    // Only this file's own literal table names reach here.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A job on a plain directory (no git), with its worktree recorded.
pub(super) async fn job_in_a_directory(
    graph: Option<&EffectiveGraph>,
) -> (AppState, JobRow, tempfile::TempDir) {
    let pool = test_pool().await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    seed_worktree(&pool, job_id, dir.path()).await;
    if let Some(graph) = graph {
        freeze(&pool, job_id, graph).await;
    }
    let state = test_state(pool.clone()).await;
    let job = load_job(&pool, job_id).await.unwrap();
    (state, job, dir)
}

pub(super) async fn queue_item(pool: &sqlx::SqlitePool, job_id: i64, ordinal: i64, status: &str) {
    let id = insert_fan_item(
        pool,
        &NewFanItem {
            job_id,
            node_id: "implement",
            ordinal,
            round: 0,
            description: "an item",
            files: None,
            depends_on: None,
            agent_id: None,
        },
    )
    .await
    .expect("queue an item");
    sqlx::query("UPDATE job_nodes SET status = ? WHERE id = ?")
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

/// The seeded autopilot, planned, with `statuses` queued, `spec` done: the shape both drivers read
/// the same way.
pub(super) async fn planned_autopilot(statuses: &[&str]) -> (AppState, JobRow, tempfile::TempDir) {
    let (state, job, dir) = job_in_a_directory(None).await;
    let pool = &state.pool;
    write_plan(dir.path(), r#"{"items":[{"description":"x"}]}"#).await;
    insert_singleton_node(pool, job.id, "plan", 0, "passed")
        .await
        .unwrap();
    seed_node(pool, job.id, "spec", "completed").await;
    for (ordinal, status) in statuses.iter().enumerate() {
        queue_item(pool, job.id, ordinal as i64, status).await;
    }
    let job = load_job(pool, job.id).await.unwrap();
    (state, job, dir)
}

pub(super) struct Refusing;

#[async_trait::async_trait]
impl AgentNodeLauncher for Refusing {
    async fn launch(
        &self,
        _ctx: &NodeCtx<'_>,
        _protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused> {
        Err(LaunchRefused::Unavailable(
            "reviewer@1.0.0 asks for [shell]; no available engine translates it".into(),
        ))
    }
}

pub(super) const TRIAGE: &str = "nodes:\n  - {id: triage, type: agent}\nedges: []\n";

/// A job on a plain directory whose `triage` node is `running` on a run that ended `run_status`
/// with `stdout`, after `attempts` retries.
pub(super) async fn triage_ended(
    run_status: &str,
    stdout: &str,
    attempts: i64,
) -> (AppState, JobRow, i64, tempfile::TempDir) {
    let graph = graph_of(TRIAGE);
    let (state, job, dir) = job_in_a_directory(Some(&graph)).await;
    let pool = &state.pool;
    let run_id = seed_node(pool, job.id, "triage", run_status).await;
    sqlx::query(
        "UPDATE runs SET stdout = ?, runner = 'claude', model = 'm1', effort = 'high' WHERE id = ?",
    )
    .bind(stdout)
    .bind(run_id)
    .execute(pool)
    .await
    .unwrap();
    insert_singleton_node(pool, job.id, "triage", 0, "running")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE job_nodes SET run_id = ?, gate_attempts = ?
         WHERE job_id = ? AND node_id = 'triage' AND ordinal IS NULL",
    )
    .bind(run_id)
    .bind(attempts)
    .bind(job.id)
    .execute(pool)
    .await
    .unwrap();
    (state, job, run_id, dir)
}

pub(super) async fn triage_status(pool: &sqlx::SqlitePool, job_id: i64) -> String {
    sqlx::query_scalar(
        "SELECT status FROM job_nodes WHERE job_id = ? AND node_id = 'triage' AND ordinal IS NULL",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub(super) const LOOP: &str = "nodes:
  - {id: work, type: agent}
  - {id: check, type: decision, condition: 'work.size == done'}
edges:
  - {from: work, to: check}
  - {from: check, to: work, when: false}
";
