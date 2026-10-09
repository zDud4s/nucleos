//! Context files an agent or a team carries (equipamento §4.3).
//!
//! An owner (an agent or a team) can be given a short list of paths — a file or a directory — that
//! its runs may read through the daemon-side `read_context` tool. This module owns the
//! `context_refs` table, the resolution of a path against the roots the owner may reference, the
//! owner CRUD routes, and `read_context` itself, which B1 exposes as an MCP tool.
//!
//! # The rule: lexical first, then `resolve_within`
//!
//! A path is checked in two steps and never in one. First **lexically**: it must be absolute and
//! start with one of the allowed roots, component by component, so a path that names no root is
//! refused without touching the disk. Only then is the remainder handed to
//! `files::resolve_within`, which is the one place that knows about `..`, unsafe names and a
//! symlink or junction that leaves the root. Doing it the other way round would let the
//! filesystem decide what counts as "inside", and `read_context` re-runs the same two steps on
//! every read, because a ref saved while `root/docs` was a directory says nothing about what
//! `root/docs` is today.
//!
//! # Which roots
//!
//! Every owner may reference the managed files root. Only an **agent** may also reference a
//! project root (`autopilot_state.project_root`): a team is a department that works through its
//! members, and a team-level context file reaching into a repository would hand every member,
//! whatever its own grants, a view of a project the owner never attached to it. A team therefore
//! gets the managed root alone.

use std::path::{Component, Path, PathBuf};

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;

use crate::files::PathError;
use crate::state::AppState;

/// The most `read_context` returns from one file. Past it the read is refused, not truncated: half
/// a document handed to a model as if it were the whole is worse than an error it can report.
pub const MAX_CONTEXT_READ_BYTES: u64 = 1024 * 1024;

/// The name the tool carries on the MCP surface.
pub const READ_CONTEXT_TOOL: &str = "read_context";

/// B1 wires this into mcp_tools' effect table: the content is a third party's.
pub const READ_CONTEXT_EFFECT: crate::mcp_tools::ToolEffect =
    crate::mcp_tools::ToolEffect::ReadsUntrusted;

/// Who carries a context ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    Agent,
    Team,
}

impl OwnerKind {
    /// The wire and column spelling: `agent` or `team`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Team => "team",
        }
    }

    /// The inverse of `as_str`; anything else is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "team" => Some(Self::Team),
            _ => None,
        }
    }
}

/// What a resolved path is on disk right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathState {
    File,
    Dir,
    Missing,
}

/// A path that passed both steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The allowed root the path sits under.
    pub root: PathBuf,
    /// The path relative to `root`.
    pub relative: PathBuf,
    /// The path `files::resolve_within` returned.
    pub target: PathBuf,
    pub state: PathState,
}

/// Why a path was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The path is relative.
    NotAbsolute,
    /// The path sits under none of the allowed roots.
    OutsideRoots,
    /// `..`, or a symlink/junction that leaves the root.
    Escapes,
    /// A name `resolve_within` refuses (reserved names, stray separators).
    Unsafe,
}

/// Resolve `path` against `roots`: lexically first, then through `files::resolve_within`.
pub fn resolve(roots: &[PathBuf], path: &Path) -> Result<Resolved, Refusal> {
    if !path.is_absolute() {
        return Err(Refusal::NotAbsolute);
    }
    // `..` is refused outright, before any root is looked at: `strip_prefix` is lexical and would
    // happily keep `root/../elsewhere` as "inside root". `components()` alone is not enough: in a
    // verbatim `\\?\` path `/` is not a separator, so `tmp/../elsewhere.txt` is a single Normal
    // component and no ParentDir is seen. The text is therefore also split on both `/` and `\`.
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
        || path
            .to_string_lossy()
            .split(['/', '\\'])
            .any(|segment| segment == "..")
    {
        return Err(Refusal::Escapes);
    }

    for root in roots {
        for spelling in root_spellings(root) {
            let Ok(relative) = path.strip_prefix(&spelling) else {
                continue;
            };
            let relative = relative.to_str().ok_or(Refusal::Unsafe)?;
            let target =
                crate::files::resolve_within(root, relative).map_err(|error| match error {
                    PathError::Unsafe => Refusal::Unsafe,
                    _ => Refusal::Escapes,
                })?;
            let state = match std::fs::metadata(&target) {
                Ok(metadata) if metadata.is_dir() => PathState::Dir,
                Ok(_) => PathState::File,
                Err(_) => PathState::Missing,
            };
            return Ok(Resolved {
                root: root.clone(),
                relative: PathBuf::from(relative),
                target,
                state,
            });
        }
    }
    Err(Refusal::OutsideRoots)
}

/// The spellings a caller may use for `root`. On Windows `canonicalize` yields a `\\?\`-prefixed
/// path while a person (or a tool) writes the plain drive form, and the two differ in their first
/// component; a UNC root (`\\?\UNC\...`) has no plain twin and is kept as it is.
#[cfg(windows)]
fn root_spellings(root: &Path) -> Vec<PathBuf> {
    let mut spellings = vec![root.to_path_buf()];
    if let Some(text) = root.to_str()
        && let Some(plain) = text.strip_prefix(r"\\?\")
        && !text.starts_with(r"\\?\UNC\")
    {
        spellings.push(PathBuf::from(plain));
    }
    spellings
}

#[cfg(not(windows))]
fn root_spellings(root: &Path) -> Vec<PathBuf> {
    vec![root.to_path_buf()]
}

/// The roots `owner` may reference: the managed root, plus — for an agent only — every project
/// root recorded in `autopilot_state`.
pub async fn allowed_roots(
    pool: &sqlx::SqlitePool,
    managed_root: &Path,
    owner: OwnerKind,
) -> Result<Vec<PathBuf>, sqlx::Error> {
    let mut roots = vec![managed_root.to_path_buf()];
    if owner == OwnerKind::Agent {
        let recorded: Vec<String> = sqlx::query_scalar(
            "SELECT project_root FROM autopilot_state WHERE project_root IS NOT NULL",
        )
        .fetch_all(pool)
        .await?;
        for project_root in recorded {
            // A project root that no longer exists grants nothing.
            if let Ok(canonical) = std::fs::canonicalize(&project_root)
                && !roots.contains(&canonical)
            {
                roots.push(canonical);
            }
        }
    }
    Ok(roots)
}

/// One row of `context_refs`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct ContextRef {
    pub id: i64,
    pub owner_kind: String,
    pub owner_id: String,
    pub path: String,
    /// `file` or `dir`, read from disk when the ref was created.
    pub kind: String,
    pub note: Option<String>,
    pub created_at: String,
}

/// Why a CRUD operation did not happen.
#[derive(Debug)]
pub enum RefError {
    /// The agent or team does not exist.
    OwnerNotFound,
    /// The ref does not exist, or belongs to another owner.
    NotFound,
    /// The owner already carries this path.
    Duplicate,
    Refused(Refusal),
    /// The path resolved but nothing is there.
    Missing,
    Db(sqlx::Error),
}

/// Every ref of an owner, oldest first.
pub async fn list(
    pool: &sqlx::SqlitePool,
    owner: OwnerKind,
    owner_id: &str,
) -> Result<Vec<ContextRef>, sqlx::Error> {
    sqlx::query_as::<_, ContextRef>(
        "SELECT id, owner_kind, owner_id, path, kind, note, created_at
         FROM context_refs WHERE owner_kind = ? AND owner_id = ? ORDER BY id",
    )
    .bind(owner.as_str())
    .bind(owner_id)
    .fetch_all(pool)
    .await
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
}

/// Record a ref. The kind comes from disk, never from the caller.
pub async fn create(
    pool: &sqlx::SqlitePool,
    managed_root: &Path,
    owner: OwnerKind,
    owner_id: &str,
    path: &str,
    note: Option<&str>,
) -> Result<ContextRef, RefError> {
    let exists_sql = match owner {
        OwnerKind::Agent => "SELECT EXISTS(SELECT 1 FROM agents WHERE id = ?)",
        OwnerKind::Team => "SELECT EXISTS(SELECT 1 FROM teams WHERE id = ?)",
    };
    let exists: i64 = sqlx::query_scalar(exists_sql)
        .bind(owner_id)
        .fetch_one(pool)
        .await
        .map_err(RefError::Db)?;
    if exists == 0 {
        return Err(RefError::OwnerNotFound);
    }

    let roots = allowed_roots(pool, managed_root, owner)
        .await
        .map_err(RefError::Db)?;
    let resolved = resolve(&roots, Path::new(path)).map_err(RefError::Refused)?;
    let kind = match resolved.state {
        PathState::File => "file",
        PathState::Dir => "dir",
        PathState::Missing => return Err(RefError::Missing),
    };

    sqlx::query_as::<_, ContextRef>(
        "INSERT INTO context_refs (owner_kind, owner_id, path, kind, note, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         RETURNING id, owner_kind, owner_id, path, kind, note, created_at",
    )
    .bind(owner.as_str())
    .bind(owner_id)
    .bind(path)
    .bind(kind)
    .bind(note)
    .bind(chrono::Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            RefError::Duplicate
        } else {
            RefError::Db(error)
        }
    })
}

/// Replace a ref's note.
pub async fn update_note(
    pool: &sqlx::SqlitePool,
    owner: OwnerKind,
    owner_id: &str,
    id: i64,
    note: Option<&str>,
) -> Result<ContextRef, RefError> {
    sqlx::query_as::<_, ContextRef>(
        "UPDATE context_refs SET note = ?
         WHERE id = ? AND owner_kind = ? AND owner_id = ?
         RETURNING id, owner_kind, owner_id, path, kind, note, created_at",
    )
    .bind(note)
    .bind(id)
    .bind(owner.as_str())
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(RefError::Db)?
    .ok_or(RefError::NotFound)
}

/// Remove a ref.
pub async fn delete(
    pool: &sqlx::SqlitePool,
    owner: OwnerKind,
    owner_id: &str,
    id: i64,
) -> Result<(), RefError> {
    let removed =
        sqlx::query("DELETE FROM context_refs WHERE id = ? AND owner_kind = ? AND owner_id = ?")
            .bind(id)
            .bind(owner.as_str())
            .bind(owner_id)
            .execute(pool)
            .await
            .map_err(RefError::Db)?
            .rows_affected();
    if removed == 0 {
        return Err(RefError::NotFound);
    }
    Ok(())
}

/// The `(agent_id, team_id)` a run was launched with, from `run_loadout`. `(None, None)` when the
/// run has no loadout row — or when the table does not exist yet.
pub async fn loadout_owner(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> Result<(Option<String>, Option<String>), sqlx::Error> {
    // The table belongs to another change and may not be in this schema yet: asking sqlite_master
    // first keeps "no table" distinct from a real database failure.
    let present: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'run_loadout')",
    )
    .fetch_one(pool)
    .await?;
    if present == 0 {
        return Ok((None, None));
    }
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT agent_id, team_id FROM run_loadout WHERE run_id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.unwrap_or((None, None)))
}

/// Why `read_context` returned no text.
#[derive(Debug)]
pub enum ReadError {
    /// The run has no loadout owner.
    NoOwner,
    /// The path is not covered by any of the owner's refs.
    NotARef,
    /// The path no longer resolves inside an allowed root.
    Refused(Refusal),
    Missing,
    NotAFile,
    TooLarge {
        bytes: u64,
    },
    Io(String),
    Db(sqlx::Error),
}

/// Read a file `run_id`'s owner carries a ref to (the file itself, or a file under a dir ref),
/// re-resolving it first.
pub async fn read_context(
    pool: &sqlx::SqlitePool,
    managed_root: &Path,
    run_id: i64,
    path: &str,
) -> Result<String, ReadError> {
    let (agent_id, team_id) = loadout_owner(pool, run_id).await.map_err(ReadError::Db)?;
    let owners: Vec<(OwnerKind, String)> = [
        agent_id.map(|id| (OwnerKind::Agent, id)),
        team_id.map(|id| (OwnerKind::Team, id)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if owners.is_empty() {
        return Err(ReadError::NoOwner);
    }

    let mut refusal: Option<Refusal> = None;
    for (owner, owner_id) in &owners {
        let roots = allowed_roots(pool, managed_root, *owner)
            .await
            .map_err(ReadError::Db)?;
        let refs = list(pool, *owner, owner_id).await.map_err(ReadError::Db)?;

        // The requested path is re-resolved on every read: what a ref named when it was saved is
        // not evidence of what the path is now.
        let requested = match resolve(&roots, Path::new(path)) {
            Ok(requested) => requested,
            Err(why) => {
                refusal.get_or_insert(why);
                continue;
            }
        };

        let covered = refs.iter().any(|carried| {
            // A ref that no longer resolves grants nothing.
            let Ok(carried_path) = resolve(&roots, Path::new(&carried.path)) else {
                return false;
            };
            if carried_path.root != requested.root {
                return false;
            }
            match carried.kind.as_str() {
                "file" => carried_path.relative == requested.relative,
                "dir" => requested.relative.starts_with(&carried_path.relative),
                _ => false,
            }
        });
        if !covered {
            continue;
        }

        return match requested.state {
            PathState::Missing => Err(ReadError::Missing),
            PathState::Dir => Err(ReadError::NotAFile),
            PathState::File => {
                let length = std::fs::metadata(&requested.target)
                    .map_err(|error| ReadError::Io(error.to_string()))?
                    .len();
                if length > MAX_CONTEXT_READ_BYTES {
                    return Err(ReadError::TooLarge { bytes: length });
                }
                tokio::fs::read_to_string(&requested.target)
                    .await
                    .map_err(|error| ReadError::Io(error.to_string()))
            }
        };
    }

    Err(refusal.map_or(ReadError::NotARef, ReadError::Refused))
}

#[derive(serde::Deserialize)]
pub struct CreateRefRequest {
    pub path: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct NoteRequest {
    #[serde(default)]
    pub note: Option<String>,
}

fn parse_owner(owner_kind: &str) -> Result<OwnerKind, (StatusCode, String)> {
    OwnerKind::parse(owner_kind).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "the owner kind must be `agent` or `team`".to_owned(),
        )
    })
}

fn ref_error_response(error: RefError) -> (StatusCode, String) {
    match error {
        RefError::OwnerNotFound => (StatusCode::NOT_FOUND, "no such agent or team".to_owned()),
        RefError::NotFound => (StatusCode::NOT_FOUND, "no such context file".to_owned()),
        RefError::Duplicate => (
            StatusCode::CONFLICT,
            "this owner already carries that path".to_owned(),
        ),
        RefError::Refused(why) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            match why {
                Refusal::NotAbsolute => "the path must be absolute",
                Refusal::OutsideRoots => "the path is outside the folders this owner may use",
                Refusal::Escapes => "the path leaves the folder it sits in",
                Refusal::Unsafe => "the path has a name that cannot be used",
            }
            .to_owned(),
        ),
        RefError::Missing => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "nothing exists at that path".to_owned(),
        ),
        RefError::Db(error) => {
            tracing::warn!(%error, "a context file operation failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal".to_owned())
        }
    }
}

pub async fn list_refs(
    State(state): State<AppState>,
    UrlPath((owner_kind, owner_id)): UrlPath<(String, String)>,
) -> Result<Json<Vec<ContextRef>>, (StatusCode, String)> {
    let owner = parse_owner(&owner_kind)?;
    list(&state.pool, owner, &owner_id)
        .await
        .map(Json)
        .map_err(|error| ref_error_response(RefError::Db(error)))
}

pub async fn create_ref(
    State(state): State<AppState>,
    UrlPath((owner_kind, owner_id)): UrlPath<(String, String)>,
    Json(request): Json<CreateRefRequest>,
) -> Result<(StatusCode, Json<ContextRef>), (StatusCode, String)> {
    let owner = parse_owner(&owner_kind)?;
    let managed_root = crate::door::files_root(&state)
        .map_err(|status| (status, "the files folder is not available".to_owned()))?;
    create(
        &state.pool,
        managed_root,
        owner,
        &owner_id,
        &request.path,
        request.note.as_deref(),
    )
    .await
    .map(|created| (StatusCode::CREATED, Json(created)))
    .map_err(ref_error_response)
}

pub async fn update_ref_note(
    State(state): State<AppState>,
    UrlPath((owner_kind, owner_id, id)): UrlPath<(String, String, i64)>,
    Json(request): Json<NoteRequest>,
) -> Result<Json<ContextRef>, (StatusCode, String)> {
    let owner = parse_owner(&owner_kind)?;
    update_note(&state.pool, owner, &owner_id, id, request.note.as_deref())
        .await
        .map(Json)
        .map_err(ref_error_response)
}

pub async fn delete_ref(
    State(state): State<AppState>,
    UrlPath((owner_kind, owner_id, id)): UrlPath<(String, String, i64)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let owner = parse_owner(&owner_kind)?;
    delete(&state.pool, owner, &owner_id, id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ref_error_response)
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, put};
    use tower::ServiceExt;

    use crate::mcp_tools::ToolEffect;

    // -----------------------------------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------------------------------

    /// A canonical temp directory and the `AppState` whose files root it is.
    async fn fixture() -> (tempfile::TempDir, PathBuf, AppState) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let state = crate::team::test_support::state_with(root.clone()).await;
        (tmp, root, state)
    }

    /// A second, unrelated canonical directory.
    fn elsewhere() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(tmp.path()).unwrap();
        (tmp, path)
    }

    fn text(path: &Path) -> &str {
        path.to_str().unwrap()
    }

    /// A directory junction (Windows) or symlink (unix) at `link` pointing at `target`.
    fn link_dir(link: &Path, target: &Path) {
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .status()
                .expect("mklink should start");
            assert!(status.success(), "mklink /J should not need elevation");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("create symlink");
    }

    /// An `outside` directory with a `secret.txt`, and a junction `root/<name>` into it.
    fn junction_out(root: &Path, name: &str) -> (tempfile::TempDir, PathBuf) {
        let (keep, outside) = elsewhere();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        link_dir(&root.join(name), &outside);
        (keep, outside)
    }

    async fn agent(pool: &sqlx::SqlitePool, name: &str) -> String {
        crate::agent::create(
            pool,
            crate::agent::AgentRequest {
                name: name.to_owned(),
                speciality: "writes things".to_owned(),
                prompt: "write".to_owned(),
                engine: "claude".into(),
                model: None,
                tool_policy: "mcp_only".into(),
            },
        )
        .await
        .unwrap()
        .id
    }

    /// A team with a director, through the public surface; returns the team id.
    async fn team(pool: &sqlx::SqlitePool) -> String {
        let director = agent(pool, "Director").await;
        crate::team::create(
            pool,
            crate::team::TeamRequest {
                name: "Marketing".to_owned(),
                mission: "sell the thing".to_owned(),
                director_agent_id: director,
                max_rounds: 3,
                max_parallel: 2,
                budget_usd: None,
                max_open_actions: crate::team::DEFAULT_MAX_OPEN_ACTIONS,
                max_live_runs: 1,
                grants: Vec::new(),
                members: Vec::new(),
            },
        )
        .await
        .unwrap()
        .team
        .id
    }

    async fn create_loadout_table(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS run_loadout (
                 run_id INTEGER PRIMARY KEY REFERENCES runs(id),
                 agent_id TEXT NULL, team_id TEXT NULL, tools TEXT, resolved_at TEXT)",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// A run, and — when `owner` is given — its loadout row.
    async fn run_for(
        pool: &sqlx::SqlitePool,
        agent_id: Option<&str>,
        team_id: Option<&str>,
    ) -> i64 {
        create_loadout_table(pool).await;
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('t', 'running', 'real', '2026-10-08T00:00:00Z') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if agent_id.is_some() || team_id.is_some() {
            sqlx::query(
                "INSERT INTO run_loadout (run_id, agent_id, team_id, tools, resolved_at)
                 VALUES (?, ?, ?, '[]', '2026-10-08T00:00:00Z')",
            )
            .bind(run_id)
            .bind(agent_id)
            .bind(team_id)
            .execute(pool)
            .await
            .unwrap();
        }
        run_id
    }

    fn router(state: AppState) -> Router {
        Router::new()
            .route(
                "/context-refs/{owner_kind}/{owner_id}",
                get(list_refs).post(create_ref),
            )
            .route(
                "/context-refs/{owner_kind}/{owner_id}/{id}",
                put(update_ref_note).delete(delete_ref),
            )
            .with_state(state)
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, json)
    }

    // -----------------------------------------------------------------------------------------
    // The table
    // -----------------------------------------------------------------------------------------

    /// The table is the last line of defence for what `create` already checks: an owner kind that
    /// is neither agent nor team, a kind that is neither file nor dir, and the same path twice for
    /// one owner are all refused by the schema itself.
    #[tokio::test]
    async fn migration_enforces_owner_kind_kind_and_uniqueness() {
        let pool = crate::testdb::fresh_pool().await;
        let insert = |owner_kind: &'static str, path: &'static str, kind: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO context_refs (owner_kind, owner_id, path, kind, created_at)
                     VALUES (?, 'a', ?, ?, '2026-10-08T00:00:00Z')",
                )
                .bind(owner_kind)
                .bind(path)
                .bind(kind)
                .execute(&pool)
                .await
            }
        };

        insert("agent", "/x", "file")
            .await
            .expect("a well-formed row is accepted");
        assert!(
            insert("robot", "/y", "file").await.is_err(),
            "owner_kind outside agent/team must be refused"
        );
        assert!(
            insert("agent", "/z", "symlink").await.is_err(),
            "kind outside file/dir must be refused"
        );
        assert!(
            insert("agent", "/x", "file").await.is_err(),
            "the same path twice for one owner must be refused"
        );
        insert("team", "/x", "file")
            .await
            .expect("the same path for another owner kind is a different ref");
    }

    // -----------------------------------------------------------------------------------------
    // Resolution
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_file_and_a_dir_inside_the_managed_root_resolve_by_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::create_dir(root.join("docs")).unwrap();
        let roots = vec![root.clone()];

        let file = resolve(&roots, &root.join("a.txt")).expect("a file inside the root resolves");
        assert_eq!(file.state, PathState::File);
        assert_eq!(file.root, root);
        assert_eq!(file.relative, PathBuf::from("a.txt"));
        assert_eq!(file.target, root.join("a.txt"));

        let dir = resolve(&roots, &root.join("docs")).expect("a dir inside the root resolves");
        assert_eq!(dir.state, PathState::Dir);
        assert_eq!(dir.relative, PathBuf::from("docs"));
    }

    #[test]
    fn a_missing_path_inside_a_root_resolves_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();

        let resolved = resolve(std::slice::from_ref(&root), &root.join("nothing-here.txt"))
            .expect("a nonexistent path inside the root is not a refusal");
        assert_eq!(resolved.state, PathState::Missing);
    }

    #[test]
    fn a_path_outside_every_root_or_relative_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (_keep, other) = elsewhere();
        std::fs::write(other.join("x.txt"), "x").unwrap();
        let roots = vec![root];

        assert_eq!(
            resolve(&roots, &other.join("x.txt")),
            Err(Refusal::OutsideRoots)
        );
        assert_eq!(
            resolve(&roots, Path::new("docs/a.txt")),
            Err(Refusal::NotAbsolute)
        );
    }

    #[test]
    fn a_parent_component_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let sneaky = PathBuf::from(format!("{}/../elsewhere.txt", root.display()));

        assert_eq!(resolve(&[root], &sneaky), Err(Refusal::Escapes));
    }

    #[test]
    fn a_junction_out_of_the_root_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (_keep, _outside) = junction_out(&root, "link");

        assert_eq!(
            resolve(
                std::slice::from_ref(&root),
                &root.join("link").join("secret.txt")
            ),
            Err(Refusal::Escapes)
        );
    }

    #[tokio::test]
    async fn an_agent_may_reference_a_project_root_and_a_team_may_not() {
        let (_tmp, root, state) = fixture().await;
        let pool = &state.pool;
        let (_keep, project) = elsewhere();
        std::fs::write(project.join("README.md"), "readme").unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('p', 'off', ?)",
        )
        .bind(text(&project))
        .execute(pool)
        .await
        .unwrap();

        let agent_roots = allowed_roots(pool, &root, OwnerKind::Agent).await.unwrap();
        assert!(
            agent_roots.contains(&root),
            "an agent keeps the managed root"
        );
        assert!(
            agent_roots.contains(&project),
            "an agent may also reference a project root"
        );
        let team_roots = allowed_roots(pool, &root, OwnerKind::Team).await.unwrap();
        assert_eq!(
            team_roots,
            vec![root.clone()],
            "a team gets the managed root alone"
        );

        let agent_id = agent(pool, "Writer").await;
        let team_id = team(pool).await;
        let readme = project.join("README.md");
        create(
            pool,
            &root,
            OwnerKind::Agent,
            &agent_id,
            text(&readme),
            None,
        )
        .await
        .expect("an agent may carry a file from a project root");
        assert!(matches!(
            create(pool, &root, OwnerKind::Team, &team_id, text(&readme), None).await,
            Err(RefError::Refused(Refusal::OutsideRoots))
        ));
    }

    // -----------------------------------------------------------------------------------------
    // The owner routes
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn http_create_records_kind_from_disk_and_lists_it() {
        let (_tmp, root, state) = fixture().await;
        let agent_id = agent(&state.pool, "Writer").await;
        std::fs::write(root.join("notes.txt"), "n").unwrap();
        std::fs::create_dir(root.join("docs")).unwrap();
        let app = router(state);
        let uri = format!("/context-refs/agent/{agent_id}");

        let notes = root.join("notes.txt");
        let (status, body) = call(
            &app,
            "POST",
            &uri,
            Some(serde_json::json!({ "path": text(&notes), "note": "the notes" })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["kind"], "file");
        assert_eq!(body["owner_kind"], "agent");
        assert_eq!(body["owner_id"], agent_id.as_str());
        assert_eq!(body["note"], "the notes");

        let docs = root.join("docs");
        let (status, body) = call(
            &app,
            "POST",
            &uri,
            Some(serde_json::json!({ "path": text(&docs) })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["kind"], "dir");

        let (status, listed) = call(&app, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn http_create_refuses_bad_paths_unknown_owners_and_duplicates() {
        let (_tmp, root, state) = fixture().await;
        let agent_id = agent(&state.pool, "Writer").await;
        std::fs::write(root.join("notes.txt"), "n").unwrap();
        let (_keep_out, outside) = elsewhere();
        std::fs::write(outside.join("x.txt"), "x").unwrap();
        let (_keep_link, _target) = junction_out(&root, "link");
        let app = router(state);
        let uri = format!("/context-refs/agent/{agent_id}");

        let escape = root.join("link").join("secret.txt");
        let missing = root.join("nothing-here.txt");
        for path in [outside.join("x.txt"), escape, missing] {
            let (status, _) = call(
                &app,
                "POST",
                &uri,
                Some(serde_json::json!({ "path": text(&path) })),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{} must be refused",
                path.display()
            );
        }

        let notes = root.join("notes.txt");
        let body = serde_json::json!({ "path": text(&notes) });
        let (status, _) = call(
            &app,
            "POST",
            "/context-refs/agent/ghost",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an unknown agent is a 404");
        let (status, _) = call(&app, "POST", "/context-refs/team/ghost", Some(body.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an unknown team is a 404");
        let (status, _) = call(&app, "POST", "/context-refs/bad/anyone", Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a bad owner kind is a 400");

        let (status, _) = call(&app, "POST", &uri, Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(&app, "POST", &uri, Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "the same path twice is a 409");
    }

    #[tokio::test]
    async fn http_update_note_and_delete_round_trip() {
        let (_tmp, root, state) = fixture().await;
        let owner = agent(&state.pool, "Writer").await;
        let stranger = agent(&state.pool, "Reviewer").await;
        std::fs::write(root.join("notes.txt"), "n").unwrap();
        let app = router(state);
        let uri = format!("/context-refs/agent/{owner}");

        let notes = root.join("notes.txt");
        let (_, created) = call(
            &app,
            "POST",
            &uri,
            Some(serde_json::json!({ "path": text(&notes) })),
        )
        .await;
        let id = created["id"].as_i64().unwrap();
        let item = format!("{uri}/{id}");

        let (status, updated) = call(
            &app,
            "PUT",
            &item,
            Some(serde_json::json!({ "note": "read this first" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(updated["note"], "read this first");
        let (_, listed) = call(&app, "GET", &uri, None).await;
        assert_eq!(listed[0]["note"], "read this first");

        let wrong_owner = format!("/context-refs/agent/{stranger}/{id}");
        let (status, _) = call(&app, "DELETE", &wrong_owner, None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "another owner cannot delete it"
        );

        let (status, _) = call(&app, "DELETE", &item, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&app, "DELETE", &item, None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a second delete finds nothing"
        );
        let (_, listed) = call(&app, "GET", &uri, None).await;
        assert!(listed.as_array().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------------------------
    // Whose run is it
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn loadout_owner_without_the_table_is_no_owner() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query("DROP TABLE IF EXISTS run_loadout")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(loadout_owner(&pool, 1).await.unwrap(), (None, None));
    }

    #[tokio::test]
    async fn loadout_owner_reads_the_runs_agent_and_team() {
        let pool = crate::testdb::fresh_pool().await;
        let both = run_for(&pool, Some("writer"), Some("marketing")).await;
        let neither = run_for(&pool, None, None).await;

        assert_eq!(
            loadout_owner(&pool, both).await.unwrap(),
            (Some("writer".to_owned()), Some("marketing".to_owned()))
        );
        assert_eq!(loadout_owner(&pool, neither).await.unwrap(), (None, None));
    }

    // -----------------------------------------------------------------------------------------
    // read_context
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn read_context_reads_an_active_file_ref() {
        let (_tmp, root, state) = fixture().await;
        let pool = &state.pool;
        let agent_id = agent(pool, "Writer").await;
        let notes = root.join("notes.txt");
        std::fs::write(&notes, "hello").unwrap();
        create(pool, &root, OwnerKind::Agent, &agent_id, text(&notes), None)
            .await
            .unwrap();
        let run_id = run_for(pool, Some(&agent_id), None).await;

        let read = read_context(pool, &root, run_id, text(&notes)).await;
        assert_eq!(read.expect("a referenced file reads"), "hello");
    }

    #[tokio::test]
    async fn read_context_reads_a_file_inside_a_dir_ref() {
        let (_tmp, root, state) = fixture().await;
        let pool = &state.pool;
        let agent_id = agent(pool, "Writer").await;
        let docs = root.join("docs");
        std::fs::create_dir_all(docs.join("sub")).unwrap();
        std::fs::write(docs.join("sub").join("a.md"), "inside").unwrap();
        create(pool, &root, OwnerKind::Agent, &agent_id, text(&docs), None)
            .await
            .unwrap();
        let run_id = run_for(pool, Some(&agent_id), None).await;

        let inner = docs.join("sub").join("a.md");
        let read = read_context(pool, &root, run_id, text(&inner)).await;
        assert_eq!(read.expect("a file under a dir ref reads"), "inside");
    }

    #[tokio::test]
    async fn read_context_refuses_a_path_that_is_not_a_ref() {
        let (_tmp, root, state) = fixture().await;
        let pool = &state.pool;
        let agent_id = agent(pool, "Writer").await;
        let carried = root.join("carried.txt");
        let other = root.join("not-carried.txt");
        std::fs::write(&carried, "c").unwrap();
        std::fs::write(&other, "o").unwrap();
        create(
            pool,
            &root,
            OwnerKind::Agent,
            &agent_id,
            text(&carried),
            None,
        )
        .await
        .unwrap();
        let run_id = run_for(pool, Some(&agent_id), None).await;
        let ownerless = run_for(pool, None, None).await;

        assert!(matches!(
            read_context(pool, &root, run_id, text(&other)).await,
            Err(ReadError::NotARef)
        ));
        assert!(matches!(
            read_context(pool, &root, ownerless, text(&carried)).await,
            Err(ReadError::NoOwner)
        ));
    }

    #[tokio::test]
    async fn read_context_refuses_another_agents_ref() {
        let (_tmp, root, state) = fixture().await;
        let pool = &state.pool;
        let owner = agent(pool, "Writer").await;
        let stranger = agent(pool, "Reviewer").await;
        let notes = root.join("notes.txt");
        std::fs::write(&notes, "private").unwrap();
        create(pool, &root, OwnerKind::Agent, &owner, text(&notes), None)
            .await
            .unwrap();
        let strangers_run = run_for(pool, Some(&stranger), None).await;

        assert!(matches!(
            read_context(pool, &root, strangers_run, text(&notes)).await,
            Err(ReadError::NotARef)
        ));
    }

    /// A ref saved while `root/docs` was a directory says nothing about what it is today. The
    /// directory is replaced by a junction out of the root; the read must re-resolve and refuse.
    #[tokio::test]
    async fn read_context_re_resolves_and_refuses_a_ref_replaced_by_a_junction() {
        let (_tmp, root, state) = fixture().await;
        let pool = &state.pool;
        let agent_id = agent(pool, "Writer").await;
        let docs = root.join("docs");
        std::fs::create_dir(&docs).unwrap();
        create(pool, &root, OwnerKind::Agent, &agent_id, text(&docs), None)
            .await
            .unwrap();
        let run_id = run_for(pool, Some(&agent_id), None).await;

        std::fs::remove_dir_all(&docs).unwrap();
        let (_keep, _outside) = junction_out(&root, "docs");

        let secret = docs.join("secret.txt");
        let read = read_context(pool, &root, run_id, text(&secret)).await;
        assert!(
            matches!(read, Err(ReadError::Refused(_))),
            "a ref replaced by a junction must be refused, got {read:?}"
        );
    }

    #[tokio::test]
    async fn read_context_refuses_a_file_over_the_size_limit() {
        let (_tmp, root, state) = fixture().await;
        let pool = &state.pool;
        let agent_id = agent(pool, "Writer").await;
        let big = root.join("big.bin");
        std::fs::write(&big, vec![b'a'; (MAX_CONTEXT_READ_BYTES + 1) as usize]).unwrap();
        create(pool, &root, OwnerKind::Agent, &agent_id, text(&big), None)
            .await
            .unwrap();
        let run_id = run_for(pool, Some(&agent_id), None).await;

        assert!(matches!(
            read_context(pool, &root, run_id, text(&big)).await,
            Err(ReadError::TooLarge { .. })
        ));
    }

    /// What `read_context` returns is a third party's text, so it marks the turn. B1 puts the
    /// constant into the effect table; this pins the value it will put there.
    #[test]
    fn read_context_effect_is_reads_untrusted() {
        assert_eq!(READ_CONTEXT_TOOL, "read_context");
        assert_eq!(READ_CONTEXT_EFFECT, ToolEffect::ReadsUntrusted);
    }
}
