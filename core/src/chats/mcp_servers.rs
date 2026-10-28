//! The MCP servers NucleOS itself knows about, and which conversation has which one on
//! (migration 0254). Owns the `mcp_servers` and `chat_mcp_servers` SQL and nothing else.
//!
//! The registry replaces `chats.ambient_mcp` ("whatever the user's own Claude config has"): a
//! server here is the owner's own entry, toggled per chat, and reaches a turn only when the turn's
//! `ToolPolicy` is `Unrestricted` (`active_for`). The fixed `nucleos` server is never a row.
//!
//! `env` VALUES are secrets. `McpServer` holds them and is deliberately not `Serialize`; the
//! only shape that leaves the daemon is `McpServerView`, which carries the names alone.

use std::collections::BTreeMap;

use sqlx::SqlitePool;

use crate::execution::runner::ToolPolicy;

/// The server every run has, which is not in the registry and cannot be switched off.
pub const FIXED_SERVER: &str = "nucleos";

/// §12 Q2: unverified on the installed Codex, so a Codex-imported server cannot be switched off.
///
/// Emitting `-c mcp_servers.<name>.enabled=false` for a key Codex may not know risks refusing
/// every Codex turn at start; leaving it out only costs a line on the card.
pub const CODEX_CAN_DISABLE_USER_SERVERS: bool = false;

/// A registry row, env values included. NOT `Serialize`, and `Debug` is hand-written: `env`
/// holds secrets.
#[derive(Clone, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// `"claude-user"`, `"claude-project"`, `"codex"`, or `None` when added by hand.
    pub imported_from: Option<String>,
    pub created_at: String,
}

impl McpServer {
    /// The names of this server's environment variables, sorted. Never the values.
    pub fn env_names(&self) -> Vec<String> {
        // A BTreeMap iterates in key order, so this is already sorted.
        self.env.keys().cloned().collect()
    }

    /// What a card or an HTTP answer may show of this server.
    pub fn view(&self) -> McpServerView {
        McpServerView {
            name: self.name.clone(),
            command: self.command.clone(),
            env_names: self.env_names(),
            imported_from: self.imported_from.clone(),
        }
    }
}

/// Env names and an arg count only: a `{:?}` in a log or a panic message must never print a secret
/// value, and an arg can carry one (`--token=...`).
impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args_len", &self.args.len())
            .field("env_names", &self.env_names())
            .field("imported_from", &self.imported_from)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// The showable half of an `McpServer`: env names only.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct McpServerView {
    pub name: String,
    pub command: String,
    pub env_names: Vec<String>,
    pub imported_from: Option<String>,
}

/// A server about to be written to the registry.
pub struct NewMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub imported_from: Option<String>,
}

/// Why `set_enabled` refused.
#[derive(Debug)]
pub enum McpToggleError {
    /// `off nucleos`: the fixed server is always on.
    CannotDisableNucleos,
    /// No registry server has that name; `known` is every name it has, sorted.
    UnknownServer {
        known: Vec<String>,
    },
    /// No conversation has that id.
    ChatNotFound,
    Db(sqlx::Error),
}

impl std::fmt::Display for McpToggleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CannotDisableNucleos => write!(f, "the {FIXED_SERVER} server cannot be disabled"),
            Self::UnknownServer { known } if known.is_empty() => {
                write!(f, "unknown MCP server; the registry is empty")
            }
            Self::UnknownServer { known } => {
                write!(f, "unknown MCP server; known: {}", known.join(", "))
            }
            Self::ChatNotFound => write!(f, "chat not found"),
            Self::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

impl std::error::Error for McpToggleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(error) => Some(error),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for McpToggleError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// The table's CHECK (one or more of `[A-Za-z0-9_-]`, and not `nucleos` in any case), plus two
/// rules the CHECK does not carry: no `__` and no trailing `_`. Either would make the CLI's
/// `mcp__<server>__<tool>` naming ambiguous — with `gh` and `gh_` both registered, the tool
/// `mcp__gh___x` would start with `gh`'s prefix — so `tool_belongs_to` would misattribute tools.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !name.contains("__")
        && !name.ends_with('_')
        && !name.eq_ignore_ascii_case(FIXED_SERVER)
}

/// Whether `tool_name` is one of `server`'s tools, by the CLI's `mcp__<server>__<tool>` naming.
pub fn tool_belongs_to(tool_name: &str, server: &str) -> bool {
    tool_name.starts_with(&format!("mcp__{server}__"))
}

type ServerRow = (String, String, String, String, Option<String>, String);

/// Interpolated into the queries below under `AssertSqlSafe`: safe by construction, since this
/// constant is the only text spliced in and every value is bound.
const COLUMNS: &str = "s.name, s.command, s.args, s.env, s.imported_from, s.created_at";

fn from_row((name, command, args, env, imported_from, created_at): ServerRow) -> McpServer {
    McpServer {
        name,
        command,
        // Lenient, like `extra_dirs`: an unreadable column reads as empty rather than refusing
        // the turn. The table's own writer only ever stores what serde produced.
        args: serde_json::from_str(&args).unwrap_or_default(),
        env: serde_json::from_str(&env).unwrap_or_default(),
        imported_from,
        created_at,
    }
}

/// Every registry server, by name.
pub async fn list(pool: &SqlitePool) -> sqlx::Result<Vec<McpServer>> {
    let rows: Vec<ServerRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM mcp_servers s ORDER BY s.name"
    )))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(from_row).collect())
}

/// The registry server called `name`, if any.
pub async fn get(pool: &SqlitePool, name: &str) -> sqlx::Result<Option<McpServer>> {
    let row: Option<ServerRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM mcp_servers s WHERE s.name = ?"
    )))
    .bind(name)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(from_row))
}

/// Adds `server` unless a server of that name exists; `true` when it was written.
///
/// Never overwrites: an import must not replace an entry the owner already has. `ON CONFLICT DO
/// NOTHING` rather than `INSERT OR IGNORE`, because the latter would also swallow a CHECK failure.
pub async fn insert_if_absent(pool: &SqlitePool, server: &NewMcpServer) -> sqlx::Result<bool> {
    let args = serde_json::to_string(&server.args).unwrap_or_else(|_| "[]".to_string());
    let env = serde_json::to_string(&server.env).unwrap_or_else(|_| "{}".to_string());
    let done = sqlx::query(
        "INSERT INTO mcp_servers (name, command, args, env, imported_from)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(name) DO NOTHING",
    )
    .bind(&server.name)
    .bind(&server.command)
    .bind(args)
    .bind(env)
    .bind(&server.imported_from)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Removes the server `name` and, by cascade, every chat's toggle of it; `true` when it existed.
pub async fn delete(pool: &SqlitePool, name: &str) -> sqlx::Result<bool> {
    let done = sqlx::query("DELETE FROM mcp_servers WHERE name = ?")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// The registry servers `chat_id` has enabled, by name.
pub async fn enabled_for_chat(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Vec<McpServer>> {
    let rows: Vec<ServerRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM chat_mcp_servers c JOIN mcp_servers s ON s.name = c.name
          WHERE c.chat_id = ? ORDER BY s.name"
    )))
    .bind(chat_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(from_row).collect())
}

/// Turns the registry server `name` on or off for `chat_id`. Idempotent both ways.
///
/// `on nucleos` is `Ok(())` (it is always on); `off nucleos` is `CannotDisableNucleos`.
pub async fn set_enabled(
    pool: &SqlitePool,
    chat_id: &str,
    name: &str,
    on: bool,
) -> Result<(), McpToggleError> {
    if name.eq_ignore_ascii_case(FIXED_SERVER) {
        return if on {
            Ok(())
        } else {
            Err(McpToggleError::CannotDisableNucleos)
        };
    }
    let chat_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chats WHERE chat_id = ?)")
            .bind(chat_id)
            .fetch_one(pool)
            .await?;
    if !chat_exists {
        return Err(McpToggleError::ChatNotFound);
    }
    if get(pool, name).await?.is_none() {
        let known = list(pool).await?.into_iter().map(|s| s.name).collect();
        return Err(McpToggleError::UnknownServer { known });
    }
    let sql = if on {
        "INSERT INTO chat_mcp_servers (chat_id, name) VALUES (?, ?)
         ON CONFLICT(chat_id, name) DO NOTHING"
    } else {
        "DELETE FROM chat_mcp_servers WHERE chat_id = ? AND name = ?"
    };
    sqlx::query(sql)
        .bind(chat_id)
        .bind(name)
        .execute(pool)
        .await?;
    Ok(())
}

/// The names of the servers enabled for the chat the run `run_id` belongs to, sorted.
///
/// Read by the hook at call time. A missing run, a run with no chat or a failed read all answer
/// empty, which is the fail-closed direction.
pub async fn enabled_names_for_run(pool: &SqlitePool, run_id: i64) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT c.name FROM runs r JOIN chat_mcp_servers c ON c.chat_id = r.chat_id
          WHERE r.id = ? ORDER BY c.name",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

/// Whether this chat had opted in to the user's ambient MCP servers before the registry existed.
///
/// Card hint only ("this chat used Claude's servers; use Import"): `chats.ambient_mcp` no longer
/// changes what a turn gets. Off when unknown or unreadable.
pub async fn legacy_ambient_mcp(pool: &SqlitePool, chat_id: &str) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT ambient_mcp FROM chats WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// The enabled servers a turn under `policy` actually gets: all of them under `Unrestricted`,
/// none otherwise (`McpOnly` means the NucleOS server alone).
pub fn active_for(policy: ToolPolicy, enabled: &[McpServer]) -> &[McpServer] {
    match policy {
        ToolPolicy::Unrestricted => enabled,
        ToolPolicy::McpOnly | ToolPolicy::None => &[],
    }
}

/// The names of `active_for(policy, enabled)`, in the order given (sorted when `enabled` is).
pub fn active_names(policy: ToolPolicy, enabled: &[McpServer]) -> Vec<String> {
    active_for(policy, enabled)
        .iter()
        .map(|s| s.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::{self, Brain};

    async fn chat(pool: &SqlitePool) -> String {
        chats::create(pool, Brain::Cloud, None).await.unwrap()
    }

    fn server(name: &str) -> NewMcpServer {
        NewMcpServer {
            name: name.to_string(),
            command: format!("{name}-cmd"),
            args: vec!["--stdio".to_string()],
            env: BTreeMap::new(),
            imported_from: None,
        }
    }

    #[test]
    fn debug_prints_env_names_not_values() {
        let row = McpServer {
            name: "gh".to_string(),
            command: "gh-mcp".to_string(),
            args: vec!["--stdio".to_string()],
            env: BTreeMap::from([("GITHUB_TOKEN".to_string(), "ghp_s3cr3t_value".to_string())]),
            imported_from: Some("claude-user".to_string()),
            created_at: "2026-10-28T10:00:00+00:00".to_string(),
        };
        let printed = format!("{row:?}");
        assert!(printed.contains("GITHUB_TOKEN"), "{printed}");
        assert!(printed.contains("gh-mcp"), "{printed}");
        assert!(!printed.contains("ghp_s3cr3t_value"), "{printed}");
        let pretty = format!("{row:#?}");
        assert!(!pretty.contains("ghp_s3cr3t_value"), "{pretty}");

        let with_secret_arg = McpServer {
            args: vec!["--token=ghp_arg_s3cr3t".to_string()],
            ..row
        };
        let printed = format!("{with_secret_arg:?}");
        assert!(!printed.contains("ghp_arg_s3cr3t"), "{printed}");
        assert!(printed.contains("args_len: 1"), "{printed}");
    }

    #[test]
    fn valid_name_refuses_names_that_make_tool_prefixes_ambiguous() {
        for bad in ["gh_", "a__b", "__", "x___y", "_"] {
            assert!(!valid_name(bad), "valid_name accepted {bad:?}");
        }
        for good in ["gh", "gh_x", "a_b-c", "_gh", "gh-"] {
            assert!(valid_name(good), "valid_name refused {good:?}");
        }
    }

    async fn add(pool: &SqlitePool, name: &str) {
        assert!(insert_if_absent(pool, &server(name)).await.unwrap());
    }

    async fn seed_run(pool: &SqlitePool, chat: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('q', 'running', 'assistant', ?, '2026-10-28T10:00:00+00:00')
             RETURNING id",
        )
        .bind(chat)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn toggles(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM chat_mcp_servers")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn names(servers: &[McpServer]) -> Vec<&str> {
        servers.iter().map(|s| s.name.as_str()).collect()
    }

    #[tokio::test]
    async fn migration_check_rejects_bad_names_and_nucleos() {
        let pool = crate::testdb::fresh_pool().await;
        for bad in ["", "a b", "a/b", "nucleos", "NucleOS"] {
            let refused = sqlx::query("INSERT INTO mcp_servers (name, command) VALUES (?, 'x')")
                .bind(bad)
                .execute(&pool)
                .await;
            assert!(refused.is_err(), "the table accepted {bad:?}");
            assert!(!valid_name(bad), "valid_name accepted {bad:?}");
        }
        sqlx::query("INSERT INTO mcp_servers (name, command) VALUES ('gh-1_x', 'x')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(valid_name("gh-1_x"));
        // `insert_if_absent` must surface a CHECK failure, not swallow it.
        assert!(insert_if_absent(&pool, &server("a b")).await.is_err());
    }

    #[tokio::test]
    async fn insert_if_absent_does_not_overwrite() {
        let pool = crate::testdb::fresh_pool().await;
        add(&pool, "gh").await;
        let mut other = server("gh");
        other.command = "other".to_string();
        assert!(!insert_if_absent(&pool, &other).await.unwrap());
        let kept = get(&pool, "gh").await.unwrap().unwrap();
        assert_eq!(kept.command, "gh-cmd");
        assert_eq!(kept.args, vec!["--stdio".to_string()]);
    }

    #[tokio::test]
    async fn list_is_sorted_and_view_has_env_names_not_values() {
        let pool = crate::testdb::fresh_pool().await;
        add(&pool, "zeta").await;
        let mut gh = server("gh");
        gh.env
            .insert("GH_TOKEN".to_string(), "fake-secret-value".to_string());
        gh.env
            .insert("A_FIRST".to_string(), "other-fake-value".to_string());
        gh.imported_from = Some("codex".to_string());
        assert!(insert_if_absent(&pool, &gh).await.unwrap());
        add(&pool, "alpha").await;

        let all = list(&pool).await.unwrap();
        assert_eq!(names(&all), ["alpha", "gh", "zeta"]);
        let found = all.iter().find(|s| s.name == "gh").unwrap();
        assert_eq!(
            found.env.get("GH_TOKEN").map(String::as_str),
            Some("fake-secret-value")
        );
        assert_eq!(found.env_names(), ["A_FIRST", "GH_TOKEN"]);

        let view = found.view();
        assert_eq!(view.imported_from.as_deref(), Some("codex"));
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("GH_TOKEN"), "the view lost the env name");
        assert!(
            !json.contains("fake-secret-value"),
            "the view carries an env value"
        );
        assert!(
            !json.contains("other-fake-value"),
            "the view carries an env value"
        );
    }

    #[tokio::test]
    async fn set_enabled_on_off_and_enabled_for_chat_sorted() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        add(&pool, "zeta").await;
        add(&pool, "gh").await;
        assert!(enabled_for_chat(&pool, &c).await.unwrap().is_empty());

        set_enabled(&pool, &c, "zeta", true).await.unwrap();
        set_enabled(&pool, &c, "gh", true).await.unwrap();
        set_enabled(&pool, &c, "gh", true).await.unwrap(); // idempotent
        assert_eq!(
            names(&enabled_for_chat(&pool, &c).await.unwrap()),
            ["gh", "zeta"]
        );

        set_enabled(&pool, &c, "zeta", false).await.unwrap();
        set_enabled(&pool, &c, "zeta", false).await.unwrap(); // idempotent
        assert_eq!(names(&enabled_for_chat(&pool, &c).await.unwrap()), ["gh"]);

        // Another chat is untouched.
        let other = chat(&pool).await;
        assert!(enabled_for_chat(&pool, &other).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn set_enabled_unknown_name_lists_known() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        add(&pool, "zeta").await;
        add(&pool, "gh").await;
        match set_enabled(&pool, &c, "nope", true).await {
            Err(McpToggleError::UnknownServer { known }) => assert_eq!(known, ["gh", "zeta"]),
            other => panic!("expected UnknownServer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_enabled_off_nucleos_is_refused_and_on_nucleos_is_noop() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        assert!(matches!(
            set_enabled(&pool, &c, "nucleos", false).await,
            Err(McpToggleError::CannotDisableNucleos)
        ));
        set_enabled(&pool, &c, "nucleos", true).await.unwrap();
        assert_eq!(toggles(&pool).await, 0);
        assert!(enabled_for_chat(&pool, &c).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn set_enabled_unknown_chat_is_chat_not_found() {
        let pool = crate::testdb::fresh_pool().await;
        add(&pool, "gh").await;
        assert!(matches!(
            set_enabled(&pool, "no-such-chat", "gh", true).await,
            Err(McpToggleError::ChatNotFound)
        ));
        assert_eq!(toggles(&pool).await, 0);
    }

    #[tokio::test]
    async fn enabled_names_for_run_reads_the_runs_chat_and_fails_closed() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let other = chat(&pool).await;
        add(&pool, "zeta").await;
        add(&pool, "gh").await;
        set_enabled(&pool, &c, "zeta", true).await.unwrap();
        set_enabled(&pool, &c, "gh", true).await.unwrap();
        set_enabled(&pool, &other, "zeta", true).await.unwrap();

        let run = seed_run(&pool, &c).await;
        assert_eq!(enabled_names_for_run(&pool, run).await, ["gh", "zeta"]);
        let other_run = seed_run(&pool, &other).await;
        assert_eq!(enabled_names_for_run(&pool, other_run).await, ["zeta"]);
        assert!(enabled_names_for_run(&pool, 999_999).await.is_empty());
    }

    #[tokio::test]
    async fn deleting_a_server_or_chat_cascades() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let d = chat(&pool).await;
        add(&pool, "gh").await;
        add(&pool, "zeta").await;
        set_enabled(&pool, &c, "gh", true).await.unwrap();
        set_enabled(&pool, &c, "zeta", true).await.unwrap();
        set_enabled(&pool, &d, "zeta", true).await.unwrap();
        assert_eq!(toggles(&pool).await, 3);

        assert!(delete(&pool, "gh").await.unwrap());
        assert!(!delete(&pool, "gh").await.unwrap());
        assert_eq!(toggles(&pool).await, 2);
        assert_eq!(names(&enabled_for_chat(&pool, &c).await.unwrap()), ["zeta"]);

        sqlx::query("DELETE FROM chats WHERE chat_id = ?")
            .bind(&c)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(toggles(&pool).await, 1);
        assert_eq!(names(&enabled_for_chat(&pool, &d).await.unwrap()), ["zeta"]);
    }

    #[test]
    fn active_for_only_under_unrestricted() {
        let enabled: Vec<McpServer> = ["gh", "zeta"]
            .into_iter()
            .map(|name| McpServer {
                name: name.to_string(),
                command: "x".to_string(),
                args: Vec::new(),
                env: BTreeMap::new(),
                imported_from: None,
                created_at: String::new(),
            })
            .collect();
        assert_eq!(
            names(active_for(ToolPolicy::Unrestricted, &enabled)),
            ["gh", "zeta"]
        );
        assert_eq!(
            active_names(ToolPolicy::Unrestricted, &enabled),
            ["gh", "zeta"]
        );
        assert!(active_for(ToolPolicy::McpOnly, &enabled).is_empty());
        assert!(active_for(ToolPolicy::None, &enabled).is_empty());
        assert!(active_names(ToolPolicy::McpOnly, &enabled).is_empty());
        assert!(active_names(ToolPolicy::None, &enabled).is_empty());
    }

    #[tokio::test]
    async fn legacy_ambient_mcp_reads_the_old_column() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        assert!(!legacy_ambient_mcp(&pool, &c).await);
        sqlx::query("UPDATE chats SET ambient_mcp = 1 WHERE chat_id = ?")
            .bind(&c)
            .execute(&pool)
            .await
            .unwrap();
        assert!(legacy_ambient_mcp(&pool, &c).await);
        assert!(!legacy_ambient_mcp(&pool, "no-such-chat").await);
    }

    #[test]
    fn tool_belongs_to_matches_the_server_prefix() {
        assert!(tool_belongs_to("mcp__gh__x", "gh"));
        assert!(!tool_belongs_to("mcp__ghx__y", "gh"));
        assert!(!tool_belongs_to("mcp__nucleos__x", "gh"));
        assert!(!tool_belongs_to("Bash", "gh"));
    }
}
