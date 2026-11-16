//! The revert half of a red's life in `postgate_state`: the columns that follow a reported culprit
//! (`revert_merge_sha`, `revert_request_id`, `revert_sha`, `fix_branch`) and their compare-and-set
//! writers. The parent module still owns the row, the red report that first starts a revert, and
//! every other writer; its callers keep using these as `postgate::<name>`.

use sqlx::SqlitePool;

use super::POSTGATE_RED_KIND;

/// Starts the revert of `culprit` for a red that was already reported without one (the incident was
/// held by someone else at the time, and the claim has since lapsed). The same five columns
/// `report_inner` writes, in one transaction with the feed line. Refused (`false`, nothing
/// written) unless the project is still red on exactly that reported culprit with no revert
/// started, so it cannot start a second one.
pub async fn restart_revert(
    pool: &SqlitePool,
    project_id: &str,
    culprit: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             revert_merge_sha = ?, revert_request_id = NULL, revert_sha = NULL, \
             fix_branch = NULL, fix_run_id = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND culprit_sha = ? AND revert_merge_sha IS NULL \
           AND red_phase IS NULL AND red_groups != '[]'",
    )
    .bind(culprit)
    .bind(project_id)
    .bind(culprit)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    crate::feed::append_on(
        &mut tx,
        Some(project_id),
        POSTGATE_RED_KIND,
        summary,
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Stores the vcs ticket of the revert of `merge_sha`. Refused (`false`) when no revert of that
/// culprit is started, so a late ticket cannot attach itself to a revert that already ended.
pub async fn set_revert_request(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    request_id: i64,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET revert_request_id = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(request_id)
    .bind(project_id)
    .bind(merge_sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Stores the commit the queue published for the revert of `merge_sha`. Same compare-and-set as
/// `set_revert_request`.
pub async fn record_revert(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    revert_sha: &str,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET revert_sha = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(revert_sha)
    .bind(project_id)
    .bind(merge_sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The revert of `merge_sha` did not happen, or happened and left no branch: the five revert
/// columns are cleared so the state is free again, and `summary` goes to the feed, in one
/// transaction. Refused (`false`, nothing written) when no revert of that culprit is started.
pub async fn fail_revert(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             revert_merge_sha = NULL, revert_request_id = NULL, revert_sha = NULL, \
             fix_branch = NULL, fix_run_id = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(project_id)
    .bind(merge_sha)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    crate::feed::append_on(
        &mut tx,
        Some(project_id),
        POSTGATE_RED_KIND,
        summary,
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Stores the `fix/` branch built on the revert of `merge_sha` and tells the owner (`summary`), in
/// one transaction. Refused (`false`, nothing written) when no revert of that culprit is started.
pub async fn set_fix_branch(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    branch: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE postgate_state SET fix_branch = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(branch)
    .bind(project_id)
    .bind(merge_sha)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    crate::feed::append_on(
        &mut tx,
        Some(project_id),
        POSTGATE_RED_KIND,
        summary,
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}
