//! DB-level tests of agent nodes: the launcher seam and its refusals, a real run's stage and link,
//! landing, output validation and its single retry, parked approvals and the autopilot's own nodes.
//!
//! The tests that create a real run need a git repository and the worktree-root override, so they
//! hold `test_env_lock()` across their awaits like every other test that does.
#![allow(clippy::await_holding_lock)]

use super::dispatch::{
    AgentNodeLauncher, EnginePin, LaunchRefused, NodeCtx, NodeLaunch, OutputCheck, ProtocolAndBody,
};
use super::drive::advance_with;
use super::engine::{self, Action, AgentTarget};
use super::tests::{job_status, seed_node};
use super::tests_job_support::{
    Refusing, TRIAGE, count, feed_kinds, feed_summary_of, graph_of, job_in_a_directory,
    triage_ended, triage_status,
};
use super::*;
use crate::models::route_advice::RunnerKind;
use crate::workflows::facts::Facts;
use crate::workflows::walk::Step;
use std::sync::Arc;

// ---- the launcher seam: refusals ----------------------------------------------------------------

struct Restricting(Vec<crate::models::route_advice::RunnerKind>);

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

/// Dispatches the `triage` node of a one-node graph through `launcher`, on a plain directory.
async fn dispatch_triage(launcher: &dyn AgentNodeLauncher) -> (AppState, JobRow) {
    let graph = graph_of(TRIAGE);
    let (state, job, dir) = job_in_a_directory(Some(&graph)).await;
    let view = load_view(&state.pool, job.id).await.unwrap();
    let facts = Facts::new();
    let pass = engine::dispatch_agent_node_with(
        launcher,
        &state,
        &job,
        AgentTarget {
            view: &view,
            facts: &facts,
            graph: &graph,
            node_id: "triage",
            replan: false,
        },
        (dir.path().to_path_buf(), "nucleos/job".to_string()),
    )
    .await;
    assert_eq!(pass, Pass::Stopped);
    (state, job)
}

#[tokio::test]
async fn a_refusing_launcher_fails_the_node_before_any_run_exists() {
    let (state, job) = dispatch_triage(&Refusing).await;
    assert_eq!(count(&state.pool, "runs").await, 0);
    assert_eq!(
        singleton_status(&state.pool, job.id, "triage", 0)
            .await
            .unwrap()
            .as_deref(),
        Some("failed")
    );
    assert_eq!(job_status(&state.pool, job.id).await, "failed");
    let said = feed_summary_of(&state.pool, "job_node_refused").await;
    assert!(
        said.contains("triage") && said.contains("reviewer@1.0.0"),
        "{said}"
    );
}

#[tokio::test]
async fn an_empty_restriction_is_refused_before_any_run_exists() {
    let (state, job) = dispatch_triage(&Restricting(Vec::new())).await;
    assert_eq!(count(&state.pool, "runs").await, 0);
    assert_eq!(
        singleton_status(&state.pool, job.id, "triage", 0)
            .await
            .unwrap()
            .as_deref(),
        Some("failed")
    );
    assert_eq!(job_status(&state.pool, job.id).await, "failed");
    let said = feed_summary_of(&state.pool, "job_node_refused").await;
    assert!(said.contains("allows no engine"), "{said}");
}

// ---- a real run: stage, link, item_id -----------------------------------------------------------

/// Dispatches `triage` through the default launcher in a real git worktree, and returns the state,
/// the job, the run it started and the guards that keep the fixtures alive.
async fn dispatch_triage_for_real() -> (AppState, JobRow, i64, Box<dyn std::any::Any>) {
    use crate::execution::runs::tests::{WorktreeRootEnv, init_contained_repo, space_free_tempdir};
    let lock = crate::execution::worktree::test_env_lock();
    let wt_root = space_free_tempdir("nucleos-c7-wt-");
    let env = WorktreeRootEnv::set(wt_root.path());
    let (container, repo) = init_contained_repo("nucleos-c7-");
    let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
        None,
        crate::state::DEFAULT_RUN_TIMEOUT,
    )
    .await;
    let project_root = repo.to_string_lossy().into_owned();
    let graph = graph_of(TRIAGE);
    let snapshot = snapshot_of_effective(&graph, None, None).unwrap();
    let job_id = insert_job(
        &state.pool,
        &NewJob {
            project_id: "proj",
            project_root: &project_root,
            rule_name: Some("nightly"),
            prompt: "triage the backlog",
            max_items: 5,
            gate_each: true,
            review: true,
            gate_retries: 0,
            head_sha: None,
            max_rounds: None,
            budget_usd: None,
            team_id: None,
            graph: Some(snapshot),
        },
    )
    .await
    .expect("start a job");
    let owner = crate::execution::worktree::Owner::Job(job_id);
    let info = crate::execution::worktree::create(&repo, owner)
        .await
        .expect("the job provisions its worktree once");
    crate::execution::worktree::record(
        &state.pool,
        owner,
        "proj",
        &project_root,
        &info.path.to_string_lossy(),
        &info.branch,
        info.base_sha.as_deref(),
    )
    .await
    .expect("record the job worktree");

    let job = load_job(&state.pool, job_id).await.unwrap();
    let view = load_view(&state.pool, job_id).await.unwrap();
    let facts = Facts::new();
    let pass = engine::dispatch_agent_node_with(
        &ProtocolAndBody,
        &state,
        &job,
        AgentTarget {
            view: &view,
            facts: &facts,
            graph: &graph,
            node_id: "triage",
            replan: false,
        },
        (info.path.clone(), info.branch.clone()),
    )
    .await;
    assert_eq!(pass, Pass::Stopped);
    let run_id: i64 = sqlx::query_scalar("SELECT id FROM runs WHERE job_id = ?")
        .bind(job_id)
        .fetch_one(&state.pool)
        .await
        .expect("a run was created");
    (
        state,
        job,
        run_id,
        Box::new((lock, env, wt_root, container)),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn a_bundle_node_id_becomes_runs_stage_and_the_run_is_linked_by_job_nodes_run_id() {
    let (state, job, run_id, _guards) = dispatch_triage_for_real().await;
    let stage: String = sqlx::query_scalar("SELECT stage FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(stage, "triage");
    let (status, linked): (String, Option<i64>) = sqlx::query_as(
        "SELECT status, run_id FROM job_nodes
         WHERE job_id = ? AND node_id = 'triage' AND ordinal IS NULL AND round = 0",
    )
    .bind(job.id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(status, "running");
    assert_eq!(linked, Some(run_id));
}

#[tokio::test(flavor = "current_thread")]
async fn runs_item_id_stays_null_for_a_singleton_node_run() {
    let (state, _job, run_id, _guards) = dispatch_triage_for_real().await;
    let item_id: Option<i64> = sqlx::query_scalar("SELECT item_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(item_id, None);
}

// ---- landing, output validation, crash -----------------------------------------------------------

/// An answer is valid when it says `OK`.
struct NeedsOk;

impl OutputCheck for NeedsOk {
    fn check(&self, text: &str) -> Result<(), String> {
        if text.contains("OK") {
            Ok(())
        } else {
            Err("the answer lacks the word OK".to_string())
        }
    }
}

/// The default protocol, plus a check every answer must pass.
struct Strict;

#[async_trait::async_trait]
impl AgentNodeLauncher for Strict {
    async fn launch(
        &self,
        _ctx: &NodeCtx<'_>,
        protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused> {
        Ok(NodeLaunch {
            output: Some(Arc::new(NeedsOk)),
            ..NodeLaunch::plain(protocol)
        })
    }

    async fn output_check(&self, _ctx: &NodeCtx<'_>) -> Option<Arc<dyn OutputCheck>> {
        Some(Arc::new(NeedsOk))
    }
}

#[test]
fn the_verdict_retries_once_then_fails() {
    use engine::{Landing, validation_verdict};
    assert_eq!(validation_verdict(0, Ok(())), Landing::Pass);
    assert_eq!(validation_verdict(1, Ok(())), Landing::Pass);
    assert_eq!(
        validation_verdict(0, Err("r".into())),
        Landing::Retry("r".into())
    );
    assert_eq!(
        validation_verdict(1, Err("r".into())),
        Landing::Fail("r".into())
    );
    assert_eq!(
        validation_verdict(5, Err("r".into())),
        Landing::Fail("r".into())
    );
}

#[test]
fn the_retry_is_pinned_to_the_refused_runs_engine_and_model_with_the_report_appended() {
    let first = NodeLaunch::plain("ORIGINAL".into());
    let retry = engine::retried_launch(
        first,
        Some("claude"),
        Some("m1".into()),
        Some("high".into()),
        "missing OK",
    );
    assert_eq!(
        retry.pin,
        Some(EnginePin {
            runner: RunnerKind::Claude,
            model: Some("m1".into()),
            effort: Some("high".into()),
            sandbox: None,
            web_search: None,
        })
    );
    assert!(retry.prompt.starts_with("ORIGINAL\n\n"), "{}", retry.prompt);
    assert!(
        retry
            .prompt
            .contains("refused by the output validator:\nmissing OK"),
        "{}",
        retry.prompt
    );
    // A run with no recorded engine leaves the launcher's own pin alone.
    let unpinned = engine::retried_launch(NodeLaunch::plain("P".into()), None, None, None, "r");
    assert_eq!(unpinned.pin, None);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_output_is_retried_once_on_the_same_engine_and_model_with_the_report_appended() {
    let (state, job, first_run, _guards) = dispatch_triage_for_real().await;
    // The run ends with an invalid answer; a restarted daemon holds no task for it.
    if let Some(task) = state
        .run_handles
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&first_run)
    {
        task.abort();
    }
    sqlx::query(
        "UPDATE runs SET status = 'completed', stdout = 'no verdict here', runner = 'claude',
                model = 'm1', effort = 'high' WHERE id = ?",
    )
    .bind(first_run)
    .execute(&state.pool)
    .await
    .unwrap();

    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();

    let (status, linked, attempts): (String, Option<i64>, i64) = sqlx::query_as(
        "SELECT status, run_id, gate_attempts FROM job_nodes
         WHERE job_id = ? AND node_id = 'triage' AND ordinal IS NULL",
    )
    .bind(job.id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        status, "running",
        "the node is not landed while its retry runs"
    );
    assert_eq!(attempts, 1);
    let second_run = linked.expect("the row names the retry's run");
    assert_ne!(second_run, first_run, "a second run was started");
    let (stage, prompt): (String, String) =
        sqlx::query_as("SELECT stage, prompt FROM runs WHERE id = ?")
            .bind(second_run)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stage, "triage");
    assert!(
        prompt.contains("refused by the output validator:\nthe answer lacks the word OK"),
        "{prompt}"
    );
    assert!(
        feed_kinds(&state.pool)
            .await
            .contains(&"job_node_retried".to_string())
    );
}

#[tokio::test]
async fn a_second_invalid_output_fails_the_node_and_the_router_hears_fail() {
    let (state, job, _run, _dir) = triage_ended("completed", "still nothing", 1).await;
    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();

    assert_eq!(triage_status(&state.pool, job.id).await, "failed");
    assert_eq!(count(&state.pool, "runs").await, 1, "no third attempt");
    let said = feed_summary_of(&state.pool, "job_node_invalid_output").await;
    assert!(
        said.contains("triage") && said.contains("lacks the word OK"),
        "{said}"
    );
    // The `fail` report itself needs a router stub to observe; the Fail arm is the only one that
    // sends it after a second refusal, and the verdict table above pins that arm.
}

#[tokio::test]
async fn valid_output_reports_pass_after_validation_not_at_run_end() {
    use crate::models::route_advice::{RouteQuery, judged_by_a_job_gate, outcome_at_run_end};
    // At run end the router stays silent for a run whose answer will be validated...
    let mut query = RouteQuery::plain("t".into(), Some("triage".into()), None, false);
    query.judged_later = true;
    assert!(judged_by_a_job_gate(&query));
    assert_eq!(
        outcome_at_run_end("completed", 0, "OK", None, judged_by_a_job_gate(&query)),
        None
    );

    // ...and the node is `passed` only once the validator has seen the answer.
    let (state, job, _run, _dir) = triage_ended("completed", "all OK", 0).await;
    assert_eq!(triage_status(&state.pool, job.id).await, "running");
    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "passed");
    assert_eq!(count(&state.pool, "runs").await, 1);
}

/// Passes only an answer that BEGINS with "OK", which a raw stream-json line never does.
struct OpensWithOk;

impl OutputCheck for OpensWithOk {
    fn check(&self, text: &str) -> Result<(), String> {
        if text.starts_with("OK") {
            Ok(())
        } else {
            Err("the answer does not open with OK".to_string())
        }
    }
}

struct StrictOpening;

#[async_trait::async_trait]
impl AgentNodeLauncher for StrictOpening {
    async fn launch(
        &self,
        _ctx: &NodeCtx<'_>,
        protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused> {
        Ok(NodeLaunch {
            output: Some(Arc::new(OpensWithOk)),
            ..NodeLaunch::plain(protocol)
        })
    }

    async fn output_check(&self, _ctx: &NodeCtx<'_>) -> Option<Arc<dyn OutputCheck>> {
        Some(Arc::new(OpensWithOk))
    }
}

/// Measured on job 30, run 900727, 2026-10-21: the researcher's answer had every section
/// `output.md` asks for, and the validator was handed the whole `claude -p` stream-json, where
/// `## Synthesis` sits inside a JSON string and never at the start of a line. The check reads the
/// run's reply, as `wire::extract_reply` gives it, not the stream.
#[tokio::test]
async fn the_output_check_reads_the_runs_reply_not_its_stream_json() {
    let stream = concat!(
        r#"{"type":"system","subtype":"init","session_id":"s"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":false,"result":"OK: the answer"}"#,
        "\n",
    );
    let (state, job, _run, _dir) = triage_ended("completed", stream, 0).await;
    engine::land_nodes_with(&StrictOpening, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "passed");
    assert_eq!(count(&state.pool, "runs").await, 1, "no retry");
}

#[tokio::test]
async fn output_is_validated_before_the_emission_is_read() {
    // An invalid answer beside an emission the facts reader would refuse (a nested value).
    let (state, job, _run, dir) = triage_ended("completed", "nope", 1).await;
    std::fs::write(
        dir.path().join(".nucleos").join("triage.json"),
        r#"{"size":{"nested":true}}"#,
    )
    .unwrap();
    let graph = graph_of(TRIAGE);

    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "failed");
    // A failed node's emission is never read: building the facts succeeds.
    engine::build_facts(&state.pool, &job, dir.path(), &graph)
        .await
        .expect("the emission of a failed node is not read");

    // Behind a VALID answer of a checked node the daemon replaces the file with the facts it
    // derives from the reply (none for `Strict`), so the agent's nested value is gone.
    let (state, job, _run, dir) = triage_ended("completed", "OK", 0).await;
    let emission = dir.path().join(".nucleos").join("triage.json");
    std::fs::write(&emission, r#"{"size":{"nested":true}}"#).unwrap();
    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "passed");
    assert_eq!(std::fs::read_to_string(&emission).unwrap(), "{}");
    engine::build_facts(&state.pool, &job, dir.path(), &graph)
        .await
        .expect("the daemon's emission replaced the agent's nested one");
}

/// Approves and derives `verdict: approved`, as a reviewer's `## Verdict` one-of would.
struct EmitsVerdict;

impl OutputCheck for EmitsVerdict {
    fn check(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn facts(&self, _reply: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut facts = serde_json::Map::new();
        facts.insert("verdict".into(), "approved".into());
        facts
    }
}

struct EmitsVerdictLauncher;

#[async_trait::async_trait]
impl AgentNodeLauncher for EmitsVerdictLauncher {
    async fn launch(
        &self,
        _ctx: &NodeCtx<'_>,
        protocol: String,
    ) -> Result<NodeLaunch, LaunchRefused> {
        Ok(NodeLaunch {
            output: Some(Arc::new(EmitsVerdict)),
            ..NodeLaunch::plain(protocol)
        })
    }

    async fn output_check(&self, _ctx: &NodeCtx<'_>) -> Option<Arc<dyn OutputCheck>> {
        Some(Arc::new(EmitsVerdict))
    }
}

#[tokio::test]
async fn the_daemon_writes_the_emission_from_the_validated_reply() {
    let (state, job, _run, dir) = triage_ended("completed", "whatever", 0).await;
    engine::land_nodes_with(&EmitsVerdictLauncher, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "passed");
    let emission = dir.path().join(".nucleos").join("triage.json");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&emission).unwrap()).unwrap();
    assert_eq!(written, serde_json::json!({"verdict": "approved"}));
    let facts = engine::build_facts(&state.pool, &job, dir.path(), &graph_of(TRIAGE))
        .await
        .unwrap();
    assert_eq!(facts.get("triage.verdict"), Some("approved"));
}

#[tokio::test]
async fn a_node_whose_run_did_not_complete_fails_without_validation() {
    let (state, job, _run, _dir) = triage_ended("failed", "OK", 0).await;
    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "failed");
    assert!(
        feed_kinds(&state.pool)
            .await
            .contains(&"job_node_failed".to_string())
    );
    assert_eq!(count(&state.pool, "runs").await, 1);
}

#[tokio::test]
async fn a_node_without_an_output_check_passes_when_its_run_completes() {
    let (state, job, _run, _dir) = triage_ended("completed", "anything", 0).await;
    engine::land_nodes_with(&ProtocolAndBody, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "passed");
}

#[tokio::test]
async fn the_autopilots_own_nodes_are_left_to_the_legacy_landing() {
    let (state, job, _dir) = job_in_a_directory(None).await;
    let run_id = seed_node(&state.pool, job.id, "review", "completed").await;
    insert_singleton_node(&state.pool, job.id, "review", 0, "running")
        .await
        .unwrap();
    sqlx::query("UPDATE job_nodes SET run_id = ? WHERE job_id = ? AND node_id = 'review'")
        .bind(run_id)
        .bind(job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();
    assert_eq!(
        singleton_status(&state.pool, job.id, "review", 0)
            .await
            .unwrap()
            .as_deref(),
        Some("running"),
        "land_nodes does not touch spec/plan/review/replan"
    );
    // And the item landing is not tripped by a singleton row that carries a run.
    reconcile_nodes(&state, &job)
        .await
        .expect("a singleton row with a run is not an item");
}

#[tokio::test]
async fn a_custom_agent_node_asking_permission_parks_the_job_awaiting_approval() {
    // The hook gives up only on `NODES_THAT_GIVE_UP` (review, replan) and on an item's node
    // (hooks/denials.rs); any other stage, `triage` included, falls through to
    // `create_action_approval` and parks as `awaiting_approval`. The job then waits on the run.
    let graph = graph_of(TRIAGE);
    let (state, job, _dir) = job_in_a_directory(Some(&graph)).await;
    let run_id = seed_node(&state.pool, job.id, "triage", "awaiting_approval").await;
    insert_singleton_node(&state.pool, job.id, "triage", 0, "running")
        .await
        .unwrap();
    sqlx::query("UPDATE job_nodes SET run_id = ? WHERE job_id = ? AND node_id = 'triage'")
        .bind(run_id)
        .bind(job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(node_awaiting_approval(&state.pool, job.id).await.unwrap());

    let pass = advance_with(&state, &job, Utc::now(), true).await;
    assert_eq!(pass, Pass::Stopped);
    assert_eq!(job_status(&state.pool, job.id).await, "running");
    assert_eq!(
        count(&state.pool, "runs").await,
        1,
        "nothing else is started behind it"
    );
    // Landing leaves the node alone while its run is parked.
    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();
    assert_eq!(triage_status(&state.pool, job.id).await, "running");
}

// `a_replan_that_fails_stops_not_fails_the_job` is already covered by
// `a_replan_that_could_not_answer_stops_the_job_rather_than_failing_it` (job.rs) and
// `a_replan_that_writes_nothing_stops_the_job_and_does_not_reuse_the_last_plan` (tests_plan_landing.rs).

#[tokio::test]
async fn crash_between_node_done_and_next_dispatch_resumes_without_rerunning_the_node() {
    let graph = graph_of(
        "nodes:
  - {id: triage, type: agent}
  - {id: build, type: agent}
edges:
  - {from: triage, to: build}
",
    );
    let (state, job, dir) = job_in_a_directory(Some(&graph)).await;
    // The daemon died after `triage` was written `passed` and before `build` was dispatched.
    insert_singleton_node(&state.pool, job.id, "triage", 0, "passed")
        .await
        .unwrap();
    std::fs::write(
        dir.path().join(".nucleos").join("triage.json"),
        r#"{"size":"small"}"#,
    )
    .unwrap();

    let (step, _, resolved) = engine::decide(&state, &job).await.unwrap();
    assert_eq!(
        step,
        Step::RunNode {
            node_id: "build".into(),
            item: None
        }
    );
    assert_eq!(
        engine::action_of(&step, &resolved.graph),
        Action::DispatchAgent {
            node_id: "build".into(),
            ordinal: None
        }
    );
    // A pass of the driver before any dispatch lands nothing and re-runs nothing.
    engine::land_nodes_with(&Strict, &state, &job)
        .await
        .unwrap();
    assert_eq!(count(&state.pool, "runs").await, 0);
    assert_eq!(triage_status(&state.pool, job.id).await, "passed");
}

#[tokio::test]
async fn a_singleton_row_left_running_with_no_run_is_released_for_a_new_dispatch() {
    let graph = graph_of(TRIAGE);
    let (state, job, _dir) = job_in_a_directory(Some(&graph)).await;
    insert_singleton_node(&state.pool, job.id, "triage", 0, "running")
        .await
        .unwrap();
    engine::land_nodes_with(&ProtocolAndBody, &state, &job)
        .await
        .unwrap();
    assert_eq!(
        singleton_status(&state.pool, job.id, "triage", 0)
            .await
            .unwrap(),
        None,
        "the walker reads the node as not entered and dispatches it again"
    );
    let (step, _, _) = engine::decide(&state, &job).await.unwrap();
    assert_eq!(
        step,
        Step::RunNode {
            node_id: "triage".into(),
            item: None
        }
    );
}

#[tokio::test]
async fn a_completed_spec_or_review_run_leaves_a_passed_row_so_its_facts_publish() {
    let (state, job, dir) = job_in_a_directory(None).await;
    seed_node(&state.pool, job.id, "spec", "completed").await;
    seed_node(&state.pool, job.id, "review", "running").await;
    engine::land_nodes_with(&ProtocolAndBody, &state, &job)
        .await
        .unwrap();
    let status = |node: &'static str| {
        let pool = state.pool.clone();
        let round = job.round;
        let job_id = job.id;
        async move {
            singleton_status(&pool, job_id, node, round)
                .await
                .unwrap()
                .as_deref()
                .map(str::to_owned)
        }
    };
    assert_eq!(status("spec").await.as_deref(), Some("passed"));
    assert_eq!(
        status("review").await,
        None,
        "a review in flight is not done"
    );

    seed_node(&state.pool, job.id, "review", "completed").await;
    engine::land_nodes_with(&ProtocolAndBody, &state, &job)
        .await
        .unwrap();
    assert_eq!(status("review").await.as_deref(), Some("passed"));

    std::fs::write(
        dir.path().join(".nucleos").join("review.json"),
        r#"{"verdict":"ok"}"#,
    )
    .unwrap();
    let graph = engine::load_graph(&state.pool, job.id).await.unwrap();
    let facts = engine::build_facts(&state.pool, &job, dir.path(), &graph)
        .await
        .unwrap();
    assert_eq!(facts.get("review.verdict"), Some("ok"));
}

#[tokio::test]
async fn an_autopilot_node_has_its_stale_emission_cleared_before_it_runs() {
    let (state, job, dir) = job_in_a_directory(None).await;
    let stale = dir.path().join(".nucleos").join("spec.json");
    std::fs::write(&stale, r#"{"old":"round one"}"#).unwrap();
    let graph = engine::load_graph(&state.pool, job.id).await.unwrap();
    let view = load_view(&state.pool, job.id).await.unwrap();
    let facts = Facts::new();
    engine::dispatch_agent_node_with(
        &Refusing,
        &state,
        &job,
        AgentTarget {
            view: &view,
            facts: &facts,
            graph: &graph,
            node_id: "spec",
            replan: false,
        },
        (dir.path().to_path_buf(), "nucleos/job".to_string()),
    )
    .await;
    assert!(
        !stale.exists(),
        "the previous visit's file must not pass for this one's"
    );
}
