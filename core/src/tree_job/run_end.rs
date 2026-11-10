//! Settling a tree node when its run ends, instead of at the next sweep.
//!
//! A run ending bumps `Topic::Runs`. [`spawn_listener`] waits on that topic and, a moment later,
//! fails every `running` node whose run is no longer live (`node_run::reconcile_running`), so the
//! node's state closes with the run and not up to a minute afterwards. The sweep still calls
//! `reconcile_running` every tick, as the backstop for a bump nobody heard (a daemon restart, a
//! writer that ended a run without bumping).

use std::time::Duration;

use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::state::AppState;
use crate::tree_job::node_run;

use nucleos_base::wake::{self, Topic};

/// How long a wake waits before settling, so a burst of run ends is one pass.
const COALESCE: Duration = Duration::from_millis(200);

/// Fails the `running` nodes whose run is gone and answers their ids. A failure is logged and
/// answers nothing: the sweep tries again.
pub(crate) async fn settle(pool: &SqlitePool) -> Vec<String> {
    match node_run::reconcile_running(pool).await {
        Ok(marked) => {
            for node in &marked {
                tracing::info!(node, "tree node settled on the end of its run");
            }
            marked
        }
        Err(error) => {
            tracing::warn!(%error, "could not settle the nodes of the runs that ended");
            Vec::new()
        }
    }
}

/// Starts the task that settles nodes on every run end. The subscription is made before this
/// returns, so a run that ends right after is not missed.
pub fn spawn_listener(state: AppState) -> JoinHandle<()> {
    let mut runs = wake::subscribe(Topic::Runs);
    tokio::spawn(async move {
        while runs.changed().await.is_ok() {
            tokio::time::sleep(COALESCE).await;
            runs.borrow_and_update();
            settle(&state.pool).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tree_job::model::Manifest;
    use crate::tree_job::store::{TreeJob, save};

    const AT: &str = "2026-10-30T10:00:00+00:00";

    async fn seed(pool: &SqlitePool, run_status: &str) {
        let manifest = Manifest::new("job-1", "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        let job = TreeJob {
            id: "job-1".to_owned(),
            root_node_id: "job-1-root".to_owned(),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: AT.to_owned(),
            updated_at: AT.to_owned(),
        };
        save(pool, &job).await.unwrap();
        sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (9001, 'node', ?, ?)")
            .bind(run_status)
            .bind(AT)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     tree_job_id, created_at, updated_at) \
             VALUES ('leaf-1', 'leaf-1', 0, 'leaf', 't', 'g', 'running', '9001', 'job-1', ?, ?)",
        )
        .bind(AT)
        .bind(AT)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn status(pool: &SqlitePool) -> String {
        sqlx::query_scalar("SELECT status FROM work_nodes WHERE id = 'leaf-1'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn settle_fails_a_running_leaf_whose_run_ended() {
        let pool = crate::testdb::fresh_pool().await;
        seed(&pool, "completed").await;

        assert_eq!(settle(&pool).await, ["leaf-1"]);

        assert_eq!(status(&pool).await, "failed");
    }

    #[tokio::test]
    async fn settle_leaves_a_leaf_whose_run_is_live() {
        let pool = crate::testdb::fresh_pool().await;
        seed(&pool, "running").await;

        assert!(settle(&pool).await.is_empty());

        assert_eq!(status(&pool).await, "running");
    }

    #[tokio::test]
    async fn a_run_end_wake_settles_the_node_without_a_sweep() {
        let state = crate::channels::testkit::state().await;
        seed(&state.pool, "completed").await;
        let listener = spawn_listener(state.clone());

        wake::bump(Topic::Runs);

        let mut settled = false;
        for _ in 0..100 {
            if status(&state.pool).await == "failed" {
                settled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        listener.abort();
        assert!(settled, "the node stayed running after the run-end wake");
    }
}
