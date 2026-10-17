//! Reading what finished nodes left behind: reconciling node runs, ingesting a plan or a replan, and opening the next round.

use super::*;

/// Folds a finished node's outcome back into the job.
///
/// A pass does this before it decides anything, and it is the only part that runs whatever the
/// brakes say: it records work that already happened. Refusing to write down a finished node
/// because the budget ran out would lose the node and repeat it.
/// What `reconcile_nodes` leaves behind for the pass that called it.
///
/// A bool would have done and would have read as a bool at the call site. This exists because the
/// one case that stops a pass — a worktree that could not be put back — retires the job as it goes,
/// and a caller that carried on would be driving a job that has already ended.
pub(super) enum Reconciled {
    KeepGoing,
    JobRetired,
}

pub(super) async fn reconcile_nodes(state: &AppState, job: &JobRow) -> sqlx::Result<Reconciled> {
    let pool = &state.pool;

    // The two statuses excluded here are `node_in_flight`'s, written out because sqlx will not take
    // SQL built at runtime. `a_node_still_in_flight_is_not_reconciled` holds the two in step.
    let landed: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT i.ordinal, r.id, r.status
         FROM job_items i JOIN runs r ON r.id = i.run_id
         WHERE i.job_id = ? AND i.status = 'running'
           AND r.status NOT IN ('running','awaiting_approval')",
    )
    .bind(job.id)
    .fetch_all(pool)
    .await?;

    for (ordinal, run_id, run_status) in landed {
        // **A terminal row whose task is still registered is not finished yet.** The run body
        // writes its ending first and only then decides on a context handoff, inserts the
        // successor and moves `job_items.run_id` onto it. A pass landing in that gap read the
        // predecessor's `completed`, called the item implemented and gated or started the next
        // item while the successor was editing the same tree. The registration outlives every
        // write the body makes — it drops when the task returns, after the handoff — so waiting
        // for it closes the gap without a second notion of "in handoff" to keep in step. A cancel
        // removes the entry itself before it writes, and a restart starts with none, so neither
        // can leave an item waiting on a task that is not there.
        if state
            .run_handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&run_id)
        {
            continue;
        }

        // Only `completed` is done. `timed_out`, `cancelled` and `interrupted` all leave a tree
        // holding edits no gate has measured, and calling any of them finished would let the next
        // item build on top of them — so all three stop the chain.
        //
        // A cancelled node is kept apart from the other two all the same, because it is the
        // difference between the work breaking and somebody stopping it. This is what makes
        // cancelling a run mean something distinct from cancelling a job: one stops a node and ends
        // the chain honestly, the other stops the chain outright. Collapsing them would report the
        // user's own decision back to them as a failure.
        let item_status = match run_status.as_str() {
            "completed" => "implemented",
            STATUS_CANCELLED => STATUS_CANCELLED,
            _ => "failed",
        };

        // **The tree is put back BEFORE the item is marked, and that order is what lets the queue
        // carry on.** The paragraph above is the reason a dead node ended the chain: its tree holds
        // edits nothing measured, and the next item would build on them. That is a statement about
        // the TREE, so it is answered by fixing the tree rather than by ending the job — exactly
        // what `record_gate` already does for a gate that went red, and this is the same move for
        // the other way an item can break. `next_step` reads the state afterwards and walks past it.
        //
        // Only for a job the owner commissioned and funded, because only there is a longer leash
        // something somebody asked for. A scheduled job keeps the old ending, untouched.
        //
        // A revert that FAILS retires the job here and now. It is the one case where carrying on is
        // worse than stopping: the branch holds work nothing has looked at, `next_step` would walk
        // past the item believing otherwise, and the item after it would build on top. Stopping
        // hands the branch to a person with everything still on it.
        if item_status == "failed" && job.commissioned_with_an_allowance() {
            let ordinal = ordinal as usize;
            let put_back = match (
                job_worktree(pool, job.id).await,
                revert_point(pool, job, ordinal).await,
            ) {
                (Ok(Some((worktree, _))), Some(footing)) => {
                    crate::execution::worktree::revert_to(&worktree, &footing)
                        .await
                        .is_ok()
                }
                _ => false,
            };
            if !put_back {
                let _ = retire(pool, job.id, STATUS_STOPPED).await;
                say(
                    pool,
                    job,
                    "job_gate_failed",
                    &format!(
                        "job {} at item {}: its node ended `{run_status}` and the worktree could \
                         not be put back, so the job stopped with the work still on the branch",
                        job.id,
                        ordinal + 1
                    ),
                )
                .await;
                return Ok(Reconciled::JobRetired);
            }
        }

        // `AND status = 'running'`: a job cancel marks the item `cancelled` on its own, and this
        // pass, having read the row before that, must not write `implemented` back over it.
        sqlx::query(
            "UPDATE job_items SET status = ? WHERE job_id = ? AND ordinal = ? AND status = 'running'",
        )
        .bind(item_status)
        .bind(job.id)
        .bind(ordinal)
        .execute(pool)
        .await?;
        match item_status {
            "failed" => {
                say(
                    pool,
                    job,
                    "job_item_failed",
                    &format!(
                        "job {} stopped at item {}: its node ended `{run_status}`",
                        job.id,
                        ordinal + 1
                    ),
                )
                .await;
            }
            STATUS_CANCELLED => {
                say(
                    pool,
                    job,
                    "job_cancelled",
                    &format!(
                        "job {} stopped at item {}: its node was cancelled",
                        job.id,
                        ordinal + 1
                    ),
                )
                .await;
            }
            _ => {}
        }
    }

    if job.stage() == "planning" {
        ingest_plan(state, job).await?;
    }
    // Unconditional, unlike the plan node's, because a replan node has no status of its own to key
    // off — it leaves the job wherever it found it. Cheap when there is nothing to take: one indexed
    // read of the latest `replan` run, and the very common case is that there has never been one.
    ingest_replan(state, job).await?;
    Ok(Reconciled::KeepGoing)
}

/// Whether one of this job's nodes has stopped to ask permission.
///
/// The job row otherwise reads `implementing` while the whole chain is blocked on a person, and
/// the only sign anywhere is a proposal in a queue that never names the job it came from.
pub(super) async fn node_awaiting_approval(pool: &SqlitePool, job_id: i64) -> sqlx::Result<bool> {
    let paused: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM runs WHERE job_id = ? AND status = 'awaiting_approval' LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    Ok(paused.is_some())
}

/// Turns a finished plan node into the job's queue.
pub(super) async fn ingest_plan(state: &AppState, job: &JobRow) -> sqlx::Result<()> {
    let pool = &state.pool;
    let Some(run_status): Option<String> = sqlx::query_scalar(
        "SELECT status FROM runs WHERE job_id = ? AND stage = 'plan' ORDER BY id DESC LIMIT 1",
    )
    .bind(job.id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(());
    };
    if node_in_flight(&run_status) {
        return Ok(());
    }

    if run_status != "completed" {
        finish(pool, job.id, Outcome::Failed).await?;
        say(
            pool,
            job,
            "job_plan_failed",
            &format!(
                "job {} could not plan: its plan node ended `{run_status}`",
                job.id
            ),
        )
        .await;
        return Ok(());
    }

    let contents = match job_worktree(pool, job.id).await? {
        Some((worktree, _)) => {
            let file = worktree
                .join(crate::execution::worktree::ARTIFACTS_DIR)
                .join(PLAN_FILE);
            tokio::fs::read(&file).await.ok()
        }
        None => None,
    };

    let planned = match parse_plan(
        contents.as_deref(),
        job.max_items.max(0) as usize,
        graph_rules(pool, job).await.as_ref(),
    ) {
        Ok(planned) => planned,
        // Absent and unreadable are both planning failures, and neither is an empty queue. The
        // queue is never invented: a job that cannot say what it meant to do does not proceed to
        // do it.
        Err(error) => {
            finish(pool, job.id, Outcome::Failed).await?;
            say(
                pool,
                job,
                "job_plan_failed",
                &format!("job {} could not plan: {error}", job.id),
            )
            .await;
            return Ok(());
        }
    };

    // One transaction for the queue and the status that says the queue exists. Written apart, a
    // crash after some of the items left a `planning` job holding half a queue: the next pass
    // read the same plan again and wrote it a second time on top of the first.
    let mut tx = pool.begin().await?;
    for (ordinal, item) in planned.items.iter().enumerate() {
        // NULL rather than `[]`: the column that says nothing reads downstream as "the planner did
        // not say", where an empty array would read as "the planner said no files". A hint that
        // will not serialize is treated as one that was never given — losing a guess is cheaper
        // than failing a plan over it.
        let files = (!item.files.is_empty())
            .then(|| serde_json::to_string(&item.files).ok())
            .flatten();
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, files, depends_on,
                                    agent_id)
             VALUES (?, ?, ?, 'pending', ?, ?, ?)",
        )
        .bind(job.id)
        .bind(ordinal as i64)
        .bind(&item.description)
        .bind(files)
        .bind(edges(&item.depends_on))
        .bind(item.agent_id.as_deref())
        .execute(&mut *tx)
        .await?;
    }
    let moved = set_live_status(&mut *tx, job.id, "implementing").await?;
    tx.commit().await?;
    if !moved {
        // Ended while its plan was being read. The queue stays written — it is what the job meant
        // to do, and reads as such — but nothing announces it as a job about to do it.
        return Ok(());
    }

    let dropped = if planned.dropped > 0 {
        // Said out loud, never silently: a queue cut from seven to five reads downstream as "the
        // planner found five things", which is a different and wrong statement about the work.
        format!(
            " ({} more were dropped at the {}-item ceiling)",
            planned.dropped, job.max_items
        )
    } else {
        String::new()
    };
    say(
        pool,
        job,
        "job_planned",
        &format!(
            "job {} planned {} item(s){dropped}",
            job.id,
            planned.items.len()
        ),
    )
    .await;
    Ok(())
}

pub const PLAN_FILE: &str = "plan.json";

/// The brief a spec node writes, beside the queue a plan node writes.
///
/// Markdown and not JSON, unlike its sibling, because nothing parses it: it is read by the planner
/// and by a person, and a schema would only invite a node to satisfy the shape while saying
/// nothing. `plan.json` is machine-read and so has to be a document with fields; this is prose and
/// is allowed to be.
pub const SPEC_FILE: &str = "spec.md";

/// The council's synthesis, written into the job's artifacts directory for the review node to read.
///
/// Markdown for the reason `SPEC_FILE` is: nothing parses it. It is a deliberation written for a
/// reader, and giving it a schema would only invite the chairman to fill fields.
///
/// Only ever present when the `job_review` consumer is on AND a council actually settled with a
/// synthesis, which is why `review_prompt` is told whether it exists rather than assuming it. See
/// that function: a prompt naming a file that is not there is an instruction that fails by
/// omission, and the node spends a tool call discovering it.
pub const COUNCIL_FILE: &str = "council.md";

/// Copies a round's plan aside and lists every archive the job has, newest last.
///
/// Best-effort, and deliberately so: a copy that fails costs the replan node its history, which the
/// prompt then says out loud rather than papering over. Failing the job instead would throw a
/// night's work away over a file copy — and the node can still read the working tree and its git log,
/// which is a worse account of what was tried but not no account at all.
///
/// Every archive, not just the one just written: round 3's replan needs to know what rounds 0, 1 and
/// 2 tried, or it reproposes the oldest of them.
pub(super) async fn archive_plans(worktree: &Path, round: i64) -> Vec<String> {
    let directory = worktree.join(crate::execution::worktree::ARTIFACTS_DIR);
    let archive = format!("plan-{round}.json");
    if let Err(error) = tokio::fs::copy(directory.join(PLAN_FILE), directory.join(&archive)).await {
        tracing::warn!(%error, round, "could not archive a round's plan for the replan node");
    }

    let mut archives = Vec::new();
    for previous in 0..=round {
        let name = format!("plan-{previous}.json");
        if tokio::fs::try_exists(directory.join(&name))
            .await
            .unwrap_or(false)
        {
            archives.push(name);
        }
    }
    archives
}

/// Takes the answer a replan node landed with, exactly once.
///
/// Read wherever the job happens to be parked rather than from a status of its own: a replan node
/// leaves `jobs.status` as it found it (`reviewing`, usually), which keeps the job holding the
/// project's concurrency slot without the slot table needing to learn
/// a new word.
pub(super) async fn ingest_replan(state: &AppState, job: &JobRow) -> sqlx::Result<()> {
    let pool = &state.pool;
    let Some((run_id, run_status)): Option<(i64, String)> = sqlx::query_as(
        "SELECT id, status FROM runs WHERE job_id = ? AND stage = 'replan' ORDER BY id DESC LIMIT 1",
    )
    .bind(job.id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(());
    };
    if node_in_flight(&run_status) {
        return Ok(());
    }
    // The marker, and the reason it is a marker: a replan that produced nothing leaves every
    // observable condition exactly as it found it, so any derived test would ingest it again on the
    // next tick and keep bumping the round until the ceiling ended the job.
    let taken: Option<i64> = sqlx::query_scalar("SELECT replan_run_id FROM jobs WHERE id = ?")
        .bind(job.id)
        .fetch_one(pool)
        .await?;
    if taken == Some(run_id) {
        return Ok(());
    }

    // Everything below records the node as taken in the same statement that acts on it, so a job
    // cannot end up acting twice on one answer.
    if run_status != "completed" {
        stop_after_replan(
            state,
            job,
            run_id,
            &format!("its replan node ended `{run_status}`"),
        )
        .await?;
        return Ok(());
    }

    let contents = match job_worktree(pool, job.id).await? {
        Some((worktree, _)) => {
            let file = worktree
                .join(crate::execution::worktree::ARTIFACTS_DIR)
                .join(PLAN_FILE);
            tokio::fs::read(&file).await.ok()
        }
        None => None,
    };
    let planned = match parse_plan(
        contents.as_deref(),
        job.max_items.max(0) as usize,
        graph_rules(pool, job).await.as_ref(),
    ) {
        Ok(planned) => planned,
        Err(error) => {
            stop_after_replan(state, job, run_id, &format!("its replan node {error}")).await?;
            return Ok(());
        }
    };

    // Ending #1. `stopped` is not the word here and `failed` certainly is not: the node that just
    // looked at the work says the work is over, which is the best evidence this system can get.
    if let Some(why) = planned.done {
        sqlx::query("UPDATE jobs SET replan_done = 1, replan_run_id = ? WHERE id = ?")
            .bind(run_id)
            .bind(job.id)
            .execute(pool)
            .await?;
        say(
            pool,
            job,
            "job_replanned",
            &format!(
                "job {} says it is done after {} round(s): {why}",
                job.id,
                job.round + 1
            ),
        )
        .await;
        return Ok(());
    }

    open_the_next_round(state, job, run_id, &planned).await
}

/// Ends a job whose replan node could not answer, keeping what the earlier rounds did.
///
/// `Stopped` and not `Failed`, which is the whole point of the distinction: the rounds that ran are
/// on the branch, gated, and worth looking at. A job reported `failed` for want of a replan teaches
/// its reader to ignore the branch.
pub(super) async fn stop_after_replan(
    state: &AppState,
    job: &JobRow,
    run_id: i64,
    why: &str,
) -> sqlx::Result<()> {
    let pool = &state.pool;
    sqlx::query("UPDATE jobs SET replan_run_id = ? WHERE id = ?")
        .bind(run_id)
        .bind(job.id)
        .execute(pool)
        .await?;
    finish(pool, job.id, Outcome::Stopped).await?;
    say(
        pool,
        job,
        "job_stopped",
        &format!(
            "job {} stopped after round {}: {why}. What the earlier rounds did is on the branch.",
            job.id,
            job.round + 1
        ),
    )
    .await;
    Ok(())
}

/// Queues what a replan asked for and moves the job onto the next round.
///
/// Ordinals CONTINUE rather than restart, because `ordinal` is half `job_items`'s primary key and
/// because the position in the loaded queue is the number `advance` looks an item up by. The round
/// is carried on the row as a label, for the replan node's history and for a person reading which
/// round a given item came from.
pub(super) async fn open_the_next_round(
    state: &AppState,
    job: &JobRow,
    run_id: i64,
    planned: &PlannedItems,
) -> sqlx::Result<()> {
    let pool = &state.pool;
    let round = job.round + 1;
    // One transaction from the ordinal read to the status, so the round opens whole or not at all.
    // `jobs.replan_run_id` is the marker that says this replan was taken, and it is written last:
    // a crash between the items and the marker left the items queued and the replan untaken, and
    // the next pass opened the same round a second time.
    let mut tx = pool.begin().await?;
    let next_ordinal: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(ordinal) + 1, 0) FROM job_items WHERE job_id = ?")
            .bind(job.id)
            .fetch_one(&mut *tx)
            .await?;

    for (offset, item) in planned.items.iter().enumerate() {
        // The same NULL-rather-than-`[]` rule as the first round's ingestion, and carried here for
        // the reason it exists there. A later round's items are planned by a node looking at the
        // same tree, so an item of round 3 arriving without its file hint is not a different kind of
        // item — it is the hint being dropped on the way in, silently, for every round but the first.
        let files = (!item.files.is_empty())
            .then(|| serde_json::to_string(&item.files).ok())
            .flatten();
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, round, files, depends_on,
                                    agent_id)
             VALUES (?, ?, ?, 'pending', ?, ?, ?, ?)",
        )
        .bind(job.id)
        .bind(next_ordinal + offset as i64)
        .bind(&item.description)
        .bind(round)
        .bind(files)
        .bind(edges(&item.depends_on))
        .bind(item.agent_id.as_deref())
        .execute(&mut *tx)
        .await?;
    }

    // A failure the new round takes over stops deciding how the job ends; its replacement does.
    // Only an item that is actually unfinished, from an earlier round, can be superseded - a number
    // naming anything else is ignored rather than trusted, which is what the `WHERE` is for.
    for ordinal in &planned.replaces {
        sqlx::query(
            "UPDATE job_items SET status = ?
             WHERE job_id = ? AND ordinal = ? AND round < ? AND status IN ('gate_failed', 'failed')",
        )
        .bind(STATUS_SUPERSEDED)
        .bind(job.id)
        .bind(*ordinal as i64)
        .bind(round)
        .execute(&mut *tx)
        .await?;
    }

    // A round that added nothing feeds the counter; one that added something resets it. Both are the
    // same UPDATE, because the round advances either way — a dry round IS a round, and counting it
    // as one is what makes `dry_rounds` reach two.
    let dry = planned.items.is_empty();
    sqlx::query(
        "UPDATE jobs
         SET round = ?, dry_rounds = CASE WHEN ? THEN dry_rounds + 1 ELSE 0 END,
             replan_done = 0, replan_run_id = ?
         WHERE id = ?",
    )
    .bind(round)
    .bind(dry)
    .bind(run_id)
    .bind(job.id)
    .execute(&mut *tx)
    .await?;

    let moved = dry || set_live_status(&mut *tx, job.id, "implementing").await?;
    tx.commit().await?;
    if !moved {
        // Ended while the replan was being read; see `ingest_plan`.
        return Ok(());
    }

    say(
        pool,
        job,
        "job_replanned",
        &format!(
            "job {} opened round {} with {} item(s)",
            job.id,
            round + 1,
            planned.items.len()
        ),
    )
    .await;
    Ok(())
}
