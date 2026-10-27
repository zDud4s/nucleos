//! Proofs on the master head (task-claims spec 2026-10-22, section 3 Verify, cases 2 and 3).

use std::path::Path;
use std::time::Instant;

use sqlx::SqlitePool;

use crate::execution::land;
use crate::verify::resolve;

/// The tip of the project's integration branch, as a full sha.
pub(super) async fn master_head(
    pool: &SqlitePool,
    project_id: &str,
    project_root: &Path,
    deadline: Instant,
) -> Result<String, String> {
    let branch = land::integration_branch(pool, project_id, project_root, deadline).await?;
    resolve::resolve_commit(project_root, &format!("refs/heads/{}", branch.as_str()), deadline).await
}
