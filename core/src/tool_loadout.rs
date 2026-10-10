//! The tools an agent or team holds beyond its box's base set, and how a run asks for one more.

use crate::mcp_tools::{self, Admission, McpBox};
use sqlx::SqliteConnection;

/// The prefix a CLI puts in front of a NucleOS tool's name.
const TOOL_PREFIX: &str = "mcp__nucleos__";

/// Whether the loadout frozen for this run lists `tool`. A missing row or an unreadable list says
/// no: a run that cannot prove it holds a tool does not hold it.
pub async fn run_lists_tool(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    tool: &str,
) -> Result<bool, sqlx::Error> {
    let tools: Option<String> =
        sqlx::query_scalar("SELECT tools FROM run_loadout WHERE run_id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
    Ok(tools
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .is_some_and(|listed| listed.iter().any(|name| name == tool)))
}

/// The box a run's MCP server is served from, derived from the run itself.
pub async fn caller_box(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> Result<Option<McpBox>, sqlx::Error> {
    let run: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT mode, job_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
    Ok(match run {
        Some((mode, _)) if mode == crate::team::TEAM_MODE => Some(McpBox::Team(run_id)),
        Some((_, Some(job_id))) => Some(McpBox::JobNode(job_id)),
        _ => None,
    })
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct ToolRow {
    pub id: i64,
    pub owner_kind: String,
    pub owner_id: String,
    pub tool: String,
    pub status: String,
    pub source: String,
    pub reason: Option<String>,
    pub run_id: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
}

/// The `ToolRow` column list as a literal, so `concat!` can build `&'static str` queries
/// (sqlx refuses a runtime `format!` string).
macro_rules! row_columns {
    () => {
        "id, owner_kind, owner_id, tool, status, source, reason, run_id, created_at, decided_at"
    };
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

async fn event(
    conn: &mut SqliteConnection,
    row: i64,
    from: Option<&str>,
    to: &str,
    note: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO loadout_tool_events (tool_row_id, from_status, to_status, note, at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(row)
    .bind(from)
    .bind(to)
    .bind(note)
    .bind(now())
    .execute(conn)
    .await?;
    Ok(())
}

async fn row_by_id(conn: &mut SqliteConnection, id: i64) -> Result<Option<ToolRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        row_columns!(),
        " FROM loadout_tools WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// Rows in id order; every filter is optional.
pub async fn list(
    pool: &sqlx::SqlitePool,
    owner_kind: Option<&str>,
    owner_id: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<ToolRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        row_columns!(),
        " FROM loadout_tools
          WHERE (?1 IS NULL OR owner_kind = ?1)
            AND (?2 IS NULL OR owner_id = ?2)
            AND (?3 IS NULL OR status = ?3)
          ORDER BY id"
    ))
    .bind(owner_kind)
    .bind(owner_id)
    .bind(status)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested {
    pub id: i64,
    pub status: String,
    pub created: bool,
}

#[derive(Debug)]
pub enum RequestError {
    NotANucleosTool,
    NoBox,
    AlreadyHeld,
    OutsideBox,
    NoAgent,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for RequestError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotANucleosTool => f.write_str(
                "That is not the name of a NucleOS tool; ask for one by its bare name, for \
                 example web_read. Built-in CLI tools cannot be requested.",
            ),
            Self::NoBox => f.write_str(
                "This run has no tool box that can be extended, so there is nothing to ask for; \
                 carry on with the tools you have.",
            ),
            Self::AlreadyHeld => f.write_str(
                "You already have that tool in this run; call it instead of asking for it.",
            ),
            Self::OutsideBox => f.write_str(
                "No box of this kind can be given that tool; do the work with the tools you \
                 have, or say in your report that it needs someone with more reach.",
            ),
            Self::NoAgent => f.write_str(
                "This run is not tied to an agent, so a request has nobody to be recorded \
                 against; carry on with the tools you have.",
            ),
            Self::Db(_) => f.write_str("The request could not be recorded; try again later."),
        }
    }
}

impl std::error::Error for RequestError {}

/// The request path for a row that already exists: a proposal keeps its place with a fresh reason,
/// an active row stays as it is, a rejected or revoked one goes back to proposed.
async fn reuse_existing(
    conn: &mut SqliteConnection,
    id: i64,
    status: String,
    reason: &str,
    run_text: &str,
) -> Result<Requested, sqlx::Error> {
    if status == "proposed" {
        sqlx::query("UPDATE loadout_tools SET reason = ?, run_id = ? WHERE id = ?")
            .bind(reason)
            .bind(run_text)
            .bind(id)
            .execute(&mut *conn)
            .await?;
        return Ok(Requested {
            id,
            status,
            created: false,
        });
    }
    if status == "active" {
        return Ok(Requested {
            id,
            status,
            created: false,
        });
    }
    sqlx::query(
        "UPDATE loadout_tools
            SET status = 'proposed', reason = ?, run_id = ?, decided_at = NULL
          WHERE id = ?",
    )
    .bind(reason)
    .bind(run_text)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    event(conn, id, Some(&status), "proposed", None).await?;
    Ok(Requested {
        id,
        status: "proposed".to_owned(),
        created: false,
    })
}

/// A run asks for one more tool. Nothing changes for the run itself: the row is a proposal the
/// owner decides on, and a later run of the same agent may hold the tool if the answer is yes.
pub async fn request(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    tool: &str,
    reason: &str,
) -> Result<Requested, RequestError> {
    let tool = tool.strip_prefix(TOOL_PREFIX).unwrap_or(tool);
    if !mcp_tools::is_nucleos_tool(tool) {
        return Err(RequestError::NotANucleosTool);
    }
    let served = caller_box(pool, run_id).await?.ok_or(RequestError::NoBox)?;
    let (base, extras) = mcp_tools::box_lists(&served).ok_or(RequestError::NoBox)?;
    match mcp_tools::box_admission(base, extras, tool) {
        Admission::Base => return Err(RequestError::AlreadyHeld),
        Admission::Outside => return Err(RequestError::OutsideBox),
        Admission::Extra => {}
    }
    let owner: Option<Option<String>> =
        sqlx::query_scalar("SELECT agent_id FROM run_loadout WHERE run_id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
    let owner = owner.flatten().ok_or(RequestError::NoAgent)?;

    let mut tx = pool.begin().await?;
    let existing: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, status FROM loadout_tools
          WHERE owner_kind = 'agent' AND owner_id = ? AND tool = ?",
    )
    .bind(&owner)
    .bind(tool)
    .fetch_optional(&mut *tx)
    .await?;
    let run_text = run_id.to_string();
    let outcome = match existing {
        None => {
            let inserted = sqlx::query(
                "INSERT INTO loadout_tools
                     (owner_kind, owner_id, tool, status, source, reason, run_id, created_at)
                 VALUES ('agent', ?, ?, 'proposed', 'request', ?, ?, ?)
                 ON CONFLICT (owner_kind, owner_id, tool) DO NOTHING",
            )
            .bind(&owner)
            .bind(tool)
            .bind(reason)
            .bind(&run_text)
            .bind(now())
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() == 1 {
                let id = inserted.last_insert_rowid();
                event(&mut tx, id, None, "proposed", None).await?;
                Requested {
                    id,
                    status: "proposed".to_owned(),
                    created: true,
                }
            } else {
                // A concurrent request inserted the row between the read and the write: continue
                // down the existing-row path with what is there now.
                let (id, status): (i64, String) = sqlx::query_as(
                    "SELECT id, status FROM loadout_tools
                      WHERE owner_kind = 'agent' AND owner_id = ? AND tool = ?",
                )
                .bind(&owner)
                .bind(tool)
                .fetch_one(&mut *tx)
                .await?;
                reuse_existing(&mut tx, id, status, reason, &run_text).await?
            }
        }
        Some((id, status)) => reuse_existing(&mut tx, id, status, reason, &run_text).await?,
    };
    tx.commit().await?;
    Ok(outcome)
}

/// Who an approval is for: the proposing agent alone, or the whole team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Agent,
    Team(String),
}

#[derive(Debug)]
pub enum DecisionError {
    NotFound,
    WrongState,
    UnknownTeam,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for DecisionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such tool row"),
            Self::WrongState => f.write_str("the tool row is not in a state that allows this"),
            Self::UnknownTeam => f.write_str("no such team"),
            Self::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

impl std::error::Error for DecisionError {}

/// Approves a proposal for the agent, or moves it to a team. When the team already holds a row
/// for the tool, that row takes the approval and the proposal is merged into it and removed; its
/// events stay, ending in a `merged` one.
pub async fn approve(
    pool: &sqlx::SqlitePool,
    id: i64,
    target: Target,
) -> Result<ToolRow, DecisionError> {
    let mut tx = pool.begin().await?;
    let source = row_by_id(&mut tx, id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if source.status != "proposed" {
        return Err(DecisionError::WrongState);
    }
    let at = now();
    let result_id = match target {
        Target::Agent => {
            if source.owner_kind != "agent" {
                return Err(DecisionError::WrongState);
            }
            sqlx::query("UPDATE loadout_tools SET status = 'active', decided_at = ? WHERE id = ?")
                .bind(&at)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            event(&mut tx, id, Some("proposed"), "active", None).await?;
            id
        }
        Target::Team(team) => {
            let known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM teams WHERE id = ?)")
                    .bind(&team)
                    .fetch_one(&mut *tx)
                    .await?;
            if !known {
                return Err(DecisionError::UnknownTeam);
            }
            let dest: Option<(i64, String)> = sqlx::query_as(
                "SELECT id, status FROM loadout_tools
                  WHERE owner_kind = 'team' AND owner_id = ? AND tool = ? AND id != ?",
            )
            .bind(&team)
            .bind(&source.tool)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            match dest {
                Some((dest_id, dest_status)) => {
                    sqlx::query("DELETE FROM loadout_tools WHERE id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query(
                        "UPDATE loadout_tools
                            SET status = 'active', decided_at = ?, reason = ?
                          WHERE id = ?",
                    )
                    .bind(&at)
                    .bind(&source.reason)
                    .bind(dest_id)
                    .execute(&mut *tx)
                    .await?;
                    let note = format!(
                        "merged from #{id} {}:{}",
                        source.owner_kind, source.owner_id
                    );
                    event(&mut tx, dest_id, Some(&dest_status), "active", Some(&note)).await?;
                    event(&mut tx, id, Some("proposed"), "merged", Some(&note)).await?;
                    dest_id
                }
                None => {
                    sqlx::query(
                        "UPDATE loadout_tools
                            SET owner_kind = 'team', owner_id = ?, status = 'active', decided_at = ?
                          WHERE id = ?",
                    )
                    .bind(&team)
                    .bind(&at)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                    event(&mut tx, id, Some("proposed"), "active", None).await?;
                    id
                }
            }
        }
    };
    let row = row_by_id(&mut tx, result_id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    tx.commit().await?;
    Ok(row)
}

async fn transition(
    pool: &sqlx::SqlitePool,
    id: i64,
    from: &str,
    to: &str,
    note: Option<&str>,
) -> Result<ToolRow, DecisionError> {
    let mut tx = pool.begin().await?;
    let row = row_by_id(&mut tx, id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if row.status != from {
        return Err(DecisionError::WrongState);
    }
    sqlx::query("UPDATE loadout_tools SET status = ?, decided_at = ? WHERE id = ?")
        .bind(to)
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    event(&mut tx, id, Some(from), to, note).await?;
    let row = row_by_id(&mut tx, id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    tx.commit().await?;
    Ok(row)
}

pub async fn reject(
    pool: &sqlx::SqlitePool,
    id: i64,
    note: Option<&str>,
) -> Result<ToolRow, DecisionError> {
    transition(pool, id, "proposed", "rejected", note).await
}

pub async fn revoke(
    pool: &sqlx::SqlitePool,
    id: i64,
    note: Option<&str>,
) -> Result<ToolRow, DecisionError> {
    transition(pool, id, "active", "revoked", note).await
}

/// Removes an owner's rows and their events; the caller's transaction decides when it lands.
/// `nucleos_base::agent::delete` carries a copy of these two statements for agents.
pub async fn delete_for_owner(
    conn: &mut SqliteConnection,
    kind: &str,
    id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM loadout_tool_events WHERE tool_row_id IN
           (SELECT id FROM loadout_tools WHERE owner_kind = ? AND owner_id = ?)",
    )
    .bind(kind)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM loadout_tools WHERE owner_kind = ? AND owner_id = ?")
        .bind(kind)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_tools::McpBox;

    async fn agent(pool: &sqlx::SqlitePool, id: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO agents
                 (id, name, speciality, prompt, engine, model, tool_policy, created_at, updated_at)
             VALUES (?, ?, 'plans', 'p', 'claude', NULL, 'mcp_only',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn team(pool: &sqlx::SqlitePool, id: &str) {
        agent(pool, "director").await;
        sqlx::query(
            "INSERT OR IGNORE INTO teams
                 (id, name, mission, director_agent_id, max_rounds, max_parallel, budget_usd,
                  created_at, updated_at)
             VALUES (?, ?, 'sell', 'director', 3, 2, NULL,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A run that belongs to a job node: `runs.job_id` set, and the job behind it.
    async fn node_run(pool: &sqlx::SqlitePool) -> i64 {
        let job = sqlx::query(
            "INSERT INTO jobs (project_id, project_root, prompt, status, max_items, gate_each, review, created_at)
             VALUES ('p', 'C:/work/repo', 'x', 'implementing', 5, 1, 1, '2026-09-27T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, job_id, created_at)
             VALUES ('x', 'running', 'worktree', ?, '2026-01-01T00:00:00Z')",
        )
        .bind(job)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn plain_run(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'worktree', '2026-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn loadout(pool: &sqlx::SqlitePool, run: i64, agent_id: Option<&str>, tools: &str) {
        sqlx::query(
            "INSERT INTO run_loadout (run_id, agent_id, team_id, tools, resolved_at)
             VALUES (?, ?, NULL, ?, '2026-01-01T00:00:00Z')",
        )
        .bind(run)
        .bind(agent_id)
        .bind(tools)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn events_of(pool: &sqlx::SqlitePool, row: i64) -> Vec<(Option<String>, String)> {
        sqlx::query_as(
            "SELECT from_status, to_status FROM loadout_tool_events
              WHERE tool_row_id = ? ORDER BY id",
        )
        .bind(row)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn event_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM loadout_tool_events")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A job-node run of agent `writer`, which has asked for `web_read`. Returns (run, row id).
    async fn proposal(pool: &sqlx::SqlitePool) -> (i64, i64) {
        agent(pool, "writer").await;
        let run = node_run(pool).await;
        loadout(pool, run, Some("writer"), r#"["verify"]"#).await;
        let asked = request(pool, run, "web_read", "need the docs")
            .await
            .unwrap();
        (run, asked.id)
    }

    #[tokio::test]
    async fn loadout_request_refuses_built_ins_and_unknown_names() {
        let pool = crate::testdb::fresh_pool().await;
        agent(&pool, "writer").await;
        let run = node_run(&pool).await;
        loadout(&pool, run, Some("writer"), r#"["verify"]"#).await;
        for name in ["Bash", "mcp__other__x", "no_such_tool"] {
            assert!(
                matches!(
                    request(&pool, run, name, "why").await,
                    Err(RequestError::NotANucleosTool)
                ),
                "{name} must not be requestable"
            );
        }
        assert_eq!(event_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn loadout_request_refuses_base_and_outside_tools() {
        let pool = crate::testdb::fresh_pool().await;
        agent(&pool, "writer").await;
        let run = node_run(&pool).await;
        loadout(&pool, run, Some("writer"), r#"["verify"]"#).await;
        assert!(matches!(
            request(&pool, run, "verify", "why").await,
            Err(RequestError::AlreadyHeld)
        ));
        assert!(matches!(
            request(&pool, run, "create_run", "why").await,
            Err(RequestError::OutsideBox)
        ));
        // The prefix a CLI prepends is stripped before judging.
        assert!(matches!(
            request(&pool, run, "mcp__nucleos__verify", "why").await,
            Err(RequestError::AlreadyHeld)
        ));
    }

    #[tokio::test]
    async fn loadout_request_refuses_a_run_without_box_or_agent() {
        let pool = crate::testdb::fresh_pool().await;
        let plain = plain_run(&pool).await;
        assert!(matches!(
            request(&pool, plain, "web_read", "why").await,
            Err(RequestError::NoBox)
        ));

        let no_row = node_run(&pool).await;
        assert!(matches!(
            request(&pool, no_row, "web_read", "why").await,
            Err(RequestError::NoAgent)
        ));

        let null_agent = node_run(&pool).await;
        loadout(&pool, null_agent, None, "[]").await;
        assert!(matches!(
            request(&pool, null_agent, "web_read", "why").await,
            Err(RequestError::NoAgent)
        ));
    }

    #[tokio::test]
    async fn loadout_request_is_idempotent_and_updates_the_reason() {
        let pool = crate::testdb::fresh_pool().await;
        agent(&pool, "writer").await;
        let run = node_run(&pool).await;
        loadout(&pool, run, Some("writer"), r#"["verify"]"#).await;

        let first = request(&pool, run, "web_read", "first reason")
            .await
            .unwrap();
        assert!(first.created);
        assert_eq!(first.status, "proposed");
        let second = request(&pool, run, "mcp__nucleos__web_read", "second reason")
            .await
            .unwrap();
        assert!(!second.created);
        assert_eq!(second.id, first.id);

        let rows = list(&pool, Some("agent"), Some("writer"), None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason.as_deref(), Some("second reason"));
        assert_eq!(rows[0].source, "request");
        assert_eq!(events_of(&pool, first.id).await.len(), 1);
    }

    #[tokio::test]
    async fn loadout_rejected_or_revoked_goes_back_to_proposed() {
        let pool = crate::testdb::fresh_pool().await;
        let (run, id) = proposal(&pool).await;

        reject(&pool, id, Some("no")).await.unwrap();
        let again = request(&pool, run, "web_read", "reason two").await.unwrap();
        assert_eq!(again.id, id);
        assert_eq!(again.status, "proposed");
        let row = list(&pool, None, None, None).await.unwrap().remove(0);
        assert_eq!(row.status, "proposed");
        assert_eq!(row.reason.as_deref(), Some("reason two"));
        assert!(row.decided_at.is_none());

        approve(&pool, id, Target::Agent).await.unwrap();
        revoke(&pool, id, None).await.unwrap();
        let third = request(&pool, run, "web_read", "reason three")
            .await
            .unwrap();
        assert_eq!(third.status, "proposed");
        let row = list(&pool, None, None, None).await.unwrap().remove(0);
        assert!(row.decided_at.is_none());
        // null->proposed, proposed->rejected, rejected->proposed, proposed->active,
        // active->revoked, revoked->proposed.
        assert_eq!(events_of(&pool, id).await.len(), 6);

        // An active row is left alone: no write, no event.
        approve(&pool, id, Target::Agent).await.unwrap();
        let held = request(&pool, run, "web_read", "reason four")
            .await
            .unwrap();
        assert_eq!(held.status, "active");
        assert_eq!(events_of(&pool, id).await.len(), 7);
    }

    #[tokio::test]
    async fn loadout_approve_for_the_team_moves_the_row_in_place() {
        let pool = crate::testdb::fresh_pool().await;
        team(&pool, "marketing").await;
        let (_, id) = proposal(&pool).await;

        let row = approve(&pool, id, Target::Team("marketing".to_owned()))
            .await
            .unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.owner_kind, "team");
        assert_eq!(row.owner_id, "marketing");
        assert_eq!(row.status, "active");
        let events = events_of(&pool, id).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events.last().unwrap().1, "active");

        assert!(matches!(
            approve(&pool, id, Target::Team("marketing".to_owned())).await,
            Err(DecisionError::WrongState)
        ));
    }

    #[tokio::test]
    async fn loadout_approve_for_the_team_merges_into_the_existing_row() {
        let pool = crate::testdb::fresh_pool().await;
        team(&pool, "marketing").await;
        let dest: i64 = sqlx::query(
            "INSERT INTO loadout_tools (owner_kind, owner_id, tool, status, source, created_at)
             VALUES ('team', 'marketing', 'web_read', 'revoked', 'owner', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let (_, source) = proposal(&pool).await;
        assert_ne!(dest, source);

        let row = approve(&pool, source, Target::Team("marketing".to_owned()))
            .await
            .unwrap();
        assert_eq!(row.id, dest);
        assert_eq!(row.status, "active");

        let rows = list(&pool, None, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, dest);
        assert_eq!(rows[0].status, "active");

        assert_eq!(events_of(&pool, dest).await.last().unwrap().1, "active");
        assert_eq!(events_of(&pool, source).await.last().unwrap().1, "merged");

        assert!(matches!(
            approve(&pool, source, Target::Agent).await,
            Err(DecisionError::NotFound)
        ));
    }

    #[tokio::test]
    async fn loadout_approve_for_the_agent_refuses_a_team_owned_row() {
        let pool = crate::testdb::fresh_pool().await;
        team(&pool, "marketing").await;
        let id: i64 = sqlx::query(
            "INSERT INTO loadout_tools (owner_kind, owner_id, tool, status, source, created_at)
             VALUES ('team', 'marketing', 'web_read', 'proposed', 'owner', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        assert!(matches!(
            approve(&pool, id, Target::Agent).await,
            Err(DecisionError::WrongState)
        ));

        let (owner_kind, status): (String, String) =
            sqlx::query_as("SELECT owner_kind, status FROM loadout_tools WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(owner_kind, "team");
        assert_eq!(status, "proposed");
    }

    #[tokio::test]
    async fn loadout_reject_and_revoke_write_events() {
        let pool = crate::testdb::fresh_pool().await;
        let (_, a) = proposal(&pool).await;
        let rejected = reject(&pool, a, Some("not now")).await.unwrap();
        assert_eq!(rejected.status, "rejected");
        assert!(rejected.decided_at.is_some());
        assert_eq!(
            events_of(&pool, a).await.last().unwrap(),
            &(Some("proposed".to_owned()), "rejected".to_owned())
        );

        agent(&pool, "other").await;
        let run = node_run(&pool).await;
        loadout(&pool, run, Some("other"), r#"["verify"]"#).await;
        let b = request(&pool, run, "web_read", "r").await.unwrap().id;
        assert!(matches!(
            revoke(&pool, b, None).await,
            Err(DecisionError::WrongState)
        ));
        approve(&pool, b, Target::Agent).await.unwrap();
        let revoked = revoke(&pool, b, Some("too risky")).await.unwrap();
        assert_eq!(revoked.status, "revoked");
        assert!(revoked.decided_at.is_some());
        assert_eq!(
            events_of(&pool, b).await.last().unwrap(),
            &(Some("active".to_owned()), "revoked".to_owned())
        );

        assert!(matches!(
            reject(&pool, 9999, None).await,
            Err(DecisionError::NotFound)
        ));
        assert!(matches!(
            revoke(&pool, 9999, None).await,
            Err(DecisionError::NotFound)
        ));
        assert!(matches!(
            reject(&pool, b, None).await,
            Err(DecisionError::WrongState)
        ));
    }

    #[tokio::test]
    async fn loadout_run_lists_tool_fails_closed() {
        let pool = crate::testdb::fresh_pool().await;
        let missing = node_run(&pool).await;
        assert!(!run_lists_tool(&pool, missing, "web_read").await.unwrap());

        let garbled = node_run(&pool).await;
        loadout(&pool, garbled, None, "not json").await;
        assert!(!run_lists_tool(&pool, garbled, "web_read").await.unwrap());

        let listed = node_run(&pool).await;
        loadout(&pool, listed, None, r#"["verify","web_read"]"#).await;
        assert!(run_lists_tool(&pool, listed, "web_read").await.unwrap());
        assert!(!run_lists_tool(&pool, listed, "create_run").await.unwrap());
    }

    /// `caller_box` is exercised here with the `McpBox` it returns, so the import is not dead.
    #[tokio::test]
    async fn loadout_caller_box_follows_the_run() {
        let pool = crate::testdb::fresh_pool().await;
        let plain = plain_run(&pool).await;
        assert!(caller_box(&pool, plain).await.unwrap().is_none());
        let node = node_run(&pool).await;
        assert!(matches!(
            caller_box(&pool, node).await.unwrap(),
            Some(McpBox::JobNode(_))
        ));
    }
}
