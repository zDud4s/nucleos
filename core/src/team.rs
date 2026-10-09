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

/// PURE: what a team may declare about itself.
///
/// An EMPTY ROSTER IS ALLOWED here and refused at the start of a run, and that split is deliberate:
/// a team halfway through being assembled is a legitimate state to save, and a team with nobody in
/// it is only a problem at the moment somebody asks it to work.
fn validate(request: &TeamRequest) -> Result<(), TeamError> {
    if request.name.trim().is_empty() {
        return Err(TeamError::Invalid("name must not be empty"));
    }
    if request.mission.trim().is_empty() {
        return Err(TeamError::Invalid(
            "mission must not be empty — it is what the director is told the department is for",
        ));
    }
    if !(1..=MAX_ROUNDS_CEILING).contains(&request.max_rounds) {
        return Err(TeamError::Invalid(
            "max_rounds is outside what a daemon allows",
        ));
    }
    if !(1..=MAX_PARALLEL_CEILING).contains(&request.max_parallel) {
        return Err(TeamError::Invalid(
            "max_parallel is outside what a daemon allows",
        ));
    }
    if request.budget_usd.is_some_and(|budget| budget <= 0.0) {
        return Err(TeamError::Invalid(
            "a budget of zero or less is a team that can never run; leave it unset instead",
        ));
    }
    // Zero is allowed and means something: a team that may hold no action open is one whose grants
    // are all `allow` or none, which is a legitimate thing to configure deliberately.
    if !(0..=MAX_OPEN_ACTIONS_CEILING).contains(&request.max_open_actions) {
        return Err(TeamError::Invalid(
            "max_open_actions is outside what a daemon allows",
        ));
    }
    if !(1..=crate::team_trigger::MAX_LIVE_TEAM_RUNS).contains(&request.max_live_runs) {
        return Err(TeamError::Invalid(
            "max_live_runs is outside what a daemon allows",
        ));
    }
    for grant in &request.grants {
        if !GRANTABLE_ACTIONS.contains(&grant.kind.as_str()) {
            return Err(TeamError::Invalid(
                "that is not an action a department may be granted",
            ));
        }
        if !GRANT_MODES.contains(&grant.mode.as_str()) {
            return Err(TeamError::Invalid("a grant is either propose or allow"));
        }
    }
    Ok(())
}

/// The id a name earns, once, and then frozen — `agent::slug`'s argument applies unchanged:
/// `team_members`, `team_runs.team_id` and the folder path on disk all point at it.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for character in name.trim().chars() {
        if character.is_ascii_alphanumeric() {
            out.extend(character.to_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_end_matches('-').to_owned()
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
}

/// Every agent the team names must be in the catalogue, asked BEFORE the write.
///
/// The foreign keys already refuse it, and they refuse it as a 500 naming nothing. The same
/// question one statement earlier is a 400 that says which agent — `agent::delete` makes the same
/// trade in the other direction.
async fn every_agent_exists(
    pool: &sqlx::SqlitePool,
    request: &TeamRequest,
) -> Result<(), TeamError> {
    for id in std::iter::once(&request.director_agent_id).chain(request.members.iter()) {
        let known: Option<String> = sqlx::query_scalar("SELECT id FROM agents WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
        if known.is_none() {
            return Err(TeamError::UnknownAgent(id.clone()));
        }
    }
    Ok(())
}

pub async fn create(pool: &sqlx::SqlitePool, request: TeamRequest) -> Result<TeamView, TeamError> {
    validate(&request)?;
    every_agent_exists(pool, &request).await?;
    let id = slug(&request.name);
    if id.is_empty() {
        return Err(TeamError::Invalid("name must contain a letter or a digit"));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                            budget_usd, max_open_actions, max_live_runs, created_at,
                            updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&request.name)
    .bind(&request.mission)
    .bind(&request.director_agent_id)
    .bind(request.max_rounds)
    .bind(request.max_parallel)
    .bind(request.budget_usd)
    .bind(request.max_open_actions)
    .bind(request.max_live_runs)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await;
    match result {
        Ok(_) => {}
        Err(error) if is_unique_violation(&error) => return Err(TeamError::DuplicateName),
        Err(error) => return Err(TeamError::Db(error)),
    }
    replace_roster(pool, &id, &request.members).await?;
    replace_grants(pool, &id, &request.grants).await?;
    get(pool, &id).await?.ok_or(TeamError::NotFound)
}

/// What a department may ask the core to do on its behalf.
///
/// **Not "the `Acts` tools".** Those are `approve_proposal`, `cancel_run`, `create_job`,
/// `create_run`, `reject_proposal`, `set_kill`, `triage_email`, `vcs_request` and `send_email` —
/// which is to say, the controls of the daemon itself. A department holding `set_kill` turns off the
/// house's autonomy; one holding `approve_proposal` approves its own proposals and thereby holds
/// every other authority by transitivity, without anybody having written that down anywhere.
///
/// So an alçada is not about tools. It is about ACTIONS, and this list is short, explicit, and
/// checked before the table is: a row in `team_grants` naming something absent from here grants
/// nothing (`a_kind_outside_the_list_is_refused_even_with_a_grant_in_the_table`).
///
/// `vcs_ticket` was in the design and is deliberately NOT here. The design called it "the cheapest
/// of the four to undo", and that was written down wrongly: `TOOL_EFFECTS` records `vcs_request` as
/// "the sharpest `Acts` on the list… the only effect on this list that outlives the daemon, and the
/// only one its owner cannot take back from here". It moves a branch in a repository other people
/// build on. Handing a department the one irreversible effect in the house as its first authority
/// is exactly backwards, and choosing WHICH git operations a department may request is a design
/// decision no spec has made. Refused until one does — asserted by
/// `no_grantable_action_is_a_control_of_the_daemon`.
pub const GRANTABLE_ACTIONS: &[&str] = &["calendar_event", "file_document", "send_email"];

/// `propose` puts a human in the middle; `allow` does not.
///
/// There is no `deny`, and its absence is the design: a team with no row for a kind may not ask for
/// it, exactly as `auth::permits` refuses anything not listed. Two ways of saying no is where they
/// eventually disagree.
const GRANT_MODES: &[&str] = &["propose", "allow"];

/// How many actions a team may leave waiting for a decision, absent an owner's opinion.
pub const DEFAULT_MAX_OPEN_ACTIONS: i64 = 5;

/// And the ceiling on that opinion. A queue nobody can work through is a queue that gets approved
/// unread, which is worse than one that refuses to grow.
pub const MAX_OPEN_ACTIONS_CEILING: i64 = 20;

/// One line of a team's alçada: an action, and whether a human sees it first.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct TeamGrant {
    pub kind: String,
    pub mode: String,
}

pub async fn grants(pool: &sqlx::SqlitePool, team_id: &str) -> Result<Vec<TeamGrant>, TeamError> {
    sqlx::query_as("SELECT kind, mode FROM team_grants WHERE team_id = ? ORDER BY kind")
        .bind(team_id)
        .fetch_all(pool)
        .await
        .map_err(TeamError::Db)
}

/// Replaced wholesale, for `replace_roster`'s reason: the editor sends the alçada as the owner left
/// it, and a merge would make revoking one impossible through the only surface that edits it.
async fn replace_grants(
    pool: &sqlx::SqlitePool,
    team_id: &str,
    grants: &[TeamGrant],
) -> Result<(), TeamError> {
    sqlx::query("DELETE FROM team_grants WHERE team_id = ?")
        .bind(team_id)
        .execute(pool)
        .await?;
    for grant in grants {
        sqlx::query("INSERT OR REPLACE INTO team_grants (team_id, kind, mode) VALUES (?, ?, ?)")
            .bind(team_id)
            .bind(&grant.kind)
            .bind(&grant.mode)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// The roster is REPLACED and not merged, because that is what the editor sends: the whole
/// membership as the owner left it. A merge would make removing somebody impossible through the
/// only surface that edits one.
async fn replace_roster(
    pool: &sqlx::SqlitePool,
    team_id: &str,
    members: &[String],
) -> Result<(), TeamError> {
    sqlx::query("DELETE FROM team_members WHERE team_id = ?")
        .bind(team_id)
        .execute(pool)
        .await?;
    for agent_id in members {
        sqlx::query("INSERT OR IGNORE INTO team_members (team_id, agent_id) VALUES (?, ?)")
            .bind(team_id)
            .bind(agent_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn list(pool: &sqlx::SqlitePool) -> Result<Vec<TeamView>, TeamError> {
    let teams: Vec<Team> = sqlx::query_as(
        "SELECT id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                max_open_actions, max_live_runs, created_at, updated_at
         FROM teams ORDER BY name",
    )
    .fetch_all(pool)
    .await?;

    let mut views = Vec::with_capacity(teams.len());
    for team in teams {
        let members = roster(pool, &team.id).await?;
        let grants = grants(pool, &team.id).await?;
        views.push(TeamView {
            team,
            members,
            grants,
        });
    }
    Ok(views)
}

pub async fn get(pool: &sqlx::SqlitePool, id: &str) -> Result<Option<TeamView>, TeamError> {
    let team: Option<Team> = sqlx::query_as(
        "SELECT id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                max_open_actions, max_live_runs, created_at, updated_at
         FROM teams WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    match team {
        None => Ok(None),
        Some(team) => {
            let members = roster(pool, &team.id).await?;
            let grants = grants(pool, &team.id).await?;
            Ok(Some(TeamView {
                team,
                members,
                grants,
            }))
        }
    }
}

pub async fn roster(pool: &sqlx::SqlitePool, team_id: &str) -> Result<Vec<String>, TeamError> {
    sqlx::query_scalar("SELECT agent_id FROM team_members WHERE team_id = ? ORDER BY agent_id")
        .bind(team_id)
        .fetch_all(pool)
        .await
        .map_err(TeamError::Db)
}

/// Everybody in this department that one of its members may address: the roster AND the director.
///
/// **`roster` alone is not this list, and the difference is not cosmetic.** `team_members` holds the
/// specialists; the director lives in `teams.director_agent_id` and is not a row there. So a
/// membership test against `roster` refuses the director — which would have made "tell the director
/// what you found" impossible, and that is the single most valuable message this department will
/// ever pass. It is exactly the sentence a specialist has to be able to send: the director is the
/// node that decides what the next round does.
///
/// A function rather than two calls at each use site, because there are two use sites — the address
/// book a specialist is shown, and the check that refuses an unknown name — and a list you can be
/// SHOWN but not WRITE to, or write to but never see, is the same bug from either side.
///
/// Deduplicated because a director may also sit on its own roster; nothing forbids it, and a name
/// printed twice reads as two people.
pub async fn addressable(pool: &sqlx::SqlitePool, team_id: &str) -> Result<Vec<String>, TeamError> {
    let mut all = roster(pool, team_id).await?;
    let director: Option<String> =
        sqlx::query_scalar("SELECT director_agent_id FROM teams WHERE id = ?")
            .bind(team_id)
            .fetch_optional(pool)
            .await
            .map_err(TeamError::Db)?;
    if let Some(director) = director
        && !all.contains(&director)
    {
        all.push(director);
    }
    all.sort();
    Ok(all)
}

/// The id is deliberately NOT recomputed from a new name: it is a reference, and a folder on disk
/// is named after it.
pub async fn update(
    pool: &sqlx::SqlitePool,
    id: &str,
    request: TeamRequest,
) -> Result<TeamView, TeamError> {
    validate(&request)?;
    every_agent_exists(pool, &request).await?;
    let affected = sqlx::query(
        "UPDATE teams
         SET name = ?, mission = ?, director_agent_id = ?, max_rounds = ?, max_parallel = ?,
             budget_usd = ?, max_open_actions = ?, max_live_runs = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(&request.name)
    .bind(&request.mission)
    .bind(&request.director_agent_id)
    .bind(request.max_rounds)
    .bind(request.max_parallel)
    .bind(request.budget_usd)
    .bind(request.max_open_actions)
    .bind(request.max_live_runs)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(pool)
    .await;
    match affected {
        Ok(result) if result.rows_affected() == 0 => return Err(TeamError::NotFound),
        Ok(_) => {}
        Err(error) if is_unique_violation(&error) => return Err(TeamError::DuplicateName),
        Err(error) => return Err(TeamError::Db(error)),
    }
    replace_roster(pool, id, &request.members).await?;
    // Revoking here does NOT cancel an action already proposed. The alçada is read when the agent
    // asks, not when the core executes — see `execute_due_actions`, and the design's risk 1.
    replace_grants(pool, id, &request.grants).await?;
    get(pool, id).await?.ok_or(TeamError::NotFound)
}

/// Refused while any run of this team is still live, for the reason `agent::delete` refuses a
/// director: the constraint would come back as a 500 naming nothing, and the same question asked
/// one statement earlier is a 409 that says why.
pub async fn delete(pool: &sqlx::SqlitePool, id: &str) -> Result<(), TeamError> {
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM team_runs
         WHERE team_id = ? AND state IN ('planning', 'working', 'delivering')",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;
    if live > 0 {
        return Err(TeamError::Invalid(
            "that team has a run in flight; cancel it first",
        ));
    }
    let past: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_runs WHERE team_id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    if past > 0 {
        return Err(TeamError::Invalid(
            "that team has runs on record; deleting it would orphan their deliveries",
        ));
    }

    // One transaction for the whole cascade (spec §4.4): the team and everything that hangs off
    // it — the loadout rows, context refs, and memory archive included — go together or not at
    // all. A NotFound below returns before the commit, and dropping the transaction rolls it back.
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM team_members WHERE team_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    // The alçada goes with the team it described. There is no equivalent worry about
    // `team_actions`: an action belongs to a RUN, and the check above already refuses to delete a
    // team that has ever had one — so a team reaching this line has no runs and therefore no
    // actions, pending or otherwise.
    sqlx::query("DELETE FROM team_grants WHERE team_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    // The team's context refs go with it: they are owned rows with no foreign key to cascade from.
    sqlx::query("DELETE FROM context_refs WHERE owner_kind = 'team' AND owner_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    // This team's own rules go with it — a rule that starts a department which no longer exists
    // fires at nothing, every window, for ever.
    sqlx::query(
        "DELETE FROM team_trigger_state WHERE trigger_id IN
           (SELECT id FROM team_triggers WHERE team_id = ?)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM team_triggers WHERE team_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    // Rules that fire ON this team are DISARMED and not deleted — the opposite treatment, for the
    // opposite reason. Such a rule still describes something its author wanted and has merely lost
    // its signal: deleting it throws away their sentence, and leaving it armed leaves a rule that
    // can never fire looking like one that might.
    sqlx::query(
        "UPDATE team_triggers SET enabled = 0, updated_at = ?
          WHERE source = 'team_finished' AND from_team = ?",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(&mut *tx)
    .await?;
    crate::tool_loadout::delete_for_owner(&mut tx, "team", id).await?;
    let result = sqlx::query("DELETE FROM teams WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        return Err(TeamError::NotFound);
    }
    // Spec §4.4: the team's memory is archived with it, in this same transaction.
    crate::agent::archive_owner_memory(&mut tx, "team", id).await?;
    tx.commit().await?;
    Ok(())
}

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

/// One item a director asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedItem {
    pub agent_id: String,
    pub description: String,
}

/// What one director node produced, after the roster and the ceiling have been applied.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedItems {
    pub items: Vec<PlannedItem>,
    /// What was cut, and why — reported rather than discarded, because a silently truncated queue
    /// reads downstream as the whole of what the planner found.
    pub dropped: Vec<String>,
    pub done: bool,
    pub why: Option<String>,
}

/// PURE: the plan a director wrote, or `None` if there is no plan in what it wrote.
///
/// `None` is the signal the round machine acts on — a relaunch in round 0, a dry round after — so
/// it must mean "unreadable" and never "readable and empty". A readable plan with no items is
/// `Some` with an empty list, and those two travel to different places.
///
/// The JSON is looked for inside the answer rather than expected to be the whole of it, because a
/// model asked for JSON writes a sentence around it perhaps a third of the time. An `agent_id`
/// outside the roster drops THAT ITEM and not the plan: a director that misspells one name in five
/// should not cost the round.
pub fn parse_team_plan(text: &str, roster: &[String], ceiling: usize) -> Option<PlannedItems> {
    let value = extract_json(text)?;

    // `done: true` wins over any items beside it, for the reason `job::parse_plan` gives: the model
    // said the work is finished, and queueing work it also mentioned would be obeying the half of
    // the answer that costs money.
    let done = value.get("done").and_then(serde_json::Value::as_bool) == Some(true);
    let why = value
        .get("why")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let mut items = Vec::new();
    let mut dropped = Vec::new();
    if !done {
        let raw = value
            .get("items")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in raw {
            let agent_id = entry.get("agent_id").and_then(serde_json::Value::as_str);
            let description = entry.get("description").and_then(serde_json::Value::as_str);
            let (Some(agent_id), Some(description)) = (agent_id, description) else {
                dropped.push("an item named no agent or no work".to_owned());
                continue;
            };
            if !roster.iter().any(|member| member == agent_id) {
                dropped.push(format!("{agent_id} is not on this team"));
                continue;
            }
            if description.trim().is_empty() {
                dropped.push(format!("{agent_id} was given no work to do"));
                continue;
            }
            if items.len() == ceiling {
                dropped.push(format!(
                    "{agent_id} was cut: a round queues at most {ceiling} items"
                ));
                continue;
            }
            items.push(PlannedItem {
                agent_id: agent_id.to_owned(),
                description: description.trim().to_owned(),
            });
        }
    }

    Some(PlannedItems {
        items,
        dropped,
        done,
        why,
    })
}

/// The outermost JSON object in a string, or `None`.
///
/// Whole-string first, because that is what a well-behaved answer is and parsing it directly is
/// what keeps a `}` inside a string literal from confusing the fallback. The fallback spans the
/// first `{` to the last `}`, which is the shape a model produces when it wraps the object in prose
/// or a code fence.
fn extract_json(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && value.is_object()
    {
        return Some(value);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&trimmed[start..=end])
        .ok()
        .filter(serde_json::Value::is_object)
}

/// Where a run goes when a director node lands.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub state: &'static str,
    pub round: i64,
    pub dry_rounds: i64,
    pub plan_retries: i64,
    /// Whether the plan's items should be queued. `false` whenever the run is leaving `working`,
    /// so that items are never written for a round that will not run.
    pub queue_items: bool,
    pub why: Option<String>,
}

/// PURE: the whole round machine, in one function.
///
/// Every transition of the design's §3 table that a director node can cause is here, and the two
/// call sites — the tick and the reconciliation — share it rather than each implementing the half
/// it needs. `plan: None` means the node produced nothing readable.
///
/// **The order is: increment the round, then check the ceiling.** A team with `max_rounds = 3` runs
/// rounds 0, 1 and 2, and the check catches it as round 3 opens. Written down because it is exactly
/// the kind of detail an implementer guesses, and guesses wrong half the time.
pub fn next_after_director(
    state: &str,
    plan: Option<&PlannedItems>,
    round: i64,
    dry_rounds: i64,
    plan_retries: i64,
    max_rounds: i64,
) -> Progress {
    let unchanged = Progress {
        state: "working",
        round,
        dry_rounds,
        plan_retries,
        queue_items: false,
        why: None,
    };

    if state == "planning" {
        return match plan {
            // Round 0 has no partial work to hand over, so an unreadable plan is worth one more
            // attempt and then nothing.
            None if plan_retries < PLAN_RETRY_LIMIT => Progress {
                state: "planning",
                plan_retries: plan_retries + 1,
                ..unchanged
            },
            None => Progress {
                state: "failed",
                why: Some("the director's plan could not be read, twice".to_owned()),
                ..unchanged
            },
            Some(plan) if plan.done => Progress {
                state: "delivering",
                why: plan.why.clone(),
                ..unchanged
            },
            // Readable, not done, and nothing to delegate. Not a dry round — there has been no
            // round — and not a failure either: the director read the request and found nothing to
            // hand out, which is an answer the delivery node should write down.
            Some(plan) if plan.items.is_empty() => Progress {
                state: "delivering",
                why: plan
                    .why
                    .clone()
                    .or_else(|| Some("the director queued no work".to_owned())),
                ..unchanged
            },
            Some(_) => Progress {
                state: "working",
                queue_items: true,
                ..unchanged
            },
        };
    }

    // A round ended. It counts whether or not the replan could be read — an unreadable plan is a
    // dry round, because there is work in the folder either way.
    let round = round + 1;
    let produced = plan.is_some_and(|plan| !plan.done && !plan.items.is_empty());
    let dry_rounds = if produced { 0 } else { dry_rounds + 1 };
    let done = plan.is_some_and(|plan| plan.done);

    if done || dry_rounds >= DRY_ROUNDS_TO_STOP || round >= max_rounds {
        return Progress {
            state: "delivering",
            round,
            dry_rounds,
            plan_retries,
            queue_items: false,
            why: plan.and_then(|plan| plan.why.clone()).or_else(|| {
                Some(if done {
                    "the director called the work finished".to_owned()
                } else if round >= max_rounds {
                    format!("the team reached its ceiling of {max_rounds} rounds")
                } else {
                    "two rounds in a row added nothing".to_owned()
                })
            }),
        };
    }

    Progress {
        state: "working",
        round,
        dry_rounds,
        plan_retries,
        queue_items: produced,
        why: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Starting a run
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum StartError {
    NotFound,
    Invalid(String),
    BudgetExhausted(String),
    QuotaExhausted(String),
    Unavailable(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("team not found"),
            Self::Invalid(message)
            | Self::BudgetExhausted(message)
            | Self::QuotaExhausted(message)
            | Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

/// `start_with` at `normal`, with no lineage: the shape most tests start a run in. Test-only since
/// `post_team_run` began carrying a speed, which left no production caller.
#[cfg(any(test, feature = "testkit"))]
pub async fn start(
    state: &AppState,
    team_id: &str,
    request: &str,
    report_to_chat_id: Option<&str>,
) -> Result<String, StartError> {
    start_with(
        state,
        team_id,
        request,
        Lineage::default(),
        crate::speed::Speed::Normal,
        report_to_chat_id,
    )
    .await
}

/// Starts a department on one request: validates, mints the key, makes the folder, writes the row.
///
/// Returns as soon as the row exists — everything after is the tick's. The four refusals here are
/// the four things that cannot be discovered later without wasting money: a team with nobody in it,
/// a director that was deleted, a local member on a machine with no local model, and a house budget
/// that is already spent.
///
/// The caller says where the run came from and where it should speak. One function and not two
/// paths: a triggered run is an ordinary run with four columns filled in, and every refusal
/// applies to it unchanged. What a trigger adds — the depth, the tree
/// ceiling, the live-run counts — is checked by `team_trigger::fire` BEFORE it gets here, because
/// those are questions about whether to start at all rather than about whether this team can.
/// `speed` is this run's, chosen by whoever started it (speed spec, section 3.4).
pub async fn start_with(
    state: &AppState,
    team_id: &str,
    request: &str,
    lineage: Lineage,
    speed: crate::speed::Speed,
    report_to_chat_id: Option<&str>,
) -> Result<String, StartError> {
    let request = request.trim();
    if request.is_empty() {
        return Err(StartError::Invalid("the request is empty".to_owned()));
    }

    // Refused HERE and not when the department first tries to speak. A destination that does not
    // exist is a mistake by whoever started the run, and they are present, at a keyboard, right now;
    // discovering it two hours later means the refusal reaches a director mid-turn instead — which
    // can do nothing about it except write the words into a delivery nobody asked for.
    //
    // `chats::brain_of` rather than a second `archived_at IS NULL` clause, for `relay::admit`'s
    // reason: "still there" has exactly one definition in this codebase and this is not a second.
    if let Some(chat_id) = report_to_chat_id {
        let lives = crate::chats::brain_of(&state.pool, chat_id)
            .await
            .map_err(|error| StartError::Unavailable(error.to_string()))?
            .is_some();
        if !lives {
            return Err(StartError::Invalid(format!(
                "there is no conversation `{chat_id}` for this department to report in"
            )));
        }
    }

    let team = get(&state.pool, team_id)
        .await
        .map_err(|error| StartError::Unavailable(error.to_string()))?
        .ok_or(StartError::NotFound)?;

    if team.members.is_empty() {
        return Err(StartError::Invalid(
            "this team has no members yet; add at least one specialist".to_owned(),
        ));
    }

    // The director is asked for by id rather than assumed present: `agent::delete` refuses to
    // delete one, but a database restored from a backup taken before the team existed would not
    // have that history.
    let director = crate::agent::get(&state.pool, &team.team.director_agent_id)
        .await
        .map_err(|error| StartError::Unavailable(error.to_string()))?
        .ok_or_else(|| {
            StartError::Invalid(format!(
                "this team's director ({}) is no longer in the catalogue",
                team.team.director_agent_id
            ))
        })?;

    // A local member with no local assistant on this machine is refused rather than silently run in
    // the cloud, which would swap the model the owner chose for one that spends.
    let mut members = Vec::with_capacity(team.members.len());
    for id in &team.members {
        let agent = crate::agent::get(&state.pool, id)
            .await
            .map_err(|error| StartError::Unavailable(error.to_string()))?
            .ok_or_else(|| {
                StartError::Invalid(format!("{id} is on this team and not in the catalogue"))
            })?;
        members.push(agent);
    }
    // `state.local_assistant.is_none()` before the migration to the assistant factory: the field
    // this read no longer exists, so this call site is the one line that migration is allowed to
    // touch beyond the `AppState` literal.
    if state.assistants.serves(crate::chats::Brain::Local).is_err()
        && let Some(local) = members
            .iter()
            .chain(std::iter::once(&director))
            .find(|agent| agent.engine == "local")
    {
        return Err(StartError::Invalid(format!(
            "{} runs on a local model and this machine has none available",
            local.id
        )));
    }

    match crate::quota::permits_new_run(state, chrono::Utc::now()).await {
        crate::budget::BudgetDecision::Allow => {}
        crate::budget::BudgetDecision::Pause { reason, source, .. } => {
            return Err(if source == crate::quota::PAUSE_SOURCE {
                StartError::QuotaExhausted(reason)
            } else {
                StartError::BudgetExhausted(reason)
            });
        }
    }

    let id = crate::auth::generate_uuid_v4();
    let workspace = workspace_for(&team.team.id, &id);

    // The folder before the row: a run whose row exists and whose folder does not is a run whose
    // every specialist fails on its first write. The other order leaves an empty directory the GC
    // collects, which costs nothing.
    let root = files_root(state).map_err(StartError::Unavailable)?;
    let folder = crate::files::resolve_within(&root, &workspace)
        .map_err(|error| StartError::Unavailable(format!("{error:?}")))?;
    std::fs::create_dir_all(&folder).map_err(|error| StartError::Unavailable(error.to_string()))?;

    let (_, secret) = crate::auth::mint_team_token(&id);
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO team_runs (id, team_id, request, workspace, token, state, created_at,
                                updated_at, trigger_id, parent_id, root_id, depth,
                                report_to_chat_id, speed)
         VALUES (?, ?, ?, ?, ?, 'planning', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&team.team.id)
    .bind(request)
    .bind(&workspace)
    .bind(&secret)
    .bind(&now)
    .bind(&now)
    .bind(lineage.trigger_id)
    .bind(&lineage.parent_id)
    // Its own id when nobody named a root. Written rather than left NULL for the reason the column
    // documents: a sentinel meaning "me" is a `COALESCE` at every reader, and one missing `COALESCE`
    // is a tree ceiling read off the wrong run.
    .bind(lineage.root_id.as_deref().unwrap_or(&id))
    .bind(lineage.depth)
    // NULL for nearly every run, which is what makes a department that reports nowhere behave
    // exactly as it did before this column existed.
    .bind(report_to_chat_id)
    .bind(speed.as_str())
    .execute(&state.pool)
    .await
    .map_err(|error| StartError::Unavailable(error.to_string()))?;

    let _ = crate::feed::append(
        &state.pool,
        None,
        "team_run_started",
        &format!("{} was asked to {request}", team.team.name),
        None,
        Some(&crate::feed::Subject::TeamRun(id.clone())),
    )
    .await;

    Ok(id)
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

/// One thing a department asked the core to do.
#[derive(Debug, Clone, PartialEq, serde::Serialize, sqlx::FromRow)]
pub struct TeamAction {
    pub id: i64,
    pub team_run_id: String,
    /// The item that asked, or NULL when the director did.
    pub ordinal: Option<i64>,
    pub kind: String,
    pub payload: String,
    pub why: String,
    pub proposal_id: Option<i64>,
    pub state: String,
    pub error: Option<String>,
    pub created_at: String,
    pub executed_at: Option<String>,
}

/// Why an action could not even be asked for. Every variant is a refusal handed back to the AGENT,
/// mid-turn, while it can still do something about it.
#[derive(Debug)]
pub enum ActionError {
    /// Not a `Scope::TeamRun` at all.
    NotADepartment,
    /// The run named by the key is gone.
    NoSuchRun,
    /// `kind` is not in `GRANTABLE_ACTIONS`. Nobody can ask for this, granted or not.
    Unknown(String),
    /// This team has no grant for it.
    Ungranted(String),
    /// The payload is not a well-formed request of that kind, with the fault named.
    Malformed(String),
    /// The team already holds as many undecided actions as it may.
    QueueFull(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ActionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADepartment => {
                formatter.write_str("only a department asks the core to act for it")
            }
            Self::NoSuchRun => formatter.write_str("no such team run"),
            Self::Unknown(kind) => write!(formatter, "nobody can do `{kind}`"),
            Self::Ungranted(kind) => {
                write!(formatter, "this department may not do `{kind}`")
            }
            Self::Malformed(why) | Self::QueueFull(why) => formatter.write_str(why),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ProposeActionRequest {
    pub kind: String,
    pub payload: serde_json::Value,
    pub why: String,
}

/// What the agent is told. **A sentence, not a status code**, because the reader is a model that has
/// to decide what to do next: "filed for approval as #41" and "this department may not send email"
/// lead to different paragraphs, and a 403 leads to a retry.
#[derive(Debug, serde::Serialize)]
pub struct ProposeActionResponse {
    pub id: i64,
    pub proposal_id: Option<i64>,
    pub outcome: String,
}

/// How many times one run may speak to its owner unbidden.
///
/// `MAX_ROUNDS_CEILING` director nodes plus the delivery is seven — the most nodes that could ever
/// have something to say — and eight leaves one over. It is a belt rather than a brake: what
/// actually bounds this is that only a DIRECTOR may report and a run has a small, fixed number of
/// director nodes.
pub const MAX_REPORTS_PER_RUN: i64 = 8;

/// A department saying something to its owner.
#[derive(Debug, serde::Deserialize)]
pub struct ReportRequest {
    pub body: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ReportResponse {
    pub id: i64,
    pub outcome: String,
}

/// Why a department could not speak. Every variant is handed to the AGENT, mid-turn.
#[derive(Debug)]
pub enum ReportError {
    /// Not a `Scope::TeamRun` at all.
    NotADepartment,
    /// The run named by the key is gone.
    NoSuchRun,
    /// A specialist tried. A department speaks to its owner with one voice, and the sentence says
    /// what to do instead — the same shape `propose_teammate` uses for the same refusal.
    NotTheDirector,
    /// This run was not pointed at a conversation, which is the ordinary state of nearly every run
    /// and therefore not an error to be retried. Named on its own so the sentence can say so.
    NowhereToReport,
    /// It was pointed at one and that conversation has since been archived. Distinct from
    /// `NowhereToReport` because they lead somewhere different: one means this department was never
    /// given a voice, the other that the room it was given has been closed.
    DestinationGone(String),
    /// Nothing to say.
    Empty,
    /// This run has spoken as often as it may.
    Ceiling(i64),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ReportError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADepartment => {
                formatter.write_str("only a department reports to the owner this way")
            }
            Self::NoSuchRun => formatter.write_str("no such team run"),
            Self::NotTheDirector => formatter.write_str(
                "only the director speaks for this department — put it in your answer and let the \
                 director pass it on",
            ),
            Self::NowhereToReport => formatter.write_str(
                "this department was not pointed at a conversation when it was started, so there is \
                 nowhere to say this; put it in your delivery instead",
            ),
            Self::DestinationGone(chat_id) => write!(
                formatter,
                "the conversation `{chat_id}` this department was told to report in has been \
                 archived; put it in your delivery instead"
            ),
            Self::Empty => formatter.write_str("say something, or say nothing at all"),
            Self::Ceiling(ceiling) => write!(
                formatter,
                "this department has already spoken {ceiling} times this run, which is as many as \
                 it may; put the rest in your delivery"
            ),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

/// A director says something in the conversation its run was pointed at. Nothing runs.
///
/// **The destination is read from the run and never from the request.** `team_runs.report_to_chat_id`
/// is written when the run is started, by the person starting it; there is no parameter here, no
/// tool that takes one, and no route a department could use to discover what conversations exist. A
/// department cannot choose who it talks to — it can only answer to the address it was given.
///
/// **Only the director**, narrowed here against `team_runs.director_run_id` exactly as
/// `propose_teammate` is, and for the same two reasons: a department speaks to its owner with one
/// voice, and the key names the RUN rather than the node, so this cannot be a question for
/// `auth::permits`.
pub async fn report(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &ReportRequest,
) -> Result<ReportResponse, ReportError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(ReportError::NotADepartment);
    };

    let body = request.body.trim();
    if body.is_empty() {
        return Err(ReportError::Empty);
    }

    let destination: Option<String> =
        sqlx::query_scalar("SELECT report_to_chat_id FROM team_runs WHERE id = ?")
            .bind(team_run_id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or(ReportError::NoSuchRun)?;
    let Some(chat_id) = destination else {
        return Err(ReportError::NowhereToReport);
    };

    // The director check comes AFTER the destination one on purpose. A specialist calling this in a
    // department that reports nowhere should be told the thing it can act on — there is nowhere to
    // say this, put it in your answer — rather than a rule about who may speak that would not have
    // helped it even if it were the director.
    let Some((from_agent_id, from_run_id)) =
        agent_of_caller(&state.pool, team_run_id, headers).await?
    else {
        return Err(ReportError::NotTheDirector);
    };
    if calling_node(&state.pool, team_run_id, headers).await != Caller::Director {
        return Err(ReportError::NotTheDirector);
    }

    // Checked again here even though `start_with` refused an absent one: a run lasts up to four
    // hours and a conversation can be archived inside that. Reported as its own refusal rather than
    // written into a conversation nobody will open again.
    if crate::chats::brain_of(&state.pool, &chat_id)
        .await?
        .is_none()
    {
        return Err(ReportError::DestinationGone(chat_id));
    }

    let said: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_notices WHERE team_run_id = ?")
        .bind(team_run_id)
        .fetch_one(&state.pool)
        .await?;
    if said >= MAX_REPORTS_PER_RUN {
        return Err(ReportError::Ceiling(MAX_REPORTS_PER_RUN));
    }

    let id = crate::chat_notices::post(
        &state.pool,
        &chat_id,
        team_run_id,
        &from_agent_id,
        from_run_id,
        body,
    )
    .await?;

    Ok(ReportResponse {
        id,
        // Says the part a model would otherwise assume away: it was SHOWN, not answered. A director
        // that believes it has started a conversation waits for a reply that is never coming, and
        // spends its remaining turns waiting.
        outcome:
            "shown to the owner. Nobody will answer it — it is a message on their screen, not \
                  a question — so carry on, and put anything that matters in your delivery as well."
                .to_owned(),
    })
}

/// One member of a department leaving words for another.
#[derive(Debug, serde::Deserialize)]
pub struct TeamNoteRequest {
    /// The colleague's `agents.id`, as printed on the left of their line in `roster_lines`.
    pub to: String,
    pub body: String,
}

/// What the agent is told, and it is a sentence for `ProposeActionResponse`'s reason: the reader is
/// a model deciding what to do next, and "they may not be started again this run" leads somewhere
/// different from "filed".
#[derive(Debug, serde::Serialize)]
pub struct TeamNoteResponse {
    pub id: i64,
    pub outcome: String,
}

/// Why a note could not be left. Every variant is handed to the AGENT, mid-turn, while it can still
/// do something about it — which is why `NoSuchColleague` names the roster rather than saying "no".
#[derive(Debug)]
pub enum NoteError {
    /// Not a `Scope::TeamRun` at all.
    NotADepartment,
    /// The run named by the key is gone.
    NoSuchRun,
    /// The daemon cannot tell which node is calling: no header, a stale id, or one belonging to
    /// another run. Refused rather than attributed to the department at large — an unattributed note
    /// is a second brief with no way to weigh it, which is the one thing the receiving node cannot
    /// recover from.
    UnknownNode,
    /// `to` is not on this department's roster, which is listed so the model can fix it.
    NoSuchColleague {
        asked: String,
        roster: Vec<String>,
    },
    /// `to` is the caller. Not harmful, and refused anyway: the words would arrive in the caller's
    /// own next prompt as though a colleague had sent them, which is a turn arguing with itself.
    Yourself,
    /// Nothing to say. A note with no words is an entry in the queue that delivers nothing and can
    /// never be delivered again.
    Empty,
    /// This run has left as many notes as it may.
    Ceiling(i64),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for NoteError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for NoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADepartment => {
                formatter.write_str("only a member of a department leaves notes for its colleagues")
            }
            Self::NoSuchRun => formatter.write_str("no such team run"),
            Self::UnknownNode => formatter.write_str(
                "this call does not say which node of the department is making it, and a note has                  to be signed by somebody",
            ),
            Self::NoSuchColleague { asked, roster } => write!(
                formatter,
                "`{asked}` is not in this department. Its members are: {}",
                roster.join(", ")
            ),
            Self::Yourself => formatter.write_str(
                "that is you — put it in your own answer instead, which is where your own findings go",
            ),
            Self::Empty => formatter.write_str("say something, or say nothing at all"),
            Self::Ceiling(ceiling) => write!(
                formatter,
                "this department has already left {ceiling} notes this run, which is as many as it                  may; put it in your answer instead"
            ),
            // The raw error, as all four siblings in this file render theirs. A friendlier sentence
            // here would read better to a model and would be the only one of five that hides what
            // actually happened — and the reader of a 500 is a person looking at a log, not the
            // agent, which got its answer and moved on.
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

/// Files one member's words for another, and does nothing else: nobody is woken and no round moves.
///
/// **Who is speaking is read from the call and never from the body.** `Scope::TeamRun` names the
/// run, `RUN_ID_HEADER` names the node, and `agent_of_caller` turns the two into an `agents.id`. A
/// `from` field on the request would let a specialist sign a colleague's name to its own finding —
/// and the receiving node, reading a quoted paragraph attributed by the daemon, has no way at all to
/// check it.
///
/// **The roster is the address book and the boundary at once.** A department may write to its own
/// members and to nobody else: there is no cross-team address, no run id to name, and no way to
/// reach a conversation. `admits`-style chain brakes are not needed here for the reason
/// `MAX_RELAY_DEPTH` exists at all — a chain can only grow one hop per ROUND, and rounds are already
/// bounded by `max_rounds` and by `DRY_ROUNDS_TO_STOP`. `MAX_NOTES_PER_RUN` is the belt.
pub async fn send_note(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &TeamNoteRequest,
) -> Result<TeamNoteResponse, NoteError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(NoteError::NotADepartment);
    };

    let body = request.body.trim();
    if body.is_empty() {
        return Err(NoteError::Empty);
    }

    let team_id: String = sqlx::query_scalar("SELECT team_id FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(NoteError::NoSuchRun)?;

    let Some((from_agent_id, from_run_id)) =
        agent_of_caller(&state.pool, team_run_id, headers).await?
    else {
        return Err(NoteError::UnknownNode);
    };

    let asked = request.to.trim();
    if asked == from_agent_id {
        return Err(NoteError::Yourself);
    }
    // `addressable` and NOT `roster`, and the first draft of this used `roster` with a comment
    // asserting the director was on it. It is not — `team_members` holds the specialists and the
    // director lives on `teams.director_agent_id` — so that draft refused the one message this
    // whole feature exists to carry. A test caught it; the comment would not have.
    let roster = addressable(&state.pool, &team_id)
        .await
        .map_err(|error| match error {
            TeamError::Db(error) => NoteError::Db(error),
            _ => NoteError::NoSuchRun,
        })?;
    if !roster.iter().any(|member| member == asked) {
        return Err(NoteError::NoSuchColleague {
            asked: asked.to_owned(),
            roster,
        });
    }

    // Counted per RUN and not per member: what the ceiling protects against is a department talking
    // instead of working, and that is a property of the department rather than of any one of them.
    let left = crate::team_notes::count_for_run(&state.pool, team_run_id).await?;
    if left >= crate::team_notes::MAX_NOTES_PER_RUN {
        return Err(NoteError::Ceiling(crate::team_notes::MAX_NOTES_PER_RUN));
    }

    let id = crate::team_notes::leave(
        &state.pool,
        team_run_id,
        &from_agent_id,
        from_run_id,
        asked,
        body,
    )
    .await?;

    Ok(TeamNoteResponse {
        id,
        // Said plainly, including the part a model would otherwise assume away: there is no reply
        // coming, and there may be no delivery either. A specialist that believes it has handed the
        // problem over stops carrying it, and the department loses the finding twice.
        outcome: format!(
            "left for {asked}. They will read it at the top of their brief the next time this              department starts them, which may not happen — say it in your own answer too, and do              not wait for a reply, because there is none coming."
        ),
    })
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

/// PURE: whether this payload is a well-formed request of this kind, and its canonical form.
///
/// **Validated when it is WRITTEN and never when it is executed.** A `send_email` with no recipient
/// has to be refused to the agent, which is still mid-turn and can still fix it — not to a human
/// three hours later, who can fix nothing and whose only options are to approve something broken or
/// throw away work already paid for.
///
/// Returns the JSON to store, re-serialised from the fields this understands rather than passed
/// through. A payload carrying extra keys stores without them, so what a human reads when approving
/// is exactly what the executor will act on.
fn validate_payload(kind: &str, payload: &serde_json::Value) -> Result<String, String> {
    // Trimmed, for the fields where surrounding space is a typo: an address, a subject, a path, a
    // timezone name.
    let text = |field: &str| -> Result<String, String> {
        payload
            .get(field)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("a `{kind}` needs a non-empty `{field}`"))
    };
    // NOT trimmed, for the fields where it is the content: a document's trailing newline is part of
    // the document, and a body's leading blank line may be deliberate. Still required to have
    // something in it — an empty file and an empty message are both requests worth refusing — but
    // what is stored is what was written.
    let body = |field: &str| -> Result<String, String> {
        payload
            .get(field)
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("a `{kind}` needs a non-empty `{field}`"))
    };

    let canonical = match kind {
        "send_email" => {
            let (to, subject, body) = (text("to")?, text("subject")?, body("body")?);
            // The house's own rule, borrowed rather than restated: a header ends at the first line
            // break, so a `\n` in `to` or `subject` does not corrupt a message — it ends that header
            // and starts one nobody approved. Asking `mailsend` means a department and the owner's
            // own send button cannot come to disagree about what an address is.
            crate::mailsend::validate(&to, &subject).map_err(str::to_owned)?;
            serde_json::json!({ "to": to, "subject": subject, "body": body })
        }
        "file_document" => {
            let (path, content) = (text("path")?, body("content")?);
            // Shape only, here. Whether the path stays inside the files root is decided by
            // `files::resolve_within` at execution, against a root this function does not have —
            // and that is the check that matters, so this one only refuses the obviously wrong.
            if path.starts_with('/') || path.starts_with('\\') || path.contains("..") {
                return Err(
                    "a `file_document` path is relative and stays inside the folder".into(),
                );
            }
            serde_json::json!({ "path": path, "content": content })
        }
        "calendar_event" => {
            let title = text("title")?;
            let starts_at_local = text("starts_at_local")?;
            let tz = text("tz")?;
            let minutes = payload
                .get("duration_minutes")
                .and_then(serde_json::Value::as_i64)
                .filter(|minutes| *minutes > 0)
                .ok_or("a `calendar_event` needs a `duration_minutes` above zero")?;
            // Both parsed HERE and stored as strings, so an unparseable date is the agent's problem
            // and not a `failed` row a human approved in good faith.
            chrono::NaiveDateTime::parse_from_str(&starts_at_local, crate::calendar::LOCAL_FORMAT)
                .map_err(|_| {
                    format!(
                        "`starts_at_local` is written {}, e.g. 2026-08-17T09:30:00",
                        crate::calendar::LOCAL_FORMAT
                    )
                })?;
            tz.parse::<chrono_tz::Tz>()
                .map_err(|_| format!("`{tz}` is not a timezone name, e.g. Europe/Lisbon"))?;
            serde_json::json!({
                "title": title,
                "starts_at_local": starts_at_local,
                "duration_minutes": minutes,
                "tz": tz,
            })
        }
        // Unreachable in production: `propose_action` checks `GRANTABLE_ACTIONS` first. Kept total
        // rather than `unreachable!` so that adding a kind to the constant and forgetting this
        // function is a refusal, not a panic in a background loop.
        _ => return Err(format!("nobody can do `{kind}`")),
    };
    Ok(canonical.to_string())
}

/// A department asks the core to do something. **It does not happen here.**
///
/// This is the whole of the design's decision #2, and the reason `TEAM_TOOLS` stays read-only
/// forever: the agent declares an intention and gets a sentence back, the turn carries on, nothing
/// blocks and nothing is resumed. `proposals::create_calendar_event` had already written the shape
/// — *"The agent never writes the event itself. This row is the whole mechanism"* — and this is that
/// mechanism with a second table in front of it.
///
/// **Which run is asking comes from the key, not from the body**, exactly as `post_read_file` insists
/// one route over. A department is the one scope in the house that names its caller.
///
/// The order of the five refusals is not arbitrary. The constant is asked before the table, so a row
/// somebody put in `team_grants` by hand cannot grant an action the house does not have. The payload
/// is checked before the ceiling, so a malformed request is a fault the agent can fix rather than
/// one that eats a slot in the queue.
pub async fn propose_action(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &ProposeActionRequest,
) -> Result<ProposeActionResponse, ActionError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(ActionError::NotADepartment);
    };

    if !GRANTABLE_ACTIONS.contains(&request.kind.as_str()) {
        return Err(ActionError::Unknown(request.kind.clone()));
    }

    let team_id: String = sqlx::query_scalar("SELECT team_id FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ActionError::NoSuchRun)?;

    let mode: Option<String> =
        sqlx::query_scalar("SELECT mode FROM team_grants WHERE team_id = ? AND kind = ?")
            .bind(&team_id)
            .bind(&request.kind)
            .fetch_optional(&state.pool)
            .await?;
    // Default deny. The absence of a row is the refusal — there is no `mode = 'deny'` to disagree
    // with it.
    let mode = mode.ok_or_else(|| ActionError::Ungranted(request.kind.clone()))?;

    let payload =
        validate_payload(&request.kind, &request.payload).map_err(ActionError::Malformed)?;
    let why = request.why.trim();
    if why.is_empty() {
        return Err(ActionError::Malformed(
            "say why, in one line — it is what the person deciding will read".to_owned(),
        ));
    }

    // Only `propose` actions occupy the queue: an `allow` action is decided already and executes on
    // the next pass, so counting it would let a fast-clearing kind block a slow one.
    if mode == "propose" {
        let ceiling: i64 = sqlx::query_scalar("SELECT max_open_actions FROM teams WHERE id = ?")
            .bind(&team_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or(DEFAULT_MAX_OPEN_ACTIONS);
        let open = open_actions_of(&state.pool, &team_id).await?;
        if open >= ceiling {
            return Err(ActionError::QueueFull(format!(
                "this department already has {open} action(s) waiting for approval, and may hold \
                 {ceiling}; the person deciding has not got to them yet"
            )));
        }
    }

    // Attribution, not authority: every node of a run may ask, and this only records which did.
    // `None` for the director and for a node the run does not recognise — see `Caller`.
    //
    // **Read before the transaction opens, and that ordering is load-bearing.** A read taken while
    // a transaction is in flight is served by a SECOND pooled connection, and against a
    // `sqlite::memory:` database a second connection is a second, empty database — so this
    // silently resolved to `Unknown` and every action was recorded as nobody's. Every read this
    // function makes happens before `begin`; everything after it is writes.
    let ordinal = match calling_node(&state.pool, team_run_id, headers).await {
        Caller::Specialist(ordinal) => Some(ordinal),
        Caller::Director | Caller::Unknown => None,
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = state.pool.begin().await?;
    // The proposal FIRST, so its id can go on the action row. One transaction and not two writes:
    // an action with no proposal is one nobody will ever decide, and a proposal with no action is a
    // button that approves nothing. `ingest_director` learned this at cost in the pillar before —
    // saving only the last write leaves the earlier ones unsaved.
    let proposal_id = if mode == "propose" {
        Some(
            crate::proposals::create_team_action_in_transaction(
                &mut transaction,
                &request.kind,
                why,
                &payload,
                &now,
            )
            .await?,
        )
    } else {
        None
    };
    let id = sqlx::query(
        "INSERT INTO team_actions
           (team_run_id, ordinal, kind, payload, why, proposal_id, state, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?)",
    )
    .bind(team_run_id)
    .bind(ordinal)
    .bind(&request.kind)
    .bind(&payload)
    .bind(why)
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *transaction)
    .await?
    .last_insert_rowid();
    transaction.commit().await?;

    let outcome = match proposal_id {
        Some(proposal_id) => format!(
            "filed for approval as #{proposal_id}. It will happen if the person says yes; carry on \
             without waiting."
        ),
        None => "queued — this department may do that without asking, and the core will do it \
                 shortly."
            .to_owned(),
    };
    Ok(ProposeActionResponse {
        id,
        proposal_id,
        outcome,
    })
}

// ---------------------------------------------------------------------------------------------
// Recruitment: a director says who it needed and did not have
// ---------------------------------------------------------------------------------------------

/// Why a director could not ask for somebody. Every variant is a sentence for the MODEL, mid-round.
#[derive(Debug)]
pub enum RecruitError {
    NotADepartment,
    /// The caller is a specialist, or a node this run no longer recognises.
    NotTheDirector,
    NoSuchRun,
    /// That id is already in the catalogue — actionable in the same turn: ask for them to be added
    /// rather than for somebody new.
    AlreadyExists(String),
    /// Already asked for, and nobody has answered yet.
    AlreadyAsked(String),
    /// `agent::validate` refused the proposal, in its own words.
    Invalid(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for RecruitError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for RecruitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADepartment => formatter.write_str("only a department may ask for a teammate"),
            Self::NotTheDirector => formatter.write_str(
                "only a director may ask for a teammate — say in your answer what you needed and \
                 did not have, and the director will pass it on",
            ),
            Self::NoSuchRun => formatter.write_str("no such team run"),
            Self::AlreadyExists(id) => write!(
                formatter,
                "`{id}` is already in the catalogue — ask the owner to add them to this team \
                 rather than hiring somebody new"
            ),
            Self::AlreadyAsked(id) => write!(
                formatter,
                "you already asked for `{id}`; it is waiting for the owner. Carry on without them"
            ),
            Self::Invalid(why) => formatter.write_str(why),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct RecruitRequest {
    pub name: String,
    pub speciality: String,
    pub prompt: String,
    /// Absent means the director's own — the only defensible guess. A director running on
    /// `claude-sonnet-5` whose new specialist runs on the same surprises nobody, where a director
    /// free to choose puts the most expensive model on everybody.
    pub engine: Option<String>,
    pub model: Option<String>,
    /// Absent means `mcp_only`, which is what every team agent already has.
    pub tool_policy: Option<String>,
    pub why: String,
    /// `suggest_model`: answer the model adviser's suggestion for this candidate and file NOTHING.
    /// Carried on this route rather than on one of its own so the team key reaches no new route —
    /// the question is the director's alone either way, and it is answered by the same check.
    #[serde(default)]
    pub suggest_only: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct RecruitResponse {
    pub proposal_id: i64,
    pub outcome: String,
}

/// A director asks which model a candidate should run on. **Nothing is filed and nothing is
/// hired**: the answer is the model adviser's suggestion or a plain "no suggestion".
///
/// The same two checks as `propose_teammate`, first and in the same order — a specialist is told
/// the same sentence it is told there.
pub async fn suggest_model(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &RecruitRequest,
) -> Result<serde_json::Value, RecruitError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(RecruitError::NotADepartment);
    };
    if calling_node(&state.pool, team_run_id, headers).await != Caller::Director {
        return Err(RecruitError::NotTheDirector);
    }
    Ok(crate::seat_advice::suggest_model(
        state.runner.router(),
        &request.speciality,
        &request.why,
        &request.prompt,
    )
    .await)
}

/// A director asks for somebody it does not have. **Nothing is hired here.**
///
/// The same shape as `propose_action` and for the same reason. What differs is where the new person
/// lands: **not in the run that asked.** `team::start` validates the whole roster before writing the
/// run's row, and somebody appearing mid-run breaks all three things it settled — every item's agent
/// exists, no member is `local` on a machine that cannot serve one, and the ceiling was computed
/// over that group. The fourth reason is the deciding one: a recruitment takes a person's time, and
/// a run that waited for one would sit `working` for days holding a `max_parallel` slot.
pub async fn propose_teammate(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &RecruitRequest,
) -> Result<RecruitResponse, RecruitError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(RecruitError::NotADepartment);
    };
    // FIRST, before anything else is read or written. It is the one check here whose failure would
    // be a governance problem rather than an inconvenience.
    if calling_node(&state.pool, team_run_id, headers).await != Caller::Director {
        return Err(RecruitError::NotTheDirector);
    }

    let team_id: String = sqlx::query_scalar("SELECT team_id FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(RecruitError::NoSuchRun)?;

    let id = slug(&request.name);
    if id.is_empty() {
        return Err(RecruitError::Invalid(
            "a name must contain a letter or a digit".to_owned(),
        ));
    }
    if crate::agent::get(&state.pool, &id)
        .await
        .map_err(|error| RecruitError::Invalid(error.to_string()))?
        .is_some()
    {
        return Err(RecruitError::AlreadyExists(id));
    }
    // The first of the two defences against asking every round. The second is in
    // `director_prompt`, and both are needed: without this the director files three lawyers, and
    // without that one it spends a turn per round discovering it already did.
    if crate::proposals::recruit_pending_for(&state.pool, &id).await? {
        return Err(RecruitError::AlreadyAsked(id));
    }

    // The director's own engine and model, when the proposal named none. Read from the catalogue
    // rather than from the run row, because `runs` has no column for a model.
    let director_agent_id: Option<String> =
        sqlx::query_scalar("SELECT director_agent_id FROM teams WHERE id = ?")
            .bind(&team_id)
            .fetch_optional(&state.pool)
            .await?;
    let director = match director_agent_id {
        Some(agent_id) => crate::agent::get(&state.pool, &agent_id)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let engine = request
        .engine
        .clone()
        .or_else(|| director.as_ref().map(|agent| agent.engine.clone()))
        .unwrap_or_else(|| "claude".to_owned());
    let mut proposed = crate::agent::AgentRequest {
        name: request.name.trim().to_owned(),
        speciality: request.speciality.trim().to_owned(),
        prompt: request.prompt.clone(),
        engine,
        model: request
            .model
            .clone()
            .or_else(|| director.as_ref().and_then(|agent| agent.model.clone())),
        tool_policy: request
            .tool_policy
            .clone()
            .unwrap_or_else(|| "mcp_only".to_owned()),
    };
    // Validated here AND at approval, and the two are not redundant. Here it is a sentence the
    // director can act on while it still holds the turn; there it is the last word, over whatever
    // the owner edited. A director asking for `unrestricted` learns so now rather than filing a
    // request that could only ever have been refused.
    crate::agent::validate_request(&proposed)
        .map_err(|why| RecruitError::Invalid(why.to_owned()))?;

    let why = request.why.trim();
    if why.is_empty() {
        return Err(RecruitError::Invalid(
            "say what you needed them for — it is the whole of what the owner will read".to_owned(),
        ));
    }

    // Under `recruit: apply`, the model adviser's pick for this candidate before the director's
    // own; any failure there is the director's own, exactly as before. Asked only once the
    // request is otherwise fileable, so a refused one sends its text nowhere; and an advised model
    // that `validate_request` would refuse is dropped for the director's.
    if request.model.is_none()
        && let Some(advised) = crate::seat_advice::recruit_default(
            state.runner.router(),
            &proposed.engine,
            &request.speciality,
            &request.why,
            &request.prompt,
        )
        .await
    {
        let director_model = proposed.model.replace(advised);
        if let Err(why) = crate::agent::validate_request(&proposed) {
            tracing::warn!(%why, "the adviser's model for a recruit was refused; keeping the director's");
            proposed.model = director_model;
        }
    }

    let payload = serde_json::json!({
        "name": proposed.name,
        "speciality": proposed.speciality,
        "prompt": proposed.prompt,
        "engine": proposed.engine,
        "model": proposed.model,
        "tool_policy": proposed.tool_policy,
    });
    let proposal_id = crate::proposals::create_agent_recruit(
        &state.pool,
        &team_id,
        team_run_id,
        &id,
        &payload,
        why,
    )
    .await?;

    Ok(RecruitResponse {
        proposal_id,
        outcome: format!(
            "filed as #{proposal_id}; the owner decides. They will not join this run — carry on \
             with who you have, and say in the delivery what was missing."
        ),
    })
}

/// Why a recruitment could not be completed once somebody said yes.
#[derive(Debug)]
pub enum HireError {
    NotFound,
    NotPending,
    /// The stored request is not one this daemon can read back.
    Malformed,
    /// The team was deleted while the request waited.
    NoSuchTeam(String),
    /// `agent::validate` refused what was approved, or the name now collides.
    Refused(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for HireError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// Hires the person a director asked for, over whatever the owner edited.
///
/// **What was approved is the authority, not what was proposed.** A director knows three of the six
/// fields well — it has just found the gap — and knows `engine`, `model` and `tool_policy` badly,
/// because those are what cost money per turn and what widen a surface. So the proposal is a
/// suggestion, the approval may carry corrections, and `agent::validate` runs over the corrections.
///
/// One transaction over three writes. An agent created without joining the roster is somebody
/// nobody asked for; a roster row pointing at an agent that was never written breaks the foreign key
/// at the NEXT run's `start`, which is the worst place to find out.
pub async fn approve_recruit(
    pool: &sqlx::SqlitePool,
    proposal_id: i64,
    edited: Option<crate::agent::AgentRequest>,
) -> Result<String, HireError> {
    let proposal = crate::proposals::get(pool, proposal_id)
        .await?
        .ok_or(HireError::NotFound)?;
    if proposal.kind != "agent-recruit" || proposal.status != "pending" {
        return Err(HireError::NotPending);
    }
    let payload: serde_json::Value = proposal
        .tool_input
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or(HireError::Malformed)?;
    let team_id = payload
        .get("team_id")
        .and_then(|value| value.as_str())
        .ok_or(HireError::Malformed)?
        .to_owned();

    let request = match edited {
        Some(edited) => edited,
        None => serde_json::from_value(payload.clone()).map_err(|_| HireError::Malformed)?,
    };
    crate::agent::validate_request(&request).map_err(|why| HireError::Refused(why.to_owned()))?;

    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = ?)")
        .bind(&team_id)
        .fetch_one(pool)
        .await?;
    if !exists {
        return Err(HireError::NoSuchTeam(team_id));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let id = slug(&request.name);
    if id.is_empty() {
        return Err(HireError::Refused(
            "a name must contain a letter or a digit".to_owned(),
        ));
    }
    let mut transaction = pool.begin().await?;
    // The decision first, and guarded: an approval that lost a race writes nothing here and the
    // rest of the transaction never runs, so nobody is hired twice.
    if !crate::proposals::transition_in_transaction(
        &mut transaction,
        proposal_id,
        "approved",
        "hired by user",
        &now,
    )
    .await?
    {
        return Err(HireError::NotPending);
    }
    let written = sqlx::query(
        "INSERT INTO agents (id, name, speciality, prompt, engine, model, tool_policy,
                             created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&request.name)
    .bind(&request.speciality)
    .bind(&request.prompt)
    .bind(&request.engine)
    .bind(&request.model)
    .bind(&request.tool_policy)
    .bind(&now)
    .bind(&now)
    .execute(&mut *transaction)
    .await;
    if written.is_err() {
        // A name that collided while the request waited. The whole transaction is dropped, so the
        // proposal is still `pending` and the owner edits the name and tries again — which is
        // trivial precisely because the approval puts the six fields in front of them.
        return Err(HireError::Refused(
            "an agent with that name or id already exists — edit the name and hire again"
                .to_owned(),
        ));
    }
    sqlx::query("INSERT OR IGNORE INTO team_members (team_id, agent_id) VALUES (?, ?)")
        .bind(&team_id)
        .bind(&id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(id)
}

/// How many of this team's actions are still waiting for somebody to decide.
///
/// Counted across the team's RUNS and not within one, because the ceiling is the owner's attention
/// and the owner has one queue. It clears the moment somebody decides — the same self-limiting
/// property `wip.rs` has, in the axis `wip.rs` cannot reach: that one counts
/// `proposals WHERE project_id = ?`, and a department has no project.
async fn open_actions_of(pool: &sqlx::SqlitePool, team_id: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM team_actions
           JOIN team_runs ON team_runs.id = team_actions.team_run_id
           JOIN proposals ON proposals.id = team_actions.proposal_id
          WHERE team_runs.team_id = ?
            AND team_actions.state = 'pending'
            AND proposals.status = 'pending'",
    )
    .bind(team_id)
    .fetch_one(pool)
    .await
}

/// Everything decided and not yet done, oldest first.
///
/// `proposal_id IS NULL` is an `allow` action, which was decided when it was written. Otherwise the
/// proposal has to read `approved`: `rejected` is handled by `refuse_action`, and `pending` means
/// nobody has answered.
const DUE_ACTIONS_SQL: &str =
    "SELECT id, team_run_id, ordinal, kind, payload, why, proposal_id, state,
                                      error, created_at, executed_at
                                 FROM team_actions
                                WHERE state = 'pending'
                                  AND (proposal_id IS NULL
                                       OR proposal_id IN (SELECT id FROM proposals
                                                           WHERE status = 'approved'))
                                ORDER BY id";

/// Does what the department asked and a human agreed to, one action at a time.
///
/// **In the tick and not in the approve handler**, for the two reasons this house always gives: an
/// HTTP handler that sends an email holds the connection open while a slow SMTP thinks, and a daemon
/// restarted in the middle of that loses the action with no trace. A `pending` row survives a
/// restart and is reconcilable by construction; an `await` inside a handler is neither. It is the
/// same decision that makes a team run driven by the database rather than by a `JoinHandle`.
///
/// Claimed by compare-and-swap BEFORE the work, so two passes overlapping cannot send one email
/// twice. `ingest_director` uses the identical pattern for the identical reason.
pub async fn execute_due_actions(state: &AppState) {
    let due: Vec<TeamAction> = match sqlx::query_as(DUE_ACTIONS_SQL).fetch_all(&state.pool).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "could not read the team actions waiting to be done");
            return;
        }
    };

    for action in due {
        // The claim. A row moved to `working` by another pass is skipped here, and the pass that
        // moved it owns it — an email that may already have gone must not be sent again.
        let claimed = sqlx::query(
            "UPDATE team_actions SET state = 'working' WHERE id = ? AND state = 'pending'",
        )
        .bind(action.id)
        .execute(&state.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .unwrap_or(false);
        if !claimed {
            continue;
        }

        let outcome = perform(state, &action).await;
        let (final_state, error) = match &outcome {
            Ok(()) => ("done", None),
            Err(why) => ("failed", Some(why.clone())),
        };
        if let Err(error) = sqlx::query(
            "UPDATE team_actions SET state = ?, error = ?, executed_at = ? WHERE id = ?",
        )
        .bind(final_state)
        .bind(error.as_deref())
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(action.id)
        .execute(&state.pool)
        .await
        {
            // The action HAPPENED and the record says `working`. Loud, because the next pass will
            // not retry it — `working` matches no claim — and a human reading the row needs to know
            // why it is stuck there.
            tracing::error!(
                action = action.id,
                %error,
                "a team action was performed and its outcome could not be recorded"
            );
            continue;
        }

        // The feed is where a person reads what the daemon did while nobody was looking, and both
        // endings belong in it: a `done` nobody sees is an email that went out unannounced.
        let _ = crate::feed::append(
            &state.pool,
            None,
            "team_action",
            &match &outcome {
                Ok(()) => format!("a department's `{}` was carried out", action.kind),
                Err(why) => format!("a department's `{}` failed: {why}", action.kind),
            },
            None,
            Some(&crate::feed::Subject::TeamRun(action.team_run_id.clone())),
        )
        .await;
    }
}

/// The four lines that actually touch the world. Everything above decides whether to reach here.
///
/// Each arm delegates: `team.rs` knows what was asked and who may ask it, and knows nothing about
/// how a message is submitted or where a folder lives. The `Err` string is what a person reads
/// beside a `failed` row the morning after, so it says what did not happen rather than which
/// function returned what.
async fn perform(state: &AppState, action: &TeamAction) -> Result<(), String> {
    let payload: serde_json::Value = serde_json::from_str(&action.payload)
        .map_err(|error| format!("the stored request could not be read back: {error}"))?;
    let text = |field: &str| -> String {
        payload
            .get(field)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    };

    match action.kind.as_str() {
        "send_email" => crate::mailsend::send(
            state,
            &crate::mailsend::SendRequest {
                to: text("to"),
                subject: text("subject"),
                body: text("body"),
            },
        )
        .await
        .map_err(|failure| failure.to_string()),

        "file_document" => {
            // The one action that leaves the sandbox the pillar built: it writes into the owner's
            // files root rather than into the run's own folder, which is the whole point — a
            // delivery nobody opens has not been delivered. Contained by `files::resolve_within`
            // against that root, the same check the Files tab makes, and by nothing else.
            let root = files_root(state)?;
            let target = crate::files::resolve_within(&root, &text("path"))
                .map_err(|_| "that path is not inside the files folder".to_owned())?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("the folder could not be made: {error}"))?;
            }
            crate::storage::write_atomic(&target, text("content").as_bytes())
                .map_err(|error| format!("the file could not be written: {error}"))
        }

        "calendar_event" => {
            // Parsed again rather than trusted, though `validate_payload` already parsed it: the row
            // has been sitting in a table since, and this is the last moment before it becomes an
            // event somebody's week is arranged around.
            let starts_at_local = chrono::NaiveDateTime::parse_from_str(
                &text("starts_at_local"),
                crate::calendar::LOCAL_FORMAT,
            )
            .map_err(|_| "the stored start time is not a date and time".to_owned())?;
            let tz: chrono_tz::Tz = text("tz")
                .parse()
                .map_err(|_| "the stored timezone is not one".to_owned())?;
            let minutes = payload
                .get("duration_minutes")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            crate::calendar::insert_event(
                &state.pool,
                &text("title"),
                starts_at_local,
                minutes,
                tz,
                // The source column says WHO put it there, which is what a person scanning their
                // week wants to know about an entry they do not remember making.
                "team",
                None,
                None,
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("the event could not be written: {error}"))
        }

        other => Err(format!("nobody can do `{other}`")),
    }
}

/// A rejected proposal closes its action.
///
/// `failed` with `error = 'rejected'` and not a `rejected` state of its own. A third state would say
/// the same thing `proposals.status` already says, in a second place, and the two would eventually
/// disagree — while `state` here answers only one question: is there anything left to do about this
/// row? There is not.
pub async fn refuse_action(pool: &sqlx::SqlitePool, proposal_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE team_actions SET state = 'failed', error = 'rejected', executed_at = ?
          WHERE proposal_id = ? AND state = 'pending'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(proposal_id)
    .execute(pool)
    .await
    .map(|_| ())
}

// ---------------------------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------------------------

/// Advances every live run as far as it will go, once.
///
/// Runs are advanced independently and a failure in one is logged rather than propagated: a
/// department that cannot be moved must not stop the sweep that moves the others.
pub async fn team_tick(state: &AppState, now: chrono::DateTime<chrono::Utc>) {
    // Before the runs, and independently of them. An action outlives the run that asked for it —
    // people decide overnight, and by morning that department has usually finished — so this loop
    // reads `team_actions` rather than walking the live runs. It is also why a pending action never
    // holds a run open: nothing here is waiting for anything there.
    execute_due_actions(state).await;

    let live: Vec<TeamRun> = match sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs
         WHERE state IN ('planning', 'working', 'delivering')
         ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "could not read the live team runs");
            return;
        }
    };

    for run in live {
        let id = run.id.clone();
        if let Err(error) = advance(state, run, now).await {
            tracing::warn!(team_run = %id, %error, "a team run could not be advanced this pass");
        }
    }
}

/// One run, one pass.
async fn advance(
    state: &AppState,
    run: TeamRun,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    // Whatever is in flight is ingested first, so a ceiling reached while a node was running does
    // not throw away a node that has already been paid for. It is the reasoning `job::reconcile_nodes`
    // states: refusing to write down a finished node because the budget ran out loses the node and
    // repeats it.
    if run.director_node != "none" {
        // One step per pass, whichever way that went: a node still in flight means nothing else can
        // happen, and a node just ingested means the row this pass is holding is already stale.
        // What comes next is decided from the row the next tick reads, ten seconds later, which is
        // the cost of never acting on a snapshot that has moved underneath.
        ingest_director(state, run).await?;
        return Ok(());
    }
    ingest_landed_items(state, &run).await?;

    // Ceilings, at the two points the design names: before a round and before delivery. Both are
    // reached here, because `director_node == none` is exactly "between nodes".
    if let Some((ending, why)) = ceiling_reached(state, &run, now).await {
        return finish(state, &run, ending, &why).await;
    }

    match run.state.as_str() {
        "planning" => launch_director(state, &run, DirectorNode::Planning).await,
        "delivering" => launch_director(state, &run, DirectorNode::Delivering).await,
        "working" => advance_round(state, &run).await,
        _ => Ok(()),
    }
}

/// Which ceiling, if any, this run has hit.
///
/// Time before money, because an expired run tells the owner something a stopped one does not: the
/// department is not slow, it has been going for four hours.
async fn ceiling_reached(
    state: &AppState,
    run: &TeamRun,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(&'static str, String)> {
    if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&run.created_at)
        && now.signed_duration_since(created.with_timezone(&chrono::Utc)) > MAX_TEAM_RUN_LIFETIME
    {
        return Some((
            "expired",
            format!(
                "the run passed its ceiling of {} hours",
                MAX_TEAM_RUN_LIFETIME.num_hours()
            ),
        ));
    }

    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::budget::budget_permits_new_run(&state.pool, now).await
    {
        return Some(("stopped", reason));
    }

    // `Option<f64>` spelled out, and the two `flatten`s are not noise: the outer one is "is there a
    // team row", the inner is "did that row name a ceiling". Decoding straight into `f64` read a
    // NULL as 0.00, so a team with NO ceiling was stopped before its first round with the message
    // "the team's ceiling of $0.00 is spent" — the whole pillar dead on arrival for every team the
    // owner did not put a number on, which is the default.
    let ceiling: Option<f64> =
        sqlx::query_scalar::<_, Option<f64>>("SELECT budget_usd FROM teams WHERE id = ?")
            .bind(&run.team_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .flatten();
    if let Some(ceiling) = ceiling {
        let spent = spend_of(&state.pool, &run.id).await;
        if spent >= ceiling {
            return Some((
                "stopped",
                format!("the team's ceiling of ${ceiling:.2} is spent (${spent:.2})"),
            ));
        }
    }

    // And the TREE's, which is a different question once a run can start another. Two teams with a
    // $5 ceiling that start each other spend $5 every time, for ever, and every run is inside its
    // own ceiling the whole time — so the check above never fires and nothing is locally wrong.
    //
    // Both checks stand, and neither weakens the other: a $1 department does not get to spend $4
    // because the root allowed it, and a chain does not get to spend without end because each link
    // is cheap. For a run a person asked for the two are the same question, since it is its own
    // root and its own tree.
    if run.root_id != run.id {
        let root_ceiling: Option<f64> = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT teams.budget_usd FROM team_runs
               JOIN teams ON teams.id = team_runs.team_id
              WHERE team_runs.id = ?",
        )
        .bind(&run.root_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .flatten();
        if let Some(root_ceiling) = root_ceiling {
            let spent = spend_of_tree(&state.pool, &run.root_id).await;
            if spent >= root_ceiling {
                return Some((
                    "stopped",
                    format!(
                        "this chain has spent ${spent:.2} of the ${root_ceiling:.2} its first run \
                         was given"
                    ),
                ));
            }
        }
    }
    None
}

/// What a whole chain has cost, every run in it and every director node of each.
///
/// Keyed on `root_id` with an index, not a recursive walk of `parent_id`: this is asked at every
/// pass of every run in the tree, one indexed read beats a CTE that arrives at the same answer by a
/// route that can be got wrong. It rests on `runs.team_run_id` (migration 0084) — without that
/// column a run's cost counts the specialists alone, and a tree ceiling built on a sum that
/// undercounts errs towards spending, multiplied by the depth.
pub async fn spend_of_tree(pool: &sqlx::SqlitePool, root_id: &str) -> f64 {
    sqlx::query_scalar::<_, Option<f64>>(
        "SELECT SUM(cost_usd) FROM runs
          WHERE team_run_id IN (SELECT id FROM team_runs WHERE root_id = ?)",
    )
    .bind(root_id)
    .fetch_one(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0)
}

/// What one run has cost, director nodes included.
///
/// Reads `runs.team_run_id` and not `team_items.run_id`, and that is why the column exists: the
/// director's plan, replans and delivery are runs of this department that no item points at.
pub async fn spend_of(pool: &sqlx::SqlitePool, team_run_id: &str) -> f64 {
    sqlx::query_scalar::<_, Option<f64>>("SELECT SUM(cost_usd) FROM runs WHERE team_run_id = ?")
        .bind(team_run_id)
        .fetch_one(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0.0)
}

/// How many specialists a round may have running at once: the team's value, widened by this
/// run's speed, never past `MAX_PARALLEL_CEILING` (speed spec, section 5.1). The column is
/// validated on the way in, but a value written around that validation is exactly what a
/// ceiling exists for, so it is applied here, at the one place that opens the fan.
async fn round_width(state: &AppState, run: &TeamRun) -> Result<i64, sqlx::Error> {
    Ok(run_capacity(state, run).await?.parallel_ceiling)
}

/// This run's speed and the width it opens, as one value: `round_width` enforces the width, and
/// `spawn_agent` hands both to the session it launches. One reader, so the number a session is
/// told and the number the fan is held to cannot come from two different queries.
async fn run_capacity(
    state: &AppState,
    run: &TeamRun,
) -> Result<crate::speed::Capacity, sqlx::Error> {
    let (stored, speed): (i64, Option<String>) = sqlx::query_as(
        "SELECT t.max_parallel, r.speed FROM teams t JOIN team_runs r ON r.team_id = t.id
         WHERE r.id = ?",
    )
    .bind(&run.id)
    .fetch_one(&state.pool)
    .await?;
    let speed = crate::speed::Speed::from_column(speed.as_deref());
    let (_, degraded) = crate::speed::team_width(speed, stored);
    if let Some(reason) = degraded {
        tracing::info!(team_run = %run.id, reason, "the speed asked for more width than this team allows");
    }
    Ok(crate::speed::Capacity::team(speed, stored))
}

/// A round: start what is pending, up to the team's parallelism, and replan when it is empty.
async fn advance_round(state: &AppState, run: &TeamRun) -> Result<(), sqlx::Error> {
    let items: Vec<TeamItem> = sqlx::query_as(
        "SELECT ordinal, round, agent_id, description, state, run_id, output_path
         FROM team_items WHERE team_run_id = ? AND round = ? ORDER BY ordinal",
    )
    .bind(&run.id)
    .bind(run.round)
    .fetch_all(&state.pool)
    .await?;

    let running = items.iter().filter(|item| item.state == "running").count() as i64;
    let pending: Vec<&TeamItem> = items
        .iter()
        .filter(|item| item.state == "pending")
        .collect();

    if pending.is_empty() && running == 0 {
        // The round is over. Replanning is a director node like any other, and `director_node`
        // is what stops the next tick launching a second one.
        return launch_director(state, run, DirectorNode::Replanning).await;
    }

    let parallel = round_width(state, run).await?;

    for item in pending
        .into_iter()
        .take((parallel - running).max(0) as usize)
    {
        if let Err(error) = launch_specialist(state, run, item).await {
            tracing::warn!(
                team_run = %run.id,
                ordinal = item.ordinal,
                %error,
                "a specialist could not be launched; the item is marked failed and the run goes on"
            );
            sqlx::query(
                "UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?",
            )
            .bind(&run.id)
            .bind(item.ordinal)
            .execute(&state.pool)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectorNode {
    Planning,
    Replanning,
    Delivering,
}

impl DirectorNode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Replanning => "replanning",
            Self::Delivering => "delivering",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Launching
// ---------------------------------------------------------------------------------------------

/// The row a launch writes before anything is spawned, so the run exists for the hook to resolve a
/// mode from and for reconciliation to find if the daemon dies here.
///
/// **`read_untrusted` is set in THIS statement and never in a second one**, for the reason 0122
/// gives about `runs.from_relay_id`: a run born holding a colleague's words cannot be allowed to
/// exist without saying so. The gap between an INSERT and a follow-up UPDATE is a window in which a
/// node carrying a stranger's text, quoted into its own prompt, reads as a turn that has read
/// nothing — and every refusal in `hooks::team_decision` depends on that state not existing.
///
/// This is the receiving half of the trade `team_notes.rs` documents. Writing a note is `WritesOwn`,
/// so a specialist that read the web can still tell a colleague what it found; the taint travels
/// with the words and lands here. A node that receives a note may read on and may no longer ask —
/// which costs a department its alçada for that node, and buys the conversation.
async fn open_run(
    state: &AppState,
    team_run_id: &str,
    prompt: &str,
    read_untrusted: bool,
) -> Result<(i64, String), sqlx::Error> {
    let session_id = crate::auth::generate_uuid_v4();
    let id = sqlx::query(
        "INSERT INTO runs (prompt, status, mode, session_id, team_run_id, read_untrusted,
                           created_at)
         VALUES (?, 'running', ?, ?, ?, ?, ?)",
    )
    .bind(prompt)
    .bind(TEAM_MODE)
    .bind(&session_id)
    .bind(team_run_id)
    .bind(i64::from(read_untrusted))
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await?
    .last_insert_rowid();
    Ok((id, session_id))
}

/// The MCP config one team agent's invocation is launched with, and the file's lifetime.
///
/// One per NODE rather than one per team run: the server it starts is the Team box and it is named
/// by the node's own run (`--box team --run <id>`), because the box decides what to serve from that
/// run's `run_loadout` row. Every node therefore has a config no other node shares, and the guard
/// removes it when the node's task ends — a daemon that runs for months would otherwise leave one
/// in the temp directory for every invocation of every department it ever ran.
///
/// **The process id is in the name, and it is not decoration.** The temp directory is shared by
/// every process on the machine, so a name built only from the id is the SAME path in two of them
/// — and both write it and both delete it. On this machine that is not hypothetical: the daemon
/// runs while suites run, and several checkouts run suites at once, each with tests that use fixed
/// ids. One deleting the other's config mid-turn is a failure with no cause visible anywhere near
/// it. `transcribe.rs` already names its recordings this way, for the same reason.
struct TeamNodeMcp {
    path: std::path::PathBuf,
}

impl TeamNodeMcp {
    fn write(team_run_id: &str, node_run_id: i64) -> std::io::Result<Self> {
        let safe: String = team_run_id
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect();
        let path = std::env::temp_dir().join(format!(
            "nucleos-team-{}-{safe}-{node_run_id}.json",
            std::process::id()
        ));
        let exe = std::env::current_exe()?.to_string_lossy().into_owned();
        // What narrows a department's surface is `--allowedTools`, taken from the node's loadout.
        let body = serde_json::to_vec(&crate::assistant::build_team_mcp_config(&exe, node_run_id))
            .map_err(std::io::Error::other)?;
        crate::storage::write_atomic(&path, &body)?;
        Ok(Self { path })
    }
}

impl Drop for TeamNodeMcp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The key this run's agents authenticate with: `team:<id>.<secret>`, rebuilt from the stored half.
async fn team_token(pool: &sqlx::SqlitePool, team_run_id: &str) -> Result<String, sqlx::Error> {
    let secret: String = sqlx::query_scalar("SELECT token FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_one(pool)
        .await?;
    Ok(format!("team:{team_run_id}.{secret}"))
}

async fn launch_director(
    state: &AppState,
    run: &TeamRun,
    node: DirectorNode,
) -> Result<(), sqlx::Error> {
    let Some((team, director)) = team_and_director(state, run).await? else {
        return finish(
            state,
            run,
            "failed",
            "this team's director is no longer in the catalogue",
        )
        .await;
    };
    let mut prompt = match node {
        DirectorNode::Delivering => delivery_prompt(state, run, &team).await,
        _ => director_prompt(state, run, &team).await,
    };

    // The director is addressable like anybody else — `teams.director_agent_id` is an `agents.id`,
    // and it is on the roster every specialist is shown. That is deliberate and is the highest-value
    // message this feature will ever carry: "I found X, it is worth replanning" reaches the node that
    // decides what the next round does, instead of waiting for a folder somebody has to read.
    let waiting = crate::team_notes::pending(&state.pool, &run.id, &director.id)
        .await
        .unwrap_or_default();
    if let Some(block) = crate::team_notes::render(&waiting) {
        prompt.push_str(&block);
    }

    let (run_id, session_id) = open_run(state, &run.id, &prompt, !waiting.is_empty()).await?;

    // AFTER the run exists, which is `notes.rs`' hard-won rule and not a detail: a node that failed
    // to start never read the words, and a note consumed by it would be lost in silence — the one
    // failure the owner cannot see and cannot repeat.
    let ids: Vec<i64> = waiting.iter().map(|note| note.id).collect();
    if let Err(error) = crate::team_notes::mark_delivered(&state.pool, &ids, run_id).await {
        tracing::warn!(
            team_run = %run.id,
            run_id,
            %error,
            "the director was given its colleagues' notes and they were not marked delivered; they \
             will arrive again"
        );
    }

    // The marker is written BEFORE the spawn and cleared by the ingestion, which is what makes it a
    // marker and not a derived condition — the lesson migration 0051 bought, where a derived
    // `replanning` cost one wasted node per round.
    sqlx::query(
        "UPDATE team_runs SET director_node = ?, director_run_id = ?, updated_at = ? WHERE id = ?",
    )
    .bind(node.as_str())
    .bind(run_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .execute(&state.pool)
    .await?;

    spawn_agent(state, run, &director, run_id, session_id, prompt).await;
    Ok(())
}

async fn launch_specialist(
    state: &AppState,
    run: &TeamRun,
    item: &TeamItem,
) -> Result<(), sqlx::Error> {
    let Some(agent) = crate::agent::get(&state.pool, &item.agent_id)
        .await
        .ok()
        .flatten()
    else {
        // The agent was deleted between the plan and the launch. One item fails; the run goes on.
        sqlx::query("UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?")
            .bind(&run.id)
            .bind(item.ordinal)
            .execute(&state.pool)
            .await?;
        return Ok(());
    };

    let team = sqlx::query_scalar::<_, String>("SELECT mission FROM teams WHERE id = ?")
        .bind(&run.team_id)
        .fetch_one(&state.pool)
        .await?;
    let mut prompt = specialist_prompt(state, run, &team, item).await;

    // Read BEFORE the run is opened and stamped AFTER, and never both in one breath. A read taken
    // and claimed together looks identical from here right up until this launch fails — and a
    // colleague's words would then be marked delivered to a node that never existed.
    let waiting = crate::team_notes::pending(&state.pool, &run.id, &item.agent_id)
        .await
        .unwrap_or_default();
    if let Some(block) = crate::team_notes::render(&waiting) {
        prompt.push_str(&block);
    }

    let (run_id, session_id) = open_run(state, &run.id, &prompt, !waiting.is_empty()).await?;

    let ids: Vec<i64> = waiting.iter().map(|note| note.id).collect();
    if let Err(error) = crate::team_notes::mark_delivered(&state.pool, &ids, run_id).await {
        tracing::warn!(
            team_run = %run.id,
            run_id,
            %error,
            "a specialist was given its colleagues' notes and they were not marked delivered; they \
             will arrive again"
        );
    }

    sqlx::query(
        "UPDATE team_items SET state = 'running', run_id = ? WHERE team_run_id = ? AND ordinal = ?",
    )
    .bind(run_id)
    .bind(&run.id)
    .bind(item.ordinal)
    .execute(&state.pool)
    .await?;

    spawn_agent(state, run, &agent, run_id, session_id, prompt).await;
    Ok(())
}

/// Spawns one invocation and lets it write its own terminal `runs` row.
///
/// Nothing awaits the task: the tick discovers the answer by reading the row, which is what makes a
/// restart mid-flight cost nothing. `spawn_registered` is what makes the run cancellable.
async fn spawn_agent(
    state: &AppState,
    run: &TeamRun,
    agent: &crate::agent::Agent,
    run_id: i64,
    session_id: String,
    prompt: String,
) {
    let token = match team_token(&state.pool, &run.id).await {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(team_run = %run.id, %error, "a team run has no key; its agent cannot call a tool");
            return;
        }
    };
    let with_tools = agent.tool_policy == "mcp_only";
    let pool = state.pool.clone();

    if agent.engine == "local" {
        let Some(model) = agent.model.clone() else {
            fail_run(&pool, run_id, "a local agent with no model").await;
            return;
        };
        // The FACTORY's chat, where this built its own `runner::OllamaChat` against
        // `runner::OLLAMA_BASE_URL`. This was the last reader in the daemon still doing so: the
        // chat route and the council seat both ask `assistants::Assistants::local_chat`, which is
        // the one place that decides which server a local model gets. On an install whose
        // `local_engine` is `openai_compatible`, a team member left here reached an Ollama that may not be
        // running, may not hold the model, and may not be the machine that was paid for -- while
        // the owner's own chat reached the server they configured. Half a daemon on each engine.
        let chat = match state.assistants.local_chat(&model) {
            Ok(chat) => chat,
            Err(refusal) => {
                // The refusal's OWN sentence, for the reason `council.rs` gives at its seat:
                // `Refusal::message` is already what the chat route shows an operator for this
                // misconfiguration, and a second wording invented here would describe one problem
                // in two voices depending on which door somebody came through. The row is closed
                // rather than left `running` -- nothing was spawned, so nothing else ever closes it.
                fail_run(&pool, run_id, &refusal.message(crate::chats::Brain::Local)).await;
                return;
            }
        };
        // The TEAM's box, never `LocalToolBox::new`: `LOCAL_TOOLS` carries `create_run` and
        // `create_job`, and the local path never passes through `hooks.rs` at all.
        let tools =
            crate::mcp_tools::LocalToolBox::for_team(daemon_url(), token, pool.clone(), run_id);
        let timeout = state.run_timeout;
        crate::runs::spawn_registered(state, run_id, async move {
            let taint = std::sync::atomic::AtomicBool::new(false);
            let empty = crate::local_agent::NoTools;
            let tool_box: &dyn crate::local_agent::ToolBox =
                if with_tools { &tools } else { &empty };
            let turn = tokio::time::timeout(
                timeout,
                crate::local_agent::run_turn(
                    // `as_ref`, because what the factory hands back is a `Box<dyn LocalChat>`
                    // and the loop takes the trait object itself -- the box is the seam, not
                    // the value.
                    chat.as_ref(),
                    tool_box,
                    crate::local_agent::SYSTEM_PROMPT,
                    &[],
                    &prompt,
                    &taint,
                ),
            )
            .await;
            if taint.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = crate::runs::mark_untrusted_context(&pool, run_id).await;
            }
            let completed_at = chrono::Utc::now().to_rfc3339();
            match turn {
                Ok(Ok(turn)) => record_local_turn(&pool, run_id, &turn, &completed_at).await,
                Ok(Err(error)) => fail_run(&pool, run_id, &error.to_string()).await,
                Err(_) => {
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'timed_out', cost_usd = 0, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                }
            }
        });
        return;
    }

    // The node's own config, held by a guard that goes into the launch task below so the file lives
    // exactly as long as the node does.
    let mcp_guard = if with_tools {
        match TeamNodeMcp::write(&run.id, run_id) {
            Ok(guard) => Some(guard),
            Err(error) => {
                tracing::warn!(team_run = %run.id, run_id, %error, "could not write the team agent's MCP config");
                None
            }
        }
    } else {
        None
    };
    let mcp_config = mcp_guard.as_ref().map(|guard| guard.path.clone());

    // What the session is told it may use. An unreadable row tells it the narrowest thing, a team
    // of one at `normal` — the direction that cannot overstate what `round_width` will open.
    let capacity = run_capacity(state, run).await.unwrap_or_else(|error| {
        tracing::warn!(team_run = %run.id, %error, "could not read the run's speed; the session is told normal");
        crate::speed::Capacity::solo()
    });

    // Spec 2026-10-08 section 6: a department agent reads the machine's knowledge, its team's and
    // its own, and no project's. Append it to the prompt because state.runner may be Codex. Since
    // R3 a team agent leaves a trace of what it was shown (this supersedes D15 for departments),
    // so a later outcome can credit the rows that were in front of it.
    let loadout = crate::loadout::resolve(
        &state.pool,
        &crate::loadout::LoadoutInput {
            agent: Some(&agent.id),
            team: Some(&run.team_id),
            project: None,
            job: None,
            task_text: &prompt,
            node: None,
            files: &[],
            // The tools and the context come out of the same resolution as the memory. A member
            // with tools runs in the Team box; one without holds none, and "none" is recorded as
            // an empty set like any other loadout. There is no working directory, so a folder ref
            // is listed in the index but never added.
            equipment: Some(crate::loadout::Equipment {
                tool_policy: if with_tools { "mcp_only" } else { "none" },
                base: crate::mcp_tools::TEAM_BASE,
                extras: crate::mcp_tools::TEAM_EXTRAS,
                managed_root: state.files_root.as_deref(),
                has_cwd: false,
            }),
        },
    )
    .await;
    if let Some(brief) = loadout.brief.as_ref()
        && let Err(error) = crate::brief::record(&state.pool, run_id, None, &brief.trace).await
    {
        tracing::warn!(team_run = %run.id, run_id, %error, "could not record what the team agent was shown");
    }
    // The row is written before the CLI starts: the route and the hook decide an extra from it, and
    // a run that cannot prove it holds a tool does not hold it.
    let run_tools = loadout.run.clone().unwrap_or_default();
    if let Err(error) = crate::loadout::record(&state.pool, run_id, &run_tools).await {
        tracing::warn!(team_run = %run.id, run_id, %error, "could not record the team agent's loadout");
    }
    let prompt = format!("{prompt}{}", loadout.block);

    let request = crate::runner::RunRequest {
        prompt,
        // The RUN's own key, never `state.token`. `auth::TEAM_ROUTES` is what it reaches.
        env: crate::runs::run_env(&token, run_id, None, capacity),
        // No working directory, exactly as a council seat has none — which is also why the
        // `PreToolUse` hook may never fire and why the route table has to hold alone.
        cwd: None,
        permission: crate::runner::Permission::Default,
        resume_session_id: None,
        mcp_config,
        mcp_job: None,
        // A member with tools is offered the Team box (`--box team --run <node>`), which serves
        // only what its loadout row lists plus the box's base. `allowed_mcp_tools` below is that
        // same set handed to the CLI as `--allowedTools`.
        tool_policy: if with_tools {
            crate::runner::ToolPolicy::McpOnly
        } else {
            crate::runner::ToolPolicy::None
        },
        progress_timeout: None,
        max_turns: Some(crate::runner::DEFAULT_MAX_TURNS),
        session_id: Some(session_id),
        fork_session: false,
        include_partial_messages: false,
        // `steerable` with no `messages`, for the reason `council.rs` documents at length: it is the
        // only way to keep the prompt off the command line, and a director's prompt carries the
        // whole folder index. Windows caps a command line at 32 767 characters, and the first real
        // council died exactly there.
        images: Vec::new(),
        steerable: true,
        classifier_governs_tools: false,
        messages: None,
        ambient_mcp: false,
        model: agent.model.clone(),
        effort: None,
        fallback_model: Vec::new(),
        add_dirs: Vec::new(),
        max_budget_usd: None,
        agents: Vec::new(),
        append_system_prompt: None,
        denied_tools: Vec::new(),
        session_name: None,
        context_window: None,
        // The loadout's effective set: the box's base plus what the owner approved for this agent
        // or its team. Empty for a member without tools.
        allowed_mcp_tools: Some(run_tools.tools.clone()),
        background_tasks: false,
    };

    let runner = state.runner.clone();
    let timeout = state.run_timeout;
    crate::runs::spawn_registered(state, run_id, async move {
        // Moved in so the node's config file is removed when the task ends — normally, by timeout,
        // or aborted by a cancel (which drops the future and with it the guard).
        let _mcp_guard = mcp_guard;
        // Asked inside the task, so a slow adviser delays this seat and never the caller handing
        // out a round. `off` asks nothing and leaves the request as built above.
        // Its outcome is reported by `ingest_landed_items`, against the `route_decision_id` this
        // writes on the run's row, once the item lands.
        let mut request = request;
        let _decision =
            crate::seat_advice::route_team_seat(&pool, runner.router(), run_id, &mut request).await;
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        // How much window this agent is holding, mirrored into the row while it runs.
        //
        // This path is a launcher of its own -- it does not go through `runs::spawn_run` -- and
        // for a long time that meant a team item, the ONE kind of run the pressure reading is
        // about, recorded none of the four things it reads. `run_prompt_with_context_fill` and the
        // mirror are what `runs.rs` does, and the reason is the same: a cancel aborts this task
        // and writes only a status, so an in-memory number would be dropped with the run most
        // worth inspecting.
        let context_fill = std::sync::Arc::new(std::sync::Mutex::new(None));
        let _live_context_fill =
            crate::runs::mirror_context_fill(&pool, run_id, std::sync::Arc::clone(&context_fill));
        let outcome = tokio::time::timeout(
            timeout,
            runner.run_prompt_with_context_fill(
                request,
                session_tx,
                transcript.clone(),
                std::sync::Arc::clone(&context_fill),
            ),
        )
        .await;
        let completed_at = chrono::Utc::now().to_rfc3339();
        match outcome {
            Err(_) => {
                let partial = transcript.lock().unwrap().clone();
                // No outcome on this branch, so `compacted` cannot be known -- but the partial
                // transcript is the whole record of a run that went too far, which is the case the
                // reading exists to see.
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'timed_out', stdout = ?, context_fill = ?,
                            context_peak = ?, tools_used = ?, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                )
                .bind(&partial)
                .bind(crate::runs::observed_context_fill(&context_fill, &partial))
                .bind(crate::runs::peak_of(&partial))
                .bind(crate::runs::tools_of(&partial))
                .bind(&completed_at)
                .bind(run_id)
                .execute(&pool)
                .await;
            }
            Ok(Ok(outcome)) if outcome.exit_code == 0 => {
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?, cost_usd = ?,
                            input_tokens = ?, output_tokens = ?, cache_read_tokens = ?,
                            num_turns = ?, context_fill = ?, context_peak = ?, compacted = ?,
                            tools_used = ?, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                )
                .bind(&outcome.stdout)
                .bind(outcome.cost_usd)
                .bind(outcome.input_tokens)
                .bind(outcome.output_tokens)
                .bind(outcome.cache_read_tokens)
                .bind(outcome.num_turns)
                .bind(crate::runs::observed_context_fill(
                    &context_fill,
                    &outcome.stdout,
                ))
                .bind(crate::runs::peak_of(&outcome.stdout))
                .bind(outcome.compacted)
                .bind(crate::runs::tools_of(&outcome.stdout))
                .bind(&completed_at)
                .bind(run_id)
                .execute(&pool)
                .await;
            }
            Ok(Ok(outcome)) => {
                // An agent that started, spoke, and exited non-zero has a stream like any other,
                // and `num_turns` is written here too: the denominator was missing on every
                // non-`completed` path, which is exactly where the fullest runs are.
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'failed', exit_code = ?, stdout = ?, stderr = ?,
                            cost_usd = ?, num_turns = ?, context_fill = ?, context_peak = ?,
                            compacted = ?, tools_used = ?, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                )
                .bind(outcome.exit_code)
                .bind(&outcome.stdout)
                .bind(&outcome.stderr)
                .bind(outcome.cost_usd)
                .bind(outcome.num_turns)
                .bind(crate::runs::observed_context_fill(
                    &context_fill,
                    &outcome.stdout,
                ))
                .bind(crate::runs::peak_of(&outcome.stdout))
                .bind(outcome.compacted)
                .bind(crate::runs::tools_of(&outcome.stdout))
                .bind(&completed_at)
                .bind(run_id)
                .execute(&pool)
                .await;
            }
            Ok(Err(error)) => fail_run(&pool, run_id, &error.to_string()).await,
        }
    });
}

/// The terminal write of a local agent's turn.
///
/// `cost_usd = 0` and not NULL: NULL is what `budget.rs` time-approximates a cost for, so leaving
/// it unset would charge the window for electricity.
///
/// `num_turns` carries the turn's TOOL CALLS, and it is the one measurement this path can make.
/// There is no `stream-json` here — no usage lines, so no `context_fill` and no peak, and nothing
/// that could say whether the model summarised itself — but a count of calls is exactly the
/// denominator `pressure::steps_of` wants, and it is a finer one than a turn count: `num_turns` is
/// the fallback precisely because a turn with twelve tools and a turn with one count the same, and
/// this number does not have that problem.
///
/// `tools_used` stays NULL on purpose. `Turn` carries a count and not the names, and a synthetic
/// list of the right length would be read as a record of which tools ran. NULL means nobody asked;
/// an invented list would be an answer, and a false one.
///
/// Separate from the branch that calls it because what it does with a turn is the part that can
/// be wrong, independently of who answered. This used to say the surrounding task could not be
/// exercised at all without a live Ollama, and that was true for exactly as long as the local
/// path built its own `OllamaChat` against `OLLAMA_BASE_URL`: it asks the factory now, so a test
/// can hand it a chat pointed anywhere and `a_local_team_member_is_served_by_the_configured_engine`
/// does.
async fn record_local_turn(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    turn: &crate::local_agent::Turn,
    completed_at: &str,
) {
    let _ = sqlx::query(
        "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?, cost_usd = 0,
                num_turns = ?, completed_at = ?
         WHERE id = ? AND status = 'running'",
    )
    .bind(&turn.answer)
    .bind(turn.tool_calls as i64)
    .bind(completed_at)
    .bind(run_id)
    .execute(pool)
    .await;
}

async fn fail_run(pool: &sqlx::SqlitePool, run_id: i64, why: &str) {
    let _ = sqlx::query(
        "UPDATE runs SET status = 'failed', stderr = ?, cost_usd = 0, completed_at = ?
         WHERE id = ? AND status = 'running'",
    )
    .bind(why)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(pool)
    .await;
}

async fn team_and_director(
    state: &AppState,
    run: &TeamRun,
) -> Result<Option<(Team, crate::agent::Agent)>, sqlx::Error> {
    let team: Option<Team> = sqlx::query_as(
        "SELECT id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                max_open_actions, max_live_runs, created_at, updated_at
         FROM teams WHERE id = ?",
    )
    .bind(&run.team_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(team) = team else {
        return Ok(None);
    };
    let director = crate::agent::get(&state.pool, &team.director_agent_id)
        .await
        .ok()
        .flatten();
    Ok(director.map(|director| (team, director)))
}

// ---------------------------------------------------------------------------------------------
// Ingesting what landed
// ---------------------------------------------------------------------------------------------

/// Specialist runs that have finished since the last pass: file the answer, mark the item.
///
/// A failed item leaves no file and does not stop the run — there is no shared worktree here
/// accumulating damage, so what a failure costs is one absence the director sees in the next
/// round's index.
async fn ingest_landed_items(state: &AppState, run: &TeamRun) -> Result<(), sqlx::Error> {
    let landed: Vec<(i64, String, i64, String)> = sqlx::query_as(
        "SELECT i.ordinal, i.agent_id, r.id, r.status
         FROM team_items i JOIN runs r ON r.id = i.run_id
         WHERE i.team_run_id = ? AND i.state = 'running' AND r.status != 'running'",
    )
    .bind(&run.id)
    .fetch_all(&state.pool)
    .await?;

    for (ordinal, agent_id, run_id, status) in landed {
        if status != "completed" {
            sqlx::query(
                "UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?",
            )
            .bind(&run.id)
            .bind(ordinal)
            .execute(&state.pool)
            .await?;
            crate::seat_advice::report_team_item(&state.pool, state.runner.router(), run_id).await;
            continue;
        }

        let answer: Option<String> = sqlx::query_scalar("SELECT stdout FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
        let answer = crate::runner::extract_reply(answer.as_deref().unwrap_or_default())
            .unwrap_or_else(|| answer.unwrap_or_default());

        // The core names the file, which is what removes collisions by construction: the ordinal is
        // unique across the whole run, so two specialists — in one round or across rounds — can
        // never choose the same name, because neither of them chooses.
        let name = format!("{ordinal}-{agent_id}.md");
        match write_into_workspace(state, run, &name, &answer) {
            Ok(()) => {
                sqlx::query(
                    "UPDATE team_items SET state = 'done', output_path = ?
                     WHERE team_run_id = ? AND ordinal = ?",
                )
                .bind(&name)
                .bind(&run.id)
                .bind(ordinal)
                .execute(&state.pool)
                .await?;
            }
            Err(error) => {
                tracing::warn!(team_run = %run.id, ordinal, %error, "could not file a specialist's answer");
                sqlx::query(
                    "UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?",
                )
                .bind(&run.id)
                .bind(ordinal)
                .execute(&state.pool)
                .await?;
            }
        }
        // The seat is reported on how its RUN ended, after the item's own state is written — so a
        // pass that could not write it retries next tick and reports once, not twice. A completed
        // run whose answer could not be filed is still `pass`: the model did the work, the daemon
        // is what failed to keep it.
        crate::seat_advice::report_team_item(&state.pool, state.runner.router(), run_id).await;
    }
    Ok(())
}

fn write_into_workspace(
    state: &AppState,
    run: &TeamRun,
    name: &str,
    body: &str,
) -> std::io::Result<()> {
    let root = files_root(state).map_err(std::io::Error::other)?;
    let folder = crate::files::resolve_within(&root, &run.workspace)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    std::fs::create_dir_all(&folder)?;
    let target = crate::files::resolve_within(&folder, name)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    crate::storage::write_atomic(&target, body.as_bytes())
}

/// Ingests the director node in flight, if it has landed.
///
/// The marker is claimed by a compare-and-swap before anything is written, so ingesting the same
/// `director_run_id` twice cannot advance the round twice — and neither call site cares which of
/// the two happened, which is why this reports nothing.
async fn ingest_director(state: &AppState, run: TeamRun) -> Result<(), sqlx::Error> {
    let Some(director_run_id) = run.director_run_id else {
        // A marker with no run behind it: nothing can land, so clear it and let the next pass
        // relaunch rather than leave the run wedged forever.
        return clear_director_node(state, &run).await;
    };

    let landed: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT status, stdout FROM runs WHERE id = ?")
            .bind(director_run_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((status, stdout)) = landed else {
        return clear_director_node(state, &run).await;
    };
    if status == "running" {
        return Ok(());
    }

    let node = run.director_node.clone();
    if status != "completed" {
        // Without a director there is nobody to replan, so the run ends with whatever is in the
        // folder. `delivering` is the one node whose failure still leaves the specialists' files.
        return finish(
            state,
            &run,
            "failed",
            &format!("the director's {node} node {status}"),
        )
        .await;
    }

    let answer = stdout.unwrap_or_default();
    let answer = crate::runner::extract_reply(&answer).unwrap_or(answer);

    if node == "delivering" {
        write_into_workspace(state, &run, DELIVERY_FILE, &answer)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        return finish(state, &run, "done", "the department delivered").await;
    }

    let team_roster = roster(&state.pool, &run.team_id).await.unwrap_or_default();
    let plan = parse_team_plan(&answer, &team_roster, MAX_ITEMS_PER_ROUND);
    for dropped in plan.iter().flat_map(|plan| plan.dropped.iter()) {
        let _ = crate::feed::append(
            &state.pool,
            None,
            "team_item_dropped",
            dropped,
            None,
            Some(&crate::feed::Subject::TeamRun(run.id.clone())),
        )
        .await;
    }

    let max_rounds: i64 = sqlx::query_scalar("SELECT max_rounds FROM teams WHERE id = ?")
        .bind(&run.team_id)
        .fetch_one(&state.pool)
        .await?;
    let progress = next_after_director(
        &run.state,
        plan.as_ref(),
        run.round,
        run.dry_rounds,
        run.plan_retries,
        max_rounds,
    );

    if progress.state == "failed" {
        return finish(
            state,
            &run,
            "failed",
            progress.why.as_deref().unwrap_or("the director failed"),
        )
        .await;
    }

    // **One transaction, and the CLAIM comes first.** Guarding only the final `UPDATE` was not
    // enough and failed loudly the first time it was tested: the items are written before it, so a
    // second ingestion of the same node inserted the same ordinals again and died on the primary
    // key — with the first copy already committed. The compare-and-swap below is `job::spawn_node`'s
    // shape: zero rows affected means another caller has already taken this node, and there is
    // nothing left to do. Both halves inside one transaction, so a crash between them cannot leave
    // a claimed node whose items were never queued.
    let mut transaction = state.pool.begin().await?;

    let claimed = sqlx::query(
        "UPDATE team_runs SET director_node = 'none', director_run_id = NULL, updated_at = ?
         WHERE id = ? AND director_node = ? AND director_run_id = ?",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .bind(&node)
    .bind(director_run_id)
    .execute(&mut *transaction)
    .await?;
    if claimed.rows_affected() == 0 {
        transaction.rollback().await?;
        return Ok(());
    }

    let mut next_ordinal = run.next_ordinal;
    if progress.queue_items
        && let Some(plan) = plan.as_ref()
    {
        for item in &plan.items {
            sqlx::query(
                "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state)
                 VALUES (?, ?, ?, ?, ?, 'pending')",
            )
            .bind(&run.id)
            .bind(next_ordinal)
            .bind(progress.round)
            .bind(&item.agent_id)
            .bind(&item.description)
            .execute(&mut *transaction)
            .await?;
            next_ordinal += 1;
        }
    }

    sqlx::query(
        "UPDATE team_runs
         SET state = ?, round = ?, next_ordinal = ?, dry_rounds = ?, plan_retries = ?,
             replanned = ?, why = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(progress.state)
    .bind(progress.round)
    .bind(next_ordinal)
    .bind(progress.dry_rounds)
    .bind(progress.plan_retries)
    .bind(if node == "replanning" {
        "done"
    } else {
        &run.replanned
    })
    .bind(&progress.why)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(())
}

async fn clear_director_node(state: &AppState, run: &TeamRun) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE team_runs SET director_node = 'none', director_run_id = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Ends a run, once. The `state IN (live)` guard is what makes a second call a no-op rather than a
/// way to overwrite how a run ended.
async fn finish(
    state: &AppState,
    run: &TeamRun,
    ending: &str,
    why: &str,
) -> Result<(), sqlx::Error> {
    // The one place a run's ending is written, so the one place worth holding to the vocabulary.
    // `ending` is a `&str`, and a typo would store a state `is_live` reads as not-live, the GC
    // reads as collectable and nothing at all recognises — a run that has quietly stopped existing
    // to every reader while still sitting in the table.
    debug_assert!(
        TERMINAL_STATES.contains(&ending),
        "{ending} is not one of the five ways a team run ends"
    );
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE team_runs
         SET state = ?, outcome = ?, why = ?, director_node = 'none', updated_at = ?,
             finished_at = ?
         WHERE id = ? AND state IN ('planning', 'working', 'delivering')",
    )
    .bind(ending)
    .bind(ending)
    .bind(why)
    .bind(&now)
    .bind(&now)
    .bind(&run.id)
    .execute(&state.pool)
    .await?;

    let _ = crate::feed::append(
        &state.pool,
        None,
        "team_run_finished",
        &format!("a team run {ending}: {why}"),
        None,
        Some(&crate::feed::Subject::TeamRun(run.id.clone())),
    )
    .await;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Prompts
// ---------------------------------------------------------------------------------------------

/// What is in the folder, as the director reads it: the item that produced each file, who wrote it,
/// and its first line.
///
/// The first line and not the body, because the index goes into every director prompt and the
/// bodies are what `read_team_file` is for.
async fn folder_index(state: &AppState, run: &TeamRun) -> String {
    let items: Vec<TeamItem> = sqlx::query_as(
        "SELECT ordinal, round, agent_id, description, state, run_id, output_path
         FROM team_items WHERE team_run_id = ? ORDER BY ordinal",
    )
    .bind(&run.id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    if items.is_empty() {
        return "The folder is empty; nothing has been done yet.".to_owned();
    }

    let root = files_root(state).ok();
    let mut out = String::new();
    for item in items {
        let first_line = item
            .output_path
            .as_ref()
            .zip(root.as_ref())
            .and_then(|(name, root)| {
                let folder = crate::files::resolve_within(root, &run.workspace).ok()?;
                let path = crate::files::resolve_within(&folder, name).ok()?;
                let body = std::fs::read_to_string(path).ok()?;
                body.lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "(no answer)".to_owned());
        out.push_str(&format!(
            "- round {} item {} [{}] {} — {}: {}\n",
            item.round,
            item.ordinal,
            item.state,
            item.agent_id,
            item.output_path.as_deref().unwrap_or("(no file)"),
            first_line
        ));
    }
    out
}

/// The address book one specialist is shown: everybody it may write to, and not itself.
///
/// Separate from `roster_lines` because the two answer different questions. That one is what the
/// DIRECTOR is shown — who it may hand work to — and a director does not hand work to itself. This
/// one is who a specialist may TELL something, which includes the director and excludes the reader.
///
/// The director is marked rather than merely listed. A specialist choosing between two names should
/// know which of them decides what the next round contains, because that is usually where a finding
/// belongs, and a bare id says nothing about it.
async fn colleagues_lines(state: &AppState, team_id: &str, excluding: &str) -> String {
    let director: Option<String> =
        sqlx::query_scalar("SELECT director_agent_id FROM teams WHERE id = ?")
            .bind(team_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    let mut out = String::new();
    for id in addressable(&state.pool, team_id).await.unwrap_or_default() {
        if id == excluding {
            continue;
        }
        if let Ok(Some(agent)) = crate::agent::get(&state.pool, &id).await {
            let role = match director.as_deref() == Some(id.as_str()) {
                true => " (directs this department and plans the next round)",
                false => "",
            };
            out.push_str(&format!("- {}: {}{role}\n", agent.id, agent.speciality));
        }
    }
    out
}

async fn roster_lines(state: &AppState, team_id: &str) -> String {
    let mut out = String::new();
    for id in roster(&state.pool, team_id).await.unwrap_or_default() {
        if let Ok(Some(agent)) = crate::agent::get(&state.pool, &id).await {
            out.push_str(&format!("- {}: {}\n", agent.id, agent.speciality));
        }
    }
    out
}

/// The director's prompt, for planning and replanning alike.
///
/// The roster carries each member's `speciality` and NOT their `prompt`: a specialist's
/// instructions are its own, and putting them in the director's context would make every round pay
/// for text that only changes what somebody else does.
async fn director_prompt(state: &AppState, run: &TeamRun, team: &Team) -> String {
    format!(
        "You direct the {} department. Its mission: {}\n\n\
         The request you are working on:\n{}\n\n\
         Your specialists, and what each is for:\n{}\n\
         What is in the department's folder so far:\n{}\n\
         Reply with JSON and nothing else, in this shape:\n\
         {{\"items\": [{{\"agent_id\": \"...\", \"description\": \"...\"}}], \"done\": false, \"why\": \"...\"}}\n\n\
         Each item is one piece of work for one specialist, described well enough to be done \
         without asking you anything. Queue at most {} of them. Set \"done\" to true when the \
         folder holds enough to answer the request — everything queued beside a true \"done\" is \
         discarded, so do not do both. \"why\" is one sentence for the person who asked.{}",
        team.name,
        team.mission,
        run.request,
        roster_lines(state, &team.id).await,
        folder_index(state, run).await,
        MAX_ITEMS_PER_ROUND,
        pending_recruits_line(state, run).await,
    )
}

/// What the director has already asked the owner for, so it does not ask again.
///
/// `propose_teammate` refuses a duplicate, and that refusal alone is not enough: a director replans
/// every round with the same prompt and the same gap in front of it, so without this it spends a
/// turn per round walking into the same closed door. `calendar_proposal_pending_for` exists for the
/// identical reason and its doc says so.
///
/// Empty string when there is nothing pending, so the prompt of a department that never asked for
/// anybody is byte for byte what it was.
async fn pending_recruits_line(state: &AppState, run: &TeamRun) -> String {
    let pending = crate::proposals::list_pending_recruits(&state.pool, Some(&run.id))
        .await
        .unwrap_or_default();
    if pending.is_empty() {
        return String::new();
    }
    let named: Vec<String> = pending
        .iter()
        .map(|proposal| {
            let speciality = proposal
                .tool_input
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|payload| {
                    payload
                        .get("speciality")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            match proposal.tool_name.as_deref() {
                Some(id) if !speciality.is_empty() => format!("{id} ({speciality})"),
                Some(id) => id.to_owned(),
                None => "somebody".to_owned(),
            }
        })
        .collect();
    format!(
        "\n\nYou have already asked the owner for: {}. They are waiting to be decided and will not \
         join this run. Do not ask again — work with who you have, and say in the delivery what \
         was left thin because of it.",
        named.join("; ")
    )
}

async fn delivery_prompt(state: &AppState, run: &TeamRun, team: &Team) -> String {
    format!(
        "You direct the {} department. Its mission: {}\n\n\
         The request:\n{}\n\n\
         What is in the department's folder:\n{}\n\
         Write the department's answer to that request. Read any file you need with the \
         read_team_file tool, naming it exactly as it appears above. Reply with the finished \
         document itself — markdown, no preamble, no JSON. It is what the person who asked will \
         open, and it is the only thing they will read.{}",
        team.name,
        team.mission,
        run.request,
        folder_index(state, run).await,
        // Cheap, and it is the difference between a bad delivery and an honest one. Without it the
        // person reading a thin section has no way to know the department knew it was thin.
        match pending_recruits_line(state, run).await.is_empty() {
            true => String::new(),
            false =>
                " Say plainly, at the end, which part is thin and why — you asked for somebody \
                      this department does not have and did not get them in time."
                    .to_owned(),
        },
    ) + &undelivered_line(state, run).await
}

/// What one member of this department told another that nobody was ever given, ready to append.
///
/// The ordinary way for a note to end up here is not a failure: the round ended, the plan did not
/// queue that colleague again, and the words stayed in the queue. But the delivery is the last
/// moment anybody looks, and a queue quietly emptied by the run ending reads downstream as the whole
/// of what the department found. `job::PlannedItems::dropped` is the precedent and the sentence is
/// its.
///
/// Appended to the DELIVERY and not to every replan, because the director already gets its own mail
/// on every node it runs — what this covers is specifically the messages addressed to somebody else
/// that nobody will now ever read.
async fn undelivered_line(state: &AppState, run: &TeamRun) -> String {
    let left = crate::team_notes::undelivered(&state.pool, &run.id)
        .await
        .unwrap_or_default();
    if left.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nSome of this department's members left notes for colleagues who were never started \
         again, so nobody read them. They are below, and they are findings this department paid for. \
         Use what is useful and leave what is not:",
    );
    for note in &left {
        out.push_str(&format!(
            "\n\n{} wrote to {}:\n\n{}",
            note.from_agent_id, note.to_agent_id, note.body
        ));
    }
    out
}

/// A specialist is told its own instructions, its own task, the mission, the roster and the index —
/// and deliberately not the rest of the plan. What the others are DOING is not its context.
///
/// **The roster is, and it was the thing missing.** A specialist could not name a colleague, so
/// there was nobody for it to address even once there was a channel: `roster_lines` prints
/// `- <agents.id>: <speciality>`, which is exactly what `send_team_note` takes. It carries each
/// member's speciality and not their prompt, for `director_prompt`'s reason — a specialist's
/// instructions are its own, and putting them here would make every node pay for text that only
/// changes what somebody else does.
async fn specialist_prompt(
    state: &AppState,
    run: &TeamRun,
    mission: &str,
    item: &TeamItem,
) -> String {
    let own = crate::agent::get(&state.pool, &item.agent_id)
        .await
        .ok()
        .flatten()
        .map(|agent| agent.prompt)
        .unwrap_or_default();
    format!(
        "{own}\n\n\
         You are working inside a department whose mission is: {mission}\n\
         The department was asked to: {}\n\n\
         Your task:\n{}\n\n\
         Who else is in this department:\n{}\n\
         What is already in the department's folder:\n{}\n\
         Answer with your work itself. Do not write it to a file — your reply IS the deliverable, \
         and it is filed for you. If you find something that changes what one of the people above \
         should be doing, tell them with the send_team_note tool, addressing them by the id on the \
         left. They will not answer you — they are a separate run — so say it in a way they can act \
         on alone, and put it in your own answer as well.",
        run.request,
        item.description,
        colleagues_lines(state, &run.team_id, &item.agent_id).await,
        folder_index(state, run).await,
    )
}

// ---------------------------------------------------------------------------------------------
// Cancellation, reconciliation, retention
// ---------------------------------------------------------------------------------------------

/// Cancels a run and everything it has in flight.
///
/// The runs are terminated BEFORE the row is marked, so there is no window in which the run reads
/// terminal while its subprocesses are still spending. A cancelled run that left an orphan is the
/// one failure `core/AGENTS.md` §*Cancellation safety* names.
pub async fn cancel(state: &AppState, id: &str) -> Result<(), TeamError> {
    let in_flight: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM runs WHERE team_run_id = ? AND status = 'running'")
            .bind(id)
            .fetch_all(&state.pool)
            .await?;
    for run_id in in_flight {
        // `finalize_termination` rather than the `/runs/{id}/cancel` handler beside it: that one is
        // an axum extractor signature, and reaching it would mean a loopback HTTP call to a
        // function already in scope. This is the same body that handler runs.
        crate::runs::finalize_termination(state, run_id, "cancelled").await;
    }

    // And then the sweep, which is not belt-and-braces but a second case: `finalize_termination`
    // writes a status only for a run it holds an ABORT HANDLE for, and there is a window between
    // `open_run` writing the row and `spawn_registered` registering one. A row left `running` there
    // would be a run this department is recorded as still spending on, forever — nothing else
    // collects it until the next daemon restart. First writer still wins, so a row that finalised
    // itself a moment ago is untouched.
    sqlx::query(
        "UPDATE runs SET status = 'cancelled', completed_at = ?
         WHERE team_run_id = ? AND status = 'running'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(&state.pool)
    .await?;

    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE team_runs
         SET state = 'cancelled', outcome = 'cancelled', why = 'the owner cancelled it',
             director_node = 'none', updated_at = ?, finished_at = ?
         WHERE id = ? AND state IN ('planning', 'working', 'delivering')",
    )
    .bind(&now)
    .bind(&now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(TeamError::NotFound);
    }
    sqlx::query("UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND state IN ('pending', 'running')")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Unwedges the runs a dead daemon left behind, at startup.
///
/// Runs AFTER `runs::reconcile_orphaned_runs`, which is what marks the abandoned subprocesses
/// `interrupted` — the same ordering, and for the same reason, that `job::reconcile_orphaned_jobs`
/// respects.
///
/// **A director node that COMPLETED is ingested rather than reset**, and that is the case worth
/// getting right: the daemon died after the CLI wrote its answer, so resetting `director_node` to
/// `none` would throw away a node that has already been paid for and run it again. The reasoning is
/// `job::reconcile_nodes`': "it records work that already happened."
pub async fn reconcile_orphaned_team_runs(state: &AppState) -> Result<(), sqlx::Error> {
    let orphaned: Vec<(String, i64)> = sqlx::query_as(
        "SELECT i.team_run_id, i.ordinal
         FROM team_items i JOIN runs r ON r.id = i.run_id
         JOIN team_runs t ON t.id = i.team_run_id
         WHERE i.state = 'running' AND r.status != 'running' AND r.status != 'completed'
           AND t.state IN ('planning', 'working', 'delivering')",
    )
    .fetch_all(&state.pool)
    .await?;
    for (team_run_id, ordinal) in orphaned {
        sqlx::query("UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?")
            .bind(&team_run_id)
            .bind(ordinal)
            .execute(&state.pool)
            .await?;
    }

    // The completed-but-uningested nodes go through the very same function the tick uses. One
    // ingestion path and not two: the assertion `job.rs` bought with migration 0051 is that
    // ingesting the same node twice must not advance the round, and it only holds if there is one
    // place that can advance it.
    let live: Vec<TeamRun> = sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs
         WHERE state IN ('planning', 'working', 'delivering') AND director_node != 'none'",
    )
    .fetch_all(&state.pool)
    .await?;
    for run in live {
        let id = run.id.clone();
        if let Err(error) = ingest_director(state, run).await {
            tracing::warn!(team_run = %id, %error, "could not reconcile a team run's director node");
        }
    }
    Ok(())
}

/// Deletes the folders of runs that ended long enough ago, by AGE and never by state.
///
/// A worktree is disposable and nobody reads it; a department's folder IS the delivery, so
/// collecting it when the run ends would delete the thing the owner was about to open.
pub async fn workspace_gc(state: &AppState, now: chrono::DateTime<chrono::Utc>) {
    let cutoff = (now - chrono::Duration::days(TEAM_WORKSPACE_RETENTION_DAYS)).to_rfc3339();
    let stale: Vec<(String, String)> = match sqlx::query_as(
        "SELECT id, workspace FROM team_runs
         WHERE finished_at IS NOT NULL AND finished_at < ?
           AND state NOT IN ('planning', 'working', 'delivering')",
    )
    .bind(&cutoff)
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "could not list the team folders due for collection");
            return;
        }
    };

    let Ok(root) = files_root(state) else {
        return;
    };
    for (id, workspace) in stale {
        let Ok(folder) = crate::files::resolve_within(&root, &workspace) else {
            continue;
        };
        if folder.exists()
            && let Err(error) = std::fs::remove_dir_all(&folder)
        {
            tracing::warn!(team_run = %id, %error, "could not collect a finished team run's folder");
        }
    }
}

pub async fn run_team_loop(state: AppState) {
    let mut interval = tokio::time::interval(TEAM_TICK);
    loop {
        interval.tick().await;
        team_tick(&state, chrono::Utc::now()).await;
    }
}

pub async fn run_workspace_gc_loop(state: AppState) {
    let mut interval = tokio::time::interval(GC_INTERVAL);
    loop {
        interval.tick().await;
        workspace_gc(&state, chrono::Utc::now()).await;
    }
}

// ---------------------------------------------------------------------------------------------
// The HTTP surface
// ---------------------------------------------------------------------------------------------

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

fn team_status(error: &TeamError) -> StatusCode {
    match error {
        TeamError::DuplicateName => StatusCode::CONFLICT,
        TeamError::Invalid(_) | TeamError::UnknownAgent(_) => StatusCode::BAD_REQUEST,
        TeamError::NotFound => StatusCode::NOT_FOUND,
        TeamError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn refuse(error: TeamError) -> (StatusCode, String) {
    (team_status(&error), error.to_string())
}

pub async fn list_teams(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamView>>, (StatusCode, String)> {
    list(&state.pool).await.map(Json).map_err(refuse)
}

pub async fn create_team(
    State(state): State<AppState>,
    Json(request): Json<TeamRequest>,
) -> Result<(StatusCode, Json<TeamView>), (StatusCode, String)> {
    create(&state.pool, request)
        .await
        .map(|team| (StatusCode::CREATED, Json(team)))
        .map_err(refuse)
}

pub async fn get_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TeamView>, (StatusCode, String)> {
    match get(&state.pool, &id).await {
        Ok(Some(team)) => Ok(Json(team)),
        Ok(None) => Err(refuse(TeamError::NotFound)),
        Err(error) => Err(refuse(error)),
    }
}

pub async fn update_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<TeamRequest>,
) -> Result<Json<TeamView>, (StatusCode, String)> {
    update(&state.pool, &id, request)
        .await
        .map(Json)
        .map_err(refuse)
}

pub async fn delete_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    delete(&state.pool, &id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(refuse)
}

#[derive(serde::Deserialize)]
pub struct StartRequest {
    pub request: String,
    /// Where this run may speak while it works, or nothing.
    ///
    /// Optional, and absent on nearly every start: a department that reports nowhere behaves exactly
    /// as it did before this existed. Given here rather than configured on the team, so the address
    /// belongs to THIS piece of work and is chosen by the person starting it.
    #[serde(default)]
    pub report_to_chat_id: Option<String>,
    /// `normal | fast | thorough`. Absent is `normal`. Given per request, not configured on the
    /// team: the team's `max_parallel` IS `normal`, and speed is a choice about this piece of work.
    #[serde(default)]
    pub speed: Option<String>,
}

#[derive(serde::Serialize)]
pub struct StartResponse {
    pub id: String,
}

/// `POST /teams/{id}/runs` → 202. The record exists; the work has not happened yet, which is what
/// makes 202 the honest code and 201 a claim about a finished resource.
pub async fn post_team_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<StartRequest>,
) -> Result<(StatusCode, Json<StartResponse>), (StatusCode, String)> {
    let speed = match body.speed.as_deref() {
        None => crate::speed::Speed::Normal,
        Some(value) => crate::speed::Speed::parse(value).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("speed `{value}` is not one of normal, fast, thorough"),
            )
        })?,
    };
    match start_with(
        &state,
        &id,
        &body.request,
        Lineage::default(),
        speed,
        body.report_to_chat_id.as_deref(),
    )
    .await
    {
        Ok(id) => Ok((StatusCode::ACCEPTED, Json(StartResponse { id }))),
        Err(error @ StartError::NotFound) => Err((StatusCode::NOT_FOUND, error.to_string())),
        Err(error @ StartError::Invalid(_)) => Err((StatusCode::BAD_REQUEST, error.to_string())),
        // 429 and not 402: the ceiling is a window that reopens, and the caller should come back.
        Err(error @ (StartError::BudgetExhausted(_) | StartError::QuotaExhausted(_))) => {
            Err((StatusCode::TOO_MANY_REQUESTS, error.to_string()))
        }
        Err(error @ StartError::Unavailable(_)) => {
            Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
        }
    }
}

pub async fn list_team_runs(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamRun>>, (StatusCode, String)> {
    sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|error| refuse(TeamError::Db(error)))
}

pub async fn get_team_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TeamRunView>, (StatusCode, String)> {
    let run: Option<TeamRun> = sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| refuse(TeamError::Db(error)))?;
    let run = run.ok_or_else(|| refuse(TeamError::NotFound))?;

    let items: Vec<TeamItem> = sqlx::query_as(
        "SELECT ordinal, round, agent_id, description, state, run_id, output_path
         FROM team_items WHERE team_run_id = ? ORDER BY ordinal",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await
    .map_err(|error| refuse(TeamError::Db(error)))?;

    let cost_usd = spend_of(&state.pool, &id).await;
    Ok(Json(TeamRunView {
        run,
        items,
        cost_usd,
    }))
}

pub async fn post_team_run_cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    cancel(&state, &id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(refuse)
}

/// Deleting a run deletes its folder with it — the one way the owner disposes of a delivery before
/// the retention does. Refused while the run is live, because the alternative is deleting a folder
/// specialists are still writing into.
pub async fn delete_team_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT state, workspace FROM team_runs WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| refuse(TeamError::Db(error)))?;
    let (run_state, workspace) = row.ok_or_else(|| refuse(TeamError::NotFound))?;
    if is_live(&run_state) {
        return Err(refuse(TeamError::Invalid(
            "that run is still going; cancel it first",
        )));
    }

    if let Ok(root) = files_root(&state)
        && let Ok(folder) = crate::files::resolve_within(&root, &workspace)
        && folder.exists()
    {
        let _ = std::fs::remove_dir_all(&folder);
    }

    // The items before the run: the foreign key points that way, and `runs.team_run_id` is left
    // pointing at nothing on purpose — a run's cost stays in the ledger after the department that
    // spent it is gone.
    sqlx::query("UPDATE runs SET team_run_id = NULL WHERE team_run_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    sqlx::query("DELETE FROM team_items WHERE team_run_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    // Before the run, like the items and for the same reason: `team_notes.team_run_id` is a real
    // foreign key and `foreign_keys` is ON, so a note left behind does not become an orphan — it
    // makes the DELETE below fail. That is what the constraint is FOR, and it is only worth having
    // if somebody remembers the other half.
    crate::team_notes::delete_for_run(&state.pool, &id)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    // And the actions, which carried the identical reference since 0086 with nothing anywhere
    // deleting them. Deleting a department that had used its alçada answered 500 with `FOREIGN KEY
    // constraint failed` and named nothing — a run that never asked for anything deleted fine, which
    // is why it went unseen. `a_department_that_asked_for_something_can_still_be_deleted` is what
    // keeps it seen.
    //
    // The `proposals` rows they point at are deliberately LEFT. `team_actions.proposal_id` is a
    // logical reference and not a constraint, and what a person was asked to approve is a fact about
    // that person's queue — it does not stop having happened because the department that asked has
    // been tidied away. Same reasoning as `runs.team_run_id` being nulled rather than cascaded, one
    // statement up.
    sqlx::query("DELETE FROM team_actions WHERE team_run_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    sqlx::query("DELETE FROM team_runs WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
}

#[derive(Debug, serde::Serialize)]
pub struct FileView {
    pub path: String,
    pub content: String,
}

/// `POST /team-actions` — **the only route of this design a department may call**, and the only one
/// it will ever gain.
///
/// It does not perform the action. See `propose_action`: the surface stops growing here because
/// what arrives is a `kind` and a payload rather than a route per action, and what leaves is a
/// sentence rather than a result.
///
/// The status codes carry the same distinction the sentences do. 400 is "nobody can do this", 403 is
/// "you may not", 422 is "you asked wrongly", 429 is "come back when somebody has decided" — and a
/// model reading only the code would still pick the right next move for three of the four.
pub async fn post_team_action(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ProposeActionRequest>,
) -> Result<Json<ProposeActionResponse>, (StatusCode, String)> {
    propose_action(&state, &scope, &headers, &request)
        .await
        .map(Json)
        .map_err(|error| {
            let status = match &error {
                ActionError::NotADepartment | ActionError::Ungranted(_) => StatusCode::FORBIDDEN,
                ActionError::NoSuchRun => StatusCode::NOT_FOUND,
                ActionError::Unknown(_) => StatusCode::BAD_REQUEST,
                ActionError::Malformed(_) => StatusCode::UNPROCESSABLE_ENTITY,
                // 429 and not 403: the ceiling is a queue that drains, and the caller — or the next
                // run of this department — should come back.
                ActionError::QueueFull(_) => StatusCode::TOO_MANY_REQUESTS,
                ActionError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })
}

/// `POST /team-recruits` — a director says who it needed and did not have.
///
/// The first route in the house that answers differently depending on WHICH NODE of a run is
/// calling. That distinction comes from `team_runs.director_run_id` and not from a scope of its own
/// — see `Caller`, and see `team::send_note`, which now asks the same question for a different
/// reason: this one to refuse a specialist, that one to sign a note.
///
/// (This paragraph opened "The second and last route this scope gains" until `POST /team-notes`
/// became the third. The count was written as a promise and did not survive one feature; what the
/// promise was reaching for is now stated once, as a rule about what belongs in the table, in
/// `auth::TEAM_ROUTES`.)
///
/// 403 for a specialist, and the body says what to do instead: put it in your answer and let the
/// director pass it on. A model reading only the code would retry; the sentence is what stops it.
pub async fn post_team_recruit(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<RecruitRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let answered = if request.suggest_only {
        suggest_model(&state, &scope, &headers, &request).await
    } else {
        propose_teammate(&state, &scope, &headers, &request)
            .await
            .map(|filed| serde_json::to_value(filed).unwrap_or_default())
    };
    answered.map(Json).map_err(|error| {
        let status = match &error {
            RecruitError::NotADepartment | RecruitError::NotTheDirector => StatusCode::FORBIDDEN,
            RecruitError::NoSuchRun => StatusCode::NOT_FOUND,
            // 409 for both: somebody with that id already exists, or a question about them is
            // already open. Neither is a malformed request, and both clear by somebody acting.
            RecruitError::AlreadyExists(_) | RecruitError::AlreadyAsked(_) => StatusCode::CONFLICT,
            RecruitError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            RecruitError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, error.to_string())
    })
}

/// `POST /team-notes` — one member of a department leaves words for another.
///
/// The third route this scope gains, and the first whose effect never leaves the run that made it:
/// what it writes is read by another node holding the very same key. The two before it record a
/// request for somebody OUTSIDE to act on; this one records a sentence for somebody inside.
///
/// The status codes carry the same distinction the sentences do, and 422 is doing the most work:
/// every one of its three causes is a model that can still fix it this turn, with the roster in
/// front of it.
pub async fn post_team_note(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<TeamNoteRequest>,
) -> Result<Json<TeamNoteResponse>, (StatusCode, String)> {
    send_note(&state, &scope, &headers, &request)
        .await
        .map(Json)
        .map_err(|error| {
            let status = match &error {
                NoteError::NotADepartment | NoteError::UnknownNode => StatusCode::FORBIDDEN,
                NoteError::NoSuchRun => StatusCode::NOT_FOUND,
                // 422 and not 400 for all three: the request is well-formed and it is the CONTENT
                // that cannot stand — a name off the roster, the caller's own name, nothing at all.
                // Each is a model that mis-typed or mis-thought, still mid-turn, and the body names
                // the members so the retry is informed rather than blind.
                NoteError::NoSuchColleague { .. } | NoteError::Yourself | NoteError::Empty => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                // 429 for `propose_action`'s reason, with one difference worth knowing: that ceiling
                // is a queue that DRAINS as a person decides, and this one does not — it is spent
                // for the life of the run. The code still says "not now, and not by retrying", which
                // is the part that governs what the model does next.
                NoteError::Ceiling(_) => StatusCode::TOO_MANY_REQUESTS,
                NoteError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })
}

/// `POST /team-reports` — a department says something to its owner, and nothing runs.
///
/// The fourth and last route of this scope, and the only one that reaches outside the department at
/// all. It is still a SAYING rather than a DOING, which is the rule `auth::TEAM_ROUTES` keeps: no
/// turn starts, nothing is spent, and the daemon picks the destination out of a column the
/// department can neither read nor set.
///
/// 403 for a specialist, with the sentence that says what to do instead — `post_team_recruit`'s
/// shape, because a model reading only the code would retry.
pub async fn post_team_report(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ReportRequest>,
) -> Result<Json<ReportResponse>, (StatusCode, String)> {
    report(&state, &scope, &headers, &request)
        .await
        .map(Json)
        .map_err(|error| {
            let status = match &error {
                ReportError::NotADepartment | ReportError::NotTheDirector => StatusCode::FORBIDDEN,
                ReportError::NoSuchRun => StatusCode::NOT_FOUND,
                // 409 for both, and not 404: the run exists and so does the request. What is absent
                // is a place to speak — either never given or since closed — and neither is fixed by
                // asking again differently, which is what a 4xx about the REQUEST would suggest.
                ReportError::NowhereToReport | ReportError::DestinationGone(_) => {
                    StatusCode::CONFLICT
                }
                ReportError::Empty => StatusCode::UNPROCESSABLE_ENTITY,
                ReportError::Ceiling(_) => StatusCode::TOO_MANY_REQUESTS,
                ReportError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })
}

/// `GET /team-runs/{id}/actions` — what that department asked for, and what became of it.
pub async fn list_team_run_actions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TeamAction>>, (StatusCode, String)> {
    sqlx::query_as(
        "SELECT id, team_run_id, ordinal, kind, payload, why, proposal_id, state, error,
                created_at, executed_at
         FROM team_actions WHERE team_run_id = ? ORDER BY id",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|error| refuse(TeamError::Db(error)))
}

/// `GET /team-actions` — everything still waiting on somebody, across every department.
///
/// Includes the `allow` actions that have not run yet, which are waiting on the tick rather than on
/// a person. Both are "asked for and not yet done", which is the question this list answers.
pub async fn list_open_actions(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamAction>>, (StatusCode, String)> {
    sqlx::query_as(
        "SELECT id, team_run_id, ordinal, kind, payload, why, proposal_id, state, error,
                created_at, executed_at
         FROM team_actions WHERE state IN ('pending', 'working') ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|error| refuse(TeamError::Db(error)))
}

/// `POST /team-files/read` — one file out of the calling run's own folder.
///
/// **Which run is reading comes from the key, not from the body.** A `team_run_id` field here would
/// be the caller naming what it may open, and this is the one scope in the house that already names
/// its caller — so it is asked rather than believed. A caller without `Scope::TeamRun` is refused
/// with 403 and not 401: it authenticated perfectly well, it is simply not a department.
///
/// The path is resolved twice, and the second resolution is the one that matters. The workspace is
/// resolved against the files root, and the caller's path against the workspace — so `files.rs`
/// applies its whitelist to what the model chose: `..`, absolutes, drive prefixes, UNC shares and
/// symlinks pointing out of the folder are all refused there rather than reasoned about here.
pub async fn post_read_file(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    Json(request): Json<ReadFileRequest>,
) -> Result<Json<FileView>, (StatusCode, String)> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err((
            StatusCode::FORBIDDEN,
            "only a team run reads a team run's folder".to_owned(),
        ));
    };

    let workspace: String = sqlx::query_scalar("SELECT workspace FROM team_runs WHERE id = ?")
        .bind(&team_run_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "no such team run".to_owned()))?;

    let root = files_root(&state).map_err(|why| (StatusCode::SERVICE_UNAVAILABLE, why))?;
    let folder = crate::files::resolve_within(&root, &workspace)
        .map_err(|_| (StatusCode::NOT_FOUND, "no such folder".to_owned()))?;
    let target = crate::files::resolve_within(&folder, &request.path).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "that path is not inside your folder".to_owned(),
        )
    })?;

    let content = tokio::fs::read_to_string(&target)
        .await
        .map_err(|error| (StatusCode::NOT_FOUND, error.to_string()))?;

    Ok(Json(FileView {
        path: request.path,
        content,
    }))
}

/// The one piece of this module's test scaffolding another module needs.
///
/// `team_trigger.rs` starts real departments to test what starts them, and a second copy of this
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
            token: crate::auth::Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            run_tails: Default::default(),
            files_root: Some(root),
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::voice::VoiceRuntime::default()),
            // Off, like `web` beside it: no test in this module drives a browser, and a department
            // reaches one — if it ever does — through the daemon client like any other agent.
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::github::GithubRuntime::default()),
            web: Arc::new(crate::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::calendar::CalendarRuntime::default()),
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
            crate::quota_client::QuotaClient::new(&address, "bearer".into()),
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

        let told = crate::chat_notices::for_chat(&state.pool, &chat_id)
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
            crate::chat_notices::for_chat(&state.pool, &chat_id)
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
        surfaces: &[(&'static str, crate::route_advice::Mode)],
    ) -> tokio::sync::mpsc::UnboundedReceiver<serde_json::Value> {
        let (url, sent) = crate::router_client::test_support::stub_router(
            200,
            serde_json::json!({
                "decision_id": "rt_team", "runner": "claude", "model": "claude-opus-5",
                "effort": "high", "estimated_cost_usd": 0.2, "rule": "hard",
            }),
        )
        .await;
        let config = crate::route_advice::RouterConfig {
            mode: crate::route_advice::Mode::Off,
            url,
            surfaces: surfaces.iter().copied().collect(),
            ..crate::route_advice::RouterConfig::off()
        };
        let inner = state.runner.clone();
        let router = std::sync::Arc::new(crate::route_advice::Router::new(
            config,
            crate::route_advice::Available {
                kind: crate::route_advice::RunnerKind::Claude,
                runner: inner.clone(),
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ));
        state.runner = std::sync::Arc::new(crate::route_advice::RoutedRunner { inner, router });
        sent
    }

    /// `suggest_model` answers only the director -- the same check, the same sentence -- and files
    /// nothing either way.
    #[tokio::test]
    async fn only_the_director_is_told_which_model_to_recruit() {
        let (mut state, _root) = state_with_root().await;
        let mut sent = advised_runner(
            &mut state,
            &[("recruit", crate::route_advice::Mode::Shadow)],
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
        let mut sent =
            advised_runner(&mut state, &[("recruit", crate::route_advice::Mode::Apply)]).await;
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
        let mut sent =
            advised_runner(&mut state, &[("recruit", crate::route_advice::Mode::Apply)]).await;
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
        state.runner = std::sync::Arc::new(crate::runner::FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(crate::runner::RunOutcome {
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
            let runner = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
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
        let runner = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
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
            &crate::local_agent::Turn {
                answer: "done".to_owned(),
                ending: crate::local_agent::Ending::Answered,
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
        ) -> Result<Box<dyn crate::local_agent::LocalChat>, crate::assistants::Refusal> {
            self.asked_for
                .lock()
                .expect("the double's recorder is never held across an await")
                .push(model.to_string());
            Ok(Box::new(
                crate::openai_compatible::OpenAiCompatibleChat::with_client(
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
        ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, crate::assistants::Refusal>
        {
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
        ) -> Result<Box<dyn crate::local_agent::LocalChat>, crate::assistants::Refusal> {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        fn assistant_for(
            &self,
            _brain: crate::chats::Brain,
            _model: Option<&str>,
        ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, crate::assistants::Refusal>
        {
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
        let mut sent =
            advised_runner(&mut state, &[("team", crate::route_advice::Mode::Apply)]).await;
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

    /// A cloud member under `team: apply` launches on the advice, and the row says so.
    #[tokio::test]
    async fn a_cloud_team_member_under_apply_launches_the_advice() {
        let (mut state, _root) = state_with_root().await;
        let fake = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
        state.runner = fake.clone();
        let mut sent =
            advised_runner(&mut state, &[("team", crate::route_advice::Mode::Apply)]).await;
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
        let fake = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
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
        let fake = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
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
        let fake = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
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
        let fake = std::sync::Arc::new(crate::runner::FakeCommandRunner::default());
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
        let router = std::sync::Arc::new(crate::route_advice::Router::new(
            crate::route_advice::RouterConfig {
                mode: crate::route_advice::Mode::Shadow,
                url,
                ..crate::route_advice::RouterConfig::off()
            },
            crate::route_advice::Available {
                kind: crate::route_advice::RunnerKind::Claude,
                runner: inner.clone(),
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ));
        state.runner = std::sync::Arc::new(crate::route_advice::RoutedRunner { inner, router });
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
}
