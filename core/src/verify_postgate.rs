//! Durable state of the post-merge gate, per project (spec 2026-10-05 §6.1, §6.3).
//!
//! One row of `postgate_state` (migration 0180) holds the last sha the gate passed on, the last
//! tip it was started for, the gate running right now, and the groups that are red with the sha
//! they have been red since. A restart finds the row as it was left, so the worker can resume or
//! requeue instead of starting over. This module is the table's only writer.
//!
//! It also names the worktree the gate runs in, `postgate-<project>` under the worktree root.
//! That directory is protected from the orphan sweep by NAME, like `integration-`:
//! `worktree::owner_from_dir_name` claims only `run-`, `job-` and `item-`, so nothing here needs
//! an `Owner` variant or a `worktrees` row.
//!
//! Called by nothing yet; F3-2 wires it.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::verify_batch::TargetState;

/// Directory-name prefix of the post-merge gate's worktree.
pub const POSTGATE_PREFIX: &str = "postgate-";

/// Where the post-merge gate checks out the target for `project_root`. Named like
/// `git_exec::integration_worktree`, including its known limit: two projects whose directories
/// share a leaf name collide under a shared `NUCLEOS_WORKTREE_ROOT`.
pub fn postgate_worktree(project_root: &Path) -> PathBuf {
    let project = project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_owned());
    crate::worktree::worktree_root(project_root).join(format!("{POSTGATE_PREFIX}{project}"))
}

/// One project's post-merge gate state, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub project_id: String,
    pub target: String,
    pub last_green_sha: Option<String>,
    pub last_attempted_sha: Option<String>,
    pub running_sha: Option<String>,
    pub running_request_id: Option<i64>,
    pub red_groups: Vec<String>,
    pub red_since_sha: Option<String>,
}

impl State {
    /// A gate is running for this project.
    pub fn running(&self) -> bool {
        self.running_sha.is_some()
    }

    /// The view `verify_batch::decide` reads, for the target's current `tip`.
    pub fn target_state<'a>(&'a self, tip: &'a str) -> TargetState<'a> {
        TargetState {
            tip,
            last_green: self.last_green_sha.as_deref(),
            last_attempted: self.last_attempted_sha.as_deref(),
            running: self.running(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    project_id: String,
    target: String,
    last_green_sha: Option<String>,
    last_attempted_sha: Option<String>,
    running_sha: Option<String>,
    running_request_id: Option<i64>,
    red_groups: String,
    red_since_sha: Option<String>,
}

impl From<Row> for State {
    fn from(row: Row) -> Self {
        let red_groups = serde_json::from_str(&row.red_groups).unwrap_or_else(|error| {
            tracing::warn!(
                project = %row.project_id,
                %error,
                "undecodable red_groups in postgate_state; reading it as empty"
            );
            Vec::new()
        });
        State {
            project_id: row.project_id,
            target: row.target,
            last_green_sha: row.last_green_sha,
            last_attempted_sha: row.last_attempted_sha,
            running_sha: row.running_sha,
            running_request_id: row.running_request_id,
            red_groups,
            red_since_sha: row.red_since_sha,
        }
    }
}

/// The stored state of `project_id`, or `None` when the gate has never touched it.
pub async fn load(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<State>> {
    let row = sqlx::query_as::<_, Row>(
        "SELECT project_id, target, last_green_sha, last_attempted_sha, running_sha, \
                running_request_id, red_groups, red_since_sha \
         FROM postgate_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(State::from))
}

/// Records that a gate for `sha` started. Refused (`false`, nothing changed) while a gate is
/// already running for the project: one gate per project (§6.3).
pub async fn start(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    sha: &str,
    request_id: Option<i64>,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "INSERT INTO postgate_state \
             (project_id, target, last_attempted_sha, running_sha, running_request_id, \
              running_started_at) \
         VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP) \
         ON CONFLICT(project_id) DO UPDATE SET \
             target = excluded.target, \
             last_attempted_sha = excluded.last_attempted_sha, \
             running_sha = excluded.running_sha, \
             running_request_id = excluded.running_request_id, \
             running_started_at = CURRENT_TIMESTAMP, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE postgate_state.running_sha IS NULL",
    )
    .bind(project_id)
    .bind(target)
    .bind(sha)
    .bind(sha)
    .bind(request_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The gate running for `sha` passed. A finish for a sha that is not the running one changes
/// nothing and returns `false`, so a late finish cannot overwrite newer state.
pub async fn finish_green(pool: &SqlitePool, project_id: &str, sha: &str) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             last_green_sha = ?, \
             running_sha = NULL, running_request_id = NULL, running_started_at = NULL, \
             red_groups = '[]', red_since_sha = NULL, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND running_sha = ?",
    )
    .bind(sha)
    .bind(project_id)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The gate running for `sha` failed in `groups`. `red_since_sha` keeps the first red sha until a
/// green clears it. Same compare-and-set as `finish_green`.
pub async fn finish_red(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
    groups: &[String],
) -> sqlx::Result<bool> {
    let encoded = serde_json::to_string(groups).unwrap_or_else(|_| "[]".to_owned());
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             running_sha = NULL, running_request_id = NULL, running_started_at = NULL, \
             red_groups = ?, \
             red_since_sha = COALESCE(red_since_sha, ?), \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND running_sha = ?",
    )
    .bind(encoded)
    .bind(sha)
    .bind(project_id)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Marks `sha` green without running a gate (`verify_batch`'s `MarkCovered`: the full gate
/// already passed on exactly that sha before it was published). Starts nothing and leaves a
/// running gate untouched.
pub async fn mark_covered(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    sha: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO postgate_state (project_id, target, last_green_sha) VALUES (?, ?, ?) \
         ON CONFLICT(project_id) DO UPDATE SET \
             target = excluded.target, \
             last_green_sha = excluded.last_green_sha, \
             red_groups = '[]', \
             red_since_sha = NULL, \
             updated_at = CURRENT_TIMESTAMP",
    )
    .bind(project_id)
    .bind(target)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify_batch::{Decision, Idle, decide};

    #[tokio::test]
    async fn a_fresh_database_has_no_postgate_state() {
        let pool = crate::testdb::fresh_pool().await;
        assert_eq!(load(&pool, "p").await.unwrap(), None);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM postgate_state")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn start_records_the_attempt_and_marks_a_gate_running() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(start(&pool, "p", "master", "a1", Some(7)).await.unwrap());

        let s = load(&pool, "p").await.unwrap().expect("a row after start");
        assert_eq!(s.project_id, "p");
        assert_eq!(s.target, "master");
        assert_eq!(s.last_attempted_sha.as_deref(), Some("a1"));
        assert_eq!(s.running_sha.as_deref(), Some("a1"));
        assert_eq!(s.running_request_id, Some(7));
        assert_eq!(s.last_green_sha, None);
        assert!(s.running());
    }

    #[tokio::test]
    async fn a_second_start_while_one_is_running_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(start(&pool, "p", "master", "a1", Some(1)).await.unwrap());
        let before = load(&pool, "p").await.unwrap();

        assert!(!start(&pool, "p", "master", "b2", Some(2)).await.unwrap());
        assert_eq!(
            load(&pool, "p").await.unwrap(),
            before,
            "state must not change"
        );
    }

    #[tokio::test]
    async fn finish_green_moves_last_green_and_clears_running_and_reds() {
        let pool = crate::testdb::fresh_pool().await;
        // A red first, so there is something for the green to clear.
        start(&pool, "p", "master", "a1", None).await.unwrap();
        assert!(
            finish_red(&pool, "p", "a1", &["core".to_owned()])
                .await
                .unwrap()
        );
        start(&pool, "p", "master", "b2", Some(3)).await.unwrap();

        assert!(finish_green(&pool, "p", "b2").await.unwrap());

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some("b2"));
        assert_eq!(s.running_sha, None);
        assert_eq!(s.running_request_id, None);
        assert!(!s.running());
        assert!(s.red_groups.is_empty());
        assert_eq!(s.red_since_sha, None);
        assert_eq!(s.last_attempted_sha.as_deref(), Some("b2"));
    }

    #[tokio::test]
    async fn finish_red_records_the_groups_and_keeps_the_first_red_sha() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", None).await.unwrap();
        let groups = vec!["core".to_owned(), "shell".to_owned()];
        assert!(finish_red(&pool, "p", "a1", &groups).await.unwrap());

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.red_groups, groups);
        assert_eq!(s.red_since_sha.as_deref(), Some("a1"));
        assert_eq!(s.running_sha, None);
        assert_eq!(s.last_green_sha, None);

        // A second red keeps the first sha it has been red since.
        start(&pool, "p", "master", "b2", None).await.unwrap();
        let later = vec!["core".to_owned()];
        assert!(finish_red(&pool, "p", "b2", &later).await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.red_groups, later);
        assert_eq!(s.red_since_sha.as_deref(), Some("a1"));
    }

    #[tokio::test]
    async fn a_finish_for_a_sha_that_is_not_running_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", Some(1)).await.unwrap();
        let before = load(&pool, "p").await.unwrap();

        assert!(!finish_green(&pool, "p", "zzz").await.unwrap());
        assert!(
            !finish_red(&pool, "p", "zzz", &["core".to_owned()])
                .await
                .unwrap()
        );
        assert_eq!(load(&pool, "p").await.unwrap(), before);

        // And with nothing running at all, or no row for the project.
        finish_green(&pool, "p", "a1").await.unwrap();
        let settled = load(&pool, "p").await.unwrap();
        assert!(!finish_green(&pool, "p", "a1").await.unwrap());
        assert!(!finish_red(&pool, "other", "a1", &[]).await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap(), settled);
    }

    #[tokio::test]
    async fn mark_covered_sets_last_green_without_starting_a_gate() {
        let pool = crate::testdb::fresh_pool().await;
        mark_covered(&pool, "p", "master", "c3").await.unwrap();

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some("c3"));
        assert_eq!(s.last_attempted_sha, None, "nothing was attempted");
        assert!(!s.running());
        assert!(s.red_groups.is_empty());
    }

    #[tokio::test]
    async fn the_stored_state_is_what_decide_consumes() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "b", None).await.unwrap();
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(
            decide(&s.target_state("b"), &[]),
            Decision::Idle(Idle::Running)
        );

        assert!(finish_green(&pool, "p", "b").await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(
            decide(&s.target_state("b"), &[]),
            Decision::Idle(Idle::UpToDate)
        );
    }

    #[test]
    fn the_postgate_worktree_is_named_by_the_project() {
        // The directory above is `worktree_root`, which reads an environment variable other tests
        // set, so only the leaf (which that variable cannot change) is asserted here.
        let root = std::path::Path::new("repos").join("alpha");
        let path = postgate_worktree(&root);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("postgate-alpha")
        );
        assert_eq!(POSTGATE_PREFIX, "postgate-");
    }
}
