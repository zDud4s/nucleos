//! M4 C2 DB-level tests: the launcher seam seen with a real state and job row.
//!
//! Left to C7 (they need the `dispatch_agent_node` driver): `a_refusing_launcher_fails_the_node_
//! before_any_run_exists` (node row `failed`, feed `job_node_refused`) and
//! `an_empty_restriction_is_refused_before_any_run_exists` (same, end to end). What is pinned here
//! is the half that exists now: the launcher is asked, a refusal comes back as `Err`, and no run
//! row is written by asking.
use super::dispatch::{
    AgentNodeLauncher, LaunchRefused, NodeCtx, NodeLaunch, ProtocolAndBody, resolve_launch_with,
};
use super::tests::{seed_job, test_state};
use super::*;
use crate::models::route_advice::RunnerKind;
use crate::workflows::{facts::Facts, graph::Node};

struct Refusing;

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

struct Restricting(Vec<RunnerKind>);

#[async_trait::async_trait]
impl AgentNodeLauncher for Restricting {
    async fn launch(
        &self,
        _ctx: &NodeCtx<'_>,
        protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused> {
        Ok(NodeLaunch {
            restrict_runners: Some(self.0.clone()),
            ..NodeLaunch::plain(protocol)
        })
    }
}

async fn runs_count(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM runs")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn node(yaml: &str) -> Node {
    serde_yaml::from_str(yaml).expect("a node")
}

#[tokio::test]
async fn the_default_launcher_through_a_real_ctx_appends_the_body_after_the_protocol() {
    let pool = crate::testdb::fresh_pool().await;
    let job_id = seed_job(&pool, "p", "running").await.unwrap();
    let state = test_state(pool.clone()).await;
    let job = load_job(&pool, job_id).await.unwrap();
    let dir = std::env::temp_dir().join(format!("nucleos-c2-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    std::fs::write(dir.join("agents/triage.md"), "be brief").unwrap();
    let node = node("id: triage\ntype: agent\nbody: agents/triage.md\n");
    let facts = Facts::new();
    let ctx = NodeCtx {
        state: &state,
        job: &job,
        node: &node,
        round: 1,
        ordinal: None,
        worktree: &dir,
        facts: &facts,
    };

    let launch = ProtocolAndBody
        .launch(&ctx, "PROTOCOL".to_owned())
        .await
        .unwrap();
    assert_eq!(launch.prompt, "PROTOCOL\n\nbe brief");
    assert!(launch.restrict_runners.is_none() && launch.pin.is_none() && launch.output.is_none());
    assert!(ProtocolAndBody.output_check(&ctx).await.is_none());
    assert_eq!(
        runs_count(&pool).await,
        0,
        "asking a launcher creates no run"
    );
}

#[tokio::test]
async fn a_refusing_launcher_comes_back_as_err_and_writes_no_run() {
    let pool = crate::testdb::fresh_pool().await;
    let job_id = seed_job(&pool, "p", "running").await.unwrap();
    let state = test_state(pool.clone()).await;
    let job = load_job(&pool, job_id).await.unwrap();
    let node = node("id: review\ntype: agent\n");
    let facts = Facts::new();
    let ctx = NodeCtx {
        state: &state,
        job: &job,
        node: &node,
        round: 1,
        ordinal: None,
        worktree: std::path::Path::new("."),
        facts: &facts,
    };

    let refused = resolve_launch_with(&Refusing, &ctx, "P".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(refused, LaunchRefused::Unavailable(_)));
    assert!(refused.reason().contains("reviewer@1.0.0"));
    assert_eq!(runs_count(&pool).await, 0);
}

#[tokio::test]
async fn an_empty_restriction_is_refused_by_resolve_launch_and_a_real_one_passes() {
    let pool = crate::testdb::fresh_pool().await;
    let job_id = seed_job(&pool, "p", "running").await.unwrap();
    let state = test_state(pool.clone()).await;
    let job = load_job(&pool, job_id).await.unwrap();
    let node = node("id: review\ntype: agent\n");
    let facts = Facts::new();
    let ctx = NodeCtx {
        state: &state,
        job: &job,
        node: &node,
        round: 1,
        ordinal: None,
        worktree: std::path::Path::new("."),
        facts: &facts,
    };

    let refused = resolve_launch_with(&Restricting(Vec::new()), &ctx, "P".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(refused, LaunchRefused::Unavailable(_)));
    assert_eq!(runs_count(&pool).await, 0);

    let launch = resolve_launch_with(&Restricting(vec![RunnerKind::Codex]), &ctx, "P".to_owned())
        .await
        .unwrap();
    assert_eq!(launch.restrict_runners, Some(vec![RunnerKind::Codex]));
}
