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

/// The feed kind of a case the driver settled itself (the kind `auto_resolve` uses).
pub(super) const FEED_KIND: &str = "work_tree_auto_resolve";

/// PURE: whether the work of a branch is already on the base. `ahead` counts the commits the branch
/// has over its merge target, `merged_in` says whether a merge commit of the target took its tip,
/// `cherry` is the `git cherry <target> <branch>` text (`-` = a patch the target holds, `+` = one it
/// lacks) and `diff_empty` whether the net diff to the target is empty. The answer is the drop
/// reason, or `None` to keep today's path.
pub(super) fn present_on_base(
    ahead: u64,
    merged_in: bool,
    cherry: &str,
    diff_empty: bool,
    target: &str,
) -> Option<String> {
    if merged_in {
        return Some(format!("already merged into {target}"));
    }
    let lines: Vec<&str> = cherry.lines().filter(|l| !l.trim().is_empty()).collect();
    if ahead > 0 && !lines.is_empty() && lines.iter().all(|l| l.starts_with('-')) {
        return Some(format!("superseded: every commit is already on {target}"));
    }
    if ahead > 0 && diff_empty {
        return Some(format!(
            "its commits add nothing over {target}: the work is already there"
        ));
    }
    None
}

/// Reads the facts for [`present_on_base`] from git. `Err` when git cannot tell.
async fn base_facts(job: &TreeJob, name: &str) -> Result<Option<String>, String> {
    let branch = job.manifest.branches.get(name).ok_or("branch vanished")?;
    let Some(git_branch) = &branch.git_branch else {
        return Ok(None);
    };
    let root = &job.manifest.project_root;
    let target = super::nesting::merge_target(&job.manifest, name)?;
    let ahead: u64 = git(
        root,
        &["rev-list", "--count", &format!("{target}..{git_branch}")],
    )
    .await?
    .parse()
    .map_err(|_| "unreadable rev-list count".to_owned())?;
    let ancestor = git(root, &["merge-base", "--is-ancestor", git_branch, &target])
        .await
        .is_ok();
    // A tip with a merge commit of the target as a child was merged: a branch that never moved
    // (its tip is only an ancestor of the target) is not proof of anything.
    let tip = git(root, &["rev-parse", git_branch]).await?;
    let merged_in = ancestor
        && git(
            root,
            &[
                "rev-list",
                "--merges",
                "--parents",
                &format!("{git_branch}..{target}"),
            ],
        )
        .await?
        .lines()
        .any(|line| line.split_whitespace().skip(1).any(|p| p == tip));
    let cherry = git(root, &["cherry", &target, git_branch]).await?;
    let diff_empty = git(
        root,
        &["diff", "--name-only", &format!("{target}...{git_branch}")],
    )
    .await?
    .is_empty();
    Ok(present_on_base(
        ahead, merged_in, &cherry, diff_empty, &target,
    ))
}

/// Drops branch `name` for `reason` when the driver may settle it itself, with one feed line.
/// `Ok(false)` when it did nothing (flag off, or the branch cannot be dropped from its state).
pub(super) async fn drop_present_on_base(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
    reason: &str,
) -> Result<bool, String> {
    if !super::auto_resolve::Config::load(&job.manifest.project_root).enabled {
        return Ok(false);
    }
    let held = job
        .manifest
        .branches
        .get_mut(name)
        .ok_or("branch vanished")?;
    if super::engine::apply_transition(held, "dropped").is_err() {
        return Ok(false);
    }
    held.extra
        .insert("drop_reason".to_owned(), serde_json::json!(reason));
    held.extra.remove(NO_CHANGE_KEY);
    let node = held.node_id.clone();
    persist(pool, job).await?;
    note_driver_resolution(
        pool,
        job,
        node.as_deref(),
        &format!("branch {name} dropped ({reason})"),
    )
    .await;
    Ok(true)
}

/// The one feed line of a case the driver settled before a decision was opened (origin `driver`).
pub(super) async fn note_driver_resolution(
    pool: &SqlitePool,
    job: &TreeJob,
    node: Option<&str>,
    what: &str,
) {
    let line = format!(
        "{}: the tree decided itself ({}) — node {} — {what}",
        job.goal,
        super::auto_resolve::Origin::Driver.as_str(),
        node.unwrap_or("-")
    );
    if let Err(error) = crate::feed::append(
        pool,
        job.project_id.as_deref(),
        FEED_KIND,
        &line,
        None,
        None,
    )
    .await
    {
        tracing::warn!(job = %job.id, %error, "could not write the driver-resolution feed line");
    }
}

/// Branch `name` merged into its target or superseded: it is dropped, nothing is gated or merged.
/// `Ok(true)` when dropped. A git error is logged and reads as "not present".
pub(super) async fn dropped_as_landed(
    pool: &SqlitePool,
    job: &mut TreeJob,
    name: &str,
) -> Result<bool, String> {
    match base_facts(job, name).await {
        Ok(Some(reason)) => drop_present_on_base(pool, job, name, &reason).await,
        Ok(None) => Ok(false),
        Err(error) => {
            tracing::warn!(job = %job.id, branch = name, %error, "could not tell whether the work is on the base");
            Ok(false)
        }
    }
}

/// Records why branch `name` changed nothing and asks for its park: it is not merged and does not
/// count as done. Nothing is asked for a branch with no node (nothing to park), nor while its node
/// still runs (an answer resumed a green branch and relaunched it): that run may still commit.
/// When the branch had commits and its work is already on the base, it is dropped instead.
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
    if dropped_as_landed(pool, job, name).await? {
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

// `mod.rs` is a shared file this node does not own, so the new test file is wired in from here.
#[cfg(test)]
#[path = "no_change_auto_tests.rs"]
mod no_change_auto_tests;

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
