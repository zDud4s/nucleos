//! Queue admission, claiming, completion, and outcomes.

use super::*;

/// Admits a request into the queue and returns its row id. Provenance and the project's declaration
/// decide the initial status: through this ordinary door, `Human`/`Shell` already carry their
/// approval and start `queued`; `Run`/`Job` are autonomous and start `awaiting_approval`.
///
/// **Nothing writes the transition out of `awaiting_approval`, and that is settled rather than
/// pending.** This said it belonged to Chunk 4 "alongside the `proposals.rs` wiring that grants it".
/// Chunk 4 landed and took the other road, the one `auth.rs` had already argued for: a run may not
/// queue on its own behalf at all (*"Queueing is Admin's"*), so `resume_approved_run` admits an
/// approved action directly as `Origin::Human` and no row ever starts at `awaiting_approval` in
/// production. `drain_once` carries the full account. This function still only writes the initial
/// state, and the `Run`/`Job` arm is the shape the day a scope exists that may ask for itself.
///
/// The repository arrives resolved rather than as fields to be trusted — see `ResolvedRepo`.
pub async fn submit(
    pool: &sqlx::SqlitePool,
    repo: &ResolvedRepo,
    op: &Op,
    origin: Origin,
) -> sqlx::Result<i64> {
    submit_on(pool, repo, op, origin).await
}
/// `submit`, for an operation kind the project declared in advance.
///
/// This is a door of its own rather than a branch inside `submit`, because the truth it asserts is
/// different: consent was given earlier and elsewhere, before this command existed. `Origin` must
/// still record who asked, so a run remains `Origin::Run(id)` rather than being rewritten as
/// `Origin::Human`. The latter would be the same kind of audit lie this module refuses for
/// `Origin::Shell`.
///
/// `Origin::needs_approval` answers whether the provenance brings its own approval. A human or
/// shell order does; a run or job request does not. Here the project's declaration brings the
/// approval instead, so the request starts `queued`. This does not wake or transition a row out of
/// `awaiting_approval`; it steps past that state when the row is created.
pub async fn submit_declared(
    pool: &sqlx::SqlitePool,
    repo: &ResolvedRepo,
    op: &Op,
    origin: Origin,
) -> sqlx::Result<i64> {
    admit(pool, repo, op, origin, "queued", false).await
}
/// `submit`, against a caller's own executor, so an admission can be part of a larger transaction.
///
/// It exists for one caller: approving a paused run's merge (`runs::resume_approved_run`) has to
/// admit the request in the SAME transaction that approves the proposal and resumes the run.
/// Neither order works outside one: admit-then-commit can queue a merge whose approval then rolls
/// back — an irreversible publication nobody authorised, and a proposal still pending so a human
/// can authorise it a second time — and commit-then-admit can resume a run told its merge is
/// queued when it is not.
///
/// Generic over the executor rather than taking a `&mut Transaction`, because `&SqlitePool` is one
/// too: `submit` is this function, and there is no second copy of the INSERT to drift from it.
pub async fn submit_on<'e, E>(
    executor: E,
    repo: &ResolvedRepo,
    op: &Op,
    origin: Origin,
) -> sqlx::Result<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let status = if origin.needs_approval() {
        "awaiting_approval"
    } else {
        "queued"
    };
    admit(executor, repo, op, origin, status, false).await
}
/// `submit`, for the landing that comes OUT of a conflict resolution.
///
/// Two things follow from the mark and neither can be recovered later, which is why it is written
/// with the row rather than stamped on afterwards. The executor VERIFIES a marked source before
/// merging it — a two-parent tip, no conflict markers — and a stamp applied after the INSERT has a
/// window the queue polls every 500ms, so the merge that most needs checking is the one likeliest to
/// be claimed unmarked. And a marked request never spawns a resolution of its own, which is what
/// stops a resolution that conflicts from minting an agent that lands, that conflicts, for ever.
///
/// A separate entry point rather than a flag on `submit`, because there is exactly one caller and
/// the truth it asserts is specific: *this branch was produced by a resolver*. Every other admission
/// reads unchanged, and the INSERT is still written once.
pub async fn submit_resolution(
    pool: &sqlx::SqlitePool,
    repo: &ResolvedRepo,
    op: &Op,
    origin: Origin,
) -> sqlx::Result<i64> {
    let status = if origin.needs_approval() {
        "awaiting_approval"
    } else {
        "queued"
    };
    admit(pool, repo, op, origin, status, true).await
}
pub(super) async fn admit<'e, E>(
    executor: E,
    repo: &ResolvedRepo,
    op: &Op,
    origin: Origin,
    status: &str,
    from_resolution: bool,
) -> sqlx::Result<i64>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    if !origin.may_queue(op) {
        return Err(sqlx::Error::Protocol(format!(
            "{} is queued only by the daemon itself (spec 2026-10-05 §6.2); a {} cannot ask for one",
            op.kind(),
            origin.as_str()
        )));
    }
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO vcs_requests (op, args, project_id, project_root, repo_key, origin, run_id, status, created_at, from_resolution)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(op.kind())
    .bind(op.to_args())
    .bind(repo.project_id())
    .bind(repo.root())
    .bind(repo.key())
    .bind(origin.as_str())
    .bind(origin.run_id())
    .bind(status)
    .bind(created_at)
    .bind(i64::from(from_resolution))
    .execute(executor)
    .await?;
    // Fired even when `executor` is an open transaction that has not committed yet: the worker then
    // finds nothing on this pass and the 30s fallback picks the row up, which is late, not lost.
    if status == "queued" {
        wake_worker();
    }
    Ok(result.last_insert_rowid())
}
/// A request the caller now holds: its row is already `running`, so nothing else for the same
/// repository can be claimed until `finish` writes a terminal status.
///
/// It carries everything an execution needs — the operation and where to perform it — because the
/// claim already read that row, and a worker that went back for `project_root` would be reading it
/// at a moment when the row it holds could no longer be trusted to be the same.
#[derive(Debug, Clone)]
pub struct ClaimedRequest {
    pub id: i64,
    pub op: Op,
    pub project_id: String,
    pub project_root: String,
    /// Whether a conflict resolver produced the branch this request wants merged.
    ///
    /// **It travels with the claim because the executor's behaviour differs on it**, which is the
    /// bar for widening this struct: a resolution's source branch has to be VERIFIED before it is
    /// merged — a two-parent tip and no conflict markers left in the tree — and an ordinary landing
    /// must not be, because neither is true of one. An executor that could not tell the two apart
    /// would either skip the check that exists to catch a flattened resolution, or apply it to every
    /// branch anybody ever asked to land.
    pub from_resolution: bool,
    /// Which run asked for this, if any — `None` for a human, a shell order, or a job.
    ///
    /// **This field deliberately lowers the bar above.** No executor branches on it — an audit field
    /// carries no behavioural difference for anything to differ ON — so by that bar alone it would
    /// not belong here. It is admitted anyway because the alternative is worse: the only other place
    /// to find it is the row itself, and `claim_next`'s own doc comment already warns what re-reading
    /// a row after the fact gets you — "whatever won a race" rather than what this claim actually
    /// held. `run_id` travels with the claim so the feed entry `drain_once` writes for it reports the
    /// run this operation was really performed for, not a row that may since have moved on.
    pub run_id: Option<i64>,
    /// The map blob the owner approved for this request, if any. The executor compares it with
    /// the map in the merge it computes, and anything else makes the row wait again (see
    /// `Outcome::AwaitingOwner`). Carried with the claim for the reason `from_resolution` is:
    /// the executor's behaviour differs on it.
    pub approved_map_blob: Option<String>,
    /// What a `BranchDelete` is judged against: the project's integration branch, resolved by
    /// `land::integration_branch` once the request is claimed (for a `Merge` too, where the test-map
    /// guard reads it). `None` for every other operation, and for a delete or merge whose project
    /// has no integration branch that can be resolved.
    ///
    /// **It travels with the claim because the executor's behaviour differs on it.** `git branch
    /// --delete` asks whether a branch is merged into its upstream, or into HEAD when it has none,
    /// and HEAD here is the project root's: whatever the main checkout happens to be parked on.
    /// Measured 2026-09-12: four branches already merged into `master` were refused four times over,
    /// because the main checkout stood on a feature branch that did not contain them. That is
    /// `land.rs`'s decision #2 defect, a target read off a checkout's HEAD, reached by the one
    /// operation that module does not build.
    pub integration_branch: Option<Branch>,
}
/// How a claimed request ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Ran, and did what was asked.
    ///
    /// **`sha` is optional because not every operation produces one**, and that is a fact about the
    /// vocabulary rather than a hedge. A merge, a push, a tag and a branch delete each name exactly
    /// one object id worth recording — what was published, what was sent, what was tagged, what was
    /// removed and could therefore be restored. A `fetch` names none: it moves however many
    /// remote-tracking refs the remote had news about, and picking one of them to put in the column
    /// would be inventing a headline.
    ///
    /// `result_sha` has been nullable since `0048_vcs_requests.sql` and `finish` already writes NULL
    /// for every non-`Succeeded` outcome, so this makes the type agree with the column rather than
    /// changing what the column can hold. An empty string would have been the alternative, and it is
    /// the worse one: a reader cannot tell it from a sha the executor failed to capture.
    Succeeded {
        sha: Option<String>,
        output_tail: String,
    },
    /// Ran and produced its result, which could not be published because the target worktree's
    /// uncommitted files are in the way. Terminal and never retried in a loop (spec §7): a working
    /// copy left dirty over an afternoon would otherwise hold the whole repository's queue.
    ///
    /// **The computed merge is not kept, and nothing here should be read as though it were.** A
    /// blocked row records no `result_sha` — `finish` writes `None` — and nothing ever looks one up,
    /// so a resubmission goes through `compute_merge` again and lands its own commit rather than
    /// publishing that one: a merge commit embeds its committer timestamp, and the clock has moved
    /// (the human had to commit or stash first), so it is not even the same sha. The first is left
    /// unreferenced and is `gc` fodder.
    ///
    /// That is the intended shape rather than a leak, and it is what makes the row terminal
    /// affordable: every object the recompute needs is already in this repository, so redoing it
    /// costs one merge in a worktree nobody is standing in — while the alternative, a terminal row
    /// holding a sha that is on no branch, is something somebody would eventually try to publish.
    Blocked { reason: String, output_tail: String },
    /// Ran and failed. A raced publish is this.
    ///
    /// **A conflicted merge used to be this and is now `Escalated`**, which is the difference
    /// between an operation that did not happen and one that did not happen and still has an owner.
    Failed {
        reason: String,
        exit_code: Option<i32>,
        output_tail: String,
    },
    /// Ran, could not complete, and **a person now owns it**. A conflicting merge is this.
    ///
    /// Terminal, and the terminality is the point rather than a limitation: nothing here resumes on
    /// its own, so the escalation ends THIS request and whatever a person decides arrives as a new
    /// one. A status that meant "paused, pending a human" would have to be excluded from the
    /// retention sweep and from `wait_for`'s terminal list, and would give the queue a second kind
    /// of liveness to reason about.
    ///
    /// **Why not `Failed`.** `failed` says the operation did not happen and that is the end of it,
    /// so the only actor left holding the conflict was whoever asked — which, for a landing, is the
    /// agent that had just finished its work and is precisely the actor this queue exists to spare
    /// from other sessions' integration. Telling it "failed" sends it looking for a conflict to fix,
    /// and the only way to find one is to manufacture it.
    ///
    /// **What is recorded is what makes the conflict reproducible, not the conflict itself.**
    /// Leaving the integration worktree conflicted was the obvious alternative and is worse: the
    /// next operation on that repository resets it, so the evidence would survive for however many
    /// seconds the queue happened to be idle. Git's own output plus the two commits it named can be
    /// replayed at any point instead, by anyone, in a worktree of their own.
    Escalated {
        reason: String,
        exit_code: Option<i32>,
        output_tail: String,
    },
    /// Computed, and it changes `nucleos.tests.yaml` to content the owner has not approved.
    ///
    /// **Not terminal, unlike every other variant.** The map decides which tests guard every
    /// merge, so a merge that changes it cannot be judged by the gate it is about to change
    /// (spec 2026-10-05 §3.4, defence 1). The row waits in `awaiting_owner`. The owner's approval
    /// puts it back to `queued` and records `map_blob`; the next execution passes only if the
    /// merge it computes carries exactly that blob, so a map edited after the approval waits
    /// again. Nothing is published and the computed merge is dropped, as for `Blocked`.
    AwaitingOwner { map_blob: String, reason: String },
    /// A push of the integration branch whose tip the post-land gate has not judged green. **Not
    /// terminal and not claimable**, like `AwaitingOwner`, but the one waited on is the daemon and not
    /// a person: when the gate turns a sha green, `release_awaiting_gate` puts the row back to
    /// `queued` and the push re-resolves the tip and decides afresh. Nothing was pushed. A red tip
    /// does not fail the push: `revert_on_red` moves the branch to a revert, which is gated, turns
    /// green, and is what is pushed in the end.
    ///
    /// **`sha` is the tip the decision was made on**, persisted in `output_tail` (never `result_sha`,
    /// which is a statement about what reached the remote and is read as such by tickets and the
    /// resolver; a parked row pushed nothing). The drain re-checks it against the gate's state right
    /// after the park, and the worker's sweep compares it to the state later, so a green written
    /// between the decision and the park is never lost.
    AwaitingGate { sha: String, reason: String },
    /// Never reached an argv — the row itself was unexecutable, which is a defect in the row and not
    /// a result of the operation.
    ///
    /// Recorded as `failed`, because there is no other honest status for it and adding one would
    /// mean a status-vocabulary change for a case that only a corrupt row can produce. It is told
    /// apart in the row **structurally**, not by reading the prose: `finish` sets the `unexecutable`
    /// column (migration 0208) for this variant and for no other, and a column, unlike
    /// `output_tail`, is not emptied by `prune_output_tails` after 30 days. The NULL tail this
    /// variant still writes is a side effect, no longer the discriminator.
    ///
    /// It does *not* mean "no subprocess ran". An operation can fail before reaching one — the
    /// integration worktree turning out not to be a worktree — and that writes an empty tail rather
    /// than NULL, because the row was executable and the environment was not. The two are different
    /// defects and belong to different people.
    Unexecutable { reason: String },
}
impl Outcome {
    /// The `status` column this outcome writes. `Unexecutable` shares `failed` with `Failed`; what
    /// tells them apart in the row is the `unexecutable` column (migration 0208), not this.
    ///
    /// One function rather than a string in each of `finish`'s match arms, because `drain_once` now
    /// needs the same answer for the feed: two places deciding what an outcome is called would
    /// drift, and the row and the feed disagreeing is exactly the contradiction the feed exists to
    /// avoid.
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Succeeded { .. } => "succeeded",
            Outcome::Blocked { .. } => "blocked",
            Outcome::Escalated { .. } => "escalated",
            Outcome::AwaitingOwner { .. } => "awaiting_owner",
            Outcome::AwaitingGate { .. } => "awaiting_gate",
            Outcome::Failed { .. } | Outcome::Unexecutable { .. } => "failed",
        }
    }
}
/// Takes the oldest claimable request for one repository and marks it `running`, or returns `None`.
///
/// `None` covers all three ordinary reasons there is nothing to do: nothing is queued, something is
/// already running for this repository, or the only rows are still `awaiting_approval` — a caller
/// waits the same way in each case, so they are not worth distinguishing.
///
/// One conditional `UPDATE`, never a `SELECT` then an `UPDATE`. The gap between those two
/// statements is exactly the race this module exists to remove: both callers would read the same
/// queued head and both would believe they own the repository. Here the winner is decided inside a
/// single statement — `NOT EXISTS` is the arbiter, so a losing caller updates zero rows and simply
/// waits rather than erroring on the unique index. That index is the backstop that makes a bug in
/// this guard impossible to ship silently, not the everyday mechanism.
///
/// `?2` appears twice but is bound once: SQLite numbers placeholder slots by their highest index,
/// not by how often each occurs, so this statement has two parameters and takes exactly two binds.
///
/// The claim, the parse and the release-on-failure are one transaction because they have to be
/// uncancellable together (`core/AGENTS.md` § "Cancellation safety", rule 3). A dropped future
/// stops at its last `.await` and never runs another line, and there are two suspension points
/// between marking a row `running` and deciding it is unexecutable — so a compensating write
/// written as a statement after those awaits is not cleanup, it is happy-path-only code. Inside a
/// transaction the question does not arise: `sqlx`'s `Transaction` rolls back when dropped, so a
/// claim abandoned at *any* await leaves the row exactly `queued`, untouched and claimable on the
/// next poll. It also means a row released this way goes `queued` → `failed` without ever being
/// observably `running`, so no queue view can show a phantom.
///
/// Wrapping the statement does not weaken the `NOT EXISTS` guard: SQLite admits one writer at a
/// time, so a second claimer's UPDATE evaluates the guard against the winner's committed row.
///
/// That holds because the claim is this transaction's **first** statement. `begin()` is deferred, so
/// no lock is taken until the UPDATE takes the write lock outright — there is no read-then-upgrade,
/// and so no `SQLITE_BUSY_SNAPSHOT`. Put a `SELECT` ahead of the claim in here and the argument
/// stops holding: the transaction becomes a reader that must upgrade, and an upgrade can fail
/// outright rather than losing cleanly. A loser that instead exhausts `busy_timeout`
/// (`storage.rs:69`) returns `Err(SQLITE_BUSY)` rather than `Ok(None)` — safe, since it claims
/// nothing, but it is a return shape the pre-transaction code could not produce, and the critical
/// section it waits on is one `serde_json::from_str` plus a commit.
pub async fn claim_next(
    pool: &sqlx::SqlitePool,
    repo_key: &str,
) -> sqlx::Result<Option<ClaimedRequest>> {
    let started_at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    // The tuple is the `RETURNING` clause's own shape and lives exactly as long as the next few
    // lines, which turn it into `ClaimedRequest`. Naming a struct for it would put the column order
    // in two places and invite them to drift; carrying `run_id` is what pushed it past the lint.
    #[allow(clippy::type_complexity)]
    let claimed: Option<(
        i64,
        String,
        String,
        String,
        String,
        bool,
        Option<i64>,
        Option<String>,
    )> = sqlx::query_as(
        "UPDATE vcs_requests
            SET status = 'running', started_at = ?1
          WHERE id = (
              SELECT id FROM vcs_requests
               WHERE repo_key = ?2 AND status = 'queued'
               ORDER BY id LIMIT 1
          )
            AND NOT EXISTS (
              SELECT 1 FROM vcs_requests WHERE repo_key = ?2 AND status = 'running'
            )
         RETURNING id, op, args, project_id, project_root, from_resolution, run_id,
                   approved_map_blob",
    )
    .bind(started_at)
    .bind(repo_key)
    .fetch_optional(&mut *transaction)
    .await?;

    let Some((id, op, args, project_id, project_root, from_resolution, run_id, approved_map_blob)) =
        claimed
    else {
        // Nothing was changed, so the rollback this drop performs is the same as a commit.
        return Ok(None);
    };
    // A row whose stored operation will not parse is an error, never `Ok(None)`: `None` means "come
    // back later", and no amount of waiting makes an unexecutable row executable. Left claimed, it
    // would hold this repository's only slot until the next daemon restart. That is the strong
    // form of the jam `core/AGENTS.md` § "Cancellation safety" describes — and this queue still HAS
    // the strong form, where runs and jobs traded it for numbered slots in migration 0053: a
    // repository has one slot and no ceiling anybody may raise. A queue that can trap the repository
    // it exists to protect is not doing its job.
    match Op::from_stored(&op, &args) {
        Ok(op) => {
            transaction.commit().await?;
            Ok(Some(ClaimedRequest {
                id,
                op,
                project_id,
                project_root,
                from_resolution,
                run_id,
                approved_map_blob,
                integration_branch: None,
            }))
        }
        Err(error) => {
            let reason =
                format!("stored operation for vcs request {id} could not be parsed: {error}");
            // Terminal rather than back to `queued`: re-queueing would hand the same unparseable
            // row out again on the next poll, forever. `Unexecutable` rather than `Failed` because
            // this row never reached an argv, and that is what sets the `unexecutable` column — the
            // structural marker `Outcome::Unexecutable`'s doc comment describes. This arm is its
            // only producer.
            let released = match finish(
                &mut *transaction,
                id,
                Outcome::Unexecutable {
                    reason: reason.clone(),
                },
            )
            .await
            {
                Ok(()) => transaction.commit().await,
                Err(error) => Err(error),
            };
            // A lost terminal write is not best-effort bookkeeping (`runs::warn_on_terminal_write_err`
            // makes the same argument), and the caller cannot infer it: it receives the *parse*
            // error and will reasonably read that as "handled, move on". The transaction keeps this
            // from jamming anything — the whole claim rolls back, so the row is `queued` rather
            // than stranded — but it does mean the next poll will hand out the same corrupt row
            // again, and a log line is the only thing that distinguishes that loop from silence.
            if let Err(error) = released {
                tracing::warn!(
                    vcs_request_id = id,
                    %error,
                    "could not record an unparseable vcs request as failed; it stays queued and will be claimed again"
                );
            }
            // The parse error is what propagates either way: it names the defect rather than its
            // symptom, and it is the one that stays true whether or not the release was recorded.
            Err(sqlx::Error::Protocol(reason))
        }
    }
}
/// Releases the repository by writing the claimed request's terminal status.
///
/// The columns an outcome does not carry are written NULL rather than left alone: one statement
/// covers every outcome, and NULL is already what those columns hold for a row that has only ever
/// been queued and claimed.
///
/// Scoped to `status = 'running'`, and a zero-row match is `RowNotFound` rather than a silent
/// `Ok(())` (the convention at `runs.rs:744`). Only the holder of a claim may end it: once Task 5's
/// restart reconciliation can mark a stranded row `interrupted` with the reason why, an unscoped
/// write would let a worker whose future outlived that reconcile flip `interrupted` to `succeeded`
/// and NULL the reason — destroying the only trace of the interruption, and reporting success for
/// work whose outcome nobody actually observed.
///
/// Generic over the executor so the claim can perform its own release inside the transaction that
/// makes the pair uncancellable; callers holding a pool pass `&pool` unchanged.
pub async fn finish<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    id: i64,
    outcome: Outcome,
) -> sqlx::Result<()> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    // Read before the match below consumes the outcome, and from `status()` rather than restated in
    // each arm — see its doc comment for why there is only one place that names a status.
    let status = outcome.status();
    let pending_map_blob = match &outcome {
        Outcome::AwaitingOwner { map_blob, .. } => Some(map_blob.clone()),
        _ => None,
    };
    // Structural, like `status`: read before the match below consumes the outcome. Only
    // `Unexecutable` marks the row (migration 0208); `Failed` and `Escalated` ran something.
    let unexecutable = matches!(&outcome, Outcome::Unexecutable { .. });
    let (result_sha, failure_reason, exit_code, output_tail) = match outcome {
        Outcome::Succeeded { sha, output_tail } => (sha, None, None, Some(output_tail)),
        Outcome::Blocked {
            reason,
            output_tail,
        } => (None, Some(reason), None, Some(output_tail)),
        // Written identically to `Failed`, and only the status differs. That is deliberate: an
        // escalation IS a failure of the operation, and the columns a reader queries — the reason,
        // git's own output, the exit code — are the same ones. What changes is who is expected to
        // do something about it, and that belongs in the status rather than in a second shape.
        Outcome::Failed {
            reason,
            exit_code,
            output_tail,
        }
        | Outcome::Escalated {
            reason,
            exit_code,
            output_tail,
        } => (None, Some(reason), exit_code, Some(output_tail)),
        Outcome::Unexecutable { reason } => (None, Some(reason), None, None),
        // Not terminal: `finished_at` stays NULL below, and no output was produced to keep.
        Outcome::AwaitingOwner { reason, .. } => (None, Some(reason), None, None),
        // The parked sha rides in `output_tail`, see `Outcome::AwaitingGate`. The column is not
        // pruned for a non-terminal row, and `result_sha` stays NULL: nothing was pushed.
        Outcome::AwaitingGate { sha, reason } => (None, Some(reason), None, Some(sha)),
    };
    let finished = sqlx::query(
        "UPDATE vcs_requests
            SET status = ?1,
                finished_at = CASE WHEN ?1 IN ('awaiting_owner', 'awaiting_gate') THEN NULL ELSE ?2 END,
                result_sha = ?3, failure_reason = ?4, exit_code = ?5, output_tail = ?6,
                pending_map_blob = COALESCE(?8, pending_map_blob),
                unexecutable = ?9
          WHERE id = ?7 AND status = 'running'",
    )
    .bind(status)
    .bind(finished_at)
    .bind(result_sha)
    .bind(failure_reason)
    .bind(exit_code)
    .bind(output_tail)
    .bind(id)
    .bind(pending_map_blob)
    .bind(unexecutable)
    .execute(executor)
    .await?;
    if finished.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}
