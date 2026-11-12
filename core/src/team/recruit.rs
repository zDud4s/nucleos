//! Team recruitment proposals and approvals.

use super::*;

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
    Ok(crate::models::seat_advice::suggest_model(
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
    // A director with no engine of its own (a package agent) hands on "the router decides".
    let engine = match request.engine.clone() {
        Some(engine) => Some(engine),
        None => match director.as_ref() {
            Some(agent) => agent.engine.clone(),
            None => Some("claude".to_owned()),
        },
    };
    let policy = request.tool_policy.as_deref().unwrap_or("mcp_only");
    let capabilities = capabilities_for_policy(policy).ok_or_else(|| {
        RecruitError::Invalid(
            "tool_policy must be mcp_only or none; unrestricted is for classifier-governed runs"
                .to_owned(),
        )
    })?;
    let mut proposed = crate::agent::AgentRequest {
        name: request.name.trim().to_owned(),
        speciality: request.speciality.trim().to_owned(),
        prompt: request.prompt.clone(),
        engine,
        model: request
            .model
            .clone()
            .or_else(|| director.as_ref().and_then(|agent| agent.model.clone())),
        capabilities,
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
        && let Some(advised) = crate::models::seat_advice::recruit_default(
            state.runner.router(),
            proposed.engine.as_deref().unwrap_or("claude"),
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
        "capabilities": proposed.capabilities,
        // The director's own vocabulary, kept beside the stored one for whoever reads the payload.
        "tool_policy": policy,
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

/// PURE: a director's `tool_policy` word as the capabilities a `user` agent stores; `None` for
/// anything but the two policies a recruit may have (`unrestricted` is never recruited).
fn capabilities_for_policy(policy: &str) -> Option<Vec<String>> {
    match policy {
        "mcp_only" => Some(vec!["mcp.nucleos".to_owned()]),
        "none" => Some(Vec::new()),
        _ => None,
    }
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
        None => {
            let mut stored: crate::agent::AgentRequest =
                serde_json::from_value(payload.clone()).map_err(|_| HireError::Malformed)?;
            // A proposal filed before the catalogue stored capabilities carries only `tool_policy`.
            if payload.get("capabilities").is_none()
                && let Some(policy) = payload.get("tool_policy").and_then(|value| value.as_str())
            {
                stored.capabilities =
                    capabilities_for_policy(policy).ok_or(HireError::Malformed)?;
            }
            stored
        }
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
        "INSERT INTO agents (id, name, speciality, prompt, engine, model, capabilities,
                             source, package_ref, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'user', NULL, ?, ?)",
    )
    .bind(&id)
    .bind(&request.name)
    .bind(&request.speciality)
    .bind(&request.prompt)
    .bind(&request.engine)
    .bind(&request.model)
    .bind(serde_json::to_string(&request.capabilities).unwrap_or_else(|_| "[]".to_owned()))
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
