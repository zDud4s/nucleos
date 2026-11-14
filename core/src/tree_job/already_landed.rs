//! A tree whose work reached the integration branch another way (a manual merge, a land outside
//! the tree's own queue request) is finished as landed instead of waiting for a root or a landing
//! that will never come.

use std::path::Path;
use std::time::Instant;

use serde_json::json;
use sqlx::SqlitePool;

use super::driver::{git, persist};
use super::landing::check_may_land;
use super::model::JOB_TERMINAL_STATE;
use super::store::TreeJob;
use crate::execution::git_exec::OPERATION_TIMEOUT;

/// Finishes `job` as landed when it is an integration tree whose branches are all settled with at
/// least one merged (`check_may_land`) and whose feature branch is already an ancestor of the
/// integration branch. The tip of the integration branch is recorded as `land_sha`. `Ok(false)`
/// when the tree is left as it was: work not on the integration branch yet, or git cannot tell.
pub(super) async fn finish_if_already_landed(
    pool: &SqlitePool,
    job: &mut TreeJob,
) -> Result<bool, String> {
    if job.manifest.extra.get("landing").and_then(|v| v.as_str()) != Some("integration")
        || check_may_land(&job.manifest).is_err()
    {
        return Ok(false);
    }
    let Some(project) = job.project_id.clone() else {
        return Ok(false);
    };
    let root = job.manifest.project_root.clone();
    let Ok(target) = crate::execution::land::integration_branch(
        pool,
        &project,
        Path::new(&root),
        Instant::now() + OPERATION_TIMEOUT,
    )
    .await
    else {
        return Ok(false);
    };
    let target = target.as_str().to_owned();
    let feature = job.manifest.feature_branch.clone();
    if feature == target
        || git(&root, &["merge-base", "--is-ancestor", &feature, &target])
            .await
            .is_err()
    {
        return Ok(false);
    }
    let Ok(sha) = git(&root, &["rev-parse", &target]).await else {
        return Ok(false);
    };

    job.state = JOB_TERMINAL_STATE.to_owned();
    job.manifest.state = JOB_TERMINAL_STATE.to_owned();
    job.manifest.extra.insert("land_sha".to_owned(), json!(sha));
    job.manifest
        .extra
        .insert("landed_elsewhere".to_owned(), json!(true));
    persist(pool, job).await?;
    super::obsolete::close_tree_logged(pool, &job.id, &super::obsolete::tree_end_reason(job)).await;
    crate::spec_backlog::estado::record_landed(pool, &job.id, Some(&sha)).await;
    Ok(true)
}
