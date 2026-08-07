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
    items: Vec<PlanItem>,
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
    /// A node is in flight; nothing to do until it lands.
    Wait,
    Finish(Outcome),
}

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
    pub items: Vec<ItemState>,
    pub review: ReviewState,
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
    for item in &job.items {
        match item {
            ItemState::Failed => return Next::Finish(Outcome::Failed),
            ItemState::Cancelled => return Next::Finish(Outcome::Cancelled),
            ItemState::GateFailed => return Next::Finish(Outcome::GateFailed),
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

    match job.review {
        ReviewState::Pending => Next::SpawnReview,
        ReviewState::Running => Next::Wait,
        ReviewState::NotWanted | ReviewState::Done => Next::Finish(Outcome::Completed),
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
    let plan_run = latest_node("plan").await?;
    let review_run = latest_node("review").await?;

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
pub async fn resume(pool: &SqlitePool, job_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE jobs
         SET status = COALESCE(resume_status, 'planning'), resume_status = NULL
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
            head_sha, created_at)
         VALUES (?, ?, ?, ?, 'planning', ?, ?, ?, ?, ?)",
    )
    .bind(job.project_id)
    .bind(job.project_root)
    .bind(job.rule_name)
    .bind(job.prompt)
    .bind(job.max_items)
    .bind(i64::from(job.gate_each))
    .bind(i64::from(job.review))
    .bind(job.head_sha)
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
    /// Reserved for Chunk 3. Accepted and **ignored** here rather than rejected: a field the client
    /// sends and the server refuses is a client that has to be rewritten the day the server learns
    /// it, and the client is a tool description a model reads.
    ///
    /// `expect` rather than `allow`, on purpose. The day Chunk 3 reads either field the lint stops
    /// firing and this attribute becomes an error, which is the compiler asking for it back. An
    /// `allow` would sit here silently covering whatever went dead next.
    #[expect(
        dead_code,
        reason = "Chunk 3 gives budget_usd and max_rounds meaning; delete this attribute then"
    )]
    pub budget_usd: Option<f64>,
    #[expect(
        dead_code,
        reason = "Chunk 3 gives budget_usd and max_rounds meaning; delete this attribute then"
    )]
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
                                    wait_reason, max_items, gate_each, head_sha, created_at
                             FROM jobs
                             WHERE status IN ('planning','implementing','gating','reviewing',
                                              'awaiting_approval','waiting')
                             ORDER BY id";

const ONE_JOB_SQL: &str = "SELECT id, project_id, project_root, prompt, status, resume_status,
                                  wait_reason, max_items, gate_each, head_sha, created_at
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
         Write them to {artifacts}/plan.json and change nothing else:\n\n\
         {{\"items\": [{{\"description\": \"...\"}}]}}\n\n\
         That file is the only thing that is read; anything you print is discarded. If there is no \
         work to do, write {{\"items\": []}} — an empty queue is a legitimate answer and is not a \
         failure. Do not begin any of the work yourself.\n\n\
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
         not edit that file. Your work is verified after you finish, so leave the tree building.",
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
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// One item of a job's queue, as the shell shows it.
#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct JobItemView {
    pub ordinal: i64,
    pub description: String,
    pub status: String,
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
    "SELECT id, project_id, rule_name, status, wait_reason, max_items, created_at, completed_at
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
                "SELECT id, project_id, rule_name, status, wait_reason, max_items, created_at,
                    completed_at
             FROM jobs WHERE project_id = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as(
                "SELECT id, project_id, rule_name, status, wait_reason, max_items, created_at,
                    completed_at
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
        "SELECT ordinal, description, status, run_id, gate_status
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

    let in_flight: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM runs WHERE job_id = ? AND status IN ('running','awaiting_approval')",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    for run_id in in_flight {
        crate::runs::finalize_termination(state, run_id, STATUS_CANCELLED).await;
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

    fn view(planned: bool, items: &[ItemState], review: ReviewState) -> JobView {
        JobView {
            planned,
            planning: false,
            items: items.to_vec(),
            review,
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

    #[test]
    fn a_failed_gate_and_an_errored_gate_end_the_job_differently() {
        // The distinction gate.rs guards and §7 of the spec insists must survive the trip up to the
        // job: a non-zero exit says the code is broken, a binary that would not start says the
        // measurement never happened. One of those is a verdict; the other is silence.
        assert_eq!(
            next_step(&view(true, &[ItemState::GateFailed], ReviewState::Pending)),
            Next::Finish(Outcome::GateFailed)
        );
        assert_eq!(
            next_step(&view(true, &[ItemState::GateErrored], ReviewState::Pending)),
            Next::Finish(Outcome::GateErrored)
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
                head_sha: None,
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

    /// Decision 6, walked rather than asserted against a seeded row: a red gate stops the chain
    /// where it broke, and the item after it never starts. On a shared worktree, letting item i+1
    /// build on unmeasured work is exactly what makes a later red gate unable to say which item
    /// caused it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_red_gate_stops_the_chain_before_the_next_item_starts() {
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

        assert_eq!(walk(&state, job_id).await, "gate_failed");

        let items: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, gate_status FROM job_items WHERE job_id = ? ORDER BY ordinal",
        )
        .bind(job_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(items[0].0, "gate_failed");
        assert_eq!(items[0].1.as_deref(), Some("failed"));
        // Never started. The partial stays on the branch and the second item is still there to do.
        assert_eq!(items[1].0, "pending");
        let stages: Vec<String> =
            sqlx::query_scalar("SELECT stage FROM runs WHERE job_id = ? ORDER BY id")
                .bind(job_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            stages,
            vec!["plan", "implement"],
            "no review is spent on a chain that already stopped"
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

        assert_eq!(
            next_step(&load_view(&pool, broken).await.unwrap()),
            Next::Finish(Outcome::GateFailed)
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
