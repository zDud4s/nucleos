//! DB-level tests of the walker as the authority: `advance_with`, how a job ends (completed, stuck,
//! unreadable emission), the brakes, loops and jobs started before the upgrade.

use super::drive::advance_with;
use super::engine::{self, Action};
use super::tests::{job_status, seed_node, write_plan};
use super::tests_job_support::{
    LOOP, count, feed_kinds, feed_summary_of, graph_of, job_in_a_directory, planned_autopilot,
    queue_item,
};
use super::*;
use crate::workflows::walk::Step;

// ---- the walker drives --------------------------------------------------------------------------

/// The finishing leg of the golden run (spec -> plan -> items -> gate -> review -> `more` false ->
/// finished), seeded rather than spawned: a run needs a git repository, and what is compared here
/// is the SAME state read by both drivers. The spawning legs are covered by the dispatch tests.
async fn finish_with(walker_drives: bool) -> (String, Vec<String>) {
    let (state, job, _dir) = planned_autopilot(&["passed"]).await;
    seed_node(&state.pool, job.id, "review", "completed").await;
    let pass = advance_with(&state, &job, Utc::now(), walker_drives).await;
    assert_eq!(pass, Pass::Stopped);
    (
        job_status(&state.pool, job.id).await,
        feed_kinds(&state.pool).await,
    )
}

#[tokio::test]
async fn the_seeded_autopilot_drives_a_job_to_completed_with_the_same_feed_kinds_as_the_legacy_driver()
 {
    let (legacy_status, legacy_feed) = finish_with(false).await;
    let (walker_status, walker_feed) = finish_with(true).await;
    assert_eq!(legacy_status, "completed");
    assert_eq!(walker_status, "completed");
    assert_eq!(walker_feed, legacy_feed);
    assert_eq!(walker_feed, vec!["job_finished".to_string()]);
}

#[tokio::test]
async fn a_decision_with_an_unpublished_key_stops_the_job_naming_node_and_key() {
    let graph = graph_of(
        "nodes:
  - {id: triage, type: agent}
  - {id: pick, type: decision, condition: 'triage.size != trivial'}
  - {id: build, type: agent}
edges:
  - {from: triage, to: pick}
  - {from: pick, to: build, when: true}
",
    );
    let (state, job, dir) = job_in_a_directory(Some(&graph)).await;
    insert_singleton_node(&state.pool, job.id, "triage", 0, "passed")
        .await
        .unwrap();

    // The node wrote no file at all.
    let pass = advance_with(&state, &job, Utc::now(), true).await;
    assert_eq!(pass, Pass::Stopped);
    assert_eq!(job_status(&state.pool, job.id).await, "stopped");
    let said = feed_summary_of(&state.pool, "job_graph_stuck").await;
    assert!(
        said.contains("pick") && said.contains("triage.size"),
        "{said}"
    );
    assert!(said.contains("wrote no file"), "{said}");
    assert_eq!(count(&state.pool, "runs").await, 0);

    // It wrote a file, without the key.
    let (state, job, dir2) = job_in_a_directory(Some(&graph)).await;
    insert_singleton_node(&state.pool, job.id, "triage", 0, "passed")
        .await
        .unwrap();
    std::fs::write(
        dir2.path().join(".nucleos").join("triage.json"),
        r#"{"other":"x"}"#,
    )
    .unwrap();
    advance_with(&state, &job, Utc::now(), true).await;
    let said = feed_summary_of(&state.pool, "job_graph_stuck").await;
    assert!(said.contains("wrote a file without it"), "{said}");
    drop(dir);
}

#[tokio::test]
async fn a_disabled_node_is_not_dispatched_and_its_edges_are_followed() {
    let mut graph = graph_of(
        "nodes:
  - {id: lint, type: command, command: 'true'}
  - {id: build, type: agent}
edges:
  - {from: lint, to: build}
",
    );
    let (state, job, _dir) = job_in_a_directory(Some(&graph)).await;
    let (step, _, _) = engine::decide(&state, &job).await.unwrap();
    assert_eq!(
        step,
        Step::RunNode {
            node_id: "lint".into(),
            item: None
        }
    );

    graph.disabled.insert("lint".to_string());
    let (state, job, _dir) = job_in_a_directory(Some(&graph)).await;
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
    let rows = nodes_of(&state.pool, job.id).await.unwrap();
    assert!(
        rows.iter().all(|row| row.node_id != "lint"),
        "a disabled node leaves no row"
    );
}

#[tokio::test]
async fn a_fan_starts_no_more_instances_than_its_ceiling() {
    // No team: one at a time whatever the graph declares (`concurrency: 8`).
    let (state, job, _dir) = planned_autopilot(&["pending", "pending", "pending"]).await;
    let (step, _, graph) = engine::decide(&state, &job).await.unwrap();
    let Action::FanOut { items, .. } = engine::action_of(&step, &graph.graph) else {
        panic!("a queue with work owed fans out, got {step:?}");
    };
    assert_eq!(items.len(), 1, "{items:?}");
}

// ---- brakes -------------------------------------------------------------------------------------

#[tokio::test]
async fn an_agent_node_still_obeys_every_brake_and_a_command_node_only_the_kill_switch() {
    // An allowance that is already spent stops anything that would START a run...
    let agent_graph = graph_of("nodes:\n  - {id: triage, type: agent}\nedges: []\n");
    let (state, job, _dir) = job_in_a_directory(Some(&agent_graph)).await;
    sqlx::query("UPDATE jobs SET budget_usd = -1.0 WHERE id = ?")
        .bind(job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let job = load_job(&state.pool, job.id).await.unwrap();
    advance_with(&state, &job, Utc::now(), true).await;
    assert_eq!(job_status(&state.pool, job.id).await, "stopped");
    assert!(
        feed_kinds(&state.pool)
            .await
            .contains(&"job_stopped".to_string())
    );
    assert_eq!(count(&state.pool, "runs").await, 0);

    // ...and a command node under the very same allowance runs: it is not a run.
    let command_graph =
        graph_of("nodes:\n  - {id: lint, type: command, command: 'git --version'}\nedges: []\n");
    let (state, job, _dir) = job_in_a_directory(Some(&command_graph)).await;
    sqlx::query("UPDATE jobs SET budget_usd = -1.0 WHERE id = ?")
        .bind(job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let job = load_job(&state.pool, job.id).await.unwrap();
    let pass = advance_with(&state, &job, Utc::now(), true).await;
    assert_eq!(pass, Pass::Continued);
    assert_eq!(
        singleton_status(&state.pool, job.id, "lint", 0)
            .await
            .unwrap()
            .as_deref(),
        Some("passed")
    );
    assert_eq!(job_status(&state.pool, job.id).await, "running");

    // The kill switch holds both.
    let (state2, job2, _dir2) = job_in_a_directory(Some(&command_graph)).await;
    crate::governance::autopilot::set_kill_switch(&state2.pool, true)
        .await
        .unwrap();
    advance_with(&state2, &job2, Utc::now(), true).await;
    assert_eq!(job_status(&state2.pool, job2.id).await, "waiting");
    assert_eq!(
        singleton_status(&state2.pool, job2.id, "lint", 0)
            .await
            .unwrap(),
        None
    );
}

// ---- pure ---------------------------------------------------------------------------------------

#[test]
fn the_autopilots_own_nodes_are_the_only_ones_with_a_hard_coded_protocol() {
    for stage in ["spec", "plan", "review", "replan"] {
        assert!(engine::is_autopilot_stage(stage), "{stage}");
    }
    for stage in ["triage", "implement", "gate"] {
        assert!(!engine::is_autopilot_stage(stage), "{stage}");
    }
}

#[test]
fn the_keys_a_workflow_reads_from_a_node_are_the_ones_its_decisions_name() {
    let graph = graph_of(
        "nodes:
  - {id: triage, type: agent}
  - {id: a, type: decision, condition: 'triage.size != trivial and triage.risky == false'}
  - {id: b, type: decision, condition: 'plan.done != true'}
  - {id: c, type: decision, condition: 'triage.size in [small, large]'}
edges: []
",
    );
    assert_eq!(
        engine::keys_read_from(&graph.graph, "triage"),
        vec!["risky".to_string(), "size".to_string()]
    );
    assert_eq!(
        engine::keys_read_from(&graph.graph, "plan"),
        vec!["done".to_string()]
    );
    assert!(engine::keys_read_from(&graph.graph, "nowhere").is_empty());
}

#[tokio::test]
async fn a_job_started_before_the_upgrade_finishes_on_the_embedded_autopilot() {
    // No snapshot (NULL = the embedded autopilot), a plan on disk and its items queued, but no
    // `plan` singleton row: only the legacy tables describe the job.
    let (state, job, dir) = job_in_a_directory(None).await;
    write_plan(dir.path(), r#"{"items":[{"description":"x"}]}"#).await;
    seed_node(&state.pool, job.id, "spec", "completed").await;
    queue_item(&state.pool, job.id, 0, "pending").await;

    let (step, _, graph) = engine::decide(&state, &job).await.unwrap();
    let Action::FanOut { items, .. } = engine::action_of(&step, &graph.graph) else {
        panic!("plan counts as done, so the queue fans out, got {step:?}");
    };
    assert_eq!(items.len(), 1);
    assert_eq!(
        count(&state.pool, "runs").await,
        1,
        "only the seeded spec run; the plan is not run again"
    );

    // Finishing leg: everything passed and reviewed -> completed, as for any autopilot job.
    sqlx::query("UPDATE job_nodes SET status = 'passed' WHERE job_id = ? AND ordinal IS NOT NULL")
        .bind(job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    seed_node(&state.pool, job.id, "review", "completed").await;
    advance_with(&state, &job, Utc::now(), true).await;
    assert_eq!(job_status(&state.pool, job.id).await, "completed");
}

#[tokio::test]
async fn an_unreadable_emission_of_a_finished_node_ends_the_job_instead_of_parking_it() {
    let graph = graph_of(LOOP);
    let (state, job, dir) = job_in_a_directory(Some(&graph)).await;
    insert_singleton_node(&state.pool, job.id, "work", 0, "passed")
        .await
        .unwrap();
    std::fs::write(
        dir.path().join(".nucleos").join("work.json"),
        r#"{"size":null}"#,
    )
    .unwrap();
    let pass = advance_with(&state, &job, Utc::now(), true).await;
    assert_eq!(pass, Pass::Stopped);
    assert_eq!(job_status(&state.pool, job.id).await, "stopped");
    let said = feed_summary_of(&state.pool, "job_graph_unreadable").await;
    assert!(said.contains("work"), "{said}");
    assert_eq!(count(&state.pool, "runs").await, 0);
}

#[test]
fn the_planners_file_is_read_for_its_scalars_and_nothing_else() {
    let extra = br#"{"items":[{"description":"x"}],"summary":{"a":1},"note":null,"done":true}"#;
    let got = engine::plan_emission(Some(extra)).expect("extra keys do not stall the job");
    assert_eq!(engine::plan_done_fact(&got), Some("true"));
    // A queue over the flat-emission ceiling is still a queue.
    let items = vec!["{\"description\":\"x\"}"; 4000].join(",");
    let big = format!(r#"{{"items":[{items}],"done":false}}"#);
    assert!(big.len() > crate::workflows::facts::MAX_EMISSION_BYTES);
    let got = engine::plan_emission(Some(big.as_bytes())).expect("a big queue is not refused");
    assert_eq!(engine::plan_done_fact(&got), Some("false"));
    // What is not a JSON object at all is still refused.
    assert!(engine::plan_emission(Some(b"")).is_err());
    assert!(engine::plan_emission(Some(b"[1]")).is_err());
}

#[tokio::test]
async fn a_return_edge_into_a_node_that_is_not_the_planner_loops_without_replan_semantics() {
    let graph = graph_of(LOOP);
    let (state, job, dir) = job_in_a_directory(Some(&graph)).await;
    sqlx::query("UPDATE jobs SET max_rounds = 3 WHERE id = ?")
        .bind(job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    insert_singleton_node(&state.pool, job.id, "work", 0, "passed")
        .await
        .unwrap();
    std::fs::write(
        dir.path().join(".nucleos").join("work.json"),
        r#"{"size":"more"}"#,
    )
    .unwrap();
    let job = load_job(&state.pool, job.id).await.unwrap();
    let (step, _, _) = engine::decide(&state, &job).await.unwrap();
    assert_eq!(
        step,
        Step::Replan {
            node_id: "work".into()
        }
    );

    let pass = advance_with(&state, &job, Utc::now(), true).await;
    assert_eq!(pass, Pass::Continued);
    let round: i64 = sqlx::query_scalar("SELECT round FROM jobs WHERE id = ?")
        .bind(job.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(round, 1, "the loop opens the next round");
    assert_eq!(
        count(&state.pool, "runs").await,
        0,
        "no replan run: `work` is not the planner"
    );
}
