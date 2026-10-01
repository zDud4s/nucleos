//! The half of the conflict resolver that starts one: it finds an escalated merge, stages the
//! conflict in a worktree of its own, and hands that to an agent.
//!
//! **It lives outside the queue's executor, and that is a finding rather than a filing decision.**
//! The executor holds a pool, not an `AppState`, so it cannot start runs at all — which turns out to
//! be exactly right: a resolution takes as long as an agent takes, and the queue's contract is that
//! a repository is held for the length of a git command. Escalating releases the repository; this
//! loop picks the conflict up afterwards, and every other project's merges carry on in between.
//!
//! The other half is `git_exec::verify_resolution`, which refuses a resolution that threw work away.
//! It landed first, deliberately: building the launcher before the check would have meant a window
//! in which an agent can publish a resolution nothing verified.

use crate::state::AppState;

/// How often the daemon looks for a conflict nobody has attempted.
///
/// Much slower than the queue's own 500ms, and the difference is what each tick costs. The queue's
/// poll is one index-covered query on an empty index, and a person is watching for the merge they
/// asked for. A tick here can mint an agent, and nobody is waiting on it in that way — the asker has
/// already been told, in the feed and on the row, that their merge did not happen. A minute's
/// latency on a conflict costs nothing next to a launch nobody wanted.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Looks for conflicts to resolve, forever. Spawned once by `main.rs`.
///
/// Four passes on one tick, and they are separate because they are about different rows at
/// different moments: one stops a resolution nobody needs any more, one starts a resolution, one
/// hands a finished resolution to the queue (`land_finished`), and one says what an
/// already-published one cost. Sharing a tick is all they share — the last runs even when the
/// others have been stopped, which is deliberate and argued at `record_discards`.
///
/// Stopping comes before starting, and the order is the point rather than a preference: a tick that
/// minted before it cancelled would be a tick that could spend on a project already at its ceiling
/// because of work that is about to be cancelled for being pointless.
pub async fn run_resolution_loop(state: AppState) {
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    let mut refused = std::collections::HashSet::new();
    loop {
        interval.tick().await;
        cancel_settled(&state).await;
        launch_once(&state).await;
        land_finished(&state.pool, &mut refused).await;
        record_discards(&state.pool).await;
    }
}

/// One escalated merge, as the launcher reads it.
#[derive(sqlx::FromRow)]
struct Candidate {
    id: i64,
    op: String,
    args: String,
    project_id: String,
    project_root: String,
    /// Git's own account of the conflict, which goes into the prompt. `None` once retention has
    /// cleared it (`vcs::prune`), and the resolution is still worth starting without it — the
    /// conflict is in the branches, not in the text.
    output_tail: Option<String>,
}

/// The oldest conflict nobody has attempted, or `None`.
///
/// **Five conditions and each excludes a different thing.** `resolution_run_id IS NULL` is the one
/// attempt for this row: a value there means an agent has already had it, whether it succeeded, gave
/// up, or crashed. `from_resolution = 0` is the loop brake: a landing that came out of a resolution
/// and conflicted anyway escalates to a person, because resolving it would produce another landing
/// that can conflict, one agent per turn, for ever. `op = 'merge'` is the vocabulary — every other
/// operation escalates for reasons an agent in a worktree cannot touch.
///
/// **The fourth is the same conflict arriving twice, and it was missing.** The one-attempt rule was
/// written per ROW, and a conflict is not a row: two people, or a person and a session's `--land`,
/// can each queue `X into Y`, and each escalation carried its own NULL. Measured in production —
/// request 30 (a human's) and request 31 (a session's) named the identical merge minutes apart, and
/// the loop minted an agent for each, so two runs sat in two worktrees resolving the same two
/// branches at once. That is the fleet migration 0081 says this brake exists to prevent, arriving by
/// the one route the per-row check does not see.
///
/// So a conflict is skipped while another resolution of the SAME operation is still live — `args` is
/// the stored JSON, so comparing it compares source and target exactly, and `LIVE_RUN_STATUSES` is
/// the same liveness the concurrency sweep derives slots from.
///
/// **Live, and not "ever attempted", and that distinction was itself corrected in production.** The
/// first version of this check blacklisted the branch pair for good, and the cost showed up within
/// minutes: `feat/frontend-ponte` had had an attempt, that attempt was cancelled without resolving
/// anything, the branch moved on and conflicted again — genuinely a new conflict — and the loop
/// refused it an agent for ever. A pair that conflicts again next week is not the same wall an agent
/// already failed at, and a resolver that stops helping the branches that live longest is one that
/// stops helping where it is most needed. Repetition against the SAME escalation is still refused,
/// by `resolution_run_id IS NULL` above; what this adds is only that two never run at once.
///
/// **The fifth is a conflict that has stopped being one, and it is the commonest case in practice.**
/// Somebody settles the merge another way — by hand, or with the target brought in first, or by the
/// resolution this loop minted landing under its own branch name — and the old escalated row stays
/// behind. It is terminal, so nothing tidies it, and to this loop it still reads as an unattempted
/// conflict. It was commoner still while the escalation itself advised the asker to merge the other
/// direction and ask again: that advice is gone (`git_exec::compute_merge` names the resolution
/// instead), and the row it leaves behind is not.
/// Watched three times on this repository in one evening: request 28 settled by 29, request 33
/// settled by 34, each leaving bait. Once it started an agent that resolved a conflict which had
/// already been settled another way, and that resolution's landing would have reopened what the
/// other one decided.
///
/// A later `succeeded` naming the same operation is what says so. Later by id, because a merge that
/// succeeded BEFORE this escalation is a different event entirely — the branches moved on and
/// conflicted afterwards, which is the ordinary way a conflict appears at all.
///
/// It does not cover the same thing happening while a resolution is already RUNNING; that one needs
/// a live run cancelled rather than a row skipped. It was named here as missing for exactly as long
/// as it took to be rediscovered with a bill attached — `cancel_settled` is now that case, and it
/// asks the settlement question in exactly these words so that a change to one is visibly a change
/// to both. What the two do not share is which runs count as live, and `settled_resolutions` argues
/// that difference where it is made: skipping is right for a resolution paused at
/// `awaiting_approval`, and stopping one is not.
///
/// Separated from `launch_once` so the filter can be tested against a pool alone. It is the part
/// that decides which conflicts a person never has to look at, and it should not need an agent
/// runner to prove.
async fn next_conflict(pool: &sqlx::SqlitePool) -> sqlx::Result<Option<Candidate>> {
    sqlx::query_as(
        "SELECT id, op, args, project_id, project_root, output_tail
           FROM vcs_requests AS conflict
          WHERE status = 'escalated'
            AND op = 'merge'
            AND resolution_run_id IS NULL
            AND from_resolution = 0
            AND NOT EXISTS (
                SELECT 1
                  FROM vcs_requests AS attempted
                  JOIN runs ON runs.id = attempted.resolution_run_id
                 WHERE attempted.project_id = conflict.project_id
                   AND attempted.args = conflict.args
                   AND runs.status IN ('running', 'awaiting_approval')
            )
            AND NOT EXISTS (
                SELECT 1 FROM vcs_requests AS settled
                 WHERE settled.project_id = conflict.project_id
                   AND settled.args = conflict.args
                   AND settled.status = 'succeeded'
                   AND settled.id > conflict.id
            )
          ORDER BY id
          LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

/// A running resolution whose conflict has stopped being one.
#[derive(sqlx::FromRow)]
struct Settled {
    request_id: i64,
    project_id: String,
    run_id: i64,
    /// The later `succeeded` request that settled the same operation — named in the feed, because
    /// "cancelled" without it reads as the daemon changing its mind.
    settled_id: i64,
}

/// Every running resolution whose conflict a later request has already settled.
///
/// **`running`, and not `next_conflict`'s pair of live statuses**, and the difference is not an
/// oversight in either place. That filter asks "is an attempt under way", and a resolution paused at
/// `awaiting_approval` is one, so it must not be handed to a second agent. This one asks "is there
/// something here to stop", and a paused run is not: `finalize_termination`'s status write is a
/// compare-and-set on `running`, so calling it for a paused run would abort the task and leave the
/// row saying `awaiting_approval` with its proposal still pending — stranded, holding its project's
/// slot, and reachable by nothing afterwards, because `reconcile_stranded_approvals` heals such a
/// row only at startup and only once its proposal has stopped being pending. That is a worse ending
/// than leaving it paused, where at least a person can still answer it. A paused agent is also not
/// spending anything, and spending is what this pass exists to stop.
///
/// Ending one properly means rejecting the proposal and closing the run in the same transaction,
/// which is `proposals`' to offer and is not offered yet.
///
/// Separated from `cancel_settled` for the reason `next_conflict` is separated from `launch_once`:
/// it is the part that decides which agents get stopped, and it should not need a run runner to
/// prove. The caller does the stopping.
async fn settled_resolutions(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<Settled>> {
    sqlx::query_as(
        "SELECT conflict.id AS request_id,
                conflict.project_id AS project_id,
                conflict.resolution_run_id AS run_id,
                (SELECT MIN(s.id)
                   FROM vcs_requests AS s
                  WHERE s.project_id = conflict.project_id
                    AND s.args = conflict.args
                    AND s.status = 'succeeded'
                    AND s.id > conflict.id) AS settled_id
           FROM vcs_requests AS conflict
           JOIN runs ON runs.id = conflict.resolution_run_id
          WHERE runs.status = 'running'
            AND EXISTS (
                SELECT 1
                  FROM vcs_requests AS s
                 WHERE s.project_id = conflict.project_id
                   AND s.args = conflict.args
                   AND s.status = 'succeeded'
                   AND s.id > conflict.id
            )
          ORDER BY conflict.id",
    )
    .fetch_all(pool)
    .await
}

/// Stops resolutions whose conflict somebody else has already settled.
///
/// **`next_conflict`'s fifth condition, for a run instead of a row, and it was named there as
/// missing.** That filter skips an escalation once a later request has succeeded on the same
/// operation, and its own comment says what it does not cover: "the same thing happening while a
/// resolution is already RUNNING; that one needs a live run cancelled rather than a row skipped".
/// This is that. A row skipped costs nothing; a run left alive costs an agent, a concurrency slot,
/// and money, for a conflict that no longer exists.
///
/// **Measured, and the bill is why this exists rather than staying a note.** Requests 79 and 80
/// escalated and each got a resolution. Both conflicts were then settled by hand and landed as
/// request 85 — so from that moment the two agents were working on a merge that had already
/// happened. Nothing stopped them. They were resumed once each the following morning, spent $3.01
/// between them, and escalated again as requests 87 and 88, naming their own resolution branches
/// against a master that had contained the answer for thirteen hours. One of them was watched
/// rediscovering, with a `grep` for duplicate shot numbers, a collision the hand resolution had
/// already fixed.
///
/// **`args` is the comparison, exactly as in `next_conflict`**: it is the stored JSON, so comparing
/// it compares source and target and nothing else. And `settled.id > conflict.id`, because a merge
/// that succeeded BEFORE this conflict is a different event — the branches moved on and conflicted
/// afterwards, which is the ordinary way a conflict comes to exist at all.
///
/// A run whose handle has gone answers `false` and is left alone rather than logged about every
/// minute: its row is stuck `running` with nothing to abort, which is `reconcile_orphaned_runs`'s
/// to fix at the next start and not this pass's to shout about.
async fn cancel_settled(state: &AppState) {
    let settled = match settled_resolutions(&state.pool).await {
        Ok(rows) => rows,
        // Best-effort like every other polling loop here: a failed poll is the next tick's problem.
        Err(error) => {
            tracing::warn!(%error, "resolver: could not look for resolutions to stop");
            return;
        }
    };

    for row in settled {
        if !crate::runs::finalize_termination(state, row.run_id, "cancelled").await {
            continue;
        }
        tracing::info!(
            vcs_request_id = row.request_id,
            run_id = row.run_id,
            settled_by = row.settled_id,
            "resolver: cancelled a resolution whose conflict was settled another way"
        );
        // Said where the person who asked for the merge is already looking, and it names the
        // request that settled it: a resolution that simply stops reads as the daemon giving up on
        // something, which is the opposite of what happened.
        let _ = crate::feed::append(
            &state.pool,
            Some(row.project_id.as_str()),
            "vcs_resolution_cancelled",
            &format!(
                "stopped resolving vcs request {}: request {} already settled the same merge",
                row.request_id, row.settled_id
            ),
            Some(row.run_id),
            Some(&crate::feed::Subject::Vcs(row.request_id)),
        )
        .await;
    }
}

/// Starts at most one resolution, and returns the run it started.
///
/// **One per tick, deliberately.** Each resolution occupies one of its project's concurrency slots,
/// so a burst of escalations launched together would have the first take the slot and the rest come
/// back `Busy` — and `Busy` is refused before the run row exists, so nothing is claimed and they
/// would simply try again next tick. Which is correct, and is also an argument for not asking: a
/// backlog drains one a minute, in id order, with each one's slot released before the next is asked
/// for.
async fn launch_once(state: &AppState) -> Option<i64> {
    // The emergency stop, and it has no exemptions — `runs::create_run` makes the same argument at
    // its own door. Fails closed: a switch that cannot be read stops runs rather than starting them.
    if crate::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return None;
    }
    // A resolution costs an agent, so it is spend, and spend is what the budget governs.
    //
    // **The WIP limit and the attention brake are deliberately NOT consulted here**, and the
    // difference is worth stating because they sit next to the budget in every other loop. Both pace
    // work the daemon is proposing to START — a scheduled rule, a repo trigger. A
    // resolution starts nothing: it finishes something already in flight, whose branch is written,
    // whose merge was asked for, and which is stuck until somebody clears the conflict. Deferring it
    // for a busy project would leave that work stranded precisely when the project is busy enough
    // for the conflict to matter.
    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::quota::permits_new_run(state, chrono::Utc::now()).await
    {
        tracing::info!(%reason, "budget exhausted; no conflict resolution started this tick");
        return None;
    }

    let candidate = match next_conflict(&state.pool).await {
        Ok(candidate) => candidate?,
        // Best-effort like every other polling loop here: a failed poll is the next tick's problem.
        Err(error) => {
            tracing::warn!(%error, "resolver: could not look for conflicts to resolve");
            return None;
        }
    };

    // `escalated` is reached by more than a conflict — `verify_resolution` refuses onto it too — and
    // the ones it refuses are already resolutions. Those are excluded by `from_resolution = 0`
    // above; what is left here is a row whose operation will not parse, which the queue itself
    // treats as terminal and unexecutable rather than as work.
    let (source, target) = match crate::vcs::Op::from_stored(&candidate.op, &candidate.args) {
        Ok(crate::vcs::Op::Merge { source, target }) => (source, target),
        Ok(other) => {
            tracing::warn!(
                vcs_request_id = candidate.id,
                op = other.kind(),
                "resolver: a row selected as a merge parsed as something else"
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(
                vcs_request_id = candidate.id,
                %error,
                "resolver: an escalated merge's stored operation could not be parsed"
            );
            return None;
        }
    };

    let prompt = resolution_prompt(
        source.as_str(),
        target.as_str(),
        candidate.output_tail.as_deref(),
    );
    let started = crate::runs::create_resolution_run(
        state,
        prompt,
        candidate.project_id.clone(),
        candidate.project_root.clone(),
        crate::runs::Resolution {
            request_id: candidate.id,
            source: source.as_str().to_owned(),
            target: target.as_str().to_owned(),
        },
    )
    .await;

    match started {
        Ok(run_id) => {
            tracing::info!(
                vcs_request_id = candidate.id,
                run_id,
                source = source.as_str(),
                target = target.as_str(),
                "resolver: started a conflict resolution"
            );
            // The feed is the only surface this pillar has, and a daemon that mints an agent on its
            // own has to say so where the person who asked for the merge is already looking.
            let _ = crate::feed::append(
                &state.pool,
                Some(candidate.project_id.as_str()),
                "vcs_resolution_started",
                &format!(
                    "resolving the conflict between {} and {} (vcs request {})",
                    source.as_str(),
                    target.as_str(),
                    candidate.id
                ),
                Some(run_id),
                Some(&crate::feed::Subject::Vcs(candidate.id)),
            )
            .await;
            Some(run_id)
        }
        // Nothing was claimed: `Busy` is refused before the run row exists, so the conflict keeps its
        // attempt and the next tick asks again. Not a warning — a busy project deferring its own
        // resolution is the documented consequence of charging a resolution a slot.
        Err(crate::runs::CreateRunError::Busy) => {
            tracing::info!(
                vcs_request_id = candidate.id,
                project_id = %candidate.project_id,
                "resolver: the project is full; its conflict waits for a slot"
            );
            None
        }
        // Not spent either, for the same reason: the refusal comes before any claim is written. Said
        // as the disk rather than as a full project, because the two are cleared by different hands.
        // Only items are asked about the disk today; this is here so the day a resolution is, the
        // log does not send somebody looking for a slot.
        Err(crate::runs::CreateRunError::NoRoomOnDisk(refusal)) => {
            tracing::info!(
                vcs_request_id = candidate.id,
                project_id = %candidate.project_id,
                %refusal,
                "resolver: the disk is too full for a checkout; its conflict waits"
            );
            None
        }
        // Everything else HAS spent the attempt, because the claim is written before the conflict is
        // staged — see `create_run_with`. `fail_provisioning` has already put the reason in the feed;
        // this is the log line that names the request it was about.
        Err(error) => {
            tracing::warn!(
                vcs_request_id = candidate.id,
                %error,
                "resolver: the conflict could not be handed to an agent"
            );
            None
        }
    }
}

/// One published resolution whose cost has not been worked out yet.
#[derive(sqlx::FromRow)]
struct Published {
    id: i64,
    project_id: String,
    project_root: String,
    /// The merge commit the queue put on the target branch. Everything the computation needs hangs
    /// off it by sha — `^2` is the resolution, `^2^2` is the branch it was asked to bring in — which
    /// is what lets this run after the worktree GC has deleted the resolution's branch by name.
    result_sha: String,
}

/// Works out what ONE published resolution left behind, and writes it on the row.
///
/// **Separate from the merge that published it, and not for tidiness.** The queue's contract is that
/// a repository is held for the length of a git command; this reads a `git show` per changed file,
/// which for a long-lived branch is hundreds of them. Doing it inside the claim would hold every
/// other operation on that repository behind an accounting pass nobody is waiting for.
///
/// **It runs even when the kill switch is engaged**, and that is the point rather than an oversight.
/// The switch stops the daemon STARTING work; this starts nothing — it reads commits that are already
/// in the repository and writes one row. The moment somebody pulls the emergency stop is also the
/// moment they most want to know what the last resolution cost.
///
/// A failure to work it out is written into the column rather than left NULL to be retried for ever.
/// One attempt, like the resolution itself: a repository that has moved away does not come back by
/// being asked every minute, and a column that always holds an answer — even "could not" — is one a
/// reader can act on.
///
/// Takes the pool and not the `AppState`, which is not tidiness: this pass starts nothing, so the
/// runner, the run handles and the kill switch are all things it has no business reaching. The
/// narrower argument is also what lets it be tested without an agent runner.
async fn record_discards(pool: &sqlx::SqlitePool) {
    let candidate: Option<Published> = match sqlx::query_as(
        "SELECT id, project_id, project_root, result_sha
           FROM vcs_requests
          WHERE from_resolution = 1
            AND status = 'succeeded'
            AND result_sha IS NOT NULL
            AND discarded IS NULL
          ORDER BY id
          LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    {
        Ok(candidate) => candidate,
        Err(error) => {
            tracing::warn!(%error, "resolver: could not look for resolutions to account for");
            return;
        }
    };
    let Some(published) = candidate else {
        return;
    };

    let deadline = std::time::Instant::now() + crate::git_exec::OPERATION_TIMEOUT;
    let record = match crate::git_exec::discarded_by_resolution(
        std::path::Path::new(&published.project_root),
        &published.result_sha,
        deadline,
    )
    .await
    {
        Ok(record) => record,
        Err(reason) => {
            tracing::warn!(
                vcs_request_id = published.id,
                %reason,
                "resolver: what a published resolution discarded could not be worked out"
            );
            format!("could not be worked out: {reason}")
        }
    };

    if let Err(error) = sqlx::query("UPDATE vcs_requests SET discarded = ? WHERE id = ?")
        .bind(&record)
        .bind(published.id)
        .execute(pool)
        .await
    {
        tracing::warn!(
            vcs_request_id = published.id,
            %error,
            "resolver: what a published resolution discarded could not be recorded"
        );
        return;
    }

    // **Only when something was actually lost.** A feed row IS a notification, and one saying
    // "nothing was lost" after every resolution trains its reader to skip the line that one day
    // says otherwise.
    if record.starts_with("nothing") {
        return;
    }
    let headline = record.lines().next().unwrap_or(&record);
    let _ = crate::feed::append(
        pool,
        Some(published.project_id.as_str()),
        "vcs_resolution_discarded",
        &format!(
            "vcs request {} published a resolution: {headline}",
            published.id
        ),
        None,
        Some(&crate::feed::Subject::Vcs(published.id)),
    )
    .await;
}

/// PURE: what the resolving agent is told.
///
/// **It describes a worktree that is already conflicted**, because it is — the daemon stages the
/// merge before the agent exists. So the instructions are "resolve and commit", never "merge", and
/// that is not a wording preference: a `git merge` from inside a session goes to the queue, and the
/// queue is what refused this merge for conflicting. An agent told to merge would circle.
///
/// The prohibitions are the ones that produce a green result nobody read. `-X ours` and its family
/// resolve every conflict instantly and correctly-looking, and half the work is gone; the reason
/// they are named individually is that "resolve it properly" does not stop an agent that is out of
/// ideas from reaching for one.
fn resolution_prompt(source: &str, target: &str, output_tail: Option<&str>) -> String {
    let mut prompt = format!(
        "You are resolving a merge conflict that the version-control queue met and could not carry.\n\
         \n\
         The conflict is ALREADY STAGED in this worktree. The tree was created on {target} and \
         {source} was merged into it half-way, so the conflicting files have git's markers in them \
         right now and MERGE_HEAD is set. `git status` lists them.\n\
         \n\
         What to do:\n\
         \n\
         1. Resolve every conflicted file. Read both sides and keep what each of them was doing — a \
         resolution's whole job is that neither side's work is lost.\n\
         2. `git add` what you resolved and `git commit`. Committing on top of what is staged \
         produces the merge commit by itself; you do not have to do anything special to get it.\n\
         3. Stop. You do not land anything: once this run has finished, the daemon hands your merge \
         commit to the queue itself, and the queue decides when it is published.\n\
         \n\
         Rules that are not negotiable:\n\
         \n\
         - Never `-X ours`, `-X theirs`, `git checkout --ours`, `git checkout --theirs`, or anything \
         else that picks a side without you having read it. A green result that chose a side on its \
         own is the failure this run exists to prevent, not a way out of it.\n\
         - Do not rebase, squash, amend the merge away, or reset. The commit must keep BOTH parents. \
         A tip with one parent is refused before anything is published, without its content being \
         read at all — because a flattened resolution is exactly what looks perfect.\n\
         - Do not merge, push, pull, or delete branches. Those go to the queue, and the queue is what \
         asked you for this.\n\
         - Never assign PATH, RUSTUP_HOME or CARGO_HOME, and do not go looking for tools. `cargo` and \
         `git` are already on PATH; an assignment to one of those variables is held for a person to \
         approve, and a resolution that stops there is one nobody gets the benefit of.\n\
         - Run ONE shell command at a time. No `&&` chains, no pipes, no `$(...)` — the classifier \
         holds compound shell for a person to approve, and a resolution that stops to be approved \
         for a `git log` is one nobody gets the benefit of. This costs you a few extra calls and \
         saves the whole run.\n\
         \n\
         If the two sides genuinely cannot be reconciled — contradictory intent, not merely awkward \
         — stop and say so instead of committing a guess. Leaving it for a person is a correct \
         ending for this run.\n"
    );
    if let Some(tail) = output_tail.map(str::trim).filter(|tail| !tail.is_empty()) {
        prompt.push_str(&format!(
            "\nGit's own account of the conflict, from when the queue met it:\n\n{tail}\n"
        ));
    }
    prompt
}

/// Whether the branch a worktree is asking to land was produced by a conflict resolver.
///
/// **It reads the run out of the BRANCH and then asks the database about that run**, and the two
/// steps are in that order because of a defect a live conflict found. It used to join `worktrees` to
/// `vcs_requests` on the tree's current owner, which is wrong the moment a resolution run pauses for
/// approval: the resume is a NEW run id and the `worktrees` row is rewritten to it, so the tree no
/// longer names the run the escalation recorded. Measured — a resolution whose inspection command was
/// held for approval landed with `from_resolution = 0`, was published without being verified, and was
/// never accounted for. The branch is opened once and never renamed, so it is the durable half.
///
/// Nothing is asked of the `worktrees` table at all now, which also means the answer survives the GC
/// collecting the tree: the question is what PRODUCED this branch, and that does not stop being true
/// when the directory goes away.
///
/// **The run the branch names is not always the run the escalation records.** A resolution that
/// paused for approval resumes as a successor (`runs.rs` moves `resolution_run_id` and the worktree
/// row to it) while the branch keeps the ORIGINAL run's name, so the lookup also matches the run
/// that currently owns the worktree on this branch. Without that arm every resumed resolution
/// landed unverified and unlinked.
///
/// Scoped to the project, or two projects' branch names would decide each other's.
pub(crate) async fn landing_is_a_resolution(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    branch: &str,
) -> bool {
    // A branch this daemon did not open cannot be a resolution's output, and every resolution's is
    // one it opened.
    let Some(run) = crate::worktree::run_behind_branch(branch) else {
        return false;
    };
    let found: sqlx::Result<bool> = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM vcs_requests
              WHERE project_id = ? AND (resolution_run_id = ? OR resolution_run_id IN (
                        SELECT owner_id FROM worktrees WHERE owner_kind = 'run' AND branch = ?))
         )",
    )
    .bind(project_id)
    .bind(run)
    .bind(branch)
    .fetch_one(pool)
    .await;
    match found {
        Ok(found) => found,
        // Fails toward verifying. A landing wrongly treated as a resolution is refused if its tip has
        // one parent, which costs an ordinary session one escalation it can answer; a resolution
        // wrongly treated as ordinary is published without anything having checked that it kept both
        // sides, which is the loss this whole mechanism exists to prevent.
        Err(error) => {
            tracing::warn!(
                project_id,
                branch,
                %error,
                "could not tell whether a landing came out of a conflict resolution; verifying it"
            );
            true
        }
    }
}

/// The escalated request a resolution's landing answers, if `branch` is one.
///
/// The same lookup `landing_is_a_resolution` makes, minus the boolean collapse — `land.rs` needs
/// the ROW, to link `vcs_requests.resolved_by` at the moment the resolution is admitted, so the
/// session still waiting on the original ticket follows the link instead of reading a terminal
/// `escalated` and stopping (design decision #5).
///
/// `None` on anything that keeps `landing_is_a_resolution` from answering `true` for the same
/// branch — a branch this daemon never opened, one whose run never escalated anything, or a
/// database read that failed. Fails toward NOT linking rather than guessing: a resolution admitted
/// without a link still lands and still tells `/wait` the truth eventually, once its own row goes
/// terminal and the caller polls it directly; a wrong link would point a person at the wrong row.
pub(crate) async fn escalated_request_id(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    branch: &str,
) -> Option<i64> {
    let run = crate::worktree::run_behind_branch(branch)?;
    let found: sqlx::Result<Option<i64>> = sqlx::query_scalar(
        "SELECT id FROM vcs_requests
          WHERE project_id = ? AND (resolution_run_id = ? OR resolution_run_id IN (
                    SELECT owner_id FROM worktrees WHERE owner_kind = 'run' AND branch = ?))
          ORDER BY id
          LIMIT 1",
    )
    .bind(project_id)
    .bind(run)
    .bind(branch)
    .fetch_optional(pool)
    .await;
    match found {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(
                project_id,
                branch,
                %error,
                "could not look up the escalation a resolution answers; landing it unlinked"
            );
            None
        }
    }
}

/// One finished resolution, as `land_finished` reads it.
#[derive(sqlx::FromRow)]
struct Finished {
    id: i64,
    op: String,
    args: String,
    project_id: String,
    worktree_path: String,
    branch: String,
    base_sha: Option<String>,
    completed_at: Option<String>,
    gate_status: Option<String>,
}

/// How long a completed run is left alone before its worktree is read, so a run that is about to
/// hand off to a successor (which then owns the tree) is not mistaken for a finished one.
const HANDOFF_GRACE: chrono::Duration = chrono::Duration::minutes(2);

/// Hands a finished conflict resolution to the queue, so the agent never has to.
///
/// The agent resolves and commits and stops; `nucleos-core --land` is held by the classifier on every
/// call and is not on its PATH anyway. This pass does what that command would have: `land::submit`
/// with the escalation's own target, which still routes the branch through `verify_resolution`
/// (`landing_is_a_resolution`) and links the escalation to the new row.
///
/// A resolution counts as finished when its run `completed` (not failed, not cancelled), has no
/// successor and ended more than `HANDOFF_GRACE` ago, and its worktree holds a committed merge: no
/// `MERGE_HEAD`, a second parent, and a HEAD that is not the base it was opened on. Idempotent from
/// existing rows alone: `resolved_by IS NULL` is the brake, and a refusal is remembered in memory
/// per (request, head) so it is reported once instead of every tick. Fails closed under the kill
/// switch.
async fn land_finished(
    pool: &sqlx::SqlitePool,
    refused: &mut std::collections::HashSet<(i64, String)>,
) {
    if crate::autopilot::kill_switch_engaged(pool)
        .await
        .unwrap_or(true)
    {
        return;
    }
    let finished: Vec<Finished> = match sqlx::query_as(
        "SELECT c.id, c.op, c.args, c.project_id, w.path AS worktree_path, w.branch,
                w.base_sha, r.completed_at, r.gate_status
           FROM vcs_requests AS c
           JOIN runs AS r ON r.id = c.resolution_run_id
           JOIN worktrees AS w
             ON w.owner_kind = 'run' AND w.owner_id = r.id AND w.removed_at IS NULL
          WHERE c.status = 'escalated'
            AND c.op = 'merge'
            AND c.from_resolution = 0
            AND c.resolved_by IS NULL
            AND r.status = 'completed'
            AND r.successor_run_id IS NULL
            AND NOT EXISTS (SELECT 1 FROM vcs_requests s
                             WHERE s.project_id = c.project_id AND s.args = c.args
                               AND s.status = 'succeeded' AND s.id > c.id)
          ORDER BY c.id",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "resolver: could not look for finished resolutions");
            return;
        }
    };

    for row in finished {
        let old_enough = row
            .completed_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .is_some_and(|at| chrono::Utc::now() - at.with_timezone(&chrono::Utc) > HANDOFF_GRACE);
        if !old_enough {
            continue;
        }
        let Some(head) = finished_merge_head(
            std::path::Path::new(&row.worktree_path),
            row.base_sha.as_deref(),
        )
        .await
        else {
            continue;
        };
        if refused.contains(&(row.id, head.clone())) {
            continue;
        }
        let target = match crate::vcs::Op::from_stored(&row.op, &row.args) {
            Ok(crate::vcs::Op::Merge { target, .. }) => target,
            _ => continue,
        };
        // The `resolved_by` link is written best-effort by `land::submit`; if it was lost, the
        // queue still holds the row this branch was admitted as. Fail toward not submitting.
        match branch_already_queued(pool, &row.project_id, &row.branch).await {
            Ok(false) => {}
            Ok(true) => continue,
            Err(error) => {
                tracing::warn!(
                    vcs_request_id = row.id,
                    %error,
                    "resolver: could not check whether a finished resolution is already queued"
                );
                continue;
            }
        }
        // `completed` says the agent stopped, not that the project's gate passed; the queue's land
        // builds nothing, so a resolution that broke the tests would be published unseen.
        match row.gate_status.as_deref() {
            None | Some("passed") => {}
            Some(gate) => {
                refused.insert((row.id, head));
                let summary = format!(
                    "{}'s conflict resolution on {} is merged but its run's gate {gate}; \
                     it was not handed to the queue and needs a person",
                    row.project_id, row.branch
                );
                if let Err(error) = crate::notify::deliver_or_defer(
                    pool,
                    crate::land::RESOLUTION_FAILED_KIND,
                    &summary,
                )
                .await
                {
                    tracing::warn!(
                        %error,
                        "resolver: could not notify about a resolution whose gate did not pass"
                    );
                }
                continue;
            }
        }
        let repo = match crate::vcs::resolve_repo(pool, &row.project_id).await {
            Ok(repo) => repo,
            Err(error) => {
                tracing::warn!(
                    vcs_request_id = row.id,
                    ?error,
                    "resolver: could not resolve the repository of a finished resolution"
                );
                continue;
            }
        };
        let deadline = std::time::Instant::now() + crate::git_exec::OPERATION_TIMEOUT;
        let project_root = std::path::PathBuf::from(repo.root());
        let outcome = crate::land::submit(
            pool,
            &repo,
            &project_root,
            &row.branch,
            Some(target.as_str()),
            deadline,
        )
        .await;
        match outcome {
            Ok(landing) => tracing::info!(
                vcs_request_id = row.id,
                landing,
                "resolver: handed a finished resolution to the queue"
            ),
            Err(refusal) => {
                refused.insert((row.id, head));
                // `NotAdmitted` was already announced by `land::submit` itself.
                if !matches!(refusal, crate::land::LandRefusal::NotAdmitted(_)) {
                    let summary = format!(
                        "{}'s conflict resolution on {} was not handed to the queue: {}",
                        row.project_id,
                        row.branch,
                        refusal.message()
                    );
                    if let Err(error) = crate::notify::deliver_or_defer(
                        pool,
                        crate::land::RESOLUTION_FAILED_KIND,
                        &summary,
                    )
                    .await
                    {
                        tracing::warn!(%error, "resolver: could not notify about a refused resolution");
                    }
                }
            }
        }
    }
}

/// Whether the project's queue already holds a merge whose source is `branch`, in any status.
///
/// `args` is JSON, so the substring filter only narrows the rows; the parse decides.
async fn branch_already_queued(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<bool> {
    let stored: Vec<String> = sqlx::query_scalar(
        "SELECT args FROM vcs_requests
          WHERE project_id = ? AND op = 'merge' AND instr(args, ?) > 0",
    )
    .bind(project_id)
    .bind(branch)
    .fetch_all(pool)
    .await?;
    Ok(stored.iter().any(|args| {
        matches!(
            crate::vcs::Op::from_stored("merge", args),
            Ok(crate::vcs::Op::Merge { source, .. }) if source.as_str() == branch
        )
    }))
}

/// The HEAD of a worktree that holds a finished merge: no `MERGE_HEAD`, a second parent, and not
/// the base the tree was opened on. `None` for anything else, including a git that would not run.
async fn finished_merge_head(path: &std::path::Path, base_sha: Option<&str>) -> Option<String> {
    async fn rev_parse(path: &std::path::Path, revision: &str) -> Option<String> {
        let output = crate::worktree::git()
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "-q", "--verify", revision])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
    if rev_parse(path, "MERGE_HEAD").await.is_some() {
        return None;
    }
    rev_parse(path, "HEAD^2").await?;
    let head = rev_parse(path, "HEAD").await?;
    if base_sha == Some(head.as_str()) {
        return None;
    }
    Some(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn escalated_merge(pool: &sqlx::SqlitePool, from_resolution: bool) -> i64 {
        escalated_merge_of(pool, "feat/x", from_resolution).await
    }

    /// A run row for the deduplication to read liveness off. Minimal on purpose: what the filter
    /// asks about a resolution's run is its status and nothing else.
    async fn insert_run(pool: &sqlx::SqlitePool, id: i64, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, prompt, status, mode, created_at)
             VALUES (?, 'proj', 'resolve it', ?, 'worktree', ?)",
        )
        .bind(id)
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .expect("insert the resolution's run");
    }

    /// An escalated merge, admitted through the real INSERT so the stored operation is the one the
    /// launcher will actually have to parse back.
    ///
    /// The source is a parameter because the attempt is claimed against the OPERATION: rows that
    /// name the same two branches are the same conflict, so a test about the per-row exclusions has
    /// to give each row a conflict of its own or it is testing deduplication by accident.
    async fn escalated_merge_of(
        pool: &sqlx::SqlitePool,
        source: &str,
        from_resolution: bool,
    ) -> i64 {
        let repo = crate::vcs::ResolvedRepo::synthetic("proj", "C:/repo", "proj");
        let op = crate::vcs::Op::Merge {
            source: source.into(),
            target: "master".into(),
        };
        let id = if from_resolution {
            crate::vcs::submit_resolution(pool, &repo, &op, crate::vcs::Origin::Shell).await
        } else {
            crate::vcs::submit(pool, &repo, &op, crate::vcs::Origin::Shell).await
        }
        .expect("admit the merge");
        sqlx::query("UPDATE vcs_requests SET status = 'escalated' WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await
            .expect("escalate it");
        id
    }

    /// Links an escalation to the run resolving it, which is what `launch_once` does the moment it
    /// mints one. The column is the only thing that says a resolution is under way at all.
    async fn resolving(pool: &sqlx::SqlitePool, request: i64, run: i64) {
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = ? WHERE id = ?")
            .bind(run)
            .bind(request)
            .execute(pool)
            .await
            .expect("link the conflict to its resolution");
    }

    /// The same merge, published — by whoever got there. Admitted through the real INSERT like every
    /// other row here, then given the ending it is being tested for.
    async fn succeeded_merge_of(
        pool: &sqlx::SqlitePool,
        source: &str,
        from_resolution: bool,
    ) -> i64 {
        let id = escalated_merge_of(pool, source, from_resolution).await;
        sqlx::query(
            "UPDATE vcs_requests SET status = 'succeeded', result_sha = 'abc' WHERE id = ?",
        )
        .bind(id)
        .execute(pool)
        .await
        .expect("publish it");
        id
    }

    /// The three exclusions, each of which prevents a different runaway.
    #[tokio::test]
    async fn only_a_conflict_that_nobody_has_attempted_is_picked_up() {
        let pool = test_pool().await;

        // Each row is a DIFFERENT conflict, or this would be testing the deduplication below rather
        // than the three per-row exclusions it is about.
        let attempted = escalated_merge_of(&pool, "feat/attempted", false).await;
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 9 WHERE id = ?")
            .bind(attempted)
            .execute(&pool)
            .await
            .unwrap();
        // A landing that came OUT of a resolution and conflicted anyway. Resolving it would produce
        // another landing that can conflict, one agent per turn, for ever. It goes to a person.
        let looped = escalated_merge_of(&pool, "feat/looped", true).await;
        // Escalated, but not a merge: nothing an agent in a worktree can do about a push.
        let other = crate::vcs::submit(
            &pool,
            &crate::vcs::ResolvedRepo::synthetic("proj", "C:/repo", "proj"),
            &crate::vcs::Op::Push {
                remote: "origin".into(),
                branch: "master".into(),
            },
            crate::vcs::Origin::Shell,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE vcs_requests SET status = 'escalated' WHERE id = ?")
            .bind(other)
            .execute(&pool)
            .await
            .unwrap();

        let fresh = escalated_merge_of(&pool, "feat/fresh", false).await;

        let picked = next_conflict(&pool)
            .await
            .expect("the query should run")
            .expect("there is one conflict left for somebody to look at");
        assert_eq!(
            picked.id, fresh,
            "picked {} — attempted={attempted}, from a resolution={looped}, not a merge={other}",
            picked.id
        );

        // And once it too has been claimed, there is nothing left to start.
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 10 WHERE id = ?")
            .bind(fresh)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            next_conflict(&pool).await.unwrap().is_none(),
            "every conflict has been attempted; a fresh agent per tick is the failure this prevents"
        );
    }

    /// **The fleet, arriving by the one route the per-row brake does not see.** A person queues
    /// `X into Y`, it conflicts; a session's `--land` queues the same merge minutes later, and it
    /// conflicts identically. Two escalations, each with its own NULL, each eligible — so two agents
    /// end up in two worktrees resolving the same two branches at once.
    ///
    /// Measured in production before this test existed: requests 30 and 31 on `nucleos`, runs 900318
    /// and 900319, both live. The attempt has to be claimed against the conflict, not the row.
    ///
    /// And the release at the end is the second correction: blacklisting the pair for good stopped
    /// the resolver helping a branch that had had one cancelled attempt and then conflicted again for
    /// a different reason. Two at once is the harm; having tried once is not.
    #[tokio::test]
    async fn the_same_conflict_queued_twice_gets_one_agent_and_not_two() {
        let pool = test_pool().await;
        let first = escalated_merge(&pool, false).await;
        let again = escalated_merge(&pool, false).await;

        let picked = next_conflict(&pool)
            .await
            .unwrap()
            .expect("the first of the two is work");
        assert_eq!(picked.id, first);

        // The launcher claims it, exactly as `create_run_with` does, and the run it names is live.
        insert_run(&pool, 11, "running").await;
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 11 WHERE id = ?")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            next_conflict(&pool).await.unwrap().is_none(),
            "the duplicate names the same merge in the same project — request {again} must not mint \
             a second agent against branches another one is already editing"
        );

        // The attempt ends without resolving anything. The branch pair is not blacklisted by having
        // been tried: a conflict that is still there, or a new one on the same two branches, is work.
        sqlx::query("UPDATE runs SET status = 'cancelled' WHERE id = 11")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            next_conflict(&pool).await.unwrap().map(|it| it.id),
            Some(again),
            "once nothing is live on this pair, the escalation nobody has attempted is work again"
        );
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 12 WHERE id = ?")
            .bind(again)
            .execute(&pool)
            .await
            .unwrap();

        // A different merge in the same project is untouched by the deduplication: it is a different
        // conflict, and nobody has looked at it.
        let elsewhere = crate::vcs::submit(
            &pool,
            &crate::vcs::ResolvedRepo::synthetic("proj", "C:/repo", "proj"),
            &crate::vcs::Op::Merge {
                source: "feat/other".into(),
                target: "master".into(),
            },
            crate::vcs::Origin::Shell,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE vcs_requests SET status = 'escalated' WHERE id = ?")
            .bind(elsewhere)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            next_conflict(&pool).await.unwrap().map(|it| it.id),
            Some(elsewhere),
            "deduplication is per conflict, not a stop on the whole project"
        );
    }

    /// **The conflict that stopped being one.** Somebody lands the merge another way and the
    /// escalated row stays behind: terminal, so nothing tidies it, and to the loop it still reads as
    /// work nobody has looked at.
    ///
    /// Watched three times in one evening on this repository. Once it minted an agent that resolved a
    /// conflict already settled by another route, and that resolution's landing would have reopened
    /// what the other one decided.
    #[tokio::test]
    async fn a_conflict_that_somebody_else_already_settled_is_not_work() {
        let pool = test_pool().await;
        let escalated = escalated_merge(&pool, false).await;
        assert_eq!(
            next_conflict(&pool).await.unwrap().map(|it| it.id),
            Some(escalated),
            "until the merge lands some other way, it is a conflict like any other"
        );

        // Somebody settles it another way — by hand, or with the target brought in first — and asks
        // again. This time it goes through.
        let settled = escalated_merge(&pool, false).await;
        sqlx::query(
            "UPDATE vcs_requests SET status = 'succeeded', result_sha = 'abc' WHERE id = ?",
        )
        .bind(settled)
        .execute(&pool)
        .await
        .unwrap();

        assert!(
            next_conflict(&pool).await.unwrap().is_none(),
            "request {settled} published this merge, so {escalated} is a conflict that no longer \
             exists — an agent started on it resolves what somebody has already decided"
        );
    }

    /// **`next_conflict`'s fifth condition, asked about a run instead of a row.** A row skipped costs
    /// nothing; an agent left working on a merge that has already happened costs a concurrency slot,
    /// an hour and money — $3.01 across requests 87 and 88, over a conflict that had been settled
    /// the previous evening.
    #[tokio::test]
    async fn a_live_resolution_whose_conflict_was_settled_elsewhere_is_named_for_stopping() {
        let pool = test_pool().await;
        let conflict = escalated_merge_of(&pool, "feat/settled", false).await;
        insert_run(&pool, 41, "running").await;
        resolving(&pool, conflict, 41).await;

        assert!(
            settled_resolutions(&pool)
                .await
                .expect("the query should run")
                .is_empty(),
            "until the merge is published the agent is doing the only thing that will publish it"
        );

        let settled = succeeded_merge_of(&pool, "feat/settled", false).await;

        let named = settled_resolutions(&pool)
            .await
            .expect("the query should run");
        assert_eq!(
            named
                .iter()
                .map(|it| (it.request_id, it.run_id, it.settled_id))
                .collect::<Vec<_>>(),
            vec![(conflict, 41, settled)],
            // The third of the three is not bookkeeping: it is what the feed line says. A
            // resolution that simply stops reads as the daemon giving up on the conflict, which is
            // the opposite of what happened to it.
            "the row, the run to stop, and the request that settled it"
        );
    }

    /// The four ways a live resolution is left alone. Each of them, got wrong, kills an agent that
    /// is doing the work.
    #[tokio::test]
    async fn a_resolution_is_stopped_only_by_a_later_settlement_of_its_own_conflict() {
        let pool = test_pool().await;

        // One: another merge succeeding says nothing about this one. `args` is the stored JSON, so
        // comparing it compares source and target and nothing else.
        let other_pair = escalated_merge_of(&pool, "feat/other-pair", false).await;
        insert_run(&pool, 51, "running").await;
        resolving(&pool, other_pair, 51).await;
        succeeded_merge_of(&pool, "feat/somebody-else", false).await;

        // Two: a merge that succeeded EARLIER is a different event. The branches moved on and
        // conflicted afterwards, which is the ordinary way a conflict comes to exist at all — and
        // an agent stopped by it would be stopped by the very landing that caused its conflict.
        succeeded_merge_of(&pool, "feat/again", false).await;
        let again = escalated_merge_of(&pool, "feat/again", false).await;
        insert_run(&pool, 52, "running").await;
        resolving(&pool, again, 52).await;

        // Three: the run has already ended, so there is nothing to stop and nothing to say about
        // it. Without this the pass would try to cancel every resolution that ever finished, every
        // minute, for as long as the row exists.
        let finished = escalated_merge_of(&pool, "feat/finished", false).await;
        insert_run(&pool, 53, "completed").await;
        resolving(&pool, finished, 53).await;
        succeeded_merge_of(&pool, "feat/finished", false).await;

        // Four: **a resolution's own landing must not stop the resolution.** It publishes the branch
        // it worked in — `nucleos/run-54`, not the source it merged — so the operation it settles is
        // not the one it was minted for, and comparing `args` is what keeps those two apart. Pinned
        // because a comparison loosened to the project would cancel every resolution at the moment
        // it succeeded, which looks like the daemon killing its own work.
        let live = escalated_merge_of(&pool, "feat/live", false).await;
        insert_run(&pool, 54, "running").await;
        resolving(&pool, live, 54).await;
        succeeded_merge_of(&pool, "nucleos/run-54", true).await;

        // Five: a resolution paused for approval is left alone, and this one is a boundary rather
        // than an exclusion. `finalize_termination` writes its status only over `running`, so
        // stopping a paused run here would abort its task and leave the row `awaiting_approval`
        // with a pending proposal — holding a slot, healed by nothing until a startup pass that
        // wants the proposal gone first. Paused, it costs nothing and a person can still answer it.
        let paused = escalated_merge_of(&pool, "feat/paused", false).await;
        insert_run(&pool, 55, "awaiting_approval").await;
        resolving(&pool, paused, 55).await;
        succeeded_merge_of(&pool, "feat/paused", false).await;

        let named: Vec<i64> = settled_resolutions(&pool)
            .await
            .expect("the query should run")
            .iter()
            .map(|it| it.request_id)
            .collect();
        assert!(
            named.is_empty(),
            "nothing here is this pass's to stop — a different pair ({other_pair}), a success \
             that predates the conflict ({again}), a run that has already ended ({finished}), a \
             resolution's own landing ({live}) and a run paused for approval ({paused}) — yet \
             these were named: {named:?}"
        );
    }

    /// The branch a resolution produced is recognised by the run the ESCALATION recorded, read out of
    /// the branch name — never by whoever owns the worktree now.
    ///
    /// **The third assertion is the defect a live conflict found.** A resolution run that pauses for
    /// approval resumes under a new run id and the `worktrees` row is handed to the successor, so the
    /// old join — worktree's current owner against `resolution_run_id` — matched nothing. The landing
    /// was published unverified and never accounted for. The branch is the half that does not move.
    #[tokio::test]
    async fn a_landing_is_recognised_as_a_resolution_by_the_run_the_escalation_recorded() {
        let pool = test_pool().await;
        let request = escalated_merge(&pool, false).await;
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 7 WHERE id = ?")
            .bind(request)
            .execute(&pool)
            .await
            .unwrap();

        assert!(landing_is_a_resolution(&pool, "proj", "nucleos/run-7").await);
        assert!(
            !landing_is_a_resolution(&pool, "proj", "nucleos/run-8").await,
            "an ordinary run's landing must not be verified as a resolution — its tip has one \
             parent, like every branch, and it would be refused for it"
        );

        // The resume: the worktree is handed to run 9, which is what a resumed approval does. The
        // branch keeps naming 7, and that is what has to carry the answer.
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Run(9),
            "proj",
            "C:/repo",
            "C:/wt/run-7",
            "nucleos/run-7",
            None,
        )
        .await
        .unwrap();
        assert!(
            landing_is_a_resolution(&pool, "proj", "nucleos/run-7").await,
            "a resolution that paused for approval and resumed under a new id is still a resolution \
             — this is the one that shipped broken"
        );

        assert!(
            !landing_is_a_resolution(&pool, "other", "nucleos/run-7").await,
            "the answer is scoped to the project, or two projects' branch names decide each other's"
        );
        assert!(
            !landing_is_a_resolution(&pool, "proj", "feat/ordinary").await,
            "a branch this daemon never opened cannot be a resolution's output"
        );
    }

    /// The name is written in one place and read back in another, and they have to agree — a branch
    /// this daemon opens must be one it can recognise later.
    #[test]
    fn the_branch_a_worktree_gets_names_the_run_it_was_opened_for() {
        let branch = crate::worktree::Owner::Run(4242).branch_name();
        assert_eq!(crate::worktree::run_behind_branch(&branch), Some(4242));
        assert_eq!(
            crate::worktree::run_behind_branch(&crate::worktree::Owner::Job(4242).branch_name()),
            None,
            "a job's tree is not a run's, and reading one as the other would attribute a resolution \
             to a run id that belongs to a different sequence"
        );
    }

    /// The prompt is where the resolution's safety is spent or kept, so the words that carry it are
    /// pinned. `-X ours` and its family resolve every conflict instantly, produce a green tree, and
    /// throw half the work away — and "resolve it properly" does not stop an agent that has run out
    /// of ideas from reaching for one.
    #[test]
    fn the_prompt_names_the_ways_a_resolution_can_look_right_and_still_be_wrong() {
        let prompt = resolution_prompt("feat/x", "master", Some("CONFLICT (content): seed.txt"));

        for forbidden in ["-X ours", "-X theirs", "--ours", "--theirs"] {
            assert!(
                prompt.contains(forbidden),
                "a strategy that picks a side unread has to be named, not implied: {forbidden}"
            );
        }
        assert!(
            prompt.contains("BOTH parents"),
            "the agent should know why a flattened resolution is refused before it makes one"
        );
        assert!(
            prompt.contains("ALREADY STAGED"),
            "an agent told to merge would go to the queue that refused this merge, and circle"
        );
        // Measured, not guessed: three of the first four resolution runs stopped dead waiting for a
        // person to approve a READ — `$(git merge-base ...)`, `cd x && grep ... | head`. The
        // classifier is right to hold compound shell; the resolver is the one run that cannot afford
        // to be held, so it is told to spend the extra calls instead.
        assert!(
            prompt.contains("ONE shell command at a time"),
            "the resolver's autonomy is what compound shell costs, and the prompt is where that is \
             cheapest to avoid"
        );
        assert!(
            prompt.contains("CONFLICT (content): seed.txt"),
            "git's own account of the conflict is the one thing here nobody has to guess at"
        );
    }

    /// The agent resolves, commits and stops: handing the commit to the queue is the daemon's job.
    /// `nucleos-core --land` is held by the classifier on every call and the binary is not on the
    /// agent's PATH anyway, so a prompt that asks for it produces improvisation instead of a landing.
    #[test]
    fn the_prompt_never_asks_the_agent_to_land_or_touch_path() {
        for output in [Some("CONFLICT (content): seed.txt"), None] {
            let prompt = resolution_prompt("feat/x", "master", output);
            assert!(
                !prompt.contains("--land"),
                "the agent is not the one who lands: {prompt}"
            );
            assert!(
                !prompt.contains("nucleos-core"),
                "the agent has no such binary to run: {prompt}"
            );
            assert!(
                prompt.contains("queue"),
                "the agent should be told where its commit goes once it stops"
            );
            for variable in ["PATH", "RUSTUP_HOME", "CARGO_HOME"] {
                assert!(
                    prompt.contains(variable),
                    "assigning {variable} is what stalled the earlier resolutions, so it is named"
                );
            }
            assert!(
                prompt.to_lowercase().contains("already on path"),
                "cargo needs no setup, and saying so is what stops the improvising"
            );
        }
    }

    fn git_at(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    /// How far the resolver's worktree got before its run ended.
    #[derive(Clone, Copy, PartialEq)]
    enum Progress {
        /// Conflicts resolved and the merge committed: a 2-parent HEAD, no `MERGE_HEAD`.
        Committed,
        /// The merge is still staged and uncommitted: `MERGE_HEAD` exists.
        MidMerge,
        /// Nothing was done: HEAD is still the base the worktree was opened on.
        Untouched,
    }

    struct Scenario {
        _container: tempfile::TempDir,
        request: i64,
        branch: String,
    }

    /// A real repository where `feat/x` and `master` conflict, a worktree on `nucleos/run-7` opened at
    /// `master` and taken to `progress`, and the rows the daemon would hold for it: the escalation
    /// (project root pointed at the real repository), its run recorded as `run_status`, finished long
    /// enough ago to be past any grace, and the worktree row owned by run 7.
    async fn resolution_scenario(
        pool: &sqlx::SqlitePool,
        prefix: &str,
        run_status: &str,
        progress: Progress,
    ) -> Scenario {
        let container = crate::git_exec::tests::space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&repo);
        assert!(git_at(&repo, &["branch", "-M", "master"]));
        assert!(git_at(&repo, &["checkout", "-q", "-b", "feat/x"]));
        std::fs::write(repo.join("seed.txt"), "theirs\n").unwrap();
        assert!(git_at(&repo, &["commit", "-am", "theirs"]));
        assert!(git_at(&repo, &["checkout", "-q", "master"]));
        std::fs::write(repo.join("seed.txt"), "ours\n").unwrap();
        assert!(git_at(&repo, &["commit", "-am", "ours"]));
        let base = crate::git_exec::tests::sha_of(&repo, "master");

        let branch = crate::worktree::Owner::Run(7).branch_name();
        let tree = container.path().join("run-7");
        assert!(git_at(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                &tree.to_string_lossy(),
                "master"
            ]
        ));
        if progress != Progress::Untouched {
            // Conflicts, so this exits non-zero by design; the staged state is what is wanted.
            let _ = git_at(&tree, &["merge", "--no-ff", "--no-commit", "feat/x"]);
            std::fs::write(tree.join("seed.txt"), "both\n").unwrap();
            assert!(git_at(&tree, &["add", "-A"]));
            if progress == Progress::Committed {
                assert!(git_at(&tree, &["commit", "-q", "-m", "resolved"]));
            }
        }

        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES ('proj', 'active', ?, 'master')",
        )
        .bind(repo.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .expect("seed the project's roster row");

        let request = escalated_merge_of(pool, "feat/x", false).await;
        sqlx::query("UPDATE vcs_requests SET project_root = ? WHERE id = ?")
            .bind(repo.to_string_lossy().into_owned())
            .bind(request)
            .execute(pool)
            .await
            .unwrap();
        resolving(pool, request, 7).await;

        insert_run(pool, 7, run_status).await;
        sqlx::query("UPDATE runs SET completed_at = ? WHERE id = 7")
            .bind((chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339())
            .execute(pool)
            .await
            .unwrap();
        crate::worktree::record(
            pool,
            crate::worktree::Owner::Run(7),
            "proj",
            &repo.to_string_lossy(),
            &tree.to_string_lossy(),
            &branch,
            Some(&base),
        )
        .await
        .unwrap();

        Scenario {
            _container: container,
            request,
            branch,
        }
    }

    /// Rows other than the escalation itself that were admitted as a resolution of `branch`.
    async fn admitted_resolutions(pool: &sqlx::SqlitePool, scenario: &Scenario) -> Vec<i64> {
        sqlx::query_scalar(
            "SELECT id FROM vcs_requests
              WHERE from_resolution = 1 AND id != ? AND args LIKE ?
              ORDER BY id",
        )
        .bind(scenario.request)
        .bind(format!("%{}%", scenario.branch))
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// **AC2.** The agent stops after committing and the daemon hands the commit over, once, and
    /// linked: the escalation points at the new row, the new row carries the resolution flag so
    /// `verify_resolution` still gates publication, and a later tick admits nothing more.
    #[tokio::test]
    async fn a_finished_resolution_is_handed_to_the_queue_exactly_once() {
        let pool = test_pool().await;
        let scenario = resolution_scenario(
            &pool,
            "nucleos-resolver-land-",
            "completed",
            Progress::Committed,
        )
        .await;

        let mut refused = Default::default();
        land_finished(&pool, &mut refused).await;

        let admitted = admitted_resolutions(&pool, &scenario).await;
        assert_eq!(admitted.len(), 1, "one landing for one finished resolution");
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT resolved_by FROM vcs_requests WHERE id = ?")
                .bind(scenario.request)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            linked,
            Some(admitted[0]),
            "the session still waiting on the escalation follows this link to the landing"
        );

        land_finished(&pool, &mut refused).await;
        assert_eq!(
            admitted_resolutions(&pool, &scenario).await,
            admitted,
            "a second pass over the same finished resolution admits nothing"
        );
    }

    /// **AC3.** Each of these leaves something a person or the agent still owns, or nothing to land.
    #[tokio::test]
    async fn a_resolution_without_a_finished_merge_is_never_handed_over() {
        for (status, progress, why) in [
            (
                "failed",
                Progress::Committed,
                "a failed run's commit is not a finished resolution",
            ),
            (
                "cancelled",
                Progress::Committed,
                "a cancelled run was stopped, not finished",
            ),
            (
                "completed",
                Progress::MidMerge,
                "MERGE_HEAD is still there, so the merge is unfinished",
            ),
            (
                "completed",
                Progress::Untouched,
                "HEAD is still the base, so nothing was resolved",
            ),
        ] {
            let pool = test_pool().await;
            let scenario =
                resolution_scenario(&pool, "nucleos-resolver-nolanding-", status, progress).await;

            let mut refused = Default::default();
            land_finished(&pool, &mut refused).await;

            assert!(
                admitted_resolutions(&pool, &scenario).await.is_empty(),
                "{why}"
            );
            let linked: Option<i64> =
                sqlx::query_scalar("SELECT resolved_by FROM vcs_requests WHERE id = ?")
                    .bind(scenario.request)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(linked, None, "{why}");
        }
    }

    /// A conflict that a later merge of the same two branches already published is settled, and
    /// handing its resolution over would re-admit a stale one.
    #[tokio::test]
    async fn a_conflict_settled_by_a_later_merge_is_never_handed_over() {
        let pool = test_pool().await;
        let scenario = resolution_scenario(
            &pool,
            "nucleos-resolver-settled-",
            "completed",
            Progress::Committed,
        )
        .await;
        let later = succeeded_merge_of(&pool, "feat/x", false).await;
        assert!(later > scenario.request, "the settling merge came later");

        let mut refused = Default::default();
        land_finished(&pool, &mut refused).await;

        assert!(
            admitted_resolutions(&pool, &scenario).await.is_empty(),
            "a conflict settled by a different merge is not re-admitted"
        );
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT resolved_by FROM vcs_requests WHERE id = ?")
                .bind(scenario.request)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked, None);
    }

    /// The `resolved_by` link is best-effort, so the pass cannot lean on it alone: a branch the
    /// queue already holds a merge of is not submitted a second time.
    #[tokio::test]
    async fn a_branch_already_in_the_queue_is_not_submitted_again() {
        let pool = test_pool().await;
        let scenario = resolution_scenario(
            &pool,
            "nucleos-resolver-queued-",
            "completed",
            Progress::Committed,
        )
        .await;
        // The row an earlier tick admitted, whose link back to the escalation was never written.
        escalated_merge_of(&pool, &scenario.branch, false).await;

        let mut refused = Default::default();
        land_finished(&pool, &mut refused).await;

        assert!(
            admitted_resolutions(&pool, &scenario).await.is_empty(),
            "nothing new is admitted for a branch the queue already holds"
        );
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT resolved_by FROM vcs_requests WHERE id = ?")
                .bind(scenario.request)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked, None);
    }

    async fn gate(pool: &sqlx::SqlitePool, status: &str) {
        sqlx::query("UPDATE runs SET gate_status = ? WHERE id = 7")
            .bind(status)
            .execute(pool)
            .await
            .unwrap();
    }

    /// `completed` says the agent stopped, not that the project's gate passed, and the queue's land
    /// builds nothing: a resolution whose gate failed is announced once and never handed over.
    #[tokio::test]
    async fn a_resolution_whose_gate_did_not_pass_is_never_handed_over() {
        for status in ["failed", "errored"] {
            let pool = test_pool().await;
            let scenario = resolution_scenario(
                &pool,
                "nucleos-resolver-gate-",
                "completed",
                Progress::Committed,
            )
            .await;
            gate(&pool, status).await;

            let mut refused = Default::default();
            land_finished(&pool, &mut refused).await;
            land_finished(&pool, &mut refused).await;

            assert!(
                admitted_resolutions(&pool, &scenario).await.is_empty(),
                "a gate that {status} keeps the resolution from the queue"
            );
            let linked: Option<i64> =
                sqlx::query_scalar("SELECT resolved_by FROM vcs_requests WHERE id = ?")
                    .bind(scenario.request)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(linked, None);
            let summaries: Vec<String> =
                sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ?")
                    .bind(crate::land::RESOLUTION_FAILED_KIND)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert_eq!(summaries.len(), 1, "announced once, not every tick");
            assert!(summaries[0].contains(status), "{}", summaries[0]);
        }
    }

    #[tokio::test]
    async fn a_resolution_whose_gate_passed_is_handed_over() {
        let pool = test_pool().await;
        let scenario = resolution_scenario(
            &pool,
            "nucleos-resolver-gate-",
            "completed",
            Progress::Committed,
        )
        .await;
        gate(&pool, "passed").await;

        let mut refused = Default::default();
        land_finished(&pool, &mut refused).await;

        let admitted = admitted_resolutions(&pool, &scenario).await;
        assert_eq!(admitted.len(), 1, "a passed gate is handed over");
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT resolved_by FROM vcs_requests WHERE id = ?")
                .bind(scenario.request)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked, Some(admitted[0]));
    }

    /// **AC4.** A resolution that paused for approval resumes under a new run id: the escalation's
    /// `resolution_run_id` and the worktree row both move to the successor while the branch keeps
    /// naming the original run. Both lookups have to follow the run that owns the tree, or the
    /// landing is published unverified and unlinked — which is what every resumed one did.
    #[tokio::test]
    async fn a_resumed_resolution_is_still_recognised_and_linked() {
        let pool = test_pool().await;
        let request = escalated_merge(&pool, false).await;
        resolving(&pool, request, 9).await;
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Run(9),
            "proj",
            "C:/repo",
            "C:/wt/run-7",
            "nucleos/run-7",
            None,
        )
        .await
        .unwrap();

        assert!(
            landing_is_a_resolution(&pool, "proj", "nucleos/run-7").await,
            "the branch names run 7 but the escalation now records 9, which owns the tree"
        );
        assert_eq!(
            escalated_request_id(&pool, "proj", "nucleos/run-7").await,
            Some(request),
            "the link has to be written for the successor's landing too"
        );
        assert!(
            !landing_is_a_resolution(&pool, "other", "nucleos/run-7").await,
            "still scoped to the project"
        );
        assert!(
            !landing_is_a_resolution(&pool, "proj", "nucleos/run-8").await,
            "a branch whose tree belongs to nobody the escalation names is not a resolution"
        );
        assert_eq!(
            escalated_request_id(&pool, "proj", "nucleos/run-8").await,
            None
        );
    }

    /// **A resolution that cannot be accounted for is still accounted for**, and the column is what
    /// stops the pass from asking again every minute for ever. A repository that has moved away does
    /// not come back by being polled, and a NULL left behind would have this loop reading the same
    /// row, running the same git, and failing the same way until somebody noticed the log.
    ///
    /// It is also the honest answer: "could not be worked out" is a different thing from "nothing was
    /// lost", and a reader who finds the second when the first is true has been told something
    /// nobody checked.
    #[tokio::test]
    async fn a_resolution_whose_repository_is_gone_records_that_rather_than_asking_for_ever() {
        let pool = test_pool().await;
        let request = escalated_merge(&pool, true).await;
        sqlx::query(
            "UPDATE vcs_requests
                SET status = 'succeeded', result_sha = 'deadbeef', project_root = 'C:/gone'
              WHERE id = ?",
        )
        .bind(request)
        .execute(&pool)
        .await
        .unwrap();

        record_discards(&pool).await;

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT discarded FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&pool)
                .await
                .unwrap();
        let recorded = recorded.expect("a row that could not be worked out still gets an answer");
        assert!(
            recorded.starts_with("could not be worked out"),
            "and the answer says which of the three states this is: {recorded}"
        );
        assert!(
            !recorded.starts_with("nothing"),
            "a repository nobody could read must never report a clean resolution"
        );

        // The pass moves on rather than finding the same row again.
        record_discards(&pool).await;
        let unchanged: Option<String> =
            sqlx::query_scalar("SELECT discarded FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unchanged.as_deref(), Some(recorded.as_str()));
    }

    /// Retention clears `output_tail` on old rows, and a conflict that outlived its text is still
    /// worth resolving — the conflict is in the branches, not in what git said about it.
    #[test]
    fn a_conflict_whose_output_has_aged_out_still_gets_a_prompt() {
        let prompt = resolution_prompt("feat/x", "master", None);
        assert!(prompt.contains("ALREADY STAGED"));
        assert!(!prompt.contains("Git's own account"));
    }
}
