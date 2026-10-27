//! The HTTP face of `task_claims`.
//!
//! Routes split in two. `/tasks/*` acts per worktree: the tools and the runs use it, and a run's
//! key crosses it because those are literal POST paths. `/projects/{id}/tasks*` is for the owner
//! only (hooks and the shell). A project that is not on the roster is "claims unavailable", never a
//! server error: the caller carries on as if nothing were claimed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{ApiTokenLevel, Scope};
use crate::state::AppState;
use crate::task_claims::{
    ClaimOutcome, Closer, Holder, HolderKind, NewTask, Notice, Status, TaskKind, TaskRow, keys,
    sessions, store,
};

/// The result of resolving one `cwd`: `(project_id, canonical worktree root)`, or `None` when no
/// project on the roster is rooted there.
type Resolved = Option<(String, String)>;

/// `cwd -> project`, remembered between hook calls. Resolving costs a git subprocess and a
/// canonicalisation of every roster root (`vcs::project_for_worktree`), and the hook fires on every
/// tool call of every session. A refusal is cached too, but briefly: registering a project must
/// start working within a minute, and an unregistered one must not cost a subprocess per tool call.
#[derive(Default)]
pub(super) struct CwdCache {
    entries: HashMap<String, (Instant, Resolved)>,
}

impl CwdCache {
    pub(super) const POSITIVE_TTL: Duration = Duration::from_secs(600);
    pub(super) const NEGATIVE_TTL: Duration = Duration::from_secs(60);
    pub(super) const MAX_ENTRIES: usize = 256;

    /// `None` is a miss; `Some(None)` is a cached "not registered".
    pub(super) fn get(&self, cwd: &str, now: Instant) -> Option<Resolved> {
        let (at, resolved) = self.entries.get(cwd)?;
        let ttl = if resolved.is_some() {
            Self::POSITIVE_TTL
        } else {
            Self::NEGATIVE_TTL
        };
        (now.saturating_duration_since(*at) <= ttl).then(|| resolved.clone())
    }

    pub(super) fn put(&mut self, cwd: &str, resolved: Resolved, now: Instant) {
        if self.entries.len() >= Self::MAX_ENTRIES {
            self.entries.clear();
        }
        self.entries.insert(cwd.to_owned(), (now, resolved));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// How an agent can name a holder when it reads who has a task.
pub(super) fn holder_label(holder: &Holder) -> String {
    match holder.kind {
        HolderKind::Run => format!("run #{}", holder.id),
        HolderKind::Job => format!("job #{}", holder.id),
        HolderKind::Postgate => "the daemon (post-merge gate)".to_owned(),
        HolderKind::Verify => "the daemon (verify)".to_owned(),
        HolderKind::Session => format!("another session in {}", holder.worktree),
    }
}

/// The first seven characters of a sha, cut on a character boundary.
fn short(sha: &str) -> &str {
    sha.char_indices()
        .nth(7)
        .map_or(sha, |(end, _)| &sha[..end])
}

/// A notice as the agent reads it: what happened and what to do about it.
pub(super) fn render_notice(notice: &Notice) -> String {
    match notice {
        Notice::ClaimLost { task, new_holder } => format!(
            "Your claim on #{} `{}` is gone: it is now held by {}. Stop working on it; take another task or ask.",
            task.id,
            task.key,
            holder_label(new_holder),
        ),
        Notice::IncidentClosed { task, sha } => {
            let at = sha
                .as_deref()
                .map(|sha| format!(" at {}", short(sha)))
                .unwrap_or_default();
            format!(
                "Incident #{} `{}` was closed{at}: master is green again. If you were fixing it, stop; \
                 if your verify inherited it, rebase on master and verify again.",
                task.id, task.key,
            )
        }
        // Delivered by chunk 4; the same call to action as `IncidentClosed`, for the verify side.
        Notice::InheritedClosed { task, sha } => {
            let at = sha
                .as_deref()
                .map(|sha| format!(" at {}", short(sha)))
                .unwrap_or_default();
            format!(
                "Incident #{} `{}` was closed{at}: master is green again. Your verify inherited it: \
                 rebase on master and verify again.",
                task.id, task.key,
            )
        }
    }
}

/// One line of the "live incidents" list.
pub(super) fn render_incident(task: &TaskRow) -> String {
    let since = task
        .detail
        .get("red_since_sha")
        .and_then(Value::as_str)
        .map(|sha| format!(" since {}", short(sha)))
        .unwrap_or_default();
    let state = match task.holder() {
        Some(holder) => format!("being fixed by {}", holder_label(&holder)),
        None => "unclaimed".to_owned(),
    };
    format!("#{} {}{since}: {state}", task.id, task.key)
}

/// Where a request acts: the project, the worktree's canonical root, and the project's own root
/// (what `keys::normalize` makes paths relative to).
pub(super) struct Located {
    pub project_id: String,
    pub root: String,
    pub project_root: std::path::PathBuf,
}

type Fail = (StatusCode, String);

const NEEDS_SESSION_OR_RUN: &str = "task claims need a session or a run";

/// Resolves the worktree a request acts in, with the fence `verify` and `record` use
/// (`verify::resolve_worktree`): a run is held to its own worktree, the owner may name any
/// registered worktree of a known project. A refusal that says the project or the worktree is not
/// known becomes "claims unavailable", which the caller must read as "carry on, nothing is
/// claimed", not as a failure.
pub(super) async fn locate(
    pool: &sqlx::SqlitePool,
    scope: &Scope,
    worktree: Option<&str>,
) -> Result<Located, Fail> {
    let caller = crate::verify::Caller::from_scope(scope).ok_or((
        StatusCode::FORBIDDEN,
        "this key cannot use task claims".to_owned(),
    ))?;
    if matches!(caller, crate::verify::Caller::Owner) && worktree.is_none() {
        return Err((StatusCode::FORBIDDEN, NEEDS_SESSION_OR_RUN.to_owned()));
    }
    let deadline = Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let (root, project_id) = crate::verify::resolve_worktree(pool, caller, worktree, deadline)
        .await
        .map_err(|error| match error {
            crate::verify::VerifyError::NotFound(why) => (
                StatusCode::NOT_FOUND,
                format!("task claims unavailable: project not registered ({why})"),
            ),
            other => (other.status(), other.message().to_owned()),
        })?;
    let root = crate::execution::git_exec::canonical(&root)
        .await
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))?;
    // `crate::inspect`, as `verify.rs` reads it; it answers `Option<String>`.
    let project_root = crate::inspect::project_root(pool, &project_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "task claims unavailable: project not registered (no root recorded)".to_owned(),
            )
        })?;
    Ok(Located {
        project_id,
        root,
        project_root: project_root.into(),
    })
}

fn internal(error: impl std::fmt::Display) -> Fail {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("task claims: {error}"),
    )
}

/// The store's refusal of a claim it will not take (a declared `master-red`) is a
/// `sqlx::Error::Protocol` carrying a message for the agent: a 409, not a server fault.
fn claim_error(error: sqlx::Error) -> Fail {
    match error {
        sqlx::Error::Protocol(message) => (StatusCode::CONFLICT, message),
        other => internal(other),
    }
}

/// How long `SessionStart` waits for the re-proof of a stale `master-red` before listing the
/// incident as it stands (spec §3 Verify, "Também no SessionStart"). The whole refresh (git
/// included) runs detached and this bounds the wait for it: the hook's own budget,
/// `CLAIMS_TIMEOUT_SECONDS` = 2 s in `ask_daemon.py`, also covers up to 1 s of token discovery.
const SESSION_REFRESH_WAIT: Duration = Duration::from_millis(500);

/// The shape the hook also checks (`SESSION_ID_SHAPE` in `ask_daemon.py`).
fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Who the caller is, as a claim holder. The run's identity comes from the key's scope, the
/// session's from the id the hook stamped, and only if `session_seen_in` has seen it in this very
/// worktree, which catches an id copied wrong or left over from another session.
pub(super) async fn holder_for(
    pool: &sqlx::SqlitePool,
    scope: &Scope,
    at: &Located,
    session_id: Option<&str>,
) -> Result<Holder, Fail> {
    match scope {
        Scope::Run(run_id) => {
            // A job node is a run whose row carries the job: the claim belongs to the job, which
            // outlives the node. A plain run holds by its own id.
            let job: Option<Option<i64>> =
                sqlx::query_scalar("SELECT job_id FROM runs WHERE id = ?")
                    .bind(run_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(internal)?;
            let (kind, id) = match job.flatten() {
                Some(job_id) => (HolderKind::Job, job_id.to_string()),
                None => (HolderKind::Run, run_id.to_string()),
            };
            Ok(Holder {
                kind,
                id,
                worktree: at.root.clone(),
            })
        }
        Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin) => {
            let session = session_id
                .ok_or_else(|| (StatusCode::FORBIDDEN, NEEDS_SESSION_OR_RUN.to_owned()))?;
            if !valid_session_id(session) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "session_id has an unexpected shape".to_owned(),
                ));
            }
            let seen = sessions::session_seen_in(pool, session, &at.root)
                .await
                .map_err(internal)?;
            if !seen {
                return Err((
                    StatusCode::FORBIDDEN,
                    format!(
                        "session {session} has not been seen in {}; the first tool call registers it, retry",
                        at.root
                    ),
                ));
            }
            Ok(Holder {
                kind: HolderKind::Session,
                id: session.to_owned(),
                worktree: at.root.clone(),
            })
        }
        _ => Err((
            StatusCode::FORBIDDEN,
            "this key cannot use task claims".to_owned(),
        )),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ClaimBody {
    worktree: Option<String>,
    session_id: Option<String>,
    key: Option<String>,
    title: Option<String>,
    guard_paths: Option<Vec<String>>,
    hint_paths: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CloseBody {
    worktree: Option<String>,
    session_id: Option<String>,
    id: Option<i64>,
    note: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ListBody {
    worktree: Option<String>,
    session_id: Option<String>,
}

type Now = chrono::DateTime<chrono::Utc>;

/// Whole seconds from `created_at` (a `wave::stamp`) to `now`; 0 for a stamp that does not parse.
fn age_seconds(created_at: &str, now: Now) -> i64 {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|at| (now - at.with_timezone(&chrono::Utc)).num_seconds().max(0))
        .unwrap_or(0)
}

/// The live tasks of one project, each with its age.
pub(super) async fn list_tasks(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    now: Now,
) -> Result<Value, Fail> {
    let rows = store::list_live(pool, project_id, now)
        .await
        .map_err(internal)?;
    let tasks: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut task = serde_json::to_value(row).unwrap_or(Value::Null);
            if let Some(fields) = task.as_object_mut() {
                fields.insert(
                    "age_seconds".to_owned(),
                    json!(age_seconds(&row.created_at, now)),
                );
            }
            task
        })
        .collect();
    Ok(json!({ "project_id": project_id, "tasks": tasks }))
}

fn bad_request(message: &str) -> Fail {
    (StatusCode::BAD_REQUEST, message.to_owned())
}

/// Runs `make` against the caller's worktree root first, then the project root: an absolute path
/// is under one or the other (or the worktree sits outside the repo), and either way it must end
/// up as the same project-relative text, so one file claimed from two worktrees is one key.
fn under_either_root<T>(
    at: &Located,
    make: impl Fn(&std::path::Path) -> Result<T, keys::KeyError>,
) -> Result<T, Fail> {
    match make(std::path::Path::new(&at.root)) {
        Err(keys::KeyError::OutsideRoot(_)) => make(&at.project_root),
        other => other,
    }
    .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))
}

fn normalize_paths(raw: Option<&[String]>, at: &Located) -> Result<Vec<String>, Fail> {
    raw.unwrap_or_default()
        .iter()
        .map(|path| under_either_root(at, |root| keys::normalize_path(path, root)))
        .collect()
}

/// `task_claim`. Losing is an answer, not an error: `won: false` with the holder, so the agent
/// can carry on with its own work.
pub(super) async fn claim_task(
    pool: &sqlx::SqlitePool,
    at: &Located,
    holder: &Holder,
    body: &ClaimBody,
    now: Now,
) -> Result<Value, Fail> {
    let raw = body
        .key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| bad_request("key is required"))?;
    // `KeyError`'s message lists the accepted prefixes; it goes to the agent as it is.
    let key = under_either_root(at, |root| keys::normalize(raw, root))?;
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map_or_else(|| key.to_string(), str::to_owned);
    let new = NewTask {
        key,
        kind: TaskKind::Declared,
        title,
        detail: json!({}),
        guard_paths: normalize_paths(body.guard_paths.as_deref(), at)?,
        hint_paths: normalize_paths(body.hint_paths.as_deref(), at)?,
    };
    match store::claim(pool, &at.project_id, new, holder, now)
        .await
        .map_err(claim_error)?
    {
        ClaimOutcome::Won { task, similar } => {
            Ok(json!({ "won": true, "task": task, "holder": null, "similar": similar }))
        }
        ClaimOutcome::Lost {
            task,
            holder,
            similar,
        } => Ok(json!({ "won": false, "task": task, "holder": holder, "similar": similar })),
    }
}

/// A note is required: the history in `task_claim_events` is read by people.
fn required_note(note: &str) -> Result<&str, Fail> {
    let note = note.trim();
    if note.is_empty() {
        Err(bad_request("note is required"))
    } else {
        Ok(note)
    }
}

/// One answer for "no such task", "not yours" and "no longer live": it must not tell which, or the
/// route would let a caller probe task ids.
fn not_held(id: i64) -> Fail {
    (
        StatusCode::CONFLICT,
        format!("task #{id} is not held by you (or is no longer live)"),
    )
}

/// `task_release`: back to `open`. No project check: a holder only ever matches its own rows (run,
/// job and session ids are unique across projects).
pub(super) async fn release_task(
    pool: &sqlx::SqlitePool,
    holder: &Holder,
    id: i64,
    note: &str,
    now: Now,
) -> Result<Value, Fail> {
    let note = required_note(note)?;
    if store::release(pool, id, holder, note, now)
        .await
        .map_err(internal)?
    {
        Ok(json!({ "id": id, "status": "open" }))
    } else {
        Err(not_held(id))
    }
}

/// `task_done`: closed by its holder.
pub(super) async fn done_task(
    pool: &sqlx::SqlitePool,
    holder: &Holder,
    id: i64,
    note: &str,
    now: Now,
) -> Result<Value, Fail> {
    let note = required_note(note)?;
    if store::done(pool, id, Closer::Holder(holder), note, now)
        .await
        .map_err(internal)?
    {
        Ok(json!({ "id": id, "status": "done" }))
    } else {
        Err(not_held(id))
    }
}

/// The database half of `POST /tasks/session`: register the session (which renews every live
/// claim it holds), then hand over what happened to its claims since the last call. Notices are
/// delivered once: `take_notices` advances the session's cursor, so a hook reply that is lost in
/// transit loses its notices — the price of never repeating them, accepted because every notice
/// is also visible to `task_list`. So they are taken last, after the (bounded) `master-red`
/// refresh: a reply that runs out of the hook's time there has not consumed any.
pub(super) async fn session_step(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    project_id: &str,
    root: &str,
    want_incidents: bool,
    executor: Option<&std::sync::Arc<crate::verify::exec::Executor>>,
    now: Now,
) -> sqlx::Result<Value> {
    sessions::touch_session(pool, session_id, root, project_id, now).await?;
    let incidents: Vec<String> = if want_incidents {
        // Re-prove a stale `master-red` before the list, so only what is still live is told.
        // The executor is a process global (`verify::installed`); without one (early startup,
        // tests) the list goes out as it stands. The refresh runs as its own task so that a
        // wait that runs out leaves it to finish (and settle the re-proof flight) in the
        // background, instead of dropping it halfway.
        if let Some(executor) = executor {
            let (pool, executor, project) = (pool.clone(), executor.clone(), project_id.to_owned());
            let refresh = tokio::spawn(async move {
                crate::verify::inherit::refresh_master_red(
                    &pool,
                    &executor,
                    &project,
                    SESSION_REFRESH_WAIT,
                    chrono::Utc::now(),
                )
                .await
            });
            match tokio::time::timeout(SESSION_REFRESH_WAIT, refresh).await {
                Ok(Ok(crate::verify::inherit::Refresh::Unavailable(why))) => {
                    tracing::warn!(
                        %why,
                        project_id,
                        "could not refresh master-red at SessionStart"
                    );
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, project_id, "the master-red refresh task failed");
                }
                // Done, or still running past the wait: list the incident as it stands.
                Ok(Ok(_)) | Err(_) => {}
            }
        }
        store::list_live(pool, project_id, now)
            .await?
            .iter()
            .filter(|task| {
                task.kind == TaskKind::Incident
                    && matches!(task.status, Status::Open | Status::Claimed)
            })
            .map(render_incident)
            .collect()
    } else {
        Vec::new()
    };
    let notices: Vec<String> = sessions::take_notices(pool, session_id, now)
        .await?
        .iter()
        .map(render_notice)
        .collect();
    Ok(
        json!({ "registered": true, "project_id": project_id, "notices": notices, "incidents": incidents }),
    )
}

#[derive(Debug, Deserialize)]
pub(super) struct SessionBody {
    session_id: String,
    cwd: String,
    #[serde(default)]
    want_incidents: bool,
}

/// `cwd → project`, shared by every hook call of the process. The lock is taken and released inside
/// the two small helpers below, never held across an `.await`.
static CWD_CACHE: std::sync::LazyLock<std::sync::Mutex<CwdCache>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(CwdCache::default()));

fn cached_cwd(cwd: &str, now: Instant) -> Option<Resolved> {
    CWD_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(cwd, now)
}

fn remember_cwd(cwd: &str, resolved: Resolved, now: Instant) {
    CWD_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .put(cwd, resolved, now);
}

/// The roster project a hook's `cwd` belongs to, as `fleet.rs:683-696` finds it: the git toplevel,
/// then `vcs::project_for_worktree`. Any refusal (not a checkout, not on the roster, roster
/// unreadable) is `None`: for a hook that means "claims do not apply here", never an error.
async fn resolve_cwd(pool: &sqlx::SqlitePool, cwd: &str) -> Resolved {
    let now = Instant::now();
    if let Some(known) = cached_cwd(cwd, now) {
        return known;
    }
    let deadline = now + crate::execution::git_exec::OPERATION_TIMEOUT;
    let resolved = async {
        let top = crate::execution::git_exec::toplevel(std::path::Path::new(cwd), deadline)
            .await
            .ok()?;
        let project_id = crate::execution::vcs::project_for_worktree(pool, &top, deadline)
            .await
            .ok()?;
        let root = crate::execution::git_exec::canonical(&top).await.ok()?;
        Some((project_id, root))
    }
    .await;
    remember_cwd(cwd, resolved.clone(), now);
    resolved
}

type Reply = Result<Json<Value>, Fail>;

/// `POST /tasks/session`, the hook's call. Fail-open: any git or database trouble answers 200
/// `{registered: false}` (and a warning), because a 500 would only make the hook spend its backoff.
pub(super) async fn post_session(
    State(state): State<AppState>,
    Json(body): Json<SessionBody>,
) -> Reply {
    if !valid_session_id(&body.session_id) {
        return Err(bad_request("session_id has an unexpected shape"));
    }
    if body.cwd.trim().is_empty() {
        return Err(bad_request("cwd is required"));
    }
    let away = || Json(json!({ "registered": false }));
    let Some((project_id, root)) = resolve_cwd(&state.pool, &body.cwd).await else {
        return Ok(away());
    };
    let executor = crate::verify::installed();
    match session_step(
        &state.pool,
        &body.session_id,
        &project_id,
        &root,
        body.want_incidents,
        executor.as_ref(),
        chrono::Utc::now(),
    )
    .await
    {
        Ok(answer) => Ok(Json(answer)),
        Err(error) => {
            tracing::warn!(%error, session_id = %body.session_id, "could not register the session");
            Ok(away())
        }
    }
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/tasks/list", post(post_list))
        .route("/tasks/claim", post(post_claim))
        .route("/tasks/release", post(post_release))
        .route("/tasks/done", post(post_done))
        .route("/tasks/session", post(post_session))
        .route("/projects/{id}/tasks", get(get_project_tasks))
        .route("/projects/{id}/tasks/claim", post(post_project_claim))
        .route(
            "/projects/{id}/tasks/{task}/release",
            post(post_project_release),
        )
        .route("/projects/{id}/tasks/{task}/done", post(post_project_done))
}

/// `locate` then `holder_for`: the two lookups every claim route starts with.
async fn acting(
    state: &AppState,
    scope: &Scope,
    worktree: Option<&str>,
    session_id: Option<&str>,
) -> Result<(Located, Holder), Fail> {
    let at = locate(&state.pool, scope, worktree).await?;
    let holder = holder_for(&state.pool, scope, &at, session_id).await?;
    Ok((at, holder))
}

async fn post_list(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(body): Json<ListBody>,
) -> Reply {
    list_in(&state.pool, &scope, body.worktree.as_deref())
        .await
        .map(Json)
}

/// A listing is a read: it needs the worktree, never a registered session or a holder.
async fn list_in(
    pool: &sqlx::SqlitePool,
    scope: &Scope,
    worktree: Option<&str>,
) -> Result<Value, Fail> {
    let at = locate(pool, scope, worktree).await?;
    list_tasks(pool, &at.project_id, chrono::Utc::now()).await
}

async fn post_claim(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(body): Json<ClaimBody>,
) -> Reply {
    let (at, holder) = acting(
        &state,
        &scope,
        body.worktree.as_deref(),
        body.session_id.as_deref(),
    )
    .await?;
    claim_task(&state.pool, &at, &holder, &body, chrono::Utc::now())
        .await
        .map(Json)
}

async fn post_release(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(body): Json<CloseBody>,
) -> Reply {
    let (_at, holder) = acting(
        &state,
        &scope,
        body.worktree.as_deref(),
        body.session_id.as_deref(),
    )
    .await?;
    let id = body.id.ok_or_else(|| bad_request("id is required"))?;
    release_task(
        &state.pool,
        &holder,
        id,
        body.note.as_deref().unwrap_or(""),
        chrono::Utc::now(),
    )
    .await
    .map(Json)
}

async fn post_done(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(body): Json<CloseBody>,
) -> Reply {
    let (_at, holder) = acting(
        &state,
        &scope,
        body.worktree.as_deref(),
        body.session_id.as_deref(),
    )
    .await?;
    let id = body.id.ok_or_else(|| bad_request("id is required"))?;
    done_task(
        &state.pool,
        &holder,
        id,
        body.note.as_deref().unwrap_or(""),
        chrono::Utc::now(),
    )
    .await
    .map(Json)
}

/// A project route acts on a worktree of that project and no other.
fn ensure_project(at: &Located, project_id: &str) -> Result<(), Fail> {
    if at.project_id == project_id {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            format!(
                "that worktree belongs to project {}, not {project_id}",
                at.project_id
            ),
        ))
    }
}

/// Reading, for the shell and future hooks: no holder needed.
async fn get_project_tasks(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Reply {
    list_tasks(&state.pool, &id, chrono::Utc::now())
        .await
        .map(Json)
}

async fn post_project_claim(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<ClaimBody>,
) -> Reply {
    let (at, holder) = acting(
        &state,
        &scope,
        body.worktree.as_deref(),
        body.session_id.as_deref(),
    )
    .await?;
    ensure_project(&at, &id)?;
    claim_task(&state.pool, &at, &holder, &body, chrono::Utc::now())
        .await
        .map(Json)
}

async fn post_project_release(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    axum::extract::Path((id, task)): axum::extract::Path<(String, i64)>,
    Json(body): Json<CloseBody>,
) -> Reply {
    let (at, holder) = acting(
        &state,
        &scope,
        body.worktree.as_deref(),
        body.session_id.as_deref(),
    )
    .await?;
    ensure_project(&at, &id)?;
    release_task(
        &state.pool,
        &holder,
        task,
        body.note.as_deref().unwrap_or(""),
        chrono::Utc::now(),
    )
    .await
    .map(Json)
}

async fn post_project_done(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    axum::extract::Path((id, task)): axum::extract::Path<(String, i64)>,
    Json(body): Json<CloseBody>,
) -> Reply {
    let (at, holder) = acting(
        &state,
        &scope,
        body.worktree.as_deref(),
        body.session_id.as_deref(),
    )
    .await?;
    ensure_project(&at, &id)?;
    done_task(
        &state.pool,
        &holder,
        task,
        body.note.as_deref().unwrap_or(""),
        chrono::Utc::now(),
    )
    .await
    .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_claims::{Holder, HolderKind, Notice};
    use std::time::{Duration, Instant};

    fn holder(kind: HolderKind, id: &str, worktree: &str) -> Holder {
        Holder {
            kind,
            id: id.to_owned(),
            worktree: worktree.to_owned(),
        }
    }

    #[test]
    fn the_cwd_cache_remembers_a_project_and_forgets_it_after_the_ttl() {
        let t0 = Instant::now();
        let mut cache = CwdCache::default();
        assert_eq!(
            cache.get("/w", t0),
            None,
            "a miss is None, not a cached refusal"
        );
        cache.put("/w", Some(("alpha".into(), "/w".into())), t0);
        assert_eq!(
            cache.get("/w", t0 + Duration::from_secs(599)),
            Some(Some(("alpha".into(), "/w".into())))
        );
        assert_eq!(
            cache.get("/w", t0 + CwdCache::POSITIVE_TTL + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn an_unregistered_cwd_is_cached_for_a_short_time_only() {
        let t0 = Instant::now();
        let mut cache = CwdCache::default();
        cache.put("/x", None, t0);
        assert_eq!(cache.get("/x", t0 + Duration::from_secs(30)), Some(None));
        assert_eq!(
            cache.get("/x", t0 + CwdCache::NEGATIVE_TTL + Duration::from_secs(1)),
            None,
            "registering the project must show up within a minute"
        );
    }

    #[test]
    fn the_cwd_cache_is_bounded() {
        let t0 = Instant::now();
        let mut cache = CwdCache::default();
        for n in 0..(CwdCache::MAX_ENTRIES + 5) {
            cache.put(&format!("/w{n}"), None, t0);
        }
        assert!(cache.len() <= CwdCache::MAX_ENTRIES);
    }

    #[test]
    fn a_holder_is_named_the_way_an_agent_can_act_on() {
        assert_eq!(holder_label(&holder(HolderKind::Run, "7", "/w")), "run #7");
        assert_eq!(holder_label(&holder(HolderKind::Job, "3", "/w")), "job #3");
        assert_eq!(
            holder_label(&holder(HolderKind::Postgate, "alpha", "/pg")),
            "the daemon (post-merge gate)"
        );
        assert_eq!(
            holder_label(&holder(HolderKind::Session, "s1", "/w2")),
            "another session in /w2"
        );
    }

    #[test]
    fn a_lost_claim_says_who_has_the_task_now() {
        let task = crate::task_claims::testing::row("test:a::b", 12);
        let text = render_notice(&Notice::ClaimLost {
            task,
            new_holder: holder(HolderKind::Run, "9", "/w"),
        });
        assert!(text.contains("#12") && text.contains("test:a::b"), "{text}");
        assert!(text.contains("run #9"), "{text}");
        assert!(
            text.contains("stop"),
            "an agent must be told to stop, not only informed: {text}"
        );
    }

    #[test]
    fn a_closed_incident_tells_the_session_to_stop_and_rebase() {
        let task = crate::task_claims::testing::row("master-red", 4);
        let text = render_notice(&Notice::IncidentClosed {
            task,
            sha: Some("abcdef1234567".into()),
        });
        assert!(text.contains("#4") && text.contains("abcdef1"), "{text}");
        assert!(
            !text.contains("abcdef1234567"),
            "the sha is shortened to 7: {text}"
        );
        assert!(text.contains("rebase") && text.contains("verify"), "{text}");
    }

    #[test]
    fn an_incident_line_names_the_red_sha_and_the_owner() {
        let mut task = crate::task_claims::testing::row("master-red", 4);
        task.detail = serde_json::json!({ "red_since_sha": "1234567890abcdef" });
        task.holder_kind = Some(HolderKind::Run);
        task.holder_id = Some("21".into());
        let line = render_incident(&task);
        assert_eq!(line, "#4 master-red since 1234567: being fixed by run #21");

        task.holder_kind = None;
        task.holder_id = None;
        assert_eq!(
            render_incident(&task),
            "#4 master-red since 1234567: unclaimed"
        );
    }

    fn located(project: &str, root: &str) -> Located {
        Located {
            project_id: project.into(),
            root: root.into(),
            project_root: root.into(),
        }
    }

    /// The smallest `jobs` row (the columns `consolidate.rs`'s tests insert): `runs.job_id` is a
    /// foreign key to it.
    async fn insert_job(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES ('alpha', '/r', 'running', 3, '2026-10-22T00:00:00+00:00') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn insert_run(pool: &sqlx::SqlitePool, cwd: &str, job: Option<i64>) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, project_id, cwd, job_id, created_at)
             VALUES ('x', 'running', 'worktree', 'alpha', ?, ?, '2026-10-22T00:00:00Z')",
        )
        .bind(cwd)
        .bind(job)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn a_plain_run_holds_by_its_run_id() {
        let pool = crate::testdb::fresh_pool().await;
        let run = insert_run(&pool, "/w", None).await;
        let holder = holder_for(&pool, &Scope::Run(run), &located("alpha", "/w"), None)
            .await
            .unwrap();
        assert_eq!(
            holder,
            Holder {
                kind: HolderKind::Run,
                id: run.to_string(),
                worktree: "/w".into()
            }
        );
    }

    #[tokio::test]
    async fn a_job_node_holds_by_its_job() {
        let pool = crate::testdb::fresh_pool().await;
        let job = insert_job(&pool).await;
        let run = insert_run(&pool, "/w", Some(job)).await;
        let holder = holder_for(&pool, &Scope::Run(run), &located("alpha", "/w"), None)
            .await
            .unwrap();
        assert_eq!(
            (holder.kind, holder.id.as_str()),
            (HolderKind::Job, job.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn a_session_is_only_believed_where_it_was_seen() {
        let pool = crate::testdb::fresh_pool().await;
        let now = chrono::Utc::now();
        sessions::touch_session(&pool, "s1", "/w", "alpha", now)
            .await
            .unwrap();

        let ok = holder_for(&pool, &Scope::Control, &located("alpha", "/w"), Some("s1"))
            .await
            .unwrap();
        assert_eq!(
            (ok.kind, ok.id.as_str(), ok.worktree.as_str()),
            (HolderKind::Session, "s1", "/w")
        );

        let elsewhere = holder_for(
            &pool,
            &Scope::Control,
            &located("alpha", "/other"),
            Some("s1"),
        )
        .await;
        assert_eq!(
            elsewhere.unwrap_err().0,
            StatusCode::FORBIDDEN,
            "seen in /w, not in /other"
        );
        let unknown = holder_for(
            &pool,
            &Scope::Control,
            &located("alpha", "/w"),
            Some("nope"),
        )
        .await;
        assert_eq!(unknown.unwrap_err().0, StatusCode::FORBIDDEN);
        let missing = holder_for(&pool, &Scope::Control, &located("alpha", "/w"), None).await;
        assert_eq!(missing.unwrap_err().0, StatusCode::FORBIDDEN);
        let admin = holder_for(
            &pool,
            &Scope::ApiToken(ApiTokenLevel::Admin),
            &located("alpha", "/w"),
            Some("s1"),
        )
        .await;
        assert!(admin.is_ok());
        let shaped = holder_for(
            &pool,
            &Scope::Control,
            &located("alpha", "/w"),
            Some("../etc"),
        )
        .await;
        assert_eq!(
            shaped.unwrap_err().0,
            StatusCode::BAD_REQUEST,
            "the id shape is checked first"
        );
    }

    #[tokio::test]
    async fn other_keys_hold_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        for scope in [
            Scope::ApiToken(ApiTokenLevel::ReadOnly),
            Scope::TeamRun("t".into()),
        ] {
            let refused = holder_for(&pool, &scope, &located("alpha", "/w"), Some("s1")).await;
            assert_eq!(refused.unwrap_err().0, StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn a_checkout_outside_the_roster_answers_claims_unavailable() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(init.success());
        let cwd = dir.path().to_string_lossy().into_owned();
        let refused = locate(&pool, &Scope::Control, Some(&cwd))
            .await
            .err()
            .expect("not on the roster");
        assert_eq!(refused.0, StatusCode::NOT_FOUND);
        assert!(
            refused
                .1
                .starts_with("task claims unavailable: project not registered"),
            "{}",
            refused.1
        );
    }

    #[tokio::test]
    async fn the_first_claim_wins_and_the_second_is_told_who_has_it() {
        let pool = crate::testdb::fresh_pool().await;
        let at = located("alpha", "/w");
        let run = holder(HolderKind::Run, "7", "/w");
        let other = holder(HolderKind::Run, "8", "/w2");
        let body = ClaimBody {
            key: Some("test:core/a.rs::b".into()),
            title: Some("b fails".into()),
            ..Default::default()
        };

        let first = claim_task(&pool, &at, &run, &body, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(first["won"], true);
        let second = claim_task(&pool, &at, &other, &body, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(second["won"], false);
        assert_eq!(second["holder"]["id"], "7");
        assert_eq!(second["task"]["id"], first["task"]["id"]);
        let again = claim_task(&pool, &at, &run, &body, chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(
            again["won"], true,
            "the same holder renews, it does not lose to itself"
        );
    }

    #[tokio::test]
    async fn a_declared_claim_on_master_red_is_a_409_not_a_500() {
        let pool = crate::testdb::fresh_pool().await;
        let body = ClaimBody {
            key: Some("master-red".into()),
            ..Default::default()
        };
        let refused = claim_task(
            &pool,
            &located("alpha", "/w"),
            &holder(HolderKind::Run, "7", "/w"),
            &body,
            chrono::Utc::now(),
        )
        .await
        .err()
        .expect("the store refuses a declared master-red");
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert!(
            refused.1.contains("created by the daemon only"),
            "{}",
            refused.1
        );
    }

    #[tokio::test]
    async fn a_session_wins_the_open_master_red_incident_through_task_claim() {
        let pool = crate::testdb::fresh_pool().await;
        let t0 = chrono::Utc::now();
        // The postgate's way in (`ensure_incident`): an open incident nobody holds.
        let new = crate::task_claims::testing::new_task(
            "master-red",
            crate::task_claims::TaskKind::Incident,
        );
        let incident = store::create_or_adopt_open(&pool, "alpha", new, t0)
            .await
            .unwrap();
        let body = ClaimBody {
            key: Some("master-red".into()),
            ..Default::default()
        };
        let at = located("alpha", "/w");
        let (s1, s2) = (
            holder(HolderKind::Session, "s1", "/w"),
            holder(HolderKind::Session, "s2", "/w2"),
        );

        let won = claim_task(&pool, &at, &s1, &body, t0)
            .await
            .expect("claiming the daemon's incident is not a refusal");
        assert_eq!(won["won"], true, "{won}");
        assert_eq!(won["task"]["id"], incident.id);
        assert_eq!(
            won["task"]["kind"], "incident",
            "the incident stays the daemon's kind"
        );
        let lost = claim_task(&pool, &at, &s2, &body, t0).await.unwrap();
        assert_eq!(lost["won"], false, "first claim wins: {lost}");
        assert_eq!(lost["holder"]["id"], "s1");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_claims")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "no second master-red");
    }

    fn located_in(worktree: &str, project_root: &str) -> Located {
        Located {
            project_id: "alpha".into(),
            root: worktree.into(),
            project_root: project_root.into(),
        }
    }

    #[tokio::test]
    async fn one_file_claimed_from_two_worktrees_is_one_key() {
        let pool = crate::testdb::fresh_pool().await;
        // One worktree inside the repo, one outside it.
        let inside = located_in("/repo/.claude/worktrees/a", "/repo");
        let outside = located_in("/elsewhere/b", "/repo");
        let first_body = ClaimBody {
            key: Some("file:/repo/.claude/worktrees/a/core/a.rs".into()),
            ..Default::default()
        };
        let second_body = ClaimBody {
            key: Some("file:/elsewhere/b/core/a.rs".into()),
            ..Default::default()
        };
        let first = claim_task(
            &pool,
            &inside,
            &holder(HolderKind::Run, "1", "/repo/.claude/worktrees/a"),
            &first_body,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(first["won"], true);
        assert_eq!(first["task"]["key"], "file:core/a.rs");
        let second = claim_task(
            &pool,
            &outside,
            &holder(HolderKind::Run, "2", "/elsewhere/b"),
            &second_body,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(
            second["won"], false,
            "the same file from another worktree is the same task"
        );
        assert_eq!(second["holder"]["id"], "1");
        // A path under the project root still resolves when the worktree is elsewhere.
        let under_project = ClaimBody {
            key: Some("file:/repo/core/a.rs".into()),
            ..Default::default()
        };
        let third = claim_task(
            &pool,
            &outside,
            &holder(HolderKind::Run, "2", "/elsewhere/b"),
            &under_project,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(third["task"]["id"], first["task"]["id"]);
    }

    #[tokio::test]
    async fn a_listing_asks_for_no_session() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(init.success());
        let cwd = dir.path().to_string_lossy().into_owned();
        // Owner, no session: the refusal is about the checkout (not on the roster), never about a
        // missing session. Registering a project in a unit test is not available here, so this
        // pins the error `list_in` can raise.
        let refused = list_in(&pool, &Scope::Control, Some(&cwd))
            .await
            .unwrap_err();
        assert_eq!(refused.0, StatusCode::NOT_FOUND);
        assert!(!refused.1.contains("session"), "{}", refused.1);
    }

    #[tokio::test]
    async fn a_malformed_key_is_a_400_that_says_what_is_accepted() {
        let pool = crate::testdb::fresh_pool().await;
        let body = ClaimBody {
            key: Some("bug in the thing".into()),
            ..Default::default()
        };
        let refused = claim_task(
            &pool,
            &located("alpha", "/w"),
            &holder(HolderKind::Run, "7", "/w"),
            &body,
            chrono::Utc::now(),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.0, StatusCode::BAD_REQUEST);
        assert!(
            refused.1.contains("test:") && refused.1.contains("topic:"),
            "{}",
            refused.1
        );
    }

    #[tokio::test]
    async fn only_the_holder_releases_or_closes() {
        let pool = crate::testdb::fresh_pool().await;
        let at = located("alpha", "/w");
        let mine = holder(HolderKind::Run, "7", "/w");
        let theirs = holder(HolderKind::Run, "8", "/w2");
        let body = ClaimBody {
            key: Some("file:core/a.rs".into()),
            ..Default::default()
        };
        let id = claim_task(&pool, &at, &mine, &body, chrono::Utc::now())
            .await
            .unwrap()["task"]["id"]
            .as_i64()
            .unwrap();

        let refused = release_task(&pool, &theirs, id, "mine now", chrono::Utc::now())
            .await
            .unwrap_err();
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert_eq!(
            done_task(&pool, &theirs, id, "done", chrono::Utc::now())
                .await
                .unwrap_err()
                .0,
            StatusCode::CONFLICT
        );

        release_task(&pool, &mine, id, "not mine after all", chrono::Utc::now())
            .await
            .unwrap();
        let listed = list_tasks(&pool, "alpha", chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(
            listed["tasks"][0]["status"], "open",
            "released goes back to open: {listed}"
        );

        claim_task(&pool, &at, &theirs, &body, chrono::Utc::now())
            .await
            .unwrap();
        done_task(&pool, &theirs, id, "fixed", chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(
            list_tasks(&pool, "alpha", chrono::Utc::now())
                .await
                .unwrap()["tasks"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn the_list_carries_owner_and_age_and_is_per_project() {
        let pool = crate::testdb::fresh_pool().await;
        let t0 = chrono::Utc::now();
        let body = ClaimBody {
            key: Some("file:core/a.rs".into()),
            ..Default::default()
        };
        claim_task(
            &pool,
            &located("alpha", "/w"),
            &holder(HolderKind::Run, "7", "/w"),
            &body,
            t0,
        )
        .await
        .unwrap();
        let listed = list_tasks(&pool, "alpha", t0 + chrono::Duration::seconds(90))
            .await
            .unwrap();
        assert_eq!(listed["tasks"][0]["holder_id"], "7");
        assert_eq!(listed["tasks"][0]["age_seconds"], 90);
        assert_eq!(
            list_tasks(&pool, "beta", t0).await.unwrap()["tasks"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn touching_a_session_registers_it_and_hands_over_each_notice_once() {
        let pool = crate::testdb::fresh_pool().await;
        let t0 = chrono::Utc::now();
        let mine = holder(HolderKind::Session, "s1", "/w");
        let body = ClaimBody {
            key: Some("file:core/a.rs".into()),
            ..Default::default()
        };
        // The session registers BEFORE it claims, as the hook makes it: `touch_session` starts a
        // new session's notice cursor at the newest event id, so a session first seen after the
        // takeover would (rightly) hear nothing about it.
        sessions::touch_session(&pool, "s1", "/w", "alpha", t0)
            .await
            .unwrap();
        claim_task(&pool, &located("alpha", "/w"), &mine, &body, t0)
            .await
            .unwrap();

        // The claim lapses and a run takes it: s1 has a ClaimLost waiting.
        let later = t0 + chrono::Duration::seconds(crate::task_claims::SESSION_LEASE_SECONDS + 60);
        claim_task(
            &pool,
            &located("alpha", "/w2"),
            &holder(HolderKind::Run, "9", "/w2"),
            &body,
            later,
        )
        .await
        .unwrap();

        let first = session_step(&pool, "s1", "alpha", "/w", false, None, later)
            .await
            .unwrap();
        assert_eq!(first["registered"], true);
        assert_eq!(first["notices"].as_array().unwrap().len(), 1, "{first}");
        assert!(first["notices"][0].as_str().unwrap().contains("run #9"));
        let second = session_step(&pool, "s1", "alpha", "/w", false, None, later)
            .await
            .unwrap();
        assert!(
            second["notices"].as_array().unwrap().is_empty(),
            "the cursor advanced"
        );
        assert!(sessions::session_seen_in(&pool, "s1", "/w").await.unwrap());
        assert!(
            first.get("incidents").is_none() || first["incidents"].as_array().unwrap().is_empty()
        );
    }

    #[tokio::test]
    async fn incidents_are_listed_only_when_asked_and_only_incidents() {
        let pool = crate::testdb::fresh_pool().await;
        let t0 = chrono::Utc::now();
        let new = crate::task_claims::testing::new_task(
            "master-red",
            crate::task_claims::TaskKind::Incident,
        );
        crate::task_claims::store::create_or_adopt_open(&pool, "alpha", new, t0)
            .await
            .unwrap();
        let declared = ClaimBody {
            key: Some("file:core/a.rs".into()),
            ..Default::default()
        };
        claim_task(
            &pool,
            &located("alpha", "/w"),
            &holder(HolderKind::Run, "7", "/w"),
            &declared,
            t0,
        )
        .await
        .unwrap();

        let quiet = session_step(&pool, "s1", "alpha", "/w", false, None, t0)
            .await
            .unwrap();
        assert!(
            quiet.get("incidents").is_none() || quiet["incidents"].as_array().unwrap().is_empty()
        );
        let asked = session_step(&pool, "s1", "alpha", "/w", true, None, t0)
            .await
            .unwrap();
        let lines = asked["incidents"].as_array().unwrap();
        assert_eq!(
            lines.len(),
            1,
            "a declared task is not an incident: {asked}"
        );
        assert!(lines[0].as_str().unwrap().contains("master-red"));
    }

    #[tokio::test]
    async fn a_project_route_refuses_a_worktree_of_another_project() {
        let at = located("beta", "/w");
        let refused = ensure_project(&at, "alpha").unwrap_err();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        assert!(ensure_project(&located("alpha", "/w"), "alpha").is_ok());
    }
}
