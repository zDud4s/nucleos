//! Tests of the decision runs in `auto_default.rs` (owner decision 2026-11-13).

use super::*;
use crate::tree_job::auto_resolve;
use crate::tree_job::model::Manifest;
use crate::tree_job::store::TreeJob;

#[test]
fn owner_only_questions_are_recognised_from_their_wording() {
    let budget = "Node n1 was not started again: the tree budget does not permit it (x). \
                  Raise the budget, or say how the node should go on.";
    let ticket = "Ticket #5 k was launched twice and nobody fixed it. Pick an owner or close it.";
    assert_eq!(classify(budget), "budget_raise");
    assert_eq!(classify(ticket), "ticket_ownership");
    assert_eq!(classify("May I edit scripts/gates.sh?"), "governance_file");
    assert_eq!(classify("Edit nucleos.tests.yaml?"), "governance_file");
    assert_eq!(
        classify("Widen the declared paths to core/?"),
        "widen_paths"
    );
    for kind in [
        "governance_file",
        "widen_paths",
        "budget_raise",
        "ticket_ownership",
    ] {
        assert!(auto_resolve::is_owner_only(kind), "{kind}");
    }
}

#[test]
fn everything_else_is_ordinary_including_a_stalled_tree() {
    assert_eq!(classify("which way?"), "ordinary");
    assert_eq!(
        classify("The tree has not moved for 30 minutes: nothing runs. What should it do?"),
        "ordinary"
    );
    assert!(!auto_resolve::is_owner_only("ordinary"));
}

#[test]
fn the_prompt_carries_mandate_question_and_state() {
    let text = decision_prompt("Ship B", "n7", "b1 is parked", "which way?");
    for part in [MANDATE, "Ship B", "n7", "b1 is parked", "which way?"] {
        assert!(text.contains(part), "{part}");
    }
}

#[test]
fn only_a_completed_run_with_text_answers() {
    assert_eq!(
        answer_of("completed", Some("  go left \n")).as_deref(),
        Some("go left")
    );
    assert_eq!(answer_of("completed", Some("  ")), None);
    assert_eq!(answer_of("completed", None), None);
    assert_eq!(answer_of("failed", Some("go left")), None);
    let long = "x".repeat(MAX_ANSWER_CHARS + 50);
    assert_eq!(
        answer_of("completed", Some(&long)).unwrap().chars().count(),
        MAX_ANSWER_CHARS
    );
}

// ----- database -----

const TREE: &str = "dr-job";
const NODE: &str = "dr-node";
const AT: &str = "2020-01-01T00:00:00+00:00";

async fn state() -> AppState {
    let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
        Some(std::time::Duration::from_secs(60)),
        std::time::Duration::from_secs(600),
    )
    .await;
    state
}

async fn seed_job(pool: &SqlitePool) {
    let manifest = Manifest::new(TREE, "feat/dr", "Decide it", "C:/nowhere", &[]);
    let job = TreeJob {
        id: TREE.to_owned(),
        root_node_id: "dr-root".to_owned(),
        project_id: None,
        feature_branch: Some("feat/dr".to_owned()),
        goal: "Decide it".to_owned(),
        state: "open".to_owned(),
        manifest,
        contract: None,
        created_at: AT.to_owned(),
        updated_at: AT.to_owned(),
    };
    store::save(pool, &job).await.unwrap();
}

async fn seed_open(pool: &SqlitePool, id: &str, question: &str) {
    sqlx::query(
        "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
         VALUES (?, ?, ?, ?, NULL, 'agent', ?, 'open')",
    )
    .bind(id)
    .bind(TREE)
    .bind(NODE)
    .bind(question)
    .bind(AT)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_transition(pool: &SqlitePool, id: &str, result: &str) {
    sqlx::query(
        "INSERT INTO tree_transitions (node_id, transition, key, result, created_at)
         VALUES (?, 'decision_run', ?, ?, ?)",
    )
    .bind(NODE)
    .bind(id)
    .bind(result)
    .bind(AT)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_run(pool: &SqlitePool, status: &str, stdout: Option<&str>) -> i64 {
    sqlx::query(
        "INSERT INTO runs (prompt, status, stdout, mode, created_at)
         VALUES ('p', ?, ?, 'shadow', ?)",
    )
    .bind(status)
    .bind(stdout)
    .bind(AT)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

async fn row(pool: &SqlitePool, id: &str) -> (String, Option<String>, Option<String>) {
    sqlx::query_as("SELECT status, answer, origin FROM decisions WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_finished_decision_run_is_applied_as_the_answer_at_once() {
    let state = state().await;
    seed_job(&state.pool).await;
    seed_open(&state.pool, "d-1", "which way?").await;
    let run = seed_run(&state.pool, "completed", Some("take the left branch")).await;
    seed_transition(&state.pool, "d-1", &run.to_string()).await;

    decision_run_tick(&state).await;

    let (status, answer, origin) = row(&state.pool, "d-1").await;
    assert_eq!(status, "answered");
    assert_eq!(answer.as_deref(), Some("take the left branch"));
    assert_eq!(origin.as_deref(), Some("auto"));
}

#[tokio::test]
async fn a_failed_decision_run_leaves_the_question_with_the_owner_for_good() {
    let state = state().await;
    seed_job(&state.pool).await;
    seed_open(&state.pool, "d-1", "which way?").await;
    let run = seed_run(&state.pool, "failed", Some("boom")).await;
    seed_transition(&state.pool, "d-1", &run.to_string()).await;

    decision_run_tick(&state).await;
    decision_run_tick(&state).await;

    assert_eq!(row(&state.pool, "d-1").await.0, "open");
    let result = run_recorded(&state.pool, NODE, "d-1").await.unwrap();
    assert_eq!(result.as_deref(), Some(RUN_FAILED));
}

#[tokio::test]
async fn an_owner_only_question_gets_no_run() {
    let state = state().await;
    seed_job(&state.pool).await;
    seed_open(
        &state.pool,
        "d-1",
        "Raise the budget, or say how the node should go on.",
    )
    .await;

    decision_run_tick(&state).await;

    assert_eq!(run_recorded(&state.pool, NODE, "d-1").await.unwrap(), None);
    assert_eq!(row(&state.pool, "d-1").await.0, "open");
}

#[tokio::test]
async fn spent_caps_start_no_run_and_runs_in_flight_count() {
    let state = state().await;
    seed_job(&state.pool).await;
    // Three runs in flight on the node: the per-node default cap is spent.
    for n in 0..3 {
        let id = format!("d-{n}");
        seed_open(&state.pool, &id, "which way?").await;
        let run = seed_run(&state.pool, "running", None).await;
        seed_transition(&state.pool, &id, &run.to_string()).await;
    }
    assert_eq!(runs_spent(&state.pool, "node_id", NODE).await.unwrap(), 3);
    seed_open(&state.pool, "d-9", "which way?").await;

    decision_run_tick(&state).await;

    assert_eq!(run_recorded(&state.pool, NODE, "d-9").await.unwrap(), None);
    assert_eq!(row(&state.pool, "d-9").await.0, "open");
}

#[tokio::test]
async fn a_run_still_live_is_left_alone() {
    let state = state().await;
    seed_job(&state.pool).await;
    seed_open(&state.pool, "d-1", "which way?").await;
    let run = seed_run(&state.pool, "running", None).await;
    seed_transition(&state.pool, "d-1", &run.to_string()).await;

    decision_run_tick(&state).await;

    assert_eq!(row(&state.pool, "d-1").await.0, "open");
    let result = run_recorded(&state.pool, NODE, "d-1").await.unwrap();
    assert_eq!(result, Some(run.to_string()));
}
