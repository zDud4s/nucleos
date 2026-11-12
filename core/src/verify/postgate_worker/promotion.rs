//! The promotion trigger of the release branch (spec 2026-11-11, D4).
//!
//! With `release_branch` set, the gate proves the integration branch and the release branch only
//! moves by an `Op::Promote` of the last green sha. This module queues that op, at most one at a
//! time, whenever the green sha is ahead of the release branch. Every failure is logged and
//! swallowed: the next tick tries again, and the gate itself must never stop for it.

use std::path::Path;

use chrono::Utc;

use crate::config::AutopilotRules;
use crate::execution::git_exec;
use crate::execution::vcs::{self, Branch, Op, Origin};
use crate::task_claims::{machine, store};
use crate::verify::postgate::State;

/// Queues a promotion of the green sha to the release branch when one is due. See the module doc.
pub(super) async fn enqueue_promotion_if_due(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    state: &State,
    rules: &AutopilotRules,
) {
    let Some(release) = rules.release_branch() else {
        return;
    };
    // A cover counts like a full green: it is the sha the gate proved last.
    let deadline = std::time::Instant::now() + git_exec::OPERATION_TIMEOUT;
    let Some(green) = state.promotable_green(project_root, deadline).await else {
        return;
    };
    if let Err(error) = enqueue(pool, project_id, project_root, state, release, &green).await {
        tracing::warn!(project_id, %error, "postgate: could not queue the promotion to the release branch");
    }
}

async fn enqueue(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    state: &State,
    release: &str,
    green: &str,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + git_exec::OPERATION_TIMEOUT;
    if !git_exec::branch_exists(project_root, release, deadline).await? {
        return Ok(());
    }
    // The release branch already holds the green sha: nothing to promote.
    if git_exec::is_ancestor(project_root, green, release, deadline).await? {
        return Ok(());
    }

    let key = machine::release_diverged_key();
    let diverged = store::live_by_key(pool, project_id, &key, Utc::now())
        .await
        .map_err(|error| format!("cannot read the release-diverged ticket: {error}"))?
        .is_some();
    if diverged {
        // Still not an ancestor of the green sha: the owner has not resolved it.
        if !git_exec::is_ancestor(project_root, release, green, deadline).await? {
            return Ok(());
        }
        machine::close_release_diverged(
            pool,
            project_id,
            "the release branch is an ancestor of the green sha again",
            Utc::now(),
        )
        .await
        .map_err(|error| format!("cannot close the release-diverged ticket: {error}"))?;
    }

    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM vcs_requests
          WHERE project_id = ? AND op = 'promote'
            AND status IN ('queued', 'running', 'awaiting_gate')",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(|error| format!("cannot look for a pending promote: {error}"))?;
    if pending > 0 {
        return Ok(());
    }

    let from = Branch::new(&state.target).map_err(|error| format!("{error:?}"))?;
    let to = Branch::new(release).map_err(|error| format!("{error:?}"))?;
    let repo = vcs::resolve_repo(pool, project_id)
        .await
        .map_err(|error| format!("cannot resolve the repository: {error:?}"))?;
    vcs::submit_declared(pool, &repo, &Op::Promote { from, to }, Origin::Daemon)
        .await
        .map_err(|error| format!("cannot queue the promote: {error}"))?;
    Ok(())
}
