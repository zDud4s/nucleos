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

use serde::{Deserialize, Serialize};

/// What was asked for, as data.
///
/// Typed rather than a command string on purpose: a string would have to be parsed, and parsing
/// shell is the surface `classifier.rs` exists to keep closed. The daemon builds every argv.
///
/// **Whoever adds the next variant here owes two things that `Merge` did not.**
///
/// 1. `git_exec::run_git` justifies having no process-tree kill with "nothing here hands git a
///    shell". That is true of `merge`, and it stops being true the day `Fetch` or `Push` lands and
///    git starts spawning ssh and credential helpers — which is the exact case spec §7's hung-command
///    row was written about, a fetch against a dead network. That comment will become wrong without
///    anybody editing it, so the obligation is recorded here, where the change has to be made.
/// 2. `Merge`'s `source`/`target` reach argv without a `--end-of-options`, and get away with it by
///    accident rather than design: a dashed string can set an option but cannot also name a commit,
///    HEAD in the integration worktree is always detached so `merge`'s upstream fallback dies, and
///    `update-ref` rejects a dashed ref name. A variant with a different argv shape does not inherit
///    any of that.
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
    // Constructed by Chunk 3, when the MCP tools give an external session its own provenance.
    #[allow(dead_code)]
    Shell,
    Run(i64),
    // Constructed by Chunk 4, when jobs submit requests of their own.
    #[allow(dead_code)]
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

/// A project resolved to the repository it names, with the key the queue locks on.
///
/// Private fields with one production constructor, because the defect this type exists to kill was
/// two fields allowed to disagree: a caller that could set `key` and `root` independently could take
/// the lock on one repository and run git in another, and the row would look entirely ordinary.
#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    project_id: String,
    root: String,
    key: String,
}

impl ResolvedRepo {
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Tests build repositories that do not exist on disk: what most of them exercise is the SQL,
    /// and making each one create a real git repository would test git twice and slow the suite.
    /// `resolve_repo` is the only constructor compiled into the daemon.
    #[cfg(test)]
    pub fn synthetic(project_id: &str, root: &str, key: &str) -> Self {
        Self {
            project_id: project_id.to_owned(),
            root: root.to_owned(),
            key: key.to_owned(),
        }
    }
}

/// Why a project could not be resolved to a repository.
///
/// Two failure arms rather than one string because the HTTP layer answers them differently and a
/// caller deserves to know which happened: an unknown project is the caller naming something that is
/// not there, and a bad root is the daemon's own recorded state being wrong.
#[derive(Debug)]
pub enum ResolveError {
    UnknownProject,
    NotARepository(String),
    Database(sqlx::Error),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProject => {
                write!(formatter, "no such project, or it has no recorded root")
            }
            Self::NotARepository(reason) => write!(formatter, "{reason}"),
            Self::Database(error) => write!(formatter, "{error}"),
        }
    }
}

/// The single production path from a project id to a repository the queue may lock.
///
/// Both halves are needed: `autopilot_state` is the only place a root is recorded, and git is what
/// makes two projects sharing a repository share a lock. It runs a subprocess, so it is neither free
/// nor infallible — that is the trade against keying on a label, which is what it replaces.
pub async fn resolve_repo(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<ResolvedRepo, ResolveError> {
    let root = crate::inspect::project_root(pool, project_id)
        .await
        .map_err(ResolveError::Database)?
        .ok_or(ResolveError::UnknownProject)?;
    let deadline = std::time::Instant::now() + crate::git_exec::OPERATION_TIMEOUT;
    let key = crate::git_exec::repo_key(std::path::Path::new(&root), deadline)
        .await
        .map_err(ResolveError::NotARepository)?;

    Ok(ResolvedRepo {
        project_id: project_id.to_owned(),
        root,
        key,
    })
}

/// Admits a request into the queue and returns its row id. Provenance alone decides the initial
/// status: `Human`/`Shell` already carry their approval and start `queued`; `Run`/`Job` are
/// autonomous and start `awaiting_approval`. The transition out of `awaiting_approval` — approved
/// into `queued`, or `rejected` — belongs to Chunk 4 alongside the `proposals.rs` wiring that
/// grants it; this function only ever writes the initial state.
///
/// The repository arrives resolved rather than as fields to be trusted — see `ResolvedRepo`.
pub async fn submit(
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
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO vcs_requests (op, args, project_id, project_root, repo_key, origin, run_id, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Ran, and did what was asked.
    Succeeded { sha: String, output_tail: String },
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
    /// Ran and failed. A conflicted merge is this, and so is a raced publish.
    Failed {
        reason: String,
        exit_code: Option<i32>,
        output_tail: String,
    },
    /// Never reached an argv — the row itself was unexecutable, which is a defect in the row and not
    /// a result of the operation.
    ///
    /// Recorded as `failed`, because there is no other honest status for it and adding one would
    /// mean a migration for a case that only a corrupt row can produce. It is told apart in the row
    /// **structurally**, not by reading the prose: every other variant writes an `output_tail`
    /// (possibly empty), and this one writes NULL. `status = 'failed' AND output_tail IS NULL` is
    /// therefore exactly "this row could not be executed", and it is queryable. The status half is
    /// not decoration — `reconcile_interrupted` writes a terminal status without touching this
    /// column, so it leaves NULL on rows where git may well have run, and so does every row still
    /// queued, running or awaiting approval.
    ///
    /// It does *not* mean "no subprocess ran". An operation can fail before reaching one — the
    /// integration worktree turning out not to be a worktree — and that writes an empty tail rather
    /// than NULL, because the row was executable and the environment was not. The two are different
    /// defects and belong to different people.
    Unexecutable { reason: String },
}

impl Outcome {
    /// The `status` column this outcome writes. `Unexecutable` shares `failed` with `Failed`; what
    /// tells them apart in the row is `output_tail IS NULL`, not this.
    ///
    /// One function rather than a string in each of `finish`'s match arms, because `drain_once` now
    /// needs the same answer for the feed: two places deciding what an outcome is called would
    /// drift, and the row and the feed disagreeing is exactly the contradiction the feed exists to
    /// avoid.
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Succeeded { .. } => "succeeded",
            Outcome::Blocked { .. } => "blocked",
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
    let claimed: Option<(i64, String, String, String, String)> = sqlx::query_as(
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
         RETURNING id, op, args, project_id, project_root",
    )
    .bind(started_at)
    .bind(repo_key)
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
            // row out again on the next poll, forever. `Unexecutable` rather than `Failed` because
            // this row never reached an argv, and that is what leaves `output_tail` NULL — the
            // structural discriminator `Outcome::Unexecutable`'s doc comment describes. This arm is
            // its only producer.
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
    let (result_sha, failure_reason, exit_code, output_tail) = match outcome {
        Outcome::Succeeded { sha, output_tail } => (Some(sha), None, None, Some(output_tail)),
        Outcome::Blocked {
            reason,
            output_tail,
        } => (None, Some(reason), None, Some(output_tail)),
        Outcome::Failed {
            reason,
            exit_code,
            output_tail,
        } => (None, Some(reason), exit_code, Some(output_tail)),
        Outcome::Unexecutable { reason } => (None, Some(reason), None, None),
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

/// What `wait_for` hands back: either the row's outcome, if the wait caught it before the deadline,
/// or its current in-flight status if not.
///
/// Serializable because it crosses the boundary Task 8 adds: an agent's blocking merge request gets
/// exactly this back as its HTTP response body, whether the queue answered inside the deadline or
/// not.
///
/// `status` is the same string the `status` column holds rather than an enum: `rejected` and
/// `cancelled` are already in that column's CHECK constraint even though nothing in this module
/// writes them yet, and a `Ticket` round-trips whichever one a row holds without this module
/// needing to know what it means.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: i64,
    pub status: String,
    pub result_sha: Option<String>,
    pub failure_reason: Option<String>,
}

/// One row as a queue listing shows it: what was asked, for which repository, by whom, and where it
/// got to.
///
/// A separate type from `Ticket` rather than a reuse of it, because the two answer different
/// questions. A ticket answers "how did MY request end" and needs the result; a listing answers
/// "what is this queue doing" and needs the operation and the project, which a ticket does not
/// carry — reusing it would produce a column of statuses attached to nothing.
///
/// `op` is the `op` column verbatim, not a parsed `Op`. Parsing can fail on a row written by an
/// older version or edited by hand, and one such row must not be able to fail the whole listing —
/// the listing is exactly where somebody would go to find out that a row is wrong.
/// `FromRow` rather than a positional tuple, for the reason `runs.rs` states: a tuple makes the
/// column-order-to-field-order correspondence load-bearing and invisible, and five of these six
/// fields are `String`, so a swap would compile and pass.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestSummary {
    pub id: i64,
    pub op: String,
    pub project_id: String,
    pub origin: String,
    pub status: String,
    pub created_at: String,
}

/// How many rows a listing returns at most.
///
/// Note what this table is: nothing prunes `vcs_requests`, so it is the permanent history of every
/// git operation this daemon has ever queued, not a snapshot of what is pending. A listing that hits
/// this cap therefore means "the daemon has been running a while" — it is not a finding, and this
/// cap is not a diagnostic. It exists only so one HTTP call cannot return an unbounded response.
///
/// When someone adds retention, or paging, this is where they start.
const LIST_LIMIT: i64 = 200;

/// Newest first, optionally narrowed to one repository.
///
/// Newest first because the question a listing answers is almost always "what just happened", and a
/// caller reading a truncated oldest-first list would be reading history while missing the present.
pub async fn list(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
) -> sqlx::Result<Vec<RequestSummary>> {
    sqlx::query_as(
        "SELECT id, op, project_id, origin, status, created_at
           FROM vcs_requests
          WHERE ?1 IS NULL OR project_id = ?1
          ORDER BY id DESC
          LIMIT ?2",
    )
    .bind(project_id)
    .bind(LIST_LIMIT)
    .fetch_all(pool)
    .await
}

/// The longest a caller that asked to wait is held before it gets a ticket instead.
///
/// Spec decision 3. The common case — an empty queue — answers from the first read and never
/// approaches this. The bad case is two merges queued behind a slow one, and the number exists so
/// that case stops killing the caller's run by timeout: the agent gets a ticket back and decides for
/// itself whether to keep waiting.
///
/// **The constraint to check this against is `state.rs`'s `DEFAULT_PROGRESS_TIMEOUT` (300s), not the
/// 600s wall clock.** A CLI blocked on a call for this long streams no events, and the progress
/// timeout is what kills a run that has gone quiet — so it binds first, and the real margin is
/// roughly 6.7x rather than the 13x the wall clock would suggest. Anyone tempted to lengthen this
/// has to answer to 300s. A wait that outlived the run waiting on it would be worse than no wait.
pub const DEFAULT_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

/// How often `wait_for` re-checks a row that has not reached a terminal status yet.
///
/// 25ms, chosen from two directions that happen to agree.
///
/// In production it bounds how often one waiting caller queries: against the ~45s deadline this
/// pillar is designed around, 10ms would be roughly 4500 reads per waiting agent and 25ms roughly
/// 1800, while the extra latency it can cost — one interval, for a request that finishes just after
/// a poll — is nothing beside a merge measured in seconds.
///
/// In the tests it is the discrimination margin, and that is the reason it is not smaller.
/// `a_finished_request_returns_its_outcome_without_waiting` proves the answer came from the read
/// *before* the first sleep, and elapsed time is the only evidence of that — a loop that slept first
/// would return the same status, just one interval later. At 10ms the assertion sat exactly on the
/// boundary with no headroom, so ordinary scheduler jitter on a loaded laptop could fail a correct
/// implementation. The interval IS the margin; this file's other timing tests are documented as
/// leaving 10x.
///
/// A `Notify` would remove the wait entirely, but nothing here has a real operation duration yet to
/// make that worth the added machinery — the same tradeoff `drain_once`'s doc comment argues for
/// `VcsExecutor` staying a plain trait rather than a channel.
const WAIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Blocks the caller until request `id` reaches a terminal status or `deadline` passes — whichever
/// comes first — and returns a `Ticket` either way.
///
/// Never an error for "still going": a caller told the wait failed would reasonably retry or give
/// up, and both are wrong when the request is simply still queued behind another. The common case —
/// an empty queue — answers from the very first read, before any sleep, so it behaves like an
/// ordinary blocking call. The bad case — two merges queued behind a slow one — stops costing the
/// caller a hard timeout: it gets a ticket back instead and decides for itself whether to keep
/// waiting.
///
/// Terminal means `succeeded`, `failed`, `blocked`, or `interrupted` — the four statuses `finish`
/// and `reconcile_interrupted` actually write today, matching the vocabulary those two already use
/// (see `finish`'s own doc comment, and the interrupted-is-terminal test above). `blocked` is as
/// terminal as the other three: the queue never retries it, so a caller held to the deadline would
/// be waiting on a row that can no longer change — and it is the outcome that most needs a human to
/// see it promptly. `queued`, `running` and `awaiting_approval` are treated identically: all three
/// can still change, so none of them ends the wait early, and if the deadline passes while a row is
/// in any of them the ticket just reports whichever one it is. `awaiting_approval` is deliberately
/// not special-cased to end the wait sooner — a human approving mid-wait is exactly the change this
/// loop is built to catch on its next poll, and treating "needs a human" as if it were "done" would
/// tell an agent to stop watching a request that is very much still alive.
///
/// An `id` with no matching row is answered `Err(RowNotFound)` on the very first read, without
/// spending any of the deadline: every id in circulation came from `submit`, which hands one back
/// only after its INSERT has committed, and nothing in this module deletes a row *today*. So a
/// missing row is not "hasn't arrived yet" — it cannot ever arrive — and polling it out to the
/// deadline would just be quietly burning the caller's wait on a request that does not exist.
///
/// The hedge is deliberate: that is a claim about the whole module, not about this function, and the
/// first retention or cleanup pass added anywhere in `vcs.rs` invalidates it silently — the failure
/// would be a caller told "no such request" about one that merely aged out. Whoever adds pruning
/// owns revisiting this.
///
/// Reads before it ever sleeps, and every subsequent iteration does the same: the terminal check
/// runs on freshly read data, not on whatever the previous iteration saw, so a row that finishes
/// between two polls is reported the moment the next read sees it rather than after the deadline.
pub async fn wait_for(
    pool: &sqlx::SqlitePool,
    id: i64,
    deadline: std::time::Duration,
) -> sqlx::Result<Ticket> {
    let started = std::time::Instant::now();
    loop {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT status, result_sha, failure_reason FROM vcs_requests WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;

        let Some((status, result_sha, failure_reason)) = row else {
            return Err(sqlx::Error::RowNotFound);
        };

        let terminal = matches!(
            status.as_str(),
            "succeeded" | "failed" | "blocked" | "interrupted"
        );
        if terminal || started.elapsed() >= deadline {
            return Ok(Ticket {
                id,
                status,
                result_sha,
                failure_reason,
            });
        }

        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
    }
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
    let id = claimed.id;
    let outcome = executor.execute(&claimed).await;
    // Cloned rather than moved so the failure paths below can still name it. `finish` consumes the
    // outcome, and a refused write would otherwise drop the only copy of a sha that git really
    // produced — leaving a commit the daemon caused recorded nowhere in the system at all.
    match finish(pool, id, outcome.clone()).await {
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
        // `None` for `run_id`: `ClaimedRequest` does not carry one and `claim_next` does not return
        // one, and widening its `RETURNING` to supply it would buy nothing today. A `run` request
        // starts `awaiting_approval` and nothing moves it to `queued` until Chunk 4 wires
        // `proposals.rs`, so every claimable request in this chunk is `Human` or `Shell` and that
        // column is NULL regardless. Chunk 4 is where threading it earns its keep.
        // (`reconcile_interrupted` does attach one, because it reads whole rows rather than a claim.)
        Ok(()) => {
            let _ = crate::feed::append(
                pool,
                Some(claimed.project_id.as_str()),
                "vcs_request_finished",
                &format!("vcs request {id} {}", outcome.status()),
                None,
            )
            .await;
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
const WORKER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

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
    /// A rendezvous every execution must reach before any of them may answer.
    ///
    /// The only way to assert concurrency without betting on a scheduler: `n` executions in flight
    /// at once release each other, and `n - 1` or fewer never return at all. A test that instead
    /// measured elapsed time would be asserting that two things overlapped by looking at how long
    /// they took, which is a guess on a loaded machine; this is the property itself.
    barrier: Option<tokio::sync::Barrier>,
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
    /// `output_tail` is non-empty and deliberately unlike the sha, for the reason
    /// `failing_with`'s doc comment gives about its own two strings: a fake whose two columns
    /// carried the same text could not tell a test that they had been swapped.
    fn succeeding_with(sha: &str) -> Self {
        Self::reporting(Outcome::Succeeded {
            sha: sha.into(),
            output_tail: format!("git printed this while succeeding at {sha}"),
        })
    }

    /// Answers, but not immediately.
    fn succeeding_slowly(sha: &str, delay: std::time::Duration) -> Self {
        Self {
            delay,
            ..Self::succeeding_with(sha)
        }
    }

    /// Answers only once `n` executions are in flight at the same moment — so a caller that runs
    /// them one after the other never gets an answer at all.
    fn rendezvous_of(n: usize, sha: &str) -> Self {
        Self {
            barrier: Some(tokio::sync::Barrier::new(n)),
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
            barrier: None,
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
        // Held here rather than before the delay so it is the last thing between being entered and
        // answering: whatever else an execution does, it does not finish until its peers arrive.
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        self.outcome.clone()
    }
}

#[cfg(test)]
mod tests {
    // `a_real_merge_lands_through_the_queue` holds `worktree::test_env_lock()`'s `MutexGuard` across
    // every await in it, and that is the point rather than an oversight: the NUCLEOS_WORKTREE_ROOT
    // override it serialises is process-wide, so it has to be held for the whole test. These are
    // `current_thread` tests with no multi-thread runtime to starve, so `await_holding_lock` is a
    // false positive — the same one `job.rs`, `runs.rs`, `worktree.rs` and `git_exec.rs` each carry.
    // An inner attribute, so it must precede every item in the module.
    #![allow(clippy::await_holding_lock)]

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

    fn repo() -> ResolvedRepo {
        repo_for("alpha")
    }

    /// In tests the repository key is the project name. That keeps every existing `claim_next(&pool,
    /// "alpha")` meaning what it meant, so the rewrite cannot silently swap a key for a label — and it
    /// leaves `two_projects_naming_one_repository_cannot_both_be_running` as the one place where the two
    /// deliberately differ.
    fn repo_for(project: &str) -> ResolvedRepo {
        ResolvedRepo::synthetic(project, "C:/repo", project)
    }

    fn merge_op() -> Op {
        Op::Merge {
            source: "feat/x".into(),
            target: "master".into(),
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

    /// Returns the `Option` rather than `unwrap_or_default()`ing it: NULL and `""` are different
    /// things in this column — NULL means the row never reached an argv — and collapsing them would
    /// erase exactly the distinction its callers are checking.
    async fn output_tail_of(pool: &sqlx::SqlitePool, id: i64) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>("SELECT output_tail FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
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
        // `repo_key` is bound to the same project this is given, for the reason `repo_for` states:
        // in tests the repository key is the project name. Left to the column's `''` default these
        // rows would be invisible to every `claim_next` and would collide with each other on the
        // partial unique index — two failures with nothing to do with what any caller is testing.
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, repo_key, origin, status, created_at)
             VALUES ('merge', ?, ?, 'C:/repo', ?, 'human', ?, '2026-08-02T00:00:00Z')",
        )
        .bind(args)
        .bind(project)
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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Shell)
            .await
            .unwrap();

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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
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

        let from_run = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        let from_job = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Job(7))
            .await
            .unwrap();

        assert_eq!(run_id_of(&pool, from_run).await, Some(7));
        assert_eq!(run_id_of(&pool, from_job).await, None);
    }

    #[tokio::test]
    async fn the_queue_is_served_in_arrival_order() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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
                output_tail: "Merge made by the 'ort' strategy.".into(),
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
        submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert!(claim_next(&pool, "alpha").await.unwrap().is_some());
        assert!(claim_next(&pool, "beta").await.unwrap().is_some());
    }

    /// Two project ids, one repository. The queue's promise is per REPOSITORY, so the second waits.
    ///
    /// Before this chunk both were claimable at once: the unique index and the claim both filtered on
    /// `project_id` while the git that would run used `project_root`. Inert only because nothing in
    /// production built a request.
    #[tokio::test]
    async fn two_projects_naming_one_repository_cannot_both_be_running() {
        let pool = test_pool().await;
        let alpha = ResolvedRepo::synthetic("alpha", "C:/repo", "SHARED");
        let beta = ResolvedRepo::synthetic("beta", "C:/repo", "SHARED");

        submit(&pool, &alpha, &merge_op(), Origin::Human)
            .await
            .unwrap();
        submit(&pool, &beta, &merge_op(), Origin::Human)
            .await
            .unwrap();

        assert!(claim_next(&pool, "SHARED").await.unwrap().is_some());
        assert!(
            claim_next(&pool, "SHARED").await.unwrap().is_none(),
            "the second project claimed the repository the first is holding"
        );
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
        let behind_it = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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
        // The only production producer of `Outcome::Unexecutable`, and the only place its NULL
        // `output_tail` can be caught being written: revert this arm to a `Failed` with an empty
        // tail and every other assertion here still passes.
        assert!(
            output_tail_of(&pool, corrupt).await.is_none(),
            "a row that never reached an argv writes no output tail; that NULL is what tells it \
             apart from an operation that ran and failed"
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
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap().unwrap();

        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: "abc123".into(),
                output_tail: "Fast-forward".into(),
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
        // Not NULL: a success keeps what it printed, and NULL in this column now means something
        // else entirely — that the row never reached an argv (`Outcome::Unexecutable`).
        assert_eq!(output_tail.as_deref(), Some("Fast-forward"));
        assert_eq!(reason, None);
    }

    /// A successful command's output has somewhere to go. Chunk 1 could not record it: a merge that
    /// succeeded with warnings — a renamed file resolved, a hook's advice — printed them into nothing.
    #[tokio::test]
    async fn a_successful_operation_keeps_what_it_printed() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: "abc123".into(),
                output_tail: "Merge made by the 'ort' strategy.".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            output_tail_of(&pool, id).await.as_deref(),
            Some("Merge made by the 'ort' strategy.")
        );
    }

    #[tokio::test]
    async fn a_failed_request_records_why_and_what_it_printed() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
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

    /// The deferred decision, settled in the row rather than in prose: `output_tail IS NULL` means the
    /// request never reached an argv. Both of these rows read `failed`, so without a structural
    /// discriminator anyone querying failures for execution diagnostics finds entries with no exit code
    /// and no output and no way to tell why.
    #[tokio::test]
    async fn a_request_that_never_ran_is_distinguishable_from_one_that_ran_and_failed() {
        let pool = test_pool().await;

        let never_ran = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            never_ran,
            Outcome::Unexecutable {
                reason: "stored operation could not be parsed".into(),
            },
        )
        .await
        .unwrap();

        let ran = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            ran,
            Outcome::Failed {
                reason: "CONFLICT (content)".into(),
                exit_code: Some(1),
                output_tail: "Automatic merge failed".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(status_of(&pool, never_ran).await, "failed");
        assert_eq!(status_of(&pool, ran).await, "failed");
        assert!(output_tail_of(&pool, never_ran).await.is_none());
        assert!(output_tail_of(&pool, ran).await.is_some());
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
                output_tail: "Merge made by the 'ort' strategy.".into(),
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
        submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();
        assert!(claim_next(&pool, "alpha").await.unwrap().is_none());
    }

    /// Both branches of the filter, and the ordering, because neither is visible from one row.
    ///
    /// The HTTP test that exercises this route inserts a single request and passes no project, so
    /// `WHERE ?1 IS NULL OR project_id = ?1` never takes its second path there and `ORDER BY id DESC`
    /// cannot be told from `ASC`. `job::list` has the same `Option` filter and covers both — this is
    /// that precedent applied.
    #[tokio::test]
    async fn a_listing_narrows_to_one_repository_and_puts_the_newest_first() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let third = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let everything = list(&pool, None).await.unwrap();
        assert_eq!(
            everything.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![third, second, first],
            "newest first: a caller reading a truncated list must see the present, not history"
        );

        let just_alpha = list(&pool, Some("alpha")).await.unwrap();
        assert_eq!(
            just_alpha.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![third, first]
        );

        // The fields that make a listing answer "what is this queue doing" rather than just
        // "something happened" — and the ones a positional row mapping could silently transpose.
        assert_eq!(just_alpha[0].op, "merge");
        assert_eq!(just_alpha[0].project_id, "alpha");
        assert_eq!(just_alpha[0].origin, "human");
        assert_eq!(just_alpha[0].status, "queued");
        assert!(!just_alpha[0].created_at.is_empty());
    }

    /// A `running` row at startup means the daemon died mid-operation, and nothing can say whether git
    /// finished. Auto-retry is not an option: a re-run `merge` is harmless, a re-run `tag` is not, and
    /// telling them apart from a cold start is guessing. It is recorded and left for a human — the
    /// same call `runs::reconcile_orphaned_runs` makes.
    #[tokio::test]
    async fn a_request_running_at_startup_is_marked_interrupted_not_retried() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        reconcile_interrupted(&pool).await.unwrap();

        let late = finish(
            &pool,
            id,
            Outcome::Succeeded {
                sha: "abc123".into(),
                output_tail: "Merge made by the 'ort' strategy.".into(),
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
        let first = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
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
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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

    /// Spec §6.4 step 5 ends "escreve `result_sha`, `succeeded`, **feed**", and spec §2.1 is why:
    /// this pillar has no view of its own precisely because every transition writes to `feed.rs`,
    /// which the shell already shows. Without this row a merge the daemon performed is invisible to
    /// the person who asked for it.
    #[tokio::test]
    async fn a_finished_request_is_reported_in_the_feed() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("abc123")).await;

        let summaries: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].contains("succeeded"), "got: {}", summaries[0]);
    }

    /// Everything the daemon logged while `body` ran.
    ///
    /// Follows `logging.rs`'s own test rather than `init()`, which installs a *global* subscriber and
    /// would panic the moment a second test did the same; `set_default` is scoped and thread-local,
    /// which is sound here because `#[tokio::test]`'s default runtime polls on the thread that set
    /// it. The writer is the non-blocking one, so the guard has to be dropped before reading back.
    ///
    /// Worth the machinery for exactly one reason: on the refused-write path the log line is not
    /// commentary, it is the only place the outcome still exists.
    async fn logged_during<F: std::future::Future>(body: F) -> String {
        let dir = tempfile::tempdir().unwrap();
        let (writer, flush_on_drop) = tracing_appender::non_blocking(
            tracing_appender::rolling::daily(dir.path(), "test.log"),
        );
        {
            let subscriber = tracing_subscriber::fmt().with_writer(writer).finish();
            let _scope = tracing::subscriber::set_default(subscriber);
            body.await;
        }
        drop(flush_on_drop);
        std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect()
    }

    /// The outcome of a merge the row refuses has nowhere else to go.
    ///
    /// `finish` consumed the outcome before this branch existed, so a real `abc123` was dropped on
    /// the floor: the row read `interrupted`, and a commit the daemon caused was recorded nowhere in
    /// the system. The log line is the whole remedy, which makes it load-bearing rather than
    /// commentary — and the previous version of it announced a jam that does not happen on this
    /// path, sending a reader hunting a stuck queue that is actually fine.
    ///
    /// Asserting on log text is not this crate's habit and should stay rare. It is justified here
    /// because both halves — that the sha survives, and that the message does not misdescribe the
    /// state — are invisible to every other assertion available.
    #[tokio::test]
    async fn an_outcome_the_row_refuses_survives_in_the_log_and_is_not_called_a_jam() {
        let pool = test_pool().await;
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_millis(100));

        let logged = logged_during(async {
            tokio::join!(drain_once(&pool, "alpha", &executor), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                reconcile_interrupted(&pool).await.unwrap()
            })
        })
        .await;

        assert!(
            logged.contains("abc123"),
            "the sha the row refused must survive somewhere: {logged}"
        );
        assert!(
            !logged.contains("stays running"),
            "the row is terminal, so the queue is not jammed and must not be reported as one: {logged}"
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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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

        // The feed append is conditional on the terminal write having succeeded, and this is the
        // only assertion in the module that says so. The row reads `interrupted`; an unconditional
        // append would sit "vcs request 1 succeeded" beside it, in the one surface spec §2.1 says
        // the user actually looks at — a contradiction, and one no status assertion can see because
        // the row is already correct.
        let announced: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            announced.is_empty(),
            "a terminal write the row refused must not be announced as a finish: {announced:?}"
        );

        // And the repository is free: a refused write means the row is terminal, so the queue moves
        // on rather than waiting behind it.
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
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
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

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
        submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        assert!(
            claim_next(&pool, "alpha").await.unwrap().is_some(),
            "once reconciled, the repository is free again"
        );
    }

    /// The worker looks for repositories by their KEY, and in production a key is never a project's
    /// name — it is git's canonical common directory. Polling `DISTINCT project_id` here would hand
    /// `claim_next` a label that no row carries, and the queue would drain nothing, for ever, in
    /// silence.
    ///
    /// **Every other test in this module is structurally blind to that.** `repo_for` makes the key
    /// equal the project name on purpose, so that moving the lock from label to key preserved each
    /// existing assertion's meaning. The cost of that choice is exactly this blindness, and it is
    /// not hypothetical: a mutation that polls `project_id` passed all 41 of the others. This is the
    /// one place where the two must differ.
    #[tokio::test]
    async fn the_worker_looks_for_repositories_by_key_and_not_by_project_name() {
        let pool = test_pool().await;
        let repo = ResolvedRepo::synthetic("alpha", "C:/repo", "a-key-that-is-not-a-project-name");
        let id = submit(&pool, &repo, &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = std::sync::Arc::new(FakeVcsExecutor::succeeding_with("abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, id).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect(
            "the worker never found the repository — it is looking for it by the project's name",
        );
    }

    /// Two repositories, one worker. If it drains them one after the other, neither of these executions
    /// can complete: the fake will not answer until both have arrived.
    #[tokio::test]
    async fn separate_repositories_are_drained_concurrently() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = std::sync::Arc::new(FakeVcsExecutor::rendezvous_of(2, "abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, first).await == "succeeded"
                    && status_of(&pool, second).await == "succeeded"
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect(
            "both repositories must be in flight at once; a worker that serialises projects deadlocks here",
        );
    }

    /// The loop keeps looping — the property the daemon's whole use of this worker rests on, and the
    /// one both tests around it are structurally blind to.
    ///
    /// They submit everything *before* the spawn, so a worker that polls exactly once and returns
    /// passes them both. In production that worker drains nothing, ever: at startup the queue is
    /// empty, and every request arrives afterwards. The failure would not be "wrong at an edge", it
    /// would be "the pillar does nothing", with the suite green.
    ///
    /// **No timing assertion, and no sleep to let a poll go by.** The first request is what proves
    /// the worker's first pass already happened — it cannot have succeeded otherwise — so the second
    /// is submitted into a worker that is provably past that pass. The only wait is for the second to
    /// finish, inside a budget 10x `WORKER_POLL_INTERVAL`, which is this file's documented margin.
    ///
    /// The second request goes to a **different repository** deliberately. A one-pass worker's
    /// spawned task loops on the repository it was given, so a second request for `alpha` could be
    /// swept up by a task that happened to still be draining — the test would then pass for a reason
    /// that is not the property. `beta` was in no pass that worker ever made, so only another poll
    /// can reach it.
    #[tokio::test]
    async fn a_request_submitted_after_the_worker_started_is_still_drained() {
        let pool = test_pool().await;
        let executor = std::sync::Arc::new(FakeVcsExecutor::succeeding_with("abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let first = submit(&pool, &repo_for("alpha"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let drained_once = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, first).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        // Not the property under test — it is the precondition for it. Asserted separately so a
        // worker that never started at all is told apart from one that started and stopped.
        drained_once.expect("the worker's first pass should drain what was queued for it");

        // Submitted only now: the pass above is over, so nothing but a later poll can find this.
        let second = submit(&pool, &repo_for("beta"), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, second).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect(
            "the worker must keep polling; one that stops after its first pass would drain nothing \
             the daemon is ever actually asked to do",
        );
    }

    /// And the other half, which the rendezvous cannot show: one repository's requests still run one at
    /// a time, in order.
    ///
    /// **What this holds, exactly, now that mutation has measured it.** Its ordering assertion is
    /// redundant against `the_queue_is_served_in_arrival_order` and
    /// `the_queue_is_drained_in_order_and_each_outcome_recorded` — reversing `claim_next`'s `ORDER BY`
    /// reddens all three. It is kept because those two call `drain_once` by hand, twice, and this is
    /// the only test where the *worker* is what reaches the second request: it pins that a spawned
    /// task drains a repository rather than one request of it.
    ///
    /// It does not pin *when*. Draining one request per tick instead of until empty passes this
    /// unchanged, because the tick is 500ms and the budget below is 5s. Closing that would take an
    /// elapsed-time assertion against `WORKER_POLL_INTERVAL` with roughly 2x of margin, which is
    /// under this file's convention and is the flaky bet
    /// `separate_repositories_are_drained_concurrently` was written to avoid. The gap is named here
    /// rather than papered over.
    #[tokio::test]
    async fn one_repository_is_still_drained_in_order_one_at_a_time() {
        let pool = test_pool().await;
        let first = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        let second = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = std::sync::Arc::new(FakeVcsExecutor::succeeding_with("abc123"));
        let worker = tokio::spawn(run_queue_worker(pool.clone(), executor.clone()));

        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if status_of(&pool, second).await == "succeeded" {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;

        worker.abort();
        settled.expect("the worker should drain the queue");
        assert_eq!(status_of(&pool, first).await, "succeeded");
        assert_eq!(
            executor
                .seen()
                .iter()
                .map(|request| request.id)
                .collect::<Vec<_>>(),
            vec![first, second],
            "arrival order, and each one only after the last finished"
        );
    }

    /// The empty-queue common case: the request is already `succeeded` before `wait_for` is ever
    /// called, so the answer must come from the first read — no sleep, no `WAIT_POLL_INTERVAL`
    /// paid at all.
    #[tokio::test]
    async fn a_finished_request_returns_its_outcome_without_waiting() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        drain_once(&pool, "alpha", &FakeVcsExecutor::succeeding_with("abc123")).await;

        let started = std::time::Instant::now();
        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("abc123"));
        // The name's actual claim: without this, a loop that sleeps before its first read would
        // report the same status and sha 50ms later and still pass every assertion above. A single
        // in-memory read takes microseconds; one `WAIT_POLL_INTERVAL` sleep alone is 10ms, so this
        // is not a close margin — it is the difference between "never slept" and "slept at all".
        assert!(
            started.elapsed() < WAIT_POLL_INTERVAL,
            "an already-finished request must answer from the first read, not pay for a poll"
        );
    }

    /// The deadline hands back a ticket, never an error: "still queued" is not a failure, and an agent
    /// told it failed would either give up or retry — both wrong.
    #[tokio::test]
    async fn an_unfinished_request_hands_back_a_ticket_rather_than_failing() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(ticket.status, "queued");
        assert!(ticket.result_sha.is_none());
    }

    /// The two tests above never read `failure_reason` or `id` — both come out `None`/moot in
    /// every case they cover, so a `wait_for` that dropped `failure_reason`, or swapped it for
    /// `output_tail`, would still pass them. A failed request is the only scenario that puts a real
    /// value in that column, so it is the only thing that can catch that class of bug.
    #[tokio::test]
    async fn a_failed_requests_ticket_carries_its_id_and_its_reason() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        drain_once(
            &pool,
            "alpha",
            &FakeVcsExecutor::failing_with("CONFLICT (content)"),
        )
        .await;

        let started = std::time::Instant::now();
        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();

        assert_eq!(ticket.id, id);
        assert_eq!(ticket.status, "failed");
        assert!(
            ticket.result_sha.is_none(),
            "nothing succeeded, so there is no commit"
        );
        assert_eq!(ticket.failure_reason.as_deref(), Some("CONFLICT (content)"));
        // Without this, a `wait_for` that dropped `failed` from its terminal set would still
        // report the right content 50ms later once the deadline forced an answer, and every
        // assertion above would still pass. This is what actually proves `failed` ends the wait as
        // fast as `succeeded` does, rather than merely agreeing with it once time runs out.
        assert!(
            started.elapsed() < WAIT_POLL_INTERVAL,
            "a failed request must answer from the first read, not pay for a poll"
        );
    }

    /// Terminal, and terminal in the way that matters: a caller waiting on a blocked request must be
    /// told now, not at the deadline. The elapsed assertion is the whole test — a wait that ran to its
    /// deadline would return the identical ticket.
    #[tokio::test]
    async fn a_blocked_request_ends_the_wait_rather_than_running_it_out() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();
        claim_next(&pool, "alpha").await.unwrap();
        finish(
            &pool,
            id,
            Outcome::Blocked {
                reason: "uncommitted changes in the target worktree are in the way".into(),
                output_tail:
                    "error: Your local changes to the following files would be overwritten by merge:\n\tnotes.txt"
                        .into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(status_of(&pool, id).await, "blocked");

        let started = std::time::Instant::now();
        let ticket = wait_for(&pool, id, Duration::from_secs(10)).await.unwrap();
        assert_eq!(ticket.status, "blocked");
        assert!(
            ticket.failure_reason.unwrap().contains("in the way"),
            "the ticket must carry why it is blocked, or the agent cannot act on it"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "blocked is terminal: the wait must not run to its deadline"
        );
    }

    /// A caller waiting on an id nothing ever inserted must not spend the deadline finding that
    /// out. Every id in circulation came from `submit`, which hands one back only after its INSERT
    /// commits, and nothing in this module ever deletes a row — so a missing row can never later
    /// appear, and treating it like "not finished yet" would silently burn the whole wait on a
    /// request that does not exist.
    #[tokio::test]
    async fn waiting_on_an_unknown_id_fails_immediately_rather_than_waiting_out_the_deadline() {
        let pool = test_pool().await;
        let started = std::time::Instant::now();

        let result = wait_for(&pool, 999_999, Duration::from_secs(5)).await;

        assert!(matches!(result, Err(sqlx::Error::RowNotFound)));
        // Two orders of magnitude under the 5s deadline: not a close race, just proof this
        // returned from the first read rather than polling until the deadline passed.
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "an id that can never exist must not cost the caller the deadline"
        );
    }

    /// `awaiting_approval` is not treated as done: a human still has to act on it, and an agent
    /// told its request had reached a stable end state would stop watching a request that is very
    /// much still alive. It is handled exactly like `queued` — reported as-is once the deadline
    /// passes, never ending the wait early.
    #[tokio::test]
    async fn a_request_still_awaiting_approval_hands_back_a_ticket_too() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Run(7))
            .await
            .unwrap();

        let ticket = wait_for(&pool, id, Duration::from_millis(50))
            .await
            .unwrap();

        assert_eq!(ticket.status, "awaiting_approval");
        assert!(ticket.result_sha.is_none());
        assert!(ticket.failure_reason.is_none());
    }

    /// The actual point of a bounded wait, not just its two edges: a request that is still queued
    /// when `wait_for` starts but finishes partway through a generous deadline must be reported as
    /// soon as the next poll sees it — "if its turn comes, it gets the result" — not held until the
    /// deadline passes regardless. Neither test above exercises this: one starts already finished,
    /// the other never finishes at all, so a `wait_for` that read the row once and then only ever
    /// re-checked the clock would pass both.
    #[tokio::test]
    async fn a_request_that_finishes_mid_wait_is_reported_before_the_deadline() {
        let pool = test_pool().await;
        let id = submit(&pool, &repo(), &merge_op(), Origin::Human)
            .await
            .unwrap();

        let executor = FakeVcsExecutor::succeeding_slowly("abc123", Duration::from_millis(20));
        let started = std::time::Instant::now();
        // The deadline is two orders of magnitude past how long the operation actually takes: what
        // this proves is that `wait_for` returns once the row finishes, not that it merely survives
        // to a deadline that happens to still be far away.
        let (ticket, drained) = tokio::join!(wait_for(&pool, id, Duration::from_secs(5)), async {
            // A head start so `wait_for`'s first read sees "queued", not "running" — the loop, not
            // a lucky initial read, is what has to notice the finish.
            tokio::time::sleep(Duration::from_millis(5)).await;
            drain_once(&pool, "alpha", &executor).await
        });

        assert!(drained, "the operation ran");
        let ticket = ticket.unwrap();
        assert_eq!(ticket.status, "succeeded");
        assert_eq!(ticket.result_sha.as_deref(), Some("abc123"));
        // A 10x margin under the 5s deadline: the operation itself finishes around 25ms in and a
        // 10ms poll interval should catch it shortly after, so 500ms is nowhere near a close race
        // in either direction.
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "wait_for must return once the request finishes, not hold the caller to the full \
             deadline: took {:?}",
            started.elapsed()
        );
    }

    /// Everything, once: a request submitted through the queue, drained by the real executor, against a
    /// real repository — and the sha in the row is the commit git actually created.
    #[tokio::test]
    async fn a_real_merge_lands_through_the_queue() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            crate::git_exec::tests::repo_with_a_branch_to_merge("nucleos-vcs-e2e-");
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-vcs-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());

        // Real root, synthetic key: what this test exercises is the executor against a repository
        // that is really there, and `repo_for`'s "the key is the project name" is what keeps
        // `drain_once(&pool, "alpha", ..)` below meaning what it meant.
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha"),
            &merge_op(),
            Origin::Human,
        )
        .await
        .unwrap();

        assert!(drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await);

        assert_eq!(status_of(&pool, id).await, "succeeded");
        let ticket = wait_for(&pool, id, Duration::ZERO).await.unwrap();
        assert_eq!(
            ticket.result_sha.unwrap(),
            crate::git_exec::tests::sha_of(&repo, "master")
        );
    }

    /// The other half of that wiring, and the half a happy path structurally cannot see: `execute`
    /// has to report what `publish` *answered*, not merely that it called it.
    ///
    /// Measured, not assumed. Rewriting the merge arm to run `publish`, discard its `Outcome` and
    /// return `Succeeded { sha: computed.new }` passes all 1059 other tests in this crate — the e2e
    /// test above included, because on its happy path the publish does land and the two shas agree.
    /// What that would ship is the worst row this pillar can write: `succeeded`, naming a sha that
    /// is on no branch, announced in the feed to the person who asked for it. Only a publish that
    /// refuses tells the two apart, so this drives one — the user is mid-edit on the very file the
    /// merge brings in, which is `a_user_s_uncommitted_file_blocks_the_publish_and_survives_it_untouched`
    /// seen from the queue's side rather than from `publish`'s.
    ///
    /// It also pins the feed's status as *derived* rather than canned, which is the whole reason
    /// `Outcome::status()` exists as one function: the summary here reads `blocked`, so a hardcoded
    /// "succeeded" cannot survive both this and `a_finished_request_is_reported_in_the_feed`.
    #[tokio::test]
    async fn a_merge_the_queue_could_not_publish_is_not_recorded_as_succeeded() {
        let _lock = crate::worktree::test_env_lock();
        let pool = test_pool().await;
        let (_container, repo) =
            crate::git_exec::tests::repo_with_a_branch_to_merge("nucleos-vcs-e2e-blocked-");
        let roots = crate::git_exec::tests::space_free_tempdir("nucleos-vcs-wt-");
        let _env = crate::git_exec::tests::WorktreeRootEnv::set(roots.path());

        // `feature.txt` is what `feat/x` adds, so the fast-forward has to write it — and it cannot,
        // because the user has an uncommitted copy of it sitting there.
        std::fs::write(repo.join("feature.txt"), "half-finished thought\n").expect("write");
        let before = crate::git_exec::tests::sha_of(&repo, "master");

        // Real root, synthetic key, for the reason the test above states.
        let id = submit(
            &pool,
            &ResolvedRepo::synthetic("alpha", &repo.to_string_lossy(), "alpha"),
            &merge_op(),
            Origin::Human,
        )
        .await
        .unwrap();

        assert!(drain_once(&pool, "alpha", &crate::git_exec::GitExecutor::default()).await);

        assert_eq!(status_of(&pool, id).await, "blocked");
        assert_eq!(
            crate::git_exec::tests::sha_of(&repo, "master"),
            before,
            "nothing was published, so the row must not claim anything was"
        );
        let ticket = wait_for(&pool, id, Duration::ZERO).await.unwrap();
        assert!(
            ticket.result_sha.is_none(),
            "nothing landed, so there is no commit to name: {ticket:?}"
        );

        let summaries: Vec<String> =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'vcs_request_finished'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(summaries, vec![format!("vcs request {id} blocked")]);
    }
}
