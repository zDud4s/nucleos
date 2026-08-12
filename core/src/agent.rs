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
            Self::InUse => formatter.write_str("that agent is a member or director of a team"),
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

/// PURE: what an agent is allowed to declare about itself.
///
/// `unrestricted` is refused rather than accepted-and-ignored. That policy exists for code runs
/// governed by the `PreToolUse` classifier (`runs::classifier_governs_tools`), and an agent of this
/// catalogue has no worktree and no hook wired — accepting it would name a barrier that is not
/// there. A `local` engine without a model is refused for the sibling reason: silently falling back
/// to cloud would swap the model the owner chose for one that spends.
fn validate(request: &AgentRequest) -> Result<(), AgentError> {
    if request.name.trim().is_empty() {
        return Err(AgentError::Invalid("name must not be empty"));
    }
    if request.speciality.trim().is_empty() {
        return Err(AgentError::Invalid(
            "speciality must not be empty — it is what a director reads to delegate",
        ));
    }
    if !matches!(request.engine.as_str(), "claude" | "codex" | "local") {
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

pub async fn create(pool: &sqlx::SqlitePool, request: AgentRequest) -> Result<Agent, AgentError> {
    validate(&request)?;
    let _ = pool;
    todo!("Task 3")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
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
}
