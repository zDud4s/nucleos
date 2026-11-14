//! A tree leaf bringing its tree's feature branch, or the project's integration branch, into its
//! OWN worktree branch.
//!
//! A leaf whose gate is red only because its branch lacks fixes already on the base has exactly one
//! cure: merge the base in. Without this the merge parked like any other (2026-11-14,
//! `obsolete-auto-close-and-sweep`: "Merging master is refused by the autonomous-run hook"), and a
//! person had to merge by hand through the queue.
//!
//! This is an implicit grant, narrower than a declared `merge` and independent of it, and it still
//! goes THROUGH the queue: the caller submits the merge as a declared one, so the run is refused the
//! command and told the ticket. Every one of these must hold, or the merge keeps asking:
//!
//! - the run is a node of a tree job (`work_nodes.tree_job_id`);
//! - the merge's checkout is the run's own worktree;
//! - the source is the tree's feature branch or the project's integration branch;
//! - the target is neither of those two, so a leaf can never merge into a shared branch.
//!
//! A merge into any other branch, a push, a deploy and a tag are not this and keep requiring approval.

use std::path::Path;
use std::time::Instant;

use sqlx::SqlitePool;

/// Whether the merge `source` -> `target`, run in `merge_root`, is a tree leaf's own sync.
pub(super) async fn is_leaf_sync_merge(
    pool: &SqlitePool,
    run_id: i64,
    project_id: &str,
    run_cwd: &Path,
    merge_root: &Path,
    source: &str,
    target: &str,
) -> bool {
    let deadline = Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let Ok(own) = crate::execution::git_exec::toplevel(run_cwd, deadline).await else {
        return false;
    };
    match (
        tokio::fs::canonicalize(&own).await,
        tokio::fs::canonicalize(merge_root).await,
    ) {
        (Ok(own), Ok(root)) if own == root => {}
        _ => return false,
    }

    let Ok(Some(Some(job_id))) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT tree_job_id FROM work_nodes WHERE run_id = ? LIMIT 1",
    )
    .bind(run_id.to_string())
    .fetch_optional(pool)
    .await
    else {
        return false;
    };
    let Ok(Some(job)) = crate::tree_job::store::load(pool, &job_id).await else {
        return false;
    };
    let feature = job.feature_branch.filter(|name| !name.trim().is_empty());

    let integration = match crate::inspect::project_root(pool, project_id).await {
        Ok(Some(root)) => {
            crate::execution::land::integration_branch(pool, project_id, Path::new(&root), deadline)
                .await
                .ok()
        }
        _ => None,
    };
    let integration = integration.as_ref().map(|branch| branch.as_str());

    let shared = |name: &str| feature.as_deref() == Some(name) || integration == Some(name);
    shared(source) && !shared(target)
}
