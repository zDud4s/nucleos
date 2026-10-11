//! Reconciliation and retention cleanup for interrupted or finished requests.

use super::*;

/// Marks every request still `running` at startup as `interrupted` — the daemon died mid-operation,
/// and nothing can say whether git finished. Called once at startup, the same moment
/// `runs::reconcile_orphaned_runs` runs its counterpart pass over `runs`.
///
/// No auto-retry: a re-run `merge` is harmless, a re-run `tag` is not, and telling the two apart
/// from a cold start is guessing. The row is left `interrupted` with a reason a human can act on,
/// not silently re-queued.
///
/// One statement, not a `SELECT` then an `UPDATE`: nothing else is racing a fresh startup for these
/// rows, so the two-step shape `claim_next`'s doc comment warns against is not the risk here — the
/// single statement is simply the smaller diff to read `RETURNING id, project_id, run_id` off.
///
/// The feed write is best-effort per row (this crate's convention — see `runs.rs`, `job.rs`,
/// `scheduler.rs`): it is observational, so a write it cannot make must not undo the row it is
/// only reporting on. Contrast `finish`, where the terminal write itself is load-bearing.
pub async fn reconcile_interrupted(pool: &sqlx::SqlitePool) -> sqlx::Result<u64> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    let reconciled: Vec<(i64, String, Option<i64>)> = sqlx::query_as(
        "UPDATE vcs_requests
            SET status = 'interrupted', finished_at = ?,
                failure_reason = 'daemon restarted mid-operation'
          WHERE status = 'running'
         RETURNING id, project_id, run_id",
    )
    .bind(finished_at)
    .fetch_all(pool)
    .await?;
    for (id, project_id, run_id) in &reconciled {
        let _ = crate::feed::append(
            pool,
            Some(project_id.as_str()),
            "vcs_request_interrupted",
            &format!("vcs request {id} interrupted: daemon restarted mid-operation"),
            *run_id,
            Some(&crate::feed::Subject::Vcs(*id)),
        )
        .await;
    }
    Ok(reconciled.len() as u64)
}
/// Cancels every request run `run_id` asked for that has not started, and returns how many.
///
/// **`running` is deliberately excluded.** Spec §7: an operation already in flight finishes. A merge
/// abandoned half-way is worse than one nobody is waiting for any more, and the queue could not stop
/// it in any case — the git it spawned belongs to the daemon, not to the run whose context ran out.
/// What this reclaims is the place a dead run would otherwise hold in the FIFO.
///
/// `awaiting_approval` is swept alongside `queued`, and that is the arm that matters most: a request
/// nobody has approved yet, belonging to a run that no longer exists, would otherwise sit there until
/// a human approved work for an agent that is gone.
///
/// **Why this cannot race the claim**, which is worth writing down because it is conditional on
/// something a future edit could break: `claim_next` opens a transaction whose FIRST statement is its
/// conditional UPDATE, so it takes SQLite's single write lock outright. Either the claim commits
/// first — the row is `running`, and the filter below excludes it — or this commits first and the
/// claim's subquery finds no `queued` row. There is no window in which a row goes `cancelled` while
/// git is running against it. And even if there were, `finish` is scoped to `status = 'running'`, so
/// a lost race lands in `drain_once`'s existing warn arm rather than overwriting a terminal status.
///
/// The feed write is best-effort per row, this crate's convention for observational writes: a feed
/// row that cannot be written must not undo the cancellation it is only reporting on.
///
/// `approved_map_blob IS NULL` spares a row the owner approved while it waited: the owner's decision,
/// not the run's end, settles it. Only `approve_for_owner` ever sets that column.
pub async fn cancel_for_run(pool: &sqlx::SqlitePool, run_id: i64) -> sqlx::Result<u64> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    let cancelled: Vec<(i64, String)> = sqlx::query_as(
        "UPDATE vcs_requests
            SET status = 'cancelled', finished_at = ?,
                failure_reason = 'the run that asked for this ended before it started'
          WHERE run_id = ? AND status IN ('queued', 'awaiting_approval')
            AND approved_map_blob IS NULL
         RETURNING id, project_id",
    )
    .bind(finished_at)
    .bind(run_id)
    .fetch_all(pool)
    .await?;

    for (id, project_id) in &cancelled {
        let _ = crate::feed::append(
            pool,
            Some(project_id.as_str()),
            "vcs_request_cancelled",
            &format!("vcs request {id} cancelled with run {run_id}"),
            Some(run_id),
            Some(&crate::feed::Subject::Vcs(*id)),
        )
        .await;
    }

    Ok(cancelled.len() as u64)
}
/// Retires every request in this repository whose submitting run is no longer alive — it ended, or
/// its row is gone — and returns how many. Called by `drain_once` before it claims.
///
/// **This is the pull half of spec §7's "the agent that submitted dies", and it exists because the
/// push half cannot be complete.** `runs::finalize_termination` sweeps the paths that go through it,
/// but a run's terminal status is also written by direct UPDATEs elsewhere, and the next one will be
/// written by somebody who does not know the list exists. Asking here — at the one point that must be
/// correct anyway, because it is where a merge is about to be executed — makes the guarantee true by
/// construction rather than by everyone remembering.
///
/// "Elsewhere" is deliberately not a number. It was written as "four other places" and an audit put
/// the real count at roughly twice that, which is the argument for this function rather than against
/// it — but a count in a comment is a claim that goes stale on its own, and this one would go stale
/// in the direction of sounding smaller than it is.
///
/// `cancel_for_run` is kept alongside it and is not redundant: it makes a cancelled run's requests
/// disappear *immediately*, rather than at the next poll of a repository that may have nothing else
/// queued for hours.
///
/// **The three cases, decided here rather than left to be reassembled by a reader.**
///
/// 1. *The run is alive* — including paused at `awaiting_approval`, which resumes — and its request
///    is **kept**. This is the case the whole shape exists to protect: a merge cancelled out from
///    under a run that was only waiting on a human is work destroyed for no reason.
/// 2. *The run has ended* and its request is **reaped**, because nothing will ever come back for it.
/// 3. *The run's row is gone* and its request is **reaped too** — a row that is not there cannot be
///    running. Nothing ties `run_id` to `runs` (`0048_vcs_requests.sql` writes no foreign key) and
///    rows really are deleted from `runs`, so this is an ordinary state and not a corrupt one. Kept,
///    such a request would eventually be claimed and **executed**: a merge performed against the
///    repository on behalf of a run that no longer exists.
///
/// Case 3 is the only reason the predicate is `NOT EXISTS (… still alive …)` rather than the
/// `EXISTS (… already ended …)` it reads as the obvious spelling of. The two agree on every row
/// whose run row exists, and differ only when it does not — where this direction is the one spec §7
/// asks for.
///
/// The status list comes from `runs::ENDED_RUN_STATUSES` rather than being spelled here. Note what
/// is NOT in it: a run at `awaiting_approval` is paused for a human and will resume, so its merge
/// must survive; and a run that finished its work normally never reaches `finalize_termination` at
/// all, which is correct — it asked for the merge and should have it.
///
/// `running` is excluded by the same status filter `cancel_for_run` uses, and for the same reason
/// spelled out there: an operation already in flight finishes. Here it is also structural — this
/// runs *before* the claim, so there is nothing of this drain's in flight to protect.
///
/// The `IN` clause's placeholders are generated from the constant's length and every element bound,
/// rather than the list being interpolated into the string: a status is data, and a query built by
/// formatting values into SQL is the shape that stops being safe the moment one of them stops being
/// a literal somebody wrote by hand.
pub async fn reap_requests_of_ended_runs(
    pool: &sqlx::SqlitePool,
    repo_key: &str,
) -> sqlx::Result<u64> {
    let placeholders = vec!["?"; crate::execution::runs::ENDED_RUN_STATUSES.len()].join(", ");
    // `run_id IS NOT NULL` is **load-bearing**, and it is the `NOT EXISTS` shape that makes it so.
    // A NULL joins nothing, so for a request no run owns the subquery is empty and `NOT EXISTS` is
    // TRUE — without this line every human's request in the repository would be reaped as though
    // its run had vanished, which is the very case 3 below is about. It reads like the sentence the
    // reaper means (a request no run owns is not one a run can have abandoned) and it is also the
    // guard; `a_humans_request_is_never_reaped` is what holds it.
    //
    // `NOT EXISTS (… alive …)` rather than `EXISTS (… ended …)`: see this function's doc comment.
    // The two differ on exactly one row shape — a `run_id` naming a run row that is gone — and this
    // is the direction that retires it instead of queueing a merge for it for ever.
    // `approved_map_blob IS NULL` spares a request the owner approved while it waited (it is back
    // to `queued` with its run_id intact): the owner's decision, not the run's end, settles it.
    let sql = format!(
        "UPDATE vcs_requests
            SET status = 'cancelled', finished_at = ?,
                failure_reason = 'the run that asked for this had already ended when the queue reached it'
          WHERE repo_key = ? AND status IN ('queued', 'awaiting_approval')
            AND approved_map_blob IS NULL
            AND run_id IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM runs
                             WHERE runs.id = vcs_requests.run_id
                               AND runs.status NOT IN ({placeholders}))
         RETURNING id, project_id, run_id"
    );

    let finished_at = chrono::Utc::now().to_rfc3339();
    // `AssertSqlSafe` because sqlx 0.9 only trusts `&'static str`, and this string is built at
    // runtime. Audited, and the audit is short: the only interpolation is `placeholders`, which is
    // `n` question marks derived from a compile-time constant's length. No value reaches the SQL —
    // the statuses themselves are bound below, one per placeholder.
    let mut query = sqlx::query_as::<_, (i64, String, Option<i64>)>(sqlx::AssertSqlSafe(sql))
        .bind(finished_at)
        .bind(repo_key.to_string());
    for status in crate::execution::runs::ENDED_RUN_STATUSES {
        query = query.bind(*status);
    }
    let reaped = query.fetch_all(pool).await?;

    // Best-effort per row, this crate's convention for observational writes: a feed row that cannot
    // be written must not undo the cancellation it is only reporting on.
    for (id, project_id, run_id) in &reaped {
        let _ = crate::feed::append(
            pool,
            Some(project_id.as_str()),
            "vcs_request_cancelled",
            &format!("vcs request {id} cancelled: the run that asked for it had already ended"),
            *run_id,
            Some(&crate::feed::Subject::Vcs(*id)),
        )
        .await;
    }

    Ok(reaped.len() as u64)
}
/// How long a finished request keeps what git printed.
///
/// Thirty days, the same window `runs::prune_transcripts` gives a run's transcript, because it is
/// the same kind of thing: the tail is what a subprocess said, kept so a person can read why an
/// operation ended the way it did, and nobody reads that a month later. What people DO read months
/// later is the metadata — which operation, against which repository, which sha came out — and that
/// is a couple of hundred bytes.
///
/// The bulk is real rather than theoretical: `git_exec::OUTPUT_TAIL_BYTES` caps one tail at 8 KiB,
/// and a conflicted merge in a large repository reaches it.
pub const DEFAULT_OUTPUT_RETENTION_DAYS: i64 = 30;
/// The window, overridable for an operator who wants a different one — the shape
/// `runs::transcript_retention_days` uses, for the reason it gives.
pub fn output_retention_days() -> i64 {
    std::env::var("NUCLEOS_VCS_OUTPUT_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_OUTPUT_RETENTION_DAYS)
}
/// Empties the output of every finished request past the window, leaving the row.
///
/// **The row survives, and here that is not merely the tidier choice — it is the only correct one.**
/// `runs::prune_transcripts` keeps its rows because a feed entry, a job item or a proposal still
/// points at them. This table has a sharper version of the same fact: `action_grants.queued_request_id`
/// (migration 0054) names a request by id and carries **no foreign key** — checked, it is a bare
/// `INTEGER`. So a DELETE would not fail and would not cascade; it would leave a takeover grant
/// pointing at nothing, and `proposals::matching_queued_request` would go on telling a run that
/// request 7 holds its work.
///
/// It also discharges, rather than merely dodging, the obligation `wait_for` records: that function
/// answers `RowNotFound` immediately because a missing row *cannot ever arrive*, and its doc comment
/// says the first pruning pass in this module invalidates that silently. Emptying instead of
/// deleting keeps it true by construction.
///
/// `COALESCE(finished_at, created_at)` because a terminal row with no finish stamp is possible —
/// `reconcile_interrupted` writes one, but a hand-edited or half-written row need not — and ageing
/// such a row from nothing would exempt it for ever.
///
/// The cutoff is RFC 3339 built in Rust and never SQLite's `datetime('now','-N days')`, for the
/// reason `web::prune` sets out in full and the other three sweeps repeat: the two spellings are
/// compared as TEXT and `T` (0x54) sorts after the space (0x20), so within the cutoff's own day a
/// row hours too old compares as newer and survives every sweep for ever.
pub async fn prune_output_tails(
    pool: &sqlx::SqlitePool,
    retain_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64> {
    if retain_days <= 0 {
        // Zero would strip every tail on the machine at the next sweep, which is not a retention
        // policy but a typo with a plausible-looking value. The reading all three existing sweeps
        // make.
        return Ok(0);
    }

    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();
    // `AssertSqlSafe` because sqlx only trusts `&'static str` and this is built at runtime. The one
    // interpolation is a row of `?` derived from a compile-time constant's length; every status and
    // the cutoff are bound. The same audit `reap_requests_of_ended_runs` writes out above.
    let placeholders = vec!["?"; TERMINAL_STATUSES.len()].join(", ");
    let mut update = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE vcs_requests
            SET output_tail = NULL
          WHERE status IN ({placeholders})
            AND COALESCE(finished_at, created_at) < ?
            AND output_tail IS NOT NULL"
    )));
    for status in TERMINAL_STATUSES {
        update = update.bind(status);
    }
    // `output_tail IS NOT NULL` above is what makes the count mean something: without it every
    // eligible row is rewritten on every hourly sweep for ever, and the log would report the same
    // number until the end of time instead of the number of tails this pass actually dropped.
    Ok(update.bind(&cutoff).execute(pool).await?.rows_affected())
}
