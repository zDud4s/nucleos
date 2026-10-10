//! Reading and writing a project's tree (`/projects/{id}/ls`, `/cat`, `/grep`, `/log`, `/diff`, `/write`…).

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
        .route("/projects/{id}/write", post(post_project_write))
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

/// Writes one file the app declares itself the author of.
///
/// The five refusals are five different facts and none of them is "no". In order, and the order is
/// the design:
///
/// 1. **423, the emergency stop.** First, before the path is so much as looked at. A write refused
///    for a path reason while the stop is engaged would tell somebody their path was wrong when the
///    answer is that nothing acts right now. The scoped brake counts too: a project held on its own
///    is held for this as well.
///
///    The cost of this guard is one thing and it is worth naming: the rules file cannot be edited
///    HERE while the stop is engaged, which is a moment somebody might well want to edit it. It
///    costs nothing they cannot recover, because layer 2's whole premise is that the editor is one
///    click away and the file is ordinary text — and what it buys is that the shell is not a door
///    with privileges an agent lacks, which is the promise §7.5 makes.
/// 2. **404, no folder.** A project the daemon has no root for has no file to write.
/// 3. **403, not ours.** Everything the table does not name, which is nearly everything. A path
///    with `..` in it lands here rather than at the path guard, and deliberately: the app answers
///    about names it owns, and a traversal is not one — so the filesystem is never touched for a
///    path nobody claimed. **`another_author` is its own refusal beside it**, for a file an
///    installed workflow declares: the fence names it, and the app still may not write it, because
///    it has no parser for it. One code for both would tell somebody the file is nobody's when it
///    has an author standing right there, and the exit for the two is different.
/// 4. **400/404, unwritable.** The claimed path that nonetheless does not land inside its home,
///    which after (3) means one thing: a directory link or junction on the way to it. (Beside it,
///    `no_machine_root` and `bad_project_id` when the project has no state directory to write
///    into at all — see [`project_state_dir`].)
/// 5. **422, invalid, WITH the parser's words.** The one refusal carrying a detail, because the raw
///    hatch is unusable without it — "unprocessable entity" sends somebody to an editor, which is
///    the surface the hatch exists to replace.
///
/// **The file written is the one the registry names, never the string the caller sent.** The two
/// normalise to the same file by the time (3) passes, and joining the caller's spelling anyway
/// would make every future change to `normalise` a security question instead of a tidiness one.
/// It is joined onto the claim's HOME: the núcleo's own rows land in
/// `~/.nucleos/projects/<id>/`, not in the project — see `project/state.rs`.
///
/// This route is in no table in `auth.rs`, so `permits` — which is default-deny — leaves it to
/// Control and Admin. That is not an omission: the project's `autopilot.yaml` carries
/// `gate_command`, and a run able to rewrite it could decide what green means for every gate it
/// will ever face.
pub(super) async fn post_project_write(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<WriteRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    // Unreadable reads as engaged, the rule `assistant.rs` already pins: a stop nobody can ask
    // about is not a stop anybody may assume is off.
    let halted = crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
        || crate::governance::autopilot::scoped_kill_engaged(&state.pool, "project", &id)
            .await
            .unwrap_or(true);
    if halted {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }

    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;

    let claims = claims_for_project(&state, &id, root.clone())
        .await
        .map_err(|status| refusal(status, "internal"))?;
    let crate::ownership::Owner::Declared(claim) = crate::ownership::owner_of(&claims, &body.path)
    else {
        return Err(refusal(StatusCode::FORBIDDEN, "not_ours"));
    };
    let Some(validate) = claim.validate else {
        // The middle state of §12, said in its own words: somebody authors this file and it is not
        // this app. `detail` carries who, because the exit depends on it — a workflow's file is
        // changed where the workflow lives.
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "refusal": "another_author",
                "detail": format!("{} authors this file", claim.owner),
            })),
        ));
    };

    let path = claim.path.clone().into_owned();
    let display = claim.display(&id);
    let home = claim_home(&state, &id, &root, claim)?;
    // A state directory is created by the first file anybody writes into it, and
    // `safe_write_target` canonicalises its root before it will answer — the same order
    // `post_machine_config` takes, and created only once the path is known to be a row.
    if matches!(claim.home, crate::ownership::Home::State) {
        std::fs::create_dir_all(&home).map_err(|error| {
            tracing::warn!(
                %error,
                project_id = %id,
                "could not create this project's state directory"
            );
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    }
    let target = inspect::safe_write_target(&home, &path)
        .map_err(|error| refusal(inspect_status(error), "unwritable"))?;

    if let Err(detail) = validate(&body.contents) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "refusal": "invalid", "detail": detail })),
        ));
    }

    let contents = body.contents;
    tokio::task::spawn_blocking(move || {
        crate::project::state::write_atomically(&target, &contents)
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(|error| {
        tracing::warn!(%error, project_id = %id, %path, "writing a project file failed");
        refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    // After the write, not before, and loudly on failure. A feed line about a write that then failed
    // claims something that did not happen; a write whose line was lost is recoverable from the log,
    // and refusing the request at this point would report a failure for work already done.
    if let Err(error) = feed::append(
        &state.pool,
        Some(&id),
        "config_written",
        &format!("{display} written from the app"),
        None,
        None,
    )
    .await
    {
        tracing::error!(
            %error,
            project_id = %id,
            %path,
            "a project file was written and its feed line was not recorded"
        );
    }

    Ok(StatusCode::NO_CONTENT)
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
