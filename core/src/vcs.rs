//! The shared-state git/`gh` queue: at most one operation per repository, ever.
//!
//! Two agents deciding to merge at the same moment is the problem this exists for. Git's index and
//! refs are shared state with no lock a second process can wait on politely, so the serialization
//! has to happen before anything reaches an argv: both requests are admitted, and they run one after
//! the other instead of colliding.
//!
//! Exclusivity is the database's job, not a mutex's. A partial unique index over `status = 'running'`
//! holds it, so it survives the daemon restart a mutex would not — and `reconcile_interrupted` is
//! what releases a slot that restart found still held.
//!
//! Requests are typed (`Op`), never command strings: parsing shell is the surface `classifier.rs`
//! exists to keep closed, so the daemon builds every argv itself. This module decides WHEN an
//! operation runs and records how it ended. It never decides whether the actor was allowed to ask
//! (`autopilot.rs`, `budget.rs`, `wip.rs`, `proposals.rs`), and never what a merge should contain.

// Chunk 1 lands the queue before Chunk 2 wires the worker loop and `GitExecutor`, so every item here
// has a test caller and no production one.
//
// **Chunk 2 must delete this attribute** in the same commit that adds the worker loop, then fix what
// the compiler reports rather than putting it back: anything still dead once a caller exists is dead
// for a reason worth reading. If one item genuinely has no caller yet, narrow it to an
// `#[allow(dead_code)]` on that item carrying the reason — do not keep the blanket.
//
// That instruction is the whole defence, because the descriptive version of this comment does not
// get removed. `attention.rs:12-13` is this same line, and its "part 2" shipped long ago
// (`scheduler.rs`, `repo_trigger.rs`, `job.rs` and `http.rs` all call `attention::` today) — the
// attribute is still there, silencing a module nobody means to silence any more. `http.rs:721` and
// `http.rs:4362` describe the same rot in `contacts.rs`.
#![cfg_attr(not(test), allow(dead_code))]

use serde::{Deserialize, Serialize};

/// What was asked for, as data.
///
/// Typed rather than a command string on purpose: a string would have to be parsed, and parsing
/// shell is the surface `classifier.rs` exists to keep closed. The daemon builds every argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Merge { source: String, target: String },
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Merge { .. } => "merge",
        }
    }

    pub fn to_args(&self) -> String {
        serde_json::to_string(self).expect("an Op is always serializable")
    }

    /// `kind` is the column, `args` the JSON payload. They are stored apart so the queue can be
    /// filtered by operation without parsing every row, which means they can also disagree — so
    /// the parse is checked against the column rather than trusted.
    pub fn from_stored(kind: &str, args: &str) -> Result<Self, String> {
        let parsed: Self = serde_json::from_str(args).map_err(|error| error.to_string())?;
        if parsed.kind() != kind {
            return Err(format!(
                "stored op column {kind} disagrees with its payload"
            ));
        }
        Ok(parsed)
    }
}

/// Who is asking, which decides whether the request needs a human's sign-off before it may queue.
///
/// A human's order in an interactive session already is the approval — asking again two seconds
/// later is friction with no safety gain. An autonomous run or job's request is not: nothing else
/// in the system has consented to it yet, so it waits. The queue itself never decides consent, only
/// ordering and mutual exclusion — this is where consent, already decided elsewhere, is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Human,
    Shell,
    Run(i64),
    Job(i64),
}

impl Origin {
    /// The exact spelling the `origin` column's CHECK constraint accepts — do not invent others.
    fn as_str(self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Shell => "shell",
            Origin::Run(_) => "run",
            Origin::Job(_) => "job",
        }
    }

    /// Human and shell requests carry their own approval; run and job requests are autonomous and
    /// have not been approved by anything yet.
    fn needs_approval(self) -> bool {
        matches!(self, Origin::Run(_) | Origin::Job(_))
    }

    /// `run_id` is populated only for `Origin::Run`. A job id written into a column named
    /// `run_id` would silently mislabel it as a run — job ids and run ids come from different
    /// sequences and would collide (see `worktree::Owner::feed_run_id`'s doc comment for the same
    /// mistake made once already). A `job_id` column arrives once jobs actually submit requests,
    /// which is not this chunk.
    fn run_id(self) -> Option<i64> {
        match self {
            Origin::Run(id) => Some(id),
            _ => None,
        }
    }
}

/// What a caller asks the queue to do, before provenance decides whether it may queue yet.
#[derive(Debug, Clone)]
pub struct SubmitRequest {
    pub op: Op,
    pub project_id: String,
    pub project_root: String,
    pub origin: Origin,
}

/// Admits a request into the queue and returns its row id. Provenance alone decides the initial
/// status: `Human`/`Shell` already carry their approval and start `queued`; `Run`/`Job` are
/// autonomous and start `awaiting_approval`. The transition out of `awaiting_approval` — approved
/// into `queued`, or `rejected` — belongs to Chunk 4 alongside the `proposals.rs` wiring that
/// grants it; this function only ever writes the initial state.
pub async fn submit(pool: &sqlx::SqlitePool, request: &SubmitRequest) -> sqlx::Result<i64> {
    let status = if request.origin.needs_approval() {
        "awaiting_approval"
    } else {
        "queued"
    };
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, run_id, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(request.op.kind())
    .bind(request.op.to_args())
    .bind(&request.project_id)
    .bind(&request.project_root)
    .bind(request.origin.as_str())
    .bind(request.origin.run_id())
    .bind(status)
    .bind(created_at)
    .execute(pool)
    .await?;
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
}

/// How a claimed request ended.
///
/// There is deliberately no `blocked` variant yet: the `status` CHECK already accepts the string,
/// but nothing in this chunk can produce that state — it becomes reachable only once publishing
/// exists — and a variant nothing constructs is dead weight the compiler is right to complain
/// about. The schema is already ready for it.
#[derive(Debug, Clone)]
pub enum Outcome {
    Succeeded {
        sha: String,
    },
    Failed {
        reason: String,
        exit_code: Option<i32>,
        output_tail: String,
    },
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
    project_id: &str,
) -> sqlx::Result<Option<ClaimedRequest>> {
    let started_at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let claimed: Option<(i64, String, String, String, String)> = sqlx::query_as(
        "UPDATE vcs_requests
            SET status = 'running', started_at = ?1
          WHERE id = (
              SELECT id FROM vcs_requests
               WHERE project_id = ?2 AND status = 'queued'
               ORDER BY id LIMIT 1
          )
            AND NOT EXISTS (
              SELECT 1 FROM vcs_requests WHERE project_id = ?2 AND status = 'running'
            )
         RETURNING id, op, args, project_id, project_root",
    )
    .bind(started_at)
    .bind(project_id)
    .fetch_optional(&mut *transaction)
    .await?;

    let Some((id, op, args, project_id, project_root)) = claimed else {
        // Nothing was changed, so the rollback this drop performs is the same as a commit.
        return Ok(None);
    };
    // A row whose stored operation will not parse is an error, never `Ok(None)`: `None` means "come
    // back later", and no amount of waiting makes an unexecutable row executable. Left claimed, it
    // would hold this repository's only slot until the next daemon restart — the jam
    // `core/AGENTS.md` § "Cancellation safety" describes for `one_open_worktree_run_per_project`,
    // where a run stranded at `running` "blocks *every* later worktree run for that project". A
    // queue that can trap the repository it exists to protect is not doing its job.
    match Op::from_stored(&op, &args) {
        Ok(op) => {
            transaction.commit().await?;
            Ok(Some(ClaimedRequest {
                id,
                op,
                project_id,
                project_root,
            }))
        }
        Err(error) => {
            let reason =
                format!("stored operation for vcs request {id} could not be parsed: {error}");
            // Terminal rather than back to `queued`: re-queueing would hand the same unparseable
            // row out again on the next poll, forever. `exit_code` is `None` and `output_tail`
            // empty because nothing ran — this row never reached an argv.
            let released = match finish(
                &mut *transaction,
                id,
                Outcome::Failed {
                    reason: reason.clone(),
                    exit_code: None,
                    output_tail: String::new(),
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
/// covers both outcomes, and NULL is already what those columns hold for a row that has only ever
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
    let (status, result_sha, failure_reason, exit_code, output_tail) = match outcome {
        Outcome::Succeeded { sha } => ("succeeded", Some(sha), None, None, None),
        Outcome::Failed {
            reason,
            exit_code,
            output_tail,
        } => ("failed", None, Some(reason), exit_code, Some(output_tail)),
    };
    let finished = sqlx::query(
        "UPDATE vcs_requests
            SET status = ?, finished_at = ?, result_sha = ?, failure_reason = ?,
                exit_code = ?, output_tail = ?
          WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(finished_at)
    .bind(result_sha)
    .bind(failure_reason)
    .bind(exit_code)
    .bind(output_tail)
    .bind(id)
    .execute(executor)
    .await?;
    if finished.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

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
        )
        .await;
    }
    Ok(reconciled.len() as u64)
}

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
/// slot until the next startup's `reconcile_interrupted` releases it — the same jam AGENTS.md
/// describes for `one_open_worktree_run_per_project`. What keeps it acceptable is who calls this:
/// Chunk 2's caller is a background loop owned by `main.rs`, whose future is dropped only when the
/// daemon exits, which is precisely the case `reconcile_interrupted` exists for.
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
    project_id: &str,
    executor: &dyn VcsExecutor,
) -> bool {
    let claimed = match claim_next(pool, project_id).await {
        Ok(Some(claimed)) => claimed,
        // Nothing queued, something already running, or only unapproved rows — all "come back
        // later", and the caller waits the same way for each.
        Ok(None) => return false,
        Err(error) => {
            tracing::warn!(
                project_id = %project_id,
                %error,
                "could not claim the next vcs request"
            );
            return false;
        }
    };
    let id = claimed.id;
    let outcome = executor.execute(&claimed).await;
    // Cloned rather than moved so the failure paths below can still name it. `finish` consumes the
    // outcome, and a refused write would otherwise drop the only copy of a sha that git really
    // produced — leaving a commit the daemon caused recorded nowhere in the system at all.
    if let Err(error) = finish(pool, id, outcome.clone()).await {
        match error {
            // Not a lost write: `finish` is scoped to `status = 'running'`, so this is the row being
            // taken out from under the operation — a restart's `reconcile_interrupted` already marked
            // it `interrupted`, the collision `a_reconciled_request_cannot_be_finished_by_a_late_worker`
            // covers. The repository is NOT jammed; the row is terminal and the queue moves on. What
            // is lost is the outcome, which the row is now refusing, so this log line is the only
            // place it survives.
            sqlx::Error::RowNotFound => tracing::warn!(
                vcs_request_id = id,
                project_id = %project_id,
                ?outcome,
                "a vcs request stopped running before its outcome arrived; the row refused it, so it is recorded here"
            ),
            // Anything else is the write itself failing, and that one does jam. This is the write
            // that releases the repository: without it the row stays `running` and every later
            // request for this repository waits behind it until a restart reconciles, so the log
            // line is the only account of why the queue stopped.
            error => tracing::error!(
                vcs_request_id = id,
                project_id = %project_id,
                ?outcome,
                %error,
                "could not record how a vcs request ended; it stays running until the daemon restarts"
            ),
        }
    }
    true
}

/// The test double for `VcsExecutor`. `#[cfg(test)]` because every user of it is a test — building it
/// into the daemon would ship an executor that can report a merge it never performed.
#[cfg(test)]
struct FakeVcsExecutor {
    outcome: Outcome,
    /// How long to take before answering. A real merge takes seconds, and a test about what happens
    /// WHILE one runs — a drain abandoned mid-operation — needs a window to abandon it in. The same
    /// reason `FakeTranscriber` carries one.
    // Qualified rather than imported: the import would be unused in the non-test build.
    delay: std::time::Duration,
    /// Every request this was handed, in the order it was handed them.
    ///
    /// Recorded rather than counted, because a fake that ignores its argument answers identically
    /// whether the claim gave it the right row or another repository's — and the order is what makes
    /// the drain's FIFO promise checkable at all. Whole `ClaimedRequest`s rather than a tuple of
    /// fields: two adjacent `String`s destructured positionally can be swapped with every assertion
    /// still passing, which is the hazard `claim_next`'s own 5-tuple carries a warning about.
    seen: std::sync::Mutex<Vec<ClaimedRequest>>,
}

#[cfg(test)]
impl FakeVcsExecutor {
    fn succeeding_with(sha: &str) -> Self {
        Self::reporting(Outcome::Succeeded { sha: sha.into() })
    }

    /// Answers, but not immediately.
    fn succeeding_slowly(sha: &str, delay: std::time::Duration) -> Self {
        Self {
            delay,
            ..Self::succeeding_with(sha)
        }
    }

    /// `reason` and `output_tail` are deliberately different strings, so a test using this fake
    /// cannot be blind to those two columns being swapped —
    /// `a_failed_request_records_why_and_what_it_printed` uses distinct ones for the same reason.
    ///
    /// Neither is asserted *through* the drain. What that leaves untested is not whether `finish`
    /// writes the columns, which has direct coverage, but whether `drain_once` forwards the
    /// executor's outcome **whole** rather than rebuilding one of its own — and the drain test's
    /// `result_sha` assertion stands for that, one field deep.
    fn failing_with(reason: &str) -> Self {
        Self::reporting(Outcome::Failed {
            reason: reason.into(),
            exit_code: Some(1),
            output_tail: format!("git printed this while failing: {reason}"),
        })
    }

    fn reporting(outcome: Outcome) -> Self {
        Self {
            outcome,
            delay: std::time::Duration::ZERO,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Derived from `seen` rather than kept beside it: a separate counter can drift from the list it
    /// is supposed to describe, and then nothing says which of the two is right.
    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn seen(&self) -> Vec<ClaimedRequest> {
        self.seen.lock().unwrap().clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl VcsExecutor for FakeVcsExecutor {
    async fn execute(&self, request: &ClaimedRequest) -> Outcome {
        // Recorded before the delay, not after: a drain abandoned mid-operation never reaches the
        // line after the await, and a test of that case still needs to see the executor was entered.
        // The guard is a temporary so it is dropped at the end of this statement — held across the
        // await it would make this future non-`Send`, which `async_trait` requires.
        self.seen.lock().unwrap().push(request.clone());
        tokio::time::sleep(self.delay).await;
        self.outcome.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::time::Duration;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn request(origin: Origin) -> SubmitRequest {
        request_for("alpha", origin)
    }

    fn request_for(project: &str, origin: Origin) -> SubmitRequest {
        SubmitRequest {
            op: Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            project_id: project.into(),
            project_root: "C:/repo".into(),
            origin,
        }
    }

    async fn status_of(pool: &sqlx::SqlitePool, id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// `expect` rather than defaulting a NULL to `""`: a regression that wrote no reason at all is
    /// exactly what this is for, and collapsing it into an empty string turns that into a bare
    /// `assertion failed` at the call site with no value to read.
    async fn failure_reason_of(pool: &sqlx::SqlitePool, id: i64) -> String {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT failure_reason FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
        .expect("a failed request records why it failed")
    }

    async fn run_id_of(pool: &sqlx::SqlitePool, id: i64) -> Option<i64> {
        sqlx::query_scalar("SELECT run_id FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A payload `Op::from_stored` accepts, for rows written straight to the table.
    const MERGE_ARGS: &str = r#"{"op":"merge","source":"feat/x","target":"master"}"#;

    /// The one place that knows the column list. `args` is a parameter because the rows worth
    /// writing by hand are exactly the ones `submit` cannot produce — a payload that will not
    /// parse, or a status no caller can reach yet.
    async fn insert(
        pool: &sqlx::SqlitePool,
        project: &str,
        status: &str,
        args: &str,
    ) -> sqlx::Result<i64> {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at)
             VALUES ('merge', ?, ?, 'C:/repo', 'human', ?, '2026-08-02T00:00:00Z')",
        )
        .bind(args)
        .bind(project)
        .bind(status)
        .execute(pool)
        .await
        .map(|inserted| inserted.last_insert_rowid())
    }

    /// Exclusivity is the database's job, not a Mutex's: a Mutex does not survive a daemon restart
    /// and this index does. Asserted sequentially on purpose — the constraint is what is under
    /// test, and the pool helper is `max_connections(1)`, so a "concurrent" version would prove
    /// less and flake more.
    #[tokio::test]
    async fn only_one_request_may_run_per_repository() {
        let pool = test_pool().await;

        insert(&pool, "alpha", "running", MERGE_ARGS)
            .await
            .expect("the first running request is allowed");

        let second = insert(&pool, "alpha", "running", MERGE_ARGS).await;
        assert!(
            second.is_err(),
            "a second running request for the same repository must be rejected"
        );

        insert(&pool, "beta", "running", MERGE_ARGS)
            .await
            .expect("a different repository is not blocked by alpha's running request");

        for _ in 0..3 {
            insert(&pool, "alpha", "queued", MERGE_ARGS)
                .await
                .expect("queued requests are not limited — only running is");
        }
    }

    /// Round-tripping through the stored form is the point: the row is the contract between the
    /// submitting process and the worker, which may be a daemon restart apart.
    #[test]
    fn an_operation_round_trips_through_its_stored_form() {
        let op = Op::Merge {
            source: "feat/x".into(),
            target: "master".into(),
        };
        let back =
            Op::from_stored(op.kind(), &op.to_args()).expect("a stored operation must parse back");
        assert_eq!(back, op);
    }

    #[test]
    fn an_unknown_operation_is_refused_rather_than_guessed() {
        assert!(Op::from_stored("rm_rf", "{}").is_err());
    }

    /// The column and the payload can disagree — a row edited by hand, or a bug that wrote one
    /// without the other. Trusting the payload would let a `merge` row execute as something else the
    /// moment a second variant exists.
    ///
    /// NOTE: with a single variant this refusal comes from serde's unknown-tag error, not from the
    /// `kind` comparison — every payload that parses at all is a `Merge`, so that branch is
    /// unreachable by construction today. The guard is written now because the moment Chunk 4 adds
    /// `Push` it stops being unreachable and starts being the thing that prevents a merge row from
    /// executing as a push. **Chunk 4 must add the case that actually covers it:**
    /// `Op::from_stored("push", <a merge payload>)`.
    #[test]
    fn a_payload_that_contradicts_its_column_is_refused() {
        assert!(Op::from_stored("merge", r#"{"op":"rm_rf"}"#).is_err());
    }

    /// A human's order in an interactive session already is the approval — asking again two
    /// seconds later is friction with no safety gain.
    #[tokio::test]
    async fn a_human_request_needs_no_second_approval() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();
        assert_eq!(status_of(&pool, id).await, "queued");
    }

    /// The shell speaks for the human sitting in front of it, so its requests queue on the same
    /// terms rather than asking a second time.
    ///
    /// This is also the only thing that constructs `Origin::Shell` at all, and the `origin` column
    /// is CHECK-constrained: an `as_str` that spelled this variant any other way would fail every
    /// real shell submit at runtime, and nothing else here would notice. Reading the column back is
    /// the half that proves it — the status assertion alone passes for any accepted spelling.
    #[tokio::test]
    async fn a_shell_request_carries_the_same_approval_a_human_s_does() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Shell)).await.unwrap();

        assert_eq!(status_of(&pool, id).await, "queued");
        let origin: String = sqlx::query_scalar("SELECT origin FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(origin, "shell");
    }

    /// An autonomous run's request is not a human's order; nothing has consented to it yet, so it
    /// must wait for a human before it can queue.
    #[tokio::test]
    async fn an_autonomous_request_waits_for_approval_before_it_can_queue() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Run(7))).await.unwrap();
        assert_eq!(status_of(&pool, id).await, "awaiting_approval");
    }

    /// A job's id must not land in a column named `run_id`.
    ///
    /// The two ids come from different sequences, so a job written there reads as a run that
    /// happens to share its number — wrong in the way that looks right. Neither admission test
    /// above would notice: both assert only on `status`, so binding NULL always, or binding the
    /// job id too, passes them. This test is the only thing holding that decision in place.
    #[tokio::test]
    async fn only_a_run_puts_its_id_in_run_id() {
        let pool = test_pool().await;

        let from_run = submit(&pool, &request(Origin::Run(7))).await.unwrap();
        let from_job = submit(&pool, &request_for("beta", Origin::Job(7)))
            .await
            .unwrap();

        assert_eq!(run_id_of(&pool, from_run).await, Some(7));
        assert_eq!(run_id_of(&pool, from_job).await, None);
    }

    #[tokio::test]
    async fn the_queue_is_served_in_arrival_order() {
        let pool = test_pool().await;
        let first = submit(&pool, &request(Origin::Human)).await.unwrap();
        let second = submit(&pool, &request(Origin::Human)).await.unwrap();

        assert_eq!(claim_next(&pool, "alpha").await.unwrap().unwrap().id, first);
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_none(),
            "the second request must wait: alpha already has one running"
        );

        finish(
            &pool,
            first,
            Outcome::Succeeded {
                sha: "abc123".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            claim_next(&pool, "alpha").await.unwrap().unwrap().id,
            second
        );
    }

    /// Serializing repositories that cannot touch each other would make this a bottleneck rather than
    /// a brake.
    #[tokio::test]
    async fn separate_repositories_do_not_wait_on_each_other() {
        let pool = test_pool().await;
        submit(&pool, &request_for("alpha", Origin::Human))
            .await
            .unwrap();
        submit(&pool, &request_for("beta", Origin::Human))
            .await
            .unwrap();

        assert!(claim_next(&pool, "alpha").await.unwrap().is_some());
        assert!(claim_next(&pool, "beta").await.unwrap().is_some());
    }

    /// A row nobody can execute must not take the repository down with it.
    ///
    /// The claim commits before the payload is parsed, so the obvious failure path — return the
    /// error — leaves the row `running` and holds alpha's only slot until the daemon restarts. The
    /// status assertion alone would not catch that: what proves the repository was actually freed
    /// is that the *next* claim returns the following request instead of `None`.
    #[tokio::test]
    async fn a_row_that_cannot_be_parsed_frees_the_repository_instead_of_jamming_it() {
        let pool = test_pool().await;
        // Written directly: `submit` cannot produce this row, which is the point — it comes from a
        // hand edit, or a downgrade that no longer knows an operation a newer build wrote.
        let corrupt = insert(&pool, "alpha", "queued", r#"{"op":"rm_rf"}"#)
            .await
            .unwrap();
        let behind_it = submit(&pool, &request(Origin::Human)).await.unwrap();

        assert!(
            claim_next(&pool, "alpha").await.is_err(),
            "an unexecutable row is an error, not a wait"
        );
        assert_eq!(status_of(&pool, corrupt).await, "failed");
        // The diagnosis has to survive into the row, or the only account of why this request died
        // is a log line the daemon may have already rotated away.
        let reason: Option<String> =
            sqlx::query_scalar("SELECT failure_reason FROM vcs_requests WHERE id = ?")
                .bind(corrupt)
                .fetch_one(&pool)
                .await
                .unwrap();
        let reason = reason.expect("a failed request records why it failed");
        assert!(
            reason.contains(&corrupt.to_string()) && reason.contains("could not be parsed"),
            "the recorded reason must name the row and say what was wrong: {reason}"
        );
        assert_eq!(
            claim_next(&pool, "alpha").await.unwrap().unwrap().id,
            behind_it,
            "the queue must move on, not hold alpha until the daemon restarts"
        );
    }

    /// A positional 5-tuple of `(i64, String, String, String, String)` is destructured by position,
    /// so `project_id` and `project_root` — adjacent in both the `RETURNING` list and the pattern —
    /// could be swapped and everything else here would still pass. This is also the only coverage
    /// that `Op` parsing works *through* the claim rather than in isolation.
    #[tokio::test]
    async fn a_claim_carries_the_operation_and_the_repository_it_names() {
        let pool = test_pool().await;
        submit(&pool, &request(Origin::Human)).await.unwrap();

        let claimed = claim_next(&pool, "alpha").await.unwrap().unwrap();
        assert_eq!(
            claimed.op,
            Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            }
        );
        assert_eq!(claimed.project_id, "alpha");
        assert_eq!(claimed.project_root, "C:/repo");
    }

    /// Any non-`running` status frees the partial index, so the ordering test would pass even if
    /// `finish` wrote the wrong status and dropped the sha entirely. What the caller keeps of a
    /// merge is the commit it produced; nothing else asserts it lands.
    #[tokio::test]
    async fn a_succeeded_request_records_the_commit_it_produced() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();
        claim_next(&pool, "alpha").await.unwrap().unwrap();

        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: "abc123".into(),
            },
        )
        .await
        .unwrap();

        let (status, sha, exit_code, output_tail, reason): (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT status, result_sha, exit_code, output_tail, failure_reason
               FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(status, "succeeded");
        assert_eq!(sha.as_deref(), Some("abc123"));
        assert_eq!(exit_code, None, "nothing failed, so there is no exit code");
        assert_eq!(output_tail, None);
        assert_eq!(reason, None);
    }

    #[tokio::test]
    async fn a_failed_request_records_why_and_what_it_printed() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();
        claim_next(&pool, "alpha").await.unwrap().unwrap();

        finish(
            &pool,
            id,
            Outcome::Failed {
                reason: "merge conflict".into(),
                exit_code: Some(1),
                output_tail: "CONFLICT (content): Merge conflict in a.txt".into(),
            },
        )
        .await
        .unwrap();

        let (status, sha, exit_code, output_tail, reason): (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT status, result_sha, exit_code, output_tail, failure_reason
               FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(status, "failed");
        assert_eq!(sha, None, "nothing succeeded, so there is no commit");
        assert_eq!(exit_code, Some(1));
        assert_eq!(
            output_tail.as_deref(),
            Some("CONFLICT (content): Merge conflict in a.txt")
        );
        assert_eq!(reason.as_deref(), Some("merge conflict"));
    }

    /// Only the holder of a claim may end it.
    ///
    /// Task 5's restart reconciliation marks stranded rows `interrupted` and records why. A worker
    /// whose future outlived that reconcile would otherwise flip the row to `succeeded` and NULL
    /// the reason — reporting success for work nobody observed finish, and destroying the only
    /// record that it was ever interrupted.
    #[tokio::test]
    async fn a_request_that_is_no_longer_running_cannot_be_finished() {
        let pool = test_pool().await;
        let id = insert(&pool, "alpha", "interrupted", MERGE_ARGS)
            .await
            .unwrap();

        let late = finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: "abc123".into(),
            },
        )
        .await;

        assert!(matches!(late, Err(sqlx::Error::RowNotFound)));
        assert_eq!(status_of(&pool, id).await, "interrupted");
    }

    // NOTE: the rollback-on-drop half of the claim's cancellation safety is deliberately not tested
    // here, and the reason is narrower than "we lack a harness" — the crate has one. `TempDb`
    // (`storage.rs`, `#[cfg(test)]`) is file-backed with `max_connections(5)`, and its own doc
    // comment advertises this very shape: "a handler parked on a pool while another connection
    // watches it". What is actually missing is a way to stop a future at a *chosen* await:
    // `Waker::noop()` does not give that deterministically, and against `test_pool`'s `:memory:`
    // single connection every attempt ends in `PoolTimedOut` after 30s, because an abandoned claim
    // never returns the one connection there is. So the test would be asserting `sqlx`'s documented
    // guarantee rather than this module's logic: `sqlx-core-0.9.0/src/transaction.rs:265-280`,
    // `impl Drop for Transaction` calls `start_rollback`, which runs "on the next asynchronous
    // invocation of the underlying connection (including if the connection is returned to a pool)".

    /// The queue must never hand out work that cannot execute — a head blocked on a sleeping human
    /// blocks every agent behind it. That is the whole reason approval precedes admission.
    #[tokio::test]
    async fn nothing_awaiting_approval_is_ever_claimable() {
        let pool = test_pool().await;
        submit(&pool, &request(Origin::Run(7))).await.unwrap();
        assert!(claim_next(&pool, "alpha").await.unwrap().is_none());
    }

    /// A `running` row at startup means the daemon died mid-operation, and nothing can say whether git
    /// finished. Auto-retry is not an option: a re-run `merge` is harmless, a re-run `tag` is not, and
    /// telling them apart from a cold start is guessing. It is recorded and left for a human — the
    /// same call `runs::reconcile_orphaned_runs` makes.
    #[tokio::test]
    async fn a_request_running_at_startup_is_marked_interrupted_not_retried() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();
        claim_next(&pool, "alpha").await.unwrap();

        let reconciled = reconcile_interrupted(&pool).await.unwrap();

        assert_eq!(status_of(&pool, id).await, "interrupted");
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_none(),
            "an interrupted request must not re-enter the queue by itself"
        );
        // The status and the "does not re-enter the queue" assertions above would both still pass
        // if `reconcile_interrupted` matched every row but wrote `failure_reason` and `finished_at`
        // as NULL, or if it reported the wrong row count to its caller (Task 8 relies on the count
        // to decide whether to log anything). `failure_reason` in particular is the only thing that
        // will ever tell a human this was a restart rather than an ordinary failure.
        assert_eq!(reconciled, 1);
        let (failure_reason, finished_at): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT failure_reason, finished_at FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            failure_reason.as_deref(),
            Some("daemon restarted mid-operation")
        );
        assert!(
            finished_at.is_some(),
            "an interrupted request is terminal and must record when"
        );
    }

    /// The reconcile and a late `finish` have to compose, not merely each be correct.
    ///
    /// `finish`'s `AND status = 'running'` guard was written for this exact collision — a worker
    /// whose future outlived the reconcile — but until now nothing put the two real functions in
    /// sequence: the guard's own test reaches `interrupted` by writing that status by hand. So the
    /// claim held by inspection of two SQL statements and by nothing else, which is how a guard
    /// gets dropped in a refactor that only reads one of them.
    ///
    /// What is actually protected is the audit record: if the late write landed, a row a restart
    /// interrupted would read `succeeded`, and the reason it says so would be gone.
    #[tokio::test]
    async fn a_reconciled_request_cannot_be_finished_by_a_late_worker() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        reconcile_interrupted(&pool).await.unwrap();

        let late = finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: "abc123".into(),
            },
        )
        .await;

        assert!(
            matches!(late, Err(sqlx::Error::RowNotFound)),
            "a finish arriving after the reconcile must be refused, not silently applied"
        );
        assert_eq!(status_of(&pool, id).await, "interrupted");
        let (reason, sha): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT failure_reason, result_sha FROM vcs_requests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("daemon restarted mid-operation"));
        assert_eq!(
            sha, None,
            "the late write must not have left its sha behind"
        );
    }

    #[tokio::test]
    async fn the_queue_is_drained_in_order_and_each_outcome_recorded() {
        let pool = test_pool().await;
        let first = submit(&pool, &request(Origin::Human)).await.unwrap();
        let second = submit(&pool, &request(Origin::Human)).await.unwrap();

        let executor = FakeVcsExecutor::succeeding_with("abc123");
        drain_once(&pool, "alpha", &executor).await;
        drain_once(&pool, "alpha", &executor).await;

        assert_eq!(status_of(&pool, first).await, "succeeded");
        assert_eq!(status_of(&pool, second).await, "succeeded");
        assert_eq!(executor.calls(), 2);

        // What the executor was handed, not just how often. An executor that ignores its argument
        // reports the same two calls whether the claim gave it the right row or another
        // repository's, so counting alone cannot catch a `claim_next` that returns the wrong one.
        let seen = executor.seen();
        // The order half of this test's name. Both rows end `succeeded` and every per-call
        // assertion below is byte-identical for the two, so without the ids a `claim_next` serving
        // newest-first would pass here unchanged.
        assert_eq!(
            seen.iter().map(|claimed| claimed.id).collect::<Vec<_>>(),
            vec![first, second],
            "the drain must serve the queue in arrival order"
        );
        for claimed in seen {
            assert_eq!(
                claimed.op,
                Op::Merge {
                    source: "feat/x".into(),
                    target: "master".into(),
                }
            );
            assert_eq!(claimed.project_id, "alpha");
            assert_eq!(claimed.project_root, "C:/repo");
        }

        // The outcome has to travel from the executor into the row. `drain_once` is the only thing
        // that carries it there, and the status assertions above pass just as well if it drops the
        // sha and writes a canned success of its own.
        let sha: Option<String> =
            sqlx::query_scalar("SELECT result_sha FROM vcs_requests WHERE id = ?")
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sha.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn a_failing_operation_is_recorded_and_frees_the_repository() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();

        drain_once(
            &pool,
            "alpha",
            &FakeVcsExecutor::failing_with("CONFLICT (content)"),
        )
        .await;

        assert_eq!(status_of(&pool, id).await, "failed");
        let reason = failure_reason_of(&pool, id).await;
        assert!(
            reason.contains("CONFLICT"),
            "the recorded reason must be the executor's own: {reason}"
        );

        // The point of this half: a failure must not leave the repository claimed forever.
        submit(&pool, &request(Origin::Human)).await.unwrap();
        assert!(claim_next(&pool, "alpha").await.unwrap().is_some());
    }

    /// The return value is the only thing Chunk 2's loop can terminate on, and both tests above
    /// discard it — so `true` unconditionally and `false` unconditionally each pass them.
    ///
    /// Neither is harmless. `true` always spins a `while drain_once(..).await {}` at 100% CPU;
    /// `false` always drains one request per poll interval forever, which looks like a slow queue
    /// rather than a bug. `#[must_use]` cannot stand in for this: on an `async fn` it marks the
    /// future, which every caller already awaits.
    #[tokio::test]
    async fn a_drain_says_whether_it_found_anything_to_do() {
        let pool = test_pool().await;
        submit(&pool, &request(Origin::Human)).await.unwrap();

        let busy = FakeVcsExecutor::succeeding_with("abc123");
        assert!(
            drain_once(&pool, "alpha", &busy).await,
            "a drain that claimed and executed a request has done something"
        );

        // A second executor, so the count below is exact rather than merely unchanged.
        let idle = FakeVcsExecutor::succeeding_with("def456");
        assert!(
            !drain_once(&pool, "alpha", &idle).await,
            "there is nothing left to claim, so the caller should wait rather than drain again"
        );
        assert_eq!(
            idle.calls(),
            0,
            "an idle drain must not reach the executor at all"
        );
    }

    /// A restart's reconcile landing while the operation is still running — the collision that makes
    /// `finish`'s refusal a real path rather than a defensive one, seen from the drain's side.
    ///
    /// `a_reconciled_request_cannot_be_finished_by_a_late_worker` covers `finish` refusing directly.
    /// What only this can show is what `drain_once` does with the refusal: it must not treat a
    /// terminal row as a jam, must not lose that the work happened, and must leave the interrupted
    /// row exactly as the reconcile wrote it. The sha the executor produced survives only in a log
    /// line from here — the row is entitled to refuse it, and does.
    ///
    /// The reconcile runs *during* the operation because that is when it really happens, and it can:
    /// the drain holds no pooled connection while it awaits the executor, so a single-connection
    /// pool still answers.
    #[tokio::test]
    async fn a_drain_whose_row_was_reconciled_out_from_under_it_does_not_overwrite_the_record() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();

        // A 10x margin over the reconcile's own wait, so which lands first is not a race: an
        // in-memory claim takes microseconds, and `reconciled == 1` below fails loudly rather than
        // passing quietly if that ever stops being true.
        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_millis(100));
        let (drained, reconciled) = tokio::join!(drain_once(&pool, "alpha", &executor), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            reconcile_interrupted(&pool).await.unwrap()
        });

        assert_eq!(
            reconciled, 1,
            "the reconcile must have caught the row mid-operation"
        );
        assert_eq!(executor.calls(), 1);
        assert!(
            drained,
            "the merge ran; a refused terminal write must not be reported as an idle tick"
        );

        let (status, sha, reason): (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, result_sha, failure_reason FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "interrupted");
        assert_eq!(
            sha, None,
            "the late write must not have left its sha behind"
        );
        assert_eq!(
            reason.as_deref(),
            Some("daemon restarted mid-operation"),
            "the reconcile's account of why must survive the drain arriving after it"
        );

        // And the repository is free: a refused write means the row is terminal, so the queue moves
        // on rather than waiting behind it.
        submit(&pool, &request(Origin::Human)).await.unwrap();
        assert!(claim_next(&pool, "alpha").await.unwrap().is_some());
    }

    /// The jam window `drain_once`'s doc comment admits to, composed with the thing that closes it.
    ///
    /// Two functions have to agree for that claim to hold, and inspecting either alone does not show
    /// it — the same gap `a_reconciled_request_cannot_be_finished_by_a_late_worker` was written for.
    ///
    /// The NOTE above explains why the *claim's* rollback-on-drop cannot be tested here: an
    /// abandoned claim never returns the single connection a `:memory:` pool has, so every attempt
    /// ends in `PoolTimedOut`. That does not transfer to this case. `claim_next` commits and drops
    /// its `Transaction` before returning, so while the drain is awaiting the executor it holds no
    /// pooled connection at all — dropping it there leaves the pool free to answer the assertions.
    #[tokio::test]
    async fn a_drain_abandoned_mid_operation_jams_the_repository_until_a_restart_reconciles() {
        let pool = test_pool().await;
        let id = submit(&pool, &request(Origin::Human)).await.unwrap();

        // Far longer than the timeout, so which of the two fires is not a race.
        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_secs(30));
        let drain = drain_once(&pool, "alpha", &executor);
        tokio::time::timeout(Duration::from_millis(50), drain)
            .await
            .expect_err("the executor is still working, so the drain cannot have finished");

        assert_eq!(
            executor.calls(),
            1,
            "the drain was abandoned inside the operation, not before it"
        );
        // Dropped at the executor's await, so `finish` never ran. This is the documented cost, not a
        // defect: the row is stranded exactly as the doc comment says it is.
        assert_eq!(status_of(&pool, id).await, "running");
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_none(),
            "the stranded row holds this repository's only slot — nothing else may claim it"
        );

        // And the compensator is what releases it, which is the half that makes the window
        // acceptable rather than merely admitted.
        assert_eq!(reconcile_interrupted(&pool).await.unwrap(), 1);
        assert_eq!(status_of(&pool, id).await, "interrupted");
        submit(&pool, &request(Origin::Human)).await.unwrap();
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_some(),
            "once reconciled, the repository is free again"
        );
    }
}
