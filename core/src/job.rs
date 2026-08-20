//! The job state machine: a sequence of runs over one shared worktree.
//!
//! A run is one `claude -p` subprocess and therefore one context window, which caps how large a
//! piece of autonomous work can be. A job lifts that cap by running several nodes in sequence —
//! `plan → implement×N → gate → review` — each with a fresh window, sharing state through the
//! worktree on disk rather than through a transcript.
//!
//! The decisions and the I/O are kept on opposite sides of a line, the same way `classifier.rs` is
//! split from `hooks.rs`: `next_step` is pure, so every interesting question about a job ("does a
//! red gate stop the chain?", "where does a resume pick up?") is a table test with no database, no
//! worktree and no subprocess. Everything below it is allowed to be dumb.
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::state::AppState;

/// Why a plan node produced no usable queue.
///
/// `Absent` and an empty `items` list are different outcomes and must stay that way: the first says
/// the plan node failed to produce anything, the second says it looked and found no work. Only the
/// first is a failure.
#[derive(Debug)]
pub enum PlanError {
    Absent,
    Unreadable(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Absent => write!(formatter, "the plan node wrote no plan.json"),
            PlanError::Unreadable(reason) => write!(formatter, "plan.json is unreadable: {reason}"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PlanFile {
    /// Absent when a replan node wrote `{"done": true}`. A plan node always writes it, so the
    /// default is what makes ONE shape read both files — and what makes a plan node that forgot the
    /// key indistinguishable from one that found nothing, which is the reading `parse_plan`'s caller
    /// already treats as a successful empty night.
    #[serde(default)]
    items: Vec<PlanItem>,
    /// A replan node saying the work is over. `#[serde(default)]` so a plan node's file, which never
    /// carries it, reads as `false` rather than as a parse failure.
    #[serde(default)]
    done: bool,
    /// Why it is over, in the node's own words. Kept for the feed: "job 12 finished after 3 rounds"
    /// is a fact, and the reason it stopped asking is the part a person can disagree with.
    #[serde(default)]
    why: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlanItem {
    description: String,
    /// Defaulted rather than required, because the field is younger than the plans that have to
    /// keep parsing: every `plan.json` written before it existed omits it, and so does any planner
    /// that takes the prompt's offer to decline. Both mean the same thing, which is nothing.
    #[serde(default)]
    files: Vec<String>,
}

/// One item of the queue: what to do, and where the planner guessed it lives.
#[derive(Debug, PartialEq, Eq)]
pub struct PlannedItem {
    pub description: String,
    /// Empty when the planner named nothing. A hint and not a boundary — it is where the implement
    /// node starts looking, never the extent of what it may touch.
    pub files: Vec<String>,
}

/// A validated work queue, plus however much of it did not fit.
#[derive(Debug, PartialEq, Eq)]
pub struct PlannedItems {
    pub items: Vec<PlannedItem>,
    /// Items the daemon's ceiling cut. Carried rather than discarded so the feed can say what was
    /// left out — a queue silently trimmed reads downstream as the whole of what the planner found.
    pub dropped: usize,
    /// A replan node declaring the work over, and why.
    ///
    /// Distinct from an empty `items` on purpose, and the distinction is the whole of ending #1
    /// versus ending #3: "there is nothing more to do" is a claim the node is making, while an empty
    /// queue is a round that happened to produce nothing and might not be the last. One ends the job
    /// now; the other feeds a counter that ends it after two.
    pub done: Option<String>,
}

/// Reads the queue a plan node produced.
///
/// Takes bytes rather than a path so the decisions here stay testable without a filesystem, and so
/// "the file was missing" is expressed by the caller as `None` rather than inferred from an IO error
/// that could equally mean a permissions problem.
pub fn parse_plan(contents: Option<&[u8]>, max_items: usize) -> Result<PlannedItems, PlanError> {
    let bytes = contents.ok_or(PlanError::Absent)?;
    let parsed: PlanFile =
        serde_json::from_slice(bytes).map_err(|error| PlanError::Unreadable(error.to_string()))?;

    let total = parsed.items.len();
    let items: Vec<PlannedItem> = parsed
        .items
        .into_iter()
        .take(max_items)
        .map(|PlanItem { description, files }| PlannedItem { description, files })
        .collect();

    Ok(PlannedItems {
        dropped: total - items.len(),
        items,
        // `done` wins over any items alongside it, and the alternative would be worse in both
        // directions: honouring the items would queue work the node just said was unnecessary, and
        // treating the pair as malformed would fail a job over a node being redundant.
        done: parsed
            .done
            .then(|| parsed.why.unwrap_or_else(|| "no reason given".to_owned())),
    })
}

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
}

/// Every variant of [`ItemState`], for the tests that have to say something about all of them.
///
/// A hand-kept list, held to the enum by `every_item_state_covers_the_enum`. The alternative is a
/// derive macro for the sake of one array, and the alternative to both — tests that enumerate the
/// states inline — is what lets a new variant be born untested everywhere at once.
#[cfg(test)]
const EVERY_ITEM_STATE: [ItemState; 14] = [
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
];

/// Whether the job still owes a review node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    NotWanted,
    Pending,
    Running,
    Done,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    SpawnPlan,
    SpawnImplement {
        ordinal: usize,
    },
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
    /// Whether a plan node has already produced a queue. Distinct from `items` being empty, which
    /// is a planner that looked and found no work.
    pub planned: bool,
    /// Whether a plan node is in flight right now.
    ///
    /// Separate from `planned` because the queue does not exist yet in either case, and without it
    /// every tick would answer `SpawnPlan` while the first planner was still running. The item
    /// queue is what stops that from happening to an implement node; the plan node has no item.
    pub planning: bool,
    /// The queue of the CURRENT round, never of every round the job has had.
    ///
    /// Load-bearing and easy to get wrong: read without filtering on `job_items.round`, round 2
    /// would see round 1's finished items sitting beside its own and start the round again from the
    /// first pending one it found.
    pub items: Vec<ItemState>,
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
    for item in &job.items {
        match item {
            ItemState::Failed if !job.has_team => return Next::Finish(Outcome::Failed),
            ItemState::Cancelled => return Next::Finish(Outcome::Cancelled),
            ItemState::GateErrored if !job.has_team => return Next::Finish(Outcome::GateErrored),
            _ => {}
        }
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
    // `Reverted` waits alongside `Running`, and is the last state left with no step of its own and
    // no writer. It is here because the alternative is worse than being early: a non-terminal state
    // this search walked past would be read as finished by every check after it, which is the one
    // direction `item_state_from` already refuses to err in. `Merging` and `Conflicted` have both
    // left this list, each having been given a step below.
    if job
        .items
        .iter()
        .any(|item| matches!(item, ItemState::Running | ItemState::Reverted))
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
        .position(|item| *item == ItemState::Merging)
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
        .position(|item| *item == ItemState::Implemented)
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
    if let Some(ordinal) = job.items.iter().position(|item| {
        matches!(
            item,
            ItemState::Pending | ItemState::GateRetriable | ItemState::Conflicted
        )
    }) {
        return Next::SpawnImplement { ordinal };
    }

    // Two ways a round closes without being reviewed, and both are about not spending a run for
    // nothing. A replan that declared the work over closed a round that had already been reviewed
    // before it ran — a second review would re-read a branch nobody is going to change. And a round
    // that queued no items has no diff of its own to read at all.
    if job.rounds.replanned == Replan::Done || job.rounds.round_added_nothing {
        return close_the_round(job);
    }

    match job.review {
        ReviewState::Pending => Next::SpawnReview,
        ReviewState::Running => Next::Wait,
        ReviewState::NotWanted | ReviewState::Done => close_the_round(job),
    }
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
fn close_the_round(job: &JobView) -> Next {
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
    if let Some(outcome) = failed_ending(job) {
        return Next::Finish(outcome);
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
fn ending(job: &JobView) -> Outcome {
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
fn failed_ending(job: &JobView) -> Option<Outcome> {
    if job.items.contains(&ItemState::GateErrored) {
        return Some(Outcome::GateErrored);
    }
    if job.items.contains(&ItemState::Failed) {
        return Some(Outcome::Failed);
    }
    if job.items.contains(&ItemState::GateFailed) {
        return Some(Outcome::GateFailed);
    }
    if job.items.contains(&ItemState::Orphaned) {
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
fn item_state_from(status: &str, gate_attempts: i64, gate_retries: i64) -> ItemState {
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
    fn claimable_as(self) -> Option<&'static str> {
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
            | ItemState::Orphaned => None,
        }
    }
}

/// Whether a run status means a node is still a job's to wait for.
///
/// `awaiting_approval` counts. It is not terminal — a person is expected to answer — and reading it
/// as finished would let the job walk past a node that has not done its work, or complete while its
/// review sits paused on a question nobody has been shown yet.
fn node_in_flight(status: &str) -> bool {
    matches!(status, "running" | "awaiting_approval")
}

/// The stage a job is really at, seeing through a parked one.
///
/// `waiting` overwrites the status underneath it, and exactly one of those carries meaning the job
/// cannot reconstruct: `planning` says the queue does not exist yet. A job parked for budget while
/// still planning would otherwise resume as planned-with-an-empty-queue, which is the honest
/// "there was no work" night, and report itself complete without having planned anything.
fn effective_status<'a>(status: &'a str, resume_status: Option<&'a str>) -> &'a str {
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
fn is_paused(status: &str) -> bool {
    matches!(status, "waiting" | STATUS_AWAITING_APPROVAL)
}

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

    let (round, dry_rounds, max_rounds, replan_done, opened_by): (
        i64,
        i64,
        Option<i64>,
        i64,
        Option<i64>,
    ) = sqlx::query_as(
        "SELECT round, dry_rounds, max_rounds, replan_done, replan_run_id FROM jobs WHERE id = ?",
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
    let items: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, gate_attempts FROM job_items WHERE job_id = ? ORDER BY ordinal",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;

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

    let plan_run = latest_node("plan").await?;

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
    let review_run: Option<String> = sqlx::query_scalar(
        "SELECT status FROM runs WHERE job_id = ? AND stage = 'review' AND id > ?
         ORDER BY id DESC LIMIT 1",
    )
    .bind(job_id)
    .bind(round_opened_at)
    .fetch_optional(pool)
    .await?;

    let review = match (review_wanted != 0, review_run.as_deref()) {
        (false, _) => ReviewState::NotWanted,
        (true, None) => ReviewState::Pending,
        (true, Some(status)) if node_in_flight(status) => ReviewState::Running,
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
        planned: stage != "planning" || !items.is_empty(),
        planning: plan_run.as_deref().is_some_and(node_in_flight),
        items: items
            .iter()
            .map(|(status, attempts)| item_state_from(status, *attempts, gate_retries))
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
        },
        has_team: team_id.is_some(),
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
/// Gives the concurrency slot back too, and this is the one place that does it for jobs — every
/// ending funnels here, from `finish` to `cancel` to the startup reconciliation, so a new ending
/// added later cannot forget. The sweep on the job tick is a backstop, not the mechanism: it frees
/// what a crash left held, within a tick, rather than what this function forgot.
pub async fn retire(pool: &SqlitePool, job_id: i64, status: &str) -> sqlx::Result<()> {
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
    sqlx::query(
        "UPDATE jobs SET status = ?, completed_at = ?, wait_reason = NULL, resume_status = NULL
         WHERE id = ?",
    )
    .bind(status)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(job_id)
    .execute(pool)
    .await?;
    crate::concurrency::release(pool, crate::worktree::Owner::Job(job_id)).await?;
    Ok(())
}

/// Records how a job ended.
pub async fn finish(pool: &SqlitePool, job_id: i64, outcome: Outcome) -> sqlx::Result<()> {
    retire(pool, job_id, outcome.as_status()).await
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
    sqlx::query(
        "UPDATE jobs
         SET resume_status = CASE WHEN status NOT IN ('waiting','awaiting_approval')
                                  THEN status ELSE resume_status END,
             status = ?,
             wait_reason = ?
         WHERE id = ?",
    )
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
/// The shape is copied onto the row rather than re-read per node: `.ai/autopilot.yaml` can be
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
            gate_retries, head_sha, max_rounds, budget_usd, created_at)
         VALUES (?, ?, ?, ?, 'planning', ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// What `POST /jobs` accepts.
///
/// Neither the mode nor the root appears here, and neither ever will: both are resolved from the
/// project's own autopilot state (`resolve_start`), because a caller that could name its own root
/// would be naming a directory this daemon then creates a worktree in and writes to.
#[derive(Deserialize)]
pub struct CreateJobRequest {
    pub project_id: String,
    pub prompt: String,
    /// What this job may spend on itself, under the house limit rather than instead of it. `None`
    /// means only the house limit governs — what every `graph:` rule has always meant.
    pub budget_usd: Option<f64>,
    /// How many rounds it may run. Cut by `config::rounds_allowed` before it is stored, never after:
    /// this number comes from a model filling in a tool call, and a ceiling applied at read time is
    /// a ceiling one forgetful caller can walk past. `None` is one round.
    pub max_rounds: Option<i64>,
}

/// Why a job cannot be started for a project.
///
/// Three refusals rather than one error string, because a caller answers them differently: an
/// unknown project is a 404, and the other two are a 422 about a machine configured differently
/// from what the request assumed rather than a request that is malformed.
#[derive(Debug, PartialEq, Eq)]
pub enum StartRefusal {
    UnknownProject,
    NotActive(crate::autopilot::Mode),
    NoRoot,
}

impl StartRefusal {
    /// The sentence handed back to whoever asked. It says which state the project is in, because a
    /// 422 that will not say what is wrong is a 422 somebody retries unchanged.
    pub fn reason(&self, project_id: &str) -> String {
        match self {
            Self::UnknownProject => format!("unknown project: {project_id}"),
            Self::NotActive(crate::autopilot::Mode::Shadow) => format!(
                "project {project_id} is in shadow mode, which is plan-only; a job writes to a \
                 worktree and needs active"
            ),
            Self::NotActive(_) => format!("project {project_id} has autopilot off"),
            Self::NoRoot => format!("project {project_id} is active but has no root recorded"),
        }
    }
}

/// What a start resolved to.
///
/// One field today. It is a value rather than a bare `String` because Chunk 4 adds the claimed slot
/// number beside it, and a caller that had unwrapped a `String` would have to be rewritten then.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedStart {
    pub project_root: String,
}

/// PURE: what a job request resolves to, or why it does not.
///
/// Neither the mode nor the root is ever chosen by whoever asks — both are read off the project's
/// own autopilot state, exactly as `resolve_run_request` does for runs.
///
/// **A job REQUIRES `Mode::Active`, and that is where this deliberately differs from that
/// function.** `resolve_run_request` maps `Shadow` onto the `shadow` run mode, which is right for a
/// run because a run can be plan-only. A job cannot: its plan node has to write the `plan.json`
/// that the queue is taken from, and a plan-only node writes nothing — so a job quietly demoted to
/// shadow would do nothing whatsoever while reporting that it was working all night. `Shadow` and
/// `Off` are refusals with reasons of their own, never fallbacks. Copying that table across without
/// noticing is the natural mistake here, which is why it has a test of its own.
pub fn resolve_start(
    roster: &[crate::autopilot::ProjectSummary],
    project_id: &str,
) -> Result<ResolvedStart, StartRefusal> {
    let project = roster
        .iter()
        .find(|project| project.project_id == project_id)
        .ok_or(StartRefusal::UnknownProject)?;
    if project.mode != crate::autopilot::Mode::Active {
        return Err(StartRefusal::NotActive(project.mode));
    }
    // Active with no root is a real state rather than an impossible one: the root is recorded when
    // a project is pointed at a directory, and the mode can be set without that having happened.
    let project_root = project.project_root.clone().ok_or(StartRefusal::NoRoot)?;
    Ok(ResolvedStart { project_root })
}

/// How a start attempt ended.
///
/// `NoRoom` replaced `AlreadyLive` when `one_live_job_per_project` came out (migration 0053). The
/// two are the same answer at different ceilings — that index could only ever say "one" — and they
/// are still not a check that failed here: the slot `INSERT` is the lock, so a scheduler tick and a
/// manual request racing for the same project cannot both pass a count and then both proceed.
///
/// Kept apart from `Failed` because the caller turns them into different answers: no room is a 409
/// the asker can act on by waiting, and `Failed` is a 500 nothing they do would have helped.
pub enum JobStart {
    Started(i64),
    NoRoom(String),
    Failed,
}

/// Everything `start` needs, with no trace of who is asking.
///
/// Deliberately plain values rather than the `&ScheduleRule` + `&GraphConfig` this used to take.
/// Those types made the scheduler the only caller that could exist — a rule and a graph config are
/// what a *rule* has — and the request now arrives from `POST /jobs` as well. Whatever ceiling the
/// caller applies to `max_items` is applied before it gets here, so this function has one job.
pub struct StartRequest<'a> {
    pub project_id: &'a str,
    pub project_root: &'a str,
    /// `None` for a job nobody scheduled. See `NewJob::rule_name`.
    pub rule_name: Option<&'a str>,
    pub prompt: &'a str,
    pub max_items: i64,
    pub gate_each: bool,
    pub review: bool,
    /// Already cut by whoever asked, exactly as `max_items` is. See [`NewJob::gate_retries`].
    pub gate_retries: i64,
    pub head_sha: Option<&'a str>,
    /// Both `None` for a scheduled job: a `graph:` rule asks for neither, which keeps it at one
    /// round under the house limit — exactly what it did before rounds existed.
    pub max_rounds: Option<i64>,
    pub budget_usd: Option<f64>,
}

/// Creates a job and provisions the worktree it will live in.
///
/// The worktree belongs to the JOB, not to any of its nodes — that is the whole reason a job can
/// outlive one context window, and it is why this provisions it here rather than letting the first
/// node do it.
///
/// This lives in `job.rs` rather than in `scheduler.rs` because creating jobs is what this module
/// is for; the module map describes the scheduler as firing runs "through `runs::create_run_inner`;
/// never defining them", and the same applies to jobs. It moved here when a second caller appeared:
/// two copies of this sequence would mean one of them learning a fix the other never learns.
pub async fn start(state: &AppState, request: &StartRequest<'_>) -> JobStart {
    // Asked before the row exists, and it is not the decision — `claim` below is. A job cannot claim
    // until it has an id, so the authoritative answer costs a row, and a scheduler firing at a full
    // project every half hour would leave a retired job behind every time. This turns that into the
    // rare case where two starts actually crossed.
    match crate::concurrency::room_for(&state.pool, request.project_id).await {
        Ok(Some(full)) => return JobStart::NoRoom(full.reason()),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                project_id = request.project_id,
                %error,
                "could not read the concurrency ceiling"
            );
            return JobStart::Failed;
        }
    }

    let job_id = match insert_job(
        &state.pool,
        &NewJob {
            project_id: request.project_id,
            project_root: request.project_root,
            rule_name: request.rule_name,
            prompt: request.prompt,
            max_items: request.max_items,
            gate_each: request.gate_each,
            review: request.review,
            gate_retries: request.gate_retries,
            head_sha: request.head_sha,
            max_rounds: request.max_rounds,
            budget_usd: request.budget_usd,
        },
    )
    .await
    {
        Ok(job_id) => job_id,
        Err(error) => {
            tracing::warn!(
                project_id = request.project_id,
                rule_name = request.rule_name.unwrap_or("(none)"),
                %error,
                "could not start a job"
            );
            return JobStart::Failed;
        }
    };

    let owner = crate::worktree::Owner::Job(job_id);

    // AFTER the row, not before it, and the reason is arithmetic rather than taste: a slot is keyed
    // on its owner's id, and the job has no id until it is inserted. So the row goes in first and is
    // retired again if there is no room — the same shape this function already uses when a worktree
    // cannot be provisioned, and it keeps the claim itself an INSERT that two racing jobs resolve on
    // the primary key rather than on a read.
    match crate::concurrency::claim(&state.pool, request.project_id, owner).await {
        Ok(crate::concurrency::ClaimOutcome::Claimed(_)) => {}
        // The race the pre-check cannot cover: somebody took the last slot in between. No room is
        // not a failure, so the row is retired quietly and the caller is told which ceiling it hit.
        Ok(crate::concurrency::ClaimOutcome::Full(full)) => {
            let _ = retire(&state.pool, job_id, STATUS_CANCELLED).await;
            return JobStart::NoRoom(full.reason());
        }
        Err(error) => {
            return fail_early(
                state,
                request.project_id,
                job_id,
                &format!("its concurrency slot could not be claimed: {error}"),
            )
            .await;
        }
    }

    let info = match crate::worktree::create(Path::new(request.project_root), owner).await {
        Ok(info) => info,
        Err(error) => {
            return fail_early(state, request.project_id, job_id, &format!("{error}")).await;
        }
    };
    let path = info.path.to_string_lossy().into_owned();
    if let Err(error) = crate::worktree::record(
        &state.pool,
        owner,
        request.project_id,
        request.project_root,
        &path,
        &info.branch,
        info.base_sha.as_deref(),
    )
    .await
    {
        // The directory exists and nothing in the database knows it does. Left alone it would be
        // invisible to the GC forever; the startup orphan sweeper recognises `job-<id>` and is what
        // eventually collects it.
        return fail_early(
            state,
            request.project_id,
            job_id,
            &format!("its worktree was created but could not be recorded: {error}"),
        )
        .await;
    }

    // Two sentences rather than one with a hole in it. A job nobody scheduled has no rule, and
    // `for rule 'None'` would be a line a person reads as a bug in the scheduler.
    let started = match request.rule_name {
        Some(rule_name) => format!(
            "job {job_id} started for rule '{rule_name}' on {}",
            info.branch
        ),
        None => format!("job {job_id} started on {}", info.branch),
    };
    let _ = crate::feed::append(
        &state.pool,
        Some(request.project_id),
        "job_started",
        &started,
        None,
    )
    .await;
    JobStart::Started(job_id)
}

/// Retires a job that never got as far as its first node, and says so where a person will see it.
///
/// A job left live with no worktree would be ticked forever and hold a concurrency slot,
/// which would take the whole project's autonomy down with it — silently, since nothing else logs.
async fn fail_early(state: &AppState, project_id: &str, job_id: i64, why: &str) -> JobStart {
    if let Err(error) = retire(&state.pool, job_id, Outcome::Failed.as_status()).await {
        tracing::error!(
            project_id,
            job_id,
            %error,
            "a job could not be provisioned AND could not be retired; it holds the project's job slot"
        );
    }
    let _ = crate::feed::append(
        &state.pool,
        Some(project_id),
        "job_failed",
        &format!("job {job_id} could not start: {why}"),
        None,
    )
    .await;
    JobStart::Failed
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
#[cfg(test)]
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
    pub created_at: String,
}

impl JobRow {
    fn stage(&self) -> &str {
        effective_status(&self.status, self.resume_status.as_deref())
    }
}

/// Spelled out rather than built from `LIVE_STATUSES`, because sqlx refuses SQL assembled at
/// runtime — a guard worth keeping. The test named above compares this text against both the
/// constant and the migration's index, so the three cannot drift apart in silence.
const LIVE_JOBS_SQL: &str = "SELECT id, project_id, project_root, prompt, status, resume_status,
                                    wait_reason, max_items, gate_each, head_sha, round, budget_usd,
                                    created_at
                             FROM jobs
                             WHERE status IN ('planning','implementing','gating','reviewing',
                                              'awaiting_approval','waiting')
                             ORDER BY id";

const ONE_JOB_SQL: &str = "SELECT id, project_id, project_root, prompt, status, resume_status,
                                  wait_reason, max_items, gate_each, head_sha, round, budget_usd,
                                    created_at
                           FROM jobs WHERE id = ?";

pub async fn live_jobs(pool: &SqlitePool) -> sqlx::Result<Vec<JobRow>> {
    sqlx::query_as(LIVE_JOBS_SQL).fetch_all(pool).await
}

async fn load_job(pool: &SqlitePool, job_id: i64) -> sqlx::Result<JobRow> {
    sqlx::query_as(ONE_JOB_SQL)
        .bind(job_id)
        .fetch_one(pool)
        .await
}

/// How long a job may live, counting every minute it spent parked.
///
/// Decision 14. Counting `waiting` is the whole point: without it, parking would be a way around
/// the ceiling, and a starved job would hold a worktree and the project's slot indefinitely. A
/// nightly job that starts at 03:00 is retired by 07:00, which is why the attention brake rarely
/// has to be the thing that stops it.
pub const MAX_JOB_LIFETIME: chrono::Duration = chrono::Duration::hours(4);

/// How many moves one pass will make for a single job.
///
/// A pass stops as soon as it starts a node, so the only steps that chain are the ones that cost no
/// run: a gate, and the finish that may follow it. The bound is a backstop against a state machine
/// that decides it can move forever without ever spawning anything.
const MAX_STEPS_PER_PASS: usize = 4;

/// Whether a job can move again in the same pass.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// A move was made that costs no run; the next one can follow immediately.
    Continued,
    /// Nothing more happens until a node lands, a brake lifts, or somebody looks at it.
    Stopped,
}

/// What every planning node has to be told about who owns the history.
///
/// Measured, not guessed. Job 12 on 2026-08-08 asked for eight modules and said "I want to review
/// each one on its own" — a sentence about how to SPLIT the work. The plan node read it as a
/// sentence about git and wrote "commit X and Y as two separate commits" into all four items. Every
/// implement node then reached for `git commit`, which is a write and must ask, and under the
/// zero-approval policy the night runs on, all four items were skipped. Nothing was built.
///
/// The prompt is the right place for the fix rather than the classifier, because the command was not
/// misjudged: `git commit` genuinely writes and genuinely must ask. What was wrong is that the item
/// asked for it at all. `worktree::checkpoint` already commits the whole tree after every green
/// gate — an item that commits by hand is redoing the job's own work through the one door that
/// stops.
///
/// Stated as what happens rather than as a prohibition, deliberately. A node told only "do not
/// commit" invents a way around it; a node told the commit already happens has no reason to.
/// What the nodes that only LOOK have to be told about how to look.
///
/// Measured across four jobs on 2026-08-08, and always the same shape: a node whose whole task is to
/// inspect the tree reaches for a shell loop or a pipeline to do it — `for f in …; do cat "$f"; done`
/// three times, `git reflog`, `ls -la && … && find … | sort`. The classifier reads a LINE, not a
/// program, so none of those is recognised, and each one stopped its node dead.
///
/// It has `Read`, `Grep` and `Glob`, all of which run without asking and do the job better. Nothing
/// was missing except being told. Said as the consequence rather than as a rule, because the
/// consequence is what makes the choice obvious: this node is what the job spends to get an answer,
/// and giving it up is not free.
const LOOK_WITH_THE_READING_TOOLS: &str = "You are running unattended, so anything that needs a person is not answered — it ends this \
     node. Look with Read, Grep and Glob rather than with shell loops or pipelines: they need \
     nobody, and they are what this node has.";

const HISTORY_IS_THE_JOBS: &str = "The job commits the tree itself once an item's gate agrees, so no \
     item should ask anyone to commit, stage or branch — that work is already done for you, and an \
     item that asks for it is skipped rather than done.";

/// The prompt the plan node is given.
///
/// It says the file is the only thing read, because it is: §5.2 of the design takes the queue from
/// `plan.json` and never from stdout, so that a stream truncated mid-write cannot be parsed into a
/// plausible short queue that reads as "there was less work than expected".
pub fn plan_prompt(task: &str, max_items: usize, artifacts: &str) -> String {
    format!(
        "You are the PLAN node of an autonomous job. Break the task below into at most {max_items} \
         items that can be done one after another, in order, in the same working tree. Prefer \
         fewer, larger items to more, smaller ones.\n\n\
         {HISTORY_IS_THE_JOBS}\n\n\
         Write them to {artifacts}/plan.json and change nothing else:\n\n\
         {{\"items\": [{{\"description\": \"...\", \"files\": [\"path\", \"...\"]}}]}}\n\n\
         \"files\" is optional and best-effort — name the files you expect the item to touch if you \
         know them, and omit the field if you do not. A wrong guess costs more than no guess.\n\n\
         That file is the only thing that is read; anything you print is discarded. If there is no \
         work to do, write {{\"items\": []}} — an empty queue is a legitimate answer and is not a \
         failure. Do not begin any of the work yourself.\n\n\
         The task:\n\n{task}"
    )
}

/// The prompt the replan node is given at the end of a round.
///
/// The node's whole job is to answer one question — is there more to do? — and the two answers go to
/// the same file for the same reason the plan node's queue does: a stream truncated mid-write must
/// not be readable as a plausible short answer.
///
/// It is given the archives and told what they are, because without them the pattern this feature
/// rests on cannot work. A replan that cannot see what round 1 tried reproposes round 1, no round
/// ever comes back empty, and the "until it dries up" brake never fires — leaving `max_rounds` as
/// the only thing between the job and its budget.
///
/// Told to prefer `done` explicitly, and that is not politeness. The failure this feature has to
/// avoid is a job that will not admit it is finished: ending #4 (out of rounds) costs a full round of
/// runs to discover, where ending #1 costs one node.
pub fn replan_prompt(task: &str, round: i64, archives: &[String], artifacts: &str) -> String {
    let history = if archives.is_empty() {
        // Reachable when the archive copy failed, and the honest thing to say is that it is missing.
        // Claiming a file that is not there sends the node looking, and what it finds is nothing.
        "The earlier rounds' plans could not be recovered, so judge from the working tree and its \
         git history alone."
            .to_owned()
    } else {
        format!(
            "What the earlier rounds already tried is in {}. Do not repropose any of it.",
            archives.join(", ")
        )
    };
    format!(
        "You are the REPLAN node of an autonomous job, at the end of round {round}. The working tree \
         holds everything the job has done so far.\n\n\
         {history}\n\n\
         Decide whether the task below is finished. Write ONE of these to {artifacts}/plan.json and \
         change nothing else:\n\n\
         {{\"done\": true, \"why\": \"...\"}}\n\
         {{\"items\": [{{\"description\": \"...\"}}]}}\n\n\
         That file is the only thing that is read; anything you print is discarded. Prefer \
         {{\"done\": true}} when the task is met — saying so ends the job in one node, where leaving \
         it to run out of rounds costs a full round of work to discover the same thing. Do not begin \
         any of the work yourself.\n\n\
         {HISTORY_IS_THE_JOBS}\n\n\
         {LOOK_WITH_THE_READING_TOOLS}\n\n\
         The task:\n\n{task}"
    )
}

/// The prompt one implement node is given.
///
/// It gets the item and the queue, and no account of how the previous node reasoned — §5.4 of the
/// design makes each node's independence structural rather than requested, by never keeping a
/// session another node could resume.
///
/// `files` is the plan node's guess, appended only when it made one: a node with no hint is given
/// the brief it has always been given, word for word, rather than a paragraph about a mechanism it
/// has nothing to put in.
///
/// `gate_output` is appended on the same terms and for the same reason: the tail of what the gate
/// printed when it rejected a PREVIOUS attempt at this item, or `None` on a first attempt — which is
/// every attempt of every job that never retries anything, and which therefore has to produce the
/// prompt this function has always produced, word for word. It is what makes a retry a retry rather
/// than a second independent guess at the same item, because §5.4 keeps the second node from ever
/// seeing the first one's session: without the output travelling in the prompt, a whole run is spent
/// rediscovering what the gate already printed.
pub fn implement_prompt(
    description: &str,
    ordinal: usize,
    total: usize,
    artifacts: &str,
    files: &[String],
    gate_output: Option<&str>,
) -> String {
    let mut prompt = format!(
        "You are item {} of {total} in an autonomous job. The working tree already holds the work \
         of the earlier items; this is the only one you do.\n\n\
         {description}\n\n\
         The full queue is in {artifacts}/plan.json for context. Do not start another item and do \
         not edit that file. Your work is verified after you finish, so leave the tree building. \
         Leave it UNCOMMITTED: the job commits for you once the gate agrees, and committing by hand \
         stops this item to ask permission for something already arranged.",
        ordinal + 1
    );
    if !files.is_empty() {
        // Told where to begin, and told in the same breath that beginning is all it is. A list
        // written before any of the earlier items ran cannot know what they moved, so a node that
        // reads it as the edge of its work stops halfway and leaves the tree half-changed.
        prompt.push_str(&format!(
            "\n\nThe plan expected this item to touch {}. Start there, and treat that list as \
             possibly incomplete or wrong — it was guessed before any of the work was done. Edit \
             whatever the item actually needs.",
            files.join(", ")
        ));
    }
    if let Some(output) = gate_output {
        // Told as what happened rather than as a rule, because it IS what happened: this item was
        // attempted, the tree was measured, and the measurement is below. A node given a policy
        // about gates would argue with it; a node given the verdict on its own work fixes it.
        prompt.push_str(&format!(
            "\n\nThis item has been attempted before. That attempt finished, the project's gate ran \
             over the tree, and the gate said no — this is the tail of what it printed:\n\n{output}\n\n\
             The work that attempt left is still in the tree: nothing was undone, so you are \
             continuing it rather than starting again. Make the gate agree."
        ));
    }
    prompt
}

/// The prompt the review node is given.
///
/// Advisory by design: §5.5 gives ship/no-ship to the deterministic gate, and this opinion travels
/// with the proposal as information.
pub fn review_prompt(base: Option<&str>, artifacts: &str) -> String {
    let diff = match base {
        Some(sha) => {
            format!("Run `git diff {sha}..HEAD` — that is the whole of what this job changed.")
        }
        // No recorded base: it could not be read when the job started. Asking for the branch's own
        // commits is worse than naming a sha and better than reviewing a guess.
        None => "Run `git log --oneline` to find the commits this job made on the current branch, \
                 and review their combined diff."
            .to_owned(),
    };
    format!(
        "You are the REVIEW node of an autonomous job. Every change on this branch was written by \
         other sessions whose reasoning you cannot see, and you are not going to be shown it. \
         Judge the diff, not the intent.\n\n\
         {diff}\n\n\
         The queue those changes were meant to satisfy is in {artifacts}/plan.json. Report what is \
         wrong, what is missing against that queue, and nothing else. Change no files.\n\n\
         {LOOK_WITH_THE_READING_TOOLS}"
    )
}

/// The job's worktree, or `None` if it has none on record.
async fn job_worktree(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Option<(PathBuf, String)>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT path, branch FROM worktrees
         WHERE owner_kind = 'job' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(path, branch)| (PathBuf::from(path), branch)))
}

fn artifacts_for(worktree: &Path) -> String {
    worktree
        .join(crate::worktree::ARTIFACTS_DIR)
        .to_string_lossy()
        .into_owned()
}

async fn say(pool: &SqlitePool, job: &JobRow, kind: &str, summary: &str) {
    // A job is not a run, so the feed row carries no run id — writing the job's id into that column
    // would point every reader at whatever run happens to share the number.
    let _ = crate::feed::append(pool, Some(&job.project_id), kind, summary, None).await;
}

/// Folds a finished node's outcome back into the job.
///
/// A pass does this before it decides anything, and it is the only part that runs whatever the
/// brakes say: it records work that already happened. Refusing to write down a finished node
/// because the budget ran out would lose the node and repeat it.
async fn reconcile_nodes(state: &AppState, job: &JobRow) -> sqlx::Result<()> {
    let pool = &state.pool;

    // The two statuses excluded here are `node_in_flight`'s, written out because sqlx will not take
    // SQL built at runtime. `a_node_still_in_flight_is_not_reconciled` holds the two in step.
    let landed: Vec<(i64, String)> = sqlx::query_as(
        "SELECT i.ordinal, r.status
         FROM job_items i JOIN runs r ON r.id = i.run_id
         WHERE i.job_id = ? AND i.status = 'running'
           AND r.status NOT IN ('running','awaiting_approval')",
    )
    .bind(job.id)
    .fetch_all(pool)
    .await?;

    for (ordinal, run_status) in landed {
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
        sqlx::query("UPDATE job_items SET status = ? WHERE job_id = ? AND ordinal = ?")
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
    Ok(())
}

/// Whether one of this job's nodes has stopped to ask permission.
///
/// The job row otherwise reads `implementing` while the whole chain is blocked on a person, and
/// the only sign anywhere is a proposal in a queue that never names the job it came from.
async fn node_awaiting_approval(pool: &SqlitePool, job_id: i64) -> sqlx::Result<bool> {
    let paused: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM runs WHERE job_id = ? AND status = 'awaiting_approval' LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;
    Ok(paused.is_some())
}

/// Turns a finished plan node into the job's queue.
async fn ingest_plan(state: &AppState, job: &JobRow) -> sqlx::Result<()> {
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
                .join(crate::worktree::ARTIFACTS_DIR)
                .join(PLAN_FILE);
            tokio::fs::read(&file).await.ok()
        }
        None => None,
    };

    let planned = match parse_plan(contents.as_deref(), job.max_items.max(0) as usize) {
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

    for (ordinal, item) in planned.items.iter().enumerate() {
        // NULL rather than `[]`: the column that says nothing reads downstream as "the planner did
        // not say", where an empty array would read as "the planner said no files". A hint that
        // will not serialize is treated as one that was never given — losing a guess is cheaper
        // than failing a plan over it.
        let files = (!item.files.is_empty())
            .then(|| serde_json::to_string(&item.files).ok())
            .flatten();
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, files)
             VALUES (?, ?, ?, 'pending', ?)",
        )
        .bind(job.id)
        .bind(ordinal as i64)
        .bind(&item.description)
        .bind(files)
        .execute(pool)
        .await?;
    }
    sqlx::query("UPDATE jobs SET status = 'implementing' WHERE id = ?")
        .bind(job.id)
        .execute(pool)
        .await?;

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

/// Copies a round's plan aside and lists every archive the job has, newest last.
///
/// Best-effort, and deliberately so: a copy that fails costs the replan node its history, which the
/// prompt then says out loud rather than papering over. Failing the job instead would throw a
/// night's work away over a file copy — and the node can still read the working tree and its git log,
/// which is a worse account of what was tried but not no account at all.
///
/// Every archive, not just the one just written: round 3's replan needs to know what rounds 0, 1 and
/// 2 tried, or it reproposes the oldest of them.
async fn archive_plans(worktree: &Path, round: i64) -> Vec<String> {
    let directory = worktree.join(crate::worktree::ARTIFACTS_DIR);
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
async fn ingest_replan(state: &AppState, job: &JobRow) -> sqlx::Result<()> {
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
                .join(crate::worktree::ARTIFACTS_DIR)
                .join(PLAN_FILE);
            tokio::fs::read(&file).await.ok()
        }
        None => None,
    };
    let planned = match parse_plan(contents.as_deref(), job.max_items.max(0) as usize) {
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
async fn stop_after_replan(
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
async fn open_the_next_round(
    state: &AppState,
    job: &JobRow,
    run_id: i64,
    planned: &PlannedItems,
) -> sqlx::Result<()> {
    let pool = &state.pool;
    let round = job.round + 1;
    let next_ordinal: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(ordinal) + 1, 0) FROM job_items WHERE job_id = ?")
            .bind(job.id)
            .fetch_one(pool)
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
            "INSERT INTO job_items (job_id, ordinal, description, status, round, files)
             VALUES (?, ?, ?, 'pending', ?, ?)",
        )
        .bind(job.id)
        .bind(next_ordinal + offset as i64)
        .bind(&item.description)
        .bind(round)
        .bind(files)
        .execute(pool)
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
    .execute(pool)
    .await?;

    if !dry {
        sqlx::query("UPDATE jobs SET status = 'implementing' WHERE id = ?")
            .bind(job.id)
            .execute(pool)
            .await?;
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

/// Starts one node, and records that the item it belongs to is now in someone's hands.
/// The item a node is being started for, and the row status its claim has to find.
///
/// The two travel together because neither is usable without the other: an ordinal says which row,
/// and `held` says what that row must still say for this pass to be the one allowed to start it.
/// `held` is never guessed here — it comes from [`ItemState::claimable_as`], applied to the verdict
/// the caller's `JobView` already carries.
#[derive(Debug, Clone, Copy)]
struct ItemClaim {
    ordinal: usize,
    /// The status the item is in right now: `pending` for an item nobody has attempted, `gate_failed`
    /// for one the gate rejected with a retry left. Also what [`release_item`] puts back, so a claim
    /// that has to be given up returns the item to what it was rather than to what it resembled.
    held: &'static str,
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
async fn item_provisioning(
    pool: &SqlitePool,
    job: &JobRow,
    stage: &'static str,
    ordinal: usize,
) -> Option<crate::runs::JobItem> {
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
    let base = crate::worktree::head_sha(&path).await.ok()?;

    Some(crate::runs::JobItem {
        job_id: job.id,
        item_id,
        stage,
        base,
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
async fn catch_up_the_item(pool: &SqlitePool, job: &JobRow, ordinal: usize) -> Option<String> {
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

    match crate::worktree::catch_up(&tree, &job_branch).await {
        Ok(crate::worktree::CatchUp::Clean) => Some(format!(
            "

Your working tree has been brought up to date with `{job_branch}`, the branch this              job is building. Everything other items have landed since you last ran is now here,              and it merged cleanly.
"
        )),
        Ok(crate::worktree::CatchUp::Conflicted) => Some(format!(
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

async fn spawn_node(
    state: &AppState,
    job: &JobRow,
    stage: &'static str,
    prompt: String,
    item: Option<ItemClaim>,
    worktree: (PathBuf, String),
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
    if let Some(ItemClaim { ordinal, held }) = item {
        let claimed = sqlx::query(
            "UPDATE job_items SET status = 'running'
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

    // What earlier work on this project learned, appended at the same seam and for the same reason:
    // this is the one place every node kind passes through, and a lesson that only reached implement
    // nodes would be a lesson the planner keeps rediscovering. After the notes deliberately — a note
    // is what the owner is saying NOW about this job, and it should be the last thing read.
    //
    // Best-effort, like the notes above: a layer that cannot be read is a reason to say so, never a
    // reason to refuse to start the node.
    let learned = match crate::refine::active_for(pool, Some(job.project_id.as_str())).await {
        Ok(learned) => learned,
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read the refinement layer");
            Vec::new()
        }
    };
    if let Some(block) = crate::refine::render(&learned) {
        prompt.push_str(&block);
    }

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
        Some(item) => {
            crate::runs::create_job_item_run(
                state,
                prompt,
                job.project_id.clone(),
                job.project_root.clone(),
                item,
            )
            .await
        }
        None => {
            crate::runs::create_job_node_run(
                state,
                prompt,
                job.project_id.clone(),
                job.project_root.clone(),
                crate::runs::JobNode {
                    job_id: job.id,
                    stage,
                    worktree_path: path.to_string_lossy().into_owned(),
                    branch,
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
            if let Some(ItemClaim { ordinal, .. }) = item {
                let _ =
                    sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = ?")
                        .bind(run_id)
                        .bind(job.id)
                        .bind(ordinal as i64)
                        .execute(pool)
                        .await;
                // Denormalised on purpose: the item statuses are what a resume actually reads, and
                // this is here so a row nobody joins does not read as a lie.
                let _ = sqlx::query(
                    "UPDATE jobs SET status = 'implementing', stage_cursor = ? WHERE id = ?",
                )
                .bind(ordinal as i64)
                .bind(job.id)
                .execute(pool)
                .await;
            } else if stage == "review" {
                let _ = sqlx::query("UPDATE jobs SET status = 'reviewing' WHERE id = ?")
                    .bind(job.id)
                    .execute(pool)
                    .await;
            }
            Step::Stopped
        }
        // Decision 15: nobody holds the project's exclusivity slot between two of a job's nodes, so
        // an ordinary run can take it. Losing that race is not a failure of the work — the worktree
        // and every item gated green are untouched — so the job parks and tries again. The
        // four-hour ceiling is what stops a starved job from waiting forever.
        Err(crate::runs::CreateRunError::Busy) => {
            release_item(pool, job, item).await;
            park(
                state,
                job,
                "slot",
                "another run holds the project's worktree slot",
            )
            .await
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

/// Gives back a claim whose node never started.
///
/// Puts back the status the claim TOOK, not a fixed `pending`. The difference is a retry: an item
/// released to `pending` would have lost the red gate that made it retriable while keeping the
/// `gate_attempts` that gate cost it, so a budget of one would be spent on an attempt that never
/// ran, and the item would come back as though nothing had ever measured it.
async fn release_item(pool: &SqlitePool, job: &JobRow, item: Option<ItemClaim>) {
    let Some(ItemClaim { ordinal, held }) = item else {
        return;
    };
    let _ = sqlx::query(
        "UPDATE job_items SET status = ? WHERE job_id = ? AND ordinal = ? AND status = 'running'",
    )
    .bind(held)
    .bind(job.id)
    .bind(ordinal as i64)
    .execute(pool)
    .await;
}

/// Parks a job, and says so once rather than every thirty seconds.
async fn park(state: &AppState, job: &JobRow, reason: &str, detail: &str) -> Step {
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
async fn merge_item(state: &AppState, job: &JobRow, ordinal: usize) -> Step {
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
    let branch = crate::worktree::Owner::Item(item_id).branch_name();

    let Ok(base) = crate::worktree::head_sha(&worktree).await else {
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

    match crate::worktree::merge_branch(&worktree, &branch).await {
        Ok(true) => {
            let _ = sqlx::query(
                "UPDATE job_items SET status = 'merging' WHERE job_id = ? AND ordinal = ?",
            )
            .bind(job.id)
            .bind(ordinal as i64)
            .execute(pool)
            .await;
            Step::Continued
        }
        Ok(false) => {
            let _ = sqlx::query(
                "UPDATE job_items SET status = 'conflicted' WHERE job_id = ? AND ordinal = ?",
            )
            .bind(job.id)
            .bind(ordinal as i64)
            .execute(pool)
            .await;
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

async fn gate_item(state: &AppState, job: &JobRow, ordinal: usize, items: usize) -> Step {
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
        tracing::debug!(job_id = job.id, ordinal, why, "item not gated");
        Step::Continued
    };

    // `gate_after_each_item: false` buys back the N suite runs, and the final gate runs regardless
    // of it: a job that never measured anything would hand back a partial nobody can trust.
    if job.gate_each == 0 && !last {
        return pass_without_measuring("gate_after_each_item is off and this is not the last item")
            .await;
    }

    let command = match crate::config::load_schedule_rules(Path::new(&job.project_root)) {
        Ok(rules) => rules.gate_command,
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

    let _ = sqlx::query("UPDATE jobs SET status = 'gating' WHERE id = ?")
        .bind(job.id)
        .execute(pool)
        .await;
    let outcome = crate::gate::run_gate(
        &worktree,
        Path::new(&job.project_root),
        &command,
        crate::state::DEFAULT_GATE_TIMEOUT,
    )
    .await;
    let _ = sqlx::query("UPDATE jobs SET status = 'implementing' WHERE id = ?")
        .bind(job.id)
        .execute(pool)
        .await;
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
async fn revert_point(pool: &SqlitePool, job: &JobRow, ordinal: usize) -> Option<String> {
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
async fn footing_for(pool: &SqlitePool, job: &JobRow, ordinal: usize) -> Option<String> {
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
async fn footing_for_run(pool: &SqlitePool, job_id: i64, run_id: i64) -> Option<String> {
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
async fn job_worktree_path(pool: &SqlitePool, job_id: i64) -> Option<PathBuf> {
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
const GATE_OUTPUT_TAIL: usize = 4096;

/// The last [`GATE_OUTPUT_TAIL`] bytes of a gate's output, cut where a character actually ends.
///
/// The boundary walk is not defensive tidiness: slicing a `String` mid-character panics, and a gate
/// prints whatever the project's tools print — a test name with an accent in it, a `✗`, a path from
/// a non-ASCII branch. Cutting by raw byte index would turn somebody's stack trace into a crashed
/// daemon on the one night it mattered.
fn gate_output_tail(output: &str) -> &str {
    if output.len() <= GATE_OUTPUT_TAIL {
        return output;
    }
    let mut start = output.len() - GATE_OUTPUT_TAIL;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    &output[start..]
}

async fn record_gate(
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
            Ok(Some((worktree, _))) => match crate::worktree::checkpoint(&worktree).await {
                Ok(sha) => checkpoint_sha = Some(sha),
                Err(error) => {
                    tracing::warn!(job_id = job.id, ordinal, %error, "could not checkpoint a green item");
                }
            },
            _ => {
                tracing::warn!(
                    job_id = job.id,
                    ordinal,
                    "no worktree on record to checkpoint"
                );
            }
        }
    }

    // Red, with a retry left: keep the tree, remember what the gate said, ask for the item again.
    //
    // BEFORE the revert below, and the order is the feature rather than tidiness. That block puts
    // the tree back to this item's footing, which is precisely the work a second attempt would start
    // from — a retriable item that reached it would be handed a blank page together with an account
    // of what the gate disliked about something no longer there. Nothing is checkpointed either:
    // nothing passed, and a footing taken over rejected work is a footing the items after this one
    // would inherit.
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

        // ASKED, not restated. `attempts <= retries` written here would be the retriability rule in
        // a second place, and the two would agree only until one of them was edited — which is the
        // failure this module has already had three times, each under a comment asserting they
        // agreed. `item_state_from` owns the comparison; this passes it the status it is about to
        // write and believes the answer.
        if let Some((attempts, retries)) = spendable
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

    // Red: put the tree back before anything else touches it.
    //
    // Before the mark, deliberately. The mark is what lets the queue move on, and the queue moving
    // on to a tree still holding the rejected work is the exact defect this replaces — the item
    // after it would build on code the gate has just called broken.
    //
    // `Errored` is NOT reverted, where the plan for this chunk said both should be. A gate that
    // would not run measured nothing, so the work it did not judge might be perfectly good, and the
    // job stops here and hands the branch to a person either way. Reverting would destroy work whose
    // only crime is that nothing looked at it. `Failed` is different in kind: something looked, and
    // said no.
    if matches!(outcome, crate::gate::GateOutcome::Failed { .. }) {
        let reverted = match (
            job_worktree(pool, job.id).await,
            revert_point(pool, job, ordinal).await,
        ) {
            (Ok(Some((worktree, _))), Some(footing)) => {
                match crate::worktree::revert_to(&worktree, &footing).await {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(job_id = job.id, ordinal, %error, "could not revert a red item");
                        false
                    }
                }
            }
            (_, None) => {
                tracing::warn!(
                    job_id = job.id,
                    ordinal,
                    "no footing to revert a red item to"
                );
                false
            }
            _ => {
                tracing::warn!(job_id = job.id, ordinal, "no worktree on record to revert");
                false
            }
        };

        if !reverted {
            // Mark the item first — a red gate is a fact whatever happened next — then stop. The
            // queue must not advance onto a tree still holding work the gate rejected, and this is
            // the one branch where continuing is worse than ending the job early.
            let _ = sqlx::query(
                "UPDATE job_items SET status = ?, gate_status = ? WHERE job_id = ? AND ordinal = ?",
            )
            .bind(item_status)
            .bind(gate_status)
            .bind(job.id)
            .bind(ordinal as i64)
            .execute(pool)
            .await;
            say(
                pool,
                job,
                "job_gate_failed",
                &format!(
                    "job {} at item {}: the gate failed and the worktree could not be put back, so the job stopped here",
                    job.id,
                    ordinal + 1
                ),
            )
            .await;
            return Step::Stopped;
        }
    }

    let written = sqlx::query(
        "UPDATE job_items SET status = ?, gate_status = ?, checkpoint_sha = ?
         WHERE job_id = ? AND ordinal = ?",
    )
    .bind(item_status)
    .bind(gate_status)
    .bind(checkpoint_sha.as_deref())
    .bind(job.id)
    .bind(ordinal as i64)
    .execute(pool)
    .await;
    if let Err(error) = written {
        tracing::warn!(job_id = job.id, ordinal, %error, "could not record a gate verdict");
        return Step::Stopped;
    }

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

/// Whether the brakes let this job start another node right now.
enum Brake {
    Go,
    /// Comes back on its own; park and try again.
    Park {
        reason: &'static str,
        detail: String,
    },
    /// Will not come back inside this job's life; stop and hand back what is on the branch.
    Stop {
        detail: String,
    },
}

/// Whether starting one more node would take this job past its own allowance, and how to say so.
///
/// Asks about the NEXT node, never the one in flight: the reserve is what a node is expected to cost,
/// so the test is "would starting another cross the line" rather than "has the line been crossed".
/// Killing a node mid-flight would throw away what it has already spent and leave the tree in a state
/// no gate has measured — the identical decision the global budget took on 2026-07-20, for the
/// identical reason.
async fn job_over_budget(
    pool: &SqlitePool,
    job: &JobRow,
    limit: f64,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<String>> {
    let spent = crate::budget::job_spend(pool, job.id, now).await?;
    let reserve = crate::budget::load_budget_config(pool)
        .await?
        .per_run_reserve_usd;
    if spent + reserve <= limit {
        return Ok(None);
    }
    Ok(Some(format!(
        "this job has spent ${spent:.2} of its ${limit:.2} allowance, and the next node reserves \
         ${reserve:.2}"
    )))
}

async fn brakes(state: &AppState, job: &JobRow, now: DateTime<Utc>) -> Brake {
    // Re-read per node rather than once per pass. A pass can spend a whole gate timeout inside one
    // job, and a stop that keeps starting work for another fifteen minutes is not a stop. Fails
    // closed: a switch that cannot be read holds the job rather than releasing it.
    match crate::autopilot::kill_switch_engaged(&state.pool).await {
        Ok(false) => {}
        _ => {
            return Brake::Park {
                reason: "kill-switch",
                detail: "the emergency stop is engaged".to_owned(),
            };
        }
    }

    // Decision 9. Which of the two limits fired decides whether waiting is patience or a hang.
    match crate::budget::budget_permits_new_run(&state.pool, now).await {
        crate::budget::BudgetDecision::Allow => {}
        crate::budget::BudgetDecision::Pause {
            reason,
            kind: crate::budget::PauseKind::Transient,
        } => {
            return Brake::Park {
                reason: "budget",
                detail: reason,
            };
        }
        crate::budget::BudgetDecision::Pause {
            reason,
            kind: crate::budget::PauseKind::Window,
        } => return Brake::Stop { detail: reason },
    }

    // The job's own ceiling, under the house's. Two different questions and both worth asking: a job
    // can be stopped by what it was given or by what is left in the till.
    //
    // `Stop` and never `Park`, which is the whole difference from the window brake above. A calendar
    // window reopens; a task's allowance does not, and a job parked on it would sit there until the
    // four-hour ceiling swept it up with `waiting` on its row and no reason a reader could act on.
    if let Some(limit) = job.budget_usd {
        match job_over_budget(&state.pool, job, limit, now).await {
            // Fails CLOSED, like every other brake here: a spend that cannot be read stops the job
            // rather than letting it keep spending against a number nobody could check.
            Err(error) => {
                return Brake::Stop {
                    detail: format!("this job's spend could not be read: {error}"),
                };
            }
            Ok(Some(detail)) => return Brake::Stop { detail },
            Ok(None) => {}
        }
    }

    // Somebody asked that this job and another not run at the same time, and the other one is
    // running. `Park` and never `Stop`: the reason lifts by itself the moment the partner ends, and
    // stopping would throw away a job for a wait measured in minutes.
    //
    // Placed after the budget and before the attention check, and the order is what the person
    // reads. Two of these can be true at once — the owner at the keyboard AND a partner holding a
    // slot — and "job 42 holds a slot and the two are excluded" is the more useful of the two,
    // because it names something that will resolve on its own and says what to watch for.
    //
    // Fails CLOSED like its neighbours, and the choice is nearly moot: a pool that cannot answer
    // this could not answer the kill switch at the top of the chain either, so the job would already
    // be parked before reaching here.
    match crate::exclusion::blocking_partner(&state.pool, job.id).await {
        Ok(None) => {}
        Ok(Some(partner)) => {
            return Brake::Park {
                reason: "excluded",
                detail: format!("job {partner} holds a slot and the two are excluded"),
            };
        }
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read this job's exclusions");
            return Brake::Park {
                reason: "excluded",
                detail: format!("this job's exclusions could not be read: {error}"),
            };
        }
    }

    // Decision 10, and the whole of what it adds: the brake was a check made once at admission, and
    // a job admitted at 03:00 could otherwise keep starting nodes at 08:00 with the owner at the
    // keyboard. The item in flight is never killed — its gate has already run or is about to — so
    // this only ever refuses to start the NEXT one.
    match crate::attention::attention_permits_new_run(
        &state.pool,
        &state.run_handles,
        &job.project_id,
        now,
    )
    .await
    {
        crate::attention::AttentionDecision::Allow => Brake::Go,
        crate::attention::AttentionDecision::Defer { reason, .. } => Brake::Park {
            reason: "attention",
            detail: reason,
        },
    }
}

/// Performs one move for a job.
async fn advance(state: &AppState, job: &JobRow, now: DateTime<Utc>) -> Step {
    let pool = &state.pool;
    let view = match load_view(pool, job.id).await {
        Ok(view) => view,
        Err(error) => {
            tracing::warn!(job_id = job.id, %error, "could not read a job");
            return Step::Stopped;
        }
    };

    let next = next_step(&view);
    if matches!(next, Next::Wait) {
        return Step::Stopped;
    }
    if let Next::Finish(outcome) = next {
        if let Err(error) = finish(pool, job.id, outcome).await {
            tracing::warn!(job_id = job.id, %error, "could not finish a job");
            return Step::Stopped;
        }
        say(
            pool,
            job,
            "job_finished",
            &format!(
                "job {} finished `{}` after {} item(s)",
                job.id,
                outcome.as_status(),
                view.items.len()
            ),
        )
        .await;
        return Step::Stopped;
    }
    // The gate is a subprocess, not a run: it starts no CLI, consumes no quota and answers to no
    // brake. Refusing to measure an item because the owner sat down would leave the tree holding
    // edits nothing has verified, which is the state decision 10 exists to avoid.
    if let Next::RunGate { ordinal } = next {
        return gate_item(state, job, ordinal, view.items.len()).await;
    }
    // A merge is a subprocess too, and it goes past the brakes for the same reason plus one of its
    // own: the item's work is already done and paid for, sitting on a branch. Refusing to bring it
    // in because the owner sat down would leave it there, diverging from the job's branch for as
    // long as the brake holds — and the divergence is what the retry has to absorb.
    if let Next::MergeItem { ordinal } = next {
        return merge_item(state, job, ordinal).await;
    }

    match brakes(state, job, now).await {
        Brake::Go => {}
        Brake::Park { reason, detail } => return park(state, job, reason, &detail).await,
        Brake::Stop { detail } => {
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
                        .filter(|item| item.claimable_as().is_some())
                        .count()
                ),
            )
            .await;
            return Step::Stopped;
        }
    }

    let Ok(Some(worktree)) = job_worktree(pool, job.id).await else {
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
        return Step::Stopped;
    };
    let artifacts = artifacts_for(&worktree.0);

    match next {
        Next::SpawnPlan => {
            let task = job.prompt.clone().unwrap_or_default();
            let prompt = plan_prompt(&task, job.max_items.max(0) as usize, &artifacts);
            spawn_node(state, job, "plan", prompt, None, worktree).await
        }
        Next::SpawnImplement { ordinal } => {
            // What the claim below has to find. Taken from the verdict this pass already reached —
            // `next_step` named this ordinal precisely because `item_state_from` called it `Pending`
            // or `GateRetriable` — rather than asked of the database a second time. The alternative
            // is a `WHERE` clause that re-derives which red gates are retriable, which is the same
            // rule in a second dialect and drifts the first time either side is edited.
            let Some(held) = view
                .items
                .get(ordinal)
                .and_then(|state| state.claimable_as())
            else {
                tracing::warn!(
                    job_id = job.id,
                    ordinal,
                    "asked to implement an item that is not startable"
                );
                return Step::Stopped;
            };
            let row = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                "SELECT description, files, gate_output FROM job_items
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
            let Some((description, files, gate_output)) = row else {
                tracing::warn!(job_id = job.id, ordinal, "a job item lost its description");
                return Step::Stopped;
            };
            // A hint that will not parse is no hint. It is an optimisation for where to start
            // reading, and refusing to run the item over it would fail the work for the sake of the
            // advice about the work.
            let files: Vec<String> = files
                .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
                .unwrap_or_default();
            let prompt = implement_prompt(
                &description,
                ordinal,
                view.items.len(),
                &artifacts,
                &files,
                gate_output.as_deref(),
            );
            spawn_node(
                state,
                job,
                "implement",
                prompt,
                Some(ItemClaim { ordinal, held }),
                worktree,
            )
            .await
        }
        Next::SpawnReview => {
            let prompt = review_prompt(job.head_sha.as_deref(), &artifacts);
            spawn_node(state, job, "review", prompt, None, worktree).await
        }
        Next::SpawnReplan => {
            // Archived BEFORE the node starts, because the node is about to overwrite `plan.json`
            // with its own answer. The round's plan has to be put aside while it still exists.
            let archives = archive_plans(&worktree.0, view.rounds.round).await;
            let task = job.prompt.clone().unwrap_or_default();
            let prompt = replan_prompt(&task, view.rounds.round, &archives, &artifacts);
            spawn_node(state, job, "replan", prompt, None, worktree).await
        }
        // All four are answered above, before the brakes and before the worktree is resolved.
        // Reaching one here means the dispatch above stopped covering something it used to.
        Next::Wait | Next::Finish(_) | Next::RunGate { .. } | Next::MergeItem { .. } => {
            Step::Stopped
        }
    }
}

/// Drives one job as far as it will go this pass.
async fn drive(state: &AppState, job: JobRow, now: DateTime<Utc>) {
    let pool = &state.pool;

    // Bookkeeping about work that already happened, before any decision and before any brake.
    if let Err(error) = reconcile_nodes(state, &job).await {
        tracing::warn!(job_id = job.id, %error, "could not reconcile a job's nodes");
        return;
    }

    // Decision 14, checked before anything is started and while parked, since parked time counts.
    if let Ok(started) = DateTime::parse_from_rfc3339(&job.created_at)
        && now.signed_duration_since(started.with_timezone(&Utc)) > MAX_JOB_LIFETIME
    {
        let _ = retire(pool, job.id, STATUS_EXPIRED).await;
        say(
            pool,
            &job,
            "job_expired",
            &format!(
                "job {} passed the {}-hour ceiling and stopped; what it finished is on its branch",
                job.id,
                MAX_JOB_LIFETIME.num_hours()
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
        if advance(state, &current, now).await == Step::Stopped {
            return;
        }
    }
}

/// One pass over every live job.
pub async fn job_tick(state: &AppState, now: DateTime<Utc>) {
    // The panic button stops the chain, not just the node — and it fails closed, so a switch that
    // cannot be read holds every job. Deliberately nothing more than that: engaging it does not
    // mark jobs terminal, because the read fails closed, and a database hiccup that retired every
    // job in flight would be a far worse failure than the one the switch is for.
    if crate::autopilot::kill_switch_engaged(&state.pool)
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
    match crate::concurrency::reconcile_orphaned_slots(&state.pool).await {
        Ok(freed) if freed > 0 => {
            tracing::warn!("freed {freed} concurrency slot(s) whose owner was no longer live");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "could not sweep orphaned concurrency slots"),
    }

    // Before the jobs are driven, because the measurement is about the state the trees are in NOW
    // and driving a job changes them. It carries a deadline of its own: a `git status` over a large
    // tree is what this pass costs, and nothing here may delay the work the tick exists to do.
    crate::collision::measure(&state.pool).await;

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
async fn reclaim_the_slot(state: &AppState, job: &JobRow) {
    let owner = crate::worktree::Owner::Job(job.id);
    match crate::concurrency::claim(&state.pool, &job.project_id, owner).await {
        Ok(crate::concurrency::ClaimOutcome::Claimed(_)) => {}
        Ok(crate::concurrency::ClaimOutcome::Full(full)) => tracing::warn!(
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
        job_tick(&state, Utc::now()).await;
    }
}

const JOB_TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// One job, as the shell lists it.
///
/// `Deserialize` is here for the route tests rather than for production: without it the shape of
/// `/jobs` can only be asserted as untyped JSON, and a field that silently stopped being sent would
/// go unnoticed.
#[derive(Debug, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct JobSummary {
    pub id: i64,
    pub project_id: String,
    pub rule_name: Option<String>,
    pub status: String,
    /// Why a `waiting` job is waiting. Budget and contention ask opposite things of a reader — one
    /// is "spend more or wait", the other is "something else is using the project" — so `waiting`
    /// alone would leave them guessing which.
    pub wait_reason: Option<String>,
    pub max_items: i64,
    /// The round the job is on, counted from zero, and how many it may run.
    ///
    /// Carried because without them a job with rounds is unreadable from outside: a queue of eight
    /// items where the first five passed and the last three are pending looks the same whether the
    /// plan node asked for eight at once — which `MAX_ITEMS_CEILING` forbids — or asked for five,
    /// finished them, and had a replan node ask for three more. This pair is what says which.
    pub round: i64,
    pub max_rounds: i64,
    pub created_at: String,
    pub completed_at: Option<String>,
    /// The slot this job holds, or `None` if it holds none.
    ///
    /// Left-joined from `project_slots` rather than kept as a column on `jobs`, because the slots
    /// table is the authority: a finished job still has its row in `jobs` and has already given the
    /// number back. Copying the slot onto `jobs` would give two truths that can disagree, and the
    /// one the daemon obeys would be the other.
    pub slot: Option<i64>,
}

/// One item of a job's queue, as the shell shows it.
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct JobItemView {
    pub ordinal: i64,
    pub description: String,
    pub status: String,
    /// The round this item was queued in. `ordinal` cannot stand in for it: ordinals continue
    /// across rounds rather than restart, so nothing in the number itself marks where one round
    /// ended and the next began.
    pub round: i64,
    /// The node that did it, so the shell can link to the transcript.
    pub run_id: Option<i64>,
    /// Carried separately from `status` because they answer different questions: `status` says
    /// where the item got to, `gate_status` says whether anything measured it. An item that reads
    /// `passed` with no gate status was never measured — the project has no gate command, or this
    /// was an intermediate item under `gate_after_each_item: false`.
    pub gate_status: Option<String>,
}

/// A job with its queue.
#[derive(Debug, serde::Serialize)]
pub struct JobDetail {
    #[serde(flatten)]
    pub job: JobSummary,
    pub items: Vec<JobItemView>,
    /// The branch the work is on, so a stopped job's partial can be found. `None` once the GC has
    /// taken the worktree.
    pub branch: Option<String>,
    /// What the owner has said to this job, delivered or still waiting.
    ///
    /// Every note and not only the pending ones, because both states answer a question the owner
    /// actually asks. Before delivery this is the only thing separating a note that is queued from
    /// one that was dropped — and an owner who cannot tell those apart leaves it a second time.
    /// After delivery, `delivered_to_run_id` is the join back to the prompt it was appended to,
    /// which is how "did it arrive in time" gets answered at all.
    pub notes: Vec<crate::notes::Note>,
}

/// The owner-kind predicate belongs in the `ON` clause and not in a `WHERE`. In a `WHERE` it would
/// turn the left join into an inner one and drop every job holding no slot — which is most of them.
const ONE_SUMMARY_SQL: &str =
    "SELECT jobs.id, jobs.project_id, jobs.rule_name, jobs.status, jobs.wait_reason,
            jobs.max_items, jobs.round, jobs.max_rounds, jobs.created_at, jobs.completed_at,
            project_slots.slot AS slot
     FROM jobs
     LEFT JOIN project_slots
       ON project_slots.owner_kind = 'job' AND project_slots.owner_id = jobs.id
     WHERE jobs.id = ?";

/// The most recent jobs, newest first.
///
/// Not filtered to live ones: a job that stopped for the budget or ran out of clock is exactly the
/// one the user needs to see, and it would vanish the moment it mattered.
pub async fn list(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<JobSummary>> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as(
                "SELECT jobs.id, jobs.project_id, jobs.rule_name, jobs.status, jobs.wait_reason,
                        jobs.max_items, jobs.round, jobs.max_rounds, jobs.created_at,
                        jobs.completed_at, project_slots.slot AS slot
                 FROM jobs
                 LEFT JOIN project_slots
                   ON project_slots.owner_kind = 'job' AND project_slots.owner_id = jobs.id
                 WHERE jobs.project_id = ? ORDER BY jobs.id DESC LIMIT ?",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT jobs.id, jobs.project_id, jobs.rule_name, jobs.status, jobs.wait_reason,
                        jobs.max_items, jobs.round, jobs.max_rounds, jobs.created_at,
                        jobs.completed_at, project_slots.slot AS slot
                 FROM jobs
                 LEFT JOIN project_slots
                   ON project_slots.owner_kind = 'job' AND project_slots.owner_id = jobs.id
                 ORDER BY jobs.id DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
}

/// Spelled out rather than assembled from `LIVE_STATUSES`, because sqlx refuses SQL built at
/// runtime — the same trade `LIVE_JOBS_SQL` makes, with the same guard:
/// `the_live_listing_names_every_live_status` compares this text against the constant.
const LIVE_LIST_SQL: &str =
    "SELECT jobs.id, jobs.project_id, jobs.rule_name, jobs.status, jobs.wait_reason,
            jobs.max_items, jobs.round, jobs.max_rounds, jobs.created_at, jobs.completed_at,
            project_slots.slot AS slot
     FROM jobs
     LEFT JOIN project_slots
       ON project_slots.owner_kind = 'job' AND project_slots.owner_id = jobs.id
     WHERE jobs.status IN ('planning','implementing','gating','reviewing',
                           'awaiting_approval','waiting')";

/// The live jobs, optionally filtered to one project.
///
/// A separate function rather than a parameter on `list`: `list` answers *what happened in this
/// project*, and its own comment says why it does not filter to live ones — a job that stopped for
/// the budget is exactly what the user needs to see. This answers a different question, *what is in
/// flight now*, and it is the only one the canvas asks.
///
/// `AssertSqlSafe` because sqlx 0.9 only trusts `&'static str`. The one interpolated thing is
/// `LIVE_LIST_SQL`, a constant of this module; the project filter is a bind parameter, so no caller
/// value ever reaches the string. Same reasoning, and same shape, as `runs::purge`.
pub async fn list_live(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<JobSummary>> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "{LIVE_LIST_SQL} AND jobs.project_id = ? ORDER BY jobs.id DESC LIMIT ?"
            )))
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "{LIVE_LIST_SQL} ORDER BY jobs.id DESC LIMIT ?"
            )))
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
}

pub async fn detail(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Option<JobDetail>> {
    let Some(job): Option<JobSummary> = sqlx::query_as(ONE_SUMMARY_SQL)
        .bind(job_id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(None);
    };

    let items: Vec<JobItemView> = sqlx::query_as(
        "SELECT ordinal, description, status, round, run_id, gate_status
         FROM job_items WHERE job_id = ? ORDER BY ordinal",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;

    let branch: Option<String> = sqlx::query_scalar(
        "SELECT branch FROM worktrees WHERE owner_kind = 'job' AND owner_id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;

    let notes = crate::notes::all(pool, job_id).await?;

    Ok(Some(JobDetail {
        job,
        items,
        branch,
        notes,
    }))
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
/// nobody is waiting for, in a worktree the job no longer owns. `reject_proposal` also takes the run
/// terminal through `worktree::release`, so the call after it is for the other case: a node parked
/// with no pending proposal left to reject.
async fn cancel_parked_node(pool: &SqlitePool, run_id: i64) -> sqlx::Result<()> {
    let pending: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM proposals
         WHERE run_id = ? AND kind = 'action-approval' AND status = 'pending'",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await?;
    for proposal_id in pending {
        if let Err(error) = crate::proposals::reject_proposal(pool, proposal_id).await {
            // Best effort, and the release below is why that is acceptable: a proposal that could
            // not be rejected leaves a stale door, where a run that stayed parked would leave the
            // whole project blocked. The worse of the two is the one this function must not skip.
            tracing::warn!(
                run_id,
                proposal_id,
                ?error,
                "could not reject the parked node's proposal"
            );
        }
    }
    crate::worktree::release(pool, run_id).await?;
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
    let Some(job): Option<JobSummary> = sqlx::query_as(ONE_SUMMARY_SQL)
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
            crate::runs::finalize_termination(state, run_id, STATUS_CANCELLED).await;
        }
    }

    // Only the item that was in someone's hands. The ones still `pending` were never started, and
    // marking them cancelled would claim the job got to them and turned back.
    sqlx::query("UPDATE job_items SET status = ? WHERE job_id = ? AND status = 'running'")
        .bind(STATUS_CANCELLED)
        .bind(job_id)
        .execute(pool)
        .await?;
    retire(pool, job_id, STATUS_CANCELLED).await?;

    let _ = crate::feed::append(
        pool,
        Some(&job.project_id),
        "job_cancelled",
        &format!("job {job_id} was cancelled; what it finished is on its branch"),
        None,
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
        )
        .await;
        retired += 1;
    }
    Ok(retired)
}

#[cfg(test)]
mod tests {
    // The walk tests hold `test_env_lock()` across their awaits on purpose: it serialises mutation
    // of the process-wide `NUCLEOS_WORKTREE_ROOT` override, which is the whole reason it exists. It
    // is a `std::sync::Mutex` because sync `#[test]`s share it, these are `current_thread` tests, and
    // there is no multi-thread runtime here to starve — the same false positive `worktree.rs` and
    // `runs.rs` already carry this allow for.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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

    async fn test_state(pool: sqlx::SqlitePool) -> AppState {
        test_state_with_runner(pool).await.0
    }

    async fn test_state_with_runner(
        pool: sqlx::SqlitePool,
    ) -> (AppState, std::sync::Arc<crate::runner::FakeCommandRunner>) {
        let runner = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
        let state = AppState {
            token: crate::auth::Token("test-token".into()),
            pool,
            runner: runner.clone(),
            triage_runner: None,
            local_triage_disabled: None,
            local_assistant: None,
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            files_root: None,
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            run_tails: Default::default(),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };
        (state, runner)
    }

    /// A job with a directory it can hand work through, and no git anywhere near it.
    ///
    /// The reconciler reads `plan.json` off the disk and looks the path up in `worktrees`; neither
    /// step cares whether git ever made the directory, so the tests that exercise the state machine
    /// do not have to build a repository to do it.
    async fn seed_worktree(pool: &sqlx::SqlitePool, job_id: i64, path: &std::path::Path) {
        crate::worktree::record(
            pool,
            crate::worktree::Owner::Job(job_id),
            "project-a",
            &path.to_string_lossy(),
            &path.to_string_lossy(),
            "nucleos/job",
            None,
        )
        .await
        .expect("record the job's worktree");
        std::fs::create_dir_all(path.join(crate::worktree::ARTIFACTS_DIR))
            .expect("create the handoff directory");
    }

    async fn write_plan(worktree: &std::path::Path, contents: &str) {
        std::fs::write(
            worktree
                .join(crate::worktree::ARTIFACTS_DIR)
                .join(PLAN_FILE),
            contents,
        )
        .expect("write a plan");
    }

    /// Adds a node run for a job, in whatever status the test needs it to have landed in.
    async fn seed_node(pool: &sqlx::SqlitePool, job_id: i64, stage: &str, status: &str) -> i64 {
        // The job's own project, not a literal: two `awaiting_approval` worktree runs sharing one
        // project id used to collide on `one_open_worktree_run_per_project`; that index is gone
        // (0053), and the shared id stays because these nodes do belong to one job's project.
        let project_id: String = sqlx::query_scalar("SELECT project_id FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .expect("the job exists");
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at, job_id, stage)
             VALUES (?, 'a node', ?, 'worktree', ?, ?, ?)",
        )
        .bind(project_id)
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(job_id)
        .bind(stage)
        .execute(pool)
        .await
        .expect("insert a node run")
        .last_insert_rowid()
    }

    async fn job_status(pool: &sqlx::SqlitePool, job_id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn item_statuses(pool: &sqlx::SqlitePool, job_id: i64) -> Vec<String> {
        sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
            .bind(job_id)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn feed_kinds(pool: &sqlx::SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT kind FROM feed ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// A job of one round, which is what every test written before rounds existed is about.
    fn view(planned: bool, items: &[ItemState], review: ReviewState) -> JobView {
        JobView {
            planned,
            planning: false,
            items: items.to_vec(),
            review,
            rounds: RoundState::default(),
            has_team: false,
        }
    }

    /// The same, for a job that was asked for more than one round.
    fn view_in_round(items: &[ItemState], review: ReviewState, rounds: RoundState) -> JobView {
        JobView {
            planned: true,
            planning: false,
            items: items.to_vec(),
            review,
            rounds,
            has_team: false,
        }
    }

    /// A job a team directs.
    ///
    /// Separate from `view` rather than a fourth parameter on it, and that is deliberate: every
    /// test written before this slice describes the job of today, and it should keep saying so
    /// without an edit. The pair of helpers is what makes "did this change the job of today?" a
    /// question the diff answers.
    fn view_with_team(items: &[ItemState], review: ReviewState) -> JobView {
        JobView {
            has_team: true,
            ..view(true, items, review)
        }
    }

    /// Seeds a job in a given status.
    ///
    /// It always starts `planning` and is moved afterwards, because that is the only status
    /// `insert_job` writes — a caller that could pick one could start a job mid-queue with no queue.
    async fn seed_job(
        pool: &sqlx::SqlitePool,
        project_id: &str,
        status: &str,
    ) -> sqlx::Result<i64> {
        seed_job_in(pool, project_id, status, "/project/a", None).await
    }

    async fn seed_job_in(
        pool: &sqlx::SqlitePool,
        project_id: &str,
        status: &str,
        project_root: &str,
        head_sha: Option<&str>,
    ) -> sqlx::Result<i64> {
        let job_id = insert_job(
            pool,
            &NewJob {
                project_id,
                project_root,
                rule_name: Some("nightly-backlog"),
                prompt: "pull from the todo list and advance what you can",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha,
                max_rounds: None,
                budget_usd: None,
            },
        )
        .await?;
        if status != "planning" {
            sqlx::query("UPDATE jobs SET status = ? WHERE id = ?")
                .bind(status)
                .bind(job_id)
                .execute(pool)
                .await?;
        }
        Ok(job_id)
    }

    #[test]
    fn a_job_walks_plan_then_each_item_then_review() {
        // One walk rather than five isolated assertions: the sequence is the behaviour, and a
        // transition that is right in isolation can still be reached in the wrong order.
        let mut items = vec![ItemState::Pending, ItemState::Pending];

        assert_eq!(
            next_step(&view(false, &[], ReviewState::Pending)),
            Next::SpawnPlan
        );
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement { ordinal: 0 }
        );

        items[0] = ItemState::Implemented;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::RunGate { ordinal: 0 }
        );

        items[0] = ItemState::Passed;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement { ordinal: 1 }
        );

        items[1] = ItemState::Passed;
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnReview
        );
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Done)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn an_empty_queue_completes_without_implementing_anything() {
        // A planner that found no work had a successful night. Only an absent plan.json is a
        // failure, and that distinction is made before this function ever sees the job.
        assert_eq!(
            next_step(&view(true, &[], ReviewState::Pending)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_failed_item_stops_the_chain_instead_of_moving_on() {
        let items = [ItemState::Failed, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::Finish(Outcome::Failed)
        );
    }

    /// The distinction gate.rs guards and §7 of the spec insists must survive the trip up to the
    /// job: a non-zero exit says the code is broken, a binary that would not start says the
    /// measurement never happened. One of those is a verdict; the other is silence.
    ///
    /// It now shows up as a difference in WHEN the job ends rather than only in what it is called.
    /// A red gate lets the queue carry on — the item's work has been reverted, and the items after
    /// it were never the ones that broke. Silence stops everything, because building on work
    /// nothing has measured is what the gate exists to prevent.
    #[test]
    fn a_failed_gate_and_an_errored_gate_end_the_job_differently() {
        assert_eq!(
            next_step(&view(true, &[ItemState::GateErrored], ReviewState::Pending)),
            Next::Finish(Outcome::GateErrored),
            "a gate that would not run has to stop the chain where it is"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateErrored, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::Finish(Outcome::GateErrored),
            "and it stops it even with work still queued behind it"
        );

        assert_eq!(
            next_step(&view(true, &[ItemState::GateFailed], ReviewState::Pending)),
            Next::SpawnReview,
            "a red gate is no longer where the job stops"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement { ordinal: 1 },
            "the item after a red one is still work worth doing"
        );
    }

    /// What a red gate DOES still decide: the ending.
    ///
    /// The point of letting the queue carry on was never to soften the verdict. A branch carrying an
    /// item the gate rejected is not one to tell somebody is complete, however many items passed
    /// after it — including the case where the red one is not the last.
    #[test]
    fn one_red_gate_makes_the_whole_job_gate_failed_however_it_ends() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Passed, ItemState::GateFailed, ItemState::Passed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::GateFailed)
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Passed],
                ReviewState::NotWanted
            )),
            Next::Finish(Outcome::GateFailed),
            "a job that wanted no review reaches the same verdict by the other door"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Passed, ItemState::Passed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::Completed),
            "and a queue nothing rejected still completes"
        );
    }

    /// Where an implemented item goes next, and it is the team that decides.
    ///
    /// Without a team the work is already on the job's branch — the items write into the one shared
    /// checkout — so there is nothing to bring anywhere and the gate follows immediately. With one,
    /// the work is on a branch of its own and the gate would measure a tree that does not have it.
    #[test]
    fn an_implemented_item_is_merged_first_only_when_a_team_wrote_it_elsewhere() {
        use ItemState::*;
        assert_eq!(
            next_step(&view(true, &[Implemented], ReviewState::NotWanted)),
            Next::RunGate { ordinal: 0 }
        );
        assert_eq!(
            next_step(&view_with_team(&[Implemented], ReviewState::NotWanted)),
            Next::MergeItem { ordinal: 0 }
        );
        // And once it has landed, the gate measures the branch it landed on.
        assert_eq!(
            next_step(&view_with_team(&[Merging], ReviewState::NotWanted)),
            Next::RunGate { ordinal: 0 }
        );
    }

    /// A conflicted item is work, not a verdict.
    ///
    /// It is found by the same positional search that finds a pending one, because it owes the same
    /// thing: a run, in the checkout it already has. What that run does differs — it resolves a
    /// staged merge instead of writing a feature — and that is a difference in the prompt. Leaving
    /// `Conflicted` out of the search would put the item down for good over two pieces of work
    /// disagreeing, which is not a judgement about either of them.
    #[test]
    fn a_conflicted_item_is_started_again_rather_than_given_up_on() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(&[Conflicted], ReviewState::NotWanted)),
            Next::SpawnImplement { ordinal: 0 }
        );
        // And it does not jump the queue: the same positional search means an earlier item still
        // goes first.
        assert_eq!(
            next_step(&view_with_team(
                &[Pending, Conflicted],
                ReviewState::NotWanted
            )),
            Next::SpawnImplement { ordinal: 0 }
        );
    }

    /// A merge that has landed is measured before another item is brought in.
    ///
    /// The order is the design and not an accident of which `if` came first: an item's divergence
    /// from the job's branch grows with every minute it waits, and the retry that has to absorb it
    /// is the most expensive step here. Draining what is in flight keeps that divergence small.
    #[test]
    fn a_landed_merge_is_gated_before_another_item_is_brought_in() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(
                &[Implemented, Merging],
                ReviewState::NotWanted
            )),
            Next::RunGate { ordinal: 1 },
            "the merge already on the branch is measured first"
        );
    }

    /// The precedence between unhappy endings, as a table — because it is a decision, and not a
    /// side effect of the order somebody happened to write the `if`s in.
    #[test]
    fn the_precedence_between_unhappy_endings() {
        use ItemState::*;
        for (items, expected) in [
            (vec![Passed, Passed], None),
            (vec![Passed, GateFailed], Some(Outcome::GateFailed)),
            (vec![Failed, Passed], Some(Outcome::Failed)),
            (vec![GateErrored, Passed], Some(Outcome::GateErrored)),
            // Silence beats a verdict, both ways round: a gate that would not start measured
            // nothing, so every other reading in this queue is in doubt.
            (vec![GateFailed, GateErrored], Some(Outcome::GateErrored)),
            (vec![Failed, GateErrored], Some(Outcome::GateErrored)),
            (vec![Failed, GateFailed], Some(Outcome::Failed)),
            // An orphan alone is impossible — it comes from a dependency that ended badly, and a
            // chain of orphans has a real failure at its root. The arm exists so the table is
            // total; if it ever decides, that invariant broke.
            (vec![Orphaned], Some(Outcome::Failed)),
            (vec![Orphaned, GateFailed], Some(Outcome::GateFailed)),
        ] {
            assert_eq!(
                failed_ending(&view_with_team(&items, ReviewState::NotWanted)),
                expected,
                "{items:?}"
            );
        }
    }

    /// **The regression that matters most in this slice: a job without a team is the job of
    /// today.**
    ///
    /// One row per item state, frozen. If one of these values changes, the change is a change of
    /// behaviour for every job that exists, and it has to be somebody's decision rather than a
    /// side effect of an edit to `failed_ending` or to the short-circuit above it.
    #[test]
    fn without_a_team_every_state_decides_exactly_as_it_did() {
        use ItemState::*;
        for (state, expected) in [
            (Pending, Next::SpawnImplement { ordinal: 0 }),
            (Running, Next::Wait),
            (Implemented, Next::RunGate { ordinal: 0 }),
            (Passed, Next::SpawnReview),
            (Failed, Next::Finish(Outcome::Failed)),
            (Cancelled, Next::Finish(Outcome::Cancelled)),
            (GateFailed, Next::SpawnReview),
            (GateRetriable, Next::SpawnImplement { ordinal: 0 }),
            (GateErrored, Next::Finish(Outcome::GateErrored)),
            (Skipped, Next::SpawnReview),
        ] {
            assert_eq!(
                next_step(&view(true, &[state], ReviewState::Pending)),
                expected,
                "{state:?} without a team"
            );
        }
    }

    /// With a team, a broken item is an item and not the end of the queue — and a cancelled one
    /// still is the end.
    ///
    /// The asymmetry is the point. `Failed` and `GateErrored` stopped the job because the shared
    /// tree held unmeasured edits the next item would have built on; a team's items each have a
    /// tree, so that reason is gone. `Cancelled` was never about trees.
    #[test]
    fn with_a_team_a_broken_item_does_not_stop_the_queue_but_a_cancelled_one_does() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(&[Failed, Pending], ReviewState::NotWanted)),
            Next::SpawnImplement { ordinal: 1 }
        );
        assert_eq!(
            next_step(&view_with_team(
                &[GateErrored, Pending],
                ReviewState::NotWanted
            )),
            Next::SpawnImplement { ordinal: 1 }
        );
        assert_eq!(
            next_step(&view_with_team(
                &[Cancelled, Pending],
                ReviewState::NotWanted
            )),
            Next::Finish(Outcome::Cancelled)
        );
    }

    /// And what carrying on must not cost: the job still ends badly.
    ///
    /// Without this, "a broken item does not stop the queue" quietly becomes "a broken item is not
    /// reported", which is the one way this slice could be worse than not doing it at all. Both
    /// doors are checked, because there are two — `ending` for a job of one round, and
    /// `close_the_round` for a job that had rounds left and must not open another over a failure.
    #[test]
    fn with_a_team_the_broken_item_still_decides_the_ending() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(&[Failed, Passed], ReviewState::NotWanted)),
            Next::Finish(Outcome::Failed),
            "through ending()"
        );

        let with_rounds = JobView {
            has_team: true,
            ..view_in_round(
                &[Failed, Passed],
                ReviewState::NotWanted,
                RoundState {
                    max_rounds: 5,
                    ..RoundState::default()
                },
            )
        };
        assert_eq!(
            next_step(&with_rounds),
            Next::Finish(Outcome::Failed),
            "and through close_the_round(), which must not open another round over it"
        );
    }

    /// A non-terminal state with no step of its own is waited for, never walked past.
    ///
    /// `Merging` and `Conflicted` have both left this list, each having been given a step — the
    /// gate that measures a landed merge, and the run that resolves a staged one. `Reverted` is the
    /// last one without either, and unreachable because nothing writes it: the red arm records
    /// `gate_failed` after the reset, exactly as a sequential job always did, and what says the
    /// branch was put back is `merge_base_sha` being set beside that verdict.
    ///
    /// The assertion is kept anyway, because the direction it guards is the expensive one. Walking
    /// past a non-terminal state is how an item is reported finished with its work still in flight.
    #[test]
    fn an_item_mid_merge_is_work_in_flight() {
        use ItemState::*;
        assert_eq!(
            next_step(&view_with_team(
                &[Reverted, Pending],
                ReviewState::NotWanted
            )),
            Next::Wait,
            "Reverted must not let the queue move on"
        );
    }

    /// A red gate that still has a retry left has not finished with its item.
    ///
    /// The three numbers are read together or not at all. `gate_attempts` counts how many times this
    /// item's gate has gone red; `gate_retries` is what the job allows; neither says anything alone,
    /// and the pair is the only thing that can tell "try again" from "give up". Without this, the
    /// first red gate is terminal for the item — which is today's behaviour, and the behaviour the
    /// retry exists to replace for the work that a missing import or an unupdated test made red.
    #[test]
    fn a_first_red_gate_with_a_retry_left_is_retriable() {
        assert_eq!(
            item_state_from("gate_failed", 1, 1),
            ItemState::GateRetriable
        );
    }

    /// The budget is spent, not renewed.
    ///
    /// The second red gate against a budget of one is where the retry stops being a retry. An item
    /// that kept re-implementing on every red gate would spend the whole night's runs on the one
    /// piece of work that cannot be made to pass, and the items behind it would never be reached.
    #[test]
    fn a_second_red_gate_with_one_retry_is_terminal() {
        assert_eq!(item_state_from("gate_failed", 2, 1), ItemState::GateFailed);
    }

    /// Zero retries has to stay reachable, because it is what every existing job IS.
    ///
    /// `jobs.gate_retries` defaults to 0 in migration 0068 precisely so that no job already
    /// scheduled quietly acquires a retry it never asked for. This is the assertion that says the
    /// unretried path is still the old path: one red gate, one dead item.
    #[test]
    fn zero_retries_is_todays_behaviour() {
        assert_eq!(item_state_from("gate_failed", 1, 0), ItemState::GateFailed);
    }

    /// `EVERY_ITEM_STATE` covers the enum.
    ///
    /// Two halves, and both are needed. The exhaustive `match` makes the compiler speak when a
    /// variant is born; the length makes the LIST speak when somebody adds the variant and leaves
    /// the array alone — without which every test that walks the array quietly tests less than it
    /// says it does.
    #[test]
    fn every_item_state_covers_the_enum() {
        for state in EVERY_ITEM_STATE {
            let _: () = match state {
                ItemState::Pending
                | ItemState::Running
                | ItemState::Implemented
                | ItemState::Passed
                | ItemState::Failed
                | ItemState::Cancelled
                | ItemState::GateFailed
                | ItemState::GateRetriable
                | ItemState::GateErrored
                | ItemState::Skipped
                | ItemState::Merging
                | ItemState::Conflicted
                | ItemState::Reverted
                | ItemState::Orphaned => (),
            };
        }
    }

    /// `LIVE_ITEM_STATUSES` is exactly the set of stored statuses that mean an item may still run.
    ///
    /// The slot sweep reads that list, so a state missing from it is a checkout and a slot freed out
    /// from under work still in flight, and a state wrongly in it is a slot held for ever. The
    /// classification below is an exhaustive `match`, which is what forces a new variant to be
    /// judged rather than defaulted.
    ///
    /// `gate_failed` is stored by two states — `GateRetriable`, which may still run, and
    /// `GateFailed`, which is over — so it belongs in the list on the strength of the first. That is
    /// the one place the list is deliberately generous, and the doc on the constant says why.
    #[test]
    fn every_unfinished_state_is_a_status_the_sweep_spares() {
        fn may_still_run(state: ItemState) -> bool {
            match state {
                ItemState::Pending
                | ItemState::Running
                | ItemState::Implemented
                | ItemState::GateRetriable
                | ItemState::Merging
                | ItemState::Conflicted
                | ItemState::Reverted => true,
                ItemState::Passed
                | ItemState::Failed
                | ItemState::Cancelled
                | ItemState::GateFailed
                | ItemState::GateErrored
                | ItemState::Skipped
                | ItemState::Orphaned => false,
            }
        }

        // Read `(0, 1)` — an unspent retry — so that `gate_failed` shows up as the state that may
        // still run, which is why it is on the list.
        let mut spared: Vec<&str> = LIVE_ITEM_STATUSES
            .into_iter()
            .filter(|status| may_still_run(item_state_from(status, 0, 1)))
            .collect();
        spared.sort_unstable();
        let mut listed: Vec<&str> = LIVE_ITEM_STATUSES.into_iter().collect();
        listed.sort_unstable();
        assert_eq!(
            spared, listed,
            "the sweep spares a status that means the item is over"
        );

        // And nothing that may still run is left off it. Both halves are needed: the check above
        // catches a status that should not be spared, this one catches a state nothing spares.
        for state in EVERY_ITEM_STATE {
            if !may_still_run(state) {
                continue;
            }
            let stored = match state {
                ItemState::Pending => "pending",
                ItemState::Running => "running",
                ItemState::Implemented => "implemented",
                ItemState::GateRetriable => "gate_failed",
                ItemState::Merging => "merging",
                ItemState::Conflicted => "conflicted",
                ItemState::Reverted => "reverted",
                other => unreachable!("{other:?} does not still run"),
            };
            assert!(
                LIVE_ITEM_STATUSES.contains(&stored),
                "{state:?} is stored as `{stored}`, which the sweep would collect"
            );
        }
    }

    /// The round trip between `claimable_as` and `item_state_from`, over every state that names a
    /// status at all.
    ///
    /// This is the pair `claimable_as` documents itself as being kept against, made into something
    /// that fails. `(0, 1)` — no attempt spent, one retry allowed — is the reading under which
    /// `gate_failed` means `GateRetriable`, which is the state that actually names that string.
    #[test]
    fn every_claimable_state_reads_back_as_itself() {
        for state in EVERY_ITEM_STATE {
            let Some(status) = state.claimable_as() else {
                continue;
            };
            assert_eq!(
                item_state_from(status, 0, 1),
                state,
                "{state:?} claims itself as `{status}`, which reads back as something else"
            );
        }
    }

    /// The other half, and the expensive one: a state whose stored status nobody taught
    /// `item_state_from` falls into the `_ =>` and is handed back to the queue as `Pending`.
    ///
    /// The item then restarts, in a tree that already holds its work, with nothing anywhere saying
    /// why. That is the failure this repository chose when it made the fallback "not finished", and
    /// it is only the right choice while the list of statuses is complete.
    #[test]
    fn no_parallel_state_is_read_as_pending() {
        for (status, expected) in [
            ("merging", ItemState::Merging),
            ("conflicted", ItemState::Conflicted),
            ("reverted", ItemState::Reverted),
            ("orphaned", ItemState::Orphaned),
        ] {
            assert_eq!(
                item_state_from(status, 0, 0),
                expected,
                "`{status}` fell through to the `_ =>` arm"
            );
        }
    }

    /// A retriable item is work to do, and the work is the SAME item.
    ///
    /// The number in `SpawnImplement` is what the caller looks the item up by, so an off-by-one here
    /// would re-implement the wrong item — or, at the end of a queue, implement one that does not
    /// exist. Item 1 went red, so item 1 is what runs again; the passed item before it is finished
    /// with and must not be touched.
    #[test]
    fn a_retriable_item_reruns_the_same_ordinal() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Passed, ItemState::GateRetriable],
                ReviewState::Pending
            )),
            Next::SpawnImplement { ordinal: 1 }
        );
    }

    /// Position decides which item runs next, not how the item got to be work.
    ///
    /// A retriable item is picked by the same positional search that picks a pending one, so an
    /// earlier retriable item outranks a later pending one. Running the pending item first would
    /// build it on a tree that still stands where the red gate left it, and the next gate could no
    /// longer say which of the two items broke it — which is the whole reason gating happens between
    /// items rather than at the end.
    #[test]
    fn a_retriable_item_is_taken_before_a_later_pending_one() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateRetriable, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement { ordinal: 0 }
        );
    }

    /// An item that has spent its retries is walked past, exactly as it is today.
    ///
    /// Said again beside the retry tests rather than left to
    /// `a_failed_gate_and_an_errored_gate_end_the_job_differently`, because this is the assertion a
    /// new state most easily breaks: `GateFailed` arriving at the positional search must still be
    /// nothing to the queue, or a job whose first item ran out of retries would sit re-implementing
    /// it and never reach the four items behind it that were never the ones that broke.
    #[test]
    fn an_item_out_of_retries_lets_the_queue_carry_on() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement { ordinal: 1 }
        );
    }

    /// The retry moves WHEN the verdict is decided, never what it is.
    ///
    /// A branch carrying an item the gate rejected is not one to tell somebody is complete, and
    /// spending a retry on it does not change that — it only means the rejection is final rather
    /// than first. A retry that softened the ending would be worse than no retry at all: the night
    /// would report `completed` over work no gate ever agreed with.
    #[test]
    fn a_job_ending_on_a_red_gate_still_reports_gate_failed() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateFailed, ItemState::Passed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::GateFailed)
        );
    }

    /// Silence is not retriable, whatever the budget says.
    ///
    /// The asymmetry the whole two-state gate design rests on. A non-zero exit is a verdict about
    /// the code and is worth another attempt; a gate that would not run measured nothing, so there
    /// is nothing to attempt again — a second implement node would be told "the gate said" and given
    /// the silence. Buying past it with a retry budget would mean building on work nothing has
    /// looked at, which is the one thing the gate exists to prevent.
    #[test]
    fn a_gate_that_could_not_run_still_ends_the_job() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::GateErrored, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::Finish(Outcome::GateErrored)
        );
        assert_eq!(
            item_state_from("gate_errored", 1, 1),
            ItemState::GateErrored,
            "a retry left over cannot turn silence into something to retry"
        );
        assert_eq!(
            item_state_from("gate_errored", 1, 3),
            ItemState::GateErrored,
            "and neither can the largest budget the ceiling allows"
        );
    }

    /// A skipped item is walked past exactly like a passed one, and colours nothing.
    ///
    /// Both halves matter. If it blocked, one unrecognised command would park the night — which is
    /// the state this chunk exists to leave. If it made the job `gate_failed`, then a job that did
    /// everything it was ALLOWED to do would be reported as broken, and the word would stop meaning
    /// anything to whoever reads it in the morning.
    #[test]
    fn a_skipped_item_neither_blocks_the_queue_nor_colours_the_ending() {
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Skipped, ItemState::Pending],
                ReviewState::Pending
            )),
            Next::SpawnImplement { ordinal: 1 },
            "the queue has to move past it"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Skipped, ItemState::Skipped],
                ReviewState::Done
            )),
            Next::Finish(Outcome::Completed),
            "a job whose every item was skipped still did what it was allowed to do"
        );
        assert_eq!(
            next_step(&view(
                true,
                &[ItemState::Skipped, ItemState::GateFailed],
                ReviewState::Done
            )),
            Next::Finish(Outcome::GateFailed),
            "and it does not hide a red gate beside it"
        );
    }

    #[test]
    fn work_resumes_at_the_first_unfinished_item_not_the_first_item() {
        // What `stage_cursor` exists for. Resuming at zero would redo work the gate already passed,
        // on a worktree that still holds it.
        let items = [ItemState::Passed, ItemState::Passed, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::SpawnImplement { ordinal: 2 }
        );
    }

    #[test]
    fn a_running_node_is_waited_for_rather_than_raced() {
        let items = [ItemState::Running, ItemState::Pending];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::Pending)),
            Next::Wait
        );
    }

    #[test]
    fn a_job_without_review_completes_at_the_last_item() {
        let items = [ItemState::Passed];
        assert_eq!(
            next_step(&view(true, &items, ReviewState::NotWanted)),
            Next::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn an_absent_plan_is_a_planning_failure_not_an_empty_queue() {
        // The pair that matters most in this module. "The planner produced nothing" is a failure;
        // "the planner found no work" is a success. Collapsing them would make every crashed plan
        // node look like a quiet, successful night, and teach the reader to ignore the feed.
        assert!(matches!(parse_plan(None, 5), Err(PlanError::Absent)));
    }

    /// The replan node's other answer, read out of the same file by the same parser.
    ///
    /// One shape for both nodes rather than two, because the failure mode of two is a plan node's
    /// file becoming unreadable to the replan parser the day somebody adds a key to one of them.
    #[test]
    fn a_replan_can_say_the_work_is_over() {
        let done = br#"{"done": true, "why": "the task is met and the suite is green"}"#;
        let plan = parse_plan(Some(done), 5).expect("a done verdict is a result, not an error");
        assert_eq!(
            plan.done.as_deref(),
            Some("the task is met and the suite is green")
        );
        assert!(plan.items.is_empty());

        // Silence about the reason is not a parse failure: the verdict is the load-bearing half, and
        // failing a job over a missing sentence would throw away the answer to keep the explanation.
        let terse = br#"{"done": true}"#;
        assert_eq!(
            parse_plan(Some(terse), 5).unwrap().done.as_deref(),
            Some("no reason given")
        );

        // `done` wins over items alongside it. Honouring the items would queue work the node just
        // said was unnecessary; calling the pair malformed would fail a job over redundancy.
        let both = br#"{"done": true, "items": [{"description": "one more thing"}]}"#;
        assert!(parse_plan(Some(both), 5).unwrap().done.is_some());
    }

    /// The distinction ending #1 and ending #3 are built on. An empty queue is a round that produced
    /// nothing and might not be the last; `done` is a claim the node is making. One ends the job now,
    /// the other feeds a counter that ends it after two.
    #[test]
    fn an_empty_queue_is_not_a_claim_that_the_work_is_over() {
        let plan = parse_plan(Some(br#"{"items": []}"#), 5).unwrap();
        assert!(plan.items.is_empty());
        assert_eq!(plan.done, None);

        // A plan node's file never carries the key at all, and must not read as a failure.
        let ordinary = br#"{"items": [{"description": "a"}]}"#;
        assert_eq!(parse_plan(Some(ordinary), 5).unwrap().done, None);
    }

    /// The replan node cannot do its job without the archives, and the prompt has to say so either
    /// way. Without them it reproposes round 1, no round ever comes back empty, and the "until it
    /// dries up" brake never fires — leaving `max_rounds` as the only thing between the job and its
    /// budget.
    #[test]
    fn the_replan_prompt_carries_what_the_earlier_rounds_tried() {
        let with = replan_prompt(
            "add shout and whisper",
            2,
            &["plan-0.json".to_owned(), "plan-1.json".to_owned()],
            "/wt/.nucleos",
        );
        assert!(with.contains("plan-0.json, plan-1.json"));
        assert!(with.contains("Do not repropose"));
        assert!(with.contains("/wt/.nucleos/plan.json"));
        assert!(with.contains("add shout and whisper"));
        // Ending #1 is one node; ending #4 costs a whole round to reach the same place, so the node
        // is told which one to prefer.
        assert!(with.contains("\"done\": true"));

        // No archives is a reachable state — the copy can fail — and the honest thing is to say so.
        // Naming a file that is not there sends the node looking, and what it finds is nothing.
        let without = replan_prompt("t", 1, &[], "/wt/.nucleos");
        assert!(without.contains("could not be recovered"));
        assert!(!without.contains("Do not repropose"));
    }

    /// Every node that could reach for git is told the history is already handled.
    ///
    /// Job 12 on 2026-08-08 is why. A task that said "I want to review each one on its own" — about
    /// how to split the work — became "commit X and Y as two separate commits" in all four items,
    /// every implement node reached for `git commit`, and under the zero-approval policy the night
    /// runs on, all four items were skipped with nothing built. The command was judged correctly;
    /// what was wrong is that the item asked for it.
    #[test]
    fn every_node_that_could_reach_for_git_is_told_the_job_commits() {
        let plan = plan_prompt("add eight modules", 5, "/wt/.nucleos");
        let replan = replan_prompt("add eight modules", 1, &[], "/wt/.nucleos");
        // No hint, because what this pins is the paragraph every node gets regardless of one.
        let implement = implement_prompt("write shout.py", 0, 4, "/wt/.nucleos", &[], None);

        for prompt in [&plan, &replan] {
            assert!(
                prompt.contains("The job commits the tree itself"),
                "a planning node was not told who owns the history"
            );
        }
        assert!(implement.contains("the job commits for you"));
        // Said as what happens, not only as a prohibition: a node told only "do not commit" invents
        // a way around it.
        assert!(plan.contains("already done for you"));
    }

    /// The nodes that only LOOK are told how to look, because the way they reached for by default
    /// was the one thing that could stop them.
    ///
    /// Across four jobs on 2026-08-08 every inspecting node reached for a shell loop or a pipeline —
    /// `for f in …; do cat "$f"; done` three times, `git reflog`, `ls -la && … && find … | sort`.
    /// The classifier reads a line and not a program, so none was recognised, and each one ended its
    /// node. `Read`, `Grep` and `Glob` need nobody and were there the whole time.
    #[test]
    fn the_nodes_that_only_look_are_told_what_to_look_with() {
        let replan = replan_prompt("t", 1, &[], "/wt/.nucleos");
        let review = review_prompt(Some("abc123"), "/wt/.nucleos");

        for prompt in [&replan, &review] {
            assert!(prompt.contains("Read, Grep and Glob"));
            // The consequence, not just the rule: this node is what the job spends to get an
            // answer, and a node told only "do not use shell" has no reason to care.
            assert!(prompt.contains("running unattended"));
        }
    }

    #[test]
    fn an_unparseable_plan_is_a_planning_failure() {
        let garbage = br#"{"items": [ truncated"#;
        assert!(matches!(
            parse_plan(Some(garbage), 5),
            Err(PlanError::Unreadable(_))
        ));
    }

    #[test]
    fn an_empty_item_list_is_a_legitimate_result() {
        let empty = br#"{"items": []}"#;
        let plan = parse_plan(Some(empty), 5).expect("an empty queue is a result, not an error");
        assert!(plan.items.is_empty());
        assert_eq!(plan.dropped, 0);
    }

    #[test]
    fn a_plan_over_max_items_is_truncated_and_says_how_much() {
        let seven = br#"{"items":[{"description":"a"},{"description":"b"},{"description":"c"},
                                  {"description":"d"},{"description":"e"},{"description":"f"},
                                  {"description":"g"}]}"#;
        let plan =
            parse_plan(Some(seven), 5).expect("an oversized plan is truncated, not rejected");

        assert_eq!(plan.items.len(), 5);
        // The queue is items, not strings: a description with no file hint is still a whole item,
        // and truncation cuts items rather than descriptions.
        assert_eq!(
            plan.items[0],
            PlannedItem {
                description: "a".to_owned(),
                files: Vec::new(),
            }
        );
        // Reported, never silent: a queue quietly cut from seven to five reads downstream as "the
        // planner found five things", which is a different and wrong statement about the work.
        assert_eq!(plan.dropped, 2);
    }

    /// The hint is the planner's, and it is optional at both ends: a planner that names files is
    /// believed, a planner that names none is not treated as having named an empty set of them.
    /// The absent case is not hypothetical — every plan.json written before this column existed
    /// looks exactly like it, and reading one has to stay a plan rather than a parse failure.
    #[test]
    fn parse_plan_reads_optional_file_hints_and_tolerates_their_absence() {
        let mixed = br#"{"items":[{"description":"x","files":["a.rs","b.rs"]},
                                  {"description":"y"}]}"#;
        let plan = parse_plan(Some(mixed), 5).expect("a plan without hints is still a plan");

        assert_eq!(
            plan.items,
            vec![
                PlannedItem {
                    description: "x".to_owned(),
                    files: vec!["a.rs".to_owned(), "b.rs".to_owned()],
                },
                PlannedItem {
                    description: "y".to_owned(),
                    files: Vec::new(),
                },
            ]
        );
        assert_eq!(plan.dropped, 0);
    }

    async fn seed_items(pool: &sqlx::SqlitePool, job_id: i64, statuses: &[&str]) {
        for (ordinal, status) in statuses.iter().enumerate() {
            sqlx::query(
                "INSERT INTO job_items (job_id, ordinal, description, status)
                 VALUES (?, ?, 'an item', ?)",
            )
            .bind(job_id)
            .bind(ordinal as i64)
            .bind(status)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// Undoing a skipped node happens in the tree that node wrote in, and nowhere else.
    ///
    /// Three cases, and the third is the one worth the test. A node with an item of its own is put
    /// back to where ITS tree began; a node without one keeps the answer it always had; and a node
    /// whose item tree has no recorded base gets `None` — **never** the job's tree as a fallback.
    /// That last arm is the whole reason these two questions were joined into one: reverting a
    /// checkout to a footing taken from a different checkout is worse than reverting nothing, and
    /// it is exactly what two separately-resolved answers produced.
    #[tokio::test]
    async fn a_skipped_node_is_undone_in_the_tree_it_wrote_in() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running", "running"]).await;
        let items: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();

        // The job's own tree and a checkpoint to fall back to, which is what a node without an item
        // of its own is answered with.
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Job(job_id),
            "project-a",
            "/repo",
            "/trees/job",
            "nucleos/job",
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET head_sha = 'head0' WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        // Item 0 has a tree with a base; item 1 has a tree that was never measured.
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Item(items[0]),
            "project-a",
            "/repo",
            "/trees/item-a",
            "nucleos/item-a",
            Some("base0"),
        )
        .await
        .unwrap();
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Item(items[1]),
            "project-a",
            "/repo",
            "/trees/item-b",
            "nucleos/item-b",
            None,
        )
        .await
        .unwrap();

        async fn run(pool: &sqlx::SqlitePool, job_id: i64, item: Option<i64>) -> i64 {
            sqlx::query(
                "INSERT INTO runs (project_id, prompt, status, mode, created_at, job_id, item_id)
                 VALUES ('project-a', 'x', 'running', 'worktree', '2026-01-01T00:00:00Z', ?, ?)",
            )
            .bind(job_id)
            .bind(item)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
        }

        let with_base = run(&pool, job_id, Some(items[0])).await;
        assert_eq!(
            revert_target(&pool, job_id, with_base).await,
            Some((PathBuf::from("/trees/item-a"), "base0".to_owned())),
            "an item is put back to where its own tree began"
        );

        // Today's link between a node and its item is `job_items.run_id`, and it is what
        // `footing_for_run` walks. The run below carries no `item_id` precisely because that is
        // every node that exists before this slice.
        let no_item = run(&pool, job_id, None).await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE id = ?")
            .bind(no_item)
            .bind(items[0])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            revert_target(&pool, job_id, no_item).await,
            Some((PathBuf::from("/trees/job"), "head0".to_owned())),
            "a node without an item keeps the answer it always had"
        );

        let unmeasured = run(&pool, job_id, Some(items[1])).await;
        assert_eq!(
            revert_target(&pool, job_id, unmeasured).await,
            None,
            "an unmeasured item tree answers nothing — reverting the job's would be worse"
        );
    }

    #[tokio::test]
    async fn a_job_still_planning_has_no_queue_yet() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(!view.planned);
        assert_eq!(next_step(&view), Next::SpawnPlan);
    }

    #[tokio::test]
    async fn a_planner_that_found_nothing_is_not_a_job_still_planning() {
        let pool = test_pool().await;
        // Past the plan node with an empty queue: the honest "there was no work" night. Reading
        // this as still-planning would spawn a second planner every tick, forever.
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(view.planned);
        assert_eq!(next_step(&view), Next::Finish(Outcome::Completed));
    }

    /// The column reaches `next_step`, or everything decided about teams is a decision no real job
    /// ever takes.
    ///
    /// The seeding is not ceremony: `storage.rs` runs with `foreign_keys` on, so a job cannot name
    /// a team out of thin air, and a team cannot name a director out of thin air either. The
    /// `UPDATE` is how a job acquires one for now — the director that writes it at creation is a
    /// later slice, and this slice deliberately leaves every production path writing NULL.
    #[tokio::test]
    async fn a_job_carries_whether_a_team_directs_it() {
        let pool = test_pool().await;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                 created_at, updated_at)
             VALUES ('dir', 'Dir', 'directing', 'lead', 'claude', 'inherit', ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES ('crew', 'Crew', 'ship it', 'dir', 3, 2, ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

        let alone = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let directed = seed_job(&pool, "project-b", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(directed)
            .execute(&pool)
            .await
            .unwrap();

        assert!(!load_view(&pool, alone).await.unwrap().has_team);
        assert!(load_view(&pool, directed).await.unwrap().has_team);
    }

    /// And the two halves joined: a directed job with a failed item keeps going, an undirected one
    /// with the same queue stops. Same rows, same loader, one column apart.
    #[tokio::test]
    async fn the_team_column_is_what_decides_whether_a_failure_ends_the_queue() {
        let pool = test_pool().await;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                 created_at, updated_at)
             VALUES ('dir', 'Dir', 'directing', 'lead', 'claude', 'inherit', ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES ('crew', 'Crew', 'ship it', 'dir', 3, 2, ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();

        let alone = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, alone, &["failed", "pending"]).await;
        let directed = seed_job(&pool, "project-b", "implementing").await.unwrap();
        seed_items(&pool, directed, &["failed", "pending"]).await;
        sqlx::query("UPDATE jobs SET team_id = 'crew' WHERE id = ?")
            .bind(directed)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            next_step(&load_view(&pool, alone).await.unwrap()),
            Next::Finish(Outcome::Failed)
        );
        assert_eq!(
            next_step(&load_view(&pool, directed).await.unwrap()),
            Next::SpawnImplement { ordinal: 1 }
        );
    }

    #[tokio::test]
    async fn a_loaded_view_resumes_at_the_first_unfinished_item() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "passed", "pending"]).await;

        let view = load_view(&pool, job_id).await.unwrap();

        assert_eq!(next_step(&view), Next::SpawnImplement { ordinal: 2 });
    }

    // ---- rounds (Chunk 3) ------------------------------------------------------------------------

    /// The whole point of landing the schema and this logic together and inert: a job nobody asked
    /// for rounds takes the pre-rounds path, verdict for verdict.
    ///
    /// `max_rounds = 1` is what `load_view` resolves a NULL column to, so this is every job that
    /// existed before migration 0051 and every `graph:` rule scheduled today.
    #[test]
    fn a_one_round_job_ends_exactly_as_it_did_before_rounds_existed() {
        for (items, expected) in [
            (vec![ItemState::Passed], Outcome::Completed),
            (
                vec![ItemState::Passed, ItemState::GateFailed],
                Outcome::GateFailed,
            ),
            (vec![ItemState::Skipped], Outcome::Completed),
        ] {
            assert_eq!(
                next_step(&view(true, &items, ReviewState::Done)),
                Next::Finish(expected),
                "{items:?}"
            );
        }
    }

    /// The four endings of §5.2, one case per row, and the order between them is what the table is
    /// for: two say `completed` and two say `stopped`, which is the difference a person reads in the
    /// morning between "I finished" and "I was cut short and it is on the branch".
    #[test]
    fn a_round_closes_by_the_first_condition_that_holds() {
        let rounds = |round, dry, replanned| RoundState {
            round,
            max_rounds: 5,
            dry_rounds: dry,
            replanned,
            ..RoundState::default()
        };

        // (1) The replan said so. Ahead of everything: the node that just looked at the work is the
        // one saying the work is over.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(1, 0, Replan::Done)
            )),
            Next::Finish(Outcome::Completed)
        );

        // (3) It dried up. `completed`, because there was nothing left to do.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(1, DRY_ROUNDS_TO_STOP, Replan::NotYet)
            )),
            Next::Finish(Outcome::Completed)
        );

        // (4) Out of rounds. `stopped`, because there may well have been more.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(4, 0, Replan::NotYet)
            )),
            Next::Finish(Outcome::Stopped)
        );

        // None of them: ask again.
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                rounds(1, 1, Replan::NotYet)
            )),
            Next::SpawnReplan
        );
    }

    /// A red gate ends the job whatever the rounds say.
    ///
    /// The per-item revert stops the NEXT ITEM building on work the gate rejected; that reason does
    /// not stop being true at a round boundary, and a replan looking at a branch carrying a red item
    /// would plan on top of it.
    #[test]
    fn a_red_gate_ends_a_job_that_had_rounds_left() {
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed, ItemState::GateFailed],
                ReviewState::Done,
                RoundState {
                    round: 0,
                    max_rounds: 5,
                    ..RoundState::default()
                }
            )),
            Next::Finish(Outcome::GateFailed)
        );
    }

    /// The sibling of `planning`, and it exists for the identical reason: between spawning the node
    /// and its items landing the queue is empty, so without it every tick would spawn another.
    #[test]
    fn a_replan_in_flight_is_waited_on_rather_than_spawned_again() {
        assert_eq!(
            next_step(&view_in_round(
                &[ItemState::Passed],
                ReviewState::Done,
                RoundState {
                    round: 1,
                    max_rounds: 5,
                    replanning: true,
                    ..RoundState::default()
                }
            )),
            Next::Wait
        );
    }

    /// The one thing an empty queue can mean.
    #[test]
    fn an_empty_queue_is_a_planner_that_looked_and_found_nothing() {
        // Round 0: the planner looked and found nothing. Done, as it always was.
        assert_eq!(
            next_step(&view_in_round(
                &[],
                ReviewState::Pending,
                RoundState {
                    max_rounds: 5,
                    ..RoundState::default()
                }
            )),
            Next::Finish(Outcome::Completed)
        );

        // Only reachable on the first round, and that is a property of the queue not being filtered
        // by round: once anything has been queued at all, this is never empty again. A round that
        // adds nothing shows up as `dry_rounds`, never as an empty queue.
    }

    /// Why the queue is read UNFILTERED, stated as the two things filtering would have broken.
    ///
    /// Filtering by round is the obvious first design, and it is wrong twice. `ordinal` is half this
    /// table's primary key, so restarting it per round collides outright — which is how this was
    /// found. And the position in the loaded queue would stop being the ordinal in the table, which
    /// is the number `advance` binds into its lookups. Unfiltered costs nothing: a round closes only
    /// when every item in it is terminal, and those are states `next_step` already walks past.
    #[tokio::test]
    async fn a_new_rounds_item_is_found_past_the_finished_ones_and_keeps_its_real_ordinal() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "skipped"]).await;
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, round)
             VALUES (?, 2, 'what round 1 asked for', 'pending', 1)",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE jobs SET round = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(
            view.items,
            vec![ItemState::Passed, ItemState::Skipped, ItemState::Pending],
            "the earlier round's items stay in the queue, terminal and walked past"
        );
        assert_eq!(view.rounds.round, 1);
        // 2, not 0. This is the number `advance` binds into
        // `SELECT description FROM job_items WHERE job_id = ? AND ordinal = ?`.
        assert_eq!(next_step(&view), Next::SpawnImplement { ordinal: 2 });
    }

    /// A NULL `max_rounds` is "nobody asked for rounds", not "use the daemon's ceiling". Resolving it
    /// the other way would switch rounds on for every `graph:` rule already scheduled, silently.
    #[tokio::test]
    async fn a_job_with_no_round_ceiling_is_a_job_of_one_round() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed"]).await;

        let view = load_view(&pool, job_id).await.unwrap();
        assert_eq!(view.rounds.max_rounds, 1);
        assert_eq!(view.rounds.round, 0);
        assert_eq!(view.rounds.replanned, Replan::NotYet);
    }

    #[tokio::test]
    async fn an_unrecognised_item_status_is_unfinished_rather_than_done() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "something-new"]).await;

        let view = load_view(&pool, job_id).await.unwrap();

        // Erring toward "not finished" costs a repeated item. Erring the other way skips work the
        // job exists to do and reports it complete, which is the failure nobody sees.
        assert_eq!(next_step(&view), Next::SpawnImplement { ordinal: 1 });
    }

    #[tokio::test]
    async fn losing_the_slot_parks_the_job_instead_of_failing_it() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        wait(&pool, job_id, "slot").await.unwrap();

        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "waiting");
        // The reason is stored because `waiting` now covers a budget window that will reopen and a
        // slot another run holds, and those ask opposite things of whoever reads the feed.
        assert_eq!(reason.as_deref(), Some("slot"));
    }

    /// The note has to leave with the pause it explains.
    ///
    /// `retire` has cleared it since it was written; `resume` did not, so an unparked job carried
    /// the reason for its last pause through everything that came after. What a reader saw was
    /// `implementing / budget` — a job working normally, labelled with a brake that lifted hours
    /// ago, which is exactly the sort of thing somebody acts on.
    #[tokio::test]
    async fn resuming_a_job_takes_the_pause_note_with_it() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        wait(&pool, job_id, "budget").await.unwrap();
        resume(&pool, job_id).await.unwrap();

        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "implementing", "the stage it was parked at");
        assert_eq!(reason, None, "nothing is waiting, so nothing is the reason");
    }

    #[tokio::test]
    async fn a_finished_job_records_which_kind_of_ending_it_had() {
        let pool = test_pool().await;
        let broken = seed_job(&pool, "project-a", "gating").await.unwrap();
        finish(&pool, broken, Outcome::GateFailed).await.unwrap();
        let unmeasured = seed_job(&pool, "project-b", "gating").await.unwrap();
        finish(&pool, unmeasured, Outcome::GateErrored)
            .await
            .unwrap();

        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM jobs ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        // Distinct in the database, not just in the enum: a reader querying jobs must still be able
        // to tell broken code from a measurement that never happened.
        assert_eq!(statuses, vec!["gate_failed", "gate_errored"]);
    }

    /// A second live job for one project is now a row the database accepts.
    ///
    /// It used to be refused here, by `one_live_job_per_project`, and that index came out in 0053.
    /// The lock did not move — it is still an `INSERT`, now into `project_slots` — but it moved
    /// OFF this function, so `insert_job` no longer refuses anything and `start` is where a full
    /// project is turned away. Asserted rather than deleted, because a reader who remembers the old
    /// behaviour needs to find out here that it changed on purpose.
    #[tokio::test]
    async fn a_second_live_job_for_a_project_is_no_longer_refused_by_the_row() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "planning")
            .await
            .expect("the first job starts");

        let second = seed_job(&pool, "project-a", "planning").await;

        assert!(
            second.is_ok(),
            "the row no longer holds exclusivity; the slot ceiling does"
        );
    }

    #[tokio::test]
    async fn a_finished_job_does_not_hold_the_project_slot() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "completed")
            .await
            .expect("a job that has finished");

        // The partial index covers live statuses only. Without that, a project would be wedged
        // forever by its own first job — a worse failure than the race the index exists to stop,
        // because nothing would ever clear it.
        seed_job(&pool, "project-a", "planning")
            .await
            .expect("a new job may start once the last one is done");
    }

    // ---- what a pass sees ----------------------------------------------------------------------

    /// The constant the tick iterates and the SQL a pass loads with have to name the same statuses,
    /// in both directions.
    ///
    /// It used to be checked against `one_live_job_per_project`, which is gone (migration 0053).
    /// The hazard survived the index, and only changed hands: a job in a status the tick does not
    /// drive still holds a concurrency slot, and `reconcile_orphaned_slots` still spares it, because
    /// the sweep's live-list is this same constant. So the project goes quiet with no error
    /// anywhere — with one slot fewer instead of none at all. `concurrency.rs` guards the sweep's
    /// half of that agreement; this guards the loader's.
    #[test]
    fn a_live_status_is_a_status_some_pass_would_load() {
        for status in LIVE_STATUSES {
            assert!(
                LIVE_JOBS_SQL.contains(&format!("'{status}'")),
                "`{status}` is live but no pass would ever load it"
            );
        }
        // And the other direction, which is the dangerous one: SQL that loads a status the tick has
        // no arm for would drive a job nothing knows how to move.
        let loaded = LIVE_JOBS_SQL
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            loaded,
            LIVE_STATUSES.len(),
            "a pass loads a status the tick does not drive: {LIVE_JOBS_SQL}"
        );
    }

    /// The same guard `LIVE_JOBS_SQL` already carries, for the same reason: sqlx refuses SQL
    /// assembled at runtime, so the list is spelled out by hand in several places and nothing but a
    /// test holds them to each other. A status missing from here would not break anything loudly —
    /// the canvas would simply stop drawing that kind of live job.
    #[test]
    fn the_live_listing_names_every_live_status() {
        for status in LIVE_STATUSES {
            assert!(
                LIVE_LIST_SQL.contains(&format!("'{status}'")),
                "the live listing does not know `{status}`"
            );
        }
        let named = LIVE_LIST_SQL
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            named,
            LIVE_STATUSES.len() + 1,
            "the listing names a status nothing drives (the +1 is the join's 'job')"
        );
    }

    /// A job that ends in a status the GC does not collect keeps its worktree forever, and nothing
    /// reports it: as far as the system is concerned the job is finished and the tree is nobody's.
    /// Written after adding `stopped` opened exactly that leak.
    #[test]
    fn every_ending_a_job_can_have_is_an_ending_the_gc_collects() {
        for status in TERMINAL_STATUSES {
            assert!(
                crate::worktree::GC_CANDIDATES_SQL.contains(&format!("'{status}'")),
                "a job can end `{status}` and its worktree would never be collected"
            );
            assert!(
                !LIVE_STATUSES.contains(&status),
                "`{status}` both holds the project's slot and is collectable"
            );
        }
    }

    /// The job row has to say it is blocked on a person. Until it did, it read `implementing` while
    /// the whole chain waited, and the only sign anywhere was a proposal in a queue that never
    /// names the job it came from.
    #[tokio::test]
    async fn a_job_whose_node_stopped_to_ask_says_so_on_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "awaiting_approval").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ?")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        job_tick(&state, Utc::now()).await;
        assert_eq!(job_status(&pool, job_id).await, STATUS_AWAITING_APPROVAL);

        // And it goes back to what it was doing once the node moves on, rather than staying stuck
        // in a status nothing clears.
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        job_tick(&state, Utc::now()).await;
        assert_ne!(job_status(&pool, job_id).await, STATUS_AWAITING_APPROVAL);
        // Past `running`, wherever the same pass carried it — this project configures no gate, so
        // the item is waved through to `passed` in the step after the one being tested here.
        assert_ne!(item_statuses(&pool, job_id).await[0], "running");
    }

    /// The hazard of putting anything in front of a job's stage: `planning` is the one status that
    /// carries meaning the job cannot rebuild. A plan node that stops to ask permission must not
    /// come back as planned-with-an-empty-queue and report itself complete having planned nothing.
    #[tokio::test]
    async fn a_plan_node_stopping_to_ask_does_not_cost_the_job_its_queue() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        let run_id = seed_node(&pool, job_id, "plan", "awaiting_approval").await;

        job_tick(&state, Utc::now()).await;
        assert_eq!(job_status(&pool, job_id).await, STATUS_AWAITING_APPROVAL);
        assert!(!load_view(&pool, job_id).await.unwrap().planned);

        sqlx::query("UPDATE runs SET status = 'cancelled' WHERE id = ?")
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        resume(&pool, job_id).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "planning");
    }

    /// Both levels of cancellation end the job, because a stopped node leaves the tree holding
    /// edits no gate measured and the next item must not build on them. What must not happen is
    /// either level reporting the user's own decision back to them as a failure.
    #[tokio::test]
    async fn cancelling_a_node_ends_its_job_cancelled_rather_than_failed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running", "pending"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "cancelled").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = 0")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["cancelled", "pending"]
        );
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Cancelled)
        );
    }

    /// The whole reason cancelling a job is a separate thing from cancelling a run: a job parked
    /// for the budget, or waiting for the slot, has no node to cancel. Without this it could only
    /// be stopped by waiting out the four-hour ceiling.
    #[tokio::test]
    async fn a_parked_job_can_be_cancelled_even_though_it_has_no_node_running() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "pending"]).await;
        wait(&pool, job_id, "budget").await.unwrap();

        assert_eq!(
            cancel(&state, job_id).await.unwrap(),
            CancelOutcome::Cancelled
        );
        assert_eq!(job_status(&pool, job_id).await, STATUS_CANCELLED);
        // The item that was never started is left alone. Marking it cancelled would claim the job
        // got to it and turned back, and the gated-green one keeps its verdict.
        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["passed", "pending"]
        );
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_cancelled".to_owned())
        );
    }

    /// Cancelling a job also ends the node that was parked asking for permission — and closes the
    /// door that could still have said yes to it.
    ///
    /// `finalize_termination` cannot do this: it needs a live handle, and it guards its write with
    /// `status = 'running'`. A parked node has neither. So the run stayed `awaiting_approval` after
    /// the job that owned it was gone, holding the project's exclusivity — an index then, a
    /// concurrency slot since 0053, and a leak either way.
    ///
    /// Found in production on 2026-08-08: job 12 was cancelled with its review node parked, and the
    /// next job came up `waiting` with `wait_reason = slot` against a project whose only live job
    /// was itself — with nothing in the feed to say why, because from the project's side nothing
    /// had gone wrong.
    #[tokio::test]
    async fn cancelling_a_job_ends_the_node_parked_on_an_approval_and_shuts_its_door() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "reviewing").await.unwrap();
        seed_items(&pool, job_id, &["passed"]).await;
        let run_id = seed_node(&pool, job_id, "review", "awaiting_approval").await;
        let proposal_id = crate::proposals::create_action_approval(
            &pool,
            run_id,
            None,
            Some("project-a"),
            "Bash",
            "the review node wanted to look around",
            Some(r#"{"command":"git branch -a"}"#),
        )
        .await
        .expect("a parked node has a proposal");

        assert_eq!(
            cancel(&state, job_id).await.unwrap(),
            CancelOutcome::Cancelled
        );

        let run_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(
            run_status, "awaiting_approval",
            "the parked node outlived the job that owned it, and holds the project's slot"
        );

        let proposal_status: String =
            sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
                .bind(proposal_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(
            proposal_status, "pending",
            "approving this would resume a node whose job is over"
        );
    }

    /// A live job that holds no slot is put back on one by the tick.
    ///
    /// The window is between `start`'s INSERT and its claim, which cannot be closed by ordering —
    /// a slot is keyed on its owner's id, so the row must exist first. The sweep is no help either:
    /// it only takes slots away from owners that died, and there is nothing here to take. Left
    /// alone, the project runs one over its ceiling for as long as the job lasts.
    #[tokio::test]
    async fn the_tick_puts_a_live_job_back_on_a_slot_it_lost() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let owner = crate::worktree::Owner::Job(job_id);
        assert_eq!(
            crate::concurrency::slot_of(&pool, owner).await.unwrap(),
            None
        );

        let job = load_job(&pool, job_id).await.unwrap();
        reclaim_the_slot(&state, &job).await;

        assert_eq!(
            crate::concurrency::slot_of(&pool, owner).await.unwrap(),
            Some(0)
        );

        // And again, because `claim` is idempotent per owner: a second pass must not take a second
        // number, or every tick would spend one until the ceiling refused the project's next start.
        reclaim_the_slot(&state, &job).await;
        assert_eq!(crate::concurrency::slots_in_flight(&pool).await.unwrap(), 1);
    }

    /// Every ending gives the slot back, and `retire` is the one place that does it.
    ///
    /// Asserted here rather than at each ending because that is the design: `finish`, `cancel` and
    /// the startup reconciliation all funnel through this function, so an ending added later cannot
    /// forget. A slot held by a finished job is silent — it lowers the project's ceiling with no
    /// error anywhere — which is exactly the failure the sweep on the tick exists to bound.
    #[tokio::test]
    async fn retiring_a_job_gives_its_concurrency_slot_back() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let owner = crate::worktree::Owner::Job(job_id);
        crate::concurrency::claim(&pool, "project-a", owner)
            .await
            .unwrap();

        retire(&pool, job_id, STATUS_CANCELLED).await.unwrap();

        assert_eq!(
            crate::concurrency::slot_of(&pool, owner).await.unwrap(),
            None
        );
    }

    /// "I stopped it" and "it had already finished" are different answers to the question the user
    /// just asked, so the second one is not reported as success.
    #[tokio::test]
    async fn cancelling_a_job_that_is_already_over_says_so() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let done = seed_job(&pool, "project-a", "completed").await.unwrap();

        assert_eq!(cancel(&state, done).await.unwrap(), CancelOutcome::NotLive);
        assert_eq!(job_status(&pool, done).await, "completed");
        assert_eq!(
            cancel(&state, 9_999).await.unwrap(),
            CancelOutcome::NotFound
        );
    }

    /// A job's queue is what the detail view is for, and two of its columns answer different
    /// questions: `status` says where an item got to, `gate_status` says whether anything measured
    /// it. An item reading `passed` with no gate status was never measured.
    #[tokio::test]
    async fn a_jobs_detail_carries_its_queue_and_says_which_items_were_measured() {
        let pool = test_pool().await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["passed", "passed"]).await;
        sqlx::query("UPDATE job_items SET gate_status = 'passed' WHERE job_id = ? AND ordinal = 1")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let detail = detail(&pool, job_id)
            .await
            .unwrap()
            .expect("the job exists");

        assert_eq!(detail.job.status, "implementing");
        assert_eq!(detail.items.len(), 2);
        assert_eq!(detail.items[0].gate_status, None);
        assert_eq!(detail.items[1].gate_status.as_deref(), Some("passed"));
        assert_eq!(detail.branch.as_deref(), Some("nucleos/job"));
        assert!(detail.items[0].ordinal < detail.items[1].ordinal);
    }

    /// Which round an item came from has to travel, because nothing else in the view carries it.
    ///
    /// Ordinals continue across rounds rather than restart, so a queue of three passed items and two
    /// pending ones reads identically whether the plan node asked for five at once or asked for
    /// three and a replan added two. The round is the only thing that separates those, and until it
    /// is on the wire the answer is only in the database.
    #[tokio::test]
    async fn a_jobs_detail_says_which_round_each_item_came_from() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "passed", "pending"]).await;
        sqlx::query("UPDATE job_items SET round = 1 WHERE job_id = ? AND ordinal = 2")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE jobs SET round = 1, max_rounds = 4 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let detail = detail(&pool, job_id)
            .await
            .unwrap()
            .expect("the job exists");

        assert_eq!((detail.job.round, detail.job.max_rounds), (1, 4));
        let rounds: Vec<i64> = detail.items.iter().map(|item| item.round).collect();
        assert_eq!(rounds, vec![0, 0, 1]);
        // The point of the field, stated as what it is not: the ordinals do not say this.
        let ordinals: Vec<i64> = detail.items.iter().map(|item| item.ordinal).collect();
        assert_eq!(ordinals, vec![0, 1, 2]);
    }

    /// A job of today reads as one round of one, not as round zero of nothing.
    #[tokio::test]
    async fn a_job_without_rounds_lists_as_a_single_round() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "implementing").await.unwrap();

        let listed = list(&pool, Some("project-a"), 20).await.unwrap();

        assert_eq!((listed[0].round, listed[0].max_rounds), (0, 1));
    }

    /// Finished jobs stay in the listing. A job that stopped for the budget or ran out of clock is
    /// exactly the one worth looking at, and filtering to live ones would make it vanish at the
    /// moment it started mattering.
    #[tokio::test]
    async fn the_listing_keeps_the_jobs_that_already_ended() {
        let pool = test_pool().await;
        let old = seed_job(&pool, "project-a", "completed").await.unwrap();
        let live = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_job(&pool, "project-b", "implementing").await.unwrap();

        let mine = list(&pool, Some("project-a"), 20).await.unwrap();
        assert_eq!(
            mine.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![live, old],
            "newest first, and the finished one is still there"
        );
        assert_eq!(list(&pool, None, 20).await.unwrap().len(), 3);
    }

    /// The canvas draws a slot, and this is what ties one to the job holding it. `None` for a
    /// finished job is not a serialisation detail: a job that ended has already given the number
    /// back, and saying it still holds one would make the column count work that does not exist.
    #[tokio::test]
    async fn a_jobs_summary_carries_the_slot_it_holds_and_drops_it_when_it_ends() {
        let pool = test_pool().await;
        let live = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let done = seed_job(&pool, "project-a", "completed").await.unwrap();
        crate::concurrency::claim(&pool, "project-a", crate::worktree::Owner::Job(live))
            .await
            .unwrap();

        let listed = list(&pool, Some("project-a"), 20).await.unwrap();

        let live_row = listed.iter().find(|row| row.id == live).unwrap();
        let done_row = listed.iter().find(|row| row.id == done).unwrap();
        assert_eq!(live_row.slot, Some(0));
        assert_eq!(done_row.slot, None);
    }

    /// Job ids and run ids come from different sequences and collide constantly. A join on the id
    /// alone would hand this job the slot belonging to the run that shares its number.
    #[tokio::test]
    async fn a_job_does_not_borrow_the_slot_of_the_run_with_its_number() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        crate::concurrency::claim(&pool, "project-a", crate::worktree::Owner::Run(job_id))
            .await
            .unwrap();

        let listed = list(&pool, Some("project-a"), 20).await.unwrap();

        assert_eq!(
            listed[0].slot, None,
            "that slot is the run's, not this job's"
        );
    }

    /// The listing's ceiling of 20 is a window onto history. A live job outside it is precisely
    /// what the canvas needs in order to describe a slot, and precisely the case the window hides.
    #[tokio::test]
    async fn the_live_listing_reaches_a_live_job_the_recent_window_would_hide() {
        let pool = test_pool().await;
        let old_live = seed_job(&pool, "project-a", "implementing").await.unwrap();
        for _ in 0..25 {
            seed_job(&pool, "project-b", "completed").await.unwrap();
        }

        let recent = list(&pool, None, 20).await.unwrap();
        assert!(
            !recent.iter().any(|row| row.id == old_live),
            "the setup did not push the live job out of the window"
        );

        let live = list_live(&pool, None, crate::concurrency::LIVE_LIST_LIMIT)
            .await
            .unwrap();
        assert!(live.iter().any(|row| row.id == old_live));
    }

    /// Only the live ones, and "live" is the same list the slot sweep spares.
    #[tokio::test]
    async fn the_live_listing_carries_nothing_terminal() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "completed").await.unwrap();
        seed_job(&pool, "project-a", "cancelled").await.unwrap();
        let alive = seed_job(&pool, "project-a", "waiting").await.unwrap();

        let live = list_live(&pool, None, crate::concurrency::LIVE_LIST_LIMIT)
            .await
            .unwrap();

        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, alive);
    }

    // ---- resolving a request into a start ------------------------------------------------------

    fn summary(
        project_id: &str,
        mode: crate::autopilot::Mode,
        root: Option<&str>,
    ) -> crate::autopilot::ProjectSummary {
        crate::autopilot::ProjectSummary {
            project_id: project_id.to_string(),
            mode,
            project_root: root.map(str::to_string),
            pending: 0,
            classes_ready: 0,
            classes_total: 0,
            withheld_classes_ready: 0,
            promotable: false,
            open_proposals: 0,
            wip_limit: None,
            queue_full: false,
        }
    }

    /// The whole refusal table, with no database in sight.
    ///
    /// The `Shadow` row is the one that matters. `resolve_run_request` maps shadow onto a real run
    /// mode, and copying that table across is the natural mistake — but a job's plan node has to
    /// WRITE `plan.json`, and a plan-only node writes nothing. A job demoted to shadow would do
    /// nothing at all while reporting that it was working, which is the failure a person only
    /// discovers in the morning.
    #[test]
    fn um_pedido_de_job_resolve_ou_recusa_com_motivo_proprio() {
        use crate::autopilot::Mode;
        let roster = vec![
            summary("off", Mode::Off, Some("/repos/off")),
            summary("shadow", Mode::Shadow, Some("/repos/shadow")),
            summary("rootless", Mode::Active, None),
            summary("live", Mode::Active, Some("/repos/live")),
        ];

        assert_eq!(
            resolve_start(&roster, "nao-existe"),
            Err(StartRefusal::UnknownProject)
        );
        assert_eq!(
            resolve_start(&roster, "off"),
            Err(StartRefusal::NotActive(Mode::Off))
        );
        assert_eq!(
            resolve_start(&roster, "shadow"),
            Err(StartRefusal::NotActive(Mode::Shadow)),
            "shadow is plan-only, so it is a refusal and never a mode a job falls back to"
        );
        assert_eq!(
            resolve_start(&roster, "rootless"),
            Err(StartRefusal::NoRoot)
        );
        assert_eq!(
            resolve_start(&roster, "live"),
            Ok(ResolvedStart {
                project_root: "/repos/live".to_string()
            })
        );

        // The two refusals that share a status code still have to be told apart by the person
        // reading them, or "422" is all the answer they get.
        assert!(
            StartRefusal::NotActive(Mode::Shadow)
                .reason("live")
                .contains("shadow")
        );
        assert!(
            StartRefusal::NotActive(Mode::Off)
                .reason("live")
                .contains("off")
        );
        assert_ne!(
            StartRefusal::NotActive(Mode::Shadow).reason("live"),
            StartRefusal::NotActive(Mode::Off).reason("live")
        );
    }

    // ---- the whole walk ------------------------------------------------------------------------

    /// A repository with a real gate, so a walk measures something rather than waving items
    /// through. The commands used are `git`, which runs everywhere this daemon does and answers in
    /// milliseconds.
    fn walkable_repo(prefix: &str, gate: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        let container = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(base)
            .expect("create space-free tempdir");
        let repo = container.path().join("repo");
        std::fs::create_dir_all(&repo).expect("create repository directory");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@x"],
            vec!["config", "user.name", "test"],
        ] {
            assert!(git_ok(&repo, &args));
        }
        std::fs::write(repo.join("seed.txt"), "seed\n").expect("seed the repository");
        for args in [vec!["add", "-A"], vec!["commit", "-m", "seed"]] {
            assert!(git_ok(&repo, &args));
        }
        std::fs::create_dir_all(repo.join(".ai")).expect("create .ai");
        std::fs::write(
            repo.join(".ai").join("autopilot.yaml"),
            format!("gate_command: {gate}\nschedules: []\n"),
        )
        .expect("write the project's gate configuration");
        (container, repo)
    }

    fn git_ok(repo: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git")
            .status
            .success()
    }

    /// **The proof of this slice: undoing a red merge takes back that merge and nothing else.**
    ///
    /// Two items with checkouts of their own. A lands and is kept; B lands and is rejected. The
    /// reset has to leave the branch holding A's file and not B's — and the reason it is not
    /// obvious is the ordinals. `footing_for`, which is what a sequential job reverts by, walks
    /// BACKWARDS THROUGH ORDINALS to the nearest item a gate agreed with. Here B is ordinal 0 and A
    /// is ordinal 1, and B merged second: reverting B by ordinal finds nothing before it and falls
    /// back to the job's `head_sha`, which takes A's merge with it.
    ///
    /// So the ordinals are deliberately the reverse of the merge order. That is not a contrived
    /// case — it is the ordinary one the moment items are allowed to finish at their own speed.
    #[tokio::test(flavor = "current_thread")]
    async fn reverting_a_red_merge_leaves_the_other_items_work_on_the_branch() {
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-merge-", "git --version");
        let trees = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(trees.path());
        let pool = test_pool().await;
        let job_id = seed_job_in(
            &pool,
            "nucleos",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();

        // The job's own checkout, on the job's branch. This is what the merges land in.
        let job_tree = crate::worktree::create(&repo, crate::worktree::Owner::Job(job_id))
            .await
            .expect("the job's checkout");
        crate::worktree::record(
            &pool,
            crate::worktree::Owner::Job(job_id),
            "nucleos",
            &repo.to_string_lossy(),
            &job_tree.path.to_string_lossy(),
            &job_tree.branch,
            job_tree.base_sha.as_deref(),
        )
        .await
        .unwrap();

        // Ordinal 0 is the one that will go red, ordinal 1 the one that must survive it.
        seed_items(&pool, job_id, &["implemented", "implemented"]).await;
        let items: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();

        // Each item works in a checkout of its own, born where the job's branch stands, and commits
        // one file there.
        for (item_id, file) in [(items[0], "red.txt"), (items[1], "green.txt")] {
            let base = crate::worktree::head_sha(&job_tree.path).await.unwrap();
            let tree = crate::worktree::adopt_or_create_at(
                &repo,
                crate::worktree::Owner::Item(item_id),
                Some(&base),
            )
            .await
            .expect("the item's checkout");
            std::fs::write(
                tree.path.join(file),
                "work
",
            )
            .expect("write the item's work");
            assert!(git_ok(&tree.path, &["add", "-A"]));
            assert!(git_ok(&tree.path, &["commit", "-m", file]));
        }

        // The green one lands first, and is kept.
        let state = test_state(pool.clone()).await;
        assert_eq!(
            merge_item(&state, &load_job(&pool, job_id).await.unwrap(), 1).await,
            Step::Continued
        );
        assert!(job_tree.path.join("green.txt").exists());

        // Then the red one lands, on top of it.
        assert_eq!(
            merge_item(&state, &load_job(&pool, job_id).await.unwrap(), 0).await,
            Step::Continued
        );
        assert!(job_tree.path.join("red.txt").exists());

        // And is undone.
        let target = revert_point(&pool, &load_job(&pool, job_id).await.unwrap(), 0)
            .await
            .expect("a red merge knows where it started");
        crate::worktree::revert_to(&job_tree.path, &target)
            .await
            .expect("put the branch back");

        assert!(
            !job_tree.path.join("red.txt").exists(),
            "the rejected merge is still on the branch"
        );
        assert!(
            job_tree.path.join("green.txt").exists(),
            "undoing item 0 took item 1's merge with it — which is what reverting by ordinal does"
        );
    }

    /// A job nobody scheduled: no rule on the row, and no rule in the line a person reads.
    ///
    /// `rule_name` was `&'a str` until a second caller appeared that has no rule to name. The
    /// column has accepted NULL since migration 0042, so this pins the type to the schema rather
    /// than widening anything — and it pins the feed line, which is the half a person actually
    /// sees. `job 7 started for rule 'None'` is the shape this exists to prevent: a line that reads
    /// like a bug in the scheduler for a job the scheduler never touched.
    #[tokio::test(flavor = "current_thread")]
    async fn um_job_sem_regra_nao_inventa_uma_no_registo_nem_no_feed() {
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-norule-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;

        let started = start(
            &state,
            &StartRequest {
                project_id: "nucleos",
                project_root: &repo.to_string_lossy(),
                rule_name: None,
                prompt: "build the thing",
                max_items: 3,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
            },
        )
        .await;

        let JobStart::Started(job_id) = started else {
            panic!("a job with no rule must still start");
        };

        let rule_name: Option<String> =
            sqlx::query_scalar("SELECT rule_name FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            rule_name, None,
            "no rule asked for this job, so the column has to say so rather than carry a sentinel"
        );

        let line: String = sqlx::query_scalar(
            "SELECT summary FROM feed WHERE kind = 'job_started' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !line.contains("rule"),
            "a job with no rule must not name one: {line}"
        );
        // The positive half, without which the assertion above would pass on an empty line.
        assert!(
            line.starts_with(&format!("job {job_id} started on ")),
            "the line still has to say what started and where: {line}"
        );
    }

    struct WorktreeRootEnv(Option<std::ffi::OsString>);
    impl WorktreeRootEnv {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("NUCLEOS_WORKTREE_ROOT");
            unsafe { std::env::set_var("NUCLEOS_WORKTREE_ROOT", path) };
            Self(previous)
        }
    }
    impl Drop for WorktreeRootEnv {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(value) => std::env::set_var("NUCLEOS_WORKTREE_ROOT", value),
                    None => std::env::remove_var("NUCLEOS_WORKTREE_ROOT"),
                }
            }
        }
    }

    /// Lets the node a pass started finish before the next pass reads its status.
    ///
    /// Nodes run as spawned tasks, so without this every pass would find its own node still
    /// `running` and answer `Wait` forever — the walk would hang rather than fail, which is the
    /// least useful way for a test to be wrong.
    async fn settle(state: &AppState, job_id: i64) {
        for _ in 0..500 {
            let running: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM runs WHERE job_id = ? AND status = 'running'",
            )
            .bind(job_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
            if running == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("a node never finished");
    }

    /// Drives a job to an ending, one pass per loop, exactly as the daemon's tick would.
    async fn walk(state: &AppState, job_id: i64) -> String {
        for _ in 0..20 {
            job_tick(state, Utc::now()).await;
            settle(state, job_id).await;
            let status = job_status(&state.pool, job_id).await;
            if !LIVE_STATUSES.contains(&status.as_str()) {
                return status;
            }
        }
        panic!(
            "the job never reached an ending; last status {}",
            job_status(&state.pool, job_id).await
        );
    }

    async fn start_job_for(
        state: &AppState,
        runner: &crate::runner::FakeCommandRunner,
        repo: &std::path::Path,
        plan: &str,
    ) -> i64 {
        *runner.plan_to_write.lock().unwrap() = Some(plan.to_owned());
        let root = repo.to_string_lossy().into_owned();
        // Through the same function `POST /jobs` and the scheduler both use, and NOT `None`.
        // `head_sha` is the footing the first item reverts to, so a helper that left it empty would
        // send every walk below down the "no footing, stop instead" branch — testing the fallback
        // and never the behaviour.
        let head_sha = crate::repo_trigger::current_branch_sha(repo, "HEAD", false).await;
        let job_id = insert_job(
            &state.pool,
            &NewJob {
                project_id: "project-a",
                project_root: &root,
                rule_name: Some("nightly"),
                prompt: "advance the backlog",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: head_sha.as_deref(),
                max_rounds: None,
                budget_usd: None,
            },
        )
        .await
        .expect("start a job");
        let owner = crate::worktree::Owner::Job(job_id);
        let info = crate::worktree::create(repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::worktree::record(
            &state.pool,
            owner,
            "project-a",
            &root,
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        job_id
    }

    /// The thesis of the whole feature, walked end to end for the first time: **one trigger
    /// produces a sequence of runs over one worktree**, so autonomous work is no longer capped by a
    /// single context window.
    ///
    /// Nothing here reaches around the machinery. The queue is written by the node into the handoff
    /// directory it learned about through its environment, and read back off disk by the daemon —
    /// so this covers the env plumbing, `prepare_artifacts`, the file contract of §5.2, the gate,
    /// and every transition between them. Seeding `job_items` directly would have exercised the
    /// parts and left the joins between them as the only place a bug could still live.
    #[tokio::test(flavor = "current_thread")]
    async fn one_trigger_becomes_a_sequence_of_runs_over_one_worktree() {
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-walk-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;

        let job_id = start_job_for(
            &state,
            &runner,
            &repo,
            r#"{"items":[{"description":"guard the cursor"},{"description":"retry on lock"}]}"#,
        )
        .await;

        assert_eq!(walk(&state, job_id).await, "completed");

        // Four runs from one trigger: plan, two items, review. This number IS the feature — one run
        // per trigger being the ceiling is the whole thing the design exists to remove.
        let stages: Vec<String> =
            sqlx::query_scalar("SELECT stage FROM runs WHERE job_id = ? ORDER BY id")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(stages, vec!["plan", "implement", "implement", "review"]);

        // One worktree, shared by all four. A second row would give the directory two owners and
        // let the GC collect it out from under a job still working in it.
        let trees: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worktrees")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(trees, 1);
        let cwds: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT cwd FROM runs WHERE job_id = ?")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(cwds.len(), 1, "every node ran in the same tree");

        // The queue came off disk, in order, and every item was actually measured.
        let items: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT description, status, gate_status FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, "guard the cursor");
        assert_eq!(items[1].0, "retry on lock");
        assert!(items.iter().all(|item| item.1 == "passed"));
        assert!(items.iter().all(|item| item.2.as_deref() == Some("passed")));

        let _ =
            crate::worktree::remove(&repo, &root.path().join(format!("job-{job_id}")), &[]).await;
    }

    /// Walked rather than asserted against a seeded row: a red gate no longer ends the night.
    ///
    /// This test used to pin the opposite, and the sentence it carried — *"Never started. The
    /// partial stays on the branch and the second item is still there to do."* — was the honest
    /// description of a real cost. The reason letting item i+1 run was unsafe is that it would build
    /// on unmeasured work; the checkpoint removes that reason by putting the tree back to the
    /// footing item i started from, so the objection no longer applies and the remaining items —
    /// which were never the ones that broke — are worth doing.
    ///
    /// The verdict is untouched: the job still ends `gate_failed`, decided at the end instead of at
    /// the first red item.
    #[tokio::test(flavor = "current_thread")]
    async fn a_red_gate_reverts_the_item_and_lets_the_queue_carry_on() {
        let _lock = crate::worktree::test_env_lock();
        // A command that runs everywhere and always fails, so this measures the chain's answer to a
        // red gate rather than to a missing binary — which is the other outcome entirely, and one
        // the design spends a whole section keeping apart from this one.
        let (_container, repo) =
            walkable_repo("nucleos-job-red-", "git rev-parse --verify no-such-ref");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, runner) = test_state_with_runner(pool.clone()).await;

        let job_id = start_job_for(
            &state,
            &runner,
            &repo,
            r#"{"items":[{"description":"first"},{"description":"second"}]}"#,
        )
        .await;

        assert_eq!(
            walk(&state, job_id).await,
            "gate_failed",
            "one red gate still decides what the job is called"
        );

        let items: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, gate_status FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        // Both, because this gate command fails for every item. The second one is the point: it ran.
        assert_eq!(items[0].0, "gate_failed");
        assert_eq!(items[0].1.as_deref(), Some("failed"));
        assert_eq!(
            items[1].0, "gate_failed",
            "the item after a red one has to have been attempted, not left pending"
        );
        let stages: Vec<String> =
            sqlx::query_scalar("SELECT stage FROM runs WHERE job_id = ? ORDER BY id")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            stages,
            vec!["plan", "implement", "implement", "review"],
            "the whole queue runs, and the review still gets to see what came out of it"
        );

        let _ =
            crate::worktree::remove(&repo, &root.path().join(format!("job-{job_id}")), &[]).await;
    }

    /// The plan node is the one node with no item to mark, so nothing else stops a second planner
    /// being started thirty seconds after the first.
    #[tokio::test]
    async fn a_planner_still_running_is_waited_for_rather_than_started_again() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_node(&pool, job_id, "plan", "running").await;

        let view = load_view(&pool, job_id).await.unwrap();

        assert!(view.planning);
        assert_eq!(next_step(&view), Next::Wait);
    }

    /// A node paused for approval has not finished. Reading it as finished would let the job walk
    /// past work that never happened, or complete while its review sits on a question nobody has
    /// been shown.
    #[tokio::test]
    async fn a_node_paused_for_approval_is_still_in_flight() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_node(&pool, job_id, "plan", "awaiting_approval").await;

        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Wait
        );

        let reviewing = seed_job(&pool, "project-b", "reviewing").await.unwrap();
        seed_items(&pool, reviewing, &["passed"]).await;
        seed_node(&pool, reviewing, "review", "awaiting_approval").await;

        assert_eq!(
            next_step(&load_view(&pool, reviewing).await.unwrap()),
            Next::Wait
        );
    }

    // ---- parking and coming back ---------------------------------------------------------------

    /// The bug the `resume_status` column exists for. `waiting` overwrites the status underneath
    /// it, and `planning` is the one status that carries meaning the job cannot rebuild: without
    /// this, a job parked for budget while still planning would come back as
    /// planned-with-an-empty-queue — the honest "there was no work" night — and report itself
    /// complete having planned nothing.
    #[tokio::test]
    async fn a_job_parked_while_planning_still_plans_when_it_comes_back() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();

        wait(&pool, job_id, "budget").await.unwrap();
        // Even parked, before anything unparks it, the queue does not exist yet.
        assert!(!load_view(&pool, job_id).await.unwrap().planned);

        resume(&pool, job_id).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "planning");
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnPlan
        );
    }

    /// Parking is re-applied every pass a brake is still on, so it has to survive being called on
    /// an already-parked job. Recording `waiting` as the stage to return to would leave the job
    /// with no way home.
    #[tokio::test]
    async fn parking_a_parked_job_does_not_lose_where_it_was() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();

        wait(&pool, job_id, "budget").await.unwrap();
        wait(&pool, job_id, "attention").await.unwrap();
        resume(&pool, job_id).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "implementing");
    }

    // ---- folding a finished node back into the job ---------------------------------------------

    #[tokio::test]
    async fn a_node_still_in_flight_is_not_reconciled() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "awaiting_approval").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ?")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(item_statuses(&pool, job_id).await, vec!["running"]);
    }

    /// Only `completed` is done. A node that timed out, was cancelled or was interrupted leaves the
    /// tree holding edits no gate has measured, and calling any of those finished would let the
    /// next item build on top of them.
    #[tokio::test]
    async fn a_node_that_did_not_complete_stops_the_chain_at_its_item() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running", "pending"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "timed_out").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ? AND ordinal = 0")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["failed", "pending"]
        );
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Failed)
        );
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_item_failed".to_owned())
        );
    }

    #[tokio::test]
    async fn a_finished_node_hands_its_item_to_the_gate() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["running"]).await;
        let run_id = seed_node(&pool, job_id, "implement", "completed").await;
        sqlx::query("UPDATE job_items SET run_id = ? WHERE job_id = ?")
            .bind(run_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(item_statuses(&pool, job_id).await, vec!["implemented"]);
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::RunGate { ordinal: 0 }
        );
    }

    // ---- the queue the planner produced --------------------------------------------------------

    /// The pair this whole module turns on. A planner that produced nothing failed; a planner that
    /// found nothing had a quiet, successful night. Collapsing them makes every crashed plan node
    /// look like a job well done, and teaches the reader to ignore the feed.
    #[tokio::test]
    async fn a_plan_node_that_wrote_no_file_fails_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "failed");
        assert!(item_statuses(&pool, job_id).await.is_empty());
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_plan_failed".to_owned())
        );
    }

    #[tokio::test]
    async fn a_planner_that_found_no_work_completes_the_job() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;
        write_plan(worktree.path(), r#"{"items": []}"#).await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(job_status(&pool, job_id).await, "implementing");
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Completed)
        );
    }

    // ---- the replan node (Chunk 3, task 11) ------------------------------------------------------

    /// Seeds a job at the end of a round, with a landed replan node and the answer it wrote.
    async fn seed_closed_round(
        pool: &sqlx::SqlitePool,
        worktree: &std::path::Path,
        run_status: &str,
        answer: Option<&str>,
    ) -> i64 {
        let job_id = seed_job(pool, "project-a", "reviewing").await.unwrap();
        seed_worktree(pool, job_id, worktree).await;
        seed_items(pool, job_id, &["passed", "passed"]).await;
        sqlx::query("UPDATE jobs SET max_rounds = 5 WHERE id = ?")
            .bind(job_id)
            .execute(pool)
            .await
            .unwrap();
        seed_node(pool, job_id, "replan", run_status).await;
        if let Some(answer) = answer {
            write_plan(worktree, answer).await;
        }
        job_id
    }

    async fn round_counters(pool: &sqlx::SqlitePool, job_id: i64) -> (i64, i64, i64) {
        sqlx::query_as("SELECT round, dry_rounds, replan_done FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Starting a replan node must not make this round's finished review vanish.
    ///
    /// The review is scoped by "which replan opened this round", and reading that from the LATEST
    /// replan run is wrong in exactly one window: while a replan is deciding whether there is a next
    /// round, it has opened nothing. Its id moved the line anyway, the round's own review dropped
    /// out of view, and the job spawned a second one over a queue that had not changed.
    ///
    /// Measured on job 17, 2026-08-08 — run 900179, one wasted node per round. `jobs.replan_run_id`
    /// is the honest line because it is written where a replan's answer is acted on, not where the
    /// node is started.
    #[tokio::test]
    async fn a_replan_in_flight_does_not_make_this_rounds_review_look_unrun() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool, "project-a", "reviewing").await.unwrap();
        seed_items(&pool, job_id, &["passed", "passed"]).await;
        sqlx::query("UPDATE jobs SET max_rounds = 5 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_node(&pool, job_id, "review", "completed").await;

        // The round's review has landed, so the round closes into a replan.
        let before = load_view(&pool, job_id).await.unwrap();
        assert_eq!(before.review, ReviewState::Done);
        assert_eq!(next_step(&before), Next::SpawnReplan);

        // ...and now that node exists and is thinking. Nothing about the round changed.
        seed_node(&pool, job_id, "replan", "running").await;

        let during = load_view(&pool, job_id).await.unwrap();
        assert_eq!(
            during.review,
            ReviewState::Done,
            "the review this round already had was still had"
        );
        assert_eq!(
            next_step(&during),
            Next::Wait,
            "waiting for the replan, not paying for a second review of the same queue"
        );
    }

    /// Ending #1. `completed` and not `stopped`, because the node that just looked at the work is
    /// the one saying the work is over — the best evidence this system can get for that claim.
    #[tokio::test]
    async fn a_replan_that_says_done_ends_the_job_completed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_closed_round(
            &pool,
            worktree.path(),
            "completed",
            Some(r#"{"done": true, "why": "the task is met"}"#),
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(round_counters(&pool, job_id).await.2, 1, "replan_done");
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::Finish(Outcome::Completed)
        );
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_replanned".to_owned())
        );
    }

    /// Ordinals CONTINUE across rounds. They are half `job_items`'s primary key, and the position in
    /// the loaded queue is the number `advance` binds into its lookups — restarting them per round
    /// would collide on the first insert and mislead on every one after.
    #[tokio::test]
    async fn a_replan_with_items_opens_the_next_round_past_the_old_ordinals() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_closed_round(
            &pool,
            worktree.path(),
            "completed",
            Some(r#"{"items": [{"description": "one more thing"}]}"#),
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        let (round, dry, done) = round_counters(&pool, job_id).await;
        assert_eq!(
            (round, dry, done),
            (1, 0, 0),
            "a round that added work is not dry"
        );
        assert_eq!(job_status(&pool, job_id).await, "implementing");

        let placed: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT ordinal, round FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(placed, vec![(0, 0), (1, 0), (2, 1)]);
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnImplement { ordinal: 2 }
        );
    }

    /// The whole reason `jobs.replan_run_id` exists, stated as the loop it prevents.
    ///
    /// A replan that produced nothing leaves every observable condition exactly as it found it: the
    /// queue is still fully terminal, `replan_done` is still 0, and the latest replan run is still
    /// the same one. Any DERIVED test for "already taken" therefore answers no on the next tick, and
    /// the round would keep advancing — one per tick, thirty seconds apart — until the ceiling ended
    /// a job that had done nothing at all.
    #[tokio::test]
    async fn a_dry_replan_is_counted_once_however_many_ticks_pass() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_closed_round(
            &pool,
            worktree.path(),
            "completed",
            Some(r#"{"items": []}"#),
        )
        .await;

        for _ in 0..3 {
            let job = load_job(&pool, job_id).await.unwrap();
            reconcile_nodes(&state, &job).await.unwrap();
        }

        let (round, dry, _) = round_counters(&pool, job_id).await;
        assert_eq!(
            (round, dry),
            (1, 1),
            "three ticks, one answer: a dry round is still one round"
        );
        // One short of the brake, so the job asks again rather than ending.
        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnReplan
        );
    }

    /// A replan that could not answer stops the job; it does not fail it.
    ///
    /// The rounds that ran are on the branch, gated green, and worth looking at. `failed` for want of
    /// a replan node teaches its reader to ignore the branch — which is the one thing the whole
    /// `Completed`/`Stopped` split exists to prevent.
    #[tokio::test]
    async fn a_replan_that_could_not_answer_stops_the_job_rather_than_failing_it() {
        for (run_status, answer) in [("failed", None), ("completed", None)] {
            let pool = test_pool().await;
            let state = test_state(pool.clone()).await;
            let worktree = tempfile::tempdir().unwrap();
            let job_id = seed_closed_round(&pool, worktree.path(), run_status, answer).await;

            let job = load_job(&pool, job_id).await.unwrap();
            reconcile_nodes(&state, &job).await.unwrap();

            assert_eq!(
                job_status(&pool, job_id).await,
                STATUS_STOPPED,
                "{run_status}"
            );
            assert!(feed_kinds(&pool).await.contains(&"job_stopped".to_owned()));
        }
    }

    /// Truncation is reported, never silent: a queue quietly cut from seven to five reads
    /// downstream as "the planner found five things", which is a different and wrong statement
    /// about the work.
    #[tokio::test]
    async fn a_plan_above_the_ceiling_is_cut_and_the_feed_says_by_how_much() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;
        let seven: Vec<String> = (0..7)
            .map(|n| format!(r#"{{"description":"item {n}"}}"#))
            .collect();
        write_plan(
            worktree.path(),
            &format!(r#"{{"items":[{}]}}"#, seven.join(",")),
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        assert_eq!(item_statuses(&pool, job_id).await.len(), 5);
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'job_planned'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            summary.contains('2'),
            "the feed must say what was left out: {summary}"
        );
    }

    /// The hint has to survive the gap between the plan node and the implement node, which is a
    /// database row and not a process — nothing of `PlannedItems` is still in memory by the time
    /// the item is spawned. An item with no hint stores NULL rather than `[]`: the column that says
    /// nothing is the one that reads downstream as "the planner did not say", and an empty array
    /// would read as "the planner said no files", which is a different claim.
    #[tokio::test]
    async fn planned_file_hints_are_stored_with_the_item() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "planning").await.unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_node(&pool, job_id, "plan", "completed").await;
        write_plan(
            worktree.path(),
            r#"{"items":[{"description":"hinted","files":["core/src/job.rs","core/src/runs.rs"]},
                        {"description":"unhinted"}]}"#,
        )
        .await;

        let job = load_job(&pool, job_id).await.unwrap();
        reconcile_nodes(&state, &job).await.unwrap();

        let stored: Vec<Option<String>> =
            sqlx::query_scalar("SELECT files FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();

        let hinted: Vec<String> =
            serde_json::from_str(stored[0].as_deref().expect("a hinted item keeps its hint"))
                .expect("the hint is stored as a JSON array");
        assert_eq!(hinted, vec!["core/src/job.rs", "core/src/runs.rs"]);
        assert_eq!(stored[1], None, "no hint is NULL, not an empty array");
    }

    // ---- the brakes between nodes --------------------------------------------------------------

    async fn set_budget(pool: &sqlx::SqlitePool, window: Option<f64>, hourly: Option<f64>) {
        sqlx::query(
            "UPDATE autopilot_global
             SET budget_limit_usd = ?, budget_hourly_limit_usd = ?, budget_per_run_reserve_usd = 1.0",
        )
        .bind(window)
        .bind(hourly)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_job_run(pool: &sqlx::SqlitePool, job_id: i64, cost: f64) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, job_id, cost_usd, created_at, completed_at)
             VALUES ('a node', 'completed', 'worktree', ?, ?, ?, ?)",
        )
        .bind(job_id)
        .bind(cost)
        .bind(Utc::now().to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    /// A job's own allowance STOPS it and never parks it, which is the whole difference from the
    /// window brake above. A calendar window reopens; a task's allowance does not, so a job parked
    /// on it would sit at `waiting` until the four-hour ceiling swept it up, wearing a reason nobody
    /// could act on.
    #[tokio::test]
    async fn a_job_that_spent_its_own_allowance_stops_rather_than_waiting() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_job_run(&pool, job_id, 4.5).await;

        // The house limit is off, so anything that fires here is the job's own.
        sqlx::query("UPDATE jobs SET budget_usd = 5.0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let job = load_job(&pool, job_id).await.unwrap();
        assert!(
            matches!(brakes(&state, &job, Utc::now()).await, Brake::Stop { .. }),
            "spent 4.50 of 5.00 and the next node reserves 1.00"
        );

        // Room for another node: the test is about the NEXT one, never the one in flight.
        sqlx::query("UPDATE jobs SET budget_usd = 20.0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        let job = load_job(&pool, job_id).await.unwrap();
        assert!(matches!(brakes(&state, &job, Utc::now()).await, Brake::Go));
    }

    /// A NULL allowance means "only the house limit governs", which is what every `graph:` rule has
    /// always meant. A job that spent plenty is untouched by a brake it never asked for.
    #[tokio::test]
    async fn a_job_with_no_allowance_of_its_own_is_governed_only_by_the_house() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_job_run(&pool, job_id, 500.0).await;

        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(job.budget_usd, None);
        assert!(matches!(brakes(&state, &job, Utc::now()).await, Brake::Go));
    }

    async fn exclude(pool: &sqlx::SqlitePool, project_id: &str, low: i64, high: i64) {
        sqlx::query(
            "INSERT INTO fleet_exclusions
                 (project_id, job_low, job_high, proposal_id, created_at)
             VALUES (?, ?, ?, 1, '2026-08-15T00:00:00Z')",
        )
        .bind(project_id)
        .bind(low)
        .bind(high)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The whole journey, with nothing along it faked.
    ///
    /// Every other test here inserts the rule straight into `fleet_exclusions`, which is right for
    /// testing the brake and wrong for testing the FEATURE: those would pass unchanged if `propose`
    /// filed the wrong pair or `approve` wrote a row nothing reads. This one walks the path a person
    /// walks — ask, approve, watch one job hold and the other wait, and watch the wait end by itself
    /// — through the same functions the routes call.
    ///
    /// The two jobs are asked about BACKWARDS (`high` first) on purpose: the ordering that makes the
    /// tie-break work has to survive the request, not just the table.
    #[tokio::test]
    async fn asking_approving_and_waiting_is_one_unbroken_chain() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let low = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let high = seed_job(&pool, "project-a", "implementing").await.unwrap();

        // 1. Somebody draws the edge. Nothing about scheduling changes yet.
        let proposal = crate::exclusion::propose(&pool, high, low, &[])
            .await
            .unwrap();
        crate::concurrency::claim(&pool, "project-a", crate::worktree::Owner::Job(low))
            .await
            .unwrap();
        let high_row = load_job(&pool, high).await.unwrap();
        assert!(
            matches!(brakes(&state, &high_row, Utc::now()).await, Brake::Go),
            "a request that nobody has approved must not hold anything"
        );

        // 2. Somebody approves it. Only now is there a rule.
        let approved = crate::exclusion::approve(&pool, proposal).await.unwrap();
        assert!(
            matches!(approved, crate::exclusion::Approved::Written(_)),
            "got {approved:?}"
        );

        // 3. The higher job waits, and the reason names what to wait for.
        let Brake::Park { reason, detail } = brakes(&state, &high_row, Utc::now()).await else {
            panic!("the higher job must wait while its partner holds a slot");
        };
        assert_eq!(reason, "excluded");
        assert!(detail.contains(&format!("job {low}")), "got: {detail}");
        // And it is written on the job itself, which is what the card reads.
        park(&state, &high_row, reason, &detail).await;
        let parked = load_job(&pool, high).await.unwrap();
        assert_eq!(parked.status, "waiting");
        assert_eq!(parked.wait_reason.as_deref(), Some("excluded"));

        // 4. The partner finishes. Nobody presses anything.
        crate::concurrency::release(&pool, crate::worktree::Owner::Job(low))
            .await
            .unwrap();
        resume(&pool, high).await.unwrap();
        let woken = load_job(&pool, high).await.unwrap();
        assert_eq!(
            woken.wait_reason, None,
            "the note outlives the pause it explains"
        );
        assert!(matches!(
            brakes(&state, &woken, Utc::now()).await,
            Brake::Go
        ));
    }

    /// One of the two waits, and it is always the same one.
    ///
    /// The asymmetry is the deadlock argument, not a detail of the query: the low id is never parked
    /// by this brake, so of any two excluded jobs at least one is always free to run. Were the
    /// tie-break decided at read time, two reads that disagreed would park both — and two jobs
    /// somebody asked to SERIALISE, stopped forever on each other, is the one failure this feature
    /// must not be able to produce.
    #[tokio::test]
    async fn the_higher_job_waits_for_its_partner_and_the_lower_one_never_does() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let low = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let high = seed_job(&pool, "project-a", "implementing").await.unwrap();
        exclude(&pool, "project-a", low, high).await;
        crate::concurrency::claim(&pool, "project-a", crate::worktree::Owner::Job(low))
            .await
            .unwrap();

        let high_row = load_job(&pool, high).await.unwrap();
        let Brake::Park { reason, detail } = brakes(&state, &high_row, Utc::now()).await else {
            panic!("the higher job must wait while its partner holds a slot");
        };
        assert_eq!(reason, "excluded");
        assert!(
            detail.contains(&format!("job {low}")),
            "the reason has to name what to wait for, got: {detail}"
        );

        // The other side of the same rule, at the same moment: never parked by it.
        let low_row = load_job(&pool, low).await.unwrap();
        assert!(matches!(
            brakes(&state, &low_row, Utc::now()).await,
            Brake::Go
        ));
    }

    /// The brake lifts by itself, which is why it is a `Park` and not a `Stop`.
    ///
    /// Two ways for it to lift, and both are tested here because they fail differently: the partner
    /// gives its slot back, or somebody revokes the rule. A brake that needed a person to restart
    /// the job would make serialising two fronts of work cost more attention than doing them by
    /// hand.
    #[tokio::test]
    async fn the_wait_ends_when_the_slot_goes_back_or_the_rule_is_revoked() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        set_budget(&pool, None, None).await;
        let low = seed_job(&pool, "project-a", "implementing").await.unwrap();
        let high = seed_job(&pool, "project-a", "implementing").await.unwrap();
        exclude(&pool, "project-a", low, high).await;
        crate::concurrency::claim(&pool, "project-a", crate::worktree::Owner::Job(low))
            .await
            .unwrap();
        let high_row = load_job(&pool, high).await.unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Park { .. }
        ));

        crate::concurrency::release(&pool, crate::worktree::Owner::Job(low))
            .await
            .unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Go
        ));

        // And with the slot taken again, revoking the rule releases it just the same.
        crate::concurrency::claim(&pool, "project-a", crate::worktree::Owner::Job(low))
            .await
            .unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Park { .. }
        ));
        sqlx::query("UPDATE fleet_exclusions SET revoked_at = '2026-08-15T01:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            brakes(&state, &high_row, Utc::now()).await,
            Brake::Go
        ));
    }

    /// Decision 9, and the whole reason `PauseKind` exists. The hourly brake lifts by itself, so
    /// the job comes back; the window ceiling does not lift before the period rolls over, so
    /// waiting for it would be a hang dressed up as patience.
    #[tokio::test]
    async fn an_hourly_brake_parks_a_job_and_a_spent_window_ends_it() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["pending"]).await;
        set_budget(&pool, None, Some(0.0)).await;

        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(advance(&state, &job, Utc::now()).await, Step::Stopped);
        assert_eq!(job_status(&pool, job_id).await, "waiting");
        let reason: Option<String> =
            sqlx::query_scalar("SELECT wait_reason FROM jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("budget"));

        set_budget(&pool, Some(0.0), None).await;
        resume(&pool, job_id).await.unwrap();
        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, STATUS_STOPPED);
        assert!(feed_kinds(&pool).await.contains(&"job_stopped".to_owned()));
    }

    /// The number in that line is what the night left behind, and a retriable item is work.
    ///
    /// An item whose gate went red with a retry still to spend is one the queue owed another
    /// implement run. Counting only `Pending` reports one item fewer than was really left, and the
    /// item it drops is precisely the one that had already cost money.
    #[tokio::test]
    async fn a_stopped_job_counts_a_retriable_item_among_the_unfinished() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        // The row a red gate with a retry left behind, written the way `record_gate` writes it: the
        // status still says `gate_failed`, and it is the attempt count read beside the job's budget
        // that makes the item retriable rather than finished with.
        seed_items(&pool, job_id, &["gate_failed", "pending"]).await;
        sqlx::query(
            "UPDATE job_items SET gate_status = 'failed', gate_attempts = 1
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();
        // The window ceiling, because it is the brake that stops rather than parks.
        set_budget(&pool, Some(0.0), None).await;

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, STATUS_STOPPED);
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'job_stopped'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            summary.contains("2 item(s) unfinished"),
            "the retriable item is work the queue still owed a run: {summary}"
        );
    }

    /// Decision 10. The brake used to be a check made once at admission, so a job admitted at 03:00
    /// would keep starting nodes at 08:00 with the owner at the keyboard.
    #[tokio::test]
    async fn the_attention_brake_refuses_the_next_node_but_never_the_gate() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        // `project_root` is the tempdir so the gate configuration below is genuinely absent rather
        // than unreadable — this test is about the brake, not about gate configuration.
        sqlx::query("UPDATE jobs SET project_root = ? WHERE id = ?")
            .bind(worktree.path().to_string_lossy().into_owned())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["implemented", "pending"]).await;
        sqlx::query(
            "INSERT INTO attention_heartbeats (scope, project_id, last_seen_at) VALUES ('project', 'project-a', ?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        // The gate is a subprocess, not a run: it starts no CLI and answers to no brake. Refusing
        // to measure would leave the tree holding edits nothing verified, which is the state
        // decision 10 exists to avoid.
        let job = load_job(&pool, job_id).await.unwrap();
        assert_eq!(advance(&state, &job, Utc::now()).await, Step::Continued);
        assert_eq!(item_statuses(&pool, job_id).await[0], "passed");

        // The next item, though, does not start.
        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;
        assert_eq!(job_status(&pool, job_id).await, "waiting");
        assert_eq!(item_statuses(&pool, job_id).await[1], "pending");
    }

    /// Decision 14, and the reason parked time counts toward it: without that, parking would be a
    /// way around the ceiling and a starved job would hold a worktree and the project's slot for
    /// as long as the contention lasted.
    #[tokio::test]
    async fn the_life_ceiling_retires_a_job_that_is_still_parked() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["pending"]).await;
        wait(&pool, job_id, "slot").await.unwrap();
        let started = Utc::now() - MAX_JOB_LIFETIME - chrono::Duration::minutes(1);
        sqlx::query("UPDATE jobs SET created_at = ? WHERE id = ?")
            .bind(started.to_rfc3339())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        job_tick(&state, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, STATUS_EXPIRED);
        assert!(feed_kinds(&pool).await.contains(&"job_expired".to_owned()));
    }

    /// The panic button stops the chain, not just the node. It does not mark anything terminal,
    /// deliberately: the read fails closed, so a database hiccup that retired every job in flight
    /// would be a far worse failure than the one the switch is for.
    #[tokio::test]
    async fn the_kill_switch_stops_the_chain_without_destroying_it() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, job_id, &["pending"]).await;
        sqlx::query("UPDATE autopilot_global SET kill_switch = 1")
            .execute(&pool)
            .await
            .unwrap();

        job_tick(&state, Utc::now()).await;

        assert_eq!(job_status(&pool, job_id).await, "implementing");
        assert_eq!(item_statuses(&pool, job_id).await, vec!["pending"]);
        assert!(feed_kinds(&pool).await.is_empty());
    }

    // ---- the gate --------------------------------------------------------------------------

    /// §7 of the design calls this the easiest mistake to make, because both readings are "did not
    /// pass". A `cargo` that would not start has to reach the user as "not measured", never as
    /// "your tests failed" — one is silence and the other is a verdict.
    #[tokio::test]
    async fn a_gate_that_could_not_run_ends_a_job_differently_from_one_that_failed() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;

        let broken = seed_job(&pool, "project-a", "implementing").await.unwrap();
        seed_items(&pool, broken, &["implemented"]).await;
        let row = load_job(&pool, broken).await.unwrap();
        record_gate(
            &state,
            &row,
            0,
            crate::gate::GateOutcome::Failed {
                exit_code: 101,
                output: "test failed".into(),
            },
        )
        .await;

        let unmeasured = seed_job(&pool, "project-b", "implementing").await.unwrap();
        seed_items(&pool, unmeasured, &["implemented"]).await;
        let row = load_job(&pool, unmeasured).await.unwrap();
        record_gate(
            &state,
            &row,
            0,
            crate::gate::GateOutcome::Errored {
                reason: "cargo not found".into(),
            },
        )
        .await;

        // Neither job has a worktree on record, so the red one takes the branch where a revert
        // cannot happen: `record_gate` marks the item anyway and answers `Stopped`, refusing to let
        // a queue advance onto a tree it could not put back. The MARK is what these assertions read.
        //
        // And there the two part company, which is the point of the test. Silence stops the job
        // where it stands. A verdict does not: the queue is spent, so the job goes on to have its
        // work reviewed, and only then is it called `gate_failed` — pinned purely in
        // `one_red_gate_makes_the_whole_job_gate_failed_however_it_ends`.
        assert_eq!(
            next_step(&load_view(&pool, broken).await.unwrap()),
            Next::SpawnReview
        );
        assert_eq!(
            next_step(&load_view(&pool, unmeasured).await.unwrap()),
            Next::Finish(Outcome::GateErrored)
        );
    }

    /// The retry budget is copied onto the job when it starts, or the config key is decoration.
    ///
    /// The join every other test in this chunk hangs off, and the only one that can fail on it.
    /// `GraphConfig::gate_retries()` can be right, `item_state_from` can be right, `record_gate` can
    /// be right, and every one of those assertions can pass while every real job runs on the column
    /// default of 0 — because nothing on the way IN ever carried the number across. The two
    /// pool-backed tests below set `jobs.gate_retries` by hand, so they cannot notice; this one goes
    /// through `NewJob` and reads the row back, which is the only shape that can.
    ///
    /// Copied rather than re-read, for the reason `0042_jobs.sql` already gives about `max_items`,
    /// `gate_each` and `review`: `.ai/autopilot.yaml` can be edited mid-flight, and a job that
    /// changed shape between its own nodes would gate some items and not others with nothing
    /// recording why. A budget re-read at each step has that failure with a worse symptom — one item
    /// retried because the file said 2 this morning, and the item beside it dropped because it says
    /// 0 now, in the same night, under the same gate.
    #[tokio::test]
    async fn a_jobs_retry_budget_comes_from_the_rule_that_started_it() {
        let pool = test_pool().await;
        let job_id = insert_job(
            &pool,
            &NewJob {
                project_id: "project-a",
                project_root: "/project/a",
                rule_name: Some("nightly-backlog"),
                prompt: "pull from the todo list and advance what you can",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 2,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
            },
        )
        .await
        .expect("start a job");

        let stored: i64 = sqlx::query_scalar("SELECT gate_retries FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            stored, 2,
            "the budget the rule asked for never reached the row the job actually runs on"
        );
    }

    /// A red gate with a retry left leaves the tree where it stands and asks for the item again.
    ///
    /// **No worktree row is seeded, and the absence is the assertion.** Today a red gate reverts the
    /// tree to the item's footing before it marks anything, and a job with no worktree on record
    /// takes the `!reverted` branch: it marks the item and stops the night, refusing to let the
    /// queue advance onto a tree it could not put back. A retry must not reach for any of that — the
    /// work it is about to redo is the work standing in the tree, and reverting it first would throw
    /// away everything the second node would otherwise start from, turning a near miss into a blank
    /// page. If the revert machinery still ran here, this job would come back with a dead queue
    /// instead of asking for item 1 again, and this test is what would notice.
    ///
    /// The count and the output are the other half. Without the count the budget never runs out and
    /// a hopeless item retries forever; without the output the second node is a second guess.
    #[tokio::test]
    async fn a_red_gate_with_a_retry_left_keeps_the_tree() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        // Set on the row rather than asked for at creation: `NewJob` carries no retry budget yet,
        // and a job is what the column says it is whatever wrote it.
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        // `implemented` is the status an item is really in when the gate measures it — `next_step`
        // answers `RunGate` for exactly that state and no other.
        seed_items(&pool, job_id, &["implemented"]).await;
        let row = load_job(&pool, job_id).await.unwrap();

        record_gate(
            &state,
            &row,
            0,
            crate::gate::GateOutcome::Failed {
                exit_code: 1,
                output: "boom".into(),
            },
        )
        .await;

        let (attempts, output): (i64, Option<String>) = sqlx::query_as(
            "SELECT gate_attempts, gate_output FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            attempts, 1,
            "an uncounted red gate is a budget that never runs out"
        );
        assert_eq!(
            output.as_deref(),
            Some("boom"),
            "the retrying node has to be able to read what the gate said"
        );

        assert_eq!(
            next_step(&load_view(&pool, job_id).await.unwrap()),
            Next::SpawnImplement { ordinal: 0 },
            "the queue has to come back to the same item, not stop and not step over it"
        );
    }

    /// Asking for the item again is not the same as starting it, and only one of those was tested.
    ///
    /// Every other test of this feature stops at `next_step`, which is pure and which answers
    /// `SpawnImplement { ordinal: 0 }` for a retriable item quite correctly. `advance` then has to
    /// CLAIM that item, and the claim is a compare-and-swap that names the status it expects:
    /// `WHERE ... AND status = 'pending'`. A retriable item's row says `gate_failed`, so the swap
    /// matched nothing, `rows_affected() == 0` took the branch that exists to stop two nodes starting
    /// on one tree, and the job stopped at the first red gate — with the rejected work still standing,
    /// because `record_gate` had correctly skipped the revert on the way in. That is worse than the
    /// behaviour the retry replaced: before it, the job at least reverted and carried on.
    ///
    /// So this test crosses from the pure half into the I/O half deliberately. It drives `advance`,
    /// not `next_step`, and it reads the two rows that prove a node actually started: the item is
    /// `running` and it has a run attached. The prompt is the third assertion, and it is what makes
    /// the whole chain load-bearing — output stored by `record_gate`, selected by `advance`, appended
    /// by `implement_prompt`, and handed to a node. Any link missing and this says so.
    /// A real repository and a real worktree, because a node that cannot be provisioned would leave
    /// the item back where it started for a reason that has nothing to do with the claim — and would
    /// look exactly like the defect this pins.
    #[tokio::test(flavor = "current_thread")]
    async fn a_retriable_item_is_claimed_and_started_not_merely_asked_for() {
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-retryclaim-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        let owner = crate::worktree::Owner::Job(job_id);
        let info = crate::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        sqlx::query("UPDATE jobs SET gate_retries = 1 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        // The row a red gate with a retry left leaves behind, written the way `record_gate` writes
        // it: the status still says `gate_failed`, and it is the count beside the job's budget that
        // makes it retriable. Seeded rather than gated so that what fails here can only be the claim.
        seed_items(&pool, job_id, &["gate_failed"]).await;
        sqlx::query(
            "UPDATE job_items SET gate_status = 'failed', gate_attempts = 1, gate_output = ?
             WHERE job_id = ? AND ordinal = 0",
        )
        .bind("FAILED tests/test_cursor.py::test_guard")
        .bind(job_id)
        .execute(&pool)
        .await
        .unwrap();

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        assert_eq!(
            item_statuses(&pool, job_id).await,
            vec!["running"],
            "the retry was asked for and never claimed, so the job stopped on an unreverted tree"
        );
        let run_id: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let run_id = run_id.expect("the retried item has a node of its own attached to it");

        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            prompt.contains("FAILED tests/test_cursor.py::test_guard"),
            "the node that has to answer the gate was not told what it said: {prompt}"
        );
    }

    /// Out of retries, the tree still governs, and nothing about that branch has moved.
    ///
    /// `gate_retries = 0` is every job written before migration 0068, so this is the regression the
    /// retry is most likely to break: with no budget the first red gate is final, and a red gate
    /// whose worktree cannot be put back still stops the night where it stands rather than letting
    /// the next item build on work the gate has just called broken.
    ///
    /// The attempt is counted anyway. It costs nothing here — there is no budget for it to be
    /// measured against — and a counter that only increments on the paths that spend it would be a
    /// counter that disagrees with itself about what happened to the item.
    #[tokio::test]
    async fn a_red_gate_out_of_retries_still_stops_when_it_cannot_revert() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        // Said out loud rather than left to the column default, because it is the premise: this is
        // a job with nothing to spend.
        sqlx::query("UPDATE jobs SET gate_retries = 0 WHERE id = ?")
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_items(&pool, job_id, &["implemented"]).await;
        let row = load_job(&pool, job_id).await.unwrap();

        let step = record_gate(
            &state,
            &row,
            0,
            crate::gate::GateOutcome::Failed {
                exit_code: 1,
                output: "boom".into(),
            },
        )
        .await;

        assert_eq!(
            step,
            Step::Stopped,
            "a tree that could not be put back still ends the pass"
        );
        assert_eq!(item_statuses(&pool, job_id).await, vec!["gate_failed"]);
        let attempts: i64 = sqlx::query_scalar(
            "SELECT gate_attempts FROM job_items WHERE job_id = ? AND ordinal = 0",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            attempts, 1,
            "the red gate happened, and it is counted whether or not it bought anything"
        );
    }

    /// `gate_after_each_item: false` buys back the intermediate suite runs. The final gate runs
    /// regardless, because a job that measured nothing hands back a partial nobody can trust.
    #[tokio::test]
    async fn the_last_item_is_gated_even_when_per_item_gating_is_off() {
        let pool = test_pool().await;
        let state = test_state(pool.clone()).await;
        let worktree = tempfile::tempdir().unwrap();
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        std::fs::create_dir_all(worktree.path().join(".ai")).unwrap();
        std::fs::write(
            worktree.path().join(".ai").join("autopilot.yaml"),
            "gate_command: definitely-not-a-real-binary\n",
        )
        .unwrap();
        sqlx::query("UPDATE jobs SET gate_each = 0, project_root = ? WHERE id = ?")
            .bind(worktree.path().to_string_lossy().into_owned())
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        seed_worktree(&pool, job_id, worktree.path()).await;
        seed_items(&pool, job_id, &["implemented", "implemented"]).await;

        let job = load_job(&pool, job_id).await.unwrap();
        gate_item(&state, &job, 0, 2).await;
        // Not measured, so no verdict is recorded against it — the item passes on the strength of
        // the final gate that is still to come.
        assert_eq!(item_statuses(&pool, job_id).await[0], "passed");
        let verdicts: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT gate_status FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(verdicts[0], None);

        gate_item(&state, &job, 1, 2).await;
        // A command that will not start is `errored`, and the distinction is what proves the last
        // item was actually measured rather than waved through like the first.
        assert_eq!(item_statuses(&pool, job_id).await[1], "gate_errored");
    }

    // ---- surviving a restart -------------------------------------------------------------------

    /// Decision 11. The discriminator cannot be liveness: by the time this runs, the startup pass
    /// over `runs` has marked every run `interrupted`, so "has no live node" is true of every job
    /// — including the ones that died in the gap between two nodes, which are the recoverable ones.
    #[tokio::test]
    async fn a_crash_keeps_the_job_only_while_its_repository_has_not_moved() {
        let pool = test_pool().await;
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();

        // No git repository at that path at all, so HEAD cannot be read: not the same as knowing it
        // stayed put, and the safe direction is to stop. What the job finished is on its branch.
        let unreadable = seed_job_in(&pool, "project-a", "implementing", &root, Some("abc123"))
            .await
            .unwrap();
        assert_eq!(reconcile_orphaned_jobs(&pool).await.unwrap(), 1);
        assert_eq!(job_status(&pool, unreadable).await, STATUS_INTERRUPTED);
        assert!(
            feed_kinds(&pool)
                .await
                .contains(&"job_interrupted".to_owned())
        );

        // A job that never recorded a HEAD cannot prove anything either.
        let unrecorded = seed_job_in(&pool, "project-b", "implementing", &root, None)
            .await
            .unwrap();
        reconcile_orphaned_jobs(&pool).await.unwrap();
        assert_eq!(job_status(&pool, unrecorded).await, STATUS_INTERRUPTED);
    }

    #[tokio::test]
    async fn a_finished_job_is_not_reconsidered_after_a_restart() {
        let pool = test_pool().await;
        let done = seed_job(&pool, "project-a", "completed").await.unwrap();

        assert_eq!(reconcile_orphaned_jobs(&pool).await.unwrap(), 0);
        assert_eq!(job_status(&pool, done).await, "completed");
    }

    // ---- what each node is told ----------------------------------------------------------------

    /// The plan node's whole contract, and the one place an empty queue has to be named as a
    /// legitimate answer — otherwise a model asked to plan will find something to plan.
    #[test]
    fn the_plan_node_is_told_the_file_is_the_only_thing_read() {
        let prompt = plan_prompt("advance the backlog", 5, "/wt/.nucleos");
        assert!(prompt.contains("advance the backlog"));
        assert!(prompt.contains("/wt/.nucleos/plan.json"));
        assert!(prompt.contains("at most 5"));
        assert!(prompt.contains(r#"{"items": []}"#));
    }

    /// The hint is asked for, and asked for as optional. A planner told to name files without being
    /// told it may decline will name some for every item, and a confident wrong list is worse than
    /// no list: the implement node is told to start there, so an invented path spends a whole
    /// context window in the wrong place.
    #[test]
    fn plan_prompt_asks_for_optional_file_hints() {
        let prompt = plan_prompt("advance the backlog", 5, "/wt/.nucleos");

        assert!(
            prompt.contains(r#""files""#),
            "the shape must name the field: {prompt}"
        );
        let lowered = prompt.to_lowercase();
        assert!(
            lowered.contains("optional") || lowered.contains("if you"),
            "naming files must be offered, not required: {prompt}"
        );
        assert!(
            lowered.contains("best effort")
                || lowered.contains("best-effort")
                || lowered.contains("guess"),
            "the planner must be told a partial answer is acceptable: {prompt}"
        );
    }

    /// Two halves of one contract. With a hint the node is told where to start *and* told the list
    /// is not a boundary — an implement node that treats a planner's guess as the full extent of the
    /// change leaves the tree half-edited. With no hint the prompt is byte-for-byte what it has
    /// always been, so a job planned before this existed is not silently given a different brief.
    #[test]
    fn implement_prompt_names_the_hinted_files_as_a_possibly_incomplete_list() {
        let hints = [
            "core/src/job.rs".to_owned(),
            "core/migrations/0052.sql".to_owned(),
        ];
        let hinted = implement_prompt("write the thing", 0, 3, "/wt/.nucleos", &hints, None);

        assert!(hinted.contains("core/src/job.rs"));
        assert!(hinted.contains("core/migrations/0052.sql"));
        assert!(
            hinted.to_lowercase().contains("incomplete"),
            "the list is a hint, not a boundary: {hinted}"
        );

        let bare = implement_prompt("write the thing", 0, 3, "/wt/.nucleos", &[], None);
        assert_eq!(
            bare,
            "You are item 1 of 3 in an autonomous job. The working tree already holds the work \
             of the earlier items; this is the only one you do.\n\n\
             write the thing\n\n\
             The full queue is in /wt/.nucleos/plan.json for context. Do not start another item \
             and do not edit that file. Your work is verified after you finish, so leave the tree \
             building. Leave it UNCOMMITTED: the job commits for you once the gate agrees, and \
             committing by hand stops this item to ask permission for something already arranged."
        );
        // Said twice on purpose: the equality above is the guarantee, and this says what it is a
        // guarantee *of* — an unhinted node is never told about a hint mechanism it has no hint for,
        // and never invited to wonder which files were meant.
        let lowered = bare.to_lowercase();
        assert!(
            !lowered.contains("hint"),
            "no hint, no hint paragraph: {bare}"
        );
        assert!(
            !lowered.contains("incomplete"),
            "nothing to be incomplete: {bare}"
        );
        assert!(
            !lowered.contains("start with"),
            "no files to start with: {bare}"
        );

        // The paragraph is appended, so everything the node was told before it is still there.
        assert!(hinted.starts_with(&bare));
    }

    /// A retry is only worth a run if the node is told what the gate said.
    ///
    /// §5.4 keeps nodes from resuming each other's sessions on purpose, so the second implement node
    /// knows nothing about the first — including that there WAS a first. Without the gate's output
    /// travelling in the prompt, a retry is a second independent guess at the same item, which is a
    /// whole run spent to rediscover what the gate already printed. That is the difference between
    /// a retry and repeating yourself.
    ///
    /// The other half is the `None` call, and it is the half that protects every job that is not
    /// retrying: an item on its first attempt is given the brief it has always been given, with
    /// nothing in it about a previous gate it never had. The byte-for-byte guarantee lives in
    /// `implement_prompt_names_the_hinted_files_as_a_possibly_incomplete_list` above, which pins the
    /// unhinted prompt as a literal; here the claim is that the output is APPENDED, so everything
    /// the node was told before it is still there and in the same place.
    #[test]
    fn the_retry_prompt_carries_the_gate_output() {
        let first = implement_prompt("write the thing", 0, 3, "/wt/.nucleos", &[], None);
        let retry = implement_prompt(
            "write the thing",
            0,
            3,
            "/wt/.nucleos",
            &[],
            Some("FAILED test_x"),
        );

        assert!(
            retry.contains("FAILED test_x"),
            "the retrying node was not told what failed: {retry}"
        );
        assert!(
            !first.contains("FAILED test_x"),
            "a first attempt must carry no account of a gate it never faced: {first}"
        );
        assert!(
            retry.starts_with(&first),
            "the gate's output is appended to the brief, not substituted for part of it: {retry}"
        );
    }

    /// §5.4: the review node's independence is structural, not requested. It is never given a
    /// builder's session because no builder session is kept for it to resume.
    #[test]
    fn the_review_node_is_given_a_diff_and_no_reasoning() {
        let with_base = review_prompt(Some("deadbeef"), "/wt/.nucleos");
        assert!(with_base.contains("git diff deadbeef..HEAD"));

        // No recorded base: git would not answer when the job started. Asking for the branch's own
        // commits is worse than naming a sha and better than reviewing a guess.
        let without = review_prompt(None, "/wt/.nucleos");
        assert!(without.contains("git log --oneline"));
    }

    /// The words an owner would leave on a job in flight, and the item they arrive beside.
    ///
    /// Distinctive strings on purpose. `seed_items` writes `'an item'` for every description, which
    /// appears inside `implement_prompt`'s own boilerplate often enough that asserting on it would
    /// pass whether or not the item's brief survived.
    const A_NOTE: &str = "when you get to item 3, update the docs too";
    const AN_ITEM: &str = "rename the cursor helper";

    /// The one seam a job's words have to cross, driven end to end.
    ///
    /// Every other test of this feature stops inside `notes`, which is pure or nearly so and which
    /// will happily store and return a note nobody ever reads. The wiring is the part that can be
    /// missing while all of that passes: `advance` selects the item, builds the brief and hands it
    /// to `spawn_node`, and unless the pending notes are appended THERE the owner's sentence is a
    /// row in a table with no reader. A node is born with a clean context window and no steering
    /// channel — `create_job_node_run` passes `steerable: false` on purpose — so the prompt is the
    /// only door, and this test opens it from the outside.
    ///
    /// It asserts on the STORED prompt, read back out of `runs`, and not on a string the test built.
    /// The chain is long — note left, queue read, text rendered, brief appended, run created, row
    /// written — and a test that composed those pieces itself would be checking its own arithmetic
    /// while any one of the links could be missing.
    ///
    /// The item's own description is asserted for beside it, because "the note reached the prompt"
    /// is satisfied by a `render` that returned the note INSTEAD of the brief. A node whose item
    /// vanished would go and do what the note said and nothing else, which is the failure the whole
    /// wording of `render` is written to avoid.
    ///
    /// And the queue is asserted empty afterwards. Delivery is what makes a note a message rather
    /// than a standing order: unconsumed, one sentence typed at midnight would be appended to item
    /// 4, item 5, the replan and the review, each of them reading it as something newly said about
    /// the work in front of it.
    ///
    /// A real repository and a real worktree, for the reason
    /// `a_retriable_item_is_claimed_and_started_not_merely_asked_for` gives: a node that could not
    /// be provisioned leaves the item exactly where a node that was never told about the note would,
    /// and the two would be indistinguishable here.
    #[tokio::test(flavor = "current_thread")]
    async fn a_note_left_on_a_job_reaches_the_next_nodes_prompt() {
        let _lock = crate::worktree::test_env_lock();
        let (_container, repo) = walkable_repo("nucleos-job-note-", "git --version");
        let root = tempfile::tempdir().expect("worktree root");
        let _env = WorktreeRootEnv::set(root.path());
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;

        let job_id = seed_job_in(
            &pool,
            "project-a",
            "implementing",
            &repo.to_string_lossy(),
            None,
        )
        .await
        .unwrap();
        let owner = crate::worktree::Owner::Job(job_id);
        let info = crate::worktree::create(&repo, owner)
            .await
            .expect("provision the job's worktree");
        crate::worktree::record(
            &pool,
            owner,
            "project-a",
            &repo.to_string_lossy(),
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job's worktree");
        seed_items(&pool, job_id, &["pending"]).await;
        sqlx::query("UPDATE job_items SET description = ? WHERE job_id = ? AND ordinal = 0")
            .bind(AN_ITEM)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();

        let note_id = crate::notes::leave(&pool, job_id, A_NOTE, "duarte")
            .await
            .expect("the owner leaves a note on a job already running");

        let job = load_job(&pool, job_id).await.unwrap();
        advance(&state, &job, Utc::now()).await;

        let run_id: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let run_id = run_id.expect("the item's node started, so there is a prompt to look at");
        let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert!(
            prompt.contains(A_NOTE),
            "the note never reached the node it was left for: {prompt}"
        );
        assert!(
            prompt.contains(AN_ITEM),
            "the note took the place of the item's brief instead of being added to it: {prompt}"
        );
        assert_eq!(
            crate::notes::pending(&pool, job_id)
                .await
                .unwrap()
                .iter()
                .map(|note| note.id)
                .collect::<Vec<_>>(),
            Vec::<i64>::new(),
            "note {note_id} is still queued after being read out, so every later node of this job \
             gets told the same thing again"
        );
    }

    /// A node that never started was told nothing, and the note has to still be waiting.
    ///
    /// `spawn_node` has an exit before the run exists, and it is not an exotic one: the item claim
    /// is a compare-and-swap that stops two passes starting two nodes on one tree, and losing it is
    /// the ordinary outcome of a second pass arriving while the first is still working. The `Busy`
    /// and provisioning-failure arms below it end the same way — no run, no prompt, nobody told.
    ///
    /// So the order inside `spawn_node` is the behaviour: read the queue, render, append, create the
    /// run, and only THEN mark the notes delivered. A `mark_delivered` at the top reads as harmless
    /// — the words were rendered, after all — and loses the owner's sentence in exactly the case
    /// nothing reports. The run is never created, the prompt is thrown away, the note is gone from
    /// the queue, and the owner learns about it by watching the job finish without doing what they
    /// asked.
    ///
    /// `spawn_node` is called directly rather than through `advance`, because `advance` derives the
    /// status it claims against from the view it just loaded and therefore cannot lose the swap
    /// inside one pass. Driving the seam itself is the only way to stand in the moment this is about.
    /// The run count is asserted as well as the note, because "no node started" is this test's
    /// premise and a premise the test assumed rather than checked would make the rest of it vacuous.
    #[tokio::test]
    async fn a_note_is_not_consumed_by_a_node_that_failed_to_start() {
        let pool = test_pool().await;
        let (state, _runner) = test_state_with_runner(pool.clone()).await;
        let job_id = seed_job(&pool, "project-a", "implementing").await.unwrap();
        // The race the compare-and-swap exists to lose: another pass claimed this item first, so its
        // row already says `running` and the claim below — which expects `pending`, the status the
        // caller last saw — matches nothing.
        seed_items(&pool, job_id, &["running"]).await;

        let note_id = crate::notes::leave(&pool, job_id, A_NOTE, "duarte")
            .await
            .expect("the owner leaves a note on a job already running");

        let job = load_job(&pool, job_id).await.unwrap();
        let step = spawn_node(
            &state,
            &job,
            "implement",
            "the brief this node would have been given".to_owned(),
            Some(ItemClaim {
                ordinal: 0,
                held: "pending",
            }),
            (PathBuf::from("/project/a/worktree"), "nucleos/job".into()),
        )
        .await;

        assert_eq!(
            step,
            Step::Stopped,
            "an item that could not be claimed has to end the pass"
        );
        let started: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE job_id = ?")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            started, 0,
            "the premise of this test is that no node started, and one did"
        );

        assert_eq!(
            crate::notes::pending(&pool, job_id)
                .await
                .unwrap()
                .iter()
                .map(|note| note.id)
                .collect::<Vec<_>>(),
            vec![note_id],
            "the note was spent on a node that never read it, and nothing will say so"
        );
    }
}
