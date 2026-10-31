//! Keyed, replay-safe node transitions, reconciliation of false `running` nodes, and agent launch.

use std::collections::{HashMap, HashSet};
use std::fmt;

use sqlx::SqlitePool;

use crate::execution::concurrency::LIVE_RUN_STATUSES;
use crate::execution::runs::CreateRunError;
use crate::state::AppState;
use crate::tree_job::engine::branch_node_may_close;
use crate::tree_job::model::JOB_TERMINAL_STATE;
use crate::tree_job::store::{self, StoreError, TreeJob};

/// The transition name `launch` records its key under.
pub(super) const LAUNCH: &str = "launch";

const NODE_PROMPT_HEADER: &str = "You are the agent for NucleOS tree node ";

/// Gives a tree node the protocol it needs to split work and report its result.
///
/// A relaunch receives the already stored prompt, so the marker must make this a no-op after the
/// first launch rather than accumulating a second copy of the instructions.
pub(super) fn node_prompt(node_id: &str, task: &str) -> String {
    if task.starts_with(NODE_PROMPT_HEADER) {
        return task.to_owned();
    }
    format!(
        "{NODE_PROMPT_HEADER}{node_id}. If the work is too large for this node, you may open children under it with node_open. Finish by writing a handoff file and calling node_close. Write the handoff file as `.nucleos-report.md` in your worktree (your current directory; the daemon keeps it out of git, so do not commit it) and pass its absolute path as handoff_path.\n\n{task}"
    )
}

#[derive(Debug)]
pub enum NodeRunError {
    Db(sqlx::Error),
    Store(StoreError),
    Create(CreateRunError),
    /// No `work_nodes` row has this id.
    NodeNotFound(String),
    /// The key is reserved but holds no run id: another call is creating the run right now, or an
    /// earlier call died (or `create_run_inner` failed) after reserving it. `launch` itself does not
    /// recover such a reservation: `store::launch_recovering` clears a key whose reservation its own
    /// failing call made, and `store::resume_all` clears the rest at boot.
    Unresolved { node_id: String, key: String },
}

impl fmt::Display for NodeRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeRunError::Db(error) => write!(f, "database error: {error}"),
            NodeRunError::Store(error) => write!(f, "{error}"),
            NodeRunError::Create(error) => write!(f, "could not create the run: {error}"),
            NodeRunError::NodeNotFound(id) => write!(f, "work node {id} not found"),
            NodeRunError::Unresolved { node_id, key } => write!(
                f,
                "launch key {key} of node {node_id} is reserved but holds no run"
            ),
        }
    }
}

impl std::error::Error for NodeRunError {}

impl From<sqlx::Error> for NodeRunError {
    fn from(error: sqlx::Error) -> Self {
        NodeRunError::Db(error)
    }
}

impl From<StoreError> for NodeRunError {
    fn from(error: StoreError) -> Self {
        NodeRunError::Store(error)
    }
}

impl From<CreateRunError> for NodeRunError {
    fn from(error: CreateRunError) -> Self {
        NodeRunError::Create(error)
    }
}

/// The outcome of a keyed transition: the stored result, and whether it was already stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub result: String,
    pub replayed: bool,
}

/// Records `(node_id, transition, key)` with `result`, or returns the result already stored for it.
/// One atomic insert decides the winner, so concurrent calls cannot both be the first.
async fn claim(
    pool: &SqlitePool,
    node_id: &str,
    transition: &str,
    key: &str,
    result: &str,
) -> Result<Applied, sqlx::Error> {
    let inserted = sqlx::query(
        "INSERT INTO tree_transitions (node_id, transition, key, result, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (node_id, transition, key) DO NOTHING",
    )
    .bind(node_id)
    .bind(transition)
    .bind(key)
    .bind(result)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?
    .rows_affected();
    if inserted == 1 {
        return Ok(Applied {
            result: result.to_owned(),
            replayed: false,
        });
    }
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = ? AND key = ?",
    )
    .bind(node_id)
    .bind(transition)
    .bind(key)
    .fetch_one(pool)
    .await?;
    Ok(Applied {
        result: stored.unwrap_or_default(),
        replayed: true,
    })
}

/// Applies a keyed transition once: the first call for a (node, transition, key) stores `result`
/// and returns it with `replayed: false`; any later call returns the stored result with
/// `replayed: true`, whatever `result` it was given.
pub async fn apply(
    pool: &SqlitePool,
    node_id: &str,
    transition: &str,
    key: &str,
    result: &str,
) -> Result<Applied, NodeRunError> {
    Ok(claim(pool, node_id, transition, key, result).await?)
}

/// Marks `failed` every `running` node of a tree job whose run is no longer live (its status is not
/// in `LIVE_RUN_STATUSES`, or its row is gone), naming the run in the summary. Returns the ids.
///
/// Spared: nodes with no `run_id`, nodes outside a tree job, and the top branch node of a branch
/// that may not close yet (see `branch_node_may_close`) and the root of the job, both while the job
/// is not finished (they stay `running` after reporting, until the driver closes them), and every
/// node of a job whose manifest cannot be read (named in the feed, once per job).
pub async fn reconcile_running(pool: &SqlitePool) -> Result<Vec<String>, NodeRunError> {
    let candidates: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, tree_job_id, run_id FROM work_nodes
         WHERE status = 'running' AND tree_job_id IS NOT NULL AND run_id IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;

    let mut jobs: HashMap<String, Option<TreeJob>> = HashMap::new();
    let mut corrupt: HashSet<String> = HashSet::new();
    let mut marked = Vec::new();
    for (id, job_id, run_id) in candidates {
        let Ok(run_number) = run_id.parse::<i64>() else {
            continue;
        };
        let status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_number)
            .fetch_optional(pool)
            .await?;
        if status
            .as_deref()
            .is_some_and(|status| LIVE_RUN_STATUSES.contains(&status))
        {
            continue;
        }
        if corrupt.contains(&job_id) {
            continue;
        }
        if !jobs.contains_key(&job_id) {
            match store::load(pool, &job_id).await {
                Ok(loaded) => {
                    jobs.insert(job_id.clone(), loaded);
                }
                Err(error @ StoreError::Manifest(_)) => {
                    // Whether the node may be failed depends on a manifest nobody can read, so it
                    // is left as it is, and the owner is told once per job.
                    store::note_corrupt(pool, &job_id, &error).await;
                    corrupt.insert(job_id);
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(Some(job)) = jobs.get(&job_id)
            && job.state != JOB_TERMINAL_STATE
            && (job.root_node_id == id
                || job.manifest.branches.values().any(|branch| {
                    branch.node_id.as_deref() == Some(id.as_str()) && !branch_node_may_close(branch)
                }))
        {
            continue;
        }
        let now = chrono::Utc::now().to_rfc3339();
        let summary = format!("run {run_id} is no longer live; the node cannot still be running");
        let changed = sqlx::query(
            "UPDATE work_nodes SET status = 'failed', summary = ?, closed_at = ?, updated_at = ?
             WHERE id = ? AND status = 'running'",
        )
        .bind(&summary)
        .bind(&now)
        .bind(&now)
        .bind(&id)
        .execute(pool)
        .await?
        .rows_affected();
        if changed == 1 {
            marked.push(id);
        }
    }
    Ok(marked)
}

/// What `create_tree_node_run` needs for a node that belongs to a tree job: the project id, the
/// project root (from the job's manifest) and the worktree the node was handed. `None` for a node
/// with no tree job, which keeps the plain launch. Holds no connection after it returns.
async fn tree_launch_context(
    pool: &SqlitePool,
    node_id: &str,
    project_id: &str,
    worktree: &str,
) -> Result<
    Option<(
        String,
        String,
        crate::execution::runs::TreeWorktree,
    )>,
    NodeRunError,
> {
    let tree_job_id: Option<String> =
        sqlx::query_scalar("SELECT tree_job_id FROM work_nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    let Some(tree_job_id) = tree_job_id else {
        return Ok(None);
    };
    let Some(job) = store::load(pool, &tree_job_id).await? else {
        return Ok(None);
    };
    // The branch only feeds text (the feed line and the info record), so any non-empty name will do
    // when the node is not bound to a branch yet.
    let branch = job
        .manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(node_id))
        .map(|branch| branch.git_branch.clone().unwrap_or_else(|| branch.name.clone()))
        .unwrap_or_else(|| node_id.to_owned());
    Ok(Some((
        project_id.to_owned(),
        job.manifest.project_root,
        crate::execution::runs::TreeWorktree {
            worktree_path: worktree.to_owned(),
            branch,
        },
    )))
}

/// Starts the agent run behind a node exactly once per `(node_id, key)` and returns its run id.
///
/// Order: the node must exist; the key is reserved with an empty result in `tree_transitions` (no
/// connection held); the run is created; the run id becomes the stored result and the node's
/// `run_id` and `session_id` are linked. A replay returns the stored id and touches nothing.
///
/// Known limit, deliberately not recovered here: a caller that finds the key reserved with an empty
/// result gets `NodeRunError::Unresolved`, and that reservation stays after a crash between the
/// reservation and the run id being stored, or after `create_run_inner` fails. This function alone
/// leaves the key spent. `store::launch_recovering` clears a key whose reservation its own failing
/// call made, and `store::resume_all` clears the remaining empty reservations at boot.
pub async fn launch(
    state: &AppState,
    node_id: &str,
    key: &str,
    prompt: String,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: &str,
) -> Result<i64, NodeRunError> {
    let pool = &state.pool;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM work_nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(pool)
        .await?;
    if exists.is_none() {
        return Err(NodeRunError::NodeNotFound(node_id.to_owned()));
    }

    let reservation = claim(pool, node_id, LAUNCH, key, "").await?;
    if reservation.replayed {
        return reservation
            .result
            .parse::<i64>()
            .map_err(|_| NodeRunError::Unresolved {
                node_id: node_id.to_owned(),
                key: key.to_owned(),
            });
    }

    // A node normally inherits its worktree cwd. Keep legacy cwd-less shadow launches unchanged:
    // without a worktree there is neither a box nor a handoff location for the node protocol.
    let prompt = match cwd.as_ref() {
        Some(_) => node_prompt(node_id, &prompt),
        None => prompt,
    };
    // A "worktree" launch with a cwd is a tree run: the cwd is the tree's own worktree, so it is
    // created as such (no second checkout, no gate, the project's classifier hook inherited from
    // the project root). Any other launch keeps the plain path, including "worktree" with no cwd,
    // which `create_run_with_policy` refuses as `Invalid`.
    let tree_run = match (mode, project_id.as_ref(), cwd.as_ref()) {
        ("worktree", Some(project_id), Some(worktree)) => {
            tree_launch_context(pool, node_id, project_id, worktree).await?
        }
        _ => None,
    };
    let run_id = match tree_run {
        Some((project_id, project_root, tree)) => {
            crate::execution::runs::create_tree_node_run(
                state,
                prompt,
                project_id,
                project_root,
                tree,
            )
            .await?
        }
        None => {
            crate::execution::runs::create_run_with_worktree_box(
                state,
                prompt,
                project_id,
                cwd,
                mode,
                false,
            )
            .await?
        }
    };

    sqlx::query(
        "UPDATE tree_transitions SET result = ? WHERE node_id = ? AND transition = ? AND key = ?",
    )
    .bind(run_id.to_string())
    .bind(node_id)
    .bind(LAUNCH)
    .bind(key)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE work_nodes
         SET run_id = ?, session_id = (SELECT session_id FROM runs WHERE id = ?), updated_at = ?
         WHERE id = ?",
    )
    .bind(run_id.to_string())
    .bind(run_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(node_id)
    .execute(pool)
    .await?;
    Ok(run_id)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::SqlitePool;

    use super::*;
    use crate::state::AppState;
    use crate::tree_job::model::{BRANCH_STATES, Branch, Manifest};
    use crate::tree_job::store::{TreeJob, save};

    const CREATED: &str = "2026-10-30T10:00:00+00:00";
    const JOB: &str = "job-1";
    /// High enough that a summary containing it cannot be a coincidence of small numbers.
    const DEAD_RUN: i64 = 4711;

    /// status, summary, run_id, session_id, updated_at, closed_at.
    type NodeRow = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
    );

    async fn seed_node(
        pool: &SqlitePool,
        id: &str,
        status: &str,
        tree_job_id: Option<&str>,
        run_id: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id,
                                     tree_job_id, created_at, updated_at)
             VALUES (?, ?, 0, 'leaf', 'title', 'goal', ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(id)
        .bind(status)
        .bind(run_id.map(|run_id| run_id.to_string()))
        .bind(tree_job_id)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_run(pool: &SqlitePool, id: i64, status: &str) {
        sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'node', ?, ?)")
            .bind(id)
            .bind(status)
            .bind(CREATED)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Stores job `id` with the given branches, each as (name, state, node id).
    async fn seed_job(pool: &SqlitePool, id: &str, state: &str, branches: &[(&str, &str, &str)]) {
        let mut manifest = Manifest::new(id, "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        manifest.state = state.to_owned();
        for (name, branch_state, node_id) in branches {
            let mut branch = Branch::new(name, &[], "code", None);
            branch.state = (*branch_state).to_owned();
            branch.node_id = Some((*node_id).to_owned());
            manifest.branches.insert((*name).to_owned(), branch);
        }
        let job = TreeJob {
            id: id.to_owned(),
            root_node_id: format!("{id}-root"),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: state.to_owned(),
            manifest,
            contract: None,
            created_at: CREATED.to_owned(),
            updated_at: CREATED.to_owned(),
        };
        save(pool, &job).await.unwrap();
    }

    async fn pool_with_job() -> SqlitePool {
        let pool = crate::testdb::fresh_pool().await;
        seed_job(&pool, JOB, "open", &[]).await;
        pool
    }

    async fn node_row(pool: &SqlitePool, id: &str) -> NodeRow {
        sqlx::query_as(
            "SELECT status, summary, run_id, session_id, updated_at, closed_at
             FROM work_nodes WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn transition_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn run_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn reconciled(pool: &SqlitePool) -> Vec<String> {
        let mut marked = reconcile_running(pool).await.unwrap();
        marked.sort();
        marked
    }

    async fn assert_failed_naming_run(pool: &SqlitePool, id: &str, run_id: i64) {
        let (status, summary, ..) = node_row(pool, id).await;
        assert_eq!(status, "failed", "node {id}");
        let summary = summary.unwrap_or_else(|| panic!("node {id} has no summary"));
        assert!(
            summary.contains(&run_id.to_string()),
            "node {id}: summary {summary:?} does not name run {run_id}"
        );
    }

    /// A state whose fake agent stays alive for the test's length. The canned session id is cleared
    /// so that the run keeps the one its row was created with: the fake would otherwise overwrite it
    /// with a fixed `fake-session-id`, which no real session resembles.
    async fn launch_state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    async fn launch_shadow(state: &AppState, node_id: &str, key: &str) -> i64 {
        launch(state, node_id, key, "do it".to_owned(), None, None, "shadow")
            .await
            .unwrap()
    }

    // ----- apply -----

    #[tokio::test]
    async fn apply_first_call_is_not_a_replay_and_writes_one_row() {
        let pool = pool_with_job().await;
        seed_node(&pool, "node-1", "running", Some(JOB), None).await;

        let applied: Result<Applied, NodeRunError> =
            apply(&pool, "node-1", "close", "key-1", "the result").await;

        assert_eq!(
            applied.unwrap(),
            Applied {
                result: "the result".to_owned(),
                replayed: false,
            }
        );
        assert_eq!(transition_rows(&pool).await, 1);
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT result FROM tree_transitions
             WHERE node_id = 'node-1' AND transition = 'close' AND key = 'key-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored.as_deref(), Some("the result"));
    }

    #[tokio::test]
    async fn apply_replay_returns_the_stored_result_even_when_given_another() {
        let pool = pool_with_job().await;
        seed_node(&pool, "node-1", "running", Some(JOB), None).await;
        apply(&pool, "node-1", "close", "key-1", "the first result")
            .await
            .unwrap();

        let replay = apply(&pool, "node-1", "close", "key-1", "a different result")
            .await
            .unwrap();

        assert_eq!(
            replay,
            Applied {
                result: "the first result".to_owned(),
                replayed: true,
            }
        );
        assert_eq!(transition_rows(&pool).await, 1);
    }

    #[tokio::test]
    async fn apply_with_another_key_transition_or_node_is_a_new_row() {
        let pool = pool_with_job().await;
        seed_node(&pool, "node-1", "running", Some(JOB), None).await;
        seed_node(&pool, "node-2", "running", Some(JOB), None).await;
        apply(&pool, "node-1", "close", "key-1", "base")
            .await
            .unwrap();

        for (node, transition, key, result) in [
            ("node-1", "close", "key-2", "other key"),
            ("node-1", "launch", "key-1", "other transition"),
            ("node-2", "close", "key-1", "other node"),
        ] {
            let applied = apply(&pool, node, transition, key, result).await.unwrap();
            assert_eq!(
                applied,
                Applied {
                    result: result.to_owned(),
                    replayed: false,
                },
                "{result}"
            );
        }

        assert_eq!(transition_rows(&pool).await, 4);
    }

    /// On a pool with several connections, so that a read-then-insert really can lose the race.
    /// Several rounds, because one round may happen not to overlap.
    #[tokio::test]
    async fn apply_called_concurrently_has_one_winner_and_one_row() {
        let pool = crate::testdb::fresh_shared_pool().await;
        seed_node(&pool, "node-1", "running", None, None).await;

        for round in 0..10 {
            let key = format!("key-{round}");
            let (left, right) = tokio::join!(
                apply(&pool, "node-1", "close", &key, "left"),
                apply(&pool, "node-1", "close", &key, "right"),
            );
            let (left, right) = (left.unwrap(), right.unwrap());

            assert_eq!(left.result, right.result, "round {round}");
            assert!(
                left.replayed != right.replayed,
                "round {round}: exactly one call must be the first, got {left:?} and {right:?}"
            );
            let rows: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions WHERE key = ?")
                    .bind(&key)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(rows, 1, "round {round}");
        }
    }

    // ----- reconcile_running -----

    #[tokio::test]
    async fn reconcile_fails_a_running_node_whose_run_was_interrupted() {
        let pool = pool_with_job().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_node(&pool, "node-1", "running", Some(JOB), Some(DEAD_RUN)).await;

        assert_eq!(reconciled(&pool).await, ["node-1"]);

        assert_failed_naming_run(&pool, "node-1", DEAD_RUN).await;
    }

    #[tokio::test]
    async fn reconcile_fails_a_running_node_whose_run_has_finished() {
        let pool = pool_with_job().await;
        let mut expected = Vec::new();
        for (offset, status) in ["completed", "failed", "cancelled", "timed_out", "superseded"]
            .into_iter()
            .enumerate()
        {
            let run_id = DEAD_RUN + offset as i64;
            let node = format!("node-{status}");
            seed_run(&pool, run_id, status).await;
            seed_node(&pool, &node, "running", Some(JOB), Some(run_id)).await;
            expected.push((node, run_id));
        }

        let marked = reconciled(&pool).await;

        let mut ids: Vec<String> = expected.iter().map(|(node, _)| node.clone()).collect();
        ids.sort();
        assert_eq!(marked, ids);
        for (node, run_id) in &expected {
            assert_failed_naming_run(&pool, node, *run_id).await;
        }
    }

    #[tokio::test]
    async fn reconcile_fails_a_running_node_whose_run_row_does_not_exist() {
        let pool = pool_with_job().await;
        seed_node(&pool, "node-1", "running", Some(JOB), Some(DEAD_RUN)).await;

        assert_eq!(reconciled(&pool).await, ["node-1"]);

        assert_failed_naming_run(&pool, "node-1", DEAD_RUN).await;
    }

    /// The pair is `execution::concurrency::LIVE_RUN_STATUSES`.
    #[tokio::test]
    async fn reconcile_leaves_a_node_whose_run_is_live() {
        let pool = pool_with_job().await;
        seed_run(&pool, DEAD_RUN, "running").await;
        seed_node(&pool, "node-running", "running", Some(JOB), Some(DEAD_RUN)).await;
        seed_run(&pool, DEAD_RUN + 1, "awaiting_approval").await;
        seed_node(&pool, "node-awaiting", "running", Some(JOB), Some(DEAD_RUN + 1)).await;
        let before = (
            node_row(&pool, "node-running").await,
            node_row(&pool, "node-awaiting").await,
        );

        assert!(reconciled(&pool).await.is_empty());

        let after = (
            node_row(&pool, "node-running").await,
            node_row(&pool, "node-awaiting").await,
        );
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn reconcile_leaves_a_node_outside_any_tree_job_even_with_a_dead_run() {
        let pool = pool_with_job().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_node(&pool, "node-1", "running", None, Some(DEAD_RUN)).await;
        let before = node_row(&pool, "node-1").await;

        assert!(reconciled(&pool).await.is_empty());

        assert_eq!(node_row(&pool, "node-1").await, before);
    }

    #[tokio::test]
    async fn reconcile_leaves_a_node_with_no_run() {
        let pool = pool_with_job().await;
        seed_node(&pool, "node-1", "running", Some(JOB), None).await;
        let before = node_row(&pool, "node-1").await;

        assert!(reconciled(&pool).await.is_empty());

        assert_eq!(node_row(&pool, "node-1").await, before);
    }

    /// The parent's reading of R3, not a sentence of the spec: the top branch node of a live job
    /// closes only when its branch is `merged` or `dropped`, so a dead run behind it is no reason to
    /// fail it. `parked` is among the states kept, although `model::TERMINAL_STATES` lists it. The
    /// leaf proves the exemption belongs to the branch node and not to the whole job.
    #[tokio::test]
    async fn reconcile_leaves_the_top_branch_node_of_a_live_job_with_a_dead_run() {
        let pool = crate::testdb::fresh_pool().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        let open: Vec<&str> = BRANCH_STATES
            .into_iter()
            .filter(|state| !matches!(*state, "merged" | "dropped"))
            .collect();
        let names: Vec<(String, String)> = open
            .iter()
            .map(|state| (format!("branch-{state}"), format!("node-{state}")))
            .collect();
        let branches: Vec<(&str, &str, &str)> = names
            .iter()
            .zip(&open)
            .map(|((name, node), state)| (name.as_str(), *state, node.as_str()))
            .collect();
        seed_job(&pool, JOB, "open", &branches).await;
        for (_, node) in &names {
            seed_node(&pool, node, "running", Some(JOB), Some(DEAD_RUN)).await;
        }
        seed_node(&pool, "leaf", "running", Some(JOB), Some(DEAD_RUN)).await;

        assert_eq!(reconciled(&pool).await, ["leaf"]);

        for (_, node) in &names {
            let (status, summary, ..) = node_row(&pool, node).await;
            assert_eq!(status, "running", "node {node}");
            assert_eq!(summary, None, "node {node}");
        }
    }

    /// The parent's reading of R3, as above. `model::JOB_TERMINAL_STATE` documents a parked job as
    /// still live, so its top branch node is held like an open job's.
    #[tokio::test]
    async fn reconcile_leaves_the_top_branch_node_of_a_parked_job_with_a_dead_run() {
        let pool = crate::testdb::fresh_pool().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_job(&pool, JOB, "parked", &[("api", "building", "branch-node")]).await;
        seed_node(&pool, "branch-node", "running", Some(JOB), Some(DEAD_RUN)).await;
        let before = node_row(&pool, "branch-node").await;

        assert!(reconciled(&pool).await.is_empty());

        assert_eq!(node_row(&pool, "branch-node").await, before);
    }

    /// The root has no branch, yet it too may sit with a dead run while its job is unfinished: the
    /// driver relaunches it. `seed_job` names the root node `<job>-root`. Once the job is finished
    /// the exemption is over, and the leaf proves it belongs to the root alone.
    #[tokio::test]
    async fn reconcile_running_spares_the_root_of_an_unfinished_job() {
        let pool = crate::testdb::fresh_pool().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_job(&pool, JOB, "open", &[]).await;
        seed_job(&pool, "job-2", JOB_TERMINAL_STATE, &[]).await;
        seed_node(&pool, "job-1-root", "running", Some(JOB), Some(DEAD_RUN)).await;
        seed_node(&pool, "job-2-root", "running", Some("job-2"), Some(DEAD_RUN)).await;
        seed_node(&pool, "leaf", "running", Some(JOB), Some(DEAD_RUN)).await;
        let before = node_row(&pool, "job-1-root").await;

        assert_eq!(reconciled(&pool).await, ["job-2-root", "leaf"]);

        assert_eq!(node_row(&pool, "job-1-root").await, before);
        assert_failed_naming_run(&pool, "job-2-root", DEAD_RUN).await;
        assert_failed_naming_run(&pool, "leaf", DEAD_RUN).await;
    }

    /// The other side of the parent's reading of R3: once the branch is `merged` or `dropped` the
    /// exemption is over, and a dead run fails the node as it would any other.
    #[tokio::test]
    async fn reconcile_fails_the_top_branch_node_once_its_branch_is_merged_or_dropped() {
        let pool = crate::testdb::fresh_pool().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_job(
            &pool,
            JOB,
            "open",
            &[
                ("api", "merged", "node-merged"),
                ("web", "dropped", "node-dropped"),
            ],
        )
        .await;
        seed_node(&pool, "node-merged", "running", Some(JOB), Some(DEAD_RUN)).await;
        seed_node(&pool, "node-dropped", "running", Some(JOB), Some(DEAD_RUN)).await;

        assert_eq!(reconciled(&pool).await, ["node-dropped", "node-merged"]);

        assert_failed_naming_run(&pool, "node-merged", DEAD_RUN).await;
        assert_failed_naming_run(&pool, "node-dropped", DEAD_RUN).await;
    }

    #[tokio::test]
    async fn reconcile_leaves_nodes_that_are_not_running() {
        let pool = pool_with_job().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        let statuses = ["planned", "done", "blocked", "handoff", "failed"];
        let mut before = Vec::new();
        for status in statuses {
            seed_node(&pool, status, status, Some(JOB), Some(DEAD_RUN)).await;
            before.push(node_row(&pool, status).await);
        }

        assert!(reconciled(&pool).await.is_empty());

        let mut after = Vec::new();
        for status in statuses {
            after.push(node_row(&pool, status).await);
        }
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn reconcile_a_second_time_returns_nothing_and_changes_nothing() {
        let pool = pool_with_job().await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_node(&pool, "node-1", "running", Some(JOB), Some(DEAD_RUN)).await;
        assert_eq!(reconciled(&pool).await, ["node-1"]);
        let before = node_row(&pool, "node-1").await;

        assert!(reconciled(&pool).await.is_empty());

        assert_eq!(node_row(&pool, "node-1").await, before);
    }

    // ----- launch -----

    #[tokio::test]
    async fn launch_creates_a_run_and_links_run_and_session_on_the_node() {
        let state = launch_state().await;
        seed_node(&state.pool, "node-1", "running", None, None).await;

        let run_id = launch_shadow(&state, "node-1", "key-1").await;

        let (prompt, mode, session): (String, String, Option<String>) =
            sqlx::query_as("SELECT prompt, mode, session_id FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(prompt, "do it");
        assert_eq!(mode, "shadow");
        let session = session.expect("create_run_with gives every run a session id");
        let (_, _, node_run_id, node_session, ..) = node_row(&state.pool, "node-1").await;
        assert_eq!(node_run_id, Some(run_id.to_string()));
        assert_eq!(node_session, Some(session));
    }

    #[tokio::test]
    async fn launch_records_its_key_under_the_launch_transition() {
        let state = launch_state().await;
        seed_node(&state.pool, "node-1", "running", None, None).await;

        launch_shadow(&state, "node-1", "key-1").await;

        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tree_transitions
             WHERE node_id = 'node-1' AND transition = 'launch' AND key = 'key-1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn launch_replay_returns_the_same_run_with_no_second_row() {
        let state = launch_state().await;
        seed_node(&state.pool, "node-1", "running", None, None).await;
        let first = launch_shadow(&state, "node-1", "key-1").await;
        let linked = node_row(&state.pool, "node-1").await;

        let replay = launch_shadow(&state, "node-1", "key-1").await;

        assert_eq!(replay, first);
        assert_eq!(run_rows(&state.pool).await, 1);
        assert_eq!(node_row(&state.pool, "node-1").await.2, linked.2);
        assert_eq!(node_row(&state.pool, "node-1").await.3, linked.3);
    }

    /// R10: each node is a run of its own, with a session of its own.
    #[tokio::test]
    async fn launch_of_another_node_with_another_key_gives_a_distinct_run_and_session() {
        let state = launch_state().await;
        seed_node(&state.pool, "node-1", "running", None, None).await;
        seed_node(&state.pool, "node-2", "running", None, None).await;

        let first = launch_shadow(&state, "node-1", "key-1").await;
        let second = launch_shadow(&state, "node-2", "key-2").await;

        assert_ne!(first, second);
        assert_eq!(run_rows(&state.pool).await, 2);
        let first_session = node_row(&state.pool, "node-1").await.3;
        let second_session = node_row(&state.pool, "node-2").await.3;
        assert!(first_session.is_some());
        assert!(second_session.is_some());
        assert_ne!(first_session, second_session);
    }

    #[tokio::test]
    async fn launch_of_an_unknown_node_is_an_error_and_leaves_no_run() {
        let state = launch_state().await;

        let launched: Result<i64, NodeRunError> = launch(
            &state,
            "no-such-node",
            "key-1",
            "do it".to_owned(),
            None,
            None,
            "shadow",
        )
        .await;

        assert!(launched.is_err());
        assert_eq!(run_rows(&state.pool).await, 0);
    }

    // ----- the tools a node's run is launched with -----

    /// Like `launch_state`, but keeps the fake runner so the launch shape it saw can be read.
    async fn launch_state_with_runner() -> (
        AppState,
        std::sync::Arc<crate::execution::runner::FakeCommandRunner>,
    ) {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        (state, runner)
    }

    /// The MCP config path, job id and allowed tools the runner was handed, once it was handed any.
    async fn seen_launch(
        runner: &crate::execution::runner::FakeCommandRunner,
    ) -> (
        Option<std::path::PathBuf>,
        Option<i64>,
        Option<Vec<String>>,
    ) {
        for _ in 0..200 {
            if let Some(seen) = runner.last_job_mcp.lock().unwrap().clone() {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the runner never received a launch");
    }

    #[tokio::test]
    async fn a_node_run_gets_the_worktree_box_for_its_cwd() {
        let (state, runner) = launch_state_with_runner().await;
        seed_node(&state.pool, "node-1", "running", Some(JOB), None).await;
        let cwd = crate::execution::git_exec::testkit::space_free_tempdir("nucleos-node-box-");
        let cwd_text = cwd.path().to_string_lossy().into_owned();

        launch(
            &state,
            "node-1",
            "key-1",
            "do it".to_owned(),
            None,
            Some(cwd_text),
            "shadow",
        )
        .await
        .unwrap();

        let (config, ..) = seen_launch(&runner).await;
        let config = config.expect("a node run is launched with --mcp-config");
        let launched_cwd = runner
            .last_cwd
            .lock()
            .unwrap()
            .clone()
            .expect("the run has a cwd");
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config).expect("the config file is on disk"))
                .unwrap();
        assert_eq!(
            written["mcpServers"]["nucleos"]["args"],
            serde_json::json!([
                "--mcp-tools",
                "--box",
                "worktree",
                "--worktree",
                launched_cwd.to_string_lossy()
            ]),
            "the box is the worktree box, bound to the run's own cwd"
        );
    }

    #[tokio::test]
    async fn a_node_runs_tool_policy_allows_the_node_tools() {
        let (state, runner) = launch_state_with_runner().await;
        seed_node(&state.pool, "node-1", "running", Some(JOB), None).await;
        let cwd = crate::execution::git_exec::testkit::space_free_tempdir("nucleos-node-tools-");

        launch(
            &state,
            "node-1",
            "key-1",
            "do it".to_owned(),
            None,
            Some(cwd.path().to_string_lossy().into_owned()),
            "shadow",
        )
        .await
        .unwrap();

        let (_, _, allowed) = seen_launch(&runner).await;
        let allowed = allowed.expect("a node run narrows --allowedTools to a list");
        for tool in ["node_open", "node_close", "node_update", "node_tree", "node_active"] {
            assert!(
                allowed.iter().any(|name| name == tool),
                "{tool} is not allowed: {allowed:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_plain_run_gets_no_worktree_box() {
        let (state, runner) = launch_state_with_runner().await;

        crate::execution::runs::create_run_inner(
            &state,
            "do it".to_owned(),
            None,
            None,
            "shadow",
            false,
        )
        .await
        .unwrap();

        assert_eq!(seen_launch(&runner).await, (None, None, None));
    }

    #[test]
    fn a_node_prompt_names_its_node_id_and_tells_it_to_close() {
        let prompt = node_prompt("node-abc-123", "Implement the parser.");

        assert!(prompt.contains("node-abc-123"), "{prompt}");
        assert!(prompt.contains("node_close"), "{prompt}");
        assert!(prompt.to_lowercase().contains("handoff"), "{prompt}");
        assert!(prompt.contains("Implement the parser."), "{prompt}");
    }
}
