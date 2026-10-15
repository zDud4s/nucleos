//! Axum handlers for the team catalogue and team runs.

use super::*;

// ---------------------------------------------------------------------------------------------
// The HTTP surface
// ---------------------------------------------------------------------------------------------

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

fn team_status(error: &TeamError) -> StatusCode {
    match error {
        TeamError::DuplicateName => StatusCode::CONFLICT,
        TeamError::Invalid(_) | TeamError::UnknownAgent(_) => StatusCode::BAD_REQUEST,
        TeamError::NotFound => StatusCode::NOT_FOUND,
        TeamError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(super) fn refuse(error: TeamError) -> (StatusCode, String) {
    (team_status(&error), error.to_string())
}

pub async fn list_teams(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamView>>, (StatusCode, String)> {
    list(&state.pool).await.map(Json).map_err(refuse)
}

pub async fn create_team(
    State(state): State<AppState>,
    Json(request): Json<TeamRequest>,
) -> Result<(StatusCode, Json<TeamView>), (StatusCode, String)> {
    create(&state.pool, request)
        .await
        .map(|team| (StatusCode::CREATED, Json(team)))
        .map_err(refuse)
}

pub async fn get_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TeamView>, (StatusCode, String)> {
    match get(&state.pool, &id).await {
        Ok(Some(team)) => Ok(Json(team)),
        Ok(None) => Err(refuse(TeamError::NotFound)),
        Err(error) => Err(refuse(error)),
    }
}

pub async fn update_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<TeamRequest>,
) -> Result<Json<TeamView>, (StatusCode, String)> {
    update(&state.pool, &id, request)
        .await
        .map(Json)
        .map_err(refuse)
}

pub async fn delete_team(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    delete(&state.pool, &id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(refuse)
}

#[derive(serde::Deserialize)]
pub struct StartRequest {
    pub request: String,
    /// Where this run may speak while it works, or nothing.
    ///
    /// Optional, and absent on nearly every start: a department that reports nowhere behaves exactly
    /// as it did before this existed. Given here rather than configured on the team, so the address
    /// belongs to THIS piece of work and is chosen by the person starting it.
    #[serde(default)]
    pub report_to_chat_id: Option<String>,
    /// `normal | fast | thorough`. Absent is `normal`. Given per request, not configured on the
    /// team: the team's `max_parallel` IS `normal`, and speed is a choice about this piece of work.
    #[serde(default)]
    pub speed: Option<String>,
}

#[derive(serde::Serialize)]
pub struct StartResponse {
    pub id: String,
}

/// `POST /teams/{id}/runs` → 202. The record exists; the work has not happened yet, which is what
/// makes 202 the honest code and 201 a claim about a finished resource.
pub async fn post_team_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<StartRequest>,
) -> Result<(StatusCode, Json<StartResponse>), (StatusCode, String)> {
    let speed = match body.speed.as_deref() {
        None => crate::speed::Speed::Normal,
        Some(value) => crate::speed::Speed::parse(value).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("speed `{value}` is not one of normal, fast, thorough"),
            )
        })?,
    };
    match start_with(
        &state,
        &id,
        &body.request,
        Lineage::default(),
        speed,
        body.report_to_chat_id.as_deref(),
    )
    .await
    {
        Ok(id) => Ok((StatusCode::ACCEPTED, Json(StartResponse { id }))),
        Err(error @ StartError::NotFound) => Err((StatusCode::NOT_FOUND, error.to_string())),
        Err(error @ StartError::Invalid(_)) => Err((StatusCode::BAD_REQUEST, error.to_string())),
        // 429 and not 402: the ceiling is a window that reopens, and the caller should come back.
        Err(error @ (StartError::BudgetExhausted(_) | StartError::QuotaExhausted(_))) => {
            Err((StatusCode::TOO_MANY_REQUESTS, error.to_string()))
        }
        Err(error @ StartError::Unavailable(_)) => {
            Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
        }
    }
}

pub async fn list_team_runs(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamRun>>, (StatusCode, String)> {
    sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|error| refuse(TeamError::Db(error)))
}

pub async fn get_team_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TeamRunView>, (StatusCode, String)> {
    let run: Option<TeamRun> = sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| refuse(TeamError::Db(error)))?;
    let run = run.ok_or_else(|| refuse(TeamError::NotFound))?;

    let items: Vec<TeamItem> = sqlx::query_as(
        "SELECT ordinal, round, agent_id, description, state, run_id, output_path
         FROM team_items WHERE team_run_id = ? ORDER BY ordinal",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await
    .map_err(|error| refuse(TeamError::Db(error)))?;

    let cost_usd = spend_of(&state.pool, &id).await;
    Ok(Json(TeamRunView {
        run,
        items,
        cost_usd,
    }))
}

pub async fn post_team_run_cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    cancel(&state, &id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(refuse)
}

/// Deleting a run deletes its folder with it — the one way the owner disposes of a delivery before
/// the retention does. Refused while the run is live, because the alternative is deleting a folder
/// specialists are still writing into.
pub async fn delete_team_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT state, workspace FROM team_runs WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| refuse(TeamError::Db(error)))?;
    let (run_state, workspace) = row.ok_or_else(|| refuse(TeamError::NotFound))?;
    if is_live(&run_state) {
        return Err(refuse(TeamError::Invalid(
            "that run is still going; cancel it first",
        )));
    }

    if let Ok(root) = files_root(&state)
        && let Ok(folder) = crate::files::resolve_within(&root, &workspace)
        && folder.exists()
    {
        let _ = std::fs::remove_dir_all(&folder);
    }

    // The items before the run: the foreign key points that way, and `runs.team_run_id` is left
    // pointing at nothing on purpose — a run's cost stays in the ledger after the department that
    // spent it is gone.
    sqlx::query("UPDATE runs SET team_run_id = NULL WHERE team_run_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    sqlx::query("DELETE FROM team_items WHERE team_run_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    // Before the run, like the items and for the same reason: `team_notes.team_run_id` is a real
    // foreign key and `foreign_keys` is ON, so a note left behind does not become an orphan — it
    // makes the DELETE below fail. That is what the constraint is FOR, and it is only worth having
    // if somebody remembers the other half.
    crate::team_notes::delete_for_run(&state.pool, &id)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    // And the actions, which carried the identical reference since 0086 with nothing anywhere
    // deleting them. Deleting a department that had used its alçada answered 500 with `FOREIGN KEY
    // constraint failed` and named nothing — a run that never asked for anything deleted fine, which
    // is why it went unseen. `a_department_that_asked_for_something_can_still_be_deleted` is what
    // keeps it seen.
    //
    // The `proposals` rows they point at are deliberately LEFT. `team_actions.proposal_id` is a
    // logical reference and not a constraint, and what a person was asked to approve is a fact about
    // that person's queue — it does not stop having happened because the department that asked has
    // been tidied away. Same reasoning as `runs.team_run_id` being nulled rather than cascaded, one
    // statement up.
    sqlx::query("DELETE FROM team_actions WHERE team_run_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    sqlx::query("DELETE FROM team_runs WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| refuse(TeamError::Db(error)))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
}

#[derive(Debug, serde::Serialize)]
pub struct FileView {
    pub path: String,
    pub content: String,
}

/// `POST /team-actions` — **the only route of this design a department may call**, and the only one
/// it will ever gain.
///
/// It does not perform the action. See `propose_action`: the surface stops growing here because
/// what arrives is a `kind` and a payload rather than a route per action, and what leaves is a
/// sentence rather than a result.
///
/// The status codes carry the same distinction the sentences do. 400 is "nobody can do this", 403 is
/// "you may not", 422 is "you asked wrongly", 429 is "come back when somebody has decided" — and a
/// model reading only the code would still pick the right next move for three of the four.
pub async fn post_team_action(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ProposeActionRequest>,
) -> Result<Json<ProposeActionResponse>, (StatusCode, String)> {
    propose_action(&state, &scope, &headers, &request)
        .await
        .map(Json)
        .map_err(|error| {
            let status = match &error {
                ActionError::NotADepartment | ActionError::Ungranted(_) => StatusCode::FORBIDDEN,
                ActionError::NoSuchRun => StatusCode::NOT_FOUND,
                ActionError::Unknown(_) => StatusCode::BAD_REQUEST,
                ActionError::Malformed(_) => StatusCode::UNPROCESSABLE_ENTITY,
                // 429 and not 403: the ceiling is a queue that drains, and the caller — or the next
                // run of this department — should come back.
                ActionError::QueueFull(_) => StatusCode::TOO_MANY_REQUESTS,
                ActionError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })
}

/// `POST /team-recruits` — a director says who it needed and did not have.
///
/// The first route in the house that answers differently depending on WHICH NODE of a run is
/// calling. That distinction comes from `team_runs.director_run_id` and not from a scope of its own
/// — see `Caller`, and see `team::send_note`, which now asks the same question for a different
/// reason: this one to refuse a specialist, that one to sign a note.
///
/// (This paragraph opened "The second and last route this scope gains" until `POST /team-notes`
/// became the third. The count was written as a promise and did not survive one feature; what the
/// promise was reaching for is now stated once, as a rule about what belongs in the table, in
/// `auth::TEAM_ROUTES`.)
///
/// 403 for a specialist, and the body says what to do instead: put it in your answer and let the
/// director pass it on. A model reading only the code would retry; the sentence is what stops it.
pub async fn post_team_recruit(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<RecruitRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let answered = if request.suggest_only {
        suggest_model(&state, &scope, &headers, &request).await
    } else {
        propose_teammate(&state, &scope, &headers, &request)
            .await
            .map(|filed| serde_json::to_value(filed).unwrap_or_default())
    };
    answered.map(Json).map_err(|error| {
        let status = match &error {
            RecruitError::NotADepartment | RecruitError::NotTheDirector => StatusCode::FORBIDDEN,
            RecruitError::NoSuchRun => StatusCode::NOT_FOUND,
            // 409 for both: somebody with that id already exists, or a question about them is
            // already open. Neither is a malformed request, and both clear by somebody acting.
            RecruitError::AlreadyExists(_) | RecruitError::AlreadyAsked(_) => StatusCode::CONFLICT,
            RecruitError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            RecruitError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, error.to_string())
    })
}

/// `POST /team-notes` — one member of a department leaves words for another.
///
/// The third route this scope gains, and the first whose effect never leaves the run that made it:
/// what it writes is read by another node holding the very same key. The two before it record a
/// request for somebody OUTSIDE to act on; this one records a sentence for somebody inside.
///
/// The status codes carry the same distinction the sentences do, and 422 is doing the most work:
/// every one of its three causes is a model that can still fix it this turn, with the roster in
/// front of it.
pub async fn post_team_note(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<TeamNoteRequest>,
) -> Result<Json<TeamNoteResponse>, (StatusCode, String)> {
    send_note(&state, &scope, &headers, &request)
        .await
        .map(Json)
        .map_err(|error| {
            let status = match &error {
                NoteError::NotADepartment | NoteError::UnknownNode => StatusCode::FORBIDDEN,
                NoteError::NoSuchRun => StatusCode::NOT_FOUND,
                // 422 and not 400 for all three: the request is well-formed and it is the CONTENT
                // that cannot stand — a name off the roster, the caller's own name, nothing at all.
                // Each is a model that mis-typed or mis-thought, still mid-turn, and the body names
                // the members so the retry is informed rather than blind.
                NoteError::NoSuchColleague { .. } | NoteError::Yourself | NoteError::Empty => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                // 429 for `propose_action`'s reason, with one difference worth knowing: that ceiling
                // is a queue that DRAINS as a person decides, and this one does not — it is spent
                // for the life of the run. The code still says "not now, and not by retrying", which
                // is the part that governs what the model does next.
                NoteError::Ceiling(_) => StatusCode::TOO_MANY_REQUESTS,
                NoteError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })
}

/// `POST /team-reports` — a department says something to its owner, and nothing runs.
///
/// The fourth and last route of this scope, and the only one that reaches outside the department at
/// all. It is still a SAYING rather than a DOING, which is the rule `auth::TEAM_ROUTES` keeps: no
/// turn starts, nothing is spent, and the daemon picks the destination out of a column the
/// department can neither read nor set.
///
/// 403 for a specialist, with the sentence that says what to do instead — `post_team_recruit`'s
/// shape, because a model reading only the code would retry.
pub async fn post_team_report(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ReportRequest>,
) -> Result<Json<ReportResponse>, (StatusCode, String)> {
    report(&state, &scope, &headers, &request)
        .await
        .map(Json)
        .map_err(|error| {
            let status = match &error {
                ReportError::NotADepartment | ReportError::NotTheDirector => StatusCode::FORBIDDEN,
                ReportError::NoSuchRun => StatusCode::NOT_FOUND,
                // 409 for both, and not 404: the run exists and so does the request. What is absent
                // is a place to speak — either never given or since closed — and neither is fixed by
                // asking again differently, which is what a 4xx about the REQUEST would suggest.
                ReportError::NowhereToReport | ReportError::DestinationGone(_) => {
                    StatusCode::CONFLICT
                }
                ReportError::Empty => StatusCode::UNPROCESSABLE_ENTITY,
                ReportError::Ceiling(_) => StatusCode::TOO_MANY_REQUESTS,
                ReportError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, error.to_string())
        })
}

/// `GET /team-runs/{id}/actions` — what that department asked for, and what became of it.
pub async fn list_team_run_actions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TeamAction>>, (StatusCode, String)> {
    sqlx::query_as(
        "SELECT id, team_run_id, ordinal, kind, payload, why, proposal_id, state, error,
                created_at, executed_at
         FROM team_actions WHERE team_run_id = ? ORDER BY id",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|error| refuse(TeamError::Db(error)))
}

/// `GET /team-actions` — everything still waiting on somebody, across every department.
///
/// Includes the `allow` actions that have not run yet, which are waiting on the tick rather than on
/// a person. Both are "asked for and not yet done", which is the question this list answers.
pub async fn list_open_actions(
    State(state): State<AppState>,
) -> Result<Json<Vec<TeamAction>>, (StatusCode, String)> {
    sqlx::query_as(
        "SELECT id, team_run_id, ordinal, kind, payload, why, proposal_id, state, error,
                created_at, executed_at
         FROM team_actions WHERE state IN ('pending', 'working') ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    .map(Json)
    .map_err(|error| refuse(TeamError::Db(error)))
}

/// `POST /team-files/read` — one file out of the calling run's own folder.
///
/// **Which run is reading comes from the key, not from the body.** A `team_run_id` field here would
/// be the caller naming what it may open, and this is the one scope in the house that already names
/// its caller — so it is asked rather than believed. A caller without `Scope::TeamRun` is refused
/// with 403 and not 401: it authenticated perfectly well, it is simply not a department.
///
/// The path is resolved twice, and the second resolution is the one that matters. The workspace is
/// resolved against the files root, and the caller's path against the workspace — so `files.rs`
/// applies its whitelist to what the model chose: `..`, absolutes, drive prefixes, UNC shares and
/// symlinks pointing out of the folder are all refused there rather than reasoned about here.
pub async fn post_read_file(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    Json(request): Json<ReadFileRequest>,
) -> Result<Json<FileView>, (StatusCode, String)> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err((
            StatusCode::FORBIDDEN,
            "only a team run reads a team run's folder".to_owned(),
        ));
    };

    let workspace: String = sqlx::query_scalar("SELECT workspace FROM team_runs WHERE id = ?")
        .bind(&team_run_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "no such team run".to_owned()))?;

    let root = files_root(&state).map_err(|why| (StatusCode::SERVICE_UNAVAILABLE, why))?;
    let folder = crate::files::resolve_within(&root, &workspace)
        .map_err(|_| (StatusCode::NOT_FOUND, "no such folder".to_owned()))?;
    let target = crate::files::resolve_within(&folder, &request.path).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "that path is not inside your folder".to_owned(),
        )
    })?;

    let content = tokio::fs::read_to_string(&target)
        .await
        .map_err(|error| (StatusCode::NOT_FOUND, error.to_string()))?;

    Ok(Json(FileView {
        path: request.path,
        content,
    }))
}
