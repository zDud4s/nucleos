//! The pure half: item and review states, `next_step`, and how a round closes. No database, no worktree, no subprocess.

use super::*;

/// Where one item of a job's queue has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Pending,
    Running,
    /// The run finished cleanly but the gate has not measured it yet.
    Implemented,
    /// Gated green, or no gate is configured for this project.
    Passed,
    Failed,
    /// Somebody stopped this node. Apart from `Failed` because it is the difference between "the
    /// work broke" and "you stopped it", and the two read as opposite things in a feed.
    Cancelled,
    GateFailed,
    /// The gate said no and this item still has a retry to spend, so it is going round again.
    ///
    /// **NOT terminal**, and that is the whole of what it means: the queue owes this item another
    /// implement run, in the tree exactly as the rejected attempt left it, with what the gate printed
    /// in its prompt. `next_step` picks it up by the same positional search that picks up `Pending`,
    /// and neither `ending()` nor `close_the_round` counts it — a job is not `gate_failed` for a
    /// verdict it has not finished answering.
    ///
    /// **In a job a team directs the tree is "as the attempt left it, plus the job's branch".** The
    /// item works in a checkout of its own, and that checkout has been standing still while other
    /// items landed; `catch_up_the_item` brings the branch in before the node starts. Without it a
    /// retry would answer a gate whose verdict was about a repository that no longer exists — and a
    /// red gate caused by somebody else's merge would not be fixable from here at all.
    ///
    /// Never stored. `job_items.status` says `gate_failed` either way; this state is what
    /// `item_state_from` makes of that status once it has read the attempt count beside the job's
    /// budget, and neither number means anything alone.
    GateRetriable,
    GateErrored,
    /// This item asked for a decision, so the job put it down and moved on.
    ///
    /// Not a failure and not a cancellation: nothing broke, and nobody stopped anything. The work
    /// was never attempted, the tree was reverted to where the item started, and a `skipped-item`
    /// proposal carries what would be needed to pick it up. `next_step` walks past it the way it
    /// walks past `Passed`, because the queue owes it nothing more.
    Skipped,
    /// The item's own branch is being merged into the job's branch.
    ///
    /// Arrives from `Implemented`; leaves for `Passed` (merged and gated green), `Conflicted`
    /// (git refused) or `Reverted` (merged and gated red). Not terminal, and nothing writes it
    /// yet — the merge step that does is a later slice of this design, and the state exists ahead
    /// of it so that `item_state_from` cannot read the row it will write as "still to do".
    Merging,
    /// The merge stopped on a conflict, and the conflict is staged in the item's own tree.
    ///
    /// Leaves for `Running` — the resolution node, in that same tree — and from there back to
    /// `Merging`. The item is put down rather than failed: a conflict is a question about two
    /// pieces of work, not a verdict on either.
    Conflicted,
    /// The merge landed, the gate went red, and the job's branch has been reset back to where it
    /// stood before the merge.
    ///
    /// Leaves for `GateRetriable` if the item still has an attempt to spend, `GateFailed` if it
    /// does not. Apart from `GateFailed` because the branch state differs and a reader needs to
    /// know it: the rejected work is off the job's branch and still on the item's.
    Reverted,
    /// Never attempted, because something it depended on ended badly.
    ///
    /// **Terminal**, and the only new terminal here. Distinct from `Skipped`, which is work a
    /// person was asked to decide about; nobody is being asked anything about an orphan, and the
    /// thing that broke is already in the queue saying so.
    Orphaned,
    /// Taken over by an item of a later round. Stored as [`STATUS_SUPERSEDED`].
    Superseded,
}

/// Every variant of [`ItemState`], for the tests that have to say something about all of them.
///
/// A hand-kept list, held to the enum by `every_item_state_covers_the_enum`. The alternative is a
/// derive macro for the sake of one array, and the alternative to both — tests that enumerate the
/// states inline — is what lets a new variant be born untested everywhere at once.
#[cfg(any(test, feature = "testkit"))]
pub(super) const EVERY_ITEM_STATE: [ItemState; 15] = [
    ItemState::Pending,
    ItemState::Running,
    ItemState::Implemented,
    ItemState::Passed,
    ItemState::Failed,
    ItemState::Cancelled,
    ItemState::GateFailed,
    ItemState::GateRetriable,
    ItemState::GateErrored,
    ItemState::Skipped,
    ItemState::Merging,
    ItemState::Conflicted,
    ItemState::Reverted,
    ItemState::Orphaned,
    ItemState::Superseded,
];

/// Whether the job still owes a review node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    NotWanted,
    Pending,
    Running,
    Done,
    /// Wanted, and not had, because no item of this round reached `passed`.
    ///
    /// A review exists to judge what the round produced, and a round in which nothing passed has
    /// put nothing on the branch: every red item was reverted to its footing before the queue moved
    /// on. Measured on job 27, 2026-09-14: round 0's only item ended `gate_failed`, its work was
    /// reverted, and the review (run 900513, $0.12) found `HEAD` at the base and a clean tree and
    /// reported "no work was done" — about work that had been done, measured, and taken back.
    ///
    /// Not a failed review, and nothing reads it as one: it closes the round the way `Done` does,
    /// but no node ran, so there is no verdict anywhere to quote. `load_view` derives it from the
    /// item rows on every read, so a restart finds the same answer rather than remembering one.
    Skipped,
    /// The round's first review failed on an API error a second attempt can get past, and it is
    /// owed again.
    ///
    /// Measured on job 26, 2026-09-13: review run 900483 exited 1 after ten `api_retry` events, its
    /// result line saying `terminal_reason: "api_error"` with no HTTP status — "Can't reach the API
    /// server — check your internet or DNS (ENOTFOUND)". It read as `Done`, and twenty seconds
    /// later the replan opened the next round with no verdict on this one. A node that never
    /// reached the model judged nothing, which is the one failure where "a review that failed is
    /// still a review that happened" is not true.
    ///
    /// Once per round, counted from the rows: the round's second review reads `Done` however it
    /// ended, so a network that stays down costs one extra node and never a loop. Not a skip — a
    /// skipped review was never owed, and this one was owed and has not been had.
    Retry,
}

/// How a job ended.
///
/// `GateFailed` and `GateErrored` stay apart all the way up from `gate::GateOutcome`: a non-zero
/// exit says the code is broken, a binary that would not start says the measurement never happened.
/// Collapsing them would report silence as a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    /// Stopped on purpose, at either level: cancelling a run stops one node, cancelling a job stops
    /// the sequence. Both end the job — a stopped node leaves the tree holding edits no gate has
    /// measured, so the next item must not build on them — but neither is a failure of the work,
    /// and a feed that says `failed` for something the user did themselves teaches them to ignore it.
    Cancelled,
    GateFailed,
    GateErrored,
    /// Ran out of an allowance rather than out of work: the round ceiling here, the job's own budget
    /// once `brakes()` learns to read it.
    ///
    /// Apart from `Completed` for the same reason `Cancelled` is apart from `Failed` — it is what a
    /// person reads in the morning. `completed` means "I finished"; `stopped` means "I was cut short
    /// and what I did is on the branch". Reporting the second as the first is how somebody stops
    /// looking at a job that still had work in it.
    Stopped,
}

/// What the daemon should do next for a job.
///
/// No longer `Copy`, because a batch is a `Vec`. The three places that relied on it took the value
/// out of a binding they went on using; they borrow now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Turn the sentence a job was asked with into a written brief, before anything is planned.
    ///
    /// First and not second, and that ordering is the whole of the node. The plan node's output is
    /// a QUEUE — descriptions the implement nodes are handed one at a time — so whatever the
    /// planner failed to work out about a vague request is not recoverable downstream: every item
    /// after it inherits the misunderstanding, and the gate measures the wrong thing correctly.
    SpawnSpec,
    SpawnPlan,
    /// The items to start now, by ordinal — **at most one** for a job without a team.
    ///
    /// A list and not an ordinal, and the difference is the whole of this design. A single ordinal
    /// can only ever be answered again after the item it names has finished, so the queue was
    /// sequential by the shape of this type before it was sequential by any rule. What decides the
    /// list is `batch_of`; what decides how much of it actually starts is the driver, because a
    /// project with no room left refuses the second checkout and the batch comes out smaller.
    SpawnImplement(Vec<usize>),
    RunGate {
        ordinal: usize,
    },
    /// Bring one item's branch into the job's, in the job's own checkout.
    ///
    /// Only ever answered for a job a team directs. Without a team the items write into the one
    /// shared checkout and there is nothing to bring anywhere — `Implemented` goes straight to
    /// [`Next::RunGate`], exactly as it always did.
    ///
    /// A step of its own rather than the first half of the gate, because what has to be exclusive is
    /// the merge and the gate merely follows it. Two steps also means `MAX_STEPS_PER_PASS` counts
    /// them apart, so one pass drains two merged-and-gated items rather than four merges with
    /// nothing measured.
    MergeItem {
        ordinal: usize,
    },
    /// Put an item down because something it depends on will never land.
    ///
    /// A step and not a silent skip, because the row has to say so. Left `pending`, the item is
    /// work nothing will ever start: it holds a slot and a checkout to the end of the job, it is
    /// counted among the unfinished, and a person reading the queue is told the job is still going
    /// to do it.
    Orphan {
        ordinal: usize,
    },
    SpawnReview,
    /// Ask again now that this round's queue is empty: either for more work, or for "done".
    SpawnReplan,
    /// A node is in flight; nothing to do until it lands.
    Wait,
    Finish(Outcome),
}

/// What the last replan node concluded, once it has landed.
///
/// Two values and not three, because "it produced a queue" is not a state this has to hold: the
/// caller writes the items, bumps the round, and the queue in `JobView::items` IS the answer. What
/// is left is the case with nothing to show for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Replan {
    /// No replan node has landed for this round.
    #[default]
    NotYet,
    /// It said `{"done": true}` — the thing that knows the work says the work is over.
    Done,
}

/// Where a job is in its sequence of rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundState {
    /// Which round the queue in `JobView::items` belongs to, counting from 0 so that the plan node's
    /// items are round 0 and the first replan opens round 1.
    pub round: i64,
    /// The ceiling this job was given, already cut by `MAX_ROUNDS_CEILING` before it got here.
    ///
    /// `1` is a job of today and takes the pre-rounds path verbatim, which is what lets the schema
    /// and this logic land without changing a single existing behaviour.
    pub max_rounds: i64,
    /// Consecutive rounds that added no new items.
    pub dry_rounds: i64,
    /// Whether a replan node is in flight right now.
    ///
    /// The sibling of `planning`, and needed for the identical reason: between spawning the node and
    /// its items landing, the queue is empty, and without this every tick would spawn another one.
    pub replanning: bool,
    /// What the last replan node said.
    pub replanned: Replan,
    /// Whether the current round queued no items of its own.
    ///
    /// Carried rather than derived from `items`, which is deliberately not filtered by round and so
    /// cannot answer it. What it decides is whether the round gets a review node at all: a round that
    /// added nothing has an unchanged branch, and reviewing it spends a whole run re-reading a diff
    /// nobody wrote.
    pub round_added_nothing: bool,
    /// Whether a broken item may open another round instead of ending the job.
    ///
    /// True for a job the owner asked for and gave an allowance to — `JobRow::commissioned_with_an_
    /// allowance`, the same predicate the budget and the life ceiling use. **Zero value is false**,
    /// so every job that existed before this ends exactly where it always ended.
    ///
    /// Named for what it licenses rather than for who asked, because two decisions read it and
    /// neither is about who asked: `next_step` walks past a broken item instead of ending the queue
    /// at it, and `close_the_round` asks for another plan instead of finishing.
    ///
    /// **Both are licensed by the same fact about the TREE, and not by the owner's wish.** A red
    /// gate is reverted by `record_gate` and a dead node by `reconcile_nodes`, each to the item's
    /// own footing, BEFORE either is marked — so by the time this is read the rejected or
    /// unmeasured work is already off the branch and there is nothing left to build on top of.
    /// Where that revert fails, neither caller reaches this at all: the job is retired on the spot,
    /// because a tree that could not be put back is the one case where carrying on is worse than
    /// stopping early.
    ///
    /// `GateErrored` stays outside this whatever it says. A gate that would not run measured
    /// nothing and is deliberately never reverted — its work might be perfectly good, and
    /// destroying it would be worse than stopping — which leaves the build-on-top argument exactly
    /// true for it and for it alone.
    pub a_broken_item_may_go_round_again: bool,
}

impl Default for RoundState {
    /// A job of one round, which is every job that existed before this struct did.
    fn default() -> Self {
        Self {
            round: 0,
            max_rounds: 1,
            dry_rounds: 0,
            replanning: false,
            replanned: Replan::NotYet,
            round_added_nothing: false,
            a_broken_item_may_go_round_again: false,
        }
    }
}

/// How many rounds in a row may add nothing before the job calls itself finished.
///
/// Two, and one would be wrong: a replan can legitimately produce nothing while the previous round's
/// work is still settling. Counting items to a target never finds the tail either — a model that
/// will not say "done" would sit against `max_rounds` spending money — so the brake is "it dried up"
/// and not "it reached a number".
pub const DRY_ROUNDS_TO_STOP: i64 = 2;

/// Everything the decision below needs to see, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    /// Whether the job has a written brief, or is past the point of wanting one.
    ///
    /// **`planned` is deliberately part of this, and it is what makes the node safe to add to a
    /// live system.** Every job that existed before the spec node did has a queue and no spec run,
    /// and without this arm the first tick after the upgrade would spawn a spec node on top of work
    /// already under way — writing a brief for a job whose items are half-done. A job that has
    /// planned is past the moment a brief would have helped, whatever its history says.
    ///
    /// A spec run that ended badly still counts. The brief is context, not a gate: without one the
    /// planner reads the raw request, which is exactly what it did before this node existed, and
    /// failing a whole job over a document that did not get written would be a worse answer than
    /// the behaviour it replaced.
    pub specced: bool,
    /// Whether a spec node is in flight right now. Same reason `planning` exists beside `planned`:
    /// the brief does not exist in either case, and without this every tick would spawn a second
    /// spec node on top of the first.
    pub speccing: bool,
    /// Whether a plan node has already produced a queue. Distinct from `items` being empty, which
    /// is a planner that looked and found no work.
    pub planned: bool,
    /// Whether a plan node is in flight right now.
    ///
    /// Separate from `planned` because the queue does not exist yet in either case, and without it
    /// every tick would answer `SpawnPlan` while the first planner was still running. The item
    /// queue is what stops that from happening to an implement node; the plan node has no item.
    pub planning: bool,
    /// Every item the job has ever queued, in ordinal order — **not** the current round's.
    ///
    /// This said the opposite until now, and the loader has always disagreed with it in writing:
    /// *"Deliberately NOT filtered by round … Filtering would have made the position in this vector
    /// stop being the ordinal in the table"*. The unfiltered read is the right one and three things
    /// rest on it — `advance` looks an item up by its position, `depends_on` names global ordinals
    /// and has to be able to point back into an earlier round, and `ending()` still reports a
    /// round-1 red gate when round 3 finishes.
    pub items: Vec<ItemView>,
    pub review: ReviewState,
    pub rounds: RoundState,
    /// Whether a team directs this job — `jobs.team_id` being set, and nothing more.
    ///
    /// A boolean and not the id: this struct is what the decision below needs to see and nothing
    /// else, and no decision below is about WHICH team.
    ///
    /// What it buys is one thing, and it is the whole of this slice: a job whose items work in
    /// trees of their own can lose an item without losing the queue. Without a team the items
    /// share one tree, so an item that broke leaves edits nothing measured where the next item
    /// would build on them — which is why `false` still stops the job at the first failure.
    pub has_team: bool,
    /// How many of this job's items may be worked on at once — `teams.max_parallel`, or **1**.
    ///
    /// One without a team, which is what makes the batch below come out as a queue of one and the
    /// sequential job identical to what it always was. Never zero: a ceiling of zero would build an
    /// empty batch for ever while the queue still owed work, and the round would close on it.
    ///
    /// **The project's free slots are deliberately NOT folded in here.** This function is pure and
    /// `concurrency.rs` says the `INSERT` is the lock; asking how much room there is would be
    /// reading a number that another job can take between the read and the claim. The driver claims
    /// and keeps what it gets, and a refusal is the batch coming out smaller — which is the same
    /// answer, arrived at where it is true.
    pub max_parallel: usize,
}

/// One item of the queue, as the decision below has to see it.
///
/// A struct and not a bare [`ItemState`] because two items being independent is not something their
/// states can say. `depends_on` and `files` are the only things that can, and they have to be in the
/// pure function — deciding outside would put "which item may run" in two places, and the second one
/// would not be table-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemView {
    pub state: ItemState,
    /// The global ordinals this item may not start before.
    ///
    /// Empty for every item of every job without a team, which is what makes those jobs read here
    /// exactly as they always did.
    pub depends_on: Vec<i64>,
    /// What the director said this item will touch.
    ///
    /// The other half of "these two may run together", and the half that answers it for items with
    /// no dependency between them at all. Two items naming one path are never started together,
    /// because the second one to merge would conflict and the retry costs more than the wait.
    ///
    /// Empty without a team, and there it is a hint the implement node is given rather than a
    /// boundary — see `implement_prompt`. It is a boundary only here, where nothing has run yet and
    /// a guess is all there is to go on.
    pub files: Vec<String>,
}

impl ItemView {
    /// A view of an item that waits for nobody and declares nothing, which is what a job without a
    /// team has.
    #[cfg(any(test, feature = "testkit"))]
    pub(super) fn plain(state: ItemState) -> Self {
        Self {
            state,
            depends_on: Vec::new(),
            files: Vec::new(),
        }
    }
}

/// Decides a job's next move from what is observable about it.
///
/// Pure, and kept that way on purpose — the same split `classifier.rs` has from `hooks.rs`. Every
/// interesting question about a job ("does a red gate stop the chain?", "where does a resume pick
/// up?") becomes a table test with no database, no worktree and no subprocess. The caller owns the
/// I/O and is free to be dumb.
pub fn next_step(job: &JobView) -> Next {
    // Failures first, and before the in-flight check: a chain with a broken item must stop even if
    // another node is still running, rather than spending budget on work about to be thrown away.
    //
    // `GateFailed` is deliberately NOT here any more, and it is the only one that left. A red gate
    // used to end the night at the first item that broke; now the tree is reverted to the item's
    // footing and the queue carries on, because the remaining items were never the ones that broke
    // and a night that stops on the first of five wastes the other four. What survives is the
    // ENDING: `ending()` below still reports the job `gate_failed`, so nothing about the job's
    // outcome is softened — only the point at which it is decided.
    //
    // `GateErrored` stays, and the asymmetry is the whole reason those two are separate states. A
    // non-zero exit is a verdict about the code; a gate that would not run is silence. Continuing
    // past a verdict is a judgement call this change makes; continuing past silence would be
    // building on work nothing has measured, which is what the gate exists to prevent.
    // The guards are what makes a team job different, and they are guards rather than a separate
    // block so that a job WITHOUT one keeps this loop exactly as it was — same arms, same order,
    // so the same item decides when more than one of them is unhappy.
    //
    // What earns them is not the wish for parallelism: it is that a team's items each work in a
    // tree of their own. The reason a lone failure stops the sequential job — the shared tree now
    // holds edits nothing has measured, and the next item would build on them — is a statement
    // about ONE tree, and it stops being true when the broken item's edits are on a branch that
    // was never merged. `ending()` still reports the job `failed`; only the moment of the report
    // moves, from the first bad item to the end of the queue.
    //
    // `Cancelled` keeps no guard, in either kind of job. It is not a claim about trees — it is a
    // person having stopped this, and carrying on would be answering them.
    // `Failed` now carries a second guard, and it is the same argument the `GateFailed` paragraph
    // above makes, arriving one state later. The reason a dead node ended the queue was that its
    // tree held edits nothing had measured and the next item would build on them — true of every
    // node that ends `timed_out`, `interrupted` or broken. `reconcile_nodes` now puts that tree
    // back to the item's footing before it marks the item, for a job the owner commissioned, so
    // the edits are gone by the time this reads the state and the reason has been removed rather
    // than overruled. A revert that could not be done never gets here: the job is retired there.
    //
    // Measured 2026-08-30: job 23's item was implemented, gated red, retried — and its retry was
    // killed mid-flight, which marked the item `failed` and ended the job with three items never
    // attempted. Nothing about those three was broken.
    let broken_item_ends_the_queue = !job.rounds.a_broken_item_may_go_round_again;
    for item in job.items.iter().map(|item| &item.state) {
        match item {
            ItemState::Failed if !job.has_team && broken_item_ends_the_queue => {
                return Next::Finish(Outcome::Failed);
            }
            ItemState::Cancelled => return Next::Finish(Outcome::Cancelled),
            ItemState::GateErrored if !job.has_team => return Next::Finish(Outcome::GateErrored),
            _ => {}
        }
    }

    // Before the plan, because a queue built from a misread request is a queue every node after it
    // obeys. `specced` is true for any job that already planned, so this arm is unreachable for work
    // that predates the node — see `JobView::specced`.
    if !job.specced {
        return if job.speccing {
            Next::Wait
        } else {
            Next::SpawnSpec
        };
    }
    if !job.planned {
        return if job.planning {
            Next::Wait
        } else {
            Next::SpawnPlan
        };
    }
    // A planner that looked and found nothing. Only reachable on the first round: the queue is not
    // filtered by round, so once anything has been queued at all this is never empty again, and a
    // round that adds nothing shows up as `dry_rounds` rather than as an empty queue.
    if job.items.is_empty() {
        return Next::Finish(Outcome::Completed);
    }
    // `Reverted` is the last state left with no step of its own and no writer. It is here because
    // the alternative is worse than being early: a non-terminal state this search walked past would
    // be read as finished by every check after it, which is the one direction `item_state_from`
    // already refuses to err in. `Merging` and `Conflicted` have both left this list, each having
    // been given a step below.
    //
    // **`Running` waits only without a team, and that one condition is the whole of continuous
    // start.** The line used to read "anything in flight, wait", and it is what made the queue
    // sequential: a job could never have two items going at once because the first one to start
    // stopped the search. A team's items work in checkouts of their own, so the reason behind the
    // line — the shared tree holding work nothing has measured — is not true for them.
    if job
        .items
        .iter()
        .any(|item| item.state == ItemState::Reverted)
        || (!job.has_team
            && job
                .items
                .iter()
                .any(|item| item.state == ItemState::Running))
    {
        return Next::Wait;
    }
    // A merge that has landed is measured before anything else moves. Ahead of the merge below, and
    // that order is the design: an item's divergence from the job's branch grows with every minute
    // it waits, and it is the retry — the most expensive step here — that has to absorb it. Draining
    // what is already in flight before starting more is what keeps that divergence small.
    if let Some(ordinal) = job
        .items
        .iter()
        .position(|item| item.state == ItemState::Merging)
    {
        return Next::RunGate { ordinal };
    }
    // Gate before starting the next item: on a shared worktree, letting item i+1 build on unmeasured
    // work means a later red gate cannot say which item broke it.
    //
    // With a team the same sentence has a merge in the middle of it. The item's work is in a
    // checkout of its own, so there is nothing on the job's branch to measure until it is brought
    // there — and bringing it there is what must not happen twice at once.
    if let Some(ordinal) = job
        .items
        .iter()
        .position(|item| item.state == ItemState::Implemented)
    {
        return if job.has_team {
            Next::MergeItem { ordinal }
        } else {
            Next::RunGate { ordinal }
        };
    }
    // A retriable item is work to do, found by the SAME positional search that finds a pending one,
    // so an earlier red-gated item outranks a later untouched one. Running the pending item first
    // would build it on a tree that still stands where the red gate left it, and the next gate could
    // no longer say which of the two broke it — which is the whole reason gating happens between
    // items rather than at the end.
    //
    // `Conflicted` is found by the same search, and it is work for the same reason: the item owes
    // a run, in the checkout it already has. What that run does differs — it resolves a merge
    // rather than writing a feature — but that is a difference in the prompt, not in whether there
    // is anything to start. Leaving it out would put the item down for good over a conflict, which
    // is not a verdict about the work.
    // An item whose dependency will never land is put down before the search below runs, so that
    // search never has to know about the graph. It stays a plain positional walk over states, and
    // "which item is next" keeps meaning one thing.
    //
    // Only `Pending` is orphaned. An item that has already run has work in a checkout of its own,
    // and whether that work is worth anything now is a question for the person reading the branch —
    // not something to be decided by a sibling's failure.
    if let Some(ordinal) = job.items.iter().position(|item| {
        item.state == ItemState::Pending
            && item.depends_on.iter().any(|dependency| {
                job.items
                    .get(*dependency as usize)
                    .is_some_and(|dependency| never_lands(dependency.state))
            })
    }) {
        return Next::Orphan { ordinal };
    }
    let batch = batch_of(job);
    if !batch.is_empty() {
        return Next::SpawnImplement(batch);
    }
    // Nothing could be started, and the two reasons for that are as far apart as reasons get.
    //
    // Something in flight is the ordinary one: the queue is at its ceiling, or everything left is
    // waiting on a dependency, and the item that lands next will free it. Nothing in flight while
    // the queue still owes a run is a queue that cannot move — an item that will never be selected
    // by the fold above and will never be put down by the orphan check either, because it has work
    // in a checkout of its own and something it depends on was skipped rather than failed.
    //
    // **It says `stopped`, and never `completed`.** `stopped` is the ending that means "I was cut
    // short and what I did is on the branch", which is exactly true here; falling through to
    // `close_the_round` would report a job with unstarted work as finished, and that is the one
    // reading nobody can catch afterwards. Unreachable without a team — there the fold always takes
    // the first claimable item — and that is why this is a guard rather than a case.
    if job
        .items
        .iter()
        .any(|item| item.state == ItemState::Running)
    {
        return Next::Wait;
    }
    if job
        .items
        .iter()
        .any(|item| item.state.claimable_as().is_some())
    {
        return Next::Finish(Outcome::Stopped);
    }

    // Two ways a round closes without being reviewed, and both are about not spending a run for
    // nothing. A replan that declared the work over closed a round that had already been reviewed
    // before it ran — a second review would re-read a branch nobody is going to change. And a round
    // that queued no items has no diff of its own to read at all.
    if job.rounds.replanned == Replan::Done || job.rounds.round_added_nothing {
        return close_the_round(job);
    }

    match job.review {
        // A review owed again is owed like one not yet had. See `ReviewState::Retry`.
        ReviewState::Pending | ReviewState::Retry => Next::SpawnReview,
        ReviewState::Running => Next::Wait,
        // A round in which nothing passed goes on exactly as it would have after its review: to
        // the replan, or to the ending. See `ReviewState::Skipped`.
        ReviewState::NotWanted | ReviewState::Done | ReviewState::Skipped => close_the_round(job),
    }
}

/// PURE: whether `next_step` answered what it did only because the round's review was skipped.
///
/// Asked by putting the review back and asking again, rather than by restating the conditions under
/// which `next_step` reaches the review: a second copy of those would agree with the first only
/// until one of them was edited. `ReviewState::Skipped` alone is not enough, because the view reads
/// it all through a round in which nothing has passed YET. While items are still to run the skip
/// has decided nothing, and a job cancelled mid-round ends at the short-circuit, review or no
/// review.
pub(super) fn the_skip_decided(view: &JobView) -> bool {
    view.review == ReviewState::Skipped
        && next_step(&JobView {
            review: ReviewState::Pending,
            ..view.clone()
        }) == Next::SpawnReview
}

/// Who the director gave this item to, in their own words, or `None`.
///
/// **The prompt is read NOW and not stored on the row**, which is what makes `job_items.agent_id` a
/// reference rather than a copy. An owner who edits an agent between the plan and the run gets the
/// edited agent doing the work, which is the behaviour a catalogue is for: the alternative freezes a
/// persona at plan time and leaves the owner unable to say why their correction had no effect.
///
/// `None` three ways, and they mean the same thing to the caller and different things to a reader:
/// the item has no agent, because its job has no team; the agent was deleted between the plan and
/// the run, which `foreign_keys` and `agent::delete` between them make very nearly impossible; or
/// the read failed. The last is the only one worth saying out loud, because it is the one where the
/// item runs with a brief that is missing something it was supposed to have.
pub(super) async fn persona_for(pool: &SqlitePool, agent_id: Option<&str>) -> Option<String> {
    let agent_id = agent_id?;
    match crate::agent::get(pool, agent_id).await {
        Ok(Some(agent)) => Some(format!(
            "{}\n\nYou are `{}` on this job's team, and you were given the item below because your \
             speciality is {}.\n\n",
            agent.prompt, agent.name, agent.speciality
        )),
        Ok(None) => {
            tracing::warn!(
                agent_id,
                "a job item names an agent the catalogue does not have"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                agent_id,
                "could not read the agent a job item was given to; it runs without its brief"
            );
            None
        }
    }
}

/// PURE: which items may be started right now, by ordinal, at most `max_parallel` of them.
///
/// **A fold and not a search**, which is the difference between this and the positional walk it
/// replaced. Each candidate is admitted against what is already in flight AND against what earlier
/// candidates of this same batch have taken, so the answer is a set that can safely run together
/// rather than the first thing that could run.
///
/// Three rules, and each rejects for a different reason:
///
/// 1. **Its dependencies have all passed.** Not "have all finished" — a dependency that failed or
///    was skipped never lands, and the item waiting on it is put down by the orphan step above
///    rather than started here.
/// 2. **Nothing it declares is already spoken for.** By work in flight, or by an earlier item of
///    this batch. Two items writing one file do not conflict in git while they are apart and break
///    the branch together when they merge, and the retry that costs is the most expensive step in
///    this design.
/// 3. **The ceiling.** Counted as the batch plus what is already `Running`, because continuous
///    start means this runs again while earlier items are still going.
///
/// **The candidate excludes itself from what blocks it.** `Conflicted` is both a state that holds
/// paths and a state that owes a run, so an item in it would otherwise be permanently ineligible —
/// blocked by its own declaration, for ever, with nothing to say why.
///
/// **Greedy by ordinal, and it can lose.** Item 1 declaring `{a, b}` takes the batch and excludes
/// both 2 (`{a}`) and 3 (`{b}`) — a batch of one where a different choice would have made two. The
/// alternative is a combinatorial search on a function that runs every 30 seconds for every live
/// job, to buy an item's worth of parallelism in a case a director can avoid by splitting its work
/// differently.
///
/// `Merging` and `Implemented` are in the blocking set and cannot be seen from here: the exclusive
/// steps above return before this is reached whenever an item is in either. They are named anyway
/// because the set is a statement about which states hold paths, and a set that is only right
/// because of the order of the checks above it is a set that breaks silently when that order moves.
pub(super) fn batch_of(job: &JobView) -> Vec<usize> {
    let running = job
        .items
        .iter()
        .filter(|item| item.state == ItemState::Running)
        .count();
    let mut batch: Vec<usize> = Vec::new();
    let mut taken: Vec<&str> = Vec::new();

    for (ordinal, item) in job.items.iter().enumerate() {
        if batch.len() + running >= job.max_parallel {
            break;
        }
        if item.state.claimable_as().is_none() {
            continue;
        }
        let landed = item.depends_on.iter().all(|dependency| {
            job.items
                .get(*dependency as usize)
                .is_some_and(|dependency| dependency.state == ItemState::Passed)
        });
        if !landed {
            continue;
        }
        let clashes = item.files.iter().any(|file| {
            taken.contains(&file.as_str())
                || job.items.iter().enumerate().any(|(other, mate)| {
                    other != ordinal && holds_paths(mate.state) && mate.files.contains(file)
                })
        });
        if clashes {
            continue;
        }
        taken.extend(item.files.iter().map(String::as_str));
        batch.push(ordinal);
    }
    batch
}

/// PURE: whether an item in this state has a claim on the paths it declared.
///
/// Everything from the moment a node starts writing until the merge has landed or been taken back.
/// `Passed` is out — its work is on the job's branch, so the next item to start inherits it rather
/// than racing it — and so is every terminal state, which owns nothing.
pub(super) fn holds_paths(state: ItemState) -> bool {
    matches!(
        state,
        ItemState::Running | ItemState::Merging | ItemState::Conflicted | ItemState::Implemented
    )
}

/// PURE: whether an item in this state will never contribute its work.
///
/// The condition that orphans everything waiting on it, and it is deliberately wider than "failed".
/// What a dependent needs to know is not whether its dependency went wrong but whether its work is
/// ever going to arrive, and for these five the answer is no.
///
/// **`Skipped` is in the list, and it is the one worth arguing about.** A skipped item is not a
/// failure — nothing broke and nobody stopped anything — and `ending()` deliberately walks past it
/// so that a job which skipped one item is not reported as failed. That stays true: a skipped item
/// ALONE still ends the job `completed`. What changes is a skipped item that something else was
/// waiting for, and there the job did not do what was asked, because a person now has to decide
/// about the prerequisite before any of it can happen.
///
/// `Cancelled` is here for completeness and never decides: it stops the job at the short-circuit.
pub(super) fn never_lands(state: ItemState) -> bool {
    matches!(
        state,
        ItemState::Failed
            | ItemState::GateFailed
            | ItemState::GateErrored
            | ItemState::Cancelled
            | ItemState::Skipped
            | ItemState::Orphaned
            // Its own work never lands: the item that took it over is a different item.
            | ItemState::Superseded
    )
}

/// PURE: what happens when a round's queue is spent and its review has been had.
///
/// The four endings of §5.2, in the order they are observable — and the order is observable because
/// two of them say `completed` and two say `stopped`, which is the difference between "I finished"
/// and "I was cut short and what I did is on the branch".
///
/// A **one-round job takes the old path verbatim** and that is the first thing this checks, because
/// it is what makes rounds inert until somebody asks for them. It is also the honest answer: a job
/// that was never asked to run more than one round did not run OUT of rounds, so reporting it
/// `stopped` would name the ceiling of a feature it never used.
pub(super) fn close_the_round(job: &JobView) -> Next {
    if job.rounds.max_rounds <= 1 {
        return Next::Finish(ending(job));
    }
    // An unhappy ending ends the job whatever the rounds say. The branch carries work the gate
    // rejected, and another round would build on top of it — which is the one thing the per-item
    // revert exists to stop happening WITHIN a round, and it does not stop being true across them.
    //
    // Asked of `failed_ending` rather than spelled out, because this used to name `GateFailed` and
    // nothing else and was right only because the short-circuit at the top of `next_step` reached
    // `Failed` and `GateErrored` first. A team job has no such short-circuit.
    //
    // **Except for a red gate on work the owner asked for, which goes round again.** The sentence
    // above is the reason to stop, and for `GateFailed` it is no longer true: `record_gate` reverts
    // the item to its footing before the queue is allowed to move, so by the time this is read the
    // rejected work is already off the branch and there is nothing to build on top of. `next_step`
    // made exactly this move once already, taking `GateFailed` out of its short-circuit so a red
    // gate stopped ending the round at the first of five items — "what survives is the ENDING". The
    // same argument crosses a round boundary, and this is the same move made there.
    //
    // Told to stop and it stops: the ceiling, the dry-round brake and the item's own gate retries
    // all still apply, so this buys ROUNDS the owner already authorised and never an extra one. And
    // it is scoped to `GateFailed` alone. `GateErrored` is deliberately not reverted — a gate that
    // would not run measured nothing, so its work might be perfectly good and destroying it would
    // be worse than stopping — which means the rejected-work argument is still exactly true for it,
    // and it still ends the job here. So does `Failed`.
    //
    // **What this does NOT do is soften the ending.** The failed item keeps its row, `failed_ending`
    // keeps finding it, and when the rounds do run out the job is still reported `gate_failed`. A
    // replan that supersedes the item with a different approach cannot clear that mark — worth
    // knowing before reading a `gate_failed` job as one that achieved nothing.
    if let Some(outcome) = failed_ending(job) {
        let go_round_again = matches!(outcome, Outcome::GateFailed | Outcome::Failed)
            && job.rounds.a_broken_item_may_go_round_again
            && job.rounds.round + 1 < job.rounds.max_rounds
            && job.rounds.dry_rounds < DRY_ROUNDS_TO_STOP;
        if !go_round_again {
            return Next::Finish(outcome);
        }
        // The in-flight check that the happy path gets below, and it is needed here for the same
        // reason: between spawning a replan and its items landing the queue is unchanged, so
        // without this every tick would spawn another one.
        return if job.rounds.replanning {
            Next::Wait
        } else {
            Next::SpawnReplan
        };
    }

    // (1) The replan declared itself done. The cheapest ending there is, and the most trustworthy:
    // the node that just looked at the work is the one saying the work is over.
    if job.rounds.replanned == Replan::Done {
        return Next::Finish(Outcome::Completed);
    }
    if job.rounds.replanning {
        return Next::Wait;
    }
    // (3) It dried up. Ahead of the ceiling deliberately: both stop the job, and this one is the
    // reading a person can act on — `completed` here means "there was nothing left", where the
    // ceiling below means "there may well have been".
    if job.rounds.dry_rounds >= DRY_ROUNDS_TO_STOP {
        return Next::Finish(Outcome::Completed);
    }
    // (4) Out of rounds. `round` counts from 0, so the round that closes is `round + 1` of them.
    if job.rounds.round + 1 >= job.rounds.max_rounds {
        return Next::Finish(Outcome::Stopped);
    }
    Next::SpawnReplan
}

/// PURE: how a job that ran its whole queue ended.
///
/// One bad item anywhere decides it, however many passed after it — which one, and why that one,
/// is [`failed_ending`]. The verdict is about the branch that is handed back, and a branch carrying
/// an item the gate rejected is not one somebody should be told is complete.
///
/// Skipped items deliberately do NOT show up here. They are work that was never attempted, recorded
/// as proposals for a person to decide on; a job that ran everything it was allowed to run did what
/// was asked of it, and reporting that as a failure would teach the reader to ignore the word.
/// `Orphaned` is the state that looks like `Skipped` and is not: nobody is being asked anything
/// about an orphan, and something did break upstream of it.
pub(super) fn ending(job: &JobView) -> Outcome {
    failed_ending(job).unwrap_or(Outcome::Completed)
}

/// PURE: the unhappy ending this queue carries, or `None` if it carries none.
///
/// **The one place the precedence between them is written**, which is the whole reason it exists.
/// `ending` and `close_the_round` both knew `GateFailed` and nothing else, and they agreed with
/// each other and with `next_step` only because the short-circuit at the top of `next_step` got to
/// `Failed` and `GateErrored` before either of them was ever called. A team job removes that net —
/// all three arrive at the end of the queue together — and two copies of one rule agree until the
/// day somebody edits one of them.
///
/// The order, worst first:
///
/// 1. `GateErrored`. Silence beats a verdict. A gate that would not start measured NOTHING, so
///    every other reading in this queue is in doubt, and it is a fact about the machine rather
///    than about the code — which makes it the one to act on first.
/// 2. `Failed`. The work broke.
/// 3. `GateFailed`. The gate looked and said no.
/// 4. `Orphaned`, and it never decides. An orphan exists only beside a dependency that ended
///    badly, and a chain of orphans has a real failure at its root, so one of the three above is
///    always present with it. The arm is here to make the table total: if it ever does decide,
///    that invariant has broken, and `failed` is the honest reading of a job that did not do what
///    it was asked with nobody having stopped it.
///
/// `Cancelled` is deliberately absent. It stops the job at the short-circuit, team or no team, and
/// never reaches here.
///
/// `Superseded` is absent too, and that absence is the point of the state: a later round took the
/// failure over, and the item that did is the one read here.
pub(super) fn failed_ending(job: &JobView) -> Option<Outcome> {
    if job
        .items
        .iter()
        .any(|item| item.state == ItemState::GateErrored)
    {
        return Some(Outcome::GateErrored);
    }
    if job.items.iter().any(|item| item.state == ItemState::Failed) {
        return Some(Outcome::Failed);
    }
    if job
        .items
        .iter()
        .any(|item| item.state == ItemState::GateFailed)
    {
        return Some(Outcome::GateFailed);
    }
    if job
        .items
        .iter()
        .any(|item| item.state == ItemState::Orphaned)
    {
        return Some(Outcome::Failed);
    }
    None
}

impl Outcome {
    /// The `jobs.status` this outcome is stored as.
    pub fn as_status(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Failed => "failed",
            Outcome::Cancelled => STATUS_CANCELLED,
            Outcome::GateFailed => "gate_failed",
            Outcome::GateErrored => "gate_errored",
            Outcome::Stopped => STATUS_STOPPED,
        }
    }
}

/// PURE: what an item's row means, read as the three numbers it takes to mean anything.
///
/// `gate_attempts` counts how many times THIS item's gate has gone red; `gate_retries` is what the
/// job allows. Neither says anything alone, and the pair is the only thing that can tell "try again"
/// from "give up" — which is why the status is not enough and this takes all three.
///
/// `gate_errored` is deliberately outside that arithmetic, whatever the budget says. A non-zero exit
/// is a verdict about the code and is worth another attempt; a gate that would not run measured
/// nothing, so there is nothing to attempt again, and buying past it with a retry would mean
/// building on work nothing has looked at.
pub fn item_state_from(status: &str, gate_attempts: i64, gate_retries: i64) -> ItemState {
    match status {
        "running" => ItemState::Running,
        "implemented" => ItemState::Implemented,
        "passed" => ItemState::Passed,
        "failed" => ItemState::Failed,
        STATUS_CANCELLED => ItemState::Cancelled,
        // `<=` and not `<`: the budget is EXTRA implement runs, and the attempt that has just been
        // counted is the one being answered. One attempt against a budget of one is the first red
        // gate of a job allowed one retry, which is exactly the case the retry exists for.
        "gate_failed" if gate_attempts <= gate_retries => ItemState::GateRetriable,
        "gate_failed" => ItemState::GateFailed,
        "gate_errored" => ItemState::GateErrored,
        STATUS_SKIPPED => ItemState::Skipped,
        STATUS_SUPERSEDED => ItemState::Superseded,
        // The four states of parallel items. Nothing writes these rows yet, and they are read
        // ahead of the writer on purpose: the `_ =>` below would take any of them for `pending`
        // and hand the item back to the queue as work nobody had started.
        "merging" => ItemState::Merging,
        "conflicted" => ItemState::Conflicted,
        "reverted" => ItemState::Reverted,
        "orphaned" => ItemState::Orphaned,
        // An unrecognised item status is treated as still to do rather than as done. Erring toward
        // "not finished" costs a repeated item; erring the other way silently skips work the job
        // was created to perform and reports it complete.
        _ => ItemState::Pending,
    }
}

impl ItemState {
    /// PURE: the `job_items.status` a startable item is sitting in, or `None` if it is not startable.
    ///
    /// The inverse of [`item_state_from`], and kept against it rather than anywhere near the SQL on
    /// purpose. `spawn_node` claims an item with a compare-and-swap that has to name the status it
    /// expects to find, and there are now two of those — `pending` for an item nobody has attempted,
    /// `gate_failed` for one going round again. The claim gets that answer from the verdict already
    /// reached by `item_state_from`, which is the ONE place the rule about which red gates are
    /// retriable (`gate_attempts <= gate_retries`) is written. Every caller asks it rather than
    /// restating it — this one and `record_gate`, which passes it the two numbers it has just read
    /// back and believes the answer. A `WHERE` clause or an inline comparison that recomputed the
    /// rule would be the same rule in two places, agreeing only until one of them was edited, and
    /// this repository has been bitten by that three times — each time under a comment asserting the
    /// two agreed.
    ///
    /// `None` for everything else, including the terminal states. An item that is not work cannot be
    /// claimed, and answering with a status for one would let a caller start a node on finished work.
    pub(super) fn claimable_as(self) -> Option<&'static str> {
        match self {
            ItemState::Pending => Some("pending"),
            ItemState::GateRetriable => Some("gate_failed"),
            // The resolution node's claim. Its caller arrives with the merge step, in a later
            // slice; the arm is written here because this function is one half of a pair kept
            // against the other half, and deferring it is the same edit with a chance of being
            // forgotten in between.
            ItemState::Conflicted => Some("conflicted"),
            // `Merging` and `Reverted` are steps the driver takes, not nodes anybody spawns, and
            // `Orphaned` is terminal.
            ItemState::Running
            | ItemState::Implemented
            | ItemState::Passed
            | ItemState::Failed
            | ItemState::Cancelled
            | ItemState::GateFailed
            | ItemState::GateErrored
            | ItemState::Skipped
            | ItemState::Merging
            | ItemState::Reverted
            | ItemState::Orphaned
            | ItemState::Superseded => None,
        }
    }
}

/// Whether a run status means a node is still a job's to wait for.
///
/// `awaiting_approval` counts. It is not terminal — a person is expected to answer — and reading it
/// as finished would let the job walk past a node that has not done its work, or complete while its
/// review sits paused on a question nobody has been shown yet.
pub(super) fn node_in_flight(status: &str) -> bool {
    matches!(status, "running" | "awaiting_approval")
}

/// The stage a job is really at, seeing through a parked one.
///
/// `waiting` overwrites the status underneath it, and exactly one of those carries meaning the job
/// cannot reconstruct: `planning` says the queue does not exist yet. A job parked for budget while
/// still planning would otherwise resume as planned-with-an-empty-queue, which is the honest
/// "there was no work" night, and report itself complete without having planned anything.
pub(super) fn effective_status<'a>(status: &'a str, resume_status: Option<&'a str>) -> &'a str {
    match (status, resume_status) {
        (status, Some(stage)) if is_paused(status) => stage,
        _ => status,
    }
}

/// The statuses that stand in front of a stage rather than replacing it.
///
/// Both are things happening *to* a job rather than things it is doing, and both end with it going
/// back to what it was at. `STATUS_AWAITING_APPROVAL` is not `waiting` because they are answered
/// differently: one clears itself when a window reopens, the other never clears until a person
/// looks at the approval queue.
pub(super) fn is_paused(status: &str) -> bool {
    matches!(status, "waiting" | STATUS_AWAITING_APPROVAL)
}
