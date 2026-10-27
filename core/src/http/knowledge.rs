//! Jobs, knowledge, owner notes and capture requests (`/jobs/*`, `/knowledge/*`, `/owner-notes/*`, `/capture-requests/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/jobs", get(get_jobs).post(create_job))
        .route("/jobs/{id}", get(get_job))
        // Distinct from `/runs/{id}/cancel`, which stops one node. Both end the job — a stopped
        // node leaves the tree holding edits no gate measured — but only this one reaches a job
        // that has no node in flight: parked for budget, waiting for the slot, or between nodes.
        .route("/jobs/{id}/cancel", post(cancel_job))
        // Speaking to a job that is already running. Scoped like `POST /runs/{id}/message` and
        // deliberately NOT like the `POST /jobs` one segment above it — see `post_job_note`.
        .route("/jobs/{id}/notes", post(post_job_note))
        // What earlier work learned, and the two things a person does with it. Owner-scoped like
        // the notes above and for a stronger reason: this is the layer that decides what every
        // later run is told, so a token that could write here could rewrite the agent's mind for
        // every project on the machine. A run has one narrower door below: one evidenced row in the
        // working layer of the caller's own job, read only there and closed when that job ends.
        .route("/knowledge", get(list_knowledge).post(post_knowledge))
        // Like `/runs/awaiting-approval` above, this literal coexists with `/{id}` because static
        // segments win in matchit.
        .route("/knowledge/recall", post(recall_knowledge))
        .route("/knowledge/findings", post(post_finding))
        .route("/knowledge/{id}", get(get_knowledge))
        .route("/knowledge/{id}/revert", post(revert_knowledge))
        // Where each distilled row came from. In no `auth.rs` table on purpose: Control and Admin
        // only, like `/knowledge` above.
        .route(
            "/distill/causes",
            get(crate::distill::origin::get_distill_causes),
        )
        // Rows the distiller flagged as near-duplicates; same access as `/distill/causes`.
        .route(
            "/distill/duplicates",
            get(crate::distill::origin::get_near_duplicates),
        )
        // The owner's own notes. In no `auth.rs` table on purpose: the table is default-deny, so a
        // route nobody lists is reachable by Control and Admin alone — the same reasoning as
        // `post_job_note`. No run, service, team or narrower API token reaches them, which is the
        // point: these are the person's thinking, and nothing in the agent's memory reads them.
        // Static segments (`search`, `graph`, `by-target`, `review`, `links`) win over `{id}` in
        // matchit.
        .route("/owner-notes", get(list_owner_notes).post(post_owner_note))
        .route("/owner-notes/search", get(search_owner_notes))
        .route("/owner-notes/graph", get(get_owner_notes_graph))
        .route("/owner-notes/by-target", get(get_owner_notes_by_target))
        .route("/owner-notes/review", get(get_owner_notes_review))
        .route(
            "/owner-notes/{id}",
            get(get_owner_note).patch(patch_owner_note),
        )
        .route("/owner-notes/{id}/related", get(get_owner_note_related))
        .route("/owner-notes/{id}/links", post(post_owner_note_link))
        .route("/owner-notes/{id}/teach", post(post_owner_note_teach))
        .route(
            "/owner-notes/links/{link_id}",
            delete(delete_owner_note_link),
        )
        // Capture requests (spec 2026-10-07-pedidos-captura). In no `auth.rs` table on purpose:
        // Control and Admin only, like the notes an answer becomes.
        .route("/capture-requests", get(get_capture_requests))
        .route(
            "/capture-requests/{job_id}/dismiss",
            post(post_capture_dismiss),
        )
        .route(
            "/capture-requests/{job_id}/answer",
            post(post_capture_answer),
        )
}

/// How many jobs one listing returns.
///
/// Finished jobs are included, so this is a window rather than a queue: a job that stopped for the
/// budget or ran out of clock is exactly the one the user needs to see, and filtering to live ones
/// would make it vanish at the moment it started mattering.
pub(super) const JOB_LIST_LIMIT: i64 = 20;

#[derive(serde::Deserialize)]
pub(super) struct JobsQuery {
    pub(super) project_id: Option<String>,
    /// Only the work in flight, without the `JOB_LIST_LIMIT` ceiling.
    ///
    /// A parameter rather than a route of its own because it is the same question with a filter.
    /// Absent, the answer is byte for byte today's — which is what leaves the Autopilot tab, which
    /// calls this every 3 seconds, exactly as it is.
    pub(super) live: Option<bool>,
}

pub(super) async fn get_jobs(
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> Result<Json<Vec<crate::execution::job::JobSummary>>, StatusCode> {
    let listed = if query.live == Some(true) {
        crate::execution::job::list_live(
            &state.pool,
            query.project_id.as_deref(),
            crate::execution::concurrency::LIVE_LIST_LIMIT,
        )
        .await
    } else {
        crate::execution::job::list(&state.pool, query.project_id.as_deref(), JOB_LIST_LIMIT).await
    };
    listed
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(serde::Serialize)]
pub(super) struct CreateJobResponse {
    pub(super) job_id: i64,
}

/// `POST /jobs` — asks for a job the way `POST /runs` asks for a run.
///
/// Follows `create_run` step for step, and the order of those steps is the interesting part.
///
/// The **global** kill switch is checked first and alone. The scoped kills, the budget and the WIP
/// limit pace proactive autonomy, and a person asking for a job through the shell or the Telegram
/// assistant is not that; the global switch is the emergency stop, and an emergency stop with
/// exemptions is not one. It fails closed — a switch that cannot be read refuses.
///
/// The whole creation is **uncancellable**. The job row is INSERTed `planning` before its worktree
/// exists, and `git worktree add` holds that window open for as long as git takes. A request
/// dropped inside it would strand a live job with no worktree, which the tick then drives forever
/// while holding one of the project's concurrency slots, which nothing but the sweep gives back.
/// Same window `create_run` documents, and wider here, because provisioning a job's worktree is
/// the slowest thing this route does.
///
/// A second job for a project that already has one is a 409, from the unique index rather than from
/// a check here. That stays the right answer until Chunk 4 replaces the index with numbered slots.
pub(super) async fn create_job(
    State(state): State<AppState>,
    Json(request): Json<crate::execution::job::CreateJobRequest>,
) -> Result<(StatusCode, Json<CreateJobResponse>), (StatusCode, String)> {
    match crate::governance::autopilot::kill_switch_engaged(&state.pool).await {
        Ok(false) => {}
        Ok(true) => {
            return Err((
                StatusCode::CONFLICT,
                "the kill switch is engaged; nothing autonomous starts".to_string(),
            ));
        }
        Err(error) => {
            tracing::warn!(%error, "create_job: could not read the kill switch — refusing");
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "the kill switch could not be read".to_string(),
            ));
        }
    }

    let roster = crate::governance::autopilot::project_roster(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "create_job: could not read the project roster");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the project roster could not be read".to_string(),
            )
        })?;
    let resolved =
        crate::execution::job::resolve_start(&roster, &request.project_id).map_err(|refusal| {
            let status = match refusal {
                crate::execution::job::StartRefusal::UnknownProject => StatusCode::NOT_FOUND,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            };
            (status, refusal.reason(&request.project_id))
        })?;

    // Read the same way the scheduler reads it. `None` when git will not answer, which crash
    // recovery reads as "cannot prove the tree stayed put" — the correct meaning rather than the
    // convenient one.
    let head_sha = crate::repo_trigger::current_branch_sha(
        std::path::Path::new(&resolved.project_root),
        "HEAD",
        false,
    )
    .await;

    let outcome = uncancellable(async move {
        crate::execution::job::start(
            &state,
            &crate::execution::job::StartRequest {
                project_id: &request.project_id,
                project_root: &resolved.project_root,
                // Nobody scheduled this one.
                rule_name: None,
                prompt: &request.prompt,
                // The daemon's ceiling, never a number the caller chose — which is why the request
                // has no field for it. The project's `autopilot.yaml` may only lower the fan-out,
                // and an HTTP
                // body filled in by a model is reviewed even less than that file is.
                max_items: crate::config::MAX_ITEMS_CEILING as i64,
                // These two DO come from the caller, unlike `max_items`, and the asymmetry is the
                // point. `max_items` is fan-out per round and has a hard ceiling nobody may raise;
                // these are how long and how much, which are the caller's to choose — under
                // `MAX_ROUNDS_CEILING` and under the house budget, both applied on the way in.
                max_rounds: request.max_rounds,
                budget_usd: request.budget_usd,
                // What a rule that said nothing gets: only the last item is gated.
                gate_each: false,
                review: true,
                // The answer a rule that said nothing about retries gets, and for the same reason:
                // nobody asked, and one more implement run told what the gate said is cheaper than
                // the item it saves. Not the caller's to choose either — the request has no field
                // for it, exactly as it has none for `max_items`.
                gate_retries: crate::config::DEFAULT_GATE_RETRIES as i64,
                head_sha: head_sha.as_deref(),
                // The caller's, like `max_rounds` and `budget_usd` above and unlike `max_items`.
                // Whether it names a team that exists is `job::start`'s to answer and not this
                // route's: the catalogue can change between a request being written and it landing,
                // and the answer has to be read where the row is made.
                team_id: request.team_id.as_deref(),
                // D13: `None` is `autopilot`. Only a workflow the project pins resolves; any other
                // name is `NoGraph`, so the caller picks among the owner's pins and nothing else.
                workflow: request.workflow.as_deref(),
            },
        )
        .await
    })
    .await
    .map_err(|status| (status, "the job could not be started".to_string()))?;

    match outcome {
        crate::execution::job::JobStart::Started(job_id) => {
            Ok((StatusCode::CREATED, Json(CreateJobResponse { job_id })))
        }
        // Still a 409, and still not a 500: no room is a state the asker can act on by waiting. The
        // reason travels because the two ceilings have different remedies — one waits for this
        // project's own work, the other for anybody's.
        crate::execution::job::JobStart::NoRoom(reason) => Err((StatusCode::CONFLICT, reason)),
        // 422 and not 404: the URL is right and the project exists, and it is one field of the body
        // that names something that does not. The same status `resolve_start` gives a project that
        // is real but configured differently from what the request assumed.
        crate::execution::job::JobStart::NoTeam(reason) => {
            Err((StatusCode::UNPROCESSABLE_ENTITY, reason))
        }
        // A pinned workflow whose graph cannot be read or parsed: the same kind of answer as a
        // missing team (a configuration the caller can fix), never a 500.
        crate::execution::job::JobStart::NoGraph(reason) => {
            Err((StatusCode::UNPROCESSABLE_ENTITY, reason))
        }
        // Past the INSERT: the row existed and `fail_early` retired it and said so in the feed. 500
        // rather than 409, because nothing the caller could change would have helped.
        crate::execution::job::JobStart::Failed => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "the job was created and could not be provisioned; it has been retired".to_string(),
        )),
    }
}

pub(super) async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::execution::job::JobDetail>, StatusCode> {
    match crate::execution::job::detail(&state.pool, id).await {
        Ok(Some(detail)) => Ok(Json(detail)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(super) async fn cancel_job(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    // Uncancellable: this terminates the node in flight and only then retires the job. A request
    // dropped in between would leave a job nothing drives with a node still running inside it.
    match uncancellable(async move { crate::execution::job::cancel(&state, id).await }).await? {
        Ok(crate::execution::job::CancelOutcome::Cancelled) => Ok(StatusCode::NO_CONTENT),
        // Already over. Not success: "I stopped it" and "it had already finished" are different
        // answers to the question the user just asked.
        Ok(crate::execution::job::CancelOutcome::NotLive) => Err(StatusCode::CONFLICT),
        Ok(crate::execution::job::CancelOutcome::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct LeaveNoteRequest {
    pub(super) body: String,
}

#[derive(serde::Serialize)]
pub(super) struct LeaveNoteResponse {
    pub(super) note_id: i64,
}

#[derive(serde::Deserialize)]
pub(super) struct KnowledgeNarrow {
    pub(super) scope_kind: Option<String>,
    pub(super) scope_id: Option<String>,
    /// Only read with `scope_kind=observation`.
    pub(super) limit: Option<i64>,
}

/// Everything the layer holds, in every status.
///
/// Not filtered to `active`, deliberately: the reviewable history IS the feature, and a screen that
/// showed only what is in force could not answer "what did it try to learn that I said no to".
///
/// `?scope_kind=&scope_id=` narrows it to one project, team or agent: both or neither, or 422.
/// The one exception is `?scope_kind=observation[&limit=]`, which lists the conversation
/// observations and nothing else; the plain list never carries them.
pub(super) async fn list_knowledge(
    State(state): State<AppState>,
    Query(narrow): Query<KnowledgeNarrow>,
) -> Result<Json<Vec<crate::knowledge::Known>>, StatusCode> {
    let scope = match (narrow.scope_kind, narrow.scope_id) {
        (Some(kind), None) if kind == "observation" => {
            return crate::knowledge::observations(&state.pool, narrow.limit.unwrap_or(100).clamp(1, 500))
                .await
                .map(Json)
                .map_err(|error| {
                    tracing::warn!(%error, "listing observations failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                });
        }
        (None, None) => None,
        (Some(kind), Some(id)) => Some(
            refinement_target(&format!("{kind}:{id}")).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?,
        ),
        _ => return Err(StatusCode::UNPROCESSABLE_ENTITY),
    };
    match &scope {
        Some(scope) => crate::knowledge::in_scope(&state.pool, scope).await,
        None => crate::knowledge::all(&state.pool).await,
    }
    .map(Json)
    .map_err(|error| {
        tracing::warn!(%error, "listing refinements failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// One refinement, read the way a person decides about it: the text, every decision it has been
/// through, what it replaced, and what replaced it.
///
/// The chain is the half `GET /knowledge` cannot give you. A list answers "what is in force";
/// this answers "what did it say before I changed it, and would I want that back" — which is the
/// question somebody asks at the moment they are considering a revert.
pub(super) async fn get_knowledge(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::knowledge::History>, StatusCode> {
    match crate::knowledge::history(&state.pool, id).await {
        Ok(Some(history)) => Ok(Json(history)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(knowledge_id = id, %error, "reading the history of what is known failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Taking one back. The half that makes approving safe to do at all.
pub(super) async fn revert_knowledge(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<RevertKnowledgeRequest>,
) -> Result<StatusCode, StatusCode> {
    let note = request
        .note
        .unwrap_or_else(|| "reverted by the owner".to_owned());
    match crate::knowledge::revert(&state.pool, id, &note).await {
        // 409 and not 404: the row may well exist and simply not be active, which is a different
        // thing for the caller to do about it.
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::CONFLICT),
        Err(error) => {
            tracing::warn!(knowledge_id = id, %error, "reverting what is known failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(serde::Deserialize)]
pub(super) struct RevertKnowledgeRequest {
    pub(super) note: Option<String>,
}

/// `POST /jobs/{id}/notes` — leaves words for whichever of this job's nodes comes next.
///
/// **Admin, by being in no scope table at all.** `auth::permits` is default-deny, so a route nobody
/// lists is reachable only by Control and Admin, and that is the grade this one wants. The table it
/// must not be added to is `RUN_CREATING_ROUTES`, where `POST /jobs` sits one path segment away:
/// creating a job authorises the prompt supplied at that moment, in advance of the work existing,
/// while a note adds a second author to work already running past every check its creation went
/// through. That is exactly why `POST /runs/{id}/message` is kept out of the same table, and a note
/// is steering with a longer wait.
///
/// The author is [`notes::OWNER`] and never a field of the body. Only Control and Admin arrive here
/// and both of them are the person, so there is nothing for a caller to claim to be — and a note
/// that could name its own author is the first half of a run leaving notes for another run.
///
/// 201 rather than 204 because the note now has an id and a life the caller can watch: it appears in
/// `GET /jobs/{id}` while it waits, and carries the run it eventually reached once it has been read
/// out. The acknowledgement says only that the words were accepted — nothing has read them yet, and
/// the next node may be minutes away or may be the review at the end of the night.
///
/// A note left on a job that has already finished is accepted and never delivered. Refusing it would
/// mean deciding here what "still able to hear you" means, and the honest place to see that is the
/// job detail, where the note sits visibly undelivered beside a job that is over.
pub(super) async fn post_job_note(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<LeaveNoteRequest>,
) -> Result<(StatusCode, Json<LeaveNoteResponse>), StatusCode> {
    // A note with no words is not a note: it would be an entry in the queue that delivers nothing
    // and can never be delivered again.
    let body = request.body.trim();
    if body.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Asked before the insert so a note for a job that does not exist is a 404 and not the 500 the
    // foreign key would otherwise make of it.
    let job: Option<i64> = sqlx::query_scalar("SELECT id FROM jobs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(job_id = id, %error, "reading a job to leave a note on it failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if job.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    crate::notes::leave(&state.pool, id, body, crate::notes::OWNER)
        .await
        .map(|note_id| (StatusCode::CREATED, Json(LeaveNoteResponse { note_id })))
        .map_err(|error| {
            tracing::warn!(job_id = id, %error, "leaving a note on a job failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Body of `POST /owner-notes`. The field is `note_text` in Rust so the redact scan keeps it out of
/// any tracing call, and `text` on the wire. No `author`: a note is the owner's by construction.
#[derive(serde::Deserialize)]
pub(super) struct PostOwnerNoteRequest {
    #[serde(rename = "text")]
    pub(super) note_text: String,
    pub(super) origin: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct PatchOwnerNoteRequest {
    #[serde(rename = "text")]
    pub(super) note_text: Option<String>,
    pub(super) state: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct OwnerNoteListQuery {
    pub(super) state: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct OwnerNoteSearchQuery {
    pub(super) q: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct OwnerNoteGraphQuery {
    pub(super) include_archived: Option<bool>,
}

#[derive(serde::Deserialize)]
pub(super) struct PostOwnerNoteLinkRequest {
    pub(super) link_type: String,
    pub(super) target_kind: String,
    pub(super) target_ref: String,
}

/// Body of `POST /owner-notes/{id}/teach`. Every field is optional: the kind defaults to `memory`
/// and the title to the note's first line.
#[derive(serde::Deserialize)]
pub(super) struct TeachOwnerNoteRequest {
    pub(super) kind: Option<String>,
    pub(super) title: Option<String>,
    pub(super) project_id: Option<String>,
}

/// The status a note failure answers with. Never logs the text: only the id and the database error.
pub(super) fn owner_note_status(
    error: &crate::owner_notes::NoteError,
    note_id: Option<i64>,
) -> StatusCode {
    use crate::owner_notes::NoteError;
    match error {
        NoteError::Empty | NoteError::UnknownOrigin | NoteError::UnknownState => {
            StatusCode::BAD_REQUEST
        }
        NoteError::NotFound => StatusCode::NOT_FOUND,
        NoteError::Db(error) => {
            tracing::warn!(note_id, %error, "an owner note operation failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

pub(super) fn owner_note_db_status(error: &sqlx::Error, note_id: Option<i64>) -> StatusCode {
    tracing::warn!(note_id, %error, "reading the owner notes failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

pub(super) async fn post_owner_note(
    State(state): State<AppState>,
    Json(request): Json<PostOwnerNoteRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let origin = request.origin.as_deref().unwrap_or("shell");
    let id = crate::owner_notes::create(&state.pool, &request.note_text, origin)
        .await
        .map_err(|error| owner_note_status(&error, None))?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

pub(super) async fn list_owner_notes(
    State(state): State<AppState>,
    Query(query): Query<OwnerNoteListQuery>,
) -> Result<Json<Vec<crate::owner_notes::OwnerNote>>, StatusCode> {
    use crate::owner_notes::StateFilter;
    let filter = match query.state.as_deref() {
        None | Some("active") => StateFilter::Active,
        Some("archived") => StateFilter::Archived,
        Some("all") => StateFilter::All,
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };
    crate::owner_notes::list(&state.pool, filter)
        .await
        .map(Json)
        .map_err(|error| owner_note_status(&error, None))
}

pub(super) async fn search_owner_notes(
    State(state): State<AppState>,
    Query(query): Query<OwnerNoteSearchQuery>,
) -> Result<Json<Vec<crate::owner_notes::OwnerNote>>, StatusCode> {
    crate::owner_notes::search(&state.pool, query.q.as_deref().unwrap_or(""))
        .await
        .map(Json)
        .map_err(|error| owner_note_status(&error, None))
}

pub(super) async fn get_owner_note(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let note = crate::owner_notes::get(&state.pool, id)
        .await
        .map_err(|error| owner_note_status(&error, Some(id)))?;
    let links_out = crate::owner_notes::links_out(&state.pool, id)
        .await
        .map_err(|error| owner_note_db_status(&error, Some(id)))?;
    let links_in = crate::owner_notes::links_in(&state.pool, "note", &id.to_string())
        .await
        .map_err(|error| owner_note_db_status(&error, Some(id)))?;
    let events = crate::owner_notes::events(&state.pool, id)
        .await
        .map_err(|error| owner_note_status(&error, Some(id)))?;
    Ok(Json(serde_json::json!({
        "note": note,
        "links_out": links_out,
        "links_in": links_in,
        "events": events,
    })))
}

pub(super) async fn patch_owner_note(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<PatchOwnerNoteRequest>,
) -> Result<Json<crate::owner_notes::OwnerNote>, StatusCode> {
    crate::owner_notes::update(
        &state.pool,
        id,
        request.note_text.as_deref(),
        request.state.as_deref(),
    )
    .await
    .map(Json)
    .map_err(|error| owner_note_status(&error, Some(id)))
}

/// Whether a project id is on the roster. Roster failures are a 500, never "absent".
pub(super) async fn owner_note_project_exists(
    state: &AppState,
    project_id: &str,
) -> Result<bool, StatusCode> {
    let roster = crate::governance::autopilot::project_roster(&state.pool)
        .await
        .map_err(|error| owner_note_db_status(&error, None))?;
    Ok(roster
        .iter()
        .any(|project| project.project_id == project_id))
}

/// Whether a path names a file under the files root.
pub(super) fn owner_note_file_exists(state: &AppState, path: &str) -> Result<bool, StatusCode> {
    let root = files_root(state)?;
    Ok(crate::files::resolve_file(root, path).is_ok_and(|target| target.is_file()))
}

pub(super) async fn post_owner_note_link(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<PostOwnerNoteLinkRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, String)> {
    use crate::owner_notes::LinkError;
    let target_ref = request.target_ref.trim();
    crate::owner_notes::link_allowed(&request.link_type, &request.target_kind, id, target_ref)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;

    let exists = match request.target_kind.as_str() {
        "project" => owner_note_project_exists(&state, target_ref)
            .await
            .map_err(|status| (status, String::new()))?,
        "file" => {
            owner_note_file_exists(&state, target_ref).map_err(|status| (status, String::new()))?
        }
        kind => crate::owner_notes::target_exists(&state.pool, kind, target_ref)
            .await
            .map_err(|error| (owner_note_db_status(&error, Some(id)), String::new()))?
            .unwrap_or(false),
    };
    if !exists {
        return Err((StatusCode::NOT_FOUND, "that target does not exist".into()));
    }

    match crate::owner_notes::add_link(
        &state.pool,
        id,
        &request.link_type,
        &request.target_kind,
        target_ref,
    )
    .await
    {
        Ok(link_id) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": link_id })),
        )),
        Err(LinkError::Duplicate) => Err((StatusCode::CONFLICT, LinkError::Duplicate.to_string())),
        Err(LinkError::NotFound) => Err((StatusCode::NOT_FOUND, "note not found".into())),
        Err(LinkError::Db(error)) => Err((owner_note_db_status(&error, Some(id)), String::new())),
        Err(other) => Err((StatusCode::BAD_REQUEST, other.to_string())),
    }
}

/// Teaching a note: it becomes a `proposed` lesson owned by the owner, and reaches no prompt until
/// the proposal is approved through `POST /proposals/{id}/approve`.
pub(super) async fn post_owner_note_teach(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<TeachOwnerNoteRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    use crate::owner_notes::TeachError;
    let kind = crate::knowledge::Kind::parse(request.kind.as_deref().unwrap_or("memory"))
        .ok_or_else(|| refusal(StatusCode::BAD_REQUEST, "unknown_kind"))?;
    // A lesson scoped to a project nobody has is one no run's scope chain ever reads.
    if let Some(project_id) = request.project_id.as_deref() {
        let known = owner_note_project_exists(&state, project_id)
            .await
            .map_err(|status| refusal(status, "internal"))?;
        if !known {
            return Err(refusal(StatusCode::BAD_REQUEST, "unknown_project"));
        }
    }
    match crate::owner_notes::teach(
        &state.pool,
        id,
        kind,
        request.title.as_deref(),
        request.project_id.as_deref(),
    )
    .await
    {
        Ok((knowledge_id, proposal_id, link_id)) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({
                "knowledge_id": knowledge_id,
                "proposal_id": proposal_id,
                "link_id": link_id,
            })),
        )),
        Err(TeachError::NotFound) => Err(refusal(StatusCode::NOT_FOUND, "not_found")),
        Err(TeachError::Archived) => Err(refusal(StatusCode::CONFLICT, "archived")),
        Err(TeachError::AlreadyTaught) => Err(refusal(StatusCode::CONFLICT, "already_taught")),
        // Only the id and the error: the note's text never reaches a log line.
        Err(error) => {
            tracing::warn!(note_id = id, %error, "teaching an owner note failed");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

pub(super) async fn delete_owner_note_link(
    State(state): State<AppState>,
    Path(link_id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    use crate::owner_notes::LinkError;
    match crate::owner_notes::remove_link(&state.pool, link_id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(LinkError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(LinkError::Db(error)) => Err(owner_note_db_status(&error, None)),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct CaptureListQuery {
    pub(super) state: Option<String>,
}

pub(super) async fn get_capture_requests(
    State(state): State<AppState>,
    Query(query): Query<CaptureListQuery>,
) -> Result<Json<Vec<crate::capture::CaptureRequest>>, (StatusCode, Json<serde_json::Value>)> {
    let listing = match query.state.as_deref() {
        None => crate::capture::Listing::Open,
        Some(s) => crate::capture::Listing::parse(s)
            .ok_or_else(|| refusal(StatusCode::BAD_REQUEST, "unknown_state"))?,
    };
    crate::capture::list(&state.pool, listing, chrono::Utc::now())
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "listing capture requests failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })
}

pub(super) async fn post_capture_dismiss(
    State(state): State<AppState>,
    Path(job_id): Path<i64>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    use crate::capture::CaptureError;
    let dismissed = crate::capture::retry_busy(
        crate::capture::BUSY_ATTEMPTS,
        crate::capture::BUSY_PAUSE,
        |error: &CaptureError| matches!(error, CaptureError::Db(db) if crate::capture::is_busy(db)),
        || crate::capture::dismiss(&state.pool, job_id, chrono::Utc::now()),
    )
    .await;
    match dismissed {
        Ok(()) => Ok(StatusCode::OK),
        Err(CaptureError::NotFound) => Err(refusal(StatusCode::NOT_FOUND, "not_found")),
        Err(CaptureError::Closed) => Err(refusal(StatusCode::CONFLICT, "closed")),
        Err(CaptureError::Db(error)) => {
            tracing::warn!(job_id, %error, "dismissing a capture request failed");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

#[derive(serde::Deserialize)]
pub(super) struct CaptureAnswerRequest {
    #[serde(rename = "text")]
    pub(super) note_text: String,
    pub(super) origin: Option<String>,
}

/// The owner's answer becomes a note linked to the job; it also closes the request and releases the
/// job when the request is still open. A late answer keeps its note and releases nothing.
pub(super) async fn post_capture_answer(
    State(state): State<AppState>,
    Path(job_id): Path<i64>,
    Json(body): Json<CaptureAnswerRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    use crate::capture::AnswerError;
    let origin = body.origin.as_deref().unwrap_or("shell");
    let (note_id, released) =
        match crate::capture::answer(&state.pool, job_id, &body.note_text, origin).await {
            Ok(done) => done,
            Err(AnswerError::NotFound) => return Err(refusal(StatusCode::NOT_FOUND, "not_found")),
            Err(AnswerError::Empty) => return Err(refusal(StatusCode::BAD_REQUEST, "empty_text")),
            Err(AnswerError::UnknownOrigin) => {
                return Err(refusal(StatusCode::BAD_REQUEST, "unknown_origin"));
            }
            Err(error @ (AnswerError::Db(_) | AnswerError::Internal(_))) => {
                tracing::warn!(job_id, ?error, "answering a capture request failed");
                return Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"));
            }
        };
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "note_id": note_id, "released": released })),
    ))
}

/// The graph: notes, the links between them and what they point at. Only entities some link
/// references are returned as targets — the rest of the house is not a node (spec S5).
pub(super) async fn get_owner_notes_graph(
    State(state): State<AppState>,
    Query(query): Query<OwnerNoteGraphQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    use crate::owner_notes::StateFilter;
    let include_archived = query.include_archived.unwrap_or(false);
    let filter = if include_archived {
        StateFilter::All
    } else {
        StateFilter::Active
    };
    let notes = crate::owner_notes::list(&state.pool, filter)
        .await
        .map_err(|error| owner_note_status(&error, None))?;
    let shown: std::collections::HashSet<i64> = notes.iter().map(|note| note.id).collect();
    let links: Vec<_> = crate::owner_notes::all_links(&state.pool)
        .await
        .map_err(|error| owner_note_db_status(&error, None))?
        .into_iter()
        .filter(|link| shown.contains(&link.note_id))
        .collect();

    let mut wanted: Vec<(&str, &str)> = Vec::new();
    for link in &links {
        let pair = (link.target_kind.as_str(), link.target_ref.as_str());
        if !wanted.contains(&pair) {
            wanted.push(pair);
        }
    }
    let mut targets = crate::owner_notes::resolve_sql_labels(&state.pool, &wanted)
        .await
        .map_err(|error| owner_note_db_status(&error, None))?;

    let roster = if targets.iter().any(|target| target.kind == "project") {
        crate::governance::autopilot::project_roster(&state.pool)
            .await
            .map_err(|error| owner_note_db_status(&error, None))?
    } else {
        Vec::new()
    };
    for target in &mut targets {
        match target.kind.as_str() {
            "project" => {
                target.missing = !roster
                    .iter()
                    .any(|project| project.project_id == target.r#ref);
                target.label = Some(target.r#ref.clone());
            }
            "file" => {
                // No files root, or a path that no longer resolves, is a missing file here and not
                // an error: the graph must still draw.
                target.missing = !owner_note_file_exists(&state, &target.r#ref).unwrap_or(false);
                target.label = Some(target.r#ref.clone());
            }
            "job" => {
                // `prompt` is nullable on old rows, so the outer Option is "row gone" and the
                // inner one "no prompt to quote".
                let row = match target.r#ref.trim().parse::<i64>() {
                    Ok(id) => sqlx::query_scalar::<_, Option<String>>(
                        "SELECT prompt FROM jobs WHERE id = ?",
                    )
                    .bind(id)
                    .fetch_optional(&state.pool)
                    .await
                    .map_err(|error| owner_note_db_status(&error, None))?,
                    Err(_) => None,
                };
                target.missing = row.is_none();
                target.label = row.map(|prompt| {
                    let first: String = prompt
                        .as_deref()
                        .and_then(|text| text.lines().next())
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect();
                    if first.is_empty() {
                        format!("job #{}", target.r#ref)
                    } else {
                        format!("job #{} \u{2014} {first}", target.r#ref)
                    }
                });
            }
            _ => {}
        }
    }
    Ok(Json(serde_json::json!({
        "notes": notes,
        "links": links,
        "targets": targets,
    })))
}

#[derive(serde::Deserialize)]
pub(super) struct OwnerNoteByTargetQuery {
    pub(super) kind: String,
    #[serde(rename = "ref")]
    pub(super) target_ref: String,
}

/// `GET /owner-notes/by-target?kind=&ref=` — the notes pinned to one target, active first. The
/// target is not looked up, so a gone target still lists the notes written about it.
pub(super) async fn get_owner_notes_by_target(
    State(state): State<AppState>,
    Query(query): Query<OwnerNoteByTargetQuery>,
) -> Result<Json<Vec<crate::owner_notes::related::TargetNote>>, (StatusCode, String)> {
    use crate::owner_notes::LinkError;
    crate::owner_notes::related::by_target(&state.pool, &query.kind, &query.target_ref)
        .await
        .map(Json)
        .map_err(|error| match error {
            LinkError::Db(error) => (owner_note_db_status(&error, None), String::new()),
            other => (StatusCode::BAD_REQUEST, other.to_string()),
        })
}

#[derive(serde::Deserialize)]
pub(super) struct OwnerNoteRelatedQuery {
    pub(super) limit: Option<i64>,
}

/// How many related notes `GET /owner-notes/{id}/related` returns when `limit` is absent.
const RELATED_DEFAULT_LIMIT: i64 = 8;
/// The most it ever returns, whatever `limit` asks for.
const RELATED_MAX_LIMIT: i64 = 20;

fn clamp_related_limit(limit: Option<i64>) -> usize {
    limit
        .unwrap_or(RELATED_DEFAULT_LIMIT)
        .clamp(1, RELATED_MAX_LIMIT) as usize
}

/// `GET /owner-notes/{id}/related?limit=` — the active notes most like this one, best first.
pub(super) async fn get_owner_note_related(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<OwnerNoteRelatedQuery>,
) -> Result<Json<Vec<crate::owner_notes::related::Related>>, StatusCode> {
    crate::owner_notes::related::related(&state.pool, id, clamp_related_limit(query.limit))
        .await
        .map(Json)
        .map_err(|error| owner_note_status(&error, Some(id)))
}

/// `GET /owner-notes/review` — the review of the current ISO week, in local time.
pub(super) async fn get_owner_notes_review(
    State(state): State<AppState>,
) -> Result<Json<crate::owner_notes::review::Review>, StatusCode> {
    crate::owner_notes::review::weekly_review(&state.pool, chrono::Local::now())
        .await
        .map(Json)
        .map_err(|error| owner_note_db_status(&error, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `channels::testkit` is `#[cfg(test)]` in the lib and does not exist when the bin's tests
    // compile; the `mod http` tests build their state with `http::tests::test_state`.
    async fn state() -> AppState {
        super::super::tests::test_state().await
    }

    async fn note(state: &AppState, text: &str) -> i64 {
        crate::owner_notes::create(&state.pool, text, "shell")
            .await
            .unwrap()
    }

    fn by_target_query(kind: &str, target_ref: &str) -> Query<OwnerNoteByTargetQuery> {
        Query(OwnerNoteByTargetQuery {
            kind: kind.to_owned(),
            target_ref: target_ref.to_owned(),
        })
    }

    #[test]
    fn the_related_limit_defaults_to_eight_and_stays_within_one_to_twenty() {
        assert_eq!(clamp_related_limit(None), 8);
        assert_eq!(clamp_related_limit(Some(5)), 5);
        assert_eq!(clamp_related_limit(Some(0)), 1);
        assert_eq!(clamp_related_limit(Some(-3)), 1);
        assert_eq!(clamp_related_limit(Some(20)), 20);
        assert_eq!(clamp_related_limit(Some(21)), 20);
        assert_eq!(clamp_related_limit(Some(i64::MAX)), 20);
    }

    #[tokio::test]
    async fn by_target_refuses_an_unknown_kind_with_a_400_and_its_reason() {
        let state = state().await;
        let (status, body) =
            get_owner_notes_by_target(State(state), by_target_query("planet", "mars"))
                .await
                .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "unknown target kind");
    }

    #[tokio::test]
    async fn by_target_lists_the_linked_notes_in_the_contract_shape() {
        let state = state().await;
        let linked = note(&state, "about the project").await;
        note(&state, "about nothing").await;
        let link_id =
            crate::owner_notes::add_link(&state.pool, linked, "relates", "project", "nucleos")
                .await
                .unwrap();

        let Json(listed) =
            get_owner_notes_by_target(State(state.clone()), by_target_query("project", "nucleos"))
                .await
                .unwrap();
        let value = serde_json::to_value(&listed).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 1);
        assert_eq!(value[0]["note"]["id"], linked);
        assert_eq!(value[0]["note"]["text"], "about the project");
        assert_eq!(value[0]["link_id"], link_id);
        assert_eq!(value[0]["link_type"], "relates");

        // A target nobody links to answers an empty list, not an error.
        let Json(none) =
            get_owner_notes_by_target(State(state), by_target_query("project", "gone"))
                .await
                .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn related_answers_404_for_an_unknown_note() {
        let state = state().await;
        let status = get_owner_note_related(
            State(state),
            Path(9999),
            Query(OwnerNoteRelatedQuery { limit: None }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn related_finds_a_note_sharing_a_target_and_never_the_note_itself() {
        let state = state().await;
        let first = note(&state, "pinned to the project").await;
        let second = note(&state, "also pinned there").await;
        for id in [first, second] {
            crate::owner_notes::add_link(&state.pool, id, "relates", "project", "nucleos")
                .await
                .unwrap();
        }

        let Json(related) = get_owner_note_related(
            State(state),
            Path(first),
            Query(OwnerNoteRelatedQuery { limit: Some(1000) }),
        )
        .await
        .unwrap();
        let value = serde_json::to_value(&related).unwrap();
        let ids: Vec<i64> = value
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["note"]["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![second]);
        assert!(value[0]["score"].as_f64().unwrap() > 0.0);
        assert!(
            value[0]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "shared_target")
        );
    }

    #[tokio::test]
    async fn review_answers_the_current_week_in_the_contract_shape() {
        let state = state().await;
        let id = note(&state, "written this week").await;

        let Json(review) = get_owner_notes_review(State(state)).await.unwrap();
        let value = serde_json::to_value(&review).unwrap();
        for key in ["week_start", "week_end"] {
            assert!(
                chrono::DateTime::parse_from_rfc3339(value[key].as_str().unwrap()).is_ok(),
                "{key} is rfc3339"
            );
        }
        assert_eq!(value["created"][0]["id"], id);
        assert_eq!(value["created"][0]["text"], "written this week");
        // No link at all, so the same note is also waiting to be connected.
        assert_eq!(value["unlinked"][0]["id"], id);
        assert!(value["resurfaced"].as_array().unwrap().is_empty());
        assert_eq!(value["waiting_proposals"], 0);
    }
}
