//! Parks the tree on a handoff chain that cannot go on (autonomy-trees spec B7).
//!
//! - A chain that reached `MAX_CONTINUATIONS` carries the B2 default "split the rest of the node
//!   into new nodes" (declared through `auto_default`, applied by nobody yet).
//! - A budget refusal carries none: budget questions always wait for the owner.
//! - The question goes to the nearest ancestor of the handed-off node that tops a branch (or is
//!   the job's root), because the handed-off node itself tops none, and that ancestor is the one
//!   left `running` while nothing continues the chain.
//! - A node is asked about once per cause: the sweep asks again every tick.

use sqlx::SqlitePool;

use super::auto_default::{self, DefaultAction};
use super::decisions::{self, DecisionError};

/// Why the chain cannot go on.
#[derive(Debug, Clone, Copy)]
pub enum Cause<'a> {
    /// The chain reached its limit of handoffs.
    TooManyHandoffs,
    /// The tree's budget refused the continuation, with the reason it gave.
    Budget(&'a str),
}

/// What [`park_chain`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The open decision that now holds the branch (or the root).
    Asked(String),
    /// The node was already asked about for this cause, or the ancestor already waits.
    AlreadyAsked,
    /// The node belongs to no tree job, or no ancestor tops a branch or is the root.
    NoAncestor,
}

fn question(node_id: &str, cause: Cause<'_>) -> String {
    match cause {
        Cause::TooManyHandoffs => format!(
            "Node {node_id} handed off too many times in a row and its chain stopped. Split what \
             is left into new nodes, or say how to go on."
        ),
        Cause::Budget(reason) => format!(
            "Node {node_id} handed off but the budget refused to continue it ({reason}). Raise the \
             budget or say how to go on."
        ),
    }
}

/// Opens the owner question for the handed-off `node_id` on its nearest parkable ancestor.
pub async fn park_chain(
    pool: &SqlitePool,
    node_id: &str,
    cause: Cause<'_>,
) -> Result<Outcome, DecisionError> {
    let Some(tree_id) = super::close::tree_job_of(pool, node_id).await? else {
        return Ok(Outcome::NoAncestor);
    };
    let text = question(node_id, cause);
    let asked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions WHERE question = ?")
        .bind(&text)
        .fetch_one(pool)
        .await?;
    if asked > 0 {
        return Ok(Outcome::AlreadyAsked);
    }
    let mut current = node_id.to_owned();
    // The depth bound only guards a parent cycle.
    for _ in 0..64 {
        let parent: Option<Option<String>> =
            sqlx::query_scalar("SELECT parent_id FROM work_nodes WHERE id = ?")
                .bind(&current)
                .fetch_optional(pool)
                .await?;
        let Some(Some(parent)) = parent else {
            return Ok(Outcome::NoAncestor);
        };
        match decisions::park_or_ask(pool, &tree_id, &parent, &text).await {
            Ok(Some(decision)) => {
                if matches!(cause, Cause::TooManyHandoffs) {
                    auto_default::attach(
                        pool,
                        &decision.id,
                        DefaultAction::SplitRemainingIntoNewNodes,
                        &format!("node {node_id} handed off too many times in a row"),
                    )
                    .await?;
                }
                return Ok(Outcome::Asked(decision.id));
            }
            Ok(None) => return Ok(Outcome::AlreadyAsked),
            Err(DecisionError::NoBranch(_)) => current = parent,
            Err(error) => return Err(error),
        }
    }
    Ok(Outcome::NoAncestor)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::conductor::liveness::Limits;
    use crate::conductor::sweep;
    use crate::state::AppState;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::store::{TreeJob, load, save};

    /// A job whose branch `api` is `building` and tops `parent`, with the handoff node `link-N`
    /// chain under it: `links` nodes, each continuing the one before, the last one `handoff`.
    async fn seed(links: usize) -> AppState {
        let state = crate::channels::testkit::state().await;
        let created = Utc::now().to_rfc3339();
        let mut manifest =
            Manifest::new("job-1", "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        manifest.state = "open".to_owned();
        let mut branch = Branch::new("api", &["api/**".to_owned()], "backend", None);
        branch.state = "building".to_owned();
        branch.node_id = Some("parent".to_owned());
        manifest.branches.insert(branch.name.clone(), branch);
        save(
            &state.pool,
            &TreeJob {
                id: "job-1".to_owned(),
                root_node_id: "job-1-root".to_owned(),
                project_id: None,
                feature_branch: Some("feat/tree".to_owned()),
                goal: "Ship it".to_owned(),
                state: "open".to_owned(),
                manifest,
                contract: None,
                created_at: created.clone(),
                updated_at: created,
            },
        )
        .await
        .unwrap();
        node(&state, "parent", None, "running", None).await;
        for n in 0..links {
            let previous = (n > 0).then(|| format!("link-{}", n - 1));
            node(
                &state,
                &format!("link-{n}"),
                Some("parent"),
                "handoff",
                previous,
            )
            .await;
        }
        state
    }

    async fn node(
        state: &AppState,
        id: &str,
        parent: Option<&str>,
        status: &str,
        continues: Option<String>,
    ) {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status,
                                     owned_paths, tree_job_id, handoff_path, continues_node_id,
                                     created_at, updated_at)
             VALUES (?, 'parent', ?, ?, 'leaf', 't', 'g', ?, '[]', 'job-1', 'C:/h.md', ?, ?, ?)",
        )
        .bind(id)
        .bind(parent)
        .bind(i64::from(parent.is_some()))
        .bind(status)
        .bind(continues)
        .bind(&now)
        .bind(&now)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn decision_rows(state: &AppState) -> Vec<(String, Option<String>)> {
        sqlx::query_as("SELECT question, default_action FROM decisions ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    async fn branch_state(state: &AppState) -> String {
        let job = load(&state.pool, "job-1").await.unwrap().unwrap();
        job.manifest.branches["api"].state.clone()
    }

    #[tokio::test]
    async fn the_fourth_handoff_parks_the_parent_with_the_split_default() {
        let state = seed(4).await;

        sweep::tick(&state, &[], &Limits::default(), Utc::now())
            .await
            .unwrap();

        assert_eq!(branch_state(&state).await, "parked");
        let rows = decision_rows(&state).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].0.contains("link-3"));
        assert_eq!(rows[0].1.as_deref(), Some("split_remaining_into_new_nodes"));
    }

    #[tokio::test]
    async fn a_second_tick_does_not_ask_again() {
        let state = seed(4).await;
        let now = Utc::now();

        sweep::tick(&state, &[], &Limits::default(), now)
            .await
            .unwrap();
        sweep::tick(&state, &[], &Limits::default(), now)
            .await
            .unwrap();

        assert_eq!(decision_rows(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn a_budget_refusal_parks_the_parent_with_no_default() {
        let state = seed(1).await;

        let outcome = park_chain(&state.pool, "link-0", Cause::Budget("tree budget spent"))
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Asked(_)));
        assert_eq!(branch_state(&state).await, "parked");
        let rows = decision_rows(&state).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].0.contains("tree budget spent"));
        assert_eq!(rows[0].1, None);
    }

    #[tokio::test]
    async fn a_node_with_no_parkable_ancestor_asks_nothing() {
        let state = seed(1).await;
        sqlx::query("UPDATE work_nodes SET parent_id = NULL WHERE id = 'link-0'")
            .execute(&state.pool)
            .await
            .unwrap();

        let outcome = park_chain(&state.pool, "link-0", Cause::TooManyHandoffs)
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::NoAncestor);
        assert!(decision_rows(&state).await.is_empty());
    }
}
