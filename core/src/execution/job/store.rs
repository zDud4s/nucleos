//! The job rows: loading a view, status writes guarded by compare-and-set, inserting, cancelling, and recovering orphans at startup.

use super::*;

/// A job whose node stopped to ask permission for one action.
pub const STATUS_AWAITING_APPROVAL: &str = "awaiting_approval";

/// Assembles what `next_step` needs from the two tables.
pub async fn load_view(pool: &SqlitePool, job_id: i64) -> sqlx::Result<JobView> {
    let (status, resume_status, review_wanted, gate_retries, team_id): (
        String,
        Option<String>,
        i64,
        i64,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status, resume_status, review, gate_retries, team_id FROM jobs WHERE id = ?",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await?;
    let stage = effective_status(&status, resume_status.as_deref());

    let (round, dry_rounds, max_rounds, replan_done, opened_by, rule_name, budget_usd): RoundColumns =
        sqlx::query_as(
            "SELECT round, dry_rounds, max_rounds, replan_done, replan_run_id, rule_name, budget_usd
             FROM jobs WHERE id = ?",
        )
        .bind(job_id)
        .fetch_one(pool)
        .await?;

    // Deliberately NOT filtered by round, and the reason is worth writing down because filtering is
    // the obvious thing to reach for. A round closes only when every item in it is terminal, so an
    // earlier round's items are all `Passed`, `Skipped`, `GateFailed` or worse — states `next_step`
    // already walks past. The unfiltered queue therefore reads correctly on its own, and it keeps
    // `ordinal` meaning one thing everywhere: the nth item of this job, ever, which is also its
    // primary key. Filtering would have made the position in this vector stop being the ordinal in
    // the table, and `advance` looks items up by that number.
    //
    // It also keeps `ending()` right across rounds: a red gate in round 1 still makes the job
    // `gate_failed` when round 3 finishes, which is what a reader of the branch needs to know.
    //
    // `gate_attempts` travels with the status because the status alone cannot say what `gate_failed`
    // means any more: read against the job's budget it is either an item to try again or an item
    // that is over, and reading it without the count would make every red gate terminal again.
    let items: Vec<(String, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status, gate_attempts, depends_on, files FROM job_items
         WHERE job_id = ? ORDER BY ordinal",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;

    // The team's ceiling, read here rather than carried on `jobs`, because it is the team's number
    // and an owner who raises it means it to apply to the job running right now. One without a
    // team, and never zero: `teams.max_parallel` is NOT NULL but nothing stops a 0 being written,
    // and a ceiling of zero would build an empty batch for ever over a queue that still owed work.
    let max_parallel: i64 = match team_id.as_deref() {
        Some(team) => sqlx::query_scalar("SELECT max_parallel FROM teams WHERE id = ?")
            .bind(team)
            .fetch_optional(pool)
            .await?
            .unwrap_or(1),
        None => 1,
    };

    // Both node states are read by stage, not from the job's status: the job can be sitting in
    // `waiting` for budget while a node is the thing that has yet to run.
    let latest_node = |stage: &'static str| async move {
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM runs WHERE job_id = ? AND stage = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(job_id)
        .bind(stage)
        .fetch_optional(pool)
        .await
    };
    // The one question the unfiltered queue above cannot answer: did THIS round put anything in it.
    let queued_this_round: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM job_items WHERE job_id = ? AND round = ?")
            .bind(job_id)
            .bind(round)
            .fetch_one(pool)
            .await?;
    // And the one after it: did anything this round queued PASS. Asked of the rows for the same
    // reason, and asked on every read rather than written down, so a round spared its review is
    // spared it again after a restart — see `ReviewState::Skipped`.
    let passed_this_round: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM job_items WHERE job_id = ? AND round = ? AND status = 'passed'",
    )
    .bind(job_id)
    .bind(round)
    .fetch_one(pool)
    .await?;

    let plan_run = latest_node("plan").await?;
    let spec_run = latest_node("spec").await?;

    // The replan node is what OPENS a round, so its id is the line between one round and the last —
    // which is how the review below is scoped without a `runs.round` column. Without the scoping the
    // second round would read the first round's finished review as its own and skip reviewing
    // itself, silently, for every round after the first.
    let latest_replan: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, status FROM runs WHERE job_id = ? AND stage = 'replan' ORDER BY id DESC LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    // `jobs.replan_run_id` and NOT the latest replan run, and the difference is a whole node.
    //
    // The latest replan may be the one deciding RIGHT NOW whether there is another round — it has
    // opened nothing. Using its id moved the line the moment that node was spawned, which dropped
    // this round's finished review out of view and made the review read `Pending` again. Measured
    // on job 17, 2026-08-08: run 900179, a second review of a round whose queue had not changed,
    // spawned between the replan being started and its answer being read. One wasted node per round.
    //
    // The column is the honest line because it is written by `open_the_next_round` and by
    // `stop_after_replan` — the two places where a replan's answer has actually been acted on.
    let round_opened_at = opened_by.unwrap_or(0);
    // The round's latest review, how many reviews the round has had, and — for a review that
    // FAILED, and only then — what it printed. The `CASE` is the point of the second column: a
    // review that completed can carry a long stream, and nothing here reads it, so it is never
    // loaded.
    let review_run: Option<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT status, CASE WHEN status = 'failed' THEN stdout END,
                (SELECT COUNT(*) FROM runs WHERE job_id = ? AND stage = 'review' AND id > ?)
         FROM runs WHERE job_id = ? AND stage = 'review' AND id > ?
         ORDER BY id DESC LIMIT 1",
    )
    .bind(job_id)
    .bind(round_opened_at)
    .bind(job_id)
    .bind(round_opened_at)
    .fetch_optional(pool)
    .await?;

    let review = match (review_wanted != 0, review_run) {
        (false, _) => ReviewState::NotWanted,
        // Only while the round has no review of its own. One already running, or landed, is read
        // for what it is, which also leaves a job that reviewed such a round before this existed
        // reading exactly as it did.
        (true, None) if passed_this_round == 0 => ReviewState::Skipped,
        (true, None) => ReviewState::Pending,
        (true, Some((status, _, _))) if node_in_flight(&status) => ReviewState::Running,
        // The one exception to the arm below, and narrow on purpose: the round's FIRST review,
        // ended `failed` (the stream is only loaded for that status), on an API error the CLI
        // itself says a second attempt can get past. A node that never reached the model judged
        // nothing. Everything else — `timed_out`, `cancelled`, a review a hook gave up on, any
        // other failure, and every second review — stays `Done`. See `ReviewState::Retry`.
        (true, Some((_, Some(stdout), 1)))
            if crate::execution::runner::failed_on_a_transient_api_error(&stdout) =>
        {
            ReviewState::Retry
        }
        // A review that failed is still a review that happened. Its verdict is advisory — §5.5 of
        // the design gives ship/no-ship to the gate — so a job does not fail for want of one, and
        // re-running it would spend a whole node to re-derive an opinion nobody is blocked on.
        (true, Some(_)) => ReviewState::Done,
    };

    Ok(JobView {
        // Two readings, either of which is enough. `planning` is the one status that means the
        // queue does not exist yet; everything past it has been through the plan node, including a
        // planner that honestly found nothing to do. And a job that HAS items has plainly planned,
        // whatever its status says — which is what stops a status lost to a bad resume from
        // spawning a second planner on top of a queue that already exists.
        // Four arms, and each one answers a job the others miss.
        //
        // A spec run that has ENDED, whichever way it ended — the ordinary case, and why a spec
        // node that failed does not strand a job (see the field's own doc).
        //
        // **A plan node existing AT ALL**, which is the arm two tests had to teach this: a job whose
        // planner is running, or paused waiting for a person to approve one action, has a stage of
        // `planning` and an empty queue, so without this it read as unspecced and the very next tick
        // sent it BACKWARDS to write a brief for a plan already under way.
        //
        // Then a job past the planning stage, and a job with a queue: two readings of "this predates
        // the spec node", kept apart because a job whose status was lost to a bad resume has one of
        // them and not the other.
        specced: spec_run
            .as_deref()
            .is_some_and(|status| !node_in_flight(status))
            || plan_run.is_some()
            || stage != "planning"
            || !items.is_empty(),
        speccing: spec_run.as_deref().is_some_and(node_in_flight),
        planned: stage != "planning" || !items.is_empty(),
        planning: plan_run.as_deref().is_some_and(node_in_flight),
        items: items
            .iter()
            .map(|(status, attempts, depends_on, files)| ItemView {
                state: item_state_from(status, *attempts, gate_retries),
                // A list that will not parse is read as no list at all, and that is the safe
                // direction: an item believed to depend on nothing is started early, where an item
                // believed to depend on something that is not there would never start. The column
                // is written by this module from a checked plan, so a failure here means the row was
                // edited by hand.
                depends_on: depends_on
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<i64>>(json).ok())
                    .unwrap_or_default(),
                // And here the same unreadable list errs the OTHER way, because the direction that
                // is safe is the other one. An item believed to declare nothing clashes with
                // nobody and is started beside anything, so two of them could write one file at
                // once. Nothing to be done about it from here — an empty list is also what an
                // honest job without a team has — which is why `parse_plan` refuses an empty
                // `files` from a director instead, before the row is ever written.
                files: files
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
                    .unwrap_or_default(),
            })
            .collect(),
        review,
        rounds: RoundState {
            round,
            // NULL means "nobody asked for rounds", which is one round and the behaviour of every
            // job written before this column existed. Resolving it to the daemon ceiling instead
            // would switch rounds on for every `graph:` rule already scheduled, silently.
            max_rounds: max_rounds.unwrap_or(1).max(1),
            dry_rounds,
            replanning: latest_replan
                .as_ref()
                .is_some_and(|(_, status)| node_in_flight(status)),
            round_added_nothing: queued_this_round == 0,
            replanned: if replan_done != 0 {
                Replan::Done
            } else {
                Replan::NotYet
            },
            a_broken_item_may_go_round_again: commissioned_with_an_allowance(
                rule_name.as_deref(),
                budget_usd,
            ),
        },
        has_team: team_id.is_some(),
        max_parallel: max_parallel.max(1) as usize,
    })
}

/// A job that stopped early because an allowance ran out, not because the work went wrong.
///
/// Two values rather than one, because they call for opposite responses. `expired` means the job
/// hit the four-hour ceiling and the rest of its queue is still worth doing. `stopped` means the
/// budget window is spent, and starting the same job again tonight would stop in the same place.
pub const STATUS_EXPIRED: &str = "expired";
pub const STATUS_STOPPED: &str = "stopped";
/// A job whose daemon died under it, and whose repository has moved on since.
pub const STATUS_INTERRUPTED: &str = "interrupted";
/// A job somebody stopped, at either level: one node, or the whole chain.
pub const STATUS_CANCELLED: &str = "cancelled";
/// An ITEM the job put down because it asked for a decision. Deliberately not in
/// [`TERMINAL_STATUSES`] below: that list is job endings, and a skipped item ends nothing — the job
/// carries on to the next one, which is the whole point of it.
pub const STATUS_SKIPPED: &str = "skipped";

/// An unfinished item a later round's replan said it takes over. Terminal, and not a failure: the
/// item that replaced it is judged in its place, which is what lets a job that went round again
/// after a failure end as something other than `failed`. Measured on job 26, 2026-09-13: its round-0
/// item failed, round 1 redid the same work and passed the gate, and the job was still reported
/// `failed` because nothing told the ending that the failure had been answered.
pub const STATUS_SUPERSEDED: &str = "superseded";

/// Every ending this module can write.
///
/// Named in one place because something else has to agree with it: `gc_candidates` collects a job's
/// worktree only for a status it lists, so an ending missing from there leaks a directory forever —
/// invisibly, because as far as the system is concerned that job is finished and its tree is
/// nobody's. `every_ending_a_job_can_have_is_an_ending_the_gc_collects` holds the two lists
/// together, and it was written because adding `stopped` had already opened exactly that leak.
pub const TERMINAL_STATUSES: [&str; 8] = [
    "completed",
    "failed",
    "gate_failed",
    "gate_errored",
    STATUS_EXPIRED,
    STATUS_STOPPED,
    STATUS_INTERRUPTED,
    STATUS_CANCELLED,
];

/// Writes a job's terminal status and stamps it done.
///
/// Clears `wait_reason` on the way out: a finished job is not waiting for anything, and a stale
/// reason left on the row is the sort of thing a feed renders forever.
///
/// Closes the job's live working findings and gives the concurrency slot back too. This is the one
/// place that does both for jobs — every ending funnels here, from `finish` to `cancel` to the
/// startup reconciliation, so a new ending added later cannot forget. The sweeps are backstops, not
/// the mechanism: they repair what a crash left behind rather than what this function forgot.
///
/// **Compare-and-set: only a live job is retired, and the answer says whether this call did it.**
/// A pass reads the job, then spends minutes in a gate or a node launch before it writes; a cancel
/// landing in between used to be overwritten by whatever that pass wrote next — `gate_failed`
/// over `cancelled`, or worse, `implementing` with the next item started. The first ending is the
/// one that stands, and a `false` here tells the caller it is not the one who ended the job.
pub async fn retire(pool: &SqlitePool, job_id: i64, status: &str) -> sqlx::Result<bool> {
    if !TERMINAL_STATUSES.contains(&status) {
        // Written anyway. A job left live would hold the project's exclusivity slot forever and
        // take the whole project's autonomy down with it, which is worse than a worktree directory
        // the GC declines to collect. The warning is what makes the leak findable at all.
        tracing::warn!(
            job_id,
            status,
            "retiring a job into a status the worktree GC does not collect"
        );
    }
    let mut tx = pool.begin().await?;
    let retired = sqlx::query(concat!(
        "UPDATE jobs SET status = ?, completed_at = ?, wait_reason = NULL, resume_status = NULL
         WHERE id = ? AND status IN ",
        live_statuses_sql!()
    ))
    .bind(status)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(job_id)
    .execute(&mut *tx)
    .await?;
    // Queued in the retirement's own transaction, but best effort: a queueing failure is logged
    // and never fails or rolls back the retirement.
    if retired.rows_affected() > 0
        && status != "completed"
        && let Err(error) = crate::distill::enqueue_job_ending_in(&mut tx, job_id).await
    {
        tracing::warn!(job_id, %error, "distill: could not queue a job ending");
    }
    tx.commit().await?;
    // Closed and released whichever way the write went. A job that was already over has no live
    // findings and holds no slot, so both are no-ops for it — and a backstop if something left one
    // behind.
    if let Err(error) = crate::knowledge::close_working_for_job(pool, job_id).await {
        // A job left live holds the project's exclusivity slot forever, which is worse than a
        // finding the hourly retention net will close later. Retirement therefore continues.
        tracing::warn!(job_id, %error, "job: failed to close working findings");
    }
    crate::execution::concurrency::release(pool, crate::execution::worktree::Owner::Job(job_id))
        .await?;
    Ok(retired.rows_affected() > 0)
}

/// Records how a job ended.
pub async fn finish(pool: &SqlitePool, job_id: i64, outcome: Outcome) -> sqlx::Result<()> {
    retire(pool, job_id, outcome.as_status()).await.map(drop)
}

/// Moves a job to another live stage, but only while it is still live.
///
/// `false` means the job ended — cancelled, expired, retired by another path — between the read
/// that led here and this write. The caller must stop: writing the stage anyway is exactly what
/// used to bring a cancelled job back to `implementing` and start its next item.
pub(super) async fn set_live_status(
    pool: &SqlitePool,
    job_id: i64,
    status: &str,
) -> sqlx::Result<bool> {
    let moved = sqlx::query(concat!(
        "UPDATE jobs SET status = ? WHERE id = ? AND status IN ",
        live_statuses_sql!()
    ))
    .bind(status)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(moved.rows_affected() > 0)
}

/// Parks a job that could not start its next node, so it is retried rather than abandoned.
///
/// Decision 15 of the design: the exclusivity slot belongs to a *run*, so nobody holds it in the gap
/// between two of a job's nodes, and an ordinary scheduler tick can take it. Losing that race is not
/// a failure of the work — the worktree and everything gated green so far are untouched — so the job
/// waits rather than reporting a partial. The 4-hour ceiling is what stops waiting forever.
///
/// `reason` is stored because `waiting` now means two different things — a budget window that will
/// reopen, and a slot another run is holding — and they call for opposite responses from a reader.
pub async fn wait(pool: &SqlitePool, job_id: i64, reason: &str) -> sqlx::Result<()> {
    pause(pool, job_id, "waiting", reason).await
}

/// Puts a pause in front of a job's stage, keeping the stage to come back to.
///
/// The CASE is what makes this safe to call on an already-paused job — every pass re-applies the
/// pause while the reason still holds. Without it, the second call would record `waiting` as the
/// stage to return to and the job would never find its way home.
pub async fn pause(pool: &SqlitePool, job_id: i64, status: &str, reason: &str) -> sqlx::Result<()> {
    // Live jobs only, for `retire`'s reason: parking a job that was cancelled meanwhile would make
    // it live again, with `cancelled` filed as the stage to return to.
    sqlx::query(concat!(
        "UPDATE jobs
         SET resume_status = CASE WHEN status NOT IN ('waiting','awaiting_approval')
                                  THEN status ELSE resume_status END,
             status = ?,
             wait_reason = ?
         WHERE id = ? AND status IN ",
        live_statuses_sql!()
    ))
    .bind(status)
    .bind(reason)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Unparks a job, returning it to the stage it was at.
///
/// Called before the brakes are re-read rather than by whichever brake lifted: the job does not
/// have to remember which one stopped it, and a second brake that came on meanwhile parks it again
/// on the same pass. `wait_reason` is therefore a note for the reader, never a latch.
///
/// Falls back to `planning`, not to `implementing`, if the stage was somehow lost. Re-planning
/// costs a run; assuming a queue exists when it does not reports work as complete that was never
/// started, which is the failure nobody sees.
///
/// Clears `wait_reason` with the status, for the reason `retire` does: a job that is no longer
/// waiting is not waiting for anything, and the note outlives the pause it explains. Left behind, a
/// job running normally reads `implementing / budget` — which names a brake that lifted hours ago
/// and is the one thing a reader would act on. `park` is unaffected either way: its "say it once"
/// test is `reason changed OR status is not waiting`, and a resumed job fails the second half.
pub async fn resume(pool: &SqlitePool, job_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE jobs
         SET status = COALESCE(resume_status, 'planning'), resume_status = NULL,
             wait_reason = NULL
         WHERE id = ? AND status IN ('waiting','awaiting_approval')",
    )
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// What a caller asks for when it starts a job.
///
/// The shape is copied onto the row rather than re-read per node: the project's `autopilot.yaml` can
/// be
/// edited mid-flight, and a job that changed shape between its own nodes would gate some items and
/// not others with nothing recording why.
pub struct NewJob<'a> {
    pub project_id: &'a str,
    pub project_root: &'a str,
    /// `None` when no rule asked for this job — a person did, through the shell or through the
    /// Telegram assistant. The column has been nullable since migration 0042, so this is the type
    /// catching up with the schema rather than a widening: a sentinel name would invent a rule that
    /// does not exist and that nothing could ever look up.
    pub rule_name: Option<&'a str>,
    pub prompt: &'a str,
    pub max_items: i64,
    pub gate_each: bool,
    pub review: bool,
    /// How many EXTRA implement runs each item may buy with a red gate. Beside `max_items`,
    /// `gate_each` and `review` because it is the same kind of thing: the shape the rule asked for,
    /// copied onto the job when it starts rather than re-read per node.
    ///
    /// Arrives already cut by `GraphConfig::gate_retries()`. The raw config field is private and
    /// must never reach here — a budget that travelled uncut would make the ceiling decorative at
    /// the one point where it is the only thing standing between a per-developer file and an
    /// unbounded number of re-implements.
    pub gate_retries: i64,
    /// The repository HEAD the job starts from. `None` when git would not answer, which crash
    /// recovery reads as "cannot prove the tree stayed put" and therefore as not resumable.
    pub head_sha: Option<&'a str>,
    /// How many rounds the caller asked for, uncut — `insert_job` applies `rounds_allowed`. `None`
    /// is one round, which is every job a `graph:` rule starts.
    pub max_rounds: Option<i64>,
    /// What the job may spend on itself. `None` leaves only the house limit.
    pub budget_usd: Option<f64>,
    /// The team directing it, already checked to exist. `None` is the job of today.
    pub team_id: Option<&'a str>,
}

/// Starts a job and returns its id.
///
/// Always `planning`: a job's first act is to plan, and a caller that could choose the starting
/// status could start one mid-queue with no queue.
///
/// Refuses nothing, and it used to. A project's second live job was turned away here by the unique
/// index `one_live_job_per_project`, which came out in migration 0053; a project may now have as
/// many live jobs as it has slots. The lock did not leave the storage layer, only moved table:
/// `start` claims a `project_slots` row, and that INSERT is what a scheduler tick and a manual
/// request racing for the same project resolve on.
pub async fn insert_job(pool: &SqlitePool, job: &NewJob<'_>) -> sqlx::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO jobs
           (project_id, project_root, rule_name, prompt, status, max_items, gate_each, review,
            gate_retries, head_sha, max_rounds, budget_usd, created_at, team_id)
         VALUES (?, ?, ?, ?, 'planning', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job.project_id)
    .bind(job.project_root)
    .bind(job.rule_name)
    .bind(job.prompt)
    .bind(job.max_items)
    .bind(i64::from(job.gate_each))
    .bind(i64::from(job.review))
    .bind(job.gate_retries)
    .bind(job.head_sha)
    // Cut HERE, at the write, and not where it is read. A ceiling applied at read time is one a
    // forgetful caller walks past; stored already cut, the row itself is the promise.
    .bind(crate::config::rounds_allowed(job.max_rounds))
    .bind(job.budget_usd)
    .bind(chrono::Utc::now().to_rfc3339())
    // Checked by `start` before this is called, because `foreign_keys` is on and an id that names
    // nothing arrives here as a raw constraint failure — which the caller turns into "the job was
    // created and could not be provisioned", a sentence that is wrong in both halves.
    .bind(job.team_id)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// The statuses that mean a job is live, and therefore the ones a tick has to drive.
///
/// Kept beside the SQL that reads it rather than spelled out at each call site, because a status
/// that falls out of this list stops being ticked while still holding a concurrency slot — the
/// sweep spares exactly these statuses too — so the project goes quiet with no error anywhere,
/// until somebody opens the database. Three tests pin the four copies together:
/// `a_live_status_is_a_status_some_pass_would_load` and `the_live_listing_names_every_live_status`
/// here, and `every_live_status_is_a_status_the_sweep_spares` in `concurrency.rs`.
pub const LIVE_STATUSES: [&str; 6] = [
    "planning",
    "implementing",
    "gating",
    "reviewing",
    "awaiting_approval",
    "waiting",
];

/// The `job_items.status` values that mean an item may still run, and therefore that anything it
/// holds — a worktree, a concurrency slot — has to be left alone.
///
/// The sweep in `concurrency.rs` reads these words out of a SQL literal — sqlx refuses SQL built at
/// run time — so this is the list that literal is held against, by
/// `every_unfinished_state_is_a_status_the_sweep_spares` here and
/// `every_live_status_is_a_status_the_sweep_spares` there. Test-only for that reason: it is the
/// second copy that makes the first one checkable, the same shape `EVERY_ITEM_STATE` has.
///
/// `gate_failed` is in the list although it is sometimes over, and that asymmetry is deliberate. It
/// means `GateRetriable` or `GateFailed` depending on `gate_attempts` weighed against the job's
/// budget — arithmetic [`item_state_from`] owns — and restating it in SQL would be the same rule in
/// a second dialect. Sparing it costs a slot held until the job ends; getting it wrong the other way
/// deletes a tree out from under work that was going to continue in it.
#[cfg(any(test, feature = "testkit"))]
pub const LIVE_ITEM_STATUSES: [&str; 7] = [
    "pending",
    "running",
    "implemented",
    "merging",
    "conflicted",
    "reverted",
    "gate_failed",
];

/// A job's own row, as the executor needs it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct JobRow {
    pub id: i64,
    pub project_id: String,
    pub project_root: String,
    pub prompt: Option<String>,
    pub status: String,
    pub resume_status: Option<String>,
    pub wait_reason: Option<String>,
    pub max_items: i64,
    pub gate_each: i64,
    // `review` is deliberately absent: `load_view` reads it in the same query as the review node's
    // status, because the two are only ever meaningful together.
    pub head_sha: Option<String>,
    /// Which round this job is on. Here as well as in `JobView` because the two answer different
    /// questions: the view's copy drives the pure decision, this one is what a node's prompt and the
    /// feed lines say out loud, and neither should have to load the other.
    pub round: i64,
    /// This job's own allowance. `None` means only the house limit governs, which is what every
    /// `graph:` rule has always meant and keeps meaning.
    pub budget_usd: Option<f64>,
    /// The schedule that started this job, and `None` when a person asked for it directly.
    ///
    /// Read only through `commissioned_with_an_allowance`, which is where the rule it feeds lives.
    /// It is the same signal `hooks::unrecognized_policy_for` reads for the same distinction — see
    /// `NewJob::rule_name` for what the column has always meant.
    pub rule_name: Option<String>,
    pub created_at: String,
}

impl JobRow {
    pub(super) fn stage(&self) -> &str {
        effective_status(&self.status, self.resume_status.as_deref())
    }

    /// Whether the owner asked for this job **and** named what it may spend.
    ///
    /// Delegates rather than restating: `load_view` asks the same question of loose columns, and
    /// this module has been bitten three times by one rule written in two places.
    pub(super) fn commissioned_with_an_allowance(&self) -> bool {
        commissioned_with_an_allowance(self.rule_name.as_deref(), self.budget_usd)
    }
}

/// The `jobs` row `load_view` reads about rounds and about who asked, in `SELECT` order.
///
/// Named because clippy counts seven of them as a type worth naming, and it is right for a better
/// reason than the count: the tuple is positional, three of its members are `Option<i64>`, and
/// nothing but the order stops `max_rounds` being bound to `replan_run_id`. Keep it in step with
/// the query below — they are read together or not at all.
pub(super) type RoundColumns = (
    i64,
    i64,
    Option<i64>,
    i64,
    Option<i64>,
    Option<String>,
    Option<f64>,
);

/// PURE: whether the owner asked for this job **and** named what it may spend.
///
/// The one question three different limits ask — the house budget in `brakes`, the life ceiling in
/// `drive`, and whether a red gate may go round again in `close_the_round` — and it lives in one
/// function so they cannot answer it differently. All three were written to pace autonomy nobody
/// asked for, and all three were governing work somebody had commissioned; the fix is the same fix,
/// so the predicate must be the same predicate.
///
/// **Both halves, and the conjunction is the whole safety argument.** `rule_name.is_none()` alone
/// would exempt a job a person asked for and gave no number to, leaving nothing at all holding it.
/// `budget_usd.is_some()` alone would exempt a scheduled job that happens to carry an allowance,
/// which is exactly the proactive autonomy the house limits exist for. Only together do they mean:
/// a person decided this should happen, and said how far it may go.
pub(super) fn commissioned_with_an_allowance(
    rule_name: Option<&str>,
    budget_usd: Option<f64>,
) -> bool {
    rule_name.is_none() && budget_usd.is_some()
}

/// Spelled out rather than built from `LIVE_STATUSES`, because sqlx refuses SQL assembled at
/// runtime — a guard worth keeping. The test named above compares this text against both the
/// constant and the migration's index, so the three cannot drift apart in silence.
pub(super) const LIVE_JOBS_SQL: &str =
    "SELECT id, project_id, project_root, prompt, status, resume_status,
                                    wait_reason, max_items, gate_each, head_sha, round, budget_usd,
                                    rule_name, created_at
                             FROM jobs
                             WHERE status IN ('planning','implementing','gating','reviewing',
                                              'awaiting_approval','waiting')
                             ORDER BY id";

pub(super) const ONE_JOB_SQL: &str =
    "SELECT id, project_id, project_root, prompt, status, resume_status,
                                  wait_reason, max_items, gate_each, head_sha, round, budget_usd,
                                  rule_name, created_at
                           FROM jobs WHERE id = ?";

pub async fn live_jobs(pool: &SqlitePool) -> sqlx::Result<Vec<JobRow>> {
    sqlx::query_as(LIVE_JOBS_SQL).fetch_all(pool).await
}

pub(super) async fn load_job(pool: &SqlitePool, job_id: i64) -> sqlx::Result<JobRow> {
    sqlx::query_as(ONE_JOB_SQL)
        .bind(job_id)
        .fetch_one(pool)
        .await
}

/// What cancelling a job did.
#[derive(Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled,
    /// Already over. Reported rather than treated as success, because "I stopped it" and "it had
    /// already finished" are different answers to the question the user just asked.
    NotLive,
    NotFound,
}

/// Ends a node that is parked on an approval, which `finalize_termination` cannot.
///
/// That function needs a live handle and guards its write with `status = 'running'`, and a parked
/// node has neither: its task ended when it asked, and its row says `awaiting_approval`. So the loop
/// below used to hand it a run it could do nothing with, and the run stayed parked after the job
/// that owned it was gone, holding the project's exclusivity and blocking every later worktree run
/// of it until a person noticed. (Measured before migration 0053, when that exclusivity was an
/// index and one strand took the whole project down. It is a slot now, so the same leak narrows the
/// project instead of stopping it — quieter, and still wrong.)
///
/// Measured on 2026-08-08: job 12 was cancelled while its review node was parked, and job 13 came up
/// `waiting` with `wait_reason = slot` against a project whose only live job was itself. Nothing in
/// the feed said why, because from the project's side nothing had gone wrong.
///
/// The proposal is closed FIRST, and that order is the point rather than tidiness: while it is
/// pending, `/approve` is a working door into a node whose job is over — it would resume work
/// nobody is waiting for, in a worktree the job no longer owns. `release` also takes the run
/// terminal, so the call after it is for the other case: a node parked with no pending proposal
/// left to expire.
pub(super) async fn cancel_parked_node(pool: &SqlitePool, run_id: i64) -> sqlx::Result<()> {
    // Expired, not rejected (spec A D14): cancelling the JOB says nothing about the action the
    // node was asking about, and a rejection here is exactly the label that poisoned the first
    // backtest. Closed FIRST for the reason this function always closed it first: while it is
    // pending, `/approve` is a working door into a node whose job is over. Best effort, and the
    // release below is why that is acceptable — a stale door is the lesser harm next to a
    // project left blocked by a parked run.
    if let Err(error) = crate::proposals::expire_for_run(
        pool,
        run_id,
        "the job was cancelled before anybody answered",
    )
    .await
    {
        tracing::warn!(
            run_id,
            ?error,
            "could not expire the parked node's proposal"
        );
    }
    crate::execution::worktree::release(pool, run_id).await?;
    Ok(())
}

/// Stops a whole job: the node in flight, and the sequence behind it.
///
/// This is the second of the two cancellation levels. Cancelling a *run* stops one node, and the
/// chain then ends `cancelled` too — a stopped node leaves the tree holding edits no gate measured,
/// so the next item must not build on them. What this adds is stopping a job that has no node in
/// flight at all: one parked for budget, waiting for the slot, or between two nodes.
///
/// The node is cancelled BEFORE the job is retired, so a daemon that dies in between heals itself:
/// the next pass sees a cancelled node and reaches the same ending. Retiring first would leave a
/// live node in a job nothing drives.
pub async fn cancel(state: &AppState, job_id: i64) -> sqlx::Result<CancelOutcome> {
    let pool = &state.pool;
    let Some(job): Option<JobSummary> = sqlx::query_as(summary_query("WHERE jobs.id = ?"))
        .bind(job_id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(CancelOutcome::NotFound);
    };
    if !LIVE_STATUSES.contains(&job.status.as_str()) {
        return Ok(CancelOutcome::NotLive);
    }

    let in_flight: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, status FROM runs WHERE job_id = ? AND status IN ('running','awaiting_approval')",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    for (run_id, status) in in_flight {
        // Branched on the run's own status rather than on what `finalize_termination` returns,
        // because the two ends need different tools and the return value does not tell them apart.
        if status == "awaiting_approval" {
            cancel_parked_node(pool, run_id).await?;
        } else {
            crate::execution::runs::finalize_termination(state, run_id, STATUS_CANCELLED).await;
        }
    }

    // Only the item that was in someone's hands. The ones still `pending` were never started, and
    // marking them cancelled would claim the job got to them and turned back.
    sqlx::query("UPDATE job_items SET status = ? WHERE job_id = ? AND status = 'running'")
        .bind(STATUS_CANCELLED)
        .bind(job_id)
        .execute(pool)
        .await?;
    if !retire(pool, job_id, STATUS_CANCELLED).await? {
        // Another ending got there first, between the liveness read above and this write. That
        // ending stands, and the caller is told the job was already over rather than cancelled.
        return Ok(CancelOutcome::NotLive);
    }

    let _ = crate::feed::append(
        pool,
        Some(&job.project_id),
        "job_cancelled",
        &format!("job {job_id} was cancelled; what it finished is on its branch"),
        None,
        Some(&crate::feed::Subject::Job(job_id)),
    )
    .await;
    Ok(CancelOutcome::Cancelled)
}

/// Retires jobs a crash left behind, unless the repository is provably where they left it.
///
/// Decision 11. The discriminator is HEAD and cannot be liveness: by the time this runs,
/// `reconcile_orphaned_runs` has already marked every run `interrupted`, so "has no live run" is
/// true of every job — including the ones that died in the gap between two nodes, which are exactly
/// the recoverable ones.
///
/// A HEAD that cannot be read, or that was never recorded, retires the job. That direction costs a
/// resumable job; the other resumes a plan written against a tree that has since moved, which §9 of
/// the parent spec calls the riskiest execution in the system. What the job finished is on its
/// branch either way.
///
/// Runs AFTER the run reconciliations, for the same reason the worktree sweep does: a job whose
/// last node is still marked `running` would look busy and be left alone forever.
pub async fn reconcile_orphaned_jobs(pool: &SqlitePool) -> sqlx::Result<u64> {
    let mut retired = 0;
    for job in live_jobs(pool).await? {
        let head =
            crate::repo_trigger::current_branch_sha(Path::new(&job.project_root), "HEAD", false)
                .await;
        if let (Some(recorded), Some(current)) = (job.head_sha.as_deref(), head.as_deref())
            && recorded == current
        {
            continue;
        }
        retire(pool, job.id, STATUS_INTERRUPTED).await?;
        let _ = crate::feed::append(
            pool,
            Some(&job.project_id),
            "job_interrupted",
            &format!(
                "job {} did not survive a restart, and the repository has moved since it started",
                job.id
            ),
            None,
            Some(&crate::feed::Subject::Job(job.id)),
        )
        .await;
        retired += 1;
    }
    Ok(retired)
}
