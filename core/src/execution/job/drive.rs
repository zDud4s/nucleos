//! The loop that drives jobs: `advance`, the council before review, starting items and the tick.

use super::*;

/// How long a job nobody asked for may live, counting every minute it spent parked.
///
/// Decision 14. Counting `waiting` is the whole point: without it, parking would be a way around
/// the ceiling, and a starved job would hold a worktree and the project's slot indefinitely. A
/// nightly job that starts at 03:00 is retired by 07:00, which is why the attention brake rarely
/// has to be the thing that stops it.
///
/// That sentence is the tell, and it is why this is no longer the only ceiling: every word of the
/// reasoning above is about a **scheduled** job. See `COMMISSIONED_JOB_LIFETIME`.
pub const MAX_JOB_LIFETIME: chrono::Duration = chrono::Duration::hours(4);

/// How long a job the owner asked for and funded may live.
///
/// **Four hours was a pacing decision about proactive autonomy, and it was retiring commissioned
/// work.** The same argument as the house budget one stage earlier: a limit written to stop the
/// daemon drifting on its own was governing a task a person had explicitly asked for. Measured
/// 2026-08-30 on job 23, which lost roughly eighty minutes of its four hours parked on the very
/// budget brake that turned out not to apply to it — parked time counts, so a brake that should
/// never have fired also ate the ceiling.
///
/// And the plainest reason of all: **a night is eight hours, not four.** A ceiling shorter than the
/// span somebody asks for makes the request impossible to satisfy however well everything else runs.
///
/// Twelve, not infinity, and that is deliberate. The real bound on a commissioned job is its
/// allowance — it stops when the money the owner named runs out, which is a truer limit for work
/// somebody is paying for than a wall clock is. This stays as the backstop for the case money
/// cannot catch: a job that idles without spending would otherwise hold a worktree and the
/// project's slot forever. `commissioned_with_an_allowance` is what picks between the two, so a
/// job with no allowance keeps the four hours and is never left with no bound at all.
pub const COMMISSIONED_JOB_LIFETIME: chrono::Duration = chrono::Duration::hours(12);

/// How long a live job may have no worktree on record before a pass calls it broken.
///
/// `start` writes the row live, claims the slot, runs `git worktree add` and records the tree, in
/// that order; until the last step lands the job has nothing to run in, and that is not a fault.
/// Generous on purpose: a `git worktree add` on a large repository on a busy disk takes minutes,
/// and failing early costs a job somebody asked for while waiting costs a few idle ticks. Past
/// it, the start that should have recorded the tree is gone — a request future dropped mid-start,
/// say — and the job is failed as before, which is what gives its slot back.
pub(super) const PROVISIONING_GRACE: chrono::Duration = chrono::Duration::minutes(10);

/// PURE: whether a job with no worktree on record may still be inside `start`.
///
/// An unreadable `created_at` answers `false`, the behaviour from before the grace existed: a job
/// that cannot say how old it is gets no open-ended pass to hold its slot.
pub(super) fn still_provisioning(created_at: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(created_at)
        .map(|created| now.signed_duration_since(created.with_timezone(&Utc)) <= PROVISIONING_GRACE)
        .unwrap_or(false)
}

/// How many moves one pass will make for a single job.
///
/// A pass stops as soon as it starts a node, so the only steps that chain are the ones that cost no
/// run: a gate, and the finish that may follow it. The bound is a backstop against a state machine
/// that decides it can move forever without ever spawning anything.
pub(super) const MAX_STEPS_PER_PASS: usize = 4;

/// What a node that cannot get a worktree slot means for the job.
///
/// The same refusal is two different facts depending on what else the job has going. With nothing
/// started, no room means the job cannot move and `waiting` is the honest status. With an item
/// already running, it means the batch is bigger than the project's ceiling and came out smaller —
/// which is what `next_step` deliberately left the driver to discover, since a slot count read
/// inside a pure function is a number somebody else can take before the claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoRoom {
    Park,
    ShrinkTheBatch,
}

/// Whether a job can move again in the same pass.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pass {
    /// A move was made that costs no run; the next one can follow immediately.
    Continued,
    /// Nothing more happens until a node lands, a brake lifts, or somebody looks at it.
    Stopped,
}

/// What the reason column says while a job is waiting on a council.
///
/// One word, like `budget` and `slot` beside it, because `park` compares it against the reason the
/// pass started with to decide whether the feed hears about this again. A detail sentence that
/// changed between ticks would flood the feed with one line every thirty seconds for the whole
/// deliberation; the id it names does not change, so it does not.
pub(super) const WAIT_COUNCIL: &str = "council";

/// What the reason column says while a job's next checkout is refused for want of disk.
///
/// Apart from `slot` because the two ask different hands: a held slot clears when another run
/// ends, and a full disk clears only when somebody deletes something. Like the rest, it is a note
/// and not a latch — the next pass resumes the job and asks the disk again.
pub(super) const WAIT_DISK: &str = "disk";

/// What the council has to say before a job's review node starts.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BeforeReview {
    /// Start the node. `council_file` is whether a synthesis is sitting beside the queue for it.
    Go { council_file: bool },
    /// A council is deliberating. The job parks and the next tick asks again.
    Deliberating,
}

/// The question the council is asked on a job's behalf.
///
/// **Written for seats that cannot see the repository, and that is not a limitation being worked
/// around — it is the honest shape of the thing.** A council seat is spawned with `cwd: None`, so
/// it has no tree, no diff and no `plan.json`; asking it to review the change would get an answer
/// invented out of the question's own words. What it CAN do from the task alone is say where a task
/// of this shape usually goes wrong, and that is a genuinely useful thing to hand somebody who is
/// about to read a diff — it is the difference between a reviewer scanning and a reviewer looking
/// somewhere.
pub(super) fn council_review_question(task: &str) -> String {
    format!(
        "An autonomous coding job was asked to do the task below and has finished it. A reviewer is \
         about to read the resulting diff. You cannot see that diff, the repository or the queue — \
         answer from the task alone, and do not pretend otherwise.\n\n\
         Say where a change satisfying this task is most likely to have gone wrong: the mistake that \
         still passes a test suite, the requirement that gets read as done when it was only \
         started, and the thing a diff of this shape usually leaves out. Be specific to this task, \
         not to software in general.\n\n\
         The task:\n\n{task}"
    )
}

/// Puts the council in front of a job's review node, when the owner asked for one.
///
/// **The council ADVISES the reviewer; it does not replace it.** The `review` node still runs and
/// still writes the opinion the proposal carries — §5.5 gives ship/no-ship to the deterministic
/// gate and nothing here moves it. All that changes is that a synthesis may be sitting in the
/// artifacts directory when the node starts, which is a directory `review_prompt` already sends it
/// to. `ReviewState` gains no variant and `next_step` stays pure, which is the whole reason this
/// lives here in the driver rather than in the state machine.
///
/// **Every failure walks the job on rather than holding it.** A council that could not start, one
/// that ended `error` or `cancelled`, one whose row has been pruned, one whose chairman left no
/// transcript, a file that would not write — each of them answers `Go { council_file: false }`, and
/// the job reviews exactly as a job with no council configured does. The asymmetry is deliberate:
/// the cost of proceeding unadvised is a slightly worse review, and the cost of waiting is a job
/// that holds a worktree and its project's concurrency slot until the lifetime ceiling retires it.
///
/// The one thing that DOES hold the job is a council still deliberating, which is bounded by the
/// council's own `timeout_seconds` and settled at daemon startup by `council::reconcile` — so a
/// parked job always eventually sees a terminal status, even across a crash.
pub(super) async fn council_before_review(
    state: &AppState,
    job: &JobRow,
    artifacts: &str,
) -> BeforeReview {
    let pool = &state.pool;
    let unadvised = BeforeReview::Go {
        council_file: false,
    };

    let existing: Option<String> = match sqlx::query_scalar::<_, Option<String>>(
        "SELECT review_council_id FROM jobs WHERE id = ?",
    )
    .bind(job.id)
    .fetch_optional(pool)
    .await
    {
        Ok(value) => value.flatten(),
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read a job's review council");
            return unadvised;
        }
    };

    let Some(council_id) = existing else {
        // Asked of the runtime and not of the row: no roster at all and a roster that did not ask
        // for this answer the same way, which is the shipped state and must stay a silent no-op.
        //
        // **Read only on this branch, never above the column.** The flag decides whether to CONVENE
        // a council, not whether to finish reading one. An owner who switches the consumer off — or
        // restarts a daemon onto an edited roster — while a job is parked on a deliberation already
        // paid for should get the synthesis it bought, not a job left waiting on a council nothing
        // will ever look at again.
        if !state.council.advises_job_review() {
            return unadvised;
        }
        let question = council_review_question(job.prompt.as_deref().unwrap_or_default());
        let council_id = match crate::council::start(state, &question, None).await {
            Ok(id) => id,
            // Unreachable while `advises_job_review` implies a roster, and answered silently anyway:
            // "there is no council" is not an error a job has done anything about.
            Err(crate::council::StartError::NotConfigured) => return unadvised,
            Err(error) => {
                tracing::warn!(job_id = job.id, %error, "a job's review council would not start");
                return unadvised;
            }
        };
        // Written before the job parks, and a failed write walks the job on. The council is running
        // either way — that money is spent — but a job that parked without recording the id would
        // convene a SECOND council on its next tick and park on that one too, which is the failure
        // this order exists to make impossible.
        if let Err(error) = sqlx::query("UPDATE jobs SET review_council_id = ? WHERE id = ?")
            .bind(&council_id)
            .bind(job.id)
            .execute(pool)
            .await
        {
            tracing::warn!(
                job_id = job.id,
                council_id,
                %error,
                "a job's review council started and could not be recorded; reviewing unadvised"
            );
            return unadvised;
        }
        return BeforeReview::Deliberating;
    };

    let row = match crate::council::get_council_row(pool, &council_id).await {
        Ok(Some(row)) => row,
        // The council is gone: pruned past its ninety days, or deleted. There is nothing to wait
        // for and nothing to read, so this is the "council that failed" case by another door.
        Ok(None) => {
            tracing::warn!(
                job_id = job.id,
                council_id,
                "a job's review council is no longer on record; reviewing unadvised"
            );
            return unadvised;
        }
        Err(error) => {
            tracing::warn!(job_id = job.id, council_id, %error, "could not read a job's review council");
            return unadvised;
        }
    };
    if !row.is_settled() {
        return BeforeReview::Deliberating;
    }

    // The column is deliberately NOT cleared here. See `0137_job_review_council.sql`: `advance`
    // reaches this arm on every tick until the node actually starts, and a review that parks for a
    // worktree slot would come back to a NULL column and convene a second council. Rewriting the
    // same file on a retry is idempotent; paying for a second deliberation is not.
    let Some(synthesis) = crate::council::synthesis_of(pool, &row).await else {
        // Terminal with no synthesis: `error`, `cancelled`, or a chairman whose transcript was
        // pruned. Nothing to say, and saying nothing must not stop the review.
        return unadvised;
    };

    let path = std::path::Path::new(artifacts).join(COUNCIL_FILE);
    if let Err(error) = tokio::fs::write(&path, synthesis.as_bytes()).await {
        tracing::warn!(
            job_id = job.id,
            council_id,
            %error,
            "could not write a job's council synthesis; reviewing unadvised"
        );
        return unadvised;
    }
    BeforeReview::Go { council_file: true }
}

/// C6: the workflow walker answers beside the legacy decision on every pass, and a disagreement is
/// said, never acted on. Only the legacy road (`WALKER_DRIVES == false`) runs it: once the walker
/// is the authority there is nothing left to compare it with.
const WALKER_SHADOW: bool = true;

/// Asks `engine::decide` what the walker would do and compares it with the legacy `Next`. Changes
/// nothing: a walker that cannot answer is logged, a disagreement is said once per (job, round,
/// pair of answers). `Stuck` has no legacy word and is always a disagreement.
pub(super) async fn shadow_differential(state: &AppState, job: &JobRow, round: i64, legacy: &Next) {
    let (step, graph) = match engine::decide(state, job).await {
        Ok((step, _facts, graph)) => (step, graph),
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "the workflow walker could not answer");
            return;
        }
    };
    let walker = bridge::step_as_next(&step, &graph.graph);
    if walker.as_ref() == Some(legacy) {
        return;
    }
    tracing::warn!(
        job_id = job.id,
        ?step,
        ?legacy,
        "the workflow walker diverged from the driver"
    );
    say_once(
        &state.pool,
        job,
        "job_walker_diverged",
        &format!(
            "job {} round {}: the workflow walker answered {step:?} where the driver answered {legacy:?}",
            job.id,
            round + 1
        ),
    )
    .await;
}

/// C7: the workflow walker is the authority. `engine::decide` answers what a job does next, for
/// every job (a NULL snapshot is the embedded autopilot). Flipping this to `false` restores the
/// legacy `next_step` driver, which stays intact beside it as the C6 oracle for one release; the
/// shadow differential then runs again.
const WALKER_DRIVES: bool = true;

/// Performs one move for a job.
pub(super) async fn advance(state: &AppState, job: &JobRow, now: DateTime<Utc>) -> Pass {
    advance_with(state, job, now, WALKER_DRIVES).await
}

/// [`advance`] with the authority chosen by the caller, so a test can run both drivers on one state.
pub(super) async fn advance_with(
    state: &AppState,
    job: &JobRow,
    now: DateTime<Utc>,
    walker_drives: bool,
) -> Pass {
    let pool = &state.pool;
    let view = match load_view(pool, job.id).await {
        Ok(view) => view,
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read a job");
            return Pass::Stopped;
        }
    };
    if walker_drives {
        return advance_by_walker(state, job, now, &view).await;
    }

    let next = next_step(&view);
    if WALKER_SHADOW {
        shadow_differential(state, job, view.rounds.round, &next).await;
    }
    if matches!(next, Next::Wait) {
        return Pass::Stopped;
    }
    say_the_skip(pool, job, &view).await;
    if let Next::Finish(outcome) = &next {
        return finish_the_job(pool, job, *outcome, view.items.len()).await;
    }
    // The gate is a subprocess, not a run: it starts no CLI, consumes no quota and answers to no
    // brake. Refusing to measure an item because the owner sat down would leave the tree holding
    // edits nothing has verified, which is the state decision 10 exists to avoid.
    if let Next::RunGate { ordinal } = &next {
        return gate_item(state, job, *ordinal, view.items.len()).await;
    }
    // A merge is a subprocess too, and it goes past the brakes for the same reason plus one of its
    // own: the item's work is already done and paid for, sitting on a branch. Refusing to bring it
    // in because the owner sat down would leave it there, diverging from the job's branch for as
    // long as the brake holds — and the divergence is what the retry has to absorb.
    if let Next::MergeItem { ordinal } = &next {
        return merge_item(state, job, *ordinal).await;
    }
    // A git reset, no run and no quota: refusing it under a brake would leave the job's branch
    // holding work a gate rejected for as long as the brake holds.
    if let Next::SettleRevert { ordinal } = &next {
        return settle_revert(state, job, *ordinal, None).await;
    }
    // Past the brakes with the other two, and it is the cheapest of the three: one UPDATE, no
    // subprocess and no run. Parking a job rather than writing it would leave the item reading
    // `pending` — work a person is told is still coming — for as long as the brake holds.
    if let Next::Orphan { ordinal } = &next {
        return orphan_item(pool, job, *ordinal).await;
    }

    if let Some(pass) = hold_for_brakes(state, job, &view, now).await {
        return pass;
    }
    let worktree = match resolve_worktree(state, job, now).await {
        Ok(worktree) => worktree,
        Err(pass) => return pass,
    };
    let artifacts = artifacts_for(&worktree.0);

    match next {
        Next::SpawnSpec => spawn_autopilot_node(state, job, &view, "spec", worktree, false).await,
        Next::SpawnPlan => spawn_autopilot_node(state, job, &view, "plan", worktree, false).await,
        Next::SpawnImplement(ordinals) => {
            spawn_items(state, job, &view, &artifacts, worktree, ordinals).await
        }
        Next::SpawnReview => {
            // In front of the node rather than inside `next_step`, which stays pure: this reads
            // the database, starts a council and writes a file, and none of those belong in a
            // function whose whole value is that it can be tested with a struct literal.
            match engine::before_review(state, job, &view, &artifacts).await {
                Ok(council_file) => {
                    spawn_autopilot_node(state, job, &view, "review", worktree, council_file).await
                }
                Err(pass) => pass,
            }
        }
        Next::SpawnReplan => {
            spawn_autopilot_node(state, job, &view, "replan", worktree, false).await
        }
        // All five are answered above, before the brakes and before the worktree is resolved.
        // Reaching one here means the dispatch above stopped covering something it used to.
        Next::Wait
        | Next::Finish(_)
        | Next::RunGate { .. }
        | Next::MergeItem { .. }
        | Next::SettleRevert { .. }
        | Next::Orphan { .. } => Pass::Stopped,
    }
}

/// The walker's pass: `engine::decide` names the step, `action_of` turns it into what the driver
/// does. Waiting, finishing, fan-boundary work, command nodes and a stuck graph cost no run and go
/// BEFORE the brakes; starting an agent node, a fan instance or a replan goes after them.
async fn advance_by_walker(
    state: &AppState,
    job: &JobRow,
    now: DateTime<Utc>,
    view: &JobView,
) -> Pass {
    let pool = &state.pool;
    // A job with no worktree on record has no facts to read and so no walk to take: all it can do
    // is start its first node, which is a run, so the brakes answer first exactly as they do on
    // the legacy road (where they stand ahead of `resolve_worktree`). Without this a braked job
    // that is still being provisioned would sit `running` instead of parking or stopping.
    if matches!(job_worktree(pool, job.id).await, Ok(None))
        && let Some(pass) = hold_for_brakes(state, job, view, now).await
    {
        return pass;
    }
    // The facts the walker reads live in the worktree, so it is resolved before the decision.
    let worktree = match resolve_worktree(state, job, now).await {
        Ok(worktree) => worktree,
        Err(pass) => return pass,
    };
    let (step, facts, graph) = match engine::decide(state, job).await {
        Ok(answer) => answer,
        Err(error @ engine::DecideError::Emission(_)) => {
            // A finished node's file will read the same on every tick, so waiting would only hold
            // the job's slot until its lifetime ceiling: it ends, with the file named in the feed.
            tracing::warn!(job_id = job.id, %error, "a node's emission is unusable; stopping the job");
            let _ = finish(pool, job.id, Outcome::Stopped).await;
            say(
                pool,
                job,
                "job_graph_unreadable",
                &format!(
                    "job {} stopped: it cannot read what its workflow needs: {error}",
                    job.id
                ),
            )
            .await;
            return Pass::Stopped;
        }
        Err(error) => {
            // Not a guess at "nothing published": the job waits, and the feed says why once.
            tracing::warn!(job_id = job.id, %error, "the workflow walker could not answer");
            say_once(
                pool,
                job,
                "job_graph_unreadable",
                &format!(
                    "job {} cannot read what its workflow needs: {error}",
                    job.id
                ),
            )
            .await;
            return Pass::Stopped;
        }
    };
    let action = engine::action_of(&step, &graph.graph);
    if matches!(action, engine::Action::Wait) {
        return Pass::Stopped;
    }
    // Said ahead of both roads a closing round can take — the ending and the replan past the
    // brakes — because the skip is the reason for either.
    say_the_skip(pool, job, view).await;

    let past_the_brakes = matches!(
        action,
        engine::Action::DispatchAgent { .. }
            | engine::Action::FanOut { .. }
            | engine::Action::Replan { .. }
    );
    if past_the_brakes && let Some(pass) = hold_for_brakes(state, job, view, now).await {
        return pass;
    }

    match action {
        engine::Action::Wait => Pass::Stopped,
        engine::Action::Finish(outcome) => {
            finish_the_job(pool, job, outcome, view.items.len()).await
        }
        engine::Action::GateErrored { .. } => {
            finish_the_job(pool, job, Outcome::GateErrored, view.items.len()).await
        }
        engine::Action::Stuck { node, key } => {
            let reason = if key == "kind" {
                format!(
                    "job {} stopped at node {node}: it is not something a job can run",
                    job.id
                )
            } else {
                // `key` is `<emitting node>.<name>`: whether that node wrote a file at all is the
                // difference between a node that did nothing and one that left a key out.
                let emitter = key.split('.').next().unwrap_or_default();
                let cause = if facts.has_emitted(emitter) {
                    format!("{emitter} wrote a file without it")
                } else {
                    format!("{emitter} wrote no file")
                };
                format!(
                    "job {} stopped at node {node}: nobody published `{key}` ({cause})",
                    job.id
                )
            };
            let _ = finish(pool, job.id, Outcome::Stopped).await;
            say(pool, job, "job_graph_stuck", &reason).await;
            Pass::Stopped
        }
        engine::Action::FanBoundary(op) => match op {
            engine::FanOp::MergeItem { item, .. } => merge_item(state, job, item).await,
            engine::FanOp::SettleRevert {
                item: Some(item), ..
            } => settle_revert(state, job, item, None).await,
            engine::FanOp::SettleRevert {
                node_id,
                item: None,
            } => {
                // No state the loader writes reads as `RevertOwed`, so this is not reachable today.
                tracing::warn!(
                    job_id = job.id,
                    node_id,
                    "a singleton revert is owed and nothing settles it"
                );
                Pass::Stopped
            }
            engine::FanOp::Orphan { item, .. } => orphan_item(pool, job, item).await,
        },
        engine::Action::DispatchCommand {
            ordinal: Some(ordinal),
            ..
        } => gate_item(state, job, ordinal, view.items.len()).await,
        engine::Action::DispatchCommand {
            node_id,
            ordinal: None,
        } => run_singleton_command(state, job, &graph, &node_id, &worktree.0).await,
        engine::Action::DispatchAgent { node_id, .. } => {
            engine::dispatch_agent_node(
                state,
                job,
                engine::AgentTarget {
                    view,
                    facts: &facts,
                    graph: &graph,
                    node_id: &node_id,
                    replan: false,
                },
                worktree,
            )
            .await
        }
        // A return edge into any node but the autopilot's `plan` is a plain loop: open the next
        // round and let the walker walk back to that node, which then runs with its own protocol
        // and its own emission cleared. Only `plan` has replan semantics (the replan prompt,
        // `runs.stage = "replan"`, the plan.json queue and its ingestion).
        engine::Action::Replan { node_id } if node_id != "plan" => {
            open_a_plain_round(pool, job).await
        }
        engine::Action::Replan { node_id } => {
            engine::dispatch_agent_node(
                state,
                job,
                engine::AgentTarget {
                    view,
                    facts: &facts,
                    graph: &graph,
                    node_id: &node_id,
                    replan: true,
                },
                worktree,
            )
            .await
        }
        engine::Action::FanOut { items, .. } => {
            let artifacts = artifacts_for(&worktree.0);
            spawn_items(state, job, view, &artifacts, worktree, items).await
        }
    }
}

/// A singleton command node: runs it, and lets the row it wrote carry the verdict to the next pass
/// (the walker reads `passed`, `failed` and `gate_errored` off it and follows the edges itself). A
/// failure with no verdict edge is the only one worth a line of its own here.
async fn run_singleton_command(
    state: &AppState,
    job: &JobRow,
    graph: &crate::workflows::effective::EffectiveGraph,
    node_id: &str,
    worktree: &Path,
) -> Pass {
    let Some(node) = graph.graph.nodes.iter().find(|node| node.id == node_id) else {
        tracing::warn!(
            job_id = job.id,
            node_id,
            "asked to run a command node the graph lacks"
        );
        return Pass::Stopped;
    };
    let instance = command_node::InstanceRef {
        round: job.round,
        ordinal: None,
    };
    let result = command_node::run_command_node(state, job, node, &instance, worktree).await;
    let gate = crate::workflows::graph::is_gate(&graph.graph.edges, node_id);
    match command_node::follow_for(&result, gate) {
        command_node::CommandFollow::Parked => Pass::Stopped,
        command_node::CommandFollow::JobFails(detail) => {
            say(
                &state.pool,
                job,
                "job_node_failed",
                &format!("job {} node {node_id} failed: {detail}", job.id),
            )
            .await;
            Pass::Continued
        }
        command_node::CommandFollow::Pass
        | command_node::CommandFollow::Fail
        | command_node::CommandFollow::GateErrored => Pass::Continued,
    }
}

/// Said ahead of both roads a closing round can take — the ending and the replan past the brakes —
/// because the skip is the reason for either, and a reader who finds a round with no review in the
/// feed should find why beside it.
async fn say_the_skip(pool: &SqlitePool, job: &JobRow, view: &JobView) {
    if the_skip_decided(view) {
        say_once(
            pool,
            job,
            "job_review_skipped",
            &format!(
                "job {} round {}: no item of the round passed, so there was nothing on the branch \
                 for a review to judge and none was run",
                job.id,
                view.rounds.round + 1
            ),
        )
        .await;
    }
}

/// Opens the next round of a job whose workflow loops back to a node that is not the planner. The
/// round column is the only thing a plain loop changes: every round-scoped node reads as not
/// entered in the new round, so the walker visits the loop's nodes again. Compare-and-set on the
/// round the pass started with, so a pass racing a second one opens one round, not two.
async fn open_a_plain_round(pool: &SqlitePool, job: &JobRow) -> Pass {
    let opened = sqlx::query("UPDATE jobs SET round = round + 1 WHERE id = ? AND round = ?")
        .bind(job.id)
        .bind(job.round)
        .execute(pool)
        .await;
    match opened {
        Ok(done) if done.rows_affected() == 1 => {
            say(
                pool,
                job,
                "job_replanned",
                &format!(
                    "job {} opened round {}: its workflow loops back to an earlier node",
                    job.id,
                    job.round + 2
                ),
            )
            .await;
            Pass::Continued
        }
        Ok(_) => Pass::Stopped,
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not open the next round");
            Pass::Stopped
        }
    }
}

/// Retires the job with `outcome` and says so.
async fn finish_the_job(pool: &SqlitePool, job: &JobRow, outcome: Outcome, items: usize) -> Pass {
    if let Err(error) = finish(pool, job.id, outcome).await {
        tracing::warn!(job_id = job.id, %error, "could not finish a job");
        return Pass::Stopped;
    }
    say(
        pool,
        job,
        "job_finished",
        &format!(
            "job {} finished `{}` after {} item(s)",
            job.id,
            outcome.as_status(),
            items
        ),
    )
    .await;
    Pass::Stopped
}

/// Puts down a pending item whose dependency will not land.
async fn orphan_item(pool: &SqlitePool, job: &JobRow, ordinal: usize) -> Pass {
    let marked = sqlx::query(
        "UPDATE job_nodes SET status = 'orphaned'
         WHERE job_id = ? AND ordinal = ? AND status = 'pending'",
    )
    .bind(job.id)
    .bind(ordinal as i64)
    .execute(pool)
    .await;
    match marked {
        // The compare-and-swap matters for the same reason `spawn_node`'s does: two passes
        // reaching this at once must not both act, and an item that stopped being `pending` in
        // between is one somebody else has already decided about.
        Ok(result) if result.rows_affected() == 1 => {
            say(
                pool,
                job,
                "job_item_orphaned",
                &format!(
                    "job {} at item {}: something it depends on will not land, so it was put down without being attempted",
                    job.id,
                    ordinal + 1
                ),
            )
            .await;
            Pass::Continued
        }
        Ok(_) => Pass::Continued,
        Err(error) => {
            tracing::warn!(job_id = job.id, ordinal, %error, "could not put down an orphaned item");
            Pass::Stopped
        }
    }
}

/// The brakes, for anything about to START a run. `None` is go; `Some` is the pass that parked or
/// stopped the job.
async fn hold_for_brakes(
    state: &AppState,
    job: &JobRow,
    view: &JobView,
    now: DateTime<Utc>,
) -> Option<Pass> {
    let pool = &state.pool;
    match brakes(state, job, now).await {
        Brake::Go => None,
        Brake::Park { reason, detail } => Some(park(state, job, reason, &detail).await),
        Brake::Stop { detail } => {
            // A batch can still have nodes in flight here; a retired job is never driven again,
            // so nothing else would stop them.
            if let Err(error) = stop_nodes_in_flight(state, job.id).await {
                tracing::warn!(job_id = job.id, %error, "could not stop a stopped job's nodes");
            }
            let _ = retire(pool, job.id, STATUS_STOPPED).await;
            say(
                pool,
                job,
                "job_stopped",
                // "Unfinished" is asked of `claimable_as` rather than spelled out as a list of
                // states, because that function is already the one place saying which states are
                // work the queue still owes a run — including `GateRetriable`, an item whose gate
                // went red with a retry left. Listing the states here would be the same rule in a
                // second dialect, agreeing only until one of them was edited.
                &format!(
                    "job {} stopped with {} item(s) unfinished: {detail}",
                    job.id,
                    view.items
                        .iter()
                        .filter(|item| item.state.claimable_as().is_some())
                        .count()
                ),
            )
            .await;
            Some(Pass::Stopped)
        }
    }
}

/// The job's worktree, or the pass that ends this one without it.
///
/// Three answers, and only one of them is a broken job. `start` inserts the row live BEFORE it
/// runs `git worktree add` (the tree is keyed on the job's id, so it cannot come first), and a
/// tick landing in that window used to fail a job that was still being born. A read that errored
/// is a database hiccup rather than a fact about the job: this pass is skipped, never the job.
async fn resolve_worktree(
    state: &AppState,
    job: &JobRow,
    now: DateTime<Utc>,
) -> Result<(PathBuf, String), Pass> {
    let pool = &state.pool;
    match job_worktree(pool, job.id).await {
        Ok(Some(worktree)) => Ok(worktree),
        Err(error) => {
            tracing::warn!(
                job_id = job.id,
                %error,
                "could not read a job's worktree; skipping this pass"
            );
            Err(Pass::Stopped)
        }
        Ok(None) if still_provisioning(&job.created_at, now) => Err(Pass::Stopped),
        Ok(None) => {
            let _ = finish(pool, job.id, Outcome::Failed).await;
            say(
                pool,
                job,
                "job_failed",
                &format!(
                    "job {} has no worktree on record to run its nodes in",
                    job.id
                ),
            )
            .await;
            Err(Pass::Stopped)
        }
    }
}

/// Starts one of the autopilot's own nodes (`spec`, `plan`, `review`, `replan`) with its
/// hard-coded prompt and no launcher: the legacy road.
async fn spawn_autopilot_node(
    state: &AppState,
    job: &JobRow,
    view: &JobView,
    stage: &str,
    worktree: (PathBuf, String),
    council_file: bool,
) -> Pass {
    let Some(prompt) =
        engine::autopilot_protocol(state, job, view, stage, &worktree.0, council_file).await
    else {
        return Pass::Stopped;
    };
    spawn_node(
        state,
        job,
        NodeSpec {
            node_id: stage,
            claim: None,
            launch: super::dispatch::NodeLaunch::plain(prompt),
        },
        worktree,
        NoRoom::Park,
    )
    .await
}

/// One node per ordinal, in order, under the ONE brake reading this pass took. A brake is a
/// statement about the JOB — its budget, the owner's attention, the window — so re-reading it
/// between two items of one batch would let the second half be refused under a rule the first half
/// was allowed under, for the same job at the same moment.
async fn spawn_items(
    state: &AppState,
    job: &JobRow,
    view: &JobView,
    artifacts: &str,
    worktree: (PathBuf, String),
    ordinals: Vec<usize>,
) -> Pass {
    let pool = &state.pool;
    let mut step = Pass::Stopped;
    for (position, ordinal) in ordinals.into_iter().enumerate() {
        // Re-read between items for the reason `drive` re-reads between passes: the item
        // before this one may have parked the job for want of a slot, or ended it because
        // its run could not be created at all, and the next item must not be started on top
        // of either. `is_paused` as well as the live list, because `waiting` IS live — a
        // parked job is one that will come back, not one that is over.
        if position > 0 {
            let Ok(current) = load_job(pool, job.id).await else {
                break;
            };
            if !LIVE_STATUSES.contains(&current.status.as_str()) || is_paused(&current.status) {
                break;
            }
        }
        step = start_item(
            state,
            job,
            view,
            artifacts,
            worktree.clone(),
            ordinal,
            // The first item parks the job when the project has no room, and the ones after
            // it do not. With something already started the job is not blocked; it is at the
            // project's ceiling, and `waiting` would be a status the next tick has to undo.
            if position == 0 {
                NoRoom::Park
            } else {
                NoRoom::ShrinkTheBatch
            },
        )
        .await;
    }
    step
}

/// The items a replan needs told about: the ones that broke, worst-known-first by ordinal.
///
/// `gate_failed` and `failed` and nothing else. `skipped` is deliberately absent — a skipped item is
/// a proposal waiting for a person, not work that went wrong, and listing it here would ask the
/// replan to route around a decision that is not its to make. `gate_errored` is absent because it
/// ends the job outright, so no replan ever runs to read it.
///
/// `reverted` is never seen here, because `next_step` settles it into `gate_failed` before any
/// round can close.
///
/// A read that fails costs the replan its context and nothing else, so it answers with an empty
/// list rather than refusing: the round it is about to open is worth more than the paragraph.
pub(super) async fn unfinished_items(pool: &SqlitePool, job_id: i64) -> Vec<Unfinished> {
    sqlx::query_as::<_, (i64, String, Option<String>)>(
        "SELECT ordinal, description, gate_output FROM job_nodes
         WHERE job_id = ? AND ordinal IS NOT NULL AND status IN ('gate_failed', 'failed')
         ORDER BY ordinal",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(ordinal, description, gate_output)| Unfinished {
        ordinal: ordinal as usize,
        description,
        gate_output,
    })
    .collect()
}

/// Starts one item's node: the whole of what the batch loop does for one ordinal.
///
/// Lifted out of the dispatch rather than left inline, because a batch runs it more than once in a
/// pass and the alternative was a sixty-line loop body inside a `match` arm.
pub(super) async fn start_item(
    state: &AppState,
    job: &JobRow,
    view: &JobView,
    artifacts: &str,
    worktree: (PathBuf, String),
    ordinal: usize,
    no_room: NoRoom,
) -> Pass {
    let pool = &state.pool;
    // What the claim below has to find. Taken from the verdict this pass already reached —
    // `next_step` named this ordinal precisely because `item_state_from` called it `Pending`
    // or `GateRetriable` — rather than asked of the database a second time. The alternative
    // is a `WHERE` clause that re-derives which red gates are retriable, which is the same
    // rule in a second dialect and drifts the first time either side is edited.
    let Some(held) = view
        .items
        .get(ordinal)
        .and_then(|item| item.state.claimable_as())
    else {
        tracing::warn!(
            job_id = job.id,
            ordinal,
            "asked to implement an item that is not startable"
        );
        return Pass::Stopped;
    };
    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT description, files, gate_output, agent_id FROM job_nodes
             WHERE job_id = ? AND ordinal = ?",
    )
    .bind(job.id)
    .bind(ordinal as i64)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    // `gate_output` is NULL for every item on its first attempt, which is every item of every
    // job that never retries anything — so the `None` this reads is the prompt staying
    // exactly as it was, not a fallback.
    let Some((description, files, gate_output, agent_id)) = row else {
        tracing::warn!(job_id = job.id, ordinal, "a job item lost its description");
        return Pass::Stopped;
    };
    // A hint that will not parse is no hint. It is an optimisation for where to start
    // reading, and refusing to run the item over it would fail the work for the sake of the
    // advice about the work.
    let files: Vec<String> = files
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .unwrap_or_default();
    let persona = persona_for(pool, agent_id.as_deref()).await;
    let prompt = implement_prompt(
        &description,
        ordinal,
        view.items.len(),
        artifacts,
        &files,
        gate_output.as_deref(),
        persona.as_deref(),
    );
    spawn_node(
        state,
        job,
        NodeSpec {
            node_id: "implement",
            claim: Some(ItemClaim { ordinal, held }),
            launch: super::dispatch::NodeLaunch::plain(prompt),
        },
        worktree,
        no_room,
    )
    .await
}

/// Drives one job as far as it will go this pass.
pub(super) async fn drive(state: &AppState, job: JobRow, now: DateTime<Utc>) {
    let pool = &state.pool;

    // Bookkeeping about work that already happened, before any decision and before any brake.
    match reconcile_nodes(state, &job).await {
        Ok(Reconciled::KeepGoing) => {
            // The bundle's own agent nodes land beside the autopilot's, on both roads: `drive` is
            // what `advance` runs under.
            if let Err(error) = landing::land_nodes(state, &job).await {
                tracing::warn!(job_id = job.id, %error, "could not land a job's agent nodes");
                return;
            }
        }
        // Already retired, and with a line in the feed saying why. Carrying on would drive a job
        // that has ended — and, in the one case that returns this, would advance the queue onto a
        // tree that could not be put back.
        Ok(Reconciled::JobRetired) => return,
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not reconcile a job's nodes");
            return;
        }
    }

    // Decision 14, checked before anything is started and while parked, since parked time counts.
    //
    // Which ceiling is a question about who asked, exactly as the house budget below is — see
    // `COMMISSIONED_JOB_LIFETIME`. The message says the number it actually applied rather than a
    // constant, so a reader is never told four hours by a job that got twelve.
    let ceiling = lifetime_ceiling(&job);
    if let Ok(started) = DateTime::parse_from_rfc3339(&job.created_at)
        && now.signed_duration_since(started.with_timezone(&Utc)) > ceiling
    {
        // The node in flight goes first, as a cancel's does: the job is never driven again once
        // it is retired, so a node left running would work on with nobody to stop it.
        if let Err(error) = stop_nodes_in_flight(state, job.id).await {
            tracing::warn!(job_id = job.id, %error, "could not stop an expired job's nodes");
        }
        let _ = retire(pool, job.id, STATUS_EXPIRED).await;
        say(
            pool,
            &job,
            "job_expired",
            &format!(
                "job {} passed the {}-hour ceiling and stopped; what it finished is on its branch",
                job.id,
                ceiling.num_hours()
            ),
        )
        .await;
        return;
    }

    // Unparked before the brakes are read, never by the brake that lifted: the job does not have to
    // remember which one stopped it, and whatever is still on parks it again below.
    if is_paused(&job.status) && resume(pool, job.id).await.is_err() {
        return;
    }

    // Said on the job itself, not left to be inferred from the approval queue. Until this, the row
    // read `implementing` while the entire chain was blocked on a person, and nothing connected the
    // proposal they were looking at to the job it came from.
    match node_awaiting_approval(pool, job.id).await {
        Ok(true) => {
            let _ = pause(pool, job.id, STATUS_AWAITING_APPROVAL, "approval").await;
            return;
        }
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read a job's paused nodes");
            return;
        }
    }

    for _ in 0..MAX_STEPS_PER_PASS {
        let Ok(current) = load_job(pool, job.id).await else {
            return;
        };
        if !LIVE_STATUSES.contains(&current.status.as_str()) {
            return;
        }
        // Carries the reason the pass STARTED with, so `park` can tell a new reason from a repeat.
        let current = JobRow {
            wait_reason: job.wait_reason.clone(),
            status: job.status.clone(),
            ..current
        };
        if advance(state, &current, now).await == Pass::Stopped {
            return;
        }
    }
}

/// One pass over every live job.
pub async fn job_tick(state: &AppState, now: DateTime<Utc>) {
    // Spec D6: before the kill switch, because saying a correction did not finish is not work,
    // and a notice that waits for the owner to lift the switch arrives too late.
    crate::judge::correction::report_ended_corrections(&state.pool).await;

    // The panic button stops the chain, not just the node — and it fails closed, so a switch that
    // cannot be read holds every job. Deliberately nothing more than that: engaging it does not
    // mark jobs terminal, because the read fails closed, and a database hiccup that retired every
    // job in flight would be a far worse failure than the one the switch is for.
    if crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return;
    }

    // Before the pass, so a slot freed here is available to the very jobs about to be driven.
    //
    // For jobs this is a backstop: `retire` gives the slot back the moment one ends, and every
    // ending funnels there. For RUNS it is the mechanism — `runs` has ten places that write a
    // terminal status and no funnel like `retire`, so a run's slot is derived from liveness instead
    // of released by hand, which cannot drift the way ten call sites can. `create_run_inner` sweeps
    // again immediately before it claims, so the derivation is current at the moment it decides
    // anything; this pass is what keeps the table honest in between.
    //
    // Either way a held slot is silent — it lowers a project's ceiling with no error anywhere — and
    // that silence is what the sweep bounds to one tick.
    match crate::execution::concurrency::reconcile_orphaned_slots(&state.pool).await {
        Ok(freed) if freed > 0 => {
            tracing::warn!("freed {freed} concurrency slot(s) whose owner was no longer live");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not sweep orphaned concurrency slots"),
    }

    // The interval's first tick is the startup pass, so this heals a crash between a final verdict
    // and its credit within one tick. It stays after the kill switch: an engaged switch defers the
    // work without losing it because `credited_at` remains NULL.
    match crate::brief::sweep(&state.pool).await {
        Ok(0) => {}
        Ok(credited) => tracing::debug!(credited, "credited briefings whose verdict is final"),
        Err(error) => tracing::warn!(%error, "could not sweep uncredited briefings"),
    }

    // Before the jobs are driven, because the measurement is about the state the trees are in NOW
    // and driving a job changes them. It carries a deadline of its own: a `git status` over a large
    // tree is what this pass costs, and nothing here may delay the work the tick exists to do.
    crate::execution::collision::measure(&state.pool).await;

    let jobs = match live_jobs(&state.pool).await {
        Ok(jobs) => jobs,
        Err(error) => {
            tracing::warn!(%error, "could not load live jobs");
            return;
        }
    };
    for job in jobs {
        reclaim_the_slot(state, &job).await;
        drive(state, job, now).await;
    }
}

/// Puts a live job back on a slot it should never have been off.
///
/// The gap this closes is small and real: `start` inserts the row and claims immediately after,
/// because a slot is keyed on its owner's id, so a daemon that dies in that window leaves a job
/// live and holding nothing. The sweep cannot heal it — that pass only takes slots AWAY from owners
/// that died, and there is nothing here to take. Left alone, the project would run one over its
/// ceiling for as long as the job lasts.
///
/// `claim` is idempotent per owner, so a job that already holds one costs a single indexed read.
///
/// A full house is logged and no more. This job is already running: refusing it a slot now would
/// change nothing about what it is doing and would only make the books disagree with the machine.
/// The ceiling governs what STARTS, and this one already did.
pub(super) async fn reclaim_the_slot(state: &AppState, job: &JobRow) {
    let owner = crate::execution::worktree::Owner::Job(job.id);
    match crate::execution::concurrency::claim(&state.pool, &job.project_id, owner).await {
        Ok(crate::execution::concurrency::ClaimOutcome::Claimed(_)) => {}
        Ok(crate::execution::concurrency::ClaimOutcome::Full(full)) => tracing::warn!(
            job_id = job.id,
            project_id = %job.project_id,
            reason = %full.reason(),
            "a live job holds no concurrency slot and there is no room to give it one"
        ),
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not confirm a live job's slot");
        }
    }
}

/// The job tick's own loop.
///
/// Separate from the scheduler's, because a pass here can sit inside `run_gate` for the whole gate
/// timeout, and sharing a loop would stall every scheduled rule in the daemon behind one project's
/// test suite.
pub async fn run_job_loop(state: AppState) {
    let mut interval = tokio::time::interval(JOB_TICK);
    loop {
        interval.tick().await;
        let tick_state = state.clone();
        survive_panic(async move { job_tick(&tick_state, Utc::now()).await }).await;
    }
}

/// Runs one tick on its own task, so a panic inside it comes back as a logged value instead of
/// ending the loop: the loop is spawned bare, and one panic would otherwise stop all job
/// scheduling until the daemon restarted. A task is used instead of `catch_unwind` because it
/// needs no extra dependency and also covers a panic on any await point of the tick.
pub(super) async fn survive_panic<F>(tick: F) -> bool
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    match tokio::spawn(tick).await {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, "a job tick panicked; the loop carries on");
            false
        }
    }
}

pub(super) const JOB_TICK: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(test)]
mod survive_panic_tests {
    use super::survive_panic;

    #[tokio::test]
    async fn a_panicking_tick_is_reported_and_the_next_one_still_runs() {
        assert!(!survive_panic(async { panic!("tick blew up") }).await);
        assert!(survive_panic(async {}).await);
    }
}
