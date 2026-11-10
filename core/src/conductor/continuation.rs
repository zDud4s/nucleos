//! Continuation of work nodes that closed because their context was exhausted.

use std::path::Path;

use sqlx::SqlitePool;

use crate::conductor::unresolved;
use crate::conductor::depth::{self, AncestorView, ChildArgs, TreeLimits};
use crate::state::AppState;
use crate::tree_cost::budget::{self, Permit};
use crate::tree_job::close::tree_job_of;
use crate::tree_job::model::JOB_TERMINAL_STATE;
use crate::tree_job::node_run::NodeRunError;
use crate::tree_job::open::{StoredLaunch, latest_launch};
use crate::tree_job::store;
use crate::work_nodes::{self, Node, NodeError, NodeLimits, OpenArgs, OpenLinks};

/// How many continuations one chain may hold. The original node does not count.
const MAX_CONTINUATIONS: usize = 3;

/// The sentence that names the handoff file, in a goal and in a prompt.
const HANDOFF_MARK: &str = "\n\nContinue from the handoff at ";

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
    /// The node's tree job is finished, or the node tops a branch: such a node closes only once its
    /// branch is merged or dropped, so nothing is left to carry on.
    WorkOver,
    /// The tree's budget refuses another node.
    Budget(String),
    /// A guard refused the sibling, or the node cannot have one. Nothing was opened.
    Refused(String),
    /// The sibling exists and its run could not be started. Calling again retries the launch.
    Launch(String),
    Db(String),
}

fn db_error(error: impl std::fmt::Display) -> ContinuationError {
    ContinuationError::Db(error.to_string())
}

fn node_error(error: NodeError) -> ContinuationError {
    match error {
        NodeError::Db(error) => db_error(error),
        refusal => ContinuationError::Refused(refusal.to_string()),
    }
}

/// `text` with the sentence naming `handoff` at its end. On a chain the sentence of the previous
/// link is taken out first, so neither a goal nor a prompt grows and the original text stays first;
/// whatever a relaunch wrote after that sentence stays too.
pub(crate) fn with_handoff(text: &str, handoff: &str) -> String {
    let kept = match text.rfind(HANDOFF_MARK) {
        Some(at) => {
            let sentence = &text[at + HANDOFF_MARK.len()..];
            let after = sentence.find('\n').map_or("", |end| &sentence[end..]);
            format!("{}{after}", &text[..at])
        }
        None => text.to_owned(),
    };
    format!("{}{HANDOFF_MARK}{handoff}.", kept.trim_end())
}

/// Handed-off tree nodes the sweep has to ask about: those with no continuation, and those whose
/// continuation still has no run because its launch failed. Nodes of a finished job are left out,
/// and so are nodes whose parent closed, since a closed parent takes no new child.
pub(crate) async fn awaiting_continuation(pool: &SqlitePool) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT n.id FROM work_nodes n
         JOIN tree_jobs j ON j.id = n.tree_job_id
         JOIN work_nodes p ON p.id = n.parent_id
         LEFT JOIN work_nodes c ON c.continues_node_id = n.id
         WHERE n.status = 'handoff' AND j.state != ?
           AND p.status NOT IN ('done', 'failed', 'handoff', 'blocked')
           AND (c.id IS NULL OR (c.run_id IS NULL AND c.status = 'running'))
         ORDER BY n.created_at, n.id",
    )
    .bind(JOB_TERMINAL_STATE)
    .fetch_all(pool)
    .await
}

/// How many continuations stand behind `node_id`, counted no further than the cap.
async fn continuations_behind(pool: &SqlitePool, node_id: &str) -> sqlx::Result<usize> {
    let mut count = 0;
    let mut current = node_id.to_owned();
    while count < MAX_CONTINUATIONS {
        let previous: Option<Option<String>> =
            sqlx::query_scalar("SELECT continues_node_id FROM work_nodes WHERE id = ?")
                .bind(&current)
                .fetch_optional(pool)
                .await?;
        let Some(Some(previous)) = previous else {
            break;
        };
        current = previous;
        count += 1;
    }
    Ok(count)
}

/// `(id, run_id)` of the continuation of `node_id`, if it has one.
async fn existing_continuation(
    pool: &SqlitePool,
    node_id: &str,
) -> sqlx::Result<Option<(String, Option<String>)>> {
    sqlx::query_as("SELECT id, run_id FROM work_nodes WHERE continues_node_id = ?")
        .bind(node_id)
        .fetch_optional(pool)
        .await
}

async fn check_work_remains(
    pool: &SqlitePool,
    job_id: &str,
    node_id: &str,
) -> Result<(), ContinuationError> {
    let job = store::load(pool, job_id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ContinuationError::Refused(format!("tree job {job_id} not found")))?;
    let tops_a_branch = job
        .manifest
        .branches
        .values()
        .any(|branch| branch.node_id.as_deref() == Some(node_id));
    if job.state == JOB_TERMINAL_STATE || tops_a_branch {
        return Err(ContinuationError::WorkOver);
    }
    Ok(())
}

/// The limits of the node's project, read as `/nodes` reads them.
async fn limits_for(pool: &SqlitePool, project_id: Option<&str>) -> NodeLimits {
    let Some(project_id) = project_id else {
        return NodeLimits::load(None);
    };
    match crate::inspect::project_root(pool, project_id).await {
        Ok(Some(root)) => NodeLimits::load(Some(Path::new(&root))),
        Ok(None) => NodeLimits::load(None),
        Err(error) => {
            tracing::warn!(project = project_id, %error, "could not read the project root");
            NodeLimits::load(None)
        }
    }
}

/// The guards of `POST /nodes` that `work_nodes::open` does not apply itself.
///
/// The child is presented with no paths, because the scope half of `check_child` asks whether a
/// child divides its parent, and this one divides nothing by design: it takes over a scope that was
/// admitted when the source opened, which may be the parent's whole scope. `open` still checks that
/// scope against the parent's and against the running siblings. So `open_children`, which only the
/// scope half reads, is not counted.
async fn check_guards(
    pool: &SqlitePool,
    source: &Node,
    parent: &Node,
    goal: &str,
) -> Result<(), ContinuationError> {
    let mut chain = Vec::new();
    let mut ancestor = Some(parent.clone());
    while let Some(node) = ancestor {
        chain.push(AncestorView {
            owned_paths: node.owned_paths,
            goal: node.goal,
            open_children: 0,
        });
        ancestor = match node.parent_id {
            Some(parent_id) => Some(
                work_nodes::get(pool, &parent_id)
                    .await
                    .map_err(node_error)?,
            ),
            None => None,
        };
    }
    chain.reverse();
    let tree_total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes WHERE root_id = ?")
        .bind(&source.root_id)
        .fetch_one(pool)
        .await
        .map_err(db_error)?;
    let child = ChildArgs {
        owned_paths: Vec::new(),
        goal: goal.to_owned(),
    };
    depth::check_child(&chain, &child, tree_total, &TreeLimits::default())
        .map_err(|refusal| ContinuationError::Refused(format!("{refusal:?}")))?;
    if let Permit::No { reason } = budget::permits_next_node(pool, &source.root_id).await {
        return Err(ContinuationError::Budget(reason));
    }
    Ok(())
}

/// Opens the sibling, or finds the one a concurrent call opened first.
async fn open_sibling(
    pool: &SqlitePool,
    source: &Node,
    handoff: &str,
    tree_job: Option<&str>,
) -> Result<String, ContinuationError> {
    let Some(parent_id) = source.parent_id.as_deref() else {
        return Err(ContinuationError::Refused(format!(
            "node {} has no parent to open a sibling under",
            source.id
        )));
    };
    let parent = work_nodes::get(pool, parent_id).await.map_err(node_error)?;
    let goal = with_handoff(&source.goal, handoff);
    check_guards(pool, source, &parent, &goal).await?;
    let args = OpenArgs {
        parent_id: Some(parent.id.clone()),
        kind: source.kind.clone(),
        title: source.title.clone(),
        goal,
        owned_paths: source.owned_paths.clone(),
        budget_tokens: source.budget_tokens,
        task_id: source.task_id.clone(),
        ..OpenArgs::default()
    };
    let links = OpenLinks {
        tree_job_id: tree_job,
        continues_node_id: Some(&source.id),
        ..OpenLinks::default()
    };
    let limits = limits_for(pool, source.project_id.as_deref()).await;
    match work_nodes::open_linked(pool, source.project_id.as_deref(), &limits, &args, links).await {
        Ok(opened) => Ok(opened.id),
        Err(NodeError::Db(error))
            if error
                .as_database_error()
                .is_some_and(|database_error| database_error.is_unique_violation()) =>
        {
            let winner = existing_continuation(pool, &source.id)
                .await
                .map_err(db_error)?;
            winner.map(|(id, _)| id).ok_or_else(|| db_error(error))
        }
        Err(error) => Err(node_error(error)),
    }
}

/// Opens and, for daemon-owned tree nodes, launches the next node in a handoff chain.
///
/// Idempotent: the unique index on `continues_node_id` allows one sibling per node, and every later
/// call returns it. A node with a tree job needs a stored launch, the only place its prompt and cwd
/// are kept; without one it is refused before anything opens, since a sibling no agent can ever be
/// started for would hold its parent's slot as `running` for good.
///
/// A launch that fails leaves the sibling in place and answers `Launch`. Calling again retries it
/// under the same key `continue-<source id>`, because `launch_recovering` gives the key back when
/// the run could not be created. A key left reserved means another call is creating the run, or a
/// crash that boot recovery clears and relaunches, so it answers the sibling with `run_id: None`.
pub async fn continue_node(
    state: &AppState,
    node_id: &str,
) -> Result<Continuation, ContinuationError> {
    let pool = &state.pool;
    let source = work_nodes::get(pool, node_id).await.map_err(node_error)?;
    if source.status != "handoff" {
        return Err(ContinuationError::NotHandoff);
    }
    let tree_job = tree_job_of(pool, node_id).await.map_err(db_error)?;
    let existing = existing_continuation(pool, node_id)
        .await
        .map_err(db_error)?;
    if let Some((sibling, run_id)) = &existing
        && (run_id.is_some() || tree_job.is_none())
    {
        return Ok(Continuation {
            node_id: sibling.clone(),
            run_id: run_id.as_deref().and_then(|run_id| run_id.parse().ok()),
        });
    }

    if existing.is_none()
        && continuations_behind(pool, node_id)
            .await
            .map_err(db_error)?
            >= MAX_CONTINUATIONS
    {
        return Err(ContinuationError::TooManyContinuations);
    }
    let Some(handoff) = source.handoff_path.as_deref() else {
        return Err(ContinuationError::Refused(format!(
            "node {node_id} closed as handoff without naming a handoff file"
        )));
    };
    let launch = match tree_job.as_deref() {
        Some(job_id) => {
            check_work_remains(pool, job_id, node_id).await?;
            let stored = latest_launch(pool, node_id).await.map_err(db_error)?;
            Some(stored.ok_or_else(|| {
                ContinuationError::Refused(format!("no launch was stored for node {node_id}"))
            })?)
        }
        None => None,
    };

    let sibling = match existing {
        Some((sibling, _)) => sibling,
        None => open_sibling(pool, &source, handoff, tree_job.as_deref()).await?,
    };
    let Some(launch) = launch else {
        return Ok(Continuation {
            node_id: sibling,
            run_id: None,
        });
    };
    let run_id = launch_sibling(state, &sibling, node_id, handoff, launch).await?;
    Ok(Continuation {
        node_id: sibling,
        run_id,
    })
}

/// `launch_recovering` rather than plain `node_run::launch`: it wraps that call, gives the key back
/// on a failed create, and stores the launch, which is what the next link of the chain starts from.
async fn launch_sibling(
    state: &AppState,
    sibling: &str,
    source_id: &str,
    handoff: &str,
    launch: StoredLaunch,
) -> Result<Option<i64>, ContinuationError> {
    let key = format!("continue-{source_id}");
    let launched = store::launch_recovering(
        state,
        sibling,
        &key,
        with_handoff(&launch.prompt, handoff),
        launch.project_id,
        launch.cwd,
        &launch.mode,
    )
    .await;
    match launched {
        Ok(run_id) => {
            let _ = unresolved::reset(&state.pool, sibling).await;
            Ok(Some(run_id))
        }
        // Counted, not skipped in silence: past `UNRESOLVED_CAP` ticks the reservation is cleared
        // (with a feed line), and the next tick launches again.
        Err(NodeRunError::Unresolved { .. }) => {
            if let Err(error) = unresolved::note(&state.pool, sibling, &key).await {
                tracing::warn!(node = sibling, %error, "could not count the unresolved launch");
            }
            Ok(None)
        }
        Err(error) => Err(ContinuationError::Launch(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::conductor::liveness::Limits;
    use crate::conductor::sweep;
    use crate::tree_job::model::Manifest;
    use crate::tree_job::open::save_launch;
    use crate::tree_job::store::{TreeJob, save};

    async fn state() -> AppState {
        crate::channels::testkit::state().await
    }

    #[allow(clippy::too_many_arguments)]
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

    async fn sibling(
        state: &AppState,
        id: &str,
    ) -> (String, String, String, Option<String>, Option<String>) {
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
        assert!(
            goal.starts_with("Keep the original goal"),
            "the goal keeps the original text first: {goal}"
        );
        assert_eq!(paths, r#"["core/src/conductor/**"]"#);
        assert_eq!(tree_job, None);
        assert_eq!(run_id, None);
    }

    /// `work_nodes` has no brief column, so the continuation's `goal` is where the handoff file has
    /// to be named.
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

    /// An open tree job with no branches, as `sweep`'s `seed_tree_node` seeds it.
    async fn seed_job(state: &AppState) {
        let created = Utc::now().to_rfc3339();
        let mut manifest =
            Manifest::new("job-1", "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        manifest.state = "open".to_owned();
        let job = TreeJob {
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
        };
        save(&state.pool, &job).await.unwrap();
    }

    async fn seed_tree_handoff(state: &AppState, id: &str) {
        seed_node(
            state,
            id,
            Some("root"),
            "root",
            1,
            "handoff",
            Some("job-1"),
            Some("C:/state/nodes/root/handoff.md"),
        )
        .await;
    }

    #[tokio::test]
    async fn the_sweep_continues_a_handoff_node_once() {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(std::time::Duration::from_secs(5)),
            std::time::Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        seed_job(&state).await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_tree_handoff(&state, "handoff").await;
        save_launch(
            &state.pool,
            &StoredLaunch {
                node_id: "handoff".to_owned(),
                key: "key-1".to_owned(),
                prompt: "do it".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
        let now = Utc::now();

        sweep::tick(&state, &[], &Limits::default(), now)
            .await
            .unwrap();
        sweep::tick(&state, &[], &Limits::default(), now)
            .await
            .unwrap();

        assert_eq!(children(&state, "root").await, 2);
        let run_id: Option<String> =
            sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE continues_node_id = 'handoff'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(run_id.is_some(), "the sibling has no run");
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 1);
    }

    /// A continuation sibling with no run whose launch key is reserved with an empty result.
    async fn seed_stuck_sibling(state: &AppState) {
        seed_job(state).await;
        seed_node(state, "root", None, "root", 0, "running", None, None).await;
        seed_tree_handoff(state, "handoff").await;
        seed_node(
            state,
            "sib",
            Some("root"),
            "root",
            1,
            "running",
            Some("job-1"),
            None,
        )
        .await;
        sqlx::query("UPDATE work_nodes SET continues_node_id = 'handoff' WHERE id = 'sib'")
            .execute(&state.pool)
            .await
            .unwrap();
        save_launch(
            &state.pool,
            &StoredLaunch {
                node_id: "handoff".to_owned(),
                key: "key-1".to_owned(),
                prompt: "do it".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tree_transitions (node_id, transition, key, result, created_at) \
             VALUES ('sib', 'launch', 'continue-handoff', '', ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn stuck_reservations(state: &AppState) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM tree_transitions WHERE node_id = 'sib' AND transition = 'launch'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_unresolved_launch_is_kept_below_the_cap() {
        let state = state().await;
        seed_stuck_sibling(&state).await;

        for _ in 1..unresolved::UNRESOLVED_CAP {
            let continuation = continue_node(&state, "handoff").await.unwrap();
            assert_eq!(continuation.run_id, None);
        }

        assert_eq!(stuck_reservations(&state).await, 1);
    }

    #[tokio::test]
    async fn an_unresolved_launch_is_cleared_with_a_feed_line_at_the_cap() {
        let state = state().await;
        seed_stuck_sibling(&state).await;

        for _ in 0..unresolved::UNRESOLVED_CAP {
            continue_node(&state, "handoff").await.unwrap();
        }

        assert_eq!(stuck_reservations(&state).await, 0);
        let lines: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM feed WHERE kind = ? AND subject = 'node:sib'",
        )
        .bind(unresolved::UNRESOLVED_CLEARED)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(lines, 1);
    }

    #[tokio::test]
    async fn the_sweep_never_continues_a_node_with_no_tree_job() {
        let state = state().await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_handoff(&state, "handoff", "root", "root").await;

        sweep::tick(&state, &[], &Limits::default(), Utc::now())
            .await
            .unwrap();

        assert_eq!(children(&state, "root").await, 1);
    }

    #[tokio::test]
    async fn the_sweep_says_once_that_a_chain_stopped() {
        let state = state().await;
        seed_job(&state).await;
        seed_node(&state, "root", None, "root", 0, "running", None, None).await;
        seed_tree_handoff(&state, "link-0").await;
        for (link, previous) in [
            ("link-1", "link-0"),
            ("link-2", "link-1"),
            ("link-3", "link-2"),
        ] {
            seed_tree_handoff(&state, link).await;
            sqlx::query("UPDATE work_nodes SET continues_node_id = ? WHERE id = ?")
                .bind(previous)
                .bind(link)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        let now = Utc::now();

        sweep::tick(&state, &[], &Limits::default(), now)
            .await
            .unwrap();
        sweep::tick(&state, &[], &Limits::default(), now)
            .await
            .unwrap();

        let lines: Vec<String> =
            sqlx::query_scalar("SELECT subject FROM feed WHERE kind = 'node_continuation_stopped'")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(lines, ["node:link-3"]);
        assert_eq!(children(&state, "root").await, 4);
    }
}
