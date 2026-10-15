//! Worktree database rows and release operations.

use super::*;

#[derive(Debug, Clone, sqlx::FromRow, PartialEq)]
pub struct WorktreeRow {
    pub owner_kind: String,
    pub owner_id: i64,
    pub project_id: String,
    pub project_root: String,
    pub path: String,
    pub branch: String,
}

impl WorktreeRow {
    /// The typed owner behind the two stored columns.
    ///
    /// `None` for a kind the CHECK constraint should have made impossible. Callers treat that as
    /// "not mine to touch" rather than guessing — the operations downstream of this delete
    /// directories, so an unrecognised row is left alone instead of being collected on a guess.
    pub fn owner(&self) -> Option<Owner> {
        match self.owner_kind.as_str() {
            "run" => Some(Owner::Run(self.owner_id)),
            "job" => Some(Owner::Job(self.owner_id)),
            "item" => Some(Owner::Item(self.owner_id)),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseOutcome {
    Released,
    NotAwaitingApproval,
    NotFound,
}

pub async fn record(
    pool: &SqlitePool,
    owner: Owner,
    project_id: &str,
    project_root: &str,
    path: &str,
    branch: &str,
    base_sha: Option<&str>,
) -> sqlx::Result<()> {
    // An upsert, and the conflict arm only ever fires for an owner whose name comes back — an item.
    // A run's id is minted with the run and a job records once, so for both of those this is the
    // plain INSERT it always was. For an item, a tree adopted on a retry writes the same row again,
    // and a tree recreated after the GC took the last one has to clear `removed_at` or the sweep
    // would collect the new checkout on the strength of the old one's ending.
    //
    // `COALESCE(excluded.base_sha, base_sha)` and not a plain overwrite: an adopted tree reports no
    // base, because where it STANDS is not where it was born, and letting a `None` erase the
    // recorded base would turn every retry into `not measured` for collision.
    sqlx::query(
        "INSERT INTO worktrees
         (owner_kind, owner_id, project_id, project_root, path, branch, base_sha,
          created_at, removed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)
         ON CONFLICT(owner_kind, owner_id) DO UPDATE SET
             project_id   = excluded.project_id,
             project_root = excluded.project_root,
             path         = excluded.path,
             branch       = excluded.branch,
             base_sha     = COALESCE(excluded.base_sha, base_sha),
             removed_at   = NULL",
    )
    .bind(owner.kind())
    .bind(owner.id())
    .bind(project_id)
    .bind(project_root)
    .bind(path)
    .bind(branch)
    .bind(base_sha)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_removed(pool: &SqlitePool, owner: Owner) -> sqlx::Result<()> {
    let removed: Vec<(Option<String>, String)> = sqlx::query_as(
        "UPDATE worktrees SET removed_at = ? WHERE owner_kind = ? AND owner_id = ?
         RETURNING project_id, path",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(owner.kind())
    .bind(owner.id())
    .fetch_all(pool)
    .await?;
    // Tests must never schedule deletions under the developer's real `~/.nucleos/warm`.
    #[cfg(any(test, feature = "testkit"))]
    let _ = removed;
    // The verification state kept for this tree is dead weight now. Detached and best-effort: the
    // executor's periodic sweep removes anything this misses.
    #[cfg(not(any(test, feature = "testkit")))]
    if let Some(root) = crate::machine_config::root() {
        for (project_id, path) in removed {
            let Some(project_id) = project_id else {
                continue;
            };
            let root = root.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(error) =
                    crate::warm::forget_worktree(&root, &project_id, Path::new(&path))
                {
                    tracing::debug!(%error, path, "could not forget the worktree's warm state");
                }
            });
        }
    }
    // The collision measurement describes a tree that is no longer on disk. Best-effort: a row left
    // behind is one warning too many, and failing the worktree's removal on account of it would
    // trade a small problem for a large one.
    if let Err(error) = crate::execution::collision::forget(pool, owner.kind(), owner.id()).await {
        tracing::warn!(?owner, %error, "could not clear the worktree's collision measurement");
    }
    Ok(())
}

pub async fn release(pool: &SqlitePool, run_id: i64) -> sqlx::Result<ReleaseOutcome> {
    // Claim the run first, with the check inside the write. Read-then-write left a gap that this
    // call then spent tens of seconds inside — `git worktree remove` retries on a backoff — and
    // anything finalising the run in that gap (a cancel, or an approval that supersedes it and hands
    // its worktree to a resume) would find `cancelled` stamped over its status and its worktree
    // deleted. Claiming before touching anything means only the winner does the destructive part.
    //
    // The order also fails in the better direction: a crash between the claim and the removal leaves
    // a `cancelled` run with a live worktree row, which the GC collects; the old order could leave a
    // removed worktree pinned by an `awaiting_approval` run, which the GC never touches.
    let claimed = sqlx::query(
        "UPDATE runs SET status = 'cancelled', completed_at = ?
         WHERE id = ? AND status = 'awaiting_approval'",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(pool)
    .await?;
    if claimed.rows_affected() == 0 {
        // Losing the claim is not an error; it only decides which of the two honest answers this is.
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
        return Ok(match exists {
            Some(_) => ReleaseOutcome::NotAwaitingApproval,
            None => ReleaseOutcome::NotFound,
        });
    }

    // Spec A D14: the run is gone, so nobody can answer for it any more. After the claim, so a
    // release that lost to an approval expires nothing; best-effort, because the run IS released
    // either way and `expire_orphaned_approvals` heals a lost write at the next start.
    if let Err(error) = crate::proposals::expire_for_run(
        pool,
        run_id,
        "the run was released before anybody answered",
    )
    .await
    {
        tracing::warn!(run_id, %error, "could not expire the released run's pending approval");
    }

    // Release stays a *run* concept: it un-pins a run paused for approval, and a run still owns its
    // own worktree. The `owner_kind` filter keeps it from ever matching a job whose id collides.
    let worktree: Option<WorktreeRow> = sqlx::query_as(
        "SELECT owner_kind, owner_id, project_id, project_root, path, branch
         FROM worktrees
         WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;

    if let Some(worktree) = &worktree {
        if let Err(error) = remove(
            Path::new(&worktree.project_root),
            Path::new(&worktree.path),
            GC_BACKOFF,
        )
        .await
        {
            if is_preservation_failure(&error) {
                tracing::warn!(
                    run_id,
                    %error,
                    "failed to preserve released worktree; deferring removal"
                );
                return Ok(ReleaseOutcome::Released);
            }
            tracing::warn!(
                run_id,
                %error,
                "failed to remove released worktree; continuing with discard"
            );
        }
        mark_removed(pool, Owner::Run(run_id)).await?;
    }

    if let Some(worktree) = worktree {
        // A discarded run's branch was left behind until now, so every rejected proposal leaked one.
        // `-d` keeps anything with unmerged commits, so this only collects the branches that carry
        // no work — which is exactly the case for a run blocked at its first high-risk action.
        let branch_deleted =
            delete_branch_if_merged(&worktree.project_root, &worktree.branch).await;
        let summary = if branch_deleted {
            format!(
                "released worktree {} + branch {}",
                worktree.path, worktree.branch
            )
        } else {
            format!("released worktree {}", worktree.path)
        };
        let _ = feed::append(
            pool,
            Some(&worktree.project_id),
            "worktree_released",
            &summary,
            Some(run_id),
            Some(&crate::feed::run_subject(pool, run_id).await),
        )
        .await;
        feed_branch_outcome(pool, &worktree, branch_deleted).await;
    }

    Ok(ReleaseOutcome::Released)
}

/// Best-effort `git branch -d`, reporting whether the branch went away.
///
/// Cleanup first commits every non-ignored, uncommitted path that fits the preservation ceiling;
/// overflow or any preservation failure leaves the worktree in place. Always `-d`, never `-D`, then
/// keeps that preservation commit (and any other unmerged work) on its branch. Together those two
/// refusals ensure autonomous cleanup cannot destroy work the human has not seen. A discarded run
/// with no commits remains the common branch this safely collects.
pub(super) async fn delete_branch_if_merged(project_root: &str, branch: &str) -> bool {
    git()
        .arg("-C")
        .arg(project_root)
        .arg("branch")
        .arg("-d")
        .arg(branch)
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

/// What a feed line about this owner's worktree is about.
///
/// Not [`Owner::feed_run_id`], which answers a narrower question — which single run to blame — and
/// has to say `None` for a job's tree. A subject has an answer for all three: a job's tree and an
/// item's tree are both the job's story, and a run's tree is whatever [`feed::run_subject`] says the
/// run belongs to. An item whose row cannot be read is the one owner with no exact key, and gets
/// none rather than a guess.
pub(super) async fn feed_subject(pool: &SqlitePool, owner: Owner) -> Option<feed::Subject> {
    match owner {
        Owner::Run(run_id) => Some(feed::run_subject(pool, run_id).await),
        Owner::Job(job_id) => Some(feed::Subject::Job(job_id)),
        Owner::Item(item_id) => {
            sqlx::query_scalar::<_, i64>("SELECT job_id FROM job_items WHERE id = ?")
                .bind(item_id)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
                .map(feed::Subject::Job)
        }
        // Never reached in practice: a wave owns no tree, so no feed line is ever about one.
        Owner::Wave(_) => None,
    }
}

/// Records what became of a branch after its worktree was collected. Kept branches are announced
/// too — an unmerged branch left behind is a thing the human may want to look at, not a silent leak.
pub(super) async fn feed_branch_outcome(pool: &SqlitePool, worktree: &WorktreeRow, deleted: bool) {
    if deleted {
        return;
    }
    let summary = format!("kept unmerged branch {}", worktree.branch);
    let subject = match worktree.owner() {
        Some(owner) => feed_subject(pool, owner).await,
        None => None,
    };
    let _ = feed::append(
        pool,
        Some(&worktree.project_id),
        "worktree_branch_kept",
        &summary,
        worktree.owner().and_then(Owner::feed_run_id),
        subject.as_ref(),
    )
    .await;
}
