//! Tree-job node wrap-up: closes a finished node run and records its outcome (scaffold stub).

use crate::state::AppState;

pub async fn tick(_state: &AppState) {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::node_run;
    use crate::tree_job::open::{self, StoredLaunch};
    use crate::tree_job::store::{self, TreeJob};

    const JOB: &str = "wrap-job";
    const NODE: &str = "wrap-node";
    const CREATED: &str = "2026-10-31T00:00:00+00:00";

    async fn state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    async fn seed_node(
        state: &AppState,
        branch_state: Option<&str>,
        job_state: &str,
        node_id: &str,
        run_id: Option<i64>,
    ) {
        let mut manifest = Manifest::new(JOB, "feat/wrap", "Wrap the run", "C:/nowhere", &[]);
        manifest.state = job_state.to_owned();
        if let Some(branch_state) = branch_state {
            let mut branch = Branch::new("branch", &["core/src/**".to_owned()], "core", None);
            branch.state = branch_state.to_owned();
            branch.node_id = Some(node_id.to_owned());
            manifest.branches.insert(branch.name.clone(), branch);
        }
        store::save(
            &state.pool,
            &TreeJob {
                id: JOB.to_owned(),
                root_node_id: "wrap-root".to_owned(),
                project_id: None,
                feature_branch: Some("feat/wrap".to_owned()),
                goal: "Wrap the run".to_owned(),
                state: job_state.to_owned(),
                manifest,
                contract: None,
                created_at: CREATED.to_owned(),
                updated_at: CREATED.to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     tree_job_id, handoff_path, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'title', 'goal', 'running', ?, ?, ?, ?, ?)",
        )
        .bind(node_id)
        .bind("wrap-root")
        .bind((branch_state.is_none()) as i64)
        .bind(if branch_state.is_some() { "orchestrator" } else { "root" })
        .bind(run_id.map(|id| id.to_string()))
        .bind(JOB)
        .bind("C:/state/nodes/wrap/handoff.md")
        .bind(CREATED)
        .bind(CREATED)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn seed_run(state: &AppState, id: i64, status: &str) {
        sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'original prompt', ?, ?)")
            .bind(id)
            .bind(status)
            .bind(CREATED)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    async fn seed_launch(state: &AppState, node_id: &str) {
        open::save_launch(
            &state.pool,
            &StoredLaunch {
                node_id: node_id.to_owned(),
                key: "initial".to_owned(),
                prompt: "original prompt".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
    }

    async fn report(state: &AppState, node_id: &str, run_id: i64, result: &str) {
        node_run::apply(&state.pool, node_id, "report", &run_id.to_string(), result)
            .await
            .unwrap();
    }

    async fn launches(state: &AppState, node_id: &str, prefix: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions WHERE node_id = ? AND transition = 'launch' AND key LIKE ?")
            .bind(node_id)
            .bind(format!("{prefix}%"))
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_dead_run_with_a_handoff_report_is_relaunched_under_a_wrap_key_with_the_handoff_sentence() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(41)).await;
        seed_run(&state, 41, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 41, "handoff").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "wrap-41").await, 1);
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id <> 41 ORDER BY id DESC LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(prompt.contains("handoff.md"), "{prompt:?}");
    }

    #[tokio::test]
    async fn a_second_tick_over_the_same_dead_run_launches_nothing() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(42)).await;
        seed_run(&state, 42, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 42, "handoff").await;

        tick(&state).await;
        let first = launches(&state, NODE, "wrap-42").await;
        tick(&state).await;

        assert_eq!(first, 1);
        assert_eq!(launches(&state, NODE, "wrap-42").await, first);
    }

    #[tokio::test]
    async fn a_live_run_with_a_handoff_report_is_ended_not_relaunched() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, None).await;
        let run_id = node_run::launch(&state, NODE, "live", "original prompt".to_owned(), None, None, "shadow")
            .await
            .unwrap();
        report(&state, NODE, run_id, "handoff").await;

        tick(&state).await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(launches(&state, NODE, "wrap-").await, 0);
    }

    #[tokio::test]
    async fn the_fourth_handoff_link_of_a_chain_stops_with_one_feed_line_and_parks_the_branch() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(43)).await;
        seed_run(&state, 43, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 43, "handoff").await;
        for key in ["wrap-old-1", "wrap-old-2", "wrap-old-3"] {
            node_run::apply(&state.pool, NODE, "launch", key, "99").await.unwrap();
        }

        tick(&state).await;

        let parked = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches["branch"]
            .state
            .clone();
        assert_eq!(parked, "parked");
        let feed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'node_continuation_stopped' AND subject = ?")
            .bind(format!("node:{NODE}"))
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(feed, 1);
    }

    #[tokio::test]
    async fn a_run_that_died_without_a_report_is_relaunched_twice_and_then_parked() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(44)).await;
        seed_run(&state, 44, "interrupted").await;
        seed_launch(&state, NODE).await;

        tick(&state).await;
        assert_eq!(launches(&state, NODE, "died-44").await, 1);
        let first: i64 = sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?").bind(first).execute(&state.pool).await.unwrap();
        tick(&state).await;
        assert_eq!(launches(&state, NODE, "died-").await, 2);
        let second: i64 = sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?").bind(second).execute(&state.pool).await.unwrap();
        tick(&state).await;

        let parked = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches["branch"]
            .state
            .clone();
        assert_eq!(parked, "parked");
    }

    #[tokio::test]
    async fn a_parked_branch_a_merged_branch_and_a_finished_job_are_never_relaunched() {
        let parked_state = state().await;
        seed_node(&parked_state, Some("parked"), "open", NODE, Some(45)).await;
        seed_run(&parked_state, 45, "interrupted").await;
        seed_launch(&parked_state, NODE).await;
        report(&parked_state, NODE, 45, "handoff").await;

        tick(&parked_state).await;

        assert_eq!(launches(&parked_state, NODE, "wrap-").await, 0);

        let merged_state = state().await;
        seed_node(&merged_state, Some("merged"), "open", NODE, Some(48)).await;
        seed_run(&merged_state, 48, "interrupted").await;
        seed_launch(&merged_state, NODE).await;
        report(&merged_state, NODE, 48, "handoff").await;
        tick(&merged_state).await;
        assert_eq!(launches(&merged_state, NODE, "wrap-").await, 0);

        let finished_state = state().await;
        seed_node(&finished_state, Some("building"), "finished", NODE, Some(49)).await;
        seed_run(&finished_state, 49, "interrupted").await;
        seed_launch(&finished_state, NODE).await;
        report(&finished_state, NODE, 49, "handoff").await;
        tick(&finished_state).await;
        assert_eq!(launches(&finished_state, NODE, "wrap-").await, 0);
    }

    #[tokio::test]
    async fn a_done_report_with_a_dead_run_is_left_to_the_driver() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(46)).await;
        seed_run(&state, 46, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 46, "done").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "wrap-").await, 0);
        let run_id: String = sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_id, "46");
    }

    #[tokio::test]
    async fn the_root_node_is_relaunched_after_a_handoff_report() {
        let state = state().await;
        seed_node(&state, None, "open", "wrap-root", Some(47)).await;
        seed_run(&state, 47, "interrupted").await;
        seed_launch(&state, "wrap-root").await;
        report(&state, "wrap-root", 47, "handoff").await;

        tick(&state).await;

        assert_eq!(launches(&state, "wrap-root", "wrap-47").await, 1);
    }
}
