//! Durable storage of tree jobs: one `tree_jobs` row per job, with its manifest as JSON text.

use std::fmt;

use sqlx::SqlitePool;

use super::model::{JOB_TERMINAL_STATE, Manifest};

/// One tree job. `state`, `goal` and `feature_branch` repeat fields of `manifest`; the caller keeps
/// them in step, because this module stores what it is given.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeJob {
    pub id: String,
    pub root_node_id: String,
    pub project_id: Option<String>,
    pub feature_branch: Option<String>,
    pub goal: String,
    pub state: String,
    pub manifest: Manifest,
    pub contract: Option<String>,
    /// RFC 3339 text.
    pub created_at: String,
    /// RFC 3339 text, set by the caller.
    pub updated_at: String,
}

#[derive(Debug)]
pub enum StoreError {
    /// No row has this job id.
    NotFound(String),
    /// A manifest could not be written as JSON, or a stored one could not be read back.
    Manifest(serde_json::Error),
    Db(sqlx::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound(id) => write!(f, "tree job {id} not found"),
            StoreError::Manifest(error) => write!(f, "tree job manifest: {error}"),
            StoreError::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        StoreError::Db(error)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        StoreError::Manifest(error)
    }
}

#[derive(sqlx::FromRow)]
struct JobRow {
    id: String,
    root_node_id: String,
    project_id: Option<String>,
    feature_branch: Option<String>,
    goal: String,
    state: String,
    manifest: String,
    contract: Option<String>,
    created_at: String,
    updated_at: String,
}

impl JobRow {
    fn into_job(self) -> Result<TreeJob, StoreError> {
        Ok(TreeJob {
            manifest: serde_json::from_str(&self.manifest)?,
            id: self.id,
            root_node_id: self.root_node_id,
            project_id: self.project_id,
            feature_branch: self.feature_branch,
            goal: self.goal,
            state: self.state,
            contract: self.contract,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

macro_rules! select_jobs {
    ($tail:literal) => {
        concat!(
            "SELECT id, root_node_id, project_id, feature_branch, goal, state, manifest, contract, \
             created_at, updated_at FROM tree_jobs ",
            $tail
        )
    };
}

/// Inserts a new job. A second job on the same id or root node is an error, not an overwrite.
pub async fn save(pool: &SqlitePool, job: &TreeJob) -> Result<(), StoreError> {
    let manifest = serde_json::to_string(&job.manifest)?;
    sqlx::query(
        "INSERT INTO tree_jobs (id, root_node_id, project_id, feature_branch, goal, state, \
         manifest, contract, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&job.id)
    .bind(&job.root_node_id)
    .bind(&job.project_id)
    .bind(&job.feature_branch)
    .bind(&job.goal)
    .bind(&job.state)
    .bind(manifest)
    .bind(&job.contract)
    .bind(&job.created_at)
    .bind(&job.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn load(pool: &SqlitePool, id: &str) -> Result<Option<TreeJob>, StoreError> {
    let row: Option<JobRow> = sqlx::query_as(select_jobs!("WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(JobRow::into_job).transpose()
}

/// Writes `state`, `manifest` and `updated_at` exactly as the job holds them. Updating a job that
/// does not exist is an error, since otherwise the caller's state would be lost without a word.
pub async fn update(pool: &SqlitePool, job: &TreeJob) -> Result<(), StoreError> {
    let manifest = serde_json::to_string(&job.manifest)?;
    let updated = sqlx::query(
        "UPDATE tree_jobs SET state = ?, manifest = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&job.state)
    .bind(manifest)
    .bind(&job.updated_at)
    .bind(&job.id)
    .execute(pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(StoreError::NotFound(job.id.clone()));
    }
    Ok(())
}

/// Every job that is not over, oldest first. Parked jobs are included, so that boot can resume
/// them; the branch list `TERMINAL_STATES` does not apply to a job.
pub async fn list_non_terminal(pool: &SqlitePool) -> Result<Vec<TreeJob>, StoreError> {
    let rows: Vec<JobRow> =
        sqlx::query_as(select_jobs!("WHERE state != ? ORDER BY created_at, id"))
            .bind(JOB_TERMINAL_STATE)
            .fetch_all(pool)
            .await?;
    rows.into_iter().map(JobRow::into_job).collect()
}

/// What a boot pass found and repaired. It relaunches nothing: no prompt is persisted, so the
/// caller only learns what is free to relaunch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resumed {
    /// Ids of the jobs that are not finished, oldest first (parked ones included).
    pub jobs: Vec<String>,
    /// Running nodes marked `failed` because their run is no longer live.
    pub failed_nodes: Vec<String>,
    /// `(node_id, key)` of every empty launch reservation deleted for a node of an unfinished job.
    pub cleared_launches: Vec<(String, String)>,
}

/// Boot recovery. Call it once, after the runs were reconciled and before anything can call
/// `launch`: it deletes every empty launch reservation of an unfinished job, which is only safe
/// while no call can still be creating a run. Job rows are never written.
///
/// A corrupt manifest makes `list_non_terminal` (and `reconcile_running`) fail whole, so one bad
/// row stops the pass for every job; this keeps the `?` behaviour of both.
pub async fn resume_all(pool: &SqlitePool) -> Result<Resumed, StoreError> {
    let jobs: Vec<String> = list_non_terminal(pool)
        .await?
        .into_iter()
        .map(|job| job.id)
        .collect();
    let failed_nodes = super::node_run::reconcile_running(pool)
        .await
        .map_err(|error| match error {
            super::node_run::NodeRunError::Store(error) => error,
            super::node_run::NodeRunError::Db(error) => StoreError::Db(error),
            other => StoreError::Db(sqlx::Error::Protocol(other.to_string())),
        })?;

    let spent: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.node_id, t.key FROM tree_transitions t
         JOIN work_nodes n ON n.id = t.node_id
         JOIN tree_jobs j ON j.id = n.tree_job_id
         WHERE t.transition = ? AND (t.result IS NULL OR t.result = '') AND j.state != ?
         ORDER BY t.node_id, t.key",
    )
    .bind(super::node_run::LAUNCH)
    .bind(JOB_TERMINAL_STATE)
    .fetch_all(pool)
    .await?;
    let mut cleared_launches = Vec::with_capacity(spent.len());
    for (node_id, key) in spent {
        if clear_spent_launch(pool, &node_id, &key).await? {
            cleared_launches.push((node_id, key));
        }
    }
    Ok(Resumed {
        jobs,
        failed_nodes,
        cleared_launches,
    })
}

/// Deletes the launch reservation `(node_id, key)` when it still holds no run id, and answers
/// whether it did. A row holding a run id is never touched: it is what makes a launch replay-safe.
pub async fn clear_spent_launch(
    pool: &SqlitePool,
    node_id: &str,
    key: &str,
) -> Result<bool, StoreError> {
    let deleted = sqlx::query(
        "DELETE FROM tree_transitions
         WHERE node_id = ? AND transition = ? AND key = ? AND (result IS NULL OR result = '')",
    )
    .bind(node_id)
    .bind(super::node_run::LAUNCH)
    .bind(key)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(deleted == 1)
}

/// `node_run::launch`, but a call whose own `create_run_inner` failed gives its key back, so the
/// same key can start the run later. `Create` can only come from the call that made the
/// reservation (a replay returns before the create), so that is the one case a live daemon may
/// clear; an `Unresolved` reservation may belong to a call still creating its run, and only boot
/// clears that.
pub async fn launch_recovering(
    state: &crate::state::AppState,
    node_id: &str,
    key: &str,
    prompt: String,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: &str,
) -> Result<i64, super::node_run::NodeRunError> {
    let outcome =
        super::node_run::launch(state, node_id, key, prompt, project_id, cwd, mode).await;
    if let Err(super::node_run::NodeRunError::Create(_)) = &outcome {
        // Best effort: the create error is the answer, and a failed clear only leaves the old
        // behaviour (the key stays spent until the next boot).
        let _ = clear_spent_launch(&state.pool, node_id, key).await;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_job::model::{Branch, Manifest};

    use std::time::Duration;

    use crate::execution::runs::CreateRunError;
    use crate::state::AppState;
    use crate::tree_job::node_run::{self, NodeRunError};

    const CREATED: &str = "2026-10-30T10:00:00+00:00";
    const LATER: &str = "2026-10-30T12:00:00+00:00";

    fn sample_job(id: &str, root_node_id: &str, state: &str) -> TreeJob {
        let mut manifest = Manifest::new(id, "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        manifest.state = state.to_owned();
        TreeJob {
            id: id.to_owned(),
            root_node_id: root_node_id.to_owned(),
            project_id: Some("project-1".to_owned()),
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: state.to_owned(),
            manifest,
            contract: Some("the contract".to_owned()),
            created_at: CREATED.to_owned(),
            updated_at: CREATED.to_owned(),
        }
    }

    #[tokio::test]
    async fn save_then_load_returns_the_same_job() {
        let pool = crate::testdb::fresh_pool().await;
        let job = sample_job("job-1", "root-1", "open");

        save(&pool, &job).await.unwrap();

        assert_eq!(load(&pool, "job-1").await.unwrap(), Some(job));
    }

    #[tokio::test]
    async fn load_of_an_unknown_id_returns_none() {
        let pool = crate::testdb::fresh_pool().await;
        save(&pool, &sample_job("job-1", "root-1", "open"))
            .await
            .unwrap();

        assert_eq!(load(&pool, "job-2").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_job_without_its_optional_columns_round_trips_as_none() {
        let pool = crate::testdb::fresh_pool().await;
        let mut job = sample_job("job-1", "root-1", "open");
        job.project_id = None;
        job.feature_branch = None;
        job.contract = None;

        save(&pool, &job).await.unwrap();

        assert_eq!(load(&pool, "job-1").await.unwrap(), Some(job));
    }

    #[tokio::test]
    async fn the_manifest_column_holds_the_manifest_as_json_text() {
        let pool = crate::testdb::fresh_pool().await;
        save(&pool, &sample_job("job-1", "root-1", "open"))
            .await
            .unwrap();

        let text: String = sqlx::query_scalar("SELECT manifest FROM tree_jobs WHERE id = ?")
            .bind("job-1")
            .fetch_one(&pool)
            .await
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(manifest["tree_id"], "job-1");
        assert_eq!(manifest["state"], "open");
        assert_eq!(manifest["version"], 1);
    }

    #[tokio::test]
    async fn update_keeps_the_row_and_changes_state_and_manifest() {
        let pool = crate::testdb::fresh_pool().await;
        let job = sample_job("job-1", "root-1", "open");
        save(&pool, &job).await.unwrap();

        let mut changed = job.clone();
        changed.state = "parked".to_owned();
        changed.manifest.state = "parked".to_owned();
        changed
            .manifest
            .branches
            .insert("api".to_owned(), Branch::new("api", &[], "backend", None));
        changed.updated_at = LATER.to_owned();
        update(&pool, &changed).await.unwrap();

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tree_jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "an update must not add a row");

        let loaded = load(&pool, "job-1").await.unwrap().unwrap();
        assert_eq!(loaded.root_node_id, job.root_node_id);
        assert_eq!(loaded.created_at, job.created_at);
        assert_eq!(loaded, changed);
        assert_ne!(loaded.manifest, job.manifest);
    }

    /// A job's terminal state is `finished`, not the branch list `TERMINAL_STATES`: `tree_state.py`
    /// counts a tree as live while `state != "finished"`, and a parked tree is resumable, so a
    /// parked job has to be listed for the daemon to pick it up again after a restart.
    #[tokio::test]
    async fn listing_returns_only_jobs_that_have_not_finished() {
        let pool = crate::testdb::fresh_pool().await;
        let open = sample_job("job-open", "root-open", "open");
        save(&pool, &open).await.unwrap();
        save(&pool, &sample_job("job-parked", "root-parked", "parked"))
            .await
            .unwrap();
        save(&pool, &sample_job("job-done", "root-done", "finished"))
            .await
            .unwrap();

        let mut listed: Vec<String> = list_non_terminal(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect();
        listed.sort();
        assert_eq!(listed, ["job-open", "job-parked"]);

        let mut finished = open;
        finished.state = "finished".to_owned();
        finished.manifest.state = "finished".to_owned();
        update(&pool, &finished).await.unwrap();

        let listed: Vec<String> = list_non_terminal(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect();
        assert_eq!(listed, ["job-parked"]);
    }

    #[tokio::test]
    async fn a_second_job_on_the_same_root_node_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        let first = sample_job("job-1", "root-1", "open");
        save(&pool, &first).await.unwrap();

        let clash = sample_job("job-2", "root-1", "open");
        assert!(save(&pool, &clash).await.is_err());

        assert_eq!(load(&pool, "job-2").await.unwrap(), None);
        assert_eq!(load(&pool, "job-1").await.unwrap(), Some(first));
    }

    /// `fresh_shared_pool` rather than `fresh_pool`: an in-memory database dies with its pool, so
    /// only a file-backed one can show that a job outlives the pool that saved it.
    #[tokio::test]
    async fn a_job_survives_closing_and_reopening_the_pool() {
        let pool = crate::testdb::fresh_shared_pool().await;
        let job = sample_job("job-1", "root-1", "open");
        save(&pool, &job).await.unwrap();

        let options = (*pool.connect_options()).clone();
        pool.close().await;
        let reopened = sqlx::SqlitePool::connect_with(options).await.unwrap();

        assert_eq!(load(&reopened, "job-1").await.unwrap(), Some(job));
        let listed = list_non_terminal(&reopened).await.unwrap();
        assert_eq!(listed.len(), 1);
        reopened.close().await;
    }

    // ----- boot: resume_all, clear_spent_launch, launch_recovering -----

    /// A mode `create_run_inner` refuses when it has no project and no cwd, before any run row
    /// exists: the deterministic way to make a create fail.
    const REFUSED_WITHOUT_A_PROJECT: &str = "worktree";
    /// High enough not to collide with an id the fake agent's runs get.
    const DEAD_RUN: i64 = 4711;
    const LIVE_RUN: i64 = 4712;

    /// Stores job `id` with the given `created_at`, and returns it as stored.
    async fn store_job(pool: &SqlitePool, id: &str, state: &str, created_at: &str) -> TreeJob {
        let mut job = sample_job(id, &format!("{id}-root"), state);
        job.created_at = created_at.to_owned();
        save(pool, &job).await.unwrap();
        job
    }

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

    /// What a `launch` leaves behind when it dies, or is still working, after reserving its key.
    async fn reserve(pool: &SqlitePool, node_id: &str, key: &str) {
        node_run::apply(pool, node_id, "launch", key, "")
            .await
            .unwrap();
    }

    /// The stored result of a transition, or `None` when it has no row.
    async fn stored_result(
        pool: &SqlitePool,
        node_id: &str,
        transition: &str,
        key: &str,
    ) -> Option<String> {
        let stored: Option<Option<String>> = sqlx::query_scalar(
            "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = ? AND key = ?",
        )
        .bind(node_id)
        .bind(transition)
        .bind(key)
        .fetch_optional(pool)
        .await
        .unwrap();
        stored.flatten()
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

    async fn node_status(pool: &SqlitePool, id: &str) -> String {
        sqlx::query_scalar("SELECT status FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_run_id(pool: &SqlitePool, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A state whose fake agent stays alive for the test's length, with one open job `job-1`
    /// holding the running node `node-1`, which has no run yet.
    async fn state_with_node() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(5)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        store_job(&state.pool, "job-1", "open", CREATED).await;
        seed_node(&state.pool, "node-1", "running", Some("job-1"), None).await;
        state
    }

    async fn recover_in_mode(state: &AppState, key: &str, mode: &str) -> Result<i64, NodeRunError> {
        launch_recovering(state, "node-1", key, "do it".to_owned(), None, None, mode).await
    }

    async fn recover(state: &AppState, key: &str) -> Result<i64, NodeRunError> {
        recover_in_mode(state, key, "shadow").await
    }

    // ----- resume_all -----

    /// Decision 5: only `finished` is terminal, so a parked job resumes at boot like an open one.
    #[tokio::test]
    async fn resume_all_lists_unfinished_jobs_oldest_first_and_never_a_finished_one() {
        let pool = crate::testdb::fresh_pool().await;
        // Inserted out of age order, so that insertion order cannot pass for "oldest first".
        store_job(
            &pool,
            "job-building",
            "building",
            "2026-10-30T10:00:03+00:00",
        )
        .await;
        store_job(&pool, "job-open", "open", "2026-10-30T10:00:02+00:00").await;
        store_job(&pool, "job-parked", "parked", "2026-10-30T10:00:01+00:00").await;
        store_job(&pool, "job-done", "finished", "2026-10-30T10:00:00+00:00").await;

        let resumed: Resumed = resume_all(&pool).await.unwrap();

        assert_eq!(resumed.jobs, ["job-parked", "job-open", "job-building"]);
    }

    /// Resuming is not a transition of the job: even when the pass fails a node of the job and
    /// clears one of its launches, the job's own row keeps its state, `updated_at` and park record.
    #[tokio::test]
    async fn resume_all_leaves_every_job_row_as_it_was() {
        let pool = crate::testdb::fresh_pool().await;
        let mut parked = sample_job("job-parked", "job-parked-root", "parked");
        parked.manifest.park = Some(serde_json::json!({"needs": "a decision"}));
        save(&pool, &parked).await.unwrap();
        let open = store_job(&pool, "job-open", "open", CREATED).await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_node(
            &pool,
            "node-dead",
            "running",
            Some("job-parked"),
            Some(DEAD_RUN),
        )
        .await;
        seed_node(&pool, "node-waiting", "running", Some("job-parked"), None).await;
        reserve(&pool, "node-waiting", "key-1").await;

        let resumed = resume_all(&pool).await.unwrap();

        assert_eq!(resumed.failed_nodes, ["node-dead"]);
        assert_eq!(
            resumed.cleared_launches,
            [("node-waiting".to_owned(), "key-1".to_owned())]
        );
        let reloaded = load(&pool, "job-parked").await.unwrap().unwrap();
        assert_eq!(reloaded.state, "parked");
        assert_eq!(reloaded.updated_at, parked.updated_at);
        assert_eq!(
            reloaded.manifest.park,
            Some(serde_json::json!({"needs": "a decision"}))
        );
        assert_eq!(reloaded, parked);
        assert_eq!(load(&pool, "job-open").await.unwrap(), Some(open));
    }

    #[tokio::test]
    async fn resume_all_fails_a_running_node_whose_run_was_interrupted_and_spares_a_live_one() {
        let pool = crate::testdb::fresh_pool().await;
        store_job(&pool, "job-1", "open", CREATED).await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_run(&pool, LIVE_RUN, "running").await;
        seed_node(&pool, "node-dead", "running", Some("job-1"), Some(DEAD_RUN)).await;
        seed_node(&pool, "node-live", "running", Some("job-1"), Some(LIVE_RUN)).await;

        let resumed = resume_all(&pool).await.unwrap();

        assert_eq!(resumed.failed_nodes, ["node-dead"]);
        assert_eq!(node_status(&pool, "node-dead").await, "failed");
        assert_eq!(node_status(&pool, "node-live").await, "running");
    }

    #[tokio::test]
    async fn a_second_resume_all_reports_the_same_jobs_and_nothing_else() {
        let pool = crate::testdb::fresh_pool().await;
        store_job(&pool, "job-1", "open", CREATED).await;
        store_job(&pool, "job-2", "parked", LATER).await;
        seed_run(&pool, DEAD_RUN, "interrupted").await;
        seed_node(&pool, "node-dead", "running", Some("job-1"), Some(DEAD_RUN)).await;
        seed_node(&pool, "node-waiting", "running", Some("job-1"), None).await;
        reserve(&pool, "node-waiting", "key-1").await;

        let first = resume_all(&pool).await.unwrap();
        assert_eq!(first.jobs, ["job-1", "job-2"]);
        assert_eq!(first.failed_nodes, ["node-dead"]);
        assert_eq!(first.cleared_launches.len(), 1);

        let second = resume_all(&pool).await.unwrap();

        assert_eq!(
            second,
            Resumed {
                jobs: first.jobs,
                failed_nodes: Vec::new(),
                cleared_launches: Vec::new(),
            }
        );
    }

    /// Decision 4: after a restart nothing can still be creating the run of an empty reservation,
    /// so boot frees the key of every job that is not over, a parked one included.
    #[tokio::test]
    async fn resume_all_clears_the_empty_launches_of_nodes_in_unfinished_jobs() {
        let pool = crate::testdb::fresh_pool().await;
        store_job(&pool, "job-open", "open", CREATED).await;
        store_job(&pool, "job-parked", "parked", CREATED).await;
        seed_node(&pool, "node-open", "running", Some("job-open"), None).await;
        seed_node(&pool, "node-parked", "running", Some("job-parked"), None).await;
        reserve(&pool, "node-open", "key-1").await;
        reserve(&pool, "node-open", "key-2").await;
        reserve(&pool, "node-parked", "key-1").await;

        let mut cleared = resume_all(&pool).await.unwrap().cleared_launches;
        cleared.sort();

        assert_eq!(
            cleared,
            [
                ("node-open".to_owned(), "key-1".to_owned()),
                ("node-open".to_owned(), "key-2".to_owned()),
                ("node-parked".to_owned(), "key-1".to_owned()),
            ]
        );
        assert_eq!(transition_rows(&pool).await, 0);
    }

    #[tokio::test]
    async fn resume_all_keeps_every_row_that_is_not_a_spent_launch_of_an_unfinished_job() {
        let pool = crate::testdb::fresh_pool().await;
        store_job(&pool, "job-open", "open", CREATED).await;
        store_job(&pool, "job-done", "finished", CREATED).await;
        seed_node(&pool, "node-open", "running", Some("job-open"), None).await;
        seed_node(&pool, "node-done", "running", Some("job-done"), None).await;
        seed_node(&pool, "node-alone", "running", None, None).await;
        node_run::apply(&pool, "node-open", "launch", "key-holding", "42")
            .await
            .unwrap();
        node_run::apply(&pool, "node-open", "close", "key-other", "")
            .await
            .unwrap();
        reserve(&pool, "node-done", "key-finished").await;
        reserve(&pool, "node-alone", "key-alone").await;

        let resumed = resume_all(&pool).await.unwrap();

        assert!(
            resumed.cleared_launches.is_empty(),
            "{:?}",
            resumed.cleared_launches
        );
        assert_eq!(transition_rows(&pool).await, 4);
        assert_eq!(
            stored_result(&pool, "node-open", "launch", "key-holding")
                .await
                .as_deref(),
            Some("42")
        );
    }

    // ----- clear_spent_launch -----

    #[tokio::test]
    async fn clear_spent_launch_deletes_only_the_empty_row_it_names() {
        let pool = crate::testdb::fresh_pool().await;
        reserve(&pool, "node-1", "key-1").await;
        reserve(&pool, "node-1", "key-2").await;
        reserve(&pool, "node-2", "key-1").await;

        let cleared: bool = clear_spent_launch(&pool, "node-1", "key-1").await.unwrap();

        assert!(cleared);
        assert_eq!(
            stored_result(&pool, "node-1", "launch", "key-1").await,
            None
        );
        assert_eq!(transition_rows(&pool).await, 2);
    }

    /// The row is what makes a launch replay-safe: losing a run id would start a second run.
    #[tokio::test]
    async fn clear_spent_launch_refuses_a_row_holding_a_run_id() {
        let pool = crate::testdb::fresh_pool().await;
        node_run::apply(&pool, "node-1", "launch", "key-1", "42")
            .await
            .unwrap();

        let cleared = clear_spent_launch(&pool, "node-1", "key-1").await.unwrap();

        assert!(!cleared);
        assert_eq!(
            stored_result(&pool, "node-1", "launch", "key-1")
                .await
                .as_deref(),
            Some("42")
        );
    }

    #[tokio::test]
    async fn clear_spent_launch_refuses_an_empty_row_of_another_transition() {
        let pool = crate::testdb::fresh_pool().await;
        node_run::apply(&pool, "node-1", "close", "key-1", "")
            .await
            .unwrap();

        let cleared = clear_spent_launch(&pool, "node-1", "key-1").await.unwrap();

        assert!(!cleared);
        assert_eq!(transition_rows(&pool).await, 1);
    }

    #[tokio::test]
    async fn clear_spent_launch_of_an_absent_row_answers_false_and_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        reserve(&pool, "node-1", "key-other").await;
        reserve(&pool, "node-other", "key-1").await;

        let cleared = clear_spent_launch(&pool, "node-1", "key-1").await.unwrap();

        assert!(!cleared);
        assert_eq!(transition_rows(&pool).await, 2);
    }

    // ----- launch_recovering -----

    /// The whole recovery with the real `launch`: a key that answers `Unresolved` for ever until a
    /// boot frees it, and then starts exactly one run.
    #[tokio::test]
    async fn a_boot_turns_an_unresolved_launch_key_into_one_run() {
        let state = state_with_node().await;
        let pool = &state.pool;
        reserve(pool, "node-1", "key-1").await;

        let before_boot = node_run::launch(
            &state,
            "node-1",
            "key-1",
            "do it".to_owned(),
            None,
            None,
            "shadow",
        )
        .await;
        assert!(
            matches!(before_boot, Err(NodeRunError::Unresolved { .. })),
            "{before_boot:?}"
        );
        assert_eq!(run_rows(pool).await, 0);

        let resumed = resume_all(pool).await.unwrap();
        assert_eq!(
            resumed.cleared_launches,
            [("node-1".to_owned(), "key-1".to_owned())]
        );

        let run_id = recover(&state, "key-1").await.unwrap();

        assert_eq!(run_rows(pool).await, 1);
        assert_eq!(node_run_id(pool, "node-1").await, Some(run_id.to_string()));
        assert_eq!(
            stored_result(pool, "node-1", "launch", "key-1").await,
            Some(run_id.to_string())
        );
    }

    /// Without the clear this is the key that answers `Unresolved` for ever: the call that made the
    /// reservation knows its own create failed, so it gives the key back.
    #[tokio::test]
    async fn a_failing_create_gives_the_key_back_and_the_same_key_then_makes_one_run() {
        let state = state_with_node().await;
        let pool = &state.pool;

        let failed = recover_in_mode(&state, "key-1", REFUSED_WITHOUT_A_PROJECT).await;

        assert!(
            matches!(
                failed,
                Err(NodeRunError::Create(CreateRunError::Invalid(_)))
            ),
            "{failed:?}"
        );
        assert_eq!(transition_rows(pool).await, 0, "the reservation is gone");
        assert_eq!(run_rows(pool).await, 0);
        assert_eq!(node_run_id(pool, "node-1").await, None);

        let run_id = recover(&state, "key-1").await.unwrap();

        assert_eq!(run_rows(pool).await, 1);
        assert_eq!(node_run_id(pool, "node-1").await, Some(run_id.to_string()));
    }

    /// A replay never reaches the create, so input that would fail it cannot cost the stored id.
    #[tokio::test]
    async fn launch_recovering_replay_returns_the_same_run_with_no_second_row() {
        let state = state_with_node().await;
        let pool = &state.pool;
        let first = recover(&state, "key-1").await.unwrap();

        let replay = recover(&state, "key-1").await.unwrap();
        let replay_with_bad_input = recover_in_mode(&state, "key-1", REFUSED_WITHOUT_A_PROJECT)
            .await
            .unwrap();

        assert_eq!(replay, first);
        assert_eq!(replay_with_bad_input, first);
        assert_eq!(run_rows(pool).await, 1);
        assert_eq!(
            stored_result(pool, "node-1", "launch", "key-1").await,
            Some(first.to_string())
        );
    }

    /// In flight is not spent. With no boot in between, an empty reservation may belong to another
    /// call that is creating its run right now, and a live daemon cannot tell that call from a dead
    /// one. Clearing here would let a second run start beside the first, so only the failing call
    /// itself and boot, when nothing can still be creating a run, may clear.
    #[tokio::test]
    async fn launch_recovering_does_not_clear_a_reservation_it_did_not_make() {
        let state = state_with_node().await;
        let pool = &state.pool;
        reserve(pool, "node-1", "key-1").await;

        let recovered = recover(&state, "key-1").await;
        // Nor with input that would fail: this call never reaches a create, so nothing failed.
        let with_bad_input = recover_in_mode(&state, "key-1", REFUSED_WITHOUT_A_PROJECT).await;

        for answer in [recovered, with_bad_input] {
            match answer {
                Err(NodeRunError::Unresolved { node_id, key }) => {
                    assert_eq!(node_id, "node-1");
                    assert_eq!(key, "key-1");
                }
                other => panic!("expected Unresolved, got {other:?}"),
            }
        }
        assert_eq!(
            stored_result(pool, "node-1", "launch", "key-1")
                .await
                .as_deref(),
            Some("")
        );
        assert_eq!(run_rows(pool).await, 0);
        assert_eq!(node_run_id(pool, "node-1").await, None);
    }
}
