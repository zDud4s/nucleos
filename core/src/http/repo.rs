//! Reading a project's tree (`/projects/{id}/ls`, `/cat`, `/grep`, `/log`, `/diff`…).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/record", get(get_project_record))
        .route("/projects/{id}/ls", get(get_project_ls))
        .route("/projects/{id}/cat", get(get_project_cat))
        .route("/projects/{id}/grep", get(get_project_grep))
        .route("/projects/{id}/diff", get(get_project_diff))
        .route("/projects/{id}/log", get(get_project_log))
        .route("/projects/{id}/branches", get(get_project_branches))
        .route("/projects/{id}/blame", get(get_project_blame))
        .route("/projects/{id}/changed", get(get_project_changed))
        .route("/projects/{id}/worktree", get(get_project_worktree))
}

/// One run's checkout, as the shell needs it to open a door to the editor.
#[derive(serde::Serialize)]
pub(super) struct WorktreeView {
    /// Absolute, on this machine. The shell joins repository-relative paths onto it.
    pub(super) path: String,
    pub(super) branch: String,
    /// `None` when the daemon never recorded a branch point, which is why nothing here can be
    /// measured against it. Reported rather than hidden, so the panel can say which absence it is.
    pub(super) base_sha: Option<String>,
    pub(super) created_at: String,
}

/// How much history, and of what. Both absent is the whole repository's recent commits.
#[derive(Deserialize)]
pub(super) struct LogQuery {
    pub(super) path: Option<String>,
    pub(super) limit: Option<usize>,
    pub(super) run: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct GrepQuery {
    pub(super) q: Option<String>,
    pub(super) path: Option<String>,
    pub(super) run: Option<i64>,
}

/// The base commit a run's worktree was cut from, and the path to it.
///
/// Both or neither: a worktree with no `base_sha` cannot be measured — the column is nullable, and
/// a NULL there means the daemon never recorded where the branch started. Answering "nothing
/// changed" for it would be the worst possible reading of "we do not know".
pub(super) async fn resolve_run_worktree(
    state: &AppState,
    id: &str,
    run: i64,
) -> Result<(PathBuf, String), StatusCode> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT path, base_sha FROM worktrees \
          WHERE owner_kind = 'run' AND owner_id = ? AND project_id = ? AND removed_at IS NULL",
    )
    .bind(run)
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match row {
        Some((path, Some(base))) => Ok((PathBuf::from(path), base)),
        // A worktree with no recorded base is a different answer from a worktree that is not there,
        // and both are refusals the caller has to be able to tell apart from an empty change set.
        Some((_, None)) => Err(StatusCode::UNPROCESSABLE_ENTITY),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// What a project would lose by leaving, and what is stopping it.
///
/// Fetched by the roster's remove control when somebody opens it, and not before: it is nine
/// `COUNT(*)`s that only matter once a person is deciding, and a roster of twenty-five would
/// otherwise run them all every three seconds to draw a button nobody pressed.
pub(super) async fn get_project_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::project_exit::ProjectRecord>, StatusCode> {
    // No `resolve_project_root` here, and that is not an oversight: it 404s a project with no
    // folder named, and `charlie` — registered, switched off, never pointed anywhere — is exactly
    // the project somebody most wants to remove. The store answers whether the roster knows the
    // name, which is the only thing this route needs to be sure of.
    match crate::project_exit::record(&state.pool, &id).await {
        Ok(Some(record)) => Ok(Json(record)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(project_id = %id, %error, "reading a project's record failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Who last touched each line of a file, in the project or in one run's worktree.
pub(super) async fn get_project_blame(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<Vec<inspect::BlameLine>>, StatusCode> {
    let root = resolve_read_root(&state, &id, query.run).await?;
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::blame(&root, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

/// What one run has changed, and how big the tree it changed it in is.
///
/// `run` is required here, unlike the readers: "what changed" has no meaning against a project root
/// with no branch point to measure from. The uncommitted diff of the main checkout is a different
/// question and `/diff` already answers it.
pub(super) async fn get_project_changed(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<inspect::Changed>, StatusCode> {
    let run = query.run.ok_or(StatusCode::BAD_REQUEST)?;
    let (root, base) = resolve_run_worktree(&state, &id, run).await?;
    tokio::task::spawn_blocking(move || inspect::changed(&root, &base))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

/// Where one run's checkout is, and what it was cut from.
///
/// **The absolute path is the point of this route.** Every door to VS Code needs one — the editor's
/// URL handler takes a full path and nothing else — and the shell has no way to build one: it holds
/// repository-relative paths, and a run's worktree is not under the project root but beside it. A
/// link built from a relative path opens nothing, silently, which is the worst way for a seam to
/// fail.
///
/// The same guard as every other run-aware read: the row must belong to the project being asked.
pub(super) async fn get_project_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<WorktreeView>, StatusCode> {
    let run = query.run.ok_or(StatusCode::BAD_REQUEST)?;
    let row: Option<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT path, branch, base_sha, created_at FROM worktrees \
          WHERE owner_kind = 'run' AND owner_id = ? AND project_id = ? AND removed_at IS NULL",
    )
    .bind(run)
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (path, branch, base_sha, created_at) = row.ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(WorktreeView {
        path,
        branch,
        base_sha,
        created_at,
    }))
}

pub(super) async fn get_project_ls(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<Vec<inspect::Entry>>, StatusCode> {
    let root = resolve_read_root(&state, &id, query.run).await?;
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::ls(&root, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

pub(super) async fn get_project_cat(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<String, StatusCode> {
    let root = resolve_read_root(&state, &id, query.run).await?;
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::cat(&root, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(inspect_status)
}

pub(super) async fn get_project_grep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GrepQuery>,
) -> Result<Json<Vec<inspect::Match>>, StatusCode> {
    let root = resolve_read_root(&state, &id, query.run).await?;
    let q = query.q.unwrap_or_default();
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::grep(&root, &q, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

/// A project's recent commits.
///
/// `path` narrows the history and travels the same road every other caller-supplied path here
/// travels: `inspect::log` runs it through `safe_join` before git sees it, because a pathspec that
/// begins with `-` is an option and one containing `..` reaches outside the repository.
pub(super) async fn get_project_log(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Json<Vec<inspect::Commit>>, StatusCode> {
    let root = resolve_read_root(&state, &id, query.run).await?;
    let rel = query.path.unwrap_or_default();
    let limit = query.limit.unwrap_or(50);
    tokio::task::spawn_blocking(move || inspect::log(&root, &rel, limit))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

/// Every local branch, and how far each is from where work lands.
pub(super) async fn get_project_branches(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<inspect::Branches>, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    tokio::task::spawn_blocking(move || inspect::branches(&root))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

/// The uncommitted diff of the project, or everything one run changed since it branched.
///
/// The two are different questions and the `run` parameter chooses between them. Without it this is
/// what it always was: the main checkout's working tree. With it, the comparison is against the
/// branch point — because a run's checkpoints are commits, and a working-tree diff of a run
/// halfway through a job shows nothing at all.
pub(super) async fn get_project_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<String, StatusCode> {
    let rel = query.path.unwrap_or_default();
    let Some(run) = query.run else {
        let root = resolve_project_root(&state, &id).await?;
        return tokio::task::spawn_blocking(move || inspect::diff(&root))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(inspect_status);
    };

    let (root, base) = resolve_run_worktree(&state, &id, run).await?;
    tokio::task::spawn_blocking(move || inspect::diff_since(&root, &base, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(inspect_status)
}
