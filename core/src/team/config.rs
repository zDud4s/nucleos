//! Team catalogue configuration, roster, and grant persistence.

use super::*;

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
    if !(1..=crate::team::trigger::MAX_LIVE_TEAM_RUNS).contains(&request.max_live_runs) {
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
pub(super) fn slug(name: &str) -> String {
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
