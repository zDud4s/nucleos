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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_job::model::{Branch, Manifest};

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
}
