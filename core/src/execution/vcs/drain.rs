//! The VCS executor seam and queue-draining worker.

use super::*;

/// The núcleo↔git boundary, the same seam `runner.rs` gives the núcleo↔model one: this module
/// decides *when* an operation may run and records how it ended, and this trait is the only thing
/// that knows how to actually perform one. Chunk 1 has only the test double below, which is why
/// nothing here builds an argv yet.
///
/// Chunk 2's real `GitExecutor` belongs in **its own module** (`git_exec.rs`), not in this file.
/// Everything it needs — argv construction, a deadline, an output ceiling, capturing what the
/// subprocess printed — is process transport, and the two comparable concerns in this crate,
/// `gate.rs` and `transcribe.rs`, are each their own module for exactly that reason. Putting it here
/// would make one file both the queue domain and the process transport, which is the coupling
/// `core/AGENTS.md`'s module map exists to prevent.
#[async_trait::async_trait]
pub trait VcsExecutor: Send + Sync {
    /// Performs the claimed operation and reports how it ended.
    ///
    /// An `Outcome` rather than a `Result` because a git command that exits non-zero is not an error
    /// of this call — it is the answer. A conflicted merge is a `Failed` the queue must record
    /// against the row, not a failure to have asked.
    async fn execute(&self, request: &ClaimedRequest) -> Outcome;
}
/// Polls an `Unpin` future (the executor's boxed one) under `catch_unwind`, so a panic comes back as a value instead of unwinding the
/// caller. `AssertUnwindSafe` is sound here because the wrapped future is dropped on a panic and
/// never polled again.
struct CatchUnwind<F>(F);

impl<F: std::future::Future + Unpin> std::future::Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send>>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::pin::Pin::new(&mut this.0).poll(cx)
        })) {
            Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(panic) => std::task::Poll::Ready(Err(panic)),
        }
    }
}

/// How many times a merge's target may move while it is being computed before the row gives up and
/// says so, rather than retrying against something that keeps rewriting the target out from under
/// it. Design decision #4: once is the ordinary race with whatever else touches the branch; three
/// in a row is somebody writing to it directly, outside the queue, and the row's failure reason
/// says that rather than "resubmit" — there being nobody left to resubmit it.
pub(super) const MOVED_TARGET_RETRY_CEILING: i64 = 3;
/// Whether `outcome` is `publish`'s answer to a target that moved between computing a merge and
/// writing it — `git_exec::publish_by_update_ref` and `publish_by_fast_forward` both phrase it
/// identically, so matching the phrase is matching the case rather than duplicating a constant
/// across the two modules that would have to agree on it.
pub(super) fn outcome_is_a_moved_target(outcome: &Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Failed { reason, .. } if reason.contains("moved while the merge was being computed")
    )
}
/// What `requeue_after_moved_target` did.
pub(super) enum MovedTargetRetry {
    /// Back to `queued`, slot dropped. Nothing to finish — the row is not terminal.
    Requeued,
    /// `attempts` reached the ceiling. This is the `Failed` to finish with instead of the one
    /// `publish` produced, because "resubmit" is not the true cause any more; this is.
    CeilingReached(Outcome),
    /// The row was not `running` any more — a restart's `reconcile_interrupted` already took it.
    /// The caller falls through to `finish`'s own handling of exactly that race.
    RowGone,
}
/// Applies design decision #4 to one claimed request whose target moved while its merge was being
/// computed: increments `attempts`, and either sends it back to `queued` or, at the ceiling,
/// returns the `Outcome` to finish with instead.
///
/// Two statements rather than one `UPDATE ... RETURNING` with a `CASE` in every column, because the
/// ceiling's `Outcome::Failed` needs a `String` built in Rust — `format!`, not SQL — and a single
/// statement would have to decide the row's fate without knowing yet what that message says. Both
/// statements are scoped to `status = 'running'`, matching `finish`, so a row a restart already
/// reconciled is left exactly as the reconcile left it rather than overwritten by a late worker.
pub(super) async fn requeue_after_moved_target(
    pool: &sqlx::SqlitePool,
    id: i64,
) -> sqlx::Result<MovedTargetRetry> {
    let attempts: Option<i64> = sqlx::query_scalar(
        "UPDATE vcs_requests SET attempts = attempts + 1 WHERE id = ? AND status = 'running' \
         RETURNING attempts",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(attempts) = attempts else {
        return Ok(MovedTargetRetry::RowGone);
    };

    if attempts >= MOVED_TARGET_RETRY_CEILING {
        return Ok(MovedTargetRetry::CeilingReached(Outcome::Failed {
            reason: format!(
                "the target moved while this merge was being computed, {attempts} times in a \
                 row — something is writing to it directly, outside the queue, so this landing is \
                 giving up rather than retrying forever"
            ),
            exit_code: None,
            output_tail: String::new(),
        }));
    }

    let requeued = sqlx::query(
        "UPDATE vcs_requests SET status = 'queued', started_at = NULL WHERE id = ? AND status = 'running'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    if requeued.rows_affected() != 1 {
        return Ok(MovedTargetRetry::RowGone);
    }
    Ok(MovedTargetRetry::Requeued)
}
/// Claims, executes and finalizes exactly one request for one repository, and says whether it found
/// anything to do — so a caller can drain until this returns `false` and only then wait.
///
/// Nothing is propagated, because there is no caller left who could act on a `Result`: this is the
/// step a polling loop repeats. A claim that errored has already dealt with its own row —
/// `claim_next` either records it terminal or rolls the whole claim back to `queued`, so either way
/// this repository is not left holding it. What a failed *terminal* write costs is argued at the
/// call site, where it gets read.
///
/// `true` means a request was claimed and executed — including when the terminal write then failed,
/// because the work did happen and a drain loop must not read that as "the queue was empty". Neither
/// failure path spins: a *refused* write leaves the row terminal, so the next claim moves on to the
/// next request, and a *lost* one leaves it `running`, so the next claim finds the repository busy
/// and returns `false`.
///
/// **Cancellation** (`core/AGENTS.md` § "Cancellation safety"). `claim_next` could make its claim and
/// its compensating release uncancellable by putting them in one transaction; this cannot use the
/// same answer. The await in the middle is git, running for as long as a merge takes, and SQLite
/// admits one writer at a time — a transaction held open across it would stall every other writer in
/// the daemon. Rolling one back would be worse than slow: it would un-claim a row whose git command
/// had already run, erasing the only record that the repository was touched.
///
/// So the window is real and is left open on purpose. A drain dropped between the claim and `finish`
/// leaves its row `running`, and the partial unique index makes that row hold the repository's only
/// slot until the next startup's `reconcile_interrupted` releases it — the jam AGENTS.md describes,
/// in the strong form runs and jobs left behind in migration 0053 and this queue keeps, because a
/// repository has exactly one slot. What keeps it acceptable is who calls this:
/// the only production caller is the **detached task** `run_queue_worker` spawns per repository, and
/// a detached task's future is dropped only at runtime shutdown, which is precisely the case
/// `reconcile_interrupted` exists for — `main.rs` runs that reconcile before it spawns the worker.
///
/// Two consequences of it being *detached* that are easy to get wrong, and one of them is a trap
/// waiting for whoever adds graceful shutdown. **Aborting `run_queue_worker` does not stop a drain
/// already in flight**: the loop owns no handle to the tasks it spawns, so an abort drops the poller
/// and leaves every running merge running — which is what the worker tests do at teardown, and why
/// they are not evidence of a clean stop. And the exposure is now one open window *per repository*
/// rather than one for the daemon, since each repository's drain is its own task.
///
/// Both halves of that are run rather than argued —
/// `a_drain_abandoned_mid_operation_jams_the_repository_until_a_restart_reconciles` drops a drain
/// inside the operation, shows the repository jammed, and then shows the reconcile releasing it.
///
/// **Do not await this inside an HTTP handler.** A client disconnecting mid-merge would strand the
/// row and jam that repository until a restart, and unlike a lost reply nobody would see it happen.
/// A handler that wants a drain goes through `http::uncancellable` (AGENTS.md rule 2) — whose spawn
/// needs owned `'static` arguments, which is a different signature from this one.
///
/// The one thing this call *can* narrow, it does: nothing is awaited between the executor returning
/// and `finish` writing the outcome, so the exposure is the operation itself and not a line longer.
/// An await added there — a feed append, a notification — would widen it for nothing; those belong
/// after the terminal write.
pub async fn drain_once(
    pool: &sqlx::SqlitePool,
    repo_key: &str,
    executor: &dyn VcsExecutor,
) -> bool {
    // Spec §7's pull half, and the reason the other writers of a run's terminal status do not each
    // need a sweep of their own — `reap_requests_of_ended_runs` argues the whole case. It goes
    // before the claim because after it the row would already be `running`, which the reap leaves
    // alone by design.
    //
    // Best-effort, like the feed appends below: a reap that could not run leaves stale requests a
    // human can cancel, whereas returning early on it would stop the queue for this repository
    // outright — including for every request whose run is perfectly alive.
    if let Err(error) = reap_requests_of_ended_runs(pool, repo_key).await {
        tracing::warn!(
            repo_key = %repo_key,
            %error,
            "could not reap the vcs requests of runs that have already ended"
        );
    }
    let claimed = match claim_next(pool, repo_key).await {
        Ok(Some(claimed)) => claimed,
        // Nothing queued, something already running, or only unapproved rows — all "come back
        // later", and the caller waits the same way for each.
        Ok(None) => return false,
        Err(error) => {
            tracing::warn!(
                repo_key = %repo_key,
                %error,
                "could not claim the next vcs request"
            );
            return false;
        }
    };
    // A `BranchDelete` is judged against it, and a `Merge` reads it for the test-map guard.
    // Resolved here rather than in `claim_next`: it runs git, and a claim is one transaction that
    // has no business holding the database while a subprocess starts. An unresolvable project keeps
    // `None`, which leaves git judging against HEAD as it always did, and git still refuses anything
    // unmerged either way. So the fallback costs a refusal and never a lost commit.
    let mut claimed = claimed;
    if matches!(claimed.op, Op::BranchDelete { .. } | Op::Merge { .. }) {
        match crate::execution::land::integration_branch(
            pool,
            &claimed.project_id,
            std::path::Path::new(&claimed.project_root),
            std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT,
        )
        .await
        {
            Ok(branch) => claimed.integration_branch = Some(branch),
            Err(reason) => tracing::warn!(
                vcs_request_id = claimed.id,
                %reason,
                "vcs: a branch delete or merge has no integration branch to be judged against: \
                 git judges a delete against the main checkout's HEAD, and a merge is held for \
                 the owner's approval whenever it changes the test map"
            ),
        }
    }
    let id = claimed.id;
    let outcome = match CatchUnwind(executor.execute(&claimed)).await {
        Ok(outcome) => outcome,
        // A panic in the executor must not leave the row `running`: the partial unique index would
        // hold the repository's only slot until a restart reconciled it. It becomes an ordinary
        // failure, written by the same `finish` below.
        Err(panic) => {
            let detail = panic
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_owned());
            tracing::error!(vcs_request_id = id, %detail, "vcs: the executor panicked");
            Outcome::Failed {
                reason: format!("the executor panicked: {detail}"),
                exit_code: None,
                output_tail: String::new(),
            }
        }
    };

    // Design decision #4. `publish` answers "the target moved while this merge was being
    // computed" with a `Failed` nobody resubmits — the row just sits there having said "resubmit"
    // to an asker who, by the whole point of this pillar, is not watching for it. A merge is a
    // `git_exec::run_git` invocation the queue itself can redo for the cost of one more claim, so
    // it does: the SAME row goes back to `queued` and drops the slot, up to a ceiling, rather than
    // ending on a message nobody reads. Scoped to `Op::Merge` and `Op::Revert` — a revert is
    // computed in the integration worktree and published by the very same compare-and-swap, so a
    // moved target means the same thing for it. Every other operation's "moved" is a different
    // failure with a different remedy, and none of the rest publishes by compare-and-swap against
    // a target this queue does not own the way it owns a merge's.
    let outcome = if matches!(claimed.op, Op::Merge { .. } | Op::Revert { .. })
        && outcome_is_a_moved_target(&outcome)
    {
        match requeue_after_moved_target(pool, id).await {
            Ok(MovedTargetRetry::Requeued) => return true,
            Ok(MovedTargetRetry::CeilingReached(replacement)) => replacement,
            // The row is no longer `running` — a restart's `reconcile_interrupted` already took it,
            // the same race `finish`'s own `RowNotFound` arm below is written to catch. Falling
            // through with the original outcome lets that arm log it exactly as it would have
            // without this branch existing at all.
            Ok(MovedTargetRetry::RowGone) => outcome,
            Err(error) => {
                tracing::warn!(
                    vcs_request_id = id,
                    repo_key = %repo_key,
                    %error,
                    "vcs: could not requeue a merge whose target moved; recording the original \
                     failure instead"
                );
                outcome
            }
        }
    } else {
        outcome
    };

    // Cloned rather than moved so the failure paths below can still name it. `finish` consumes the
    // outcome, and a refused write would otherwise drop the only copy of a sha that git really
    // produced — leaving a commit the daemon caused recorded nowhere in the system at all.
    //
    // The terminal write now rides a transaction so a land's distillation cause is queued with it.
    // `finish` still goes FIRST, which keeps the deferred-begin write-first argument above, and the
    // queueing is best effort: its failure is logged and never fails or rolls back the write.
    let finished = async {
        let mut tx = pool.begin().await?;
        finish(&mut *tx, id, outcome.clone()).await?;
        if matches!(outcome, Outcome::Succeeded { .. })
            && let Err(error) = crate::distill::enqueue_landed_in(&mut tx, id).await
        {
            tracing::warn!(request_id = id, %error, "distill: could not queue a landed job");
        }
        tx.commit().await
    }
    .await;
    match finished {
        // Spec §6.4's fifth step, and spec §2.1's whole argument for this pillar having no view of
        // its own: every transition writes to `feed.rs`, which the shell already shows. Without this
        // row, a merge the daemon performed is invisible to the person who asked for it.
        //
        // Best-effort with `let _`, this crate's convention for observational writes
        // (`reconcile_interrupted` above, and `runs.rs`, `job.rs`, `scheduler.rs`): a feed row that
        // cannot be written must not undo the terminal write it is only reporting on.
        //
        // **Only on `Ok`, and that is not decoration.** `finish` is scoped to `status = 'running'`,
        // so it returns `RowNotFound` when a restart's `reconcile_interrupted` took the row first.
        // An unconditional append would then announce "vcs request 7 succeeded" in the one surface
        // spec §2.1 says the user looks at, while the row itself reads `interrupted`.
        //
        // The status comes from the outcome this function already holds, never from re-reading the
        // row — the row is what the feed is reporting on, and reading it back would report whatever
        // won a race rather than what this operation did.
        //
        // `claimed.run_id` for `run_id`, not a re-read of the row: same argument as the line above,
        // and the same reason `ClaimedRequest.run_id`'s own doc comment gives for carrying the field
        // at all — this claim is the one true account of what was executed and for whom, and the row
        // could have moved on by the time anyone went back to look at it.
        //
        // **`Origin::Run`, `needs_approval`, `cancel_for_run` and `reap_requests_of_ended_runs` are no
        // longer dormant in the way this comment used to mean it.** `submit_declared` is a real,
        // production-shaped door for a `Run`-origin row: it admits straight to `queued` on the
        // strength of a project's PRIOR declaration (`project_policy::declare_git_op`, listed and
        // withdrawn through `/projects/{id}/git-ops`), and it is called from inside the
        // PreToolUse hook — the moment a session's own git command is classified — on the strength of
        // what the PROJECT declared in advance, not on the run's behalf. That is the shape `auth.rs`
        // is left untouched for: no route opens to `Scope::Run`, because the daemon submitting a
        // declared operation is never the run asking for itself — it is the hook honouring a consent
        // Admin already gave, exactly as `runs::resume_approved_run`'s door is a human giving one in
        // the moment. `auth.rs`'s own argument — `POST /vcs/requests` is a sibling of `/email/send`
        // rather than of `/runs`, *"Queueing is Admin's"* — holds for the declared door for the same
        // reason it holds for the ordinary one: it was Admin who authorised this, whichever moment
        // that authorisation was actually given in.
        Ok(()) => {
            // **An escalation carries its reason into the summary; nothing else does.** The other
            // statuses are answers to a question somebody asked and is waiting on, so the id and
            // the word are enough to send them to the row. An escalation is the opposite: it says a
            // person now owns something they did not ask about and are not waiting for, and
            // "vcs request 21 escalated" gives them no way to know whether it is theirs without
            // going to look. The feed is the only surface spec §2.1 gives this module, so it is
            // where the reason has to be legible or it is nowhere.
            let summary = match &outcome {
                Outcome::Escalated { reason, .. } => {
                    format!("vcs request {id} escalated — {reason}")
                }
                Outcome::AwaitingOwner { .. } => {
                    format!("vcs request {id} changes the test map and waits for your approval")
                }
                other => format!("vcs request {id} {}", other.status()),
            };
            let _ = crate::feed::append(
                pool,
                Some(claimed.project_id.as_str()),
                "vcs_request_finished",
                &summary,
                claimed.run_id,
                Some(&crate::feed::Subject::Vcs(id)),
            )
            .await;
            // After the terminal write on purpose: cleanup is best-effort and detached, and
            // nothing may be awaited between the executor and `finish` (see the doc on
            // `drain_once`). Only a landing that really succeeded leaves a target dir to drop.
            if matches!(outcome, Outcome::Succeeded { .. }) {
                crate::execution::land::clean_after_landing(pool, &claimed);
            }
        }
        // Not a lost write: `finish` is scoped to `status = 'running'`, so this is the row being
        // taken out from under the operation — a restart's `reconcile_interrupted` already marked
        // it `interrupted`, the collision `a_reconciled_request_cannot_be_finished_by_a_late_worker`
        // covers. The repository is NOT jammed; the row is terminal and the queue moves on. What
        // is lost is the outcome, which the row is now refusing, so this log line is the only
        // place it survives.
        Err(sqlx::Error::RowNotFound) => tracing::warn!(
            vcs_request_id = id,
            repo_key = %repo_key,
            ?outcome,
            "a vcs request stopped running before its outcome arrived; the row refused it, so it is recorded here"
        ),
        // Anything else is the write itself failing, and that one does jam. This is the write
        // that releases the repository: without it the row stays `running` and every later
        // request for this repository waits behind it until a restart reconciles, so the log
        // line is the only account of why the queue stopped.
        Err(error) => tracing::error!(
            vcs_request_id = id,
            repo_key = %repo_key,
            ?outcome,
            %error,
            "could not record how a vcs request ended; it stays running until the daemon restarts"
        ),
    }
    true
}
/// How often the worker looks for repositories with queued work.
///
/// Much shorter than this daemon's other background loops (`scheduler.rs` 30s, `repo_trigger.rs`
/// 5min, `worktree::run_gc` 30min) because this is the only one a human is actively waiting on: they
/// asked for a merge and are watching for it. The cost of the interval is one query that the
/// `vcs_requests_queued` partial index covers exactly, over an index that is *empty* whenever
/// nothing is queued — which is almost always.
///
/// A `tokio::sync::Notify` would remove the interval entirely and is the obvious next step if this
/// ever shows up in a profile. It is not here yet because it has to be signalled from `submit`, which
/// would give the queue a second way to be woken and a second way to be missed — worth it for real
/// latency, not for 500ms.
pub(super) const WORKER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
/// Drains every repository with queued work, forever. Spawned once by `main.rs`.
///
/// One task per repository per tick, rather than a loop over repositories: draining them in sequence
/// would make a ten-minute fetch in one project the reason another project's merge is late, and
/// "separate repositories do not wait on each other" is the promise the whole per-repository locking
/// design exists to keep.
///
/// **No bookkeeping of which repositories are already draining, deliberately.** A redundant task for
/// a repository that is already busy is not a hazard: its `claim_next` finds a `running` row, returns
/// `None`, and the task exits — the database is the arbiter, exactly as it is for everything else
/// here. The cost is one index-covered query per tick per busy repository, and what it buys is that
/// there is no in-memory set that can disagree with the database about who holds what.
///
/// One consequence worth naming before somebody reads it as a defect: a redundant claim can also
/// exhaust `busy_timeout` (10s, `storage.rs:69`) against another writer and come back
/// `Err(SQLITE_BUSY)` rather than `Ok(None)` — `claim_next`'s own doc comment describes this. It is
/// still safe, because nothing was claimed and `drain_once` returning `false` ends the loop rather
/// than spinning, but it surfaces as a `could not claim the next vcs request` warning that is
/// expected under contention.
///
/// Each spawned task drains until its repository is empty rather than taking one request, so the
/// second of two queued merges does not wait a tick for no reason.
pub async fn run_queue_worker(pool: sqlx::SqlitePool, executor: std::sync::Arc<dyn VcsExecutor>) {
    let mut interval = tokio::time::interval(WORKER_POLL_INTERVAL);
    loop {
        interval.tick().await;

        let repositories: Vec<String> = match sqlx::query_scalar(
            "SELECT DISTINCT repo_key FROM vcs_requests WHERE status = 'queued'",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(repositories) => repositories,
            // Best-effort, like every other polling loop in this crate: a failed poll is the next
            // tick's problem, not a reason to stop draining every repository for ever.
            Err(error) => {
                tracing::warn!(%error, "vcs: could not look for repositories with queued work");
                continue;
            }
        };

        for repo_key in repositories {
            let pool = pool.clone();
            let executor = std::sync::Arc::clone(&executor);
            tokio::spawn(
                async move { while drain_once(&pool, &repo_key, executor.as_ref()).await {} },
            );
        }
    }
}
