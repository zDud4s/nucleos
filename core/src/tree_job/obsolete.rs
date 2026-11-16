//! Closing decisions that can no longer act on an answer: an `open` row becomes `obsolete`
//! (`decisions::mark_obsolete`) with the reason, never deleted, never an answered row.
//!
//! Two halves. The hooks (`close_tree`, `close_parked_decision`) run where a tree ends or a branch
//! leaves its park. `sweep_obsolete` closes what the hooks missed (a crash, a state written some
//! other way) and runs at daemon start and on every conductor tick; it is idempotent.
//!
//! A decision whose branch is still parked on it, or whose root still waits on it, is never touched.

use std::collections::HashMap;

use serde_json::Value;
use sqlx::SqlitePool;

use super::decisions::{self, Decision, ROOT_PARK_KEY};
use super::model::JOB_TERMINAL_STATE;
use super::store::{self, TreeJob};

/// Ids `decisions::park` and `ask_root` make, `d-…`: the only ones whose life is tied to a branch
/// (or the root) waiting on them.
const PARK_PREFIX: &str = "d-";
const PENDING_PREFIX: &str = "pending-";

/// Closes every open decision of tree `tree_id`: the tree is over, nothing can act on an answer.
/// Returns how many changed.
pub async fn close_tree(
    pool: &SqlitePool,
    tree_id: &str,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM decisions WHERE tree_id = ? AND status = ?")
            .bind(tree_id)
            .bind(decisions::STATUS_OPEN)
            .fetch_all(pool)
            .await?;
    let mut closed = 0;
    for id in ids {
        if decisions::mark_obsolete(pool, &id, reason).await? {
            closed += 1;
        }
    }
    Ok(closed)
}

/// Closes the decision a branch was parked on, read from the branch's `park` record taken before it
/// left the park. `false` when there is none or it is not open.
pub async fn close_parked_decision(
    pool: &SqlitePool,
    park: Option<&Value>,
    reason: &str,
) -> Result<bool, sqlx::Error> {
    let Some(id) = park
        .and_then(|park| park.get("decision_id"))
        .and_then(Value::as_str)
    else {
        return Ok(false);
    };
    decisions::mark_obsolete(pool, id, reason).await
}

/// Closes every open park decision (`d-…`) of node `node_id` in tree `tree_id`: the branch it tops
/// was dropped, so nothing can act on an answer. A branch's `park` record names only the last
/// decision it was parked on, so an earlier one still open is found by node, not by that record.
/// Returns how many changed.
pub async fn close_node_decisions(
    pool: &SqlitePool,
    tree_id: &str,
    node_id: &str,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM decisions WHERE tree_id = ? AND node_id = ? AND status = ? AND id LIKE ?",
    )
    .bind(tree_id)
    .bind(node_id)
    .bind(decisions::STATUS_OPEN)
    .bind(format!("{PARK_PREFIX}%"))
    .fetch_all(pool)
    .await?;
    let mut closed = 0;
    for id in ids {
        if decisions::mark_obsolete(pool, &id, reason).await? {
            closed += 1;
        }
    }
    Ok(closed)
}

/// [`close_node_decisions`], logged the same way as [`close_tree_logged`].
pub async fn close_node_decisions_logged(
    pool: &SqlitePool,
    tree_id: &str,
    node_id: &str,
    reason: &str,
) {
    if let Err(error) = close_node_decisions(pool, tree_id, node_id, reason).await {
        tracing::warn!(job = tree_id, node = node_id, %error, "could not close the decisions of a dropped branch");
    }
}

/// [`close_tree`] for a caller that has already written its own state: a failure is logged and the
/// sweep catches the rest.
pub async fn close_tree_logged(pool: &SqlitePool, tree_id: &str, reason: &str) {
    if let Err(error) = close_tree(pool, tree_id, reason).await {
        tracing::warn!(job = tree_id, %error, "could not close the decisions of a finished tree");
    }
    if let Err(error) = crate::distill::enqueue_tree_end_in(pool, tree_id).await {
        tracing::warn!(job = tree_id, %error, "could not queue a finished tree for distillation");
    }
}

/// [`close_parked_decision`], logged the same way.
pub async fn close_parked_decision_logged(pool: &SqlitePool, park: Option<&Value>, reason: &str) {
    if let Err(error) = close_parked_decision(pool, park, reason).await {
        tracing::warn!(%error, "could not close the decision of a branch that left its park");
    }
}

/// Why the end of tree `job` obsoletes its decisions.
pub fn tree_end_reason(job: &TreeJob) -> String {
    let how = job
        .manifest
        .extra
        .get("finish")
        .and_then(|finish| finish["status"].as_str())
        .unwrap_or(if job.manifest.extra.contains_key("land_sha") {
            "landed"
        } else {
            "finished"
        });
    format!("tree job {} is {how}", job.id)
}

fn pending_record_matches(job: &TreeJob, id: &str) -> bool {
    let Some(pending) = job
        .manifest
        .extra
        .get("pending_starts")
        .and_then(Value::as_object)
    else {
        return false;
    };
    pending
        .iter()
        .any(|(name, record)| match record["decision_id"].as_str() {
            Some(recorded) => recorded == id,
            None => {
                let generation = record["generation"].as_u64().unwrap_or(1);
                super::branch_start::pending_decision_id(&job.id, name, generation) == id
            }
        })
}

/// `None` when no branch of `job` sits behind `node_id`; else whether it is parked on `id`.
fn branch_parked_on(job: &TreeJob, node_id: &str, id: &str) -> Option<bool> {
    let branch = job
        .manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(node_id))?;
    let parked_here = branch.state == "parked"
        && branch
            .park
            .as_ref()
            .and_then(|park| park.get("decision_id"))
            .and_then(Value::as_str)
            == Some(id);
    Some(parked_here)
}

fn root_waits_on(job: &TreeJob, id: &str) -> bool {
    job.manifest
        .extra
        .get(ROOT_PARK_KEY)
        .and_then(|record| record.get("decision_id"))
        .and_then(Value::as_str)
        == Some(id)
}

/// The reason `decision` is obsolete, or `None` to leave it open. Pure: `job` is its tree (`None`
/// when the row names a tree that is gone), `node_status` its node's status (`None` when the node
/// is gone), `all_jobs` the unfinished jobs, read only for a `pending-*` row that names no tree.
fn verdict(
    decision: &Decision,
    job: Option<&TreeJob>,
    node_status: Option<&str>,
    all_jobs: &[TreeJob],
) -> Option<String> {
    let id = decision.id.as_str();
    let Some(tree_id) = decision.tree_id.as_deref() else {
        // No tree: only a `pending-*` row says what it waits on.
        let orphaned = id.starts_with(PENDING_PREFIX)
            && !all_jobs.iter().any(|job| pending_record_matches(job, id));
        return orphaned.then(|| "the pending start no longer exists".to_owned());
    };
    let Some(job) = job else {
        return Some(format!("tree job {tree_id} no longer exists"));
    };
    if job.state == JOB_TERMINAL_STATE {
        return Some(tree_end_reason(job));
    }
    if id.starts_with(PENDING_PREFIX) {
        return (!pending_record_matches(job, id))
            .then(|| "the pending start no longer exists".to_owned());
    }
    let node_id = decision.node_id.as_deref()?;
    match branch_parked_on(job, node_id, id) {
        // Still parked on this very decision: never touched.
        Some(true) => return None,
        Some(false) if id.starts_with(PARK_PREFIX) => {
            return Some("the branch is no longer parked on this decision".to_owned());
        }
        None if job.root_node_id == node_id && root_waits_on(job, id) => return None,
        None if job.root_node_id == node_id && id.starts_with(PARK_PREFIX) => {
            return Some("the root no longer waits on this decision".to_owned());
        }
        _ => {}
    }
    match node_status {
        None => Some(format!("node {node_id} no longer exists")),
        Some(status) if status != "planned" && status != "running" => {
            Some(format!("node {node_id} is closed ({status})"))
        }
        Some(_) => None,
    }
}

/// Closes every open decision that can no longer act on its answer, by the rules of [`verdict`].
/// Idempotent: a second run finds nothing open to close. Returns how many it closed. A tree whose
/// manifest cannot be read is skipped, so nothing is closed on a guess.
pub async fn sweep_obsolete(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let open: Vec<Decision> = sqlx::query_as(
        "SELECT id, tree_id, node_id, question, answer, origin, at, status FROM decisions \
         WHERE status = ? ORDER BY at, id",
    )
    .bind(decisions::STATUS_OPEN)
    .fetch_all(pool)
    .await?;
    if open.is_empty() {
        return Ok(0);
    }
    // Without the list of live jobs an ownerless `pending-*` row cannot be judged: stop here.
    let Ok(all_jobs) = store::list_non_terminal(pool).await else {
        return Ok(0);
    };
    let mut jobs: HashMap<String, Option<TreeJob>> = HashMap::new();
    let mut closed = 0;
    for decision in &open {
        let job = match decision.tree_id.as_deref() {
            None => None,
            Some(tree_id) => {
                if !jobs.contains_key(tree_id) {
                    match store::load(pool, tree_id).await {
                        Ok(loaded) => {
                            jobs.insert(tree_id.to_owned(), loaded);
                        }
                        // Unreadable manifest or a failed read: leave this tree's rows alone.
                        Err(_) => continue,
                    }
                }
                jobs.get(tree_id).and_then(Option::as_ref)
            }
        };
        let node_status: Option<String> = match decision.node_id.as_deref() {
            Some(node_id) => {
                sqlx::query_scalar("SELECT status FROM work_nodes WHERE id = ?")
                    .bind(node_id)
                    .fetch_optional(pool)
                    .await?
            }
            None => None,
        };
        if let Some(reason) = verdict(decision, job, node_status.as_deref(), &all_jobs)
            && decisions::mark_obsolete(pool, &decision.id, &reason).await?
        {
            closed += 1;
        }
    }
    Ok(closed)
}

/// [`sweep_obsolete`] that logs instead of failing, for the daemon's start and tick.
pub async fn sweep_logged(pool: &SqlitePool) {
    match sweep_obsolete(pool).await {
        Ok(0) => {}
        Ok(closed) => tracing::info!(closed, "closed obsolete decisions"),
        Err(error) => tracing::warn!(%error, "could not sweep obsolete decisions"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::SqlitePool;

    use super::*;
    use crate::tree_job::model::{Branch, Manifest};

    const TREE: &str = "job-1";
    const ROOT: &str = "job-1-root";
    const AT: &str = "2026-10-30T10:00:00+00:00";

    async fn seed_node(pool: &SqlitePool, id: &str, status: &str) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status,
                                     tree_job_id, created_at, updated_at)
             VALUES (?, ?, NULL, 0, 'orchestrator', 't', 'g', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(ROOT)
        .bind(status)
        .bind(TREE)
        .bind(AT)
        .bind(AT)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Job `TREE` in `state` with the branches `(name, state, node id)`; every node row is running.
    async fn seed_tree(pool: &SqlitePool, state: &str, branches: &[(&str, &str, &str)]) {
        let mut manifest = Manifest::new(TREE, "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        manifest.state = state.to_owned();
        for &(name, branch_state, node_id) in branches {
            seed_node(pool, node_id, "running").await;
            let mut branch = Branch::new(name, &["core/x.rs".to_owned()], "core", None);
            branch.state = branch_state.to_owned();
            branch.node_id = Some(node_id.to_owned());
            manifest.branches.insert(name.to_owned(), branch);
        }
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: ROOT.to_owned(),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: state.to_owned(),
            manifest,
            contract: None,
            created_at: AT.to_owned(),
            updated_at: AT.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
    }

    async fn insert(pool: &SqlitePool, id: &str, tree: Option<&str>, node: Option<&str>) {
        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
             VALUES (?, ?, ?, 'q', NULL, 'agent', ?, 'open')",
        )
        .bind(id)
        .bind(tree)
        .bind(node)
        .bind(AT)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn status(pool: &SqlitePool, id: &str) -> String {
        sqlx::query_scalar("SELECT status FROM decisions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn set_park(pool: &SqlitePool, branch: &str, state: &str, decision: Option<&str>) {
        let mut job = store::load(pool, TREE).await.unwrap().unwrap();
        let entry = job.manifest.branches.get_mut(branch).unwrap();
        entry.state = state.to_owned();
        entry.park = decision.map(|id| json!({ "prev_state": "building", "decision_id": id }));
        store::update(pool, &job).await.unwrap();
    }

    #[tokio::test]
    async fn a_decision_its_branch_is_parked_on_stays_open() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "open", &[("api", "parked", "node-api")]).await;
        set_park(&pool, "api", "parked", Some("d-live")).await;
        insert(&pool, "d-live", Some(TREE), Some("node-api")).await;
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 0);
        assert_eq!(status(&pool, "d-live").await, "open");
    }

    #[tokio::test]
    async fn the_decisions_of_a_finished_tree_become_obsolete() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "finished", &[("api", "parked", "node-api")]).await;
        set_park(&pool, "api", "parked", Some("d-end")).await;
        insert(&pool, "d-end", Some(TREE), Some("node-api")).await;
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 1);
        assert_eq!(status(&pool, "d-end").await, "obsolete");
    }

    #[tokio::test]
    async fn close_tree_closes_every_open_decision_of_that_tree_only() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "open", &[("api", "parked", "node-api")]).await;
        insert(&pool, "d-a", Some(TREE), Some("node-api")).await;
        insert(&pool, "d-b", Some(TREE), None).await;
        insert(&pool, "d-other", Some("other"), None).await;
        assert_eq!(close_tree(&pool, TREE, "abandoned").await.unwrap(), 2);
        assert_eq!(status(&pool, "d-a").await, "obsolete");
        assert_eq!(status(&pool, "d-other").await, "open");
    }

    #[tokio::test]
    async fn close_node_decisions_closes_every_open_park_of_that_node_only() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "open", &[("api", "parked", "node-api")]).await;
        // The park record names only the latest decision: the earlier one is still open.
        set_park(&pool, "api", "parked", Some("d-new")).await;
        insert(&pool, "d-old", Some(TREE), Some("node-api")).await;
        insert(&pool, "d-new", Some(TREE), Some("node-api")).await;
        insert(&pool, "pending-x", Some(TREE), Some("node-api")).await;
        insert(&pool, "d-sibling", Some(TREE), Some("node-other")).await;
        let closed = close_node_decisions(&pool, TREE, "node-api", "dropped")
            .await
            .unwrap();
        assert_eq!(closed, 2);
        assert_eq!(status(&pool, "d-old").await, "obsolete");
        assert_eq!(status(&pool, "d-new").await, "obsolete");
        assert_eq!(status(&pool, "pending-x").await, "open");
        assert_eq!(status(&pool, "d-sibling").await, "open");
    }

    #[tokio::test]
    async fn a_dropped_branch_decision_becomes_obsolete() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "open", &[("api", "parked", "node-api")]).await;
        set_park(&pool, "api", "dropped", Some("d-drop")).await;
        insert(&pool, "d-drop", Some(TREE), Some("node-api")).await;
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 1);
        assert_eq!(status(&pool, "d-drop").await, "obsolete");
    }

    #[tokio::test]
    async fn a_branch_already_past_the_park_makes_its_decision_obsolete() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "open", &[("api", "building", "node-api")]).await;
        insert(&pool, "d-past", Some(TREE), Some("node-api")).await;
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 1);
        assert_eq!(status(&pool, "d-past").await, "obsolete");
    }

    #[tokio::test]
    async fn the_hook_closes_the_decision_of_a_branch_leaving_its_park() {
        let pool = crate::testdb::fresh_pool().await;
        insert(&pool, "d-hook", Some(TREE), Some("node-api")).await;
        let park = json!({ "prev_state": "building", "decision_id": "d-hook" });
        assert!(
            close_parked_decision(&pool, Some(&park), "moved on")
                .await
                .unwrap()
        );
        assert!(
            !close_parked_decision(&pool, None, "moved on")
                .await
                .unwrap()
        );
        assert_eq!(status(&pool, "d-hook").await, "obsolete");
    }

    #[tokio::test]
    async fn a_vanished_pending_start_makes_its_decision_obsolete() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "open", &[("api", "building", "node-api")]).await;
        let mut job = store::load(&pool, TREE).await.unwrap().unwrap();
        job.manifest.extra.insert(
            "pending_starts".to_owned(),
            json!({ "kept": { "decision_id": "pending-job-1-kept-1" } }),
        );
        store::update(&pool, &job).await.unwrap();
        insert(&pool, "pending-job-1-kept-1", Some(TREE), Some("node-api")).await;
        insert(&pool, "pending-job-1-gone-1", Some(TREE), Some("node-api")).await;
        insert(&pool, "pending-lost-job-x-1", None, None).await;
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 2);
        assert_eq!(status(&pool, "pending-job-1-kept-1").await, "open");
        assert_eq!(status(&pool, "pending-job-1-gone-1").await, "obsolete");
        assert_eq!(status(&pool, "pending-lost-job-x-1").await, "obsolete");
    }

    #[tokio::test]
    async fn a_closed_or_missing_node_makes_a_non_park_decision_obsolete() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "open", &[("api", "building", "node-api")]).await;
        seed_node(&pool, "node-done", "done").await;
        insert(
            &pool,
            "start-refused-job-1-x-1",
            Some(TREE),
            Some("node-done"),
        )
        .await;
        insert(
            &pool,
            "start-refused-job-1-y-1",
            Some(TREE),
            Some("node-gone"),
        )
        .await;
        insert(
            &pool,
            "start-refused-job-1-z-1",
            Some(TREE),
            Some("node-api"),
        )
        .await;
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 2);
        assert_eq!(status(&pool, "start-refused-job-1-z-1").await, "open");
    }

    #[tokio::test]
    async fn the_sweep_is_idempotent_and_leaves_answered_rows_alone() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "finished", &[("api", "building", "node-api")]).await;
        insert(&pool, "d-open", Some(TREE), Some("node-api")).await;
        insert(&pool, "d-answered", Some(TREE), Some("node-api")).await;
        sqlx::query(
            "UPDATE decisions SET status = 'answered', answer = 'A' WHERE id = 'd-answered'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 1);
        assert_eq!(sweep_obsolete(&pool).await.unwrap(), 0);
        assert_eq!(status(&pool, "d-answered").await, "answered");
        let answer: Option<String> =
            sqlx::query_scalar("SELECT answer FROM decisions WHERE id = 'd-answered'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(answer.as_deref(), Some("A"));
    }
}
