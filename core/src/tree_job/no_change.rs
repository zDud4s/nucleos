//! A code branch that changed nothing: why (`no_change_reason`), the record beside its park
//! (`NO_CHANGE_KEY`) and the check the driver runs before it gates or merges a branch
//! (`parked_as_no_change`).

use std::path::Path;

use sqlx::SqlitePool;

use super::driver::{Park, git, node_report, now, persist};
use super::store::TreeJob;

/// The record a code branch that changed nothing carries beside its park: `{"reason", "at"}`. It
/// is not a state of its own: the branch is parked (`decisions::park`), so it is neither merged nor
/// done, blocks the landing, and the owner's answer relaunches its node or drops it.
pub(super) const NO_CHANGE_KEY: &str = "no_change";

/// Why the branch has nothing to merge, or `None` when it changes a file. The diff is taken from
/// the merge base with the branch it merges into (`nesting::merge_target`) to its tip, so no commit
/// and commits that cancel out both read as no change. The worktree is checkpointed first: an
/// implement node leaves its work uncommitted. `Err` when git cannot tell (no such branch, a parent
/// that merges nowhere), and the caller then keeps today's path.
async fn no_change_reason(job: &TreeJob, name: &str) -> Result<Option<String>, String> {
    let branch = job.manifest.branches.get(name).ok_or("branch vanished")?;
    if let Some(worktree) = &branch.worktree {
        let ctx = super::checkpoint_ctx::for_branch(job, name, false);
        crate::execution::worktree::checkpoint_for(Path::new(worktree), &ctx)
            .await
            .map_err(|e| e.to_string())?;
    }
    let Some(git_branch) = &branch.git_branch else {
        return Ok(None);
    };
    let target = super::nesting::merge_target(&job.manifest, name)?;
    let range = format!("{target}...{git_branch}");
    let changed = git(&job.manifest.project_root, &["diff", "--name-only", &range]).await?;
    if !changed.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "{git_branch} has no change over {target}: no commit, or commits whose diff is empty"
    )))
}

/// Records why branch `name` changed nothing and asks for its park: it is not merged and does not
/// count as done. Nothing is asked for a branch with no node (nothing to park), nor while its node
/// still runs (an answer resumed a green branch and relaunched it): that run may still commit.
async fn park_no_change(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
    reason: &str,
    parks: &mut Vec<Park>,
) -> Result<(), String> {
    let node = job
        .manifest
        .branches
        .get(name)
        .and_then(|held| held.node_id.clone());
    if let Some(node) = &node
        && node_report(pool, node).await?.live
    {
        return Ok(());
    }
    let held = job
        .manifest
        .branches
        .get_mut(name)
        .ok_or("branch vanished")?;
    held.extra.insert(
        NO_CHANGE_KEY.to_owned(),
        serde_json::json!({"reason": reason, "at": now()}),
    );
    persist(pool, job).await?;
    if let Some(node) = node {
        parks.push((
            node,
            format!(
                "Branch {name} produced no change ({reason}), so nothing was merged. Answer to have its node try again, or drop the branch."
            ),
        ));
    }
    Ok(())
}

/// Checks branch `name` for a change before it is gated or merged. `Ok(true)` when it changed
/// nothing and was handed to [`park_no_change`]; a record left by an earlier empty pass is cleared
/// once the branch changes something. A git error is logged and reads as a change.
pub(super) async fn parked_as_no_change(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
    parks: &mut Vec<Park>,
) -> Result<bool, String> {
    match no_change_reason(job, name).await {
        Ok(Some(reason)) => {
            park_no_change(pool, job, name, &reason, parks).await?;
            Ok(true)
        }
        Ok(None) => {
            let cleared = job
                .manifest
                .branches
                .get_mut(name)
                .is_some_and(|held| held.extra.remove(NO_CHANGE_KEY).is_some());
            if cleared {
                persist(pool, job).await?;
            }
            Ok(false)
        }
        Err(error) => {
            tracing::warn!(job = %job.id, branch = name, %error, "could not tell whether the branch changed anything");
            Ok(false)
        }
    }
}
