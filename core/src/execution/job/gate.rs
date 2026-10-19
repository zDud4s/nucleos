//! Merging an item and gating it: checkpoints, the retry budget, reverts and recording the verdict.

use super::*;

/// Runs the gate for one item and records its verdict on that item.
/// Brings one item's branch into the job's, in the job's own checkout.
///
/// **`merge_base_sha` is written BEFORE the merge, and that order is the whole of this function.**
/// It is the commit the job's branch stands on right now, and after the merge nothing on the branch
/// says which of its commits this merge added. A merge that landed with the write lost would leave
/// an item on the branch with no way back off it, and the `reset --hard` that undoes a red gate
/// would have no target — so the write comes first and a failure to write refuses the merge.
///
/// `--no-ff` and not a fast-forward. The reset would work either way, but a fast-forward leaves no
/// merge commit, and then nothing in the history says this item arrived as one thing rather than as
/// a handful of commits that happen to be adjacent.
///
/// A conflict is aborted here and resolved elsewhere. Aborting leaves the job's checkout clean,
/// which is what the next item's merge depends on; resolving would mean an agent editing the job's
/// branch in the shared checkout, which is the thing every worktree in this design exists to avoid.
/// The item is put down as `conflicted` and the run that clears it works in the item's own tree —
/// the procedure `git_exec::compute_merge` already tells an agent to follow when the queue refuses
/// its merge.
pub(super) async fn merge_item(state: &AppState, job: &JobRow, ordinal: usize) -> Step {
    let pool = &state.pool;

    let Ok(Some((worktree, _))) = job_worktree(pool, job.id).await else {
        tracing::warn!(
            job_id = job.id,
            ordinal,
            "no worktree on record to merge into"
        );
        return Step::Stopped;
    };
    let item_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = ?")
            .bind(job.id)
            .bind(ordinal as i64)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let Some(item_id) = item_id else {
        tracing::warn!(job_id = job.id, ordinal, "no item row to merge");
        return Step::Stopped;
    };
    let branch = crate::execution::worktree::Owner::Item(item_id).branch_name();

    // **The item's work is committed HERE, and until this existed nothing committed it at all.**
    //
    // An implement node is told in as many words to leave its tree uncommitted: `implement_prompt`
    // says "the job commits for you once the gate agrees", and for a job without a team that is
    // exactly what happens — `record_gate`'s green arm checkpoints the one shared checkout. A team's
    // item has a checkout of its own, and its gate runs AFTER the merge rather than before it, so
    // the moment that used to commit comes too late. The branch stayed empty, the merge below
    // brought nothing across, and every item passed a gate over a job branch that had never
    // received its work. Nothing anywhere reported it, because an empty merge succeeds.
    //
    // Before the base is read and before anything is recorded, so a checkpoint that fails leaves no
    // half-finished merge behind it.
    //
    // A checkpoint over a tree with nothing in it is a no-op that answers with HEAD — `checkpoint`
    // commits only when `git status` has something to say — so an item whose node wrote nothing
    // merges an empty branch, which is the truthful outcome rather than an error.
    let item_tree: Option<String> = sqlx::query_scalar(
        "SELECT path FROM worktrees
         WHERE owner_kind = 'item' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(item_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(item_tree) = item_tree else {
        tracing::warn!(
            job_id = job.id,
            ordinal,
            "no checkout on record for the item being merged"
        );
        return Step::Stopped;
    };
    if let Err(error) = crate::execution::worktree::checkpoint(Path::new(&item_tree)).await {
        // Fatal here, where the same failure is survivable in `record_gate`. There the work is
        // already on the branch and what is lost is the next item's footing; here the work exists
        // only in a working tree, and merging without it would put a green gate on a branch that is
        // missing the thing it is measuring.
        tracing::warn!(job_id = job.id, ordinal, %error, "could not commit an item's work, so nothing was merged");
        return Step::Stopped;
    }

    let Ok(base) = crate::execution::worktree::head_sha(&worktree).await else {
        tracing::warn!(
            job_id = job.id,
            ordinal,
            "could not read where the job's branch stands, so nothing was merged"
        );
        return Step::Stopped;
    };
    let recorded =
        sqlx::query("UPDATE job_items SET merge_base_sha = ? WHERE job_id = ? AND ordinal = ?")
            .bind(&base)
            .bind(job.id)
            .bind(ordinal as i64)
            .execute(pool)
            .await;
    if let Err(error) = recorded {
        tracing::warn!(job_id = job.id, ordinal, %error, "could not record where a merge would start, so nothing was merged");
        return Step::Stopped;
    }

    match crate::execution::worktree::merge_branch(&worktree, &branch).await {
        Ok(true) => {
            // The branch already holds the merge, so a status the row never got is a job that
            // would carry on over a state the database does not have: stop, like the checks above.
            let marked = crate::storage::settle_write("job item (merging)", job.id, || {
                sqlx::query(
                    "UPDATE job_items SET status = 'merging' WHERE job_id = ? AND ordinal = ?",
                )
                .bind(job.id)
                .bind(ordinal as i64)
                .execute(pool)
            })
            .await;
            if !marked {
                tracing::warn!(
                    job_id = job.id,
                    ordinal,
                    "could not mark a merged item as merging, so the job stops here"
                );
                return Step::Stopped;
            }
            Step::Continued
        }
        Ok(false) => {
            let marked = crate::storage::settle_write("job item (conflicted)", job.id, || {
                sqlx::query(
                    "UPDATE job_items SET status = 'conflicted' WHERE job_id = ? AND ordinal = ?",
                )
                .bind(job.id)
                .bind(ordinal as i64)
                .execute(pool)
            })
            .await;
            if !marked {
                tracing::warn!(
                    job_id = job.id,
                    ordinal,
                    "could not mark a conflicting item as conflicted, so the job stops here"
                );
                return Step::Stopped;
            }
            say(
                pool,
                job,
                "job_item_conflicted",
                &format!(
                    "job {} at item {}: `{branch}` does not merge into the job's branch, so it was                      put down and nothing was published",
                    job.id,
                    ordinal + 1
                ),
            )
            .await;
            Step::Continued
        }
        Err(error) => {
            tracing::warn!(job_id = job.id, ordinal, %error, "could not merge an item");
            Step::Stopped
        }
    }
}

/// Which measurement an item's gate takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ItemGate {
    /// The project's whole gate command, as it has always been.
    Full,
    /// `verify(scope)` over the job worktree's diff from its recorded base.
    Scope,
}

/// `Scope` only with a map the measured work cannot have rewritten, and an executor to run it on.
/// The last item is `Full` unless `scoped_final` (the switch AND `gate_after_land`, so a
/// post-merge gate measures the whole target afterwards): without it the last item is the one
/// measurement a partial can be trusted on, so a test map never narrows it. Every other case keeps
/// today's behaviour, because nothing may measure less than it does now without a map worth
/// trusting.
pub(super) fn item_gate(
    last: bool,
    map_trusted: bool,
    executor_running: bool,
    scoped_final: bool,
) -> ItemGate {
    if (!last || scoped_final) && map_trusted && executor_running {
        ItemGate::Scope
    } else {
        ItemGate::Full
    }
}

/// True when the job tree holds a valid test map that is byte-identical to the project root's, and
/// every script its group commands name is too.
///
/// Same reason as `tampered_gate_script`: a verdict must not depend on a file the work being
/// measured may have rewritten. An agent that narrowed `nucleos.tests.yaml` would otherwise pick
/// which tests judge it, and one that rewrote `scripts/gates.sh` (what a map's commands run) would
/// pick what they mean: the full gate refuses that, so a scoped one must not be easier to fool.
/// Blocking: reads files from disk.
pub(crate) fn map_is_trusted(worktree: &Path, project_root: &Path) -> bool {
    let crate::tests_map::MapState::Valid(map) = crate::tests_map::load(worktree) else {
        return false;
    };
    let read = |root: &Path| std::fs::read(root.join(crate::tests_map::MAP_FILE));
    match (read(worktree), read(project_root)) {
        (Ok(tree), Ok(root)) if tree == root => {}
        _ => return false,
    }
    let commands = map.tests.groups.values().flat_map(|group| {
        [
            Some(group.command.as_str()),
            group.check.as_deref(),
            group.select.as_deref(),
        ]
        .into_iter()
        .flatten()
    });
    crate::gate::tampered_command_scripts(project_root, worktree, commands).is_none()
}

pub(super) async fn gate_item(
    state: &AppState,
    job: &JobRow,
    ordinal: usize,
    items: usize,
) -> Step {
    gate_item_with(state, job, ordinal, items, crate::verify::installed()).await
}

/// `gate_item` with the verify executor handed in, so a test can drive both gate paths without the
/// process-wide install that only `main` performs.
pub(super) async fn gate_item_with(
    state: &AppState,
    job: &JobRow,
    ordinal: usize,
    items: usize,
    installed: Option<std::sync::Arc<crate::verify::exec::Executor>>,
) -> Step {
    let pool = &state.pool;
    let last = ordinal + 1 == items;

    // Deliberately takes NO checkpoint, where `record_gate`'s green arm does.
    //
    // A checkpoint is the commit a gate agreed with, and there was no gate here. Leaving
    // `checkpoint_sha` NULL makes `footing_for` look further back — past every ungated item, to the
    // last measured one or to `jobs.head_sha` — so a later red gate takes the whole ungated stretch
    // with it when it reverts.
    //
    // That is the intended reading rather than an oversight: with `gate_after_each_item` off,
    // nothing has agreed with any of that work, so there is no point in it worth returning to. A
    // footing minted here would name a commit no gate ever blessed, which is exactly the false
    // comfort the column exists to avoid.
    let pass_without_measuring = |why: &'static str| async move {
        let _ =
            sqlx::query("UPDATE job_items SET status = 'passed' WHERE job_id = ? AND ordinal = ?")
                .bind(job.id)
                .bind(ordinal as i64)
                .execute(pool)
                .await;
        credit_the_briefing(pool, job, ordinal).await;
        tracing::debug!(job_id = job.id, ordinal, why, "item not gated");
        Step::Continued
    };

    // `gate_after_each_item: false` buys back the N suite runs, and the final gate runs regardless
    // of it: a job that never measured anything would hand back a partial nobody can trust.
    if job.gate_each == 0 && !last {
        return pass_without_measuring("gate_after_each_item is off and this is not the last item")
            .await;
    }

    let (command, scoped_final) = match crate::config::load_schedule_rules(
        state.machine_config_root.as_deref(),
        &job.project_id,
    ) {
        Ok(rules) => {
            let scoped_final = rules.scoped_final();
            (rules.gate_command, scoped_final)
        }
        // Unreadable is not the same as absent, and this is the distinction §7 of the design says
        // is the easiest to get wrong: a configuration nobody can read means the measurement did
        // not happen, which is not the same as a project that has no gate.
        Err(error) => {
            return record_gate(
                state,
                job,
                ordinal,
                crate::gate::GateOutcome::Errored {
                    reason: format!("gate configuration is unreadable: {error}"),
                },
            )
            .await;
        }
    };
    let Some(command) = command else {
        return pass_without_measuring("the project configures no gate command").await;
    };
    let Ok(Some((worktree, _))) = job_worktree(pool, job.id).await else {
        return record_gate(
            state,
            job,
            ordinal,
            crate::gate::GateOutcome::Errored {
                reason: "the job has no worktree on record to measure".to_owned(),
            },
        )
        .await;
    };

    // An intermediate item with a trusted test map is measured by the daemon's verify executor
    // over the job's own diff, and so is the last one when `scoped_final` is on; everything else falls through to the full gate below, untouched.
    let executor = if last && !scoped_final {
        None
    } else {
        installed
    };
    let trusted = if executor.is_some() {
        let tree = worktree.clone();
        let root = PathBuf::from(&job.project_root);
        tokio::task::spawn_blocking(move || map_is_trusted(&tree, &root))
            .await
            .unwrap_or(false)
    } else {
        false
    };
    if item_gate(last, trusted, executor.is_some(), scoped_final) == ItemGate::Scope
        && let Some(executor) = executor
    {
        if matches!(set_live_status(pool, job.id, "gating").await, Ok(false)) {
            return Step::Stopped;
        }
        // The timeout counts from the submit, so time queued behind other work is part of it.
        let verdict = crate::verify::gate_job_scope(
            &executor,
            job.id,
            &worktree,
            crate::state::DEFAULT_GATE_TIMEOUT,
        )
        .await;
        if matches!(
            set_live_status(pool, job.id, "implementing").await,
            Ok(false)
        ) {
            tracing::info!(
                job_id = job.id,
                ordinal,
                "job ended while its gate ran; the outcome is not recorded"
            );
            return Step::Stopped;
        }
        // The last item's gate is the one the owner reads as "the job passed", so the feed says
        // which gate measured it, whatever the verdict (an unavailable scope says the full gate follows).
        if last {
            say(
                pool,
                job,
                "job_gate_scoped",
                &crate::verify::scoped_final_note(
                    &format!("job {} at item {}", job.id, ordinal + 1),
                    &verdict,
                ),
            )
            .await;
        }
        match verdict {
            crate::verify::ScopeVerdict::Measured(outcome) => {
                return record_gate(state, job, ordinal, outcome).await;
            }
            // On the LAST item a trusted map's "no group covers this diff" is a verdict: the map
            // was re-checked after the ticket came back, so the same bytes that answered are still
            // the root's, and the item takes a footing like a green one. Anywhere else, and on a
            // map that stopped being trusted meanwhile, nothing is minted.
            crate::verify::ScopeVerdict::NothingRan if last => {
                let tree = worktree.clone();
                let root = PathBuf::from(&job.project_root);
                let still_trusted =
                    tokio::task::spawn_blocking(move || map_is_trusted(&tree, &root))
                        .await
                        .unwrap_or(false);
                if !still_trusted {
                    return pass_without_measuring(
                        "the test map selects no group for the job's diff",
                    )
                    .await;
                }
                return pass_unmeasured_with_checkpoint(pool, job, ordinal, &worktree).await;
            }
            crate::verify::ScopeVerdict::NothingRan => {
                return pass_without_measuring("the test map selects no group for the job's diff")
                    .await;
            }
            // The scope request could not even be submitted, so nothing was measured: fall
            // through to the full gate below, exactly as if no executor were installed.
            crate::verify::ScopeVerdict::Unavailable => {}
        }
    }

    // Both writes are compare-and-set, and losing either stops the pass. A gate takes minutes, and
    // a cancel is most likely to land inside one: the old unconditional `implementing` afterwards
    // brought the cancelled job back to life, and `record_gate` then checkpointed and moved on to
    // the next item. `Ok(false)` is the job having ended; an `Err` says nothing about the job and
    // is let through as before.
    if matches!(set_live_status(pool, job.id, "gating").await, Ok(false)) {
        return Step::Stopped;
    }
    let outcome = crate::verify::runs::timed_gate(
        Some(pool),
        crate::verify::runs::GateContext {
            project_id: Some(&job.project_id),
            origin: crate::verify::runs::ORIGIN_JOB_ITEM,
            origin_id: Some(job.id),
            ordinal: Some(ordinal as i64),
        },
        &worktree,
        Path::new(&job.project_root),
        &command,
        crate::state::DEFAULT_GATE_TIMEOUT,
    )
    .await;
    if matches!(
        set_live_status(pool, job.id, "implementing").await,
        Ok(false)
    ) {
        tracing::info!(
            job_id = job.id,
            ordinal,
            "job ended while its gate ran; the outcome is not recorded"
        );
        return Step::Stopped;
    }
    record_gate(state, job, ordinal, outcome).await
}

/// Where the job's branch is put back to when this item's work has to come off it.
///
/// **Two answers, and which one is right depends on how the work got onto the branch.**
///
/// An item that was MERGED has `merge_base_sha`: the commit the branch stood on immediately before
/// that merge. Resetting there removes that merge and nothing else, whatever order the other items
/// arrived in — which is the only property that survives items landing out of ordinal order.
///
/// An item that was WRITTEN into the shared checkout has no merge and no base, and falls back to
/// [`footing_for`], which walks back through the ordinals to the nearest item a gate agreed with.
/// That is still exactly right there, because in a sequential queue the order work reached the
/// branch IS the order of the ordinals — and it is the only answer available, since nothing recorded
/// a per-item boundary.
///
/// The pair is read here rather than at the call site so that "which reset target" is one decision
/// with one place to look, instead of a condition threaded through the red-gate arm.
pub(super) async fn revert_point(
    pool: &SqlitePool,
    job: &JobRow,
    ordinal: usize,
) -> Option<String> {
    let merged: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT merge_base_sha FROM job_items WHERE job_id = ? AND ordinal = ?",
    )
    .bind(job.id)
    .bind(ordinal as i64)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten();
    if merged.is_some() {
        return merged;
    }
    footing_for(pool, job, ordinal).await
}

/// The commit an item at `ordinal` falls back to when its own work has to be undone.
///
/// PURE apart from the read. The nearest earlier item that a gate agreed with, or — for the first
/// item, and for a job whose earlier items were all skipped — the SHA the job was created at.
///
/// `None` is the one shape callers must handle rather than paper over: a job created before this
/// column existed, or one whose `head_sha` could not be read at creation, has no footing at all, and
/// the honest response is to leave the tree alone and stop rather than guess at a revision.
pub(super) async fn footing_for(pool: &SqlitePool, job: &JobRow, ordinal: usize) -> Option<String> {
    let earlier: Option<String> = sqlx::query_scalar(
        "SELECT checkpoint_sha FROM job_items
         WHERE job_id = ? AND ordinal < ? AND checkpoint_sha IS NOT NULL
         ORDER BY ordinal DESC LIMIT 1",
    )
    .bind(job.id)
    .bind(ordinal as i64)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    earlier.or_else(|| job.head_sha.clone())
}

/// The footing for the item a given run owns, for callers outside this module.
///
/// `hooks.rs` knows a `run_id` and nothing else — it is answering a tool call, not walking a queue —
/// so it cannot supply the ordinal [`footing_for`] wants. This resolves it, and deliberately reuses
/// the same query rather than growing a second answer to "what does this item fall back to".
pub(super) async fn footing_for_run(pool: &SqlitePool, job_id: i64, run_id: i64) -> Option<String> {
    let ordinal: i64 =
        sqlx::query_scalar("SELECT ordinal FROM job_items WHERE job_id = ? AND run_id = ?")
            .bind(job_id)
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()?;

    let job = load_job(pool, job_id).await.ok()?;
    footing_for(pool, &job, ordinal as usize).await
}

/// The job's worktree path, for callers outside this module that hold no `JobRow`.
pub(super) async fn job_worktree_path(pool: &SqlitePool, job_id: i64) -> Option<PathBuf> {
    job_worktree(pool, job_id)
        .await
        .ok()
        .flatten()
        .map(|(path, _)| path)
}

/// Where a node's half-written edits are, and what to put that tree back to.
///
/// **One question and one answer, because the two halves have to agree.** They used to be asked
/// separately — `job_worktree_path` for the tree, `footing_for_run` for the sha — and separately is
/// how they came to disagree: the first always answered with the JOB's checkout, and a node working
/// in a tree of its own would have had that tree reverted to a checkpoint it never wrote, while its
/// own edits stayed exactly where they were. Reverting the wrong tree is worse than reverting none.
///
/// For a node with an item of its own the answer is that item's checkout and the commit it was born
/// on — `worktrees.base_sha`, which migration 0062 defines as *"where the worktree branched from"*,
/// and where an item that never got started belongs. `None` if that base was never recorded, and
/// **deliberately not a fallback to the job's tree**: a missing base is a tree this cannot put
/// right, which the caller reports and leaves alone.
pub async fn revert_target(
    pool: &SqlitePool,
    job_id: i64,
    run_id: i64,
) -> Option<(PathBuf, String)> {
    // `Option<i64>` is named as the scalar type rather than left to inference, and it is not
    // style. Inferred as `i64`, a NULL `item_id` does not come back as `None` — it comes back as a
    // run that claims to be working on item 0, and this then answers `None` because no worktree
    // belongs to that item. Every node that exists before this column did has a NULL here, so the
    // wrong spelling turns "revert the item's tree" into "revert nothing" for all of them.
    let item_id: Option<i64> =
        sqlx::query_scalar::<_, Option<i64>>("SELECT item_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten();

    if let Some(item_id) = item_id {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT path, base_sha FROM worktrees
             WHERE owner_kind = 'item' AND owner_id = ? AND removed_at IS NULL",
        )
        .bind(item_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
        let (path, base_sha) = row?;
        return Some((PathBuf::from(path), base_sha?));
    }

    let path = job_worktree_path(pool, job_id).await?;
    let sha = footing_for_run(pool, job_id, run_id).await?;
    Some((path, sha))
}

/// How much of a red gate's output is kept for the node that has to answer it.
///
/// `gate.rs` already caps what it captures at 1 MiB, which is the right bound for "do not let a
/// runaway suite eat memory" and the wrong one for everything downstream: a megabyte per item in the
/// database, and a megabyte pasted into a prompt, is its own defect. A few KB of the TAIL is where
/// a test runner puts its failures and its summary line, which is the part a retry can act on.
pub(super) const GATE_OUTPUT_TAIL: usize = 4096;

/// How much gate output ONE replan prompt may carry, across every unfinished item in it.
///
/// [`GATE_OUTPUT_TAIL`] bounds what a single item contributes and nothing bounded the sum, which is
/// a different quantity: `unfinished_items` has no `LIMIT` and no round filter, so a replan lists
/// every failure of the whole job that nothing has taken over. `MAX_ROUNDS_CEILING` is 20 and
/// `MAX_ITEMS_CEILING` is 5 — the ceiling's own comment already says "100 items in the worst case" —
/// so the unbounded sum is 100 × 4 KiB ≈ **400 KiB** of gate output in the one prompt that decides
/// what the job does next. At the harness's own 4 chars/token that is ~100k tokens spent on
/// re-reading old failures.
///
/// Derived rather than picked: `GATE_OUTPUT_TAIL * MAX_ITEMS_CEILING` is exactly one full round's
/// failures at full detail. That is the round the replan is answering, and it is the number that
/// moves if either input does.
///
/// The budget buys gate OUTPUT and never an item's identity. Every unfinished item stays listed
/// whatever the budget does, because — in `replan_prompt`'s own words — "a failure nothing takes
/// over still decides how the job ends", so dropping one from the list would hide from the node the
/// very thing it needs a `"replaces"` for. What a squeezed item loses is the gate's words, and it
/// says so where they would have been.
pub(super) const REPLAN_GATE_BUDGET: usize = GATE_OUTPUT_TAIL * crate::config::MAX_ITEMS_CEILING;

/// Credit only after the item's verdict write.
///
/// `credit_item` reads that verdict back from the row, so a write that failed credits nothing and
/// the tick's sweep can retry after the durable state is final.
pub(super) async fn credit_the_briefing(pool: &SqlitePool, job: &JobRow, ordinal: usize) {
    if let Err(error) = crate::brief::credit_item(pool, job.id, ordinal as i64).await {
        tracing::warn!(job_id = job.id, ordinal, %error, "could not credit an item's briefing");
    }
}

/// The last [`GATE_OUTPUT_TAIL`] bytes of a gate's output, cut where a character actually ends.
///
/// The boundary walk is not defensive tidiness: slicing a `String` mid-character panics, and a gate
/// prints whatever the project's tools print — a test name with an accent in it, a `✗`, a path from
/// a non-ASCII branch. Cutting by raw byte index would turn somebody's stack trace into a crashed
/// daemon on the one night it mattered.
pub(super) fn gate_output_tail(output: &str) -> &str {
    if output.len() <= GATE_OUTPUT_TAIL {
        return output;
    }
    let mut start = output.len() - GATE_OUTPUT_TAIL;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    &output[start..]
}

/// Queue what an item verdict just written may have caused (a recovery or an exhaustion), inside
/// the verdict's own transaction. Best effort: a failure is logged with ids only and never fails
/// or rolls back the verdict.
pub(super) async fn queue_the_verdict(
    conn: &mut sqlx::SqliteConnection,
    job_id: i64,
    ordinal: i64,
) {
    if let Err(error) = crate::distill::enqueue_item_verdict_in(conn, job_id, ordinal).await {
        tracing::warn!(job_id, ordinal, %error, "distill: could not queue an item verdict");
    }
}

/// Pass the last item whose scoped gate ran nothing over a trusted map, taking a checkpoint.
///
/// Unlike `gate_after_each_item` off, where no gate was even asked, here a trusted test map
/// answered "no test covers this diff" for the whole job, and that answer is a verdict: the work
/// is on the branch and nothing the map vouches for disagrees with it, so the tree is committed
/// the way a green item's is and `checkpoint_sha` records it.
///
/// Deliberately not `record_gate(Passed)`: nothing was measured, so `gate_status` stays NULL and
/// `route_advice` is not told a pass. The status and the checkpoint are ONE statement, for the
/// reason `record_gate` gives (an item that reads `passed` with no checkpoint, for a moment, can
/// revert past work that was agreed with). A checkpoint that cannot be taken warns and writes
/// NULL; it never fails the item. It does not `queue_the_verdict` (distill) because nothing was
/// measured, same as `pass_without_measuring`.
pub(super) async fn pass_unmeasured_with_checkpoint(
    pool: &SqlitePool,
    job: &JobRow,
    ordinal: usize,
    worktree: &Path,
) -> Step {
    let checkpoint_sha = match crate::execution::worktree::checkpoint(worktree).await {
        Ok(sha) => Some(sha),
        Err(error) => {
            tracing::warn!(job_id = job.id, ordinal, %error, "could not checkpoint an unmeasured last item");
            None
        }
    };
    let written = sqlx::query(
        "UPDATE job_items SET status = 'passed', checkpoint_sha = ? WHERE job_id = ? AND ordinal = ?",
    )
    .bind(checkpoint_sha)
    .bind(job.id)
    .bind(ordinal as i64)
    .execute(pool)
    .await;
    if let Err(error) = written {
        tracing::warn!(job_id = job.id, ordinal, %error, "could not record an unmeasured last item");
        return Step::Stopped;
    }
    credit_the_briefing(pool, job, ordinal).await;
    tracing::debug!(
        job_id = job.id,
        ordinal,
        "last item not measured; checkpointed"
    );
    Step::Continued
}

pub(super) async fn record_gate(
    state: &AppState,
    job: &JobRow,
    ordinal: usize,
    outcome: crate::gate::GateOutcome,
) -> Step {
    let pool = &state.pool;
    // Three states out of three, never two. A binary that would not start says the measurement did
    // not happen; a non-zero exit says the code is broken. One of those is a verdict and the other
    // is silence, and reporting silence as a verdict stops the wrong work.
    let (item_status, gate_status, note) = match &outcome {
        crate::gate::GateOutcome::Passed => ("passed", "passed", None),
        crate::gate::GateOutcome::Failed { exit_code, .. } => (
            "gate_failed",
            "failed",
            Some(format!("the gate failed with exit code {exit_code}")),
        ),
        crate::gate::GateOutcome::Errored { reason } => (
            "gate_errored",
            "errored",
            Some(format!("the gate could not be measured: {reason}")),
        ),
    };
    // Told to the llm-router before anything below can return early. A DB read and a spawn, so it
    // cannot hold the tick on the network; an item whose run was not advised reports nothing.
    if let Some(router) = state.runner.router() {
        crate::models::route_advice::report_item_gate(
            pool,
            router,
            job.id,
            ordinal as i64,
            &outcome,
        )
        .await;
    }

    // Green: take the footing the next item will stand on.
    //
    // Written in the SAME statement as the verdict, not a second one after it. Two writes leave a
    // window where the item reads `passed` with no checkpoint, and an item that fails inside that
    // window reverts to the item BEFORE this one — throwing away work a gate had just agreed with,
    // silently, and only under a race nobody would reproduce.
    //
    // A checkpoint that cannot be taken does not fail the item. The work is on the branch either
    // way; what is lost is the next item's footing, and `footing_for` already answers that with the
    // nearest earlier one. Refusing a green gate over a `git commit` would turn a disk hiccup into a
    // failed night.
    let mut checkpoint_sha = None;
    if matches!(outcome, crate::gate::GateOutcome::Passed) {
        match job_worktree(pool, job.id).await {
            Ok(Some((worktree, _))) => {
                match crate::execution::worktree::checkpoint(&worktree).await {
                    Ok(sha) => checkpoint_sha = Some(sha),
                    Err(error) => {
                        tracing::warn!(job_id = job.id, ordinal, %error, "could not checkpoint a green item");
                    }
                }
            }
            _ => {
                tracing::warn!(
                    job_id = job.id,
                    ordinal,
                    "no worktree on record to checkpoint"
                );
            }
        }
    }

    // Red, with a retry left: remember what the gate said and ask for the item again.
    //
    // A SEQUENTIAL item keeps the shared tree: it is written straight into the job's checkout, so
    // its second attempt starts from the rejected work, and nothing is checkpointed (nothing passed,
    // and a footing taken over rejected work is one the items after this would inherit). A MERGED
    // item (`merge_base_sha` set) retries in its own tree, so it takes the revert below first: the
    // job's branch is reset to the merge base and the retry brings the branch back into the item's
    // tree, which keeps its work (spec 2026-08-19 section 3.2, decision #9). Without that reset the
    // rejected merge would stay on the job branch while other items merge on top of it.
    if let crate::gate::GateOutcome::Failed { exit_code, output } = &outcome {
        // Counted on BOTH paths, whether or not the count buys anything. A counter that only
        // incremented where a retry was spent would disagree with itself about what happened to the
        // item — and the budget it is read against is a number that can differ between two jobs
        // gating the same repository on the same night.
        if let Err(error) = sqlx::query(
            "UPDATE job_items SET gate_attempts = gate_attempts + 1, gate_output = ?
             WHERE job_id = ? AND ordinal = ?",
        )
        .bind(gate_output_tail(output))
        .bind(job.id)
        .bind(ordinal as i64)
        .execute(pool)
        .await
        {
            tracing::warn!(job_id = job.id, ordinal, %error, "could not count a red gate against its item");
        }

        // Read back rather than reasoned about, and read as a pair: the count and the budget are the
        // only two numbers that can tell a first red gate from a last one. Fails CLOSED — a budget
        // that cannot be read spends nothing, which is exactly today's behaviour.
        let spendable: Option<(i64, i64)> = sqlx::query_as(
            "SELECT i.gate_attempts, j.gate_retries
             FROM job_items i JOIN jobs j ON j.id = i.job_id
             WHERE i.job_id = ? AND i.ordinal = ?",
        )
        .bind(job.id)
        .bind(ordinal as i64)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        // A merged item's work lives on the job's branch as a merge, not in the shared tree.
        let merged: bool = sqlx::query_scalar::<_, bool>(
            "SELECT merge_base_sha IS NOT NULL FROM job_items WHERE job_id = ? AND ordinal = ?",
        )
        .bind(job.id)
        .bind(ordinal as i64)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);

        // ASKED, not restated. `attempts <= retries` written here would be the retriability rule in
        // a second place, and the two would agree only until one of them was edited — which is the
        // failure this module has already had three times, each under a comment asserting they
        // agreed. `item_state_from` owns the comparison; this passes it the status it is about to
        // write and believes the answer.
        if let Some((attempts, retries)) = spendable
            && !merged
            && item_state_from(item_status, attempts, retries) == ItemState::GateRetriable
        {
            let written = sqlx::query(
                "UPDATE job_items SET status = ?, gate_status = ? WHERE job_id = ? AND ordinal = ?",
            )
            .bind(item_status)
            .bind(gate_status)
            .bind(job.id)
            .bind(ordinal as i64)
            .execute(pool)
            .await;
            if let Err(error) = written {
                tracing::warn!(job_id = job.id, ordinal, %error, "could not record a red gate that still has a retry");
                return Step::Stopped;
            }
            say(
                pool,
                job,
                "job_gate_failed",
                &format!(
                    "job {} at item {}: the gate failed with exit code {exit_code}, so the item is \
                     being implemented again with what the gate said (attempt {attempts} of \
                     {retries} allowed)",
                    job.id,
                    ordinal + 1
                ),
            )
            .await;
            return Step::Continued;
        }
    }

    // Red: take the rejected work off the branch before anything else touches it.
    //
    // Two halves with a durable mark between them. The item is written `reverted` first, then
    // [`settle_revert`] does the reset and writes the final status. A restart between the two leaves
    // `reverted` on the row and `next_step` answers `SettleRevert`, so the reset is finished
    // instead of the queue carrying on over a tree still holding what the gate rejected.
    //
    // `Errored` is NOT reverted, where the plan for this chunk said both should be. A gate that
    // would not run measured nothing, so the work it did not judge might be perfectly good, and the
    // job stops here and hands the branch to a person either way. Reverting would destroy work whose
    // only crime is that nothing looked at it. `Failed` is different in kind: something looked, and
    // said no.
    if matches!(outcome, crate::gate::GateOutcome::Failed { .. }) {
        let marked = sqlx::query(
            "UPDATE job_items SET status = 'reverted', gate_status = ? WHERE job_id = ? AND ordinal = ?",
        )
        .bind(gate_status)
        .bind(job.id)
        .bind(ordinal as i64)
        .execute(pool)
        .await;
        if let Err(error) = marked {
            // The item is still `implemented`/`merging`, so the next pass gates it again.
            tracing::warn!(job_id = job.id, ordinal, %error, "could not mark a red item for revert");
            return Step::Stopped;
        }
        return settle_revert(state, job, ordinal, note).await;
    }

    let written = async {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "UPDATE job_items SET status = ?, gate_status = ?, checkpoint_sha = ?
             WHERE job_id = ? AND ordinal = ?",
        )
        .bind(item_status)
        .bind(gate_status)
        .bind(checkpoint_sha.as_deref())
        .bind(job.id)
        .bind(ordinal as i64)
        .execute(&mut *tx)
        .await?;
        queue_the_verdict(&mut tx, job.id, ordinal as i64).await;
        tx.commit().await
    }
    .await;
    if let Err(error) = written {
        tracing::warn!(job_id = job.id, ordinal, %error, "could not record a gate verdict");
        return Step::Stopped;
    }
    credit_the_briefing(pool, job, ordinal).await;

    if let Some(note) = note {
        say(
            pool,
            job,
            "job_gate_failed",
            &format!("job {} at item {}: {note}", job.id, ordinal + 1),
        )
        .await;
    }
    Step::Continued
}

/// The second half of `reverted`: put the job's branch back and write the item's final status.
///
/// Reached from [`record_gate`] right after it marks the item `reverted`, and from
/// `Next::SettleRevert` after a restart that found the mark. Idempotent: a `reset --hard` to the
/// same target twice is the same reset, and nothing else can move the job's branch while an item is
/// `reverted` (`next_step` settles it before anything else).
///
/// A revert that cannot be done writes `revert_failed` and retires the job `stopped` here, AND
/// `next_step` short-circuits on that status. `Step::Stopped` alone only ends the pass, and the next
/// tick used to carry on over the work the gate had rejected (the defect this replaces).
pub(super) async fn settle_revert(
    state: &AppState,
    job: &JobRow,
    ordinal: usize,
    note: Option<String>,
) -> Step {
    let pool = &state.pool;
    let target = match (
        job_worktree(pool, job.id).await,
        revert_point(pool, job, ordinal).await,
    ) {
        (Ok(Some((worktree, _))), Some(footing)) => {
            match crate::execution::worktree::revert_to(&worktree, &footing).await {
                Ok(()) => Ok(footing),
                Err(error) => {
                    tracing::warn!(job_id = job.id, ordinal, %error, "could not revert a red item");
                    Err(error.to_string())
                }
            }
        }
        (_, None) => {
            tracing::warn!(
                job_id = job.id,
                ordinal,
                "no footing to revert a red item to"
            );
            Err("no revert point on record".to_string())
        }
        _ => {
            tracing::warn!(job_id = job.id, ordinal, "no worktree on record to revert");
            Err("no worktree on record".to_string())
        }
    };

    match target {
        Ok(target) => {
            let written = async {
                let mut tx = pool.begin().await?;
                sqlx::query(
                    "UPDATE job_items SET status = 'gate_failed'
                     WHERE job_id = ? AND ordinal = ? AND status = 'reverted'",
                )
                .bind(job.id)
                .bind(ordinal as i64)
                .execute(&mut *tx)
                .await?;
                queue_the_verdict(&mut tx, job.id, ordinal as i64).await;
                tx.commit().await
            }
            .await;
            if let Err(error) = written {
                // The row stays `reverted`; the next pass settles it again.
                tracing::warn!(job_id = job.id, ordinal, %error, "could not settle a reverted item");
                return Step::Stopped;
            }
            credit_the_briefing(pool, job, ordinal).await;

            let spendable: Option<(i64, i64)> = sqlx::query_as(
                "SELECT i.gate_attempts, j.gate_retries
                 FROM job_items i JOIN jobs j ON j.id = i.job_id
                 WHERE i.job_id = ? AND i.ordinal = ?",
            )
            .bind(job.id)
            .bind(ordinal as i64)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            let said = note.as_deref().unwrap_or("the gate failed");
            let message = match spendable {
                Some((attempts, retries))
                    if item_state_from("gate_failed", attempts, retries)
                        == ItemState::GateRetriable =>
                {
                    format!(
                        "job {} at item {}: {said}; the job's branch was put back to {target:.12} \
                         and the item is being implemented again (attempt {attempts} of {retries} \
                         allowed)",
                        job.id,
                        ordinal + 1
                    )
                }
                _ => format!(
                    "job {} at item {}: {said}; its work was taken off the job's branch",
                    job.id,
                    ordinal + 1
                ),
            };
            say(pool, job, "job_gate_failed", &message).await;
            Step::Continued
        }
        Err(reason) => {
            let written = async {
                let mut tx = pool.begin().await?;
                sqlx::query(
                    "UPDATE job_items SET status = 'revert_failed'
                     WHERE job_id = ? AND ordinal = ? AND status = 'reverted'",
                )
                .bind(job.id)
                .bind(ordinal as i64)
                .execute(&mut *tx)
                .await?;
                // The item's own exhaustion verdict is queued with the status, as on the
                // `gate_failed` path.
                queue_the_verdict(&mut tx, job.id, ordinal as i64).await;
                tx.commit().await
            }
            .await;
            if let Err(error) = written {
                tracing::warn!(job_id = job.id, ordinal, %error, "could not record a failed revert");
            }
            credit_the_briefing(pool, job, ordinal).await;
            let _ = retire(pool, job.id, STATUS_STOPPED).await;
            say(
                pool,
                job,
                "job_gate_failed",
                &format!(
                    "job {} at item {}: the gate failed and the job's branch could not be put back \
                     ({reason}), so the work the gate rejected is still on it and the job stopped here",
                    job.id,
                    ordinal + 1
                ),
            )
            .await;
            Step::Stopped
        }
    }
}
