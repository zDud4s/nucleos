use serde::{Deserialize, Serialize};

/// One named agent in the house catalogue.
///
/// Deliberately not owned by a team: a council seat is not a member of any department, and the
/// alternative — a catalogue per team — is the second place the same truth gets declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Agent {
    pub id: String,
    pub name: String,
    /// One line. This is what a director reads to decide who gets the work, so it is load-bearing
    /// and not decoration.
    pub speciality: String,
    pub prompt: String,
    pub engine: String,
    pub model: Option<String>,
    pub tool_policy: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct AgentRequest {
    pub name: String,
    pub speciality: String,
    pub prompt: String,
    pub engine: String,
    pub model: Option<String>,
    pub tool_policy: String,
}

#[derive(Debug)]
pub enum AgentError {
    DuplicateName,
    Invalid(&'static str),
    NotFound,
    InUse,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for AgentError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Says "or id" because both collisions arrive as the same unique violation: two names
            // the UNIQUE lets coexist can still slug down to one id, and "that name already exists"
            // would then be false in a way the owner cannot act on.
            Self::DuplicateName => {
                formatter.write_str("an agent with that name or id already exists")
            }
            Self::Invalid(message) => formatter.write_str(message),
            Self::NotFound => formatter.write_str("agent not found"),
            // Names the work as well as the rosters, because they are undone in different places
            // and the owner has to be told which one to go to. Told only about teams, somebody who
            // has already left every team reads this as the daemon being wrong.
            Self::InUse => formatter.write_str(
                "that agent directs or belongs to a team, or is holding work that names it",
            ),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(error) => Some(error),
            Self::DuplicateName | Self::Invalid(_) | Self::NotFound | Self::InUse => None,
        }
    }
}

/// The engines an agent may declare.
///
/// A constant rather than a `matches!` arm, because `council.rs` has to translate every one of them
/// into a `SeatKind`: an engine added here with no translation there is a seat that cannot run, and
/// the test that catches it needs both lists to be readable from one place.
pub const ENGINES: &[&str] = &["claude", "codex", "local"];

/// PURE: what an agent is allowed to declare about itself.
///
/// `unrestricted` is refused rather than accepted-and-ignored. That policy exists for code runs
/// governed by the `PreToolUse` classifier (`runs::classifier_governs_tools`), and an agent of this
/// catalogue has no worktree and no hook wired — accepting it would name a barrier that is not
/// there. A `local` engine without a model is refused for the sibling reason: silently falling back
/// to cloud would swap the model the owner chose for one that spends.
///
/// Public because a recruitment is validated TWICE and by two callers: once when a director
/// proposes, so the refusal reaches a model that can still fix it, and once over whatever the owner
/// edited at approval — which is the one that decides. Two copies of these rules would be two
/// answers to "what may an agent declare about itself".
pub fn validate_request(request: &AgentRequest) -> Result<(), &'static str> {
    validate(request).map_err(|error| match error {
        AgentError::Invalid(message) => message,
        // `validate` returns nothing else, and a total mapping beats an `unreachable!` in a path a
        // background approval walks.
        _ => "that is not an agent this daemon will accept",
    })
}

fn validate(request: &AgentRequest) -> Result<(), AgentError> {
    if request.name.trim().is_empty() {
        return Err(AgentError::Invalid("name must not be empty"));
    }
    if request.speciality.trim().is_empty() {
        return Err(AgentError::Invalid(
            "speciality must not be empty — it is what a director reads to delegate",
        ));
    }
    if !ENGINES.contains(&request.engine.as_str()) {
        return Err(AgentError::Invalid("engine must be claude, codex or local"));
    }
    if !matches!(request.tool_policy.as_str(), "mcp_only" | "none") {
        return Err(AgentError::Invalid(
            "tool_policy must be mcp_only or none; unrestricted is for classifier-governed runs",
        ));
    }
    if request.engine == "local" && request.model.is_none() {
        return Err(AgentError::Invalid("a local agent must name its model"));
    }
    Ok(())
}

/// PURE: the id a name earns, ONCE.
///
/// Derived here rather than accepted from the caller, so creating an agent cannot smuggle in an id
/// that collides with one a team already references. It is computed at creation and then frozen:
/// `update` never recomputes it, so after a rename the id and the name DO disagree, by design. The
/// id is what `team_members`, `teams.director_agent_id` and `team_items.agent_id` point at, and a
/// reference that changes when somebody edits a label is a reference that breaks in silence.
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

pub async fn create(pool: &sqlx::SqlitePool, request: AgentRequest) -> Result<Agent, AgentError> {
    validate(&request)?;
    let id = slug(&request.name);
    if id.is_empty() {
        return Err(AgentError::Invalid("name must contain a letter or a digit"));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
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
    .execute(pool)
    .await;
    match result {
        Ok(_) => get(pool, &id).await?.ok_or(AgentError::NotFound),
        Err(error) if is_unique_violation(&error) => Err(AgentError::DuplicateName),
        Err(error) => Err(AgentError::Db(error)),
    }
}

pub async fn list(pool: &sqlx::SqlitePool) -> Result<Vec<Agent>, AgentError> {
    sqlx::query_as(
        "SELECT id, name, speciality, prompt, engine, model, tool_policy, created_at, updated_at
         FROM agents ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(AgentError::Db)
}

pub async fn get(pool: &sqlx::SqlitePool, id: &str) -> Result<Option<Agent>, AgentError> {
    sqlx::query_as(
        "SELECT id, name, speciality, prompt, engine, model, tool_policy, created_at, updated_at
         FROM agents WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(AgentError::Db)
}

/// The id is deliberately NOT recomputed from the new name: it is a reference, and
/// `team_members`, `teams.director_agent_id` and `team_items.agent_id` point at it.
pub async fn update(
    pool: &sqlx::SqlitePool,
    id: &str,
    request: AgentRequest,
) -> Result<Agent, AgentError> {
    validate(&request)?;
    let result = sqlx::query(
        "UPDATE agents
         SET name = ?, speciality = ?, prompt = ?, engine = ?, model = ?, tool_policy = ?,
             updated_at = ?
         WHERE id = ?",
    )
    .bind(&request.name)
    .bind(&request.speciality)
    .bind(&request.prompt)
    .bind(&request.engine)
    .bind(&request.model)
    .bind(&request.tool_policy)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(pool)
    .await;
    match result {
        Ok(result) if result.rows_affected() == 0 => Err(AgentError::NotFound),
        Ok(_) => get(pool, id).await?.ok_or(AgentError::NotFound),
        Err(error) if is_unique_violation(&error) => Err(AgentError::DuplicateName),
        Err(error) => Err(AgentError::Db(error)),
    }
}

pub async fn delete(pool: &sqlx::SqlitePool, id: &str) -> Result<(), AgentError> {
    // Asked before the DELETE even though the foreign keys already refuse it — measured, not
    // assumed: without this the same call comes back as
    // `Db(SqliteError { code: 787, "FOREIGN KEY constraint failed" })`, which reaches the owner as
    // a 500 naming nothing. This turns the identical refusal into a 409 that says which side is
    // standing on the agent. It does not replace the constraint; it explains it.
    //
    // The two WORK tables are named as well as the two roster ones, and the difference between them
    // is the reason. Being on a roster and having been given work are separate facts: an agent taken
    // off a team still owns every item already assigned to it, and the roster checks stop seeing it
    // the moment the membership row goes. Without these two, the sequence "remove from the team,
    // then delete the agent" reaches the owner as exactly the 500 this function exists to prevent.
    let in_use: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM teams WHERE director_agent_id = ?)
             OR EXISTS (SELECT 1 FROM team_members WHERE agent_id = ?)
             OR EXISTS (SELECT 1 FROM team_items WHERE agent_id = ?)
             OR EXISTS (SELECT 1 FROM job_items WHERE agent_id = ?)",
    )
    .bind(id)
    .bind(id)
    .bind(id)
    .bind(id)
    .fetch_one(pool)
    .await?;
    if in_use {
        return Err(AgentError::InUse);
    }

    let result = sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AgentError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn request(name: &str) -> AgentRequest {
        AgentRequest {
            name: name.to_owned(),
            speciality: "writes short copy".to_owned(),
            prompt: "You write short copy.".to_owned(),
            engine: "claude".to_owned(),
            model: Some("claude-opus-5".to_owned()),
            tool_policy: "mcp_only".to_owned(),
        }
    }

    #[tokio::test]
    async fn an_unrestricted_tool_policy_is_refused() {
        let pool = pool().await;
        let mut asked = request("copywriter");
        asked.tool_policy = "unrestricted".to_owned();
        assert!(matches!(
            create(&pool, asked).await,
            Err(AgentError::Invalid(_))
        ));
    }

    /// Named for the engine and not for "a local agent", because `cargo test agent::` matches by
    /// substring and the run is filtered with `--skip local_agent` to keep `local_agent::`'s
    /// fourteen tests out. A test whose own name contains `local_agent` would be skipped by that
    /// same filter, silently, forever.
    #[tokio::test]
    async fn a_local_engine_without_a_model_is_refused() {
        let pool = pool().await;
        let mut asked = request("analyst");
        asked.engine = "local".to_owned();
        asked.model = None;
        assert!(matches!(
            create(&pool, asked).await,
            Err(AgentError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn an_unknown_engine_is_refused() {
        let pool = pool().await;
        let mut asked = request("analyst");
        asked.engine = "gemini".to_owned();
        assert!(matches!(
            create(&pool, asked).await,
            Err(AgentError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn create_list_get_update_and_delete_round_trip() {
        let pool = pool().await;
        let first = create(&pool, request("copywriter")).await.unwrap();
        let second = create(&pool, request("analyst")).await.unwrap();

        let listed = list(&pool).await.unwrap();
        assert_eq!(
            listed.iter().map(|agent| &agent.name).collect::<Vec<_>>(),
            ["analyst", "copywriter"]
        );
        assert_eq!(get(&pool, &first.id).await.unwrap(), Some(first.clone()));

        let mut changed = request("renamed");
        changed.speciality = "writes long copy".to_owned();
        let updated = update(&pool, &first.id, changed).await.unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.speciality, "writes long copy");
        assert_eq!(updated.id, first.id);
        assert_eq!(second.engine, "claude");

        delete(&pool, &first.id).await.unwrap();
        assert_eq!(get(&pool, &first.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn duplicate_names_are_a_typed_conflict() {
        let pool = pool().await;
        create(&pool, request("copywriter")).await.unwrap();
        assert!(matches!(
            create(&pool, request("copywriter")).await,
            Err(AgentError::DuplicateName)
        ));
    }

    #[tokio::test]
    async fn deleting_an_unknown_id_is_reported() {
        assert!(matches!(
            delete(&pool().await, "nobody").await,
            Err(AgentError::NotFound)
        ));
    }

    #[tokio::test]
    async fn the_id_is_a_slug_of_the_name() {
        let pool = pool().await;
        let created = create(&pool, request("Head of Content")).await.unwrap();
        assert_eq!(created.id, "head-of-content");
    }

    /// Two names the `UNIQUE` on `name` lets coexist, that the slug collapses into one id. Worth a
    /// test because it is the only surprising thing in this slice: the row is refused for a reason
    /// the owner did not type.
    #[tokio::test]
    async fn two_names_that_slug_the_same_are_a_conflict() {
        let pool = pool().await;
        create(&pool, request("Head of Content")).await.unwrap();
        assert!(matches!(
            create(&pool, request("head-of-content")).await,
            Err(AgentError::DuplicateName)
        ));
    }

    async fn team_with(pool: &sqlx::SqlitePool, director: &str, member: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES ('marketing', 'Marketing', 'sells', ?, 4, 2, ?, ?)",
        )
        .bind(director)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO team_members (team_id, agent_id) VALUES ('marketing', ?)")
            .bind(member)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_director_cannot_be_deleted() {
        let pool = pool().await;
        let director = create(&pool, request("head")).await.unwrap();
        let member = create(&pool, request("copywriter")).await.unwrap();
        team_with(&pool, &director.id, &member.id).await;
        let outcome = delete(&pool, &director.id).await;
        assert!(matches!(outcome, Err(AgentError::InUse)), "{outcome:?}");
    }

    #[tokio::test]
    async fn a_member_cannot_be_deleted() {
        let pool = pool().await;
        let director = create(&pool, request("head")).await.unwrap();
        let member = create(&pool, request("copywriter")).await.unwrap();
        team_with(&pool, &director.id, &member.id).await;
        let outcome = delete(&pool, &member.id).await;
        assert!(matches!(outcome, Err(AgentError::InUse)), "{outcome:?}");
    }

    /// Off the team and still holding work, which is the case the two roster checks cannot see.
    ///
    /// Being on a roster and having been given work are separate facts, and the second one outlives
    /// the first: the membership row goes and the job item still names this agent. Without the work
    /// tables in the check, "remove from the team, then delete" reaches the owner as the raw
    /// `FOREIGN KEY constraint failed` — a 500 naming nothing, which is the exact outcome this
    /// function exists to turn into a 409.
    #[tokio::test]
    async fn an_agent_taken_off_a_team_still_cannot_be_deleted_while_it_holds_work() {
        let pool = pool().await;
        let director = create(&pool, request("head")).await.unwrap();
        let member = create(&pool, request("copywriter")).await.unwrap();
        team_with(&pool, &director.id, &member.id).await;
        sqlx::query("INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
                     VALUES ('project-a', 'C:/somewhere', 'implementing', 5, '2026-08-20T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, agent_id)
             VALUES (1, 0, 'an item', 'pending', ?)",
        )
        .bind(&member.id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM team_members WHERE agent_id = ?")
            .bind(&member.id)
            .execute(&pool)
            .await
            .unwrap();

        let outcome = delete(&pool, &member.id).await;

        assert!(matches!(outcome, Err(AgentError::InUse)), "{outcome:?}");
    }

    #[tokio::test]
    async fn an_agent_in_no_team_still_deletes() {
        let pool = pool().await;
        let director = create(&pool, request("head")).await.unwrap();
        let member = create(&pool, request("copywriter")).await.unwrap();
        let spare = create(&pool, request("analyst")).await.unwrap();
        team_with(&pool, &director.id, &member.id).await;
        delete(&pool, &spare.id).await.unwrap();
    }
}
