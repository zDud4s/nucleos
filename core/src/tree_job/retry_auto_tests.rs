//! Tests of the parks the driver settles itself (`retry::classify_park`, `resolve_open_parks`).

use super::*;
use crate::tree_job::model::Manifest;

#[test]
fn a_dropped_base_is_a_stale_base() {
    let q = "Branch b is green but stands on a, which was dropped, so it can never merge; how should it go on?";
    assert_eq!(classify_park(q), Some(AutoCause::StaleBase));
}

#[test]
fn a_fence_refusal_is_ours_only_when_the_base_owns_the_files() {
    let plain = "Branch b cannot merge into feat/x: it changed files outside its declared paths [core/a/**]: core/b.rs. Move that change to the branch that owns those paths, or have the parent widen this branch's paths.";
    assert_eq!(classify_park(plain), None);
    let marked = format!("{plain} {FROM_BASE_MARK}");
    assert_eq!(classify_park(&marked), Some(AutoCause::FenceFromBase));
    let both = format!("{marked} and it adds a migration whose number the parent already has");
    assert_eq!(classify_park(&both), None);
}

#[test]
fn a_gate_that_never_measured_is_ours_only_for_infrastructure() {
    let head =
        "The gate of branch b ended 3 times in a row without measuring anything; last reason: ";
    for reason in [
        "pool timeout while waiting for a target dir",
        "database is locked",
        "the verify queue timed out",
    ] {
        assert_eq!(
            classify_park(&format!("{head}{reason}")),
            Some(AutoCause::GateInfrastructure),
            "{reason}"
        );
    }
    assert_eq!(classify_park(&format!("{head}the suite panicked")), None);
    assert_eq!(
        classify_park(&format!("{head}failed to spawn (os error 206)")),
        None
    );
}

#[test]
fn a_node_that_could_not_start_is_ours_only_on_a_lock() {
    let head = "Node n1 could not be started again 3 times in a row (last error: ";
    let tail = "). Fix the cause and answer to try again, or say how the branch should go on.";
    for error in [
        "database is locked",
        "launch key branch-b of node n1 is reserved but holds no run",
    ] {
        assert_eq!(
            classify_park(&format!("{head}{error}{tail}")),
            Some(AutoCause::LaunchLocked),
            "{error}"
        );
    }
    assert_eq!(classify_park(&format!("{head}no such file{tail}")), None);
    assert_eq!(
        classify_park(&format!("{head}database is locked (os error 206){tail}")),
        None
    );
}

#[test]
fn widening_paths_is_never_a_cause() {
    assert_eq!(
        classify_park("Please widen the paths of branch b to include core/x.rs"),
        None
    );
}

#[test]
fn no_answer_widens_the_paths() {
    for cause in [
        AutoCause::StaleBase,
        AutoCause::FenceFromBase,
        AutoCause::GateInfrastructure,
        AutoCause::LaunchLocked,
    ] {
        assert!(!cause.answer().is_empty());
        assert!(
            !cause
                .answer()
                .to_ascii_lowercase()
                .contains("widen your paths")
        );
    }
    assert!(AutoCause::FenceFromBase.answer().contains("Do not widen"));
}

// ----- database -----

const TREE: &str = "ry-job";
const NODE: &str = "ry-node";
const AT: &str = "2020-01-01T00:00:00+00:00";

async fn state() -> AppState {
    let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
        Some(std::time::Duration::from_secs(60)),
        std::time::Duration::from_secs(600),
    )
    .await;
    let manifest = Manifest::new(TREE, "feat/ry", "Retry it", "C:/nowhere", &[]);
    let job = TreeJob {
        id: TREE.to_owned(),
        root_node_id: "ry-root".to_owned(),
        project_id: None,
        feature_branch: Some("feat/ry".to_owned()),
        goal: "Retry it".to_owned(),
        state: "open".to_owned(),
        manifest,
        contract: None,
        created_at: AT.to_owned(),
        updated_at: AT.to_owned(),
    };
    store::save(&state.pool, &job).await.unwrap();
    state
}

async fn open(pool: &SqlitePool, id: &str, question: &str) {
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

async fn row(pool: &SqlitePool, id: &str) -> (String, Option<String>, Option<String>) {
    sqlx::query_as("SELECT status, origin, answer FROM decisions WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_deterministic_park_is_answered_by_the_driver_and_others_stay_open() {
    let state = state().await;
    open(
        &state.pool,
        "d-gate",
        "The gate of branch b ended 3 times in a row without measuring anything; last reason: database is locked",
    )
    .await;
    open(&state.pool, "d-widen", "Widen the paths of branch b?").await;

    resolve_open_parks(&state, TREE).await;

    let (status, origin, answer) = row(&state.pool, "d-gate").await;
    assert_eq!(status, "answered");
    assert_eq!(origin.as_deref(), Some("driver"));
    assert_eq!(
        answer.as_deref(),
        Some(AutoCause::GateInfrastructure.answer())
    );
    let (status, _, _) = row(&state.pool, "d-widen").await;
    assert_eq!(status, "open");
    let feed: Vec<String> =
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'work_tree_auto_resolve'")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    assert_eq!(feed.len(), 1);
}

#[tokio::test]
async fn the_per_node_cap_bounds_the_reruns() {
    let state = state().await;
    for n in 0..5 {
        open(
            &state.pool,
            &format!("d-{n}"),
            "The gate of branch b ended 3 times in a row without measuring anything; last reason: pool timeout",
        )
        .await;
    }
    resolve_all_open_parks(&state).await;
    let answered: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM decisions WHERE status = 'answered'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(answered, 3, "decision_runs_per_node defaults to 3");
}
