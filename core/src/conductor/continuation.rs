//! Continuation of work nodes that closed because their context was exhausted.

use crate::state::AppState;

/// The sibling created to carry on a handed-off node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continuation {
    pub node_id: String,
    pub run_id: Option<i64>,
}

/// Refusals that are meaningful to callers of [`continue_node`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationError {
    NotHandoff,
    TooManyContinuations,
}

/// Opens and, for daemon-owned tree nodes, launches the next node in a handoff chain.
pub async fn continue_node(
    _state: &AppState,
    _node_id: &str,
) -> Result<Continuation, ContinuationError> {
    todo!("D-CONT implementation belongs to the GREEN phase")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::conductor::liveness::Limits;
    use crate::conductor::sweep;

    async fn state() -> AppState {
        crate::channels::testkit::state().await
    }

    async fn seed_node(
        state: &AppState,
        id: &str,
        parent_id: Option<&str>,
        root_id: &str,
        depth: i64,
        status: &str,
        tree_job_id: Option<&str>,
        handoff_path: Option<&str>,
    ) {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status, \
                                     owned_paths, tree_job_id, handoff_path, created_at, updated_at, closed_at) \
             VALUES (?, ?, ?, ?, 'leaf', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(root_id)
        .bind(parent_id)
        .bind(depth)
        .bind(format!("title-{id}"))
        .bind("Keep the original goal")
        .bind(status)
        .bind(r#"["core/src/conductor/**"]"#)
        .bind(tree_job_id)
        .bind(handoff_path)
        .bind(&now)
        .bind(&now)
        .bind((status == "handoff").then_some(now))
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn seed_handoff(state: &AppState, id: &str, parent_id: &str, root_id: &str) {
        seed_node(
            state,
            id,
            Some(parent_id),
            root_id,
            1,
            "handoff",
            None,
            Some("C:/state/nodes/root/handoff.md"),
        )
        .await;
    }

    async fn sibling(state: &AppState, id: &str) -> (String, String, String, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT parent_id, goal, owned_paths, tree_job_id, run_id FROM work_nodes WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    async fn children(state: &AppState, parent_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes WHERE parent_id = ?")
            .bind(parent_id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn handoff_node_gets_a_continuation_with_same_scope() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_handoff(&state, "handoff", "root", "root").await;

        let continuation = continue_node(&state, "handoff").await.unwrap();

        let (parent, goal, paths, tree_job, run_id) = sibling(&state, &continuation.node_id).await;
        assert_eq!(parent, "root");
        assert!(goal.starts_with("Keep the original goal"), "the goal keeps the original text first: {goal}");
        assert_eq!(paths, r#"["core/src/conductor/**"]"#);
        assert_eq!(tree_job, None);
        assert_eq!(run_id, None);
    }

    /// `work_nodes` has no brief column, so the continuation's `goal` is where the handoff file has
    /// to be named. This conflicts with the exact-equality assertion on `goal` in
    /// `handoff_node_gets_a_continuation_with_same_scope`, which GREEN must reconcile.
    #[tokio::test]
    async fn continuation_brief_names_the_handoff_file() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_handoff(&state, "handoff", "root", "root").await;

        let continuation = continue_node(&state, "handoff").await.unwrap();

        let (_, goal, ..) = sibling(&state, &continuation.node_id).await;
        assert!(
            goal.contains("C:/state/nodes/root/handoff.md"),
            "the goal does not name the handoff file: {goal:?}"
        );
        assert!(
            goal.contains("Keep the original goal"),
            "the goal lost the original text: {goal:?}"
        );
    }

    #[tokio::test]
    async fn continuation_of_a_done_node_is_refused() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_node(&state, "done", Some("root"), "root", 1, "done", None, None).await;

        assert_eq!(
            continue_node(&state, "done").await,
            Err(ContinuationError::NotHandoff)
        );
    }

    #[tokio::test]
    async fn continuation_respects_the_budget() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_handoff(&state, "handoff", "root", "root").await;
        sqlx::query(
            "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
            .bind("root")
            .bind(0.0)
            .bind(80.0)
            .bind(Utc::now().to_rfc3339())
            .bind(Utc::now().to_rfc3339())
            .execute(&state.pool)
            .await
            .unwrap();

        assert!(continue_node(&state, "handoff").await.is_err());
        assert_eq!(children(&state, "root").await, 1);
    }

    #[tokio::test]
    async fn second_call_returns_the_same_continuation() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_handoff(&state, "handoff", "root", "root").await;

        let first = continue_node(&state, "handoff").await.unwrap();
        let second = continue_node(&state, "handoff").await.unwrap();

        assert_eq!(second, first);
        assert_eq!(children(&state, "root").await, 2);
    }

    #[tokio::test]
    async fn fourth_continuation_of_a_chain_is_refused() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_handoff(&state, "handoff-0", "root", "root").await;

        let first = continue_node(&state, "handoff-0").await.unwrap();
        hand_off(&state, &first.node_id, "handoff-1").await;
        let second = continue_node(&state, &first.node_id).await.unwrap();
        hand_off(&state, &second.node_id, "handoff-2").await;
        let third = continue_node(&state, &second.node_id).await.unwrap();
        hand_off(&state, &third.node_id, "handoff-3").await;

        assert_eq!(
            continue_node(&state, &third.node_id).await,
            Err(ContinuationError::TooManyContinuations)
        );
    }

    async fn hand_off(state: &AppState, id: &str, name: &str) {
        sqlx::query("UPDATE work_nodes SET status = 'handoff', handoff_path = ? WHERE id = ?")
            .bind(format!("C:/state/nodes/root/{name}.md"))
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_sweep_continues_a_handoff_node_once() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_handoff(&state, "handoff", "root", "root").await;
        let now = Utc::now();

        sweep::tick(&state, &[], &Limits::default(), now).await.unwrap();
        sweep::tick(&state, &[], &Limits::default(), now).await.unwrap();

        assert_eq!(children(&state, "root").await, 2);
    }
}
