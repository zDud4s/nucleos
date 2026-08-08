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
}

/// A validated work queue, plus however much of it did not fit.
#[derive(Debug, PartialEq, Eq)]
pub struct PlannedItems {
    pub items: Vec<String>,
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
    let items: Vec<String> = parsed
        .items
        .into_iter()
        .take(max_items)
        .map(|item| item.description)
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
    GateErrored,
    /// This item asked for a decision, so the job put it down and moved on.
    ///
    /// Not a failure and not a cancellation: nothing broke, and nobody stopped anything. The work
    /// was never attempted, the tree was reverted to where the item started, and a `skipped-item`
    /// proposal carries what would be needed to pick it up. `next_step` walks past it the way it
    /// walks past `Passed`, because the queue owes it nothing more.
    Skipped,
}

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
    for item in &job.items {
        match item {
            ItemState::Failed => return Next::Finish(Outcome::Failed),
            ItemState::Cancelled => return Next::Finish(Outcome::Cancelled),
            ItemState::GateErrored => return Next::Finish(Outcome::GateErrored),
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
    if job.items.contains(&ItemState::Running) {
        return Next::Wait;
    }
    // Gate before starting the next item: on a shared worktree, letting item i+1 build on unmeasured
    // work means a later red gate cannot say which item broke it.
    if let Some(ordinal) = job
        .items
        .iter()
        .position(|item| *item == ItemState::Implemented)
    {
        return Next::RunGate { ordinal };
    }
    if let Some(ordinal) = job
        .items
        .iter()
        .position(|item| *item == ItemState::Pending)
    {
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
    // A red gate ends the job whatever the rounds say. The branch carries work the gate rejected,
    // and another round would build on top of it — which is the one thing the per-item revert exists
    // to stop happening WITHIN a round, and it does not stop being true across them.
    if job.items.contains(&ItemState::GateFailed) {
        return Next::Finish(Outcome::GateFailed);
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
/// One red gate anywhere makes the job `gate_failed`, however many items passed after it. The
/// verdict is about the branch that is handed back, and a branch carrying an item the gate rejected
/// is not one somebody should be told is complete.
///
/// Skipped items deliberately do NOT show up here. They are work that was never attempted, recorded
/// as proposals for a person to decide on; a job that ran everything it was allowed to run did what
/// was asked of it, and reporting that as a failure would teach the reader to ignore the word.
fn ending(job: &JobView) -> Outcome {
    if job.items.contains(&ItemState::GateFailed) {
        Outcome::GateFailed
    } else {
        Outcome::Completed
    }
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

fn item_state_from(status: &str) -> ItemState {
    match status {
        "running" => ItemState::Running,
        "implemented" => ItemState::Implemented,
        "passed" => ItemState::Passed,
        "failed" => ItemState::Failed,
        STATUS_CANCELLED => ItemState::Cancelled,
        "gate_failed" => ItemState::GateFailed,
        "gate_errored" => ItemState::GateErrored,
        STATUS_SKIPPED => ItemState::Skipped,
        // An unrecognised item status is treated as still to do rather than as done. Erring toward
        // "not finished" costs a repeated item; erring the other way silently skips work the job
        // was created to perform and reports it complete.
        _ => ItemState::Pending,
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
    let (status, resume_status, review_wanted): (String, Option<String>, i64) =
        sqlx::query_as("SELECT status, resume_status, review FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await?;
    let stage = effective_status(&status, resume_status.as_deref());

    let (round, dry_rounds, max_rounds, replan_done): (i64, i64, Option<i64>, i64) =
        sqlx::query_as("SELECT round, dry_rounds, max_rounds, replan_done FROM jobs WHERE id = ?")
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
    let items: Vec<String> =
        sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
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
    let round_opened_at = latest_replan.as_ref().map_or(0, |(id, _)| *id);
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
        items: items.iter().map(|s| item_state_from(s)).collect(),
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
/// Fails when the project already has a live one. That refusal is the unique index
/// `one_live_job_per_project` (migration 0042) rather than a check here, deliberately: with the
/// constraint in the storage layer the INSERT itself is the lock, so a scheduler tick and a manual
/// request racing for the same project cannot both pass a check and then both proceed. It mirrors
/// what `one_open_worktree_run_per_project` already does for runs.
pub async fn insert_job(pool: &SqlitePool, job: &NewJob<'_>) -> sqlx::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO jobs
           (project_id, project_root, rule_name, prompt, status, max_items, gate_each, review,
            head_sha, max_rounds, budget_usd, created_at)
         VALUES (?, ?, ?, ?, 'planning', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(job.project_id)
    .bind(job.project_root)
    .bind(job.rule_name)
    .bind(job.prompt)
    .bind(job.max_items)
    .bind(i64::from(job.gate_each))
    .bind(i64::from(job.review))
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
/// `AlreadyLive` is not a check that failed here — it is `one_live_job_per_project` (migration
/// 0042) refusing the INSERT. With the constraint in the storage layer the INSERT *is* the lock, so
/// a scheduler tick and a manual request racing for the same project cannot both pass a check and
/// then both proceed. It mirrors what `one_open_worktree_run_per_project` does for runs.
pub enum JobStart {
    Started(i64),
    AlreadyLive,
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
            head_sha: request.head_sha,
            max_rounds: request.max_rounds,
            budget_usd: request.budget_usd,
        },
    )
    .await
    {
        Ok(job_id) => job_id,
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|database_error| database_error.is_unique_violation()) =>
        {
            return JobStart::AlreadyLive;
        }
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
        Ok(crate::concurrency::ClaimOutcome::ProjectFull { limit }) => {
            return fail_early(
                state,
                request.project_id,
                job_id,
                &format!("this project already has {limit} pieces of work in flight"),
            )
            .await;
        }
        Ok(crate::concurrency::ClaimOutcome::HouseFull { limit }) => {
            return fail_early(
                state,
                request.project_id,
                job_id,
                &format!("the machine already has {limit} pieces of work in flight"),
            )
            .await;
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
/// A job left live with no worktree would be ticked forever and hold `one_live_job_per_project`,
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

/// The statuses `one_live_job_per_project` covers, and therefore the ones a tick has to drive.
///
/// Kept beside the SQL that reads it rather than spelled out at each call site, because a status
/// that falls out of this list stops being ticked while still holding the project's exclusivity
/// slot: the project would go quiet with no error anywhere, until somebody opened the database.
/// `a_live_status_the_index_covers_is_also_a_status_the_tick_drives` pins it to the migration.
pub const LIVE_STATUSES: [&str; 6] = [
    "planning",
    "implementing",
    "gating",
    "reviewing",
    "awaiting_approval",
    "waiting",
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
         {{\"items\": [{{\"description\": \"...\"}}]}}\n\n\
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
         The task:\n\n{task}"
    )
}

/// The prompt one implement node is given.
///
/// It gets the item and the queue, and no account of how the previous node reasoned — §5.4 of the
/// design makes each node's independence structural rather than requested, by never keeping a
/// session another node could resume.
pub fn implement_prompt(
    description: &str,
    ordinal: usize,
    total: usize,
    artifacts: &str,
) -> String {
    format!(
        "You are item {} of {total} in an autonomous job. The working tree already holds the work \
         of the earlier items; this is the only one you do.\n\n\
         {description}\n\n\
         The full queue is in {artifacts}/plan.json for context. Do not start another item and do \
         not edit that file. Your work is verified after you finish, so leave the tree building. \
         Leave it UNCOMMITTED: the job commits for you once the gate agrees, and committing by hand \
         stops this item to ask permission for something already arranged.",
        ordinal + 1
    )
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
         wrong, what is missing against that queue, and nothing else. Change no files."
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

    for (ordinal, description) in planned.items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status)
             VALUES (?, ?, ?, 'pending')",
        )
        .bind(job.id)
        .bind(ordinal as i64)
        .bind(description)
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
/// project's exclusivity slot through `one_live_job_per_project` without that index needing to learn
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

    for (offset, description) in planned.items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, round)
             VALUES (?, ?, ?, 'pending', ?)",
        )
        .bind(job.id)
        .bind(next_ordinal + offset as i64)
        .bind(description)
        .bind(round)
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
async fn spawn_node(
    state: &AppState,
    job: &JobRow,
    stage: &'static str,
    prompt: String,
    item: Option<usize>,
    worktree: (PathBuf, String),
) -> Step {
    let pool = &state.pool;

    // Claimed BEFORE the run exists, so a creation that succeeds and a bookkeeping write that fails
    // cannot leave an item looking untouched while a node works on it — the next pass would start a
    // second node on the same tree.
    if let Some(ordinal) = item {
        let claimed = sqlx::query(
            "UPDATE job_items SET status = 'running'
             WHERE job_id = ? AND ordinal = ? AND status = 'pending'",
        )
        .bind(job.id)
        .bind(ordinal as i64)
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
    let created = crate::runs::create_job_node_run(
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
    .await;

    match created {
        Ok(run_id) => {
            if let Some(ordinal) = item {
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

async fn release_item(pool: &SqlitePool, job: &JobRow, item: Option<usize>) {
    let Some(ordinal) = item else { return };
    let _ = sqlx::query(
        "UPDATE job_items SET status = 'pending' WHERE job_id = ? AND ordinal = ? AND status = 'running'",
    )
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
pub async fn footing_for_run(pool: &SqlitePool, job_id: i64, run_id: i64) -> Option<String> {
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
pub async fn job_worktree_path(pool: &SqlitePool, job_id: i64) -> Option<PathBuf> {
    job_worktree(pool, job_id)
        .await
        .ok()
        .flatten()
        .map(|(path, _)| path)
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
            footing_for(pool, job, ordinal).await,
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

    match brakes(state, job, now).await {
        Brake::Go => {}
        Brake::Park { reason, detail } => return park(state, job, reason, &detail).await,
        Brake::Stop { detail } => {
            let _ = retire(pool, job.id, STATUS_STOPPED).await;
            say(
                pool,
                job,
                "job_stopped",
                &format!(
                    "job {} stopped with {} item(s) unfinished: {detail}",
                    job.id,
                    view.items
                        .iter()
                        .filter(|item| **item == ItemState::Pending)
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
            let description: Option<String> = sqlx::query_scalar(
                "SELECT description FROM job_items WHERE job_id = ? AND ordinal = ?",
            )
            .bind(job.id)
            .bind(ordinal as i64)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            let Some(description) = description else {
                tracing::warn!(job_id = job.id, ordinal, "a job item lost its description");
                return Step::Stopped;
            };
            let prompt = implement_prompt(&description, ordinal, view.items.len(), &artifacts);
            spawn_node(state, job, "implement", prompt, Some(ordinal), worktree).await
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
        Next::Wait | Next::Finish(_) | Next::RunGate { .. } => Step::Stopped,
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

    let jobs = match live_jobs(&state.pool).await {
        Ok(jobs) => jobs,
        Err(error) => {
            tracing::warn!(%error, "could not load live jobs");
            return;
        }
    };
    for job in jobs {
        drive(state, job, now).await;
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
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
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
}

const ONE_SUMMARY_SQL: &str =
    "SELECT id, project_id, rule_name, status, wait_reason, max_items, round, max_rounds,
        created_at, completed_at
     FROM jobs WHERE id = ?";

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
                "SELECT id, project_id, rule_name, status, wait_reason, max_items, round,
                    max_rounds, created_at, completed_at
             FROM jobs WHERE project_id = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, project_id, rule_name, status, wait_reason, max_items, round,
                    max_rounds, created_at, completed_at
             FROM jobs ORDER BY id DESC LIMIT ?",
            )
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

    Ok(Some(JobDetail { job, items, branch }))
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
/// that owned it was gone — holding `one_open_worktree_run_per_project` and blocking every later
/// worktree run of that project until a person noticed.
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
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
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
        // project id would collide on `one_open_worktree_run_per_project`, which is migration 0009
        // doing exactly its job and has nothing to do with what the test is asking.
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
        let implement = implement_prompt("write shout.py", 0, 4, "/wt/.nucleos");

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
        // Reported, never silent: a queue quietly cut from seven to five reads downstream as "the
        // planner found five things", which is a different and wrong statement about the work.
        assert_eq!(plan.dropped, 2);
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

    #[tokio::test]
    async fn a_second_live_job_for_a_project_is_rejected_by_the_index() {
        let pool = test_pool().await;
        seed_job(&pool, "project-a", "planning")
            .await
            .expect("the first job starts");

        let second = seed_job(&pool, "project-a", "planning").await;

        // Rejected by the unique index rather than by a check in this module: the INSERT is the
        // lock, so the scheduler tick and a manual POST racing for the same project cannot both win.
        assert!(second.is_err(), "a project may have only one live job");
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

    /// The constant the tick iterates and the index the migration created have to name the same
    /// statuses. A status that only the index knows about holds the project's exclusivity slot
    /// while nothing drives it: the project goes quiet, with no error anywhere, until somebody
    /// opens the database.
    #[tokio::test]
    async fn a_live_status_the_index_covers_is_also_a_status_the_tick_drives() {
        let pool = test_pool().await;
        let index: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE name = 'one_live_job_per_project'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        for status in LIVE_STATUSES {
            assert!(
                index.contains(&format!("'{status}'")),
                "the tick drives `{status}` but the index does not hold the slot for it"
            );
            assert!(
                LIVE_JOBS_SQL.contains(&format!("'{status}'")),
                "`{status}` is live but no pass would ever load it"
            );
        }
        // And the other direction, which is the dangerous one.
        let covered = index
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            covered,
            LIVE_STATUSES.len(),
            "the index covers a status the tick does not drive: {index}"
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
    /// the job that owned it was gone, holding `one_open_worktree_run_per_project` and blocking
    /// every later worktree run of that project.
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
}
