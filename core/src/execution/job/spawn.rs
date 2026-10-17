//! Spawning a node: the worktree, the item claim and provisioning, the loadout, and parking or releasing an item.

use super::*;

/// The job's worktree, or `None` if it has none on record.
pub(super) async fn job_worktree(
    pool: &SqlitePool,
    job_id: i64,
) -> sqlx::Result<Option<(PathBuf, String)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT path, branch FROM worktrees
         WHERE owner_kind = 'job' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(path, branch)| (PathBuf::from(path), branch)))
}

pub(super) fn artifacts_for(worktree: &Path) -> String {
    worktree
        .join(crate::execution::worktree::ARTIFACTS_DIR)
        .to_string_lossy()
        .into_owned()
}

/// Whether the spec node's brief is on disk in this worktree.
///
/// Asked of the disk and not of the view, because the view knows a spec NODE ran and the prompts
/// that name the file are about a spec FILE existing. A node that ended badly, or ended well and
/// wrote nothing, leaves the two apart — and no node may be sent to a file that is not there. The
/// plan node and the review are the two sent to it.
pub(super) async fn spec_is_written(worktree: &Path) -> bool {
    tokio::fs::metadata(
        worktree
            .join(crate::execution::worktree::ARTIFACTS_DIR)
            .join(SPEC_FILE),
    )
    .await
    .is_ok()
}

pub(super) async fn say(pool: &SqlitePool, job: &JobRow, kind: &str, summary: &str) {
    // A job is not a run, so the feed row carries no run id — writing the job's id into that column
    // would point every reader at whatever run happens to share the number.
    let _ = crate::feed::append(
        pool,
        Some(&job.project_id),
        kind,
        summary,
        None,
        Some(&crate::feed::Subject::Job(job.id)),
    )
    .await;
}

/// `say`, unless the feed already carries this exact line for this job's project.
///
/// For a row written on the way INTO a step that can still be refused after it: a replan parked for
/// want of a slot, or a review parked behind a council, comes back through the same arm on every
/// tick until it starts, and a row said on each pass is the feed of one sentence repeated that
/// `park` already refuses to write. The summaries this is given name the job and the round, so
/// "already said" is exact.
pub(super) async fn say_once(pool: &SqlitePool, job: &JobRow, kind: &str, summary: &str) {
    let said: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM feed WHERE project_id = ? AND kind = ? AND summary = ? LIMIT 1",
    )
    .bind(&job.project_id)
    .bind(kind)
    .bind(summary)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    if said.is_none() {
        say(pool, job, kind, summary).await;
    }
}

/// Starts one node, and records that the item it belongs to is now in someone's hands.
/// The item a node is being started for, and the row status its claim has to find.
///
/// The two travel together because neither is usable without the other: an ordinal says which row,
/// and `held` says what that row must still say for this pass to be the one allowed to start it.
/// `held` is never guessed here — it comes from [`ItemState::claimable_as`], applied to the verdict
/// the caller's `JobView` already carries.
#[derive(Debug, Clone, Copy)]
pub(super) struct ItemClaim {
    pub(super) ordinal: usize,
    /// The status the item is in right now: `pending` for an item nobody has attempted, `gate_failed`
    /// for one the gate rejected with a retry left. Also what [`release_item`] puts back, so a claim
    /// that has to be given up returns the item to what it was rather than to what it resembled.
    pub(super) held: &'static str,
}

/// What this item needs to be given a checkout of its own, or `None` if it should work in the
/// job's.
///
/// `None` for four different reasons and they are all the same answer, which is why they are read
/// here rather than at the call site: the job has no team, the item has no row, the job has no
/// branch to be born on, or the branch has no tip. **The last two are the ones worth naming.** An
/// item's tree is born on the tip of its job's branch as the last gate left it, and that is the
/// whole of what makes a dependency graph mean anything — an item whose dependency has just landed
/// starts WITH that work. There is no honest fallback: `None` would give git's default, the project
/// checkout's HEAD, which is a commit with none of this job's work in it, and the graph would be
/// decorative. Falling back to the shared tree is slower and correct; falling back to `master` is
/// faster and wrong.
pub(super) async fn item_provisioning(
    pool: &SqlitePool,
    job: &JobRow,
    stage: &'static str,
    ordinal: usize,
) -> Option<crate::execution::runs::JobItem> {
    let team_id: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT team_id FROM jobs WHERE id = ?")
            .bind(job.id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten();
    team_id?;

    let item_id: i64 =
        sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = ?")
            .bind(job.id)
            .bind(ordinal as i64)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()?;

    // The job's own checkout stands on the tip of the job's branch — it is where its nodes have
    // been committing all along — so its HEAD is the commit an item should be born on, read from
    // the tree rather than from a ref name that a second worktree could be holding.
    let (path, _) = job_worktree(pool, job.id).await.ok().flatten()?;
    let base = crate::execution::worktree::head_sha(&path).await.ok()?;

    Some(crate::execution::runs::JobItem {
        job_id: job.id,
        item_id,
        stage,
        base,
        // Filled by `spawn_node` once the loadout is resolved.
        loadout: None,
    })
}

/// Brings the job's branch into this item's own checkout, and says what happened.
///
/// `None` when there is nothing to do or nothing to do it to: no team, no checkout of the item's
/// own, or the item is starting for the first time. In every one of those the item works where it
/// always worked and the prompt says nothing extra.
///
/// **A conflict is left staged, and that is the design rather than a fallback.** The alternative —
/// telling the agent to merge — cannot work: any `git merge` an agent runs goes to the approval
/// queue, and what the agent would be asking permission for is the merge that just failed. It would
/// circle, and no wording gets it out, because the refusal is structural. With the conflict already
/// in the files, the agent does what an agent does: edits and commits. `worktree::stage_conflict`
/// carries the same argument for the VCS resolver, and this is the same inversion for a job item.
pub(super) async fn catch_up_the_item(
    pool: &SqlitePool,
    job: &JobRow,
    ordinal: usize,
) -> Option<String> {
    let item_id: i64 =
        sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = ?")
            .bind(job.id)
            .bind(ordinal as i64)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()?;
    let tree: Option<String> = sqlx::query_scalar(
        "SELECT path FROM worktrees
         WHERE owner_kind = 'item' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(item_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let tree = std::path::PathBuf::from(tree?);
    let (_, job_branch) = job_worktree(pool, job.id).await.ok().flatten()?;

    match crate::execution::worktree::catch_up(&tree, &job_branch).await {
        Ok(crate::execution::worktree::CatchUp::Clean) => Some(format!(
            "

Your working tree has been brought up to date with `{job_branch}`, the branch this              job is building. Everything other items have landed since you last ran is now here,              and it merged cleanly.
"
        )),
        Ok(crate::execution::worktree::CatchUp::Conflicted) => Some(format!(
            "

Merging `{job_branch}` — the branch this job is building — into your work              conflicts, and the merge has been left STAGED in your working tree: the conflict              markers are in the files and MERGE_HEAD is set.

             Resolve the conflicted files and commit. Do NOT run `git merge`, `git merge --abort`              or `git rebase`: the merge you would be asking for is the one already staged here, and              the request would be refused. Editing and committing is the whole of the work.
"
        )),
        Err(error) => {
            // Not a reason to refuse to start. The checkout still holds the item's own work and the
            // node can still make progress in it; what is lost is being up to date, and the merge
            // that follows will meet the same divergence and say so where it can be acted on.
            tracing::warn!(job_id = job.id, ordinal, %error, "could not bring the job's branch into an item's tree");
            None
        }
    }
}

/// What a node's briefing needs to know about WHO is working: the team the job belongs to, the
/// agent an item was given to, and the files that item declares.
///
/// Every read is best-effort. A node with no team, no item, or a row that cannot be read is briefed
/// with the links it does have, because refusing to start the node over the advice about the node
/// would fail the work for the sake of the advice.
pub(super) async fn node_loadout(
    pool: &SqlitePool,
    job: &JobRow,
    item: Option<ItemClaim>,
) -> (Option<String>, Option<String>, Vec<String>) {
    let team_read =
        sqlx::query_scalar::<_, Option<String>>("SELECT team_id FROM jobs WHERE id = ?")
            .bind(job.id)
            .fetch_optional(pool)
            .await;
    let team = match team_read {
        Ok(team) => team.flatten(),
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read a job's team; its node is briefed without it");
            None
        }
    };
    let Some(ItemClaim { ordinal, .. }) = item else {
        return (team, None, Vec::new());
    };
    let row = match sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT agent_id, files FROM job_items WHERE job_id = ? AND ordinal = ?",
    )
    .bind(job.id)
    .bind(ordinal as i64)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        Err(error) => {
            tracing::warn!(job_id = job.id, ordinal, %error, "could not read an item's agent; its node is briefed without it");
            None
        }
    };
    let (agent, files) = row.unwrap_or((None, None));
    // A hint that will not parse is no hint, as `start_item` treats it.
    let files: Vec<String> = files
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .unwrap_or_default();
    (team, agent, files)
}

pub(super) async fn spawn_node(
    state: &AppState,
    job: &JobRow,
    stage: &'static str,
    prompt: String,
    item: Option<ItemClaim>,
    worktree: (PathBuf, String),
    no_room: NoRoom,
) -> Step {
    let pool = &state.pool;

    // Claimed BEFORE the run exists, so a creation that succeeds and a bookkeeping write that fails
    // cannot leave an item looking untouched while a node works on it — the next pass would start a
    // second node on the same tree.
    //
    // Still a compare-and-swap, and the zero-rows branch below is still load-bearing: it is what
    // stops two passes starting two nodes on one tree. What changed is only that the status it
    // compares against is passed in rather than hard-coded — a hard-coded `'pending'` silently
    // refused every retriable item, which read as the job stopping for no reason.
    // The gate verdict belongs to the attempt it measured, so every new attempt clears it in the
    // same compare-and-swap that claims the item. This matters after a replan: the Superseded arm of
    // `brief::item_verdict` reads the stored verdict before it falls back to the latest run.
    if let Some(ItemClaim { ordinal, held }) = item {
        let claimed = sqlx::query(
            "UPDATE job_items SET status = 'running', gate_status = NULL
             WHERE job_id = ? AND ordinal = ? AND status = ?",
        )
        .bind(job.id)
        .bind(ordinal as i64)
        .bind(held)
        .execute(pool)
        .await;
        match claimed {
            Ok(result) if result.rows_affected() == 1 => {}
            Ok(_) => return Step::Stopped,
            Err(error) => {
                tracing::warn!(job_id = job.id, %error, "could not claim a job item");
                return Step::Stopped;
            }
        }
    }

    let (path, branch) = worktree;

    // What the owner said to this job while it was running, appended to the brief this node was
    // handed rather than put in its place. Here and not in each caller because this is the one seam
    // every node kind passes through — plan, implement, gate retry, replan, review — and a note
    // addressed to "whichever node comes next" must not depend on which kind that turned out to be.
    //
    // Read AFTER the claim and marked delivered only once a run exists, which is the whole order:
    // a note spent on a node that never started is lost silently, and the owner learns about it by
    // watching the job finish without doing what they asked. Best-effort on the way in — a queue
    // that cannot be read is not a reason to refuse to start the node, only a reason to say so.
    let waiting = match crate::notes::pending(pool, job.id).await {
        Ok(waiting) => waiting,
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read a job's notes");
            Vec::new()
        }
    };
    // The query is the node's own task text, before catch-up, notes, or knowledge are appended.
    let task_text = prompt.clone();
    let mut prompt = prompt;
    // Before the notes and before the lessons, because it is about the CHECKOUT this node is
    // about to open its editor in, and everything else is about the work. An item going round
    // again — because its gate went red, or because its merge conflicted — has a checkout of its
    // own that has been standing still while the job's branch moved on. Bringing the branch in is
    // what makes the second attempt an attempt at the CURRENT state rather than at the state the
    // first one saw.
    //
    // This is the contract `GateRetriable` used to name the other way round: "in the tree exactly
    // as the rejected attempt left it". It still is that, plus everything that landed since — and
    // that addition is what a red gate caused by somebody else's merge needs in order to be
    // fixable at all.
    if let Some(ItemClaim { ordinal, .. }) = item
        && let Some(block) = catch_up_the_item(pool, job, ordinal).await
    {
        prompt.push_str(&block);
    }
    if let Some(block) = crate::notes::render(&waiting) {
        prompt.push_str(&block);
    }

    // What earlier work learned, appended at the same seam and for the same reason: this is the
    // one place every node kind passes through, and a lesson that only reached implement nodes
    // would be a lesson the planner keeps rediscovering. After the notes deliberately — a note is
    // what the owner is saying NOW about this job, and it should be the last thing read.
    //
    // Resolved through the loadout, so the chain is machine -> project -> team -> agent -> job: a
    // team's item reads its own agent's memory and its team's besides the job's, and no other
    // agent's. Best-effort, like the notes above: a layer that cannot be read is a reason to say
    // so, never a reason to refuse to start the node.
    let (team, agent, files) = node_loadout(pool, job, item).await;
    let loadout = crate::loadout::resolve(
        pool,
        &crate::loadout::LoadoutInput {
            agent: agent.as_deref(),
            team: team.as_deref(),
            project: Some(&job.project_id),
            job: Some(job.id),
            task_text: &task_text,
            node: crate::loadout::node_kind(stage),
            files: &files,
            // The tools and the context come out of the same resolution as the memory. The job-node
            // box owns the built-ins and a job node has never read its agent's `tool_policy`, so the
            // policy is `mcp_only` for every node; and every node works in a checkout, so a folder
            // ref can be handed over as a directory to add.
            equipment: Some(crate::loadout::Equipment {
                tool_policy: "mcp_only",
                base: crate::mcp_tools::JOB_NODE_BASE,
                extras: crate::mcp_tools::JOB_NODE_EXTRAS,
                managed_root: state.files_root.as_deref(),
                has_cwd: true,
            }),
        },
    )
    .await;
    prompt.push_str(&loadout.block);
    let briefing = loadout.brief;
    let run_tools = loadout.run.clone();

    // An item of a job a team directs gets a checkout of its own; everything else — every node of
    // every job without a team, and this job's own plan, replan and review nodes — works in the
    // job's, exactly as before.
    //
    // Keyed on the item and on the team together, and both halves matter. Without a team there is
    // no reason to pay for a second checkout, and the sequential job is what this must not change.
    // Without an item there is nothing to name a tree after: a plan node has no row in `job_items`,
    // and a review node reads the branch the whole job produced rather than any one item's.
    let own_tree = match item {
        Some(ItemClaim { ordinal, .. }) => item_provisioning(pool, job, stage, ordinal).await,
        None => None,
    };
    let created = match own_tree {
        Some(mut item) => {
            item.loadout = run_tools.clone();
            crate::execution::runs::create_job_item_run(
                state,
                prompt,
                job.project_id.clone(),
                job.project_root.clone(),
                item,
            )
            .await
        }
        None => {
            crate::execution::runs::create_job_node_run(
                state,
                prompt,
                job.project_id.clone(),
                job.project_root.clone(),
                crate::execution::runs::JobNode {
                    job_id: job.id,
                    stage,
                    worktree_path: path.to_string_lossy().into_owned(),
                    branch,
                    item_ordinal: item.as_ref().map(|claim| claim.ordinal),
                    loadout: run_tools.clone(),
                },
            )
            .await
        }
    };

    match created {
        Ok(run_id) => {
            // The run exists, so the words are in a prompt something will read: only now do they
            // leave the queue. Unconsumed, one sentence typed at midnight would be appended to item
            // 4, item 5, the replan and the review, each of them reading it as something newly said
            // about the work in front of it.
            let delivered = waiting.iter().map(|note| note.id).collect::<Vec<_>>();
            if let Err(error) = crate::notes::mark_delivered(pool, &delivered, run_id).await {
                // Said out loud rather than swallowed: the failure this leaves is a note delivered
                // again to the next node, which is confusing but not silent, and there is nothing
                // to undo — the run is already started.
                tracing::warn!(job_id = job.id, run_id, %error, "could not mark a job's notes delivered");
            }
            if let Some(briefing) = briefing.as_ref() {
                let item_id = match item {
                    Some(ItemClaim { ordinal, .. }) => match sqlx::query_scalar(
                        "SELECT id FROM job_items WHERE job_id = ? AND ordinal = ?",
                    )
                    .bind(job.id)
                    .bind(ordinal as i64)
                    .fetch_optional(pool)
                    .await
                    {
                        Ok(id) => id,
                        // A NULL item_id is never credited, so say this failure out loud.
                        Err(error) => {
                            tracing::warn!(job_id = job.id, run_id, ordinal, %error, "could not resolve a node's item; its briefing trace is written without one and will never be credited");
                            None
                        }
                    },
                    None => None,
                };
                // The trace names a run, which exists only now, in the same order as note delivery.
                // Record it exactly once per run because its primary key rejects a second pass; the
                // item is what the eventual verdict credits.
                if let Err(error) =
                    crate::brief::record(pool, run_id, item_id, &briefing.trace).await
                {
                    tracing::warn!(job_id = job.id, run_id, %error, "could not record a node's briefing trace");
                }
            }
            // Whether the job was still live to record the node against. `None` is a write that
            // failed, which says nothing about the job and is left alone as before.
            let still_live = if let Some(ItemClaim { ordinal, .. }) = item {
                if let Err(error) =
                    sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = ?")
                        .bind(run_id)
                        .bind(job.id)
                        .bind(ordinal as i64)
                        .execute(pool)
                        .await
                {
                    // Said out loud: an item that does not name its node is judged without it.
                    tracing::warn!(job_id = job.id, run_id, ordinal, %error, "could not record a node on its item");
                }
                // Denormalised on purpose: the item statuses are what a resume actually reads, and
                // this is here so a row nobody joins does not read as a lie.
                sqlx::query(concat!(
                    "UPDATE jobs SET status = 'implementing', stage_cursor = ?
                     WHERE id = ? AND status IN ",
                    live_statuses_sql!()
                ))
                .bind(ordinal as i64)
                .bind(job.id)
                .execute(pool)
                .await
                .ok()
                .map(|moved| moved.rows_affected() > 0)
            } else if stage == "review" {
                set_live_status(pool, job.id, "reviewing").await.ok()
            } else {
                // A spec, plan or replan node moves no status of its own, so it only READS whether
                // the job is still live. It used to answer `None` here, and a cancel that landed
                // while one of these nodes was being created left it running on a dead job.
                sqlx::query_scalar::<_, i64>(concat!(
                    "SELECT COUNT(*) FROM jobs WHERE id = ? AND status IN ",
                    live_statuses_sql!()
                ))
                .bind(job.id)
                .fetch_one(pool)
                .await
                .ok()
                .map(|live| live > 0)
            };
            if still_live == Some(false) {
                // The job ended while this node was being created — a cancel that swept the job's
                // runs before this one existed. Left running, it would edit a tree nobody drives
                // any more, so it is stopped the way that cancel would have stopped it.
                tracing::info!(
                    job_id = job.id,
                    run_id,
                    "job ended while its node started; cancelling the node"
                );
                crate::execution::runs::finalize_termination(state, run_id, STATUS_CANCELLED).await;
            }
            Step::Stopped
        }
        // Decision 15: nobody holds the project's exclusivity slot between two of a job's nodes, so
        // an ordinary run can take it. Losing that race is not a failure of the work — the worktree
        // and every item gated green are untouched — so the job parks and tries again. The
        // four-hour ceiling is what stops a starved job from waiting forever.
        Err(crate::execution::runs::CreateRunError::Busy) => {
            release_item(pool, job, item).await;
            match no_room {
                NoRoom::Park => {
                    park(
                        state,
                        job,
                        "slot",
                        "another run holds the project's worktree slot",
                    )
                    .await
                }
                // Nothing to park: this job already has an item going, and the batch simply came
                // out smaller than the fold proposed. Parking here would tell a reader the job is
                // blocked while it is in fact working, and the park would be lifted by the next
                // tick anyway.
                NoRoom::ShrinkTheBatch => Step::Stopped,
            }
        }
        // The same trade as a held slot — the work is untouched, so the job parks and the next pass
        // asks again — under a reason of its own. On 2026-09-14 this arm did not exist: a disk below
        // the floor came back as `Busy`, the job read `wait_reason slot` and fed "another run holds
        // the project's worktree slot", and anybody reading it went looking for a run that was not
        // there. Only the `worktree_provision_failed` line after it said the disk was full.
        //
        // The detail is the refusal itself, free space and floor, so the job's own line says what
        // to go and clear. `park` says it once per reason rather than per sentence, so the MiB
        // figure drifting between ticks does not flood the feed.
        Err(crate::execution::runs::CreateRunError::NoRoomOnDisk(refusal)) => {
            release_item(pool, job, item).await;
            match no_room {
                NoRoom::Park => {
                    park(
                        state,
                        job,
                        WAIT_DISK,
                        &format!("the disk is too full for another checkout: {refusal}"),
                    )
                    .await
                }
                NoRoom::ShrinkTheBatch => Step::Stopped,
            }
        }
        // A database error is the store hiccuping, not the work being wrong: the item goes back and
        // the next tick asks again, rather than failing a job whose worktree and gated items are
        // untouched. Nothing is parked, because no wait reason describes it.
        Err(error) if create_error_is_transient(&error) => {
            release_item(pool, job, item).await;
            tracing::warn!(
                job_id = job.id,
                %error,
                "could not create the {stage} node's run; trying again next tick"
            );
            Step::Stopped
        }
        Err(error) => {
            release_item(pool, job, item).await;
            let _ = finish(pool, job.id, Outcome::Failed).await;
            say(
                pool,
                job,
                "job_failed",
                &format!("job {} could not start its {stage} node: {error}", job.id),
            )
            .await;
            Step::Stopped
        }
    }
}

/// Whether a failed run creation says something about the moment rather than about the job: a
/// database error passes, an invalid request or a worktree that cannot be provisioned does not.
fn create_error_is_transient(error: &crate::execution::runs::CreateRunError) -> bool {
    matches!(error, crate::execution::runs::CreateRunError::Db(_))
}

#[cfg(test)]
mod create_error_tests {
    use super::create_error_is_transient;
    use crate::execution::runs::CreateRunError;

    #[test]
    fn a_database_error_is_transient_and_the_rest_are_not() {
        assert!(create_error_is_transient(&CreateRunError::Db(
            sqlx::Error::PoolTimedOut
        )));
        assert!(!create_error_is_transient(&CreateRunError::Invalid("x")));
        assert!(!create_error_is_transient(&CreateRunError::Worktree(
            std::io::Error::other("boom")
        )));
    }
}

/// Gives back a claim whose node never started.
///
/// Puts back the status the claim TOOK, not a fixed `pending`. The difference is a retry: an item
/// released to `pending` would have lost the red gate that made it retriable while keeping the
/// `gate_attempts` that gate cost it, so a budget of one would be spent on an attempt that never
/// ran, and the item would come back as though nothing had ever measured it.
///
/// The claim clears the previous attempt's gate verdict. Giving up a retry claim therefore restores
/// `failed` alongside `gate_failed`, the same pairing [`record_gate`] writes, so a later replan cannot
/// mistake that released red gate for an ungated attempt.
pub(super) async fn release_item(pool: &SqlitePool, job: &JobRow, item: Option<ItemClaim>) {
    let Some(ItemClaim { ordinal, held }) = item else {
        return;
    };
    let _ = sqlx::query(
        "UPDATE job_items
         SET status = ?,
             gate_status = CASE WHEN ? = 'gate_failed' THEN 'failed' ELSE gate_status END
         WHERE job_id = ? AND ordinal = ? AND status = 'running'",
    )
    .bind(held)
    .bind(held)
    .bind(job.id)
    .bind(ordinal as i64)
    .execute(pool)
    .await;
}

/// Parks a job, and says so once rather than every thirty seconds.
pub(super) async fn park(state: &AppState, job: &JobRow, reason: &str, detail: &str) -> Step {
    let pool = &state.pool;
    if let Err(error) = wait(pool, job.id, reason).await {
        tracing::warn!(job_id = job.id, %error, "could not park a job");
        return Step::Stopped;
    }
    // Only when the reason changed. A job parked for three hours would otherwise write a feed row
    // every tick, which is 360 lines saying the same thing and a feed nobody reads afterwards.
    if job.wait_reason.as_deref() != Some(reason) || job.status != "waiting" {
        say(
            pool,
            job,
            "job_waiting",
            &format!("job {} is waiting: {detail}", job.id),
        )
        .await;
    }
    Step::Stopped
}
