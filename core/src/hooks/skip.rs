//! Putting a job item down: the nodes that may give up, and skipping the item.

use super::*;

/// A job's node asked for a decision, so the job puts the item down and carries on without it.
///
/// The other half of `pause_for_approval`, and it exists because parking is the wrong answer for a
/// job. A run belongs to a person who is going to come back to it; a job is the thing that was
/// supposed to work while nobody was watching, and stopping it dead on the first unrecognised shell
/// command is what "work through the night" met in practice — measured on 2026-08-07, where a
/// two-item job in a four-file repository parked seven times.
///
/// **The order of the three writes below is the whole safety argument, and it is not arbitrary.**
///
/// 1. The run is already terminated by the caller. That `.await` is the one `core/AGENTS.md` names
///    as biting hardest: it kills the CLI whose hook script owns the connection being answered, so
///    everything after it runs on borrowed time.
/// 2. **Mark the item.** First, and before anything that can fail or block — done by the caller in
///    `put_the_item_down`, because whether the mark landed is also what decides that this road is
///    the right one at all. An item left `running` in a job nobody is driving is a job that answers
///    `Wait` for ever — `next_step` sees a live node, there is no live node, and no later pass
///    rescues it. Every other loss here is recoverable; that one is not.
/// 3. **Take the pause off the run.** The caller parked it to ask; the asking is over. A run left
///    `awaiting_approval` parks the whole job through `job::node_awaiting_approval`, which is the
///    same stop by another door — the item reads `skipped` and the job waits on it anyway.
///    Recoverable, unlike (2): `reconcile_stranded_approvals` writes the same `interrupted` at the
///    next startup, which is why it goes second and not first.
/// 4. Revert the tree, then record the proposal. Both may fail, and neither failure is allowed to
///    take the mark with it: an item skipped without a proposal is work nobody will be reminded of,
///    which is bad and survivable, where an item stuck `running` is a dead job.
pub(super) struct SkippedItem {
    pub(super) run_id: i64,
    pub(super) job_id: i64,
    pub(super) session_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) tool_name: String,
    pub(super) tool_input: String,
    pub(super) reason: String,
    /// Whether this run owned an item whose half-written edits have to be undone.
    ///
    /// False for a review node, the other thing that takes this road, and the difference is not
    /// cosmetic. A review node owns no item, so `footing_for_run` would answer for whichever item
    /// ran last — and reverting to that footing would throw away the checkpoint of the job's final
    /// item, which is the one thing the review existed to look at. A node that changes nothing by
    /// design has nothing to revert.
    pub(super) had_an_item: bool,
}

/// The job stages that give their node up rather than parking the job on it.
///
/// Both own no item, and both already have an ending written for the case where their run does not
/// come back: `ingest_replan`'s failure arm hands the job to `stop_after_replan` (`Stopped`, with
/// the rounds that ran still on the branch), and `load_view` reads any terminal review as `Done`
/// ("a review that failed is still a review that happened").
///
/// `plan` is deliberately absent. Its ending exists too — `Outcome::Failed` — but it is the only one
/// that throws away a whole job to save a slot: nothing has been done yet, so there is no partial
/// work to preserve, and a single answer turns that same job into a night's work.
pub(super) const NODES_THAT_GIVE_UP: [&str; 2] = ["review", "replan"];

/// Step (2) of the skip, and the question that decides whether there is a skip at all: mark this
/// run's item `skipped`, and say whether there was one.
///
/// Scoped to the item this run owns, so a job whose other items are in flight is untouched. A
/// `false` answer has two shapes and the caller treats them alike, because the right move is the
/// same for both — park and ask:
///
/// - **No item exists.** A plan or review node. There is nothing to put down, and skipping a job's
///   plan would leave it with no queue to carry on with.
/// - **The item is no longer `running`.** A cancel or a reconcile got there first and its verdict is
///   the newer one; overwriting it would be this handler talking over a decision already made.
///
/// A database error also answers `false`, which parks the run rather than skipping an item that may
/// still be `running`. Loud, and recoverable by hand — where a job stalled on a live item is not.
pub(super) async fn put_the_item_down(pool: &sqlx::SqlitePool, job_id: i64, run_id: i64) -> bool {
    let marked = sqlx::query(
        "UPDATE job_nodes SET status = ? WHERE job_id = ? AND run_id = ? AND status = 'running'",
    )
    .bind(crate::execution::job::STATUS_SKIPPED)
    .bind(job_id)
    .bind(run_id)
    .execute(pool)
    .await;
    match marked {
        Ok(result) if result.rows_affected() == 1 => true,
        Ok(_) => {
            tracing::info!(
                run_id,
                job_id,
                "pretooluse-decision: no running job item for this run — parking it instead"
            );
            false
        }
        Err(error) => {
            tracing::error!(
                run_id,
                job_id,
                %error,
                "pretooluse-decision: could not mark a job item skipped — parking the run instead"
            );
            false
        }
    }
}

pub(super) async fn skip_the_item(state: AppState, item: SkippedItem) {
    let SkippedItem {
        run_id,
        job_id,
        session_id,
        project_id,
        tool_name,
        tool_input,
        reason,
        had_an_item,
    } = item;

    // (3) Take the pause off the RUN. The caller terminated it to `awaiting_approval`, because at
    // that point the answer was still "ask a person". It is not any more — the answer was "skip it",
    // and it has already been given.
    //
    // Without this the job stops anyway, one step later and for a different reason:
    // `job::node_awaiting_approval` asks whether ANY run of the job is `awaiting_approval` and parks
    // the whole job when one is, and `node_in_flight` counts that status as a node still to wait
    // for. The item would read `skipped` while the job sat on a node nobody would ever answer —
    // measured on 2026-08-08, job 3: item 0 `skipped`, job `awaiting_approval`, nothing pending.
    //
    // `interrupted` and not something new: it is what `reconcile_stranded_approvals` writes at
    // startup for precisely this row (an `awaiting_approval` run with no `action-approval` proposal),
    // and what `pause_for_approval` rolls back to when a pause turns out not to be one. Writing it
    // here is the same verdict without waiting for a restart.
    //
    // AFTER the item's mark, never before, for two reasons. If this write is lost the damage is
    // recoverable — that same startup reconciler writes it — where an item left `running` is a job
    // that answers `Wait` for ever and no pass rescues. And the window between the two writes is
    // only safe in this order: `job::reconcile_nodes` folds back every item that is still `running`
    // whose run has left flight, and would read this one as `failed` and stop the chain. Marked
    // first, the item is no longer `running` and that query cannot see it at all.
    //
    // Guarded on the status the caller set, so a cancel that landed in between keeps the last word.
    let unpaused = sqlx::query(
        "UPDATE runs SET status = 'interrupted', completed_at = ?
         WHERE id = ? AND status = 'awaiting_approval'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(&state.pool)
    .await;
    if let Err(error) = unpaused {
        tracing::error!(
            run_id,
            job_id,
            %error,
            "pretooluse-decision: could not lift the pause off a skipped item's run — the job will park until restart"
        );
    }

    // (4a) Put the tree back. The item wrote whatever it wrote before it asked, and the next item
    // must not build on a half-done change nobody approved.
    //
    // Only for an item. A review node reaches this function too and must NOT come through here:
    // it owns no item, so `footing_for_run` would answer for whichever one ran last, and reverting
    // to that footing would discard the checkpoint of the job's final item — the very work the
    // review was there to read. A node that changes nothing by design has nothing to put back.
    if had_an_item {
        // One call and not two. The tree and the sha are a pair — reverting a checkout to a footing
        // taken from a different one is worse than reverting nothing — and asking for them together
        // is what keeps them from being resolved off different keys, which is how a node working in
        // its item's own tree came to have the job's reverted instead.
        match crate::execution::job::revert_target(&state.pool, job_id, run_id).await {
            Some((worktree, sha)) => {
                if let Err(error) = crate::execution::worktree::revert_to(&worktree, &sha).await {
                    tracing::warn!(run_id, job_id, %error, "pretooluse-decision: could not revert a skipped item");
                }
            }
            None => tracing::warn!(
                run_id,
                job_id,
                "pretooluse-decision: no worktree or no footing to revert a skipped item to"
            ),
        }
    }

    // (4b) The proposal. A different `kind` from an action approval, and `wip.rs` counts only the
    // other one: this is work NOT YET DONE waiting on a decision, where the WIP limit exists to cap
    // work already done waiting to be looked at. Conflating them closes the autonomy this change
    // just opened, at the third skipped item of the night.
    if let Err(error) = crate::proposals::create_skipped_item(
        &state.pool,
        run_id,
        session_id.as_deref(),
        project_id.as_deref(),
        &tool_name,
        &reason,
        Some(&tool_input),
    )
    .await
    {
        tracing::warn!(run_id, job_id, %error, "pretooluse-decision: failed to record a skipped-item proposal");
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "proposal_record_failed",
            &format!("failed to record skipped-item proposal: {error}"),
            Some(run_id),
            Some(&crate::feed::run_subject(&state.pool, run_id).await),
        )
        .await;
        // Deliberately NOT rolled back, where the action-approval path above rolls its pause back.
        // There the run is stuck `awaiting_approval` with nothing to approve, holding an index that
        // blocks the project. Here the item is `skipped`, the job moves on, and what is lost is the
        // reminder — a worse outcome than having it, and a far better one than a job that stalls.
    }
}
