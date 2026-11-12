//! The branches a refused `tree_branch_start` left behind ("ghosts"): how to tell one from a live
//! branch, and how a later start of the same caller clears them out of its way. Split from `branch_start.rs`, which calls it before its first step.

use chrono::Utc;
use serde_json::{Value, json};

use super::branch_start::{Refusal, existing_node, store_refusal};
use super::http::job_lock;
use super::open;
use super::store;
use crate::state::AppState;

/// `branch.extra` key naming the node whose start added the branch (`post_branch` writes it).
pub(super) const START_CALLER_KEY: &str = "start_caller";

/// PURE: whether `branch` is one a refused start of `caller` left behind: added (and maybe opened)
/// by that caller, bound to no node, and with no pending record, so no run ever worked in it. The
/// caller is the one recorded on the branch; a branch written before that record is taken as the
/// parent branch's node, or the tree's root for a stem-level branch. The `root` branch and a
/// `queued` branch with no recorded caller (one `POST /branches` added directly) never are.
pub(super) fn is_ghost_of(
    job: &store::TreeJob,
    branch: &super::model::Branch,
    caller: &str,
) -> bool {
    if branch.node_id.is_some() || branch.name == super::root::ROOT_BRANCH {
        return false;
    }
    let pending = job
        .manifest
        .extra
        .get("pending_starts")
        .and_then(|held| held.get(&branch.name))
        .is_some();
    if pending {
        return false;
    }
    let recorded = branch.extra.get(START_CALLER_KEY).and_then(Value::as_str);
    match branch.state.as_str() {
        "queued" => recorded == Some(caller),
        "open" => match recorded {
            Some(recorded) => recorded == caller,
            None => {
                let inferred = match branch.parent.as_deref() {
                    Some(parent) => job
                        .manifest
                        .branches
                        .get(parent)
                        .and_then(|parent| parent.node_id.as_deref()),
                    None => Some(job.root_node_id.as_str()),
                };
                inferred == Some(caller)
            }
        },
        _ => false,
    }
}

/// PURE: whether `branch` was added with exactly this shape, the test `post_branch` replays on.
pub(super) fn same_shape(
    branch: &super::model::Branch,
    paths: &[String],
    layer: &str,
    base: Option<&str>,
    parent: Option<&str>,
) -> bool {
    branch.paths == paths
        && branch.layer == layer
        && branch.base.as_deref() == base
        && branch.parent.as_deref() == parent
}

/// Clears the way for a start of `name` by `caller` through the branches an earlier refused start
/// of the same caller left behind (`is_ghost_of`), under the job's lock and from a fresh read: a
/// ghost of the same name with another shape is taken out (its worktree and git branch with it, so
/// the new start opens afresh), and a ghost of another name whose paths overlap the new ones is
/// dropped. A ghost of the same name and shape stays: the start replays it. Nothing else is
/// touched, so every other conflict still answers as `post_branch` does. The caller must not hold
/// the lock.
#[allow(clippy::too_many_arguments)]
pub(super) async fn clear_ghosts(
    state: &AppState,
    job_id: &str,
    caller: &str,
    name: &str,
    paths: &[String],
    layer: &str,
    base: Option<&str>,
    parent: Option<&str>,
) -> Result<(), Refusal> {
    let lock = job_lock(job_id);
    let _guard = lock.lock().await;
    let Some(mut job) = store::load(&state.pool, job_id)
        .await
        .map_err(store_refusal)?
    else {
        return Ok(());
    };
    let mut changed = false;
    let replaced = job
        .manifest
        .branches
        .get(name)
        .filter(|branch| {
            is_ghost_of(&job, branch, caller) && !same_shape(branch, paths, layer, base, parent)
        })
        .cloned();
    if let Some(ghost) = replaced {
        // A node made by a start that crashed before its bind still counts: it is found again.
        if existing_node(state, caller, name).await?.is_none() {
            if let Some(worktree) = ghost.worktree.as_deref() {
                open::discard_unbound_worktree(
                    &job.manifest.project_root,
                    worktree,
                    ghost.git_branch.as_deref(),
                )
                .await;
            }
            job.manifest.branches.remove(name);
            changed = true;
        }
    }
    let at = Utc::now().to_rfc3339();
    // The branch of this very name is the replay's business, never an overlap to clear.
    let mut others = job.manifest.clone();
    others.branches.remove(name);
    while let Some(other) = super::http::overlap_outside_base_chain(&others, paths, base, parent) {
        let Some(found) = others.branches.get(&other) else {
            break;
        };
        if !is_ghost_of(&job, found, caller)
            || existing_node(state, caller, &other).await?.is_some()
        {
            break;
        }
        // Out of the view, so the next pass looks past it.
        others.branches.remove(&other);
        let Some(branch) = job.manifest.branches.get_mut(&other) else {
            break;
        };
        branch.state = "dropped".to_owned();
        branch.extra.insert(
            "drop_reason".to_owned(),
            json!(format!(
                "no node was ever bound to it; replaced by the start of branch {name}"
            )),
        );
        branch.extra.insert("dropped_at".to_owned(), json!(at));
        changed = true;
    }
    if changed {
        job.updated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        store::update(&state.pool, &job)
            .await
            .map_err(store_refusal)?;
        tracing::info!(
            job = job_id,
            branch = name,
            "a start replaced the leftovers of a refused one"
        );
    }
    Ok(())
}
