//! Departments: a director agent that plans, specialists that answer, and a folder that is the
//! delivery.
//!
//! This module decides the SHAPE of the work — the roster, the rounds, the replanning, the folder
//! index — and nothing that executes it. How one talks to a model is `runner.rs` and
//! `local_agent.rs`, when there is money to spend is `budget.rs`, who is who is `auth.rs`, what a
//! tool does to the turn that called it is `hooks.rs` and `mcp_tools.rs`, and how a path resolves
//! inside the managed root is `files.rs`.
//!
//! **Everything a run needs to be resumed is in the database, and nothing is in memory.** A pass is
//! `team_tick`, which reads the live runs and moves each one as far as it can this second; a
//! subprocess landing is discovered by reading its `runs` row, not by awaiting a task. That is what
//! makes a daemon restart mid-round a non-event rather than a recovery procedure — the same tick
//! that would have ingested the answer ingests it after the restart, and `reconcile_orphaned_team_runs`
//! only has to deal with the runs that genuinely died.
//!
//! The loop is its own rather than the scheduler's, and the reason is this pillar's rather than
//! borrowed: a pass launches up to `max_parallel` subprocesses and writes files at a cadence
//! nothing else in the house shares.

mod actions;
mod api;
mod config;
mod ingest;
mod launch;
mod lifecycle;
mod messages;
mod plan;
mod prompts;
mod recruit;
mod rounds;
pub mod trigger;

pub use actions::*;
pub use api::*;
pub use config::*;
pub use lifecycle::*;
pub use messages::*;
pub use plan::*;
pub use recruit::*;
pub use rounds::*;

use actions::validate_payload;
#[cfg(test)]
use axum::Json;
#[cfg(test)]
use axum::extract::{Path, State};
#[cfg(test)]
use axum::http::StatusCode;
use config::slug;
use ingest::*;
use launch::*;
use prompts::*;
use rounds::{round_width, run_capacity};

use crate::state::AppState;

/// The `runs.mode` every invocation of a department carries — director and specialist alike.
///
/// One mode and not two, because everything that reads it asks the same question and wants the same
/// answer: the budget counts it, the hook refuses it every acting tool, the policy tables call it
/// unattended. Which of the two roles a run played is a question `team_items` answers, and only the
/// engine ever asks it.
pub const TEAM_MODE: &str = "team";

/// The states in which a team run is still going somewhere.
///
/// Three readers need this answer and none of them may keep its own copy: the token's death rule
/// in `auth::resolve` (a key that outlives its run is a working key in a log file), the folder GC,
/// and startup reconciliation. `job.rs` carries `TERMINAL_STATUSES`/`LIVE_STATUSES` for the same
/// reason, and `concurrency.rs` pays for a test — `every_live_status_is_a_status_the_sweep_spares`
/// — precisely because three copies of a list like this drift apart one edit at a time.
pub const LIVE_STATES: &[&str] = &["planning", "working", "delivering"];

/// Where a team run stops, and it stops in five distinguishable ways.
///
/// `stopped` and `expired` are deliberately not `failed`. Nothing failed — a ceiling was reached —
/// and an owner shown `failed` goes looking for an error that does not exist. The job graph draws
/// the same distinction for the same reason.
pub const TERMINAL_STATES: &[&str] = &["done", "stopped", "expired", "failed", "cancelled"];

/// How often a pass runs. Slower than the job loop's 30s would be wrong in the other direction: a
/// department's rounds are short, and a whole round of specialists can land between two ticks.
const TEAM_TICK: std::time::Duration = std::time::Duration::from_secs(10);

/// How often a pass runs while the previous one found no live run and no due action. Starting a run
/// rings [`TEAM_WAKE`], so only an approval granted to an already-finished run waits this long.
const TEAM_IDLE_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// Rung when a run is started, to cut the idle wait short.
static TEAM_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Ceilings the daemon imposes on what a team may declare, whatever the owner types.
///
/// The roster is static and written by the owner (design §1.1), so what is left to bound is how far
/// the director may run with it: rounds times parallelism is the multiplier on the bill.
pub const MAX_ROUNDS_CEILING: i64 = 6;
pub const MAX_PARALLEL_CEILING: i64 = 8;

/// How many items one plan may queue.
///
/// Cut items are REPORTED and not dropped in silence, for the reason `job::PlannedItems::dropped`
/// records: a queue quietly truncated reads downstream as the whole of what the planner found.
const MAX_ITEMS_PER_ROUND: usize = 8;

/// Two consecutive rounds that add nothing end the work.
///
/// One would be wrong: a replan can legitimately produce nothing while the previous round settles.
/// Counting items up to a target is worse still — it never finds the tail, and a model that never
/// says "finished" would run to `max_rounds` spending. The brake is "it dried up", not "it reached
/// a number", and the reasoning is `job::DRY_ROUNDS_TO_STOP`'s.
const DRY_ROUNDS_TO_STOP: i64 = 2;

/// How many times an unreadable plan relaunches the planner in round 0.
///
/// Only in round 0, and that asymmetry is the design: in `planning` there is no partial work to
/// deliver, so giving up would hand the owner nothing at all. In a later round there IS work, and
/// an unreadable plan is just a dry round.
const PLAN_RETRY_LIMIT: i64 = 1;

/// Four hours, as `MAX_JOB_LIFETIME` is. A department that has been going this long has stopped
/// answering the request the owner recognises.
const MAX_TEAM_RUN_LIFETIME: chrono::Duration = chrono::Duration::hours(4);

/// How long a finished run's folder survives.
///
/// A constant and not a setting, unlike `web::prune`'s retention: that one is a preference about
/// how much of the world to keep, this one is about how long the owner has to read their own
/// delivery, and a number nobody needs to tune should not have a place to tune it.
const TEAM_WORKSPACE_RETENTION_DAYS: i64 = 30;

/// How often the folder GC sweeps. Daily: the thing it collects is measured in days.
const GC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// What the owner opens. Named in the delivery prompt, and the one file the core writes on the
/// director's behalf.
const DELIVERY_FILE: &str = "entrega.md";

/// Where this daemon is reached. A function and not a `const`, because the port is no longer a
/// compile-time fact: a second instance binds its own (`daemon_client::PORT_VAR`), and a const
/// would send its runs to whichever daemon happens to hold the default.
fn daemon_url() -> String {
    crate::daemon_client::daemon_url()
}

/// PURE: whether a run in this state is still one the daemon is advancing.
pub fn is_live(state: &str) -> bool {
    LIVE_STATES.contains(&state)
}

/// PURE: where one run's folder lives, relative to the managed files root.
///
/// Relative and never absolute, because it is stored: a root that moves — a machine restored from
/// backup, a data directory relocated — must not invalidate every folder written before the move.
pub fn workspace_for(team_id: &str, team_run_id: &str) -> String {
    format!("teams/{team_id}/{team_run_id}")
}

// ---------------------------------------------------------------------------------------------
// The catalogue of teams
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, serde::Serialize, sqlx::FromRow)]
pub struct Team {
    pub id: String,
    pub name: String,
    /// What the department does. Enters the director's prompt in every round — it is the standing
    /// half of the instruction, where `request` is the occasional half.
    pub mission: String,
    pub director_agent_id: String,
    pub max_rounds: i64,
    pub max_parallel: i64,
    pub budget_usd: Option<f64>,
    /// How many actions this team may leave waiting for a human at once. See
    /// `DEFAULT_MAX_OPEN_ACTIONS`.
    pub max_open_actions: i64,
    /// How many runs of this department may be in flight at once. One by default: a rule that
    /// fires while the last run is still going SKIPS its window rather than queueing, because a
    /// queue is a debt the machine then tries to pay all at once.
    pub max_live_runs: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A team with its roster and its alçada, which is how the shell reads one: the membership and what
/// it may ask for ARE the team.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TeamView {
    #[serde(flatten)]
    pub team: Team,
    pub members: Vec<String>,
    /// Only the kinds this team was granted. An empty list is a department that can only write
    /// memos, which is what every team is until somebody decides otherwise.
    pub grants: Vec<TeamGrant>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TeamRequest {
    pub name: String,
    pub mission: String,
    pub director_agent_id: String,
    pub max_rounds: i64,
    pub max_parallel: i64,
    pub budget_usd: Option<f64>,
    /// Defaulted rather than required, so a client written before this column existed still saves a
    /// team instead of 400ing on a field it has never heard of.
    #[serde(default = "default_max_open_actions")]
    pub max_open_actions: i64,
    #[serde(default = "default_max_live_runs")]
    pub max_live_runs: i64,
    #[serde(default)]
    pub members: Vec<String>,
    /// Replaced wholesale, exactly as `members` is, and for the same reason: the editor sends the
    /// alçada as the owner left it, and a merge would make revoking one impossible through the only
    /// surface that edits it.
    #[serde(default)]
    pub grants: Vec<TeamGrant>,
}

fn default_max_open_actions() -> i64 {
    DEFAULT_MAX_OPEN_ACTIONS
}

fn default_max_live_runs() -> i64 {
    1
}

#[derive(Debug)]
pub enum TeamError {
    DuplicateName,
    Invalid(&'static str),
    NotFound,
    /// An agent the team names is not in the catalogue.
    UnknownAgent(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for TeamError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for TeamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateName => {
                formatter.write_str("a team with that name or id already exists")
            }
            Self::Invalid(message) => formatter.write_str(message),
            Self::NotFound => formatter.write_str("team not found"),
            Self::UnknownAgent(id) => write!(formatter, "no agent named {id} is in the catalogue"),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}
impl std::error::Error for TeamError {}

// ---------------------------------------------------------------------------------------------
// A run, and the plan that drives it
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, serde::Serialize, sqlx::FromRow)]
pub struct TeamRun {
    pub id: String,
    pub team_id: String,
    pub request: String,
    pub workspace: String,
    pub state: String,
    /// Which director node is IN FLIGHT: `none` | `planning` | `replanning` | `delivering`.
    ///
    /// One column and not three booleans, because two booleans can both be true and the enum makes
    /// that state inexpressible. Separate from `state` because it answers a different question —
    /// `state` says where the run is, this says whether the node has already been launched. Without
    /// it every tick would launch another planner while the first one runs, which is the mistake
    /// `job::JobView::planning` exists to record; the cost of getting it wrong is measured in
    /// `job.rs` at one wasted node per round.
    pub director_node: String,
    pub director_run_id: Option<i64>,
    pub round: i64,
    pub next_ordinal: i64,
    pub dry_rounds: i64,
    pub plan_retries: i64,
    pub replanned: String,
    pub outcome: Option<String>,
    pub why: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    /// The rule that started this, or NULL when a person did.
    pub trigger_id: Option<i64>,
    /// The run whose ending started this. For drawing the chain; it decides nothing.
    pub parent_id: Option<String>,
    /// The run this tree grew from. **This is what decides the money** — see
    /// `team_trigger::tree_has_room`. A run a person asked for is its own root, never NULL: a NULL
    /// meaning "I am the root" makes every reader write `COALESCE(root_id, id)`, and the day one
    /// forgets, the tree ceiling reads the wrong run.
    pub root_id: String,
    pub depth: i64,
}

/// Where a run came from, for the columns above.
///
/// Defaulted to "a person asked, and this is its own tree", which is what every caller that is not
/// a trigger means. `start` takes no lineage at all and `start_with` takes one, so the ordinary
/// path cannot accidentally declare itself the child of something.
#[derive(Debug, Clone, Default)]
pub struct Lineage {
    pub trigger_id: Option<i64>,
    pub parent_id: Option<String>,
    /// `None` means this run is its own root.
    pub root_id: Option<String>,
    pub depth: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, sqlx::FromRow)]
pub struct TeamItem {
    pub ordinal: i64,
    pub round: i64,
    pub agent_id: String,
    pub description: String,
    pub state: String,
    pub run_id: Option<i64>,
    pub output_path: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct TeamRunView {
    #[serde(flatten)]
    pub run: TeamRun,
    pub items: Vec<TeamItem>,
    /// What this run has spent so far, summed over every run it started — the director's nodes
    /// included, which is what `runs.team_run_id` exists for.
    pub cost_usd: f64,
}

fn files_root(state: &AppState) -> Result<std::path::PathBuf, String> {
    crate::door::files_root(state)
        .map(std::path::Path::to_path_buf)
        .map_err(|_| "no files folder is configured on this machine".to_owned())
}

// ---------------------------------------------------------------------------------------------
// Alçada: what a department asks for, and what the core does about it
// ---------------------------------------------------------------------------------------------

/// Which node of a team run is calling, and what it is.
///
/// A team's key names the RUN: the director and every specialist of one run hold the identical
/// token, by design — the key belongs to the run. Two questions are nonetheless about the node, and
/// this is where they are answered from the one column that already knows: `team_runs.director_run_id`
/// names the director's node while it is in flight.
///
/// **Not a new `Scope`.** A `Scope::TeamDirector` would be a fifth family of credentials expressing
/// a condition a column already answers, and it would duplicate the token's death rule, the mint,
/// the arm in `resolve` and the line in `permits` — five places for a question that is `WHERE id = ?`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// The director's node, in flight right now.
    Director,
    /// A specialist working an item of this run, and which one.
    Specialist(i64),
    /// A node this run does not recognise: no header, a stale id, or one belonging to another run.
    /// Treated as the least authority rather than as an error, because that is the safe direction
    /// and the honest one — the daemon does not know who this is.
    Unknown,
}

/// Resolves the calling node against the run its key authenticated.
///
/// The header is checked against THIS run, always. A run id from somewhere else — another
/// department, another era — matches neither the director column nor an item of this run, so it
/// resolves to `Unknown` and holds nothing.
async fn calling_node(
    pool: &sqlx::SqlitePool,
    team_run_id: &str,
    headers: &axum::http::HeaderMap,
) -> Caller {
    let Some(run_id) = headers
        .get(crate::daemon_client::RUN_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return Caller::Unknown;
    };

    let director: Option<Option<i64>> =
        sqlx::query_scalar("SELECT director_run_id FROM team_runs WHERE id = ?")
            .bind(team_run_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if director == Some(Some(run_id)) {
        return Caller::Director;
    }

    match sqlx::query_scalar::<_, i64>(
        "SELECT ordinal FROM team_items WHERE team_run_id = ? AND run_id = ?",
    )
    .bind(team_run_id)
    .bind(run_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(ordinal)) => Caller::Specialist(ordinal),
        _ => Caller::Unknown,
    }
}

/// Which member of the department is making this call, and from which node.
///
/// `None` for a node the run does not recognise, which `send_note` turns into a refusal rather than
/// into an unsigned note. The two halves come from different places on purpose: `calling_node`
/// answers WHICH ROLE from `team_runs.director_run_id` and `team_items.run_id`, and the run id is
/// read from the header directly because that is the row this note has to point at as its evidence.
///
/// Reading the header twice — here and inside `calling_node` — is deliberate over threading it
/// through: `calling_node` is called by three other functions that want the role and not the id, and
/// widening its return to suit one caller would put an unused half in front of all of them.
async fn agent_of_caller(
    pool: &sqlx::SqlitePool,
    team_run_id: &str,
    headers: &axum::http::HeaderMap,
) -> Result<Option<(String, i64)>, sqlx::Error> {
    let Some(run_id) = headers
        .get(crate::daemon_client::RUN_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return Ok(None);
    };

    match calling_node(pool, team_run_id, headers).await {
        Caller::Director => Ok(sqlx::query_scalar::<_, String>(
            "SELECT t.director_agent_id FROM teams t
               JOIN team_runs r ON r.team_id = t.id
              WHERE r.id = ?",
        )
        .bind(team_run_id)
        .fetch_optional(pool)
        .await?
        .map(|agent_id| (agent_id, run_id))),
        Caller::Specialist(ordinal) => Ok(sqlx::query_scalar::<_, String>(
            "SELECT agent_id FROM team_items WHERE team_run_id = ? AND ordinal = ?",
        )
        .bind(team_run_id)
        .bind(ordinal)
        .fetch_optional(pool)
        .await?
        .map(|agent_id| (agent_id, run_id))),
        Caller::Unknown => Ok(None),
    }
}

/// The one piece of this module's test scaffolding another module needs.
///
/// `team/trigger.rs` starts real departments to test what starts them, and a second copy of this
/// `AppState` would be a second set of answers to "what does a daemon with a files folder look
/// like" — kept in step by hand, and wrong on the day somebody adds a field.
#[cfg(test)]
pub mod test_support {
    pub async fn state_with(root: std::path::PathBuf) -> crate::state::AppState {
        super::tests::test_state(root).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Scope;
    use crate::speed::Speed;

    #[tokio::test]
    async fn an_idle_pass_reports_no_work_and_the_loop_waits_longer() {
        let (state, _root) = state_with_root().await;
        assert!(
            !team_tick(&state, chrono::Utc::now()).await,
            "nothing live, nothing due"
        );
        assert_eq!(lifecycle::team_wait(false), TEAM_IDLE_TICK);
        assert_eq!(lifecycle::team_wait(true), TEAM_TICK);
        assert!(TEAM_IDLE_TICK > TEAM_TICK);

        marketing(&state).await;
        start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        assert!(
            team_tick(&state, chrono::Utc::now()).await,
            "a live run is work"
        );
    }

    // -----------------------------------------------------------------------------------------
    // The vocabulary three readers share
    // -----------------------------------------------------------------------------------------

    /// The two lists partition the state machine of the design's §3 — every state is in exactly one
    /// of them.
    ///
    /// Written against the transition table rather than against the constants, so that adding a
    /// sixth ending to `TERMINAL_STATES` without teaching the machine about it, or teaching the
    /// machine a state neither list knows, both fail here rather than in whichever of the three
    /// readers happens to meet the unknown value first.
    #[test]
    fn every_state_of_the_machine_is_live_or_terminal_and_never_both() {
        const EVERY_STATE: &[&str] = &[
            "planning",
            "working",
            "delivering",
            "done",
            "stopped",
            "expired",
            "failed",
            "cancelled",
        ];

        for state in EVERY_STATE {
            assert_ne!(
                LIVE_STATES.contains(state),
                TERMINAL_STATES.contains(state),
                "{state} must be live or terminal, and exactly one of the two"
            );
        }
        assert_eq!(LIVE_STATES.len() + TERMINAL_STATES.len(), EVERY_STATE.len());
    }

    /// A state nobody declared is not live. The default matters: `is_live` gates a credential, and
    /// a typo that read as live would keep a token working forever.
    #[test]
    fn an_unknown_state_is_not_live() {
        for state in ["", "running", "Planning", "workin", "done "] {
            assert!(!is_live(state), "{state:?} must not read as a live run");
        }
    }

    /// `stopped` and `expired` are endings and are NOT failures, which is the distinction an owner
    /// reads: shown `failed`, they go looking for an error that does not exist.
    #[test]
    fn a_ceiling_is_an_ending_and_not_a_failure() {
        for ceiling in ["stopped", "expired"] {
            assert!(TERMINAL_STATES.contains(&ceiling));
            assert_ne!(ceiling, "failed");
        }
    }

    // -----------------------------------------------------------------------------------------
    // Reading a director's plan
    // -----------------------------------------------------------------------------------------

    fn roster() -> Vec<String> {
        vec!["copywriter".to_owned(), "researcher".to_owned()]
    }

    #[test]
    fn a_plain_plan_is_read_as_written() {
        let plan = parse_team_plan(
            r#"{"items":[{"agent_id":"copywriter","description":"draft the post"}],"done":false}"#,
            &roster(),
            MAX_ITEMS_PER_ROUND,
        )
        .expect("a well-formed plan is readable");

        assert_eq!(
            plan.items,
            vec![PlannedItem {
                agent_id: "copywriter".to_owned(),
                description: "draft the post".to_owned(),
            }]
        );
        assert!(plan.dropped.is_empty());
        assert!(!plan.done);
    }

    /// A model asked for JSON writes a sentence around it perhaps a third of the time, and a code
    /// fence more often than that. Neither is a reason to lose a paid-for plan.
    #[test]
    fn a_plan_wrapped_in_prose_or_a_fence_is_still_a_plan() {
        for text in [
            "Here is the plan:\n```json\n{\"items\":[],\"done\":true}\n```\nHope that helps.",
            "{\"items\":[],\"done\":true}",
            "  \n{\"items\": [], \"done\": true}\n\n",
        ] {
            let plan = parse_team_plan(text, &roster(), MAX_ITEMS_PER_ROUND)
                .unwrap_or_else(|| panic!("{text:?} should have been readable"));
            assert!(plan.done);
        }
    }

    /// `None` means unreadable and must never mean "readable and empty": the round machine relaunches
    /// on the first and counts a dry round on the second, and those are different places to go.
    #[test]
    fn an_unreadable_answer_is_none_and_an_empty_plan_is_not() {
        for text in ["", "I could not do that", "[1,2,3]", "null", "{"] {
            assert!(
                parse_team_plan(text, &roster(), MAX_ITEMS_PER_ROUND).is_none(),
                "{text:?} should be unreadable"
            );
        }

        let empty = parse_team_plan(r#"{"items":[]}"#, &roster(), MAX_ITEMS_PER_ROUND)
            .expect("an empty plan is a plan");
        assert!(empty.items.is_empty());
        assert!(!empty.done);
    }

    /// One misspelled name in five must not cost the round. The item falls, with a reason the feed
    /// can carry, and its siblings run.
    #[test]
    fn an_agent_outside_the_roster_drops_its_own_item_only() {
        let plan = parse_team_plan(
            r#"{"items":[
                 {"agent_id":"copywriter","description":"draft"},
                 {"agent_id":"designer","description":"a logo"},
                 {"agent_id":"researcher","description":"find the numbers"}
               ]}"#,
            &roster(),
            MAX_ITEMS_PER_ROUND,
        )
        .unwrap();

        assert_eq!(plan.items.len(), 2);
        assert!(plan.items.iter().all(|item| item.agent_id != "designer"));
        assert_eq!(plan.dropped.len(), 1);
        assert!(plan.dropped[0].contains("designer"));
    }

    /// Cut items are REPORTED, not discarded: a silently truncated queue reads downstream as the
    /// whole of what the planner found.
    #[test]
    fn the_ceiling_reports_what_it_cut() {
        let items: Vec<String> = (0..5)
            .map(|index| format!(r#"{{"agent_id":"copywriter","description":"item {index}"}}"#))
            .collect();
        let plan = parse_team_plan(
            &format!(r#"{{"items":[{}]}}"#, items.join(",")),
            &roster(),
            2,
        )
        .unwrap();

        assert_eq!(plan.items.len(), 2);
        assert_eq!(
            plan.dropped.len(),
            3,
            "three were cut and three must be said"
        );
    }

    /// `done: true` beside items is a model answering both halves of the question. The half that
    /// costs money loses.
    #[test]
    fn done_wins_over_any_items_beside_it() {
        let plan = parse_team_plan(
            r#"{"items":[{"agent_id":"copywriter","description":"more work"}],"done":true,
                "why":"the folder already answers it"}"#,
            &roster(),
            MAX_ITEMS_PER_ROUND,
        )
        .unwrap();

        assert!(plan.done);
        assert!(
            plan.items.is_empty(),
            "queueing work beside a true `done` is obeying the expensive half"
        );
        assert_eq!(plan.why.as_deref(), Some("the folder already answers it"));
    }

    #[test]
    fn an_item_with_no_work_in_it_is_dropped() {
        let plan = parse_team_plan(
            r#"{"items":[
                 {"agent_id":"copywriter","description":"   "},
                 {"agent_id":"copywriter"},
                 {"description":"nobody asked"}
               ]}"#,
            &roster(),
            MAX_ITEMS_PER_ROUND,
        )
        .unwrap();

        assert!(plan.items.is_empty());
        assert_eq!(plan.dropped.len(), 3);
    }

    // -----------------------------------------------------------------------------------------
    // The round machine
    // -----------------------------------------------------------------------------------------

    fn plan_with(count: usize) -> PlannedItems {
        PlannedItems {
            items: (0..count)
                .map(|index| PlannedItem {
                    agent_id: "copywriter".to_owned(),
                    description: format!("item {index}"),
                })
                .collect(),
            dropped: Vec::new(),
            done: false,
            why: None,
        }
    }

    fn finished_plan() -> PlannedItems {
        PlannedItems {
            items: Vec::new(),
            dropped: Vec::new(),
            done: true,
            why: None,
        }
    }

    #[test]
    fn a_first_plan_with_items_opens_round_zero() {
        let progress = next_after_director("planning", Some(&plan_with(2)), 0, 0, 0, 3);
        assert_eq!(progress.state, "working");
        assert_eq!(progress.round, 0, "the plan's items ARE round zero");
        assert!(progress.queue_items);
    }

    #[test]
    fn a_first_plan_that_says_done_delivers_without_a_round() {
        let progress = next_after_director("planning", Some(&finished_plan()), 0, 0, 0, 3);
        assert_eq!(progress.state, "delivering");
        assert!(!progress.queue_items);
    }

    /// Round 0 has no partial work to hand over, so an unreadable plan is worth one more attempt —
    /// and exactly one.
    #[test]
    fn an_unreadable_first_plan_relaunches_once_and_then_fails() {
        let first = next_after_director("planning", None, 0, 0, 0, 3);
        assert_eq!(first.state, "planning");
        assert_eq!(first.plan_retries, 1);

        let second = next_after_director("planning", None, 0, 0, first.plan_retries, 3);
        assert_eq!(second.state, "failed");
        assert!(
            second.why.is_some(),
            "a failure the owner reads needs a reason"
        );
    }

    /// The ordering the design writes down because implementers guess it wrong: increment the
    /// round, THEN check the ceiling. A team with `max_rounds = 3` runs rounds 0, 1 and 2.
    #[test]
    fn a_team_runs_max_rounds_rounds_and_delivers_as_the_next_one_opens() {
        let mut round = 0;
        for expected in [0, 1] {
            let progress = next_after_director("working", Some(&plan_with(1)), round, 0, 0, 3);
            assert_eq!(progress.state, "working");
            assert_eq!(progress.round, expected + 1);
            assert!(progress.queue_items);
            round = progress.round;
        }

        let last = next_after_director("working", Some(&plan_with(1)), round, 0, 0, 3);
        assert_eq!(last.round, 3);
        assert_eq!(last.state, "delivering");
        assert!(
            !last.queue_items,
            "items must never be written for a round that will not run"
        );
        assert!(last.why.unwrap().contains("ceiling"));
    }

    /// One dry round is not a signal — a replan can legitimately produce nothing while the previous
    /// round settles. Two in a row is.
    #[test]
    fn two_dry_rounds_deliver_and_one_does_not() {
        let first = next_after_director("working", Some(&plan_with(0)), 0, 0, 0, 6);
        assert_eq!(first.state, "working");
        assert_eq!(first.dry_rounds, 1);

        let second = next_after_director(
            "working",
            Some(&plan_with(0)),
            first.round,
            first.dry_rounds,
            0,
            6,
        );
        assert_eq!(second.state, "delivering");
        assert_eq!(second.dry_rounds, 2);
        assert!(second.why.unwrap().contains("added nothing"));
    }

    /// A round that produces work resets the count, which is what makes the brake "it dried up"
    /// rather than "two of the rounds were quiet".
    #[test]
    fn a_productive_round_resets_the_dry_count() {
        let dry = next_after_director("working", Some(&plan_with(0)), 0, 0, 0, 6);
        assert_eq!(dry.dry_rounds, 1);

        let wet = next_after_director(
            "working",
            Some(&plan_with(2)),
            dry.round,
            dry.dry_rounds,
            0,
            6,
        );
        assert_eq!(wet.dry_rounds, 0);
        assert_eq!(wet.state, "working");
    }

    /// In a later round there IS work in the folder, so an unreadable replan is a dry round rather
    /// than a relaunch — the asymmetry with round 0 stated as an assertion.
    #[test]
    fn an_unreadable_later_plan_is_a_dry_round_and_never_a_failure() {
        let progress = next_after_director("working", None, 1, 0, 0, 6);
        assert_eq!(progress.state, "working");
        assert_eq!(progress.dry_rounds, 1);
        assert_eq!(
            progress.plan_retries, 0,
            "retries are a round-zero mechanism"
        );

        let second =
            next_after_director("working", None, progress.round, progress.dry_rounds, 0, 6);
        assert_eq!(second.state, "delivering");
    }

    #[test]
    fn a_director_that_says_done_mid_flight_delivers() {
        let progress = next_after_director("working", Some(&finished_plan()), 1, 0, 0, 6);
        assert_eq!(progress.state, "delivering");
        assert!(progress.why.unwrap().contains("finished"));
    }

    // -----------------------------------------------------------------------------------------
    // The database, end to end
    // -----------------------------------------------------------------------------------------

    pub(super) async fn test_state(root: std::path::PathBuf) -> AppState {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};
        let pool = crate::testdb::fresh_pool().await;
        AppState {
            startup_config: Default::default(),
            token: crate::auth::Token("test-token".into()),
            pool,
            channels: Default::default(),
            runner: Arc::new(crate::execution::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            utility_model: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            run_tails: Default::default(),
            fleet: Default::default(),
            files_root: Some(root),
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            // Off, like `web` beside it: no test in this module drives a browser, and a department
            // reaches one — if it ever does — through the daemon client like any other agent.
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// A state whose files root is a real, canonicalised directory.
    ///
    /// Canonicalised as `files::ensure_root` does at startup, and for the reason written there:
    /// every containment check compares against this path, and on Windows a temp directory arrives
    /// as a short name that no resolved child compares equal to.
    async fn state_with_root() -> (AppState, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let state = test_state(std::fs::canonicalize(root.path()).unwrap()).await;
        (state, root)
    }

    async fn state_with_quota() -> (AppState, tempfile::TempDir) {
        let (mut state, root) = state_with_root().await;
        let now = chrono::Utc::now();
        let address = crate::quota::test_support::stub_sidecar(
            crate::quota::test_support::live_answer("claude", "5h", 1.0, None, now),
        )
        .await;
        crate::quota::test_support::arm(&state.pool, true, 85, 90).await;
        state.quota = std::sync::Arc::new(crate::quota::QuotaRuntime::new(
            crate::quota::client::QuotaClient::new(&address, "bearer".into()),
            "claude".into(),
        ));
        (state, root)
    }

    async fn insert_agent(state: &AppState, id: &str, engine: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO agents
                 (id, name, speciality, prompt, engine, model, tool_policy, created_at, updated_at)
             VALUES (?, ?, 'writes things', 'you write things', ?, NULL, 'mcp_only',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .bind(engine)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// A team with a director and two specialists, through the public surface.
    async fn marketing(state: &AppState) -> TeamView {
        for id in ["director", "copywriter", "researcher"] {
            insert_agent(state, id, "claude").await;
        }
        create(
            &state.pool,
            TeamRequest {
                name: "Marketing".to_owned(),
                mission: "sell the thing".to_owned(),
                director_agent_id: "director".to_owned(),
                max_rounds: 3,
                max_parallel: 2,
                budget_usd: None,
                max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: vec!["copywriter".to_owned(), "researcher".to_owned()],
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_team_round_trips_with_its_roster() {
        let (state, _root) = state_with_root().await;
        let team = marketing(&state).await;

        assert_eq!(team.team.id, "marketing");
        assert_eq!(team.members, vec!["copywriter", "researcher"]);

        let listed = list(&state.pool).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].members.len(), 2);
    }

    /// The roster is replaced and not merged, because that is what the editor sends. A merge would
    /// make removing somebody impossible through the only surface that edits one.
    #[tokio::test]
    async fn editing_a_team_replaces_its_roster_rather_than_adding_to_it() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;

        let updated = update(
            &state.pool,
            "marketing",
            TeamRequest {
                name: "Marketing".to_owned(),
                mission: "sell the thing".to_owned(),
                director_agent_id: "director".to_owned(),
                max_rounds: 3,
                max_parallel: 2,
                budget_usd: None,
                max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: vec!["copywriter".to_owned()],
            },
        )
        .await
        .unwrap();

        assert_eq!(updated.members, vec!["copywriter"]);
    }

    #[tokio::test]
    async fn a_team_may_not_name_an_agent_that_is_not_in_the_catalogue() {
        let (state, _root) = state_with_root().await;
        insert_agent(&state, "director", "claude").await;

        let refusal = create(
            &state.pool,
            TeamRequest {
                name: "Ghosts".to_owned(),
                mission: "haunt".to_owned(),
                director_agent_id: "director".to_owned(),
                max_rounds: 2,
                max_parallel: 1,
                budget_usd: None,
                max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: vec!["nobody".to_owned()],
            },
        )
        .await;
        assert!(matches!(refusal, Err(TeamError::UnknownAgent(id)) if id == "nobody"));
    }

    /// The ceilings the daemon imposes whatever the owner types, because rounds times parallelism
    /// is the multiplier on the bill.
    #[tokio::test]
    async fn the_daemons_ceilings_are_not_the_owners_to_raise() {
        let (state, _root) = state_with_root().await;
        insert_agent(&state, "director", "claude").await;

        for (rounds, parallel) in [
            (0, 1),
            (MAX_ROUNDS_CEILING + 1, 1),
            (1, 0),
            (1, MAX_PARALLEL_CEILING + 1),
        ] {
            let refusal = create(
                &state.pool,
                TeamRequest {
                    name: format!("Team {rounds}x{parallel}"),
                    mission: "work".to_owned(),
                    director_agent_id: "director".to_owned(),
                    max_rounds: rounds,
                    max_parallel: parallel,
                    budget_usd: None,
                    max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                    max_live_runs: 1,
                    grants: Vec::new(),
                    members: Vec::new(),
                },
            )
            .await;
            assert!(
                matches!(refusal, Err(TeamError::Invalid(_))),
                "{rounds} rounds x {parallel} parallel should be refused"
            );
        }
    }

    /// Invariant (II) at the place that opens a round: a `max_parallel` written straight into
    /// the column does not open 99 specialists, at any speed.
    #[tokio::test]
    async fn a_round_never_opens_past_the_ceiling_whatever_the_column_says() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        sqlx::query("UPDATE teams SET max_parallel = 99 WHERE id = 'marketing'")
            .execute(&state.pool)
            .await
            .unwrap();
        for speed in [Speed::Normal, Speed::Fast, Speed::Thorough] {
            let id = start_with(
                &state,
                "marketing",
                "write it",
                Lineage::default(),
                speed,
                None,
            )
            .await
            .unwrap();
            let run = fetch_run(&state, &id).await;
            assert_eq!(
                round_width(&state, &run).await.unwrap(),
                MAX_PARALLEL_CEILING
            );
        }
    }

    /// Section 5.2: the team's value is `normal`, `fast` doubles it, and a serial team stays serial.
    #[tokio::test]
    async fn fast_widens_a_round_and_never_a_serial_team() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        for (stored, speed, expected) in [
            (2, Speed::Normal, 2),
            (2, Speed::Fast, 4),
            (1, Speed::Fast, 1),
        ] {
            sqlx::query("UPDATE teams SET max_parallel = ? WHERE id = 'marketing'")
                .bind(stored)
                .execute(&state.pool)
                .await
                .unwrap();
            let id = start_with(
                &state,
                "marketing",
                "write it",
                Lineage::default(),
                speed,
                None,
            )
            .await
            .unwrap();
            let run = fetch_run(&state, &id).await;
            assert_eq!(
                round_width(&state, &run).await.unwrap(),
                expected,
                "{stored} {speed:?}"
            );
        }
    }

    /// A run started before the column existed has NULL, and NULL is `normal`.
    #[tokio::test]
    async fn a_run_with_no_speed_is_normal() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write it", None).await.unwrap();
        sqlx::query("UPDATE team_runs SET speed = NULL WHERE id = ?")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        let run = fetch_run(&state, &id).await;
        assert_eq!(round_width(&state, &run).await.unwrap(), 2);
    }

    /// An unknown speed on a request is the caller's mistake, made at a keyboard, now.
    #[tokio::test]
    async fn an_unknown_speed_on_a_request_is_refused() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let refusal = post_team_run(
            State(state.clone()),
            Path("marketing".to_owned()),
            Json(StartRequest {
                request: "write it".to_owned(),
                report_to_chat_id: None,
                speed: Some("fsat".to_owned()),
            }),
        )
        .await;
        let Err((status, message)) = refusal else {
            panic!("accepted an unknown speed")
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(message.contains("fsat"));
    }

    // -----------------------------------------------------------------------------------------
    // Alçada
    // -----------------------------------------------------------------------------------------

    /// Grants a team an alçada and starts a run, returning that run's id.
    async fn team_with_grant(state: &AppState, kind: &str, mode: &str) -> String {
        marketing(state).await;
        sqlx::query("INSERT INTO team_grants (team_id, kind, mode) VALUES ('marketing', ?, ?)")
            .bind(kind)
            .bind(mode)
            .execute(&state.pool)
            .await
            .unwrap();
        start(state, "marketing", "write the launch post", None)
            .await
            .unwrap()
    }

    fn an_email() -> serde_json::Value {
        serde_json::json!({
            "to": "list@example.com",
            "subject": "we launch tomorrow",
            "body": "Details inside.",
        })
    }

    async fn ask(
        state: &AppState,
        run_id: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<ProposeActionResponse, ActionError> {
        propose_action(
            state,
            &Scope::TeamRun(run_id.to_owned()),
            &axum::http::HeaderMap::new(),
            &ProposeActionRequest {
                kind: kind.to_owned(),
                payload,
                why: "the launch is tomorrow and the list asked to be told".to_owned(),
            },
        )
        .await
    }

    /// Default deny, and it is the ABSENCE of a row that denies — there is no `mode = 'deny'` for a
    /// second opinion to disagree with.
    #[tokio::test]
    async fn a_department_with_no_alcada_may_ask_for_nothing() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        for kind in GRANTABLE_ACTIONS {
            let refusal = ask(&state, &run_id, kind, an_email()).await;
            assert!(
                matches!(&refusal, Err(ActionError::Ungranted(named)) if named == kind),
                "{kind}: got {refusal:?}"
            );
        }
        let written: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_actions")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(written, 0, "a refusal leaves no row to decide");
    }

    /// The constant is asked BEFORE the table, so a row somebody wrote into `team_grants` by hand
    /// cannot grant an action the house does not have.
    #[tokio::test]
    async fn a_kind_outside_the_list_is_refused_even_with_a_grant_in_the_table() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "set_kill", "allow").await;

        let refusal = ask(&state, &run_id, "set_kill", serde_json::json!({})).await;
        assert!(
            matches!(&refusal, Err(ActionError::Unknown(named)) if named == "set_kill"),
            "got {refusal:?}"
        );
    }

    /// The list of what a department may be granted, held against the list of what it must never
    /// be. Written out by hand on both sides: "everything that is not `Acts`" would have handed a
    /// marketing department the controls of the daemon, decided by whoever added the next tool.
    #[test]
    fn no_grantable_action_is_a_control_of_the_daemon() {
        // A department holding `set_kill` turns off the house's autonomy. One holding
        // `approve_proposal` approves its own proposals, and thereby holds every other authority by
        // transitivity, without anybody having written that down.
        for forbidden in [
            "approve_proposal",
            "reject_proposal",
            "set_kill",
            "cancel_run",
            "create_run",
            "create_job",
            "triage_email",
            // The sharpest of all, and the one the design named and this refuses: `TOOL_EFFECTS`
            // records it as the only effect in the house that outlives the daemon and that its
            // owner cannot take back from here.
            "vcs_request",
            "vcs_ticket",
        ] {
            assert!(
                !GRANTABLE_ACTIONS.contains(&forbidden),
                "`{forbidden}` is a control of the daemon and no department may be granted it"
            );
        }
        // And every grantable kind is understood by the validator, so a grant cannot name something
        // the writer would refuse and the owner would only discover mid-run.
        for kind in GRANTABLE_ACTIONS {
            let refusal = validate_payload(kind, &serde_json::json!({}))
                .expect_err("an empty payload is not a request");
            assert!(
                !refusal.contains("nobody can do"),
                "`{kind}` is grantable and `validate_payload` does not know it"
            );
        }
    }

    /// **The test that decides whether this design is safe.**
    ///
    /// A specialist reads a web page. The page contains text asking for an email to be sent. Two
    /// independent things stop it, and this asserts the second: `propose_action` is `Acts`, so the
    /// turn that called a `ReadsUntrusted` tool cannot reach it for the rest of that turn. (The
    /// first is that the page arrived summarised by a local model rather than raw — `web.rs`.)
    #[tokio::test]
    async fn a_specialist_that_read_the_web_may_not_ask_for_an_action_in_that_turn() {
        use crate::mcp_tools::{ToolEffect, tool_effect};

        assert_eq!(
            tool_effect("web_read"),
            ToolEffect::ReadsUntrusted,
            "the premise: reading a page marks the turn"
        );
        assert_eq!(
            tool_effect("read_team_file"),
            ToolEffect::ReadsUntrusted,
            "and so does reading what another specialist wrote out of one"
        );
        // The conclusion, and the whole reason `propose_action` is classified as an action despite
        // performing none: `hooks.rs` refuses every `Acts` tool once the run is marked, so a page
        // that asks for an email reaches a turn that can no longer ask for one.
        assert_eq!(
            tool_effect("propose_action"),
            ToolEffect::Acts,
            "if this ever becomes ReadsOwn, a web page can send mail"
        );
    }

    /// A `propose` grant writes the action AND the proposal, or neither. An action with no proposal
    /// is one nobody will ever decide; a proposal with no action is a button that approves nothing.
    #[tokio::test]
    async fn a_propose_grant_files_the_action_and_the_question_together() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "send_email", "propose").await;

        let filed = ask(&state, &run_id, "send_email", an_email())
            .await
            .unwrap();
        let proposal_id = filed.proposal_id.expect("a propose grant asks somebody");
        assert!(filed.outcome.contains(&format!("#{proposal_id}")));

        let action: TeamAction = sqlx::query_as(
            "SELECT id, team_run_id, ordinal, kind, payload, why, proposal_id, state, error,
                    created_at, executed_at
             FROM team_actions WHERE id = ?",
        )
        .bind(filed.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(action.state, "pending");
        assert_eq!(action.proposal_id, Some(proposal_id));

        let proposal = crate::proposals::get(&state.pool, proposal_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(proposal.kind, "team-action");
        assert_eq!(proposal.status, "pending");
        // The action's kind, so a person scanning the queue knows what they are agreeing to without
        // opening the row.
        assert_eq!(proposal.tool_name.as_deref(), Some("send_email"));
        assert_eq!(
            proposal.project_id, None,
            "a department has no project, which is why the per-project wip ceiling never sees this"
        );
        // Nothing has happened yet, and nothing will until somebody says so.
        execute_due_actions(&state).await;
        let still: String = sqlx::query_scalar("SELECT state FROM team_actions WHERE id = ?")
            .bind(filed.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(still, "pending", "an undecided action is not carried out");
    }

    /// `allow` is the owner saying they do not want to be in the middle. No proposal is written, and
    /// the next pass of the tick does it.
    #[tokio::test]
    async fn an_allow_grant_asks_nobody_and_runs_on_the_next_pass() {
        let (state, root) = state_with_root().await;
        let run_id = team_with_grant(&state, "file_document", "allow").await;

        let filed = ask(
            &state,
            &run_id,
            "file_document",
            serde_json::json!({ "path": "launch/post.md", "content": "# Launch\n" }),
        )
        .await
        .unwrap();
        assert_eq!(filed.proposal_id, None);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM proposals")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0
        );

        execute_due_actions(&state).await;

        let action: (String, Option<String>) =
            sqlx::query_as("SELECT state, error FROM team_actions WHERE id = ?")
                .bind(filed.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(action, ("done".to_owned(), None), "{action:?}");
        let written = std::fs::read_to_string(
            std::fs::canonicalize(root.path())
                .unwrap()
                .join("launch")
                .join("post.md"),
        )
        .unwrap();
        assert_eq!(written, "# Launch\n");
    }

    /// Two passes overlapping must not do the same thing twice. The claim is a compare-and-swap
    /// before the work, exactly as `ingest_director` does it — an email that may already have gone
    /// is not sent again.
    #[tokio::test]
    async fn an_approved_action_is_carried_out_once_and_once_only() {
        let (state, root) = state_with_root().await;
        let run_id = team_with_grant(&state, "file_document", "allow").await;
        let filed = ask(
            &state,
            &run_id,
            "file_document",
            serde_json::json!({ "path": "note.md", "content": "once" }),
        )
        .await
        .unwrap();

        let (first, second) =
            tokio::join!(execute_due_actions(&state), execute_due_actions(&state));
        let _ = (first, second);

        let executed: Vec<String> =
            sqlx::query_scalar("SELECT state FROM team_actions WHERE id = ?")
                .bind(filed.id)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(executed, ["done"]);
        assert!(
            std::fs::canonicalize(root.path())
                .unwrap()
                .join("note.md")
                .exists()
        );
    }

    /// Two fields and not one. `proposals.status` says what the human decided; `team_actions.state`
    /// says what the world answered. Merged, `failed` would read as "the person refused" and
    /// `approved` would be a lie about a message that never left.
    #[tokio::test]
    async fn an_action_that_fails_does_not_contradict_the_approval() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "send_email", "propose").await;
        let filed = ask(&state, &run_id, "send_email", an_email())
            .await
            .unwrap();
        let proposal_id = filed.proposal_id.unwrap();

        crate::proposals::transition(&state.pool, proposal_id, "approved", "approved by user")
            .await
            .unwrap();
        // No submission host is configured in a test state, so the send fails at the first check
        // and nothing leaves the process.
        execute_due_actions(&state).await;

        let action: (String, Option<String>) =
            sqlx::query_as("SELECT state, error FROM team_actions WHERE id = ?")
                .bind(filed.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(action.0, "failed");
        assert!(action.1.is_some_and(|why| !why.is_empty()));
        assert_eq!(
            crate::proposals::get(&state.pool, proposal_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "approved",
            "the person did say yes, and a failure afterwards does not unsay it"
        );
    }

    /// The ceiling is per TEAM — `wip.rs` counts per project and a department has none — and it
    /// clears the moment somebody decides.
    #[tokio::test]
    async fn the_open_action_ceiling_is_per_team_and_frees_when_somebody_decides() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "send_email", "propose").await;
        sqlx::query("UPDATE teams SET max_open_actions = 2 WHERE id = 'marketing'")
            .execute(&state.pool)
            .await
            .unwrap();

        let first = ask(&state, &run_id, "send_email", an_email())
            .await
            .unwrap();
        ask(&state, &run_id, "send_email", an_email())
            .await
            .unwrap();
        let refusal = ask(&state, &run_id, "send_email", an_email()).await;
        assert!(
            matches!(&refusal, Err(ActionError::QueueFull(why)) if why.contains('2')),
            "got {refusal:?}"
        );

        // A department next door is untouched by a queue it did not fill.
        insert_agent(&state, "engineer", "claude").await;
        create(
            &state.pool,
            TeamRequest {
                name: "Support".to_owned(),
                mission: "answer people".to_owned(),
                director_agent_id: "engineer".to_owned(),
                max_rounds: 2,
                max_parallel: 1,
                budget_usd: None,
                max_open_actions: 1,
                max_live_runs: 1,
                grants: vec![TeamGrant {
                    kind: "send_email".to_owned(),
                    mode: "propose".to_owned(),
                }],
                members: vec!["engineer".to_owned()],
            },
        )
        .await
        .unwrap();
        let support_run = start(&state, "support", "answer the backlog", None)
            .await
            .unwrap();
        ask(&state, &support_run, "send_email", an_email())
            .await
            .expect("another department's queue is not this one's");

        // Deciding frees the slot, which is the self-limiting property `wip.rs` has in the other
        // axis: the queue cannot grow past what somebody is willing to work through.
        crate::proposals::transition(
            &state.pool,
            first.proposal_id.unwrap(),
            "rejected",
            "rejected by user",
        )
        .await
        .unwrap();
        ask(&state, &run_id, "send_email", an_email())
            .await
            .expect("a decided action no longer holds a slot");
    }

    /// A malformed request is refused to the AGENT, mid-turn, while it can still fix it — and not to
    /// a person three hours later whose only options are approving something broken or throwing away
    /// work already paid for.
    #[tokio::test]
    async fn a_malformed_payload_is_refused_to_the_agent_and_never_reaches_a_person() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "send_email", "propose").await;

        for broken in [
            serde_json::json!({ "subject": "no recipient", "body": "x" }),
            serde_json::json!({ "to": "", "subject": "empty recipient", "body": "x" }),
            serde_json::json!({ "to": "nobody", "subject": "no domain", "body": "x" }),
            // The header-injection rule, borrowed from `mailsend::validate` rather than restated.
            serde_json::json!({ "to": "a@b.c\nBcc: c@d.e", "subject": "x", "body": "x" }),
            serde_json::json!({ "to": "a@b.c", "subject": "no body", "body": "  " }),
        ] {
            let refusal = ask(&state, &run_id, "send_email", broken.clone()).await;
            assert!(
                matches!(refusal, Err(ActionError::Malformed(_))),
                "{broken} should not be a message"
            );
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM proposals")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0,
            "nothing malformed reached the queue"
        );

        // And a request with no reason is refused too: the sentence is what the person deciding
        // reads, and a queue whose entries do not explain themselves gets approved unread.
        let unexplained = propose_action(
            &state,
            &Scope::TeamRun(run_id.clone()),
            &axum::http::HeaderMap::new(),
            &ProposeActionRequest {
                kind: "send_email".to_owned(),
                payload: an_email(),
                why: "   ".to_owned(),
            },
        )
        .await;
        assert!(matches!(unexplained, Err(ActionError::Malformed(_))));
    }

    /// A run may finish with actions nobody has decided, and they outlive it. The opposite would
    /// hold a department in `working` until somebody opened a laptop — occupying a `max_parallel`
    /// slot and counting against the four-hour ceiling the whole time.
    #[tokio::test]
    async fn a_run_may_end_with_actions_still_undecided() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "send_email", "propose").await;
        let filed = ask(&state, &run_id, "send_email", an_email())
            .await
            .unwrap();

        cancel(&state, &run_id).await.unwrap();
        let run_state: String = sqlx::query_scalar("SELECT state FROM team_runs WHERE id = ?")
            .bind(&run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(TERMINAL_STATES.contains(&run_state.as_str()), "{run_state}");

        let action: String = sqlx::query_scalar("SELECT state FROM team_actions WHERE id = ?")
            .bind(filed.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            action, "pending",
            "the question outlives the run that asked"
        );
    }

    /// The path is contained by `files::resolve_within` against the files root, which is the one
    /// thing standing between a department and the rest of the disk.
    #[tokio::test]
    async fn a_filed_document_cannot_leave_the_files_folder() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "file_document", "allow").await;

        for escape in ["../outside.md", "/etc/passwd", "a/../../outside.md"] {
            let refusal = ask(
                &state,
                &run_id,
                "file_document",
                serde_json::json!({ "path": escape, "content": "x" }),
            )
            .await;
            assert!(
                matches!(refusal, Err(ActionError::Malformed(_))),
                "{escape} should not be a path inside the folder"
            );
        }
    }

    #[test]
    fn a_calendar_event_with_an_absurd_duration_is_refused_before_it_is_filed() {
        for minutes in [serde_json::json!(i64::MAX), serde_json::json!(44_641)] {
            let refusal = validate_payload(
                "calendar_event",
                &serde_json::json!({
                    "title": "launch review",
                    "starts_at_local": "2026-08-17T09:30:00",
                    "duration_minutes": minutes,
                    "tz": "Europe/Lisbon",
                }),
            );
            assert!(refusal.is_err(), "{minutes} minutes must be refused");
        }
    }

    /// An event the department asked for, written by the core after a person said yes — the shape
    /// `proposals::create_calendar_event` established, with a department in place of the triage.
    #[tokio::test]
    async fn an_approved_calendar_event_is_written_by_the_core() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "calendar_event", "propose").await;
        let filed = ask(
            &state,
            &run_id,
            "calendar_event",
            serde_json::json!({
                "title": "launch review",
                "starts_at_local": "2026-08-17T09:30:00",
                "duration_minutes": 45,
                "tz": "Europe/Lisbon",
            }),
        )
        .await
        .unwrap();

        crate::proposals::transition(
            &state.pool,
            filed.proposal_id.unwrap(),
            "approved",
            "approved by user",
        )
        .await
        .unwrap();
        execute_due_actions(&state).await;

        let event: (String, String, i64, String) =
            sqlx::query_as("SELECT title, tz, duration_minutes, source FROM calendar_events")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            event,
            (
                "launch review".to_owned(),
                "Europe/Lisbon".to_owned(),
                45,
                "team".to_owned()
            )
        );
    }

    /// Only a department asks, and which one comes from the KEY. A `team_run_id` in the body would
    /// be the caller naming what it may do, which is what `post_read_file` refuses one route over.
    #[tokio::test]
    async fn only_a_team_run_may_ask_and_the_key_says_which() {
        let (state, _root) = state_with_root().await;
        team_with_grant(&state, "send_email", "allow").await;

        for scope in [Scope::Control, Scope::Run(1)] {
            let refusal = propose_action(
                &state,
                &scope,
                &axum::http::HeaderMap::new(),
                &ProposeActionRequest {
                    kind: "send_email".to_owned(),
                    payload: an_email(),
                    why: "because".to_owned(),
                },
            )
            .await;
            assert!(matches!(refusal, Err(ActionError::NotADepartment)));
        }

        let refusal = ask(&state, "never-existed", "send_email", an_email()).await;
        assert!(matches!(refusal, Err(ActionError::NoSuchRun)));
    }

    // -----------------------------------------------------------------------------------------
    // Recruitment
    // -----------------------------------------------------------------------------------------

    /// The header the daemon's own client sends, standing in for one node of a run.
    fn as_node(run_id: i64) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            crate::daemon_client::RUN_ID_HEADER,
            run_id.to_string().parse().unwrap(),
        );
        headers
    }

    /// Puts a director's node and one specialist's item in flight on the same run, and hands back
    /// the two run ids. Both nodes hold the identical team key — that is the point of the first
    /// test below.
    async fn director_and_specialist(state: &AppState, team_run_id: &str) -> (i64, i64) {
        let director_run: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('plan', 'running', 'team', '2026-08-16T10:00:00Z') RETURNING id",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE team_runs SET director_run_id = ?, director_node = 'planning' WHERE id = ?",
        )
        .bind(director_run)
        .bind(team_run_id)
        .execute(&state.pool)
        .await
        .unwrap();

        let specialist_run: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('work', 'running', 'team', '2026-08-16T10:00:00Z') RETURNING id",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state,
                                     run_id)
             VALUES (?, 1, 0, 'copywriter', 'draft the post', 'running', ?)",
        )
        .bind(team_run_id)
        .bind(specialist_run)
        .execute(&state.pool)
        .await
        .unwrap();

        (director_run, specialist_run)
    }

    /// A department that used its alçada can still be deleted afterwards.
    ///
    /// **A pre-existing bug, found by reading the schema while designing `team_notes`.**
    /// `team_actions.team_run_id` is `TEXT NOT NULL REFERENCES team_runs(id)` (0086), `storage.rs`
    /// runs with `foreign_keys` ON, and `delete_team_run` deletes `team_items` and `team_runs` and
    /// nothing else — so the DELETE fails with `FOREIGN KEY constraint failed` and the owner gets a
    /// 500 naming nothing. It bites only a run that actually asked for something, which is why it
    /// went unnoticed: a department that never used its alçada deletes fine.
    ///
    /// `team_notes` was designed around this rather than repeating it: it carries the same NOT NULL
    /// reference AND `delete_team_run` clears it, which is the whole bargain — the constraint is
    /// what makes forgetting loud, and it is only worth having if somebody remembers.
    ///
    /// The proposal is left behind on purpose, exactly as `runs.team_run_id` is nulled rather than
    /// cascaded: what a person was asked to approve is a fact about that person's queue, and it does
    /// not stop existing because the department that asked has been tidied away.
    #[tokio::test]
    async fn a_department_that_asked_for_something_can_still_be_deleted() {
        let (state, _root) = state_with_root().await;
        let id = team_with_grant(&state, "send_email", "propose").await;
        ask(&state, &id, "send_email", an_email())
            .await
            .expect("the alçada was granted");

        // Only a finished run may be deleted, which is the route's own first check.
        sqlx::query("UPDATE team_runs SET state = 'done' WHERE id = ?")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();

        let outcome = delete_team_run(State(state.clone()), Path(id.clone())).await;

        assert!(
            outcome.is_ok(),
            "a department that used its alçada could not be deleted: {:?}",
            outcome.err()
        );
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_runs WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    /// The delete is one transaction and the folder goes only after it commits. A statement that
    /// fails (a trigger stands in for any constraint) must leave the rows AND the folder as they were.
    #[tokio::test]
    async fn a_failed_delete_leaves_the_rows_and_the_folder() {
        let (state, _root) = state_with_root().await;
        let id = team_with_grant(&state, "send_email", "propose").await;
        ask(&state, &id, "send_email", an_email()).await.unwrap();
        sqlx::query("UPDATE team_runs SET state = 'done' WHERE id = ?")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        let workspace: String = sqlx::query_scalar("SELECT workspace FROM team_runs WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let folder =
            crate::files::resolve_within(&files_root(&state).unwrap(), &workspace).unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("keep.md"), "x").unwrap();
        sqlx::query(
            "CREATE TRIGGER refuse_run_delete BEFORE DELETE ON team_runs
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let outcome = delete_team_run(State(state.clone()), Path(id.clone())).await;

        assert!(outcome.is_err());
        assert!(
            folder.join("keep.md").exists(),
            "the folder survived a failed delete"
        );
        let actions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM team_actions WHERE team_run_id = ?")
                .bind(&id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(actions, 1, "the earlier statements rolled back with it");
    }

    /// Spec §4.4: `context_refs` is polymorphic (`owner_kind`, `owner_id`), so no foreign key can
    /// cascade it and deleting a team has to clear its own rows. A reference owned by one of its
    /// members is that agent's, and stays.
    #[tokio::test]
    async fn deleting_a_team_removes_its_context_refs() {
        let (state, _root) = state_with_root().await;
        let team = marketing(&state).await;
        for (kind, owner) in [("team", team.team.id.as_str()), ("agent", "copywriter")] {
            sqlx::query(
                "INSERT INTO context_refs (owner_kind, owner_id, path, kind, note, created_at)
                 VALUES (?, ?, 'notes/brief.md', 'file', NULL, '2026-10-08T00:00:00Z')",
            )
            .bind(kind)
            .bind(owner)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        delete(&state.pool, &team.team.id).await.unwrap();

        let team_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM context_refs WHERE owner_kind = 'team' AND owner_id = ?",
        )
        .bind(&team.team.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let agent_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM context_refs WHERE owner_kind = 'agent' AND owner_id = 'copywriter'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(team_rows, 0, "the deleted team kept its refs");
        assert_eq!(
            agent_rows, 1,
            "a member agent's refs were removed with the team"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Reporting to the owner
    // -----------------------------------------------------------------------------------------

    /// Reports as one node of a run, through the same surface the route uses.
    async fn report_from(
        state: &AppState,
        team_run_id: &str,
        run_id: i64,
        body: &str,
    ) -> Result<ReportResponse, ReportError> {
        report(
            state,
            &crate::auth::Scope::TeamRun(team_run_id.to_owned()),
            &as_node(run_id),
            &ReportRequest {
                body: body.to_owned(),
            },
        )
        .await
    }

    /// Starts a department pointed at a conversation, and hands back both ids.
    async fn run_reporting_into(state: &AppState) -> (String, String) {
        let chat_id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        let id = start_with(
            state,
            "marketing",
            "write the launch post",
            Lineage::default(),
            crate::speed::Speed::Normal,
            Some(&chat_id),
        )
        .await
        .unwrap();
        (id, chat_id)
    }

    /// The whole of part B: a director says something, and it is in the conversation, attributed.
    ///
    /// Asserted on `chat_notices` and on the unread count rather than on a return value, because
    /// what this feature promises is that a person sitting in that conversation is CALLED BACK to
    /// it. A row nobody is told about is the same as no row.
    #[tokio::test]
    async fn what_a_director_says_lands_in_the_conversation_and_calls_the_owner_back() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let (id, chat_id) = run_reporting_into(&state).await;
        let (director_run, _copywriter_run) = director_and_specialist(&state, &id).await;

        report_from(
            &state,
            &id,
            director_run,
            "a fonte que deste está morta desde Março",
        )
        .await
        .expect("a director may speak in the conversation its run was pointed at");

        let told = crate::chats::notices::for_chat(&state.pool, &chat_id)
            .await
            .unwrap();
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].body, "a fonte que deste está morta desde Março");
        assert_eq!(told[0].from_agent_id, "director");
        assert_eq!(told[0].team_run_id, id);

        assert_eq!(
            crate::chats::get(&state.pool, &chat_id)
                .await
                .unwrap()
                .expect("the conversation exists")
                .notices_waiting,
            1,
            "the department spoke and nothing called the owner back to the conversation"
        );

        // And nothing ran. This is the line that separates a report from a relay: `send_to_chat`
        // would have started a turn here, spent money, and been refused outright if the owner were
        // away.
        let turns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE chat_id = ? AND mode = 'assistant'",
        )
        .bind(&chat_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(turns, 0, "a report started a turn, which makes it a relay");
    }

    /// A specialist is refused, and told what to do instead.
    ///
    /// A department speaks to its owner with one voice: eight specialists reporting into a
    /// conversation are eight interruptions about one piece of work.
    #[tokio::test]
    async fn a_specialist_may_not_speak_for_the_department() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let (id, chat_id) = run_reporting_into(&state).await;
        let (_director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        let refusal = report_from(&state, &id, copywriter_run, "eu acho que...")
            .await
            .expect_err("a specialist spoke for the whole department");

        assert!(matches!(refusal, ReportError::NotTheDirector));
        assert!(
            refusal.to_string().contains("let the director pass it on"),
            "the refusal did not say what to do with the finding: {refusal}"
        );
        assert!(
            crate::chats::notices::for_chat(&state.pool, &chat_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A department nobody pointed anywhere is told so, plainly, and it is not an error.
    ///
    /// This is the ordinary state of nearly every run — `report_to_chat_id` is NULL unless somebody
    /// filled it — so the sentence matters more than the code: a model that reads it should write
    /// the words into its delivery, not retry.
    #[tokio::test]
    async fn a_department_pointed_nowhere_is_told_to_put_it_in_the_delivery() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (director_run, _copywriter_run) = director_and_specialist(&state, &id).await;

        let refusal = report_from(&state, &id, director_run, "olha lá")
            .await
            .expect_err("a department with no destination spoke somewhere");

        assert!(matches!(refusal, ReportError::NowhereToReport));
        assert!(
            refusal
                .to_string()
                .contains("put it in your delivery instead")
        );
    }

    /// Naming a conversation that does not exist is refused when the RUN is started.
    ///
    /// Refused there and not two hours later: the mistake belongs to whoever started the run, and
    /// they are at a keyboard now. A director discovering it mid-turn can do nothing about it.
    #[tokio::test]
    async fn a_department_cannot_be_pointed_at_a_conversation_that_is_not_there() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;

        let refusal = start_with(
            &state,
            "marketing",
            "write the launch post",
            Lineage::default(),
            crate::speed::Speed::Normal,
            Some("no-such-chat"),
        )
        .await
        .expect_err("a department was pointed at a conversation that does not exist");

        assert!(matches!(refusal, StartError::Invalid(_)));
        assert!(refusal.to_string().contains("no-such-chat"));
    }

    /// A conversation archived while the department worked is refused mid-run, on its own line.
    ///
    /// Distinct from `NowhereToReport` because the two lead somewhere different: never given a
    /// voice, versus given a room that has since been closed. A run lasts up to four hours and a
    /// conversation can be put away inside that, so this is not a hypothetical.
    #[tokio::test]
    async fn a_conversation_put_away_mid_run_is_named_as_such() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let (id, chat_id) = run_reporting_into(&state).await;
        let (director_run, _copywriter_run) = director_and_specialist(&state, &id).await;

        crate::chats::archive(&state.pool, &chat_id).await.unwrap();

        let refusal = report_from(&state, &id, director_run, "tarde demais")
            .await
            .expect_err("a department spoke into an archived conversation");

        assert!(matches!(refusal, ReportError::DestinationGone(_)));
        assert!(refusal.to_string().contains(&chat_id));
    }

    /// A department that only talks is stopped, and told where the rest goes.
    #[tokio::test]
    async fn a_department_may_not_fill_a_conversation_with_its_own_voice() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let (id, _chat_id) = run_reporting_into(&state).await;
        let (director_run, _copywriter_run) = director_and_specialist(&state, &id).await;

        for _ in 0..MAX_REPORTS_PER_RUN {
            report_from(&state, &id, director_run, "mais uma")
                .await
                .expect("under the ceiling");
        }

        let refusal = report_from(&state, &id, director_run, "e mais uma")
            .await
            .expect_err("a department spoke past its ceiling");
        assert!(matches!(refusal, ReportError::Ceiling(_)));
        assert!(
            refusal
                .to_string()
                .contains("put the rest in your delivery")
        );
    }

    /// A rule that fires when nobody is asking points its run at no conversation.
    ///
    /// The one place this could have gone wrong quietly: `team_trigger::fire` passes `None`, so a
    /// department started by a cron rule at four in the morning has nowhere to speak and says so —
    /// rather than inheriting an address from a conversation nobody is in.
    #[tokio::test]
    async fn a_triggered_run_speaks_nowhere() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start_with(
            &state,
            "marketing",
            "the nightly sweep",
            Lineage {
                trigger_id: Some(1),
                parent_id: None,
                root_id: None,
                depth: 1,
            },
            crate::speed::Speed::Normal,
            None,
        )
        .await
        .unwrap();

        let destination: Option<String> =
            sqlx::query_scalar("SELECT report_to_chat_id FROM team_runs WHERE id = ?")
                .bind(&id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(destination.is_none());
    }

    // -----------------------------------------------------------------------------------------
    // Notes between colleagues
    // -----------------------------------------------------------------------------------------

    /// The run row as `team_tick` reads it, so the launches below see what production sees.
    async fn load_run(state: &AppState, id: &str) -> TeamRun {
        sqlx::query_as(
            "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                    next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                    updated_at, finished_at, trigger_id, parent_id, root_id, depth
               FROM team_runs WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    /// Leaves a note as one node of a run, through the same surface the route uses.
    async fn note_from(
        state: &AppState,
        team_run_id: &str,
        run_id: i64,
        to: &str,
        body: &str,
    ) -> Result<TeamNoteResponse, NoteError> {
        send_note(
            state,
            &crate::auth::Scope::TeamRun(team_run_id.to_owned()),
            &as_node(run_id),
            &TeamNoteRequest {
                to: to.to_owned(),
                body: body.to_owned(),
            },
        )
        .await
    }

    /// The whole feature in one test: a specialist tells a colleague something, and the colleague's
    /// next brief carries it, word for word, with the sender's name on it.
    ///
    /// The delivery happens at `launch_specialist`, so what this asserts is the PROMPT the daemon
    /// actually wrote to `runs.prompt` — not that a row exists. A row that nothing reads out is the
    /// failure this feature is most likely to have and the one hardest to see.
    #[tokio::test]
    async fn what_one_specialist_tells_another_is_at_the_top_of_their_next_brief() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (_director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        note_from(
            &state,
            &id,
            copywriter_run,
            "researcher",
            "o preço mudou na terça, a página antiga está errada",
        )
        .await
        .expect("a colleague on the roster can be told");

        // The researcher is queued and then launched, which is the seam the delivery hangs off.
        let run = load_run(&state, &id).await;
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state)
             VALUES (?, 2, 0, 'researcher', 'check the pricing page', 'pending')",
        )
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
        let item: TeamItem = sqlx::query_as(
            "SELECT ordinal, round, agent_id, description, state, run_id, output_path
               FROM team_items WHERE team_run_id = ? AND ordinal = 2",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();

        launch_specialist(&state, &run, &item).await.unwrap();

        let prompt: String = sqlx::query_scalar(
            "SELECT prompt FROM runs WHERE team_run_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(
            prompt.contains("o preço mudou na terça, a página antiga está errada"),
            "the note never reached the colleague's brief: {prompt}"
        );
        assert!(
            prompt.contains("copywriter"),
            "the note arrived unattributed, which makes it indistinguishable from the brief"
        );
    }

    /// A node handed a colleague's words is born having read them, and may therefore no longer ask.
    ///
    /// **This is the governance decision of the whole feature, and it is asserted on the ROW rather
    /// than on the tool**, because the marking is what `hooks::team_decision` reads and the tool
    /// refusal is tested there. Writing a note is `WritesOwn` so a specialist that read the web can
    /// still tell a colleague what it found; the taint travels WITH the words and lands here. The
    /// cost is real and deliberate: this node has lost its alçada for this turn.
    ///
    /// The second half — a node with no mail is untouched — is the more important of the two. Every
    /// department that never uses this feature has to keep the authority it has today, and a
    /// marking applied unconditionally would take the alçada away from all of them.
    #[tokio::test]
    async fn a_node_handed_a_colleagues_words_is_born_having_read_them() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (_director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        let run = load_run(&state, &id).await;

        // First: a colleague with no mail, launched exactly as today.
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state)
             VALUES (?, 2, 0, 'researcher', 'check the pricing page', 'pending')",
        )
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
        let item: TeamItem = sqlx::query_as(
            "SELECT ordinal, round, agent_id, description, state, run_id, output_path
               FROM team_items WHERE team_run_id = ? AND ordinal = 2",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        launch_specialist(&state, &run, &item).await.unwrap();
        let clean: i64 = sqlx::query_scalar(
            "SELECT read_untrusted FROM runs WHERE team_run_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            clean, 0,
            "a department that leaves no notes lost its alçada anyway"
        );

        // Then: the same agent, with a colleague's words waiting.
        note_from(
            &state,
            &id,
            copywriter_run,
            "researcher",
            "lê isto primeiro",
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state)
             VALUES (?, 3, 1, 'researcher', 'check it again', 'pending')",
        )
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
        let item: TeamItem = sqlx::query_as(
            "SELECT ordinal, round, agent_id, description, state, run_id, output_path
               FROM team_items WHERE team_run_id = ? AND ordinal = 3",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        launch_specialist(&state, &run, &item).await.unwrap();

        let (tainted, prompt): (i64, String) = sqlx::query_as(
            "SELECT read_untrusted, prompt FROM runs WHERE team_run_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(
            prompt.contains("lê isto primeiro"),
            "the note did not travel"
        );
        assert_eq!(
            tainted, 1,
            "a node was handed a colleague's words and can still act on them"
        );
    }

    /// The note is signed by the node that made the call, never by anything in the body.
    ///
    /// There is no `from` field to test the absence of — the point is that the identity comes from
    /// `RUN_ID_HEADER`, which the local client fills from an id the calling process cannot read, let
    /// alone alter. What this pins is that the resolution WORKS: a specialist's note is signed with
    /// that specialist's `agents.id` and not with the department's name or nothing at all.
    #[tokio::test]
    async fn a_note_is_signed_by_the_node_that_made_the_call() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        note_from(&state, &id, copywriter_run, "researcher", "do especialista")
            .await
            .unwrap();
        note_from(&state, &id, director_run, "researcher", "do director")
            .await
            .unwrap();

        let signatures: Vec<(String, String)> =
            sqlx::query_as("SELECT from_agent_id, body FROM team_notes ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            signatures,
            vec![
                ("copywriter".to_owned(), "do especialista".to_owned()),
                ("director".to_owned(), "do director".to_owned()),
            ]
        );
    }

    /// A specialist tells the DIRECTOR something, and the director's next node reads it.
    ///
    /// The most valuable message this feature carries, and the one an earlier draft of `send_note`
    /// refused: it checked `roster`, which is `team_members` and does not hold the director. This
    /// exercises the other launch path too — `launch_director` — which nothing else here covers.
    #[tokio::test]
    async fn a_specialist_can_tell_the_director_and_the_director_reads_it() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (_director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        note_from(
            &state,
            &id,
            copywriter_run,
            "director",
            "vale a pena replanear: metade do que pediste já está feito noutro sítio",
        )
        .await
        .expect("the director is addressable by one of its own specialists");

        // A replanning node, launched the way a finished round launches one. The marker written by
        // `director_and_specialist` is cleared first, because `launch_director` writes its own.
        sqlx::query(
            "UPDATE team_runs SET director_node = 'none', director_run_id = NULL WHERE id = ?",
        )
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
        let run = load_run(&state, &id).await;
        launch_director(&state, &run, DirectorNode::Replanning)
            .await
            .unwrap();

        let (prompt, tainted): (String, i64) = sqlx::query_as(
            "SELECT prompt, read_untrusted FROM runs WHERE team_run_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(
            prompt.contains("vale a pena replanear"),
            "the director replanned without the finding that should have changed the plan: {prompt}"
        );
        assert_eq!(
            tainted, 1,
            "the director was handed a specialist's words and can still file proposals on them"
        );
    }

    /// A node the run does not recognise is refused rather than allowed to write anonymously.
    ///
    /// The safe direction and the honest one: an unattributed paragraph appended to a colleague's
    /// brief is a second brief with no way to weigh it against the first, and the receiving node
    /// cannot recover from that — it has only the text.
    #[tokio::test]
    async fn a_node_nobody_recognises_may_not_leave_an_unsigned_note() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        director_and_specialist(&state, &id).await;

        let refusal = note_from(&state, &id, 999_999, "researcher", "de quem?").await;

        assert!(matches!(refusal, Err(NoteError::UnknownNode)));
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_notes")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "an unsigned note was filed anyway");
    }

    /// Naming somebody who is not in the department is refused, and the refusal names the roster.
    ///
    /// The naming is the point rather than politeness. The caller is a model, mid-turn, that has the
    /// roster in its prompt and got a name slightly wrong; "no" sends it to guess again, and the
    /// list lets it fix the call it already meant to make.
    #[tokio::test]
    async fn a_name_off_the_roster_is_refused_with_the_roster_in_the_answer() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (_director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        let refusal = note_from(&state, &id, copywriter_run, "lawyer", "olá")
            .await
            .expect_err("somebody outside the department was reachable");

        let said = refusal.to_string();
        assert!(
            said.contains("lawyer"),
            "the refusal did not say what was asked for"
        );
        assert!(
            said.contains("copywriter") && said.contains("researcher") && said.contains("director"),
            "the refusal did not name the department: {said}"
        );
    }

    /// Writing to yourself is refused. Harmless, and refused anyway.
    ///
    /// The words would arrive at the top of the caller's OWN next brief, attributed to a colleague
    /// who is the caller — a turn quoting itself back and weighing it as somebody else's finding.
    #[tokio::test]
    async fn a_member_may_not_leave_a_note_for_itself() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (_director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        let refusal = note_from(&state, &id, copywriter_run, "copywriter", "nota para mim").await;

        assert!(matches!(refusal, Err(NoteError::Yourself)));
    }

    /// The ceiling is per RUN and counts everybody, because what it protects against is a department
    /// talking instead of working.
    #[tokio::test]
    async fn a_department_that_only_talks_is_stopped_at_the_ceiling() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        // Two senders, alternating, so what is exercised is the RUN's total and not one member's.
        for turn in 0..crate::team_notes::MAX_NOTES_PER_RUN {
            let (from, to) = if turn % 2 == 0 {
                (copywriter_run, "researcher")
            } else {
                (director_run, "copywriter")
            };
            note_from(&state, &id, from, to, "mais uma")
                .await
                .expect("under the ceiling");
        }

        let refusal = note_from(&state, &id, copywriter_run, "researcher", "e mais uma").await;
        assert!(matches!(refusal, Err(NoteError::Ceiling(_))));
        // And the refusal tells the model where to put it instead, which is the difference between
        // a ceiling and a wall.
        assert!(
            refusal
                .unwrap_err()
                .to_string()
                .contains("put it in your answer instead"),
            "the ceiling refused without saying what to do with the finding"
        );
    }

    /// What nobody was ever told reaches the delivery, rather than disappearing with the run.
    #[tokio::test]
    async fn words_nobody_read_are_handed_to_the_delivery() {
        let (state, _root) = state_with_root().await;
        let team = marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (_director_run, copywriter_run) = director_and_specialist(&state, &id).await;

        note_from(
            &state,
            &id,
            copywriter_run,
            "researcher",
            "a fonte de 2019 está morta",
        )
        .await
        .unwrap();

        let run = load_run(&state, &id).await;
        let prompt = delivery_prompt(&state, &run, &team.team).await;

        assert!(
            prompt.contains("a fonte de 2019 está morta"),
            "a finding the department paid for was dropped when the run ended: {prompt}"
        );
    }

    /// A specialist is told who its colleagues are, which is what makes anybody addressable at all.
    #[tokio::test]
    async fn a_specialist_is_told_who_else_is_in_the_department() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let run = load_run(&state, &id).await;
        let item = TeamItem {
            ordinal: 1,
            round: 0,
            agent_id: "copywriter".to_owned(),
            description: "draft the post".to_owned(),
            state: "pending".to_owned(),
            run_id: None,
            output_path: None,
        };

        let prompt = specialist_prompt(&state, &run, "sell the thing", &item).await;

        assert!(
            prompt.contains("researcher"),
            "no colleague was nameable: {prompt}"
        );
        assert!(
            prompt.contains("send_team_note"),
            "nothing said how to reach them"
        );
    }

    fn a_lawyer() -> RecruitRequest {
        RecruitRequest {
            name: "Contracts lawyer".to_owned(),
            speciality: "reads contracts and flags what binds us".to_owned(),
            prompt: "You are a lawyer. Be exact about obligations.".to_owned(),
            engine: None,
            model: None,
            tool_policy: None,
            why: "the launch has a distribution agreement nobody here can read".to_owned(),
            suggest_only: false,
        }
    }

    /// A runner fronted by a stub model adviser answering `claude-opus-5`, with every surface
    /// named in `surfaces` at its mode. Returns what the stub was asked.
    async fn advised_runner(
        state: &mut AppState,
        surfaces: &[(&'static str, crate::models::route_advice::Mode)],
    ) -> tokio::sync::mpsc::UnboundedReceiver<serde_json::Value> {
        let (url, sent) = crate::router_client::test_support::stub_router(
            200,
            serde_json::json!({
                "decision_id": "rt_team", "runner": "claude", "model": "claude-opus-5",
                "effort": "high", "estimated_cost_usd": 0.2, "rule": "hard",
            }),
        )
        .await;
        let config = crate::models::route_advice::RouterConfig {
            mode: crate::models::route_advice::Mode::Off,
            url,
            surfaces: surfaces.iter().copied().collect(),
            ..crate::models::route_advice::RouterConfig::off()
        };
        let inner = state.runner.clone();
        let router = std::sync::Arc::new(crate::models::route_advice::Router::new(
            config,
            crate::models::route_advice::Available {
                kind: crate::models::route_advice::RunnerKind::Claude,
                runner: inner.clone(),
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ));
        state.runner =
            std::sync::Arc::new(crate::models::route_advice::RoutedRunner { inner, router });
        sent
    }

    /// `suggest_model` answers only the director -- the same check, the same sentence -- and files
    /// nothing either way.
    #[tokio::test]
    async fn only_the_director_is_told_which_model_to_recruit() {
        let (mut state, _root) = state_with_root().await;
        let mut sent = advised_runner(
            &mut state,
            &[("recruit", crate::models::route_advice::Mode::Shadow)],
        )
        .await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, specialist_run) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());

        let refused = suggest_model(&state, &scope, &as_node(specialist_run), &a_lawyer()).await;
        assert!(
            matches!(refused, Err(RecruitError::NotTheDirector)),
            "got {refused:?}"
        );
        assert!(
            sent.try_recv().is_err(),
            "a specialist's question reached the adviser"
        );

        let answered = suggest_model(&state, &scope, &as_node(director_run), &a_lawyer())
            .await
            .unwrap();
        assert_eq!(answered["model"], "claude-opus-5");
        assert_eq!(answered["rule"], "hard");
        assert!(sent.try_recv().is_ok());
        assert!(
            !crate::proposals::recruit_pending_for(&state.pool, "contracts-lawyer")
                .await
                .unwrap(),
            "a question about a model filed a recruit"
        );
    }

    /// Under `recruit: apply` a recruit that named no model gets the adviser's; one that named a
    /// model keeps it and nothing is asked.
    #[tokio::test]
    async fn a_recruit_with_no_model_takes_the_advice_under_apply() {
        let (mut state, _root) = state_with_root().await;
        let mut sent = advised_runner(
            &mut state,
            &[("recruit", crate::models::route_advice::Mode::Apply)],
        )
        .await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());

        let named = RecruitRequest {
            name: "Tax adviser".to_owned(),
            model: Some("claude-haiku-5".to_owned()),
            ..a_lawyer()
        };
        let filed = propose_teammate(&state, &scope, &as_node(director_run), &named)
            .await
            .unwrap();
        assert!(sent.try_recv().is_err(), "a named model was second-guessed");
        let hired = approve_recruit(&state.pool, filed.proposal_id, None)
            .await
            .unwrap();
        let agent = crate::agent::get(&state.pool, &hired)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(agent.model.as_deref(), Some("claude-haiku-5"));

        let filed = propose_teammate(&state, &scope, &as_node(director_run), &a_lawyer())
            .await
            .unwrap();
        let body = sent.try_recv().expect("the adviser was asked");
        assert_eq!(body["runners"], serde_json::json!(["claude"]));
        let hired = approve_recruit(&state.pool, filed.proposal_id, None)
            .await
            .unwrap();
        let agent = crate::agent::get(&state.pool, &hired)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(agent.model.as_deref(), Some("claude-opus-5"));
    }

    /// A recruit that is refused anyway — no `why`, or a request `validate_request` rejects — is
    /// refused before the adviser hears anything: the task text is not sent for nothing.
    #[tokio::test]
    async fn a_recruit_refused_on_its_own_terms_never_reaches_the_adviser() {
        let (mut state, _root) = state_with_root().await;
        let mut sent = advised_runner(
            &mut state,
            &[("recruit", crate::models::route_advice::Mode::Apply)],
        )
        .await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());

        let no_why = RecruitRequest {
            why: "   ".to_owned(),
            ..a_lawyer()
        };
        let unrestricted = RecruitRequest {
            tool_policy: Some("unrestricted".to_owned()),
            ..a_lawyer()
        };
        for refused in [no_why, unrestricted] {
            let answer = propose_teammate(&state, &scope, &as_node(director_run), &refused).await;
            assert!(
                matches!(answer, Err(RecruitError::Invalid(_))),
                "got {answer:?}"
            );
            assert!(sent.try_recv().is_err(), "the adviser was asked");
        }
    }

    /// **The test that proves the director check works without inventing a scope.**
    ///
    /// The two nodes present the IDENTICAL key — a team's token names the run, not the node — and
    /// get different answers, because the answer comes from `team_runs.director_run_id` rather than
    /// from the credential. A specialist is refused with a sentence telling it what to do instead.
    #[tokio::test]
    async fn only_the_director_recruits_and_a_specialist_hears_why_not() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, specialist_run) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());

        let refusal = propose_teammate(&state, &scope, &as_node(specialist_run), &a_lawyer()).await;
        assert!(
            matches!(&refusal, Err(RecruitError::NotTheDirector)),
            "got {refusal:?}"
        );
        assert!(
            refusal
                .unwrap_err()
                .to_string()
                .contains("say in your answer"),
            "a specialist is told what to do instead of only being told no"
        );

        // A node this run does not know — a stale id, or one from another department — holds the
        // LEAST authority rather than the most.
        let stranger = propose_teammate(&state, &scope, &as_node(9_999), &a_lawyer()).await;
        assert!(matches!(stranger, Err(RecruitError::NotTheDirector)));
        let headerless =
            propose_teammate(&state, &scope, &axum::http::HeaderMap::new(), &a_lawyer()).await;
        assert!(matches!(headerless, Err(RecruitError::NotTheDirector)));

        // The same key, the same route, the same body — and the director is heard.
        let filed = propose_teammate(&state, &scope, &as_node(director_run), &a_lawyer())
            .await
            .expect("the director may ask");
        assert!(filed.outcome.contains("will not join this run"));
    }

    /// The catalogue and the roster move together or not at all. An agent hired into nothing is
    /// somebody nobody asked for; a roster row pointing at nobody breaks the foreign key at the
    /// next run's `start`, which is the worst place to find out.
    #[tokio::test]
    async fn hiring_writes_the_agent_and_the_roster_together() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let filed = propose_teammate(
            &state,
            &Scope::TeamRun(run_id.clone()),
            &as_node(director_run),
            &a_lawyer(),
        )
        .await
        .unwrap();

        let hired = approve_recruit(&state.pool, filed.proposal_id, None)
            .await
            .unwrap();
        assert_eq!(hired, "contracts-lawyer");
        let agent = crate::agent::get(&state.pool, &hired)
            .await
            .unwrap()
            .unwrap();
        // Engine defaults to the DIRECTOR's, which is the only defensible guess: a specialist of a
        // department that runs on one engine running on the same surprises nobody.
        assert_eq!(agent.engine, "claude");
        // Through the view, because the tests have a `roster` of their own that shadows the module
        // function — and the view is what the shell reads anyway.
        assert!(
            get(&state.pool, "marketing")
                .await
                .unwrap()
                .unwrap()
                .members
                .contains(&hired)
        );
        assert_eq!(
            crate::proposals::get(&state.pool, filed.proposal_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "approved"
        );

        // A second approval finds nothing left to decide, so nobody is hired twice.
        assert!(matches!(
            approve_recruit(&state.pool, filed.proposal_id, None).await,
            Err(HireError::NotPending)
        ));
    }

    /// Asked once, stopped twice — by the tool, and by the prompt that keeps the director from
    /// walking into the tool's refusal every round.
    #[tokio::test]
    async fn a_director_does_not_ask_for_the_same_person_twice() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());
        propose_teammate(&state, &scope, &as_node(director_run), &a_lawyer())
            .await
            .unwrap();

        let again = propose_teammate(&state, &scope, &as_node(director_run), &a_lawyer()).await;
        assert!(
            matches!(&again, Err(RecruitError::AlreadyAsked(id)) if id == "contracts-lawyer"),
            "got {again:?}"
        );

        let run: TeamRun = sqlx::query_as(
            "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                    next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                    updated_at, finished_at, trigger_id, parent_id, root_id, depth
             FROM team_runs WHERE id = ?",
        )
        .bind(&run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let team = get(&state.pool, "marketing").await.unwrap().unwrap();
        let prompt = director_prompt(&state, &run, &team.team).await;
        assert!(prompt.contains("already asked the owner for"), "{prompt}");
        assert!(prompt.contains("contracts-lawyer"));
        assert!(prompt.contains("Do not ask again"));
        // And the delivery is told to say what was thin, which is the difference between a bad
        // answer and an honest one.
        let delivery = delivery_prompt(&state, &run, &team.team).await;
        assert!(delivery.contains("which part is thin"), "{delivery}");
    }

    /// A department that asked for nobody has the prompt it always had, byte for byte.
    #[tokio::test]
    async fn a_department_that_asked_for_nobody_reads_the_prompt_it_always_did() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let run: TeamRun = sqlx::query_as(
            "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                    next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                    updated_at, finished_at, trigger_id, parent_id, root_id, depth
             FROM team_runs WHERE id = ?",
        )
        .bind(&run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let team = get(&state.pool, "marketing").await.unwrap().unwrap();

        let prompt = director_prompt(&state, &run, &team.team).await;
        assert!(!prompt.contains("already asked"));
        assert!(prompt.trim_end().ends_with("for the person who asked."));
    }

    /// A name already in the catalogue gets an answer the director can act on in the same turn,
    /// rather than a refusal it can only report.
    #[tokio::test]
    async fn a_name_already_in_the_catalogue_says_to_add_them_instead() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let mut asked = a_lawyer();
        asked.name = "Copywriter".to_owned();

        let refusal = propose_teammate(
            &state,
            &Scope::TeamRun(run_id),
            &as_node(director_run),
            &asked,
        )
        .await;
        assert!(
            matches!(&refusal, Err(RecruitError::AlreadyExists(id)) if id == "copywriter"),
            "got {refusal:?}"
        );
        assert!(
            refusal
                .unwrap_err()
                .to_string()
                .contains("add them to this team")
        );
    }

    /// The proposal is a suggestion; what the owner approved is the authority.
    #[tokio::test]
    async fn what_is_hired_is_what_the_owner_approved_and_not_what_was_proposed() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());

        // Refused at the PROPOSAL too, so the director learns while it can still act rather than
        // filing something that could only ever have been refused.
        let mut greedy = a_lawyer();
        greedy.tool_policy = Some("unrestricted".to_owned());
        assert!(matches!(
            propose_teammate(&state, &scope, &as_node(director_run), &greedy).await,
            Err(RecruitError::Invalid(_))
        ));

        let filed = propose_teammate(&state, &scope, &as_node(director_run), &a_lawyer())
            .await
            .unwrap();
        // The last word is over what was EDITED. An owner who edits it into something the daemon
        // refuses is refused there, and the question stays open for them to correct.
        let bad = approve_recruit(
            &state.pool,
            filed.proposal_id,
            Some(crate::agent::AgentRequest {
                name: "Contracts lawyer".to_owned(),
                speciality: "reads contracts".to_owned(),
                prompt: "You are a lawyer.".to_owned(),
                engine: "local".to_owned(),
                model: None,
                tool_policy: "mcp_only".to_owned(),
            }),
        )
        .await;
        assert!(matches!(bad, Err(HireError::Refused(_))), "{bad:?}");
        assert_eq!(
            crate::proposals::get(&state.pool, filed.proposal_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "pending"
        );

        let hired = approve_recruit(
            &state.pool,
            filed.proposal_id,
            Some(crate::agent::AgentRequest {
                name: "House counsel".to_owned(),
                speciality: "reads contracts".to_owned(),
                prompt: "You are a lawyer.".to_owned(),
                engine: "claude".to_owned(),
                model: Some("claude-sonnet-5".to_owned()),
                tool_policy: "none".to_owned(),
            }),
        )
        .await
        .unwrap();
        let agent = crate::agent::get(&state.pool, &hired)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(agent.name, "House counsel");
        assert_eq!(agent.tool_policy, "none");
    }

    /// Refusing is "not this one", never "stop asking". The next run of that department meets the
    /// same gap and may say so again.
    #[tokio::test]
    async fn a_refused_recruitment_may_be_asked_for_again() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let run_id = start(&state, "marketing", "prepare the launch", None)
            .await
            .unwrap();
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());
        let filed = propose_teammate(&state, &scope, &as_node(director_run), &a_lawyer())
            .await
            .unwrap();

        crate::proposals::transition(&state.pool, filed.proposal_id, "rejected", "not hired")
            .await
            .unwrap();
        assert_eq!(
            crate::agent::get(&state.pool, "contracts-lawyer")
                .await
                .unwrap(),
            None,
            "refusing leaves nothing behind, because nothing was created"
        );

        propose_teammate(&state, &scope, &as_node(director_run), &a_lawyer())
            .await
            .expect("nobody told the director to stop asking, only not that one");
    }

    /// A `propose` action reaches the person's queue, so the proposals topic is signalled once the
    /// transaction is committed.
    #[tokio::test]
    async fn wake_signal_team_action_proposal_bumps_proposals() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "send_email", "propose").await;
        let (director_run, _) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());
        let mut rx = nucleos_base::wake::subscribe(nucleos_base::wake::Topic::Proposals);
        let before = *rx.borrow_and_update();

        propose_action(
            &state,
            &scope,
            &as_node(director_run),
            &ProposeActionRequest {
                kind: "send_email".to_owned(),
                payload: an_email(),
                why: "the list asked to be told".to_owned(),
            },
        )
        .await
        .unwrap();

        // Other tests of this binary share the global bus: assert a lower bound only.
        assert!(rx.borrow().wrapping_sub(before) >= 1);
    }

    /// Attribution, from the same header and by the same rule: the item that asked is recorded, the
    /// director's own request is recorded as nobody's item, and a node this run does not know is
    /// recorded as neither rather than as a guess.
    #[tokio::test]
    async fn an_action_records_which_node_asked_for_it() {
        let (state, _root) = state_with_root().await;
        let run_id = team_with_grant(&state, "send_email", "allow").await;
        let (director_run, specialist_run) = director_and_specialist(&state, &run_id).await;
        let scope = Scope::TeamRun(run_id.clone());

        let ask_as = async |headers: axum::http::HeaderMap| {
            propose_action(
                &state,
                &scope,
                &headers,
                &ProposeActionRequest {
                    kind: "send_email".to_owned(),
                    payload: an_email(),
                    why: "the list asked to be told".to_owned(),
                },
            )
            .await
            .unwrap()
            .id
        };

        assert_eq!(
            calling_node(&state.pool, &run_id, &as_node(specialist_run)).await,
            Caller::Specialist(1)
        );
        let by_specialist = ask_as(as_node(specialist_run)).await;
        let by_director = ask_as(as_node(director_run)).await;
        let by_stranger = ask_as(as_node(9_999)).await;

        let ordinal = async |id: i64| -> Option<i64> {
            sqlx::query_scalar("SELECT ordinal FROM team_actions WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap()
        };
        assert_eq!(ordinal(by_specialist).await, Some(1));
        assert_eq!(ordinal(by_director).await, None);
        assert_eq!(ordinal(by_stranger).await, None);
    }

    /// A team halfway through being assembled is a legitimate thing to save; asking it to work is
    /// not. The two refusals live at different moments on purpose.
    #[tokio::test]
    async fn an_empty_roster_saves_and_refuses_to_start() {
        let (state, _root) = state_with_root().await;
        insert_agent(&state, "director", "claude").await;
        create(
            &state.pool,
            TeamRequest {
                name: "Empty".to_owned(),
                mission: "nothing yet".to_owned(),
                director_agent_id: "director".to_owned(),
                max_rounds: 2,
                max_parallel: 1,
                budget_usd: None,
                max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: Vec::new(),
            },
        )
        .await
        .expect("a half-built team is a legitimate thing to save");

        let refusal = start(&state, "empty", "do some work", None).await;
        assert!(
            matches!(&refusal, Err(StartError::Invalid(why)) if why.contains("no members")),
            "got {refusal:?}"
        );
    }

    /// Not silently degraded to the cloud, which would swap the model the owner chose for one that
    /// spends.
    #[tokio::test]
    async fn a_local_member_on_a_machine_with_no_local_model_refuses_at_the_start() {
        let (state, _root) = state_with_root().await;
        insert_agent(&state, "director", "claude").await;
        insert_agent(&state, "localist", "local").await;
        create(
            &state.pool,
            TeamRequest {
                name: "Local".to_owned(),
                mission: "stay on this machine".to_owned(),
                director_agent_id: "director".to_owned(),
                max_rounds: 2,
                max_parallel: 1,
                budget_usd: None,
                max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: vec!["localist".to_owned()],
            },
        )
        .await
        .unwrap();

        let refusal = start(&state, "local", "do some work", None).await;
        assert!(
            matches!(&refusal, Err(StartError::Invalid(why)) if why.contains("local model")),
            "got {refusal:?}"
        );
    }

    /// A department's whole output is files in a folder, so a machine without one has nowhere to
    /// put the answer. The refusal is what makes the shared root worth sharing: this pillar and the
    /// Files tab read the same `AppState.files_root` through the same helper, and an installation
    /// missing it must say the same thing to both rather than half-starting a run whose deliverable
    /// has no home. Refusing here also means no row, no key and no charge.
    #[tokio::test]
    async fn a_machine_with_no_files_folder_refuses_to_start_a_department() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let state = AppState {
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            ..state
        };

        let refusal = start(&state, "marketing", "write the launch post", None).await;
        assert!(
            matches!(&refusal, Err(StartError::Unavailable(why)) if why.contains("files folder")),
            "got {refusal:?}"
        );
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "a refused start must not leave a run behind");
    }

    #[tokio::test]
    async fn starting_a_run_writes_the_row_the_key_and_the_folder() {
        let (state, root) = state_with_root().await;
        marketing(&state).await;

        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let run: TeamRun = sqlx::query_as(
            "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                    next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                    updated_at, finished_at, trigger_id, parent_id, root_id, depth
             FROM team_runs WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();

        assert_eq!(run.state, "planning");
        assert_eq!(run.director_node, "none");
        assert_eq!(run.round, 0);
        assert_eq!(run.next_ordinal, 1);
        assert_eq!(run.workspace, format!("teams/marketing/{id}"));
        assert!(
            std::fs::canonicalize(root.path())
                .unwrap()
                .join("teams")
                .join("marketing")
                .join(&id)
                .is_dir(),
            "the folder must exist before any specialist tries to be filed into it"
        );

        // The key resolves, which is the whole point of minting it here.
        let token = team_token(&state.pool, &id).await.unwrap();
        assert!(token.starts_with(&format!("team:{id}.")));
    }

    /// The ordinal is monotonic across the WHOLE run and never restarts per round, which is what
    /// makes two files unable to collide — in one round or between rounds.
    #[tokio::test]
    async fn an_ordinal_is_unique_across_the_whole_run() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        for round in 0..3 {
            for _ in 0..2 {
                let next: i64 =
                    sqlx::query_scalar("SELECT next_ordinal FROM team_runs WHERE id = ?")
                        .bind(&id)
                        .fetch_one(&state.pool)
                        .await
                        .unwrap();
                sqlx::query(
                    "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description,
                                             state)
                     VALUES (?, ?, ?, 'copywriter', 'work', 'pending')",
                )
                .bind(&id)
                .bind(next)
                .bind(round)
                .execute(&state.pool)
                .await
                .unwrap();
                sqlx::query("UPDATE team_runs SET next_ordinal = ? WHERE id = ?")
                    .bind(next + 1)
                    .bind(&id)
                    .execute(&state.pool)
                    .await
                    .unwrap();
            }
        }

        let ordinals: Vec<i64> = sqlx::query_scalar(
            "SELECT ordinal FROM team_items WHERE team_run_id = ? ORDER BY ordinal",
        )
        .bind(&id)
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(ordinals, vec![1, 2, 3, 4, 5, 6]);

        let names: std::collections::BTreeSet<String> = ordinals
            .iter()
            .map(|ordinal| format!("{ordinal}-copywriter.md"))
            .collect();
        assert_eq!(
            names.len(),
            ordinals.len(),
            "two rounds wrote the same file"
        );
    }

    /// A run in `working` whose items were left `running` by a dead daemon is unwedged at startup;
    /// its own items are the only ones touched.
    #[tokio::test]
    async fn reconciliation_fails_the_items_a_dead_daemon_abandoned() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let (run_id, _) = open_run(&state, &id, "work", false).await.unwrap();
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state,
                                     run_id)
             VALUES (?, 1, 0, 'copywriter', 'work', 'running', ?)",
        )
        .bind(&id)
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE team_runs SET state = 'working' WHERE id = ?")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();

        reconcile_orphaned_team_runs(&state).await.unwrap();

        let item_state: String = sqlx::query_scalar(
            "SELECT state FROM team_items WHERE team_run_id = ? AND ordinal = 1",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(item_state, "failed");
    }

    /// The case that is easiest to implement wrongly and the only one that throws away money: the
    /// daemon died AFTER the CLI wrote the director's answer. Resetting the marker would repeat a
    /// node that has already been paid for.
    #[tokio::test]
    async fn a_director_node_that_completed_unread_is_ingested_and_not_repeated() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let (run_id, _) = open_run(&state, &id, "plan it", false).await.unwrap();
        sqlx::query("UPDATE runs SET status = 'completed', exit_code = 0, stdout = ? WHERE id = ?")
            .bind(r#"{"items":[{"agent_id":"copywriter","description":"draft the post"}]}"#)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE team_runs SET director_node = 'planning', director_run_id = ? WHERE id = ?",
        )
        .bind(run_id)
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();

        reconcile_orphaned_team_runs(&state).await.unwrap();

        let run: TeamRun = fetch_run(&state, &id).await;
        assert_eq!(
            run.state, "working",
            "the paid-for plan must have been read"
        );
        assert_eq!(run.director_node, "none");
        assert_eq!(run.next_ordinal, 2);

        let queued: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM team_items WHERE team_run_id = ?")
                .bind(&id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(queued, 1);
    }

    /// The assertion `job.rs` bought with migration 0051: one node ingested twice must not advance
    /// the round twice. The marker is cleared in the same statement that records the move, and the
    /// `WHERE director_node = ?` guard is what makes the second call a no-op.
    #[tokio::test]
    async fn ingesting_the_same_director_node_twice_does_not_advance_the_round_twice() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        sqlx::query("UPDATE team_runs SET state = 'working', round = 0 WHERE id = ?")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();

        let (run_id, _) = open_run(&state, &id, "replan", false).await.unwrap();
        sqlx::query("UPDATE runs SET status = 'completed', exit_code = 0, stdout = ? WHERE id = ?")
            .bind(r#"{"items":[{"agent_id":"copywriter","description":"another draft"}]}"#)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE team_runs SET director_node = 'replanning', director_run_id = ? WHERE id = ?",
        )
        .bind(run_id)
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();

        let before = fetch_run(&state, &id).await;
        ingest_director(&state, before.clone()).await.unwrap();
        let once = fetch_run(&state, &id).await;
        assert_eq!(once.round, 1);

        // The stale row again, exactly as a second caller would hold it.
        ingest_director(&state, before).await.unwrap();
        let twice = fetch_run(&state, &id).await;
        assert_eq!(
            twice.round, 1,
            "the round moved twice for one paid-for node"
        );
        assert_eq!(
            twice.next_ordinal, once.next_ordinal,
            "the items were queued twice"
        );
    }

    /// A team item is the ONE kind of run the pressure reading is about, and this path is its own
    /// launcher -- it never touches `runs::spawn_run`. Until this test, every column that reading
    /// depends on came back NULL for exactly the runs it was built to measure.
    #[tokio::test]
    async fn a_specialists_run_records_the_window_it_cost_and_whether_it_compacted() {
        let (mut state, _root) = state_with_root().await;
        // Climbs to 190k, compacts, ends at 40k -- two tool calls along the way.
        state.runner = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(crate::execution::runner::RunOutcome {
                exit_code: 0,
                stdout: [
                    r#"{"type":"assistant","message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":189000},"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
                    r#"{"type":"assistant","message":{"usage":{"input_tokens":500,"cache_read_input_tokens":39500},"content":[{"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}}]}}"#,
                ]
                .join("\n"),
                stderr: String::new(),
                session_id: Some("team-pressure-session".into()),
                cost_usd: Some(0.03),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: Some(2),
                compacted: true,
                model_requested: false,
            })),
            ..Default::default()
        });
        marketing(&state).await;
        sqlx::query(
            "INSERT INTO team_runs (id, team_id, request, workspace, token, state,
                                    created_at, updated_at)
             VALUES ('tr-pressure', 'marketing', 'write it', 'ws', 'a-secret', 'working',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        let run = fetch_run(&state, "tr-pressure").await;
        let agent = crate::agent::Agent {
            id: "copywriter".to_owned(),
            name: "copywriter".to_owned(),
            speciality: "writes".to_owned(),
            prompt: "write".to_owned(),
            engine: "claude".to_owned(),
            model: None,
            // Not `mcp_only`: this test is about what the terminal write records, and the MCP
            // branch would put a config file in the machine's temp directory to prove nothing.
            tool_policy: "unrestricted".to_owned(),
            created_at: "2026-08-26T00:00:00Z".to_owned(),
            updated_at: "2026-08-26T00:00:00Z".to_owned(),
        };
        let (run_id, session_id) = open_run(&state, &run.id, "do the work", false)
            .await
            .unwrap();

        spawn_agent(
            &state,
            &run,
            &agent,
            run_id,
            session_id,
            "do the work".into(),
        )
        .await;

        for _ in 0..80 {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if status != "running" {
                let measured: (Option<String>, Option<i64>, Option<i64>, i64) = sqlx::query_as(
                    "SELECT tools_used, context_peak, context_fill, compacted
                     FROM runs WHERE id = ?",
                )
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
                let (tools, peak, fill, compacted) = measured;

                let tools: Vec<serde_json::Value> =
                    serde_json::from_str(&tools.expect("tools_used written")).unwrap();
                assert_eq!(tools.len(), 2, "the two calls the stream made");
                assert_eq!(peak, Some(190_000), "the peak");
                assert_eq!(fill, Some(40_000), "where it ended, after compacting");
                assert_eq!(compacted, 1, "it compacted, and the column has to say so");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the specialist's run never reached a terminal status");
    }

    /// A department's session is told the run's speed and the round width that speed opens, under
    /// the neutral names the workflow reads. The width is the one `round_width` enforces — the
    /// team's column widened and bounded here — so the workflow is never told a number the daemon
    /// would not itself open.
    #[tokio::test]
    async fn a_departments_session_is_told_its_speed_and_ceiling() {
        for (speed, expected_ceiling) in [("normal", "2"), ("fast", "4"), ("thorough", "2")] {
            let (mut state, _root) = state_with_root().await;
            let runner =
                std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
            state.runner = runner.clone();
            marketing(&state).await;
            let id = format!("tr-speed-{speed}");
            sqlx::query(
                "INSERT INTO team_runs (id, team_id, request, workspace, token, state, speed,
                                        created_at, updated_at)
                 VALUES (?, 'marketing', 'write it', 'ws', 'a-secret', 'working', ?,
                         '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
            )
            .bind(&id)
            .bind(speed)
            .execute(&state.pool)
            .await
            .unwrap();
            let run = fetch_run(&state, &id).await;
            let agent = crate::agent::Agent {
                id: "copywriter".to_owned(),
                name: "copywriter".to_owned(),
                speciality: "writes".to_owned(),
                prompt: "write".to_owned(),
                engine: "claude".to_owned(),
                model: None,
                // Not `mcp_only`, for the reason the pressure test above gives.
                tool_policy: "unrestricted".to_owned(),
                created_at: "2026-08-26T00:00:00Z".to_owned(),
                updated_at: "2026-08-26T00:00:00Z".to_owned(),
            };
            let (run_id, session_id) = open_run(&state, &run.id, "do the work", false)
                .await
                .unwrap();

            spawn_agent(
                &state,
                &run,
                &agent,
                run_id,
                session_id,
                "do the work".into(),
            )
            .await;
            settled_run(&state, run_id).await;

            let env = runner
                .last_env
                .lock()
                .unwrap()
                .clone()
                .expect("the launch reached the runner");
            let value = |name: &str| {
                env.iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            };
            assert_eq!(value("WORKFLOW_SPEED").as_deref(), Some(speed));
            assert_eq!(
                value("WORKFLOW_PARALLEL_CEILING").as_deref(),
                Some(expected_ceiling),
                "{speed}"
            );
        }
    }

    /// R3 supersedes D15 here: a department agent's briefing is traced like any other run's.
    #[tokio::test]
    async fn a_department_agent_is_told_what_the_house_knows_and_leaves_a_trace() {
        let (mut state, _root) = state_with_root().await;
        let runner = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        state.runner = runner.clone();
        marketing(&state).await;
        sqlx::query(
            "INSERT INTO team_runs (id, team_id, request, workspace, token, state,
                                    created_at, updated_at)
             VALUES ('tr-knowledge', 'marketing', 'write it', 'ws', 'a-secret', 'working',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'machine', NULL, 'owner', 'memory',
                     'zanzibar house rule', 'body', 'active',
                     '2026-08-19T00:00:00+00:00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        let run = fetch_run(&state, "tr-knowledge").await;
        let agent = crate::agent::Agent {
            id: "copywriter".to_owned(),
            name: "copywriter".to_owned(),
            speciality: "writes".to_owned(),
            prompt: "write".to_owned(),
            engine: "claude".to_owned(),
            model: None,
            tool_policy: "unrestricted".to_owned(),
            created_at: "2026-08-26T00:00:00Z".to_owned(),
            updated_at: "2026-08-26T00:00:00Z".to_owned(),
        };
        let (run_id, session_id) = open_run(&state, &run.id, "zanzibar work", false)
            .await
            .unwrap();

        spawn_agent(
            &state,
            &run,
            &agent,
            run_id,
            session_id,
            "zanzibar work".into(),
        )
        .await;

        for _ in 0..80 {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if status != "running" {
                let prompt = runner.last_prompt.lock().unwrap().clone().unwrap();
                assert!(prompt.starts_with("zanzibar work"));
                assert!(prompt.contains("zanzibar house rule"));
                let traces: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM run_knowledge WHERE run_id = ?")
                        .bind(run_id)
                        .fetch_one(&state.pool)
                        .await
                        .unwrap();
                assert_eq!(traces, 1, "R3: the one machine row it was shown is traced");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the department agent's run never reached a terminal status");
    }

    /// A local member has no stream, so it has no peak and no `compacted` -- but it does know how
    /// many calls it made, and that is the denominator.
    ///
    /// Without this the pressure reading saw a local agent's items as having no steps at all, which
    /// reads as a layer that did nothing rather than one nobody measured.
    #[tokio::test]
    async fn a_local_agents_tool_calls_are_the_steps_it_took() {
        let (state, _root) = state_with_root().await;
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('work', 'running', 'team', '2026-08-26T00:00:00Z') RETURNING id",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();

        record_local_turn(
            &state.pool,
            run_id,
            &crate::models::local_agent::Turn {
                answer: "done".to_owned(),
                ending: crate::models::local_agent::Ending::Answered,
                tool_calls: 7,
            },
            "2026-08-26T01:00:00Z",
        )
        .await;

        let (status, turns, cost): (String, Option<i64>, Option<f64>) =
            sqlx::query_as("SELECT status, num_turns, cost_usd FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();

        assert_eq!(status, "completed");
        assert_eq!(turns, Some(7), "the calls it made are the steps it took");
        assert_eq!(cost, Some(0.0), "zero and not NULL — see the doc comment");
    }

    /// An OpenAI-compatible server on this machine, answering one sentence that exists nowhere
    /// else in this module — so a member that wrote it down can only have got it from here.
    async fn stub_openai_compatible_member(answer: &str) -> String {
        let answer = answer.to_string();
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move || {
                let answer = answer.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "choices": [{ "message": { "role": "assistant", "content": answer } }]
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    /// An `Assistants` whose local route is ONE OpenAI-compatible address, recording every model it
    /// was asked to build a chat for. The same double `council.rs` keeps for a seat, for the same
    /// reason: `NoAssistants` answers `assistant_for`, and a team member does not want one — it
    /// wants the chat, which is what `local_chat` hands out.
    struct MemberAssistants {
        base_url: String,
        /// Every model `local_chat` was asked for, in call order.
        asked_for: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::assistants::Assistants for MemberAssistants {
        fn local_chat(
            &self,
            model: &str,
        ) -> Result<Box<dyn crate::models::local_agent::LocalChat>, crate::assistants::Refusal>
        {
            self.asked_for
                .lock()
                .expect("the double's recorder is never held across an await")
                .push(model.to_string());
            Ok(Box::new(
                crate::models::openai_compatible::OpenAiCompatibleChat::with_client(
                    reqwest::Client::new(),
                    self.base_url.clone(),
                    model.to_string(),
                    // `None`, deliberately: a loopback OpenAI-compatible server asks for no key, which
                    // is the case `OpenAiCompatibleChat::new` refuses and `with_client` exists to express.
                    None,
                ),
            ))
        }

        fn assistant_for(
            &self,
            _brain: crate::chats::Brain,
            _model: Option<&str>,
        ) -> Result<
            std::sync::Arc<crate::models::local_agent::LocalAssistant>,
            crate::assistants::Refusal,
        > {
            Err(crate::assistants::Refusal::NotServedByThisFactory)
        }

        fn serves(&self, brain: crate::chats::Brain) -> Result<(), crate::assistants::Refusal> {
            match brain {
                crate::chats::Brain::Local => Ok(()),
                _ => Err(crate::assistants::Refusal::RouteNotConfigured),
            }
        }

        async fn can_serve(
            &self,
            _brain: crate::chats::Brain,
            _model: &str,
        ) -> Result<(), crate::assistants::Refusal> {
            Ok(())
        }

        async fn declared_for(
            &self,
            _brain: crate::chats::Brain,
            _models: &[String],
        ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
            std::collections::HashMap::new()
        }
    }

    /// A factory with no local route at all, which is what an install that configured none has.
    struct NoLocalRoute;

    #[async_trait::async_trait]
    impl crate::assistants::Assistants for NoLocalRoute {
        fn local_chat(
            &self,
            _model: &str,
        ) -> Result<Box<dyn crate::models::local_agent::LocalChat>, crate::assistants::Refusal>
        {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        fn assistant_for(
            &self,
            _brain: crate::chats::Brain,
            _model: Option<&str>,
        ) -> Result<
            std::sync::Arc<crate::models::local_agent::LocalAssistant>,
            crate::assistants::Refusal,
        > {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        fn serves(&self, _brain: crate::chats::Brain) -> Result<(), crate::assistants::Refusal> {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        async fn can_serve(
            &self,
            _brain: crate::chats::Brain,
            _model: &str,
        ) -> Result<(), crate::assistants::Refusal> {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        async fn declared_for(
            &self,
            _brain: crate::chats::Brain,
            _models: &[String],
        ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
            std::collections::HashMap::new()
        }
    }

    /// A local member's agent row, the one shape both tests below need.
    fn local_member(model: &str) -> crate::agent::Agent {
        crate::agent::Agent {
            id: "researcher".to_owned(),
            name: "researcher".to_owned(),
            speciality: "reads".to_owned(),
            prompt: "read".to_owned(),
            engine: "local".to_owned(),
            model: Some(model.to_owned()),
            // Not `mcp_only`: what is under test is which server answered, and the MCP branch
            // would write a config file into the machine's temp directory to prove nothing.
            tool_policy: "unrestricted".to_owned(),
            created_at: "2026-08-26T00:00:00Z".to_owned(),
            updated_at: "2026-08-26T00:00:00Z".to_owned(),
        }
    }

    /// Opens a team run this module's helpers can drive a member off.
    async fn team_run_for_member(state: &AppState, id: &str) -> TeamRun {
        marketing(state).await;
        sqlx::query(
            "INSERT INTO team_runs (id, team_id, request, workspace, token, state,
                                    created_at, updated_at)
             VALUES (?, 'marketing', 'find it', 'ws', 'a-secret', 'working',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
        fetch_run(state, id).await
    }

    /// Polls until the run leaves `running`, and answers what it recorded.
    async fn settled_run(
        state: &AppState,
        run_id: i64,
    ) -> (String, Option<String>, Option<String>) {
        for _ in 0..80 {
            let row: (String, Option<String>, Option<String>) =
                sqlx::query_as("SELECT status, stdout, stderr FROM runs WHERE id = ?")
                    .bind(run_id)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            if row.0 != "running" {
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the member's run never reached a terminal status");
    }

    /// A local team member asks the factory for its chat, exactly as a chat turn and a council
    /// seat do.
    ///
    /// Without this, `spawn_agent` keeps building its own `runner::OllamaChat` against
    /// `runner::OLLAMA_BASE_URL` — the last reader in this daemon still doing so. On an install
    /// whose `local_engine` is `openai_compatible`, the owner's chat and every council seat then reach the
    /// server that was configured while every TEAM member quietly reaches an Ollama that may not
    /// be running, may not hold the model, and may not be the machine that was paid for. What is
    /// asserted is the member's own recorded answer, because that sentence exists only on the
    /// loopback server the factory was pointed at.
    #[tokio::test]
    async fn a_local_team_member_is_served_by_the_configured_engine() {
        const ANSWER: &str = "the answer that exists only on this loopback server";
        let base_url = stub_openai_compatible_member(ANSWER).await;

        let (mut state, _root) = state_with_root().await;
        let assistants = std::sync::Arc::new(MemberAssistants {
            base_url,
            asked_for: std::sync::Mutex::new(Vec::new()),
        });
        state.assistants = assistants.clone();

        let run = team_run_for_member(&state, "tr-local-member").await;
        let agent = local_member("a-frontier-moe");
        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();

        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;

        let (status, stdout, _) = settled_run(&state, run_id).await;
        assert_eq!(status, "completed");
        assert_eq!(
            stdout.as_deref(),
            Some(ANSWER),
            "the member answered from the configured engine, not from Ollama"
        );
        assert_eq!(
            assistants
                .asked_for
                .lock()
                .expect("the recorder is never held across an await")
                .as_slice(),
            ["a-frontier-moe"],
            "the factory was asked for the member's own model, once"
        );
    }

    /// A local member is never routed, whatever the team surface says: its model is the one the
    /// local engine serves, and the adviser is not asked.
    #[tokio::test]
    async fn a_local_team_member_is_never_routed() {
        const ANSWER: &str = "a local answer";
        let base_url = stub_openai_compatible_member(ANSWER).await;
        let (mut state, _root) = state_with_root().await;
        let mut sent = advised_runner(
            &mut state,
            &[("team", crate::models::route_advice::Mode::Apply)],
        )
        .await;
        state.assistants = std::sync::Arc::new(MemberAssistants {
            base_url,
            asked_for: std::sync::Mutex::new(Vec::new()),
        });
        let run = team_run_for_member(&state, "tr-local-unrouted").await;
        let agent = local_member("a-frontier-moe");
        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();

        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;

        assert_eq!(settled_run(&state, run_id).await.0, "completed");
        assert!(sent.try_recv().is_err(), "a local seat asked the adviser");
        let mode: Option<String> = sqlx::query_scalar("SELECT route_mode FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(mode, None);
    }

    /// A local member records the loadout the Claude path records, before its loop starts: the
    /// daemon admits `request_tool` and `read_context` by that row, and the local box offers them
    /// by the same tools. A member without tools records an empty one.
    #[tokio::test]
    async fn a_local_team_member_records_its_loadout_like_a_cloud_one() {
        let base_url = stub_openai_compatible_member("done").await;
        let (mut state, _root) = state_with_root().await;
        state.assistants = std::sync::Arc::new(MemberAssistants {
            base_url,
            asked_for: std::sync::Mutex::new(Vec::new()),
        });
        let run = team_run_for_member(&state, "tr-local-loadout").await;

        for (policy, expected) in [
            ("mcp_only", loadout_team_base()),
            ("unrestricted", Vec::new()),
        ] {
            let agent = crate::agent::Agent {
                tool_policy: policy.to_owned(),
                ..local_member("a-frontier-moe")
            };
            let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();

            spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;

            assert_eq!(settled_run(&state, run_id).await.0, "completed");
            let (_, _, tools) = loadout_row_of(&state, run_id).await;
            assert_eq!(tools, expected, "tool_policy {policy}");
        }
    }

    /// A cloud member under `team: apply` launches on the advice, and the row says so.
    #[tokio::test]
    async fn a_cloud_team_member_under_apply_launches_the_advice() {
        let (mut state, _root) = state_with_root().await;
        let fake = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        state.runner = fake.clone();
        let mut sent = advised_runner(
            &mut state,
            &[("team", crate::models::route_advice::Mode::Apply)],
        )
        .await;
        let run = team_run_for_member(&state, "tr-cloud-routed").await;
        let agent = crate::agent::Agent {
            engine: "claude".to_owned(),
            model: Some("claude-sonnet-5".to_owned()),
            ..local_member("unused")
        };
        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();

        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;
        settled_run(&state, run_id).await;

        assert!(sent.try_recv().is_ok());
        assert_eq!(
            *fake.last_model.lock().unwrap(),
            Some(Some("claude-opus-5".to_owned()))
        );
        // A run with no speed of its own is `normal`, whose ceiling is `medium`.
        assert_eq!(
            *fake.last_effort.lock().unwrap(),
            Some(Some("medium".to_owned()))
        );
        let decision: Option<String> =
            sqlx::query_scalar("SELECT route_decision_id FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(decision.as_deref(), Some("rt_team"));
    }

    /// A team member reads its own memory and its team's, nobody else's, and the briefing leaves a
    /// trace that names the rows it considered.
    #[tokio::test]
    async fn a_team_member_is_briefed_with_its_own_and_its_teams_memory_and_leaves_a_trace() {
        let (mut state, _root) = state_with_root().await;
        let fake = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        state.runner = fake.clone();
        let run = team_run_for_member(&state, "tr-loadout").await;
        let agent = crate::agent::Agent {
            engine: "claude".to_owned(),
            model: Some("claude-sonnet-5".to_owned()),
            ..local_member("unused")
        };

        let mut ids = Vec::new();
        for (scope_kind, scope_id, title) in [
            ("agent", "researcher", "zanzibar researcher-note"),
            ("team", "marketing", "zanzibar marketing-note"),
            ("agent", "someone-else", "zanzibar someone-else-note"),
            ("team", "sales", "zanzibar sales-note"),
        ] {
            let result = sqlx::query(
                "INSERT INTO knowledge
                   (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
                 VALUES ('semantic', ?, ?, 'owner', 'memory', ?, 'body', 'active',
                         '2026-08-19T00:00:00+00:00')",
            )
            .bind(scope_kind)
            .bind(scope_id)
            .bind(title)
            .execute(&state.pool)
            .await
            .unwrap();
            ids.push(result.last_insert_rowid());
        }
        let (someone_else, sales) = (ids[2], ids[3]);

        let (run_id, session_id) = open_run(&state, &run.id, "zanzibar", false).await.unwrap();
        spawn_agent(&state, &run, &agent, run_id, session_id, "zanzibar".into()).await;
        settled_run(&state, run_id).await;

        let prompt = fake
            .last_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("the member's launch received a prompt");
        assert!(prompt.contains("researcher-note"), "{prompt}");
        assert!(prompt.contains("marketing-note"), "{prompt}");
        assert!(!prompt.contains("someone-else-note"), "{prompt}");
        assert!(!prompt.contains("sales-note"), "{prompt}");

        let traced: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_knowledge WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(traced > 0, "the briefing left no trace for the run");
        for foreign in [someone_else, sales] {
            let rows: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM run_knowledge WHERE run_id = ? AND knowledge_id = ?",
            )
            .bind(run_id)
            .bind(foreign)
            .fetch_one(&state.pool)
            .await
            .unwrap();
            assert_eq!(rows, 0, "row {foreign} belongs to no link of this member");
        }
    }

    /// A cloud member whose tools are on, ready for the loadout tests below.
    fn loadout_member(tool_policy: &str) -> crate::agent::Agent {
        crate::agent::Agent {
            engine: "claude".to_owned(),
            model: Some("claude-sonnet-5".to_owned()),
            tool_policy: tool_policy.to_owned(),
            ..local_member("unused")
        }
    }

    /// A `context_refs` row for an agent or a team, pointing at a file under the managed root.
    async fn loadout_file_ref(
        state: &AppState,
        owner_kind: &str,
        owner_id: &str,
        name: &str,
    ) -> String {
        let root = state.files_root.clone().expect("the test state has a root");
        let path = root.join(name);
        std::fs::write(&path, "a context file").unwrap();
        let path = path.to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO context_refs (owner_kind, owner_id, path, kind, note, created_at)
             VALUES (?, ?, ?, 'file', NULL, '2026-10-09T00:00:00Z')",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(&path)
        .execute(&state.pool)
        .await
        .unwrap();
        path
    }

    /// What `run_loadout` froze for a run: the agent, the team and the sorted tool list.
    async fn loadout_row_of(
        state: &AppState,
        run_id: i64,
    ) -> (Option<String>, Option<String>, Vec<String>) {
        let (agent, team, tools): (Option<String>, Option<String>, String) =
            sqlx::query_as("SELECT agent_id, team_id, tools FROM run_loadout WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .expect("the member recorded a run_loadout row before it started");
        let mut tools: Vec<String> = serde_json::from_str(&tools).expect("tools is a JSON array");
        tools.sort();
        (agent, team, tools)
    }

    fn loadout_team_base() -> Vec<String> {
        let mut base: Vec<String> = crate::mcp_tools::TEAM_BASE
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect();
        base.sort();
        base
    }

    /// A team member launches in the Team box with the loadout it recorded: the row, the
    /// allow-list the runner was handed and the prompt's context index all come from one
    /// resolution, and the MCP config is the node's own, named by its run.
    #[tokio::test]
    async fn loadout_a_team_member_launches_with_its_loadout_and_box() {
        let (mut state, _root) = state_with_root().await;
        let fake = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        state.runner = fake.clone();
        let run = team_run_for_member(&state, "tr-loadout-box").await;
        let agent = loadout_member("mcp_only");
        let own_file = loadout_file_ref(&state, "agent", "researcher", "researcher-brief.md").await;
        let team_file = loadout_file_ref(&state, "team", "marketing", "marketing-style.md").await;

        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();
        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;
        settled_run(&state, run_id).await;

        // The row: owners, and exactly the team box's base with nothing approved.
        let (row_agent, row_team, tools) = loadout_row_of(&state, run_id).await;
        assert_eq!(row_agent.as_deref(), Some("researcher"));
        assert_eq!(row_team.as_deref(), Some("marketing"));
        assert_eq!(tools, loadout_team_base());

        // The launch: the allow-list is the row's, and the config is the node's own.
        let (config, job, allowed) = fake
            .last_job_mcp
            .lock()
            .unwrap()
            .clone()
            .expect("the member's launch was recorded");
        assert_eq!(job, None, "a team member is not a job node");
        let mut allowed = allowed.expect("a member with tools launches with an allow-list");
        allowed.sort();
        assert_eq!(allowed, tools, "the launch must carry the loadout's tools");
        let config = config.expect("a member with tools is offered an MCP server");
        let name = config.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.ends_with(&format!("-{run_id}.json")),
            "the config is the node's own, named by its run, not the team run's: {name}"
        );

        // The prompt: both refs are in the index, there is no directory to add without a cwd.
        let prompt = fake.last_prompt.lock().unwrap().clone().expect("a prompt");
        assert!(prompt.contains("Context files you were given"), "{prompt}");
        assert!(prompt.contains(&own_file), "{prompt}");
        assert!(prompt.contains(&team_file), "{prompt}");
        assert_eq!(
            fake.last_add_dirs.lock().unwrap().clone(),
            Some(Vec::new()),
            "a team member has no working directory, so no directory is added"
        );
    }

    /// Another agent's approval row (a tool for the copywriter) and another agent's or team's refs
    /// leave this member's row, allow-list and prompt exactly as they were: the base, nothing more.
    #[tokio::test]
    async fn loadout_a_team_member_never_receives_another_agents_tools_or_refs() {
        let (mut state, _root) = state_with_root().await;
        let fake = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        state.runner = fake.clone();
        let run = team_run_for_member(&state, "tr-loadout-iso").await;
        let agent = loadout_member("mcp_only");
        loadout_file_ref(&state, "agent", "researcher", "researcher-brief.md").await;
        loadout_file_ref(&state, "agent", "copywriter", "copywriter-private.md").await;
        loadout_file_ref(&state, "team", "sales", "sales-private.md").await;
        sqlx::query(
            "INSERT INTO loadout_tools
                 (owner_kind, owner_id, tool, status, source, reason, run_id, created_at)
             VALUES ('agent', 'copywriter', 'web_read', 'active', 'owner', NULL, NULL,
                     '2026-10-09T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();
        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;
        settled_run(&state, run_id).await;

        let (_, _, tools) = loadout_row_of(&state, run_id).await;
        assert_eq!(
            tools,
            loadout_team_base(),
            "nobody approved anything for this member: exactly the base"
        );
        let (_, _, allowed) = fake.last_job_mcp.lock().unwrap().clone().expect("a launch");
        // web_read is in TEAM_BASE, so isolation is shown by the set being exactly the base.
        let mut allowed = allowed.expect("a member with tools launches with an allow-list");
        allowed.sort();
        assert_eq!(
            allowed,
            loadout_team_base(),
            "another agent's approval changes nothing in this member's launch"
        );
        let prompt = fake.last_prompt.lock().unwrap().clone().expect("a prompt");
        assert!(prompt.contains("researcher-brief.md"), "{prompt}");
        assert!(!prompt.contains("copywriter-private"), "{prompt}");
        assert!(!prompt.contains("sales-private"), "{prompt}");
    }

    /// A member whose tools are off still leaves a row, and an empty one: a run that cannot prove
    /// it holds a tool does not hold it, and "none" is a loadout like any other.
    #[tokio::test]
    async fn loadout_a_member_without_tools_records_an_empty_set() {
        let (mut state, _root) = state_with_root().await;
        let fake = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        state.runner = fake.clone();
        let run = team_run_for_member(&state, "tr-loadout-none").await;
        let agent = loadout_member("none");
        loadout_file_ref(&state, "agent", "researcher", "researcher-brief.md").await;

        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();
        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;
        settled_run(&state, run_id).await;

        let (row_agent, row_team, tools) = loadout_row_of(&state, run_id).await;
        assert_eq!(row_agent.as_deref(), Some("researcher"));
        assert_eq!(row_team.as_deref(), Some("marketing"));
        assert!(
            tools.is_empty(),
            "a member without tools holds none: {tools:?}"
        );

        assert_eq!(
            *fake.last_mcp_config.lock().unwrap(),
            None,
            "no tools, no MCP server"
        );
        // Nothing to open a file with, so no index: the index is there only with the tool.
        let prompt = fake.last_prompt.lock().unwrap().clone().expect("a prompt");
        assert!(!prompt.contains("Context files you were given"), "{prompt}");
    }

    /// A member whose factory has no local route fails carrying the refusal's OWN sentence.
    ///
    /// The refusal travels verbatim for the reason `council.rs` gives at its seat: `Refusal::
    /// message` is already what the chat route shows an operator for this misconfiguration, and a
    /// second wording invented here would describe one problem in two voices depending on which
    /// door somebody came through. The row is closed rather than left `running`, because nothing
    /// was spawned and so nothing else will ever close it.
    #[tokio::test]
    async fn a_local_team_member_whose_factory_has_no_local_route_says_so_and_stops() {
        let (mut state, _root) = state_with_root().await;
        state.assistants = std::sync::Arc::new(NoLocalRoute);

        let run = team_run_for_member(&state, "tr-no-local-route").await;
        let agent = local_member("a-frontier-moe");
        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();

        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;

        let (status, _, stderr) = settled_run(&state, run_id).await;
        assert_eq!(status, "failed");
        assert_eq!(
            stderr.as_deref(),
            Some(
                crate::assistants::Refusal::RouteNotConfigured
                    .message(crate::chats::Brain::Local)
                    .as_str()
            ),
            "the operator is told what the chat route would have told them"
        );
    }

    async fn fetch_run(state: &AppState, id: &str) -> TeamRun {
        sqlx::query_as(
            "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                    next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                    updated_at, finished_at, trigger_id, parent_id, root_id, depth
             FROM team_runs WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    /// A failed specialist is an absence in the folder, not the end of the department.
    #[tokio::test]
    async fn a_failed_specialist_does_not_stop_the_run() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        sqlx::query("UPDATE team_runs SET state = 'working' WHERE id = ?")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();

        let (failed, _) = open_run(&state, &id, "one", false).await.unwrap();
        let (ok, _) = open_run(&state, &id, "two", false).await.unwrap();
        sqlx::query("UPDATE runs SET status = 'failed' WHERE id = ?")
            .bind(failed)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'completed', stdout = 'the draft' WHERE id = ?")
            .bind(ok)
            .execute(&state.pool)
            .await
            .unwrap();
        for (ordinal, run_id) in [(1, failed), (2, ok)] {
            sqlx::query(
                "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state,
                                         run_id)
                 VALUES (?, ?, 0, 'copywriter', 'work', 'running', ?)",
            )
            .bind(&id)
            .bind(ordinal)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        let run = fetch_run(&state, &id).await;
        ingest_landed_items(&state, &run).await.unwrap();

        let states: Vec<(i64, String, Option<String>)> = sqlx::query_as(
            "SELECT ordinal, state, output_path FROM team_items WHERE team_run_id = ? ORDER BY ordinal",
        )
        .bind(&id)
        .fetch_all(&state.pool)
        .await
        .unwrap();

        assert_eq!(states[0].1, "failed");
        assert_eq!(states[0].2, None, "a failed item leaves no file");
        assert_eq!(states[1].1, "done");
        assert_eq!(states[1].2.as_deref(), Some("2-copywriter.md"));
        assert_eq!(fetch_run(&state, &id).await.state, "working");
    }

    /// A landed item tells the router how its seat went, against the decision on its run's row:
    /// filed is `pass`, failed is `fail`, and an item launched unrouted reports nothing.
    #[tokio::test]
    async fn a_landed_item_reports_its_seats_outcome_to_the_router() {
        let (mut state, _root) = state_with_root().await;
        let (url, mut received) = crate::router_client::test_support::outcome_router(200).await;
        let inner = state.runner.clone();
        let router = std::sync::Arc::new(crate::models::route_advice::Router::new(
            crate::models::route_advice::RouterConfig {
                mode: crate::models::route_advice::Mode::Shadow,
                url,
                ..crate::models::route_advice::RouterConfig::off()
            },
            crate::models::route_advice::Available {
                kind: crate::models::route_advice::RunnerKind::Claude,
                runner: inner.clone(),
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ));
        state.runner =
            std::sync::Arc::new(crate::models::route_advice::RoutedRunner { inner, router });
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let mut items = Vec::new();
        for (ordinal, status, decision) in [
            (1, "failed", Some("rt_failed")),
            (2, "completed", Some("rt_done")),
            (3, "completed", None),
        ] {
            let (run_id, _) = open_run(&state, &id, "work", false).await.unwrap();
            sqlx::query(
                "UPDATE runs SET status = ?, stdout = 'the draft', route_decision_id = ?
                 WHERE id = ?",
            )
            .bind(status)
            .bind(decision)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
            items.push((ordinal, run_id));
        }
        for (ordinal, run_id) in items {
            sqlx::query(
                "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state,
                                         run_id)
                 VALUES (?, ?, 0, 'copywriter', 'work', 'running', ?)",
            )
            .bind(&id)
            .bind(ordinal)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        let run = fetch_run(&state, &id).await;
        ingest_landed_items(&state, &run).await.unwrap();
        // A second pass finds nothing still running, so nothing is reported twice.
        ingest_landed_items(&state, &run).await.unwrap();

        let mut reports = Vec::new();
        while let Ok(Some(report)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), received.recv()).await
        {
            reports.push(report);
        }
        reports.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            reports,
            vec![
                ("rt_done".to_owned(), serde_json::json!({"status": "pass"})),
                (
                    "rt_failed".to_owned(),
                    serde_json::json!({"status": "fail"})
                ),
            ]
        );
    }

    /// The core names the file, which is what makes a collision between two specialists impossible
    /// by construction rather than by agreement.
    #[tokio::test]
    async fn the_core_files_a_specialists_answer_under_a_name_it_chose() {
        let (state, root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let (run_id, _) = open_run(&state, &id, "one", false).await.unwrap();
        sqlx::query(
            "UPDATE runs SET status = 'completed', stdout = 'the launch post' WHERE id = ?",
        )
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state,
                                     run_id)
             VALUES (?, 7, 0, 'copywriter', 'work', 'running', ?)",
        )
        .bind(&id)
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();

        let run = fetch_run(&state, &id).await;
        ingest_landed_items(&state, &run).await.unwrap();

        let filed = std::fs::canonicalize(root.path())
            .unwrap()
            .join("teams")
            .join("marketing")
            .join(&id)
            .join("7-copywriter.md");
        assert_eq!(std::fs::read_to_string(filed).unwrap(), "the launch post");
    }

    /// A delivery is not collected when its run ends — it IS the delivery — and it is collected by
    /// age. The two halves of that sentence are the two assertions.
    #[tokio::test]
    async fn a_recent_delivery_survives_the_gc_and_an_old_one_does_not() {
        let (state, root) = state_with_root().await;
        marketing(&state).await;
        let now = chrono::Utc::now();

        let mut folders = Vec::new();
        for (id_hint, finished) in [
            ("recent", now - chrono::Duration::days(1)),
            (
                "old",
                now - chrono::Duration::days(TEAM_WORKSPACE_RETENTION_DAYS + 1),
            ),
        ] {
            let id = start(&state, "marketing", id_hint, None).await.unwrap();
            sqlx::query("UPDATE team_runs SET state = 'done', finished_at = ? WHERE id = ?")
                .bind(finished.to_rfc3339())
                .bind(&id)
                .execute(&state.pool)
                .await
                .unwrap();
            let folder = std::fs::canonicalize(root.path())
                .unwrap()
                .join("teams")
                .join("marketing")
                .join(&id);
            std::fs::write(folder.join("entrega.md"), "the answer").unwrap();
            folders.push(folder);
        }

        workspace_gc(&state, now).await;

        assert!(
            folders[0].is_dir(),
            "a delivery a day old is what the owner is about to read"
        );
        assert!(
            !folders[1].exists(),
            "a delivery past its retention should be collected"
        );
    }

    /// A live run's folder is never collected, whatever its age — the ONE way this could delete
    /// work in progress.
    #[tokio::test]
    async fn the_gc_never_touches_a_live_run() {
        let (state, root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "still going", None)
            .await
            .unwrap();
        sqlx::query("UPDATE team_runs SET finished_at = ? WHERE id = ?")
            .bind((chrono::Utc::now() - chrono::Duration::days(999)).to_rfc3339())
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();

        workspace_gc(&state, chrono::Utc::now()).await;

        let folder = std::fs::canonicalize(root.path())
            .unwrap()
            .join("teams")
            .join("marketing")
            .join(&id);
        assert!(
            folder.is_dir(),
            "a run still in `planning` is not finished, whatever its timestamp says"
        );
    }

    /// §*Cancellation safety*: a cancelled run leaves nothing in flight spending.
    #[tokio::test]
    async fn cancelling_leaves_no_run_still_spending() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let (run_id, _) = open_run(&state, &id, "one", false).await.unwrap();
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state,
                                     run_id)
             VALUES (?, 1, 0, 'copywriter', 'work', 'running', ?)",
        )
        .bind(&id)
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();

        cancel(&state, &id).await.unwrap();

        let run = fetch_run(&state, &id).await;
        assert_eq!(run.state, "cancelled");
        assert!(run.finished_at.is_some());
        let still_running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE team_run_id = ? AND status = 'running'",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            still_running, 0,
            "a cancelled department left a run spending"
        );
        let items_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM team_items WHERE team_run_id = ? AND state IN ('pending', 'running')",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(items_left, 0);
    }

    /// Every ending, not just a cancel, leaves nothing in flight: a run that fails or is
    /// delivered must not keep its specialists spending, nor leave their items open.
    #[tokio::test]
    async fn finishing_a_run_stops_its_specialists_and_closes_their_items() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let (run_id, _) = open_run(&state, &id, "one", false).await.unwrap();
        sqlx::query(
            "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state,
                                     run_id)
             VALUES (?, 1, 0, 'copywriter', 'work', 'running', ?)",
        )
        .bind(&id)
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();

        let run = fetch_run(&state, &id).await;
        finish(&state, &run, "failed", "the director failed")
            .await
            .unwrap();

        let still_running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE team_run_id = ? AND status = 'running'",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            still_running, 0,
            "a finished department left a run spending"
        );
        let open_items: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM team_items WHERE team_run_id = ? AND state IN ('pending', 'running')",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(open_items, 0, "a finished department left open items");
    }

    /// A delivery that cannot be written must end the run, not fail the same way every tick while
    /// the run counts as live forever.
    #[tokio::test]
    async fn a_delivery_that_cannot_be_written_fails_the_run() {
        let (state, root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let run = fetch_run(&state, &id).await;
        // Put a FILE where the workspace folder should be, so the write cannot succeed.
        let folder = std::fs::canonicalize(root.path())
            .unwrap()
            .join(&run.workspace);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
        std::fs::write(&folder, b"in the way").unwrap();

        let (run_id, _) = open_run(&state, &id, "deliver", false).await.unwrap();
        sqlx::query(
            "UPDATE runs SET status = 'completed', exit_code = 0, stdout = 'done' WHERE id = ?",
        )
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE team_runs SET state = 'delivering', director_node = 'delivering',
                                  director_run_id = ? WHERE id = ?",
        )
        .bind(run_id)
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();

        let run = fetch_run(&state, &id).await;
        ingest_director(&state, run).await.unwrap();

        let after = fetch_run(&state, &id).await;
        assert_eq!(after.state, "failed");
        assert!(after.finished_at.is_some());
    }

    /// An auto-mode (`allow`) action still pending when the owner cancels must not be carried out
    /// afterwards.
    #[tokio::test]
    async fn a_cancelled_run_does_not_carry_out_its_pending_actions() {
        let (state, root) = state_with_root().await;
        let run_id = team_with_grant(&state, "file_document", "allow").await;
        let filed = ask(
            &state,
            &run_id,
            "file_document",
            serde_json::json!({ "path": "after-cancel.md", "content": "no" }),
        )
        .await
        .unwrap();

        cancel(&state, &run_id).await.unwrap();
        execute_due_actions(&state).await;

        let done: String = sqlx::query_scalar("SELECT state FROM team_actions WHERE id = ?")
            .bind(filed.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(done, "done", "an action ran after its run was cancelled");
        assert!(
            !std::fs::canonicalize(root.path())
                .unwrap()
                .join("after-cancel.md")
                .exists()
        );
    }

    /// The due-actions query spells its terminal states out (sqlx refuses a query built at
    /// runtime), so this holds that list equal to `TERMINAL_STATES`: a state added to one and not
    /// the other would let an ended run's actions run again.
    #[test]
    fn the_due_actions_query_skips_every_terminal_state() {
        let sql = actions::DUE_ACTIONS_SQL;
        let start = sql
            .find("WHERE state IN (")
            .expect("the terminal-state list")
            + 16;
        let end = start + sql[start..].find(')').expect("the list closes");
        let mut listed: Vec<String> = sql[start..end]
            .split(',')
            .map(|state| state.trim().trim_matches('\'').to_owned())
            .collect();
        let mut expected: Vec<String> = TERMINAL_STATES.iter().map(|s| (*s).to_owned()).collect();
        listed.sort();
        expected.sort();
        assert_eq!(listed, expected);
    }

    /// A pending `allow` action belongs to a run that is going somewhere. Whichever way the run
    /// ended (every `TERMINAL_STATES` entry, not only a cancel) its action is not carried out; a
    /// run still working keeps getting its actions done.
    #[tokio::test]
    async fn pending_actions_run_only_while_their_team_run_is_live() {
        for (ending, runs) in TERMINAL_STATES
            .iter()
            .map(|s| (*s, false))
            .chain(std::iter::once(("working", true)))
        {
            let (state, root) = state_with_root().await;
            let run_id = team_with_grant(&state, "file_document", "allow").await;
            let file = format!("{ending}-run.md");
            let filed = ask(
                &state,
                &run_id,
                "file_document",
                serde_json::json!({ "path": file, "content": "x" }),
            )
            .await
            .unwrap();
            sqlx::query("UPDATE team_runs SET state = ? WHERE id = ?")
                .bind(ending)
                .bind(&run_id)
                .execute(&state.pool)
                .await
                .unwrap();

            execute_due_actions(&state).await;

            let after: String = sqlx::query_scalar("SELECT state FROM team_actions WHERE id = ?")
                .bind(filed.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            let written = std::fs::canonicalize(root.path())
                .unwrap()
                .join(&file)
                .exists();
            if runs {
                assert_eq!(after, "done", "a {ending} run's action was not carried out");
                assert!(written, "a {ending} run's file was not written");
            } else {
                assert_eq!(after, "pending", "a {ending} run's action was carried out");
                assert!(!written, "a {ending} run's file was written");
            }
        }
    }

    /// Cancelling twice is a no-op rather than a way to overwrite how a run ended.
    #[tokio::test]
    async fn a_finished_run_cannot_be_cancelled_into_a_different_ending() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        let run = fetch_run(&state, &id).await;
        finish(&state, &run, "done", "the department delivered")
            .await
            .unwrap();

        assert!(matches!(
            cancel(&state, &id).await,
            Err(TeamError::NotFound)
        ));
        assert_eq!(fetch_run(&state, &id).await.state, "done");

        // Started and finished are one department run's story, keyed by that run's id.
        let lines: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT kind, subject FROM feed
             WHERE kind IN ('team_run_started', 'team_run_finished') ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        let team_run = format!("team_run:{id}");
        assert_eq!(
            lines,
            [
                ("team_run_started".to_string(), Some(team_run.clone())),
                ("team_run_finished".to_string(), Some(team_run)),
            ]
        );
    }

    /// The column migration 0084 adds, doing the job it was added for: the director's own nodes are
    /// part of what the run cost.
    #[tokio::test]
    async fn a_runs_spend_counts_the_directors_nodes_and_not_another_runs() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let mine = start(&state, "marketing", "mine", None).await.unwrap();
        let theirs = start(&state, "marketing", "theirs", None).await.unwrap();

        for (team_run, cost) in [(&mine, 1.5), (&mine, 0.5), (&theirs, 10.0)] {
            let (run_id, _) = open_run(&state, team_run, "work", false).await.unwrap();
            sqlx::query("UPDATE runs SET status = 'completed', cost_usd = ? WHERE id = ?")
                .bind(cost)
                .bind(run_id)
                .execute(&state.pool)
                .await
                .unwrap();
        }

        assert!((spend_of(&state.pool, &mine).await - 2.0).abs() < 1e-9);
        assert!((spend_of(&state.pool, &theirs).await - 10.0).abs() < 1e-9);
    }

    /// The per-team ceiling stops the run BETWEEN rounds, and stops it as `stopped` rather than
    /// `failed`: nothing failed, a ceiling was reached.
    #[tokio::test]
    async fn a_spent_team_budget_stops_the_run_before_a_round_and_not_as_a_failure() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        update(
            &state.pool,
            "marketing",
            TeamRequest {
                name: "Marketing".to_owned(),
                mission: "sell the thing".to_owned(),
                director_agent_id: "director".to_owned(),
                max_rounds: 3,
                max_parallel: 2,
                budget_usd: Some(1.0),
                max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: vec!["copywriter".to_owned()],
            },
        )
        .await
        .unwrap();
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let (run_id, _) = open_run(&state, &id, "work", false).await.unwrap();
        sqlx::query("UPDATE runs SET status = 'completed', cost_usd = 2.0 WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();

        team_tick(&state, chrono::Utc::now()).await;

        let run = fetch_run(&state, &id).await;
        assert_eq!(run.state, "stopped");
        assert_eq!(run.outcome.as_deref(), Some("stopped"));
        assert!(run.why.unwrap().contains("ceiling"));
    }

    #[tokio::test]
    async fn quota_brake_never_stops_a_running_team() {
        let (state, _root) = state_with_quota().await;
        marketing(&state).await;
        crate::quota::test_support::arm(&state.pool, false, 85, 90).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();
        crate::quota::test_support::arm(&state.pool, true, 85, 90).await;

        team_tick(&state, chrono::Utc::now()).await;

        let run = fetch_run(&state, &id).await;
        assert_ne!(run.state, "stopped", "quota must not stop a running team");
    }

    #[tokio::test]
    async fn quota_brake_refuses_to_start_a_team() {
        let (state, _root) = state_with_quota().await;
        marketing(&state).await;

        let error = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap_err();
        match error {
            StartError::QuotaExhausted(reason) => assert!(reason.contains("quota"), "{reason}"),
            other => panic!("expected a quota refusal, got {other:?}"),
        }
    }

    /// Four hours is a ceiling on the clock, and it ends the run `expired` — which tells the owner
    /// something `stopped` does not.
    #[tokio::test]
    async fn a_run_past_its_lifetime_expires() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        team_tick(
            &state,
            chrono::Utc::now() + MAX_TEAM_RUN_LIFETIME + chrono::Duration::minutes(1),
        )
        .await;

        let run = fetch_run(&state, &id).await;
        assert_eq!(run.state, "expired");
        assert_ne!(run.state, "failed");
    }

    /// `director_node` is a marker and not a derived condition, and this is what it buys: a second
    /// pass while a node is in flight must not launch another one. The cost of getting it wrong is
    /// measured in `job.rs` at one wasted node per round.
    /// `director_node` is a marker and not a derived condition, and this is what it buys: while a
    /// node is in flight, no pass may launch a second one. The cost of getting it wrong is measured
    /// in `job.rs` at one wasted node per round.
    ///
    /// The in-flight state is built here rather than produced by a first tick, and that is not
    /// shortcutting: the fake runner answers instantly, so a tick-produced node would already have
    /// LANDED by the second pass and the test would be asserting ingestion instead of the thing it
    /// is named after. What is being pinned is a pass meeting a director run that is still running.
    #[tokio::test]
    async fn a_pass_does_not_launch_a_second_director_while_one_is_in_flight() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        let (in_flight, _) = open_run(&state, &id, "plan it", false).await.unwrap();
        sqlx::query(
            "UPDATE team_runs SET director_node = 'planning', director_run_id = ? WHERE id = ?",
        )
        .bind(in_flight)
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();

        for _ in 0..3 {
            team_tick(&state, chrono::Utc::now()).await;
        }

        let after = fetch_run(&state, &id).await;
        assert_eq!(after.director_node, "planning");
        assert_eq!(after.director_run_id, Some(in_flight));

        let nodes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE team_run_id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(nodes, 1, "three passes launched {nodes} director nodes");
    }

    /// The counterpart: with nothing in flight, a pass DOES launch the planner. Without this the
    /// test above would pass just as well for a tick that does nothing at all.
    ///
    /// It is also what caught the ceiling check reading a NULL `budget_usd` as $0.00 — a team with
    /// no ceiling, which is the default, was stopped before its first round. Nothing else would
    /// have noticed: every other test here either sets a ceiling or never reaches a tick.
    #[tokio::test]
    async fn a_pass_launches_the_planner_when_nothing_is_in_flight() {
        let (state, _root) = state_with_root().await;
        marketing(&state).await;
        let id = start(&state, "marketing", "write the launch post", None)
            .await
            .unwrap();

        team_tick(&state, chrono::Utc::now()).await;

        let nodes: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE team_run_id = ? AND mode = ?")
                .bind(&id)
                .bind(TEAM_MODE)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let run = fetch_run(&state, &id).await;
        assert_eq!(
            nodes, 1,
            "the pass launched no planner; the run is {} ({:?})",
            run.state, run.why
        );
    }

    // -----------------------------------------------------------------------------------------
    // The folder, and who may read it
    // -----------------------------------------------------------------------------------------

    async fn run_with_a_folder(state: &AppState, id: &str) -> std::path::PathBuf {
        insert_agent(state, "director", "claude").await;
        insert_agent(state, "copywriter", "claude").await;
        sqlx::query(
            "INSERT OR IGNORE INTO teams
                 (id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                  created_at, updated_at)
             VALUES ('marketing', 'Marketing', 'sell', 'director', 3, 2, NULL,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO team_runs (id, team_id, request, workspace, token, state,
                                    created_at, updated_at)
             VALUES (?, 'marketing', 'r', ?, 'secret', 'working',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(workspace_for("marketing", id))
        .execute(&state.pool)
        .await
        .unwrap();

        let folder = state
            .files_root
            .as_ref()
            .unwrap()
            .join("teams")
            .join("marketing")
            .join(id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("1-copywriter.md"), "the launch post").unwrap();
        folder
    }

    async fn read_as(
        state: &AppState,
        scope: Scope,
        path: &str,
    ) -> Result<Json<FileView>, (StatusCode, String)> {
        post_read_file(
            State(state.clone()),
            axum::Extension(scope),
            Json(ReadFileRequest {
                path: path.to_owned(),
            }),
        )
        .await
    }

    #[tokio::test]
    async fn a_run_reads_its_own_folder() {
        let (state, _root) = state_with_root().await;
        run_with_a_folder(&state, "run-1").await;

        let answer = read_as(
            &state,
            Scope::TeamRun("run-1".to_owned()),
            "1-copywriter.md",
        )
        .await
        .unwrap();
        assert_eq!(answer.content, "the launch post");
    }

    /// 403 and not 401, for the reason `require_token` gives: the caller authenticated perfectly
    /// well, it is simply not a department. Every other scope is refused, the control token
    /// included — this route answers a run about its own folder, and the owner has the Files tab
    /// for the same bytes.
    #[tokio::test]
    async fn no_scope_but_a_team_run_reads_a_team_folder() {
        let (state, _root) = state_with_root().await;
        run_with_a_folder(&state, "run-1").await;

        for scope in [
            Scope::Control,
            Scope::Run(1),
            Scope::Service(crate::auth::Service::Council),
            Scope::ApiToken(crate::auth::ApiTokenLevel::Admin),
        ] {
            let refusal = read_as(&state, scope.clone(), "1-copywriter.md")
                .await
                .expect_err("only a department reads a department's folder");
            assert_eq!(refusal.0, StatusCode::FORBIDDEN, "{scope:?}");
        }
    }

    /// One department's key opens one department's folder, and the folder is not an argument.
    ///
    /// The assertion that matters is the neighbour's file: naming it by a relative path is how a
    /// caller would try to make the argument decide, and `files.rs` refuses it before anything is
    /// opened.
    #[tokio::test]
    async fn a_run_cannot_read_out_of_its_own_folder() {
        let (state, root) = state_with_root().await;
        run_with_a_folder(&state, "run-1").await;
        run_with_a_folder(&state, "run-2").await;
        std::fs::write(
            std::fs::canonicalize(root.path())
                .unwrap()
                .join("owner-notes.md"),
            "private",
        )
        .unwrap();

        for path in [
            "../run-2/1-copywriter.md",
            "..\\run-2\\1-copywriter.md",
            "../../../owner-notes.md",
            "....//run-2/1-copywriter.md",
            "/etc/passwd",
            "C:\\Windows\\win.ini",
        ] {
            let refusal = read_as(&state, Scope::TeamRun("run-1".to_owned()), path)
                .await
                .unwrap_err();
            assert!(
                refusal.0 == StatusCode::BAD_REQUEST || refusal.0 == StatusCode::NOT_FOUND,
                "{path} answered {}",
                refusal.0
            );
        }

        // And the run beside it reads its own copy perfectly well, so the loop above is refusing
        // the escape rather than the whole route.
        assert!(
            read_as(
                &state,
                Scope::TeamRun("run-2".to_owned()),
                "1-copywriter.md"
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_token_for_a_run_that_left_no_folder_is_a_404_and_not_a_panic() {
        let (state, _root) = state_with_root().await;

        let refusal = read_as(&state, Scope::TeamRun("never-existed".to_owned()), "a.md")
            .await
            .unwrap_err();
        assert_eq!(refusal.0, StatusCode::NOT_FOUND);
    }

    /// One `knowledge` row of the given scope and status, shaped like `loadout.rs`'s `seed`.
    async fn seed_knowledge(
        pool: &sqlx::SqlitePool,
        scope_kind: &str,
        scope_id: &str,
        status: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', ?, ?, 'owner', 'memory', 'a memory', 'body', ?,
                     '2026-08-19T00:00:00+00:00')",
        )
        .bind(scope_kind)
        .bind(scope_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn knowledge_status(pool: &sqlx::SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, ended_at FROM knowledge WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The `(from_status, to_status)` pairs recorded for one knowledge row, oldest first.
    async fn knowledge_transitions(
        pool: &sqlx::SqlitePool,
        id: i64,
    ) -> Vec<(Option<String>, String)> {
        sqlx::query_as(
            "SELECT from_status, to_status FROM knowledge_events WHERE knowledge_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Spec §4.4: deleting a team archives the memory that was scoped to it, in the same
    /// transaction, and touches nothing else. The `agent` row carrying the team's id proves the
    /// scope kind is honoured and not only the id; the `rejected` row proves a row that is already
    /// out of candidacy keeps the status it earned.
    #[tokio::test]
    async fn deleting_a_team_archives_its_memory_and_nothing_else() {
        let (state, _root) = state_with_root().await;
        let t = marketing(&state).await.team.id;
        insert_agent(&state, "second-director", "claude").await;
        let u = create(
            &state.pool,
            TeamRequest {
                name: "Support".to_owned(),
                mission: "answer people".to_owned(),
                director_agent_id: "second-director".to_owned(),
                max_rounds: 3,
                max_parallel: 2,
                budget_usd: None,
                max_open_actions: DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: Vec::new(),
            },
        )
        .await
        .unwrap()
        .team
        .id;
        let active = seed_knowledge(&state.pool, "team", &t, "active").await;
        let proposed = seed_knowledge(&state.pool, "team", &t, "proposed").await;
        let rejected = seed_knowledge(&state.pool, "team", &t, "rejected").await;
        let others = seed_knowledge(&state.pool, "team", &u, "active").await;
        let agent_row = seed_knowledge(&state.pool, "agent", &t, "active").await;

        delete(&state.pool, &t).await.unwrap();

        for id in [active, proposed] {
            let (status, ended_at) = knowledge_status(&state.pool, id).await;
            assert_eq!(status, "archived", "row {id}");
            assert!(ended_at.is_some(), "row {id} must carry an ended_at");
        }
        assert_eq!(knowledge_status(&state.pool, rejected).await.0, "rejected");
        assert_eq!(knowledge_status(&state.pool, others).await.0, "active");
        assert_eq!(knowledge_status(&state.pool, agent_row).await.0, "active");

        assert_eq!(
            knowledge_transitions(&state.pool, active).await,
            [(Some("active".to_owned()), "archived".to_owned())]
        );
        assert_eq!(
            knowledge_transitions(&state.pool, proposed).await,
            [(Some("proposed".to_owned()), "archived".to_owned())]
        );
        for id in [rejected, others, agent_row] {
            assert!(
                knowledge_transitions(&state.pool, id).await.is_empty(),
                "row {id}"
            );
        }
    }

    /// Spec §4.4: the archive rides the same transaction as the delete, so a refused delete leaves
    /// the memory exactly as it was, with no event written.
    #[tokio::test]
    async fn a_team_with_runs_on_record_keeps_its_memory() {
        let (state, _root) = state_with_root().await;
        let team = marketing(&state).await.team.id;
        sqlx::query(
            "INSERT INTO team_runs (id, team_id, request, workspace, token, state,
                                    created_at, updated_at)
             VALUES ('tr-finished', ?, 'write it', 'ws', 'a-secret', 'done',
                     '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')",
        )
        .bind(&team)
        .execute(&state.pool)
        .await
        .unwrap();
        let memory = seed_knowledge(&state.pool, "team", &team, "active").await;

        let outcome = delete(&state.pool, &team).await;

        assert!(matches!(outcome, Err(TeamError::Invalid(_))), "{outcome:?}");
        assert_eq!(knowledge_status(&state.pool, memory).await.0, "active");
        assert!(knowledge_transitions(&state.pool, memory).await.is_empty());
    }

    #[tokio::test]
    async fn loadout_deleting_a_team_removes_its_tools_and_events() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO agents
                 (id, name, speciality, prompt, engine, model, tool_policy, created_at, updated_at)
             VALUES ('director', 'Director', 'plans', 'p', 'claude', NULL, 'mcp_only',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for id in ["marketing", "sales"] {
            sqlx::query(
                "INSERT INTO teams
                     (id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                      created_at, updated_at)
                 VALUES (?, ?, 'sell', 'director', 3, 2, NULL,
                         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .bind(id)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        let mut rows = Vec::new();
        for owner in ["marketing", "sales"] {
            let row = sqlx::query(
                "INSERT INTO loadout_tools (owner_kind, owner_id, tool, status, source, created_at)
                 VALUES ('team', ?, 'web_read', 'active', 'owner', '2026-01-01T00:00:00Z')",
            )
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
            sqlx::query(
                "INSERT INTO loadout_tool_events (tool_row_id, from_status, to_status, at)
                 VALUES (?, NULL, 'active', '2026-01-01T00:00:00Z')",
            )
            .bind(row)
            .execute(&pool)
            .await
            .unwrap();
            rows.push(row);
        }

        delete(&pool, "marketing").await.unwrap();

        let tools = "SELECT COUNT(*) FROM loadout_tools WHERE id = ?";
        let events = "SELECT COUNT(*) FROM loadout_tool_events WHERE tool_row_id = ?";
        for (sql, row, expected, what) in [
            (tools, rows[0], 0_i64, "the deleted team's tool row stayed"),
            (events, rows[0], 0, "the deleted team's events stayed"),
            (tools, rows[1], 1, "another team's tool row went"),
            (events, rows[1], 1, "another team's events went"),
        ] {
            let found: i64 = sqlx::query_scalar(sql)
                .bind(row)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(found, expected, "{what}");
        }
    }

    /// A tick advances a snapshot of the live runs. A cancel that lands between that read and the
    /// spawn finds no node to kill, marks the run cancelled and returns, and the tick then launches
    /// a node for a run that has ended: an agent of a cancelled department, spending, with nobody
    /// left to stop it. The launch re-reads the run once the node is registered and stops it.
    #[tokio::test]
    async fn a_node_launched_from_a_snapshot_the_cancel_outran_is_stopped() {
        let (mut state, _root) = state_with_root().await;
        let fake = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default());
        *fake.delay.lock().unwrap() = Some(std::time::Duration::from_secs(30));
        state.runner = fake.clone();
        // The tick's snapshot, taken while the run is still `working`.
        let run = team_run_for_member(&state, "tr-cancel-race").await;
        super::lifecycle::cancel(&state, &run.id).await.unwrap();

        let agent = crate::agent::Agent {
            engine: "claude".to_owned(),
            model: None,
            ..local_member("unused")
        };
        let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();
        spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;

        assert!(
            !state.run_handles.lock().unwrap().contains_key(&run_id),
            "the node of a cancelled run is still registered, and still running"
        );
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    /// A seat is launched with what the run has left of its team's ceiling, and only by a runner
    /// that enforces one: `CodexCliRunner` refuses any request carrying `max_budget_usd`, so a
    /// daemon on Codex must keep launching its seats uncapped rather than failing every one.
    #[tokio::test]
    async fn a_seat_is_capped_at_what_its_run_has_left_only_by_a_runner_that_enforces_it() {
        for (honours, expected) in [(true, Some(1.5)), (false, None)] {
            let (mut state, _root) = state_with_root().await;
            let fake = std::sync::Arc::new(crate::execution::runner::FakeCommandRunner {
                honours_budget: honours,
                ..Default::default()
            });
            state.runner = fake.clone();
            let run = team_run_for_member(&state, "tr-seat-budget").await;
            sqlx::query("UPDATE teams SET budget_usd = 2.0 WHERE id = 'marketing'")
                .execute(&state.pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO runs (prompt, status, mode, team_run_id, cost_usd, created_at)
                 VALUES ('earlier seat', 'completed', 'team', ?, 0.5, '2026-08-26T00:00:00Z')",
            )
            .bind(&run.id)
            .execute(&state.pool)
            .await
            .unwrap();
            let agent = crate::agent::Agent {
                engine: "claude".to_owned(),
                model: None,
                ..local_member("unused")
            };
            let (run_id, session_id) = open_run(&state, &run.id, "find it", false).await.unwrap();

            spawn_agent(&state, &run, &agent, run_id, session_id, "find it".into()).await;
            settled_run(&state, run_id).await;

            assert_eq!(
                *fake.last_max_budget_usd.lock().unwrap(),
                Some(expected),
                "honours_max_budget = {honours}"
            );
        }
    }
}
